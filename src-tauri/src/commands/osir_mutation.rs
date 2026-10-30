//! Completion policy for irreversible cloud mutations and saved native grants.
//! Contains no credentials and never deletes a saved provider.

const SAVE_UNCONFIRMED: &str =
    "连接保存结果未确认，请检查供应商列表，勿直接重复保存；已保留连接凭据";
const CLEANUP_INCOMPLETE: &str = "连接未保存，本地凭据清理未完成；请勿直接重试";

/// After cloud success, a local/session failure cannot imply a safe POST retry.
pub(super) fn after_cloud_success<T, E>(
    finish: impl FnOnce() -> Result<T, E>,
    uncertain: impl FnOnce() -> String,
) -> Result<T, String> {
    finish().map_err(|_| uncertain())
}

/// A saved tool grant outlives account logout. Only confirmed row absence permits
/// deleting its vault record; failed/unknown DB outcomes retain the credential.
pub(super) fn save_connection<T>(
    save: impl FnOnce() -> Result<T, String>,
    row_absent: impl FnOnce() -> Result<bool, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
    check_session: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    match save() {
        Ok(saved) => {
            check_session().map_err(|_| SAVE_UNCONFIRMED.to_string())?;
            Ok(saved)
        }
        Err(error) => match row_absent() {
            Ok(true) => {
                cleanup().map_err(|_| CLEANUP_INCOMPLETE.to_string())?;
                Err(error)
            }
            Ok(false) | Err(_) => Err(SAVE_UNCONFIRMED.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn post_creation_session_and_final_generation_failures_are_uncertain() {
        for fail_at in [0, 1, 2] {
            let result = after_cloud_success(
                || {
                    for step in 0..3 {
                        if step == fail_at {
                            return Err("stale or sync failure");
                        }
                    }
                    Ok(7)
                },
                || "uncertain: reconcile before creating again".into(),
            );
            assert_eq!(
                result.unwrap_err(),
                "uncertain: reconcile before creating again"
            );
        }
        assert_eq!(
            after_cloud_success(|| Ok::<_, ()>(7), || panic!("success")),
            Ok(7)
        );
    }

    #[test]
    fn logout_after_save_preserves_row_and_vault() {
        let row = Cell::new(false);
        let secret = Cell::new(true);
        let result = save_connection(
            || {
                row.set(true);
                Ok(7)
            },
            || panic!("successful save needs no rollback lookup"),
            || {
                secret.set(false);
                Ok(())
            },
            || Err("stale".into()),
        );
        assert_eq!(result.unwrap_err(), SAVE_UNCONFIRMED);
        assert!(row.get());
        assert!(secret.get());
    }

    #[test]
    fn failed_or_unknown_save_with_possible_row_never_deletes_vault() {
        for lookup in [Ok(false), Err("DB unavailable".into())] {
            let result = save_connection(
                || Err::<(), _>("save failed".into()),
                || lookup,
                || panic!("must retain recoverable secret"),
                || panic!("save failed"),
            );
            assert_eq!(result.unwrap_err(), SAVE_UNCONFIRMED);
        }
    }

    #[test]
    fn confirmed_absence_cleans_vault_and_preserves_original_failure() {
        let secret = Cell::new(true);
        let result = save_connection(
            || Err::<(), _>("save failed".into()),
            || Ok(true),
            || {
                secret.set(false);
                Ok(())
            },
            || panic!("save failed"),
        );
        assert_eq!(result.unwrap_err(), "save failed");
        assert!(!secret.get());
    }

    #[test]
    fn cleanup_failure_is_explicit_and_does_not_claim_success() {
        let result = save_connection(
            || Err::<(), _>("save failed".into()),
            || Ok(true),
            || Err("vault unavailable".into()),
            || panic!("save failed"),
        );
        assert_eq!(result.unwrap_err(), CLEANUP_INCOMPLETE);
    }

    #[test]
    fn successful_save_returns_result_without_cleanup() {
        assert_eq!(
            save_connection(|| Ok(7), || panic!("saved"), || panic!("saved"), || Ok(())),
            Ok(7)
        );
    }
}
