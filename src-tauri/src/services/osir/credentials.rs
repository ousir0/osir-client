//! Native credentials; never register these types as IPC responses.
//! Run Keychain calls on a blocking worker. Publish login/rotation only after
//! store succeeds; clear memory on logout even when delete fails.
use serde::{Deserialize, Serialize};
use url::Url;

// Intentionally neither Debug nor serde.
pub struct SecretRecord {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds. Expired access may be loaded only through refresh recovery.
    pub expires_at: i64,
    pub account_id: String,
    pub origin: String,
    /// Space-separated OAuth scopes.
    pub scope: String,
    /// Native random slot. None selects the legacy v1 account item.
    pub(super) slot: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    version: u8,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    account_id: String,
    origin: String,
    scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    slot: Option<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("需要在系统中确认凭据访问")]
    InteractionRequired,
    #[error("安全凭据存储不可用")]
    Unavailable,
    #[error("安全凭据存储操作失败")]
    Storage,
    #[error("凭据格式无效")]
    InvalidFormat,
    #[error("凭据服务地址不匹配")]
    OriginMismatch,
    #[error("凭据账户不匹配")]
    AccountMismatch,
    #[error("账户凭据已过期")]
    Expired,
}

/// Native storage seam. Never log payloads/backend errors. Failed writes retain
/// the old item; missing reads return None; deleting an absent item succeeds.
pub trait CredentialBackend: Send + Sync {
    fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError>;
    fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError>;
    /// Add-only and silent. Never inspect or update a predecessor's ACL.
    fn create(&self, _service: &str, _account: &str, _bytes: &[u8]) -> Result<(), CredentialError> {
        Err(CredentialError::Unavailable)
    }
    /// A browser login explicitly saves a fresh item and may unlock the store.
    /// Background token refresh must continue to use the silent write path.
    fn create_interactive(
        &self,
        service: &str,
        account: &str,
        bytes: &[u8],
    ) -> Result<(), CredentialError> {
        self.create(service, account, bytes)
    }
    /// Only a direct user action may request the system authorization dialog.
    fn read_interactive(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        self.read(service, account)
    }
    fn write_interactive(
        &self,
        service: &str,
        account: &str,
        bytes: &[u8],
    ) -> Result<(), CredentialError> {
        self.write(service, account, bytes)
    }
    /// Startup must not summon a system password prompt behind the app.
    fn read_without_prompt(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        self.read(service, account)
    }
    fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError>;
}
pub struct NativeKeychain;

// Development HOME isolates tool files, but is not the OS login home.
#[cfg(target_os = "macos")]
fn development_keychain(
) -> Result<Option<security_framework::os::macos::keychain::SecKeychain>, CredentialError> {
    if let Some(path) = crate::distribution::isolated_keychain() {
        return security_framework::os::macos::keychain::SecKeychain::open(path)
            .map(Some)
            .map_err(keychain_open_error);
    }
    if crate::distribution::is_isolated() {
        return Err(CredentialError::Unavailable);
    }
    if !cfg!(debug_assertions) || std::env::var_os("CC_SWITCH_TEST_HOME").is_none() {
        return Ok(None);
    }
    let path = std::env::var_os("OSIR_DEV_KEYCHAIN").ok_or(CredentialError::Unavailable)?;
    security_framework::os::macos::keychain::SecKeychain::open(std::path::PathBuf::from(path))
        .map(Some)
        .map_err(keychain_open_error)
}

#[cfg(target_os = "macos")]
impl CredentialBackend for NativeKeychain {
    fn create(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
        native_keychain_access(false, || create_native_password(service, account, bytes))
    }
    fn create_interactive(
        &self,
        service: &str,
        account: &str,
        bytes: &[u8],
    ) -> Result<(), CredentialError> {
        native_keychain_access(true, || create_native_password(service, account, bytes))
    }
    fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
        native_keychain_access(false, || write_native_password(service, account, bytes))
    }
    fn write_interactive(
        &self,
        service: &str,
        account: &str,
        bytes: &[u8],
    ) -> Result<(), CredentialError> {
        native_keychain_access(true, || write_native_password(service, account, bytes))
    }
    fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
        native_keychain_access(false, || read_native_password(service, account))
    }
    fn read_interactive(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        native_keychain_access(true, || read_native_password(service, account))
    }
    fn read_without_prompt(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        self.read(service, account)
    }
    fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        native_keychain_access(false, || remove_native_password(service, account))
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialAccess {
    Idle,
    Silent,
    Interactive,
}

#[cfg(any(target_os = "macos", test))]
struct CredentialAccessGate {
    state: std::sync::Mutex<CredentialAccess>,
    changed: std::sync::Condvar,
}

#[cfg(any(target_os = "macos", test))]
impl CredentialAccessGate {
    const fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(CredentialAccess::Idle),
            changed: std::sync::Condvar::new(),
        }
    }

    fn acquire(
        &self,
        interactive: bool,
        timeout: std::time::Duration,
    ) -> Result<CredentialAccessGuard<'_>, CredentialError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        if *state == CredentialAccess::Silent {
            (state, _) = self
                .changed
                .wait_timeout_while(state, timeout, |state| *state == CredentialAccess::Silent)
                .map_err(|_| CredentialError::Unavailable)?;
        }
        // Never wait behind a system dialog or queue a second interactive action.
        if *state != CredentialAccess::Idle {
            return Err(CredentialError::Unavailable);
        }
        *state = if interactive {
            CredentialAccess::Interactive
        } else {
            CredentialAccess::Silent
        };
        Ok(CredentialAccessGuard(self))
    }
}

