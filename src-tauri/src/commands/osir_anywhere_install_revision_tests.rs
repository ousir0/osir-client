//! Narrow tests import the real signed manifest and installed ASAR reader.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use rsa::{
    pkcs1v15::SigningKey,
    pkcs8::EncodePublicKey,
    signature::{SignatureEncoding, Signer},
    RsaPrivateKey,
};
use serde_json::json;
use sha2::Sha256;

fn info(revision: Value) -> Value {
    json!({"productId":"osir-anywhere","desktopVersion":"2.2.1", "serverVersion":"2.2.0", "revision":revision})
}

fn signed(value: Value) -> (Vec<u8>, String) {
    static KEY: std::sync::OnceLock<RsaPrivateKey> = std::sync::OnceLock::new();
    let key = KEY.get_or_init(|| RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap());
    let payload = serde_json::to_vec(&value).unwrap();
    let signature = SigningKey::<Sha256>::new(key.clone()).sign(&payload);
    (
        serde_json::to_vec(&json!({"payload": STANDARD.encode(payload),
        "signature": STANDARD.encode(signature.to_bytes())}))
        .unwrap(),
        key.to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap(),
    )
}

fn release() -> Value {
    json!({"schemaVersion":1,"productId":"osir-anywhere","channel":"stable",
        "releaseId":"2.2.1.build.7","version":"2.2.1","publishedAt":"2026-01-01T00:00:00Z",
        "minimumClientVersion":"0.1.11",
        "artifacts":[{"platform":"darwin","arch":"universal",
            "url":"https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/2.2.1.build.7/OSIR-Anywhere.dmg",
            "size":1024,"sha256":"ab".repeat(32),"filename":"OSIR-Anywhere.dmg"}]})
}

#[test]
fn signed_revision_defaults_zero_and_accepts_only_bounded_u32() {
    let (bytes, pem) = signed(release());
    assert_eq!(manifest::verify(&bytes, &pem).unwrap().revision, 0);
    for revision in [0, 1, 7, 65535] {
        let mut value = release();
        value["revision"] = revision.into();
        let (bytes, pem) = signed(value);
        assert_eq!(manifest::verify(&bytes, &pem).unwrap().revision, revision);
    }
    for revision in [
        json!(-1),
        json!(65536),
        json!(4294967296u64),
        json!(1.5),
        json!(true),
        json!("7"),
        Value::Null,
    ] {
        let mut value = release();
        value["revision"] = revision;
        let (bytes, pem) = signed(value);
        assert!(manifest::verify(&bytes, &pem).is_err());
    }
}

