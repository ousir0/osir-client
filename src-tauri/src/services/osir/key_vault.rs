//! Native-only model key storage. No IPC command may expose load/expose.
//!
//! Run backend operations on a blocking worker. Derive bindings from trusted
//! native session/provider state, never accept a renderer's claimed account.
//! Persist only KeyReference in the provider DB, after store succeeds. Each
//! store creates an immutable random reference: commit the DB replacement before
//! deleting its previous reference; remove the new reference if the DB write fails.
//! The OS keychain is the integrity boundary; this module detects malformed or
//! mismatched records, not an attacker able to replace a valid keychain secret.
use super::credentials::{CredentialBackend, NativeKeychain};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
use url::Url;
use uuid::Uuid;

pub const GATEWAY_ORIGIN: &str = "https://api.osirclaw.com";
const VERSION: u8 = 1;
const MAX_SECRET: usize = 16384;
const MAX_RECORD: usize = 32768;
const MAX_RECOVERED_GRANTS: usize = 128;

// Process-local only, shared across vault handles. Populated exclusively by a
// successful user save/recovery; reads and startup never summon system UI.
// Each entry includes the complete immutable binding and service namespace.
#[derive(Default)]
struct GrantCache(Vec<(String, KeyReference, KeyBinding, String)>);
type SharedGrantCache = Arc<Mutex<GrantCache>>;
fn native_grant_cache() -> SharedGrantCache {
    static CACHE: OnceLock<SharedGrantCache> = OnceLock::new();
    CACHE.get_or_init(Default::default).clone()
}

// Account and legacy commands compile recovery helpers in separate modules;
// their journal writers must still serialize through the same native lock.
pub(crate) fn recovery_lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    static OPERATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    OPERATION.lock().map_err(|_| "凭据恢复状态不可用".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyTool {
    Codex,
    ClaudeCode,
    ClaudeDesktop,
    OpenCode,
    GeminiCli,
}

/// Non-secret context. group_id=None is a real ungrouped key, never a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    pub account_origin: String,
    pub account_id: String,
    pub key_id: i64,
    pub group_id: Option<i64>,
    pub tool: KeyTool,
    pub provider_id: String,
    pub gateway_origin: String,
}

/// Safe DB reference, not authority to read a credential. Validate again on use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyReference {
    version: u8,
    id: String,
}
impl KeyReference {
    fn validate(&self) -> Result<(), KeyVaultError> {
        let id = Uuid::parse_str(&self.id).map_err(|_| KeyVaultError::InvalidFormat)?;
        if self.version != VERSION || id.get_version_num() != 4 || id.to_string() != self.id {
            return Err(KeyVaultError::InvalidFormat);
        }
        Ok(())
    }
}

/// Intentionally no Debug, Clone, Serialize, Display, Deref or AsRef.
pub struct NativeKeySecret(String);
impl NativeKeySecret {
    pub fn new(value: String) -> Result<Self, KeyVaultError> {
        validate_secret(&value)?;
        Ok(Self(value))
    }
    /// Only native config application / proxy request construction may call this.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyMetadata {
    pub reference: KeyReference,
    pub binding: KeyBinding,
    pub masked_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyVaultError {
    #[error("模型凭据格式无效")]
    InvalidFormat,
    #[error("模型凭据绑定不匹配")]
    BindingMismatch,
    #[error("模型凭据安全存储失败")]
    Storage,
    #[error("需要在系统中确认模型凭据访问")]
    InteractionRequired,
    #[error("模型凭据安全存储暂不可用")]
    Unavailable,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    version: u8,
    reference: KeyReference,
    binding: KeyBinding,
    secret: String,
}

pub struct KeyVault<B = NativeKeychain> {
    backend: B,
    service: String,
    account_origin: String,
    recovered: SharedGrantCache,
}
impl KeyVault<NativeKeychain> {
    /// Trusted native configuration; gateway origin is fixed, never configurable.
    pub fn new(account_origin: &str, identity: &str) -> Result<Self, KeyVaultError> {
        let mut vault = Self::with_backend(account_origin, identity, NativeKeychain)?;
        vault.recovered = native_grant_cache();
        Ok(vault)
    }
}
impl<B: CredentialBackend> KeyVault<B> {
    pub fn with_backend(
        account_origin: &str,
        identity: &str,
        backend: B,
    ) -> Result<Self, KeyVaultError> {
        if identity.is_empty()
            || identity.len() > 255
            || !identity
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(KeyVaultError::InvalidFormat);
        }
        let build = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        Ok(Self {
            backend,
            service: format!("{identity}.osir.model-keys.v1.{build}"),
            account_origin: origin(account_origin)?,
            recovered: Default::default(),
        })
    }

