use super::*;

fn values(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn environment(command: &Command) -> HashMap<String, Option<String>> {
    command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

#[test]
fn macos_proxy_parser_uses_only_enabled_root_static_routes() {
    let actual = parse_static_proxies(
        r#"<dictionary> {
          HTTPEnable : 1
          HTTPProxy : 127.0.0.1
          HTTPPort : 7890
          HTTPSEnable : 1
          HTTPSProxy : ::1
          HTTPSPort : 7891
          ProxyAutoConfigEnable : 1
          ProxyAutoConfigURLString : https://ignored.example/proxy.pac
          __SCOPED__ : <dictionary> {
            en0 : <dictionary> {
              HTTPProxy : nested.example
              HTTPPort : 9999
            }
          }
        }"#,
    );
    assert_eq!(actual.http.as_deref(), Some("http://127.0.0.1:7890"));
    // A macOS HTTPS proxy speaks HTTP CONNECT; this is deliberately http://.
    assert_eq!(actual.https.as_deref(), Some("http://[::1]:7891"));
    for input in [
        "<dictionary> {\n HTTPEnable : 0\n HTTPProxy : host\n HTTPPort : 1234\n}",
        "<dictionary> {\n HTTPEnable : 1\n HTTPProxy : bad@host\n HTTPPort : 1234\n}",
        "<dictionary> {\n HTTPEnable : 1\n HTTPProxy : host\n HTTPPort : 65536\n}",
        "<dictionary> {\n HTTPEnable : 1\n HTTPProxy : host\n HTTPPort : 0\n}",
        "<dictionary> {\n ProxyAutoConfigEnable : 1\n ProxyAutoConfigURLString : https://ignored.example/pac\n}",
    ] {
        assert_eq!(parse_static_proxies(input), StaticProxies::default());
    }
}

#[test]
fn proxy_priority_preserves_exact_environment_and_fills_npm_equivalents() {
    let inherited = values(&[
        ("https_proxy", "http://environment.example:8000"),
        ("HTTP_PROXY", ""),
        ("NO_PROXY", "localhost,127.0.0.1"),
    ]);
    let system = || StaticProxies {
        http: Some("http://system.example:7000".into()),
        https: Some("http://system.example:7001".into()),
    };
    let additions: HashMap<_, _> =
        proxy_additions(Some("http://app.example:6000"), &inherited, system())
            .into_iter()
            .collect();
    assert!(!additions.contains_key("HTTP_PROXY"));
    assert!(!additions.contains_key("https_proxy"));
    assert_eq!(
        additions["npm_config_https_proxy"],
        "http://app.example:6000"
    );
    assert_eq!(additions["NPM_CONFIG_NOPROXY"], "localhost,127.0.0.1");
    let additions: HashMap<_, _> = proxy_additions(None, &inherited, system())
        .into_iter()
        .collect();
    assert_eq!(
        additions["npm_config_https_proxy"],
        "http://environment.example:8000"
    );
    assert_eq!(additions["http_proxy"], "http://system.example:7000");
    for invalid in [
        "https://example.test\nsecret",
        "file:///etc/passwd",
        "http://",
    ] {
        assert!(valid_proxy(invalid).is_none());
    }
}

#[test]
fn child_proxy_merge_handles_env_clear_and_explicit_removals_without_source_changes() {
    let parent_before: Vec<_> = PROXY_KEYS.iter().map(|key| std::env::var_os(key)).collect();
    let mut command = Command::new("anchored/package-manager");
    command
        .args([
            "install",
            "--registry=https://registry.npmjs.org",
            "pinned-package@1.2.3",
        ])
        .current_dir("private-install")
        .env("https_proxy", "http://child.example:9000")
        .env("HTTP_PROXY", "")
        .env_remove("npm_config_proxy");
    let program = command.get_program().to_owned();
    let args: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
    let cwd = command.get_current_dir().map(|path| path.to_owned());
    apply_proxy_sources(
        &mut command,
        None,
        values(&[
            ("HTTPS_PROXY", "http://parent.example:8000"),
            ("NO_PROXY", "localhost"),
        ]),
        StaticProxies::default(),
    );
    let env = environment(&command);
    assert_eq!(
        env["https_proxy"].as_deref(),
        Some("http://child.example:9000")
    );
    assert_eq!(
        env["HTTPS_PROXY"].as_deref(),
        Some("http://parent.example:8000")
    );
    assert_eq!(
        env["npm_config_https_proxy"].as_deref(),
        Some("http://child.example:9000")
    );
    assert_eq!(env["HTTP_PROXY"].as_deref(), Some(""));
    assert_eq!(env["npm_config_proxy"], None);
    assert_eq!(env["npm_config_noproxy"].as_deref(), Some("localhost"));
    assert_eq!(command.get_program(), program);
    assert_eq!(command.get_args().collect::<Vec<_>>(), args);
    assert_eq!(command.get_current_dir(), cwd.as_deref());
    assert_eq!(
        PROXY_KEYS
            .iter()
            .map(|key| std::env::var_os(key))
            .collect::<Vec<_>>(),
        parent_before
    );
    let mut cleared = Command::new("fixture");
    cleared.env_clear();
    apply_proxy_sources(
        &mut cleared,
        None,
        values(&[("HTTPS_PROXY", "http://parent.example:8000")]),
        StaticProxies::default(),
    );
    let cleared = environment(&cleared);
    assert_eq!(
        cleared["HTTPS_PROXY"].as_deref(),
        Some("http://parent.example:8000")
    );
    assert_eq!(
        cleared["npm_config_https_proxy"].as_deref(),
        Some("http://parent.example:8000")
    );
}

#[cfg(not(target_os = "windows"))]
fn failed_output(stderr: &str) -> Output {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw(1 << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[cfg(target_os = "windows")]
fn failed_output(stderr: &str) -> Output {
    use std::os::windows::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw(1),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn lifecycle_failure_categories_never_return_secret_output() {
    for (diagnostic, expected) in [
        ("npm ERR! ENOTFOUND", Failure::Dns),
        ("getaddrinfo EAI_AGAIN", Failure::Dns),
        ("curl: (60) certificate verify failed", Failure::Network),
        ("npm ERR! ECONNRESET", Failure::Network),
        ("EACCES permission denied", Failure::Permission),
        ("ENOSPC no space left on device", Failure::DiskFull),
        ("npm WARN EBADENGINE Unsupported engine", Failure::Node),
        (
            "/usr/bin/env: node: No such file or directory",
            Failure::Node,
        ),
        ("'node' is not recognized", Failure::Node),
        ("npm: command not found", Failure::Node),
        (
            "Run the postinstall manually: node private/path/install.cjs",
            Failure::NativeIncomplete,
        ),
        (
            "Error: Cannot find module optional-binary\nMissing optional dependency",
            Failure::NativeIncomplete,
        ),
        ("TOOL_INSTALL_NATIVE_MISSING", Failure::NativeIncomplete),
        ("ETIMEDOUT", Failure::Timeout),
        ("unknown installation error", Failure::Failed),
    ] {
        let output = failed_output(&format!("{diagnostic}\nAuthorization: Bearer fixture-secret\nhttps://user:fixture-password@example.test/download?token=fixture-token"));
        let actual = classify(&output).unwrap_err();
        assert_eq!(actual, expected);
        let message = actual.message();
        for forbidden in [
            "fixture-secret",
            "fixture-password",
            "fixture-token",
            "Authorization",
            "example.test",
        ] {
            assert!(!message.contains(forbidden));
        }
    }
}

#[test]
fn bounded_capture_retains_only_tail() {
    let mut input = vec![b'a'; 1024 * 1024];
    input.extend_from_slice(b"final-diagnostic");
    let (kept, truncated) = tail(std::io::Cursor::new(&input), 4096);
    assert!(truncated);
    assert_eq!(kept.len(), 4096);
    assert!(kept.ends_with(b"final-diagnostic"));
}

#[cfg(not(target_os = "windows"))]
#[test]
fn child_capture_bounds_runtime_and_inherited_background_pipes() {
    for script in ["sleep 30", "sleep 30 & exit 0"] {
        let started = Instant::now();
        let result = execute(
            Command::new("/bin/sh").args(["-c", script]),
            Duration::from_millis(120),
        );
        assert!(matches!(result, Err(Failure::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    let output = execute(
        Command::new("/bin/sh").args(["-c", "printf fixture-stdout; printf fixture-stderr >&2"]),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(output.stdout, b"fixture-stdout");
    assert_eq!(output.stderr, b"fixture-stderr");
}

/// Opt-in network acceptance. No account/key, production config, global install,
/// or inference is used. Parent runs this after normal focused tests pass.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires OSIR_TOOL_INSTALL_ACCEPTANCE=1 and downloads the exact official Claude package into a private temporary prefix"]
fn official_claude_install_acceptance_uses_production_child_proxy() {
    assert_eq!(
        std::env::var("OSIR_TOOL_INSTALL_ACCEPTANCE").as_deref(),
        Ok("1"),
        "explicit isolated installation opt-in is required"
    );
    let root = tempfile::Builder::new()
        .prefix("osir-claude-install-acceptance-")
        .tempdir()
        .unwrap();
    let root_path = root.path();
    for directory in ["home", "tmp", "config", "data", "state", "cache", "install"] {
        std::fs::create_dir(root_path.join(directory)).unwrap();
    }
    let userconfig = root_path.join("user.npmrc");
    let globalconfig = root_path.join("global.npmrc");
    std::fs::write(&userconfig, "").unwrap();
    std::fs::write(&globalconfig, "").unwrap();
    let path = super::super::login_shell_path(super::super::CommandDeadline::from_timeout(Some(
        Duration::from_secs(4),
    )))
    .or_else(|| std::env::var("PATH").ok())
    .unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let npm = std::env::var_os("OSIR_TOOL_INSTALL_NPM")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            super::super::resolve_path_default(
                "npm",
                super::super::CommandDeadline::from_timeout(Some(Duration::from_secs(5))),
            )
            .ok()
            .flatten()
        })
        .expect("npm must be installed for isolated package acceptance");
    assert!(
        npm.is_absolute() && npm.is_file(),
        "the acceptance npm path must be an existing absolute file"
    );
    let isolated = |program: &std::path::Path| {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir(root_path)
            .env("PATH", &path)
            .env("HOME", root_path.join("home"))
            .env("TMPDIR", root_path.join("tmp"))
            .env("XDG_CONFIG_HOME", root_path.join("config"))
            .env("XDG_DATA_HOME", root_path.join("data"))
            .env("XDG_STATE_HOME", root_path.join("state"))
            .env("XDG_CACHE_HOME", root_path.join("cache"))
            .env("NPM_CONFIG_USERCONFIG", &userconfig)
            .env("NPM_CONFIG_GLOBALCONFIG", &globalconfig)
            .env("NPM_CONFIG_CACHE", root_path.join("cache"))
            .env("NPM_CONFIG_PREFIX", root_path.join("install"))
            .env("NPM_CONFIG_REGISTRY", "https://registry.npmjs.org");
        command
    };
    let mut install = isolated(&npm);
    install
        .args([
            "install",
            "--global",
            "--no-audit",
            "--no-fund",
            "--registry=https://registry.npmjs.org",
        ])
        .arg("--prefix")
        .arg(root_path.join("install"))
        .arg("--cache")
        .arg(root_path.join("cache"))
        .arg("--userconfig")
        .arg(&userconfig)
        .arg("--globalconfig")
        .arg(&globalconfig)
        .arg("@anthropic-ai/claude-code@2.1.312");
    // Production helper is the only proxy injection path in this acceptance.
    run_lifecycle(&mut install, "claude_install_isolated_acceptance")
        .unwrap_or_else(|safe_error| panic!("{safe_error}"));
    let mut version = isolated(&root_path.join("install/bin/claude"));
    version.arg("--version");
    let output = execute(&mut version, Duration::from_secs(30))
        .unwrap_or_else(|error| panic!("{}", error.message()));
    classify(&output).unwrap_or_else(|error| panic!("{}", error.message()));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            == Some("2.1.312"),
        "the isolated installation must report the pinned version"
    );
    println!("OSIR_TOOL_INSTALL_ACCEPTANCE status=ok tool=claude version=2.1.312 isolated=true inference=0");
}

#[cfg(target_os = "macos")]
fn acceptance_exact_npm_version(version: &str) -> bool {
    if version.is_empty() || version.len() > 96 || !version.is_ascii() {
        return false;
    }
    let (without_build, build) = version
        .split_once('+')
        .map_or((version, None), |(core, build)| (core, Some(build)));
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, pre)| (core, Some(pre)));
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<_> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|part| numeric(part)) {
        return false;
    }
    let identifiers = |value: &str, prerelease: bool| {
        value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!prerelease || !part.bytes().all(|byte| byte.is_ascii_digit()) || numeric(part))
        })
    };
    prerelease.is_none_or(|pre| identifiers(pre, true))
        && build.is_none_or(|build| identifiers(build, false))
}

