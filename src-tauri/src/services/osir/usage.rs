//! User-side usage filters. Never accept admin user_id/account_id selectors.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageQuery {
    pub period: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub api_key_id: Option<i64>,
    pub group_id: Option<i64>,
    pub model: Option<String>,
    pub request_type: Option<String>,
    pub stream: Option<bool>,
    pub billing_type: Option<i32>,
    pub billing_mode: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub timezone: Option<String>,
    pub granularity: Option<String>,
    pub include_trend: Option<bool>,
    pub include_model_stats: Option<bool>,
    pub include_group_stats: Option<bool>,
}

impl UsageQuery {
    /// Validates the exact query accepted by one of the three fixed usage endpoints.
    pub fn validate_for(
        &self,
        resource: super::client::AccountResource,
    ) -> Result<Vec<(String, String)>, &'static str> {
        use super::client::AccountResource;
        let list = matches!(resource, AccountResource::Usage);
        let stats = matches!(resource, AccountResource::UsageStats);
        let snapshot = matches!(resource, AccountResource::UsageSnapshot);
        if !list && !stats && !snapshot {
            return Err("用量端点无效");
        }
        if (!list
            && (self.page.is_some()
                || self.page_size.is_some()
                || self.sort_by.is_some()
                || self.sort_order.is_some()))
            || (!snapshot
                && (self.granularity.is_some()
                    || self.include_trend.is_some()
                    || self.include_model_stats.is_some()
                    || self.include_group_stats.is_some()))
            || (list && self.period.is_some())
        {
            return Err("用量端点不支持此筛选");
        }
        let mut params = self.parameters()?;
        // Handler gives request_type precedence over the legacy stream selector.
        if self.request_type.is_some() {
            params.retain(|(k, _)| k != "stream");
        }
        Ok(params)
    }

    /// Shared validation; IPC callers must use validate_for / resources::validate_query.
    pub fn parameters(&self) -> Result<Vec<(String, String)>, &'static str> {
        if self.page == Some(0)
            || self.page.is_some_and(|p| p > 1_000_000)
            || self.page_size.is_some_and(|n| n == 0 || n > 100)
            || self.api_key_id.is_some_and(|id| id <= 0)
            || self.group_id.is_some_and(|id| id <= 0)
        {
            return Err("用量分页或筛选参数无效");
        }
        if self
            .granularity
            .as_deref()
            .is_some_and(|s| s != "hour" && s != "day")
        {
            return Err("用量统计粒度无效");
        }
        for date in [&self.start_date, &self.end_date].into_iter().flatten() {
            if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
                return Err("用量日期格式无效");
            }
        }
        if let (Some(start), Some(end)) = (&self.start_date, &self.end_date) {
            let start = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
                .map_err(|_| "用量日期格式无效")?;
            let end = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
                .map_err(|_| "用量日期格式无效")?;
            if start > end {
                return Err("用量结束日期早于开始日期");
            }
        }
        if self
            .request_type
            .as_deref()
            .is_some_and(|v| !matches!(v, "unknown" | "sync" | "stream" | "ws_v2" | "cyber"))
            || self
                .billing_type
                .is_some_and(|v| !(-128..=127).contains(&v))
            || self
                .billing_mode
                .as_deref()
                .is_some_and(|v| !matches!(v, "token" | "per_request" | "image" | "video"))
            || self
                .period
                .as_deref()
                .is_some_and(|v| !matches!(v, "today" | "week" | "month"))
            || self
                .sort_by
                .as_deref()
                .is_some_and(|v| !matches!(v, "id" | "model" | "created_at"))
            || self
                .sort_order
                .as_deref()
                .is_some_and(|v| !matches!(v, "asc" | "desc"))
        {
            return Err("用量筛选值无效");
        }
        let value = serde_json::to_value(self).map_err(|_| "用量参数无效")?;
        let object = value.as_object().ok_or("用量参数无效")?;
        let mut params = BTreeMap::new();
        for (key, value) in object {
            if value.is_null() {
                continue;
            }
            let text = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
                return Err("用量筛选值无效");
            }
            if !text.is_empty() {
                params.insert(key.clone(), text);
            }
        }
        Ok(params.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_query_rejects_admin_identity_filters() {
        assert!(serde_json::from_value::<UsageQuery>(json!({"user_id": 2})).is_err());
        assert!(serde_json::from_value::<UsageQuery>(json!({"account_id": 2})).is_err());
    }

    #[test]
    fn filters_keep_timezone_key_and_chart_selection() {
        let query: UsageQuery = serde_json::from_value(json!({
            "page_size": 20, "api_key_id": 8, "group_id": 9,
            "timezone": "Asia/Shanghai", "include_trend": false,
            "start_date": "2026-10-26", "end_date": "2026-10-27"
        }))
        .unwrap();
        let params: BTreeMap<_, _> = query.parameters().unwrap().into_iter().collect();
        assert_eq!(params["timezone"], "Asia/Shanghai");
        assert_eq!(params["api_key_id"], "8");
        assert_eq!(params["include_trend"], "false");
    }

    #[test]
    fn invalid_ranges_and_unbounded_pages_are_rejected() {
        for value in [
            json!({"page_size": 101}),
            json!({"page": 0}),
            json!({"start_date": "2026-02-30"}),
            json!({"start_date": "2026-10-27", "end_date": "2026-10-26"}),
            json!({"granularity": "second"}),
        ] {
            assert!(serde_json::from_value::<UsageQuery>(value)
                .unwrap()
                .parameters()
                .is_err());
        }
    }
}

