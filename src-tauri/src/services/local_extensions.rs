//! Read-only inventory of native extension files. These sources must never be
//! reused as destinations by configuration projection or automatic sync.
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;
use serde_json::Value;

const MAX_CONFIG: u64 = 4 * 1024 * 1024;
const MAX_PROJECTS: usize = 64;
const MAX_ENTRIES: usize = 8192;
static ORIGINAL_ROOTS: OnceLock<Vec<(String, PathBuf)>> = OnceLock::new();

/// Called before channel isolation changes process environment variables.
/// Keep only paths; no credential values are captured.
pub(crate) fn capture_environment() {
    ORIGINAL_ROOTS.get_or_init(|| {
        [
            ("codex", "CODEX_HOME"),
            ("claude", "CLAUDE_CONFIG_DIR"),
            ("opencode", "OPENCODE_CONFIG_DIR"),
            // The development launcher already isolated HOME before native startup.
            ("codex", "OSIR_HISTORY_CODEX_HOME"),
            ("claude", "OSIR_HISTORY_CLAUDE_HOME"),
            ("gemini", "OSIR_HISTORY_GEMINI_HOME"),
        ]
        .into_iter()
        .filter_map(|(app, name)| {
            let path = PathBuf::from(std::env::var_os(name)?);
            path.is_absolute().then(|| (app.to_string(), path))
        })
        .collect()
    });
}

