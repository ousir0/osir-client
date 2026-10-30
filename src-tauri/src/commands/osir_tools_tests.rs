use super::*;

#[tokio::test]
async fn inventory_cold_requests_share_one_probe_and_invalidation_never_waits_for_it() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let cache = InventoryCache::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, receiver) = tokio::sync::watch::channel(false);
    let probe = || {
        let calls = calls.clone();
        let mut receiver = receiver.clone();
        async move {
            let attempt = calls.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                while !*receiver.borrow_and_update() {
                    receiver.changed().await.unwrap();
                }
            }
            let mut record = OsirToolDetection::new("codex", ToolKind::Desktop);
            record.version = Some(attempt.to_string());
            vec![record]
        }
        .boxed()
    };
    let first = cache.records(probe);
    let second = cache.records(probe);
    tokio::pin!(first, second);
    assert!(futures::poll!(&mut first).is_pending());
    assert!(futures::poll!(&mut second).is_pending());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_millis(100), cache.invalidate())
        .await
        .expect("launch invalidation must not wait behind version subprocesses");
    release.send(true).unwrap();
    let (first, second) = tokio::join!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(first[0].version.as_deref(), Some("1"));
    assert_eq!(second, first, "old probe cannot overwrite the invalidation");
    assert_eq!(cache.records(probe).await, first);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "fresh result is cached");
}

fn output(success: bool, stdout: &str, stderr: &str) -> std::process::Output {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(if success { 0 } else { 256 })
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(if success { 0 } else { 1 })
    };
    std::process::Output {
        status,
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn successful_cli_returns_only_version_token() {
    let record = cli_result(
        "claude",
        false,
        Ok(output(true, "2.1.3 (Claude Code)\n", "")),
    );
    assert!(record.installed);
    assert_eq!(record.status, ToolStatus::Installed);
    assert_eq!(record.version.as_deref(), Some("2.1.3"));
    assert_eq!(record.kind, ToolKind::Cli);
    assert!(record.path.is_none());
    assert!(record.error.is_none());
}

#[test]
fn stderr_version_and_prerelease_are_supported() {
    let record = cli_result(
        "opencode",
        false,
        Ok(output(true, "", "v1.2.3-beta.4+build.7\n")),
    );
    assert!(record.installed);
    assert_eq!(record.version.as_deref(), Some("1.2.3-beta.4+build.7"));
}

#[test]
fn installed_but_broken_is_not_missing_and_errors_are_redacted() {
    for diagnostic in [
        "",
        "Authorization: Bearer secret-token; sk-abcdef; /private/history",
    ] {
        let record = cli_result("gemini", false, Ok(output(false, diagnostic, diagnostic)));
        assert!(record.installed);
        assert_eq!(record.status, ToolStatus::Error);
        assert!(record.version.is_none());
        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("secret-token"));
        assert!(!encoded.contains("sk-abcdef"));
        assert!(!encoded.contains("/private/history"));
    }
}

#[test]
fn unreadable_version_is_installed_with_error() {
    let record = cli_result("claude", false, Ok(output(true, "unexpected output", "")));
    assert!(record.installed);
    assert_eq!(record.status, ToolStatus::Error);
    assert!(record.version.is_none());
}

#[test]
fn missing_is_distinct_from_unknown_environment() {
    let missing = cli_result("gemini", false, Err("gemini is not installed".into()));
    assert!(!missing.installed);
    assert_eq!(missing.status, ToolStatus::NotInstalled);
    assert!(missing.error.is_none());
    let unknown = cli_result("gemini", false, Err("Command timed out after 4s".into()));
    assert!(!unknown.installed);
    assert_eq!(unknown.status, ToolStatus::Unavailable);
    assert!(unknown.error.is_some());
}

#[test]
fn timeout_or_missing_interpreter_preserves_installation_evidence() {
    for (present, diagnostic) in [
        (true, "Command timed out after 4s"),
        (
            false,
            "Failed to run claude: interpreter unavailable /private/path",
        ),
        (
            false,
            "claude is installed but its default installation is ambiguous",
        ),
        (true, "claude is not installed"),
    ] {
        let record = cli_result("claude", present, Err(diagnostic.into()));
        assert!(record.installed, "{diagnostic}");
        assert_eq!(record.status, ToolStatus::Error);
        assert!(!record.error.unwrap().contains("/private/path"));
    }
}

