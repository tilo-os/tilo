//! `OAuth2` for the mail providers that block plain-password IMAP/SMTP —
//! Gmail and Outlook **require** OAuth, so the setup flow signs the user in
//! through the browser instead of asking for a password.
//!
//! The flow itself (PKCE, the loopback redirect, `state` validation, token
//! exchange and refresh) is the shared `tilo-oauth` client that Settings
//! also uses for cloud storage; this module only describes the two mail
//! providers as data and adapts tokens to the vault and to `XOAUTH2`.
//!
//! ## Client registration
//! A working flow needs a registered OAuth client id. Tilo ships without one
//! baked in unless the build provides it, so the id is read from
//! `TILO_MAIL_GOOGLE_CLIENT_ID` / `TILO_MAIL_MICROSOFT_CLIENT_ID`, falling
//! back to the shared `TILO_OAUTH_GOOGLE_CLIENT_ID` /
//! `TILO_OAUTH_MICROSOFT_CLIENT_ID`, each at runtime first and then at build
//! time. Without one the setup UI explains that sign-in is unavailable.

use base64::Engine as _;

pub use tilo_oauth::TokenSet as OAuthTokens;

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

    /// The provider descriptor: endpoints, IMAP + SMTP scopes and the
    /// parameters that make the provider issue a refresh token.
    pub fn descriptor(self) -> tilo_oauth::Provider {
        match self {
            OAuthProvider::Google => tilo_oauth::Provider {
                id: "google".into(),
                label: "Google".into(),
                authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_endpoint: "https://oauth2.googleapis.com/token".into(),
                revocation: tilo_oauth::Revocation::Rfc7009 {
                    endpoint: "https://oauth2.googleapis.com/revoke".into(),
                },
                scopes: vec!["https://mail.google.com/".into()],
                authorization_params: vec![
                    ("access_type".into(), "offline".into()),
                    ("prompt".into(), "consent".into()),
                ],
            },
            OAuthProvider::Microsoft => tilo_oauth::Provider {
                id: "microsoft".into(),
                label: "Microsoft".into(),
                authorization_endpoint:
                    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
                token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
                revocation: tilo_oauth::Revocation::Unsupported,
                scopes: vec![
                    "offline_access".into(),
                    "https://outlook.office.com/IMAP.AccessAsUser.All".into(),
                    "https://outlook.office.com/SMTP.Send".into(),
                ],
                authorization_params: Vec::new(),
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OAuthProvider::Google => "Google",
            OAuthProvider::Microsoft => "Microsoft",
        }
    }

    /// Where the registered client id comes from: Mail's own key, then the
    /// client shared with Settings.
    fn client_ids(self) -> [tilo_oauth::ClientId; 2] {
        match self {
            OAuthProvider::Google => [
                tilo_oauth::ClientId::new(
                    "TILO_MAIL_GOOGLE_CLIENT_ID",
                    option_env!("TILO_MAIL_GOOGLE_CLIENT_ID"),
                ),
                tilo_oauth::ClientId::new(
                    "TILO_OAUTH_GOOGLE_CLIENT_ID",
                    option_env!("TILO_OAUTH_GOOGLE_CLIENT_ID"),
                ),
            ],
            OAuthProvider::Microsoft => [
                tilo_oauth::ClientId::new(
                    "TILO_MAIL_MICROSOFT_CLIENT_ID",
                    option_env!("TILO_MAIL_MICROSOFT_CLIENT_ID"),
                ),
                tilo_oauth::ClientId::new(
                    "TILO_OAUTH_MICROSOFT_CLIENT_ID",
                    option_env!("TILO_OAUTH_MICROSOFT_CLIENT_ID"),
                ),
            ],
        }
    }

    /// The configuration key named in "not configured" messages.
    pub fn client_id_env(self) -> &'static str {
        let [own, _shared] = self.client_ids();
        own.key
    }

    /// The registered client id, if provisioned. `None` means "no client id
    /// configured" — the flow can be explained but not completed.
    pub fn client_id(self) -> Option<String> {
        self.client_ids()
            .iter()
            .find_map(tilo_oauth::ClientId::resolve)
    }
}

