//! Read only the compiled context. No app/runtime, directories, settings,
//! database, credentials, logging initialization or network belongs here.
use crate::update_distribution;
use serde::Serialize;
use std::{ffi::OsString, io::Write};
use tauri::Config;

const FLAG: &str = "--osir-release-diagnostics";

#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Normal,
    Diagnostics,
    Invalid,
}

fn invocation(args: &[OsString]) -> Invocation {
    if args.len() == 1 && args[0] == FLAG {
        Invocation::Diagnostics
    } else if args
        .iter()
        .any(|arg| arg.as_encoded_bytes().starts_with(FLAG.as_bytes()))
    {
        Invocation::Invalid
    } else {
        Invocation::Normal
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseDiagnostics {
    schema_version: u8,
    identifier: String,
    version: Option<String>,
    package_version: String,
    release_build: bool,
    bundle_type: Option<String>,
    updater: PublicUpdater,
    release_marker: Option<ReleaseMarker>,
    update_distribution: update_distribution::UpdateDistribution,
}

#[derive(Serialize)]
struct PublicUpdater {
    pubkey: Option<String>,
    endpoints: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseMarker {
    schema_version: u8,
    channel: String,
    bundle_artifacts: bool,
}

pub(crate) fn report(
    config: &Config,
    package_version: &str,
    bundle_type: Option<String>,
) -> ReleaseDiagnostics {
    let updater = config.plugins.0.get("updater");
    let pubkey = updater
        .and_then(|value| value.get("pubkey"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| update_distribution::public_key(value).is_some())
        .map(str::to_owned);
    // Never echo arbitrary plugin JSON, credentials in URLs, unknown fields or
    // invalid keys. Invalid public fields become null and fail the build check.
    let endpoints = updater
        .and_then(|value| value.get("endpoints"))
        .and_then(serde_json::Value::as_array)
        .filter(|values| values.len() <= 12)
        .and_then(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| update_distribution::valid_endpoint(value))
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<_>>>()
        });
    let release_marker = config
        .plugins
        .0
        .get("osir-updates")
        .and_then(serde_json::Value::as_object)
        .filter(|marker| marker.len() == 3)
        .and_then(|marker| {
            let channel = marker.get("channel")?.as_str()?;
            if marker.get("schemaVersion")?.as_u64()? != 1
                || !matches!(channel, "prod" | "beta" | "dev")
            {
                return None;
            }
            Some(ReleaseMarker {
                schema_version: 1,
                channel: channel.to_owned(),
                bundle_artifacts: marker.get("bundleArtifacts")?.as_bool()?,
            })
        });
    ReleaseDiagnostics {
        schema_version: 1,
        identifier: config.identifier.clone(),
        version: config.version.clone(),
        package_version: package_version.to_owned(),
        release_build: !cfg!(debug_assertions),
        bundle_type,
        updater: PublicUpdater { pubkey, endpoints },
        release_marker,
        // The IPC get_update_distribution command calls this same function.
        update_distribution: update_distribution::capability(config),
    }
}

fn write_report(writer: &mut impl Write, report: &impl Serialize) -> Result<(), ()> {
    serde_json::to_writer(&mut *writer, report).map_err(|_| ())?;
    writer.write_all(b"\n").map_err(|_| ())
}

pub(crate) fn dispatch<T: Serialize>(
    args: impl IntoIterator<Item = OsString>,
    create_report: impl FnOnce() -> T,
) -> Option<i32> {
    let args: Vec<_> = args.into_iter().collect();
    match invocation(&args) {
        Invocation::Normal => None,
        Invocation::Invalid => {
            eprintln!("Release diagnostics requires exactly one --osir-release-diagnostics flag.");
            Some(64)
        }
        Invocation::Diagnostics => {
            let result = write_report(&mut std::io::stdout().lock(), &create_report());
            if result.is_err() {
                eprintln!("Release diagnostics output failed.");
            }
            Some(if result.is_ok() { 0 } else { 1 })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::json;

    #[test]
    fn exact_flag_only_and_invalid_invocations_never_construct_context() {
        assert_eq!(invocation(&[FLAG.into()]), Invocation::Diagnostics);
        for args in [
            vec![FLAG.into(), "extra".into()],
            vec!["extra".into(), FLAG.into()],
            vec![FLAG.into(), FLAG.into()],
            vec![format!("{FLAG}=true").into()],
            vec![format!("{FLAG}-extra").into()],
        ] {
            assert_eq!(invocation(&args), Invocation::Invalid);
            assert_eq!(
                dispatch(args, || -> serde_json::Value {
                    panic!("must not generate context")
                }),
                Some(64)
            );
        }
        for args in [
            vec![],
            vec!["osir-client://callback".into()],
            vec!["--help".into()],
        ] {
            assert_eq!(
                dispatch(args, || -> serde_json::Value {
                    panic!("must not generate context")
                }),
                None
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn malformed_non_utf8_diagnostics_never_falls_through_to_startup() {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = FLAG.as_bytes().to_vec();
        bytes.push(0xff);
        assert_eq!(
            invocation(&[OsString::from_vec(bytes)]),
            Invocation::Invalid
        );
    }

    #[test]
    fn reports_only_safe_compiled_public_fields_and_actual_capability() {
        let key = STANDARD.encode("untrusted comment: fixture public key\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n");
        let mut config = Config::default();
        config.identifier = "com.osir.client".into();
        config.version = Some("0.1.1".into());
        config.plugins.0.insert("updater".into(), json!({
            "pubkey": key, "endpoints": ["https://osirclaw.com/api/v1/osir-client/updater/prod.json"],
            "secret": "must-never-be-output"
        }));
        config.plugins.0.insert(
            "osir-updates".into(),
            json!({
                "schemaVersion": 1, "channel": "prod", "bundleArtifacts": true
            }),
        );
        config
            .plugins
            .0
            .insert("private".into(), json!({"token": "must-never-be-output"}));
        let output = serde_json::to_value(report(&config, "0.1.1", Some("app".into()))).unwrap();
        assert_eq!(output["identifier"], config.identifier);
        assert_eq!(output["version"], "0.1.1");
        assert_eq!(output["packageVersion"], "0.1.1");
        assert_eq!(output["updater"]["pubkey"], key);
        assert_eq!(output["releaseMarker"]["bundleArtifacts"], true);
        assert_eq!(
            output["updateDistribution"],
            serde_json::to_value(update_distribution::capability(&config)).unwrap()
        );
        assert!(!output.to_string().contains("must-never-be-output"));
        assert!(!output.to_string().contains("secret"));
        assert_eq!(output.as_object().unwrap().len(), 9);
    }

    #[test]
    fn malformed_public_fields_and_unknown_marker_cannot_disclose_secrets() {
        let mut config = Config::default();
        config.plugins.0.insert("updater".into(), json!({
            "pubkey": "secret-invalid-key", "endpoints": ["https://user:secret@osirclaw.com/update?token=secret"]
        }));
        config.plugins.0.insert(
            "osir-updates".into(),
            json!({
                "schemaVersion": 1, "channel": "prod", "bundleArtifacts": true, "token": "secret"
            }),
        );
        let output = serde_json::to_value(report(&config, "0.1.1", None)).unwrap();
        assert!(output["updater"]["pubkey"].is_null());
        assert!(output["updater"]["endpoints"].is_null());
        assert!(output["releaseMarker"].is_null());
        assert!(!output.to_string().contains("secret"));
        let mut bytes = Vec::new();
        write_report(&mut bytes, &output).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            output
        );
        assert_eq!(bytes.last(), Some(&b'\n'));
    }
}
