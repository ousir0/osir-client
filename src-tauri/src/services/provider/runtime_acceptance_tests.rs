//! Production writers exercised with synthetic credentials in an isolated home.
//! Optional exports are consumed by scripts/osir-tool-runtime-check.py; these
//! writer assertions alone do not constitute installed-tool acceptance.
use super::service::{BackupError, RestoreStatus, Tool, ToolBackups};
use crate::{
    app_config::AppType,
    database::Database,
    mode::current::{provider_for, Purpose},
    provider::Provider,
    services::provider::ProviderService,
    settings::{get_settings, update_settings, AppSettings},
    store::AppState,
};
use serde_json::{json, Value};
use serial_test::serial;
use std::{ffi::OsString, fs, path::PathBuf, sync::Arc};

struct Home {
    previous_env: Vec<(&'static str, Option<OsString>)>,
    previous_settings: AppSettings,
    dir: tempfile::TempDir,
}
impl Home {
    fn new() -> Self {
        // Strict backup paths reject symlink ancestors (/var on macOS).
        let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let previous_settings = get_settings();
        let previous_env: Vec<(&'static str, Option<OsString>)> = [
            "HOME",
            "USERPROFILE",
            "CC_SWITCH_TEST_HOME",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "OPENCODE_DB",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        for (key, _) in &previous_env {
            std::env::set_var(key, dir.path());
        }
        std::env::set_var("OPENCODE_DB", dir.path().join("opencode.db"));
        let custom = |name: &str| {
            let path = dir.path().join(name);
            fs::create_dir_all(&path).unwrap();
            Some(path.to_string_lossy().into_owned())
        };
        update_settings(AppSettings {
            claude_config_dir: custom(".claude"),
            codex_config_dir: custom(".codex"),
            gemini_config_dir: custom(".gemini"),
            opencode_config_dir: custom(".config/opencode"),
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .unwrap();
        assert!(crate::live::engine::DeviceStore::for_device()
            .state_path()
            .starts_with(dir.path()));
        Self {
            previous_env,
            previous_settings,
            dir,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn seed(&self, tool: Tool) {
        match tool {
            Tool::Codex => {
                fs::write(self.path(".codex/config.toml"), "# user-owned comment\nmodel = \"old\"\ncli_auth_credentials_store = \"file\"\n[mcp_servers.fixture]\ncommand = \"/usr/bin/false\"\nenabled = false\n[projects.\"/fixture/project\"]\ntrust_level = \"untrusted\"\n").unwrap();
                fs::write(
                    self.path(".codex/auth.json"),
                    br#"{"OPENAI_API_KEY":"synthetic-original-native-key"}"#,
                )
                .unwrap();
            }
            Tool::Claude => {
                fs::write(
                    self.path(".claude/settings.json"),
                    serde_json::to_vec_pretty(&json!({
                        "env": {"USER_FLAG": "keep"},
                        "permissions": {"defaultMode": "plan"},
                        "mcpServers": {"fixture": {"command": "/usr/bin/false", "disabled": true}}
                    }))
                    .unwrap(),
                )
                .unwrap();
                fs::write(self.path(".claude/.credentials.json"), br#"{"claudeAiOauth":{"accessToken":"synthetic-native-auth","refreshToken":"synthetic-native-refresh"}}"#).unwrap();
                fs::write(self.path(".claude.json"), br#"{"mcpServers":{"fixture":{"command":"/usr/bin/false","disabled":true}},"hasCompletedOnboarding":true,"customUserPreference":"keep"}"#).unwrap();
            }
            Tool::Gemini => {
                fs::write(
                    self.path(".gemini/.env"),
                    "# user-owned comment\nUSER_FLAG=keep\n",
                )
                .unwrap();
                fs::write(self.path(".gemini/settings.json"), serde_json::to_vec_pretty(&json!({
                    "mcpServers": {"fixture": {"command": "/usr/bin/false", "excludeTools": ["*"]}},
                    "security": {"auth": {"selectedType": "oauth-personal", "userChoice": true}},
                    "model": {"maxSessionTurns": 3},
                    "ui": {"theme": "Default Light"},
                    "telemetry": {"enabled": false}
                })).unwrap()).unwrap();
                fs::write(self.path(".gemini/oauth_creds.json"), br#"{"access_token":"synthetic-native-auth","refresh_token":"synthetic-native-refresh"}"#).unwrap();
            }
            Tool::Opencode => {
                fs::write(self.path(".config/opencode/opencode.json"), serde_json::to_vec_pretty(&json!({
                    "theme": "system",
                    "autoupdate": false,
                    "mcp": {"fixture": {"type": "local", "command": ["/usr/bin/false"], "enabled": false}},
                    "provider": {"user-owned": {"npm": "@ai-sdk/openai-compatible", "models": {"keep": {"name": "Keep"}}, "options": {"apiKey": "synthetic-user-owned-key", "baseURL": "http://127.0.0.1:9/v1"}}}
                })).unwrap()).unwrap();
                fs::create_dir_all(self.path(".local/share/opencode")).unwrap();
                fs::write(
                    self.path(".local/share/opencode/auth.json"),
                    br#"{"user-owned":{"type":"api","key":"synthetic-native-auth"}}"#,
                )
                .unwrap();
            }
        }
        for relative in paths(tool) {
            assert!(self.path(relative).starts_with(self.dir.path()));
        }
    }

    fn export(&self, tool: Tool, phase: &str, assertions: Value) {
        let Some(root) = std::env::var_os("OSIR_RUNTIME_ACCEPTANCE_EXPORT") else {
            return;
        };
        let root = PathBuf::from(root);
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(root.is_absolute() && root.starts_with(repo.join("output/acceptance")));
        let destination = root.join(tool.id()).join(phase);
        fs::create_dir_all(&destination).unwrap();
        let mut files = Vec::new();
        for relative in paths(tool).iter().chain(auth_paths(tool).iter()) {
            let source = self.path(relative);
            if source.exists() {
                let target = destination.join(relative);
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::copy(source, target).unwrap();
                files.push(*relative);
            }
        }
        fs::write(destination.join("writer-evidence.json"), serde_json::to_vec_pretty(&json!({
            "tool": tool.id(), "phase": phase, "source": "production ProviderService/native writer",
            "isolatedWriterHome": self.dir.path(), "syntheticCredentials": true,
            "files": files, "assertions": assertions
        })).unwrap()).unwrap();
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        crate::mode::operation::failpoint::crash_at(None);
        let _ = update_settings(self.previous_settings.clone());
        for (key, value) in &self.previous_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
fn paths(tool: Tool) -> Vec<&'static str> {
    match tool {
        Tool::Codex => vec![".codex/config.toml", ".codex/cc-switch-model-catalog.json"],
        Tool::Claude => vec![".claude/settings.json"],
        Tool::Gemini => vec![".gemini/.env", ".gemini/settings.json"],
        Tool::Opencode => vec![".config/opencode/opencode.json"],
    }
}
fn auth_paths(tool: Tool) -> Vec<&'static str> {
    match tool {
        Tool::Codex => vec![".codex/auth.json"],
        Tool::Claude => vec![".claude/.credentials.json", ".claude.json"],
        Tool::Gemini => vec![".gemini/oauth_creds.json"],
        Tool::Opencode => vec![".local/share/opencode/auth.json"],
    }
}
fn app(tool: Tool) -> AppType {
    match tool {
        Tool::Codex => AppType::Codex,
        Tool::Claude => AppType::Claude,
        Tool::Gemini => AppType::Gemini,
        Tool::Opencode => AppType::OpenCode,
    }
}
fn provider(tool: Tool, label: &str) -> Provider {
    let endpoint = format!("http://127.0.0.1:12391/{}/{label}", tool.id());
    let model = format!("osir-fixture-{label}");
    let config = match tool {
        Tool::Codex => json!({
            "auth": {"OPENAI_API_KEY": format!("synthetic-provider-{label}")},
            "config": format!("model = \"{model}\"\nmodel_provider = \"fixture\"\n[model_providers.fixture]\nname = \"Fixture {label}\"\nbase_url = \"{endpoint}/v1\"\nwire_api = \"responses\"\n"),
            "modelCatalog": {"models": [{"model": model, "input_modalities": ["text"]}, {"model": format!("osir-alternate-{label}"), "input_modalities": ["text"]}]}
        }),
        Tool::Claude => {
            json!({"model": model, "env": {"ANTHROPIC_AUTH_TOKEN": format!("synthetic-provider-{label}"), "ANTHROPIC_BASE_URL": endpoint, "ANTHROPIC_MODEL": model, "ANTHROPIC_SMALL_FAST_MODEL": model}})
        }
        Tool::Gemini => {
            json!({"env": {"GEMINI_API_KEY": format!("synthetic-provider-{label}"), "GOOGLE_GEMINI_BASE_URL": endpoint, "GEMINI_MODEL": model}, "config": {"model": {"name": model}}})
        }
        Tool::Opencode => {
            json!({"npm": "@ai-sdk/openai-compatible", "name": format!("Fixture {label}"), "options": {"baseURL": format!("{endpoint}/v1"), "apiKey": format!("synthetic-provider-{label}")}, "models": {(model): {"name": format!("Fixture {label}"), "limit": {"context": 32768, "output": 2048}}}})
        }
    };
    Provider::with_id(
        format!("osir-fixture-{label}"),
        format!("Fixture {label}"),
        config,
        None,
    )
}
fn file_bytes(home: &Home, tool: Tool) -> Vec<Option<Vec<u8>>> {
    paths(tool)
        .iter()
        .map(|p| fs::read(home.path(p)).ok())
        .collect()
}
fn assert_user_fields(home: &Home, tool: Tool) {
    match tool {
        Tool::Codex => {
            let text = fs::read_to_string(home.path(paths(tool)[0])).unwrap();
            assert!(text.contains("# user-owned comment"));
            let doc: toml::Value = text.parse().unwrap();
            assert_eq!(
                doc["mcp_servers"]["fixture"]["enabled"].as_bool(),
                Some(false)
            );
            assert_eq!(
                doc["projects"]["/fixture/project"]["trust_level"].as_str(),
                Some("untrusted")
            );
        }
        Tool::Claude => {
            let doc: Value =
                serde_json::from_slice(&fs::read(home.path(paths(tool)[0])).unwrap()).unwrap();
            assert_eq!(doc["env"]["USER_FLAG"], "keep");
            assert_eq!(doc["permissions"]["defaultMode"], "plan");
            assert_eq!(doc["mcpServers"]["fixture"]["disabled"], true);
        }
        Tool::Gemini => {
            let env = fs::read_to_string(home.path(paths(tool)[0])).unwrap();
            assert!(env.contains("# user-owned comment") && env.contains("USER_FLAG=keep"));
            let doc: Value =
                serde_json::from_slice(&fs::read(home.path(paths(tool)[1])).unwrap()).unwrap();
            assert_eq!(doc["security"]["auth"]["userChoice"], true);
            assert_eq!(doc["mcpServers"]["fixture"]["command"], "/usr/bin/false");
            assert_eq!(doc["ui"]["theme"], "Default Light");
        }
        Tool::Opencode => {
            let doc: Value =
                serde_json::from_slice(&fs::read(home.path(paths(tool)[0])).unwrap()).unwrap();
            assert_eq!(doc["autoupdate"], false);
            assert_eq!(doc["mcp"]["fixture"]["enabled"], false);
            assert_eq!(
                doc["provider"]["user-owned"]["options"]["apiKey"],
                "synthetic-user-owned-key"
            );
        }
    }
}
fn exercise(tool: Tool) {
    let home = Home::new();
    home.seed(tool);
    let state = AppState::new(Arc::new(Database::memory().unwrap()));
    let providers = [provider(tool, "a"), provider(tool, "b")];
    for p in &providers {
        state.db.save_provider(tool.id(), p).unwrap();
    }
    let auth_before: Vec<_> = auth_paths(tool)
        .iter()
        .map(|p| fs::read(home.path(p)).unwrap())
        .collect();
    let activate = |p: &Provider| {
        ProviderService::switch(&state, app(tool), &p.id).unwrap();
        // OpenCode provider enablement is additive; default selection is its
        // existing separate native operation, not a ProviderService pointer.
        if tool == Tool::Opencode {
            crate::opencode_config::set_default_model(&format!("{}/{}", p.id, p.id)).unwrap();
        }
        assert_user_fields(&home, tool);
        for (path, expected) in auth_paths(tool).iter().zip(&auth_before) {
            assert_eq!(&fs::read(home.path(path)).unwrap(), expected);
        }
    };
    activate(&providers[0]);
    let a = file_bytes(&home, tool);
    home.export(tool, "a", json!({"userFieldsMcpAuthPreserved": true}));
    let backup = ToolBackups::configured(tool);
    let snapshot = backup.create().unwrap();
    activate(&providers[1]);
    assert_ne!(file_bytes(&home, tool), a);
    home.export(tool, "b", json!({"userFieldsMcpAuthPreserved": true, "additiveProviderEnablement": tool == Tool::Opencode}));
    let preview = backup.preview(&snapshot.id).unwrap();
    assert_eq!(
        backup.restore(&preview.preview_id, true).unwrap().status,
        RestoreStatus::Restored
    );
    assert_eq!(file_bytes(&home, tool), a);
    assert_user_fields(&home, tool);
    let expected_pointer = if tool == Tool::Opencode {
        None
    } else {
        Some(providers[1].id.clone())
    };
    assert_eq!(
        provider_for(&state.db, &app(tool), Purpose::Direct).unwrap(),
        expected_pointer
    );
    home.export(tool, "restored-a", json!({
        "byteExactSnapshotRestore": true, "providerPointerIncludedInSnapshot": false, "pointerAfterFileRestore": expected_pointer
    }));
    activate(&providers[1]);
    let conflict = backup.preview(&snapshot.id).unwrap();
    let first = home.path(paths(tool)[0]);
    let before = fs::read(&first).unwrap();
    let mut edited = before.clone();
    edited.push(b'\n');
    fs::write(&first, &edited).unwrap();
    assert_eq!(
        backup.restore(&conflict.preview_id, true).err(),
        Some(BackupError::Changed)
    );
    assert_eq!(fs::read(&first).unwrap(), edited);
    fs::write(first, before).unwrap();
    activate(&providers[0]);
    home.export(tool, "reswitched-a", json!({
        "stalePreviewRejectedWithoutOverwrite": true, "explicitProviderSwitchAfterFileRestore": true, "userFieldsMcpAuthPreserved": true
    }));
}

#[test]
#[serial]
fn runtime_acceptance_codex_writer_switch_restore_conflict() {
    exercise(Tool::Codex);
}
#[test]
#[serial]
fn runtime_acceptance_claude_writer_switch_restore_conflict() {
    exercise(Tool::Claude);
}
#[test]
#[serial]
fn runtime_acceptance_gemini_writer_switch_restore_conflict() {
    exercise(Tool::Gemini);
}
#[test]
#[serial]
fn runtime_acceptance_opencode_writer_switch_restore_conflict() {
    exercise(Tool::Opencode);
}
