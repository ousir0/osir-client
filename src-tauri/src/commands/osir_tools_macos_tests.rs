use super::*;
use serde_json::json;
use std::os::unix::fs::symlink;

#[test]
fn cooperative_activation_yields_before_request_and_propagates_system_rejection() {
    let calls = std::cell::RefCell::new(Vec::new());
    for accepted in [true, false] {
        calls.borrow_mut().clear();
        let result = request_application_activation(
            true,
            || calls.borrow_mut().push("yield to exact target"),
            || {
                calls.borrow_mut().push("activate from current app");
                accepted
            },
            || panic!("cooperative-capable systems must not use the legacy request"),
        );
        assert_eq!(
            *calls.borrow(),
            ["yield to exact target", "activate from current app"]
        );
        if accepted {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err(), ACTIVATION_DENIED);
        }
    }
    calls.borrow_mut().clear();
    request_application_activation(
        false,
        || panic!("unavailable yield selector must never be sent"),
        || panic!("unavailable cooperative selector must never be sent"),
        || {
            calls.borrow_mut().push("legacy activate all windows");
            true
        },
    )
    .unwrap();
    assert_eq!(*calls.borrow(), ["legacy activate all windows"]);
}

#[tokio::test]
async fn main_thread_timeout_cancels_queued_side_effects() {
    let mut executed = 0;
    let (tx, rx) = tokio::sync::oneshot::channel();
    assert!(tokio::time::timeout(Duration::from_millis(1), rx)
        .await
        .is_err());
    run_queued_main_action(tx, || {
        executed += 1;
        Ok(())
    });
    assert_eq!(
        executed, 0,
        "timed-out queued activation/termination must not run"
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    run_queued_main_action(tx, || {
        executed += 1;
        Ok("completed")
    });
    assert_eq!(rx.await.unwrap().unwrap(), "completed");
    assert_eq!(executed, 1, "a live caller still runs exactly once");
}

fn bundle(root: &Path, name: &str, bundle_id: &str) -> PathBuf {
    let app = root.join(name);
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    let binary = app.join("Contents/MacOS/Electron");
    // Deliberately not a runnable app: inventory must only inspect its metadata.
    std::fs::write(&binary, b"fixture only; never execute").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict>
        <key>CFBundleIdentifier</key><string>{bundle_id}</string>
        <key>CFBundleExecutable</key><string>Electron</string>
        <key>CFBundleShortVersionString</key><string>5.5.6</string>
        <key>CFBundleVersion</key><string>556</string>
        </dict></plist>"#
    );
    std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
    app.canonicalize().unwrap()
}

#[test]
fn fixed_candidates_include_managed_codex_and_never_cli_binaries() {
    let root = PathBuf::from("/Applications");
    assert_eq!(
        candidate_paths("codex", &[root.clone()]),
        vec![root.join("Codex.app"), root.join("ChatGPT.app")]
    );
    assert_eq!(
        candidate_paths("workbuddy", &[root.clone()]),
        vec![
            root.join("WorkBuddy.app"),
            root.join("Tencent WorkBuddy.app")
        ]
    );
    let managed = PathBuf::from("/Users/fixture/Applications/OSIR Codex");
    assert!(candidate_paths("codex", &[managed.clone()]).contains(&managed.join("Codex.app")));
    assert_eq!(
        candidate_roots("codex", Some(Path::new("/Users/fixture"))),
        vec![
            managed,
            PathBuf::from("/Applications"),
            PathBuf::from("/Users/fixture/Applications"),
        ]
    );
    assert_eq!(
        candidate_roots("claude-desktop", Some(Path::new("/Users/fixture")))[0],
        PathBuf::from("/Applications")
    );
}

