use super::*;

fn fixture() -> (tempfile::TempDir, ToolBackups) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("isolated-home");
    fs::create_dir_all(&home).unwrap();
    let paths = vec![home.join("config.toml"), home.join("catalog.json")];
    fs::write(&paths[0], b"fixture-secret").unwrap();
    fs::write(&paths[1], b"old catalog").unwrap();
    let s = ToolBackups::new(DeviceStore::at(home.join(".cc-switch")), Tool::Codex, paths);
    (tmp, s)
}

#[test]
fn metadata_excludes_secrets_and_private_snapshot_roundtrips() {
    let (_tmp, s) = fixture();
    let snap = s.create().unwrap();
    let encoded = serde_json::to_string(&s.list().unwrap()).unwrap();
    assert!(!encoded.contains("fixture-secret"));
    assert_eq!(
        s.payload(&snap).unwrap()[0].as_deref(),
        Some(b"fixture-secret".as_slice())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(s.dir(&snap.id).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(s.dir(&snap.id).unwrap().join("0.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    fs::write(&s.paths[0], b"current").unwrap();
    let preview = s.preview(&snap.id).unwrap();
    let report = s.restore(&preview.preview_id, true).unwrap();
    assert_eq!(report.status, RestoreStatus::Restored);
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"fixture-secret");
    let safety = s
        .load(report.safety_snapshot_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        s.payload(&safety).unwrap()[0].as_deref(),
        Some(b"current".as_slice())
    );
}

#[cfg(windows)]
#[test]
fn windows_snapshot_acl_and_hardlink_protection() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, *},
    };
    let (_tmp, s) = fixture();
    let snapshot = s.before_write().unwrap();
    let payload = s.dir(&snapshot.id).unwrap().join("0.bin");
    windows::verify_path(&s.root).unwrap();
    windows::verify_path(&payload).unwrap();
    assert_eq!(s.before_write().unwrap().id, snapshot.id);
    let alias = s.root.join("alias.bin");
    fs::hard_link(&payload, &alias).unwrap();
    assert!(matches!(s.payload(&snapshot), Err(BackupError::UnsafePath)));
    fs::remove_file(alias).unwrap();
    // Deliberately broaden the saved payload ACL; no further private read or
    // automatic reuse is allowed, even though its bytes and hash are unchanged.
    let sddl: Vec<u16> = "D:P(A;;FA;;;WD)\0".encode_utf16().collect();
    let wide: Vec<u16> = payload.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut descriptor = std::ptr::null_mut();
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut()
            ),
            0
        );
        let ok = SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        assert_ne!(ok, 0);
    }
    assert!(matches!(s.payload(&snapshot), Err(BackupError::UnsafePath)));
    assert!(matches!(s.before_write(), Err(BackupError::UnsafePath)));
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"fixture-secret");
}

#[cfg(windows)]
#[test]
fn windows_public_backup_directory_is_refused() {
    let (_tmp, s) = fixture();
    fs::create_dir_all(&s.root).unwrap();
    // The inherited user-profile ACL includes administrators; it must never be
    // accepted as a new private snapshot directory without explicit protection.
    assert!(matches!(s.before_write(), Err(BackupError::UnsafePath)));
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"fixture-secret");
}

#[test]
fn preview_hash_change_refuses_restore_without_writing() {
    let (_tmp, s) = fixture();
    let snap = s.create().unwrap();
    let preview = s.preview(&snap.id).unwrap();
    fs::write(&s.paths[0], b"external update").unwrap();
    assert!(matches!(
        s.restore(&preview.preview_id, true),
        Err(BackupError::Changed)
    ));
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"external update");
    assert!(!s.list().unwrap().pending_recovery);
}

