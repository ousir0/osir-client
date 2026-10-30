use super::*;

fn provider(settings: serde_json::Value) -> crate::provider::Provider {
    crate::provider::Provider::with_id("fixture".into(), "Fixture".into(), settings, None)
}

#[test]
fn fixed_catalog_maps_every_cli_without_accepting_arbitrary_commands() {
    for id in VALID_TOOLS {
        assert!(tool_app(id).is_ok(), "{id}");
    }
    assert_eq!(tool_app("grok").unwrap(), AppType::GrokBuild);
    for id in [
        "workbuddy",
        "claude-desktop",
        "/bin/sh",
        "codex;echo bad",
        "",
    ] {
        assert!(tool_app(id).is_err());
    }
}

#[test]
fn claude_uses_explicit_native_settings_and_private_environment_transport() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings O'Brien.json");
    let secret = "synthetic-'$(touch never)-`never`";
    std::fs::write(
        &settings,
        serde_json::to_vec(&serde_json::json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": secret,
                "ANTHROPIC_BASE_URL": "https://fixture.osirclaw.com",
                "ANTHROPIC_MODEL": "fixture-model"
            },
            "permissions": {"allow": ["Read"]}
        }))
        .unwrap(),
    )
    .unwrap();
    let before = std::fs::read(&settings).unwrap();
    let launch = claude_launch(root.path(), &settings).unwrap();
    assert_eq!(launch.args, ["--settings", settings.to_str().unwrap()]);
    assert_eq!(env_value(&launch, "ANTHROPIC_AUTH_TOKEN"), Some(secret));
    assert_eq!(env_value(&launch, "ANTHROPIC_MODEL"), Some("fixture-model"));
    assert!(launch.env.contains(&("ANTHROPIC_API_KEY", None)));
    assert!(launch.env.contains(&("CLAUDE_CODE_USE_VERTEX", None)));
    assert!(!unix_launch_command(&launch).contains(secret));
    assert!(unix_launch_env_file(&launch).contains("CC_SWITCH_LAUNCH_ENV_"));
    assert_eq!(std::fs::read(&settings).unwrap(), before);
}

#[test]
fn claude_default_layout_keeps_mcp_at_home_without_losing_isolation_or_settings() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let directory = home.join(".claude");
    let mut launch = projected_terminal_launch(&AppType::Claude, &directory, None).unwrap();
    launch.args = vec![
        "--settings".into(),
        directory.join("settings.json").display().to_string(),
    ];
    let args = launch.args.clone();
    preserve_claude_default_mcp_path(&mut launch, &directory, home);
    isolate_current_cli_launch(&mut launch, home).unwrap();
    assert!(launch.env.contains(&("CLAUDE_CONFIG_DIR", None)));
    assert_eq!(env_value(&launch, "HOME"), home.to_str());
    assert_eq!(
        env_value(&launch, "XDG_CONFIG_HOME"),
        home.join(".config").to_str()
    );
    assert_eq!(launch.args, args);

    let custom = home.join("custom-claude");
    let mut launch = projected_terminal_launch(&AppType::Claude, &custom, None).unwrap();
    preserve_claude_default_mcp_path(&mut launch, &custom, home);
    assert_eq!(env_value(&launch, "CLAUDE_CONFIG_DIR"), custom.to_str());
}

#[test]
fn production_launch_preserves_user_home_and_existing_native_state() {
    let root = tempfile::tempdir().unwrap();
    for id in ["claude", "codex", "opencode", "gemini"] {
        let mut launch = plain_launch(id);
        set_env(&mut launch, "HOME", Some("/fixture/user-home".into()));
        set_env(
            &mut launch,
            "XDG_DATA_HOME",
            Some("/fixture/user-data".into()),
        );
        let original = launch.env.clone();
        current_configuration_home(&mut launch, root.path(), false).unwrap();
        assert_eq!(
            launch.env, original,
            "production {id} must keep the existing user state"
        );
        current_configuration_home(&mut launch, root.path(), true).unwrap();
        assert_eq!(env_value(&launch, "HOME"), root.path().to_str());
        assert_eq!(
            env_value(&launch, "XDG_DATA_HOME"),
            root.path().join(".local/share").to_str()
        );
    }
}

#[test]
fn native_login_remains_launchable_while_incomplete_osir_projection_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings.json");
    let native = provider(serde_json::json!({"env": {}}));
    for contents in [None, Some("{}"), Some(r#"{"env":{}}"#)] {
        if let Some(contents) = contents {
            std::fs::write(&settings, contents).unwrap();
        }
        let launch = claude_launch(root.path(), &settings).unwrap();
        assert!(validate_osir_projection(&AppType::Claude, None, &launch, None).is_ok());
        assert!(validate_osir_projection(&AppType::Claude, Some(&native), &launch, None).is_ok());
        let osir = provider(
            serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://fixture.osirclaw.com"}}),
        );
        assert_eq!(
            validate_osir_projection(&AppType::Claude, Some(&osir), &launch, None),
            Err(CONFIG_MISSING.into())
        );
    }
}