    pub fn store(
        &self,
        binding: &KeyBinding,
        secret: &NativeKeySecret,
    ) -> Result<KeyMetadata, KeyVaultError> {
        let prepared = self.prepare_store(binding, secret)?;
        self.store_prepared(&prepared, secret)
    }

    /// Native-only preallocation. Persist its non-secret reference/binding in a
    /// durable intent journal before calling store_prepared. Does not write vault.
    pub(crate) fn prepare_store(
        &self,
        binding: &KeyBinding,
        secret: &NativeKeySecret,
    ) -> Result<KeyMetadata, KeyVaultError> {
        self.validate_binding(binding)?;
        validate_secret(secret.expose())?;
        Ok(KeyMetadata {
            reference: KeyReference {
                version: VERSION,
                id: Uuid::new_v4().to_string(),
            },
            binding: binding.clone(),
            masked_key: mask(secret.expose()),
        })
    }

    /// The caller serializes journal/DB changes. Never replace an existing grant.
    pub(crate) fn store_prepared(
        &self,
        prepared: &KeyMetadata,
        secret: &NativeKeySecret,
    ) -> Result<KeyMetadata, KeyVaultError> {
        let binding = &prepared.binding;
        let reference = prepared.reference.clone();
        reference.validate()?;
        self.validate_binding(binding)?;
        validate_secret(secret.expose())?;
        if self
            .backend
            .read(&self.service, &reference.id)
            .map_err(|_| KeyVaultError::Storage)?
            .is_some()
        {
            return Err(KeyVaultError::InvalidFormat);
        }
        let wire = StoredRecord {
            version: VERSION,
            reference: reference.clone(),
            binding: binding.clone(),
            secret: secret.expose().to_owned(),
        };
        let bytes = serde_json::to_vec(&wire).map_err(|_| KeyVaultError::InvalidFormat)?;
        if bytes.len() > MAX_RECORD {
            return Err(KeyVaultError::InvalidFormat);
        }
        self.backend
            .write_interactive(&self.service, &reference.id, &bytes)
            .map_err(|_| KeyVaultError::Storage)?;
        Ok(KeyMetadata {
            reference,
            binding: binding.clone(),
            masked_key: mask(secret.expose()),
        })
    }

    /// expected MUST be independently derived from current native authorization
    /// and provider state, not copied from stored metadata or untrusted IPC.
    pub fn load(
        &self,
        reference: &KeyReference,
        expected: &KeyBinding,
    ) -> Result<Option<NativeKeySecret>, KeyVaultError> {
        reference.validate()?;
        self.validate_binding(expected)?;
        if let Some(secret) = self.cached(reference, expected)? {
            return Ok(Some(secret));
        }
        self.read_record(reference, expected)?
            .map(|record| NativeKeySecret::new(record.secret))
            .transpose()
    }

    /// Direct user recovery only. Reads the existing immutable item; never
    /// creates a key, changes a binding or writes tool configuration.
    pub(crate) fn load_for_user(
        &self,
        reference: &KeyReference,
        expected: &KeyBinding,
    ) -> Result<Option<NativeKeySecret>, KeyVaultError> {
        self.forget(reference)?;
        let Some(record) = self.read_record_inner(reference, expected, true)? else {
            return Ok(None);
        };
        self.remember(reference, expected, &record.secret)?;
        NativeKeySecret::new(record.secret).map(Some)
    }

