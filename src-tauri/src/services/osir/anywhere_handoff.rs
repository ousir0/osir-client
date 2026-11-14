//! Native-only first-party account handoff. No source credentials cross IPC.
//! These DTOs are not Tauri commands and must only be called after validating
//! the installed receiver and the explicit Client installation/open action.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

pub(crate) const RECIPIENT: &str = "osir-anywhere-desktop";
pub(crate) const TARGET: &str = "https://anywhere.osirclaw.com";
pub(crate) const ACTIONS: [&str; 3] = ["identity", "connect-local", "bootstrap-configured-tools"];

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HandoffRequest {
    pub version: u32,
    pub flow_id: String,
    pub receiver_instance_id: String,
    pub state: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub recipient_client_id: String,
    pub target_origin: String,
    pub actions: Vec<String>,
}

fn random_field(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

impl HandoffRequest {
    pub(crate) fn validate(&self) -> bool {
        self.version == 1
            && random_field(&self.flow_id, 32, 128)
            && random_field(&self.receiver_instance_id, 32, 128)
            && random_field(&self.state, 43, 43)
            && random_field(&self.code_challenge, 43, 43)
            && [&self.state, &self.code_challenge]
                .into_iter()
                .all(|value| {
                    URL_SAFE_NO_PAD.decode(value).ok().is_some_and(|decoded| {
                        decoded.len() == 32 && URL_SAFE_NO_PAD.encode(decoded) == *value
                    })
                })
            && self.code_challenge_method == "S256"
            && self.recipient_client_id == RECIPIENT
            && self.target_origin == TARGET
            && self.actions.iter().map(String::as_str).eq(ACTIONS)
    }
}

// Deliberately no Debug or Serialize: a one-time credential must never be
// returned to the renderer or accidentally included in diagnostic logs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandoffCode {
    pub code: String,
    pub expires_in: u32,
    pub recipient_client_id: String,
    pub audience: String,
}

impl HandoffCode {
    pub(crate) fn validate(&self) -> bool {
        random_field(&self.code, 43, 43)
            && URL_SAFE_NO_PAD
                .decode(&self.code)
                .ok()
                .is_some_and(|bytes| {
                    bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == self.code
                })
            && self.expires_in == 60
            && self.recipient_client_id == RECIPIENT
            && self.audience == "osir-anywhere-server"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> serde_json::Value {
        json!({"version":1,"flowId":"f".repeat(43),
            "receiverInstanceId":"r".repeat(43),"state":URL_SAFE_NO_PAD.encode([1;32]),
            "codeChallenge":URL_SAFE_NO_PAD.encode([2;32]),"codeChallengeMethod":"S256",
            "recipientClientId":RECIPIENT,"targetOrigin":TARGET,"actions":ACTIONS})
    }

    #[test]
    fn handoff_request_is_closed_and_bound_to_first_party_receiver() {
        let original = request();
        assert!(serde_json::from_value::<HandoffRequest>(original.clone())
            .unwrap()
            .validate());
        for (field, value) in [
            ("version", json!(2)),
            ("flowId", json!("../../other")),
            ("receiverInstanceId", json!("other")),
            ("state", json!("a".repeat(42))),
            ("codeChallenge", json!("!".repeat(43))),
            ("codeChallengeMethod", json!("plain")),
            ("recipientClientId", json!("osir-anywhere-web")),
            (
                "targetOrigin",
                json!("https://anywhere.osirclaw.com.evil.test"),
            ),
            ("actions", json!(["identity"])),
        ] {
            let mut invalid = original.clone();
            invalid[field] = value;
            assert!(
                !serde_json::from_value::<HandoffRequest>(invalid)
                    .unwrap()
                    .validate(),
                "{field}"
            );
        }
        for field in [
            "accountId",
            "accessToken",
            "refreshToken",
            "codeVerifier",
            "apiKey",
        ] {
            let mut extra = original.clone();
            extra[field] = json!("synthetic-never-accepted");
            assert!(
                serde_json::from_value::<HandoffRequest>(extra).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn handoff_code_requires_exact_lifetime_recipient_and_audience() {
        let original = json!({"code":URL_SAFE_NO_PAD.encode([3;32]),"expires_in":60,
            "recipient_client_id":RECIPIENT,"audience":"osir-anywhere-server"});
        assert!(serde_json::from_value::<HandoffCode>(original.clone())
            .unwrap()
            .validate());
        for (field, value) in [
            ("code", json!("secret")),
            ("expires_in", json!(3600)),
            ("recipient_client_id", json!("other")),
            ("audience", json!("other")),
        ] {
            let mut invalid = original.clone();
            invalid[field] = value;
            assert!(!serde_json::from_value::<HandoffCode>(invalid)
                .unwrap()
                .validate());
        }
    }
}
