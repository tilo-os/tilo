//! `iCloud` Drive sign-in, behind a small trait.
//!
//! Apple offers no public API for `iCloud` Drive; the provider crate speaks
//! the web API `iCloud.com` uses, and its sign-in is interactive: Apple ID
//! and password, then usually a six-digit code shown on a trusted device.
//! What comes back is a long-lived *session state* (trust and session
//! tokens, cookies) that Settings seals in the vault in place of the
//! password, which is never stored. From that state the provider derives
//! short-lived request headers ([`ICloudHeaders`]) for the storage daemon.
//!
//! [`ICloudAuth`] is the surface Settings needs. [`WebICloud`] implements it
//! with `cloud-source-icloud`'s [`Authenticator`]; [`UnavailableICloud`]
//! reports [`ICloudAuthError::Unavailable`] (Settings shows a clear "not
//! available" state) for a custodian that was given no `iCloud` sign-in.

use std::fmt;

use cloud_source_icloud::ICloudConfig;
use cloud_source_icloud::auth::{
    AuthError, Authenticator, Endpoints, PendingSignIn, SignIn, StoredSession,
};
use zeroize::Zeroizing;

/// Opaque long-lived session state, serialised by the provider. Sealed in
/// the vault; never handed to the daemon.
#[derive(Clone, PartialEq, Eq)]
pub struct ICloudSessionState(pub Zeroizing<String>);

impl fmt::Debug for ICloudSessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ICloudSessionState(..)")
    }
}

/// Opaque state of a sign-in waiting for its verification code.
#[derive(Clone, PartialEq, Eq)]
pub struct ICloudChallenge(pub Zeroizing<String>);

impl fmt::Debug for ICloudChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ICloudChallenge(..)")
    }
}

/// Short-lived request headers for the storage daemon.
pub struct ICloudHeaders {
    pub headers: Vec<(String, Zeroizing<String>)>,
    /// Unix seconds after which the headers stop working, if known.
    pub expires_unix: Option<u64>,
}

impl fmt::Debug for ICloudHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ICloudHeaders")
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("expires_unix", &self.expires_unix)
            .finish()
    }
}

/// The outcome of submitting Apple ID and password.
#[derive(Debug)]
pub enum ICloudSignIn {
    /// Signed in without a code (a trusted session was still valid).
    Complete(ICloudSessionState),
    /// Apple sent a verification code to the person's trusted devices.
    NeedsCode(ICloudChallenge),
}

/// Typed `iCloud` sign-in failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ICloudAuthError {
    /// Apple ID or password rejected.
    InvalidCredentials,
    /// The verification code was wrong or expired.
    InvalidCode,
    /// Advanced Data Protection is on and web access to `iCloud` data is off,
    /// so `iCloud` Drive cannot be reached through the web API.
    AdvancedDataProtection,
    /// The session needs the two-factor step again.
    NeedsReauthentication,
    /// The Apple ID is locked or needs attention at appleid.apple.com.
    AccountLocked,
    /// Apple could not be reached.
    Offline,
    /// No `iCloud` provider is linked into this build.
    Unavailable,
    /// Anything else, without secrets or response bodies.
    Other(String),
}

impl fmt::Display for ICloudAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCredentials => "Apple ID or password is incorrect",
            Self::InvalidCode => "that verification code is incorrect or expired",
            Self::AdvancedDataProtection => {
                "Advanced Data Protection is on for this Apple ID, so iCloud Drive is not \
                 reachable from the web. On your iPhone, iPad or Mac, turn on \u{201c}Access \
                 iCloud Data on the Web\u{201d} in iCloud settings, then try again"
            }
            Self::NeedsReauthentication => "iCloud needs you to confirm this device again",
            Self::AccountLocked => {
                "this Apple ID is locked or needs attention; visit appleid.apple.com"
            }
            Self::Offline => "could not reach iCloud",
            Self::Unavailable => "iCloud Drive sign-in is not available in this build",
            Self::Other(message) => return f.write_str(message),
        })
    }
}

impl std::error::Error for ICloudAuthError {}

/// The `iCloud` sign-in surface Settings drives.
pub trait ICloudAuth: Send + Sync {
    /// Whether this build can sign in to `iCloud` Drive at all.
    fn is_available(&self) -> bool {
        true
    }

    /// Submit Apple ID and password.
    ///
    /// # Errors
    /// See [`ICloudAuthError`].
    fn sign_in(&self, apple_id: &str, password: &str) -> Result<ICloudSignIn, ICloudAuthError>;

    /// Submit the verification code for a pending sign-in.
    ///
    /// # Errors
    /// See [`ICloudAuthError`].
    fn verify_code(
        &self,
        challenge: &ICloudChallenge,
        code: &str,
    ) -> Result<ICloudSessionState, ICloudAuthError>;

    /// Derive short-lived request headers from a stored session, renewing it
    /// where the session allows. Returns the (possibly renewed) state to
    /// store back. [`ICloudAuthError::NeedsReauthentication`] when the
    /// session requires the two-factor step again.
    ///
    /// # Errors
    /// See [`ICloudAuthError`].
    fn resume(
        &self,
        session: &ICloudSessionState,
    ) -> Result<(ICloudSessionState, ICloudHeaders), ICloudAuthError>;