#[cfg(any(target_os = "macos", test))]
struct CredentialAccessGuard<'a>(&'a CredentialAccessGate);

#[cfg(any(target_os = "macos", test))]
impl Drop for CredentialAccessGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            *state = CredentialAccess::Idle;
            self.0.changed.notify_all();
        }
    }
}

// Keychain interaction is process-wide. Requests may wait behind short silent
// operations, but an active system dialog never acquires queued followers.
#[cfg(target_os = "macos")]
fn native_keychain_access<T>(
    interactive: bool,
    operation: impl FnOnce() -> Result<T, CredentialError>,
) -> Result<T, CredentialError> {
    use security_framework::os::macos::keychain::SecKeychain;
    static ACCESS: CredentialAccessGate = CredentialAccessGate::new();
    let _access = ACCESS
        .acquire(interactive, std::time::Duration::from_secs(2))
        .map_err(|error| {
            // Fixed diagnostic only: no item service, account, payload or path.
            log::warn!("OSIR native credential access deferred: gate_busy");
            error
        })?;
    let _silent = if interactive {
        None
    } else {
        Some(SecKeychain::disable_user_interaction().map_err(|error| {
            log::warn!(
                "OSIR native credential silent-access setup failed: osstatus={}",
                error.code()
            );
            CredentialError::Unavailable
        })?)
    };
    operation()
}

#[cfg(target_os = "macos")]
fn keychain_error(error: security_framework::base::Error) -> CredentialError {
    // OSStatus is public diagnostic metadata. Never format the backend error,
    // because its description can contain item metadata on other platforms.
    log::warn!(
        "OSIR native credential operation failed: osstatus={}",
        error.code()
    );
    match error.code() {
        -25308 | -25293 | -128 => CredentialError::InteractionRequired,
        _ => CredentialError::Storage,
    }
}

#[cfg(target_os = "macos")]
fn keychain_open_error(error: security_framework::base::Error) -> CredentialError {
    log::warn!(
        "OSIR native credential store open failed: osstatus={}",
        error.code()
    );
    CredentialError::Unavailable
}

#[cfg(target_os = "macos")]
fn find_native_password_item(
    keychain: &security_framework::os::macos::keychain::SecKeychain,
    service: &str,
    account: &str,
) -> Result<Option<security_framework::os::macos::keychain_item::SecKeychainItem>, CredentialError>
{
    use security_framework::item::{ItemClass, ItemSearchOptions, Reference, SearchResult};
    // Updating a password must not first request the previous password. On a
    // rebuilt development executable, that unnecessary read can prompt for a
    // separate ACL permission before the actual update is authorized.
    let results = ItemSearchOptions::new()
        .keychains(std::slice::from_ref(keychain))
        .class(ItemClass::generic_password())
        .service(service)
        .account(account)
        .load_refs(true)
        .load_attributes(false)
        .load_data(false)
        .limit(2_i64)
        .search();
    let mut results = match results {
        Ok(results) => results,
        Err(error) if error.code() == -25300 => return Ok(None),
        Err(error) => return Err(keychain_error(error)),
    };
    if results.len() > 1 {
        return Err(CredentialError::Storage);
    }
    match results.pop() {
        None => Ok(None),
        Some(SearchResult::Ref(Reference::KeychainItem(item))) => Ok(Some(item)),
        _ => Err(CredentialError::Storage),
    }
}

/// Shared ordering seam for synthetic tests: the lookup supplies an opaque item
/// handle, never password bytes. A failed lookup/update cannot delete/recreate
/// an existing item or cause a second authorization operation.
#[cfg(any(target_os = "macos", test))]
fn write_password_item<Item>(
    metadata: impl FnOnce() -> Result<Option<Item>, CredentialError>,
    update: impl FnOnce(Item) -> Result<(), CredentialError>,
    create: impl FnOnce() -> Result<(), CredentialError>,
) -> Result<(), CredentialError> {
    match metadata()? {
        Some(item) => update(item),
        None => create(),
    }
}

#[cfg(target_os = "macos")]
fn create_native_password(
    service: &str,
    account: &str,
    bytes: &[u8],
) -> Result<(), CredentialError> {
    let keychain = development_keychain()?
        .map_or_else(
            security_framework::os::macos::keychain::SecKeychain::default,
            Ok,
        )
        .map_err(keychain_open_error)?;
    keychain
        .add_generic_password(service, account, bytes)
        .map_err(keychain_error)
}