#[cfg(target_os = "macos")]
fn acceptance_selected_npm_tools(filter: &str) -> Option<Vec<&'static str>> {
    if filter.len() > 64 {
        return None;
    }
    let mut selected = Vec::new();
    for tool in filter.split(',').map(str::trim) {
        let tool = match tool {
            "codex" => "codex",
            "opencode" => "opencode",
            "gemini" => "gemini",
            _ => return None,
        };
        if !selected.contains(&tool) {
            selected.push(tool);
        }
    }
    (!selected.is_empty()).then_some(selected)
}

/// Inspect bounded in-memory output only. The return value is a closed set of
/// codes; raw registry/install diagnostics are never printed or written out.
#[cfg(target_os = "macos")]
fn acceptance_npm_diagnostic(output: &Output) -> &'static str {
    use std::os::unix::process::ExitStatusExt;

    if output.status.signal().is_some() {
        return "SIGNAL";
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    for (tokens, code) in [
        (&["etarget"][..], "ETARGET"),
        (&["e404", "404 not found"][..], "E404"),
        (
            &["sandbox-exec:", "sandbox deny", "sandbox: deny"][..],
            "SANDBOX_DENIED",
        ),
        (
            &[
                "ebadplatform",
                "unsupported platform",
                "unsupported architecture",
            ][..],
            "UNSUPPORTED_PLATFORM",
        ),
        (&["ebadengine", "unsupported engine"][..], "EBADENGINE"),
        (&["enoent", "no such file or directory"][..], "ENOENT"),
        (&["enotsup", "enosys", "not supported"][..], "UNSUPPORTED"),
        (&["npm error signal", "signal sig"][..], "SIGNAL"),
        (
            &[
                "postinstall",
                "lifecycle script",
                "npm error command failed",
            ][..],
            "POSTINSTALL_FAILED",
        ),
    ] {
        if tokens.iter().any(|token| text.contains(token)) {
            return code;
        }
    }
    classify(output).err().map_or("ok", Failure::code)
}

