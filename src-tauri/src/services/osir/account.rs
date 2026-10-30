//! Account, balance, key, group, subscription, and usage service boundary.
//! Concrete DTOs and API methods will be added against the deployed OSIR API
//! contract; empty or unavailable responses must never be presented as zero data.

use serde::{Deserialize, Serialize};

/// Explicit allowlist: nested API keys and server-only account fields never
/// survive projection into the desktop account summary.
#[derive(Clone, Deserialize, Serialize)]
pub struct AccountProfile {
    pub id: i64,
    pub email: String,
    pub username: String,
    pub balance: serde_json::Number,
}

/// Wire key has no Debug or Serialize implementation. Never pass it to IPC.
#[derive(Deserialize)]
pub struct WireKey {
    pub id: i64,
    pub name: String,
    key: String,
    pub group_id: Option<i64>,
    pub group: Option<KeyGroup>,
    pub status: String,
    pub quota: serde_json::Number,
    pub quota_used: serde_json::Number,
    pub expires_at: Option<String>,
}

/// Minimal account relation; never deserialize or project group credentials.
#[derive(Clone, Deserialize, Serialize)]
pub struct KeyGroup {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KeyMetadata {
    pub id: i64,
    pub name: String,
    pub masked_key: String,
    pub group_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<KeyGroup>,
    pub status: String,
    pub quota: serde_json::Number,
    pub quota_used: serde_json::Number,
    pub expires_at: Option<String>,
}

impl WireKey {
    /// Move a verified key into native storage/application only. This type and
    /// operation must never be exposed through a generic IPC serializer.
    pub(crate) fn into_active_secret(
        self,
    ) -> Result<super::key_vault::NativeKeySecret, super::client::AccountError> {
        if self.id <= 0
            || self.status != "active"
            || self.group_id.is_some_and(|id| id <= 0)
            || self.key.is_empty()
            || self.key.len() > 16384
            || !self.key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(super::client::AccountError::InvalidResponse);
        }
        if let Some(expires) = self.expires_at.as_deref() {
            let expires = chrono::DateTime::parse_from_rfc3339(expires)
                .map_err(|_| super::client::AccountError::InvalidResponse)?;
            if expires.timestamp() <= chrono::Utc::now().timestamp() {
                return Err(super::client::AccountError::Forbidden);
            }
        }
        super::key_vault::NativeKeySecret::new(self.key)
            .map_err(|_| super::client::AccountError::InvalidResponse)
    }

    pub fn metadata(&self) -> KeyMetadata {
        // Very short values get no suffix, avoiding disclosure of the full key.
        let masked_key = if self.key.chars().count() > 8 {
            let suffix: String = self
                .key
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("••••{suffix}")
        } else {
            "••••".to_string()
        };
        KeyMetadata {
            id: self.id,
            name: self.name.clone(),
            masked_key,
            group_id: self.group_id,
            group: self
                .group
                .as_ref()
                .filter(|group| group.id > 0 && Some(group.id) == self.group_id)
                .cloned(),
            status: self.status.clone(),
            quota: self.quota.clone(),
            quota_used: self.quota_used.clone(),
            expires_at: self.expires_at.clone(),
        }
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    fn fixture(status: &str, group: serde_json::Value, expiry: serde_json::Value) -> WireKey {
        serde_json::from_value(serde_json::json!({
            "id": 3, "name": "work", "key": "sk-fixture-private-key",
            "group_id": group, "status": status, "quota": 0, "quota_used": 0,
            "expires_at": expiry
        }))
        .unwrap()
    }

    #[test]
    fn native_consumption_preserves_real_ungrouped_keys() {
        assert!(
            fixture("active", serde_json::Value::Null, serde_json::Value::Null)
                .into_active_secret()
                .is_ok()
        );
        assert!(
            fixture("active", serde_json::json!(7), serde_json::Value::Null)
                .into_active_secret()
                .is_ok()
        );
    }

    #[test]
    fn native_consumption_rejects_disabled_expired_and_malformed_keys() {
        for key in [
            fixture("disabled", serde_json::json!(7), serde_json::Value::Null),
            fixture("active", serde_json::json!(0), serde_json::Value::Null),
            fixture(
                "active",
                serde_json::json!(7),
                serde_json::json!("2000-01-01T00:00:00Z"),
            ),
            fixture("active", serde_json::json!(7), serde_json::json!("invalid")),
        ] {
            assert!(key.into_active_secret().is_err());
        }
        for value in ["", "bad\nkey", "含非ASCII"] {
            let mut key = fixture("active", serde_json::json!(7), serde_json::Value::Null);
            key.key = value.into();
            assert!(key.into_active_secret().is_err());
        }
    }

    #[test]
    fn account_projection_removes_nested_secrets() {
        let profile: AccountProfile = serde_json::from_value(serde_json::json!({
            "id": 1, "email": "test@example.com", "username": "test",
            "balance": 0, "api_keys": [{"key": "secret-value"}],
            "refresh_token": "private-refresh"
        }))
        .unwrap();
        let serialized = serde_json::to_string(&profile).unwrap();
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("refresh"));
        assert!(serialized.contains("balance"));
    }