    pub(crate) fn forget(&self, reference: &KeyReference) -> Result<(), KeyVaultError> {
        let mut cache = self
            .recovered
            .lock()
            .map_err(|_| KeyVaultError::Unavailable)?;
        cache
            .0
            .retain(|(service, cached, _, _)| service != &self.service || cached != reference);
        Ok(())
    }

    fn remember(
        &self,
        reference: &KeyReference,
        binding: &KeyBinding,
        secret: &str,
    ) -> Result<(), KeyVaultError> {
        let mut cache = self
            .recovered
            .lock()
            .map_err(|_| KeyVaultError::Unavailable)?;
        cache
            .0
            .retain(|(service, cached, _, _)| service != &self.service || cached != reference);
        if cache.0.len() >= MAX_RECOVERED_GRANTS {
            cache.0.remove(0);
        }
        cache.0.push((
            self.service.clone(),
            reference.clone(),
            binding.clone(),
            secret.to_owned(),
        ));
        Ok(())
    }

    fn cached(
        &self,
        reference: &KeyReference,
        expected: &KeyBinding,
    ) -> Result<Option<NativeKeySecret>, KeyVaultError> {
        let cache = self
            .recovered
            .lock()
            .map_err(|_| KeyVaultError::Unavailable)?;
        let Some((_, _, binding, secret)) = cache
            .0
            .iter()
            .find(|(service, cached, _, _)| service == &self.service && cached == reference)
        else {
            return Ok(None);
        };
        if binding != expected {
            return Err(KeyVaultError::BindingMismatch);
        }
        NativeKeySecret::new(secret.clone()).map(Some)
    }

    /// Checks binding before deletion, including for stale/corrupt references.
    /// A malformed record requires native recovery; never blindly remove it.
    pub fn delete(
        &self,
        reference: &KeyReference,
        expected: &KeyBinding,
    ) -> Result<(), KeyVaultError> {
        self.forget(reference)?;
        if self.read_record(reference, expected)?.is_some() {
            self.backend
                .remove(&self.service, &reference.id)
                .map_err(|_| KeyVaultError::Storage)?;
        }
        Ok(())
    }