/// Serialise a token bundle into a vault [`accounts::Secret`] (the credential
/// store keeps it sealed at rest, never in the plaintext account JSON).
pub fn tokens_to_secret(tokens: &OAuthTokens) -> accounts::Secret {
    accounts::Secret {
        password: String::new(),
        token: serde_json::to_string(tokens).ok(),
    }
}

/// Recover a token bundle from a vault [`accounts::Secret`], if it holds one.
pub fn tokens_from_secret(secret: &accounts::Secret) -> Option<OAuthTokens> {
    let raw = secret.token.as_ref()?;
    serde_json::from_str(raw).ok()
}

/// The `XOAUTH2` SASL initial-response string IMAP `AUTHENTICATE` and SMTP
/// `AUTH` expect (RFC 7628 / Google + Microsoft mail): the base64 of
/// `user=<email>^Aauth=Bearer <token>^A^A` where `^A` is the `\x01` control
/// byte.
pub fn xoauth2_sasl(user: &str, access_token: &str) -> String {
    let raw = format!("user={user}\x01auth=Bearer {access_token}\x01\x01");
    base64_standard(raw.as_bytes())
}

/// Standard base64 (RFC 4648 §4, `+`/`/` alphabet, `=` padding) — the encoding
/// SASL/XOAUTH2 mandates.
pub fn base64_standard(input: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}

/// Refresh an expired access token using its refresh token, carrying the
/// refresh token forward when the provider does not rotate it.
///
/// # Errors
/// Returns `Err` if the token endpoint cannot be reached or rejects the
/// refresh token.
#[cfg(feature = "live")]
pub fn refresh_tokens(
    provider: OAuthProvider,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, String> {
    tilo_oauth::client::OAuthClient::new()
        .and_then(|client| client.refresh(&provider.descriptor(), client_id, refresh_token))
        .map_err(|error| error.to_string())
}

/// Run the browser sign-in: bind the loopback redirect, open the consent
/// page, capture the code and redeem it. Blocks until the user consents or
/// `timeout` elapses.
///
/// # Errors
/// Returns `Err` if the browser cannot open, the redirect never arrives or
/// fails validation, or the code exchange fails.
#[cfg(feature = "live")]
pub fn run_loopback_authorization(
    provider: OAuthProvider,
    client_id: &str,
    login_hint: &str,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
    timeout: std::time::Duration,
) -> Result<OAuthTokens, String> {
    let descriptor = provider.descriptor();
    let authorization =
        tilo_oauth::client::Authorization::begin(&descriptor, client_id, Some(login_hint))
            .map_err(|error| error.to_string())?;
    open_browser(authorization.url())
        .map_err(|error| format!("could not open sign-in browser: {error}"))?;
    let client = tilo_oauth::client::OAuthClient::new().map_err(|error| error.to_string())?;
    authorization
        .complete(&client, timeout)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "live")]
    #[test]
    fn authorization_reports_browser_failure_before_waiting_for_callback() {
        let result = run_loopback_authorization(
            OAuthProvider::Google,
            "test-client",
            "fixture@example.invalid",
            |url| {
                assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
                Err("fixture browser unavailable".into())
            },
            std::time::Duration::ZERO,
        );
        assert_eq!(
            result.unwrap_err(),
            "could not open sign-in browser: fixture browser unavailable"
        );
    }

    fn url_for(provider: OAuthProvider, hint: Option<&str>) -> String {
        let descriptor = provider.descriptor();
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
        assert!(url.contains("scope=https%3A%2F%2Fmail.google.com%2F"));
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
        assert!(url.contains("offline_access"));
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
    fn base64_standard_matches_rfc4648_vectors() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_standard(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn xoauth2_sasl_matches_google_documentation_vector() {
        assert_eq!(
            xoauth2_sasl(
                "someuser@example.com",
                "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg"
            ),
            "dXNlcj1zb21ldXNlckBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LnZGOWRmdDRxbVRjMk52YjNSbGNrQmhkSFJoZG1semRHRXVZMjl0Q2cBAQ=="
        );
    }

    #[test]
    fn tokens_round_trip_through_a_vault_secret() {
        let tokens = OAuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_in: 3600,
            obtained_at: 1000,
        };
        let secret = tokens_to_secret(&tokens);
        assert!(secret.password.is_empty());
        assert_eq!(tokens_from_secret(&secret), Some(tokens));
    }
}
