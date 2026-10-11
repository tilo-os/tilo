//! `OAuth2` for the mail providers that block plain-password IMAP/SMTP —
//! Gmail and Outlook **require** OAuth, so the setup flow signs the user in
//! through the browser instead of asking for a password.
//!
//! Mail owns none of the `OAuth` machinery. The client registration (Google's
//! Desktop client id and secret, Microsoft's public client) and the mail
//! scopes come from Tilo's one client table,
//! [`account_custody::ClientTable`] (`oauth-clients.toml`), the same
//! registration Settings signs in to Google Drive and `OneDrive` with. The
//! grant is sealed in the account vault and refreshed by
//! [`account_custody::Custodian`]; Mail only ever holds a current access
//! token, long enough to present it as `XOAUTH2`.

use account_custody::{ClientTable, OAuthIssuer, OAuthUse};

/// The OAuth providers we preset for. Both are IMAP/SMTP mail providers that
/// require OAuth rather than a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

impl OAuthProvider {
    /// Recognise a provider from an email domain (lower-cased).
    pub fn from_domain(domain: &str) -> Option<Self> {
        match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => Some(OAuthProvider::Google),
            "outlook.com" | "hotmail.com" | "live.com" | "office365.com" | "msn.com" => {
                Some(OAuthProvider::Microsoft)
            }
            _ => None,
        }
    }

    /// The provider of an OAuth account: from its address, else from its
    /// IMAP server (a Google Workspace or Microsoft 365 address on its own
    /// domain).
    pub fn for_account(email: &str, imap_host: &str) -> Option<Self> {
        let domain = email.rsplit_once('@').map_or("", |(_, domain)| domain);
        Self::from_domain(domain).or_else(|| match imap_host.trim().to_ascii_lowercase().as_str() {
            "imap.gmail.com" | "imap.googlemail.com" => Some(OAuthProvider::Google),
            "outlook.office365.com" | "imap-mail.outlook.com" => Some(OAuthProvider::Microsoft),
            _ => None,
        })
    }

    /// The issuer whose client this provider signs in through.
    pub fn issuer(self) -> OAuthIssuer {
        match self {
            OAuthProvider::Google => OAuthIssuer::Google,
            OAuthProvider::Microsoft => OAuthIssuer::Microsoft,
        }
    }

    /// Mail from this provider, as a use of its client.
    pub fn oauth_use(self) -> OAuthUse {
        OAuthUse::mail(self.issuer())
    }

    pub fn label(self) -> &'static str {
        self.issuer().label()
    }

    /// Whether this build can sign in to this provider's mail. `false` means
    /// no client id is configured: the flow can be explained but not
    /// completed.
    pub fn is_configured(self) -> bool {
        ClientTable::defaults().is_configured(self.oauth_use())
    }
}

/// The `XOAUTH2` SASL initial response IMAP `AUTHENTICATE` expects (RFC 7628
/// / Google + Microsoft mail), before base64: `user=<email>^Aauth=Bearer
/// <token>^A^A` where `^A` is the `\x01` control byte.
pub fn xoauth2_response(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// The async-imap `XOAUTH2` authenticator: it answers with the raw
/// [`xoauth2_response`], which async-imap base64-encodes before sending.
#[cfg(feature = "live")]
pub struct XOAuth2 {
    pub user: String,
    pub token: String,
}

#[cfg(feature = "live")]
impl async_imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        xoauth2_response(&self.user, &self.token)
    }
}

/// What Mail shows for a custody failure. Messages that need a new sign-in
/// name OAuth so the account is marked "sign in again".
#[cfg(feature = "live")]
fn custody_message(error: &account_custody::CustodyError) -> String {
    use account_custody::CustodyError;
    match error {
        CustodyError::NeedsAuthentication => {
            "the OAuth sign-in is no longer valid; sign in again".into()
        }
        CustodyError::NotConfigured { provider, .. } => {
            format!("{provider} sign-in isn’t available in this build")
        }
        other => other.to_string(),
    }
}

/// A current access token for an OAuth mail account, refreshed and resealed
/// by custody when due. `force` refreshes even when the token looks valid
/// (the server just rejected it).
///
/// # Errors
/// A message naming why no token is available (not signed in, revoked,
/// offline, provider not configured).
#[cfg(feature = "live")]
pub fn access_token(
    custodian: &account_custody::Custodian,
    account_id: &str,
    provider: OAuthProvider,
    force: bool,
) -> Result<String, String> {
    custodian
        .oauth_access_token(
            &accounts::AccountId::from(account_id.to_string()),
            provider.oauth_use(),
            force,
        )
        .map(|token| token.to_string())
        .map_err(|error| custody_message(&error))
}

