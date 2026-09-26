//! Mail's OAuth presets: which issuer and scopes Gmail and Outlook need for
//! IMAP/SMTP, and the `XOAUTH2` SASL encoding those servers expect.
//!
//! The authorization-code + PKCE flow itself, token refresh, and sealing
//! tokens in the vault live in [`accounts::oauth`], the one implementation
//! every account consumer shares.

use accounts::oauth::{ClientSlot, OAuthClient};
use base64::Engine as _;

pub use accounts::oauth::{OAuthTokens, tokens_from_secret, tokens_to_secret};

/// The mail providers that require OAuth rather than a password.
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

    /// Space-separated scopes granting IMAP + SMTP access.
    pub fn scopes(self) -> &'static str {
        match self {
            OAuthProvider::Google => "https://mail.google.com/",
            OAuthProvider::Microsoft => {
                "offline_access https://outlook.office.com/IMAP.AccessAsUser.All \
                 https://outlook.office.com/SMTP.Send"
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OAuthProvider::Google => "Google",
            OAuthProvider::Microsoft => "Microsoft",
        }
    }

    /// The registration slot this provider signs in through.
    pub fn slot(self) -> ClientSlot {
        match self {
            OAuthProvider::Google => ClientSlot::Google,
            OAuthProvider::Microsoft => ClientSlot::Microsoft,
        }
    }

    /// The environment variable that overrides the registered client id.
    pub fn client_id_env(self) -> String {
        self.slot().client_id_env()
    }

    /// The OAuth client for this provider's mail scopes, when a client id is
    /// registered for its slot. `None` means sign-in can be shown but not
    /// completed on this installation.
    pub fn client(self) -> Option<OAuthClient> {
        self.slot()
            .registration()
            .map(|registration| registration.client(self.scopes()))
    }

    /// The registered client id, if this installation can complete sign-in.
    pub fn client_id(self) -> Option<String> {
        self.client().map(|client| client.client_id)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn base64_standard_matches_rfc4648_vectors() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_standard(&[0xfb, 0xff, 0xbf]), "+/+/");
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
    fn outlook_signs_in_through_the_common_microsoft_tenant() {
        let registration = accounts::oauth::ClientRegistration {
            slot: OAuthProvider::Microsoft.slot(),
            client_id: "app-guid".into(),
            tenant: "common".into(),
            authority: "https://login.microsoftonline.com".into(),
        };
        let client = registration.client(OAuthProvider::Microsoft.scopes());
        assert_eq!(
            client.issuer.authorization_endpoint,
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
        );
        assert!(client.scopes.contains("IMAP.AccessAsUser.All"));
        assert!(client.extra_authorization_params.is_empty());
    }
}
