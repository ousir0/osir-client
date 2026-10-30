use super::{import_opencode_providers_from_live, ProviderService};
use crate::app_config::AppType;
use crate::database::Database;
use crate::opencode_config;
use crate::provider::Provider;
use crate::settings::{get_settings, update_settings, AppSettings};
use crate::store::AppState;
use serde_json::json;
use serial_test::serial;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

struct Fixture {
    previous_settings: AppSettings,
    previous_env: Vec<(&'static str, Option<OsString>)>,
    config_path: PathBuf,
    state: AppState,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let previous_env = ["CC_SWITCH_TEST_HOME", "OPENCODE_DB"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        std::env::set_var("CC_SWITCH_TEST_HOME", dir.path());
        std::env::set_var("OPENCODE_DB", dir.path().join("opencode.db"));
        let previous_settings = get_settings();
        let config_dir = dir.path().join("opencode");
        fs::create_dir_all(&config_dir).unwrap();
        update_settings(AppSettings {
            opencode_config_dir: Some(config_dir.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
        Self {
            previous_settings,
            previous_env,
            config_path: config_dir.join("opencode.json"),
            state: AppState::new(Arc::new(Database::memory().unwrap())),
            _dir: dir,
        }
    }

    fn imported(&self) -> Provider {
        self.state
            .db
            .get_provider_by_id("opencode-go", "opencode")
            .unwrap()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Restore the cached settings while writes still target the temporary home.
        update_settings(self.previous_settings.clone()).unwrap();
        for (key, value) in &self.previous_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

const KEY_ONLY_CONFIG: &str = r#"{
  // Keep this comment and the user's formatting during import.
  "provider": {"opencode-go": {"options": {"apiKey": "{env:OPENCODE_API_KEY}"}}}
}"#;

#[test]
#[serial]
fn osir_key_replacement_keeps_saved_identity_and_rejects_applied_opencode() {
    use crate::services::osir::{
        connection::{
            provider_row, ConnectionRequest, ReplaceConnectionKeyRequest, TrustedKeyReplacement,
        },
        key_vault::KeyMetadata,
    };
    let fixture = Fixture::new();
    let grant = managed_v2_grant();
    let mut saved = provider_row(
        &ConnectionRequest {
            tool: "opencode".into(),
            key_id: 1,
            name: "Original connection".into(),
            hide_aggregation_name: None,
            model: "old".into(),
            models: vec!["old".into()],
            generation: 1,
        },
        grant.provider.id.clone(),
    )
    .unwrap();
    saved.meta = grant.provider.meta.clone();
    saved.meta.as_mut().unwrap().live_config_managed = Some(false);
    saved.settings_config["options"]["timeout"] = json!(1234);
    saved.notes = Some("Preserve user notes".into());
    fixture.state.db.save_provider("opencode", &saved).unwrap();
    let old = saved.meta.as_ref().unwrap().osir_binding.as_ref().unwrap();
    ProviderService::check_osir_key_replacement(
        &fixture.state,
        AppType::OpenCode,
        &saved.id,
        &old.reference,
    )
    .unwrap();
    let mut binding = old.binding.clone();
    binding.key_id = 2;
    binding.group_id = Some(9);
    let reference =
        serde_json::from_value(json!({"version":1,"id":uuid::Uuid::new_v4().to_string()})).unwrap();
    let metadata = KeyMetadata {
        binding,
        reference,
        masked_key: "••••new2".into(),
    };
    let request = ReplaceConnectionKeyRequest {
        tool: "opencode".into(),
        provider_id: saved.id.clone(),
        key_id: 2,
        name: saved.name.clone(),
        model: "new".into(),
        models: vec!["new".into()],
        generation: 1,
        expected_reference: old.reference.clone(),
    };
    let updated = ProviderService::replace_osir_key(
        &fixture.state,
        AppType::OpenCode,
        &saved.id,
        &old.reference,
        |_| {
            TrustedKeyReplacement::new(request, &metadata)
                .map_err(crate::error::AppError::InvalidInput)
        },
        |result| result,
    )
    .unwrap();
    assert_eq!(updated.id, saved.id);
    assert_eq!(updated.name, saved.name);
    assert_eq!(updated.notes, saved.notes);
    assert_eq!(updated.settings_config["options"]["timeout"], 1234);
    assert_eq!(
        updated.settings_config["models"],
        json!({"new":{"name":"new","attachment":true,"modalities":{"input":["text","image"],"output":["text"]}}})
    );
    assert!(!fixture.config_path.exists());
    let latest = updated
        .meta
        .as_ref()
        .unwrap()
        .osir_binding
        .as_ref()
        .unwrap();
    assert_eq!(latest.reference, metadata.reference);
    assert_eq!(latest.binding.group_id, Some(9));
    assert!(ProviderService::check_osir_key_replacement(
        &fixture.state,
        AppType::OpenCode,
        &saved.id,
        &old.reference
    )
    .is_err());
    fs::write(
        &fixture.config_path,
        serde_json::to_vec(&json!({"provider":{updated.id.clone():updated.settings_config}}))
            .unwrap(),
    )
    .unwrap();
    let before = fs::read(&fixture.config_path).unwrap();
    let prepared = std::cell::Cell::new(false);
    let result = ProviderService::replace_osir_key(
        &fixture.state,
        AppType::OpenCode,
        &saved.id,
        &latest.reference,
        |_| {
            prepared.set(true);
            unreachable!("must reject before storing a grant")
        },
        |result| result,
    );
    assert!(result.is_err());
    assert!(!prepared.get());
    assert_eq!(fs::read(&fixture.config_path).unwrap(), before);
}

#[test]
#[serial]
fn opencode_builtin_import_is_read_only_and_existing_override_is_editable() {
    let fixture = Fixture::new();
    fs::write(&fixture.config_path, KEY_ONLY_CONFIG).unwrap();
    let original_permissions = fs::metadata(&fixture.config_path).unwrap().permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&fixture.config_path, read_only).unwrap();

    let imported = import_opencode_providers_from_live(&fixture.state);
    fs::set_permissions(&fixture.config_path, original_permissions).unwrap();
    assert_eq!(imported.unwrap(), 1);
    assert_eq!(
        import_opencode_providers_from_live(&fixture.state).unwrap(),
        0
    );
    assert_eq!(
        fs::read_to_string(&fixture.config_path).unwrap(),
        KEY_ONLY_CONFIG
    );

    let mut provider = fixture.imported();
    assert!(provider.settings_config.get("npm").is_none());
    assert!(provider.settings_config["options"].get("baseURL").is_none());
    provider.settings_config["options"]["apiKey"] = json!("edited-test-key");
    ProviderService::update(&fixture.state, AppType::OpenCode, None, provider).unwrap();
    let saved = opencode_config::get_providers().unwrap();
    assert_eq!(saved["opencode-go"]["options"]["apiKey"], "edited-test-key");
    assert!(saved["opencode-go"].get("npm").is_none());
    assert!(saved["opencode-go"]["options"].get("baseURL").is_none());
}

#[test]
#[serial]
fn opencode_builtin_partial_model_override_can_omit_display_name() {
    let fixture = Fixture::new();
    opencode_config::set_provider(
        "opencode-go",
        json!({"models": {"glm-5": {"limit": {"context": 100000, "output": 10000}}}}),
    )
    .unwrap();
    assert_eq!(
        import_opencode_providers_from_live(&fixture.state).unwrap(),
        1
    );
    let mut provider = fixture.imported();
    assert!(provider.settings_config["models"]["glm-5"]
        .get("name")
        .is_none());
    provider.settings_config["options"]["apiKey"] = json!("edited-test-key");
    ProviderService::update(&fixture.state, AppType::OpenCode, None, provider).unwrap();
    let saved = opencode_config::get_providers().unwrap();
    assert!(saved["opencode-go"]["models"]["glm-5"]
        .get("name")
        .is_none());
    assert_eq!(
        saved["opencode-go"]["models"]["glm-5"]["limit"]["context"],
        100000
    );
}

#[test]
#[serial]
fn opencode_builtin_incomplete_copies_cannot_be_added_to_live() {
    let fixture = Fixture::new();
    fs::write(&fixture.config_path, KEY_ONLY_CONFIG).unwrap();
    for (index, config) in [
        json!({"options": {"apiKey": "test-key"}}),
        json!({"npm": "@ai-sdk/openai-compatible", "models": {}}),
        json!({"models": {"glm-5": {"name": "GLM 5"}}}),
        json!({"npm": "  ", "models": {"glm-5": {"name": "GLM 5"}}}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("opencode-go-copy-{index}");
        let provider = Provider::with_id(id.clone(), id.clone(), config, None);
        ProviderService::add(&fixture.state, AppType::OpenCode, provider, false).unwrap();
        let error = ProviderService::switch(&fixture.state, AppType::OpenCode, &id).unwrap_err();
        assert!(error.to_string().contains("npm"));
        assert_eq!(
            fs::read_to_string(&fixture.config_path).unwrap(),
            KEY_ONLY_CONFIG
        );
        let saved = fixture
            .state
            .db
            .get_provider_by_id(&id, "opencode")
            .unwrap()
            .unwrap();
        assert_eq!(saved.meta.unwrap().live_config_managed, Some(false));
    }

    let complete = Provider::with_id(
        "custom-copy".into(),
        "Complete copy".into(),
        json!({"npm": "@ai-sdk/openai-compatible", "models": {"glm-5": {"name": "GLM 5"}}}),
        None,
    );
    ProviderService::add(&fixture.state, AppType::OpenCode, complete, false).unwrap();
    ProviderService::switch(&fixture.state, AppType::OpenCode, "custom-copy").unwrap();
    assert!(opencode_config::get_providers()
        .unwrap()
        .contains_key("custom-copy"));
}

#[test]
#[serial]
fn opencode_builtin_removed_override_cannot_reuse_live_membership() {
    let fixture = Fixture::new();
    fs::write(&fixture.config_path, KEY_ONLY_CONFIG).unwrap();
    import_opencode_providers_from_live(&fixture.state).unwrap();
    opencode_config::remove_provider("opencode-go").unwrap();
    let previous = fs::read(&fixture.config_path).unwrap();
    let error =
        ProviderService::switch(&fixture.state, AppType::OpenCode, "opencode-go").unwrap_err();
    assert!(error.to_string().contains("npm"));
    assert_eq!(fs::read(&fixture.config_path).unwrap(), previous);
}

#[test]
#[serial]
fn opencode_builtin_credential_only_import_does_not_create_config() {
    if crate::config::sqlite_unsupported_in_temp_dir() {
        return;
    }
    let fixture = Fixture::new();
    let db_path = opencode_config::get_opencode_db_path();
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE credential (id TEXT, integration_id TEXT, label TEXT, value TEXT,
            connector_id TEXT, active INTEGER, time_created INTEGER);
         INSERT INTO credential VALUES ('go', 'opencode-go', 'default',
            '{\"type\":\"key\",\"key\":\"credential-test-key\"}', NULL, 1, 1);",
    )
    .unwrap();
    drop(db);
    let original_db = fs::read(&db_path).unwrap();
    assert_eq!(
        import_opencode_providers_from_live(&fixture.state).unwrap(),
        0
    );
    assert!(!fixture.config_path.exists());
    assert!(fixture
        .state
        .db
        .get_all_providers("opencode")
        .unwrap()
        .is_empty());
    assert_eq!(fs::read(&db_path).unwrap(), original_db);
}

#[cfg(unix)]
#[test]
#[serial]
fn opencode_builtin_import_preserves_symlink() {
    let fixture = Fixture::new();
    let target = fixture.config_path.with_file_name("dotfiles.json");
    fs::write(&target, KEY_ONLY_CONFIG).unwrap();
    std::os::unix::fs::symlink(&target, &fixture.config_path).unwrap();
    assert_eq!(
        import_opencode_providers_from_live(&fixture.state).unwrap(),
        1
    );
    assert!(fs::symlink_metadata(&fixture.config_path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(target).unwrap(), KEY_ONLY_CONFIG);
}

#[test]
#[serial]
fn opencode_jsonc_provider_and_mcp_workflows_preserve_comments() {
    let fixture = Fixture::new();
    let path = fixture.config_path.with_extension("jsonc");
    let other = r#"{"provider":{"ignored":{"options":{"apiKey":"json-only"}}},"mcp":{"ignored":{"type":"local","command":["ignored"]}}}"#;
    fs::write(&fixture.config_path, other).unwrap();
    let source = r#"{
  /* 用户配置 */
  "model": "keep-this", // 默认模型
  "provider": {"opencode-go": {"options": {"apiKey": "old"}}},
  "mcp": {"existing": {"type": "local", "command": ["npx", "old"], "enabled": true}},
}"#;
    fs::write(&path, source).unwrap();
    assert_eq!(
        import_opencode_providers_from_live(&fixture.state).unwrap(),
        1
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), source);
    let mut provider = fixture.imported();
    provider.settings_config["options"]["apiKey"] = json!("updated");
    ProviderService::update(&fixture.state, AppType::OpenCode, None, provider).unwrap();
    let added = Provider::with_id(
        "custom".into(),
        "Custom".into(),
        json!({
            "npm":"@ai-sdk/openai-compatible", "models":{"test":{"name":"Test"}}
        }),
        None,
    );
    ProviderService::add(&fixture.state, AppType::OpenCode, added, false).unwrap();
    ProviderService::add_to_live_config(&fixture.state, AppType::OpenCode, "custom").unwrap();
    opencode_config::set_default_model("custom/test").unwrap();
    assert_eq!(
        opencode_config::get_default_model().unwrap().as_deref(),
        Some("custom/test")
    );
    assert!(opencode_config::get_providers()
        .unwrap()
        .contains_key("custom"));
    assert_eq!(
        opencode_config::get_providers().unwrap()["opencode-go"]["options"]["apiKey"],
        "updated"
    );
    ProviderService::remove_from_live_config(&fixture.state, AppType::OpenCode, "custom").unwrap();
    ProviderService::delete(&fixture.state, AppType::OpenCode, "opencode-go").unwrap();
    assert!(opencode_config::get_providers().unwrap().is_empty());

    let mut config = crate::app_config::MultiAppConfig::default();
    let before_import = fs::read(&path).unwrap();
    assert_eq!(crate::mcp::import_from_opencode(&mut config).unwrap(), 1);
    assert_eq!(fs::read(&path).unwrap(), before_import);
    assert_eq!(
        config.mcp.servers.as_ref().unwrap()["existing"].server["command"],
        "npx"
    );
    for (id, spec) in [
        ("existing", json!({"command":"npx","args":["updated"]})),
        (
            "added",
            json!({"type":"http","url":"https://example.com/mcp"}),
        ),
    ] {
        crate::mcp::sync_single_server_to_opencode(&config, id, &spec).unwrap();
    }
    let servers = opencode_config::get_mcp_servers().unwrap();
    assert_eq!(servers["existing"]["command"], json!(["npx", "updated"]));
    assert_eq!(servers["added"]["type"], "remote");
    for id in ["existing", "added"] {
        crate::mcp::remove_server_from_opencode(id).unwrap();
    }
    assert!(opencode_config::get_mcp_servers().unwrap().is_empty());
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains("/* 用户配置 */"));
    assert!(saved.contains("\"model\": \"custom/test\", // 默认模型"));
    assert_eq!(fs::read_to_string(&fixture.config_path).unwrap(), other);
}

#[test]
#[serial]
fn opencode_jsonc_omo_registration_removal_and_failure_rollback() {
    use crate::services::omo::{OmoService, STANDARD};
    let fixture = Fixture::new();
    let path = fixture.config_path.with_extension("jsonc");
    let other = "{\"plugin\":[\"ignored\"]}";
    fs::write(&fixture.config_path, other).unwrap();
    let source = "{/* OpenCode 注释 */\"model\":\"keep\",\"plugin\":[\"unrelated\"]}";
    fs::write(&path, source).unwrap();
    let unified = fixture._dir.path().join(".omo/omo.jsonc");
    fs::create_dir_all(unified.parent().unwrap()).unwrap();
    let original_omo = "{/* OMO 注释 */\"[codex]\":{\"agents\":{}},\"_migrations\":[\"keep\"]}";
    fs::write(&unified, original_omo).unwrap();
    OmoService::write_config_to_file(&fixture.state, &STANDARD).unwrap();
    assert_eq!(
        opencode_config::read_opencode_config().unwrap()["plugin"],
        json!(["unrelated", "oh-my-openagent@latest"])
    );
    assert!(fs::read_to_string(&path)
        .unwrap()
        .contains("/* OpenCode 注释 */"));
    assert!(fs::read_to_string(&unified)
        .unwrap()
        .contains("/* OMO 注释 */"));
    OmoService::delete_config_file(&STANDARD).unwrap();
    assert_eq!(
        opencode_config::read_opencode_config().unwrap()["plugin"],
        json!(["unrelated"])
    );
    let omo: serde_json::Value = json5::from_str(&fs::read_to_string(&unified).unwrap()).unwrap();
    assert!(omo.get("[opencode]").is_none());
    assert_eq!(omo["_migrations"], json!(["keep"]));

    // Plugin sync failure must restore the exact OMO bytes for both enable and disable.
    for original in [
        original_omo,
        "{/*keep*/\"[opencode]\":{\"agents\":{\"old\":{}}},\"[codex]\":{}}",
    ] {
        fs::write(&unified, original).unwrap();
        fs::write(&path, "{broken").unwrap();
        assert!(OmoService::write_config_to_file(&fixture.state, &STANDARD).is_err());
        assert_eq!(fs::read_to_string(&unified).unwrap(), original);
        assert!(OmoService::delete_config_file(&STANDARD).is_err());
        assert_eq!(fs::read_to_string(&unified).unwrap(), original);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
    }
    assert_eq!(fs::read_to_string(&fixture.config_path).unwrap(), other);
}

const MANAGED_V2_SECRET: &str = "fixture-opencode-v2-materialized-secret";

fn managed_v2_grant() -> crate::services::osir::managed::test_support::Fixture {
    crate::services::osir::managed::test_support::Fixture::new(
        &AppType::OpenCode,
        json!({
            "name": "Managed V2 fixture",
            "package": "@ai-sdk/openai-compatible",
            "settings": {"baseURL": "https://api.osirclaw.com/v1", "timeout": 12000},
            "models": {"fixture-model": {"name": "Fixture model"}}
        }),
        MANAGED_V2_SECRET,
    )
}

fn managed_v2_document() -> serde_json::Value {
    json!({
        "model": "ordinary/fixture-model",
        "small_model": "ordinary/small-fixture",
        "theme": "user-theme",
        "provider": {"ordinary": {"options": {"apiKey": "ordinary-synthetic-key"}}},
        "providers": {"native-neighbor": {"settings": {"timeout": 9000}}},
        "mcp": {"local": {"type": "local", "command": ["fixture-mcp"], "enabled": true}}
    })
}

fn assert_managed_row_unresolved(fixture: &Fixture, provider: &Provider) {
    let saved = fixture
        .state
        .db
        .get_provider_by_id(&provider.id, "opencode")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&saved).unwrap(),
        serde_json::to_value(provider).unwrap()
    );
    let rows =
        serde_json::to_string(&fixture.state.db.get_all_providers("opencode").unwrap()).unwrap();
    assert!(!rows.contains(MANAGED_V2_SECRET));
    assert!(!rows.contains("ordinary-synthetic-key"));
}

