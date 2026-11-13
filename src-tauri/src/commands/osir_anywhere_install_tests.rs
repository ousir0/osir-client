//! Synthetic, headless fixtures only. No real packages, installers or user apps.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use rsa::{
    pkcs1v15::SigningKey,
    pkcs8::EncodePublicKey,
    signature::{SignatureEncoding, Signer},
    RsaPrivateKey,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn release_value() -> Value {
    json!({"schemaVersion":1,"productId":"osir-anywhere","channel":"stable",
        "releaseId":"2.1.1","version":"2.1.1","publishedAt":"2026-01-01T00:00:00Z",
        "artifacts":[{"platform":"darwin","arch":"universal",
            "url":"https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/OSIR-Anywhere.dmg",
            "size":1024,"sha256":"ab".repeat(32),"filename":"OSIR-Anywhere.dmg"}]})
}

fn signing_key() -> &'static RsaPrivateKey {
    static KEY: std::sync::OnceLock<RsaPrivateKey> = std::sync::OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap())
}

fn signed(value: Value) -> (Vec<u8>, String) {
    let payload = serde_json::to_vec(&value).unwrap();
    let key = signing_key();
    let signature = SigningKey::<Sha256>::new(key.clone()).sign(&payload);
    let envelope = json!({"payload":STANDARD.encode(payload),"signature":STANDARD.encode(signature.to_bytes())});
    (
        serde_json::to_vec(&envelope).unwrap(),
        key.to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap(),
    )
}

#[test]
fn anywhere_release_verifies_exact_signed_payload_and_packaged_public_key() {
    use rsa::pkcs8::DecodePublicKey;
    rsa::RsaPublicKey::from_public_key_pem(manifest::PUBLIC_KEY).unwrap();
    let (bytes, key) = signed(release_value());
    let release = manifest::verify(&bytes, &key).unwrap();
    assert_eq!(
        release.select("darwin", "arm64", "0.1.9").unwrap().arch,
        "universal"
    );
    assert!(release.select("win32", "arm64", "0.1.9").is_err());
    let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let mut payload = STANDARD
        .decode(envelope["payload"].as_str().unwrap())
        .unwrap();
    payload.push(b' '); // Even valid equivalent JSON must have an authentic signature.
    envelope["payload"] = STANDARD.encode(payload).into();
    assert!(manifest::verify(&serde_json::to_vec(&envelope).unwrap(), &key).is_err());
    assert!(manifest::verify(&bytes, manifest::PUBLIC_KEY).is_err());
}

