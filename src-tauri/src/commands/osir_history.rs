//! Explicit local inspection, independent of OSIR account authorization.
//! No arbitrary paths, automatic repair, polling or startup history work.
use crate::services::osir::history_inspect::{self, InspectError, Inspection};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "code", content = "reason")]
pub enum HistoryCommandError {
    Busy,
    InvalidThreadId,
    Inspection(InspectError),
    WorkerUnavailable,
    Repair(crate::services::osir::history_repair::RepairError),
    TargetUnavailable,
    PreviewExpired,
    InvalidConfirmation,
}
struct Permit(&'static AtomicBool);
impl Permit {
    fn acquire(gate: &'static AtomicBool) -> Result<Self, HistoryCommandError> {
        gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(gate))
            .map_err(|_| HistoryCommandError::Busy)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn validate_thread_id(id: &str) -> Result<(), HistoryCommandError> {
    // Parameterized SQLite query; reject controls/path-looking input before scheduling.
    if id.is_empty()
        || id.len() > 256
        || id.trim() != id
        || id
            .chars()
            .any(|c| c.is_control() || (c == '/' || c as u32 == 92))
    {
        return Err(HistoryCommandError::InvalidThreadId);
    }
    Ok(())
}

#[tauri::command]
pub async fn osir_inspect_codex_history(
    thread_id: String,
) -> Result<Inspection, HistoryCommandError> {
    validate_thread_id(&thread_id)?;
    let permit = Permit::acquire(&BUSY)?;
    // Permit belongs to the worker: cancellation of the IPC future must not allow
    // another inspection while the blocking worker still holds source snapshots.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let home = crate::session_manager::sources::codex_home();
        history_inspect::inspect_configured(&home, &thread_id)
            .map_err(HistoryCommandError::Inspection)
    })
    .await
    .map_err(|_| HistoryCommandError::WorkerUnavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_thread_ids_are_rejected_without_native_reads() {
        for id in ["", " ", "../thread", "a/b", "a\\b", "a\n", " trailing "] {
            assert!(matches!(
                validate_thread_id(id),
                Err(HistoryCommandError::InvalidThreadId)
            ));
        }
        assert!(validate_thread_id(&"x".repeat(257)).is_err());
        assert!(validate_thread_id("fixture-thread").is_ok());
    }
    #[test]
    fn single_worker_gate_returns_busy_and_releases_after_unwind() {
        static GATE: AtomicBool = AtomicBool::new(false);
        let result = std::panic::catch_unwind(|| {
            let _permit = Permit::acquire(&GATE).unwrap();
            assert!(matches!(
                Permit::acquire(&GATE),
                Err(HistoryCommandError::Busy)
            ));
            panic!("fixture worker panic");
        });
        assert!(result.is_err());
        assert!(Permit::acquire(&GATE).is_ok());
    }
    #[tokio::test]
    async fn cancelled_waiter_does_not_release_running_worker_permit() {
        static GATE: AtomicBool = AtomicBool::new(false);
        let permit = Permit::acquire(&GATE).unwrap();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (done, done_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            let permit = permit;
            started.send(()).unwrap();
            // Bounded even if a test assertion unwinds before sending release.
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            drop(permit);
            let _ = done.send(());
        });
        started_rx.await.unwrap();
        handle.abort();
        assert!(matches!(
            Permit::acquire(&GATE),
            Err(HistoryCommandError::Busy)
        ));
        release.send(()).unwrap();
        done_rx.await.unwrap();
        assert!(Permit::acquire(&GATE).is_ok());
    }
}

use crate::services::osir::history_repair::{
    self, Mapping, Plan, Preview, Receipt, Route, SavedOperation,
};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

