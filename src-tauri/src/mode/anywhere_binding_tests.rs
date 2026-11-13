use super::*;
use crate::database::Database;
use crate::provider::Provider;
use serde_json::json;
use std::sync::Arc;

fn models(ids: &[&str]) -> Vec<BindingModel> {
    ids.iter()
        .map(|id| BindingModel {
            id: (*id).into(),
            title: (*id).into(),
        })
        .collect()
}
fn config(model: &str) -> String {
    format!("model = {model:?}\nmodel_reasoning_effort = \"high\"\nmodel_catalog_json = \"catalog.json\"\n")
}
fn catalog() -> Vec<u8> {
    serde_json::to_vec(&json!({"models":[
        {"slug":"ccs-a/gpt-a","display_name":"Model A","secret":"CATALOG_SECRET_SENTINEL"},
        {"slug":"ccs-b/gpt-b","display_name":"Model B"},
        {"slug":"stale-155","display_name":"Do not export"}
    ]}))
    .unwrap()
}
fn row(id: &str, model: &str) -> Provider {
    Provider::with_id(
        id.into(),
        id.into(),
        json!({
            "auth":{"OPENAI_API_KEY":"PROVIDER_SECRET_SENTINEL"},
            "config":format!("model = {model:?}\n"),
            "modelCatalog":{"models":[{"model":model}]}
        }),
        None,
    )
}
fn state_with_stack() -> (AppState, state::AppLiveState) {
    let db = Arc::new(Database::memory().unwrap());
    let a = row("a", "gpt-a");
    let b = row("b", "gpt-b");
    db.save_provider("codex", &a).unwrap();
    db.save_provider("codex", &b).unwrap();
    let mut stack = state::StackState {
        enabled: true,
        ..Default::default()
    };
    stack::allocate_key(&mut stack, &a);
    stack::allocate_key(&mut stack, &b);
    stack.members = vec!["a".into(), "b".into()];
    let saved = state::AppLiveState {
        mode: Some(state::Mode::Proxy),
        attached: true,
        // Empty route model set allows us to test the legacy default route separately.
        proxy_route: Some("a".into()),
        stack,
        ..Default::default()
    };
    let oauth_home = tempfile::tempdir().unwrap();
    let app = AppState {
        proxy_service: crate::services::ProxyService::new(db.clone()),
        db,
        usage_cache: Arc::new(crate::services::UsageCache::new()),
        codex_oauth_manager: Arc::new(
            crate::proxy::providers::codex_oauth_auth::CodexOAuthManager::new(
                oauth_home.path().to_owned(),
            ),
        ),
    };
    (app, saved)
}

#[test]
fn descriptor_filters_catalog_sorts_and_uses_exact_bytes() {
    let config = config("ccs-a/gpt-a");
    let catalog = catalog();
    let doc = build(
        "/fixture/.codex",
        "proxy",
        config.as_bytes(),
        Some(&catalog),
        models(&["ccs-b/gpt-b", "ccs-a/gpt-a"]),
        100,
    )
    .unwrap();
    assert_eq!(
        doc["models"],
        json!([
            {"id":"ccs-a/gpt-a","title":"Model A"}, {"id":"ccs-b/gpt-b","title":"Model B"}
        ])
    );
    assert_eq!(doc["defaultModel"], "ccs-a/gpt-a");
    assert_eq!(doc["defaultReasoningEffort"], "high");
    assert_eq!(doc["expiresAtMs"], 30100);
    assert_eq!(doc["configSha256"], sha256_hex(config.as_bytes()));
    assert_eq!(doc["catalogSha256"], sha256_hex(&catalog));
    let mut stable = doc.clone();
    stable.as_object_mut().unwrap().remove("expiresAtMs");
    stable.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        doc["revision"],
        sha256_hex(&crate::config::sorted_json_bytes(&stable).unwrap())
    );
    let renewed = build(
        "/fixture/.codex",
        "proxy",
        config.as_bytes(),
        Some(&catalog),
        models(&["ccs-a/gpt-a", "ccs-b/gpt-b"]),
        1000,
    )
    .unwrap();
    assert_eq!(doc["revision"], renewed["revision"]);
    assert_ne!(doc["expiresAtMs"], renewed["expiresAtMs"]);
}