    #[test]
    fn key_projection_preserves_group_and_unlimited_quota_without_secret() {
        for secret in ["sk-private-abcdefgh", "short"] {
            let wire: WireKey = serde_json::from_value(serde_json::json!({
                "id": 3, "name": "work", "key": secret, "group_id": 7,
                "status": "active", "quota": 0, "quota_used": 12.3,
                "expires_at": null, "user": {"api_keys": [{"key": secret}]}
            }))
            .unwrap();
            let safe = wire.metadata();
            assert_eq!(safe.group_id, Some(7));
            assert_eq!(safe.quota.as_u64(), Some(0));
            assert!(!serde_json::to_string(&safe).unwrap().contains(secret));
        }
    }

    #[test]
    fn key_group_projection_requires_the_actual_key_group_and_is_minimal() {
        for (group_id, nested_id, expected) in [
            (Some(7), 7, true),
            (Some(7), 8, false),
            (None, 7, false),
            (Some(0), 0, false),
        ] {
            let mut wire = serde_json::to_value(
                fixture(
                    "active",
                    serde_json::json!(group_id),
                    serde_json::Value::Null,
                )
                .metadata(),
            )
            .unwrap();
            // Synthetic wire data only, never a real account response.
            wire["key"] = serde_json::json!("synthetic-key-never-serialized");
            wire["group"] = serde_json::json!({
                "id": nested_id, "name": "Group",
                "accounts": [{"credentials": "synthetic-private"}],
            });
            let wire: WireKey = serde_json::from_value(wire).unwrap();
            let safe = serde_json::to_value(wire.metadata()).unwrap();
            assert_eq!(safe.get("group").is_some(), expected);
            if expected {
                assert_eq!(safe["group"], serde_json::json!({"id": 7, "name": "Group"}));
            }
            assert!(!safe.to_string().contains("synthetic-private"));
            assert!(!safe.to_string().contains("synthetic-key-never-serialized"));
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountServiceStatus {
    pub connected: bool,
    pub available: bool,
    pub message: String,
}

impl Default for AccountServiceStatus {
    fn default() -> Self {
        Self {
            connected: false,
            available: false,
            message: "登录后同步账户资源；当前授权服务尚未接通".to_string(),
        }
    }
}

// Explicit user DTO schemas. Unknown server relations never survive.
use super::resources::{Field, Shape};

pub(super) const GROUP_FIELDS: &[Field] = &[
    Field::new("id", Shape::Integer, false),
    Field::new("name", Shape::Text, false),
    Field::new("description", Shape::Text, false),
    Field::new("platform", Shape::Text, false),
    Field::new("rate_multiplier", Shape::Number, false),
    Field::new("is_exclusive", Shape::Bool, false),
    Field::new("status", Shape::Text, false),
    Field::new("subscription_type", Shape::Text, false),
    Field::new("daily_limit_usd", Shape::Number, true),
    Field::new("weekly_limit_usd", Shape::Number, true),
    Field::new("monthly_limit_usd", Shape::Number, true),
    Field::new("allow_image_generation", Shape::Bool, false),
    Field::new("allow_batch_image_generation", Shape::Bool, false),
    Field::new("image_rate_independent", Shape::Bool, false),
    Field::new("image_rate_multiplier", Shape::Number, false),
    Field::new("batch_image_discount_multiplier", Shape::Number, false),
    Field::new("batch_image_hold_multiplier", Shape::Number, false),
    Field::new("video_rate_independent", Shape::Bool, false),
    Field::new("video_rate_multiplier", Shape::Number, false),
    Field::new("peak_rate_enabled", Shape::Bool, false),
    Field::new("peak_start", Shape::Text, false),
    Field::new("peak_end", Shape::Text, false),
    Field::new("peak_rate_multiplier", Shape::Number, false),
    Field::new("image_price_1k", Shape::Number, true),
    Field::new("image_price_2k", Shape::Number, true),
    Field::new("image_price_4k", Shape::Number, true),
    Field::new("video_price_480p", Shape::Number, true),
    Field::new("video_price_720p", Shape::Number, true),
    Field::new("video_price_1080p", Shape::Number, true),
    Field::new("claude_code_only", Shape::Bool, false),
    Field::new("fallback_group_id", Shape::Integer, true),
    Field::new("fallback_group_id_on_invalid_request", Shape::Integer, true),
    Field::new("allow_messages_dispatch", Shape::Bool, false),
    Field::new("require_oauth_only", Shape::Bool, false),
    Field::new("require_privacy_set", Shape::Bool, false),
    Field::new("rpm_limit", Shape::Integer, false),
    Field::new("created_at", Shape::Text, false),
    Field::new("updated_at", Shape::Text, false),
];

pub(super) const KEY_FIELDS: &[Field] = &[
    Field::new("key", Shape::MaskedKey, false),
    Field::new("id", Shape::Integer, false),
    Field::new("name", Shape::Text, false),
    Field::new("group_id", Shape::Integer, true),
    Field::new("status", Shape::Text, false),
    Field::new("last_used_at", Shape::Text, true),
    Field::new("quota", Shape::Number, false),
    Field::new("quota_used", Shape::Number, false),
    Field::new("expires_at", Shape::Text, true),
    Field::new("created_at", Shape::Text, false),
    Field::new("updated_at", Shape::Text, false),
    Field::new("current_concurrency", Shape::Integer, false),
    Field::new("rate_limit_5h", Shape::Number, false),
    Field::new("rate_limit_1d", Shape::Number, false),
    Field::new("rate_limit_7d", Shape::Number, false),
    Field::new("usage_5h", Shape::Number, false),
    Field::new("usage_1d", Shape::Number, false),
    Field::new("usage_7d", Shape::Number, false),
    Field::new("window_5h_start", Shape::Text, true),
    Field::new("window_1d_start", Shape::Text, true),
    Field::new("window_7d_start", Shape::Text, true),
    Field::new("reset_5h_at", Shape::Text, true),
    Field::new("reset_1d_at", Shape::Text, true),
    Field::new("reset_7d_at", Shape::Text, true),
    Field::new("group", Shape::Object(GROUP_FIELDS), true),
];

pub(super) const SUBSCRIPTION_FIELDS: &[Field] = &[
    Field::new("id", Shape::Integer, false),
    Field::new("group_id", Shape::Integer, false),
    Field::new("starts_at", Shape::Text, false),
    Field::new("expires_at", Shape::Text, false),
    Field::new("status", Shape::Text, false),
    Field::new("daily_window_start", Shape::Text, true),
    Field::new("weekly_window_start", Shape::Text, true),
    Field::new("monthly_window_start", Shape::Text, true),
    Field::new("daily_usage_usd", Shape::Number, false),
    Field::new("weekly_usage_usd", Shape::Number, false),
    Field::new("monthly_usage_usd", Shape::Number, false),
    Field::new("created_at", Shape::Text, false),
    Field::new("updated_at", Shape::Text, false),
    Field::new("revoked_at", Shape::Text, true),
    Field::new("group", Shape::Object(GROUP_FIELDS), true),
];

/// Only GET /keys filters actually consumed by the user handler/repository.
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeysQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub search: Option<String>,
    pub status: Option<String>,
    pub group_id: Option<i64>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}
pub fn validate_keys_query(
    value: serde_json::Value,
) -> Result<Vec<(String, String)>, &'static str> {
    let q: KeysQuery = serde_json::from_value(value).map_err(|_| "密钥查询参数无效")?;
    if q.page.is_some_and(|v| v == 0 || v > 1_000_000)
        || q.page_size.is_some_and(|v| v == 0 || v > 100)
        || q.group_id.is_some_and(|v| v <= 0)
        || q.status
            .as_deref()
            .is_some_and(|v| !matches!(v, "active" | "inactive" | "quota_exhausted" | "expired"))
        || q.sort_by.as_deref().is_some_and(|v| {
            !matches!(
                v,
                "id" | "name" | "status" | "expires_at" | "last_used_at" | "created_at"
            )
        })
        || q.sort_order
            .as_deref()
            .is_some_and(|v| !matches!(v, "asc" | "desc"))
    {
        return Err("密钥查询参数无效");
    }
    let value = serde_json::to_value(q).map_err(|_| "密钥查询参数无效")?;
    let mut out = Vec::new();
    for (key, value) in value.as_object().ok_or("密钥查询参数无效")? {
        if value.is_null() {
            continue;
        }
        let text = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
            return Err("密钥查询参数无效");
        }
        out.push((key.clone(), text));
    }
    Ok(out)
}
