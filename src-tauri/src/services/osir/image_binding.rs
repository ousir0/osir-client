//! Native image contract reader, independent persisted selection and proxy target.
//! Standard Images capability never authorizes the separate Codex extension headers.
//! Standard Images handlers consume this target through the existing proxy.
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::{key_vault::KeyBinding, managed};
use crate::{
    app_config::AppType,
    provider::{ManagedBinding, Provider},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("图片配置无效或服务端生图能力不可用，请重新选择")]
pub(crate) struct ImageBindingError;
type Result<T> = std::result::Result<T, ImageBindingError>;

// Deliberately neither Deserialize nor Serialize: IPC cannot submit a verified contract.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageProtocol {
    OsirCodexNativeV1,
}

pub(crate) struct VerifiedImageModel {
    pub id: String,
    pub generations: bool,
    pub edits: bool,
}

pub(crate) struct VerifiedImageContract {
    owner: ManagedBinding,
    revision: String,
    models: Vec<VerifiedImageModel>,
    protocol: ImageProtocol,
    valid_until: Instant,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireImageContract {
    account_id: i64,
    key_id: i64,
    group_id: i64,
    revision: String,
    protocol: String,
    expires_at: String,
    models: Vec<WireImageModel>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireImageModel {
    id: String,
    #[serde(default)]
    generations: bool,
    #[serde(default)]
    edits: bool,
}

/// Decode only the real server contract. This is deliberately NOT converted
/// into OsirCodexNativeV1: openai-images-v1 does not attest the extension headers.
/// The authenticated transport caller must check session generation and origin.
pub(crate) fn decode_openai_image_contract(
    data: serde_json::Value,
    provider: &Provider,
    current_key: &KeyBinding,
) -> Result<OpenAiImageContract> {
    saved(provider, current_key)?;
    let wire: WireImageContract = serde_json::from_value(data).map_err(|_| ImageBindingError)?;
    let account_id = current_key
        .account_id
        .parse::<i64>()
        .map_err(|_| ImageBindingError)?;
    if account_id <= 0
        || wire.account_id != account_id
        || wire.key_id != current_key.key_id
        || wire.group_id <= 0
        || Some(wire.group_id) != current_key.group_id
        || wire.protocol != "openai-images-v1"
        || !text(&wire.revision)
        || wire.models.is_empty()
        || wire.models.len() > 512
    {
        return Err(ImageBindingError);
    }
    let expires =
        chrono::DateTime::parse_from_rfc3339(&wire.expires_at).map_err(|_| ImageBindingError)?;
    let lifetime = (expires.with_timezone(&chrono::Utc) - chrono::Utc::now())
        .to_std()
        .map_err(|_| ImageBindingError)?;
    if lifetime.is_zero() || lifetime > Duration::from_secs(60) {
        return Err(ImageBindingError);
    }
    let mut ids = std::collections::HashSet::new();
    for model in &wire.models {
        if !text(&model.id) || !(model.generations || model.edits) || !ids.insert(&model.id) {
            return Err(ImageBindingError);
        }
    }
    Ok(OpenAiImageContract {
        wire,
        valid_until: Instant::now() + lifetime,
    })
}

/// Separate type prevents a standard Images contract from authorizing Codex
/// extension headers. The native reader and standard Images route use this type.
#[derive(Clone)]
pub(crate) struct OpenAiImageContract {
    wire: WireImageContract,
    valid_until: Instant,
}

const SELECTION_KEY: &str = "osir.codex.image-selection.v1";
const CUSTOM_KEY: &str = "osir.codex.image-custom.v1";
pub(crate) const LOCAL_IMAGE_HEADER: &str = "x-openai-actor-authorization";
pub(crate) const LOCAL_IMAGE_MARKER: &str = "osir-local-images";

pub(crate) fn has_local_image_marker(headers: &http::HeaderMap) -> bool {
    headers
        .get(LOCAL_IMAGE_HEADER)
        .is_some_and(|value| value == LOCAL_IMAGE_MARKER)
}

#[derive(Serialize)]
pub(crate) struct GlobalImageDto {
    pub status: &'static str,
    pub reason: Option<&'static str>,
    pub selection: Option<GlobalImageSelectionDto>,
    pub candidates: Vec<ImageCandidateDto>,
}
#[derive(Serialize)]
pub(crate) struct GlobalImageSelectionDto {
    pub source: &'static str,
    pub provider_id: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub version: String,
    pub has_key: bool,
}
impl AppliedImageDto {
    pub(crate) fn summary(&self) -> GlobalImageSelectionDto {
        GlobalImageSelectionDto {
            source: "saved",
            provider_id: Some(self.provider_id.clone()),
            endpoint: "https://api.osirclaw.com/v1".into(),
            model: self.model.clone(),
            version: self.version.clone(),
            has_key: true,
        }
    }
}
#[derive(Serialize)]
pub(crate) struct ImageCandidateDto {
    pub provider_id: String,
    pub name: String,
    pub group_id: Option<i64>,
    pub masked_key: &'static str,
}
// Input-only: never derive Debug or Serialize for a secret-bearing request.
#[derive(Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum GlobalImageSave {
    Saved {
        provider_id: String,
        #[serde(default)]
        model: String,
        generation: u64,
    },
    Custom {
        endpoint: String,
        model: String,
        api_key: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomImage {
    endpoint: String,
    model: String,
    version: String,
}
// Endpoint and credential are stored together in the OS credential store.
// A modified settings record cannot send an existing secret to another host.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomSecret {
    endpoint: String,
    api_key: String,
}
fn custom_selection(db: &crate::database::Database) -> Result<Option<CustomImage>> {
    match db.get_setting(CUSTOM_KEY).map_err(|_| ImageBindingError)? {
        None => Ok(None),
        Some(value) if value == "null" => Ok(None),
        Some(value) => serde_json::from_str(&value)
            .map(Some)
            .map_err(|_| ImageBindingError),
    }
}
pub(crate) fn configured(db: &crate::database::Database) -> Result<bool> {
    Ok(custom_selection(db)?.is_some() || load_selection(db)?.is_some())
}
fn custom_endpoint(input: &str) -> Result<String> {
    let url = url::Url::parse(input).map_err(|_| ImageBindingError)?;
    if input.len() > 2048
        || input.trim() != input
        || input.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ImageBindingError);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
fn custom_service(identity: &str) -> String {
    format!("{identity}.osir.global-images.v1")
}
fn custom_secret_matches(selection: &CustomImage, secret: &CustomSecret) -> bool {
    secret.endpoint == selection.endpoint
        && custom_endpoint(&selection.endpoint).is_ok_and(|e| e == selection.endpoint)
        && !secret.api_key.is_empty()
        && secret.api_key.len() <= 8192
        && secret.api_key.trim() == secret.api_key
        && !secret.api_key.chars().any(char::is_control)
}
async fn read_custom_secret(identity: String, selection: &CustomImage) -> Result<CustomSecret> {
    use super::credentials::{CredentialBackend, NativeKeychain};
    let service = custom_service(&identity);
    let item = selection.version.clone();
    let bytes = tokio::task::spawn_blocking(move || NativeKeychain.read(&service, &item))
        .await
        .map_err(|_| ImageBindingError)?
        .map_err(|_| ImageBindingError)?
        .ok_or(ImageBindingError)?;
    let secret: CustomSecret = serde_json::from_slice(&bytes).map_err(|_| ImageBindingError)?;
    if !custom_secret_matches(selection, &secret) {
        return Err(ImageBindingError);
    }
    Ok(secret)
}
pub(crate) async fn save_custom(
    db: &crate::database::Database,
    identity: String,
    endpoint: String,
    model: String,
    api_key: String,
) -> Result<GlobalImageSelectionDto> {
    use super::credentials::{CredentialBackend, NativeKeychain};
    let endpoint = custom_endpoint(&endpoint)?;
    if !text(&model)
        || api_key.is_empty()
        || api_key.len() > 8192
        || api_key.trim() != api_key
        || api_key.chars().any(char::is_control)
    {
        return Err(ImageBindingError);
    }
    let old = custom_selection(db)?;
    let selection = CustomImage {
        endpoint: endpoint.clone(),
        model,
        version: uuid::Uuid::new_v4().to_string(),
    };
    let service = custom_service(&identity);
    let item = selection.version.clone();
    let bytes =
        serde_json::to_vec(&CustomSecret { endpoint, api_key }).map_err(|_| ImageBindingError)?;
    let service_write = service.clone();
    let item_write = item.clone();
    tokio::task::spawn_blocking(move || NativeKeychain.write(&service_write, &item_write, &bytes))
        .await
        .map_err(|_| ImageBindingError)?
        .map_err(|_| ImageBindingError)?;
    let encoded = serde_json::to_string(&selection).map_err(|_| ImageBindingError)?;
    let saved = (|| {
        let mut conn = db.conn.lock().map_err(|_| ImageBindingError)?;
        let tx = conn.transaction().map_err(|_| ImageBindingError)?;
        tx.execute(
            "INSERT OR REPLACE INTO settings (key,value) VALUES (?1,?2)",
            rusqlite::params![CUSTOM_KEY, encoded],
        )
        .map_err(|_| ImageBindingError)?;
        tx.execute("DELETE FROM settings WHERE key = ?1", [SELECTION_KEY])
            .map_err(|_| ImageBindingError)?;
        tx.commit().map_err(|_| ImageBindingError)
    })();
    let obsolete = if saved.is_err() {
        Some(item)
    } else {
        old.map(|v| v.version)
    };
    if let Some(item) = obsolete {
        let _ = tokio::task::spawn_blocking(move || NativeKeychain.remove(&service, &item)).await;
    }
    saved?;
    Ok(GlobalImageSelectionDto {
        source: "custom",
        provider_id: None,
        endpoint: selection.endpoint,
        model: selection.model,
        version: selection.version,
        has_key: true,
    })
}
pub(crate) async fn global_get(
    identity: String,
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
) -> Result<GlobalImageDto> {
    let generation = auth.status().generation;
    let groups = image_groups(auth, generation).await.ok();
    let candidates = db
        .get_all_providers("codex")
        .map_err(|_| ImageBindingError)?
        .values()
        .filter_map(|p| {
            let b = p.meta.as_ref()?.osir_binding.as_ref()?;
            saved(p, &b.binding).ok()?;
            session_matches(auth, generation, &b.binding).ok()?;
            if !eligible_image_group(groups.as_ref()?, b.binding.group_id) {
                return None;
            }
            Some(ImageCandidateDto {
                provider_id: p.id.clone(),
                name: p.name.clone(),
                group_id: b.binding.group_id,
                masked_key: "••••",
            })
        })
        .collect();
    if let Some(c) = custom_selection(db)? {
        let has_key = read_custom_secret(identity, &c).await.is_ok();
        return Ok(GlobalImageDto {
            status: if has_key { "unverified" } else { "unavailable" },
            reason: Some(if has_key {
                "自定义接口已配置，生图能力尚未验证，实际请求结果以上游返回为准"
            } else {
                "自定义图片接口密钥不可用，请重新保存"
            }),
            selection: Some(GlobalImageSelectionDto {
                source: "custom",
                provider_id: None,
                endpoint: c.endpoint,
                model: c.model,
                version: c.version,
                has_key,
            }),
            candidates,
        });
    }
    let Some(s) = load_selection(db)? else {
        return Ok(GlobalImageDto {
            status: "unconfigured",
            reason: None,
            selection: None,
            candidates,
        });
    };
    let ready = match provider_binding(db, &s.binding.provider_id) {
        Ok((p, b)) if b.binding == s.binding && b.reference == s.reference => {
            read_image_permission(auth, &p, &b, auth.status().generation)
                .await
                .is_ok()
        }
        _ => false,
    };
    let version = s.dto()?.version;
    Ok(GlobalImageDto {
        status: if ready { "unverified" } else { "unavailable" },
        reason: Some(if ready {
            "已配置标准 Images 接口，实际生图结果尚未验证；默认模型为服务端现有默认 gpt-image-2"
        } else {
            "账户或密钥不可用，或分组未开启 OpenAI 生图权限"
        }),
        selection: Some(GlobalImageSelectionDto {
            source: "saved",
            provider_id: Some(s.binding.provider_id),
            endpoint: "https://api.osirclaw.com/v1".into(),
            model: s.model,
            version,
            has_key: true,
        }),
        candidates,
    })
}
pub(crate) async fn prepare_auto_default(
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
    generation: u64,
) -> Result<Option<PreparedImageSelection>> {
    if configured(db)? {
        return Ok(None);
    }
    let groups = image_groups(auth, generation).await?;
    let providers = db
        .get_all_providers("codex")
        .map_err(|_| ImageBindingError)?;
    for p in providers
        .values()
        .filter(|p| {
            p.meta
                .as_ref()
                .and_then(|m| m.osir_binding.as_ref())
                .is_some_and(|b| {
                    eligible_image_group(&groups, b.binding.group_id)
                        && session_matches(auth, generation, &b.binding).is_ok()
                })
        })
        .take(16)
    {
        if let Ok(prepared) = prepare_selection(auth, db, &p.id, "", generation).await {
            return Ok(Some(prepared));
        }
    }
    Err(ImageBindingError)
}
pub(crate) async fn custom_target(
    db: &crate::database::Database,
    identity: String,
    endpoint: &str,
    body: &mut serde_json::Value,
) -> Result<Option<crate::mode::stack::StackTarget>> {
    let Some(c) = custom_selection(db)? else {
        return Ok(None);
    };
    if !matches!(endpoint, "/images/generations" | "/images/edits") || !text(&c.model) {
        return Err(ImageBindingError);
    }
    let secret = read_custom_secret(identity, &c).await?;
    if custom_selection(db)?.is_none_or(|now| {
        now.version != c.version || now.endpoint != c.endpoint || now.model != c.model
    }) {
        return Err(ImageBindingError);
    }
    let object = body.as_object_mut().ok_or(ImageBindingError)?;
    if object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|m| m.starts_with("ccs-"))
    {
        return Err(ImageBindingError);
    }
    object.insert("model".into(), serde_json::json!(c.model));
    let mut config = toml_edit::DocumentMut::new();
    config["model_provider"] = toml_edit::value("image_custom");
    config["model_providers"]["image_custom"]["base_url"] = toml_edit::value(c.endpoint);
    config["model_providers"]["image_custom"]["wire_api"] = toml_edit::value("responses");
    config["model_providers"]["image_custom"]["requires_openai_auth"] = toml_edit::value(true);
    let provider = Provider::with_id(
        format!("osir-image-{}", c.version),
        "自定义图片接口".into(),
        serde_json::json!({"config":config.to_string(), "auth":{"OPENAI_API_KEY":secret.api_key}}),
        None,
    );
    Ok(Some(crate::mode::stack::StackTarget {
        provider,
        upstream_model: c.model.clone(),
        original_model: c.model,
    }))
}

#[derive(Serialize)]
pub(crate) struct ImageModelDto {
    pub id: String,
    pub generations: bool,
    pub edits: bool,
}
#[derive(Serialize)]
pub(crate) struct ImageContractDto {
    pub provider_id: String,
    pub generation: u64,
    pub revision: String,
    pub protocol: &'static str,
    pub expires_at: String,
    pub models: Vec<ImageModelDto>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppliedImageDto {
    pub provider_id: String,
    pub model: String,
    pub revision: String,
    /// Unique local binding version, not the server capability revision.
    pub version: String,
}
// Persist only exact native authority references and the user-selected model.
// Never deserialize this from IPC, and never include it in a command response.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliedImage {
    binding: KeyBinding,
    reference: super::key_vault::KeyReference,
    model: String,
    revision: String,
    #[serde(default)]
    version: String,
}
impl AppliedImage {
    fn dto(&self) -> Result<AppliedImageDto> {
        // Old records remain explicitly removable. Their fingerprint includes the
        // native key/reference; every new bind receives a fresh UUID, preventing ABA.
        let version = if self.version.is_empty() {
            let bytes = serde_json::to_vec(self).map_err(|_| ImageBindingError)?;
            format!("legacy-{:x}", sha2::Sha256::digest(bytes))
        } else {
            self.version.clone()
        };
        Ok(AppliedImageDto {
            provider_id: self.binding.provider_id.clone(),
            model: self.model.clone(),
            revision: self.revision.clone(),
            version,
        })
    }
}
pub(crate) struct PreparedImageSelection {
    selection: AppliedImage,
    valid_until: Instant,
}

fn provider_binding(
    db: &crate::database::Database,
    id: &str,
) -> Result<(Provider, ManagedBinding)> {
    let provider = db
        .get_provider_by_id(id, "codex")
        .map_err(|_| ImageBindingError)?
        .ok_or(ImageBindingError)?;
    let binding = provider
        .meta
        .as_ref()
        .and_then(|m| m.osir_binding.as_ref())
        .ok_or(ImageBindingError)?;
    let binding = saved(&provider, &binding.binding)?;
    Ok((provider, binding))
}
fn session_matches(
    auth: &super::session::AuthManager,
    generation: u64,
    binding: &KeyBinding,
) -> Result<()> {
    let status = auth.status();
    if !status.connected
        || status.generation != generation
        || status.profile.as_ref().map(|p| p.id.to_string()).as_deref()
            != Some(binding.account_id.as_str())
    {
        return Err(ImageBindingError);
    }
    Ok(())
}
// One native flight across keys (bounded transport concurrency), at most 64
// metadata entries. Successful reuse is <=10s AND <=server expiry (max 60s).
// Revocation/group permission changes can therefore lag by at most 10s; local
// logout, provider deletion/reference rotation are checked on every admission.
// Failures suppress identical bursts for 2s. No timers, tasks or background polls.
const IMAGE_CACHE_TTL: Duration = Duration::from_secs(10);
const IMAGE_FAILURE_TTL: Duration = Duration::from_secs(2);
const IMAGE_CACHE_LIMIT: usize = 64;
type ContractCache = std::collections::BTreeMap<String, (Instant, Option<OpenAiImageContract>)>;
static CONTRACT_CACHE: std::sync::OnceLock<tokio::sync::Mutex<ContractCache>> =
    std::sync::OnceLock::new();
static CONTRACT_READERS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();

type PermissionCache = std::collections::BTreeMap<String, (Instant, bool)>;
type GroupCache = std::collections::BTreeMap<(usize, u64), (Instant, Option<serde_json::Value>)>;
static GROUP_CACHE: std::sync::OnceLock<tokio::sync::Mutex<GroupCache>> =
    std::sync::OnceLock::new();
async fn image_groups(
    auth: &super::session::AuthManager,
    generation: u64,
) -> Result<serde_json::Value> {
    if !auth.status().connected || auth.status().generation != generation {
        return Err(ImageBindingError);
    }
    let key = (
        auth as *const super::session::AuthManager as usize,
        generation,
    );
    let mut cache = tokio::time::timeout(
        Duration::from_secs(35),
        GROUP_CACHE
            .get_or_init(|| tokio::sync::Mutex::new(GroupCache::new()))
            .lock(),
    )
    .await
    .map_err(|_| ImageBindingError)?;
    cache.retain(|_, (until, _)| *until > Instant::now());
    let result = if let Some((_, groups)) = cache.get(&key) {
        groups.clone().ok_or(ImageBindingError)
    } else {
        let result = auth
            .read_account::<serde_json::Value>(super::client::AccountResource::Groups, &[])
            .await
            .map_err(|_| ImageBindingError)
            .and_then(|reply| {
                if reply.generation == generation && reply.data.is_array() {
                    Ok(reply.data)
                } else {
                    Err(ImageBindingError)
                }
            });
        if cache.len() >= IMAGE_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(
            key,
            (
                Instant::now()
                    + if result.is_ok() {
                        IMAGE_CACHE_TTL
                    } else {
                        IMAGE_FAILURE_TTL
                    },
                result.as_ref().ok().cloned(),
            ),
        );
        result
    };
    if !auth.status().connected || auth.status().generation != generation {
        return Err(ImageBindingError);
    }
    result
}
static PERMISSION_CACHE: std::sync::OnceLock<tokio::sync::Mutex<PermissionCache>> =
    std::sync::OnceLock::new();

fn standard_image_model(model: &str) -> Result<String> {
    let model = if model.is_empty() {
        "gpt-image-2"
    } else {
        model
    };
    if !text(model) || model.starts_with("ccs-") {
        return Err(ImageBindingError);
    }
    Ok(model.into())
}
fn eligible_image_group(groups: &serde_json::Value, group_id: Option<i64>) -> bool {
    groups.as_array().is_some_and(|groups| {
        groups.iter().any(|g| {
            g.get("id")
                .and_then(serde_json::Value::as_i64)
                .is_some_and(|id| id > 0 && Some(id) == group_id)
                && g.get("status").and_then(serde_json::Value::as_str) == Some("active")
                && g.get("platform").and_then(serde_json::Value::as_str) == Some("openai")
                && g.get("allow_image_generation")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        })
    })
}
async fn cached_permission(
    cache: &tokio::sync::Mutex<PermissionCache>,
    key: String,
    fetch: impl std::future::Future<Output = Result<()>>,
) -> Result<Instant> {
    let mut cache = tokio::time::timeout(Duration::from_secs(35), cache.lock())
        .await
        .map_err(|_| ImageBindingError)?;
    let now = Instant::now();
    cache.retain(|_, (until, _)| *until > now);
    if let Some((until, ok)) = cache.get(&key) {
        return if *ok {
            Ok(*until)
        } else {
            Err(ImageBindingError)
        };
    }
    let ok = fetch.await.is_ok();
    let until = Instant::now()
        + if ok {
            IMAGE_CACHE_TTL
        } else {
            IMAGE_FAILURE_TTL
        };
    if cache.len() >= IMAGE_CACHE_LIMIT {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (until, _))| *until)
            .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(key, (until, ok));
    if ok {
        Ok(until)
    } else {
        Err(ImageBindingError)
    }
}
async fn read_image_permission(
    auth: &super::session::AuthManager,
    provider: &Provider,
    binding: &ManagedBinding,
    generation: u64,
) -> Result<Instant> {
    session_matches(auth, generation, &binding.binding)?;
    saved(provider, &binding.binding)?;
    let _reader = CONTRACT_READERS
        .get_or_init(|| tokio::sync::Semaphore::new(64))
        .try_acquire()
        .map_err(|_| ImageBindingError)?;
    let key = serde_json::to_string(&(
        auth as *const super::session::AuthManager as usize,
        generation,
        &binding.binding,
        &binding.reference,
    ))
    .map_err(|_| ImageBindingError)?;
    let cache = PERMISSION_CACHE.get_or_init(|| tokio::sync::Mutex::new(PermissionCache::new()));
    let result = cached_permission(cache, key, async {
        let reply = auth
            .read_key(binding.binding.key_id, generation)
            .await
            .map_err(|_| ImageBindingError)?;
        if reply.generation != generation
            || reply.data.id != binding.binding.key_id
            || reply.data.group_id != binding.binding.group_id
        {
            return Err(ImageBindingError);
        }
        // Active status and expiry are validated natively; secret never leaves Rust.
        let current_secret = reply
            .data
            .into_active_secret()
            .map_err(|_| ImageBindingError)?;
        let groups = image_groups(auth, generation).await?;
        if !eligible_image_group(&groups, binding.binding.group_id) {
            return Err(ImageBindingError);
        }
        let binding = binding.clone();
        tokio::task::spawn_blocking(move || {
            let saved_secret = managed::vault()
                .map_err(|_| ImageBindingError)?
                .load(&binding.reference, &binding.binding)
                .map_err(|_| ImageBindingError)?
                .ok_or(ImageBindingError)?;
            if saved_secret.expose() != current_secret.expose() {
                return Err(ImageBindingError);
            }
            Ok(())
        })
        .await
        .map_err(|_| ImageBindingError)??;
        Ok(())
    })
    .await;
    session_matches(auth, generation, &binding.binding)?;
    result
}