#[test]
fn no_catalog_empty_hash_missing_effort_null_and_live_top_level_default() {
    let config = b"model = 'gpt-live'\n[profiles.other]\nmodel = 'not-default'\nmodel_reasoning_effort = 'low'\n";
    let doc = build(
        "/fixture/.codex",
        "direct",
        config,
        None,
        models(&["gpt-live"]),
        0,
    )
    .unwrap();
    assert_eq!(doc["catalogSha256"], "");
    assert_eq!(doc["defaultReasoningEffort"], Value::Null);
    assert_eq!(doc["defaultModel"], "gpt-live");
}

#[test]
fn enabled_removed_deleted_members_filter_authoritative_ids() {
    let (state, mut saved) = state_with_stack();
    let initial = authorized_models(&state, &saved).unwrap();
    assert_eq!(
        initial.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["ccs-b/gpt-b", "gpt-a"]
    );
    // Historical keys and on-disk rows survive member removal; neither authorizes export.
    saved.stack.members.retain(|id| id != "b");
    let after = authorized_models(&state, &saved).unwrap();
    assert_eq!(
        after.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["gpt-a"]
    );
    saved.stack.members.push("b".into());
    saved.stack.enabled = false;
    assert_eq!(
        authorized_models(&state, &saved)
            .unwrap()
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-a"]
    );
    saved.stack.enabled = true;
    state.db.delete_provider("codex", "b").unwrap();
    assert_eq!(
        authorized_models(&state, &saved)
            .unwrap()
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-a"]
    );
}

#[test]
fn official_stack_models_only_use_published_native_rows() {
    let (state, mut saved) = state_with_stack();
    let mut official = row("official", "not-authoritative");
    official.category = Some("official".into());
    state.db.save_provider("codex", &official).unwrap();
    let key = stack::allocate_key(&mut saved.stack, &official);
    saved.stack.members.push(official.id.clone());
    saved
        .stack
        .codex_official_catalogs
        .insert(key.clone(), vec![json!({"slug":"gpt-official"})]);
    let ids = authorized_models(&state, &saved).unwrap();
    assert!(ids
        .iter()
        .any(|m| m.id == format!("ccs-{key}/gpt-official")));
    assert!(!ids.iter().any(|m| m.id.contains("not-authoritative")));
}

#[test]
#[serial_test::serial]
fn official_non_stack_defaults_are_not_native_model_authority() {
    let _home = IsolatedHome::new();
    let (app, mut saved) = state_with_stack();
    let mut official = row("official", "gpt-official");
    official.category = Some("official".into());
    app.db.save_provider("codex", &official).unwrap();
    app.db.set_current_provider("codex", "official").unwrap();
    crate::settings::set_current_provider(&AppType::Codex, Some("official")).unwrap();
    saved.mode = Some(state::Mode::Direct);
    saved.stack.enabled = false;
    assert_eq!(authorized_models(&app, &saved).err(), Some(UNAVAILABLE));
    saved.mode = Some(state::Mode::Proxy);
    saved.proxy_route = Some("official".into());
    assert_eq!(authorized_models(&app, &saved).err(), Some(UNAVAILABLE));
}

#[test]
fn provider_key_rotation_does_not_change_revision_or_leak_secrets() {
    let (state, saved) = state_with_stack();
    let secret_config = b"model = 'gpt-a'\n# CONFIG_SECRET_SENTINEL\n[model_providers.fixture]\nbase_url = 'https://ENDPOINT_SENTINEL.invalid'\nexperimental_bearer_token = 'BEARER_SECRET_SENTINEL'\n";
    let before = build(
        "/fixture/.codex",
        "proxy",
        secret_config,
        None,
        authorized_models(&state, &saved).unwrap(),
        1,
    )
    .unwrap();
    let mut changed = row("b", "gpt-b");
    changed.settings_config["auth"]["OPENAI_API_KEY"] = json!("ROTATED_SECRET_SENTINEL");
    state.db.save_provider("codex", &changed).unwrap();
    let after = build(
        "/fixture/.codex",
        "proxy",
        secret_config,
        None,
        authorized_models(&state, &saved).unwrap(),
        2,
    )
    .unwrap();
    assert_eq!(before["revision"], after["revision"]);
    let serialized = serde_json::to_string(&after).unwrap();
    for sentinel in [
        "CONFIG_SECRET_SENTINEL",
        "PROVIDER_SECRET_SENTINEL",
        "ROTATED_SECRET_SENTINEL",
        "ENDPOINT_SENTINEL",
        "BEARER_SECRET_SENTINEL",
        "settings_config",
        "base_url",
        "auth",
    ] {
        assert!(!serialized.contains(sentinel));
    }
    let filtered = build(
        "/fixture/.codex",
        "proxy",
        config("ccs-a/gpt-a").as_bytes(),
        Some(&catalog()),
        models(&["ccs-a/gpt-a"]),
        1,
    )
    .unwrap();
    assert!(!serde_json::to_string(&filtered)
        .unwrap()
        .contains("CATALOG_SECRET_SENTINEL"));
}