#[derive(Clone)]
struct Source {
    app: String,
    path: PathBuf,
    scope: String,
    toml: bool,
    field: &'static str,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalExtensionInventory {
    pub mcp: Vec<LocalMcpEntry>,
    pub sources: Vec<LocalExtensionSource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMcpEntry {
    pub id: String,
    pub name: String,
    pub app: String,
    pub source_path: String,
    pub scope: String,
    pub transport: String,
    pub configured: bool,
    pub managed: bool,
}

const LOCAL_IMPORT_TAG: &str = "osir-local-import";
const RETAINED: &str = "••••（已保存）";

/// Imported native definitions remain in the native database. An editor gets
/// placeholders and can replace them explicitly; unchanged placeholders merge
/// back into the previous value before validation and config writes.
pub(crate) fn redact_imported(server: &mut crate::app_config::McpServer) {
    fn redact(value: &mut Value, field: &str) {
        match value {
            Value::String(text)
                if !(field == "type"
                    && matches!(text.as_str(), "stdio" | "http" | "sse" | "local" | "remote")) =>
            {
                *text = RETAINED.into()
            }
            Value::Array(values) => values.iter_mut().for_each(|value| redact(value, "")),
            Value::Object(values) => values
                .iter_mut()
                .for_each(|(field, value)| redact(value, field)),
            _ => (),
        }
    }
    if server.tags.iter().any(|tag| tag == LOCAL_IMPORT_TAG) {
        redact(&mut server.server, "");
    }
}

pub(crate) fn retain_imported_values(
    next: &mut crate::app_config::McpServer,
    previous: &crate::app_config::McpServer,
) {
    fn retain(next: &mut Value, previous: &Value) {
        if next.as_str() == Some(RETAINED) {
            *next = previous.clone();
            return;
        }
        match (next, previous) {
            (Value::Object(next), Value::Object(previous)) => {
                for (key, value) in next {
                    if let Some(old) = previous.get(key) {
                        retain(value, old);
                    }
                }
            }
            (Value::Array(next), Value::Array(previous)) => {
                for (value, old) in next.iter_mut().zip(previous) {
                    retain(value, old);
                }
            }
            _ => (),
        }
    }
    if previous.tags.iter().any(|tag| tag == LOCAL_IMPORT_TAG) {
        retain(&mut next.server, &previous.server);
        if !next.tags.iter().any(|tag| tag == LOCAL_IMPORT_TAG) {
            next.tags.push(LOCAL_IMPORT_TAG.into());
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalExtensionSource {
    pub app: String,
    pub path: String,
    pub status: &'static str,
}

fn read_config(path: &Path, toml: bool) -> Result<Option<Value>, &'static str> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("unreadable"),
    };
    let metadata = file.metadata().map_err(|_| "unreadable")?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG {
        return Err("invalid");
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable")?;
    if bytes.len() as u64 > MAX_CONFIG {
        return Err("invalid");
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid")?;
    if toml {
        let value = text.parse::<toml::Value>().map_err(|_| "invalid")?;
        serde_json::to_value(value).map(Some).map_err(|_| "invalid")
    } else if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "opencode.json" | "opencode.jsonc"))
    {
        crate::jsonc_document::JsoncDocument::parse(text)
            .map(|document| Some(document.value().clone()))
            .map_err(|_| "invalid")
    } else {
        serde_json::from_str(text).map(Some).map_err(|_| "invalid")
    }
}

fn add_root(roots: &mut Vec<(String, PathBuf)>, app: &str, path: PathBuf) {
    if path.is_absolute() && !roots.iter().any(|entry| entry.0 == app && entry.1 == path) {
        roots.push((app.into(), path));
    }
}

fn resolve_custom(value: &str, home: &Path) -> Option<PathBuf> {
    let path = value.trim();
    let path = if let Some(suffix) = path.strip_prefix("~/") {
        home.join(suffix)
    } else {
        PathBuf::from(path)
    };
    path.is_absolute().then_some(path)
}

fn roots() -> Result<(PathBuf, Vec<(String, PathBuf)>), String> {
    let home = if crate::distribution::is_osir() {
        crate::distribution::system_user_home().map_err(|_| "无法读取本机用户目录，请重试。")?
    } else {
        crate::config::get_home_dir()
    };
    let settings = crate::settings::get_settings();
    let mut roots = default_roots(&home);
    for (app, value) in [
        ("codex", settings.codex_config_dir),
        ("claude", settings.claude_config_dir),
        ("gemini", settings.gemini_config_dir),
        ("opencode", settings.opencode_config_dir),
    ] {
        if let Some(path) = value
            .as_deref()
            .and_then(|value| resolve_custom(value, &home))
        {
            add_root(&mut roots, app, path);
        }
    }
    if let Some(original) = ORIGINAL_ROOTS.get() {
        for (app, path) in original {
            add_root(&mut roots, app, path.clone());
        }
    }
    // Include current isolated/custom destinations too, without treating them
    // as the only native source or writing to the external paths above.
    let current = crate::config::get_home_dir();
    for (app, path) in default_roots(&current) {
        add_root(&mut roots, &app, path);
    }
    Ok((home, roots))
}

fn default_roots(home: &Path) -> Vec<(String, PathBuf)> {
    vec![
        ("codex".into(), home.join(".codex")),
        ("claude".into(), home.join(".claude")),
        ("gemini".into(), home.join(".gemini")),
        ("opencode".into(), home.join(".config/opencode")),
    ]
}

fn sources(home: &Path, roots: &[(String, PathBuf)]) -> (Vec<Source>, Vec<PathBuf>) {
    let mut sources = Vec::new();
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    for (app, root) in roots {
        let (file, field, toml) = match app.as_str() {
            "codex" => ("config.toml", "mcp_servers", true),
            "claude" => (".claude.json", "mcpServers", false),
            "gemini" => ("settings.json", "mcpServers", false),
            "opencode" => ("opencode.json", "mcp", false),
            _ => continue,
        };
        let mut paths = vec![
            if app == "opencode" && root.join("opencode.jsonc").exists() {
                root.join("opencode.jsonc")
            } else {
                root.join(file)
            },
        ];
        if app == "claude" && root.file_name().is_some_and(|name| name == ".claude") {
            if let Some(parent) = root.parent() {
                paths.push(parent.join(".claude.json"));
            }
        }
        for path in paths {
            if seen.insert(path.clone()) {
                if matches!(app.as_str(), "codex" | "claude") {
                    if let Ok(Some(value)) = read_config(&path, toml) {
                        if let Some(entries) = value.get("projects").and_then(Value::as_object) {
                            for key in entries.keys().take(MAX_PROJECTS) {
                                add_project(&mut projects, PathBuf::from(key));
                            }
                        }
                    }
                }
                sources.push(Source {
                    app: app.clone(),
                    path,
                    scope: "user".into(),
                    toml,
                    field,
                });
            }
        }
        if app == "codex" {
            if let Ok(Some(state)) = read_config(&root.join(".codex-global-state.json"), false) {
                for key in ["electron-saved-workspace-roots", "saved-project-roots"] {
                    if let Some(paths) = state.get(key).and_then(Value::as_array) {
                        for path in paths.iter().filter_map(Value::as_str).take(MAX_PROJECTS) {
                            add_project(&mut projects, PathBuf::from(path));
                        }
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    sources.push(Source {
        app: "claude-desktop".into(),
        path: home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        scope: "user".into(),
        toml: false,
        field: "mcpServers",
    });
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        sources.push(Source {
            app: "claude-desktop".into(),
            path: PathBuf::from(appdata).join("Claude/claude_desktop_config.json"),
            scope: "user".into(),
            toml: false,
            field: "mcpServers",
        });
    }
    for project in &projects {
        for (app, suffix, field, toml) in [
            ("claude", ".mcp.json", "mcpServers", false),
            ("codex", ".codex/config.toml", "mcp_servers", true),
            ("gemini", ".gemini/settings.json", "mcpServers", false),
            ("opencode", "opencode.json", "mcp", false),
        ] {
            let path = if app == "opencode" && project.join("opencode.jsonc").exists() {
                project.join("opencode.jsonc")
            } else {
                project.join(suffix)
            };
            if seen.insert(path.clone()) {
                sources.push(Source {
                    app: app.into(),
                    path,
                    scope: "project".into(),
                    toml,
                    field,
                });
            }
        }
    }
    (sources, projects)
}

fn add_project(projects: &mut Vec<PathBuf>, path: PathBuf) {
    if projects.len() < MAX_PROJECTS
        && path.is_absolute()
        && path.is_dir()
        && !projects.contains(&path)
    {
        projects.push(path);
    }
}

pub(crate) fn skill_sources() -> Vec<(PathBuf, String)> {
    let Ok((home, roots)) = roots() else {
        return Vec::new();
    };
    let (_, projects) = sources(&home, &roots);
    let mut result: Vec<_> = roots
        .into_iter()
        .map(|(app, root)| (root.join("skills"), app))
        .collect();
    result.push((home.join(".agents/skills"), "agents".into()));
    for project in projects {
        for (suffix, label) in [
            (".claude/skills", "claude-project"),
            (".agents/skills", "agents-project"),
            (".codex/skills", "codex-project"),
        ] {
            result.push((project.join(suffix), label.into()));
        }
    }
    result
}

pub fn scan(db: &crate::database::Database) -> Result<LocalExtensionInventory, String> {
    let (home, roots) = roots()?;
    let managed = db
        .get_all_mcp_servers()
        .map_err(|_| "无法读取已保存的 MCP 服务，请重试。")?;
    Ok(scan_sources(
        &sources(&home, &roots).0,
        &managed.keys().cloned().collect(),
    ))
}

pub fn import_mcp(db: &crate::database::Database, entry_id: &str) -> Result<(), String> {
    let (home, roots) = roots()?;
    let sources = sources(&home, &roots).0;
    import_from_sources(db, entry_id, &sources)
}

fn import_from_sources(
    db: &crate::database::Database,
    entry_id: &str,
    sources: &[Source],
) -> Result<(), String> {
    // Rebuild the allowlisted source inventory at confirmation time. IPC never
    // accepts a raw source path, spec, environment value or auth header.
    let inventory = scan_sources(sources, &HashSet::new());
    let entry = inventory
        .mcp
        .iter()
        .find(|entry| entry.id == entry_id)
        .ok_or("来源已变化，请重新扫描后导入。")?;
    if db
        .get_all_mcp_servers()
        .map_err(|_| "无法读取服务列表，请重试。")?
        .contains_key(&entry.name)
    {
        return Err("已存在同名服务，请在管理列表中检查配置。".into());
    }
    for source in sources {
        let Ok(Some(config)) = read_config(&source.path, source.toml) else {
            continue;
        };
        for (scope, value) in config_groups(&source, &config) {
            let Some(servers) = value.get(source.field).and_then(Value::as_object) else {
                continue;
            };
            for (name, spec) in servers {
                if entry_id != record_id(&source, &scope, name) {
                    continue;
                }
                if !spec.is_object() {
                    return Err("来源已变化，请重新扫描后导入。".into());
                }
                let server = crate::app_config::McpServer {
                    id: name.clone(),
                    name: name.clone(),
                    server: if source.app == "opencode" {
                        crate::mcp::convert_from_opencode_format(spec)
                            .map_err(|_| "此服务配置无法导入，请检查后重试。")?
                    } else {
                        spec.clone()
                    },
                    apps: crate::app_config::McpApps::default(),
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: vec![LOCAL_IMPORT_TAG.into()],
                };
                // Explicit import only stores a disabled record. It does not
                // sync, launch a service or alter a native source file.
                return match db.insert_mcp_server(&server) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err("已存在同名服务，请在管理列表中检查配置。".into()),
                    Err(_) => Err("导入失败，请重试。".into()),
                };
            }
        }
    }
    Err("来源已变化，请重新扫描后导入。".into())
}

fn config_groups<'a>(source: &Source, config: &'a Value) -> Vec<(String, &'a Value)> {
    let mut groups = vec![(source.scope.clone(), config)];
    if source.app == "claude" {
        if let Some(projects) = config.get("projects").and_then(Value::as_object) {
            groups.extend(
                projects
                    .iter()
                    .take(MAX_PROJECTS)
                    .map(|(path, value)| (format!("project:{path}"), value)),
            );
        }
    }
    groups
}

fn record_id(source: &Source, scope: &str, name: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        source.app,
        source.path.display(),
        scope,
        name
    )
}

fn scan_sources(sources: &[Source], managed: &HashSet<String>) -> LocalExtensionInventory {
    let mut inventory = LocalExtensionInventory::default();
    for source in sources {
        let config = match read_config(&source.path, source.toml) {
            Ok(Some(config)) => {
                inventory.sources.push(LocalExtensionSource {
                    app: source.app.clone(),
                    path: source.path.display().to_string(),
                    status: "loaded",
                });
                config
            }
            Ok(None) => continue,
            Err(status) => {
                inventory.sources.push(LocalExtensionSource {
                    app: source.app.clone(),
                    path: source.path.display().to_string(),
                    status,
                });
                continue;
            }
        };
        for (scope, value) in config_groups(source, &config) {
            let Some(servers) = value.get(source.field).and_then(Value::as_object) else {
                continue;
            };
            for (name, spec) in servers
                .iter()
                .take(MAX_ENTRIES.saturating_sub(inventory.mcp.len()))
            {
                if !spec.is_object() {
                    continue;
                }
                let transport = if spec
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "http" | "sse" | "remote"))
                    || spec.get("url").is_some()
                {
                    "http"
                } else {
                    "stdio"
                };
                // Never return commands, arguments, URLs, env values or headers.
                inventory.mcp.push(LocalMcpEntry {
                    id: record_id(source, &scope, name),
                    name: name.clone(),
                    app: source.app.clone(),
                    source_path: source.path.display().to_string(),
                    scope: if scope.starts_with("project") {
                        "project".into()
                    } else {
                        scope.clone()
                    },
                    transport: transport.into(),
                    configured: spec.get("enabled").and_then(Value::as_bool) != Some(false)
                        && spec.get("disabled").and_then(Value::as_bool) != Some(true),
                    managed: managed.contains(name),
                });
            }
        }
    }
    inventory.mcp.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.source_path.cmp(&b.source_path))
    });
    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opencode_jsonc_inventory_import_uses_the_active_file_and_preserves_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".config/opencode");
        fs::create_dir_all(&root).unwrap();
        let original =
            "{ // keep fixture comment\n mcp: {active: {type:'local', command:['fixture'],},},}\n";
        fs::write(root.join("opencode.jsonc"), original).unwrap();
        fs::write(
            root.join("opencode.json"),
            r#"{"mcp":{"inactive":{"type":"local","command":["unused"]}}}"#,
        )
        .unwrap();
        let (sources, _) = sources(temp.path(), &vec![("opencode".into(), root.clone())]);
        let inventory = scan_sources(&sources, &HashSet::new());
        assert_eq!(inventory.mcp.len(), 1);
        assert_eq!(inventory.mcp[0].name, "active");
        let db = crate::database::Database::memory().unwrap();
        import_from_sources(&db, &inventory.mcp[0].id, &sources).unwrap();
        assert_eq!(
            db.get_all_mcp_servers().unwrap()["active"].server["command"],
            "fixture"
        );
        assert!(db.get_all_mcp_servers().unwrap()["active"]
            .apps
            .enabled_apps()
            .is_empty());
        assert_eq!(
            fs::read_to_string(root.join("opencode.jsonc")).unwrap(),
            original
        );
    }
    #[test]
    fn native_inventory_finds_global_project_and_desktop_servers_without_exposing_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(home.join("project/.claude/skills")).unwrap();
        fs::write(home.join(".codex/config.toml"), "[mcp_servers.private]\ncommand='secret-command'\nargs=['token=secret']\n[mcp_servers.private.env]\nAPI_KEY='do-not-return'\n").unwrap();
        fs::write(home.join(".claude.json"), serde_json::json!({"mcpServers":{"claude-local":{"url":"https://private.test?token=secret","headers":{"Authorization":"never-return"}}},"projects":{home.join("project").display().to_string():{"mcpServers":{"project-preferences":{"command":"secret"}}}}}).to_string()).unwrap();
        fs::write(
            home.join("project/.mcp.json"),
            r#"{"mcpServers":{"project-file":{"command":"secret","disabled":true}}}"#,
        )
        .unwrap();
        #[cfg(target_os = "macos")]
        {
            let desktop = home.join("Library/Application Support/Claude");
            fs::create_dir_all(&desktop).unwrap();
            fs::write(
                desktop.join("claude_desktop_config.json"),
                r#"{"mcpServers":{"desktop":{"command":"desktop-secret"}}}"#,
            )
            .unwrap();
        }
        let (sources, projects) = sources(home, &default_roots(home));
        assert_eq!(projects, vec![home.join("project")]);
        let report = scan_sources(&sources, &HashSet::from(["private".into()]));
        assert_eq!(
            report.mcp.len(),
            if cfg!(target_os = "macos") { 5 } else { 4 }
        );
        assert!(
            report
                .mcp
                .iter()
                .find(|entry| entry.name == "private")
                .unwrap()
                .managed
        );
        assert!(
            !report
                .mcp
                .iter()
                .find(|entry| entry.name == "project-file")
                .unwrap()
                .configured
        );
        let dto = serde_json::to_string(&report).unwrap();
        for secret in [
            "secret-command",
            "token=secret",
            "API_KEY",
            "do-not-return",
            "private.test",
            "Authorization",
            "never-return",
            "desktop-secret",
        ] {
            assert!(!dto.contains(secret));
        }
        assert!(!home.join(".osir_client").exists());
    }

    #[test]
    fn explicit_import_is_disabled_atomic_and_retains_native_secrets_on_edit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        let original = r#"{"mcpServers":{"fixture":{"command":"private-command","args":["private-arg"],"env":{"TOKEN":"private-token"},"headers":{"Authorization":"private-header"},"extra":{"keep":"private-value"}}}}"#;
        fs::write(&path, original).unwrap();
        let source = Source {
            app: "claude".into(),
            path: path.clone(),
            scope: "user".into(),
            toml: false,
            field: "mcpServers",
        };
        let id = scan_sources(&[source.clone()], &HashSet::new())
            .mcp
            .remove(0)
            .id;
        let db = crate::database::Database::memory().unwrap();
        assert!(import_from_sources(&db, "untrusted:/other/file", &[source.clone()]).is_err());
        import_from_sources(&db, &id, &[source.clone()]).unwrap();
        assert!(import_from_sources(&db, &id, &[source]).is_err());
        let saved = db
            .get_all_mcp_servers()
            .unwrap()
            .shift_remove("fixture")
            .unwrap();
        assert_eq!(saved.apps, crate::app_config::McpApps::default());
        let mut editor = saved.clone();
        redact_imported(&mut editor);
        assert!(!serde_json::to_string(&editor).unwrap().contains("private-"));
        editor.server["command"] = serde_json::json!("explicit-new-command");
        editor.tags.clear();
        retain_imported_values(&mut editor, &saved);
        assert_eq!(editor.server["command"], "explicit-new-command");
        assert_eq!(editor.server["args"], saved.server["args"]);
        assert_eq!(editor.server["env"], saved.server["env"]);
        assert_eq!(editor.server["headers"], saved.server["headers"]);
        assert_eq!(editor.server["extra"], saved.server["extra"]);
        assert!(editor.tags.iter().any(|tag| tag == LOCAL_IMPORT_TAG));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn opencode_import_reuses_native_format_conversion_without_exposing_values() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("opencode.json");
        fs::write(&path, r#"{"mcp":{"fixture":{"type":"local","command":["fixture-command","--option"],"environment":{"TOKEN":"fixture-token"},"enabled":true}}}"#).unwrap();
        let source = Source {
            app: "opencode".into(),
            path,
            scope: "user".into(),
            toml: false,
            field: "mcp",
        };
        let id = scan_sources(&[source.clone()], &HashSet::new())
            .mcp
            .remove(0)
            .id;
        let db = crate::database::Database::memory().unwrap();
        import_from_sources(&db, &id, &[source]).unwrap();
        let saved = db
            .get_all_mcp_servers()
            .unwrap()
            .shift_remove("fixture")
            .unwrap();
        assert_eq!(saved.server["type"], "stdio");
        assert_eq!(saved.server["command"], "fixture-command");
        assert_eq!(saved.server["args"], serde_json::json!(["--option"]));
        assert_eq!(saved.server["env"]["TOKEN"], "fixture-token");
        assert_eq!(saved.apps, crate::app_config::McpApps::default());
    }
    #[test]
    fn malformed_and_oversize_sources_report_fixed_status_without_parser_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        let source = Source {
            app: "claude".into(),
            path: path.clone(),
            scope: "user".into(),
            toml: false,
            field: "mcpServers",
        };
        fs::write(&path, "{secret-user-content").unwrap();
        let report = scan_sources(&[source.clone()], &HashSet::new());
        assert_eq!(report.sources[0].status, "invalid");
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("secret-user-content"));
        fs::write(&path, vec![b' '; (MAX_CONFIG + 1) as usize]).unwrap();
        assert_eq!(
            scan_sources(&[source], &HashSet::new()).sources[0].status,
            "invalid"
        );
    }
}