async fn read_contract(
    auth: &super::session::AuthManager,
    provider: &Provider,
    binding: &ManagedBinding,
    generation: u64,
) -> Result<OpenAiImageContract> {
    session_matches(auth, generation, &binding.binding)?;
    saved(provider, &binding.binding)?;
    // Bound gate waiters as well as cache size; overload fails locally instead
    // of spawning unbounded pending account requests.
    let _reader = CONTRACT_READERS
        .get_or_init(|| tokio::sync::Semaphore::new(64))
        .try_acquire()
        .map_err(|_| ImageBindingError)?;
    // No credentials or untrusted display text in this native-only cache key.
    let key = serde_json::to_string(&(
        auth as *const super::session::AuthManager as usize,
        generation,
        &binding.binding,
        &binding.reference,
    ))
    .map_err(|_| ImageBindingError)?;
    let cache = CONTRACT_CACHE.get_or_init(|| tokio::sync::Mutex::new(ContractCache::new()));
    cached_contract(
        cache,
        key,
        || session_matches(auth, generation, &binding.binding),
        fetch_contract(auth, provider, binding, generation),
    )
    .await
}

// Keep one gate around lookup + fetch + insert so followers reuse both success
// and failure. A local cache can exercise the same coordinator without accounts.
async fn cached_contract(
    cache: &tokio::sync::Mutex<ContractCache>,
    key: String,
    validate: impl FnOnce() -> Result<()>,
    fetch: impl std::future::Future<Output = Result<OpenAiImageContract>>,
) -> Result<OpenAiImageContract> {
    let mut cache = tokio::time::timeout(Duration::from_secs(35), cache.lock())
        .await
        .map_err(|_| ImageBindingError)?;
    validate()?;
    let now = Instant::now();
    cache.retain(|_, (deadline, _)| *deadline > now);
    if let Some((_, contract)) = cache.get(&key) {
        return contract.clone().ok_or(ImageBindingError);
    }
    // Holding the async gate through the wire request makes simultaneous readers
    // of this exact key join its result rather than issue a second cloud request.
    let result = fetch.await.map(|mut contract| {
        contract.valid_until = contract.valid_until.min(Instant::now() + IMAGE_CACHE_TTL);
        contract
    });
    let entry = match &result {
        Ok(contract) => (contract.valid_until, Some(contract.clone())),
        Err(_) => (Instant::now() + IMAGE_FAILURE_TTL, None),
    };
    if cache.len() >= IMAGE_CACHE_LIMIT {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (deadline, _))| *deadline)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(key, entry);
    result
}

