use super::*;
use crate::mode::operation::{failpoint, settle, RecoveryOutcome};
use crate::services::osir::managed::test_support::Fixture as Grant;
use crate::settings::{get_settings, update_settings, AppSettings};
use serde_json::json;
use serial_test::serial;
use std::{ffi::OsString, fs};

const SECRET: &str = "fixture-codex-materialized-secret";
struct Home {
    settings: AppSettings,
    env: Option<OsString>,
    _dir: tempfile::TempDir,
}
impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let settings = get_settings();
        let env = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", dir.path());
        let config = dir.path().join("codex");
        fs::create_dir_all(&config).unwrap();
        update_settings(AppSettings {
            codex_config_dir: Some(config.to_string_lossy().into_owned()),
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .unwrap();
        assert!(get_codex_config_path().starts_with(dir.path()));
        assert!(DeviceStore::for_device()
            .state_path()
            .starts_with(dir.path()));
        fs::write(
            get_codex_config_path(),
            r#"# user comment
model = "old"
[mcp_servers.local]
command = "fixture-mcp"
"#,
        )
        .unwrap();
        fs::write(get_codex_auth_path(), br#"{"tokens":{"access_token":"fixture-native-login","refresh_token":"fixture-native-refresh"},"last_refresh":"fixture-time"}"#).unwrap();
        Self {
            settings,
            env,
            _dir: dir,
        }
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        failpoint::crash_at(None);
        let _ = update_settings(self.settings.clone());
        match &self.env {
            Some(v) => std::env::set_var("CC_SWITCH_TEST_HOME", v),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
    }
}
fn grant() -> Grant {
    Grant::new(
        &AppType::Codex,
        json!({"auth":{}, "config":r#"model_provider = "osir"
model = "fixture-model"
[model_providers.osir]
name = "OSIR"
base_url = "https://api.osirclaw.com/v1"
wire_api = "responses"
"#}),
        SECRET,
    )
}
fn apply(db: &Database, provider: &Provider) -> Result<OperationReport, AppError> {
    let prepared = Prepared::default();
    let planned = plan(db, &Owner::None, &Target::Direct(Some(provider)), &prepared)?;
    run(
        db,
        op::SWITCH,
        planned,
        &prepared,
        PendingTarget::pointer(Some(provider.id.clone())),
    )
}
fn assert_live(db: &Database, provider: &Provider, auth: &[u8]) {
    let text = fs::read_to_string(get_codex_config_path()).unwrap();
    let doc: toml::Value = text.parse().unwrap();
    let id = doc["model_provider"].as_str().unwrap();
    assert_eq!(
        doc["model_providers"][id]["experimental_bearer_token"].as_str(),
        Some(SECRET)
    );
    assert_eq!(
        doc["mcp_servers"]["local"]["command"].as_str(),
        Some("fixture-mcp")
    );
    assert!(text.contains("# user comment"));
    assert_eq!(fs::read(get_codex_auth_path()).unwrap(), auth);
    let saved = db
        .get_provider_by_id(&provider.id, "codex")
        .unwrap()
        .unwrap();
    assert!(!serde_json::to_string(&saved).unwrap().contains(SECRET));
    assert_eq!(
        db.get_current_provider("codex").unwrap().as_deref(),
        Some(provider.id.as_str())
    );
}
#[test]
#[serial]
fn managed_codex_writer_preserves_native_login_and_rejects_missing_key() {
    let _home = Home::new();
    let grant = grant();
    let db = Database::memory().unwrap();
    db.save_provider("codex", &grant.provider).unwrap();
    let auth = fs::read(get_codex_auth_path()).unwrap();
    apply(&db, &grant.provider).unwrap();
    assert_live(&db, &grant.provider, &auth);
    let before = fs::read(get_codex_config_path()).unwrap();
    grant.clear_credentials();
    assert!(apply(&db, &grant.provider).is_err());
    assert_eq!(fs::read(get_codex_config_path()).unwrap(), before);
    assert_live(&db, &grant.provider, &auth);
}

#[test]
#[serial]
fn managed_codex_stack_writer_publishes_stable_ids_and_matching_default() {
    use crate::mode::stack::{codex_model_ids, Member};
    let _home = Home::new();
    let mut a = grant();
    let mut b = grant();
    for fixture in [&mut a, &mut b] {
        fixture.provider.settings_config["modelCatalog"] =
            json!({"models": [{"model": "fixture-model"}]});
    }
    let db = Database::memory().unwrap();
    for fixture in [&a, &b] {
        db.save_provider("codex", &fixture.provider).unwrap();
    }
    let auth = fs::read(get_codex_auth_path()).unwrap();
    for (route, selected) in [(&a.provider, "a"), (&b.provider, "b")] {
        let members: Vec<_> = [(&a.provider, "a"), (&b.provider, "b")]
            .into_iter()
            .map(|(provider, key)| Member {
                provider: provider.clone(),
                key: key.into(),
                route: provider.id == route.id,
                model_ids: codex_model_ids(key, provider),
            })
            .collect();
        let mut identity = crate::mode::state::StackState::default();
        identity.enabled = true;
        identity.managed_identity_required = true;
        for member in &members {
            identity
                .keys
                .insert(member.key.clone(), member.provider.id.clone());
            identity.members.push(member.provider.id.clone());
        }
        let prepared = Prepared::default();
        let target = Target::Proxy {
            route,
            base_url: "http://127.0.0.1:15721/v1",
            stack: &members,
            identity: Some(&identity),
        };
        let planned = plan(&db, &Owner::None, &target, &prepared).unwrap();
        run(
            &db,
            op::SWITCH,
            planned,
            &prepared,
            PendingTarget::pointer(Some(route.id.clone())),
        )
        .unwrap();
        let text = fs::read_to_string(get_codex_config_path()).unwrap();
        let doc: toml::Value = text.parse().unwrap();
        assert_eq!(
            doc["model"].as_str().unwrap(),
            format!("ccs-{selected}/fixture-model")
        );
        let catalog: Value =
            serde_json::from_slice(&fs::read(get_codex_model_catalog_path()).unwrap()).unwrap();
        let ids: std::collections::BTreeSet<_> = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["slug"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["ccs-a/fixture-model", "ccs-b/fixture-model"]
                .into_iter()
                .collect()
        );
        assert_eq!(
            doc["mcp_servers"]["local"]["command"].as_str(),
            Some("fixture-mcp")
        );
        assert_eq!(fs::read(get_codex_auth_path()).unwrap(), auth);
        assert!(!text.contains(SECRET));
    }
}
#[test]
#[serial]
fn managed_codex_writer_recovers_interrupted_publish() {
    for (point, outcome) in [
        ("pending", RecoveryOutcome::Discarded),
        ("published:0", RecoveryOutcome::RolledForward),
    ] {
        let _home = Home::new();
        let grant = grant();
        let db = Database::memory().unwrap();
        db.save_provider("codex", &grant.provider).unwrap();
        let auth = fs::read(get_codex_auth_path()).unwrap();
        let before = fs::read(get_codex_config_path()).unwrap();
        failpoint::crash_at(Some(point));
        assert!(apply(&db, &grant.provider).is_err());
        failpoint::crash_at(None);
        assert_eq!(settle(&db, "codex").unwrap(), Some(outcome));
        if point == "pending" {
            assert_eq!(fs::read(get_codex_config_path()).unwrap(), before);
            assert_eq!(db.get_current_provider("codex").unwrap(), None);
            apply(&db, &grant.provider).unwrap();
        }
        assert_live(&db, &grant.provider, &auth);
        assert_eq!(settle(&db, "codex").unwrap(), None);
    }
}

#[test]
#[serial]
fn managed_codex_default_switch_stashes_native_login_and_official_restores_it() {
    let home = Home::new();
    // Home intentionally opts in for the other tests; exercise the default here only.
    assert!(!AppSettings::default().preserve_codex_official_auth_on_switch);
    update_settings(AppSettings {
        preserve_codex_official_auth_on_switch: false,
        ..get_settings()
    })
    .unwrap();

    let grant = grant();
    let db = Database::memory().unwrap();
    let mut official = Provider::with_id(
        "fixture-official".into(),
        "Official fixture".into(),
        json!({"auth": {}, "config": "model = 'fixture-official-model'"}),
        None,
    );
    official.category = Some("official".into());
    db.save_provider("codex", &official).unwrap();
    db.save_provider("codex", &grant.provider).unwrap();
    db.set_current_provider("codex", &official.id).unwrap();
    let config = fs::read_to_string(get_codex_config_path()).unwrap();
    fs::write(
        get_codex_config_path(),
        format!(
            "sandbox_mode = 'workspace-write'
user_fixture_flag = true
{config}"
        ),
    )
    .unwrap();
    let auth: Value = serde_json::from_slice(&fs::read(get_codex_auth_path()).unwrap()).unwrap();
    let original: toml::Value = fs::read_to_string(get_codex_config_path())
        .unwrap()
        .parse()
        .unwrap();
    let stash_path = DeviceStore::for_device().file(STASH_FILENAME);
    assert!(stash_path.starts_with(home._dir.path()));
    assert!(!stash_path.exists());

    let assert_db_clean = || {
        let rows = serde_json::to_string(&db.get_all_providers("codex").unwrap()).unwrap();
        for forbidden in [SECRET, "fixture-native-login", "fixture-native-refresh"] {
            assert!(
                !rows.contains(forbidden),
                "credentials must stay out of provider rows"
            );
        }
    };
    apply(&db, &grant.provider).unwrap();
    assert!(
        !get_codex_auth_path().exists(),
        "default mode removes auth.json"
    );
    let stash: Value = serde_json::from_slice(&fs::read(&stash_path).unwrap()).unwrap();
    let logins = stash["logins"].as_object().unwrap();
    assert_eq!(logins.len(), 1);
    assert_eq!(logins.values().next().unwrap(), &auth);
    assert!(!serde_json::to_string(&stash).unwrap().contains(SECRET));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&stash_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let managed_text = fs::read_to_string(get_codex_config_path()).unwrap();
    let managed: toml::Value = managed_text.parse().unwrap();
    let route = managed["model_provider"].as_str().unwrap();
    assert_eq!(
        managed["model_providers"][route]["experimental_bearer_token"].as_str(),
        Some(SECRET)
    );
    assert_eq!(managed["mcp_servers"], original["mcp_servers"]);
    assert_eq!(managed["sandbox_mode"], original["sandbox_mode"]);
    assert_eq!(managed["user_fixture_flag"], original["user_fixture_flag"]);
    assert!(managed_text.contains("# user comment"));
    assert_eq!(
        db.get_current_provider("codex").unwrap().as_deref(),
        Some(grant.provider.id.as_str())
    );
    assert_db_clean();

    apply(&db, &official).unwrap();
    let restored: Value =
        serde_json::from_slice(&fs::read(get_codex_auth_path()).unwrap()).unwrap();
    assert_eq!(
        restored, auth,
        "restore the complete native JSON, including refresh metadata"
    );
    let text = fs::read_to_string(get_codex_config_path()).unwrap();
    let config: toml::Value = text.parse().unwrap();
    assert_eq!(config["mcp_servers"], original["mcp_servers"]);
    assert_eq!(config["sandbox_mode"], original["sandbox_mode"]);
    assert_eq!(config["user_fixture_flag"], original["user_fixture_flag"]);
    assert!(text.contains("# user comment"));
    assert!(!text.contains(SECRET));
    assert_eq!(
        db.get_current_provider("codex").unwrap().as_deref(),
        Some(official.id.as_str())
    );
    let stash: Value = serde_json::from_slice(&fs::read(&stash_path).unwrap()).unwrap();
    assert!(stash["logins"].as_object().unwrap().is_empty());
    assert_db_clean();
}

fn identity_proxy_write(
    db: &Database,
    route: &Provider,
    identity: &crate::mode::state::StackState,
    prepared: &Prepared,
) -> Result<(), AppError> {
    let members =
        crate::mode::stack::published_members(db, &AppType::Codex, identity, Some(&route.id))?;
    let target = Target::Proxy {
        route,
        base_url: "http://127.0.0.1:15721/v1",
        stack: &members,
        identity: Some(identity),
    };
    let planned = plan(db, &Owner::None, &target, prepared)?;
    let pending = PendingTarget {
        state: Some(crate::mode::state::ModeState {
            mode: Some(crate::mode::state::Mode::Proxy),
            attached: true,
            proxy_route: Some(route.id.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    run(db, op::STACK, planned, prepared, pending)?;
    Ok(())
}

#[test]
#[serial]
fn managed_mixed_stack_writer_keeps_ordinary_default_encoded_after_removal() {
    use crate::mode::{stack, state};
    let _home = Home::new();
    let mut managed = grant();
    managed.provider.settings_config["modelCatalog"] =
        json!({"models":[{"model":"fixture-model"}]});
    let mut ordinary = managed.provider.clone();
    ordinary.id = "ordinary".into();
    ordinary.name = "Ordinary".into();
    ordinary.meta = None;
    ordinary.settings_config["auth"] = json!({"OPENAI_API_KEY":"fixture-ordinary-key"});
    let db = Database::memory().unwrap();
    for p in [&managed.provider, &ordinary] {
        db.save_provider("codex", p).unwrap();
    }
    let mut identity = state::StackState::default();
    identity.enabled = true;
    let a = stack::allocate_key(&mut identity, &managed.provider);
    let b = stack::allocate_key(&mut identity, &ordinary);
    identity.members = vec![managed.provider.id.clone(), ordinary.id.clone()];
    let auth = fs::read(get_codex_auth_path()).unwrap();
    for route in [&managed.provider, &ordinary, &managed.provider, &ordinary] {
        identity_proxy_write(&db, route, &identity, &Prepared::default()).unwrap();
        let text = fs::read_to_string(get_codex_config_path()).unwrap();
        let doc: toml::Value = text.parse().unwrap();
        let key = identity.key_of(&route.id).unwrap();
        assert_eq!(
            doc["model"].as_str(),
            Some(format!("ccs-{key}/fixture-model").as_str())
        );
        assert_eq!(
            doc["mcp_servers"]["local"]["command"].as_str(),
            Some("fixture-mcp")
        );
        for (key, id) in [(&a, &managed.provider.id), (&b, &ordinary.id)] {
            match stack::resolve(
                &db,
                &DeviceStore::for_device(),
                &AppType::Codex,
                &format!("ccs-{key}/fixture-model"),
            )
            .unwrap()
            {
                stack::Resolved::Hit(target) => assert_eq!(&target.provider.id, id),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(fs::read(get_codex_auth_path()).unwrap(), auth);
    }
    identity.members.retain(|id| id != &managed.provider.id);
    db.delete_provider("codex", &managed.provider.id).unwrap();
    identity_proxy_write(&db, &ordinary, &identity, &Prepared::default()).unwrap();
    let catalog: Value =
        serde_json::from_slice(&fs::read(get_codex_model_catalog_path()).unwrap()).unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["models"][0]["slug"],
        format!("ccs-{b}/fixture-model")
    );
    assert!(matches!(
        stack::resolve(
            &db,
            &DeviceStore::for_device(),
            &AppType::Codex,
            "fixture-model"
        )
        .unwrap(),
        stack::Resolved::Miss(_)
    ));
}

#[test]
#[serial]
fn managed_official_default_writer_publishes_native_rows_and_preserves_login() {
    use crate::mode::{stack, state};
    let _home = Home::new();
    let db = Database::memory().unwrap();
    let mut official = Provider::with_id(
        "official-fixture".into(),
        "Official".into(),
        json!({"auth":{}, "config":"model = \"native-fixture\"\n"}),
        None,
    );
    official.category = Some("official".into());
    db.save_provider("codex", &official).unwrap();
    let mut identity = state::StackState::default();
    identity.enabled = true;
    identity.managed_identity_required = true;
    let key = stack::allocate_key(&mut identity, &official);
    identity.members.push(official.id.clone());
    let mut managed = grant();
    managed.provider.settings_config["modelCatalog"] =
        json!({"models":[{"model":"fixture-model"}]});
    db.save_provider("codex", &managed.provider).unwrap();
    let managed_key = stack::allocate_key(&mut identity, &managed.provider);
    identity.members.push(managed.provider.id.clone());
    let prepared = Prepared {
        native: Some(NativeRows::Bundled {
            rows: vec![
                json!({"slug":"native-fixture", "display_name":"Native fixture", "priority":1, "context_window":12345}),
            ],
        }),
        ..Default::default()
    };
    let auth = fs::read(get_codex_auth_path()).unwrap();
    identity_proxy_write(&db, &official, &identity, &prepared).unwrap();
    assert_eq!(fs::read(get_codex_auth_path()).unwrap(), auth);
    let text = fs::read_to_string(get_codex_config_path()).unwrap();
    let doc: toml::Value = text.parse().unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(format!("ccs-{key}/native-fixture").as_str())
    );
    assert_eq!(
        doc["mcp_servers"]["local"]["command"].as_str(),
        Some("fixture-mcp")
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(get_codex_model_catalog_path()).unwrap()).unwrap();
    assert_eq!(catalog["models"][0]["context_window"], 12345);
    assert!(state::stack(&DeviceStore::for_device(), "codex")
        .unwrap()
        .codex_official_catalogs
        .contains_key(&key));
    assert!(matches!(
        stack::resolve(
            &db,
            &DeviceStore::for_device(),
            &AppType::Codex,
            &format!("ccs-{key}/native-fixture")
        )
        .unwrap(),
        stack::Resolved::Hit(_)
    ));
    assert!(matches!(
        stack::resolve(
            &db,
            &DeviceStore::for_device(),
            &AppType::Codex,
            "native-fixture"
        )
        .unwrap(),
        stack::Resolved::Miss(_)
    ));
    match stack::resolve(
        &db,
        &DeviceStore::for_device(),
        &AppType::Codex,
        &format!("ccs-{managed_key}/fixture-model"),
    )
    .unwrap()
    {
        stack::Resolved::Hit(target) => assert_eq!(target.provider.id, managed.provider.id),
        other => panic!("{other:?}"),
    }
    let before = fs::read(get_codex_config_path()).unwrap();
    assert!(identity_proxy_write(&db, &official, &identity, &Prepared::default()).is_err());
    assert_eq!(fs::read(get_codex_config_path()).unwrap(), before);
    assert_eq!(fs::read(get_codex_auth_path()).unwrap(), auth);
}

#[test]
#[serial]
fn managed_stable_stack_external_catalog_fails_before_file_changes() {
    use crate::mode::{stack, state};
    let _home = Home::new();
    let grant = grant();
    let mut ordinary = grant.provider.clone();
    ordinary.meta = None;
    ordinary.id = "external".into();
    ordinary.settings_config["auth"] = json!({"OPENAI_API_KEY":"fixture-ordinary-key"});
    let text = ordinary.settings_config["config"]
        .as_str()
        .unwrap()
        .to_string();
    ordinary.settings_config["config"] = json!(format!(
        "model_catalog_json = \"/fixture/user-catalog.json\"\n{text}"
    ));
    let db = Database::memory().unwrap();
    db.save_provider("codex", &ordinary).unwrap();
    let mut identity = state::StackState::default();
    identity.enabled = true;
    identity.managed_identity_required = true;
    stack::allocate_key(&mut identity, &ordinary);
    identity.members.push(ordinary.id.clone());
    let before = fs::read(get_codex_config_path()).unwrap();
    assert!(identity_proxy_write(&db, &ordinary, &identity, &Prepared::default()).is_err());
    assert_eq!(fs::read(get_codex_config_path()).unwrap(), before);
}

#[test]
#[serial]
fn managed_official_catalog_state_recovers_with_published_files() {
    use crate::mode::{stack, state};
    let _home = Home::new();
    let db = Database::memory().unwrap();
    let mut official = Provider::with_id(
        "official-recovery".into(),
        "Official".into(),
        json!({"auth":{},"config":""}),
        None,
    );
    official.category = Some("official".into());
    db.save_provider("codex", &official).unwrap();
    let mut identity = state::StackState::default();
    identity.enabled = true;
    identity.managed_identity_required = true;
    let key = stack::allocate_key(&mut identity, &official);
    identity.members.push(official.id.clone());
    let prepared = Prepared {
        native: Some(NativeRows::Bundled {
            rows: vec![json!({"slug":"native-recovery", "priority":1})],
        }),
        ..Default::default()
    };
    failpoint::crash_at(Some("published:0"));
    let result = identity_proxy_write(&db, &official, &identity, &prepared);
    failpoint::crash_at(None);
    assert!(result.is_err());
    assert_eq!(
        settle(&db, "codex").unwrap(),
        Some(RecoveryOutcome::RolledForward)
    );
    let doc: toml::Value = fs::read_to_string(get_codex_config_path())
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        doc["model"].as_str(),
        Some(format!("ccs-{key}/native-recovery").as_str())
    );
    assert!(state::stack(&DeviceStore::for_device(), "codex")
        .unwrap()
        .codex_official_catalogs
        .contains_key(&key));
    assert!(matches!(
        stack::resolve(
            &db,
            &DeviceStore::for_device(),
            &AppType::Codex,
            &format!("ccs-{key}/native-recovery")
        )
        .unwrap(),
        stack::Resolved::Hit(_)
    ));
}

#[test]
#[serial]
fn ordinary_proxy_without_identity_policy_retains_bare_default() {
    let _home = Home::new();
    let fixture = grant();
    let mut ordinary = fixture.provider.clone();
    ordinary.meta = None;
    ordinary.settings_config["auth"] = json!({"OPENAI_API_KEY":"ordinary-fixture-key"});
    let db = Database::memory().unwrap();
    db.save_provider("codex", &ordinary).unwrap();
    let target = Target::Proxy {
        route: &ordinary,
        base_url: "http://127.0.0.1:15721/v1",
        stack: &[],
        identity: None,
    };
    let prepared = Prepared::default();
    let planned = plan(&db, &Owner::None, &target, &prepared).unwrap();
    run(&db, op::STACK, planned, &prepared, PendingTarget::default()).unwrap();
    let doc: toml::Value = fs::read_to_string(get_codex_config_path())
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(doc["model"].as_str(), Some("fixture-model"));
}