#[test]
fn anywhere_release_system_signature_policy_is_authenticated_and_platform_specific() {
    use manifest::SystemSignature::*;
    for (platform, declared, expected) in [
        ("darwin", None, DeveloperIdNotarized),
        ("win32", None, Authenticode),
        ("darwin", Some("unsigned"), Unsigned),
        ("win32", Some("unsigned"), Unsigned),
        (
            "darwin",
            Some("developer-id-notarized"),
            DeveloperIdNotarized,
        ),
        ("win32", Some("authenticode"), Authenticode),
    ] {
        let mut value = release_value();
        let artifact = &mut value["artifacts"][0];
        artifact["platform"] = platform.into();
        artifact["arch"] = "x64".into();
        if platform == "win32" {
            artifact["filename"] = "OSIR-Anywhere.exe".into();
            artifact["url"]="https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/OSIR-Anywhere.exe".into();
        }
        if let Some(declared) = declared {
            artifact["systemSignature"] = declared.into();
        }
        let (bytes, key) = signed(value);
        let release = manifest::verify(&bytes, &key).unwrap();
        assert_eq!(release.artifacts[0].signature_policy().unwrap(), expected);
    }
    for invalid in ["authenticode", "ad-hoc", "unknown", ""] {
        let mut value = release_value();
        value["artifacts"][0]["systemSignature"] = invalid.into();
        let (bytes, key) = signed(value);
        assert!(manifest::verify(&bytes, &key).is_err());
    }
    let mut value = release_value();
    value["artifacts"][0]["systemSignature"] = "developer-id-notarized".into();
    let (bytes, key) = signed(value);
    let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let mut changed: Value = serde_json::from_slice(
        &STANDARD
            .decode(envelope["payload"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    changed["artifacts"][0]["systemSignature"] = "unsigned".into();
    envelope["payload"] = STANDARD
        .encode(serde_json::to_vec(&changed).unwrap())
        .into();
    assert!(manifest::verify(&serde_json::to_vec(&envelope).unwrap(), &key).is_err());
    let (bytes, key) = signed(changed);
    let release = manifest::verify(&bytes, &key).unwrap();
    assert_eq!(release.artifacts[0].signature_policy().unwrap(), Unsigned);
    let result = AnywhereReleaseStatus {
        status: "ready",
        version: Some(release.version),
        platform: Some("darwin".into()),
        arch: Some("universal".into()),
        size: Some(1024),
        error: None,
        system_signature: Some("unsigned"),
    };
    assert_eq!(
        serde_json::to_value(result).unwrap()["systemSignature"],
        "unsigned"
    );
    let mut artifact = release.artifacts[0].clone();
    artifact.platform = "win32".into();
    artifact.system_signature = Some(DeveloperIdNotarized);
    assert!(artifact.signature_policy().is_err());
}

#[test]
fn anywhere_install_unsigned_is_not_a_fallback_for_bad_or_unknown_system_signatures() {
    use manifest::SystemSignature::*;
    use signature::MacSignature::{AdHoc, DeveloperId, OtherSigned};
    assert_eq!(
        signature::macos_integrity(-67062, false, false).unwrap(),
        signature::MacSignature::Unsigned
    );
    assert_eq!(signature::macos_integrity(0, true, true).unwrap(), AdHoc);
    assert_eq!(
        signature::macos_integrity(0, true, false).unwrap(),
        OtherSigned
    );
    assert!(signature::macos_policy(
        manifest::SystemSignature::Unsigned,
        signature::MacSignature::Unsigned,
        false
    )
    .is_ok());
    assert!(signature::macos_policy(manifest::SystemSignature::Unsigned, AdHoc, false).is_ok());
    assert!(signature::macos_policy(DeveloperIdNotarized, DeveloperId, true).is_ok());
    for code in [-67022, -67061, -67056, -67671, -67030, -50, 1] {
        for identifier in [false, true] {
            for adhoc in [false, true] {
                assert!(signature::macos_integrity(code, identifier, adhoc).is_err());
            }
        }
    }
    assert!(signature::macos_integrity(-67062, true, false).is_err());
    assert!(signature::macos_integrity(0, false, false).is_err());
    for actual in [signature::MacSignature::Unsigned, AdHoc, OtherSigned] {
        assert!(signature::macos_policy(DeveloperIdNotarized, actual, true).is_err());
    }
    assert!(signature::macos_policy(DeveloperIdNotarized, DeveloperId, false).is_err());
    for actual in [DeveloperId, OtherSigned] {
        assert!(
            signature::macos_policy(manifest::SystemSignature::Unsigned, actual, false).is_err()
        );
    }
    assert!(signature::windows_policy(Unsigned, "NotSigned", false, "OSIR Anywhere").is_ok());
    assert!(signature::windows_policy(Authenticode, "Valid", true, "OSIR Anywhere").is_ok());
    for status in [
        "HashMismatch",
        "NotTrusted",
        "UnknownError",
        "NotSupportedFileFormat",
        "Incompatible",
        "",
    ] {
        for policy in [Unsigned, Authenticode] {
            assert!(signature::windows_policy(policy, status, false, "OSIR Anywhere").is_err());
        }
    }
    assert!(signature::windows_policy(Unsigned, "NotSigned", true, "OSIR Anywhere").is_err());
    assert!(signature::windows_policy(Unsigned, "Valid", true, "OSIR Anywhere").is_err());
    assert!(signature::windows_policy(Authenticode, "NotSigned", false, "OSIR Anywhere").is_err());
    assert!(signature::windows_policy(Authenticode, "Valid", false, "OSIR Anywhere").is_err());
    assert!(signature::windows_policy(Unsigned, "NotSigned", false, "Agents Anywhere").is_err());
}

fn fake_nsis(certificate: bool) -> Vec<u8> {
    let mut bytes = vec![0; if certificate { 2056 } else { 2048 }];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
    bytes[128..132].copy_from_slice(b"PE\0\0");
    bytes[132..134].copy_from_slice(&0x14cu16.to_le_bytes());
    bytes[134..136].copy_from_slice(&1u16.to_le_bytes());
    bytes[148..150].copy_from_slice(&224u16.to_le_bytes());
    bytes[150..152].copy_from_slice(&2u16.to_le_bytes());
    bytes[152..154].copy_from_slice(&0x10bu16.to_le_bytes());
    bytes[244..248].copy_from_slice(&16u32.to_le_bytes());
    // One PE section [512,1024), followed by a synthetic NSIS overlay.
    bytes[392..396].copy_from_slice(&512u32.to_le_bytes());
    bytes[396..400].copy_from_slice(&512u32.to_le_bytes());
    bytes[1028..1044].copy_from_slice(b"\xef\xbe\xad\xdeNullsoftInst");
    bytes[1044..1048].copy_from_slice(&1u32.to_le_bytes());
    bytes[1048..1052].copy_from_slice(&1024u32.to_le_bytes());
    if certificate {
        bytes[280..284].copy_from_slice(&2048u32.to_le_bytes());
        bytes[284..288].copy_from_slice(&8u32.to_le_bytes());
        bytes[2048..2052].copy_from_slice(&8u32.to_le_bytes());
    }
    bytes
}

#[test]
fn anywhere_install_pe_nsis_requires_real_structure_and_distinguishes_certificate_presence() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    for present in [false, true] {
        std::fs::write(fixture.path(), fake_nsis(present)).unwrap();
        assert_eq!(
            signature::pe_nsis_certificate(fixture.path()).unwrap(),
            present
        );
    }
    for mutation in 0..7 {
        let mut bytes = fake_nsis(false);
        match mutation {
            0 => bytes[1028] = 0, // generic PE with no NSIS header
            1 => bytes[1048..1052].copy_from_slice(&4096u32.to_le_bytes()),
            2 => bytes[280..284].copy_from_slice(&2048u32.to_le_bytes()), // half-empty certificate table
            3 => bytes[284..288].copy_from_slice(&8u32.to_le_bytes()),
            4 => bytes[1024] = 1, // uninstaller, not the installer
            5 => bytes[150..152].copy_from_slice(&0u16.to_le_bytes()),
            _ => bytes.truncate(1025),
        }
        std::fs::write(fixture.path(), bytes).unwrap();
        assert!(
            signature::pe_nsis_certificate(fixture.path()).is_err(),
            "mutation {mutation}"
        );
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn anywhere_install_preserves_quarantine_and_verifies_bytes_after_marking() {
    use std::os::fd::AsRawFd;
    let cache = io::private_cache().await.unwrap();
    let path = cache.path().join("origin.dmg");
    let bytes = fake_dmg();
    std::fs::write(&path, &bytes).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let artifact = artifact(
        "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/origin.dmg".into(),
        &bytes,
    );
    io::mark_download_origin(&path, &artifact).await.unwrap();
    let file = io::pin_file(&path).unwrap();
    let mut before = vec![0u8; 1024];
    let length = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"com.apple.quarantine".as_ptr(),
            before.as_mut_ptr().cast(),
            before.len(),
            0,
            0,
        )
    };
    assert!(length > 0);
    before.truncate(length as usize);
    assert!(String::from_utf8_lossy(&before).starts_with("0081;"));
    io::mark_download_origin(&path, &artifact).await.unwrap();
    let mut after = vec![0u8; 1024];
    let length = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"com.apple.quarantine".as_ptr(),
            after.as_mut_ptr().cast(),
            after.len(),
            0,
            0,
        )
    };
    assert!(length > 0);
    after.truncate(length as usize);
    assert_eq!(before, after);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[cfg(windows)]
#[tokio::test]
async fn anywhere_install_private_cache_protects_directory_and_inherited_file_acl() {
    use super::super::osir_tools::windows_support::{powershell, ps_literal};
    let cache = io::private_cache().await.unwrap();
    let file = cache.path().join("acl-probe");
    std::fs::write(&file, b"non-secret ACL fixture").unwrap();
    // Use the same fixed Windows PowerShell host as production. The Windows
    // CI pwsh -> cargo -> powershell chain retains the incompatible module path;
    // neither cache protection nor this read-back may depend on module autoload.
    let script = format!(
        r#"
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$directory = [IO.Directory]::GetAccessControl({})
$file = [IO.File]::GetAccessControl({})
if (-not $directory.AreAccessRulesProtected -or $directory.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid) {{ throw 'directory protection' }}
foreach ($entry in @($directory, $file)) {{
  $rules = @($entry.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
  if ($rules.Count -ne 2) {{ throw 'unexpected grant count' }}
  foreach ($rule in $rules) {{
    if ($rule.IdentityReference.Value -notin @($sid, 'S-1-5-18') -or $rule.AccessControlType -ne 'Allow' -or $rule.FileSystemRights -ne 'FullControl') {{ throw 'unexpected grant' }}
  }}
  foreach ($who in @($sid, 'S-1-5-18')) {{
    if (@($rules | Where-Object {{ $_.IdentityReference.Value -eq $who }}).Count -ne 1) {{ throw 'missing principal' }}
  }}
}}
foreach ($rule in $directory.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {{
  if ($rule.IsInherited -or $rule.InheritanceFlags -ne 'ContainerInherit,ObjectInherit' -or $rule.PropagationFlags -ne 'None') {{ throw 'directory inheritance' }}
}}
foreach ($rule in $file.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {{
  if (-not $rule.IsInherited) {{ throw 'file inheritance' }}
}}
'true'
"#,
        ps_literal(&cache.path().to_string_lossy()),
        ps_literal(&file.to_string_lossy()),
    );
    assert_eq!(
        powershell(&script, std::time::Duration::from_secs(15))
            .await
            .unwrap(),
        json!(true)
    );
}

#[cfg(windows)]
#[tokio::test]
async fn anywhere_install_preserves_windows_internet_zone_without_modifying_package_bytes() {
    let cache = io::private_cache().await.unwrap();
    let path = cache.path().join("origin.exe");
    let bytes = fake_nsis(false);
    std::fs::write(&path, &bytes).unwrap();
    let mut artifact = artifact(
        "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/origin.exe".into(),
        &bytes,
    );
    artifact.platform = "win32".into();
    artifact.filename = "origin.exe".into();
    io::mark_download_origin(&path, &artifact).await.unwrap();
    let mut zone = path.as_os_str().to_os_string();
    zone.push(":Zone.Identifier");
    let zone = std::path::PathBuf::from(zone);
    assert!(std::fs::read_to_string(&zone).unwrap().contains("ZoneId=3"));
    std::fs::write(&zone, "[ZoneTransfer]\r\nZoneId=4\r\n").unwrap();
    io::mark_download_origin(&path, &artifact).await.unwrap();
    assert!(std::fs::read_to_string(&zone).unwrap().contains("ZoneId=4"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn anywhere_release_rejects_signed_wrong_identity_and_unsafe_objects() {
    let original = release_value();
    for (field, value) in [
        ("schemaVersion", json!(2)),
        ("productId", json!("osir-client")),
        ("channel", json!("beta")),
        ("releaseId", json!("..")),
        ("version", json!("2.1.1-beta")),
        ("publishedAt", json!("2099-01-01T00:00:00Z")),
    ] {
        let mut value_under_test = original.clone();
        value_under_test[field] = value;
        let (bytes, key) = signed(value_under_test);
        assert!(manifest::verify(&bytes, &key).is_err(), "{field}");
    }
    for url in [
        "http://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/OSIR-Anywhere.dmg",
        "https://osirvedio.cn-nb1.rains3.com:443/osir-anywhere/releases/2.1.1/OSIR-Anywhere.dmg",
        "https://osirvedio.cn-nb1.rains3.com/osir-client/releases/2.1.1/OSIR-Anywhere.dmg",
        "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/other/OSIR-Anywhere.dmg",
        "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/OSIR-Anywhere.dmg?token=x",
        "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/OSIR-Anywhere.dmg#x",
        "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.1.1/%4fSIR-Anywhere.dmg",
    ] {
        let mut value = original.clone(); value["artifacts"][0]["url"] = url.into();
        let (bytes,key) = signed(value);
        assert!(manifest::verify(&bytes,&key).is_err(),"{url}");
    }
    let mut duplicate = original;
    let first = duplicate["artifacts"][0].clone();
    duplicate["artifacts"].as_array_mut().unwrap().push(first);
    let (bytes, key) = signed(duplicate);
    assert!(manifest::verify(&bytes, &key).is_err());
}

#[test]
fn anywhere_release_uses_exact_arch_and_enforces_client_and_server_versions() {
    let mut value = release_value();
    value["minimumClientVersion"] = "0.2.0".into();
    let release: manifest::Release = serde_json::from_value(value).unwrap();
    assert!(release.select("darwin", "x64", "0.1.9").is_err());
    assert!(release.select("darwin", "x64", "0.2.0").is_ok());
    assert!(release.select("win32", "x64", "0.2.0").is_err());
    for (body, minimum, accepted) in [
        (
            r#"{"status":"ok","version":"2.1.0","serverTime":"fixture"}"#,
            "2.1.0",
            true,
        ),
        (r#"{"status":"ok","version":"2.1.10"}"#, "2.1.2", true),
        (r#"{"status":"ok","version":"2.1.0"}"#, "2.1.1", false),
        (r#"{"status":"down","version":"99.0.0"}"#, "2.1.0", false),
        (
            r#"{"status":"ok","desktopVersion":"99.0.0"}"#,
            "2.1.0",
            false,
        ),
        (r#"{"status":"ok","version":"2.1.0-beta"}"#, "2.1.0", false),
        ("<html>404</html>", "2.1.0", false),
    ] {
        assert_eq!(
            manifest::check_health(body.as_bytes(), minimum).is_ok(),
            accepted,
            "{body}"
        );
    }
    assert!(manifest::check_health(&vec![b' '; 16385], "2.1.0").is_err());
}

#[test]
fn anywhere_install_contract_and_cancellation_boundary() {
    let state = OsirAnywhereInstaller::default();
    assert_eq!(
        serde_json::to_value(state.status()).unwrap(),
        json!({"phase":"idle","downloadedBytes":0,"totalBytes":null,"error":null})
    );
    let (send, mut receive) = tokio::sync::watch::channel(false);
    {
        let mut inner = state.lock();
        inner.active = true;
        inner.cancel = Some(send);
        inner.status.phase = AnywhereInstallPhase::Downloading;
    }
    assert_eq!(state.cancel().phase, AnywhereInstallPhase::Cancelled);
    assert!(*receive.borrow_and_update());
    assert!(state.begin_opening().is_err());
    state.finish(Err("cancelled".into()));
    assert!(!state.lock().active);
    assert_eq!(state.status().phase, AnywhereInstallPhase::Cancelled);
    {
        let mut inner = state.lock();
        inner.active = true;
        inner.status.phase = AnywhereInstallPhase::Opening;
    }
    assert_eq!(state.cancel().phase, AnywhereInstallPhase::Opening);
    state.finish(Ok(()));
    assert_eq!(state.status().phase, AnywhereInstallPhase::Opening);
    let result = AnywhereReleaseStatus {
        status: "not_configured",
        version: None,
        platform: Some("darwin".into()),
        arch: Some("arm64".into()),
        size: None,
        error: None,
        system_signature: None,
    };
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"status":"not_configured","version":null,"platform":"darwin","arch":"arm64","size":null,"error":null})
    );
}

fn fake_dmg() -> Vec<u8> {
    let mut bytes = vec![0; 1024];
    bytes[512..516].copy_from_slice(b"koly");
    bytes
}

async fn fixture_server(
    status: &str,
    body: Vec<u8>,
    content_type: &str,
    slow: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fixture.dmg", listener.local_addr().unwrap());
    let headers=format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nLocation: https://example.invalid/other.dmg\r\nConnection: close\r\n\r\n",body.len());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let _ = socket.read(&mut request).await;
        if socket.write_all(headers.as_bytes()).await.is_err() {
            return;
        }
        if slow {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        let _ = socket.write_all(&body).await;
    });
    (url, task)
}
fn artifact(url: String, bytes: &[u8]) -> manifest::Artifact {
    manifest::Artifact {
        platform: "darwin".into(),
        arch: "universal".into(),
        url,
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        filename: "fixture.dmg".into(),
        system_signature: None,
    }
}
fn fixture_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[tokio::test]
async fn anywhere_install_download_verifies_bytes_reuses_cache_and_rejects_mutation() {
    let bytes = fake_dmg();
    let (url, server) =
        fixture_server("200 OK", bytes.clone(), "application/octet-stream", false).await;
    let artifact = artifact(url, &bytes);
    let cache = io::private_cache().await.unwrap();
    let installer = OsirAnywhereInstaller::default();
    let (_send, mut cancel) = tokio::sync::watch::channel(false);
    let path = io::download_with_client(
        &installer,
        cache.path(),
        &artifact,
        &mut cancel,
        fixture_client(),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // Server is gone: success now proves validated cache reuse without transport.
    assert_eq!(
        io::download_with_client(
            &installer,
            cache.path(),
            &artifact,
            &mut cancel,
            fixture_client()
        )
        .await
        .unwrap(),
        path
    );
    std::fs::write(&path, vec![0; 1024]).unwrap();
    assert!(io::verify_file(&path, &artifact, &mut cancel)
        .await
        .is_err());
}

#[tokio::test]
async fn anywhere_install_rejects_redirect_error_html_hash_and_truncation() {
    let good = fake_dmg();
    for (status, body, kind) in [
        ("302 Found", good.clone(), "application/octet-stream"),
        ("404 Not Found", good.clone(), "application/octet-stream"),
        ("200 OK", good.clone(), "text/html"),
        ("200 OK", vec![0; 1024], "application/octet-stream"),
        ("200 OK", vec![0; 512], "application/octet-stream"),
    ] {
        let (url, server) = fixture_server(status, body, kind, false).await;
        let artifact = artifact(url, &good);
        let cache = io::private_cache().await.unwrap();
        let (_send, mut cancel) = tokio::sync::watch::channel(false);
        assert!(
            io::download_with_client(
                &OsirAnywhereInstaller::default(),
                cache.path(),
                &artifact,
                &mut cancel,
                fixture_client()
            )
            .await
            .is_err(),
            "{status} {kind}"
        );
        server.await.unwrap();
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn anywhere_install_cancellation_interrupts_transport_and_removes_partial() {
    let bytes = fake_dmg();
    let (url, server) =
        fixture_server("200 OK", bytes.clone(), "application/octet-stream", true).await;
    let artifact = artifact(url, &bytes);
    let cache = io::private_cache().await.unwrap();
    let (send, mut cancel) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        send.send(true).unwrap();
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        io::download_with_client(
            &OsirAnywhereInstaller::default(),
            cache.path(),
            &artifact,
            &mut cancel,
            fixture_client(),
        ),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("取消"));
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    server.abort();
}

#[cfg(target_os = "macos")]
#[test]
fn anywhere_install_dmg_requires_unique_contained_bundle_and_exact_release_identity() {
    let root = tempfile::tempdir().unwrap();
    assert!(signature::macos::mounted_app(root.path()).is_err());
    let app = root.path().join("OSIR Anywhere.app");
    std::fs::create_dir(&app).unwrap();
    assert_eq!(
        signature::macos::mounted_app(root.path()).unwrap(),
        app.canonicalize().unwrap()
    );
    std::fs::create_dir(root.path().join("Other.app")).unwrap();
    assert!(signature::macos::mounted_app(root.path()).is_err());
    std::fs::remove_dir(root.path().join("Other.app")).unwrap();
    std::fs::remove_dir(&app).unwrap();
    std::os::unix::fs::symlink("/Applications", &app).unwrap();
    assert!(signature::macos::mounted_app(root.path()).is_err());
    let valid = json!({"CFBundleIdentifier":"com.osir.anywhere.desktop","CFBundleName":"OSIR Anywhere","CFBundleDisplayName":"OSIR Anywhere","CFBundleExecutable":"OSIR Anywhere","CFBundlePackageType":"APPL","CFBundleShortVersionString":"0.1.10"});
    signature::macos::metadata(&valid, "0.1.10").unwrap();
    assert!(signature::macos::metadata(&valid, "0.1.11").is_err());
    for (key, value) in [
        ("CFBundleIdentifier", "com.anywhere.desktop"),
        ("CFBundleName", "Agents Anywhere"),
        ("CFBundleDisplayName", "OSIR Client"),
        ("CFBundleExecutable", "../Other"),
        ("CFBundlePackageType", "BNDL"),
    ] {
        let mut wrong = valid.clone();
        wrong[key] = value.into();
        assert!(signature::macos::metadata(&wrong, "0.1.10").is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn anywhere_install_dmg_package_identity_is_bounded_and_not_a_plist_lookalike() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    for mutation in 0..6 {
        let mut product = json!({"name":"osir-anywhere-desktop-workbench","productName":"OSIR Anywhere","main":"dist/electron/main.js","version":"0.1.10"});
        match mutation {
            1 => product["name"] = "agents-anywhere-desktop-workbench".into(),
            2 => product["productName"] = "OSIR Client".into(),
            3 => product["version"] = "0.1.9".into(),
            _ => {}
        }
        let product = serde_json::to_vec(&product).unwrap();
        let mut entry = json!({"size":product.len(),"offset":"0"});
        if mutation == 4 {
            entry["link"] = "../Other".into();
        }
        if mutation == 5 {
            entry["offset"] = "18446744073709551615".into();
        }
        let header = serde_json::to_vec(&json!({"files":{"package.json":entry}})).unwrap();
        let header_size = header.len() as u32 + 8;
        let mut bytes = Vec::new();
        for n in [4u32, header_size, header_size - 4, header.len() as u32] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.extend(header);
        bytes.extend(product);
        std::fs::write(fixture.path(), &bytes).unwrap();
        assert_eq!(
            signature::macos::package(fixture.path(), "0.1.10").is_ok(),
            mutation == 0
        );
    }
}
