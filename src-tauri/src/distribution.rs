//! Establish channel isolation before the runtime, settings caches or plugins start.
use std::path::PathBuf;
use std::sync::OnceLock;

static IDENTITY: OnceLock<String> = OnceLock::new();
static ISOLATED_KEYCHAIN: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn is_osir() -> bool {
    IDENTITY.get().is_some_and(|id| {
        matches!(
            id.as_str(),
            "com.osir.client" | "com.osir.client.dev" | "com.osir.client.beta"
        )
    })
}

pub fn isolated_keychain() -> Option<PathBuf> {
    ISOLATED_KEYCHAIN.get().and_then(Clone::clone)
}

pub fn is_isolated() -> bool {
    IDENTITY
        .get()
        .is_some_and(|id| matches!(id.as_str(), "com.osir.client.dev" | "com.osir.client.beta"))
}

/// Settings imported from another channel must not redirect an isolated build
/// into the user's live tool or application directories. Do not change the
/// saved setting: returning None lets the caller use its isolated default.
pub(crate) fn checked_directory_override(path: PathBuf) -> Option<PathBuf> {
    let sandbox = std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from);
    checked_directory_override_for(path, is_isolated(), sandbox.as_deref())
}

fn checked_directory_override_for(
    path: PathBuf,
    isolated: bool,
    sandbox: Option<&std::path::Path>,
) -> Option<PathBuf> {
    if !isolated {
        // Preserve production/upstream custom-path semantics, including paths
        // that are relative, do not exist yet, or use symbolic links.
        return Some(path);
    }
    let sandbox = sandbox.filter(|root| root.is_absolute())?;
    if !path.is_absolute() {
        return None;
    }
    let sandbox = std::fs::canonicalize(sandbox).ok()?;
    if !sandbox.is_dir() {
        return None;
    }

    // A tool override may name a directory that has not been created yet.
    // Resolve its nearest existing ancestor, keeping symlink/.. semantics
    // intact. In particular, exists() would hide dangling symbolic links.
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // file_name() rejects an unresolved trailing '..'. Never
                // lexically collapse it across a missing directory/symlink.
                missing.push(ancestor.file_name()?.to_os_string());
                if !ancestor.pop() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    let mut resolved = std::fs::canonicalize(ancestor).ok()?;
    if !resolved.is_dir() || !resolved.starts_with(&sandbox) {
        return None;
    }
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Some(resolved)
}

pub fn development_close_exits() -> bool {
    IDENTITY.get().is_some_and(|id| id == "com.osir.client.dev")
        && std::env::var("OSIR_DEV_KEEP_TRAY").as_deref() != Ok("1")
}

pub async fn configure_proxy(db: &crate::database::Database) -> Result<(), crate::error::AppError> {
    configure_proxy_for(IDENTITY.get().map(String::as_str), db).await
}

async fn configure_proxy_for(
    identity: Option<&str>,
    db: &crate::database::Database,
) -> Result<(), crate::error::AppError> {
    let port = match identity {
        Some("com.osir.client.dev") => 15724,
        Some("com.osir.client.beta") => 15725,
        _ => return Ok(()),
    };
    let mut config = db.get_global_proxy_config().await?;
    // Move only the shared defaults (including imported channel defaults).
    // An explicit custom port remains the user's responsibility.
    if (15721..=15725).contains(&config.listen_port) {
        if config.listen_port != port || config.listen_address != "127.0.0.1" {
            config.listen_port = port;
            config.listen_address = "127.0.0.1".into();
            db.update_global_proxy_config(config).await?;
        }
    }
    Ok(())
}

fn sandbox_home(
    channel: &str,
    real_home: &std::path::Path,
    development_override: Option<PathBuf>,
) -> PathBuf {
    if channel == "dev" {
        development_override
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| real_home.join(".osir-client-dev/sandbox"))
    } else {
        real_home.join(".osir-client-beta/sandbox")
    }
}

fn checked_system_home(home: Option<PathBuf>) -> std::io::Result<PathBuf> {
    home.filter(|path| path.is_absolute()).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "无法确定本机系统用户目录")
    })
}

/// HOME is rewritten below and is inherited by both LaunchServices and direct
/// restarts. Read the effective OS account instead so isolation is idempotent.
#[cfg(unix)]
pub(crate) fn system_user_home() -> std::io::Result<PathBuf> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    // Account databases can require a larger getpwuid_r buffer. Bound retries
    // and keep the backing bytes alive until pw_dir has been copied.
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut record = MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status));
        }
        if found.is_null() {
            return checked_system_home(None);
        }
        // Success with a non-null result initializes record, and pw_dir is
        // either null or a NUL-terminated string within the live lookup buffer.
        let record = unsafe { record.assume_init() };
        let home = if record.pw_dir.is_null() {
            None
        } else {
            let bytes = unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes();
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
        };
        return checked_system_home(home);
    }
}

