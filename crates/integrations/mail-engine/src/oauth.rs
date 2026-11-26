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