#[cfg(target_os = "macos")]
fn write_native_password(
    service: &str,
    account: &str,
    bytes: &[u8],
) -> Result<(), CredentialError> {
    if let Some(keychain) = development_keychain()? {
        return write_password_item(
            || find_native_password_item(&keychain, service, account),
            |mut item| item.set_password(bytes).map_err(keychain_error),
            || {
                keychain
                    .add_generic_password(service, account, bytes)
                    .map(|_| ())
                    .map_err(keychain_error)
            },
        );
    }
    security_framework::passwords::set_generic_password(service, account, bytes)
        .map_err(keychain_error)
}
#[cfg(target_os = "macos")]
fn read_native_password(service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
    if let Some(keychain) = development_keychain()? {
        return match keychain.find_generic_password(service, account) {
            Ok((bytes, _)) => Ok(Some(bytes.to_vec())),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(error) => Err(keychain_error(error)),
        };
    }
    match security_framework::passwords::get_generic_password(service, account) {
        Ok(bytes) => Ok(Some(bytes)),
        // errSecItemNotFound; no direct security-framework-sys dependency.
        Err(e) if e.code() == -25300 => Ok(None),
        Err(error) => Err(keychain_error(error)),
    }
}
#[cfg(target_os = "macos")]
fn remove_native_password(service: &str, account: &str) -> Result<(), CredentialError> {
    if let Some(keychain) = development_keychain()? {
        match keychain.find_generic_password(service, account) {
            Ok((_, item)) => item.delete(),
            Err(e) if e.code() == -25300 => return Ok(()),
            Err(error) => return Err(keychain_error(error)),
        }
        return match keychain.find_generic_password(service, account) {
            Err(e) if e.code() == -25300 => Ok(()),
            _ => Err(CredentialError::Storage),
        };
    }
    match security_framework::passwords::delete_generic_password(service, account) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == -25300 => Ok(()),
        Err(error) => Err(keychain_error(error)),
    }
}
#[cfg(any(target_os = "windows", test))]
#[path = "credentials_windows.rs"]
mod windows;

