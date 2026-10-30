//! Real file writes with synthetic credentials and a current-thread memory vault.
use super::gemini_direct;
use crate::{
    app_config::AppType,
    database::Database,
    gemini_config::{get_gemini_env_path, get_gemini_settings_path},
    mode::operation::{failpoint, settle, RecoveryOutcome},
    provider::Provider,
    services::osir::{key_vault::GATEWAY_ORIGIN, managed::test_support::Fixture},
};
use serde_json::{json, Value};
use serial_test::serial;
use std::{env, ffi::OsString, fs};
use tempfile::TempDir;

const SECRET: &str = "synthetic-gemini-managed-test-secret";
const OLD_ENV: &str = "# user comment
USER_FLAG=keep
GEMINI_API_KEY=synthetic-old-key
GOOGLE_GEMINI_BASE_URL=https://old.invalid
GEMINI_MODEL=old-model
";

struct IsolatedHome {
    dir: TempDir,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl IsolatedHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let previous = [
            "HOME",
            "USERPROFILE",
            "CC_SWITCH_TEST_HOME",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
        ]
        .into_iter()
        .map(|key| {
            let old = env::var_os(key);
            env::set_var(key, dir.path());
            (key, old)
        })
        .collect();
        let home = Self { dir, previous };
        crate::settings::reload_settings().unwrap();
        // Fail before touching files if a cached custom directory escaped isolation.
        assert!(get_gemini_env_path().starts_with(home.dir.path()));
        assert!(get_gemini_settings_path().starts_with(home.dir.path()));
        home
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        failpoint::crash_at(None);
        for (key, value) in &self.previous {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
        let _ = crate::settings::reload_settings();
    }
}

fn original_settings() -> Value {
    json!({
        "mcpServers": {"user-server": {"command": "fixture-command", "args": ["--keep"]}},
        "security": {"auth": {"selectedType": "oauth-personal", "userChoice": true}, "userPolicy": "keep"},
        "model": {"name": "old-model", "userLimit": 42},
        "ui": {"theme": "user-theme"}
    })
}

fn seed() -> (Database, Fixture) {
    let fixture = Fixture::new(
        &AppType::Gemini,
        json!({
            "env": {"GOOGLE_GEMINI_BASE_URL": GATEWAY_ORIGIN, "GEMINI_MODEL": "fixture-model"},
            "config": {"model": {"name": "fixture-model"}}
        }),
        SECRET,
    );
    let db = Database::memory().unwrap();
    db.save_provider(
        "gemini",
        &Provider::with_id("old".into(), "Old fixture".into(), json!({}), None),
    )
    .unwrap();
    db.save_provider("gemini", &fixture.provider).unwrap();
    db.set_current_provider("gemini", "old").unwrap();
    fs::create_dir_all(get_gemini_env_path().parent().unwrap()).unwrap();
    fs::write(get_gemini_env_path(), OLD_ENV).unwrap();
    fs::write(
        get_gemini_settings_path(),
        serde_json::to_vec_pretty(&original_settings()).unwrap(),
    )
    .unwrap();
    (db, fixture)
}

fn bytes() -> (Vec<u8>, Vec<u8>) {
    (
        fs::read(get_gemini_env_path()).unwrap(),
        fs::read(get_gemini_settings_path()).unwrap(),
    )
}