/// Run the browser sign-in for `provider`'s mail and seal the grant under
/// `account_id`: bind the loopback redirect, open the consent page, capture
/// the code and redeem it with the shared client registration. Blocks until
/// the user consents or the sign-in times out.
///
/// # Errors
/// Returns `Err` if sign-in is not configured, the browser cannot open, the
/// redirect never arrives or fails validation, the code exchange fails, or
/// the vault cannot be written.
#[cfg(feature = "live")]
pub fn sign_in(
    custodian: &account_custody::Custodian,
    account_id: &str,
    provider: OAuthProvider,
    login_hint: &str,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let authorization = custodian
        .begin_sign_in_for(provider.oauth_use(), Some(login_hint))
        .map_err(|error| custody_message(&error))?;
    open_browser(authorization.url())
        .map_err(|error| format!("could not open sign-in browser: {error}"))?;
    let tokens = account_custody::Custodian::finish_sign_in(authorization)
        .map_err(|error| custody_message(&error))?;
    custodian
        .seal_oauth(&accounts::AccountId::from(account_id.to_string()), &tokens)
        .map_err(|error| format!("could not save sign-in credentials: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "live")]
    #[test]
    fn authorization_reports_browser_failure_before_waiting_for_callback() {
        let root = tempfile::tempdir().unwrap();
        let custodian = account_custody::Custodian::for_root(Some(root.path().to_path_buf()))
            .with_client_table(
                ClientTable::parse(
                    "[google]\nclient_id = \"test-client\"\n[google.scopes]\nmail = [\"https://mail.google.com/\"]\n",
                )
                .unwrap(),
            );
        let result = sign_in(
            &custodian,
            "fixture@example.invalid",
            OAuthProvider::Google,
            "fixture@example.invalid",
            |url| {
                assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
                Err("fixture browser unavailable".into())
            },
        );
        assert_eq!(
            result.unwrap_err(),
            "could not open sign-in browser: fixture browser unavailable"
        );
    }

    fn url_for(provider: OAuthProvider, hint: Option<&str>) -> String {
        let descriptor = ClientTable::defaults().descriptor(provider.oauth_use());
        tilo_oauth::AuthorizationRequest {
            provider: &descriptor,
            client_id: "cid",
            redirect_uri: "http://127.0.0.1:8080/oauth",
            code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            state: "s",
            login_hint: hint,
        }
        .url()
    }

    #[test]
    fn google_url_requests_mail_scope_and_a_refresh_token() {
        let url = url_for(OAuthProvider::Google, Some("person@gmail.com"));
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("scope=https%3A%2F%2Fmail.google.com%2F&"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("login_hint=person%40gmail.com"));
    }

    #[test]
    fn microsoft_url_uses_microsoft_endpoint_and_no_google_params() {
        let url = url_for(OAuthProvider::Microsoft, None);
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"));
        assert!(!url.contains("access_type=offline"));
        assert!(url.contains("IMAP.AccessAsUser.All"));
        assert!(url.contains("SMTP.Send"));
        assert!(url.contains("offline_access"));
        assert!(!url.contains("Files.ReadWrite"));
    }

    #[test]
    fn the_shared_client_table_configures_gmail_and_outlook() {
        // The committed registrations cover both; an environment override
        // can only replace them, never remove them.
        assert!(OAuthProvider::Google.is_configured());
        assert!(OAuthProvider::Microsoft.is_configured());
    }

    #[test]
    fn provider_recognised_from_domain() {
        assert_eq!(
            OAuthProvider::from_domain("gmail.com"),
            Some(OAuthProvider::Google)
        );
        assert_eq!(
            OAuthProvider::from_domain("Outlook.com"),
            Some(OAuthProvider::Microsoft)
        );
        assert_eq!(OAuthProvider::from_domain("fastmail.com"), None);
    }

    #[test]
    fn provider_recognised_from_the_imap_server_of_a_custom_domain() {
        assert_eq!(
            OAuthProvider::for_account("ada@example.org", "imap.gmail.com"),
            Some(OAuthProvider::Google)
        );
        assert_eq!(
            OAuthProvider::for_account("ada@example.org", "outlook.office365.com"),
            Some(OAuthProvider::Microsoft)
        );
        assert_eq!(
            OAuthProvider::for_account("ada@gmail.com", ""),
            Some(OAuthProvider::Google)
        );
        assert_eq!(
            OAuthProvider::for_account("ada@example.org", "mail.example.org"),
            None
        );
    }

    #[test]
    fn xoauth2_response_matches_google_documentation_vector() {
        use base64::Engine as _;
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(xoauth2_response(
                "someuser@example.com",
                "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg"
            )),
            "dXNlcj1zb21ldXNlckBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LnZGOWRmdDRxbVRjMk52YjNSbGNrQmhkSFJoZG1semRHRXVZMjl0Q2cBAQ=="
        );
    }
}