#[test]
#[serial]
fn managed_opencode_v2_writer_materializes_settings_and_preserves_neighbors() {
    let fixture = Fixture::new();
    let grant = managed_v2_grant();
    fixture
        .state
        .db
        .save_provider("opencode", &grant.provider)
        .unwrap();
    let original = managed_v2_document();
    fs::write(
        &fixture.config_path,
        serde_json::to_vec_pretty(&original).unwrap(),
    )
    .unwrap();
    opencode_config::write_provider(&grant.provider).unwrap();
    let live: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.config_path).unwrap()).unwrap();
    let mut expected = original;
    let mut materialized = grant.provider.settings_config.clone();
    materialized["settings"]["apiKey"] = json!(MANAGED_V2_SECRET);
    materialized["models"]["fixture-model"]["capabilities"] =
        json!({"tools":true,"input":["text","image"],"output":["text"]});
    expected["providers"][&grant.provider.id] = materialized;
    assert_eq!(live, expected, "only the target V2 declaration is added");
    assert!(live["providers"][&grant.provider.id]
        .get("options")
        .is_none());
    assert!(live["provider"].get(&grant.provider.id).is_none());
    assert_managed_row_unresolved(&fixture, &grant.provider);
    let before = fs::read(&fixture.config_path).unwrap();
    grant.clear_credentials();
    assert!(opencode_config::write_provider(&grant.provider).is_err());
    assert_eq!(fs::read(&fixture.config_path).unwrap(), before);
    assert_managed_row_unresolved(&fixture, &grant.provider);
}