    fn read_record(
        &self,
        reference: &KeyReference,
        expected: &KeyBinding,
    ) -> Result<Option<StoredRecord>, KeyVaultError> {
        self.read_record_inner(reference, expected, false)
    }
    fn read_record_inner(
        &self,
        reference: &KeyReference,
        expected: &KeyBinding,
        interactive: bool,
    ) -> Result<Option<StoredRecord>, KeyVaultError> {
        reference.validate()?;
        self.validate_binding(expected)?;
        let result = if interactive {
            self.backend.read_interactive(&self.service, &reference.id)
        } else {
            self.backend.read(&self.service, &reference.id)
        };
        let Some(bytes) = result.map_err(|error| {
            // The backend error is a closed native enum, never an OS error
            // string or credential payload. Keep the reason available for
            // native recovery diagnosis without exposing item identifiers.
            log::warn!("OSIR managed credential read failed: {error:?}");
            match error {
                super::credentials::CredentialError::InteractionRequired => {
                    KeyVaultError::InteractionRequired
                }
                super::credentials::CredentialError::Unavailable => KeyVaultError::Unavailable,
                _ => KeyVaultError::Storage,
            }
        })?
        else {
            return Ok(None);
        };
        if bytes.len() > MAX_RECORD {
            return Err(KeyVaultError::InvalidFormat);
        }
        let record: StoredRecord =
            serde_json::from_slice(&bytes).map_err(|_| KeyVaultError::InvalidFormat)?;
        if record.version != VERSION {
            return Err(KeyVaultError::InvalidFormat);
        }
        record.reference.validate()?;
        self.validate_binding(&record.binding)?;
        if record.reference != *reference || record.binding != *expected {
            return Err(KeyVaultError::BindingMismatch);
        }
        validate_secret(&record.secret)?;
        Ok(Some(record))
    }
    fn validate_binding(&self, binding: &KeyBinding) -> Result<(), KeyVaultError> {
        validate_id(&binding.account_id)?;
        validate_id(&binding.provider_id)?;
        if binding.key_id <= 0 || binding.group_id.is_some_and(|id| id <= 0) {
            return Err(KeyVaultError::InvalidFormat);
        }
        // Exact canonical strings keep the persisted tuple unambiguous.
        if origin(&binding.account_origin)? != binding.account_origin
            || binding.account_origin != self.account_origin
            || binding.gateway_origin != GATEWAY_ORIGIN
        {
            return Err(KeyVaultError::BindingMismatch);
        }
        Ok(())
    }
}
fn validate_secret(secret: &str) -> Result<(), KeyVaultError> {
    if secret.is_empty()
        || secret.len() > MAX_SECRET
        || !secret.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(KeyVaultError::InvalidFormat);
    }
    Ok(())
}
fn validate_id(value: &str) -> Result<(), KeyVaultError> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(KeyVaultError::InvalidFormat);
    }
    Ok(())
}
fn origin(value: &str) -> Result<String, KeyVaultError> {
    let url = Url::parse(value).map_err(|_| KeyVaultError::InvalidFormat)?;
    if value.trim() != value
        || value.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(KeyVaultError::InvalidFormat);
    }
    Ok(url.origin().ascii_serialization())
}
fn mask(secret: &str) -> String {
    if secret.len() <= 8 {
        "••••".into()
    } else {
        format!("••••{}", &secret[secret.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::super::credentials::CredentialError;
    use super::*;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct State {
        items: HashMap<(String, String), Vec<u8>>,
        fail_write: bool,
        fail_read: bool,
        fail_delete: bool,
    }
    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<State>>);
    impl CredentialBackend for Memory {
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            let mut state = self.0.lock().unwrap();
            if state.fail_write {
                return Err(CredentialError::Storage);
            }
            state
                .items
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            let state = self.0.lock().unwrap();
            if state.fail_read {
                return Err(CredentialError::Storage);
            }
            Ok(state.items.get(&(service.into(), account.into())).cloned())
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            let mut state = self.0.lock().unwrap();
            if state.fail_delete {
                return Err(CredentialError::Storage);
            }
            state.items.remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    fn fixture() -> (KeyVault<Memory>, KeyBinding, NativeKeySecret) {
        (
            KeyVault::with_backend(
                "https://osirclaw.com",
                "com.osir.client.dev",
                Memory::default(),
            )
            .unwrap(),
            KeyBinding {
                account_origin: "https://osirclaw.com".into(),
                account_id: "42".into(),
                key_id: 7,
                group_id: Some(3),
                tool: KeyTool::Codex,
                provider_id: "provider-1".into(),
                gateway_origin: GATEWAY_ORIGIN.into(),
            },
            NativeKeySecret::new("fixture-secret-1234".into()).unwrap(),
        )
    }
    #[test]
    fn roundtrip_safe_reference_and_idempotent_delete() {
        let (vault, binding, secret) = fixture();
        let meta = vault.store(&binding, &secret).unwrap();
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains(secret.expose()));
        assert_eq!(meta.masked_key, "••••1234");
        let reference: KeyReference =
            serde_json::from_str(&serde_json::to_string(&meta.reference).unwrap()).unwrap();
        assert!(vault.load(&reference, &binding).unwrap().unwrap().expose() == secret.expose());
        vault.delete(&reference, &binding).unwrap();
        vault.delete(&reference, &binding).unwrap();
        assert!(vault.load(&reference, &binding).unwrap().is_none());
    }

    #[test]
    fn recovered_grants_share_only_an_exact_native_service_reference_and_binding() {
        let (vault, binding, secret) = fixture();
        let reference = vault.store(&binding, &secret).unwrap().reference;
        vault.load_for_user(&reference, &binding).unwrap().unwrap();
        vault.backend.0.lock().unwrap().fail_read = true;
        let mut same = KeyVault::with_backend(
            &binding.account_origin,
            "com.osir.client.dev",
            vault.backend.clone(),
        )
        .unwrap();
        same.recovered = vault.recovered.clone();
        assert_eq!(
            same.load(&reference, &binding).unwrap().unwrap().expose(),
            secret.expose()
        );
        let mut wrong = binding.clone();
        wrong.key_id += 1;
        assert!(matches!(
            same.load(&reference, &wrong),
            Err(KeyVaultError::BindingMismatch)
        ));
        let mut other = KeyVault::with_backend(
            &binding.account_origin,
            "com.osir.client.beta",
            vault.backend.clone(),
        )
        .unwrap();
        other.recovered = vault.recovered.clone();
        assert!(matches!(
            other.load(&reference, &binding),
            Err(KeyVaultError::Storage)
        ));
        vault.backend.0.lock().unwrap().fail_read = false;
        vault.delete(&reference, &binding).unwrap();
        assert!(same.load(&reference, &binding).unwrap().is_none());
    }

    #[test]
    fn recovered_native_grants_are_bounded_and_failed_recovery_does_not_keep_an_old_grant() {
        let (vault, binding, secret) = fixture();
        let reference = vault.store(&binding, &secret).unwrap().reference;
        vault.load_for_user(&reference, &binding).unwrap().unwrap();
        vault.backend.0.lock().unwrap().fail_read = true;
        assert!(vault.load_for_user(&reference, &binding).is_err());
        assert!(vault.load(&reference, &binding).is_err());
        vault.backend.0.lock().unwrap().fail_read = false;
        for _ in 0..=MAX_RECOVERED_GRANTS {
            let prepared = vault.prepare_store(&binding, &secret).unwrap();
            vault
                .remember(&prepared.reference, &binding, secret.expose())
                .unwrap();
        }
        assert_eq!(
            vault.recovered.lock().unwrap().0.len(),
            MAX_RECOVERED_GRANTS
        );
        assert!(vault.cached(&reference, &binding).unwrap().is_none());
    }
    #[test]
    fn rejects_every_wrong_expected_binding_and_prevents_delete() {
        let (vault, binding, secret) = fixture();
        let reference = vault.store(&binding, &secret).unwrap().reference;
        for field in [
            "account_origin",
            "account_id",
            "key_id",
            "group_id",
            "tool",
            "provider_id",
            "gateway_origin",
        ] {
            let mut wrong = binding.clone();
            match field {
                "account_origin" => wrong.account_origin = "https://other.example".into(),
                "account_id" => wrong.account_id = "43".into(),
                "key_id" => wrong.key_id = 8,
                "group_id" => wrong.group_id = None,
                "tool" => wrong.tool = KeyTool::ClaudeCode,
                "provider_id" => wrong.provider_id = "other".into(),
                _ => wrong.gateway_origin = "https://other.example".into(),
            }
            assert!(matches!(
                vault.load(&reference, &wrong),
                Err(KeyVaultError::BindingMismatch)
            ));
            assert_eq!(
                vault.delete(&reference, &wrong),
                Err(KeyVaultError::BindingMismatch)
            );
        }
        assert!(vault.load(&reference, &binding).unwrap().is_some());
    }
    #[test]
    fn rejects_tampered_records_versions_unknown_fields_and_references() {
        let (vault, binding, secret) = fixture();
        let reference = vault.store(&binding, &secret).unwrap().reference;
        let slot = (vault.service.clone(), reference.id.clone());
        let original = vault.backend.0.lock().unwrap().items[&slot].clone();
        for (pointer, value) in [
            ("/version", serde_json::json!(2)),
            ("/reference/version", serde_json::json!(2)),
            (
                "/reference/id",
                serde_json::json!(Uuid::new_v4().to_string()),
            ),
            ("/binding/account_id", serde_json::json!("99")),
            (
                "/binding/account_origin",
                serde_json::json!("https://other.example"),
            ),
            ("/binding/key_id", serde_json::json!(9)),
            ("/binding/group_id", serde_json::json!(null)),
            ("/binding/tool", serde_json::json!("gemini_cli")),
            ("/binding/provider_id", serde_json::json!("other")),
            (
                "/binding/gateway_origin",
                serde_json::json!("https://other.example"),
            ),
            ("/secret", serde_json::json!("bad\nheader")),
        ] {
            let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
            *record.pointer_mut(pointer).unwrap() = value;
            vault
                .backend
                .0
                .lock()
                .unwrap()
                .items
                .insert(slot.clone(), serde_json::to_vec(&record).unwrap());
            assert!(vault.load(&reference, &binding).is_err(), "{pointer}");
        }
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record["unknown"] = serde_json::json!(true);
        for bytes in [
            serde_json::to_vec(&record).unwrap(),
            b"not-json".to_vec(),
            vec![b'x'; MAX_RECORD + 1],
        ] {
            vault
                .backend
                .0
                .lock()
                .unwrap()
                .items
                .insert(slot.clone(), bytes);
            assert!(matches!(
                vault.load(&reference, &binding),
                Err(KeyVaultError::InvalidFormat)
            ));
        }
        let mut wrong = reference.clone();
        wrong.version = 2;
        assert!(vault.load(&wrong, &binding).is_err());
        wrong.version = 1;
        wrong.id = "../unsafe".into();
        assert!(vault.load(&wrong, &binding).is_err());
    }
    #[test]
    fn validates_secret_and_never_discloses_short_strings() {
        let (vault, binding, _) = fixture();
        for bad in [
            String::new(),
            "bad header".into(),
            "bad\r\n".into(),
            "秘密".into(),
            "x".repeat(MAX_SECRET + 1),
            "\u{7f}".into(),
        ] {
            assert!(NativeKeySecret::new(bad).is_err());
        }
        for len in 1..=8 {
            let secret = NativeKeySecret::new("x".repeat(len)).unwrap();
            assert_eq!(vault.store(&binding, &secret).unwrap().masked_key, "••••");
        }
        assert!(NativeKeySecret::new("x".repeat(MAX_SECRET)).is_ok());
    }
    #[test]
    fn failed_operations_are_safe_and_never_replace_previous_secret() {
        let (vault, binding, secret) = fixture();
        let old = vault.store(&binding, &secret).unwrap().reference;
        vault.backend.0.lock().unwrap().fail_write = true;
        assert!(matches!(
            vault.store(
                &binding,
                &NativeKeySecret::new("replacement".into()).unwrap()
            ),
            Err(KeyVaultError::Storage)
        ));
        assert!(vault.load(&old, &binding).unwrap().unwrap().expose() == secret.expose());
        vault.backend.0.lock().unwrap().fail_delete = true;
        assert_eq!(vault.delete(&old, &binding), Err(KeyVaultError::Storage));
        vault.backend.0.lock().unwrap().fail_read = true;
        assert!(matches!(
            vault.load(&old, &binding),
            Err(KeyVaultError::Storage)
        ));
    }
    #[test]
    fn isolates_identity_build_origin_and_model_credential_namespace() {
        let (vault, binding, secret) = fixture();
        let reference = vault.store(&binding, &secret).unwrap().reference;
        let other = KeyVault::with_backend(
            &binding.account_origin,
            "com.osir.client",
            vault.backend.clone(),
        )
        .unwrap();
        assert!(other.load(&reference, &binding).unwrap().is_none());
        assert_eq!(vault.service.ends_with(".debug"), cfg!(debug_assertions));
        assert!(vault.service.contains(".osir.model-keys.v1."));
        let other_origin = KeyVault::with_backend(
            "https://other.example",
            "com.osir.client.dev",
            vault.backend.clone(),
        )
        .unwrap();
        assert!(other_origin.load(&reference, &binding).is_err());
        for bad in [
            "http://osirclaw.com",
            "https://u:p@osirclaw.com",
            "https://osirclaw.com/api",
            "https://osirclaw.com?x=1",
            "https://osirclaw.com/#x",
        ] {
            assert!(KeyVault::with_backend(bad, "com.test", Memory::default()).is_err());
        }
        assert!(
            KeyVault::with_backend(&binding.account_origin, "bad/identity", Memory::default())
                .is_err()
        );
    }
}