#[test]
fn cli_evidence_checks_exact_files_without_executing_them() {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let binary = root.path().join("claude");
    #[cfg(windows)]
    let binary = root.path().join("claude.cmd");
    std::fs::write(&binary, b"not an executable").unwrap();
    assert!(cli_file_in_dirs("claude", &[root.path().to_owned()]));
    assert!(!cli_file_in_dirs("gemini", &[root.path().to_owned()]));
    assert!(!cli_file_in_dirs("claude", &[PathBuf::from(".")]));
}

#[test]
fn serialization_matches_frontend_contract_including_nulls() {
    let record = OsirToolDetection::new("codex", ToolKind::Desktop);
    assert_eq!(
        serde_json::to_value(record).unwrap(),
        serde_json::json!({
            "id": "codex", "installed": false, "version": null, "path": null,
            "status": "not_installed", "error": null, "kind": "desktop",
            "runtimeStatus": "unavailable", "runtimeError": null,
            "launchSupported": false, "closeSupported": false, "restartSupported": false
        })
    );
    assert_eq!(WORKBUDDY_DOWNLOAD_URL, "https://www.workbuddy.ai/");
    assert_eq!(
        serde_json::to_value(OsirToolClose {
            runtime_status: ToolRuntimeStatus::Stopped,
            closed: true,
            already_stopped: false,
        })
        .unwrap(),
        serde_json::json!({
            "runtimeStatus": "stopped", "closed": true, "alreadyStopped": false
        })
    );
}

#[test]
fn version_parser_rejects_non_version_and_excessive_output() {
    for raw in [
        "",
        "secret-token",
        "1.2",
        "1234567890123456.2.3",
        "abc1.2.3",
    ] {
        assert_eq!(parsed_version(raw.as_bytes()), None, "{raw}");
    }
    let raw = format!("{}1.2.3", "x".repeat(4096));
    assert_eq!(parsed_version(raw.as_bytes()), None);
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn unsupported_desktops_are_explicit() {
    for id in ["codex", "claude-desktop", "workbuddy"] {
        let record = detect_desktop(id).await;
        assert!(!record.installed);
        assert_eq!(record.status, ToolStatus::Unavailable);
        assert!(record.error.is_some());
    }
}

#[test]
fn launch_targets_are_fixed_and_desktop_claude_is_separate_from_cli() {
    for id in std::iter::once("codex").chain(CLI_TOOLS) {
        assert!(valid_launch_target(id, "cli"));
    }
    for id in ["codex", "claude-desktop", "workbuddy"] {
        assert!(valid_launch_target(id, "desktop"));
    }
    for (id, target) in [
        ("claude", "desktop"),
        ("claude-desktop", "cli"),
        ("workbuddy", "cli"),
        ("/bin/sh", "cli"),
        ("unknown", "cli"),
        ("codex", "--desktop"),
    ] {
        assert!(!valid_launch_target(id, target));
    }
}

#[test]
fn cli_requires_existing_project_directory_and_desktop_rejects_it() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("项目 O'Brien $(no-command)");
    std::fs::create_dir(&project).unwrap();
    assert_eq!(
        launch_directory("cli", Some(project.to_string_lossy().into())).unwrap(),
        Some(project.canonicalize().unwrap())
    );
    assert!(launch_directory("cli", None).is_err());
    assert!(launch_directory("cli", Some("relative/project".into())).is_err());
    assert!(launch_directory("cli", Some("/tmp/invalid\nproject".into())).is_err());
    assert!(launch_directory(
        "cli",
        Some(root.path().join("missing").to_string_lossy().into())
    )
    .is_err());
    let file = root.path().join("file");
    std::fs::write(&file, b"fixture").unwrap();
    assert!(launch_directory("cli", Some(file.to_string_lossy().into())).is_err());
    assert!(launch_directory("desktop", Some(project.to_string_lossy().into())).is_err());
    assert!(launch_directory("desktop", None).unwrap().is_none());
}

#[test]
fn launch_response_reports_runtime_separately_from_request_acceptance() {
    let value = serde_json::to_value(OsirToolLaunch {
        isolated: true,
        already_running: false,
        runtime_status: ToolRuntimeStatus::Unavailable,
        foreground: true,
        exit_code: None,
        restarted: false,
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({"isolated": true, "alreadyRunning": false, "runtimeStatus": "unavailable", "foreground": true, "exitCode": null, "restarted": false})
    );
}