#[test]
fn codex_profile_running_bundle_remains_the_open_and_restart_target_after_candidate_changes() {
    let preferred = Path::new("/fixture/Applications/OSIR Codex/Codex.app");
    let running = Path::new("/Applications/ChatGPT.app");
    assert_eq!(
        codex_profile_bundle_choice(preferred, Some(running)),
        running
    );
    let pid = Some(4242);
    for restart in [false, true] {
        let bundle = codex_profile_bundle_choice(preferred, Some(running));
        let action = codex_profile_action(pid, true, restart).unwrap();
        assert_eq!(
            bundle, running,
            "changing install preference must not migrate a live profile"
        );
        assert_eq!(
            action,
            if restart {
                CodexProfileAction::Restart(4242)
            } else {
                CodexProfileAction::Activate(4242)
            }
        );
    }
    assert_eq!(codex_profile_bundle_choice(preferred, None), preferred);
}

#[tokio::test]
async fn codex_running_bundle_metadata_requires_its_exact_main_executable() {
    let root = tempfile::tempdir().unwrap();
    let managed = bundle(root.path(), "Codex.app", BUNDLE_ID);
    let running = bundle(root.path(), "ChatGPT.app", BUNDLE_ID);
    let expected = checked_codex_executable(&running, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(expected, running.join("Contents/MacOS/Electron"));
    assert_ne!(
        expected,
        checked_codex_executable(&managed, Duration::from_secs(1))
            .await
            .unwrap()
    );
    let wrong = bundle(root.path(), "Wrong.app", "com.openai.chat");
    assert!(checked_codex_executable(&wrong, Duration::from_secs(1))
        .await
        .is_err());
}

#[test]
fn codex_running_profile_is_activated_and_restart_requires_its_exact_owned_pid() {
    assert_eq!(
        codex_profile_action(Some(4242), true, false).unwrap(),
        CodexProfileAction::Activate(4242),
        "reopening a proven running profile must not spawn another app"
    );
    assert_eq!(
        codex_profile_action(Some(4242), true, true).unwrap(),
        CodexProfileAction::Restart(4242)
    );
    assert_eq!(
        codex_profile_action(None, false, true).unwrap(),
        CodexProfileAction::Start,
        "restart of a stopped profile is one initial launch"
    );
    for restart in [false, true] {
        assert!(codex_profile_action(Some(4242), false, restart).is_err());
        assert!(codex_profile_action(Some(1), true, restart).is_err());
    }
}

#[test]
fn codex_launch_entries_reject_overlapping_operations() {
    let guard = codex_launch_guard().unwrap();
    assert!(codex_launch_guard().is_err());
    assert!(
        desktop_action_guard("codex").is_err(),
        "close/restart shares the same launch gate"
    );
    let claude = desktop_action_guard("claude-desktop").unwrap();
    let workbuddy = desktop_action_guard("workbuddy").unwrap();
    assert!(desktop_action_guard("claude-desktop").is_err());
    assert!(desktop_action_guard("workbuddy").is_err());
    assert!(
        desktop_action_guard("claude").is_err(),
        "CLI never gains desktop close controls"
    );
    drop((claude, workbuddy));
    drop(guard);
    assert!(codex_launch_guard().is_ok());
}

#[test]
fn desktop_thread_links_bind_only_one_literal_id() {
    assert_eq!(
        desktop_session_url("fixture-123").unwrap(),
        "codex://threads/fixture-123"
    );
    for id in [
        "",
        "../other",
        "a?thread=other",
        "a#other",
        "a\nother",
        "--last",
    ] {
        assert!(desktop_session_url(id).is_err());
    }
}

#[test]
fn desktop_close_rejects_recycled_pid_changed_executable_or_another_instance() {
    let birth = (123, 456);
    assert!(validate_desktop_instance_identity(4242, birth, 4242, Some(birth), true).is_ok());
    assert!(validate_desktop_instance_identity(4242, birth, 71780, Some(birth), true).is_err());
    assert!(validate_desktop_instance_identity(4242, birth, 4242, Some((123, 457)), true).is_err());
    assert!(validate_desktop_instance_identity(4242, birth, 4242, None, true).is_err());
    assert!(validate_desktop_instance_identity(4242, birth, 4242, Some(birth), false).is_err());
    assert!(validate_desktop_instance_identity(1, birth, 1, Some(birth), true).is_err());
}

#[test]
fn codex_pending_timeout_retries_reuse_the_owned_process_until_it_exits() {
    let key = CodexProfileKey {
        bundle: PathBuf::from("/fixture/Codex.app"),
        user_data_dir: PathBuf::from("/fixture/.codex/osir-desktop-profile"),
    };
    let mut pending = CodexPendingSnapshot {
        key: key.clone(),
        pid: 4242,
        state: CodexOwnedState::Running,
    };
    for _ in 0..10 {
        let owned_pid = codex_pending_pid(&key, Some(&pending)).unwrap();
        assert_eq!(
            codex_profile_action(owned_pid, true, false).unwrap(),
            CodexProfileAction::Activate(4242),
            "even without SingletonLock, retry after a timeout must not spawn"
        );
    }
    let mut other_profile = key.clone();
    other_profile.user_data_dir = PathBuf::from("/fixture/other-profile");
    assert_eq!(
        codex_pending_pid(&other_profile, Some(&pending)).unwrap(),
        None
    );
    let mut other_bundle = key.clone();
    other_bundle.bundle = PathBuf::from("/fixture/Other Codex.app");
    assert_eq!(
        codex_pending_pid(&other_bundle, Some(&pending)).unwrap(),
        None
    );
    pending.state = CodexOwnedState::Changed;
    assert!(codex_pending_pid(&key, Some(&pending)).is_err());
    pending.state = CodexOwnedState::Exited;
    let owned_pid = codex_pending_pid(&key, Some(&pending)).unwrap();
    assert_eq!(
        codex_profile_action(owned_pid, false, false).unwrap(),
        CodexProfileAction::Start,
        "a completed child receipt allows a fresh attempt"
    );
}

#[test]
fn codex_owned_receipt_checks_birth_and_child_exit_without_reading_process_arguments() {
    let (exit_tx, exited) = tokio::sync::watch::channel(false);
    let mut owned = CodexOwnedLaunch {
        key: CodexProfileKey {
            bundle: PathBuf::from("/fixture/Codex.app"),
            user_data_dir: PathBuf::from("/fixture/.codex/osir-desktop-profile"),
        },
        pid: std::process::id() as i32,
        birth: process_birth(std::process::id() as i32),
        executable: std::env::current_exe().unwrap().canonicalize().unwrap(),
        started: Instant::now(),
        exited,
    };
    assert_eq!(owned.snapshot().state, CodexOwnedState::Running);
    owned.birth = owned.birth.map(|(seconds, micros)| (seconds + 1, micros));
    assert_eq!(owned.snapshot().state, CodexOwnedState::Changed);
    exit_tx.send(true).unwrap();
    assert_eq!(owned.snapshot().state, CodexOwnedState::Exited);
}

#[test]
fn codex_dev_desktop_uses_the_same_codex_home_and_a_separate_electron_instance() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().to_path_buf();
    let codex_home = home.join(".codex");
    let profile =
        CodexDevProfile::new(home.clone(), codex_home.clone(), Some(home.clone())).unwrap();
    let executable = Path::new("/fixture/Codex.app/Contents/MacOS/Electron");
    let command = profile.command(executable);
    let command = command.as_std();
    let args = command
        .get_args()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(command.get_program(), executable);
    assert!(!args.contains(&"-n".into()));
    let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(
        env.get(std::ffi::OsStr::new("HOME")),
        Some(&Some(home.as_os_str()))
    );
    assert_eq!(
        env.get(std::ffi::OsStr::new("CODEX_HOME")),
        Some(&Some(codex_home.as_os_str()))
    );
    assert_eq!(
        env.get(std::ffi::OsStr::new("CODEX_ELECTRON_USER_DATA_PATH")),
        Some(&Some(profile.user_data_dir.as_os_str()))
    );
    assert!(args.contains(&format!(
        "--user-data-dir={}",
        profile.user_data_dir.display()
    )));
    assert!(
        !profile.user_data_dir.exists(),
        "constructing a launch plan does not write configuration"
    );
    assert_eq!(profile.running_pid(), None);
    assert!(CodexDevProfile::new(
        home.clone(),
        PathBuf::from("/outside/.codex"),
        Some(home.clone())
    )
    .is_err());
    assert!(CodexDevProfile::new(home.clone(), codex_home.clone(), None).is_err());
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), &codex_home).unwrap();
    assert!(CodexDevProfile::new(home.clone(), codex_home, Some(home)).is_err());
}

