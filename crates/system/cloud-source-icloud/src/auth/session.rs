//! The persisted result of a sign-in.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use cloud_source::Credential;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{AccountInfo, AuthError};
use crate::http::{Cookies, secret};

/// An iCloud web session: the cookies `accountLogin` set, plus the idmsa
/// session and trust tokens that renew it. Custody seals it in the vault
/// (it is all secret apart from the URLs and IDs);
/// [`StoredSession::credential`] is what a Drive source borrows per request.
///
/// The serialized form ([`StoredSession::to_json`]) is what the vault holds:
/// keep it compatible.
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub(crate) apple_id: String,
    #[serde(default)]
    pub(crate) dsid: Option<String>,
    #[serde(default)]
    pub(crate) account_country: String,
    #[serde(with = "secret")]
    pub(crate) session_token: Zeroizing<String>,
    #[serde(with = "secret")]
    pub(crate) trust_token: Zeroizing<String>,
    pub(crate) cookies: Cookies,
    /// The account's `drivews` partition, e.g. `https://p42-drivews.icloud.com`.
    #[serde(default)]
    pub(crate) drive_url: String,
    /// The account's `docws` partition.
    #[serde(default)]
    pub(crate) docs_url: String,
    /// `accountLogin` accepted the token but still wants a second factor.
    #[serde(skip)]
    pub(crate) needs_second_factor: bool,
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredSession")
            .field("apple_id", &self.apple_id)
            .field("dsid", &self.dsid)
            .field("cookies", &self.cookies)
            .field("drive_url", &self.drive_url)
            .field("docs_url", &self.docs_url)
            .finish_non_exhaustive()
    }
}

impl StoredSession {
    /// The session `setup.icloud.com/accountLogin` opened from idmsa's
    /// session and trust tokens: the cookies it set and what it said about
    /// the account (`info`).
    ///
    /// # Errors
    /// [`AuthError::AdvancedDataProtection`] or
    /// [`AuthError::DriveUnavailable`] when the web API cannot read the
    /// account's Drive.
    pub fn from_login(
        apple_id: &str,
        account_country: &str,
        session_token: Zeroizing<String>,
        trust_token: Zeroizing<String>,
        cookies: Cookies,
        info: &AccountInfo,
    ) -> Result<Self, AuthError> {
        let mut session = Self {
            apple_id: apple_id.to_owned(),
            dsid: None,
            account_country: account_country.to_owned(),
            session_token,
            trust_token,
            cookies,
            drive_url: String::new(),
            docs_url: String::new(),
            needs_second_factor: false,
        };
        session.apply(info)?;
        Ok(session)
    }

    /// This session after `validate` accepted it: with the cookies it
    /// rotated and what it said about the account (`info`).
    ///
    /// # Errors
    /// As [`StoredSession::from_login`].
    pub fn revalidated(&self, cookies: &Cookies, info: &AccountInfo) -> Result<Self, AuthError> {
        let mut renewed = self.clone();
        renewed.cookies.merge(cookies);
        renewed.apply(info)?;
        Ok(renewed)
    }

    pub fn apple_id(&self) -> &str {
        &self.apple_id
    }

    pub fn dsid(&self) -> Option<&str> {
        self.dsid.as_deref()
    }

    pub fn drive_url(&self) -> &str {
        &self.drive_url
    }

    pub fn docs_url(&self) -> &str {
        &self.docs_url
    }

    /// The idmsa account country `accountLogin` is told.
    pub fn account_country(&self) -> &str {
        &self.account_country
    }

    /// `accountLogin` accepted the session but still wants a second factor.
    pub fn needs_second_factor(&self) -> bool {
        self.needs_second_factor
    }

    pub fn has_trust_token(&self) -> bool {
        !self.trust_token.is_empty()
    }

    /// The trust token, to pass to a new sign-in so Apple can skip the
    /// second factor. Secret.
    pub fn trust_token(&self) -> &str {
        &self.trust_token
    }

    /// The idmsa session token `accountLogin` renews the cookies from.
    /// Secret.
    pub fn session_token(&self) -> &str {
        &self.session_token
    }

    /// The iCloud web cookies. Secret.
    pub fn cookies(&self) -> &Cookies {
        &self.cookies
    }

    /// The per-request credential: the iCloud web cookies.
    pub fn credential(&self) -> Credential {
        Credential::Session {
            headers: vec![("Cookie".to_owned(), self.cookies.header())],
            expires: None,
        }
    }

    /// Serialized for storage (JSON). Secret.
    pub fn to_json(&self) -> Zeroizing<String> {
        Zeroizing::new(serde_json::to_string(self).unwrap_or_default())
    }

    /// From [`StoredSession::to_json`], or the same base64-encoded (the form
    /// an environment variable or CI secret carries comfortably).
    pub fn from_json(text: &str) -> Result<Self, AuthError> {
        let text = text.trim();
        if let Ok(session) = serde_json::from_str(text) {
            return Ok(session);
        }
        let decoded = Zeroizing::new(
            BASE64
                .decode(text.as_bytes())
                .map_err(|_| AuthError::Protocol("stored session"))?,
        );
        serde_json::from_slice(&decoded).map_err(|_| AuthError::Protocol("stored session"))
    }

    /// Take what an `accountLogin`/`validate` answer says about the account.
    fn apply(&mut self, info: &AccountInfo) -> Result<(), AuthError> {
        if let Some(dsid) = &info.ds_info.dsid {
            self.dsid = Some(dsid.clone());
        }
        self.needs_second_factor = info.untrusted();
        if self.needs_second_factor {
            return Ok(());
        }
        let (drive, docs) = info.check_web_access(&self.cookies)?;
        self.drive_url = drive;
        self.docs_url = docs;
        Ok(())
    }
}