fn fixture() -> BindingFixture {
    BindingFixture {
        codex_home: "/fixture/.codex".into(),
        connection_mode: "proxy".into(),
        pending: false,
        verified: true,
        proxy_running: true,
        config: "model = 'gpt-a'".into(),
        catalog: None,
        models: models(&["gpt-a"]),
    }
}

#[test]
fn pending_unverified_and_stopped_proxy_fail_closed_with_fixed_errors() {
    for (pending, verified, running, expected) in [
        (true, true, true, "connection_pending"),
        (false, false, true, UNAVAILABLE),
        (false, true, false, "proxy_unavailable"),
    ] {
        let mut input = fixture();
        input.pending = pending;
        input.verified = verified;
        input.proxy_running = running;
        let doc = build_fixture(input);
        assert_eq!(doc["available"], false);
        assert_eq!(doc["errorCode"], expected);
        assert_eq!(doc.as_object().unwrap().len(), 5);
    }
}

#[test]
fn default_missing_from_live_or_effective_catalog_is_unavailable() {
    for (config, catalog, ids) in [
        ("model = 'missing'".into(), None, models(&["gpt-a"])),
        ("# no default".into(), None, models(&["gpt-a"])),
        (
            config("ccs-a/gpt-a"),
            Some(String::from_utf8(catalog()).unwrap()),
            models(&["ccs-b/gpt-b"]),
        ),
    ] {
        let mut input = fixture();
        input.config = config;
        input.catalog = catalog;
        input.models = ids;
        let doc = build_fixture(input);
        assert_eq!(doc["available"], false);
        assert_eq!(doc["errorCode"], "default_model_missing");
    }
}

#[test]
fn malformed_input_returns_only_fixed_error() {
    let mut input = fixture();
    input.config = "INVALID_SECRET_SENTINEL = [".into();
    let doc = build_fixture(input);
    assert_eq!(doc["errorCode"], "config_invalid");
    assert!(!serde_json::to_string(&doc).unwrap().contains("SENTINEL"));
    assert_eq!(doc.as_object().unwrap().len(), 5);
}

#[test]
fn byte_changes_alter_hash_and_revision_but_refresh_does_not() {
    let first = build("/fixture", "direct", b"model='a'", None, models(&["a"]), 1).unwrap();
    let changed = build(
        "/fixture",
        "direct",
        b"model='a'\n",
        None,
        models(&["a"]),
        1,
    )
    .unwrap();
    assert_ne!(first["configSha256"], changed["configSha256"]);
    assert_ne!(first["revision"], changed["revision"]);
    let mut cat = catalog();
    let original = cat.clone();
    cat.push(b'\n');
    let a = build(
        "/fixture",
        "proxy",
        config("ccs-a/gpt-a").as_bytes(),
        Some(&original),
        models(&["ccs-a/gpt-a"]),
        1,
    )
    .unwrap();
    let b = build(
        "/fixture",
        "proxy",
        config("ccs-a/gpt-a").as_bytes(),
        Some(&cat),
        models(&["ccs-a/gpt-a"]),
        1,
    )
    .unwrap();
    assert_ne!(a["catalogSha256"], b["catalogSha256"]);
    assert_ne!(a["revision"], b["revision"]);
}

