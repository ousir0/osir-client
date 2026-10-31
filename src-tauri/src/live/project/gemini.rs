//! Gemini CLI 的投影：供应商行 → `.env` 和 `settings.json` 的关键字段。
//!
//! - `.env`：先删掉所有命中 `gemini_floor_env` 的行，再写目标供应商的；其他行（包括注释、
//!   空行和顺序）不动。
//! - `settings.json`：只改供应商自己的认证、模型和上下文限制字段，所在对象的其他键不动。
//!
//! Gemini 没有供应商独有字段：兼容选项都是环境变量，已经在关键字段里。行里其余的内容
//! （旧版回填进来的整份 `settings.json`、非关键的环境变量）不投影，它们归用户和客户端。

use serde_json::{json, Value};

use crate::live::floor;
use crate::live::patch::dotenv::DotenvPatch;
use crate::live::patch::json::JsonPatch;
use crate::live::patch::KeyPath;

/// Google 登录（官方卡）。
pub const SELECTED_TYPE_OAUTH: &str = "oauth-personal";
/// API Key（第三方和代理）。
pub const SELECTED_TYPE_API_KEY: &str = "gemini-api-key";

/// 代理契约里指向本地代理的两个变量。
pub const BASE_URL_ENV: &str = "GOOGLE_GEMINI_BASE_URL";
pub const API_KEY_ENV: &str = "GEMINI_API_KEY";
const MODEL_ENV: &str = "GEMINI_MODEL";
const DEFAULT_HISTORY_WINDOW_MAX_TOKENS: u64 = 150_000;

/// 一个供应商在 Gemini CLI 两个文件里拥有的键。
#[derive(Debug, Clone, PartialEq)]
pub struct GeminiProjection {
    /// `.env` 里的关键字段，按行里的顺序。
    pub env: Vec<(String, String)>,
    /// `security.auth.selectedType`：由供应商类型决定，不读行里的值（旧版回填会把用户在
    /// Gemini CLI 里 `/auth` 临时选的认证方式存进行，照写会让第三方卡走 Google 登录）。
    /// `None` 只在「清空关键字段」时出现（没有可写回的直连供应商）。
    pub selected_type: Option<&'static str>,
    /// `model.name`（Gemini CLI 的 `/model` 写在这里）。行里没有就从 live 删掉。
    pub model_name: Option<Value>,
    /// `experimental.contextManagement`，启用 Gemini CLI 的历史压缩。
    pub context_management: Option<Value>,
    /// `contextManagement.historyWindow.maxTokens`，供应商对应的压缩阈值。
    pub history_window_max_tokens: Option<Value>,
}

impl GeminiProjection {
    /// 从供应商行（或编辑器里的完整配置）取出关键字段。`official` 由调用方按供应商
    /// 类型判定（`detect_gemini_auth_type`）。
    pub fn of(settings: &Value, official: bool) -> Self {
        let env = settings
            .get("env")
            .and_then(Value::as_object)
            .map(|env| {
                env.iter()
                    .filter(|(key, _)| floor::gemini_floor_env(key))
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let model_name = settings
            .pointer("/config/model/name")
            .filter(|value| !value.is_null())
            .cloned();
        let context_management = settings
            .pointer("/config/experimental/contextManagement")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!(true));
        let history_window_max_tokens = settings
            .pointer("/config/contextManagement/historyWindow/maxTokens")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!(DEFAULT_HISTORY_WINDOW_MAX_TOKENS));
        Self {
            env,
            selected_type: Some(if official {
                SELECTED_TYPE_OAUTH
            } else {
                SELECTED_TYPE_API_KEY
            }),
            model_name,
            context_management: Some(context_management),
            history_window_max_tokens: Some(history_window_max_tokens),
        }
    }

    /// 只清空关键字段，不写任何值。
    pub fn empty() -> Self {
        Self {
            env: Vec::new(),
            selected_type: None,
            model_name: None,
            context_management: None,
            history_window_max_tokens: None,
        }
    }