#[test]
fn osir_endpoint_must_match_the_applied_direct_or_proxy_configuration() {
    let row = provider(
        serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://fixture.osirclaw.com/"}}),
    );
    let mut launch = plain_launch("claude");
    set_env(
        &mut launch,
        "ANTHROPIC_AUTH_TOKEN",
        Some("synthetic-fixture".into()),
    );
    set_env(
        &mut launch,
        "ANTHROPIC_BASE_URL",
        Some("https://fixture.osirclaw.com".into()),
    );
    assert!(validate_osir_projection(&AppType::Claude, Some(&row), &launch, None).is_ok());
    set_env(
        &mut launch,
        "ANTHROPIC_BASE_URL",
        Some("https://unrelated.example".into()),
    );
    assert_eq!(
        validate_osir_projection(&AppType::Claude, Some(&row), &launch, None),
        Err(CONFIG_CHANGED.into())
    );
    set_env(
        &mut launch,
        "ANTHROPIC_BASE_URL",
        Some("http://127.0.0.1:1234".into()),
    );
    assert!(validate_osir_projection(
        &AppType::Claude,
        Some(&row),
        &launch,
        Some("http://127.0.0.1:1234/")
    )
    .is_ok());
    set_env(&mut launch, "ANTHROPIC_AUTH_TOKEN", None);
    assert_eq!(
        validate_osir_projection(
            &AppType::Claude,
            Some(&row),
            &launch,
            Some("http://127.0.0.1:1234")
        ),
        Err(CONFIG_MISSING.into())
    );
}

#[test]
fn invalid_native_settings_are_rejected_with_a_fixed_redacted_error() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings.json");
    for value in ["[]", "{synthetic-secret", "null"] {
        std::fs::write(&settings, value).unwrap();
        assert_eq!(
            claude_launch(root.path(), &settings).err().as_deref(),
            Some(CONFIG_UNREADABLE)
        );
    }
}

#[test]
fn agy_official_login_does_not_need_or_create_a_private_profile() {
    let mut row = provider(serde_json::json!({
        "env": {"GEMINI_API_KEY":"synthetic-stale", "GOOGLE_GEMINI_BASE_URL":"https://stale.example"},
        "config": {"model": {"name": "Fixture Model"}}
    }));
    row.category = Some("official".into());
    let launch = agy_launch(Some(&row), None).unwrap();
    assert_eq!(launch.args, ["--model=Fixture Model"]);
    assert_eq!(env_value(&launch, "GEMINI_API_KEY"), None);
    assert_eq!(env_value(&launch, "GOOGLE_GEMINI_BASE_URL"), None);
    assert!(agy_launch(None, None).unwrap().args.is_empty());
}

#[test]
fn agy_key_mode_reuses_only_an_already_applied_matching_profile() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("existing-profile");
    let row = provider(serde_json::json!({
        "env": {"GEMINI_API_KEY":"synthetic-key", "GOOGLE_GEMINI_BASE_URL":"https://fixture.example"},
        "config": {"model": {"name":"Fixture Model"}}
    }));
    assert_eq!(
        agy_launch(Some(&row), Some(&profile)).err().as_deref(),
        Some(CONFIG_MISSING)
    );
    assert!(!profile.exists());
    let settings = profile.join("antigravity-cli/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"modelProvider":"gemini","model":"Fixture Model","preserved":true}"#,
    )
    .unwrap();
    let before = std::fs::read(&settings).unwrap();
    let launch = agy_launch(Some(&row), Some(&profile)).unwrap();
    assert_eq!(launch.args, [format!("--gemini_dir={}", profile.display())]);
    assert_eq!(env_value(&launch, "GEMINI_API_KEY"), Some("synthetic-key"));
    assert!(!unix_launch_command(&launch).contains("synthetic-key"));
    assert_eq!(std::fs::read(&settings).unwrap(), before);
    std::fs::write(
        &settings,
        r#"{"modelProvider":"gemini","model":"Changed Model"}"#,
    )
    .unwrap();
    assert_eq!(
        agy_launch(Some(&row), Some(&profile)).err().as_deref(),
        Some(CONFIG_CHANGED)
    );
}

