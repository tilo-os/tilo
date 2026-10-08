//! The iCloud session this provider consumes, and what it needs to know
//! about Apple's sign-in endpoints.
//!
//! Obtaining a session (the SRP-6a sign-in, two-factor codes, trust tokens,
//! renewal) is credential custody, not provider work: it lives in
//! `account-custody` (`account_custody::icloud::auth`), on Apple's SRP from
//! `tilo-crypto`. What remains here is the session's persisted form
//! ([`StoredSession`]), which a Drive source borrows per request, the
//! account description `setup.icloud.com` answers with ([`AccountInfo`]),
//! the endpoints ([`Endpoints`]) and the error vocabulary both sides share
//! ([`AuthError`]).
//!
//! Accounts with Advanced Data Protection keep Drive's keys off Apple's
//! servers; the web API cannot read them. That is reported as
//! [`AuthError::AdvancedDataProtection`], never as a generic failure.
//!
//! Nothing here logs; secrets are zeroized when dropped and redacted from
//! `Debug`.

use std::time::Duration;

use serde::Deserialize;
use url::Url;

use crate::http::Cookies;

mod session;

pub use session::StoredSession;

/// iCloud.com's public OAuth widget key, the one every reference client
/// sends as `X-Apple-Widget-Key`.
pub const WIDGET_KEY: &str = "d39ba9916b7251055b22c7f910e2ea796ee65e98b2ddecea8f5dde8d9d1a815d";

/// The cookie that carries Drive's decryption grant on Advanced Data
/// Protection accounts.
const PCS_DOCUMENTS_COOKIE: &str = "X-APPLE-WEBAUTH-PCS-Documents";

/// Where the sign-in endpoints live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// `https://idmsa.apple.com/appleauth/auth`.
    pub auth: Url,
    /// `https://setup.icloud.com/setup/ws/1`.
    pub setup: Url,
    pub widget_key: String,
}

impl Endpoints {
    /// Apple's production endpoints.
    pub fn apple() -> Self {
        Self::parse(
            "https://idmsa.apple.com/appleauth/auth",
            "https://setup.icloud.com/setup/ws/1",
        )
    }

    /// `<base>/appleauth/auth` and `<base>/setup/ws/1`, for a fake server.
    pub fn at(base: &Url) -> Self {
        let base = base.as_str().trim_end_matches('/');
        Self::parse(
            &format!("{base}/appleauth/auth"),
            &format!("{base}/setup/ws/1"),
        )
    }

    fn parse(auth: &str, setup: &str) -> Self {
        #[allow(clippy::expect_used, reason = "constant and caller-built URLs")]
        Self {
            auth: Url::parse(auth).expect("auth endpoint"),
            setup: Url::parse(setup).expect("setup endpoint"),
            widget_key: WIDGET_KEY.to_owned(),
        }
    }

    /// The origin idmsa's web client claims (`Origin`, `Referer`).
    pub fn auth_origin(&self) -> String {
        self.auth.origin().ascii_serialization()
    }
}

/// Why signing in or refreshing failed. Messages carry no secrets.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the Apple ID or password is incorrect")]
    InvalidCredentials,
    #[error("this Apple ID is locked; unlock it at iforgot.apple.com")]
    AccountLocked,
    #[error("the verification code is incorrect")]
    InvalidCode,
    /// The pending sign-in is no longer valid on Apple's side.
    #[error("the sign-in timed out; start again")]
    SignInExpired,
    #[error("no trusted phone number can receive a code")]
    NoTrustedPhone,
    #[error("iCloud web access is unavailable with Advanced Data Protection")]
    AdvancedDataProtection,
    #[error("iCloud Drive is not enabled for this Apple ID")]
    DriveUnavailable,
    /// The session can no longer be renewed without a person entering a new
    /// two-factor code.
    #[error("the iCloud session expired; sign in again with a verification code")]
    SessionExpired,
    #[error("Apple is rate limiting sign-in")]
    RateLimited { retry_after: Option<Duration> },
    #[error("Apple's sign-in service is unreachable")]
    Offline,
    /// Apple answered something this client does not understand; the
    /// unofficial API probably changed.
    #[error("unexpected sign-in response: {0}")]
    Protocol(&'static str),
    #[error("Apple's sign-in service answered {0}")]
    Status(u16),
}

impl From<AuthError> for cloud_source::Error {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Offline => Self::Offline,
            AuthError::RateLimited { retry_after } => Self::RateLimited { retry_after },
            AuthError::AdvancedDataProtection | AuthError::DriveUnavailable => Self::Forbidden,
            AuthError::Protocol(what) => Self::InvalidResponse(what),
            AuthError::Status(status) => Self::Status(status),
            AuthError::InvalidCredentials
            | AuthError::AccountLocked
            | AuthError::InvalidCode
            | AuthError::SignInExpired
            | AuthError::NoTrustedPhone
            | AuthError::SessionExpired => Self::Unauthorized,
        }
    }
}

/// `setup.icloud.com`'s account description (`accountLogin`, `validate`),
/// the parts used here. Read with [`crate::http::read_json`]; a
/// [`StoredSession`] takes what it says.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountInfo {
    pub(crate) ds_info: DsInfo,
    pub(crate) hsa_challenge_required: bool,
    pub(crate) hsa_trusted_browser: Option<bool>,
    pub(crate) webservices: Webservices,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct DsInfo {
    pub dsid: Option<String>,
    pub hsa_version: u32,
    pub is_web_access_allowed: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Webservices {
    pub drivews: Option<Webservice>,
    pub docws: Option<Webservice>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Webservice {
    pub url: String,
    pub pcs_required: bool,
    pub status: Option<String>,
}

impl AccountInfo {
    /// A session that still needs its second factor.
    fn untrusted(&self) -> bool {
        self.hsa_challenge_required && self.hsa_trusted_browser != Some(true)
    }

    /// Refuse accounts whose Drive the web API cannot read.
    pub(crate) fn check_web_access(
        &self,
        cookies: &Cookies,
    ) -> Result<(String, String), AuthError> {
        if self.ds_info.is_web_access_allowed == Some(false) {
            return Err(AuthError::AdvancedDataProtection);
        }
        let (Some(drive), Some(docs)) = (&self.webservices.drivews, &self.webservices.docws) else {
            return Err(AuthError::DriveUnavailable);
        };
        if drive.url.is_empty() || docs.url.is_empty() {
            return Err(AuthError::DriveUnavailable);
        }
        if (drive.pcs_required || docs.pcs_required) && !cookies.contains(PCS_DOCUMENTS_COOKIE) {
            return Err(AuthError::AdvancedDataProtection);
        }
        Ok((drive.url.clone(), docs.url.clone()))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn web_access_is_refused_under_advanced_data_protection() {
        let info: AccountInfo = serde_json::from_value(json!({
            "webservices": {
                "drivews": {"url": "https://p1-drivews.icloud.com", "pcsRequired": true},
                "docws": {"url": "https://p1-docws.icloud.com", "pcsRequired": true}
            }
        }))
        .unwrap();
        assert!(matches!(
            info.check_web_access(&Cookies::default()),
            Err(AuthError::AdvancedDataProtection)
        ));
        let granted = Cookies::from_header("X-APPLE-WEBAUTH-PCS-Documents=grant");
        assert!(info.check_web_access(&granted).is_ok());
        assert_eq!(
            AuthError::AdvancedDataProtection.to_string(),
            "iCloud web access is unavailable with Advanced Data Protection"
        );
    }
}