#[test]
#[serial]
fn managed_opencode_v1_writer_rejects_same_id_v2_conflict_without_changing_bytes() {
    let fixture = Fixture::new();
    let grant = managed_v2_grant();
    let mut legacy = grant.provider.clone();
    legacy.settings_config = json!({
        "npm": "@ai-sdk/openai-compatible",
        "options": {"baseURL": "https://api.osirclaw.com/v1"},
        "models": {"fixture-model": {"name": "Fixture model"}}
    });
    fixture.state.db.save_provider("opencode", &legacy).unwrap();
    let mut original = managed_v2_document();
    original["provider"][&legacy.id] = json!({"options": {"apiKey": "old-synthetic-key"}});
    original["providers"][&legacy.id] = json!({"settings": {"timeout": 1234}});
    let before = serde_json::to_vec_pretty(&original).unwrap();
    fs::write(&fixture.config_path, &before).unwrap();
    let error = opencode_config::write_provider(&legacy).unwrap_err();
    assert!(error.to_string().contains("V2"));
    assert!(!error.to_string().contains(MANAGED_V2_SECRET));
    assert_eq!(fs::read(&fixture.config_path).unwrap(), before);
    assert_managed_row_unresolved(&fixture, &legacy);
}

#[test]
#[serial]
fn managed_opencode_v2_writer_rejects_invalid_documents_before_atomic_commit() {
    let fixture = Fixture::new();
    let grant = managed_v2_grant();
    fixture
        .state
        .db
        .save_provider("opencode", &grant.provider)
        .unwrap();
    // OpenCode uses one checked atomic file replacement, not a multi-file journal.
    // Failures before that replacement must preserve even malformed input bytes.
    for source in [
        "{broken".to_owned(),
        serde_json::to_string(&json!({"model": "keep", "providers": []})).unwrap(),
        serde_json::to_string(&json!({"model": "keep", "providers": null})).unwrap(),
    ] {
        fs::write(&fixture.config_path, source.as_bytes()).unwrap();
        let error = opencode_config::write_provider(&grant.provider).unwrap_err();
        assert!(!error.to_string().contains(MANAGED_V2_SECRET));
        assert_eq!(fs::read(&fixture.config_path).unwrap(), source.as_bytes());
        assert_managed_row_unresolved(&fixture, &grant.provider);
    }
    // A failed attempt must not poison later writes after the user repairs the file.
    fs::write(
        &fixture.config_path,
        serde_json::to_vec(&managed_v2_document()).unwrap(),
    )
    .unwrap();
    opencode_config::write_provider(&grant.provider).unwrap();
    assert_eq!(
        opencode_config::read_opencode_config().unwrap()["providers"][&grant.provider.id]
            ["settings"]["apiKey"],
        MANAGED_V2_SECRET
    );
    assert_managed_row_unresolved(&fixture, &grant.provider);
}