fn assert_live(expected_user_settings: Value) {
    let (env, settings) = bytes();
    let env = String::from_utf8(env).unwrap();
    assert!(env
        .lines()
        .any(|line| line == format!("GEMINI_API_KEY={SECRET}")));
    assert!(env
        .lines()
        .any(|line| line == format!("GOOGLE_GEMINI_BASE_URL={GATEWAY_ORIGIN}")));
    assert!(env.lines().any(|line| line == "GEMINI_MODEL=fixture-model"));
    assert!(env.contains("# user comment"));
    assert!(env.contains("USER_FLAG=keep"));
    assert!(!env.contains("synthetic-old-key"));
    assert!(!env.contains("OSIR_NATIVE_MANAGED_REFERENCE"));
    let mut expected = expected_user_settings;
    expected["security"]["auth"]["selectedType"] = json!("gemini-api-key");
    expected["model"]["name"] = json!("fixture-model");
    assert_eq!(
        serde_json::from_slice::<Value>(&settings).unwrap(),
        expected
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(get_gemini_env_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

fn assert_unresolved(db: &Database, fixture: &Fixture) {
    let row = db
        .get_provider_by_id(&fixture.provider.id, "gemini")
        .unwrap()
        .unwrap();
    assert_eq!(row.settings_config, fixture.provider.settings_config);
    assert!(row.meta.as_ref().unwrap().osir_binding.is_some());
    assert!(
        !serde_json::to_string(&db.get_all_providers("gemini").unwrap())
            .unwrap()
            .contains(SECRET)
    );
    assert!(!serde_json::to_string(&fixture.provider)
        .unwrap()
        .contains(SECRET));
}

#[test]
#[serial]
fn managed_gemini_switch_materializes_files_and_preserves_user_fields() {
    let _home = IsolatedHome::new();
    let (db, fixture) = seed();
    let db = std::sync::Arc::new(db);
    let state = crate::store::AppState::new(db.clone());
    super::ProviderService::switch(&state, AppType::Gemini, &fixture.provider.id).unwrap();
    assert_live(original_settings());
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some(fixture.provider.id.as_str())
    );
    assert_unresolved(&db, &fixture);
}

#[test]
#[serial]
fn managed_gemini_reapply_preserves_pointer_and_user_edits() {
    let _home = IsolatedHome::new();
    let (db, fixture) = seed();
    let mut edited = original_settings();
    edited["ui"]["theme"] = json!("edited-theme");
    edited["mcpServers"]["another"] = json!({"command": "another-fixture"});
    fs::write(
        get_gemini_settings_path(),
        serde_json::to_vec(&edited).unwrap(),
    )
    .unwrap();
    gemini_direct::reapply(&db, &fixture.provider).unwrap();
    assert_live(edited);
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some("old")
    );
    assert_unresolved(&db, &fixture);
}

#[test]
#[serial]
fn managed_gemini_missing_vault_keeps_files_and_pointer_unchanged() {
    let _home = IsolatedHome::new();
    let (db, fixture) = seed();
    let before = bytes();
    fixture.clear_credentials();
    assert!(gemini_direct::switch_to(&db, &fixture.provider).is_err());
    assert_eq!(bytes(), before);
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some("old")
    );
    assert!(gemini_direct::reapply(&db, &fixture.provider).is_err());
    assert_eq!(bytes(), before);
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some("old")
    );
    assert!(settle(&db, "gemini").unwrap().is_none());
    assert_unresolved(&db, &fixture);
}

#[test]
#[serial]
fn managed_gemini_partial_publish_recovers_both_files_and_pointer() {
    let _home = IsolatedHome::new();
    let (db, fixture) = seed();
    let before = bytes();
    failpoint::crash_at(Some("published:0"));
    let result = gemini_direct::switch_to(&db, &fixture.provider);
    failpoint::crash_at(None);
    assert!(result.is_err());
    let partial = bytes();
    assert_ne!(partial.0, before.0);
    assert!(String::from_utf8(partial.0).unwrap().contains(SECRET));
    assert_eq!(partial.1, before.1);
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some("old")
    );
    assert_unresolved(&db, &fixture);
    // Same recovery entry used by the next operation, independent of UI retry.
    assert!(matches!(
        settle(&db, "gemini").unwrap(),
        Some(RecoveryOutcome::RolledForward)
    ));
    assert_live(original_settings());
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some(fixture.provider.id.as_str())
    );
    assert!(settle(&db, "gemini").unwrap().is_none());
    assert_unresolved(&db, &fixture);
}

#[test]
#[serial]
fn managed_gemini_unpublished_failure_discards_both_files_and_keeps_pointer() {
    let _home = IsolatedHome::new();
    let (db, fixture) = seed();
    let before = bytes();
    failpoint::crash_at(Some("pending"));
    let result = gemini_direct::switch_to(&db, &fixture.provider);
    failpoint::crash_at(None);
    assert!(result.is_err());
    assert_eq!(bytes(), before);
    assert!(matches!(
        settle(&db, "gemini").unwrap(),
        Some(RecoveryOutcome::Discarded)
    ));
    assert_eq!(bytes(), before);
    assert_eq!(
        db.get_current_provider("gemini").unwrap().as_deref(),
        Some("old")
    );
    assert!(settle(&db, "gemini").unwrap().is_none());
    assert_unresolved(&db, &fixture);
}
