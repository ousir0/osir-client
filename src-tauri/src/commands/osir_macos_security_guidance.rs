//! Conservative classification of diagnostics from Apple's /usr/bin/open.
//! No localized prose, quarantine attribute, exit status alone or handoff
//! timeout establishes a system policy denial. Raw diagnostics stay native.

pub(super) const OPEN_STDERR_LIMIT: usize = 8 * 1024;

// Best-effort System Settings navigation, NOT a stable public Apple API.
// General is the security/Gatekeeper area; Privacy selects permission categories.
// The OS may ignore this anchor. Approval always remains with the user/system.
pub(super) const SECURITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?General";

pub(super) fn should_open_security_settings(exit_code: Option<i32>, stderr: &[u8]) -> bool {
    if !matches!(exit_code, Some(code) if code != 0) || stderr.len() > OPEN_STDERR_LIMIT {
        return false;
    }
    let Ok(diagnostic) = std::str::from_utf8(stderr) else {
        return false;
    };
    diagnostic.lines().any(|line| {
        // Accept a top-level NSError, not a number in a path, localized text,
        // or an unrelated NSUnderlyingError. Unknown open output fails closed.
        let error = line.strip_prefix("Error Domain=").or_else(|| {
            line.rsplit_once(", error=Error Domain=")
                .map(|(_, error)| error)
        });
        let Some(code) = error.and_then(|error| error.strip_prefix("NSOSStatusErrorDomain Code="))
        else {
            return false;
        };
        let Some((code, _)) = code.split_once(char::is_whitespace) else {
            return false;
        };
        // Public macOS SDK LaunchServices/LSConstants.h:
        // kLSNoLaunchPermissionErr = -10826 (managed-network launch permission).
        // This proves a launch-policy refusal, not specifically Gatekeeper or
        // that "Open Anyway" is available. Do not whitelist private -1067x
        // errors, generic -10810, or signature/assessment errors by inference.
        code == "-10826"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DENIED: &[u8] = b"The application cannot be opened, error=Error Domain=NSOSStatusErrorDomain Code=-10826 \"(null)\" UserInfo={}\n";

    #[test]
    fn explicit_public_launch_policy_denial_is_the_only_positive() {
        assert!(should_open_security_settings(Some(1), DENIED));
        assert!(should_open_security_settings(
            Some(1),
            b"Error Domain=NSOSStatusErrorDomain Code=-10826 \"localized description\""
        ));
    }

    #[test]
    fn accepted_launch_timeout_spawn_failure_and_signal_do_not_prompt() {
        // None covers no completed exit code: timeout, spawn error or signal.
        assert!(!should_open_security_settings(Some(0), DENIED));
        assert!(!should_open_security_settings(None, DENIED));
        assert!(!should_open_security_settings(None, b""));
    }

    #[test]
    fn generic_failure_crash_network_and_quarantine_are_not_policy_evidence() {
        for text in [
            "",
            "launch timed out; process stopped",
            "application crashed",
            "network unavailable",
            "Operation not permitted",
            "Permission denied",
            "com.apple.quarantine",
            "unidentified developer",
            "rejected",
            "macOS cannot verify this app",
            "kLSNoLaunchPermissionErr",
        ] {
            assert!(
                !should_open_security_settings(Some(1), text.as_bytes()),
                "{text}"
            );
        }
    }

    #[test]
    fn unrelated_and_private_osstatus_codes_do_not_prompt() {
        for code in [
            -10810, -10814, -10822, -10825, -10827, -10661, -10671, -10673, -67050, -67062, -66992,
            -108260,
        ] {
            let text = format!("Error Domain=NSOSStatusErrorDomain Code={code} \"failure\"");
            assert!(
                !should_open_security_settings(Some(1), text.as_bytes()),
                "{code}"
            );
        }
    }

    #[test]
    fn number_without_exact_top_level_domain_is_not_evidence() {
        for text in [
            "Error Domain=NSCocoaErrorDomain Code=-10826 \"failure\"",
            "Error Domain=NSPOSIXErrorDomain Code=-10826 \"failure\"",
            "LSOpenURLsWithRole() failed with error -10826.",
            "Error Domain=NSOSStatusErrorDomain Code=-10826garbage \"failure\"",
            "Error Domain=NSOSStatusErrorDomain Code=-10826",
            "Error Domain=NSCocoaErrorDomain Code=256 UserInfo={NSUnderlyingError=Error Domain=NSOSStatusErrorDomain Code=-10826 \"failure\"}",
            "/tmp/Error Domain=NSOSStatusErrorDomain Code=-10826 app not found",
        ] {
            assert!(!should_open_security_settings(Some(1), text.as_bytes()), "{text}");
        }
    }

    #[test]
    fn malformed_or_oversized_diagnostics_do_not_prompt() {
        let mut text = DENIED.to_vec();
        text.resize(OPEN_STDERR_LIMIT + 1, b' ');
        assert!(!should_open_security_settings(Some(1), &text));
        assert!(!should_open_security_settings(Some(1), &[255, 254]));
    }

    #[test]
    fn settings_link_targets_security_general_area() {
        assert_eq!(
            SECURITY_SETTINGS_URL,
            "x-apple.systempreferences:com.apple.preference.security?General"
        );
    }
}