#[test]
fn revision_is_inside_authenticated_payload_not_envelope_or_other_source() {
    let (bytes, pem) = signed(release());
    let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let mut payload: Value = serde_json::from_slice(
        &STANDARD
            .decode(envelope["payload"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    payload["revision"] = 7.into();
    envelope["payload"] = STANDARD
        .encode(serde_json::to_vec(&payload).unwrap())
        .into();
    assert!(manifest::verify(&serde_json::to_vec(&envelope).unwrap(), &pem).is_err());
    let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
    envelope["revision"] = 7.into();
    assert!(manifest::verify(&serde_json::to_vec(&envelope).unwrap(), &pem).is_err());
}

#[test]
fn minimum_client_compares_public_base_and_keeps_strict_version_rules() {
    let (bytes, pem) = signed(release());
    let manifest = manifest::verify(&bytes, &pem).unwrap();
    for current in ["0.1.11", "0.1.11+1", "0.1.11+65535", "0.1.12+1"] {
        assert!(
            manifest.select("darwin", "arm64", current).is_ok(),
            "{current}"
        );
    }
    for current in [
        "0.1.10+65535",
        "0.1.11+0",
        "0.1.11+01",
        "0.1.11+65536",
        "0.1.11+beta",
        "0.1.11+1.2",
        "0.1.11+1+2",
        "0.1.11-beta",
        "0.01.11+1",
    ] {
        assert!(
            manifest.select("darwin", "arm64", current).is_err(),
            "{current}"
        );
    }
    assert_eq!(manifest::client_version("0.1.11+9").unwrap(), [0, 1, 11]);
    // Anywhere and Server versions must remain ordinary stable versions.
    assert!(manifest::version("2.2.1+7").is_err());
    let mut value = release();
    value["minimumClientVersion"] = "0.1.11+9".into();
    let (bytes, pem) = signed(value);
    assert!(manifest::verify(&bytes, &pem)
        .unwrap()
        .select("darwin", "arm64", "0.1.11+1")
        .is_ok());
}

#[test]
fn upgrade_order_uses_revision_only_for_equal_public_versions() {
    for (current, current_rev, release, release_rev, expected) in [
        ("2.2.1", 0, "2.2.1", 1, true),
        ("2.2.1", 1, "2.2.1", 2, true),
        ("2.2.1", 9, "2.2.1", 10, true),
        ("2.2.1", 10, "2.2.1", 10, false),
        ("2.2.1", 10, "2.2.1", 9, false),
        ("2.2.1", 10, "2.2.1", 0, false),
        ("2.2.1", 65534, "2.2.1", 65535, true),
        ("2.2.2", 0, "2.2.1", 65535, false),
        ("2.2.1", 65535, "2.2.2", 0, true),
    ] {
        assert_eq!(
            needs_install(current, current_rev, release, release_rev).unwrap(),
            expected
        );
    }
    assert!(needs_install("2.2.1", 65536, "2.2.1", 1).is_err());
    assert!(needs_install("unknown", 0, "2.2.1", 1).is_err());
}

fn asar(root: &Path, relative: &str, build: Option<Value>, mutate: impl FnOnce(&mut Value)) {
    let package = serde_json::to_vec(&json!({"name":"osir-anywhere-desktop-workbench",
        "productName":"OSIR Anywhere","main":"dist/electron/main.js","version":"2.2.1"}))
    .unwrap();
    let mut data = package.clone();
    let mut header = json!({"files":{"package.json":{"offset":"0","size":package.len()}}});
    if let Some(build) = build {
        let bytes = serde_json::to_vec(&build).unwrap();
        header["files"]["dist"] = json!({"files":{"build-info.json":{
            "offset":data.len().to_string(),"size":bytes.len()}}});
        data.extend_from_slice(&bytes);
    }
    mutate(&mut header);
    let header = serde_json::to_vec(&header).unwrap();
    let header_size = (header.len() + 8).next_multiple_of(4);
    let mut bytes = Vec::new();
    for word in [
        4,
        header_size as u32,
        (header_size - 4) as u32,
        header.len() as u32,
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(&header);
    bytes.resize(header_size + 8, 0);
    bytes.extend_from_slice(&data);
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn installed_revision_reads_packed_build_info_for_both_platform_layouts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for relative in ["Contents/Resources/app.asar", "resources/app.asar"] {
        for (build, expected) in [
            (None, 0),
            (Some(json!({"serverVersion":"2.2.0"})), 0),
            (
                Some(json!({"productId":"osir-anywhere", "desktopVersion":"2.2.1"})),
                0,
            ),
            (Some(info(json!(0))), 0),
            (Some(info(json!(7))), 7),
            (Some(info(json!(65535))), 65535),
        ] {
            asar(&root, relative, build, |_| {});
            assert_eq!(read_app(&root, "2.2.1", relative).unwrap(), expected);
            if relative
                == if cfg!(target_os = "windows") {
                    "resources/app.asar"
                } else {
                    "Contents/Resources/app.asar"
                }
            {
                assert_eq!(installed(&root, "2.2.1").unwrap(), expected);
            }
        }
    }
}

#[test]
fn malformed_build_info_is_never_silently_treated_as_legacy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for build in [
        info(json!(-1)),
        info(json!(65536)),
        info(json!(true)),
        info(json!("7")),
        info(json!(null)),
        info(json!(1.5)),
        Value::Null,
        json!([]),
        json!({}),
        json!({"revision":7}),
        json!({"productId":"other", "desktopVersion":"2.2.1", "revision":7}),
        json!({"productId":"osir-anywhere", "desktopVersion":"2.2.2", "revision":7}),
    ] {
        asar(&root, "Contents/Resources/app.asar", Some(build), |_| {});
        assert!(read_app(&root, "2.2.1", "Contents/Resources/app.asar").is_err());
    }
}

#[test]
fn build_revision_is_bound_to_same_app_product_version_and_packed_entry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let relative = "Contents/Resources/app.asar";
    asar(&root, relative, Some(info(json!(7))), |_| {});
    assert!(read_app(&root, "2.2.2", relative).is_err());
    // A nearby sidecar never supplies the revision.
    std::fs::write(
        root.join("Contents/Resources/build-info.json"),
        br#"{"revision":65535}"#,
    )
    .unwrap();
    assert_eq!(read_app(&root, "2.2.1", relative).unwrap(), 7);
    for (key, value) in [
        ("unpacked", json!(true)),
        ("link", json!("other.json")),
        ("offset", json!("18446744073709551615")),
        ("offset", json!("9999999")),
        ("size", json!(262145)),
    ] {
        asar(&root, relative, Some(info(json!(7))), |header| {
            header["files"]["dist"]["files"]["build-info.json"][key] = value;
        });
        assert!(read_app(&root, "2.2.1", relative).is_err(), "{key}");
    }
    for link in ["dist", "package.json"] {
        asar(&root, relative, Some(info(json!(7))), |header| {
            header["files"][link]["link"] = "other".into();
        });
        assert!(read_app(&root, "2.2.1", relative).is_err());
    }
}

#[cfg(unix)]
#[test]
fn linked_app_archive_and_truncated_metadata_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let app = root.join("App");
    let relative = "Contents/Resources/app.asar";
    asar(&app, relative, Some(info(json!(7))), |_| {});
    let alias = root.join("Alias");
    std::os::unix::fs::symlink(&app, &alias).unwrap();
    assert!(read_app(&alias, "2.2.1", relative).is_err());
    let archive = app.join(relative);
    std::fs::rename(&archive, app.join("archive")).unwrap();
    std::os::unix::fs::symlink(app.join("archive"), &archive).unwrap();
    assert!(read_app(&app, "2.2.1", relative).is_err());
    std::fs::remove_file(&archive).unwrap();
    std::fs::write(&archive, [4, 0, 0, 0]).unwrap();
    assert!(read_app(&app, "2.2.1", relative).is_err());
}
