use super::*;

// Default tests use synthetic packages/profiles/CLI only. The explicitly ignored
// consumer tests additionally accept opt-in real artifacts and the official
// Desktop CLI, always with temporary homes and without starting a DSH runtime.
struct Fixture {
    dir: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    resources: PathBuf,
    cli: PathBuf,
    root: PathBuf,
}

const OTHER: &str = "# keep native patch exactly\n- id: native\n  config: {dynamic: !!js process.env.SYNTHETIC_ONLY}\n";

fn tarball(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    use std::io::Write;
    let mut tar = Vec::new();
    for (name, content) in files {
        let mut header = [0u8; 512];
        let name = format!("package/{name}");
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        let sum: usize = header.iter().map(|b| *b as usize).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        tar.extend(header);
        tar.extend(content);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.extend([0; 1024]);
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&tar).unwrap();
    gzip.finish().unwrap()
}

fn package_files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("package.json".into(), serde_json::to_vec(&json!({
            "name":PACKAGE,"version":VERSION,"main":"./lib/index.js",
            "dsh":{"bundle":{"patch":"./cordis.patch.yml"}}
        })).unwrap()),
        ("cordis.patch.yml".into(), format!("- insert:\n  - id: {ID}\n    name: '{PACKAGE}'\n    config: {{pollIntervalMs: 1234}}\n").into_bytes()),
        ("lib/index.js".into(), b"// synthetic Host fixture; never executed\nexport const Config = { osirConnectorDataDir: 'string' };\n".to_vec()),
    ])
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("osir dsh fixture ")
            .tempdir()
            .unwrap();
        let previous = ["DSH_HOME", "CC_SWITCH_TEST_HOME"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        for key in ["DSH_HOME", "CC_SWITCH_TEST_HOME"] {
            std::env::set_var(key, dir.path());
        }
        let resources = dir.path().join("Anywhere.app/Contents/Resources");
        let cli = dir.path().join("fake dsh");
        let root = connector_root(Some(dir.path().join("Application Support"))).unwrap();
        for name in ["web", "desktop", "headless"] {
            fs::create_dir_all(profile_dir(name)).unwrap();
            fs::write(profile_path(name), OTHER).unwrap();
        }
        fs::write(
            home_patch_path(),
            "- id: home-owned\n  config: {keep: true}\n",
        )
        .unwrap();
        fs::write(
            data_dir().join("fake-profile.json"),
            serde_json::to_vec(&json!({
                "customUserField":true,"dependencies":{PACKAGE:"file:synthetic"},
                "dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base",PACKAGE]}}
            }))
            .unwrap(),
        )
        .unwrap();
        let this = Self {
            dir,
            previous,
            resources,
            cli,
            root,
        };
        this.artifact(&package_files());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(
                &this.cli,
                r##"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$DSH_HOME/cli-args"
test "$1" = plugin && test "$2" = --profile && test "$3" = desktop && test "$4" = add
test ! -f "$DSH_HOME/fake-fail" || exit 1
test ! -f "$DSH_HOME/fake-noop" || exit 0
destination="$DSH_HOME/profiles/desktop/node_modules/@osir/anywhere-dsh-bridge"
/bin/mkdir -p "$destination"
/usr/bin/tar -xzf "$5" -C "$destination" --strip-components=1
/bin/cp "$DSH_HOME/fake-profile.json" "$DSH_HOME/profiles/desktop/package.json"
"##,
            )
            .unwrap();
            fs::set_permissions(&this.cli, fs::Permissions::from_mode(0o700)).unwrap();
        }
        this
    }
    fn artifact(&self, files: &BTreeMap<String, Vec<u8>>) {
        let payload = tarball(files);
        let target = self.resources.join("dsh-bridge");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("osir-dsh-bridge.tgz"), &payload).unwrap();
        fs::write(
            target.join("package-info.json"),
            serde_json::to_vec(&json!({
                "schemaVersion":1,"name":PACKAGE,"version":VERSION,
                "sha256":format!("{:x}", Sha256::digest(&payload)),"size":payload.len()
            }))
            .unwrap(),
        )
        .unwrap();
    }
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        target_paths()
            .into_iter()
            .chain([home_patch_path()])
            .map(|p| {
                let bytes = fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect()
    }
    fn unchanged(&self, before: &[(PathBuf, Vec<u8>)]) {
        for (path, bytes) in before {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

#[test]
fn osir_bridge_product_gate_and_root_match_packaged_desktop_contract() {
    assert!(enabled_for(true, false));
    assert!(!enabled_for(false, false));
    assert!(!enabled_for(true, true));
    assert!(!enabled_for(false, true));
    let app_data = std::env::temp_dir().join("Application Support");
    assert_eq!(
        connector_root(Some(app_data.clone())).unwrap(),
        app_data.join("OSIR Anywhere/connector")
    );
    assert!(connector_root(None).is_err());
    assert!(connector_root(Some("relative".into())).is_err());
}

#[test]
fn osir_bridge_inject_preserves_user_config_and_native_patch_and_is_idempotent() {
    let root = std::env::temp_dir().join("OSIR Anywhere/connector");
    let text = format!("{OTHER}- id: {ID}\n  disabled: false\n  config: {{pollIntervalMs: 42, userOwned: {{keep: yes}}}}\n");
    let next = inject(&text, &root, &Yaml::Mapping(Default::default())).unwrap();
    assert!(next.starts_with(OTHER));
    let row = entries(&next).unwrap().pop().unwrap();
    assert_eq!(row["config"]["pollIntervalMs"], Yaml::from(42));
    assert_eq!(row["config"]["userOwned"]["keep"], Yaml::from("yes"));
    assert_eq!(
        row["config"]["osirConnectorDataDir"].as_str(),
        root.to_str()
    );
    assert_eq!(inject(&next, &root, &Yaml::Null).unwrap(), next);
}

#[test]
fn osir_bridge_rejects_ambiguous_disabled_dynamic_and_duplicate_overrides() {
    let root = std::env::temp_dir();
    for text in [
        format!("- id: {ID}\n  disabled: true\n"),
        format!("- id: {ID}\n  name: other-package\n"),
        format!("- id: {ID}\n  config: !!js process.env.SYNTHETIC_ONLY\n"),
        format!("- id: {ID}\n- id: {ID}\n"),
        format!("- insert:\n  - id: {ID}\n    name: '{PACKAGE}'\n"),
    ] {
        assert!(
            inject(&text, &root, &Yaml::Mapping(Default::default())).is_err(),
            "{text}"
        );
    }
}

#[test]
#[serial_test::serial]
fn osir_bridge_artifact_rejects_missing_tampered_misidentified_and_upstream_packages() {
    let fixture = Fixture::new();
    assert!(Artifact::load(&fixture.resources).is_ok());
    for (name, version) in [(UPSTREAM_PACKAGE, "2.2.0"), (PACKAGE, "2.2.0")] {
        let mut files = package_files();
        let mut manifest: Value = serde_json::from_slice(&files["package.json"]).unwrap();
        manifest["name"] = json!(name);
        manifest["version"] = json!(version);
        files.insert(
            "package.json".into(),
            serde_json::to_vec(&manifest).unwrap(),
        );
        fixture.artifact(&files);
        assert!(Artifact::load(&fixture.resources).is_err());
    }
    fixture.artifact(&package_files());
    fs::write(
        fixture.resources.join("dsh-bridge/osir-dsh-bridge.tgz"),
        b"tampered",
    )
    .unwrap();
    assert!(Artifact::load(&fixture.resources).is_err());
    fs::remove_file(fixture.resources.join("dsh-bridge/package-info.json")).unwrap();
    assert!(Artifact::load(&fixture.resources)
        .unwrap_err_string()
        .contains("缺失"));
}

// Only format the error; never include package or CLI output in diagnostics.
trait ErrorText {
    fn unwrap_err_string(self) -> String;
}
impl<T> ErrorText for Result<T, AppError> {
    fn unwrap_err_string(self) -> String {
        match self {
            Err(error) => error.to_string(),
            Ok(_) => panic!("expected error"),
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn osir_bridge_missing_artifact_and_upstream_conflicts_never_invoke_cli_or_write_patches() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    fs::remove_file(fixture.resources.join("dsh-bridge/osir-dsh-bridge.tgz")).unwrap();
    assert!(
        prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
            .await
            .is_err()
    );
    fixture.unchanged(&before);
    fixture.artifact(&package_files());
    fs::write(
        profile_dir("desktop").join("package.json"),
        serde_json::to_vec(&json!({
            "dsh":{"profile":{"bundles":[UPSTREAM_PACKAGE]}}
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
            .await
            .unwrap_err_string()
            .contains("冲突")
    );
    assert!(!fixture.dir.path().join("cli-args").exists());
    fixture.unchanged(&before);
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn osir_bridge_official_add_injects_desktop_preserves_other_profiles_and_does_not_reinstall()
{
    let fixture = Fixture::new();
    prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
        .await
        .unwrap();
    let args = read_text(&fixture.dir.path().join("cli-args")).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(&args[..4], &["plugin", "--profile", "desktop", "add"]);
    let tarball = Path::new(args[4]);
    assert!(tarball.is_absolute() && tarball.is_file());
    assert!(tarball.starts_with(fixture.dir.path().join(".osir-plugin-artifacts")));
    let desktop = read_text(&profile_path("desktop")).unwrap();
    assert!(desktop.starts_with(OTHER));
    let row = entries(&desktop).unwrap().pop().unwrap();
    assert_eq!(row["id"].as_str(), Some(ID));
    assert_eq!(
        row["config"]["osirConnectorDataDir"].as_str(),
        fixture.root.to_str()
    );
    assert_eq!(row["config"]["pollIntervalMs"], Yaml::from(1234));
    for name in ["web", "headless"] {
        assert_eq!(read_text(&profile_path(name)).unwrap(), OTHER);
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(profile_dir("desktop").join("package.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["customUserField"], true);
    fs::remove_file(fixture.dir.path().join("cli-args")).unwrap();
    let before = fixture.snapshot();
    prepare(&fixture.resources, &fixture.cli, true, &fixture.root)
        .await
        .unwrap();
    fixture.unchanged(&before);
    assert!(!fixture.dir.path().join("cli-args").exists());
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn osir_bridge_cli_failure_noop_and_running_runtime_cannot_claim_ready() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    assert!(
        prepare(&fixture.resources, &fixture.cli, true, &fixture.root)
            .await
            .is_err()
    );
    assert!(!fixture.dir.path().join("cli-args").exists());
    fs::write(fixture.dir.path().join("fake-fail"), "").unwrap();
    assert!(
        prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
            .await
            .unwrap_err_string()
            .contains("安装失败")
    );
    fixture.unchanged(&before);
    fs::remove_file(fixture.dir.path().join("fake-fail")).unwrap();
    fs::write(fixture.dir.path().join("fake-noop"), "").unwrap();
    assert!(
        prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
            .await
            .unwrap_err_string()
            .contains("未被")
    );
    fixture.unchanged(&before);
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn osir_bridge_installed_host_tampering_and_home_shadowing_fail_without_writes() {
    let fixture = Fixture::new();
    prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
        .await
        .unwrap();
    let before = fixture.snapshot();
    fs::write(
        profile_dir("desktop")
            .join("node_modules")
            .join(PACKAGE)
            .join("lib/index.js"),
        b"old host",
    )
    .unwrap();
    assert!(
        prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
            .await
            .unwrap_err_string()
            .contains("不兼容")
    );
    fixture.unchanged(&before);
    fs::write(
        home_patch_path(),
        format!("- id: {ID}\n  config: {{osirConnectorDataDir: /old}}\n"),
    )
    .unwrap();
    let before = fixture.snapshot();
    assert!(
        prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
            .await
            .unwrap_err_string()
            .contains("全局")
    );
    fixture.unchanged(&before);
}

#[test]
#[serial_test::serial]
fn osir_bridge_atomic_profile_plan_rolls_back_every_file_on_commit_failure() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    let paths: Vec<_> = before.iter().map(|(p, _)| p.clone()).collect();
    let result = edit_files_and_commit(
        &paths,
        |texts| {
            texts
                .iter()
                .map(|text| inject(text, &fixture.root, &Yaml::Mapping(Default::default())))
                .collect()
        },
        || Err::<(), _>(failure("synthetic commit failure")),
    );
    assert!(result.is_err());
    fixture.unchanged(&before);
}

#[test]
#[serial_test::serial]
fn osir_bridge_explicitly_disabled_upstream_is_preserved_and_home_reenable_blocks() {
    let fixture = Fixture::new();
    fs::write(
        profile_dir("desktop").join("package.json"),
        serde_json::to_vec(&json!({
            "dsh":{"profile":{"bundles":[UPSTREAM_PACKAGE]}}
        }))
        .unwrap(),
    )
    .unwrap();
    let disabled = format!("{OTHER}- id: {UPSTREAM_ID}\n  disabled: true\n");
    fs::write(profile_path("desktop"), &disabled).unwrap();
    let paths = vec![profile_path("desktop")];
    assert!(preflight(&paths, std::slice::from_ref(&disabled), "").is_ok());
    assert!(preflight(
        &paths,
        &[disabled],
        &format!("- id: {UPSTREAM_ID}\n  disabled: false\n")
    )
    .is_err());
    assert!(fixture.dir.path().exists());
}

#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn osir_bridge_installer_timeout_stops_child_group() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::write(
        &fixture.cli,
        "#!/bin/sh\n/bin/sleep 5\nprintf leaked > \"$DSH_HOME/late-write\"\n",
    )
    .unwrap();
    fs::set_permissions(&fixture.cli, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(install(
        &fixture.cli,
        &fixture.dir.path().join("unused.tgz"),
        fixture.dir.path(),
        std::time::Duration::from_millis(25)
    )
    .await
    .unwrap_err_string()
    .contains("超时"));
    assert!(!fixture.dir.path().join("late-write").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn osir_bridge_desktop_cli_selects_only_contained_official_wrapper() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let bundle = fixture.path().join("DeepSeek Harness.app");
    let path = bundle.join("Contents/Resources/runtime/cli/bin/dsh");
    assert!(desktop_cli(&bundle).is_err());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "#!/bin/sh\nexec node ordinary-dsh/lib/bin.js \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(desktop_cli(&bundle).is_err());
    let script = "#!/bin/sh\nELECTRON_RUN_AS_NODE=1 exec \"$resources/../MacOS/DeepSeek Harness\" --expose-internals \"$resources/app.asar/dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/cli.js\" \"$@\"\n";
    fs::write(&path, script).unwrap();
    assert_eq!(
        desktop_cli(&bundle).unwrap(),
        fs::canonicalize(&path).unwrap()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(desktop_cli(&bundle).is_err());
    fs::remove_file(&path).unwrap();
    let outside = fixture.path().join("external-dsh");
    fs::write(&outside, script).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(outside, &path).unwrap();
    assert!(desktop_cli(&bundle).is_err());
}

/// Copy the producer's exact payload/info; never regenerate or "repair" it in
/// the consumer test. Explicit opt-in keeps default CI independent of local
/// artifacts. A missing or mismatched artifact fails instead of skipping.
fn real_artifact(fixture: &Fixture) -> Artifact {
    let resources =
        PathBuf::from(std::env::var_os("OSIR_DSH_CONSUMER_RESOURCES").expect(
            "set OSIR_DSH_CONSUMER_RESOURCES to a resources directory containing dsh-bridge",
        ));
    assert!(resources.is_absolute());
    for name in ["osir-dsh-bridge.tgz", "package-info.json"] {
        fs::copy(
            resources.join("dsh-bridge").join(name),
            fixture.resources.join("dsh-bridge").join(name),
        )
        .unwrap();
    }
    let artifact = Artifact::load(&fixture.resources).unwrap();
    eprintln!(
        "real consumer artifact: version={VERSION} bytes={} sha256={:x} members={}",
        artifact.bytes.len(),
        Sha256::digest(&artifact.bytes),
        artifact.files.len()
    );
    artifact
}

#[tokio::test]
#[ignore = "explicit producer artifact required; no real CLI/network used"]
#[serial_test::serial]
async fn osir_bridge_real_artifact_load_installed_and_profile_flow() {
    let fixture = Fixture::new();
    let artifact = real_artifact(&fixture);
    assert!(!artifact.installed(&profile_dir("desktop")).unwrap());
    // Materialize only verified regular members into a synthetic installed
    // package; the separate official-CLI test proves actual installation.
    let package = profile_dir("desktop").join("node_modules").join(PACKAGE);
    for (name, bytes) in &artifact.files {
        let path = package.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fs::copy(
        data_dir().join("fake-profile.json"),
        profile_dir("desktop").join("package.json"),
    )
    .unwrap();
    assert!(artifact.installed(&profile_dir("desktop")).unwrap());
    prepare(&fixture.resources, &fixture.cli, false, &fixture.root)
        .await
        .unwrap();
    assert!(!fixture.dir.path().join("cli-args").exists());
    let text = read_text(&profile_path("desktop")).unwrap();
    assert!(text.starts_with(OTHER));
    let row = entries(&text).unwrap().pop().unwrap();
    assert_eq!(row["id"].as_str(), Some(ID));
    assert_eq!(
        row["config"]["osirConnectorDataDir"].as_str(),
        fixture.root.to_str()
    );
    let before = fixture.snapshot();
    prepare(&fixture.resources, &fixture.cli, true, &fixture.root)
        .await
        .unwrap();
    fixture.unchanged(&before);
    fs::write(
        package.join("lib/index.js"),
        "// incompatible installed Host\n",
    )
    .unwrap();
    assert!(artifact.installed(&profile_dir("desktop")).is_err());
    fixture.unchanged(&before);
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "explicit real artifact + native DSH Desktop wrapper; installs only in a temporary DSH_HOME"]
#[serial_test::serial]
async fn osir_bridge_real_official_desktop_cli_install_and_profile_flow() {
    // Require a sanitized outer process, so the official CLI/package manager
    // cannot read the user's npm/git/home credentials. The test runner supplies
    // these paths under its own temporary evidence directory via env -i.
    let sandbox = PathBuf::from(
        std::env::var_os("OSIR_DSH_CONSUMER_SANDBOX").expect("explicit sandbox required"),
    );
    let real_home = PathBuf::from(std::env::var_os("HOME").expect("isolated HOME required"));
    assert!(sandbox.is_absolute() && real_home.starts_with(&sandbox) && real_home != sandbox);
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "npm_config_userconfig",
        "npm_config_globalconfig",
    ] {
        assert!(PathBuf::from(
            std::env::var_os(key).unwrap_or_else(|| panic!("set isolated {key}"))
        )
        .starts_with(&sandbox));
    }
    let fixture = Fixture::new();
    let artifact = real_artifact(&fixture);
    let bundle = PathBuf::from(
        std::env::var_os("OSIR_DSH_CONSUMER_DESKTOP_BUNDLE")
            .expect("explicit DSH app bundle required"),
    );
    // Same selection function as the Client macOS Desktop call site. No PATH
    // fallback and no test-created wrapper: use the actual official script.
    let cli = desktop_cli(&bundle).unwrap();
    let wrapper_sha = format!("{:x}", Sha256::digest(fs::read(&cli).unwrap()));
    eprintln!(
        "real Client-selected desktop wrapper: {} sha256={wrapper_sha}",
        cli.display()
    );
    assert!(!artifact.installed(&profile_dir("desktop")).unwrap());
    // Desktop's official CLI requires an already initialized Desktop profile.
    // Exercise the missing-profile failure without launching the application.
    assert!(prepare(&fixture.resources, &cli, false, &fixture.root)
        .await
        .is_err());
    assert!(!profile_dir("desktop").join("package.json").exists());
    assert_eq!(read_text(&profile_path("desktop")).unwrap(), OTHER);
    // Use the shipped pure initializer, as Desktop does; never hand-write the
    // dependency manifest or boot the Host to set up this isolated fixture.
    let init = fixture.dir.path().join("init-official-profile.mjs");
    fs::write(&init, r#"import { pathToFileURL } from 'node:url';
const boot = await import(pathToFileURL(process.argv[2] + '/Contents/Resources/app.asar/dsh/node_modules/@deepseek-ai/dsh-app-boot/lib/index.js').href);
boot.initProfile(process.argv[3], ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app']);
"#).unwrap();
    let initialized = tokio::process::Command::new(bundle.join("Contents/MacOS/DeepSeek Harness"))
        .env("ELECTRON_RUN_AS_NODE", "1")
        .arg("--expose-internals")
        .arg(&init)
        .arg(&bundle)
        .arg(profile_dir("desktop"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(
        initialized.success(),
        "official pure profile initialization failed"
    );
    if let Err(error) = prepare(&fixture.resources, &cli, false, &fixture.root).await {
        let preserved = sandbox.join("official-install-failure");
        fs::rename(fixture.dir.path(), &preserved).unwrap();
        panic!(
            "official install failed: {error}; isolated evidence: {}",
            preserved.display()
        );
    }
    assert!(artifact.installed(&profile_dir("desktop")).unwrap());
    let manifest: Value =
        serde_json::from_slice(&fs::read(profile_dir("desktop").join("package.json")).unwrap())
            .unwrap();
    let source = manifest["dependencies"][PACKAGE]
        .as_str()
        .expect("official add must persist dependency");
    assert!(
        source.contains(".osir-plugin-artifacts/") && source.ends_with("/osir-dsh-bridge.tgz"),
        "{source}"
    );
    assert!(profile_dir("desktop").join("pnpm-lock.yaml").is_file());
    let text = read_text(&profile_path("desktop")).unwrap();
    assert!(text.starts_with(OTHER));
    let row = entries(&text).unwrap().pop().unwrap();
    assert_eq!(
        row["config"]["osirConnectorDataDir"].as_str(),
        fixture.root.to_str()
    );
    for name in ["web", "headless"] {
        assert_eq!(read_text(&profile_path(name)).unwrap(), OTHER);
    }
    let before = fixture.snapshot();
    prepare(&fixture.resources, &cli, true, &fixture.root)
        .await
        .unwrap();
    fixture.unchanged(&before);
    eprintln!("official desktop install verified: enabled={PACKAGE}@{VERSION}; profile injection/idempotence passed; root={}", fixture.root.display());
    fs::write(
        sandbox.join("official-install-success.json"),
        serde_json::to_vec_pretty(&json!({
            "package":PACKAGE,"version":VERSION,"size":artifact.bytes.len(),
            "sha256":format!("{:x}", Sha256::digest(&artifact.bytes)),
            "wrapper":cli,"wrapperSha256":wrapper_sha,
            "profileManifest":manifest,"profilePatch":text,
            "initializedProfileRequired":true,"installedHostMatchesArtifact":true,
            "profileIdempotent":true,"runtimeStarted":false
        }))
        .unwrap(),
    )
    .unwrap();
}