async fn fetch_contract(
    auth: &super::session::AuthManager,
    provider: &Provider,
    binding: &ManagedBinding,
    generation: u64,
) -> Result<OpenAiImageContract> {
    session_matches(auth, generation, &binding.binding)?;
    let b = &binding.binding;
    let reply = auth
        .read_image_contract(b.key_id, generation, &b.account_id, &b.account_origin)
        .await
        .map_err(|_| ImageBindingError)?;
    if reply.generation != generation {
        return Err(ImageBindingError);
    }
    let contract = decode_openai_image_contract(reply.data, provider, b)?;
    session_matches(auth, generation, b)?;
    Ok(contract)
}

pub(crate) async fn preview(
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
    provider_id: &str,
    generation: u64,
) -> Result<ImageContractDto> {
    let (provider, binding) = provider_binding(db, provider_id)?;
    let valid_until = read_image_permission(auth, &provider, &binding, generation).await?;
    let (_, latest) = provider_binding(db, provider_id)?;
    if !same(&binding, &latest) {
        return Err(ImageBindingError);
    }
    Ok(ImageContractDto {
        provider_id: provider_id.into(),
        generation,
        revision: "group-permission-v1".into(),
        protocol: "openai-images-v1",
        expires_at: (chrono::Utc::now()
            + chrono::Duration::from_std(valid_until.saturating_duration_since(Instant::now()))
                .map_err(|_| ImageBindingError)?)
        .to_rfc3339(),
        // Group permission does not attest a model catalog or operation support.
        models: vec![],
    })
}

