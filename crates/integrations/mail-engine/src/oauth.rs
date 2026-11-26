//! `OAuth2` for the mail providers that block plain-password IMAP and SMTP: Gmail and Outlook
//! require it, so setup signs the person in through the browser instead of asking for a
//! password.
//!
//! Mail owns none of the machinery: the client registration and mail scopes come from Tilo's
//! one client table ([`account_credentials::ClientTable`]). The grant is
//! an entry of the workspace's vault, renewed where the vault is; Mail is handed only a current
//! access token, which the mail client presents as `XOAUTH2`.

use account_credentials::{ClientTable, OAuthIssuer, OAuthUse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

impl OAuthProvider {
    pub fn from_domain(domain: &str) -> Option<Self> {
        match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => Some(OAuthProvider::Google),
            "outlook.com" | "hotmail.com" | "live.com" | "office365.com" | "msn.com" => {
                Some(OAuthProvider::Microsoft)
            }
            _ => None,
        }
    }

    pub fn for_account(email: &str, imap_host: &str) -> Option<Self> {
        let domain = email.rsplit_once('@').map_or("", |(_, domain)| domain);
        Self::from_domain(domain).or_else(|| match imap_host.trim().to_ascii_lowercase().as_str() {
            "imap.gmail.com" | "imap.googlemail.com" => Some(OAuthProvider::Google),
            "outlook.office365.com" | "imap-mail.outlook.com" => Some(OAuthProvider::Microsoft),
            _ => None,
        })
    }

    pub fn issuer(self) -> OAuthIssuer {
        match self {
            OAuthProvider::Google => OAuthIssuer::Google,
            OAuthProvider::Microsoft => OAuthIssuer::Microsoft,
        }
    }

    pub fn oauth_use(self) -> OAuthUse {
        OAuthUse::mail(self.issuer())
    }

    pub fn label(self) -> &'static str {
        self.issuer().label()
    }

    pub fn is_configured(self) -> bool {
        ClientTable::defaults().is_configured(self.oauth_use())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    tilo_test_env::scenario! {
        /// OAuth providers: how one is recognised, the authorization URL it is asked with and the XOAUTH2 answer.
        fn providers_are_recognised_and_their_sign_in_requests_are_formed;

        phase google_url_requests_mail_scope_and_a_refresh_token;
        {
            let url = url_for(OAuthProvider::Google, Some("person@gmail.com"));
            assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
            assert!(url.contains("scope=https%3A%2F%2Fmail.google.com%2F&"));
            assert!(url.contains("access_type=offline"));
            assert!(url.contains("prompt=consent"));
            assert!(url.contains("login_hint=person%40gmail.com"));
        }

        phase microsoft_url_uses_microsoft_endpoint_and_no_google_params;
        {
            let url = url_for(OAuthProvider::Microsoft, None);
            assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"));
            assert!(!url.contains("access_type=offline"));
            assert!(url.contains("IMAP.AccessAsUser.All"));
            assert!(url.contains("SMTP.Send"));
            assert!(url.contains("offline_access"));
            assert!(!url.contains("Files.ReadWrite"));
        }

        phase the_shared_client_table_configures_gmail_and_outlook;
        {
            // The committed registrations cover both; an environment override
            // can only replace them, never remove them.
            assert!(OAuthProvider::Google.is_configured());
            assert!(OAuthProvider::Microsoft.is_configured());
        }

        phase provider_recognised_from_domain;
        {
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

        phase provider_recognised_from_the_imap_server_of_a_custom_domain;
        {
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
    }
}