#[test]
fn controlled_restart_never_targets_the_current_process_or_system_pids() {
    assert!(validate_restart_target(std::process::id() as i32).is_err());
    assert!(validate_restart_target(1).is_err());
    assert!(validate_restart_target(-1).is_err());
    assert!(validate_restart_target(999999).is_ok());
    assert!(process_is_alive(std::process::id() as i32));
    assert!(!process_is_alive(1));
    assert!(!process_is_alive(-1));
}

#[tokio::test]
async fn missing_desktop_does_not_infer_installation_from_cli() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("codex"), b"fixture CLI").unwrap();
    let record = detect_candidates("codex", &candidate_paths("codex", &[root.path().into()])).await;
    assert!(!record.installed);
    assert_eq!(record.status, ToolStatus::NotInstalled);
}

#[tokio::test]
async fn workbuddy_metadata_is_real_local_plist_not_a_version_placeholder() {
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "WorkBuddy.app", WORKBUDDY_BUNDLE_ID);
    let record = detect_candidates("workbuddy", &[path.clone()]).await;
    assert!(record.installed);
    assert_eq!(record.status, ToolStatus::Installed);
    assert_eq!(record.version.as_deref(), Some("5.5.6"));
    assert_eq!(record.path.as_deref(), path.to_str());
}