fn select_image_model(models: &[WireImageModel], requested: &str) -> Result<String> {
    models
        .iter()
        .find(|m| (requested.is_empty() || m.id == requested) && (m.generations || m.edits))
        .map(|m| m.id.clone())
        .ok_or(ImageBindingError)
}

pub(crate) async fn prepare_selection(
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
    provider_id: &str,
    model: &str,
    generation: u64,
) -> Result<PreparedImageSelection> {
    let (provider, owner) = provider_binding(db, provider_id)?;
    let valid_until = read_image_permission(auth, &provider, &owner, generation).await?;
    // Actual existing server default, not a fabricated capability catalog.
    let model = standard_image_model(model)?;
    Ok(PreparedImageSelection {
        selection: AppliedImage {
            binding: owner.binding,
            reference: owner.reference,
            model,
            revision: "group-permission-v1".into(),
            version: uuid::Uuid::new_v4().to_string(),
        },
        valid_until,
    })
}

/// Caller holds the existing Codex switch lock. A failed write keeps the previous
/// selection. It is intentionally independent of chat default/Stack membership.
pub(crate) fn commit_selection(
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
    generation: u64,
    prepared: PreparedImageSelection,
) -> Result<AppliedImageDto> {
    let selection = prepared.selection;
    let (_, current) = provider_binding(db, &selection.binding.provider_id)?;
    if current.binding != selection.binding
        || current.reference != selection.reference
        || Instant::now() >= prepared.valid_until
    {
        return Err(ImageBindingError);
    }
    session_matches(auth, generation, &selection.binding)?;
    let encoded = serde_json::to_string(&selection).map_err(|_| ImageBindingError)?;
    auth.with_current_image_account(
        generation,
        &selection.binding.account_id,
        &selection.binding.account_origin,
        || {
            let mut conn = db.conn.lock().map_err(|_| ImageBindingError)?;
            let tx = conn.transaction().map_err(|_| ImageBindingError)?;
            tx.execute(
                "INSERT OR REPLACE INTO settings (key,value) VALUES (?1,?2)",
                rusqlite::params![SELECTION_KEY, encoded],
            )
            .map_err(|_| ImageBindingError)?;
            tx.execute("DELETE FROM settings WHERE key = ?1", [CUSTOM_KEY])
                .map_err(|_| ImageBindingError)?;
            tx.commit().map_err(|_| ImageBindingError)
        },
    )
    .map_err(|_| ImageBindingError)??;
    selection.dto()
}
fn load_selection(db: &crate::database::Database) -> Result<Option<AppliedImage>> {
    match db
        .get_setting(SELECTION_KEY)
        .map_err(|_| ImageBindingError)?
    {
        None => Ok(None),
        Some(value) if value == "null" => Ok(None),
        Some(value) => serde_json::from_str(&value)
            .map(Some)
            .map_err(|_| ImageBindingError),
    }
}
pub(crate) fn current(db: &crate::database::Database) -> Result<Option<AppliedImageDto>> {
    load_selection(db)?
        .map(|selection| selection.dto())
        .transpose()
}
/// Caller holds Codex's switch lock. The session mutex remains held through
/// compare-and-delete, so logout cannot interleave between auth check and deletion.
pub(crate) fn unbind(
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
    generation: u64,
    expected: &AppliedImageDto,
) -> Result<()> {
    let encoded = db
        .get_setting(SELECTION_KEY)
        .map_err(|_| ImageBindingError)?
        .ok_or(ImageBindingError)?;
    let selection: AppliedImage = serde_json::from_str(&encoded).map_err(|_| ImageBindingError)?;
    if selection.dto()? != *expected {
        return Err(ImageBindingError);
    }
    auth.with_current_image_account(
        generation,
        &selection.binding.account_id,
        &selection.binding.account_origin,
        || delete_selection(db, &encoded),
    )
    .map_err(|_| ImageBindingError)?
}

