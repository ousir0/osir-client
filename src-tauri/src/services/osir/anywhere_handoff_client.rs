use super::super::anywhere_handoff::{HandoffCode, HandoffRequest};
use super::*;

impl AccountClient {
    /// One bounded POST, no automatic mutation replay. Only the native caller
    /// receives the resulting short-lived, receiver-bound code.
    pub(crate) async fn authorize_anywhere_handoff(
        &self,
        request: &HandoffRequest,
        access_token: &str,
    ) -> Result<HandoffCode, AccountError> {
        if !request.validate() {
            return Err(AccountError::Configuration);
        }
        if access_token.is_empty() {
            return Err(AccountError::Unauthorized);
        }
        let url = self
            .base
            .join("auth/desktop/anywhere/handoff/authorize")
            .map_err(|_| AccountError::Configuration)?;
        let code: HandoffCode = self
            .send_and_read(
                self.http
                    .post(url)
                    .bearer_auth(access_token)
                    .json(request)
                    .timeout(Duration::from_secs(10)),
                RequestClass::Standard,
            )
            .await?;
        if !code.validate() {
            return Err(AccountError::InvalidResponse);
        }
        Ok(code)
    }
}
