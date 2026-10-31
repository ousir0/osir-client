//! Stack 模式（界面和代码都叫 Stack）。
//!
//! Stack 模式和路由模式在界面上二选一，内部都是代理模式：Stack 模式多一个开关位
//! （[`StackState::enabled`]）。Stack 模式下供应商列表是累加式的：添加的每一家（第三方）的
//! 模型以带保留前缀的 id 发布给客户端，选中后请求直达那一家；不带前缀的请求发往「默认」
//! 那家（代理路由），不做故障转移。Claude Code 的四档别名（启动默认、后台任务、子代理别名）
//! 都指向默认那家列表里的第一个模型（[`claude_route_default`]），平时用哪个由用户在
//! `/model` 里选。
//!
//! - 名单和 key 登记簿存在 `live-state.json`（[`StackState`]），增删和客户端文件在同一个
//!   操作里提交（`controller::set_stack_member`）；默认那家也在名单里，不能移除；
//! - key 一经分配永久归这家（[`allocate_key`]）：客户端会一直带着选中过的 id，key 改了
//!   指向，旧 id 就会被悄悄发到另一家；
//! - 带保留前缀的 id 解不出来（不在 Stack 模式、成员已移除、供应商已删除、key 没登记）一律
//!   报错，不回落到默认路由（[`resolve`]）：回落会用别家的钱、别家的模型回答，用户看不出来；
//! - Stack 请求不读也不写任何路由状态（熔断器、故障转移、「正在使用」、代理统计），见
//!   `proxy::forwarder` 的 `routing_state_enabled`。

use serde::Serialize;
use serde_json::{Map, Value};

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::live::engine::DeviceStore;
use crate::live::project::claude::{env_string, has_one_m_marker, ONE_M_MARKER_FOR_CLIENT};
use crate::provider::{ClaudeStackModel, Provider};
use crate::proxy::model_mapper::strip_one_m_suffix_for_upstream;
use crate::services::provider::codex_client_catalog::StaleClients;

use super::state::{self, StackState};

/// Claude Code 的 Stack 模型 id：`ccs-claude-<key>--<model>`。id 里要有 `claude` 才进
/// `/model` 选择器，不以 `claude-` 开头 MAX 窗口才生效。
const CLAUDE_PREFIX: &str = "ccs-claude-";
const CLAUDE_SEPARATOR: &str = "--";
/// Codex 的 Stack 模型 id：`ccs-<key>/<model>`。
const CODEX_PREFIX: &str = "ccs-";
const CODEX_SEPARATOR: char = '/';
/// Claude Desktop 的 Stack 模型 id：`claude-fable-ccs-<6 位数字>`（[`desktop_code`]）。Desktop 的
/// 选择器只收含 `claude` 等字样、又不含厂商名（kimi、glm、seed- 等一长串黑名单）的 id，所以 id
/// 里放不下 key 和模型名，只放数字；末尾 8 位数字会被当成日期剥掉，所以是 6 位。`fable` 前缀让
/// Desktop 给出 effort 选择器，内嵌的 Claude Code 不会因此把它当成真的 Fable。
const DESKTOP_PREFIX: &str = "claude-fable-ccs-";
const DESKTOP_CODE_DIGITS: usize = 6;

/// key 的最大长度：只是为了模型 id 不至于太长。
const KEY_MAX_LEN: usize = 24;

/// Claude Code 在没有 `CLAUDE_CODE_MAX_CONTEXT_TOKENS` 时按这个窗口算。
pub const CLAUDE_DEFAULT_WINDOW: u64 = 200_000;
pub const CLAUDE_DEFAULT_AUTO_COMPACT_WINDOW: u64 = 100_000;

/// 支持 Stack 模型的应用。
pub fn supports_stack(app: &AppType) -> bool {
    matches!(
        app,
        AppType::Claude | AppType::Codex | AppType::ClaudeDesktop
    )
}

/// 给 `provider` 一个 key：登记过就用原来的（移除后重新加入，旧会话里的 id 继续有效），
/// 没有就按图标、名称生成一个新的写进登记簿。
///
/// 新 key 和登记簿里所有的 key 去重，不只是当前成员：已移除、已删除的供应商的 key 也
/// 占着位置，旧 id 才不会被发给新来的这家。
// Metadata only: account ids, never tokens. Retired keys stay occupied forever.
const OFFICIAL_BINDINGS: &str = "codexOfficialBindings";
pub(super) const RETIRED_KEYS: &str = "codexRetiredKeys";

fn official_binding(provider: &Provider) -> Option<Value> {
    crate::proxy::providers::is_codex_official_provider(provider).then(|| {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
            .map(Value::String)
            .unwrap_or(Value::Null)
    })
}

fn binding_matches(stack: &StackState, key: &str, provider: &Provider) -> bool {
    let stored = stack.extra.get(OFFICIAL_BINDINGS).and_then(|v| v.get(key));
    stored == official_binding(provider).as_ref()
        && !stack
            .extra
            .get(RETIRED_KEYS)
            .and_then(Value::as_array)
            .is_some_and(|keys| keys.iter().any(|v| v.as_str() == Some(key)))
}

pub fn allocate_key(stack: &mut StackState, provider: &Provider) -> String {
    if stack.keys.is_empty() && stack.members.is_empty() {
        stack.identity_checked = true;
    }
    // Set before the existing-key fast path too (upgrading an older registry).
    // The controller commits this with its normal configuration transaction.
    if is_osir_managed(provider) {
        stack.managed_identity_required = true;
    }
    if let Some(key) = stack.key_of(&provider.id).map(str::to_string) {
        if binding_matches(stack, &key, provider) {
            return key;
        }
        stack
            .extra
            .entry(RETIRED_KEYS)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("retired key registry must be an array")
            .push(Value::String(key));
    }
    // 自定义图标是一长串 base64，拿来生成 key 没有意义，只用内置图标名
    let icon = provider
        .icon
        .as_deref()
        .filter(|icon| !crate::services::provider::icon::is_custom_icon(icon));
    let base = [icon, Some(provider.name.as_str())]
        .into_iter()
        .flatten()
        .map(slug)
        .find(|candidate| !candidate.is_empty())
        .unwrap_or_else(|| {
            let id: String = slug(&provider.id)
                .chars()
                .filter(|c| *c != '-')
                .take(6)
                .collect();
            format!("p{id}")
        });
    let mut key = base.clone();
    let mut suffix = 2;
    while stack.keys.contains_key(&key) {
        key = format!("{base}-{suffix}");
        suffix += 1;
    }
    stack.keys.insert(key.clone(), provider.id.clone());
    if let Some(binding) = official_binding(provider) {
        stack
            .extra
            .entry(OFFICIAL_BINDINGS)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("official binding registry must be an object")
            .insert(key.clone(), binding);
    }
    key
}

/// 小写 ASCII，只保留 `[a-z0-9-]`，其余字符换成 `-`，连续的 `-` 合并，首尾的 `-` 去掉。
/// 结果里不会有 `--`（Claude id 的分隔符）和 `/`（Codex id 的分隔符）。
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= KEY_MAX_LEN {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Stack 模型 id。Claude 的上游是 1M 窗口时末尾带 `[1M]`，Claude Code 才按 1M 计算；Claude
/// Desktop 的 1M 写在模型条目里（`supports1m` + `prefer1m`），id 不带标记。
pub fn encode(app: &AppType, key: &str, model: &str, one_m: bool) -> String {
    match app {
        AppType::Codex => format!("{CODEX_PREFIX}{key}{CODEX_SEPARATOR}{model}"),
        AppType::ClaudeDesktop => format!("{DESKTOP_PREFIX}{}", desktop_code(key, model)),
        _ => {
            let marker = if one_m { ONE_M_MARKER_FOR_CLIENT } else { "" };
            format!("{CLAUDE_PREFIX}{key}{CLAUDE_SEPARATOR}{model}{marker}")
        }
    }
}

/// Claude Desktop Stack 模型 id 里的数字：key 和上游模型名的 FNV-1a 散列取 6 位。不用标准库的
/// 散列：它不保证跨版本稳定，客户端会一直带着选中过的 id。
fn desktop_code(key: &str, model: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes().chain([0]).chain(model.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!(
        "{:0width$}",
        hash % 10u64.pow(DESKTOP_CODE_DIGITS as u32),
        width = DESKTOP_CODE_DIGITS
    )
}

/// 解码的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded<'a> {
    /// 不带保留前缀：普通模型名，照旧走代理路由。
    Plain,
    /// 带保留前缀，但切不出 key 和模型。
    Malformed,
    /// 带保留前缀。`model` 可以含 `/` 和 `--`（key 里没有这两种分隔符，在第一个处切开）。
    Stack {
        key: &'a str,
        model: &'a str,
        /// Claude id 末尾带着 1M 标记。
        one_m: bool,
    },
    /// Claude Desktop 的 Stack id：只有数字，key 和模型要按名单反查（[`resolve`]）。
    Code(&'a str),
}

/// 按客户端的格式解码模型 id。Claude：`ccs-claude-` 开头；Codex：`ccs-` 开头且含 `/`。
pub fn decode<'a>(app: &AppType, id: &'a str) -> Decoded<'a> {
    let (rest, separator, one_m) = match app {
        AppType::Claude => {
            let Some(rest) = id.strip_prefix(CLAUDE_PREFIX) else {
                return Decoded::Plain;
            };
            let stripped = strip_one_m_suffix_for_upstream(rest);
            (stripped, CLAUDE_SEPARATOR, stripped.len() != rest.len())
        }
        AppType::Codex => {
            let Some(rest) = id.strip_prefix(CODEX_PREFIX) else {
                return Decoded::Plain;
            };
            if !rest.contains(CODEX_SEPARATOR) {
                return Decoded::Plain;
            }
            (rest, "/", false)
        }
        AppType::ClaudeDesktop => {
            let Some(rest) = id.strip_prefix(DESKTOP_PREFIX) else {
                return Decoded::Plain;
            };
            // 1M 条目 Desktop 发请求时用 `<id>[1m]`（`prefer1m` 自动补；旧版另列一行）
            let code = strip_one_m_suffix_for_upstream(rest);
            return if code.len() == DESKTOP_CODE_DIGITS && code.bytes().all(|b| b.is_ascii_digit())
            {
                Decoded::Code(code)
            } else {
                Decoded::Malformed
            };
        }
        _ => return Decoded::Plain,
    };
    match rest.split_once(separator) {
        Some((key, model)) if !key.is_empty() && !model.is_empty() => {
            Decoded::Stack { key, model, one_m }
        }
        _ => Decoded::Malformed,
    }
}