// SQL CAS also protects against import/settings writers outside the Codex
// switch lock. Never clear a replacement record, even after an ABA rebind.
fn delete_selection(db: &crate::database::Database, encoded: &str) -> Result<()> {
    let conn = db.conn.lock().map_err(|_| ImageBindingError)?;
    let changed = conn
        .execute(
            "DELETE FROM settings WHERE key = ?1 AND value = ?2",
            rusqlite::params![SELECTION_KEY, encoded],
        )
        .map_err(|_| ImageBindingError)?;
    if changed != 1 {
        return Err(ImageBindingError);
    }
    Ok(())
}

/// Hook ONLY from existing standalone Images handlers, before ordinary Stack
/// resolution. Some target uses existing RequestContext single-provider/no-failover
/// forwarding. Error is terminal: never retry against the chat default.
/// Does not send requests, materialize secrets, add extension headers, or log bodies.
pub(crate) async fn resolve_request(
    auth: &super::session::AuthManager,
    db: &crate::database::Database,
    endpoint: &str,
    body: &mut serde_json::Value,
) -> Result<Option<crate::mode::stack::StackTarget>> {
    if !matches!(endpoint, "/images/generations" | "/images/edits") {
        return Err(ImageBindingError);
    }
    let Some(selection) = load_selection(db)? else {
        return Ok(None);
    };
    let (provider, owner) = provider_binding(db, &selection.binding.provider_id)?;
    if owner.binding != selection.binding || owner.reference != selection.reference {
        return Err(ImageBindingError);
    }
    let generation = auth.status().generation;
    read_image_permission(auth, &provider, &owner, generation).await?;
    // Recheck after HTTP before constructing the exact provider snapshot.
    let (provider, latest) = provider_binding(db, &selection.binding.provider_id)?;
    if !same(&owner, &latest)
        || load_selection(db)?
            .as_ref()
            .map(|s| (&s.binding, &s.reference, &s.model, &s.revision, &s.version))
            != Some((
                &selection.binding,
                &selection.reference,
                &selection.model,
                &selection.revision,
                &selection.version,
            ))
    {
        return Err(ImageBindingError);
    }
    session_matches(auth, generation, &selection.binding)?;
    let object = body.as_object_mut().ok_or(ImageBindingError)?;
    // Reject explicit Stack identities: they belong to chat selection and cannot
    // override the independent image binding or silently charge a different key.
    if object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|m| m.starts_with("ccs-"))
    {
        return Err(ImageBindingError);
    }
    object.insert(
        "model".into(),
        serde_json::Value::String(selection.model.clone()),
    );
    Ok(Some(crate::mode::stack::StackTarget {
        provider,
        upstream_model: selection.model.clone(),
        original_model: selection.model,
    }))
}

