use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopAuthStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopAuthCapability {
    pub status: DesktopAuthStatus,
    pub reason: String,
}

impl Default for DesktopAuthCapability {
    fn default() -> Self {
        Self {
            status: DesktopAuthStatus::Unavailable,
            reason: "OSIR 通用桌面授权接口尚未接通".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRecovery {
    Pending,
    Restoring,
    UnlockRequired,
    Retryable,
    Blocked,
    Idle,
}

/// Safe IPC state. No access/refresh token, verifier, code or raw HTTP error.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub connected: bool,
    pub recovery: SessionRecovery,
    /// Derived from the active grant; false while disconnected.
    pub can_create_keys: bool,
    pub generation: u64,
    pub profile: Option<super::account::AccountProfile>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutReport {
    pub generation: u64,
    pub local_cleared: bool,
    pub remote_revoked: bool,
}