/// Claude Stack 供应商发布给客户端的一个模型（Codex 的由目录条目描述，见
/// `codex_config::plan_codex_stack_catalog`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackModel {
    /// 发布给客户端的 id（带保留前缀）。
    pub id: String,
    /// 发往上游的模型名：行里配置的原值（可能带 `[1M]`，转发时和路由请求一样处理）。
    pub upstream: String,
    /// 模型自己的显示名（行里配的，没有是模型名），不带供应商名。
    pub name: String,
    /// 选择器里显示的名字：`<显示名>（<供应商名>）`。
    pub display_name: String,
    /// 选择器里的说明。
    pub description: String,
    /// 上游是 1M 窗口（id 带 `[1M]`）。
    pub one_m: bool,
    /// 非 1M 模型的窗口：行里的 `CLAUDE_CODE_MAX_CONTEXT_TOKENS`，没有是 200K。
    pub window: u64,
    /// 行里的自动压缩参考窗口，没有是 100K。
    pub auto_compact_window: u64,
}

/// Claude 行发布的模型：配了 Stack 模型列表（`meta.stackModels`）就是列表，清空了就什么都不
/// 发布；没配时是模型映射，即 `ANTHROPIC_MODEL` 和各档 `ANTHROPIC_DEFAULT_*_MODEL`，显示名取
/// 对应档位的 `*_MODEL_NAME`。按去掉 1M 标记后的名字去重（任何一处带标记就按 1M）。
pub fn claude_models(key: &str, provider: &Provider) -> Vec<StackModel> {
    let listed = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.stack_models.as_deref());
    let found = match listed {
        Some(list) => listed_models(list),
        None => claude_env(provider).map(mapped_models).unwrap_or_default(),
    };
    stack_models(key, provider, found)
}

/// 行里要发布的一个模型（还没加前缀）。
struct Found {
    /// 去掉 1M 标记的模型名。
    model: String,
    /// 发往上游的原值（1M 模型带标记）。
    upstream: String,
    name: Option<String>,
    one_m: bool,
}

/// 按去掉 1M 标记后的名字去重地加入 `found`。同一个模型有一处带 1M 标记就按 1M，发往上游的
/// 也用带标记的那个写法；显示名取第一个有的。
fn push_found(found: &mut Vec<Found>, upstream: &str, name: Option<String>) {
    let model = strip_one_m_suffix_for_upstream(upstream).trim().to_string();
    if model.is_empty() {
        return;
    }
    let one_m = has_one_m_marker(upstream);
    match found.iter_mut().find(|entry| entry.model == model) {
        Some(entry) => {
            if one_m && !entry.one_m {
                entry.upstream = upstream.to_string();
                entry.one_m = true;
            }
            if entry.name.is_none() {
                entry.name = name;
            }
        }
        None => found.push(Found {
            model,
            upstream: upstream.to_string(),
            name,
            one_m,
        }),
    }
}

fn claude_env(provider: &Provider) -> Option<&Map<String, Value>> {
    provider
        .settings_config
        .get("env")
        .and_then(Value::as_object)
}

/// Stack 模型列表（`meta.stackModels`）里的模型。
fn listed_models(list: &[ClaudeStackModel]) -> Vec<Found> {
    let mut found = Vec::new();
    for entry in list {
        let raw = entry.model.trim();
        let model = strip_one_m_suffix_for_upstream(raw).trim();
        let upstream = if entry.one_m || has_one_m_marker(raw) {
            format!("{model}{ONE_M_MARKER_FOR_CLIENT}")
        } else {
            model.to_string()
        };
        let name = entry
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        push_found(&mut found, &upstream, name);
    }
    found
}

/// 模型映射里的模型：`ANTHROPIC_MODEL` 和各档 `ANTHROPIC_DEFAULT_*_MODEL`。
fn mapped_models(env: &Map<String, Value>) -> Vec<Found> {
    const ROLES: [(&str, Option<&str>); 5] = [
        ("ANTHROPIC_MODEL", None),
        (
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            Some("ANTHROPIC_DEFAULT_OPUS_MODEL_NAME"),
        ),
        (
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            Some("ANTHROPIC_DEFAULT_SONNET_MODEL_NAME"),
        ),
        (
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            Some("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"),
        ),
        (
            "ANTHROPIC_DEFAULT_FABLE_MODEL",
            Some("ANTHROPIC_DEFAULT_FABLE_MODEL_NAME"),
        ),
    ];
    let mut found = Vec::new();
    for (model_key, name_key) in ROLES {
        let Some(upstream) = env_string(env, model_key) else {
            continue;
        };
        let name = name_key.and_then(|name_key| env_string(env, name_key).map(str::to_string));
        push_found(&mut found, upstream, name);
    }
    found
}

