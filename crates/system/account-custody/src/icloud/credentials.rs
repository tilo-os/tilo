//! A [`CredentialSource`] over a [`StoredSession`] that keeps it alive with
//! [`Authenticator::refresh`].

use std::fmt;
use std::sync::{Arc, Mutex};

use cloud_source::{Credential, CredentialSource};
use cloud_source_icloud::auth::{AuthError, StoredSession};
use zeroize::Zeroizing;

use super::auth::Authenticator;

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
    ///
    /// # Errors
    /// What [`Authenticator::refresh`] reports.
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