#[tokio::test]
async fn renamed_codex_uses_existing_identity_and_classic_is_excluded() {
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "ChatGPT.app", "com.openai.chat");
    let record = detect_candidates("codex", &[path.clone()]).await;
    assert!(!record.installed);
    assert!(record.path.is_none());
    // Existing Codex Desktop discovery accepts com.openai.codex under this name.
    let path = bundle(root.path(), "ChatGPT.app", BUNDLE_ID);
    let record = detect_candidates("codex", &[path]).await;
    assert!(record.installed);
    assert_eq!(record.status, ToolStatus::Installed);
}

#[tokio::test]
async fn corrupt_bundle_is_unavailable_until_product_identity_can_be_read() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("WorkBuddy.app");
    std::fs::create_dir(&path).unwrap();
    let record = detect_candidates("workbuddy", &[path]).await;
    assert!(!record.installed);
    assert_eq!(record.status, ToolStatus::Unavailable);
    assert!(record.path.is_none());
    assert!(record.version.is_none());
}

#[tokio::test]
async fn identified_bundle_with_missing_executable_is_installed_but_broken() {
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "WorkBuddy.app", WORKBUDDY_BUNDLE_ID);
    std::fs::remove_file(path.join("Contents/MacOS/Electron")).unwrap();
    let record = detect_candidates("workbuddy", &[path]).await;
    assert!(record.installed);
    assert_eq!(record.status, ToolStatus::Error);
    assert!(record.path.is_none());
}

#[tokio::test]
async fn broken_candidate_does_not_hide_a_valid_installation() {
    let root = tempfile::tempdir().unwrap();
    let broken = root.path().join("WorkBuddy.app");
    std::fs::create_dir(&broken).unwrap();
    let valid = bundle(root.path(), "Tencent WorkBuddy.app", WORKBUDDY_BUNDLE_ID);
    let record = detect_candidates("workbuddy", &[broken, valid.clone()]).await;
    assert_eq!(record.status, ToolStatus::Installed);
    assert_eq!(record.path.as_deref(), valid.to_str());
}