    /// 代理契约：本地代理地址、占位 Key 和 API Key 认证，加上路由供应商的模型名。
    ///
    /// 路由的其他关键字段（Vertex 选择器、项目、Key 的传法、自定义头）不写：客户端只和
    /// 本地代理说话，上游由代理按路由供应商连接。模型名跟着路由走：换一家模型名不同的
    /// 路由，客户端要跟着改（和 Claude Code 的契约一致）。
    pub fn proxy_contract(route: &Self, proxy_base_url: &str, placeholder: &str) -> Self {
        let mut env: Vec<(String, String)> = route
            .env
            .iter()
            .filter(|(key, _)| key == MODEL_ENV)
            .cloned()
            .collect();
        env.push((BASE_URL_ENV.to_string(), proxy_base_url.to_string()));
        env.push((API_KEY_ENV.to_string(), placeholder.to_string()));
        Self {
            env,
            selected_type: Some(SELECTED_TYPE_API_KEY),
            model_name: route.model_name.clone(),
            context_management: route.context_management.clone(),
            history_window_max_tokens: route.history_window_max_tokens.clone(),
        }
    }

    pub fn env_patch(&self) -> DotenvPatch {
        DotenvPatch {
            clear: Some(floor::gemini_floor_env),
            set: self.env.clone(),
            ..DotenvPatch::default()
        }
    }

    pub fn settings_patch(&self) -> JsonPatch {
        let mut patch = JsonPatch::default();
        match self.selected_type {
            Some(selected) => patch.set.push((selected_type_path(), json!(selected))),
            None => patch.remove.push(selected_type_path()),
        }
        match &self.model_name {
            Some(name) => patch.set.push((model_name_path(), name.clone())),
            None => patch.remove.push(model_name_path()),
        }
        match &self.context_management {
            Some(enabled) => patch.set.push((context_management_path(), enabled.clone())),
            None => patch.remove.push(context_management_path()),
        }
        match &self.history_window_max_tokens {
            Some(limit) => patch
                .set
                .push((history_window_max_tokens_path(), limit.clone())),
            None => patch.remove.push(history_window_max_tokens_path()),
        }
        patch
    }

    /// 摘要用的规范形式。
    pub fn to_value(&self) -> Value {
        json!({
            "env": self.env,
            "selectedType": self.selected_type,
            "modelName": self.model_name,
            "contextManagement": self.context_management,
            "historyWindowMaxTokens": self.history_window_max_tokens,
        })
    }
}

pub fn selected_type_path() -> KeyPath {
    KeyPath::new(&["security", "auth", "selectedType"])
}

pub fn model_name_path() -> KeyPath {
    KeyPath::new(&["model", "name"])
}

pub fn context_management_path() -> KeyPath {
    KeyPath::new(&["experimental", "contextManagement"])
}