fn text(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn saved(provider: &Provider, account: &KeyBinding) -> Result<ManagedBinding> {
    managed::validate_managed_provider_shape(&AppType::Codex, provider)
        .map_err(|_| ImageBindingError)?;
    let binding = provider
        .meta
        .as_ref()
        .and_then(|m| m.osir_binding.as_ref())
        .ok_or(ImageBindingError)?;
    if &binding.binding != account {
        return Err(ImageBindingError);
    }
    Ok(binding.clone())
}
fn same(a: &ManagedBinding, b: &ManagedBinding) -> bool {
    // Display-only masked_key is never authority and never copied into the IPC DTO.
    a.source == b.source && a.binding == b.binding && a.reference == b.reference
}

impl VerifiedImageContract {
    /// TRUST BOUNDARY: call ONLY after native verification of current account,
    /// active/unexpired owned key, exact group, actual catalog capabilities and
    /// the fixed protocol. Never feed renderer claims, name-derived flags, or a
    /// plain /models response here. This constructor checks consistency, not
    /// server authenticity. No current API adapter is implemented or implied.
    pub(crate) fn from_verified_native(
        provider: &Provider,
        current_key: &KeyBinding,
        revision: String,
        protocol: ImageProtocol,
        models: Vec<VerifiedImageModel>,
        lifetime: Duration,
    ) -> Result<Self> {
        let owner = saved(provider, current_key)?;
        if !text(&revision)
            || models.is_empty()
            || models.len() > 4096
            || lifetime.is_zero()
            || lifetime > Duration::from_secs(60)
        {
            return Err(ImageBindingError);
        }
        let mut ids = std::collections::HashSet::new();
        for model in &models {
            if !text(&model.id) || !(model.generations || model.edits) || !ids.insert(&model.id) {
                return Err(ImageBindingError);
            }
        }
        Ok(Self {
            owner,
            revision,
            models,
            protocol,
            valid_until: Instant::now() + lifetime,
        })
    }
    fn check(
        &self,
        current: &ManagedBinding,
        model: &str,
        operation: ImageOperation,
    ) -> Result<()> {
        if Instant::now() >= self.valid_until || !same(&self.owner, current) {
            return Err(ImageBindingError);
        }
        let entry = self
            .models
            .iter()
            .find(|m| m.id == model)
            .ok_or(ImageBindingError)?;
        if match operation {
            ImageOperation::Generate => entry.generations,
            ImageOperation::Edit => entry.edits,
        } {
            Ok(())
        } else {
            Err(ImageBindingError)
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImageOperation {
    Generate,
    Edit,
}

// Native state only. Never contains a secret, never accepted over IPC.
pub(crate) struct ImageBinding {
    owner: ManagedBinding,
    model: String,
    operation: ImageOperation,
    revision: String,
    protocol: ImageProtocol,
}

/// Explicit allowlist projection: no credentials, vault reference, account identity,
/// masked_key (untrusted display text), headers or materialized provider settings.
#[derive(Serialize)]
pub(crate) struct ImageBindingDto {
    pub provider_id: String,
    pub model: String,
    pub operation: ImageOperation,
}

impl ImageBinding {
    pub(crate) fn select(
        provider: &Provider,
        current_key: &KeyBinding,
        contract: Option<&VerifiedImageContract>,
        selected_model: &str,
        operation: ImageOperation,
    ) -> Result<Self> {
        let contract = contract.ok_or(ImageBindingError)?;
        let owner = saved(provider, current_key)?;
        contract.check(&owner, selected_model, operation)?;
        Ok(Self {
            owner,
            model: selected_model.into(),
            operation,
            revision: contract.revision.clone(),
            protocol: contract.protocol,
        })
    }
    /// Re-read provider/key/contract under the caller's native write/request lock.
    /// Deletion, key rotation, account/group changes or catalog removal fail closed.
    pub(crate) fn validate(
        &self,
        provider: Option<&Provider>,
        current_key: &KeyBinding,
        contract: Option<&VerifiedImageContract>,
    ) -> Result<()> {
        let current = saved(provider.ok_or(ImageBindingError)?, current_key)?;
        let contract = contract.ok_or(ImageBindingError)?;
        if !same(&self.owner, &current)
            || self.revision != contract.revision
            || self.protocol != contract.protocol
        {
            return Err(ImageBindingError);
        }
        contract.check(&current, &self.model, self.operation)
    }
    pub(crate) fn dto(&self) -> ImageBindingDto {
        ImageBindingDto {
            provider_id: self.owner.binding.provider_id.clone(),
            model: self.model.clone(),
            operation: self.operation,
        }
    }
    /// Fixed names and marker, chosen model only. No caller-supplied header map.
    /// Request construction must validate() again and resolve the existing native
    /// ManagedBinding in the vault; this method never materializes credentials.
    pub(crate) fn headers(
        &self,
        provider: Option<&Provider>,
        current_key: &KeyBinding,
        contract: Option<&VerifiedImageContract>,
    ) -> Result<[(&'static str, String); 2]> {
        self.validate(provider, current_key, contract)?;
        Ok([
            (
                "x-openai-actor-authorization",
                "local-image-extension".into(),
            ),
            ("x-osir-codex-image-model", self.model.clone()),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::osir::managed::test_support::Fixture;
    use serde_json::json;
    #[test]
    fn standard_images_use_explicit_group_permission_and_real_server_default() {
        let group = json!({"id":3,"status":"active","platform":"openai","allow_image_generation":true,"name":"arbitrary"});
        assert!(eligible_image_group(&json!([group.clone()]), Some(3)));
        assert!(!eligible_image_group(&json!([group.clone()]), Some(4)));
        for (field, value) in [
            ("status", json!("disabled")),
            ("platform", json!("grok")),
            ("allow_image_generation", json!(false)),
        ] {
            let mut changed = group.clone();
            changed[field] = value;
            assert!(!eligible_image_group(&json!([changed]), Some(3)));
        }
        let mut missing = group;
        missing
            .as_object_mut()
            .unwrap()
            .remove("allow_image_generation");
        assert!(!eligible_image_group(&json!([missing]), Some(3)));
        assert_eq!(standard_image_model("").unwrap(), "gpt-image-2");
        assert_eq!(standard_image_model("chosen").unwrap(), "chosen");
        assert!(standard_image_model("ccs-other").is_err());
    }
    #[tokio::test]
    async fn standard_permission_cache_singleflight_and_failure_cooldown() {
        let cache = tokio::sync::Mutex::new(PermissionCache::new());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let fetch = || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(())
        };
        let (a, b) = tokio::join!(
            cached_permission(&cache, "same".into(), fetch()),
            cached_permission(&cache, "same".into(), fetch())
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        for _ in 0..2 {
            assert!(cached_permission(&cache, "failure".into(), async {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(ImageBindingError)
            })
            .await
            .is_err());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(cached_permission(&cache, "new-generation".into(), fetch())
            .await
            .is_ok());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
    #[test]
    fn global_custom_endpoint_secret_binding_and_input_boundaries() {
        for endpoint in [
            "http://example.com/v1",
            "https://user:pass@example.com/v1",
            "https://example.com/v1?q=1",
            "https://example.com/v1#fragment",
        ] {
            assert!(custom_endpoint(endpoint).is_err());
        }
        let endpoint = custom_endpoint("https://example.com/v1/").unwrap();
        assert_eq!(endpoint, "https://example.com/v1");
        let selection = CustomImage {
            endpoint: endpoint.clone(),
            model: "image-model".into(),
            version: "fixture".into(),
        };
        let mut secret = CustomSecret {
            endpoint,
            api_key: "fixture-custom-secret".into(),
        };
        assert!(custom_secret_matches(&selection, &secret));
        secret.endpoint = "https://other.example/v1".into();
        assert!(!custom_secret_matches(&selection, &secret));
        assert!(!serde_json::to_string(&selection)
            .unwrap()
            .contains("fixture-custom-secret"));
        assert!(serde_json::from_value::<GlobalImageSave>(
            json!({"source":"saved","provider_id":"p","generation":1})
        )
        .is_ok());
        assert!(serde_json::from_value::<GlobalImageSave>(json!({"source":"saved","provider_id":"p","generation":1,"endpoint":"https://other.example","api_key":"fixture"})).is_err());
    }

    #[test]
    fn global_custom_configuration_and_default_model_are_independent() {
        let db = crate::database::Database::memory().unwrap();
        assert!(!configured(&db).unwrap());
        let selection = CustomImage {
            endpoint: "https://example.com/v1".into(),
            model: "image-model".into(),
            version: "fixture".into(),
        };
        db.set_setting(CUSTOM_KEY, &serde_json::to_string(&selection).unwrap())
            .unwrap();
        assert!(configured(&db).unwrap());
        assert!(load_selection(&db).unwrap().is_none());
        assert_eq!(custom_selection(&db).unwrap().unwrap().model, "image-model");
        let models = vec![
            WireImageModel {
                id: "unavailable".into(),
                generations: false,
                edits: false,
            },
            WireImageModel {
                id: "first".into(),
                generations: true,
                edits: false,
            },
            WireImageModel {
                id: "chosen".into(),
                generations: true,
                edits: true,
            },
        ];
        assert_eq!(select_image_model(&models, "").unwrap(), "first");
        assert_eq!(select_image_model(&models, "chosen").unwrap(), "chosen");
        assert!(select_image_model(&models, "missing").is_err());
        assert!(select_image_model(&models, "unavailable").is_err());
    }

    #[test]
    fn local_image_marker_requires_exact_value() {
        let mut headers = http::HeaderMap::new();
        assert!(!has_local_image_marker(&headers));
        headers.insert(LOCAL_IMAGE_HEADER, http::HeaderValue::from_static("other"));
        assert!(!has_local_image_marker(&headers));
        headers.insert(
            LOCAL_IMAGE_HEADER,
            http::HeaderValue::from_static(LOCAL_IMAGE_MARKER),
        );
        assert!(has_local_image_marker(&headers));
    }

    fn fixture() -> Fixture {
        Fixture::new(
            &AppType::Codex,
            json!({"auth":{},"config":r#"model = "chat-fixture"
model_provider = "osir"
[model_providers.osir]
name = "OSIR"
base_url = "https://api.osirclaw.com/v1"
wire_api = "responses"
"#}),
            "never-return-fixture-secret",
        )
    }
    fn key(p: &Provider) -> KeyBinding {
        p.meta
            .as_ref()
            .unwrap()
            .osir_binding
            .as_ref()
            .unwrap()
            .binding
            .clone()
    }
    fn contract(p: &Provider, id: &str) -> VerifiedImageContract {
        VerifiedImageContract::from_verified_native(
            p,
            &key(p),
            "fixture-revision".into(),
            ImageProtocol::OsirCodexNativeV1,
            vec![VerifiedImageModel {
                id: id.into(),
                generations: true,
                edits: false,
            }],
            Duration::from_secs(60),
        )
        .unwrap()
    }
    fn wire_contract(lifetime: Duration) -> OpenAiImageContract {
        OpenAiImageContract {
            wire: WireImageContract {
                account_id: 7,
                key_id: 1,
                group_id: 1,
                revision: "fixture".into(),
                protocol: "openai-images-v1".into(),
                expires_at: "fixture-unused-by-cache".into(),
                models: vec![WireImageModel {
                    id: "image".into(),
                    generations: true,
                    edits: false,
                }],
            },
            valid_until: Instant::now() + lifetime,
        }
    }

    #[tokio::test]
    async fn image_cache_singleflight_reuses_success_and_revalidates_hits() {
        let cache = tokio::sync::Mutex::new(ContractCache::new());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let fetch = || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(wire_contract(Duration::from_secs(60)))
        };
        let (a, b) = tokio::join!(
            cached_contract(&cache, "same".into(), || Ok(()), fetch()),
            cached_contract(&cache, "same".into(), || Ok(()), fetch()),
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(a.unwrap().valid_until <= Instant::now() + IMAGE_CACHE_TTL);
        // Logout/generation validation remains mandatory even on a warm hit.
        assert!(
            cached_contract(&cache, "same".into(), || Err(ImageBindingError), fetch())
                .await
                .is_err()
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn image_cache_failure_cooldown_expiry_and_server_deadline() {
        let cache = tokio::sync::Mutex::new(ContractCache::new());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..2 {
            assert!(cached_contract(&cache, "key".into(), || Ok(()), async {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(ImageBindingError)
            })
            .await
            .is_err());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        {
            let mut entries = cache.lock().await;
            assert!(entries["key"].0 <= Instant::now() + IMAGE_FAILURE_TTL);
            entries.get_mut("key").unwrap().0 = Instant::now();
        }
        let contract = wire_contract(Duration::from_secs(1));
        let deadline = contract.valid_until;
        let result = cached_contract(&cache, "key".into(), || Ok(()), async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(contract)
        })
        .await
        .unwrap();
        assert_eq!(result.valid_until, deadline);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Distinct authority/generation keys cannot borrow an existing result.
        assert!(
            cached_contract(&cache, "rotated-key".into(), || Ok(()), async {
                Err(ImageBindingError)
            })
            .await
            .is_err()
        );
    }

    #[test]
    fn image_unbind_sql_cas_preserves_replacement_and_rejects_aba_version() {
        let f = fixture();
        let owner = f
            .provider
            .meta
            .as_ref()
            .unwrap()
            .osir_binding
            .as_ref()
            .unwrap();
        let mut selection = AppliedImage {
            binding: owner.binding.clone(),
            reference: owner.reference.clone(),
            model: "image".into(),
            revision: "revision".into(),
            version: "old".into(),
        };
        let old_dto = selection.dto().unwrap();
        let old = serde_json::to_string(&selection).unwrap();
        selection.version = "new".into();
        assert!(selection.dto().unwrap() != old_dto);
        let replacement = serde_json::to_string(&selection).unwrap();
        let db = crate::database::Database::memory().unwrap();
        db.set_setting(SELECTION_KEY, &replacement).unwrap();
        assert!(delete_selection(&db, &old).is_err());
        assert_eq!(
            db.get_setting(SELECTION_KEY).unwrap().as_deref(),
            Some(replacement.as_str())
        );
        delete_selection(&db, &replacement).unwrap();
        assert!(db.get_setting(SELECTION_KEY).unwrap().is_none());
        assert!(delete_selection(&db, &replacement).is_err());
    }

    #[test]
    fn explicit_contract_only_and_dto_is_allowlisted() {
        let f = fixture();
        let k = key(&f.provider);
        let c = contract(&f.provider, "opaque-catalog-id");
        assert!(ImageBinding::select(
            &f.provider,
            &k,
            None,
            "opaque-catalog-id",
            ImageOperation::Generate
        )
        .is_err());
        assert!(ImageBinding::select(
            &f.provider,
            &k,
            Some(&c),
            "unknown",
            ImageOperation::Generate
        )
        .is_err());
        assert!(ImageBinding::select(
            &f.provider,
            &k,
            Some(&c),
            "opaque-catalog-id",
            ImageOperation::Edit
        )
        .is_err());
        let b = ImageBinding::select(
            &f.provider,
            &k,
            Some(&c),
            "opaque-catalog-id",
            ImageOperation::Generate,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(b.dto()).unwrap(),
            json!({"provider_id":f.provider.id,"model":"opaque-catalog-id","operation":"generate"})
        );
        let h = b.headers(Some(&f.provider), &k, Some(&c)).unwrap();
        assert_eq!(
            h[1],
            ("x-osir-codex-image-model", "opaque-catalog-id".into())
        );
    }

    fn model(id: &str, generations: bool, edits: bool) -> VerifiedImageModel {
        VerifiedImageModel {
            id: id.into(),
            generations,
            edits,
        }
    }

    fn try_contract(
        p: &Provider,
        k: &KeyBinding,
        revision: &str,
        models: Vec<VerifiedImageModel>,
        ttl: Duration,
    ) -> Result<VerifiedImageContract> {
        VerifiedImageContract::from_verified_native(
            p,
            k,
            revision.into(),
            ImageProtocol::OsirCodexNativeV1,
            models,
            ttl,
        )
    }

    #[test]
    fn empty_duplicate_operationless_and_oversized_catalogs_are_rejected() {
        let f = fixture();
        let k = key(&f.provider);
        for models in [
            vec![],
            vec![model("", true, false)],
            vec![model("same", true, false), model("same", false, true)],
            vec![model("no-operation", false, false)],
            (0..4097)
                .map(|i| model(&format!("model-{i}"), true, false))
                .collect(),
        ] {
            assert!(
                try_contract(&f.provider, &k, "revision", models, Duration::from_secs(60)).is_err()
            );
        }
        // The catalog limit is inclusive; rejection above is not an arbitrary small limit.
        assert!(try_contract(
            &f.provider,
            &k,
            "revision",
            (0..4096)
                .map(|i| model(&format!("model-{i}"), true, false))
                .collect(),
            Duration::from_secs(60),
        )
        .is_ok());
    }

    fn invalid_text_values() -> Vec<String> {
        let mut values = vec![
            String::new(),
            " ".into(),
            " leading".into(),
            "trailing ".into(),
            "模型".into(),
            "x".repeat(257),
        ];
        // Every ASCII control, including embedded CR/LF, TAB, NUL and DEL.
        for byte in (0u8..=31).chain(std::iter::once(127)) {
            values.push(format!("before{}after", char::from(byte)));
        }
        values
    }

    #[test]
    fn unsafe_header_model_values_are_rejected_without_truncation() {
        let f = fixture();
        let k = key(&f.provider);
        let c = contract(&f.provider, "valid-model");
        for id in invalid_text_values() {
            assert!(try_contract(
                &f.provider,
                &k,
                "revision",
                vec![model(&id, true, false)],
                Duration::from_secs(60),
            )
            .is_err());
            assert!(
                ImageBinding::select(&f.provider, &k, Some(&c), &id, ImageOperation::Generate,)
                    .is_err()
            );
        }
        let id = "x".repeat(256);
        let c = contract(&f.provider, &id);
        let b =
            ImageBinding::select(&f.provider, &k, Some(&c), &id, ImageOperation::Generate).unwrap();
        assert_eq!(b.headers(Some(&f.provider), &k, Some(&c)).unwrap()[1].1, id);
    }

    #[test]
    fn invalid_revisions_and_out_of_range_ttls_are_rejected() {
        let f = fixture();
        let k = key(&f.provider);
        for revision in invalid_text_values() {
            assert!(try_contract(
                &f.provider,
                &k,
                &revision,
                vec![model("model", true, false)],
                Duration::from_secs(60),
            )
            .is_err());
        }
        for ttl in [
            Duration::ZERO,
            Duration::from_secs(60) + Duration::from_nanos(1),
            Duration::from_secs(61),
            Duration::MAX,
        ] {
            assert!(try_contract(
                &f.provider,
                &k,
                "revision",
                vec![model("model", true, false)],
                ttl,
            )
            .is_err());
        }
        assert!(try_contract(
            &f.provider,
            &k,
            &"r".repeat(256),
            vec![model("model", false, true)],
            Duration::from_secs(60),
        )
        .is_ok());
    }

    #[test]
    fn reference_and_revision_rotation_require_new_selection() {
        let f = fixture();
        let k = key(&f.provider);
        let c = contract(&f.provider, "model");
        let b = ImageBinding::select(&f.provider, &k, Some(&c), "model", ImageOperation::Generate)
            .unwrap();
        let mut rotated = f.provider.clone();
        rotated
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .reference =
            serde_json::from_value(json!({"version":1,"id":uuid::Uuid::new_v4().to_string()}))
                .unwrap();
        assert!(
            ImageBinding::select(&rotated, &k, Some(&c), "model", ImageOperation::Generate,)
                .is_err()
        );
        let rotated_contract = contract(&rotated, "model");
        assert!(b
            .headers(Some(&rotated), &k, Some(&rotated_contract))
            .is_err());
        assert!(b
            .headers(Some(&f.provider), &k, Some(&rotated_contract))
            .is_err());
        let replacement = ImageBinding::select(
            &rotated,
            &k,
            Some(&rotated_contract),
            "model",
            ImageOperation::Generate,
        )
        .unwrap();
        assert!(replacement
            .headers(Some(&rotated), &k, Some(&rotated_contract))
            .is_ok());
        let mut revised = contract(&f.provider, "model");
        revised.revision = "new-revision".into();
        assert!(b.headers(Some(&f.provider), &k, Some(&revised)).is_err());
        assert!(b.headers(Some(&f.provider), &k, None).is_err());
    }

    #[test]
    fn changed_source_and_invalid_provider_shapes_fail_closed_at_all_entry_points() {
        let f = fixture();
        let k = key(&f.provider);
        let c = contract(&f.provider, "model");
        let b = ImageBinding::select(&f.provider, &k, Some(&c), "model", ImageOperation::Generate)
            .unwrap();
        for case in 0..9 {
            let mut p = f.provider.clone();
            match case {
                0 => p.meta = None,
                1 => p.meta.as_mut().unwrap().osir_binding = None,
                2 => {
                    p.meta
                        .as_mut()
                        .unwrap()
                        .osir_binding
                        .as_mut()
                        .unwrap()
                        .source = crate::provider::AuthBindingSource::ProviderConfig
                }
                3 => p.id.push('x'),
                4 => p.category = Some("official".into()),
                5 => p.settings_config["config"] = json!("[invalid"),
                6 => {
                    let config = p.settings_config["config"]
                        .as_str()
                        .unwrap()
                        .replace("https://api.osirclaw.com/v1", "https://invalid.example/v1");
                    p.settings_config["config"] = json!(config);
                }
                7 => {
                    p.settings_config["auth"] =
                        json!({"OPENAI_API_KEY":"fixture-only-inline-secret"})
                }
                _ => p.settings_config["auth"] = json!("invalid-auth-shape"),
            }
            assert!(
                try_contract(
                    &p,
                    &k,
                    "revision",
                    vec![model("model", true, false)],
                    Duration::from_secs(60),
                )
                .is_err(),
                "constructor accepted case {case}"
            );
            assert!(
                ImageBinding::select(&p, &k, Some(&c), "model", ImageOperation::Generate).is_err(),
                "selection accepted case {case}"
            );
            assert!(
                b.validate(Some(&p), &k, Some(&c)).is_err(),
                "validation accepted case {case}"
            );
            assert!(
                b.headers(Some(&p), &k, Some(&c)).is_err(),
                "headers accepted case {case}"
            );
        }
    }
    #[test]
    fn binding_changes_catalog_removal_and_expiry_fail_closed() {
        let f = fixture();
        let k = key(&f.provider);
        let mut c = contract(&f.provider, "opaque-id");
        let b = ImageBinding::select(
            &f.provider,
            &k,
            Some(&c),
            "opaque-id",
            ImageOperation::Generate,
        )
        .unwrap();
        assert!(b.validate(None, &k, Some(&c)).is_err());
        for field in 0..4 {
            let mut changed = k.clone();
            match field {
                0 => changed.account_id.push('x'),
                1 => changed.key_id += 1,
                2 => changed.group_id = None,
                _ => changed.provider_id.push('x'),
            }
            assert!(b.validate(Some(&f.provider), &changed, Some(&c)).is_err());
        }
        let removed = contract(&f.provider, "replacement-id");
        assert!(b.validate(Some(&f.provider), &k, Some(&removed)).is_err());
        let mut rotated = f.provider.clone();
        rotated
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .reference =
            serde_json::from_value(json!({"version":1,"id":uuid::Uuid::new_v4().to_string()}))
                .unwrap();
        assert!(b.validate(Some(&rotated), &k, Some(&c)).is_err());
        c.valid_until = Instant::now();
        assert!(b.headers(Some(&f.provider), &k, Some(&c)).is_err());
    }
}