fn managed_import_fixture() -> Provider {
    use crate::provider::{AuthBindingSource, ManagedBinding, ProviderMeta};
    use crate::services::osir::{
        key_vault::{KeyBinding, KeyTool, GATEWAY_ORIGIN},
        managed,
    };
    let mut provider = Provider::with_id(
        "managed-fixture".into(),
        "Managed".into(),
        json!({
            "npm":"@ai-sdk/openai-compatible", "options":{"baseURL":"https://api.osirclaw.com/v1"}, "models":{"old":{}}
        }),
        None,
    );
    provider.meta = Some(ProviderMeta {
        osir_binding: Some(ManagedBinding {
            source: AuthBindingSource::ManagedAccount,
            binding: KeyBinding {
                account_origin: managed::trusted_account_origin().unwrap(),
                account_id: "fixture-account".into(),
                key_id: 1,
                group_id: Some(2),
                tool: KeyTool::OpenCode,
                provider_id: provider.id.clone(),
                gateway_origin: GATEWAY_ORIGIN.into(),
            },
            reference: serde_json::from_value(
                json!({"version":1,"id":"2ca88e87-4834-4ec2-9eaf-a53272cd615a"}),
            )
            .expect("valid opaque reference fixture"),
            masked_key: "masked".into(),
        }),
        ..Default::default()
    });
    provider
}

