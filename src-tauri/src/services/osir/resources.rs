//! Pure, endpoint-specific projection of the account client's unwrapped data.
//! Register this module in osir/mod.rs before use. No network or credential access.
use super::{account, client::AccountResource, usage};
use serde_json::{Map, Value};

/// Fixed messages only: never includes server payloads, paths, or serde errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeError {
    InvalidResponse,
    InvalidQuery,
}
impl std::fmt::Display for SafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidResponse => "账户数据格式不兼容",
            Self::InvalidQuery => "账户资源查询参数无效",
        })
    }
}
impl std::error::Error for SafeError {}

/// A closed schema language: no arbitrary JSON subtree can be copied.
#[derive(Clone, Copy)]
pub(super) enum Shape {
    Text,
    MaskedKey,
    Integer,
    Number,
    Bool,
    Object(&'static [Field]),
    Array(&'static [Field]),
    ImageCounts,
}
pub(super) struct Field {
    name: &'static str,
    shape: Shape,
    nullable: bool,
}
impl Field {
    pub(super) const fn new(name: &'static str, shape: Shape, nullable: bool) -> Self {
        Self {
            name,
            shape,
            nullable,
        }
    }
}

fn object(value: Value, fields: &[Field]) -> Result<Value, SafeError> {
    let input = value.as_object().ok_or(SafeError::InvalidResponse)?;
    // Missing fields stay absent, allowing unavailable UI rather than fake zeroes.
    // An empty schema match is rejected instead of accepting an API error object.
    let mut out = Map::new();
    for field in fields {
        if let Some(value) = input.get(field.name) {
            let safe = if value.is_null() && field.nullable {
                Value::Null
            } else {
                project(value.clone(), field.shape)?
            };
            out.insert(
                if matches!(field.shape, Shape::MaskedKey) {
                    "masked_key"
                } else {
                    field.name
                }
                .into(),
                safe,
            );
        }
    }
    if out.is_empty() {
        return Err(SafeError::InvalidResponse);
    }
    Ok(Value::Object(out))
}
fn project(value: Value, shape: Shape) -> Result<Value, SafeError> {
    match shape {
        Shape::Text if value.is_string() => Ok(value),
        Shape::MaskedKey if value.is_string() => Ok(Value::String("••••".into())),
        Shape::Integer if value.is_i64() || value.is_u64() => Ok(value),
        Shape::Number if value.is_number() => Ok(value),
        Shape::Bool if value.is_boolean() => Ok(value),
        Shape::Object(fields) => object(value, fields),
        Shape::Array(fields) => value
            .as_array()
            .ok_or(SafeError::InvalidResponse)?
            .iter()
            .cloned()
            .map(|v| object(v, fields))
            .collect(),
        // Numeric-only dictionary: reject non-dimension labels, so dynamic keys
        // cannot smuggle a secret. These are the server's image billing buckets.
        Shape::ImageCounts => {
            let input = value.as_object().ok_or(SafeError::InvalidResponse)?;
            let mut out = Map::new();
            for (key, count) in input {
                if !matches!(key.as_str(), "1K" | "2K" | "4K") {
                    return Err(SafeError::InvalidResponse);
                }
                out.insert(key.clone(), project(count.clone(), Shape::Integer)?);
            }
            Ok(Value::Object(out))
        }
        _ => Err(SafeError::InvalidResponse),
    }
}
fn page(value: Value, fields: &'static [Field]) -> Result<Value, SafeError> {
    let input = value.as_object().ok_or(SafeError::InvalidResponse)?;
    let mut out = Map::new();
    let items = input.get("items").ok_or(SafeError::InvalidResponse)?;
    out.insert(
        "items".into(),
        project(items.clone(), Shape::Array(fields))?,
    );
    for name in ["total", "page", "page_size"] {
        let n = input
            .get(name)
            .and_then(Value::as_u64)
            .ok_or(SafeError::InvalidResponse)?;
        if name != "total" && n == 0 {
            return Err(SafeError::InvalidResponse);
        }
        out.insert(name.into(), Value::from(n));
    }
    Ok(Value::Object(out))
}

const PROFILE: &[Field] = &[
    Field::new("id", Shape::Integer, false),
    Field::new("email", Shape::Text, false),
    Field::new("username", Shape::Text, false),
    Field::new("balance", Shape::Number, false),
];
const ANNOUNCEMENT: &[Field] = &[
    Field::new("id", Shape::Integer, false),
    Field::new("title", Shape::Text, false),
    Field::new("content", Shape::Text, false),
    Field::new("notify_mode", Shape::Text, false),
    Field::new("starts_at", Shape::Text, true),
    Field::new("ends_at", Shape::Text, true),
    Field::new("read_at", Shape::Text, true),
    Field::new("created_at", Shape::Text, false),
    Field::new("updated_at", Shape::Text, false),
];
const SNAPSHOT: &[Field] = &[
    Field::new("generated_at", Shape::Text, false),
    Field::new("start_date", Shape::Text, false),
    Field::new("end_date", Shape::Text, false),
    Field::new("granularity", Shape::Text, false),
    Field::new("trend", Shape::Array(usage::TREND_FIELDS), true),
    Field::new("models", Shape::Array(usage::MODEL_FIELDS), true),
    Field::new("groups", Shape::Array(usage::GROUP_STATS_FIELDS), true),
];

/// The input is AccountClient::get::<Value>()'s DATA, not its HTTP envelope.
/// Pagination is exactly {items,total,page,page_size}; other endpoints retain
/// their real array/object/null shapes. Only the fixed AccountResource is accepted.
pub fn sanitize(resource: AccountResource, value: Value) -> Result<Value, SafeError> {
    match resource {
        AccountResource::Profile => object(value, PROFILE),
        AccountResource::Keys => page(value, account::KEY_FIELDS),
        AccountResource::Groups => project(value, Shape::Array(account::GROUP_FIELDS)),
        AccountResource::GroupRates => {
            // nil map is a legitimate unavailable rate table, not an empty map.
            if value.is_null() {
                return Ok(Value::Null);
            }
            let input = value.as_object().ok_or(SafeError::InvalidResponse)?;
            let mut out = Map::new();
            for (id, rate) in input {
                let parsed = id.parse::<i64>().map_err(|_| SafeError::InvalidResponse)?;
                if parsed <= 0 || parsed.to_string() != *id {
                    return Err(SafeError::InvalidResponse);
                }
                out.insert(id.clone(), project(rate.clone(), Shape::Number)?);
            }
            Ok(Value::Object(out))
        }
        AccountResource::Subscriptions => {
            project(value, Shape::Array(account::SUBSCRIPTION_FIELDS))
        }
        AccountResource::Usage => page(value, usage::USAGE_FIELDS),
        AccountResource::UsageStats => object(value, usage::STATS_FIELDS),
        AccountResource::UsageSnapshot => object(value, SNAPSHOT),
        AccountResource::Announcements => project(value, Shape::Array(ANNOUNCEMENT)),
    }
}

/// Endpoint-specific query validation. Rejects unknown/ignored selectors rather
/// than silently broadening a query. Values use JSON types (not string booleans).
pub fn validate_query(
    resource: AccountResource,
    value: Value,
) -> Result<Vec<(String, String)>, SafeError> {
    let input = value.as_object().ok_or(SafeError::InvalidQuery)?;
    match resource {
        AccountResource::Usage | AccountResource::UsageStats | AccountResource::UsageSnapshot => {
            let query: usage::UsageQuery =
                serde_json::from_value(value).map_err(|_| SafeError::InvalidQuery)?;
            query
                .validate_for(resource)
                .map_err(|_| SafeError::InvalidQuery)
        }
        AccountResource::Keys => {
            account::validate_keys_query(value).map_err(|_| SafeError::InvalidQuery)
        }
        AccountResource::Announcements => {
            if input.keys().any(|k| k != "unread_only") {
                return Err(SafeError::InvalidQuery);
            }
            match input.get("unread_only") {
                None | Some(Value::Null) => Ok(vec![]),
                Some(Value::Bool(b)) => Ok(vec![(
                    "unread_only".into(),
                    if *b { "1" } else { "0" }.into(),
                )]),
                _ => Err(SafeError::InvalidQuery),
            }
        }
        _ if input.is_empty() => Ok(vec![]),
        _ => Err(SafeError::InvalidQuery),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recursive_projection_never_copies_secret_relations() {
        let row = json!({"id":1,"model":"m", "input_tokens":9,
            "api_key":{"id":2,"key":"SECRET", "group_id":3,
                "user":{"token":"SECRET"}, "group":{"id":3,"accounts":[{"credentials":"SECRET"}]}},
            "subscription":{"id":4,"user":{"key":"SECRET"},"group":{"id":3,"account":{"key":"SECRET"}}},
            "account":{"credentials":"SECRET"},"token":"SECRET"});
        let out = sanitize(
            AccountResource::Usage,
            json!({"items":[row],"total":1,"page":1,"page_size":20,"token":"SECRET"}),
        )
        .unwrap();
        assert_eq!(out.as_object().unwrap().len(), 4);
        assert_eq!(out["items"][0]["api_key"]["group_id"], 3);
        assert_eq!(out["items"][0]["input_tokens"], 9);
        assert!(!out.to_string().contains("SECRET"));
        assert!(out["items"][0]["api_key"].get("key").is_none());
        assert_eq!(out["items"][0]["api_key"]["masked_key"], "••••");
    }

    #[test]
    fn scalar_slots_cannot_smuggle_objects_or_arrays() {
        for field in account::GROUP_FIELDS {
            assert!(sanitize(
                AccountResource::Groups,
                json!([{field.name:{"token":"SECRET"}}])
            )
            .is_err());
        }
        for field in usage::USAGE_FIELDS {
            if matches!(
                field.shape,
                Shape::Text | Shape::Integer | Shape::Number | Shape::Bool
            ) {
                assert!(sanitize(AccountResource::Usage,json!({"items":[{field.name:[{"key":"SECRET"}]}],"total":1,"page":1,"page_size":20})).is_err());
            }
        }
        let err = sanitize(AccountResource::Keys, json!({"message":"SECRET"})).unwrap_err();
        assert!(!format!("{err:?} {err}").contains("SECRET"));
    }

    #[test]
    fn all_usage_billing_token_image_and_aggregate_fields_survive() {
        let mut row = Map::new();
        for field in usage::USAGE_FIELDS {
            let value = match field.shape {
                Shape::Text => json!("fixture"),
                Shape::Integer => json!(12),
                Shape::Number => json!(0.123456789),
                Shape::Bool => json!(false),
                Shape::ImageCounts => json!({"1K":1,"2K":2,"4K":3}),
                _ => Value::Null,
            };
            row.insert(field.name.into(), value);
        }
        let out = sanitize(
            AccountResource::Usage,
            json!({"items":[row.clone()],"total":1,"page":1,"page_size":20}),
        )
        .unwrap();
        assert_eq!(out["items"][0], Value::Object(row));
        for (fields, resource) in [(usage::STATS_FIELDS, AccountResource::UsageStats)] {
            let mut data = Map::new();
            for f in fields {
                data.insert(
                    f.name.into(),
                    match f.shape {
                        Shape::Integer => json!(1),
                        Shape::Number => json!(1.23456789),
                        _ => json!([]),
                    },
                );
            }
            assert_eq!(
                sanitize(resource, Value::Object(data.clone())).unwrap(),
                Value::Object(data)
            );
        }
    }

    #[test]
    fn real_shapes_and_unavailable_values_are_preserved() {
        for resource in [
            AccountResource::Groups,
            AccountResource::Subscriptions,
            AccountResource::Announcements,
        ] {
            assert_eq!(sanitize(resource, json!([])).unwrap(), json!([]));
            assert!(sanitize(resource, json!({"items":[]})).is_err());
        }
        assert_eq!(
            sanitize(AccountResource::GroupRates, Value::Null).unwrap(),
            Value::Null
        );
        assert_eq!(
            sanitize(AccountResource::GroupRates, json!({"3":1.25})).unwrap(),
            json!({"3":1.25})
        );
        assert!(sanitize(AccountResource::GroupRates, json!({"SECRET":1})).is_err());
        assert!(sanitize(AccountResource::GroupRates, json!({"3":{"key":"SECRET"}})).is_err());
        let snapshot = json!({"generated_at":"now","trend":[{"date":"2026-10-27","actual_cost":1.2,"token":"SECRET"}],"models":[],"groups":null,"key":"SECRET"});
        let out = sanitize(AccountResource::UsageSnapshot, snapshot).unwrap();
        assert!(out.get("start_date").is_none());
        assert!(out["groups"].is_null());
        assert_eq!(out["trend"][0]["actual_cost"], 1.2);
        assert!(!out.to_string().contains("SECRET"));
    }

    #[test]
    fn queries_reject_ignored_parameters_and_keep_actual_mapping() {
        for (resource, q) in [
            (AccountResource::UsageStats, json!({"page":1})),
            (AccountResource::Usage, json!({"include_trend":true})),
            (AccountResource::UsageSnapshot, json!({"sort_by":"model"})),
            (AccountResource::Groups, json!({"page":1})),
            (AccountResource::Subscriptions, json!({"status":"active"})),
            (
                AccountResource::Announcements,
                json!({"unread_only":"true"}),
            ),
            (AccountResource::Usage, json!({"user_id":1})),
            (AccountResource::Usage, json!({"billing_mode":"SECRET"})),
        ] {
            assert!(validate_query(resource, q).is_err());
        }
        let q=validate_query(AccountResource::UsageSnapshot,json!({"group_id":2,"api_key_id":3,"request_type":"cyber","stream":true,"billing_mode":"video","timezone":"Asia/Shanghai","include_trend":false})).unwrap();
        let q: std::collections::BTreeMap<_, _> = q.into_iter().collect();
        assert_eq!(q["include_trend"], "false");
        assert_eq!(q["billing_mode"], "video");
        assert!(!q.contains_key("stream"));
        assert_eq!(
            validate_query(AccountResource::Announcements, json!({"unread_only":true})).unwrap(),
            vec![("unread_only".into(), "1".into())]
        );
        assert!(validate_query(AccountResource::Keys, json!({"page_size":101})).is_err());
        assert!(validate_query(
            AccountResource::Keys,
            json!({"search":"work","sort_by":"name","page_size":20})
        )
        .is_ok());
    }
}