#[tokio::test]
async fn wrong_workbuddy_identity_is_never_launchable() {
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "WorkBuddy.app", "org.unrelated.app");
    let record = detect_candidates("workbuddy", &[path]).await;
    assert!(!record.installed);
    assert!(record.path.is_none());
}

#[test]
fn executable_names_cannot_escape_bundle_or_inject_arguments() {
    for name in [
        "",
        ".",
        "..",
        "../Electron",
        "/usr/bin/open",
        "nested/Electron",
        "a\\b",
        "Electron\n",
    ] {
        assert!(!safe_executable_name(name), "{name:?}");
    }
    assert!(safe_executable_name("Electron"));
    assert!(safe_executable_name("Codex"));
}

#[test]
fn executable_symlink_outside_bundle_is_rejected_for_both_desktops() {
    for (id, identity) in [("workbuddy", WORKBUDDY_BUNDLE_ID), ("codex", BUNDLE_ID)] {
        let root = tempfile::tempdir().unwrap();
        let path = bundle(root.path(), "fixture.app", identity);
        let executable = path.join("Contents/MacOS/Electron");
        std::fs::remove_file(&executable).unwrap();
        symlink("/usr/bin/true", executable).unwrap();
        let result = installation_from_metadata(
            &path,
            id,
            &json!({
                "CFBundleIdentifier": identity, "CFBundleExecutable": "Electron"
            }),
        );
        assert!(result.is_err());
    }
}

#[test]
fn workbuddy_launch_profile_matches_native_model_directory_and_overrides_user_data() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().to_path_buf();
    let profile = WorkBuddyProfile::new(home.clone(), true, Some(home.clone())).unwrap();
    assert_eq!(
        profile.config_dir.join("models.json"),
        home.join(".workbuddy/models.json")
    );
    let command = profile.command(Path::new("/fixture/WorkBuddy.app/Contents/MacOS/Electron"));
    let command = command.as_std();
    let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(
        env.get(std::ffi::OsStr::new("HOME")),
        Some(&Some(home.as_os_str()))
    );
    assert_eq!(
        env.get(std::ffi::OsStr::new("WORKBUDDY_CONFIG_DIR")),
        Some(&Some(profile.config_dir.as_os_str()))
    );
    assert_eq!(
        env.get(std::ffi::OsStr::new("WORKBUDDY_USER_DATA_DIR")),
        Some(&Some(profile.user_data_dir.as_os_str()))
    );
    assert!(command.get_args().next().is_none());
    assert_eq!(command.get_current_dir(), Some(home.as_path()));
}

#[test]
fn workbuddy_isolated_launch_never_falls_back_to_formal_home_or_follows_profile_links() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().to_path_buf();
    assert!(WorkBuddyProfile::new(home.clone(), true, None).is_err());
    assert!(WorkBuddyProfile::new(home.clone(), true, Some(home.join("different"))).is_err());
    assert!(WorkBuddyProfile::new(PathBuf::from("relative"), false, None).is_err());
    let target = tempfile::tempdir().unwrap();
    symlink(target.path(), home.join(".workbuddy")).unwrap();
    assert!(WorkBuddyProfile::new(home.clone(), true, Some(home)).is_err());
}

#[test]
fn workbuddy_existing_profile_probe_only_checks_a_positive_live_pid() {
    let root = tempfile::tempdir().unwrap();
    let profile = WorkBuddyProfile::new(root.path().into(), false, None).unwrap();
    std::fs::create_dir_all(&profile.user_data_dir).unwrap();
    assert_eq!(profile.running_pid(), None);
    let lock = profile.user_data_dir.join("SingletonLock");
    symlink(format!("fixture-{}", std::process::id()), &lock).unwrap();
    assert_eq!(profile.running_pid(), Some(std::process::id() as i32));
    std::fs::remove_file(&lock).unwrap();
    symlink("fixture--1", &lock).unwrap();
    assert_eq!(profile.running_pid(), None);
}