#[test]
#[serial]
fn osir_snapshot_opencode_remove_and_delete_preserve_neighbors() {
    for delete in [false, true] {
        let fixture = Fixture::new();
        let provider = managed_import_fixture();
        fixture
            .state
            .db
            .save_provider("opencode", &provider)
            .unwrap();
        let original = json!({
            "mcp":{"local":{"command":["fixture-command"]}},
            "model":"ordinary/m",
            "provider":{
                "ordinary":{"options":{"apiKey":"ordinary-fixture-key"}},
                (provider.id.clone()):{"options":{"apiKey":"synthetic-osir-key"}}
            }
        });
        let bytes = serde_json::to_vec_pretty(&original).unwrap();
        fs::write(&fixture.config_path, &bytes).unwrap();
        if delete {
            ProviderService::delete(&fixture.state, AppType::OpenCode, &provider.id).unwrap();
        } else {
            ProviderService::remove_from_live_config(
                &fixture.state,
                AppType::OpenCode,
                &provider.id,
            )
            .unwrap();
        }
        let root = crate::live::engine::DeviceStore::for_device()
            .file("backups/osir-tool-config/opencode");
        let snapshots: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(fs::read(snapshots[0].join("0.bin")).unwrap(), bytes);
        let metadata = fs::read_to_string(snapshots[0].join("manifest.json")).unwrap();
        assert!(!metadata.contains("synthetic-osir-key"));
        let live: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture.config_path).unwrap()).unwrap();
        assert!(live["provider"].get(&provider.id).is_none());
        assert_eq!(
            live["provider"]["ordinary"],
            original["provider"]["ordinary"]
        );
        assert_eq!(live["mcp"], original["mcp"]);
        assert_eq!(live["model"], original["model"]);
        assert_eq!(
            fixture
                .state
                .db
                .get_provider_by_id(&provider.id, "opencode")
                .unwrap()
                .is_none(),
            delete
        );
    }
}