/// 给找到的模型加上前缀、显示名和窗口。非 1M 模型的窗口是行里的
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS`（Claude Code 只有一个全局窗口，没法按模型设），没有是 200K。
fn stack_models(key: &str, provider: &Provider, found: Vec<Found>) -> Vec<StackModel> {
    let window = claude_env(provider)
        .and_then(|env| env.get("CLAUDE_CODE_MAX_CONTEXT_TOKENS"))
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .filter(|window| *window > 0)
        .unwrap_or(CLAUDE_DEFAULT_WINDOW);
    let auto_compact_window = claude_env(provider)
        .and_then(|env| env.get("CLAUDE_CODE_AUTO_COMPACT_WINDOW"))
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .filter(|window| *window >= 100_000)
        .unwrap_or(CLAUDE_DEFAULT_AUTO_COMPACT_WINDOW);
    found
        .into_iter()
        .map(|found| {
            let name = found.name.unwrap_or_else(|| found.model.clone());
            let shown_window = if found.one_m { 1_000_000 } else { window };
            StackModel {
                id: encode(&AppType::Claude, key, &found.model, found.one_m),
                display_name: display_name(&name, &provider.name),
                name,
                description: model_description(&found.model, shown_window),
                upstream: found.upstream,
                one_m: found.one_m,
                window,
                auto_compact_window,
            }
        })
        .collect()
}

/// Codex 行发布的模型 id：行里的模型目录，没有配置目录时只有行的 `model`。显示名和窗口
/// 由目录条目决定（`codex_config::plan_codex_stack_catalog`）。
pub fn codex_model_ids(key: &str, provider: &Provider) -> Vec<String> {
    let config = provider
        .settings_config
        .get("config")
        .and_then(Value::as_str)
        .unwrap_or("");
    let models = if is_osir_managed(provider) {
        crate::codex_config::managed_codex_published_models(&provider.settings_config)
    } else {
        crate::codex_config::codex_published_models(&provider.settings_config, config)
    };
    models
        .into_iter()
        .map(|model| encode(&AppType::Codex, key, &model, false))
        .collect()
}

pub(crate) fn official_model_ids(stack: &StackState, key: &str) -> Vec<String> {
    stack
        .codex_official_catalogs
        .get(key)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("slug").and_then(Value::as_str))
        .map(|model| encode(&AppType::Codex, key, model, false))
        .collect()
}

/// Claude Desktop Stack 供应商发布的一个模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopStackModel {
    /// 发布给 Desktop 的 id（`claude-fable-ccs-<数字>`）。
    pub id: String,
    /// 发往上游的模型名：行里映射表的目标模型。
    pub upstream: String,
    /// 选择器里显示的名字（`labelOverride`）：`<供应商名> / <模型显示名>`。
    pub label: String,
    /// 映射表里这个模型标了 1M。
    pub supports_1m: bool,
}

/// Claude Desktop 行发布的模型：映射表的目标模型，按模型名去重（任何一条标了 1M 就按 1M），
/// 显示名取映射表里的，没有是模型名。映射表不完整（不能做路由）的行什么都不发布。
pub fn desktop_models(key: &str, provider: &Provider) -> Vec<DesktopStackModel> {
    let Ok(routes) = crate::claude_desktop_config::configured_proxy_model_routes(provider) else {
        return Vec::new();
    };
    let mut models: Vec<DesktopStackModel> = Vec::new();
    for route in routes {
        if let Some(existing) = models
            .iter_mut()
            .find(|model| model.upstream == route.upstream_model)
        {
            existing.supports_1m |= route.supports_1m;
            continue;
        }
        let name = route
            .label_override
            .unwrap_or_else(|| route.upstream_model.clone());
        models.push(DesktopStackModel {
            id: encode(&AppType::ClaudeDesktop, key, &route.upstream_model, false),
            label: format!("{} / {name}", provider.name),
            upstream: route.upstream_model,
            supports_1m: route.supports_1m,
        });
    }
    models
}

/// Claude Desktop 的这些成员发布给客户端的模型，按名单顺序。路由那家不发布：它的映射表就是
/// 默认的那几行。两个模型撞了同一个数字（概率很小）时后一个不发布：id 是散列出来的，换数字
/// 就不稳定了。
pub fn desktop_published(members: &[Member]) -> Vec<DesktopStackModel> {
    let mut seen = std::collections::HashSet::new();
    let mut published = Vec::new();
    for member in members.iter().filter(|member| !member.route) {
        for model in desktop_models(&member.key, &member.provider) {
            if !seen.insert(model.id.clone()) {
                log::warn!(
                    "Claude Desktop 聚合模型 {} 的 id {} 与前面的模型重复，不发布",
                    model.upstream,
                    model.id
                );
                continue;
            }
            published.push(model);
        }
    }
    published
}

/// 选择器里 Stack 模型的显示名：`<模型显示名>（<供应商名>）`。
pub fn display_name(model: &str, provider_name: &str) -> String {
    if provider_name.trim().is_empty() {
        model.to_string()
    } else {
        format!("{model}（{provider_name}）")
    }
}

/// Presentation only. Never derive this label from icon/brand/URL or use it as
/// a route key. Persisted stack keys remain stable when the user renames a row.
pub fn aggregation_name(provider: &Provider) -> &str {
    if provider
        .meta
        .as_ref()
        .and_then(|meta| meta.hide_aggregation_name)
        .unwrap_or(true)
    {
        ""
    } else {
        provider.name.trim()
    }
}

pub fn provider_display_name(model: &str, provider: &Provider) -> String {
    display_name(model, aggregation_name(provider))
}

/// 选择器里 Stack 模型的说明：`<上游模型名> · <窗口>`。显示名可以是用户自己起的，id 又带着
/// 前缀，上游真正的模型名只有这里看得到；供应商名已经在显示名里，不再重复。窗口未知
/// （`0`）时只写模型名。
pub fn model_description(model: &str, window: u64) -> String {
    match window_label(window) {
        Some(window) => format!("{model} · {window}"),
        None => model.to_string(),
    }
}

/// 窗口的简写：整百万写 `1M`，其余按千写 `256K`，不足一千照原样。
fn window_label(window: u64) -> Option<String> {
    match window {
        0 => None,
        w if w % 1_000_000 == 0 => Some(format!("{}M", w / 1_000_000)),
        w if w >= 1_000 => Some(format!("{}K", (w + 500) / 1_000)),
        w => Some(w.to_string()),
    }
}

/// 一家 Stack 供应商发布给客户端的模型 id。Codex 路由那家的整张目录就是默认路由的目录行，
/// 非托管路由不再带前缀发布；OSIR 托管路由也发布稳定前缀。Claude 路由那家整张列表照常发布（它的第一个模型同时占着四档别名，见
/// [`claude_route_default`]）。
fn model_ids_of(app: &AppType, key: &str, provider: &Provider, route: bool) -> Vec<String> {
    match app {
        AppType::Claude => claude_models(key, provider)
            .into_iter()
            .map(|model| model.id)
            .collect(),
        AppType::Codex if !route || is_osir_managed(provider) => codex_model_ids(key, provider),
        AppType::ClaudeDesktop if !route => desktop_models(key, provider)
            .into_iter()
            .map(|model| model.id)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn is_osir_managed(provider: &Provider) -> bool {
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.osir_binding.as_ref())
        .is_some()
}

// Missing historical rows cannot prove ordinary ownership. New ordinary
// registries are classified at allocation; old complete registries can migrate.
fn requires_identity(db: &Database, app: &state::AppLiveState) -> Result<bool, AppError> {
    if app.stack.managed_identity_required {
        return Ok(true);
    }
    for id in app
        .stack
        .keys
        .values()
        .chain(app.stack.members.iter())
        .chain(app.proxy_route.iter())
    {
        match db.get_provider_by_id(id, "codex")? {
            Some(provider) if is_osir_managed(&provider) => return Ok(true),
            None if !app.stack.identity_checked => return Ok(true),
            _ => (),
        }
    }
    Ok(false)
}

/// Called only while holding the controller's application switch lock, before
/// recovery/deletion can discard the surviving evidence. Request routing is read-only.
pub(crate) fn migrate_codex_identity_locked(
    db: &Database,
    store: &DeviceStore,
) -> Result<(), AppError> {
    let snapshot = state::identity_snapshot(store)?;
    let Some(app) = snapshot.apps.get("codex") else {
        return Ok(());
    };
    let required = requires_identity(db, app)?;
    if app.stack.identity_checked && (!required || app.stack.managed_identity_required) {
        return Ok(());
    }
    state::update(store, |live| {
        let stack = &mut live.apps.entry("codex".into()).or_default().stack;
        stack.managed_identity_required |= required;
        stack.identity_checked = true;
    })
}

fn rejects_plain_codex_model(db: &Database, store: &DeviceStore) -> Result<bool, AppError> {
    let snapshot = state::identity_snapshot(store)?;
    let Some(app) = snapshot.apps.get("codex") else {
        return Ok(false);
    };
    if app.mode != Some(state::Mode::Proxy) || !app.stack.enabled {
        return Ok(false);
    }
    requires_identity(db, app)
}

/// 名单里的一家。
#[derive(Debug, Clone)]
pub struct Member {
    pub provider: Provider,
    pub key: String,
    /// 这家是路由那家（默认），见 [`is_published`]。
    pub route: bool,
    /// 发布给客户端的模型 id。
    pub model_ids: Vec<String>,
}

/// 名单里还在库里的成员，按加入顺序。库里已经没有的跳过（删除供应商会先把它移出名单，
/// 删行前失败才会留下）。`route` 是代理模式下的路由供应商（不在代理模式时为 `None`）。
pub fn members(
    db: &Database,
    app: &AppType,
    stack: &StackState,
    route: Option<&str>,
) -> Result<Vec<Member>, AppError> {
    let mut members = Vec::with_capacity(stack.members.len());
    for id in &stack.members {
        let Some(key) = stack.key_of(id) else {
            log::warn!("{} 的 Stack 模型成员 {id} 没有登记 key，跳过", app.as_str());
            continue;
        };
        let Some(provider) = db.get_provider_by_id(id, app.as_str())? else {
            continue;
        };
        let route = route == Some(id.as_str());
        let model_ids = if app == &AppType::Codex && stack.managed_identity_required {
            if crate::services::provider::codex_direct::is_official(&provider) {
                official_model_ids(stack, key)
            } else {
                codex_model_ids(key, &provider)
            }
        } else {
            model_ids_of(app, key, &provider, route)
        };
        members.push(Member {
            key: key.to_string(),
            provider,
            route,
            model_ids,
        });
    }
    Ok(members)
}

/// 这个成员发布 Stack 模型。Codex 路由那家不发布（它的模型已经是默认路由的目录行），Claude
/// 路由那家照常发布；名单都保留。契约、Codex 目录、Claude Code 的模型发现和给前端的名单都按
/// 这一条算。
pub fn is_published(member: &Member) -> bool {
    !member.route || !member.model_ids.is_empty()
}

/// 发布 Stack 模型的成员（按名单顺序，见 [`is_published`]）。Stack 模式关着（路由模式）时
/// 没有：名单留着，下次进入 Stack 模式时恢复。
pub fn published_members(
    db: &Database,
    app: &AppType,
    stack: &StackState,
    route: Option<&str>,
) -> Result<Vec<Member>, AppError> {
    if !stack.enabled || stack.members.is_empty() || !supports_stack(app) {
        return Ok(Vec::new());
    }
    let mut members = members(db, app, stack, route)?;
    members.retain(|member| {
        is_published(member)
            || (app == &AppType::Codex && stack.managed_identity_required && member.route)
    });
    Ok(members)
}

/// Claude 的这些成员发布给客户端的模型，按名单顺序。
pub fn claude_published(members: &[Member]) -> Vec<StackModel> {
    members
        .iter()
        .flat_map(|member| claude_models(&member.key, &member.provider))
        .collect()
}

/// 默认那家（路由）列表里的第一个模型：Stack 模式下 Claude Code 的四档别名（启动默认、后台
/// 任务、子代理别名）都指向它。列表的顺序就是模型映射的顺序（`ANTHROPIC_MODEL` 在前），
/// 所以没配列表的行用的是它的主模型。路由那家不在发布的成员里（Stack 模式关着、它没有模型）
/// 时没有。
pub fn claude_route_default(members: &[Member]) -> Option<StackModel> {
    let route = members.iter().find(|member| member.route)?;
    claude_models(&route.key, &route.provider)
        .into_iter()
        .next()
}

/// 这个应用在 Stack 模式（代理模式且 Stack 模式开着）。读不出状态按不在处理。
pub fn stack_mode_now(app: &AppType) -> bool {
    if !supports_stack(app) {
        return false;
    }
    state::stack_mode(&DeviceStore::for_device(), app.as_str()).unwrap_or_else(|error| {
        log::warn!(
            "读取 {} 的 Stack 模式失败，按不在处理: {error}",
            app.as_str()
        );
        false
    })
}

/// 在 Stack 名单里（不管什么模式）。
pub fn is_member(app: &AppType, provider_id: &str) -> Result<bool, AppError> {
    if !supports_stack(app) {
        return Ok(false);
    }
    Ok(state::stack(&DeviceStore::for_device(), app.as_str())?.is_member(provider_id))
}

/// Claude Code 现在发布的 Stack 模型：代理模式下按已落定的名单和路由算，不在代理模式时没有。
pub fn claude_published_now(db: &Database) -> Result<Vec<StackModel>, AppError> {
    let app = AppType::Claude;
    let store = DeviceStore::for_device();
    let mode = state::mode_state(&store, app.as_str())?;
    if !mode.is_proxy() {
        return Ok(Vec::new());
    }
    let stack = state::stack(&store, app.as_str())?;
    Ok(claude_published(&published_members(
        db,
        &app,
        &stack,
        mode.proxy_route.as_deref(),
    )?))
}

/// Claude Desktop 现在发布的 Stack 模型（`/v1/models` 用）：同 [`claude_published_now`]。
pub fn desktop_published_now(db: &Database) -> Result<Vec<DesktopStackModel>, AppError> {
    let app = AppType::ClaudeDesktop;
    let store = DeviceStore::for_device();
    let mode = state::mode_state(&store, app.as_str())?;
    if !mode.is_proxy() {
        return Ok(Vec::new());
    }
    let stack = state::stack(&store, app.as_str())?;
    Ok(desktop_published(&published_members(
        db,
        &app,
        &stack,
        mode.proxy_route.as_deref(),
    )?))
}

/// 选中 Stack 模型的请求要发往的那一家。
#[derive(Debug, Clone)]
pub struct StackTarget {
    pub provider: Provider,
    /// 发往上游的模型名。
    pub upstream_model: String,
    /// 客户端发来的带前缀 id（只用于日志展示）。
    pub original_model: String,
}

/// 带保留前缀的 id 为什么解不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackMiss {
    /// 裸模型没有绑定供应商，不能推断为当前默认供应商。
    IdentityRequired,
    /// 这个应用不在 Stack 模式（路由模式下名单留着，但不发布、也不转发）。
    StackOff,
    /// key 没登记，或 id 切不出 key 和模型。
    Unknown,
    /// 这家已经从 Stack 名单移除。
    Removed,
    /// 这家已经从 CC Switch 删除。
    Deleted,
}

impl StackMiss {
    /// 返回给客户端的错误文案。
    pub fn message(&self, model: &str) -> String {
        let product = crate::osir_identity::product_name();
        match self {
            Self::IdentityRequired => format!(
                "模型 {model} 未指定供应商，请在 Codex 模型列表中重新选择后重试 (Model {model} has no provider identity; select it again from the Codex model list and retry)"
            ),
            Self::StackOff => format!(
                "聚合的模型 {model} 只能在聚合模式下使用，当前没有开启聚合模式，请在模型列表里重新选择 (Aggregated model {model} only works in Stack mode, which is off; pick a model from the model list again)"
            ),
            Self::Unknown => format!(
                "聚合的模型 {model} 在 {product} 里不存在，请在模型列表里重新选择 (Aggregated model {model} is unknown to {product}; pick a model from the model list again)"
            ),
            Self::Removed => format!(
                "聚合的模型 {model} 已从 {product} 移除，请在模型列表里重新选择 (Aggregated model {model} was removed from {product}; pick a model from the model list again)"
            ),
            Self::Deleted => format!(
                "聚合的模型 {model} 对应的供应商已删除，请在模型列表里重新选择 (The provider of aggregated model {model} was deleted; pick a model from the model list again)"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Resolved {
    /// 普通模型名，照旧走代理路由。
    Plain,
    Hit(Box<StackTarget>),
    /// 带保留前缀但解不出来：报错，不回落到默认路由。名单为空时也一样。
    Miss(StackMiss),
}

/// 解析请求里的模型 id。OSIR 托管 Codex Stack 拒绝无法证明归属的裸 id。带前缀的只在
/// Stack 模式下解析：路由模式下名单留着，客户端手里旧的 Stack id 也不能转给名单里的那家。
pub fn resolve(
    db: &Database,
    store: &DeviceStore,
    app: &AppType,
    model: &str,
) -> Result<Resolved, AppError> {
    let (key, model_part, one_m) = match decode(app, model) {
        Decoded::Plain => {
            if app == &AppType::Codex && rejects_plain_codex_model(db, store)? {
                return Ok(Resolved::Miss(StackMiss::IdentityRequired));
            }
            return Ok(Resolved::Plain);
        }
        Decoded::Malformed => return Ok(Resolved::Miss(StackMiss::Unknown)),
        Decoded::Code(code) => return resolve_desktop(db, store, app, model, code),
        Decoded::Stack { key, model, one_m } => (key, model, one_m),
    };
    if !state::stack_mode(store, app.as_str())? {
        return Ok(Resolved::Miss(StackMiss::StackOff));
    }
    let stack = state::stack(store, app.as_str())?;
    let Some(provider_id) = stack.keys.get(key) else {
        return Ok(Resolved::Miss(StackMiss::Unknown));
    };
    let provider = db.get_provider_by_id(provider_id, app.as_str())?;
    let Some(provider) = provider else {
        return Ok(Resolved::Miss(StackMiss::Deleted));
    };
    if !stack.is_member(provider_id) {
        return Ok(Resolved::Miss(StackMiss::Removed));
    }
    if app == &AppType::Codex && !binding_matches(&stack, key, &provider) {
        return Ok(Resolved::Miss(StackMiss::Unknown));
    }
    // Unbound official cards use the client's current native login. An old
    // official ID must not silently borrow another selected official account.
    if app == &AppType::Codex
        && crate::services::provider::codex_direct::is_official(&provider)
        && provider
            .meta
            .as_ref()
            .and_then(|m| m.managed_account_id_for("codex_oauth"))
            .is_none()
        && state::mode_state(store, "codex")?.proxy_route.as_deref() != Some(provider.id.as_str())
    {
        return Ok(Resolved::Miss(StackMiss::Unknown));
    }
    // Validate against the same directory we publish, scoped to this provider.
    // A stale Codex selection must not reach an unpublished upstream model.
    let published = if app == &AppType::Codex
        && crate::services::provider::codex_direct::is_official(&provider)
    {
        official_model_ids(&stack, key)
    } else {
        codex_model_ids(key, &provider)
    };
    if app == &AppType::Codex && !published.iter().any(|published| published == model) {
        return Ok(Resolved::Miss(StackMiss::Unknown));
    }
    // Claude 发往上游的是行里配置的原值（可能带 1M 标记），和路由请求映射出来的一样；
    // 行里已经没有这个模型时照原样发（上游自己决定认不认）。Codex 的 id 就是行里的模型名。
    let upstream_model = match app {
        AppType::Claude => claude_models(key, &provider)
            .into_iter()
            .find(|published| {
                strip_one_m_suffix_for_upstream(&published.upstream).trim() == model_part
            })
            .map(|published| published.upstream),
        _ => None,
    }
    .unwrap_or_else(|| {
        if one_m {
            format!("{model_part}{ONE_M_MARKER_FOR_CLIENT}")
        } else {
            model_part.to_string()
        }
    });
    Ok(Resolved::Hit(Box::new(StackTarget {
        provider,
        upstream_model,
        original_model: model.to_string(),
    })))
}

/// Claude Desktop 的数字 id：先按名单顺序在发布出去的成员（路由那家以外）里找，和
/// [`desktop_published`] 的去重同一个顺序，撞号时命中的就是选择器里显示的那个；再找路由那家
/// （它变成默认后不再发布，但客户端可能还带着旧 id）。找不到再看登记簿里已移除的那些，区分
/// 「已移除」和「不存在」；已删除的供应商算不出模型，归到「不存在」。
fn resolve_desktop(
    db: &Database,
    store: &DeviceStore,
    app: &AppType,
    model: &str,
    code: &str,
) -> Result<Resolved, AppError> {
    if !state::stack_mode(store, app.as_str())? {
        return Ok(Resolved::Miss(StackMiss::StackOff));
    }
    let stack = state::stack(store, app.as_str())?;
    let route = state::mode_state(store, app.as_str())?.proxy_route;
    let id = format!("{DESKTOP_PREFIX}{code}");
    let members = members(db, app, &stack, route.as_deref())?;
    let published = members.iter().filter(|member| !member.route);
    let route = members.iter().filter(|member| member.route);
    for member in published.chain(route) {
        if let Some(found) = desktop_models(&member.key, &member.provider)
            .into_iter()
            .find(|published| published.id == id)
        {
            return Ok(Resolved::Hit(Box::new(StackTarget {
                provider: member.provider.clone(),
                upstream_model: found.upstream,
                original_model: model.to_string(),
            })));
        }
    }
    let find = |key: &str, provider_id: &str| -> Result<bool, AppError> {
        let Some(provider) = db.get_provider_by_id(provider_id, app.as_str())? else {
            return Ok(false);
        };
        Ok(desktop_models(key, &provider)
            .iter()
            .any(|published| published.id == id))
    };
    for (key, provider_id) in &stack.keys {
        if !stack.is_member(provider_id) && find(key, provider_id)? {
            return Ok(Resolved::Miss(StackMiss::Removed));
        }
    }
    Ok(Resolved::Miss(StackMiss::Unknown))
}

/// 给前端：名单里的一家。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackMemberView {
    pub provider_id: String,
    /// 发布给客户端的模型 id。
    pub model_ids: Vec<String>,
    /// 这家是默认那家（代理路由）。Claude 的照常发布，第一个模型同时占着四档别名；Codex 的
    /// 模型是默认路由的目录行，`model_ids` 为空，默认换到别家后才带前缀发布。
    pub route: bool,
    /// 这家不做默认、只做成员时发布几个模型。换默认的预览用它：现在的默认那家（Codex、
    /// Claude Desktop）`model_ids` 为空，换走后它的模型才发布。
    pub member_models: usize,
}

/// 给前端：Codex 聚合面板「子代理模型」下拉的一项（见 `codex_direct::stack_subagent_choices`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackSubagentModel {
    /// 发布给客户端的模型 id（`ccs-<key>/<model>`），也是写进 `[agents]` 的值。
    pub id: String,
    pub provider_id: String,
    /// `<模型显示名>（<供应商名>）`，和模型选择器里一样。
    pub display_name: String,
    /// 这个模型支持的推理强度，按目录里的顺序。
    pub efforts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

/// 给前端：Stack 模式的状态、名单和提示。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackView {
    /// 在 Stack 模式（代理模式且 Stack 模式开着）。
    pub active: bool,
    pub members: Vec<StackMemberView>,
    /// Codex Stack 模型客户端看不到或看不全：`routeOwnsCatalog` 路由那家自己管理模型目录
    /// 文件，Stack 模型不发布；官方做路由时官方模型列表暂未取到：`officialModelsBundled`
    /// 暂用 Codex 自带的列表（可能缺账号专属的模型），`officialModelsUnavailable` Stack 模型
    /// 暂不可用；`officialModelsOutdated` 本机 Codex 太旧，拉到的官方列表里没有能选的模型。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<&'static str>,
    /// `notice` 是 `officialModelsBundled` / `officialModelsUnavailable` 时，没取到列表的原因
    /// 码：先是官方列表的（`noLogin`、`loginStale`、`loginIncomplete`、`noVersion`、
    /// `fetchFailed`、`invalidList`），不可用时再跟自带列表的（`noCli`、`cliFailed`、
    /// `nothingUsable`）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notice_reasons: Vec<&'static str>,
    /// Codex 客户端还在用旧的模型列表（启动时读的目录），Stack 模型看不到，要重启才行。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_clients: Option<StaleClients>,
    /// `stale_clients` 对应的目录和登录（见 `codex_client_catalog::revision`）：用户关掉提示后，
    /// 同一个供应商上目录又改了，这个值变了，前端重新显示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_revision: Option<String>,
    /// Codex：聚合面板「子代理模型」下拉的选项，发布 Stack 模型的成员按名单顺序。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subagent_models: Vec<StackSubagentModel>,
}

pub fn member_views(app: &AppType, members: &[Member]) -> Vec<StackMemberView> {
    members
        .iter()
        .map(|member| StackMemberView {
            provider_id: member.provider.id.clone(),
            model_ids: member.model_ids.clone(),
            route: member.route,
            member_models: if member.route {
                model_ids_of(app, &member.key, &member.provider, false).len()
            } else {
                member.model_ids.len()
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(id: &str, name: &str, icon: Option<&str>, env: Value) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            name.to_string(),
            json!({ "env": env }),
            None,
        );
        provider.icon = icon.map(str::to_string);
        provider
    }

    #[test]
    fn codex_display_preference_uses_saved_connection_name_without_changing_route_identity() {
        let mut row = provider("first", "用户保存的连接", Some("osir"), json!({}));
        let mut stack = StackState::default();
        let key = allocate_key(&mut stack, &row);
        let id = encode(&AppType::Codex, &key, "shared-model", false);
        assert_eq!(provider_display_name("shared-model", &row), "shared-model");
        row.meta = Some(serde_json::from_value(json!({"hideAggregationName": true})).unwrap());
        assert_eq!(provider_display_name("shared-model", &row), "shared-model");
        row.meta.as_mut().unwrap().hide_aggregation_name = Some(false);
        assert_eq!(
            provider_display_name("shared-model", &row),
            "shared-model（用户保存的连接）"
        );
        row.name = "新连接名称".into();
        assert_eq!(
            provider_display_name("shared-model", &row),
            "shared-model（新连接名称）"
        );
        assert_eq!(allocate_key(&mut stack, &row), key);
        assert_eq!(encode(&AppType::Codex, &key, "shared-model", false), id);
        let second = provider("second", "新连接名称", Some("osir"), json!({}));
        let second_key = allocate_key(&mut stack, &second);
        assert_ne!(
            encode(&AppType::Codex, &second_key, "shared-model", false),
            id
        );
    }

    #[test]
    fn official_stack_resolver_rejects_rebound_and_legacy_account_ids() {
        let fx = fixture();
        let mut row = Provider::with_id(
            "native".into(),
            "Official".into(),
            json!({"auth":{}, "config":""}),
            None,
        );
        row.category = Some("official".into());
        let bind = |id: &str| {
            serde_json::from_value(json!({"authBinding": {
                "source":"managed_account", "authProvider":"codex_oauth", "accountId":id
            }}))
            .unwrap()
        };
        row.meta = Some(bind("a"));
        fx.db.save_provider("codex", &row).unwrap();
        let mut key = String::new();
        state::update(&fx.store, |live| {
            let stack = &mut live.apps.get_mut("codex").unwrap().stack;
            stack.members.push(row.id.clone());
            stack.keys.insert("legacy".into(), row.id.clone());
            stack
                .codex_official_catalogs
                .insert("legacy".into(), vec![json!({"slug":"shared"})]);
            key = allocate_key(stack, &row);
            stack
                .codex_official_catalogs
                .insert(key.clone(), vec![json!({"slug":"shared"})]);
        })
        .unwrap();
        let old_id = encode(&AppType::Codex, &key, "shared", false);
        assert_eq!(hit(resolve_in(&fx, AppType::Codex, &old_id)).0, row.id);
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-legacy/shared")),
            StackMiss::Unknown
        );
        row.meta = Some(bind("b"));
        fx.db.save_provider("codex", &row).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, &old_id)),
            StackMiss::Unknown
        );
        state::update(&fx.store, |live| {
            let stack = &mut live.apps.get_mut("codex").unwrap().stack;
            let next = allocate_key(stack, &row);
            stack
                .codex_official_catalogs
                .insert(next, vec![json!({"slug":"shared"})]);
        })
        .unwrap();
        let stack = state::stack(&fx.store, "codex").unwrap();
        let new_id = encode(
            &AppType::Codex,
            stack.key_of(&row.id).unwrap(),
            "shared",
            false,
        );
        assert_eq!(hit(resolve_in(&fx, AppType::Codex, &new_id)).0, row.id);
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, &old_id)),
            StackMiss::Unknown
        );
        row.meta = None;
        fx.db.save_provider("codex", &row).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, &new_id)),
            StackMiss::Unknown
        );
    }

    #[test]
    fn official_rebinding_retires_old_key_without_reassigning_it() {
        let mut provider = Provider::with_id(
            "official-fixture".into(),
            "Official".into(),
            serde_json::json!({"auth":{}, "config":""}),
            None,
        );
        provider.category = Some("official".into());
        let bind = |account: &str| {
            serde_json::from_value(serde_json::json!({"authBinding": {
                "source":"managed_account", "authProvider":"codex_oauth", "accountId":account
            }}))
            .unwrap()
        };
        provider.meta = Some(bind("a"));
        let mut stack = StackState::default();
        let first = allocate_key(&mut stack, &provider);
        assert!(binding_matches(&stack, &first, &provider));
        provider.meta = Some(bind("b"));
        assert!(!binding_matches(&stack, &first, &provider));
        let second = allocate_key(&mut stack, &provider);
        assert_ne!(first, second);
        assert_eq!(stack.keys.get(&first), Some(&provider.id));
        assert_eq!(stack.key_of(&provider.id), Some(second.as_str()));
        assert!(binding_matches(&stack, &second, &provider));
        provider.meta = Some(bind("a"));
        assert!(!binding_matches(&stack, &first, &provider));
        let third = allocate_key(&mut stack, &provider);
        assert_ne!(third, first);
        assert_ne!(third, second);
        let restored: StackState =
            serde_json::from_str(&serde_json::to_string(&stack).unwrap()).unwrap();
        assert!(!binding_matches(&restored, &first, &provider));
        assert!(binding_matches(&restored, &third, &provider));
    }

    #[test]
    fn keys_come_from_the_icon_then_the_name() {
        let mut stack = StackState::default();
        let kimi = provider("a", "Kimi For Coding", Some("kimi"), json!({}));
        let named = provider("b", "My Relay 2.0!", None, json!({}));
        let chinese = provider("c1b2c3d4-e5f6", "智谱", None, json!({}));
        let blank_icon = provider("d", "DeepSeek", Some("  "), json!({}));
        let custom_icon = provider(
            "e",
            "Acme",
            Some("data:image/png;base64,iVBORw0KGgo="),
            json!({}),
        );

        assert_eq!(allocate_key(&mut stack, &kimi), "kimi");
        assert_eq!(allocate_key(&mut stack, &named), "my-relay-2-0");
        assert_eq!(allocate_key(&mut stack, &chinese), "pc1b2c3");
        assert_eq!(allocate_key(&mut stack, &blank_icon), "deepseek");
        assert_eq!(allocate_key(&mut stack, &custom_icon), "acme");
    }

    #[test]
    fn keys_never_contain_the_separators_and_stay_short() {
        for text in ["a--b", "a/b", "--edge--", "UPPER__lower", &"x".repeat(80)] {
            let key = slug(text);
            assert!(!key.contains("--") && !key.contains('/'), "{text} → {key}");
            assert!(
                !key.starts_with('-') && !key.ends_with('-'),
                "{text} → {key}"
            );
            assert!(key.len() <= KEY_MAX_LEN, "{text} → {key}");
        }
        assert_eq!(slug("UPPER__lower"), "upper-lower");
    }

    #[test]
    fn a_key_stays_with_its_provider_forever() {
        let mut stack = StackState::default();
        let a = provider("a", "Kimi", Some("kimi"), json!({}));
        let b = provider("b", "Kimi Coding Plan", Some("kimi"), json!({}));

        assert_eq!(allocate_key(&mut stack, &a), "kimi");
        stack.members.push("a".to_string());
        // A 移除（登记簿保留）后，同图标的 B 拿不到 A 的 key。
        stack.members.clear();
        assert_eq!(allocate_key(&mut stack, &b), "kimi-2");
        // A 重新加入，拿回原来的 key。
        assert_eq!(allocate_key(&mut stack, &a), "kimi");
        assert_eq!(stack.keys.len(), 2);
    }

    #[test]
    fn claude_ids_round_trip() {
        let claude = AppType::Claude;
        for (model, one_m) in [
            ("kimi-k3", false),
            ("glm-5.2", true),
            ("vendor/model", false),
            ("model--with--dashes", true),
        ] {
            let id = encode(&claude, "kimi", model, one_m);
            assert_eq!(
                decode(&claude, &id),
                Decoded::Stack {
                    key: "kimi",
                    model,
                    one_m
                },
                "{id}"
            );
        }
        // 标记大小写不敏感。
        assert_eq!(
            decode(&claude, "ccs-claude-k--m[1m]"),
            Decoded::Stack {
                key: "k",
                model: "m",
                one_m: true
            }
        );
    }

    #[test]
    fn codex_ids_round_trip() {
        let codex = AppType::Codex;
        let id = encode(&codex, "deepseek", "deepseek/deepseek-v4-pro", false);
        assert_eq!(id, "ccs-deepseek/deepseek/deepseek-v4-pro");
        assert_eq!(
            decode(&codex, &id),
            Decoded::Stack {
                key: "deepseek",
                model: "deepseek/deepseek-v4-pro",
                one_m: false
            }
        );
    }

    #[test]
    fn plain_ids_and_other_apps_are_not_decoded() {
        let (claude, codex) = (AppType::Claude, AppType::Codex);
        assert_eq!(decode(&claude, "claude-sonnet-5"), Decoded::Plain);
        assert_eq!(decode(&claude, "kimi-k3"), Decoded::Plain);
        // 路由那家自己的 `deepseek/…` 不被同名的 Stack key 截走。
        assert_eq!(decode(&codex, "deepseek/deepseek-v4-pro"), Decoded::Plain);
        assert_eq!(decode(&codex, "ccs-without-separator"), Decoded::Plain);
        assert_eq!(
            decode(&AppType::ClaudeDesktop, "ccs-claude-k--m"),
            Decoded::Plain
        );
        assert_eq!(decode(&AppType::GrokBuild, "ccs-k/m"), Decoded::Plain);
    }

    #[test]
    fn reserved_ids_that_do_not_split_are_malformed() {
        let (claude, codex) = (AppType::Claude, AppType::Codex);
        assert_eq!(decode(&claude, "ccs-claude-kimi"), Decoded::Malformed);
        assert_eq!(decode(&claude, "ccs-claude---m"), Decoded::Malformed);
        assert_eq!(decode(&claude, "ccs-claude-k--"), Decoded::Malformed);
        assert_eq!(decode(&codex, "ccs-/m"), Decoded::Malformed);
        assert_eq!(decode(&codex, "ccs-k/"), Decoded::Malformed);
    }

    #[test]
    fn claude_rows_publish_each_model_once() {
        let row = provider(
            "p",
            "Zhipu",
            None,
            json!({
                "ANTHROPIC_MODEL": "glm-5.2",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2[1M]",
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "GLM 5.2",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7-air",
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "128000"
            }),
        );
        let models = claude_models("zhipu", &row);
        assert_eq!(
            models,
            vec![
                StackModel {
                    id: "ccs-claude-zhipu--glm-5.2[1M]".to_string(),
                    upstream: "glm-5.2[1M]".to_string(),
                    name: "GLM 5.2".to_string(),
                    display_name: "GLM 5.2（Zhipu）".to_string(),
                    description: "glm-5.2 · 1M".to_string(),
                    one_m: true,
                    window: 128_000,
                    auto_compact_window: CLAUDE_DEFAULT_AUTO_COMPACT_WINDOW,
                },
                StackModel {
                    id: "ccs-claude-zhipu--glm-4.7-air".to_string(),
                    upstream: "glm-4.7-air".to_string(),
                    name: "glm-4.7-air".to_string(),
                    display_name: "glm-4.7-air（Zhipu）".to_string(),
                    description: "glm-4.7-air · 128K".to_string(),
                    one_m: false,
                    window: 128_000,
                    auto_compact_window: CLAUDE_DEFAULT_AUTO_COMPACT_WINDOW,
                },
            ]
        );
        let bare = provider("q", "Bare", None, json!({ "ANTHROPIC_AUTH_TOKEN": "sk" }));
        assert!(claude_models("bare", &bare).is_empty());
        let default_window = provider("r", "R", None, json!({ "ANTHROPIC_MODEL": "m" }));
        assert_eq!(
            claude_models("r", &default_window)[0].window,
            CLAUDE_DEFAULT_WINDOW
        );
    }

    #[test]
    fn the_description_names_the_upstream_model_and_its_window() {
        assert_eq!(model_description("kimi-k3", 256_000), "kimi-k3 · 256K");
        assert_eq!(model_description("kimi-k3", 262_144), "kimi-k3 · 262K");
        assert_eq!(model_description("glm-5.2", 1_000_000), "glm-5.2 · 1M");
        assert_eq!(model_description("glm-5.2", 1_050_000), "glm-5.2 · 1050K");
        assert_eq!(model_description("m", 0), "m");
    }

    fn with_stack_models(mut provider: Provider, models: Value) -> Provider {
        provider.meta = Some(crate::provider::ProviderMeta {
            stack_models: serde_json::from_value(models).unwrap(),
            ..Default::default()
        });
        provider
    }

    #[test]
    fn an_emptied_list_publishes_nothing_and_an_unset_one_follows_the_mapping() {
        let mapped = provider("p", "Kimi", None, json!({ "ANTHROPIC_MODEL": "kimi-k3" }));
        let emptied = with_stack_models(mapped.clone(), json!([]));
        assert!(claude_models("kimi", &emptied).is_empty());
        assert_eq!(
            serde_json::to_value(emptied.meta.as_ref().unwrap()).unwrap()["stackModels"],
            json!([])
        );

        let unset = with_stack_models(mapped, Value::Null);
        assert_eq!(unset.meta.as_ref().unwrap().stack_models, None);
        let ids: Vec<String> = claude_models("kimi", &unset)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["ccs-claude-kimi--kimi-k3"]);
        let meta = serde_json::to_value(unset.meta.as_ref().unwrap()).unwrap();
        assert!(meta.get("stackModels").is_none());
    }

    #[test]
    fn a_stack_model_list_replaces_the_mapping() {
        let row = with_stack_models(
            provider(
                "p",
                "Kimi",
                None,
                json!({
                    "ANTHROPIC_MODEL": "kimi-k3",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "kimi-k3-turbo",
                    "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "256000"
                }),
            ),
            json!([
                { "model": " kimi-k3 ", "displayName": "Kimi K3" },
                { "model": "kimi-k3-thinking", "displayName": " ", "oneM": true },
                { "model": "  " },
                { "model": "kimi-k3[1m]", "displayName": "ignored" }
            ]),
        );
        let summary: Vec<(String, String, String, bool, u64)> = claude_models("kimi", &row)
            .into_iter()
            .map(|model| {
                (
                    model.id,
                    model.upstream,
                    model.display_name,
                    model.one_m,
                    model.window,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "ccs-claude-kimi--kimi-k3[1M]".to_string(),
                    "kimi-k3[1M]".to_string(),
                    "Kimi K3（Kimi）".to_string(),
                    true,
                    256_000,
                ),
                (
                    "ccs-claude-kimi--kimi-k3-thinking[1M]".to_string(),
                    "kimi-k3-thinking[1M]".to_string(),
                    "kimi-k3-thinking（Kimi）".to_string(),
                    true,
                    256_000,
                ),
            ]
        );
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: DeviceStore,
        db: Database,
    }

    /// Claude Code、Codex 都在 Stack 模式。Claude 的 kimi、zhipu 在名单里；gone 登记过但已
    /// 移除；deleted 在名单里但行已经删了。Codex 名单为空。
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::at(dir.path());
        let db = Database::memory().unwrap();
        for row in [
            provider(
                "kimi",
                "Kimi",
                Some("kimi"),
                json!({ "ANTHROPIC_MODEL": "kimi-k3" }),
            ),
            provider(
                "zhipu",
                "Zhipu",
                None,
                json!({ "ANTHROPIC_MODEL": "glm-5.2[1M]" }),
            ),
            provider("gone", "Gone", None, json!({ "ANTHROPIC_MODEL": "g-1" })),
        ] {
            db.save_provider("claude", &row).unwrap();
        }
        state::update(&store, |live| {
            let claude = live.apps.entry("claude".to_string()).or_default();
            claude.mode = Some(state::Mode::Proxy);
            let stack = &mut claude.stack;
            stack.enabled = true;
            stack.members = ["kimi", "zhipu", "deleted"].map(str::to_string).to_vec();
            for id in ["kimi", "zhipu", "gone", "deleted"] {
                stack.keys.insert(id.to_string(), id.to_string());
            }
            let codex = live.apps.entry("codex".to_string()).or_default();
            codex.mode = Some(state::Mode::Proxy);
            codex.stack.enabled = true;
        })
        .unwrap();
        Fixture {
            _dir: dir,
            store,
            db,
        }
    }

    fn resolve_in(fx: &Fixture, app: AppType, model: &str) -> Resolved {
        resolve(&fx.db, &fx.store, &app, model).unwrap()
    }

    fn hit(resolved: Resolved) -> (String, String, String) {
        match resolved {
            Resolved::Hit(target) => (
                target.provider.id,
                target.upstream_model,
                target.original_model,
            ),
            other => panic!("expected a hit, got {other:?}"),
        }
    }

    fn miss(resolved: Resolved) -> StackMiss {
        match resolved {
            Resolved::Miss(miss) => miss,
            other => panic!("expected a miss, got {other:?}"),
        }
    }

    #[test]
    fn a_claude_route_publishes_its_whole_list_and_leads_with_its_default() {
        let fx = fixture();
        let stack = state::stack(&fx.store, "claude").unwrap();
        let members = |route| published_members(&fx.db, &AppType::Claude, &stack, route).unwrap();
        let ids = |route| {
            claude_published(&members(route))
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>()
        };
        // 路由那家照常发布，和不在代理模式时算出来的一样。
        let all = ids(None);
        assert!(all.contains(&"ccs-claude-kimi--kimi-k3".to_string()));
        assert_eq!(ids(Some("kimi")), all);
        assert_eq!(claude_route_default(&members(None)), None);
        let default = claude_route_default(&members(Some("zhipu"))).unwrap();
        assert_eq!(
            (default.id.as_str(), default.name.as_str()),
            ("ccs-claude-zhipu--glm-5.2[1M]", "glm-5.2")
        );

        // 界面上标出路由那家，它的模型 id 照常列出。
        let views = member_views(
            &AppType::Claude,
            &super::members(&fx.db, &AppType::Claude, &stack, Some("kimi")).unwrap(),
        );
        let route_flags: Vec<(&str, bool, usize)> = views
            .iter()
            .map(|view| (view.provider_id.as_str(), view.route, view.model_ids.len()))
            .collect();
        assert_eq!(route_flags, vec![("kimi", true, 1), ("zhipu", false, 1)]);
    }

    #[test]
    fn the_default_is_the_first_model_of_the_routes_list() {
        let fx = fixture();
        let kimi = with_stack_models(
            provider(
                "kimi",
                "Kimi",
                Some("kimi"),
                json!({ "ANTHROPIC_MODEL": "kimi-k3" }),
            ),
            json!([
                { "model": "kimi-k3-mini", "displayName": "K3 Mini" },
                { "model": "kimi-k3" }
            ]),
        );
        fx.db.save_provider("claude", &kimi).unwrap();
        let stack = state::stack(&fx.store, "claude").unwrap();
        let published = published_members(&fx.db, &AppType::Claude, &stack, Some("kimi")).unwrap();
        let default = claude_route_default(&published).unwrap();
        assert_eq!(
            (default.id.as_str(), default.name.as_str()),
            ("ccs-claude-kimi--kimi-k3-mini", "K3 Mini")
        );
        let ids: Vec<String> = claude_published(&published)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "ccs-claude-kimi--kimi-k3-mini".to_string(),
                "ccs-claude-kimi--kimi-k3".to_string(),
                "ccs-claude-zhipu--glm-5.2[1M]".to_string(),
            ]
        );
    }

    #[test]
    fn stacked_ids_resolve_to_their_provider_and_upstream_model() {
        let fx = fixture();
        assert_eq!(
            hit(resolve_in(&fx, AppType::Claude, "ccs-claude-kimi--kimi-k3")),
            (
                "kimi".to_string(),
                "kimi-k3".to_string(),
                "ccs-claude-kimi--kimi-k3".to_string()
            )
        );
        // 行里带 1M 标记的模型：不管客户端发来的 id 带不带标记，上游都用行里的原值。
        for id in ["ccs-claude-zhipu--glm-5.2", "ccs-claude-zhipu--glm-5.2[1m]"] {
            assert_eq!(
                hit(resolve_in(&fx, AppType::Claude, id)).1,
                "glm-5.2[1M]",
                "{id}"
            );
        }
        // 行里已经没有的模型照原样发。
        assert_eq!(
            hit(resolve_in(&fx, AppType::Claude, "ccs-claude-kimi--kimi-k9")).1,
            "kimi-k9"
        );
    }

    #[test]
    fn stacked_ids_that_cannot_be_resolved_never_fall_back_to_the_route() {
        let fx = fixture();
        let claude = |id| miss(resolve_in(&fx, AppType::Claude, id));
        assert_eq!(claude("ccs-claude-gone--g-1"), StackMiss::Removed);
        assert_eq!(claude("ccs-claude-deleted--d-1"), StackMiss::Deleted);
        assert_eq!(claude("ccs-claude-nobody--m"), StackMiss::Unknown);
        assert_eq!(claude("ccs-claude-kimi"), StackMiss::Unknown);
        // 名单为空（这个应用从没加过 Stack 模型）也一样报错。
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-kimi/kimi-k3")),
            StackMiss::Unknown
        );
        let message = StackMiss::Removed.message("ccs-claude-gone--g-1");
        assert!(message.contains("ccs-claude-gone--g-1"), "{message}");
    }

    #[test]
    fn stacked_ids_are_rejected_outside_stack_mode() {
        let fx = fixture();
        let id = "ccs-claude-kimi--kimi-k3";
        assert!(matches!(
            resolve_in(&fx, AppType::Claude, id),
            Resolved::Hit(_)
        ));

        // 路由模式：名单和 key 都留着，客户端手里的旧 id 也不能转给名单里的那家。
        state::update(&fx.store, |live| {
            live.apps.get_mut("claude").unwrap().stack.enabled = false;
        })
        .unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Claude, id)),
            StackMiss::StackOff
        );

        // 退回直连后附加位不动，同样不解析。
        state::update(&fx.store, |live| {
            let claude = live.apps.get_mut("claude").unwrap();
            claude.stack.enabled = true;
            claude.mode = Some(state::Mode::Direct);
        })
        .unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Claude, id)),
            StackMiss::StackOff
        );
        let message = StackMiss::StackOff.message(id);
        assert!(message.contains(id), "{message}");
    }

    #[test]
    fn codex_ids_resolve_to_the_catalog_model_and_leave_the_routes_names_alone() {
        let fx = fixture();
        let mut deepseek = provider("ds", "DeepSeek", Some("deepseek"), json!({}));
        deepseek.settings_config = json!({
            "auth": {},
            "config": "model = \"deepseek-v4-flash\"\n",
            "modelCatalog": { "models": [{ "model": "deepseek-v4-pro" }] },
        });
        fx.db.save_provider("codex", &deepseek).unwrap();
        state::update(&fx.store, |live| {
            let stack = &mut live.apps.entry("codex".to_string()).or_default().stack;
            stack.members = vec!["ds".to_string()];
            stack.keys.insert("deepseek".to_string(), "ds".to_string());
        })
        .unwrap();

        assert_eq!(
            codex_model_ids("deepseek", &deepseek),
            vec!["ccs-deepseek/deepseek-v4-pro"]
        );
        assert_eq!(
            hit(resolve_in(
                &fx,
                AppType::Codex,
                "ccs-deepseek/deepseek-v4-pro"
            )),
            (
                "ds".to_string(),
                "deepseek-v4-pro".to_string(),
                "ccs-deepseek/deepseek-v4-pro".to_string()
            )
        );
        // 路由那家自己的 `deepseek/...`（OpenRouter 写法）不带保留前缀，照旧走默认路由。
        assert!(matches!(
            resolve_in(&fx, AppType::Codex, "deepseek/deepseek-v4-pro"),
            Resolved::Plain
        ));
    }

    #[test]
    fn codex_stale_models_are_rejected_per_provider_publication() {
        let fx = fixture();
        let mut first = provider("first", "First", None, json!({}));
        first.settings_config = json!({
            "config": "model = \"stale-default\"\n",
            "modelCatalog": { "models": [{ "model": "vendor/shared" }] },
        });
        let mut second = first.clone();
        second.id = "second".to_string();
        fx.db.save_provider("codex", &first).unwrap();
        fx.db.save_provider("codex", &second).unwrap();
        state::update(&fx.store, |live| {
            let stack = &mut live.apps.entry("codex".to_string()).or_default().stack;
            stack.members = vec!["first".to_string(), "second".to_string()];
            stack.keys.insert("first".to_string(), "first".to_string());
            stack
                .keys
                .insert("second".to_string(), "second".to_string());
        })
        .unwrap();
        let old = "ccs-first/vendor/shared";
        assert_eq!(hit(resolve_in(&fx, AppType::Codex, old)).1, "vendor/shared");
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-first/stale-default")),
            StackMiss::Unknown
        );
        first.settings_config["modelCatalog"]["models"] = json!([{ "model": "replacement" }]);
        fx.db.save_provider("codex", &first).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, old)),
            StackMiss::Unknown
        );
        assert_eq!(
            hit(resolve_in(&fx, AppType::Codex, "ccs-second/vendor/shared")).0,
            "second"
        );
        assert_eq!(
            hit(resolve_in(&fx, AppType::Codex, "ccs-first/replacement")).1,
            "replacement"
        );
        // Empty/absent catalogs follow the existing publisher default fallback.
        for catalog in [json!({ "models": [] }), Value::Null] {
            first.settings_config["modelCatalog"] = catalog;
            fx.db.save_provider("codex", &first).unwrap();
            assert_eq!(
                hit(resolve_in(&fx, AppType::Codex, "ccs-first/stale-default")).1,
                "stale-default"
            );
            assert_eq!(
                miss(resolve_in(&fx, AppType::Codex, old)),
                StackMiss::Unknown
            );
        }
    }

    #[test]
    fn managed_codex_identity_survives_default_switch_and_rejects_stale_routes() {
        use crate::services::osir::managed::test_support::Fixture as Grant;
        let fx = fixture();
        let settings = json!({"auth": {}, "config": "model = \"shared\"\nmodel_provider = \"osir\"\n[model_providers.osir]\nname = \"OSIR\"\nbase_url = \"https://api.osirclaw.com/v1\"\nwire_api = \"responses\"\n", "modelCatalog": {"models": [{"model": "shared"}]}});
        let a = Grant::new(&AppType::Codex, settings.clone(), "fixture-a-secret");
        let b = Grant::new(&AppType::Codex, settings, "fixture-b-secret");
        for row in [&a.provider, &b.provider] {
            fx.db.save_provider("codex", row).unwrap();
        }
        state::update(&fx.store, |live| {
            let codex = live.apps.get_mut("codex").unwrap();
            codex.stack.members = vec![a.provider.id.clone(), b.provider.id.clone()];
            codex.stack.keys.insert("a".into(), a.provider.id.clone());
            codex.stack.keys.insert("b".into(), b.provider.id.clone());
            codex.proxy_route = Some(a.provider.id.clone());
        })
        .unwrap();
        for default in [&a.provider.id, &b.provider.id] {
            state::update(&fx.store, |live| {
                live.apps.get_mut("codex").unwrap().proxy_route = Some(default.clone())
            })
            .unwrap();
            let stack = state::stack(&fx.store, "codex").unwrap();
            let rows = published_members(&fx.db, &AppType::Codex, &stack, Some(default)).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows.iter().find(|m| m.route).unwrap().model_ids.len(), 1);
            assert_eq!(
                hit(resolve_in(&fx, AppType::Codex, "ccs-a/shared")).0,
                a.provider.id
            );
            assert_eq!(
                hit(resolve_in(&fx, AppType::Codex, "ccs-b/shared")).0,
                b.provider.id
            );
            assert_eq!(
                miss(resolve_in(&fx, AppType::Codex, "shared")),
                StackMiss::IdentityRequired
            );
        }
        let message = StackMiss::IdentityRequired.message("gpt-6.1-sol");
        assert!(message.contains("未指定供应商"));
        assert!(!message.contains("不存在"));
        assert!(
            !state::stack(&fx.store, "codex")
                .unwrap()
                .managed_identity_required
        );
        let mut changed = a.provider.clone();
        changed.settings_config["modelCatalog"]["models"] = json!([{"model": "replacement"}]);
        fx.db.save_provider("codex", &changed).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-a/shared")),
            StackMiss::Unknown
        );
        changed.settings_config["modelCatalog"]["models"] = json!([]);
        fx.db.save_provider("codex", &changed).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-a/shared")),
            StackMiss::Unknown
        );
        state::update(&fx.store, |live| {
            live.apps
                .get_mut("codex")
                .unwrap()
                .stack
                .members
                .retain(|id| id != &a.provider.id)
        })
        .unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-a/shared")),
            StackMiss::Removed
        );
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "shared")),
            StackMiss::IdentityRequired
        );
        assert_eq!(
            hit(resolve_in(&fx, AppType::Codex, "ccs-b/shared")).0,
            b.provider.id
        );
        fx.db.delete_provider("codex", &a.provider.id).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-a/shared")),
            StackMiss::Deleted
        );
        state::update(&fx.store, |live| {
            live.apps.get_mut("codex").unwrap().stack.enabled = false
        })
        .unwrap();
        assert!(matches!(
            resolve_in(&fx, AppType::Codex, "shared"),
            Resolved::Plain
        ));
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "ccs-a/shared")),
            StackMiss::StackOff
        );
    }

    #[test]
    fn managed_tombstone_survives_all_rows_deleted_and_restart() {
        use crate::services::osir::managed::test_support::Fixture as Grant;
        let fx = fixture();
        let grant = Grant::new(
            &AppType::Codex,
            json!({
                "auth": {}, "config": "model = \"shared\"\nmodel_provider = \"osir\"\n[model_providers.osir]\nname = \"OSIR\"\nbase_url = \"https://api.osirclaw.com/v1\"\nwire_api = \"responses\"\n",
                "modelCatalog": {"models": [{"model": "shared"}]}
            }),
            "fixture-tombstone-secret",
        );
        fx.db.save_provider("codex", &grant.provider).unwrap();
        let mut ordinary = provider("ordinary", "Ordinary", None, json!({}));
        ordinary.settings_config = json!({"config": "model = \"shared\""});
        fx.db.save_provider("codex", &ordinary).unwrap();
        let key = state::update(&fx.store, |live| {
            let codex = live.apps.get_mut("codex").unwrap();
            // Exercise the existing-key upgrade path, not only a fresh key.
            codex
                .stack
                .keys
                .insert("old-key".into(), grant.provider.id.clone());
            let key = allocate_key(&mut codex.stack, &grant.provider);
            codex.stack.members = vec![grant.provider.id.clone()];
            codex.proxy_route = Some(grant.provider.id.clone());
            key
        })
        .unwrap();
        assert_eq!(key, "old-key");
        // No request has run: persistence must come from the write transaction.
        state::update(&fx.store, |live| {
            let codex = live.apps.get_mut("codex").unwrap();
            codex.stack.members = vec![ordinary.id.clone()];
            allocate_key(&mut codex.stack, &ordinary);
            codex.proxy_route = Some(ordinary.id.clone());
        })
        .unwrap();
        fx.db.delete_provider("codex", &grant.provider.id).unwrap();
        let reopened = DeviceStore::at(fx._dir.path());
        assert!(
            state::stack(&reopened, "codex")
                .unwrap()
                .managed_identity_required
        );
        assert_eq!(
            miss(resolve(&fx.db, &reopened, &AppType::Codex, "shared").unwrap()),
            StackMiss::IdentityRequired
        );
        assert_eq!(
            miss(resolve(&fx.db, &reopened, &AppType::Codex, "ccs-old-key/shared").unwrap()),
            StackMiss::Deleted
        );
        fx.db.delete_provider("codex", &ordinary.id).unwrap();
        assert_eq!(
            miss(resolve(&fx.db, &reopened, &AppType::Codex, "shared").unwrap()),
            StackMiss::IdentityRequired
        );
        // Toggling Stack does not erase the marker; ordinary routing stays usable.
        state::update(&reopened, |live| {
            live.apps.get_mut("codex").unwrap().stack.enabled = false
        })
        .unwrap();
        assert!(matches!(
            resolve(&fx.db, &reopened, &AppType::Codex, "shared").unwrap(),
            Resolved::Plain
        ));
        state::update(&reopened, |live| {
            live.apps.get_mut("codex").unwrap().stack.enabled = true
        })
        .unwrap();
        assert_eq!(
            miss(resolve(&fx.db, &reopened, &AppType::Codex, "shared").unwrap()),
            StackMiss::IdentityRequired
        );
    }

    #[test]
    fn managed_legacy_requests_are_read_only_and_unknown_tombstones_fail_closed() {
        let fx = fixture();
        state::update(&fx.store, |live| {
            live.apps
                .get_mut("codex")
                .unwrap()
                .stack
                .keys
                .insert("lost".into(), "deleted-before-upgrade".into());
        })
        .unwrap();
        let before = std::fs::read(fx.store.state_path()).unwrap();
        assert_eq!(
            miss(resolve_in(&fx, AppType::Codex, "shared")),
            StackMiss::IdentityRequired
        );
        assert_eq!(std::fs::read(fx.store.state_path()).unwrap(), before);
        migrate_codex_identity_locked(&fx.db, &fx.store).unwrap();
        assert!(
            state::stack(&fx.store, "codex")
                .unwrap()
                .managed_identity_required
        );
        std::fs::write(fx.store.state_path(), "{ broken").unwrap();
        assert!(resolve(&fx.db, &fx.store, &AppType::Codex, "shared").is_err());
        assert_eq!(
            std::fs::read_to_string(fx.store.state_path()).unwrap(),
            "{ broken"
        );
    }

    #[test]
    fn ordinary_codex_tombstones_keep_plain_model_compatibility() {
        let fx = fixture();
        let row = provider("ordinary", "Ordinary", None, json!({}));
        fx.db.save_provider("codex", &row).unwrap();
        state::update(&fx.store, |live| {
            allocate_key(&mut live.apps.get_mut("codex").unwrap().stack, &row);
        })
        .unwrap();
        fx.db.delete_provider("codex", &row.id).unwrap();
        assert!(
            !state::stack(&fx.store, "codex")
                .unwrap()
                .managed_identity_required
        );
        assert!(matches!(
            resolve_in(&fx, AppType::Codex, "shared"),
            Resolved::Plain
        ));
        // Corrupt policy must not bypass a possibly persisted managed tombstone.
        std::fs::write(fx.store.state_path(), "{ broken").unwrap();
        assert!(resolve(&fx.db, &fx.store, &AppType::Codex, "shared").is_err());
    }

    #[test]
    fn plain_models_do_not_read_any_state() {
        let fx = fixture();
        // 状态文件坏了也不影响普通请求：不带保留前缀时根本不读。
        std::fs::write(fx.store.state_path(), "{ not json").unwrap();
        for (app, model) in [
            (AppType::Claude, "claude-sonnet-5"),
            (AppType::Claude, "kimi-k3"),
            (AppType::ClaudeDesktop, "ccs-claude-kimi--kimi-k3"),
        ] {
            assert!(
                matches!(resolve_in(&fx, app, model), Resolved::Plain),
                "{model}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(fx.store.state_path()).unwrap(),
            "{ not json"
        );
    }

    mod claude_desktop {
        use super::*;
        use crate::provider::{ClaudeDesktopMode, ClaudeDesktopModelRoute, ProviderMeta};

        /// Desktop 2.31226.1 选择器的过滤（`bo`）：命中厂商黑名单就丢，否则 id 要是纯档位别名或
        /// 含 claude 等字样。黑名单随 Desktop 版本变长，升级后从 app.asar 里重新抄一遍。
        fn desktop_keeps(id: &str) -> bool {
            let blacklist = regex::Regex::new(
                r"ark-code|astron|command-r|deepseek|doubao|gemini|gemma|glm|gpt|grok|hermes|hy3|kimi|lfm|\bling\b|llama|longcat|mimo|minimax|mistral|mixtral|moonshot|nemotron|openai|phi-|qianfan|qwen|tc-code|\bunic\b|yi-|stepfun|step-3|seed-|bytedance|hunyuan|granite|amazon\.nova|nova-|devstral|ministral|ernie|codex|arcee|trinity|abab|phi\d|\bk2\.|\bm2\.|jamba|arctic|solar|mercury|zamba|kat-coder|\bds-|dpsk",
            )
            .unwrap();
            let id = id.to_lowercase();
            !blacklist.is_match(&id)
                && [
                    "claude",
                    "sonnet",
                    "opus",
                    "haiku",
                    "fable",
                    "mythos",
                    "anthropic",
                ]
                .iter()
                .any(|word| id.contains(word))
        }

        /// Desktop 归一化模型身份时剥掉末尾的 `-<8 位数字>`（日期）。
        fn desktop_identity(id: &str) -> String {
            let id = id.to_lowercase();
            regex::Regex::new(r"[-@]\d{8}$")
                .unwrap()
                .replace(&id, "")
                .into_owned()
        }

        fn card(id: &str, name: &str, routes: &[(&str, &str, Option<&str>, bool)]) -> Provider {
            let mut provider = Provider::with_id(
                id.to_string(),
                name.to_string(),
                json!({ "env": {
                    "ANTHROPIC_BASE_URL": format!("https://{id}.example"),
                    "ANTHROPIC_AUTH_TOKEN": format!("sk-{id}"),
                }}),
                None,
            );
            provider.meta = Some(ProviderMeta {
                claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
                claude_desktop_model_routes: routes
                    .iter()
                    .map(|(route, model, label, one_m)| {
                        (
                            route.to_string(),
                            ClaudeDesktopModelRoute {
                                model: model.to_string(),
                                label_override: label.map(str::to_string),
                                supports_1m: one_m.then_some(true),
                            },
                        )
                    })
                    .collect(),
                ..Default::default()
            });
            provider
        }

        /// Desktop 在聚合模式。kimi、zhipu 在名单里；gone 登记过但已移除。
        fn fixture() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let store = DeviceStore::at(dir.path());
            let db = Database::memory().unwrap();
            for row in [
                card(
                    "kimi",
                    "Kimi",
                    &[
                        ("claude-sonnet-5", "kimi-k3", Some("Kimi K3"), true),
                        ("claude-haiku-4-5", "kimi-k3-turbo", None, false),
                    ],
                ),
                card(
                    "zhipu",
                    "Zhipu",
                    &[("claude-sonnet-5", "glm-5.2", None, false)],
                ),
                card("gone", "Gone", &[("claude-sonnet-5", "g-1", None, false)]),
            ] {
                db.save_provider("claude-desktop", &row).unwrap();
            }
            state::update(&store, |live| {
                let desktop = live.apps.entry("claude-desktop".to_string()).or_default();
                desktop.mode = Some(state::Mode::Proxy);
                desktop.proxy_route = Some("zhipu".to_string());
                let stack = &mut desktop.stack;
                stack.enabled = true;
                stack.members = vec!["zhipu".to_string(), "kimi".to_string()];
                for key in ["kimi", "zhipu", "gone"] {
                    stack.keys.insert(key.to_string(), key.to_string());
                }
            })
            .unwrap();
            Fixture {
                _dir: dir,
                store,
                db,
            }
        }

        #[test]
        fn ids_are_digits_only_and_survive_the_desktop_picker() {
            let id = encode(&AppType::ClaudeDesktop, "kimi", "kimi-k3", true);
            assert_eq!(
                id,
                encode(&AppType::ClaudeDesktop, "kimi", "kimi-k3", false)
            );
            assert_ne!(
                id,
                encode(&AppType::ClaudeDesktop, "kimi", "kimi-k3-turbo", false)
            );
            assert_ne!(
                id,
                encode(&AppType::ClaudeDesktop, "kimi-2", "kimi-k3", false)
            );
            // 散列写死，换算法会让客户端手里的 id 失效
            assert_eq!(id, "claude-fable-ccs-473908");

            let keys = [
                "kimi",
                "zhipu",
                "deepseek",
                "seed",
                "my-relay-2-0",
                "p1a2b3",
            ];
            let models = [
                "kimi-k3",
                "glm-5.2",
                "deepseek-v4-pro",
                "seed-oss",
                "gpt-5.5",
                "k2.5",
            ];
            for key in keys {
                for model in models {
                    let id = encode(&AppType::ClaudeDesktop, key, model, false);
                    let digits = id.strip_prefix(DESKTOP_PREFIX).unwrap();
                    assert_eq!(digits.len(), DESKTOP_CODE_DIGITS, "{id}");
                    assert!(digits.bytes().all(|b| b.is_ascii_digit()), "{id}");
                    assert!(desktop_keeps(&id), "Desktop would drop {id}");
                    assert_eq!(desktop_identity(&id), id, "Desktop would trim {id}");
                    assert_eq!(decode(&AppType::ClaudeDesktop, &id), Decoded::Code(digits));
                }
            }
            // Claude Code 的写法 Desktop 会丢掉，所以不能照搬
            assert!(!desktop_keeps("ccs-claude-kimi--kimi-k3"));
        }

        #[test]
        fn decoding_takes_the_1m_row_and_rejects_anything_but_six_digits() {
            let app = AppType::ClaudeDesktop;
            assert_eq!(
                decode(&app, "claude-fable-ccs-012345[1m]"),
                Decoded::Code("012345")
            );
            assert_eq!(decode(&app, "claude-fable-ccs-1234"), Decoded::Malformed);
            assert_eq!(
                decode(&app, "claude-fable-ccs-12345678"),
                Decoded::Malformed
            );
            assert_eq!(decode(&app, "claude-fable-ccs-12345a"), Decoded::Malformed);
            assert_eq!(decode(&app, "claude-sonnet-5"), Decoded::Plain);
            assert_eq!(decode(&app, "claude-fable-5"), Decoded::Plain);
            // 别的应用不认这个前缀
            assert_eq!(
                decode(&AppType::Claude, "claude-fable-ccs-123456"),
                Decoded::Plain
            );
        }

        #[test]
        fn members_publish_their_mapped_models_and_the_route_publishes_none() {
            let fx = fixture();
            let stack = state::stack(&fx.store, "claude-desktop").unwrap();
            let members =
                published_members(&fx.db, &AppType::ClaudeDesktop, &stack, Some("zhipu")).unwrap();
            let published = desktop_published(&members);
            assert_eq!(
                published
                    .iter()
                    .map(|model| (
                        model.upstream.as_str(),
                        model.label.as_str(),
                        model.supports_1m
                    ))
                    .collect::<Vec<_>>(),
                vec![
                    ("kimi-k3-turbo", "Kimi / kimi-k3-turbo", false),
                    ("kimi-k3", "Kimi / Kimi K3", true),
                ]
            );
            assert!(published.iter().all(|model| desktop_keeps(&model.id)));
            // 默认那家现在不发布，但换默认的预览要知道它换走后会发布几个
            let views = member_views(
                &AppType::ClaudeDesktop,
                &super::super::members(&fx.db, &AppType::ClaudeDesktop, &stack, Some("zhipu"))
                    .unwrap(),
            );
            let counts: Vec<(&str, usize, usize)> = views
                .iter()
                .map(|view| {
                    (
                        view.provider_id.as_str(),
                        view.model_ids.len(),
                        view.member_models,
                    )
                })
                .collect();
            assert_eq!(counts, vec![("zhipu", 0, 1), ("kimi", 2, 2)]);

            // 默认换成 kimi 后，zhipu 的模型带数字 id 发布，kimi 的不再发布
            let members =
                published_members(&fx.db, &AppType::ClaudeDesktop, &stack, Some("kimi")).unwrap();
            assert_eq!(
                desktop_published(&members)
                    .into_iter()
                    .map(|model| model.upstream)
                    .collect::<Vec<_>>(),
                vec!["glm-5.2"]
            );
        }

        #[test]
        fn ids_resolve_to_the_member_and_never_fall_back_to_the_route() {
            let fx = fixture();
            let app = AppType::ClaudeDesktop;
            let kimi = encode(&app, "kimi", "kimi-k3", false);
            assert_eq!(
                hit(resolve_in(&fx, app.clone(), &format!("{kimi}[1m]"))),
                (
                    "kimi".to_string(),
                    "kimi-k3".to_string(),
                    format!("{kimi}[1m]")
                )
            );
            // 路由那家自己的数字 id 也认（它做默认前客户端可能选过）
            let zhipu = encode(&app, "zhipu", "glm-5.2", false);
            assert_eq!(hit(resolve_in(&fx, app.clone(), &zhipu)).1, "glm-5.2");

            let gone = encode(&app, "gone", "g-1", false);
            assert_eq!(
                miss(resolve_in(&fx, app.clone(), &gone)),
                StackMiss::Removed
            );
            assert_eq!(
                miss(resolve_in(
                    &fx,
                    app.clone(),
                    &encode(&app, "kimi", "kimi-k9", false)
                )),
                StackMiss::Unknown
            );
            assert_eq!(
                miss(resolve_in(&fx, app.clone(), "claude-fable-ccs-12")),
                StackMiss::Unknown
            );
            // 路由那家的映射表 id 不经过这里
            assert!(matches!(
                resolve_in(&fx, app.clone(), "claude-sonnet-5"),
                Resolved::Plain
            ));

            state::update(&fx.store, |live| {
                live.apps.get_mut("claude-desktop").unwrap().stack.enabled = false;
            })
            .unwrap();
            assert_eq!(miss(resolve_in(&fx, app, &kimi)), StackMiss::StackOff);
        }

        #[test]
        fn a_colliding_id_resolves_to_the_published_member_not_an_earlier_route() {
            let app = AppType::ClaudeDesktop;
            let id = encode(&app, "relay-1944", "kimi-k3", false);
            assert_eq!(id, encode(&app, "relay-484", "kimi-k3", false));

            let dir = tempfile::tempdir().unwrap();
            let fx = Fixture {
                store: DeviceStore::at(dir.path()),
                db: Database::memory().unwrap(),
                _dir: dir,
            };
            for key in ["relay-484", "relay-1944"] {
                let row = card(key, key, &[("claude-sonnet-5", "kimi-k3", None, false)]);
                fx.db.save_provider("claude-desktop", &row).unwrap();
            }
            state::update(&fx.store, |live| {
                let desktop = live.apps.entry("claude-desktop".to_string()).or_default();
                desktop.mode = Some(state::Mode::Proxy);
                desktop.proxy_route = Some("relay-484".to_string());
                let stack = &mut desktop.stack;
                stack.enabled = true;
                // 路由那家排在前面
                stack.members = vec!["relay-484".to_string(), "relay-1944".to_string()];
                for key in ["relay-484", "relay-1944"] {
                    stack.keys.insert(key.to_string(), key.to_string());
                }
            })
            .unwrap();

            let stack = state::stack(&fx.store, "claude-desktop").unwrap();
            let members = published_members(&fx.db, &app, &stack, Some("relay-484")).unwrap();
            let published = desktop_published(&members);
            assert_eq!(published.len(), 1);
            assert_eq!(published[0].label, "relay-1944 / kimi-k3");
            assert_eq!(hit(resolve_in(&fx, app, &id)).0, "relay-1944");
        }
    }
}