#[tokio::test]
async fn claude_desktop_requires_its_own_bundle_identity_and_not_cli() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        candidate_paths("claude-desktop", &[root.path().to_path_buf()]),
        vec![root.path().join("Claude.app")]
    );
    assert!(candidate_paths("unknown", &[root.path().to_path_buf()]).is_empty());
    let path = bundle(root.path(), "Claude.app", CLAUDE_BUNDLE_ID);
    assert!(
        detect_candidates("claude-desktop", &[path.clone()])
            .await
            .installed
    );
    assert!(!detect_candidates("codex", &[path]).await.installed);
}

#[test]
fn interactive_cli_requires_a_live_foreground_tty() {
    assert!(foreground_cli_session(2, 123, 40, 40));
    assert!(!foreground_cli_session(5, 123, 40, 40));
    assert!(!foreground_cli_session(2, u32::MAX, 40, 40));
    assert!(!foreground_cli_session(2, 123, 40, 0));
    assert!(!foreground_cli_session(2, 123, 40, 80));
}

#[test]
fn scripts_and_ambiguous_processes_do_not_claim_running_or_stopped() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("gemini");
    std::fs::write(&script, b"#!/usr/bin/env node\n").unwrap();
    assert!(!native_macho(&script));
    assert_eq!(cli_process_status(&script), ToolRuntimeStatus::Unavailable);
    assert_eq!(
        cli_process_status(&root.path().join("missing")),
        ToolRuntimeStatus::Unavailable
    );
    assert!(!process_matches(std::process::id() as i32, &script));
    assert!(!process_matches(-1, &script));
    assert_eq!(desktop_identity("claude"), None);
    assert_eq!(desktop_identity("unknown"), None);
}

#[tokio::test]
async fn linked_apps_and_oversized_or_linked_plists_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "Original.app", WORKBUDDY_BUNDLE_ID);
    let linked = root.path().join("WorkBuddy.app");
    symlink(&path, &linked).unwrap();
    assert!(inspect_bundle(&linked, "workbuddy", Duration::from_secs(1))
        .await
        .is_err());
    let plist = path.join("Contents/Info.plist");
    std::fs::write(&plist, vec![b'x'; METADATA_LIMIT as usize + 1]).unwrap();
    assert!(inspect_bundle(&path, "workbuddy", Duration::from_secs(1))
        .await
        .is_err());
    std::fs::remove_file(&plist).unwrap();
    symlink("/etc/hosts", &plist).unwrap();
    assert!(inspect_bundle(&path, "workbuddy", Duration::from_secs(1))
        .await
        .is_err());
}

#[test]
fn unknown_version_remains_null_and_arbitrary_metadata_is_not_exposed() {
    let root = tempfile::tempdir().unwrap();
    let path = bundle(root.path(), "fixture.app", WORKBUDDY_BUNDLE_ID);
    let result = installation_from_metadata(
        &path,
        "workbuddy",
        &json!({
            "CFBundleIdentifier": WORKBUDDY_BUNDLE_ID, "CFBundleExecutable": "Electron",
            "CFBundleShortVersionString": "Authorization: secret-token", "CFBundleVersion": 556
        }),
    )
    .unwrap()
    .unwrap();
    assert!(result.version.is_none());
    assert_eq!(result.build.as_deref(), Some("556"));
}

#[tokio::test]
async fn native_subprocess_deadline_is_enforced_without_launching_any_tool() {
    let start = Instant::now();
    assert!(system_output(
        "/bin/sleep",
        &[],
        Path::new("10"),
        Duration::from_millis(25)
    )
    .await
    .is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}
