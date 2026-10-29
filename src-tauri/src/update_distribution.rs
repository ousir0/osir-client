//! Update capability comes only from the app's compiled configuration. Never
//! accept a renderer URL, public key, release marker or runtime environment flag.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use tauri::Config;

pub const OSIR_UNCONFIGURED: &str = "OSIR 更新渠道未配置：尚无可用的 OSIR 签名更新源。";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDistribution {
    kind: &'static str,
    updates_enabled: bool,
    reason: Option<&'static str>,
}

fn public_key(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() > 16384 {
        return None;
    }
    let decoded = STANDARD.decode(value).ok()?;
    if STANDARD.encode(&decoded) != value {
        return None;
    }
    let text = std::str::from_utf8(&decoded).ok()?;
    let mut lines = text.trim_end().lines();
    if !lines.next()?.starts_with("untrusted comment: ") {
        return None;
    }
    let encoded = lines.next()?;
    let key = STANDARD.decode(encoded).ok()?;
    if lines.next().is_some()
        || STANDARD.encode(&key) != encoded
        || key.len() != 42
        || !matches!(&key[..2], b"Ed" | b"ED")
    {
        return None;
    }
    Some(key)
}

fn decoded_path(path: &str) -> Option<String> {
    let mut bytes = path.bytes();
    let mut decoded = Vec::with_capacity(path.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

fn valid_endpoint(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 2048
        || value
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || b"\\{}".contains(&b))
    {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let Some(url::Host::Domain(domain)) = url.host() else {
        return false;
    };
    let host = domain.to_ascii_lowercase();
    let Some(path) = decoded_path(url.path()) else {
        return false;
    };
    let path = path.to_ascii_lowercase();
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() == Some(443)
        && host.contains('.')
        && !host.ends_with('.')
        && !["localhost", "ccswitch.io"].contains(&host.as_str())
        && ![".localhost", ".local", ".internal", ".ccswitch.io"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
        && !url.path().ends_with('/')
        && !([
            "github.com",
            "raw.githubusercontent.com",
            "objects.githubusercontent.com",
        ]
        .contains(&host.as_str())
            && path.contains("/farion1231/cc-switch"))
}

fn valid_osir_configuration(config: &Config, channel: &str) -> bool {
    // Tauri's BundleConfig::ToTokens deliberately replaces the build-only
    // create_updater_artifacts with false. The explicit OSIR release wrapper
    // writes this marker together with true and the validated updater fields;
    // plugin configuration survives code generation and is immutable at runtime.
    let Some(marker) = config
        .plugins
        .0
        .get("osir-updates")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if marker.len() != 3
        || marker
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        || marker.get("channel").and_then(serde_json::Value::as_str) != Some(channel)
        || marker
            .get("bundleArtifacts")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return false;
    }
    let Some(updater) = config
        .plugins
        .0
        .get("updater")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if updater.keys().any(|key| key.starts_with("dangerous")) {
        return false;
    }
    let Some(key) = updater
        .get("pubkey")
        .and_then(serde_json::Value::as_str)
        .and_then(public_key)
    else {
        return false;
    };
    let upstream = serde_json::from_str::<serde_json::Value>(include_str!("../tauri.conf.json"))
        .ok()
        .and_then(|value| {
            value
                .get("plugins")?
                .get("updater")?
                .get("pubkey")?
                .as_str()
                .and_then(public_key)
        });
    let Some(upstream) = upstream else {
        return false;
    };
    if key[10..] == upstream[10..] {
        return false;
    }
    let Some(endpoints) = updater
        .get("endpoints")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    endpoints.len() == 1 && endpoints[0].as_str().is_some_and(valid_endpoint)
}

fn capability_for(config: &Config, release: bool) -> UpdateDistribution {
    if config.identifier == "com.ccswitch.desktop" {
        return UpdateDistribution {
            kind: "ccswitch",
            updates_enabled: true,
            reason: None,
        };
    }
    if matches!(
        config.identifier.as_str(),
        "com.osir.client" | "com.osir.client.beta" | "com.osir.client.dev"
    ) {
        let channel = match config.identifier.as_str() {
            "com.osir.client" => Some("prod"),
            "com.osir.client.beta" => Some("beta"),
            _ => None,
        };
        let enabled =
            release && channel.is_some_and(|channel| valid_osir_configuration(config, channel));
        return UpdateDistribution {
            kind: "osir",
            updates_enabled: enabled,
            reason: (!enabled).then_some(OSIR_UNCONFIGURED),
        };
    }
    UpdateDistribution {
        kind: "unavailable",
        updates_enabled: false,
        reason: Some("当前发行版更新渠道未配置。"),
    }
}

pub fn capability(config: &Config) -> UpdateDistribution {
    capability_for(config, !cfg!(debug_assertions))
}

pub fn ensure_update_available(config: &Config) -> Result<(), String> {
    let current = capability(config);
    if current.updates_enabled {
        Ok(())
    } else {
        Err(current.reason.unwrap_or("更新渠道未配置").into())
    }
}

#[tauri::command]
pub fn get_update_distribution(app: tauri::AppHandle) -> UpdateDistribution {
    capability(app.config())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PUBLIC: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";
    fn config(identifier: &str, channel: &str) -> Config {
        let mut config = Config::default();
        config.identifier = identifier.into();
        config.plugins.0.insert(
            "osir-updates".into(),
            json!({"schemaVersion": 1, "channel": channel, "bundleArtifacts": true}),
        );
        config.plugins.0.insert("updater".into(), json!({"pubkey": STANDARD.encode(PUBLIC), "endpoints": ["https://updates.osir-check.net/beta/latest.json"]}));
        config
    }

    #[test]
    fn opt_in_release_requires_matching_compiled_marker_and_config() {
        for (identifier, channel) in [
            ("com.osir.client.beta", "beta"),
            ("com.osir.client", "prod"),
        ] {
            let config = config(identifier, channel);
            assert!(capability_for(&config, true).updates_enabled);
            assert!(!capability_for(&config, false).updates_enabled);
            // Generated bundle settings are false; only wrapper-owned plugin
            // marker survives Tauri code generation. No renderer/env overrides.
            assert_eq!(config.bundle.create_updater_artifacts, Default::default());
            let response = serde_json::to_value(capability_for(&config, true)).unwrap();
            assert_eq!(
                response,
                json!({"kind": "osir", "updatesEnabled": true, "reason": null})
            );
        }
    }

    #[test]
    fn default_development_unsigned_and_unknown_distributions_fail_closed() {
        assert!(!capability_for(&config("com.osir.client.dev", "dev"), true).updates_enabled);
        let mut default = Config::default();
        default.identifier = "com.osir.client.beta".into();
        assert!(!capability_for(&default, true).updates_enabled);
        default.identifier = "unknown".into();
        assert_eq!(capability_for(&default, true).kind, "unavailable");
        default.identifier = "com.ccswitch.desktop".into();
        assert!(capability_for(&default, false).updates_enabled);
    }

    #[test]
    fn compile_mode_gate_is_applied_by_actual_capability() {
        assert_eq!(
            capability(&config("com.osir.client.beta", "beta")).updates_enabled,
            !cfg!(debug_assertions)
        );
    }

    #[test]
    fn missing_wrong_and_extra_markers_are_rejected() {
        for marker in [
            serde_json::Value::Null,
            json!({}),
            json!({"schemaVersion": 1, "channel": "prod", "bundleArtifacts": true}),
            json!({"schemaVersion": 1, "channel": "beta", "bundleArtifacts": false}),
            json!({"schemaVersion": 1, "channel": "beta", "bundleArtifacts": true, "override": true}),
        ] {
            let mut config = config("com.osir.client.beta", "beta");
            config.plugins.0.insert("osir-updates".into(), marker);
            assert!(!capability_for(&config, true).updates_enabled);
        }
    }

    #[test]
    fn invalid_upstream_and_comment_disguised_public_keys_are_rejected() {
        let upstream: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let upstream = upstream["plugins"]["updater"]["pubkey"].as_str().unwrap();
        let inner = public_key(upstream).unwrap();
        let disguised = STANDARD.encode(format!(
            "untrusted comment: renamed\n{}\n",
            STANDARD.encode(inner)
        ));
        for key in [
            "".into(),
            "placeholder".into(),
            upstream.into(),
            disguised,
            format!("{}\n", STANDARD.encode(PUBLIC)),
        ] {
            let mut config = config("com.osir.client.beta", "beta");
            config.plugins.0.get_mut("updater").unwrap()["pubkey"] = json!(key);
            assert!(!capability_for(&config, true).updates_enabled);
        }
    }

    #[test]
    fn endpoint_rejects_local_upstream_insecure_or_dynamic_sources() {
        for endpoint in [
            "http://updates.osir-check.net/latest.json",
            "https://user:password@updates.osir-check.net/latest.json",
            "https://updates.osir-check.net/latest.json?token=secret",
            "https://updates.osir-check.net/latest.json#part",
            "https://127.0.0.1/latest.json",
            "https://[::1]/latest.json",
            "https://localhost/latest.json",
            "https://home.local/latest.json",
            "https://host.internal/latest.json",
            "https://dl.ccswitch.io/latest.json",
            "https://dl.ccswitch.io./latest.json",
            "https://github.com/farion1231/cc-switch/releases/latest/download/latest.json",
            "https://raw.githubusercontent.com/farion1231%2Fcc-switch/main/latest.json",
            "https://updates.osir-check.net/{target}/latest.json",
            "https://updates.osir-check.net/latest.json\n",
            "https://updates.osir-check.net:444/latest.json",
            "https://updates.osir-check.net/beta/",
        ] {
            let mut config = config("com.osir.client.beta", "beta");
            config.plugins.0.get_mut("updater").unwrap()["endpoints"] = json!([endpoint]);
            assert!(!capability_for(&config, true).updates_enabled, "{endpoint}");
        }
        for endpoints in [
            json!([]),
            json!([
                "https://updates.osir-check.net/one.json",
                "https://updates.osir-check.net/two.json"
            ]),
        ] {
            let mut config = config("com.osir.client.beta", "beta");
            config.plugins.0.get_mut("updater").unwrap()["endpoints"] = endpoints;
            assert!(!capability_for(&config, true).updates_enabled);
        }
        let mut config = config("com.osir.client.beta", "beta");
        config.plugins.0.get_mut("updater").unwrap()["dangerousAcceptInvalidCerts"] = json!(true);
        assert!(!capability_for(&config, true).updates_enabled);
    }
}