#[cfg(target_os = "windows")]
impl CredentialBackend for NativeKeychain {
    fn create(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
        windows::write(service, account, bytes, true)
    }
    fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
        windows::write(service, account, bytes, false)
    }
    fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
        windows::read(service, account)
    }
    fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        windows::remove(service, account)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl CredentialBackend for NativeKeychain {
    fn write(&self, _: &str, _: &str, _: &[u8]) -> Result<(), CredentialError> {
        Err(CredentialError::Unavailable)
    }
    fn read(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, CredentialError> {
        Err(CredentialError::Unavailable)
    }
    fn remove(&self, _: &str, _: &str) -> Result<(), CredentialError> {
        Err(CredentialError::Unavailable)
    }
}

pub struct CredentialStore<B = NativeKeychain> {
    backend: B,
    service: String,
    origin: String,
}
impl CredentialStore<NativeKeychain> {
    /// Both arguments come from trusted native configuration, never IPC.
    /// Pass the running app identity, e.g. com.osir.client.dev.
    pub fn new(origin: &str, identity: &str) -> Result<Self, CredentialError> {
        Self::with_backend(origin, identity, NativeKeychain)
    }
}
impl<B: CredentialBackend> CredentialStore<B> {
    pub fn with_backend(origin: &str, identity: &str, backend: B) -> Result<Self, CredentialError> {
        if identity.is_empty()
            || identity.len() > 255
            || !identity
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err(CredentialError::InvalidFormat);
        }
        let build = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        Ok(Self {
            backend,
            service: format!("{identity}.osir.credentials.v1.{build}"),
            origin: canonical_origin(origin)?,
        })
    }
    pub fn store(&self, record: &SecretRecord) -> Result<(), CredentialError> {
        let bytes = self.encode(record)?;
        let item = self.item_at(&record.account_id, record.slot.as_deref())?;
        self.backend.write(&self.service, &item, &bytes)
    }
    pub(super) fn create(&self, record: &SecretRecord) -> Result<(), CredentialError> {
        self.create_inner(record, false)
    }
    pub(super) fn create_for_user(&self, record: &SecretRecord) -> Result<(), CredentialError> {
        self.create_inner(record, true)
    }
    fn create_inner(
        &self,
        record: &SecretRecord,
        interactive: bool,
    ) -> Result<(), CredentialError> {
        if record.slot.is_none() {
            return Err(CredentialError::InvalidFormat);
        }
        let bytes = self.encode(record)?;
        let item = self.item_at(&record.account_id, record.slot.as_deref())?;
        if interactive {
            self.backend
                .create_interactive(&self.service, &item, &bytes)
        } else {
            self.backend.create(&self.service, &item, &bytes)
        }
    }
    fn encode(&self, record: &SecretRecord) -> Result<Vec<u8>, CredentialError> {
        self.validate(record, &record.account_id, false)?;
        let wire = StoredRecord {
            version: if record.slot.is_some() { 2 } else { 1 },
            access_token: record.access_token.clone(),
            refresh_token: record.refresh_token.clone(),
            expires_at: record.expires_at,
            account_id: record.account_id.clone(),
            origin: self.origin.clone(),
            scope: record.scope.clone(),
            slot: record.slot.clone(),
        };
        let bytes = serde_json::to_vec(&wire).map_err(|_| CredentialError::InvalidFormat)?;
        if bytes.len() > 65536 {
            return Err(CredentialError::InvalidFormat);
        }
        Ok(bytes)
    }
    /// Account is selected by native session state. Corrupt or expired records
    /// remain deletable; refresh recovery is a session-controller decision.
    pub fn load(&self, account: &str) -> Result<Option<SecretRecord>, CredentialError> {
        self.load_inner(account, None, false, false)
    }
    /// Native recovery only; an expired access token must never authorize a GET.
    #[cfg(test)]
    pub(super) fn load_for_refresh(
        &self,
        account: &str,
    ) -> Result<Option<SecretRecord>, CredentialError> {
        self.load_inner(account, None, true, false)
    }
    #[cfg(test)]
    pub(super) fn load_slot_for_refresh(
        &self,
        account: &str,
        slot: Option<&str>,
    ) -> Result<Option<SecretRecord>, CredentialError> {
        self.load_inner(account, slot, true, false)
    }
    pub(super) fn load_for_user(
        &self,
        account: &str,
        slot: Option<&str>,
    ) -> Result<Option<SecretRecord>, CredentialError> {
        self.decode_record(
            account,
            slot,
            true,
            self.backend
                .read_interactive(&self.service, &self.item_at(account, slot)?)?,
        )
    }
    pub(super) fn load_for_startup(
        &self,
        account: &str,
        slot: Option<&str>,
    ) -> Result<Option<SecretRecord>, CredentialError> {
        self.load_inner(account, slot, true, true)
    }
    fn load_inner(
        &self,
        account: &str,
        slot: Option<&str>,
        allow_expired: bool,
        without_prompt: bool,
    ) -> Result<Option<SecretRecord>, CredentialError> {
        let item = self.item_at(account, slot)?;
        let bytes = if without_prompt {
            self.backend.read_without_prompt(&self.service, &item)
        } else {
            self.backend.read(&self.service, &item)
        }?;
        self.decode_record(account, slot, allow_expired, bytes)
    }
    fn decode_record(
        &self,
        account: &str,
        slot: Option<&str>,
        allow_expired: bool,
        bytes: Option<Vec<u8>>,
    ) -> Result<Option<SecretRecord>, CredentialError> {
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        if bytes.len() > 65536 {
            return Err(CredentialError::InvalidFormat);
        }
        let wire: StoredRecord =
            serde_json::from_slice(&bytes).map_err(|_| CredentialError::InvalidFormat)?;
        if !matches!((wire.version, slot), (1, None) | (2, Some(_))) || wire.slot.as_deref() != slot
        {
            return Err(CredentialError::InvalidFormat);
        }
        let record = SecretRecord {
            access_token: wire.access_token,
            refresh_token: wire.refresh_token,
            expires_at: wire.expires_at,
            account_id: wire.account_id,
            origin: wire.origin,
            scope: wire.scope,
            slot: wire.slot,
        };
        self.validate(&record, account, allow_expired)?;
        Ok(Some(record))
    }
    pub fn delete(&self, account: &str) -> Result<(), CredentialError> {
        self.delete_slot(account, None)
    }
    pub(super) fn delete_slot(
        &self,
        account: &str,
        slot: Option<&str>,
    ) -> Result<(), CredentialError> {
        self.backend
            .remove(&self.service, &self.item_at(account, slot)?)
    }
    fn item(&self, account: &str) -> Result<String, CredentialError> {
        self.item_at(account, None)
    }
    fn item_at(&self, account: &str, slot: Option<&str>) -> Result<String, CredentialError> {
        validate_account(account)?;
        validate_slot(slot)?;
        // Tuple encoding avoids delimiter collisions; no tokens in labels.
        match slot {
            Some(slot) => serde_json::to_string(&(&self.origin, account, slot)),
            None => serde_json::to_string(&(&self.origin, account)),
        }
        .map_err(|_| CredentialError::InvalidFormat)
    }
    fn validate(
        &self,
        record: &SecretRecord,
        account: &str,
        allow_expired: bool,
    ) -> Result<(), CredentialError> {
        validate_account(&record.account_id)?;
        validate_slot(record.slot.as_deref())?;
        if record.account_id != account {
            return Err(CredentialError::AccountMismatch);
        }
        if canonical_origin(&record.origin)? != self.origin {
            return Err(CredentialError::OriginMismatch);
        }
        for token in [&record.access_token, &record.refresh_token] {
            if token.is_empty()
                || token.len() > 16384
                || !token.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(CredentialError::InvalidFormat);
            }
        }
        if record.scope.is_empty()
            || record.scope.len() > 4096
            || record.scope.split(' ').any(|s| {
                s.is_empty()
                    || !s.bytes().all(|b| {
                        b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b)
                    })
            })
            || record.expires_at <= 0
            || record.expires_at > 253402300799
        {
            return Err(CredentialError::InvalidFormat);
        }
        if !allow_expired && record.expires_at <= chrono::Utc::now().timestamp() {
            return Err(CredentialError::Expired);
        }
        Ok(())
    }
}
fn validate_slot(slot: Option<&str>) -> Result<(), CredentialError> {
    if slot.is_some_and(|value| {
        value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) {
        return Err(CredentialError::InvalidFormat);
    }
    Ok(())
}
fn validate_account(account: &str) -> Result<(), CredentialError> {
    if account.is_empty()
        || account.len() > 256
        || account.trim() != account
        || account.chars().any(char::is_control)
    {
        return Err(CredentialError::InvalidFormat);
    }
    Ok(())
}
fn canonical_origin(origin: &str) -> Result<String, CredentialError> {
    let url = Url::parse(origin).map_err(|_| CredentialError::InvalidFormat)?;
    if origin.trim() != origin
        || origin.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CredentialError::InvalidFormat);
    }
    Ok(url.origin().ascii_serialization())
}