    /// End the session at Apple, best effort.
    fn sign_out(&self, _session: &ICloudSessionState) {}

    /// The non-secret mount configuration of accounts signed in here (the
    /// provider's own serde configuration), `Null` when there is none.
    fn storage_config(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

/// `iCloud` sign-in through `cloud-source-icloud`'s [`Authenticator`]:
/// SRP with Apple ID and password, the two-factor code, and renewal of the
/// stored session without a person.
#[derive(Clone, Debug)]
pub struct WebICloud {
    auth: Authenticator,
}

impl WebICloud {
    /// Against `endpoints` (Apple's, or a fake server's).
    ///
    /// # Errors
    /// [`ICloudAuthError::Offline`] when the HTTPS client cannot be built.
    pub fn new(endpoints: Endpoints) -> Result<Self, ICloudAuthError> {
        Authenticator::new(endpoints)
            .map(|auth| Self { auth })
            .map_err(auth_error)
    }

    /// Against Apple's production endpoints.
    ///
    /// # Errors
    /// See [`Self::new`].
    pub fn apple() -> Result<Self, ICloudAuthError> {
        Self::new(Endpoints::apple())
    }
}

fn auth_error(error: AuthError) -> ICloudAuthError {
    match error {
        AuthError::InvalidCredentials => ICloudAuthError::InvalidCredentials,
        AuthError::InvalidCode => ICloudAuthError::InvalidCode,
        AuthError::AccountLocked => ICloudAuthError::AccountLocked,
        AuthError::AdvancedDataProtection => ICloudAuthError::AdvancedDataProtection,
        AuthError::SessionExpired => ICloudAuthError::NeedsReauthentication,
        AuthError::Offline => ICloudAuthError::Offline,
        other => ICloudAuthError::Other(other.to_string()),
    }
}

fn session_state(session: &StoredSession) -> ICloudSessionState {
    ICloudSessionState(session.to_json())
}

fn stored(session: &ICloudSessionState) -> Result<StoredSession, ICloudAuthError> {
    StoredSession::from_json(&session.0).map_err(|_| ICloudAuthError::NeedsReauthentication)
}

impl ICloudAuth for WebICloud {
    fn sign_in(&self, apple_id: &str, password: &str) -> Result<ICloudSignIn, ICloudAuthError> {
        match self
            .auth
            .sign_in(apple_id, password, None)
            .map_err(auth_error)?
        {
            SignIn::Complete(session) => Ok(ICloudSignIn::Complete(session_state(&session))),
            SignIn::NeedsCode(pending) => {
                let text = serde_json::to_string(&pending)
                    .map_err(|_| ICloudAuthError::Other("could not keep the sign-in".into()))?;
                Ok(ICloudSignIn::NeedsCode(ICloudChallenge(Zeroizing::new(
                    text,
                ))))
            }
        }
    }

    fn verify_code(
        &self,
        challenge: &ICloudChallenge,
        code: &str,
    ) -> Result<ICloudSessionState, ICloudAuthError> {
        let mut pending: PendingSignIn = serde_json::from_str(&challenge.0)
            .map_err(|_| ICloudAuthError::Other("the sign-in expired; start again".into()))?;
        self.auth
            .submit_code(&mut pending, code)
            .map(|session| session_state(&session))
            .map_err(auth_error)
    }

    fn resume(
        &self,
        session: &ICloudSessionState,
    ) -> Result<(ICloudSessionState, ICloudHeaders), ICloudAuthError> {
        let renewed = self
            .auth
            .refresh(&stored(session)?, None)
            .map_err(auth_error)?;
        let cloud_source::Credential::Session { headers, .. } = renewed.credential() else {
            return Err(ICloudAuthError::Other(
                "the iCloud session has no web cookies".into(),
            ));
        };
        Ok((
            session_state(&renewed),
            ICloudHeaders {
                headers,
                // Apple does not say; a rejected request asks for new ones.
                expires_unix: None,
            },
        ))
    }

    fn storage_config(&self) -> serde_json::Value {
        serde_json::to_value(ICloudConfig::with_endpoints(self.auth.endpoints()))
            .unwrap_or_default()
    }
}

/// The [`ICloudAuth`] of a build without an `iCloud` provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableICloud;

impl ICloudAuth for UnavailableICloud {
    fn is_available(&self) -> bool {
        false
    }

    fn sign_in(&self, _apple_id: &str, _password: &str) -> Result<ICloudSignIn, ICloudAuthError> {
        Err(ICloudAuthError::Unavailable)
    }

    fn verify_code(
        &self,
        _challenge: &ICloudChallenge,
        _code: &str,
    ) -> Result<ICloudSessionState, ICloudAuthError> {
        Err(ICloudAuthError::Unavailable)
    }

    fn resume(
        &self,
        _session: &ICloudSessionState,
    ) -> Result<(ICloudSessionState, ICloudHeaders), ICloudAuthError> {
        Err(ICloudAuthError::Unavailable)
    }
}