pub fn history_window_max_tokens_path() -> KeyPath {
    KeyPath::new(&["contextManagement", "historyWindow", "maxTokens"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::patch::LivePatch;
    use std::path::Path;

    fn apply_env(projection: &GeminiProjection, pre: &str) -> String {
        String::from_utf8(
            projection
                .env_patch()
                .apply(Path::new(".env"), Some(pre.as_bytes()))
                .unwrap(),
        )
        .unwrap()
    }

    fn apply_settings(projection: &GeminiProjection, pre: &str) -> Value {
        let out = projection
            .settings_patch()
            .apply(Path::new("settings.json"), Some(pre.as_bytes()))
            .unwrap();
        serde_json::from_slice(&out).unwrap()
    }

    #[test]
    fn only_key_fields_are_projected() {
        let row = json!({
            "env": {
                "GOOGLE_GEMINI_BASE_URL": "https://b.example",
                "GEMINI_API_KEY": "key-b",
                "GEMINI_SANDBOX": "docker",
                "GEMINI_MODEL": "gemini-b",
            },
            "config": {
                "model": {"name": "gemini-b-pro", "compressionThreshold": 0.5},
                "security": {"auth": {"selectedType": "oauth-personal"}},
                "ui": {"theme": "dark"},
            },
        });
        let projection = GeminiProjection::of(&row, false);
        assert_eq!(
            projection.env,
            vec![
                ("GOOGLE_GEMINI_BASE_URL".into(), "https://b.example".into()),
                ("GEMINI_API_KEY".into(), "key-b".into()),
                ("GEMINI_MODEL".into(), "gemini-b".into()),
            ]
        );
        // 行里回填进来的 oauth-personal 不算：第三方卡一律用 API Key。
        assert_eq!(projection.selected_type, Some(SELECTED_TYPE_API_KEY));
        assert_eq!(projection.model_name, Some(json!("gemini-b-pro")));
    }

    #[test]
    fn env_switch_keeps_user_lines_and_clears_the_previous_route() {
        let vertex = GeminiProjection::of(
            &json!({"env": {"GOOGLE_GENAI_USE_VERTEXAI": "true", "GOOGLE_CLOUD_PROJECT": "p"}}),
            false,
        );
        let pre = "# mine\nGEMINI_SANDBOX=docker\nGEMINI_API_KEY=key-a\nDEBUG=1\nGOOGLE_GEMINI_BASE_URL=https://a.example\n";
        assert_eq!(
            apply_env(&vertex, pre),
            "# mine\nGEMINI_SANDBOX=docker\nDEBUG=1\nGOOGLE_GENAI_USE_VERTEXAI=true\nGOOGLE_CLOUD_PROJECT=p\n"
        );
    }

    #[test]
    fn settings_switch_updates_supplier_fields_and_preserves_user_fields() {
        let official = GeminiProjection::of(&json!({"env": {}}), true);
        let pre = r#"{"model": {"name": "gemini-a", "compressionThreshold": 0.5}, "security": {"auth": {"selectedType": "gemini-api-key", "useExternal": true}}, "mcpServers": {"x": {"command": "y"}}}"#;
        assert_eq!(
            apply_settings(&official, pre),
            json!({
                "model": {"compressionThreshold": 0.5},
                "experimental": {"contextManagement": true},
                "contextManagement": {"historyWindow": {"maxTokens": 150000}},
                "security": {"auth": {"selectedType": "oauth-personal", "useExternal": true}},
                "mcpServers": {"x": {"command": "y"}},
            })
        );
    }

    #[test]
    fn proxy_contract_points_the_route_at_the_local_proxy() {
        let route = GeminiProjection::of(
            &json!({"env": {
                "GOOGLE_GEMINI_BASE_URL": "https://b.example",
                "GEMINI_API_KEY": "real",
                "GEMINI_API_KEY_AUTH_MECHANISM": "bearer",
                "GEMINI_MODEL": "m",
            }}),
            false,
        );
        let contract =
            GeminiProjection::proxy_contract(&route, "http://127.0.0.1:15721", "PROXY_MANAGED");
        assert_eq!(
            contract.env,
            vec![
                ("GEMINI_MODEL".into(), "m".into()),
                (BASE_URL_ENV.into(), "http://127.0.0.1:15721".into()),
                (API_KEY_ENV.into(), "PROXY_MANAGED".into()),
            ]
        );
        assert_eq!(contract.selected_type, Some(SELECTED_TYPE_API_KEY));
    }

    #[test]
    fn supplier_context_limit_projects_and_follows_proxy_route() {
        let row = json!({
            "config": {
                "experimental": {"contextManagement": true},
                "contextManagement": {"historyWindow": {"maxTokens": 150000}},
            }
        });
        let projection = GeminiProjection::of(&row, false);
        let live = r#"{"experimental":{"contextManagement":false,"other":true},"contextManagement":{"historyWindow":{"maxTokens":90000,"other":1}},"model":{"name":"old"},"mcpServers":{"keep":{}}}"#;
        assert_eq!(
            apply_settings(&projection, live),
            json!({
                "experimental": {"contextManagement": true, "other": true},
                "contextManagement": {"historyWindow": {"maxTokens": 150000, "other": 1}},
                "model": {},
                "security": {"auth": {"selectedType": "gemini-api-key"}},
                "mcpServers": {"keep": {}}
            })
        );

        let proxy = GeminiProjection::proxy_contract(
            &projection,
            "http://127.0.0.1:15721",
            "PROXY_MANAGED",
        );
        assert_eq!(proxy.context_management, Some(json!(true)));
        assert_eq!(proxy.history_window_max_tokens, Some(json!(150000)));
    }

    #[test]
    fn context_management_defaults_are_applied_and_explicit_opt_out_is_preserved() {
        let default = GeminiProjection::of(&json!({}), false);
        assert_eq!(default.context_management, Some(json!(true)));
        assert_eq!(default.history_window_max_tokens, Some(json!(150000)));

        let opted_out = GeminiProjection::of(
            &json!({
                "config": {
                    "experimental": {"contextManagement": false},
                    "contextManagement": {"historyWindow": {"maxTokens": 120000}}
                }
            }),
            false,
        );
        assert_eq!(opted_out.context_management, Some(json!(false)));
        assert_eq!(opted_out.history_window_max_tokens, Some(json!(120000)));
    }
}