#[test]
fn failure_after_first_write_rolls_back_and_can_retry_with_fresh_preview() {
    let (_tmp, s) = fixture();
    let snap = s.create().unwrap();
    for path in &s.paths {
        fs::write(path, b"current").unwrap();
    }
    let preview = s.preview(&snap.id).unwrap();
    let report = s
        .restore_inner(&preview.preview_id, true, &mut |i, _| {
            if i == 1 {
                Err(BackupError::Storage)
            } else {
                Ok(())
            }
        })
        .unwrap();
    assert_eq!(report.status, RestoreStatus::RolledBack);
    for path in &s.paths {
        assert_eq!(fs::read(path).unwrap(), b"current");
    }
    assert!(!s.list().unwrap().pending_recovery);
    let preview = s.preview(&snap.id).unwrap();
    assert_eq!(
        s.restore(&preview.preview_id, true).unwrap().status,
        RestoreStatus::Restored
    );
}

#[test]
fn mid_restore_external_edit_survives_rollback_and_leaves_durable_recovery() {
    let (_tmp, s) = fixture();
    let snap = s.create().unwrap();
    for path in &s.paths {
        fs::write(path, b"current").unwrap();
    }
    let preview = s.preview(&snap.id).unwrap();
    let report = s
        .restore_inner(&preview.preview_id, true, &mut |i, s| {
            if i == 1 {
                fs::write(&s.paths[1], b"external concurrent edit").unwrap();
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(report.status, RestoreStatus::RecoveryRequired);
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"current");
    assert_eq!(fs::read(&s.paths[1]).unwrap(), b"external concurrent edit");
    assert!(s.list().unwrap().pending_recovery);
    assert_eq!(s.recover().unwrap().status, RestoreStatus::RecoveryRequired);
    // Explicitly resolving the conflicting external edit permits safe recovery.
    fs::write(&s.paths[1], b"current").unwrap();
    assert_eq!(s.recover().unwrap().status, RestoreStatus::RolledBack);
}

#[test]
fn four_tools_only_and_tampered_manifest_cannot_escape_allowlist() {
    for tool in ["codex", "claude", "opencode", "gemini"] {
        assert!(serde_json::from_value::<Tool>(serde_json::json!(tool)).is_ok());
    }
    assert!(serde_json::from_value::<Tool>(serde_json::json!("pi")).is_err());
    let (_tmp, s) = fixture();
    let mut snap = s.create().unwrap();
    snap.files[0].path = s.root.join("auth.json").to_string_lossy().into_owned();
    json_write(&s.dir(&snap.id).unwrap().join("manifest.json"), &snap).unwrap();
    assert!(matches!(
        s.preview(&snap.id),
        Err(BackupError::InvalidSnapshot)
    ));
    assert!(s.preview("../outside").is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_and_hardlinks_are_rejected_before_capture() {
    let (_tmp, s) = fixture();
    let external = s.paths[0].parent().unwrap().join("external");
    fs::write(&external, b"not a config").unwrap();
    fs::remove_file(&s.paths[0]).unwrap();
    std::os::unix::fs::symlink(&external, &s.paths[0]).unwrap();
    assert!(matches!(s.create(), Err(BackupError::UnsafePath)));
    fs::remove_file(&s.paths[0]).unwrap();
    fs::hard_link(&external, &s.paths[0]).unwrap();
    assert!(matches!(s.create(), Err(BackupError::UnsafePath)));
}

#[test]
fn automatic_snapshots_deduplicate_at_capacity_and_refuse_new_content() {
    let (_tmp, s) = fixture();
    let original = s.before_write().unwrap();
    assert_eq!(s.before_write().unwrap().id, original.id);
    // Fill the existing capacity with distinct, private, restorable snapshots.
    for i in 1..MAX_SNAPSHOTS {
        fs::write(&s.paths[0], format!("fixture version {i}")).unwrap();
        s.before_write().unwrap();
    }
    assert_eq!(s.list().unwrap().snapshots.len(), MAX_SNAPSHOTS);
    fs::write(&s.paths[0], b"fixture-secret").unwrap();
    assert_eq!(s.before_write().unwrap().id, original.id);
    fs::write(&s.paths[0], b"new version at capacity").unwrap();
    assert!(matches!(s.before_write(), Err(BackupError::LimitReached)));
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"new version at capacity");
    // No rotation may silently remove a recovery source.
    assert_eq!(s.list().unwrap().snapshots.len(), MAX_SNAPSHOTS);
    assert!(s.payload(&original).is_ok());
}

#[test]
fn automatic_capture_allows_attached_but_not_pending_or_restore_writes() {
    let (_tmp, s) = fixture();
    private_dir(s.store.state_path().parent().unwrap()).unwrap();
    let attached = serde_json::json!({
        "apps": {"codex": {"mode": "proxy", "attached": true}}
    });
    json_write(&s.store.state_path(), &attached).unwrap();
    let snapshot = s.before_write().unwrap();
    assert!(matches!(s.create(), Err(BackupError::ToolBusy)));
    assert!(matches!(
        s.preview(&snapshot.id),
        Err(BackupError::ToolBusy)
    ));
    assert!(matches!(s.recover(), Err(BackupError::ToolBusy)));
    let mut pending = attached;
    pending["apps"]["codex"]["pending"] = serde_json::json!({
        "op": "apply", "files": []
    });
    json_write(&s.store.state_path(), &pending).unwrap();
    assert!(matches!(s.before_write(), Err(BackupError::ToolBusy)));
    remove(&s.store.state_path()).unwrap();
    private_write(&s.journal_path(), b"pending restore").unwrap();
    assert!(matches!(
        s.before_write(),
        Err(BackupError::PendingRecovery)
    ));
    assert_eq!(s.list().unwrap().snapshots.len(), 1);
}

#[test]
fn automatic_capture_reads_only_allowlist_and_revalidates_reused_payload() {
    let (_tmp, s) = fixture();
    let home = s.paths[0].parent().unwrap();
    // Directories in place of excluded files would fail read() if enumerated.
    // No directory traversal or auth/credential/session discovery is permitted.
    for excluded in [
        "auth.json",
        "oauth_creds.json",
        ".claude.json",
        "mcp",
        "sessions",
    ] {
        fs::create_dir(home.join(excluded)).unwrap();
        fs::write(home.join(excluded).join("excluded-secret"), b"never copy").unwrap();
    }
    let snapshot = s.before_write().unwrap();
    assert_eq!(snapshot.files, s.infos(&s.capture().unwrap()));
    assert_eq!(
        fs::read_dir(s.dir(&snapshot.id).unwrap()).unwrap().count(),
        3
    );
    assert_eq!(s.before_write().unwrap().id, snapshot.id);
    private_write(&s.dir(&snapshot.id).unwrap().join("0.bin"), b"tampered").unwrap();
    assert!(matches!(
        s.before_write(),
        Err(BackupError::InvalidSnapshot)
    ));
    assert_eq!(s.list().unwrap().snapshots.len(), 1);
    assert_eq!(fs::read(&s.paths[0]).unwrap(), b"fixture-secret");
}

struct IsolatedProviderHome {
    dir: tempfile::TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_settings: crate::settings::AppSettings,
}
impl IsolatedProviderHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let canonical_home = dir.path().canonicalize().unwrap();
        let previous_settings = crate::settings::get_settings();
        let previous_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", &canonical_home);
        let home = Self {
            dir,
            previous_home,
            previous_settings,
        };
        let config = home.dir.path().canonicalize().unwrap().join("claude");
        fs::create_dir_all(&config).unwrap();
        crate::settings::update_settings(crate::settings::AppSettings {
            claude_config_dir: Some(config.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .unwrap();
        home
    }
}
impl Drop for IsolatedProviderHome {
    fn drop(&mut self) {
        // Restore cached settings while writes still target the temporary home.
        let _ = crate::settings::update_settings(self.previous_settings.clone());
        match &self.previous_home {
            Some(home) => std::env::set_var("CC_SWITCH_TEST_HOME", home),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
    }
}

#[test]
#[serial_test::serial]
fn managed_provider_switch_update_and_key_replacement_refuse_before_live_or_pointer_write() {
    use crate::{
        app_config::AppType,
        services::{
            osir::{connection, key_vault::KeyMetadata, managed::test_support::Fixture},
            ProviderService,
        },
    };
    let _home = IsolatedProviderHome::new();
    let backups = ToolBackups::configured(Tool::Claude);
    let isolated_root = _home.dir.path().canonicalize().unwrap();
    assert!(backups.paths.iter().all(|p| p.starts_with(&isolated_root)));
    assert!(backups.root.starts_with(&isolated_root));
    let live_path = crate::config::get_claude_settings_path();
    fs::write(&live_path, br#"{"env":{"USER_FLAG":"keep"}}"#).unwrap();
    let original_live = fs::read(&live_path).unwrap();
    let grant = Fixture::new(
        &AppType::Claude,
        serde_json::json!({
            "env": {"ANTHROPIC_BASE_URL": "https://api.osirclaw.com", "ANTHROPIC_MODEL": "old"}
        }),
        "synthetic-backup-key",
    );
    let state = crate::store::AppState::new(std::sync::Arc::new(
        crate::database::Database::memory().unwrap(),
    ));
    // A file blocking the backup directory is a deterministic storage failure.
    private_dir(backups.root.parent().unwrap()).unwrap();
    private_write(&backups.root, b"blocked storage").unwrap();
    // Saving an inactive connection does not inspect or require backup storage.
    ProviderService::add_without_switching(&state, AppType::Claude, grant.provider.clone())
        .unwrap();
    let saved_before_switch = state
        .db
        .get_provider_by_id(&grant.provider.id, "claude")
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&live_path).unwrap(), original_live);
    let pointer = || {
        crate::mode::current::provider_for(
            &state.db,
            &AppType::Claude,
            crate::mode::current::Purpose::Direct,
        )
        .unwrap()
    };
    assert!(pointer().is_none());
    assert!(ProviderService::switch(&state, AppType::Claude, &grant.provider.id).is_err());
    assert_eq!(fs::read(&live_path).unwrap(), original_live);
    assert!(pointer().is_none());
    remove(&backups.root).unwrap();
    ProviderService::switch(&state, AppType::Claude, &grant.provider.id).unwrap();
    assert_eq!(pointer().as_deref(), Some(grant.provider.id.as_str()));
    let snapshots = backups.list().unwrap().snapshots;
    assert_eq!(snapshots.len(), 1);
    let snapshot = snapshots.into_iter().next().unwrap();
    assert_eq!(
        backups.payload(&snapshot).unwrap()[0].as_deref(),
        Some(original_live.as_slice())
    );
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("synthetic-backup-key"));
    let before_live = fs::read(&live_path).unwrap();
    // Prevent a later automatic capture; preserve the already-saved snapshot.
    private_write(&backups.journal_path(), b"pending recovery").unwrap();
    let mut edited = grant.provider.clone();
    edited.settings_config["env"]["ANTHROPIC_MODEL"] = serde_json::json!("new");
    assert!(ProviderService::update(&state, AppType::Claude, None, edited).is_err());
    let old = grant
        .provider
        .meta
        .as_ref()
        .unwrap()
        .osir_binding
        .as_ref()
        .unwrap();
    let mut binding = old.binding.clone();
    binding.key_id += 1;
    let stored = KeyMetadata {
        binding,
        reference: serde_json::from_value(
            serde_json::json!({"version": 1, "id": uuid::Uuid::new_v4().to_string()}),
        )
        .unwrap(),
        masked_key: "••••fixture".into(),
    };
    let request = connection::ReplaceConnectionKeyRequest {
        tool: "claude".into(),
        provider_id: grant.provider.id.clone(),
        key_id: stored.binding.key_id,
        name: grant.provider.name.clone(),
        model: "new".into(),
        models: vec![],
        generation: 1,
        expected_reference: old.reference.clone(),
    };
    assert!(ProviderService::replace_osir_key(
        &state,
        AppType::Claude,
        &grant.provider.id,
        &old.reference,
        |_| connection::TrustedKeyReplacement::new(request, &stored)
            .map_err(crate::error::AppError::InvalidInput),
        |result| result,
    )
    .is_err());
    let saved = state
        .db
        .get_provider_by_id(&grant.provider.id, "claude")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        serde_json::to_value(saved_before_switch).unwrap()
    );
    assert_eq!(fs::read(&live_path).unwrap(), before_live);
    assert_eq!(pointer().as_deref(), Some(grant.provider.id.as_str()));
}