#[test]
#[serial]
fn osir_snapshot_opencode_failure_blocks_removal_and_database_delete() {
    for delete in [false, true] {
        let fixture = Fixture::new();
        let provider = managed_import_fixture();
        fixture
            .state
            .db
            .save_provider("opencode", &provider)
            .unwrap();
        let bytes = serde_json::to_vec(&json!({"provider":{(provider.id.clone()):{}}})).unwrap();
        fs::write(&fixture.config_path, &bytes).unwrap();
        let root = crate::live::engine::DeviceStore::for_device().file("backups/osir-tool-config");
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(root, b"synthetic blocked backup directory").unwrap();
        let result = if delete {
            ProviderService::delete(&fixture.state, AppType::OpenCode, &provider.id)
        } else {
            ProviderService::remove_from_live_config(
                &fixture.state,
                AppType::OpenCode,
                &provider.id,
            )
        };
        assert!(result.is_err());
        assert_eq!(fs::read(&fixture.config_path).unwrap(), bytes);
        assert!(fixture
            .state
            .db
            .get_provider_by_id(&provider.id, "opencode")
            .unwrap()
            .is_some());
    }
}

#[test]
#[serial]
fn managed_opencode_writer_materializes_vault_and_rejects_missing_secret() {
    use crate::services::osir::{
        credentials::{CredentialBackend, CredentialError},
        key_vault::{KeyVault, NativeKeySecret},
        managed::test_support,
    };
    #[derive(Clone, Default)]
    struct Memory(std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>);
    impl CredentialBackend for Memory {
        fn write(&self, _: &str, _: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            *self.0.lock().unwrap() = Some(bytes.to_vec());
            Ok(())
        }
        fn read(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn remove(&self, _: &str, _: &str) -> Result<(), CredentialError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let mut provider = managed_import_fixture();
    let memory = Memory::default();
    let binding = provider
        .meta
        .as_mut()
        .unwrap()
        .osir_binding
        .as_mut()
        .unwrap();
    let vault = KeyVault::with_backend(
        &binding.binding.account_origin,
        "com.osir.fixture",
        memory.clone(),
    )
    .unwrap();
    let stored = vault
        .store(
            &binding.binding,
            &NativeKeySecret::new("fixture-opencode-materialized-secret".into()).unwrap(),
        )
        .unwrap();
    binding.reference = stored.reference;
    binding.masked_key = stored.masked_key;
    fixture
        .state
        .db
        .save_provider("opencode", &provider)
        .unwrap();
    let original = json!({"mcp":{"local":{"command":["fixture-command"]}},"model":"ordinary/m","provider":{"ordinary":{"options":{"apiKey":"ordinary-fixture-key"}}}});
    fs::write(&fixture.config_path, serde_json::to_vec(&original).unwrap()).unwrap();
    test_support::with_vault(vault, || {
        opencode_config::write_provider(&provider).unwrap();
        let live: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture.config_path).unwrap()).unwrap();
        assert_eq!(
            live["provider"][&provider.id]["options"]["apiKey"],
            "fixture-opencode-materialized-secret"
        );
        assert_eq!(live["mcp"], original["mcp"]);
        assert_eq!(live["model"], original["model"]);
        assert_eq!(
            live["provider"]["ordinary"],
            original["provider"]["ordinary"]
        );
        let saved = fixture
            .state
            .db
            .get_provider_by_id(&provider.id, "opencode")
            .unwrap()
            .unwrap();
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("fixture-opencode-materialized-secret"));
        let before = fs::read(&fixture.config_path).unwrap();
        *memory.0.lock().unwrap() = None;
        assert!(opencode_config::write_provider(&provider).is_err());
        assert_eq!(fs::read(&fixture.config_path).unwrap(), before);
    });
}