#[test]
fn contract_limits_and_title_fallback() {
    let doc = build(
        "/fixture",
        "proxy",
        config("ccs-a/gpt-a").as_bytes(),
        Some(br#"{"models":[{"slug":"ccs-a/gpt-a","display_name":""}]}"#),
        models(&["ccs-a/gpt-a"]),
        1,
    )
    .unwrap();
    assert_eq!(doc["models"][0]["title"], "ccs-a/gpt-a");
    let long = "a".repeat(257);
    assert!(build(
        "/fixture",
        "direct",
        format!("model={long:?}").as_bytes(),
        None,
        models(&[&long]),
        1
    )
    .is_err());
    let entries = (0..2049)
        .map(|n| BindingModel {
            id: format!("m{n}"),
            title: format!("m{n}"),
        })
        .collect();
    assert_eq!(
        build("/fixture", "direct", b"model='m0'", None, entries, 1).unwrap_err(),
        "models_invalid"
    );
}

#[cfg(unix)]
#[test]
fn atomic_file_is_0600_directory_0700_and_rejects_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let directory = root.join("anywhere");
    write_binding(&directory, b"first").unwrap();
    let path = directory.join("codex-binding.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // Existing reader sees the old inode; new readers see the replacement.
    use std::io::Read;
    let mut old = fs::File::open(&path).unwrap();
    write_binding(&directory, b"second").unwrap();
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"first");
    assert_eq!(fs::read(&path).unwrap(), b"second");
    let victim = root.join("victim");
    fs::write(&victim, b"untouched").unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&victim, &path).unwrap();
    assert_eq!(
        write_binding(&directory, b"bad"),
        Err("binding_unsafe_path")
    );
    assert_eq!(fs::read(&victim).unwrap(), b"untouched");
    fs::remove_file(&path).unwrap();
    symlink(root.join("missing"), &path).unwrap();
    assert!(write_binding(&directory, b"bad").is_err());
    let linked = root.join("linked");
    symlink(&directory, &linked).unwrap();
    assert!(write_binding(&linked, b"bad").is_err());
    assert!(write_binding(&linked.join("nested"), b"bad").is_err());
    assert!(!directory.join("nested").exists());
}

#[test]
fn five_published_members_intersect_155_historical_catalog_rows() {
    let (app, mut saved) = state_with_stack();
    for index in 2..5 {
        let provider = row(&format!("p{index}"), &format!("gpt-{index}"));
        app.db.save_provider("codex", &provider).unwrap();
        stack::allocate_key(&mut saved.stack, &provider);
        saved.stack.members.push(provider.id);
    }
    saved.stack.managed_identity_required = true;
    let authorized = authorized_models(&app, &saved).unwrap();
    assert_eq!(authorized.len(), 5);
    let mut rows: Vec<_> = authorized
        .iter()
        .map(|model| json!({"slug":model.id}))
        .collect();
    for index in 5..155 {
        rows.push(json!({"slug":format!("ccs-historical/gpt-{index}")}));
    }
    let catalog = serde_json::to_vec(&json!({"models":rows})).unwrap();
    let before = build(
        "/fixture",
        "proxy",
        config("ccs-a/gpt-a").as_bytes(),
        Some(&catalog),
        authorized,
        1,
    )
    .unwrap();
    assert_eq!(before["models"].as_array().unwrap().len(), 5);
    saved.stack.members.retain(|id| id != "b");
    let after = build(
        "/fixture",
        "proxy",
        config("ccs-a/gpt-a").as_bytes(),
        Some(&catalog),
        authorized_models(&app, &saved).unwrap(),
        2,
    )
    .unwrap();
    assert_eq!(after["models"].as_array().unwrap().len(), 4);
    assert_ne!(before["revision"], after["revision"]);
    assert_eq!(before["catalogSha256"], after["catalogSha256"]);
}

struct IsolatedHome {
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}
impl IsolatedHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let previous = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", path);
        crate::settings::reload_settings().unwrap();
        Self {
            _dir: dir,
            previous,
        }
    }
}
impl Drop for IsolatedHome {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var("CC_SWITCH_TEST_HOME", previous);
        } else {
            std::env::remove_var("CC_SWITCH_TEST_HOME");
        }
        crate::settings::reload_settings().unwrap();
    }
}