#[cfg(unix)]
fn executable_fixture(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn direct_script_runs_node_shebang_in_selected_project_with_isolated_home_and_no_rc() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("项目 O'Brien $(touch should-not-exist)");
    let home = root.path().join("isolated-home");
    let bin = root.path().join("node bin");
    let terminal_home = root.path().join("terminal-home");
    for path in [&project, &home, &bin, &terminal_home] {
        std::fs::create_dir(path).unwrap();
    }
    for file in [".zshenv", ".zprofile", ".zshrc", ".bashrc", ".profile"] {
        std::fs::write(
            terminal_home.join(file),
            "compdef fixture_should_never_run\ntouch rc-ran\n",
        )
        .unwrap();
    }
    let settings = home.join("settings.json");
    let secret = "synthetic-'$(touch no-secret-expansion)-`no-command`";
    std::fs::write(
        &settings,
        serde_json::to_vec(&serde_json::json!({"env":{"ANTHROPIC_AUTH_TOKEN":secret}})).unwrap(),
    )
    .unwrap();
    // This fake interpreter proves /usr/bin/env node resolves the explicitly
    // injected PATH. No installed Node, real CLI or network request is used.
    executable_fixture(&bin.join("node"), "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$HOME\" \"$CLAUDE_CONFIG_DIR\" \"$ANTHROPIC_AUTH_TOKEN\" \"${CC_SWITCH_LAUNCH_ENV_0-unset}\" \"$@\" > \"$OSIR_FIXTURE_OUT\"\nexit 17\n");
    let source = root.path().join("fake-cli.js");
    executable_fixture(&source, "#!/usr/bin/env node\n// Synthetic entry only.\n");
    let entry = root.path().join("fixture O'Brien cli");
    symlink(&source, &entry).unwrap();
    let mut launch = claude_launch(&home, &settings).unwrap();
    launch.program = path_value(&launchable_entry(entry.clone()).unwrap()).unwrap();
    launch
        .args
        .push("literal ' $(touch no-argv-expansion) 中文".into());
    isolate_current_cli_launch(&mut launch, &home).unwrap();
    set_env(
        &mut launch,
        "PATH",
        Some(format!("{}:/usr/bin:/bin", bin.display())),
    );
    let output = root.path().join("output");
    set_env(
        &mut launch,
        "OSIR_FIXTURE_OUT",
        Some(output.to_str().unwrap().into()),
    );
    let additional_cleanup = root.path().join("cleanup-fixture");
    std::fs::write(&additional_cleanup, "fixture").unwrap();
    launch.cleanup.push(additional_cleanup.clone());
    let script_file = root.path().join("launch O'Brien.sh");
    let env_file = root.path().join("environment O'Brien.sh");
    let text = script(&launch, &project, &script_file, &env_file, None);
    assert!(text.starts_with("#!/bin/sh\numask 077\n"));
    assert!(!text.contains(secret));
    assert!(!text.contains(" -lic "));
    assert!(!text.contains(" -lc "));
    crate::config::atomic_write_private(&env_file, unix_launch_env_file(&launch).as_bytes())
        .unwrap();
    crate::config::atomic_write_private(&script_file, text.as_bytes()).unwrap();
    for path in [&env_file, &script_file] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let result = std::process::Command::new("/bin/sh")
        .arg(&script_file)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &terminal_home)
        .env("ANTHROPIC_AUTH_TOKEN", "wrong-inherited-token")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(17));
    let lines = std::fs::read_to_string(output)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            project.to_str().unwrap(),
            home.to_str().unwrap(),
            home.to_str().unwrap(),
            secret,
            "unset",
            entry.to_str().unwrap(),
            "--settings",
            settings.to_str().unwrap(),
            "literal ' $(touch no-argv-expansion) 中文"
        ]
    );
    for file in [&script_file, &env_file, &additional_cleanup] {
        assert!(!file.exists());
    }
    for file in [
        "rc-ran",
        "should-not-exist",
        "no-secret-expansion",
        "no-argv-expansion",
    ] {
        assert!(!root.path().join(file).exists());
        assert!(!project.join(file).exists());
    }
    assert!(settings.exists(), "native settings are retained");
}

#[cfg(unix)]
#[test]
fn disappearing_project_exits_without_launching_tool_and_cleans_temporary_files() {
    let root = tempfile::tempdir().unwrap();
    let launch = plain_launch("/usr/bin/false");
    let script_file = root.path().join("launch.sh");
    let env_file = root.path().join("env.sh");
    crate::config::atomic_write_private(&env_file, b"").unwrap();
    crate::config::atomic_write_private(
        &script_file,
        script(
            &launch,
            &root.path().join("missing"),
            &script_file,
            &env_file,
            None,
        )
        .as_bytes(),
    )
    .unwrap();
    let result = std::process::Command::new("/bin/sh")
        .arg(&script_file)
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(!script_file.exists());
    assert!(!env_file.exists());
}