#[test]
#[serial]
fn managed_live_import_cleans_secret_preserves_binding_and_additive_neighbors() {
    let fixture = Fixture::new();
    let provider = managed_import_fixture();
    fixture
        .state
        .db
        .save_provider("opencode", &provider)
        .unwrap();
    for native in [false, true] {
        let slot = if native { "settings" } else { "options" };
        let mut live =
            json!({"name":"Managed", "models":{"new":{"name":"New","limit":{"input":8192}}}});
        live[slot] =
            json!({"baseURL":"https://api.osirclaw.com/v1","apiKey":"fixture-managed-live-secret"});
        if !native {
            live["npm"] = json!("@ai-sdk/openai-compatible");
        }
        let root = if native { "providers" } else { "provider" };
        let mut document =
            json!({"mcp":{"local":{"command":"fixture-command"}},"model":"ordinary/m"});
        let mut ordinary = json!({});
        ordinary[slot] = json!({"apiKey":"ordinary-key"});
        let mut renamed = json!({});
        renamed[slot] =
            json!({"baseURL":"https://api.osirclaw.com/v1","apiKey":"fixture-managed-live-secret"});
        document[root] = json!({"managed-fixture":live,"ordinary":ordinary,"renamed-managed":renamed,"forged":{"meta":{"osirBinding":{"reference":"forged"}}}});
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&fixture.config_path, &bytes).unwrap();
        import_opencode_providers_from_live(&fixture.state).unwrap();
        assert_eq!(fs::read(&fixture.config_path).unwrap(), bytes);
        let saved = fixture
            .state
            .db
            .get_provider_by_id(&provider.id, "opencode")
            .unwrap()
            .unwrap();
        assert_eq!(
            saved.meta.as_ref().unwrap().osir_binding,
            provider.meta.as_ref().unwrap().osir_binding
        );
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("fixture-managed-live-secret"));
        assert_eq!(
            saved.settings_config["models"]["new"]["limit"]["input"],
            8192
        );
        assert!(fixture
            .state
            .db
            .get_provider_by_id("renamed-managed", "opencode")
            .unwrap()
            .is_none());
        assert!(fixture
            .state
            .db
            .get_provider_by_id("forged", "opencode")
            .unwrap()
            .is_none());
        assert_eq!(
            fixture
                .state
                .db
                .get_provider_by_id("ordinary", "opencode")
                .unwrap()
                .unwrap()
                .settings_config[slot]["apiKey"],
            "ordinary-key"
        );
    }
}

#[test]
#[serial]
fn managed_live_import_rejects_endpoint_and_metadata_forgery_without_changing_db() {
    let fixture = Fixture::new();
    let provider = managed_import_fixture();
    fixture
        .state
        .db
        .save_provider("opencode", &provider)
        .unwrap();
    for live in [
        json!({"options":{"baseURL":"https://evil.invalid","apiKey":"fixture-managed-live-secret"}}),
        json!({"options":{"baseURL":"https://api.osirclaw.com/v1","apiKey":"fixture-managed-live-secret"},"osir_binding":{"reference":"forged"}}),
    ] {
        fs::write(
            &fixture.config_path,
            serde_json::to_vec(&json!({"provider":{"managed-fixture":live}})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            import_opencode_providers_from_live(&fixture.state).unwrap(),
            0
        );
        let saved = fixture
            .state
            .db
            .get_provider_by_id(&provider.id, "opencode")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(&provider).unwrap()
        );
    }
}