use super::resources::{Field, Shape};

pub(super) const USAGE_FIELDS: &[Field] = &[
    Field::new("id", Shape::Integer, false),
    Field::new("api_key_id", Shape::Integer, false),
    Field::new("request_id", Shape::Text, false),
    Field::new("model", Shape::Text, false),
    Field::new("service_tier", Shape::Text, true),
    Field::new("reasoning_effort", Shape::Text, true),
    Field::new("group_id", Shape::Integer, true),
    Field::new("subscription_id", Shape::Integer, true),
    Field::new("input_tokens", Shape::Integer, false),
    Field::new("output_tokens", Shape::Integer, false),
    Field::new("cache_creation_tokens", Shape::Integer, false),
    Field::new("cache_read_tokens", Shape::Integer, false),
    Field::new("cache_creation_5m_tokens", Shape::Integer, false),
    Field::new("cache_creation_1h_tokens", Shape::Integer, false),
    Field::new("input_cost", Shape::Number, false),
    Field::new("output_cost", Shape::Number, false),
    Field::new("cache_creation_cost", Shape::Number, false),
    Field::new("cache_read_cost", Shape::Number, false),
    Field::new("total_cost", Shape::Number, false),
    Field::new("actual_cost", Shape::Number, false),
    Field::new("rate_multiplier", Shape::Number, false),
    Field::new("billing_type", Shape::Integer, false),
    Field::new("request_type", Shape::Text, false),
    Field::new("stream", Shape::Bool, false),
    Field::new("openai_ws_mode", Shape::Bool, false),
    Field::new("duration_ms", Shape::Integer, true),
    Field::new("first_token_ms", Shape::Integer, true),
    Field::new("image_count", Shape::Integer, false),
    Field::new("image_size", Shape::Text, true),
    Field::new("image_input_size", Shape::Text, true),
    Field::new("image_output_size", Shape::Text, true),
    Field::new("image_output_tokens", Shape::Integer, false),
    Field::new("image_output_cost", Shape::Number, false),
    Field::new("image_size_source", Shape::Text, true),
    Field::new("image_size_breakdown", Shape::ImageCounts, true),
    Field::new("media_type", Shape::Text, true),
    Field::new("cache_ttl_overridden", Shape::Bool, false),
    Field::new("billing_mode", Shape::Text, true),
    Field::new("created_at", Shape::Text, false),
    Field::new("api_key", Shape::Object(super::account::KEY_FIELDS), true),
    Field::new("group", Shape::Object(super::account::GROUP_FIELDS), true),
    Field::new(
        "subscription",
        Shape::Object(super::account::SUBSCRIPTION_FIELDS),
        true,
    ),
];

pub(super) const STATS_FIELDS: &[Field] = &[
    Field::new("total_requests", Shape::Integer, false),
    Field::new("total_input_tokens", Shape::Integer, false),
    Field::new("total_output_tokens", Shape::Integer, false),
    Field::new("total_cache_tokens", Shape::Integer, false),
    Field::new("total_cache_creation_tokens", Shape::Integer, false),
    Field::new("total_cache_read_tokens", Shape::Integer, false),
    Field::new("total_tokens", Shape::Integer, false),
    Field::new("total_cost", Shape::Number, false),
    Field::new("total_actual_cost", Shape::Number, false),
    Field::new("average_duration_ms", Shape::Number, false),
    Field::new("endpoints", Shape::Array(ENDPOINT_FIELDS), true),
];

pub(super) const TREND_FIELDS: &[Field] = &[
    Field::new("date", Shape::Text, false),
    Field::new("requests", Shape::Integer, false),
    Field::new("input_tokens", Shape::Integer, false),
    Field::new("output_tokens", Shape::Integer, false),
    Field::new("cache_creation_tokens", Shape::Integer, false),
    Field::new("cache_read_tokens", Shape::Integer, false),
    Field::new("total_tokens", Shape::Integer, false),
    Field::new("cost", Shape::Number, false),
    Field::new("actual_cost", Shape::Number, false),
];

pub(super) const MODEL_FIELDS: &[Field] = &[
    Field::new("model", Shape::Text, false),
    Field::new("requests", Shape::Integer, false),
    Field::new("input_tokens", Shape::Integer, false),
    Field::new("output_tokens", Shape::Integer, false),
    Field::new("cache_creation_tokens", Shape::Integer, false),
    Field::new("cache_read_tokens", Shape::Integer, false),
    Field::new("total_tokens", Shape::Integer, false),
    Field::new("cost", Shape::Number, false),
    Field::new("actual_cost", Shape::Number, false),
];

pub(super) const GROUP_STATS_FIELDS: &[Field] = &[
    Field::new("group_id", Shape::Integer, false),
    Field::new("group_name", Shape::Text, false),
    Field::new("requests", Shape::Integer, false),
    Field::new("total_tokens", Shape::Integer, false),
    Field::new("cost", Shape::Number, false),
    Field::new("actual_cost", Shape::Number, false),
];

pub(super) const ENDPOINT_FIELDS: &[Field] = &[
    Field::new("endpoint", Shape::Text, false),
    Field::new("requests", Shape::Integer, false),
    Field::new("total_tokens", Shape::Integer, false),
    Field::new("cost", Shape::Number, false),
    Field::new("actual_cost", Shape::Number, false),
];