#[cfg(target_os = "macos")]
fn acceptance_npm_view_version(command: &mut Command) -> Result<String, (Failure, &'static str)> {
    apply_proxy_environment(command);
    let output =
        execute(command, Duration::from_secs(45)).map_err(|failure| (failure, failure.code()))?;
    if !output.status.success() {
        return Err((
            classify(&output).unwrap_err(),
            acceptance_npm_diagnostic(&output),
        ));
    }
    if output.stdout.len() > 256 {
        return Err((Failure::Failed, "INVALID_METADATA"));
    }
    let version: String = serde_json::from_slice(&output.stdout)
        .map_err(|_| (Failure::Failed, "INVALID_METADATA"))?;
    if !acceptance_exact_npm_version(&version) {
        return Err((Failure::Failed, "INVALID_METADATA"));
    }
    Ok(version)
}

#[cfg(target_os = "macos")]
#[test]
fn isolated_npm_acceptance_rejects_ranges_commands_and_other_tool_filters() {
    for version in ["0.1.0", "1.19.5", "0.66.0", "2.0.0-beta.2+build.01"] {
        assert!(acceptance_exact_npm_version(version));
    }
    for version in [
        "",
        "latest",
        "v1.2.3",
        "^1.2.3",
        "1.2",
        "01.2.3",
        "1.2.3-beta.01",
        "1.2.3+",
        "1.2.3\n",
        "1.2.3 --global",
        "1.2.3;touch marker",
        "1.2.3/../../",
    ] {
        assert!(!acceptance_exact_npm_version(version));
    }
    assert_eq!(
        acceptance_selected_npm_tools("codex,opencode,gemini,codex"),
        Some(vec!["codex", "opencode", "gemini"])
    );
    for filter in [
        "",
        "claude",
        "codex,claude",
        "codex,",
        "all",
        "codex;gemini",
    ] {
        assert!(acceptance_selected_npm_tools(filter).is_none());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn isolated_npm_acceptance_diagnostics_are_fixed_codes_without_raw_details() {
    use std::os::unix::process::ExitStatusExt;

    for (diagnostic, expected) in [
        ("npm error code ETARGET", "ETARGET"),
        (r#"{"error":{"code":"E404"}}"#, "E404"),
        ("npm error code ENOENT", "ENOENT"),
        (
            "sandbox-exec: sandbox_apply: Operation not permitted",
            "SANDBOX_DENIED",
        ),
        ("npm error code EBADPLATFORM", "UNSUPPORTED_PLATFORM"),
        ("unsupported architecture", "UNSUPPORTED_PLATFORM"),
        ("npm error code EBADENGINE", "EBADENGINE"),
        ("ENOTSUP not supported", "UNSUPPORTED"),
        (
            "npm error command failed\nnpm error command sh -c node install.mjs",
            "POSTINSTALL_FAILED",
        ),
        ("npm error signal SIGTERM", "SIGNAL"),
        ("npm error ENOTFOUND", "TOOL_INSTALL_DNS"),
        ("unknown diagnostic", "TOOL_INSTALL_FAILED"),
    ] {
        let output = failed_output(&format!(
            "{diagnostic}\nAuthorization: Bearer fixture-secret\nhttps://user:fixture-password@example.test/?token=fixture-token"
        ));
        assert_eq!(acceptance_npm_diagnostic(&output), expected);
    }
    let mut signalled = failed_output("fixture-secret");
    signalled.status = std::process::ExitStatus::from_raw(9);
    assert_eq!(acceptance_npm_diagnostic(&signalled), "SIGNAL");
}

/// Opt-in official npm acceptance for the other three provider CLIs. Installs
/// are local to fresh temporary projects, not npm global installations. This
/// test never reads account credentials, starts a conversation, or runs Claude.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires OSIR_TOOL_INSTALL_ACCEPTANCE=1; installs only selected official Codex/OpenCode/Gemini npm packages in temporary projects, without inference"]
fn official_provider_clis_install_acceptance_uses_production_child_proxy() {
    use std::path::{Path, PathBuf};

    assert_eq!(
        std::env::var("OSIR_TOOL_INSTALL_ACCEPTANCE").as_deref(),
        Ok("1"),
        "explicit isolated installation opt-in is required"
    );
    let optional_env = |name: &str| match std::env::var(name) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("acceptance environment values must be valid Unicode")
        }
    };
    let filter = optional_env("OSIR_TOOL_INSTALL_ACCEPTANCE_TOOLS")
        .unwrap_or_else(|| "codex,opencode,gemini".into());
    let selected = acceptance_selected_npm_tools(&filter)
        .expect("tool filter must contain only codex, opencode, or gemini");
    let metadata_only = match optional_env("OSIR_TOOL_INSTALL_ACCEPTANCE_METADATA_ONLY").as_deref()
    {
        None => false,
        Some("1") => {
            assert_eq!(
                selected,
                vec!["opencode"],
                "metadata-only diagnosis requires the explicit opencode-only tool filter"
            );
            true
        }
        Some(_) => panic!("metadata-only diagnosis must be absent or explicitly enabled with 1"),
    };
    let diagnostic_install = match optional_env("OSIR_TOOL_INSTALL_ACCEPTANCE_DIAGNOSTIC_INSTALL")
        .as_deref()
    {
        None => false,
        Some("1") => {
            assert!(
                !metadata_only && selected == vec!["opencode"],
                "diagnostic installation requires opencode-only filtering and cannot be metadata-only"
            );
            true
        }
        Some(_) => panic!("diagnostic installation must be absent or explicitly enabled with 1"),
    };
    // Do not invoke a login shell or source the user's shell/Node managers.
    let path = "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    let npm = std::env::var_os("OSIR_TOOL_INSTALL_NPM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/npm"));
    assert!(
        npm.is_absolute() && npm.is_file(),
        "the acceptance npm path must be an existing absolute file"
    );
    let npm = npm
        .canonicalize()
        .expect("could not resolve the acceptance npm path");
    assert!(
        !npm.starts_with("/Users") && !npm.starts_with("/System/Volumes/Data/Users"),
        "npm must be installed outside the user's production home"
    );
    let mut failed = false;
    for tool in selected {
        let (package, binary, version_env, default_version) = match tool {
            "codex" => (
                "@openai/codex",
                "codex",
                "OSIR_TOOL_INSTALL_CODEX_VERSION",
                None,
            ),
            "opencode" => (
                "opencode-ai",
                "opencode",
                "OSIR_TOOL_INSTALL_OPENCODE_VERSION",
                Some("1.19.5"),
            ),
            "gemini" => (
                "@google/gemini-cli",
                "gemini",
                "OSIR_TOOL_INSTALL_GEMINI_VERSION",
                Some("0.66.0"),
            ),
            _ => unreachable!(),
        };
        // Keep this test's official package mapping aligned with production.
        assert_eq!(super::super::npm_package_for(tool), Some(package));
        let root = tempfile::Builder::new()
            .prefix("osir-provider-cli-install-acceptance-")
            .tempdir()
            .expect("could not create a private acceptance directory");
        let fixture_path = root
            .path()
            .canonicalize()
            .expect("could not resolve the private acceptance directory");
        let root_path = fixture_path.join("private");
        std::fs::create_dir(&root_path)
            .expect("could not prepare the private acceptance directory");
        for directory in [
            "home", "tmp", "config", "data", "state", "cache", "install", "codex",
        ] {
            std::fs::create_dir(root_path.join(directory))
                .expect("could not prepare the private acceptance directory");
        }
        let userconfig = root_path.join("user.npmrc");
        let globalconfig = root_path.join("global.npmrc");
        for config in [&userconfig, &globalconfig] {
            std::fs::write(config, "").expect("could not prepare private npm configuration");
        }
        std::fs::write(
            root_path.join("install/package.json"),
            r#"{"name":"osir-isolated-cli-acceptance","private":true,"version":"0.0.0"}"#,
        )
        .expect("could not prepare the private npm project");
        // The policy and synthetic outside marker are deliberately not writable
        // by sandboxed descendants. The canonical private subdirectory is the
        // only file-write exception, including npm lifecycle scripts.
        let outside_marker = fixture_path.join("outside-marker");
        std::fs::write(&outside_marker, "osir-synthetic-marker")
            .expect("could not prepare the synthetic sandbox marker");
        let literal = |path: &Path| {
            let text = path.to_str().expect("acceptance paths must be Unicode");
            assert!(
                !text.chars().any(char::is_control),
                "acceptance paths must not contain control characters"
            );
            serde_json::to_string(text).expect("could not encode the acceptance sandbox path")
        };
        let sandbox_profile = fixture_path.join("sandbox.sb");
        std::fs::write(
            &sandbox_profile,
            format!(
                "(version 1)\n(allow default)\n(deny file-write*)\n\
             (allow file-write* (subpath {}) (literal \"/dev/null\"))\n\
             (deny file-read* (subpath \"/Users\"))\n\
             (deny file-read* (subpath \"/System/Volumes/Data/Users\"))\n\
             (deny file-read* (literal {}))\n",
                literal(&root_path),
                literal(&outside_marker),
            ),
        )
        .expect("could not prepare the acceptance sandbox policy");
        let isolated = |program: &Path| {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command
                .arg("-f")
                .arg(&sandbox_profile)
                .arg(program)
                .env_clear()
                .current_dir(&root_path)
                .env("PATH", &path)
                .env("HOME", root_path.join("home"))
                .env("TMPDIR", root_path.join("tmp"))
                .env("XDG_CONFIG_HOME", root_path.join("config"))
                .env("XDG_DATA_HOME", root_path.join("data"))
                .env("XDG_STATE_HOME", root_path.join("state"))
                .env("XDG_CACHE_HOME", root_path.join("cache"))
                .env("CODEX_HOME", root_path.join("codex"))
                .env("NPM_CONFIG_USERCONFIG", &userconfig)
                .env("NPM_CONFIG_GLOBALCONFIG", &globalconfig)
                .env("NPM_CONFIG_CACHE", root_path.join("cache"))
                .env("NPM_CONFIG_PREFIX", root_path.join("install"))
                .env("NPM_CONFIG_GLOBAL", "false")
                .env("NPM_CONFIG_REGISTRY", "https://registry.npmjs.org")
                .env("NPM_CONFIG_LOGLEVEL", "error")
                .env("NPM_CONFIG_LOGS_MAX", "0")
                .env("NPM_CONFIG_UPDATE_NOTIFIER", "false")
                .env("CI", "1")
                .env("NO_COLOR", "1")
                .env("DO_NOT_TRACK", "1")
                .env("DISABLE_TELEMETRY", "1")
                .env("DISABLE_ERROR_REPORTING", "1")
                .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
                .env("OPENCODE_DISABLE_MODELS_FETCH", "1")
                .env("OPENCODE_DISABLE_DEFAULT_PLUGINS", "1")
                .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "1")
                .env("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1");
            command
        };
        let mut requested_version =
            optional_env(version_env).or_else(|| default_version.map(str::to_owned));
        if let Some(version) = &requested_version {
            assert!(
                acceptance_exact_npm_version(version),
                "version override must be an exact npm semver, without a tag, range, or command"
            );
        }
        let mut stage = "sandbox";
        let mut diagnostic = "ok";
        let mut metadata_verified = false;
        let mut installation_attempted = false;
        let mut version_verified = false;
        let accepted = (|| -> Result<(), Failure> {
            // Preflight only synthetic files. Never read real user contents to
            // test a denial. A policy/runtime incompatibility stops downloads.
            let mut read_outside = isolated(Path::new("/bin/cat"));
            read_outside.arg(&outside_marker);
            let output = execute(&mut read_outside, Duration::from_secs(5))?;
            if output.status.success() || !output.stdout.is_empty() {
                return Err(Failure::Failed);
            }
            let mut write_outside = isolated(Path::new("/bin/sh"));
            write_outside
                .args(["-c", "printf blocked > \"$1\"", "sh"])
                .arg(&outside_marker);
            let output = execute(&mut write_outside, Duration::from_secs(5))?;
            if output.status.success()
                || std::fs::read(&outside_marker)
                    .map_err(|_| Failure::Failed)?
                    .as_slice()
                    != b"osir-synthetic-marker"
            {
                return Err(Failure::Failed);
            }
            let inside_marker = root_path.join("inside-marker");
            let mut write_inside = isolated(Path::new("/bin/sh"));
            write_inside
                .args(["-c", "printf allowed > \"$1\"", "sh"])
                .arg(&inside_marker);
            let output = execute(&mut write_inside, Duration::from_secs(5))?;
            if !output.status.success()
                || std::fs::read(&inside_marker)
                    .map_err(|_| Failure::Failed)?
                    .as_slice()
                    != b"allowed"
            {
                return Err(Failure::Failed);
            }
            // Official OpenCode postinstall verifies its binary using Node's
            // stdio: "ignore", which opens the nonpersistent null device.
            // This exception permits no additional filesystem destination.
            let mut null_control = isolated(Path::new("/opt/homebrew/bin/node"));
            null_control.args([
                "-e",
                "const r=require('node:child_process').spawnSync('/usr/bin/true',[],{stdio:'ignore'});process.exit(r.error?1:r.status??1)",
            ]);
            let output = execute(&mut null_control, Duration::from_secs(5))?;
            if !output.status.success() {
                return Err(Failure::Failed);
            }
            stage = "metadata";
            let query_version = |selector: &str| {
                let mut view = isolated(&npm);
                view.args([
                    "view",
                    selector,
                    "version",
                    "--json",
                    "--registry=https://registry.npmjs.org",
                    "--fetch-retries=0",
                    "--fetch-timeout=30000",
                ]);
                acceptance_npm_view_version(&mut view)
            };
            if requested_version.is_none() {
                // Codex has no guessed version: query only the public version
                // field with bounded runtime and the production proxy merge.
                let version =
                    query_version("@openai/codex@latest").map_err(|(failure, code)| {
                        diagnostic = code;
                        failure
                    })?;
                requested_version = Some(version);
            }
            let version = requested_version.as_deref().ok_or(Failure::Failed)?;
            // Every exact requested version must exist in the official registry
            // before any package tarball or lifecycle script is downloaded.
            let published =
                query_version(&format!("{package}@{version}")).map_err(|(failure, code)| {
                    diagnostic = code;
                    failure
                })?;
            if published != version {
                diagnostic = "VERSION_MISMATCH";
                return Err(Failure::Failed);
            }
            metadata_verified = true;
            if metadata_only {
                return Ok(());
            }
            stage = "install";
            installation_attempted = true;
            let mut install = isolated(&npm);
            install
                .args([
                    "install",
                    "--no-save",
                    "--package-lock=false",
                    "--include=optional",
                    "--no-audit",
                    "--no-fund",
                    "--registry=https://registry.npmjs.org",
                ])
                .arg("--prefix")
                .arg(root_path.join("install"))
                .arg(format!("{package}@{version}"));
            // No shell, --global, user npmrc, production home, or prompt.
            // Normal acceptance uses the production wrapper. A separately
            // opted-in diagnostic run captures once, in memory, with its same
            // proxy merge, deadline, process cleanup and bounded capture. It
            // is labelled explicitly and never counts as run_lifecycle proof.
            if diagnostic_install {
                apply_proxy_environment(&mut install);
                let output = execute(&mut install, LIFECYCLE_TIMEOUT).map_err(|failure| {
                    diagnostic = failure.code();
                    failure
                })?;
                if !output.status.success() {
                    diagnostic = acceptance_npm_diagnostic(&output);
                    return Err(classify(&output).unwrap_err());
                }
            } else {
                run_lifecycle(&mut install, &format!("{tool}_install_isolated_acceptance"))
                    .map_err(|safe_error| {
                        let failure = [
                            Failure::Dns,
                            Failure::Network,
                            Failure::Permission,
                            Failure::DiskFull,
                            Failure::Node,
                            Failure::NativeIncomplete,
                            Failure::Timeout,
                            Failure::Failed,
                        ]
                        .into_iter()
                        .find(|failure| safe_error.starts_with(failure.code()))
                        .unwrap_or(Failure::Failed);
                        diagnostic = failure.code();
                        failure
                    })?;
            }
            stage = "version";
            let executable = root_path.join("install/node_modules/.bin").join(binary);
            let mut probe = isolated(&executable);
            probe.arg("--version");
            let output = execute(&mut probe, Duration::from_secs(45))?;
            classify(&output)?;
            let stdout = std::str::from_utf8(&output.stdout).map_err(|_| Failure::Failed)?;
            let reported = if tool == "codex" {
                stdout
                    .trim()
                    .strip_prefix("codex-cli ")
                    .ok_or(Failure::Failed)?
                    .trim()
            } else {
                stdout.trim()
            };
            if !acceptance_exact_npm_version(reported) || reported != version {
                diagnostic = "VERSION_MISMATCH";
                return Err(Failure::Failed);
            }
            version_verified = true;
            Ok(())
        })();
        if accepted.is_err() && diagnostic == "ok" {
            diagnostic = if stage == "sandbox" {
                "SANDBOX_PREFLIGHT_FAILED"
            } else {
                accepted.as_ref().unwrap_err().code()
            };
        }
        failed |= accepted.is_err();
        println!(
            "{}",
            serde_json::json!({
                "event": "OSIR_TOOL_INSTALL_ACCEPTANCE",
                "status": if accepted.is_ok() { "ok" } else { "error" },
                "mode": if metadata_only { "metadata_only" } else if diagnostic_install { "diagnostic_install" } else { "install_and_version" },
                "tool": tool,
                "package": package,
                "version": requested_version,
                "stage": stage,
                "category": accepted.err().map_or("ok", Failure::code),
                "diagnostic": diagnostic,
                "metadataVerified": metadata_verified,
                "installationAttempted": installation_attempted,
                "versionVerified": version_verified,
                "productionLifecycleVerified": !diagnostic_install && version_verified,
                "source": "official_npm",
                "sandbox": "private_root_only",
                "isolated": true,
                "global": false,
                "inference": 0,
            })
        );
        if stage == "sandbox" && failed {
            break;
        }
        // RAII removes each tool's entire temporary home/config/cache/install.
    }
    assert!(
        !failed,
        "one or more isolated CLI acceptances failed; see fixed JSON metadata"
    );
}
