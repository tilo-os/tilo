//! The persisted result of a sign-in, and a [`CredentialSource`] that keeps
//! it alive.

use std::fmt;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use cloud_source::{Credential, CredentialSource};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{AccountInfo, AuthError, Authenticator};
use crate::http::{Cookies, secret};

/// An iCloud web session: the cookies `accountLogin` set, plus the idmsa
/// session and trust tokens that renew it. Settings stores it (it is all
/// secret apart from the URLs and IDs); [`StoredSession::credential`] is
/// what a Drive source borrows per request.
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

    pub fn has_trust_token(&self) -> bool {
        !self.trust_token.is_empty()
    }

    /// The trust token, to pass to [`Authenticator::sign_in`] when the person
    /// signs in again so Apple can skip the second factor. Secret.
    pub fn trust_token(&self) -> &str {
        &self.trust_token
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
    pub(crate) fn apply(&mut self, info: &AccountInfo) -> Result<(), AuthError> {
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

/// What [`SessionCredentials`] knows about its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// The last credential handed out has not been rejected.
    Active,
    /// Renewal failed with [`AuthError::SessionExpired`]: a person has to
    /// sign in again with a verification code.
    NeedsSignIn,
    /// The account turned on Advanced Data Protection.
    AdvancedDataProtection,
}

type Renewed = Box<dyn Fn(&StoredSession) + Send + Sync>;

struct State {
    session: StoredSession,
    stale: bool,
    status: SessionStatus,
}

struct Inner {
    auth: Authenticator,
    password: Option<Zeroizing<String>>,
    state: Mutex<State>,
    renewed: Option<Renewed>,
}

/// A [`CredentialSource`] over a [`StoredSession`] that renews it with
/// [`Authenticator::refresh`] after the server rejects it, and remembers when
/// only a person can help ([`SessionStatus::NeedsSignIn`]). Cheap to clone.
#[derive(Clone)]
pub struct SessionCredentials {
    inner: Arc<Inner>,
}

impl fmt::Debug for SessionCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCredentials")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl SessionCredentials {
    /// `password`, when Settings keeps it, lets a renewal re-run SRP with the
    /// trust token after the web cookies and session token have lapsed.
    /// `renewed` is called with every renewed session so it can be stored.
    pub fn new(
        auth: Authenticator,
        session: StoredSession,
        password: Option<Zeroizing<String>>,
        renewed: Option<Renewed>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                auth,
                password,
                state: Mutex::new(State {
                    session,
                    stale: false,
                    status: SessionStatus::Active,
                }),
                renewed,
            }),
        }
    }

    pub fn status(&self) -> SessionStatus {
        self.inner
            .state
            .lock()
            .map_or(SessionStatus::NeedsSignIn, |state| state.status)
    }

    /// The current session (for storage).
    pub fn session(&self) -> Option<StoredSession> {
        self.inner
            .state
            .lock()
            .ok()
            .map(|state| state.session.clone())
    }

    /// Check the session with Apple now, renewing it if needed.
    pub fn verify(&self) -> Result<(), AuthError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AuthError::Protocol("poisoned"))?;
        self.renew(&mut state)
    }

    fn renew(&self, state: &mut State) -> Result<(), AuthError> {
        let password = self.inner.password.as_ref().map(|p| p.as_str());
        match self.inner.auth.refresh(&state.session, password) {
            Ok(session) => {
                state.session = session;
                state.stale = false;
                state.status = SessionStatus::Active;
                if let Some(renewed) = &self.inner.renewed {
                    renewed(&state.session);
                }
                Ok(())
            }
            Err(error) => {
                match error {
                    AuthError::SessionExpired => state.status = SessionStatus::NeedsSignIn,
                    AuthError::AdvancedDataProtection => {
                        state.status = SessionStatus::AdvancedDataProtection;
                    }
                    _ => {}
                }
                Err(error)
            }
        }
    }
}

impl CredentialSource for SessionCredentials {
    fn credential(&self) -> cloud_source::Result<Credential> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| cloud_source::Error::Unauthorized)?;
        match state.status {
            SessionStatus::NeedsSignIn => return Err(cloud_source::Error::Unauthorized),
            SessionStatus::AdvancedDataProtection => return Err(cloud_source::Error::Forbidden),
            SessionStatus::Active => {}
        }
        if state.stale {
            self.renew(&mut state)?;
        }
        Ok(state.session.credential())
    }

    fn rejected(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.stale = true;
        }
    }
}