#[tokio::test]
#[serial_test::serial]
async fn publisher_is_read_only_and_invalidates_pending_proxy_and_corrupt_state() {
    let _home = IsolatedHome::new();
    let (app, _) = state_with_stack();
    app.db.set_current_provider("codex", "a").unwrap();
    crate::settings::set_current_provider(&AppType::Codex, Some("a")).unwrap();
    let root = crate::codex_config::get_codex_config_dir();
    fs::create_dir_all(&root).unwrap();
    let config = b"model = \"gpt-a\"\n";
    let auth = br#"{"OPENAI_API_KEY":"PROVIDER_SECRET_SENTINEL"}"#;
    fs::write(root.join("config.toml"), config).unwrap();
    fs::write(root.join("auth.json"), auth).unwrap();
    let store = DeviceStore::for_device();
    let path = store.file("anywhere/codex-binding.json");
    publish(&app).await.unwrap();
    let read = || serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(read()["available"], true);
    assert_eq!(read()["connectionMode"], "direct");
    assert_eq!(
        read()["codexHome"],
        root.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(read()["defaultReasoningEffort"], Value::Null);
    state::set_pending(
        &store,
        "codex",
        Some(state::Pending {
            op: "fixture".into(),
            files: vec![],
            target: Default::default(),
            published: false,
        }),
    )
    .unwrap();
    let journal = fs::read(store.state_path()).unwrap();
    publish(&app).await.unwrap();
    assert_eq!(read()["available"], false);
    assert_eq!(read()["errorCode"], "connection_pending");
    assert_eq!(fs::read(store.state_path()).unwrap(), journal);
    state::update(&store, |live| {
        let saved = live.apps.get_mut("codex").unwrap();
        saved.pending = None;
        saved.mode = Some(state::Mode::Proxy);
        saved.attached = true;
        saved.proxy_route = Some("a".into());
    })
    .unwrap();
    publish(&app).await.unwrap();
    assert_eq!(read()["available"], false);
    // Live is unverified as well as the proxy being stopped. Both fail closed.
    fs::write(store.state_path(), b"CORRUPT_STATE_SECRET_SENTINEL").unwrap();
    publish(&app).await.unwrap();
    assert_eq!(read()["available"], false);
    assert_eq!(read()["errorCode"], UNAVAILABLE);
    assert_eq!(read().as_object().unwrap().len(), 5);
    assert_eq!(
        fs::read(store.state_path()).unwrap(),
        b"CORRUPT_STATE_SECRET_SENTINEL"
    );
    assert_eq!(fs::read(root.join("config.toml")).unwrap(), config);
    assert_eq!(fs::read(root.join("auth.json")).unwrap(), auth);
    assert!(!fs::read_dir(store.file("")).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("corrupt-")));
    assert!(!serde_json::to_string(&read()).unwrap().contains("SENTINEL"));
    // Exporting a stale pointer must not invoke the normal settings repair.
    fs::remove_file(store.state_path()).unwrap();
    crate::settings::set_current_provider(&AppType::Codex, Some("missing-provider")).unwrap();
    publish(&app).await.unwrap();
    assert_eq!(read()["available"], false);
    assert_eq!(
        crate::settings::get_settings()
            .current_provider_codex
            .as_deref(),
        Some("missing-provider")
    );
}

#[test]
#[serial_test::serial]
fn proxy_refresh_and_provider_key_rotation_do_not_invalidate_sampling() {
    let _home = IsolatedHome::new();
    let (app, saved) = state_with_stack();
    let store = DeviceStore::for_device();
    state::update(&store, |live| {
        live.apps.insert("codex".into(), saved);
    })
    .unwrap();
    let root = crate::codex_config::get_codex_config_dir();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("config.toml"), b"model = 'gpt-a'").unwrap();
    fs::write(
        root.join("auth.json"),
        br#"{"tokens":{"refresh_token":"OLD_SYNTHETIC"}}"#,
    )
    .unwrap();
    let before = sample_stamp(&app).unwrap();
    let mut changed = row("b", "gpt-b");
    changed.settings_config["auth"]["OPENAI_API_KEY"] = json!("NEW_SYNTHETIC");
    app.db.save_provider("codex", &changed).unwrap();
    fs::write(
        root.join("auth.json"),
        br#"{"tokens":{"refresh_token":"NEW_SYNTHETIC"}}"#,
    )
    .unwrap();
    assert_eq!(before, sample_stamp(&app).unwrap());
}
