//! Offline policy tests only. No process discovery, launch, close or restart.
use super::*;

fn installed() -> Installation {
    Installation {
        root: r"C:\Program Files\WindowsApps\OpenAI.Codex_fixture".into(),
        executable: r"C:\Program Files\WindowsApps\OpenAI.Codex_fixture\app\ChatGPT.exe".into(),
        relative: "app/ChatGPT.exe".into(),
        version: "26.1027.30824.22943".into(),
        family: Some("OpenAI.Codex_2p2nqsd0c76g0".into()),
        app_id: Some("App".into()),
        protocol: true,
        package: Some(CODEX_PACKAGE.into()),
        publisher: CODEX_PUBLISHER.into(),
    }
}

#[test]
fn msix_version_accepts_full_u16_components() {
    let version = version_number("26.1027.30824.22943").unwrap();
    assert_eq!(
        version,
        (26u64 << 48) | (1027 << 32) | (30824 << 16) | 22943
    );
    assert!(version < 9_007_199_254_740_991);
    assert!(version_number("26.1027.3384.0").is_ok());
    assert!(version_number("26.65535.65535.65535").is_ok());
}

#[test]
fn msix_version_orders_all_four_components() {
    assert!(
        version_number("26.1027.30824.22944").unwrap()
            > version_number("26.1027.30824.22943").unwrap()
    );
    assert!(
        version_number("26.1028.0.0").unwrap() > version_number("26.1027.65535.65535").unwrap()
    );
}

#[test]
fn msix_version_rejects_overflow_and_invalid_text() {
    for value in [
        "26.1027.65536.0",
        "65535.65535.65535.65535",
        "32.0.0.0",
        "26.1.2",
        "26.1.2.3.4",
        "26.-1.2.3",
        "26.+1.2.3",
        "26.1.2.3\n",
    ] {
        assert!(version_number(value).is_err(), "{value}");
    }
}

#[test]
fn codex_identity_accepts_current_manifest_entry() {
    installed().validate("codex").unwrap();
}

#[test]
fn codex_identity_accepts_registered_store_package_without_using_policy_constants() {
    // Literal metadata from the Store/mirror package. Do not build this fixture
    // from CODEX_PUBLISHER: doing so hid the 0.1.3 publisher/family mismatch.
    let entry: Installation = serde_json::from_str(
        r#"{
          "root":"C:\\Program Files\\WindowsApps\\OpenAI.Codex_26.1027.3384.0_x64__2p2nqsd0c76g0",
          "executable":"C:\\Program Files\\WindowsApps\\OpenAI.Codex_26.1027.3384.0_x64__2p2nqsd0c76g0\\app\\ChatGPT.exe",
          "relative":"app/ChatGPT.exe", "version":"26.1027.3384.0",
          "family":"OpenAI.Codex_2p2nqsd0c76g0", "appId":"App",
          "protocol":true, "package":"OpenAI.Codex",
          "publisher":"CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B"
        }"#,
    )
    .unwrap();
    entry.validate("codex").unwrap();
}

#[test]
fn codex_identity_accepts_legacy_gui_entry() {
    let mut entry = installed();
    entry.relative = r"app\Codex.exe".into();
    entry.executable = format!(r"{}\app\Codex.exe", entry.root);
    entry.validate("codex").unwrap();
}

#[test]
fn codex_identity_rejects_unrelated_chatgpt_or_publisher() {
    let mut entry = installed();
    entry.package = Some("OpenAI.ChatGPT-Desktop".into());
    assert!(entry.validate("codex").is_err());
    entry.package = Some(CODEX_PACKAGE.into());
    entry.publisher = "CN=Other Publisher".into();
    assert!(entry.validate("codex").is_err());
}

#[test]
fn codex_identity_rejects_bundled_cli_and_native_host() {
    for relative in [
        r"app\resources\codex.exe",
        r"app\resources\codex-command-runner.exe",
        r"..\ChatGPT.exe",
        r"app\..\ChatGPT.exe",
    ] {
        let mut entry = installed();
        entry.relative = relative.into();
        entry.executable = format!(r"{}\{}", entry.root, relative);
        assert!(entry.validate("codex").is_err());
    }
}

#[test]
fn codex_identity_rejects_wrong_family_or_app() {
    let mut entry = installed();
    entry.family = Some("OtherPackage_family".into());
    assert!(entry.validate("codex").is_err());
    entry.family = Some("OpenAI.Codex_2p2nqsd0c76g0".into());
    entry.app_id = Some("CodexCoreCommandRunner".into());
    assert!(entry.validate("codex").is_err());
}

#[test]
fn windows_paths_are_exact_and_case_insensitive() {
    assert!(same_path(
        r"\\?\C:\Program Files\App",
        "c:/program files/app/"
    ));
    assert!(!same_path(r"C:\App", r"C:\App-Other"));
    assert!(!same_path("", ""));
}

#[test]
fn windows_sources_are_fixed_official_architecture_links() {
    assert_eq!(
        package_url("x86_64").unwrap(),
        "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-x64.msix"
    );
    assert_eq!(
        package_url("aarch64").unwrap(),
        "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-arm64.msix"
    );
    assert!(package_url("x86").is_err());
    assert_eq!(STORE_URL, "https://apps.microsoft.com/detail/9PLM9XGG6VKS");
}

#[test]
fn desktop_session_protocol_does_not_accept_url_syntax() {
    assert_eq!(
        session_url("019aa123-abcd-7000-8000-123456789abc").unwrap(),
        "codex://threads/019aa123-abcd-7000-8000-123456789abc"
    );
    for id in [
        "",
        "-latest",
        "id?target=other",
        "../thread",
        "id#fragment",
        "id\";exit",
        "id\n",
    ] {
        assert!(session_url(id).is_err());
    }
}

#[test]
fn powershell_values_are_single_literal_arguments() {
    assert_eq!(
        ps_literal("C:\\folder's\\file.exe"),
        "'C:\\folder''s\\file.exe'"
    );
    assert_eq!(ps_literal("$(not-a-command)"), "'$(not-a-command)'");
}