/// Non-secret native journal. No path or contents originate in IPC. Atomic,
/// synced replacement makes logout independent of Keychain delete availability.
/// A false active bit is a durable tombstone, including during token rotation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountReference {
    version: u8,
    origin: String,
    pub account: Option<String>,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
}
pub(super) struct SessionJournal {
    origin: String,
    path: Option<std::path::PathBuf>,
    memory: std::sync::Mutex<Option<AccountReference>>,
}
impl SessionJournal {
    pub fn memory(origin: String) -> Self {
        Self {
            origin,
            path: None,
            memory: std::sync::Mutex::new(None),
        }
    }
    pub fn persistent(origin: String, directory: &std::path::Path) -> Self {
        // Caller uses Tauri's identity-specific local app data directory.
        let name = if cfg!(debug_assertions) {
            "osir-session-debug.json"
        } else {
            "osir-session-release.json"
        };
        Self {
            origin,
            path: Some(directory.join(name)),
            memory: std::sync::Mutex::new(None),
        }
    }
    pub fn read(&self) -> Result<Option<AccountReference>, CredentialError> {
        let Some(path) = &self.path else {
            return Ok(self.memory.lock().unwrap().clone());
        };
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(CredentialError::Storage),
        };
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| CredentialError::Storage)?;
        if bytes.len() > 4096 {
            return Err(CredentialError::InvalidFormat);
        }
        let reference: AccountReference =
            serde_json::from_slice(&bytes).map_err(|_| CredentialError::InvalidFormat)?;
        if !matches!(
            (reference.version, reference.slot.as_ref()),
            (1, None) | (2, Some(_))
        ) || reference.origin != self.origin
            || (reference.active && reference.account.is_none())
        {
            return Err(CredentialError::InvalidFormat);
        }
        if let Some(account) = &reference.account {
            validate_account(account)?;
        }
        validate_slot(reference.slot.as_deref())?;
        if reference.slot.is_some() && reference.account.is_none() {
            return Err(CredentialError::InvalidFormat);
        }
        Ok(Some(reference))
    }
    pub fn write(&self, account: Option<&str>, active: bool) -> Result<(), CredentialError> {
        self.write_slot(account, None, active)
    }
    pub fn write_slot(
        &self,
        account: Option<&str>,
        slot: Option<&str>,
        active: bool,
    ) -> Result<(), CredentialError> {
        if let Some(account) = account {
            validate_account(account)?;
        }
        if active && account.is_none() {
            return Err(CredentialError::InvalidFormat);
        }
        validate_slot(slot)?;
        if slot.is_some() && account.is_none() {
            return Err(CredentialError::InvalidFormat);
        }
        let reference = AccountReference {
            version: if slot.is_some() { 2 } else { 1 },
            origin: self.origin.clone(),
            account: account.map(str::to_owned),
            active,
            slot: slot.map(str::to_owned),
        };
        let Some(path) = &self.path else {
            *self.memory.lock().unwrap() = Some(reference);
            return Ok(());
        };
        use std::io::Write;
        let directory = path.parent().ok_or(CredentialError::Storage)?;
        std::fs::create_dir_all(directory).map_err(|_| CredentialError::Storage)?;
        let mut temp =
            tempfile::NamedTempFile::new_in(directory).map_err(|_| CredentialError::Storage)?;
        serde_json::to_writer(&mut temp, &reference).map_err(|_| CredentialError::Storage)?;
        temp.flush().map_err(|_| CredentialError::Storage)?;
        temp.as_file()
            .sync_all()
            .map_err(|_| CredentialError::Storage)?;
        temp.persist(path).map_err(|_| CredentialError::Storage)?;
        #[cfg(unix)]
        std::fs::File::open(directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| CredentialError::Storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    #[test]
    fn credential_gate_queues_operations_behind_short_silent_reads() {
        use std::{sync::mpsc, time::Duration};
        for interactive in [false, true] {
            let gate = CredentialAccessGate::new();
            let held = gate.acquire(false, Duration::ZERO).unwrap();
            std::thread::scope(|scope| {
                let (started, starting) = mpsc::channel();
                let (finished, result) = mpsc::channel();
                let gate = &gate;
                scope.spawn(move || {
                    started.send(()).unwrap();
                    let acquired = gate.acquire(interactive, Duration::from_secs(2));
                    finished.send(acquired.is_ok()).unwrap();
                });
                starting.recv_timeout(Duration::from_secs(1)).unwrap();
                assert!(matches!(
                    result.recv_timeout(Duration::from_millis(20)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
                drop(held);
                assert!(result.recv_timeout(Duration::from_secs(1)).unwrap());
            });
            assert!(gate.acquire(false, Duration::ZERO).is_ok());
        }
    }

    #[test]
    fn credential_gate_bounds_waits_and_never_queues_system_authorization() {
        use std::time::Duration;
        let gate = CredentialAccessGate::new();
        let held = gate.acquire(false, Duration::ZERO).unwrap();
        assert!(matches!(
            gate.acquire(false, Duration::from_millis(5)),
            Err(CredentialError::Unavailable)
        ));
        assert!(matches!(
            gate.acquire(true, Duration::from_millis(5)),
            Err(CredentialError::Unavailable)
        ));
        drop(held);
        let held = gate.acquire(true, Duration::ZERO).unwrap();
        for interactive in [false, true] {
            assert!(matches!(
                gate.acquire(interactive, Duration::from_secs(30)),
                Err(CredentialError::Unavailable)
            ));
        }
        drop(held);
        assert!(gate.acquire(false, Duration::ZERO).is_ok());
    }

    #[test]
    fn native_existing_password_update_uses_only_an_item_handle_and_preserves_its_acl() {
        use std::cell::RefCell;
        // The lookup handle deliberately has no password or ACL payload.
        struct Handle(u64);
        let item = RefCell::new((41, "existing-private-acl", b"synthetic-old".to_vec()));
        let calls = RefCell::new(Vec::new());
        write_password_item(
            || {
                calls.borrow_mut().push("metadata");
                Ok(Some(Handle(41)))
            },
            |handle| {
                calls.borrow_mut().push("update");
                let mut item = item.borrow_mut();
                assert_eq!(item.0, handle.0);
                // The real adapter calls set_password on this same item and
                // supplies only new data, leaving its identity/ACL untouched.
                item.2 = b"synthetic-successor".to_vec();
                Ok(())
            },
            || panic!("an existing item must never be recreated"),
        )
        .unwrap();
        assert_eq!(*calls.borrow(), ["metadata", "update"]);
        assert_eq!(
            *item.borrow(),
            (41, "existing-private-acl", b"synthetic-successor".to_vec())
        );
    }

    #[test]
    fn native_missing_password_creates_once_without_loading_old_password_data() {
        use std::cell::RefCell;
        let calls = RefCell::new(Vec::new());
        write_password_item(
            || {
                calls.borrow_mut().push("metadata");
                Ok(None::<u64>)
            },
            |_| panic!("a missing item cannot be updated"),
            || {
                calls.borrow_mut().push("create");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*calls.borrow(), ["metadata", "create"]);
    }

    #[test]
    fn native_metadata_or_update_errors_never_delete_recreate_or_retry_a_password_item() {
        use std::cell::RefCell;
        let errors: [fn() -> CredentialError; 3] = [
            || CredentialError::InteractionRequired,
            || CredentialError::Unavailable,
            || CredentialError::Storage,
        ];
        for error in errors {
            let calls = RefCell::new(Vec::new());
            let result = write_password_item::<u64>(
                || {
                    calls.borrow_mut().push("metadata");
                    Err(error())
                },
                |_| panic!("failed metadata cannot authorize an update"),
                || panic!("failed metadata cannot authorize item creation"),
            );
            assert_eq!(result, Err(error()));
            assert_eq!(*calls.borrow(), ["metadata"]);
            let result = write_password_item(
                || Ok(Some(41_u64)),
                |id| {
                    assert_eq!(id, 41);
                    calls.borrow_mut().push("update");
                    Err(error())
                },
                || panic!("failed update must preserve the old item, not recreate it"),
            );
            assert_eq!(result, Err(error()));
            assert_eq!(*calls.borrow(), ["metadata", "update"]);
        }
    }
    #[derive(Default)]
    struct MemoryBackend {
        items: Mutex<HashMap<(String, String), Vec<u8>>>,
        fail_write: Mutex<bool>,
        fail_delete: Mutex<bool>,
    }
    impl CredentialBackend for MemoryBackend {
        fn create(
            &self,
            service: &str,
            account: &str,
            bytes: &[u8],
        ) -> Result<(), CredentialError> {
            if *self.fail_write.lock().unwrap() {
                return Err(CredentialError::Storage);
            }
            let mut items = self.items.lock().unwrap();
            let key = (service.into(), account.into());
            if items.contains_key(&key) {
                return Err(CredentialError::Storage);
            }
            items.insert(key, bytes.to_vec());
            Ok(())
        }
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            if *self.fail_write.lock().unwrap() {
                return Err(CredentialError::Storage);
            }
            self.items
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            if *self.fail_delete.lock().unwrap() {
                return Err(CredentialError::Storage);
            }
            self.items
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    fn fixture() -> (CredentialStore<MemoryBackend>, SecretRecord) {
        (
            CredentialStore::with_backend(
                "https://osir.example",
                "com.osir.client.dev",
                MemoryBackend::default(),
            )
            .unwrap(),
            SecretRecord {
                access_token: "test-access".into(),
                refresh_token: "test-refresh".into(),
                expires_at: chrono::Utc::now().timestamp() + 3600,
                account_id: "42".into(),
                origin: "https://osir.example/".into(),
                scope: "account:read usage:read".into(),
                slot: None,
            },
        )
    }
    #[test]
    fn expired_access_is_available_only_to_refresh_recovery_and_still_bound() {
        let (store, record) = fixture();
        store.store(&record).unwrap();
        let key = (store.service.clone(), store.item("42").unwrap());
        let mut items = store.backend.items.lock().unwrap();
        let mut wire: serde_json::Value = serde_json::from_slice(&items[&key]).unwrap();
        wire["expires_at"] = serde_json::json!(1);
        items.insert(key.clone(), serde_json::to_vec(&wire).unwrap());
        drop(items);
        assert!(matches!(store.load("42"), Err(CredentialError::Expired)));
        assert_eq!(store.load_for_refresh("42").unwrap().unwrap().expires_at, 1);
        wire["account_id"] = serde_json::json!("43");
        store
            .backend
            .items
            .lock()
            .unwrap()
            .insert(key.clone(), serde_json::to_vec(&wire).unwrap());
        assert!(matches!(
            store.load_for_refresh("42"),
            Err(CredentialError::AccountMismatch)
        ));
        wire["account_id"] = serde_json::json!("42");
        wire["origin"] = serde_json::json!("https://other.example");
        store
            .backend
            .items
            .lock()
            .unwrap()
            .insert(key, serde_json::to_vec(&wire).unwrap());
        assert!(matches!(
            store.load_for_refresh("42"),
            Err(CredentialError::OriginMismatch)
        ));
    }
    #[test]
    fn journal_persists_only_bound_metadata_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let journal = SessionJournal::persistent("https://osir.example".into(), directory.path());
        journal.write(Some("42"), true).unwrap();
        let restarted = SessionJournal::persistent("https://osir.example".into(), directory.path());
        let reference = restarted.read().unwrap().unwrap();
        assert!(reference.active);
        assert_eq!(reference.account.as_deref(), Some("42"));
        let path = journal.path.as_ref().unwrap();
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(!contents.contains("token"));
        assert!(
            SessionJournal::persistent("https://other.example".into(), directory.path())
                .read()
                .is_err()
        );
        journal.write(Some("42"), false).unwrap();
        assert!(!restarted.read().unwrap().unwrap().active);
        std::fs::write(path, b"{}").unwrap();
        assert!(restarted.read().is_err());
    }
    #[test]
    fn fresh_slot_add_is_isolated_from_legacy_and_cannot_overwrite_a_collision() {
        let (store, mut fresh) = fixture();
        store.store(&fresh).unwrap();
        let legacy_bytes = store.backend.items.lock().unwrap().clone();
        fresh.slot = Some("a".repeat(32));
        fresh.access_token = "fresh-access".into();
        store.create(&fresh).unwrap();
        let saved = store.backend.items.lock().unwrap().clone();
        for (item, bytes) in legacy_bytes {
            assert_eq!(saved[&item], bytes);
        }
        assert_eq!(saved.len(), 2);
        assert_eq!(
            store
                .load_slot_for_refresh("42", fresh.slot.as_deref())
                .unwrap()
                .unwrap()
                .access_token,
            "fresh-access"
        );
        fresh.access_token = "must-not-overwrite".into();
        assert_eq!(store.create(&fresh), Err(CredentialError::Storage));
        assert_eq!(*store.backend.items.lock().unwrap(), saved);
        store.delete_slot("42", fresh.slot.as_deref()).unwrap();
        assert_eq!(
            store.load("42").unwrap().unwrap().access_token,
            "test-access"
        );
    }
    #[test]
    fn fresh_login_can_unlock_storage_without_reading_or_replacing_existing_items() {
        struct LockedStore;
        impl CredentialBackend for LockedStore {
            fn create(&self, _: &str, _: &str, _: &[u8]) -> Result<(), CredentialError> {
                Err(CredentialError::InteractionRequired)
            }
            fn create_interactive(
                &self,
                _: &str,
                _: &str,
                _: &[u8],
            ) -> Result<(), CredentialError> {
                Ok(())
            }
            fn write(&self, _: &str, _: &str, _: &[u8]) -> Result<(), CredentialError> {
                panic!("fresh login must remain add-only")
            }
            fn read(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, CredentialError> {
                panic!("fresh login must not inspect old credentials")
            }
            fn remove(&self, _: &str, _: &str) -> Result<(), CredentialError> {
                panic!("fresh login must not delete old credentials")
            }
        }
        let (_, mut record) = fixture();
        record.slot = Some("d".repeat(32));
        let store =
            CredentialStore::with_backend("https://osir.example/", "test.osir.dev", LockedStore)
                .unwrap();
        assert_eq!(
            store.create(&record),
            Err(CredentialError::InteractionRequired)
        );
        store.create_for_user(&record).unwrap();
    }
    #[test]
    fn slot_and_version_are_bound_inside_the_keychain_record() {
        let (store, mut record) = fixture();
        record.slot = Some("b".repeat(32));
        store.create(&record).unwrap();
        let item = (
            store.service.clone(),
            store.item_at("42", record.slot.as_deref()).unwrap(),
        );
        let original = store.backend.items.lock().unwrap()[&item].clone();
        for (field, value) in [
            ("slot", serde_json::json!("c".repeat(32))),
            ("version", serde_json::json!(1)),
            ("slot", serde_json::Value::Null),
        ] {
            let mut wire: serde_json::Value = serde_json::from_slice(&original).unwrap();
            wire[field] = value;
            store
                .backend
                .items
                .lock()
                .unwrap()
                .insert(item.clone(), serde_json::to_vec(&wire).unwrap());
            assert!(store
                .load_slot_for_refresh("42", record.slot.as_deref())
                .is_err());
        }
        for invalid in ["", "../bad", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "a"] {
            assert!(store.load_slot_for_refresh("42", Some(invalid)).is_err());
        }
        assert!(store.load("42").unwrap().is_none());
    }
    #[test]
    fn journal_v2_roundtrips_slot_but_rejects_ambiguous_or_malformed_references() {
        let directory = tempfile::tempdir().unwrap();
        let journal = SessionJournal::persistent("https://osir.example".into(), directory.path());
        let slot = "d".repeat(32);
        journal.write_slot(Some("42"), Some(&slot), true).unwrap();
        let reference = journal.read().unwrap().unwrap();
        assert!(reference.active);
        assert_eq!(reference.slot.as_deref(), Some(slot.as_str()));
        let path = journal.path.as_ref().unwrap();
        let original = std::fs::read(path).unwrap();
        let text = std::str::from_utf8(&original).unwrap();
        assert!(!text.contains("access_token") && !text.contains("refresh_token"));
        for (field, value) in [
            ("version", serde_json::json!(1)),
            ("slot", serde_json::json!("../invalid")),
            ("account", serde_json::Value::Null),
        ] {
            let mut wire: serde_json::Value = serde_json::from_slice(&original).unwrap();
            wire[field] = value;
            std::fs::write(path, serde_json::to_vec(&wire).unwrap()).unwrap();
            assert!(journal.read().is_err());
        }
        journal.write(Some("42"), true).unwrap();
        assert!(journal.read().unwrap().unwrap().slot.is_none());
        journal.write_slot(Some("42"), Some(&slot), false).unwrap();
        assert!(!journal.read().unwrap().unwrap().active);
    }
    #[test]
    fn saves_replaces_and_deletes_without_real_keychain() {
        let (store, mut record) = fixture();
        assert!(store.load("42").unwrap().is_none());
        store.store(&record).unwrap();
        record.access_token = "rotated".into();
        store.store(&record).unwrap();
        let loaded = store.load("42").unwrap().unwrap();
        assert!(loaded.access_token == record.access_token);
        assert!(loaded.refresh_token == record.refresh_token);
        assert_eq!(loaded.expires_at, record.expires_at);
        assert_eq!(loaded.scope, record.scope);
        assert!(store.load("43").unwrap().is_none());
        store.delete("42").unwrap();
        store.delete("42").unwrap();
        assert!(store.load("42").unwrap().is_none());
    }
    #[test]
    fn failed_write_retains_previous_record_and_delete_failure_is_visible() {
        let (store, mut record) = fixture();
        store.store(&record).unwrap();
        *store.backend.fail_write.lock().unwrap() = true;
        record.access_token = "replacement".into();
        assert_eq!(store.store(&record), Err(CredentialError::Storage));
        assert!(store.load("42").unwrap().unwrap().access_token == "test-access");
        *store.backend.fail_delete.lock().unwrap() = true;
        assert_eq!(store.delete("42"), Err(CredentialError::Storage));
        assert!(store.load("42").unwrap().is_some());
    }
    #[test]
    fn rejects_wrong_origin_expiry_and_tampered_account() {
        let (store, mut record) = fixture();
        record.origin = "https://other.example".into();
        assert_eq!(store.store(&record), Err(CredentialError::OriginMismatch));
        record.origin = store.origin.clone();
        record.expires_at = chrono::Utc::now().timestamp();
        assert_eq!(store.store(&record), Err(CredentialError::Expired));
        for (field, value, expected) in [
            (
                "account_id",
                serde_json::json!("43"),
                CredentialError::AccountMismatch,
            ),
            (
                "origin",
                serde_json::json!("https://other.example"),
                CredentialError::OriginMismatch,
            ),
            ("expires_at", serde_json::json!(1), CredentialError::Expired),
            (
                "version",
                serde_json::json!(2),
                CredentialError::InvalidFormat,
            ),
            (
                "access_token",
                serde_json::json!("bad header"),
                CredentialError::InvalidFormat,
            ),
        ] {
            let (_, valid) = fixture();
            store.store(&valid).unwrap();
            let key = (store.service.clone(), store.item("42").unwrap());
            let mut items = store.backend.items.lock().unwrap();
            let mut wire: serde_json::Value = serde_json::from_slice(&items[&key]).unwrap();
            wire[field] = value;
            items.insert(key, serde_json::to_vec(&wire).unwrap());
            drop(items);
            assert!(matches!(store.load("42"), Err(error) if error == expected));
        }
        store
            .backend
            .write(
                &store.service,
                &store.item("42").unwrap(),
                b"malformed-secret",
            )
            .unwrap();
        assert!(matches!(
            store.load("42"),
            Err(CredentialError::InvalidFormat)
        ));
    }
    #[test]
    fn isolates_identity_origin_and_build() {
        let (store, _) = fixture();
        let other = CredentialStore::with_backend(
            "https://other.example",
            "com.osir.client.dev",
            MemoryBackend::default(),
        )
        .unwrap();
        assert_ne!(store.item("42").unwrap(), other.item("42").unwrap());
        let production = CredentialStore::with_backend(
            "https://osir.example",
            "com.osir.client",
            MemoryBackend::default(),
        )
        .unwrap();
        assert_ne!(store.service, production.service);
        assert_eq!(store.service.ends_with(".debug"), cfg!(debug_assertions));
        for origin in [
            "http://osir.example",
            "https://u:p@osir.example",
            "https://osir.example/api",
            "https://osir.example?x=1",
            "https://osir.example/#x",
        ] {
            assert!(canonical_origin(origin).is_err());
        }
        assert_eq!(
            canonical_origin("https://OSIR.example:443/").unwrap(),
            store.origin
        );
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn native_backend_never_falls_back_to_plaintext() {
        assert_eq!(
            NativeKeychain.write("test", "test", b"fake"),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            NativeKeychain.read("test", "test"),
            Err(CredentialError::Unavailable)
        );
        assert_eq!(
            NativeKeychain.remove("test", "test"),
            Err(CredentialError::Unavailable)
        );
    }
}
