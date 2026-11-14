use super::super::anywhere_handoff::{HandoffCode, HandoffRequest};
use super::*;

impl<B: CredentialBackend + 'static> AuthManager<B> {
    /// Called only for a native, validated install/open handoff. Source tokens
    /// remain inside the account service. Logout or account switching invalidates
    /// a pending result before its one-time credential can be delivered.
    pub(crate) async fn authorize_anywhere_handoff(
        &self,
        request: &HandoffRequest,
        expected_generation: u64,
        expected_account: &str,
    ) -> Result<HandoffCode, SessionError> {
        self.require_enabled()?;
        self.check(expected_generation)?;
        if !request.validate() {
            return Err(SessionError::Configuration);
        }
        let (generation, record) = self.access().await?;
        if generation != expected_generation || record.account_id != expected_account {
            return Err(SessionError::Superseded);
        }
        let code = self
            .client
            .authorize_anywhere_handoff(request, &record.access_token)
            .await?;
        self.check(generation)?;
        Ok(code)
    }
}
