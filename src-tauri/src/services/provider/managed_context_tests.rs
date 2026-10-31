//! Exercise managed context edits through the locked service and real isolated files.
use super::{tests::with_test_home, ProviderService};
use crate::{
    app_config::AppType,
    codex_config::{get_codex_auth_path, get_codex_config_path, get_codex_model_catalog_path},
    config::get_claude_settings_path,
    gemini_config::{get_gemini_env_path, get_gemini_settings_path},
    opencode_config::get_opencode_config_path,
    provider::Provider,
    services::osir::managed::test_support::Fixture,
    settings::{get_settings, update_settings, AppSettings},
    store::AppState,
};
use serde_json::{json, Value};
use serial_test::serial;
use std::{ffi::OsString, fs, path::Path, sync::mpsc, time::Duration};

const SECRET: &str = "synthetic-managed-context-secret";
const CODEX_CONFIG: &str = "model = \"fixture-model\"\nmodel_provider = \"osir\"\n[model_providers.osir]\nname = \"OSIR\"\nbase_url = \"https://api.osirclaw.com/v1\"\nwire_api = \"responses\"\n";

struct SettingsGuard {
    settings: AppSettings,
    env: Vec<(&'static str, Option<OsString>)>,
}

impl SettingsGuard {
    fn new(home: &Path) -> Self {
        let settings = get_settings();
        let env = ["USERPROFILE", "LOCALAPPDATA", "XDG_CONFIG_HOME"]
            .into_iter()
            .map(|key| {
                let previous = std::env::var_os(key);
                std::env::set_var(key, home);
                (key, previous)
            })
            .collect();
        let guard = Self { settings, env };
        update_settings(AppSettings::default()).unwrap();
        for path in live_paths() {
            assert!(path.starts_with(home), "live path escaped test home");
        }
        guard
    }
}

impl Drop for SettingsGuard {
    fn drop(&mut self) {
        // with_test_home still owns the isolated HOME while restoring the cache.
        let _ = update_settings(self.settings.clone());
        for (key, previous) in &self.env {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn isolated(test: impl FnOnce(&AppState)) {
    with_test_home(|state, home| {
        let _settings = SettingsGuard::new(home);
        test(state);
    });
}

fn live_paths() -> Vec<std::path::PathBuf> {
    vec![
        get_codex_config_path(),
        get_codex_auth_path(),
        get_codex_model_catalog_path(),
        get_claude_settings_path(),
        get_gemini_env_path(),
        get_gemini_settings_path(),
        get_opencode_config_path().unwrap(),
    ]
}

fn live_bytes() -> Vec<Option<Vec<u8>>> {
    live_paths()
        .iter()
        .map(|path| match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("cannot read isolated live file: {error}"),
        })
        .collect()
}

fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn fixture(app: &AppType) -> Fixture {
    let settings = match app {
        AppType::Claude => json!({"env": {
            "ANTHROPIC_BASE_URL": "https://api.osirclaw.com",
            "ANTHROPIC_MODEL": "fixture-model"
        }}),
        AppType::Gemini => json!({
            "env": {"GOOGLE_GEMINI_BASE_URL": "https://api.osirclaw.com", "GEMINI_MODEL": "fixture-model"},
            "config": {"model": {"name": "fixture-model"}}
        }),
        AppType::Codex => json!({"auth": {}, "config": CODEX_CONFIG,
            "modelCatalog": {"models": [{"model": "fixture-model"}, {"model": "fixture-other"}]}
        }),
        AppType::OpenCode => json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": {"baseURL": "https://api.osirclaw.com/v1"},
            "models": {"fixture-model": {"name": "Fixture model"}}
        }),
        _ => unreachable!(),
    };
    Fixture::new(app, settings, SECRET)
}

fn saved(state: &AppState, app: &AppType, id: &str) -> Provider {
    state
        .db
        .get_provider_by_id(id, app.as_str())
        .unwrap()
        .unwrap()
}

fn activate(state: &AppState, app: &AppType, fixture: &Fixture) -> Provider {
    state
        .db
        .save_provider(app.as_str(), &fixture.provider)
        .unwrap();
    ProviderService::switch(state, app.clone(), &fixture.provider.id).unwrap();
    // OpenCode activation adds native live-management metadata.
    saved(state, app, &fixture.provider.id)
}

fn context_edit(app: &AppType, base: &Provider) -> Provider {
    let mut edited = base.clone();
    match app {
        AppType::Claude => {
            edited.settings_config["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"] = json!("240000");
            edited.settings_config["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"] = json!("180000");
        }
        AppType::Gemini => {
            edited.settings_config["config"]["experimental"] = json!({"contextManagement": false});
            edited.settings_config["config"]["contextManagement"] =
                json!({"historyWindow": {"maxTokens": 180000}});
        }
        AppType::Codex => {
            let mut config: toml::Value = base.settings_config["config"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            config
                .as_table_mut()
                .unwrap()
                .insert("model_context_window".into(), toml::Value::Integer(240000));
            config.as_table_mut().unwrap().insert(
                "model_auto_compact_token_limit".into(),
                toml::Value::Integer(180000),
            );
            edited.settings_config["config"] = json!(toml::to_string(&config).unwrap());
            edited.settings_config["modelCatalog"]["models"][0]["contextWindow"] = json!(240000);
            edited.settings_config["modelCatalog"]["models"][1]["context_window"] = json!(320000);
        }
        AppType::OpenCode => {
            edited.settings_config["models"]["fixture-model"]["limit"] =
                json!({"context": 240000, "output": 32000});
        }
        _ => unreachable!(),
    }
    edited
}

fn assert_safe_saved(base: &Provider, updated: &Provider) {
    assert_eq!(
        updated.meta.as_ref().unwrap().osir_binding,
        base.meta.as_ref().unwrap().osir_binding
    );
    assert!(!serde_json::to_string(updated).unwrap().contains(SECRET));
}

#[test]
#[serial]
fn managed_context_claude_update_checked_saves_and_projects_both_limits() {
    isolated(|state| {
        let app = AppType::Claude;
        let grant = fixture(&app);
        let base = activate(state, &app, &grant);
        let edited = context_edit(&app, &base);
        assert!(ProviderService::update_checked(
            state,
            app.clone(),
            None,
            edited.clone(),
            Some(&base)
        )
        .unwrap());
        let updated = saved(state, &app, &base.id);
        assert_safe_saved(&base, &updated);
        let live = read_json(get_claude_settings_path());
        for field in [
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        ] {
            assert_eq!(
                updated.settings_config["env"][field],
                edited.settings_config["env"][field]
            );
            assert_eq!(live["env"][field], edited.settings_config["env"][field]);
        }
        assert_eq!(live["env"]["ANTHROPIC_MODEL"], "fixture-model");
        assert!(live["env"]
            .as_object()
            .unwrap()
            .values()
            .any(|value| value == SECRET));
    });
}

#[test]
#[serial]
fn managed_context_gemini_update_checked_saves_and_projects_history_and_compression() {
    isolated(|state| {
        let app = AppType::Gemini;
        let grant = fixture(&app);
        let base = activate(state, &app, &grant);
        let edited = context_edit(&app, &base);
        assert!(ProviderService::update_checked(
            state,
            app.clone(),
            None,
            edited.clone(),
            Some(&base)
        )
        .unwrap());
        let updated = saved(state, &app, &base.id);
        assert_safe_saved(&base, &updated);
        let live = read_json(get_gemini_settings_path());
        for pointer in [
            "/experimental/contextManagement",
            "/contextManagement/historyWindow/maxTokens",
        ] {
            assert_eq!(
                live.pointer(pointer),
                edited.settings_config["config"].pointer(pointer)
            );
            assert_eq!(
                updated.settings_config["config"].pointer(pointer),
                live.pointer(pointer)
            );
        }
        assert_eq!(live["model"]["name"], "fixture-model");
        assert!(fs::read_to_string(get_gemini_env_path())
            .unwrap()
            .contains(SECRET));
    });
}

#[test]
#[serial]
fn managed_context_codex_update_checked_projects_config_and_catalog_aliases() {
    isolated(|state| {
        let app = AppType::Codex;
        let grant = fixture(&app);
        let base = activate(state, &app, &grant);
        let auth_before = fs::read(get_codex_auth_path()).ok();
        let edited = context_edit(&app, &base);
        assert!(ProviderService::update_checked(
            state,
            app.clone(),
            None,
            edited.clone(),
            Some(&base)
        )
        .unwrap());
        let updated = saved(state, &app, &base.id);
        assert_safe_saved(&base, &updated);
        let live: toml::Value = fs::read_to_string(get_codex_config_path())
            .unwrap()
            .parse()
            .unwrap();
        let stored: toml::Value = updated.settings_config["config"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        for (field, expected) in [
            ("model_context_window", 240000),
            ("model_auto_compact_token_limit", 180000),
        ] {
            assert_eq!(live[field].as_integer(), Some(expected));
            assert_eq!(stored[field].as_integer(), Some(expected));
        }
        assert_eq!(live["model"].as_str(), Some("fixture-model"));
        assert_eq!(
            updated.settings_config["modelCatalog"],
            edited.settings_config["modelCatalog"]
        );
        let catalog = read_json(get_codex_model_catalog_path());
        for model in catalog["models"].as_array().unwrap() {
            assert_eq!(model["context_window"], 240000);
            assert_eq!(model["auto_compact_token_limit"], 180000);
        }
        assert_eq!(catalog["models"].as_array().unwrap().len(), 2);
        assert_eq!(fs::read(get_codex_auth_path()).ok(), auth_before);

        let mut catalog_only = updated.clone();
        let mut config = stored;
        config
            .as_table_mut()
            .unwrap()
            .remove("model_context_window");
        config
            .as_table_mut()
            .unwrap()
            .remove("model_auto_compact_token_limit");
        catalog_only.settings_config["config"] = json!(toml::to_string(&config).unwrap());
        assert!(ProviderService::update_checked(
            state,
            app.clone(),
            None,
            catalog_only,
            Some(&updated)
        )
        .unwrap());
        let catalog = read_json(get_codex_model_catalog_path());
        for (slug, expected) in [("fixture-model", 240000), ("fixture-other", 320000)] {
            let model = catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["slug"] == slug)
                .unwrap();
            assert_eq!(model["context_window"], expected);
            assert_eq!(model["max_context_window"], expected);
        }
    });
}

#[test]
#[serial]
fn managed_context_opencode_update_checked_saves_and_projects_model_limits() {
    isolated(|state| {
        let app = AppType::OpenCode;
        let grant = fixture(&app);
        let base = activate(state, &app, &grant);
        let edited = context_edit(&app, &base);
        assert!(ProviderService::update_checked(
            state,
            app.clone(),
            None,
            edited.clone(),
            Some(&base)
        )
        .unwrap());
        let updated = saved(state, &app, &base.id);
        assert_safe_saved(&base, &updated);
        assert_eq!(
            updated.settings_config["models"],
            edited.settings_config["models"]
        );
        let live = read_json(get_opencode_config_path().unwrap());
        assert_eq!(
            live["provider"][&base.id]["models"]["fixture-model"]["limit"],
            json!({"context": 240000, "output": 32000})
        );
        assert_eq!(
            live["provider"][&base.id]["options"]["baseURL"],
            "https://api.osirclaw.com/v1"
        );
    });
}

#[test]
#[serial]
fn managed_context_update_checked_rejects_endpoint_and_metadata_without_writes() {
    isolated(|state| {
        for app in [
            AppType::Claude,
            AppType::Gemini,
            AppType::Codex,
            AppType::OpenCode,
        ] {
            let grant = fixture(&app);
            let base = activate(state, &app, &grant);
            let before = live_bytes();
            let mut endpoint = context_edit(&app, &base);
            match app {
                AppType::Claude => {
                    endpoint.settings_config["env"]["ANTHROPIC_BASE_URL"] =
                        json!("https://untrusted.invalid")
                }
                AppType::Gemini => {
                    endpoint.settings_config["env"]["GOOGLE_GEMINI_BASE_URL"] =
                        json!("https://untrusted.invalid")
                }
                AppType::OpenCode => {
                    endpoint.settings_config["options"]["baseURL"] =
                        json!("https://untrusted.invalid")
                }
                AppType::Codex => {
                    let mut config: toml::Value = endpoint.settings_config["config"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap();
                    config["model_providers"]["osir"]["base_url"] =
                        toml::Value::String("https://untrusted.invalid".into());
                    endpoint.settings_config["config"] = json!(toml::to_string(&config).unwrap());
                }
                _ => unreachable!(),
            }
            let mut metadata = context_edit(&app, &base);
            metadata.meta.as_mut().unwrap().api_format = Some("untrusted-format".into());
            let mut binding = context_edit(&app, &base);
            binding
                .meta
                .as_mut()
                .unwrap()
                .osir_binding
                .as_mut()
                .unwrap()
                .binding
                .key_id += 1;
            for attack in [endpoint, metadata, binding] {
                assert!(
                    ProviderService::update_checked(state, app.clone(), None, attack, Some(&base))
                        .is_err(),
                    "{} accepted forbidden edit",
                    app.as_str()
                );
                assert_eq!(
                    serde_json::to_value(saved(state, &app, &base.id)).unwrap(),
                    serde_json::to_value(&base).unwrap()
                );
                assert_eq!(live_bytes(), before);
            }
        }
    });
}

#[test]
#[serial]
fn managed_context_queued_stale_expected_base_cannot_roll_back_new_model() {
    isolated(|state| {
        let app = AppType::Gemini;
        let grant = fixture(&app);
        let base = activate(state, &app, &grant);
        let incoming = context_edit(&app, &base);
        let guard =
            tauri::async_runtime::block_on(state.proxy_service.lock_switch_for_app("gemini"));
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let expected = base.clone();
            let worker = scope.spawn(move || {
                started_tx.send(()).unwrap();
                let result = ProviderService::update_checked(
                    state,
                    AppType::Gemini,
                    None,
                    incoming,
                    Some(&expected),
                );
                done_tx.send(()).unwrap();
                result
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                done_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            let mut latest = base.clone();
            latest.settings_config["env"]["GEMINI_MODEL"] = json!("newer-model");
            latest.settings_config["config"]["model"]["name"] = json!("newer-model");
            state.db.save_provider("gemini", &latest).unwrap();
            // Simulate the preceding native model write while it owns the switch lock.
            super::gemini_direct::reapply(state.db.as_ref(), &latest).unwrap();
            let before = live_bytes();
            drop(guard);
            let error = worker.join().unwrap().unwrap_err();
            assert!(
                error.to_string().contains("连接设置已变化"),
                "unexpected rejection: {error}"
            );
            assert_eq!(
                serde_json::to_value(saved(state, &app, &base.id)).unwrap(),
                serde_json::to_value(&latest).unwrap()
            );
            assert_eq!(live_bytes(), before);
            assert_eq!(
                read_json(get_gemini_settings_path())["model"]["name"],
                "newer-model"
            );
        });
    });
}