const PREVIEW_TTL: Duration = Duration::from_secs(300);
#[derive(Clone, Default)]
pub struct OsirHistoryState {
    preview: Arc<Mutex<Option<StoredPreview>>>,
}
struct StoredPreview {
    id: String,
    created: Instant,
    plan: Plan,
    target: NativeTarget,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetChoice {
    provider_id: String,
    provider_name: String,
    model_id: String,
}
struct NativeTarget {
    choice: TargetChoice,
    route: Route,
    fingerprint: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOptions {
    source: Route,
    targets: Vec<TargetChoice>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReply {
    preview_id: String,
    expires_in_seconds: u64,
    preview: Preview,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    Complete,
    Rollback,
}

/// Read native saved membership + current published catalog. Stack choices must
/// resolve to their exact owner; plain IDs never borrow the default paid route.
fn native_targets(
    state: &crate::store::AppState,
) -> Result<Vec<NativeTarget>, HistoryCommandError> {
    use crate::{
        app_config::AppType,
        live::engine::DeviceStore,
        mode::{current, stack, state as mode_state},
    };
    let invalid = || HistoryCommandError::TargetUnavailable;
    let store = DeviceStore::for_device();
    let mode = mode_state::mode_state(&store, "codex").map_err(|_| invalid())?;
    let stack_state = mode_state::stack(&store, "codex").map_err(|_| invalid())?;
    let live = crate::codex_config::read_codex_config_text().map_err(|_| invalid())?;
    let doc: toml::Value = live.parse().map_err(|_| invalid())?;
    let bucket = doc
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .unwrap_or("openai");
    if bucket.is_empty() || bucket.len() > 256 || bucket.chars().any(char::is_control) {
        return Err(invalid());
    }
    let catalog = crate::codex_config::read_codex_model_catalog_simplified_from_live()
        .map_err(|_| invalid())?;
    let published: Vec<&str> = catalog
        .as_ref()
        .and_then(|c| c.get("models"))
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("model").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let providers = state.db.get_all_providers("codex").map_err(|_| invalid())?;
    let mut choices = Vec::new();
    if mode.is_proxy() && stack_state.enabled {
        if !mode.attached {
            return Err(invalid());
        }
        let members = stack::members(
            &state.db,
            &AppType::Codex,
            &stack_state,
            mode.proxy_route.as_deref(),
        )
        .map_err(|_| invalid())?;
        for member in members {
            for model in member.model_ids {
                if !published.contains(&model.as_str()) {
                    continue;
                }
                let resolved = stack::resolve(&state.db, &store, &AppType::Codex, &model)
                    .map_err(|_| invalid())?;
                if !matches!(resolved,stack::Resolved::Hit(ref hit) if hit.provider.id==member.provider.id)
                {
                    continue;
                }
                choices.push((member.provider.id.clone(), model));
            }
        }
    } else {
        // Non-stack repair is restricted to the currently connected saved row.
        if mode.is_proxy() && !mode.attached {
            return Err(invalid());
        }
        let id = if mode.is_proxy() {
            mode.proxy_route.clone()
        } else {
            current::provider_for(&state.db, &AppType::Codex, current::Purpose::Direct)
                .map_err(|_| invalid())?
        }
        .ok_or_else(invalid)?;
        let provider = providers.get(&id).ok_or_else(invalid)?;
        let config = provider
            .settings_config
            .get("config")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        let saved: toml::Value = config.parse().map_err(|_| invalid())?;
        if !mode.is_proxy()
            && (saved
                .get("model_provider")
                .and_then(toml::Value::as_str)
                .unwrap_or("openai")
                != bucket
                || crate::codex_config::extract_codex_base_url(config)
                    != crate::codex_config::extract_codex_base_url(&live))
        {
            return Err(invalid());
        }
        let models = if stack::is_osir_managed(provider) {
            crate::codex_config::managed_codex_published_models(&provider.settings_config)
        } else {
            crate::codex_config::codex_published_models(&provider.settings_config, config)
        };
        for model in models {
            if !matches!(
                stack::decode(&AppType::Codex, &model),
                stack::Decoded::Plain
            ) {
                continue;
            }
            if catalog.is_some() && !published.contains(&model.as_str()) {
                continue;
            }
            if mode.is_proxy()
                && !matches!(
                    stack::resolve(&state.db, &store, &AppType::Codex, &model)
                        .map_err(|_| invalid())?,
                    stack::Resolved::Plain
                )
            {
                continue;
            }
            choices.push((id.clone(), model));
        }
    }
    if choices.len() > 512 {
        return Err(invalid());
    }
    let mut out = Vec::new();
    for (id, model) in choices {
        if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            continue;
        }
        let provider = providers.get(&id).ok_or_else(invalid)?;
        if crate::mode::stack::is_osir_managed(provider) {
            crate::services::osir::managed::validate_managed_provider_shape(
                &AppType::Codex,
                provider,
            )
            .map_err(|_| invalid())?;
        }
        // Sensitive provider material is hashed natively, never logged/returned.
        let bytes = serde_json::to_vec(&(provider, &mode, &stack_state, &live, &catalog))
            .map_err(|_| invalid())?;
        let fingerprint = format!("{:x}", Sha256::digest(bytes));
        out.push(NativeTarget {
            choice: TargetChoice {
                provider_id: id,
                provider_name: provider.name.clone(),
                model_id: model.clone(),
            },
            route: Route {
                provider: bucket.into(),
                model,
            },
            fingerprint,
        });
    }
    out.sort_by(|a, b| {
        (&a.choice.provider_id, &a.choice.model_id)
            .cmp(&(&b.choice.provider_id, &b.choice.model_id))
    });
    out.dedup_by(|a, b| {
        a.choice.provider_id == b.choice.provider_id && a.choice.model_id == b.choice.model_id
    });
    Ok(out)
}
fn select_target(
    state: &crate::store::AppState,
    provider: &str,
    model: &str,
) -> Result<NativeTarget, HistoryCommandError> {
    if provider.len() > 256 || model.len() > 256 {
        return Err(HistoryCommandError::TargetUnavailable);
    }
    native_targets(state)?
        .into_iter()
        .find(|t| t.choice.provider_id == provider && t.choice.model_id == model)
        .ok_or(HistoryCommandError::TargetUnavailable)
}

#[tauri::command]
pub async fn osir_codex_history_options(
    app: tauri::AppHandle,
    thread_id: String,
) -> Result<RepairOptions, HistoryCommandError> {
    validate_thread_id(&thread_id)?;
    let permit = Permit::acquire(&BUSY)?;
    let state = app.state::<crate::store::AppState>().inner().clone();
    // Acquire only the switch lock: lock_settled would perform configuration
    // migration/recovery, which is not authorized by a read-only history action.
    let switch = state.proxy_service.lock_switch_for_app("codex").await;
    tokio::task::spawn_blocking(move || {
        let (_permit, _switch) = (permit, switch);
        let source =
            history_repair::source(&crate::codex_config::get_codex_config_dir(), &thread_id)
                .map_err(HistoryCommandError::Repair)?;
        let targets = native_targets(&state)?
            .into_iter()
            .map(|t| t.choice)
            .collect();
        Ok(RepairOptions { source, targets })
    })
    .await
    .map_err(|_| HistoryCommandError::WorkerUnavailable)?
}
#[tauri::command]
pub async fn osir_codex_history_preview(
    app: tauri::AppHandle,
    state: tauri::State<'_, OsirHistoryState>,
    thread_id: String,
    provider_id: String,
    model_id: String,
) -> Result<PreviewReply, HistoryCommandError> {
    validate_thread_id(&thread_id)?;
    let permit = Permit::acquire(&BUSY)?;
    let slot = state.preview.clone();
    slot.lock().map_err(|_| HistoryCommandError::Busy)?.take();
    let application = app.state::<crate::store::AppState>().inner().clone();
    let switch = application.proxy_service.lock_switch_for_app("codex").await;
    tokio::task::spawn_blocking(move || {
        let (_permit, _switch) = (permit, switch);
        let target = select_target(&application, &provider_id, &model_id)?;
        let home = crate::codex_config::get_codex_config_dir();
        let source =
            history_repair::source(&home, &thread_id).map_err(HistoryCommandError::Repair)?;
        let mut plan = history_repair::preview(
            &home,
            Mapping {
                thread_id,
                source,
                target: target.route.clone(),
            },
        )
        .map_err(HistoryCommandError::Repair)?;
        plan.bind_target(history_repair::TargetBinding {
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            fingerprint: target.fingerprint.clone(),
        });
        let current = select_target(&application, &provider_id, &model_id)?;
        if current.fingerprint != target.fingerprint || current.route != target.route {
            return Err(HistoryCommandError::TargetUnavailable);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let reply = PreviewReply {
            preview_id: id.clone(),
            expires_in_seconds: PREVIEW_TTL.as_secs(),
            preview: plan.preview().clone(),
        };
        *slot.lock().map_err(|_| HistoryCommandError::Busy)? = Some(StoredPreview {
            id: id.clone(),
            created: Instant::now(),
            plan,
            target,
        });
        // One bounded slot; expiry frees native conversation snapshots even when
        // the renderer disappears. Old expiry cannot remove a newer preview.
        tokio::runtime::Handle::current().spawn(async move {
            tokio::time::sleep(PREVIEW_TTL).await;
            if let Ok(mut entry) = slot.lock() {
                if entry.as_ref().is_some_and(|p| p.id == id) {
                    entry.take();
                }
            }
        });
        Ok(reply)
    })
    .await
    .map_err(|_| HistoryCommandError::WorkerUnavailable)?
}
#[tauri::command]
pub async fn osir_codex_history_apply(
    app: tauri::AppHandle,
    state: tauri::State<'_, OsirHistoryState>,
    preview_id: String,
    confirmed: bool,
) -> Result<Receipt, HistoryCommandError> {
    if !confirmed {
        return Err(HistoryCommandError::InvalidConfirmation);
    }
    let permit = Permit::acquire(&BUSY)?;
    let saved = {
        let mut slot = state
            .preview
            .lock()
            .map_err(|_| HistoryCommandError::Busy)?;
        if !slot.as_ref().is_some_and(|p| p.id == preview_id) {
            return Err(HistoryCommandError::PreviewExpired);
        }
        slot.take().ok_or(HistoryCommandError::PreviewExpired)?
    };
    if saved.created.elapsed() >= PREVIEW_TTL {
        return Err(HistoryCommandError::PreviewExpired);
    }
    let application = app.state::<crate::store::AppState>().inner().clone();
    let switch = application.proxy_service.lock_switch_for_app("codex").await;
    tokio::task::spawn_blocking(move || {
        let (_permit, _switch) = (permit, switch);
        let current = select_target(
            &application,
            &saved.target.choice.provider_id,
            &saved.target.choice.model_id,
        )?;
        if current.fingerprint != saved.target.fingerprint || current.route != saved.target.route {
            return Err(HistoryCommandError::TargetUnavailable);
        }
        history_repair::apply(saved.plan).map_err(HistoryCommandError::Repair)
    })
    .await
    .map_err(|_| HistoryCommandError::WorkerUnavailable)?
}
#[tauri::command]
pub async fn osir_codex_history_operations() -> Result<Vec<SavedOperation>, HistoryCommandError> {
    let permit = Permit::acquire(&BUSY)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        history_repair::operations(&crate::codex_config::get_codex_config_dir())
            .map_err(HistoryCommandError::Repair)
    })
    .await
    .map_err(|_| HistoryCommandError::WorkerUnavailable)?
}
#[tauri::command]
pub async fn osir_codex_history_recover(
    app: tauri::AppHandle,
    state: tauri::State<'_, OsirHistoryState>,
    operation_id: String,
    action: RecoveryAction,
    confirmed: bool,
) -> Result<Receipt, HistoryCommandError> {
    if !confirmed {
        return Err(HistoryCommandError::InvalidConfirmation);
    }
    let permit = Permit::acquire(&BUSY)?;
    state
        .preview
        .lock()
        .map_err(|_| HistoryCommandError::Busy)?
        .take();
    let application = app.state::<crate::store::AppState>().inner().clone();
    let switch = application.proxy_service.lock_switch_for_app("codex").await;
    tokio::task::spawn_blocking(move || {
        let (_permit, _switch) = (permit, switch);
        let home = crate::codex_config::get_codex_config_dir();
        if matches!(action, RecoveryAction::Complete) {
            let bound = history_repair::recovery_binding(&home, &operation_id)
                .map_err(HistoryCommandError::Repair)?;
            let target = select_target(&application, &bound.provider_id, &bound.model_id)?;
            if target.fingerprint != bound.fingerprint {
                return Err(HistoryCommandError::TargetUnavailable);
            }
        }
        history_repair::recover(
            &home,
            &operation_id,
            matches!(action, RecoveryAction::Rollback),
        )
        .map_err(HistoryCommandError::Repair)
    })
    .await
    .map_err(|_| HistoryCommandError::WorkerUnavailable)?
}