#[cfg(not(unix))]
pub(crate) fn system_user_home() -> std::io::Result<PathBuf> {
    // On Windows dirs uses the OS known-folder API rather than inherited HOME.
    checked_system_home(dirs::home_dir())
}

// Pre-runtime discovery must not wait indefinitely for macOS security services.
// The child only reports the default path; it never reads an account secret.
#[cfg(any(target_os = "macos", test))]
fn bounded_output(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output(),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error,
                    _ => std::io::Error::new(std::io::ErrorKind::TimedOut, "系统凭据目录查询超时"),
                });
            }
        }
    }
}

pub fn initialize(identifier: &str) -> std::io::Result<()> {
    crate::services::local_extensions::capture_environment();
    let _ = IDENTITY.set(identifier.to_owned());
    let channel = match identifier {
        "com.osir.client.dev" => "dev",
        "com.osir.client.beta" => "beta",
        _ => return Ok(()),
    };
    let real_home = system_user_home()?;
    let sandbox = sandbox_home(
        channel,
        &real_home,
        std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
    );
    crate::session_manager::sources::capture_system_roots(&real_home, &sandbox);
    std::fs::create_dir_all(&sandbox)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sandbox, std::fs::Permissions::from_mode(0o700))?;
    }
    // Resolve the existing OS credential store before HOME is isolated.
    // This command reads no credential and cannot request an access password.
    #[cfg(target_os = "macos")]
    {
        let keychain = if channel == "dev" {
            std::env::var_os("OSIR_DEV_KEYCHAIN").map(PathBuf::from)
        } else {
            None
        }
        .or_else(|| {
            let result = bounded_output(
                std::process::Command::new("/usr/bin/security")
                    .args(["default-keychain", "-d", "user"])
                    // This metadata lookup must also survive inherited sandbox
                    // variables; never alter the parent process to perform it.
                    .env("HOME", &real_home)
                    .env("USERPROFILE", &real_home),
                std::time::Duration::from_millis(1500),
            )
            .ok()?;
            if !result.status.success() {
                return None;
            }
            let raw = std::str::from_utf8(&result.stdout)
                .ok()?
                .trim()
                .trim_matches('"');
            let path = PathBuf::from(raw);
            path.is_absolute().then_some(path)
        });
        let _ = ISOLATED_KEYCHAIN.set(keychain);
    }
    // Called at the start of main, before Tauri creates threads.
    for (name, path) in [
        ("HOME", sandbox.clone()),
        ("USERPROFILE", sandbox.clone()),
        ("CC_SWITCH_TEST_HOME", sandbox.clone()),
        ("CODEX_HOME", sandbox.join(".codex")),
        ("CLAUDE_CONFIG_DIR", sandbox.join(".claude")),
        ("OPENCODE_CONFIG_DIR", sandbox.join(".config/opencode")),
        ("XDG_CONFIG_HOME", sandbox.join(".config")),
        ("XDG_DATA_HOME", sandbox.join(".local/share")),
        (
            "OPENCODE_DB",
            sandbox.join(".local/share/opencode/opencode.db"),
        ),
    ] {
        std::env::set_var(name, path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_overrides_stay_in_each_channel_sandbox_without_creating_targets() {
        let fixture = tempfile::tempdir().unwrap();
        let dev = fixture.path().join("dev");
        let beta = fixture.path().join("beta");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::create_dir_all(&beta).unwrap();

        for (sandbox, other_channel) in [(&dev, &beta), (&beta, &dev)] {
            let root = sandbox.canonicalize().unwrap();
            assert_eq!(
                checked_directory_override_for(sandbox.clone(), true, Some(sandbox)),
                Some(root.clone())
            );
            for suffix in [
                ".codex",
                ".claude",
                ".gemini",
                ".config/opencode",
                "app/config",
            ] {
                let target = sandbox.join(suffix);
                assert_eq!(
                    checked_directory_override_for(target.clone(), true, Some(sandbox)),
                    Some(root.join(suffix))
                );
                assert!(!target.exists(), "validation must not create directories");
                assert_eq!(
                    checked_directory_override_for(other_channel.join(suffix), true, Some(sandbox)),
                    None
                );
            }
            let existing = sandbox.join("existing");
            std::fs::create_dir(&existing).unwrap();
            assert_eq!(
                checked_directory_override_for(existing, true, Some(sandbox)),
                Some(root.join("existing"))
            );
            let sibling_prefix = sandbox.with_file_name(format!(
                "{}-outside",
                sandbox.file_name().unwrap().to_string_lossy()
            ));
            assert_eq!(
                checked_directory_override_for(sibling_prefix, true, Some(sandbox)),
                None
            );
        }
    }

    #[test]
    fn directory_overrides_resolve_parents_and_fail_closed_on_ambiguous_paths() {
        let fixture = tempfile::tempdir().unwrap();
        let sandbox = fixture.path().join("sandbox");
        std::fs::create_dir_all(sandbox.join("existing")).unwrap();
        let root = sandbox.canonicalize().unwrap();
        assert_eq!(
            checked_directory_override_for(
                sandbox.join("existing/../new/config"),
                true,
                Some(&sandbox)
            ),
            Some(root.join("new/config"))
        );
        std::fs::write(sandbox.join("file"), b"fixture").unwrap();
        for target in [
            sandbox.join("../outside"),
            sandbox.join("existing/../../outside"),
            sandbox.join("missing/../../outside"),
            sandbox.join("file"),
            sandbox.join("file/child"),
            PathBuf::from("relative/config"),
            PathBuf::new(),
        ] {
            assert_eq!(
                checked_directory_override_for(target.clone(), true, Some(&sandbox)),
                None,
                "unexpected isolated override: {target:?}"
            );
        }
    }

    #[test]
    fn directory_overrides_require_an_available_absolute_sandbox() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("target");
        let missing = fixture.path().join("missing");
        let file = fixture.path().join("file");
        std::fs::write(&file, b"fixture").unwrap();
        for sandbox in [
            None,
            Some(std::path::Path::new("relative")),
            Some(missing.as_path()),
            Some(file.as_path()),
        ] {
            assert_eq!(
                checked_directory_override_for(target.clone(), true, sandbox),
                None
            );
        }
    }

    #[test]
    fn directory_overrides_keep_production_custom_paths_unchanged() {
        let fixture = tempfile::tempdir().unwrap();
        for path in [
            fixture.path().join("custom"),
            fixture.path().join("custom/../new"),
            PathBuf::from("relative/config"),
            PathBuf::new(),
        ] {
            assert_eq!(
                checked_directory_override_for(path.clone(), false, None),
                Some(path)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_overrides_resolve_symlinks_and_reject_escapes_and_retargets() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let sandbox = fixture.path().join("sandbox");
        let outside = fixture.path().join("outside");
        std::fs::create_dir_all(sandbox.join("inside")).unwrap();
        std::fs::create_dir_all(outside.join("child")).unwrap();
        let root = sandbox.canonicalize().unwrap();
        let alias = sandbox.join("alias");
        symlink("inside", &alias).unwrap();
        assert_eq!(
            checked_directory_override_for(alias.join("new/config"), true, Some(&sandbox)),
            Some(root.join("inside/new/config"))
        );
        std::fs::remove_file(&alias).unwrap();
        symlink(&outside, &alias).unwrap();
        assert_eq!(
            checked_directory_override_for(alias.join("new/config"), true, Some(&sandbox)),
            None
        );

        symlink(outside.join("child"), sandbox.join("escape")).unwrap();
        symlink("../outside", sandbox.join("relative-escape")).unwrap();
        symlink(outside.join("missing"), sandbox.join("dangling")).unwrap();
        symlink("cycle", sandbox.join("cycle")).unwrap();
        for target in [
            sandbox.join("escape/../new"),
            sandbox.join("relative-escape/new/config"),
            sandbox.join("dangling"),
            sandbox.join("dangling/new/config"),
            sandbox.join("cycle/new"),
        ] {
            assert_eq!(
                checked_directory_override_for(target.clone(), true, Some(&sandbox)),
                None,
                "unexpected symbolic-link override: {target:?}"
            );
        }
        assert_eq!(
            checked_directory_override_for(alias.clone(), false, Some(&sandbox)),
            Some(alias)
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_discovery_times_out_and_reaps_child() {
        let started = std::time::Instant::now();
        let error = bounded_output(
            std::process::Command::new("/bin/sleep").arg("2"),
            std::time::Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let output = bounded_output(
            std::process::Command::new("/usr/bin/printf").arg("ready"),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ready");
    }
    #[tokio::test]
    async fn channel_proxy_defaults_are_separate_and_custom_settings_survive() {
        let db = crate::database::Database::memory().unwrap();
        configure_proxy_for(Some("com.osir.client.dev"), &db)
            .await
            .unwrap();
        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            15724
        );
        configure_proxy_for(Some("com.osir.client.beta"), &db)
            .await
            .unwrap();
        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            15725
        );
        let mut custom = db.get_global_proxy_config().await.unwrap();
        custom.listen_port = 18181;
        db.update_global_proxy_config(custom).await.unwrap();
        configure_proxy_for(Some("com.osir.client.beta"), &db)
            .await
            .unwrap();
        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            18181
        );
        let prod = crate::database::Database::memory().unwrap();
        configure_proxy_for(Some("com.osir.client"), &prod)
            .await
            .unwrap();
        assert_eq!(
            prod.get_global_proxy_config().await.unwrap().listen_port,
            15721
        );
    }
    #[test]
    fn beta_rejects_inherited_development_home_and_channels_do_not_share_tool_files() {
        let fixture = tempfile::tempdir().unwrap();
        let real = fixture.path();
        let dev = sandbox_home("dev", real, Some(real.join("checkout/.osir-dev-home")));
        let beta = sandbox_home("beta", real, Some(dev.clone()));
        assert_ne!(dev.join(".codex"), beta.join(".codex"));
        assert_ne!(beta.join(".codex"), real.join(".codex"));
        assert_eq!(beta, real.join(".osir-client-beta/sandbox"));
        assert_eq!(
            sandbox_home("dev", real, Some(PathBuf::from("relative"))),
            real.join(".osir-client-dev/sandbox")
        );
        assert!(!dev.exists() && !beta.exists());
    }

    #[test]
    fn beta_restarts_keep_the_system_account_root_instead_of_nesting_inherited_sandboxes() {
        let fixture = tempfile::tempdir().unwrap();
        let system_home = checked_system_home(Some(fixture.path().join("account"))).unwrap();
        let initial = sandbox_home("beta", &system_home, None);
        let mut inherited_home = initial.clone();
        for _ in 0..3 {
            // A restart inherits HOME and CC_SWITCH_TEST_HOME from the running
            // isolated app. Only the OS account root selects Beta's directory.
            let restarted = sandbox_home("beta", &system_home, Some(inherited_home.clone()));
            assert_eq!(restarted, initial);
            assert!(!restarted.starts_with(inherited_home.join(".osir-client-beta")));
            inherited_home = restarted;
        }
        let development_home = system_home.join("checkout/.osir-dev-home");
        assert_eq!(
            sandbox_home("dev", &system_home, Some(development_home.clone())),
            development_home
        );
        assert_eq!(
            sandbox_home("beta", &system_home, Some(development_home)),
            initial
        );
        assert!(
            !system_home.exists(),
            "selection must not create account or tool files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn system_account_home_ignores_inherited_sandbox_environment() {
        const PROBE: &str = "OSIR_SYSTEM_ACCOUNT_HOME_TEST_PROBE";
        const EXPECTED: &str = "OSIR_SYSTEM_ACCOUNT_HOME_TEST_EXPECTED";
        if std::env::var_os(PROBE).is_some() {
            let expected = PathBuf::from(std::env::var_os(EXPECTED).unwrap());
            let inherited = PathBuf::from(std::env::var_os("HOME").unwrap());
            let account_home = system_user_home().unwrap();
            assert_eq!(account_home, expected);
            assert_ne!(account_home, inherited);
            assert_eq!(
                sandbox_home(
                    "beta",
                    &account_home,
                    std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from)
                ),
                expected.join(".osir-client-beta/sandbox")
            );
            return;
        }

        let expected = system_user_home().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let inherited = fixture.path().join(".osir-client-beta/sandbox");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "distribution::tests::system_account_home_ignores_inherited_sandbox_environment",
            ])
            .env(PROBE, "1")
            .env(EXPECTED, &expected)
            .env("HOME", &inherited)
            .env("USERPROFILE", &inherited)
            .env("CC_SWITCH_TEST_HOME", &inherited)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated account-home probe failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            !inherited.exists(),
            "probe must not create tool or account files"
        );
    }

    #[test]
    fn isolation_requires_an_absolute_system_user_home_without_environment_fallback() {
        for home in [
            None,
            Some(PathBuf::new()),
            Some(PathBuf::from("relative/account")),
        ] {
            assert_eq!(
                checked_system_home(home).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn system_account_home_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = b"/isolated-fixture/account-\xff";
        let home = checked_system_home(Some(PathBuf::from(std::ffi::OsString::from_vec(
            bytes.to_vec(),
        ))))
        .unwrap();
        assert_eq!(home.as_os_str().as_bytes(), bytes);
        assert_eq!(
            sandbox_home("beta", &home, None),
            home.join(".osir-client-beta/sandbox")
        );
    }
}
