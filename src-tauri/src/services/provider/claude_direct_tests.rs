use super::*;
use crate::mode::operation::{failpoint, settle, RecoveryOutcome};
use crate::services::osir::managed::test_support::Fixture as ManagedFixture;
use crate::settings::{get_settings, update_settings, AppSettings};
use serde_json::{json, Value};
use serial_test::serial;
use std::{ffi::OsString, fs, path::PathBuf};

const SECRET: &str = "fixture-claude-managed-secret-only";

struct Home {
    previous_env: Vec<(&'static str, Option<OsString>)>,
    previous_settings: AppSettings,
    path: PathBuf,
    db: std::sync::Arc<Database>,
    _dir: tempfile::TempDir,
}
impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let previous_settings = get_settings();
        let previous_env = ["HOME", "USERPROFILE", "CC_SWITCH_TEST_HOME"]
            .into_iter()
            .map(|key| {
                let previous = std::env::var_os(key);
                std::env::set_var(key, dir.path());
                (key, previous)
            })
            .collect();
        let config_dir = dir.path().join("claude");
        fs::create_dir_all(&config_dir).unwrap();
        update_settings(AppSettings {
            claude_config_dir: Some(config_dir.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
        let path = get_claude_settings_path();
        assert!(path.starts_with(dir.path()));
        assert!(crate::live::engine::DeviceStore::for_device()
            .state_path()
            .starts_with(dir.path()));
        let home = Self {
            previous_env,
            previous_settings,
            path,
            db: std::sync::Arc::new(Database::memory().unwrap()),
            _dir: dir,
        };
        fs::write(
            &home.path,
            serde_json::to_vec_pretty(&json!({
                "env": {"USER_FLAG": "keep", "ANTHROPIC_API_KEY": "old-fixture-key"},
                "mcpServers": {"local": {"command": "fixture-mcp", "args": ["keep"]}},
                "permissions": {"allow": ["Read"]},
                "hooks": {"Stop": []}
            }))
            .unwrap(),
        )
        .unwrap();
        home
    }
    fn live(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap()
    }
    fn save(&self, p: &Provider) {
        self.db.save_provider("claude", p).unwrap();
    }
    fn assert_saved(&self, p: &Provider) {
        let saved = self
            .db
            .get_provider_by_id(&p.id, "claude")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::to_value(p).unwrap()
        );
        assert!(!serde_json::to_string(&saved).unwrap().contains(SECRET));
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        failpoint::crash_at(None);
        // Restore cached settings while writes still target the isolated home.
        let _ = update_settings(self.previous_settings.clone());
        for (key, previous) in &self.previous_env {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
fn managed() -> ManagedFixture {
    ManagedFixture::new(
        &AppType::Claude,
        json!({"env": {"ANTHROPIC_MODEL": "fixture-model-a"}}),
        SECRET,
    )
}
fn assert_user_fields(before: &Value, after: &Value) {
    for key in ["mcpServers", "permissions", "hooks"] {
        assert_eq!(before[key], after[key]);
    }
    assert_eq!(after["env"]["USER_FLAG"], "keep");
}

#[test]
#[serial]
fn managed_claude_switch_and_reapply_real_settings_preserve_user_fields_and_keyless_db() {
    let home = Home::new();
    let fixture = managed();
    let before = home.live();
    home.save(&fixture.provider);
    let state = crate::store::AppState::new(home.db.clone());
    crate::services::provider::ProviderService::switch(
        &state,
        AppType::Claude,
        &fixture.provider.id,
    )
    .unwrap();
    let live = home.live();
    assert_user_fields(&before, &live);
    assert_eq!(live["env"]["ANTHROPIC_AUTH_TOKEN"], SECRET);
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        "https://api.osirclaw.com"
    );
    assert!(live["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(
        home.db.get_current_provider("claude").unwrap().as_deref(),
        Some(fixture.provider.id.as_str())
    );
    home.assert_saved(&fixture.provider);
    let mut edited = fixture.provider.clone();
    edited.settings_config["env"]["ANTHROPIC_MODEL"] = json!("fixture-model-b");
    home.save(&edited);
    reapply(&home.db, Some(&fixture.provider), &edited).unwrap();
    assert_eq!(home.live()["env"]["ANTHROPIC_MODEL"], "fixture-model-b");
    assert_eq!(home.live()["env"]["ANTHROPIC_AUTH_TOKEN"], SECRET);
    assert_user_fields(&before, &home.live());
    home.assert_saved(&edited);
    assert_eq!(
        home.db.get_current_provider("claude").unwrap().as_deref(),
        Some(edited.id.as_str())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&home.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
#[serial]
fn managed_claude_missing_credential_keeps_file_and_pointer_unchanged() {
    let home = Home::new();
    let fixture = managed();
    home.save(&fixture.provider);
    switch_to(&home.db, None, &fixture.provider).unwrap();
    let before = fs::read(&home.path).unwrap();
    fixture.clear_credentials();
    assert!(switch_to(&home.db, Some(&fixture.provider), &fixture.provider).is_err());
    assert!(reapply(&home.db, Some(&fixture.provider), &fixture.provider).is_err());
    assert_eq!(fs::read(&home.path).unwrap(), before);
    assert_eq!(
        home.db.get_current_provider("claude").unwrap().as_deref(),
        Some(fixture.provider.id.as_str())
    );
    assert_eq!(settle(&home.db, "claude").unwrap(), None);
    home.assert_saved(&fixture.provider);
}

#[test]
#[serial]
fn managed_claude_interrupted_switch_recovers_real_file_and_pointer() {
    for (point, outcome) in [
        ("pending", RecoveryOutcome::Discarded),
        ("published:0", RecoveryOutcome::RolledForward),
    ] {
        let home = Home::new();
        let fixture = managed();
        home.save(&fixture.provider);
        let before = fs::read(&home.path).unwrap();
        let before_value = home.live();
        failpoint::crash_at(Some(point));
        assert!(switch_to(&home.db, None, &fixture.provider).is_err());
        failpoint::crash_at(None);
        assert_eq!(home.db.get_current_provider("claude").unwrap(), None);
        assert_eq!(settle(&home.db, "claude").unwrap(), Some(outcome));
        if point == "pending" {
            assert_eq!(fs::read(&home.path).unwrap(), before);
            assert_eq!(home.db.get_current_provider("claude").unwrap(), None);
            switch_to(&home.db, None, &fixture.provider).unwrap();
        }
        assert_eq!(home.live()["env"]["ANTHROPIC_AUTH_TOKEN"], SECRET);
        assert_user_fields(&before_value, &home.live());
        assert_eq!(
            home.db.get_current_provider("claude").unwrap().as_deref(),
            Some(fixture.provider.id.as_str())
        );
        assert_eq!(settle(&home.db, "claude").unwrap(), None);
        home.assert_saved(&fixture.provider);
    }
}