#[test]
fn receipt_accepts_only_complete_bounded_state_records() {
    for text in [
        "",
        "started 123",
        "started 1\n",
        "exited 123 999\n",
        "started 123 credential\n",
        "exited 123 -1\n",
    ] {
        assert_eq!(receipt_state(text), CliLaunchState::Pending);
    }
    assert_eq!(
        receipt_state("started 123\nexited 123 "),
        CliLaunchState::Started { pid: 123 }
    );
    assert_eq!(
        receipt_state("started 123\nexited 123 17\n"),
        CliLaunchState::Exited { pid: 123, code: 17 }
    );
}

#[tokio::test]
async fn unconsumed_launch_environment_script_and_receipt_expire_together() {
    let root = tempfile::tempdir().unwrap();
    let env = root.path().join("environment.sh");
    let script = root.path().join("launcher.sh");
    for path in [&env, &script] {
        crate::config::atomic_write_private(path, b"synthetic fixture").unwrap();
    }
    let receipt = CliLaunchReceipt::new().unwrap();
    receipt.expire_after(
        vec![env.clone(), script.clone()],
        std::time::Duration::from_millis(10),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    while [&env, &script, &receipt.path]
        .iter()
        .any(|path| path.exists())
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!([&env, &script, &receipt.path]
        .iter()
        .all(|path| !path.exists()));
}

#[cfg(unix)]
#[test]
fn receipt_reports_this_request_exit_without_collecting_tool_output() {
    let root = tempfile::tempdir().unwrap();
    for code in [0, 17, 127] {
        let entry = root.path().join("fixture-cli");
        executable_fixture(&entry, &format!("#!/bin/sh\nexit {code}\n"));
        let mut launch = plain_launch("fixture");
        launch.program = entry.to_str().unwrap().into();
        let receipt = CliLaunchReceipt::new().unwrap();
        let receipt_path = receipt.path.clone();
        let script_file = root.path().join("launch.sh");
        let env_file = root.path().join("env.sh");
        crate::config::atomic_write_private(&env_file, b"").unwrap();
        crate::config::atomic_write_private(
            &script_file,
            script(
                &launch,
                root.path(),
                &script_file,
                &env_file,
                Some(&receipt.path),
            )
            .as_bytes(),
        )
        .unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg(&script_file)
            .env_clear()
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        assert!(
            matches!(receipt.state(), CliLaunchState::Exited { code: actual, .. } if actual == code)
        );
        let metadata = std::fs::read_to_string(&receipt.path).unwrap();
        assert!(metadata
            .lines()
            .all(|line| line.starts_with("started ") || line.starts_with("exited ")));
        assert!(!script_file.exists());
        assert!(!env_file.exists());
        drop(receipt);
        assert!(!receipt_path.exists());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn interactive_fixture_keeps_a_real_tty_and_stdin_while_recording_exit_metadata() {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("interactive-fixture");
    executable_fixture(&entry, "#!/bin/sh\ntest -t 0 && test -t 1 && test -t 2 || exit 91\nIFS= read -r input\ntest \"$input\" = fixture-message || exit 92\nexit 7\n");
    let mut launch = plain_launch("fixture");
    launch.program = entry.to_str().unwrap().into();
    let receipt = CliLaunchReceipt::new().unwrap();
    let script_file = root.path().join("launch.sh");
    let env_file = root.path().join("env.sh");
    crate::config::atomic_write_private(&env_file, b"").unwrap();
    crate::config::atomic_write_private(
        &script_file,
        script(
            &launch,
            root.path(),
            &script_file,
            &env_file,
            Some(&receipt.path),
        )
        .as_bytes(),
    )
    .unwrap();
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg(&script_file)
        .env_clear()
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(0, libc::getpgrp()) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while receipt.state() == CliLaunchState::Pending && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(matches!(receipt.state(), CliLaunchState::Started { .. }));
    // Write only to this synthetic PTY. No Terminal app or real CLI is accessed,
    // and the test never reads even the fixture's terminal output.
    master.write_all(b"fixture-message\n").unwrap();
    let mut discarded_terminal_output = false;
    let status = loop {
        if !discarded_terminal_output && matches!(receipt.state(), CliLaunchState::Exited { .. }) {
            // A real terminal drains its PTY continuously. This fixture must
            // never collect tool output, so on macOS the last slave close can
            // wait forever for unread bytes. Once the tool has exited and its
            // metadata is recorded, discard only this synthetic PTY's queued
            // bytes, without closing the master and sending an early SIGHUP.
            assert_eq!(
                unsafe { libc::tcflush(master.as_raw_fd(), libc::TCIOFLUSH) },
                0
            );
            discarded_terminal_output = true;
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill(); // This test-owned synthetic fixture only.
            panic!("synthetic TTY fixture did not exit");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(7));
    assert!(matches!(
        receipt.state(),
        CliLaunchState::Exited { code: 7, .. }
    ));
}
