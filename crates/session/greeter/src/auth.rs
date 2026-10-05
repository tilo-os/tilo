//! Background authentication worker + the UI-facing update protocol.
//!
//! The greetd conversation ([`crate::greetd_client`]) is blocking: it
//! waits on socket round-trips and — for fingerprint — on the user
//! physically touching the reader. Running it inline in the `ui` event
//! loop would freeze the greeter (no "Signing in…" frame, no live
//! fingerprint prompt). So [`spawn_auth`] runs it on a worker thread and
//! pumps progress back into the running [`ui::App`] via
//! [`ui::app::UserEvent::External`] — the same out-of-band channel the
//! installer uses to stream backend output.
//!
//! The payload is a compact, dependency-free string ([`AuthUpdate`]) so it
//! rides the `External(String)` variant without pulling in serde. A failure
//! crosses as its meaning only ([`Failure`]); greetd's and PAM's raw text
//! stays in the log and never reaches the screen.

use winit::event_loop::EventLoopProxy;
use zeroize::Zeroizing;

use ui::app::UserEvent;

use crate::greetd_client::{AuthError, AuthPrompt, Denial, authenticate_and_start};

/// Field separator for the [`AuthUpdate`] wire form. A control character
/// that never appears in PAM prompt text or our own copy.
const SEP: char = '\u{1f}';

/// Why a sign-in attempt ended without a session, as the person signing in
/// needs to understand it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Wrong passcode (or username). Never says which.
    Credentials,
    /// Too many failed attempts.
    Locked,
    AccountExpired,
    PasscodeExpired,
    /// The account may not sign in on this machine.
    NotPermitted,
    /// The credentials could not be checked right now; retrying may work.
    Unavailable,
    /// Signed in, but the desktop session did not start.
    SessionStart,
    /// greetd itself could not be reached.
    Service,
}

impl Failure {
    const ALL: [Self; 8] = [
        Self::Credentials,
        Self::Locked,
        Self::AccountExpired,
        Self::PasscodeExpired,
        Self::NotPermitted,
        Self::Unavailable,
        Self::SessionStart,
        Self::Service,
    ];

    fn wire(self) -> &'static str {
        match self {
            Self::Credentials => "credentials",
            Self::Locked => "locked",
            Self::AccountExpired => "account-expired",
            Self::PasscodeExpired => "passcode-expired",
            Self::NotPermitted => "not-permitted",
            Self::Unavailable => "unavailable",
            Self::SessionStart => "session",
            Self::Service => "service",
        }
    }
}

impl From<&AuthError> for Failure {
    fn from(error: &AuthError) -> Self {
        match error {
            AuthError::Denied { reason, .. } => match reason {
                Denial::Credentials => Self::Credentials,
                Denial::Locked => Self::Locked,
                Denial::AccountExpired => Self::AccountExpired,
                Denial::PasscodeExpired => Self::PasscodeExpired,
                Denial::NotPermitted => Self::NotPermitted,
                Denial::Unavailable => Self::Unavailable,
            },
            AuthError::SessionStart(_) => Self::SessionStart,
            // greetd refusing the request itself (a session slot still being
            // torn down) passes on a retry.
            AuthError::Greetd(_) => Self::Unavailable,
            AuthError::Transport(_) => Self::Service,
        }
    }
}

/// A state update sent from the auth worker thread to the `GreeterApp` on
/// the UI thread. Encoded to / decoded from the [`UserEvent::External`]
/// string payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthUpdate {
    /// A PAM conversation prompt to display while auth is in progress
    /// (fingerprint instructions, info, or a warning). `is_error` styles
    /// it as a warning.
    Prompt { text: String, is_error: bool },
    /// Credentials accepted; greetd is starting the session. Lets the UI
    /// resolve its in-flight state into a visible success frame instead of
    /// holding a spinner until the process exits.
    Accepted,
    /// The attempt ended without a session — return to the form.
    Failed(Failure),
}

impl AuthUpdate {
    /// Encode to the compact `External` wire form.
    pub fn encode(&self) -> String {
        match self {
            AuthUpdate::Prompt { text, is_error } => {
                format!("P{SEP}{}{SEP}{text}", i32::from(*is_error))
            }
            AuthUpdate::Accepted => "A".to_string(),
            AuthUpdate::Failed(failure) => format!("F{SEP}{}", failure.wire()),
        }
    }

    /// Decode a wire payload produced by [`AuthUpdate::encode`]. Returns
    /// `None` for anything that isn't a recognised update (so unrelated
    /// `External` payloads are ignored).
    pub fn decode(s: &str) -> Option<Self> {
        let mut it = s.splitn(3, SEP);
        match it.next()? {
            "A" => Some(AuthUpdate::Accepted),
            "P" => {
                let flag = it.next()?;
                let text = it.next().unwrap_or("").to_string();
                Some(AuthUpdate::Prompt {
                    text,
                    is_error: flag == "1",
                })
            }
            "F" => {
                let wire = it.next()?;
                Failure::ALL
                    .into_iter()
                    .find(|failure| failure.wire() == wire)
                    .map(AuthUpdate::Failed)
            }
            _ => None,
        }
    }
}

/// Run the greetd authenticate-and-start exchange on a background thread,
/// reporting prompts and the final failure back to the UI through `proxy`.
///
/// On success the exchange `process::exit(0)`s from the worker (greetd then
/// takes over the VT), so this thread simply ends the process. On failure
/// it logs greetd's own description and sends an [`AuthUpdate::Failed`] so
/// the UI returns to the form.
pub fn spawn_auth(
    proxy: EventLoopProxy<UserEvent>,
    sock: String,
    username: String,
    password: Zeroizing<String>,
    cmd: Vec<String>,
    env: Vec<String>,
) {
    std::thread::spawn(move || {
        let prompt_proxy = proxy.clone();
        let accepted_proxy = proxy.clone();
        let result = authenticate_and_start(
            &sock,
            &username,
            &password,
            &cmd,
            &env,
            move |prompt: AuthPrompt| {
                let _ = prompt_proxy.send_event(UserEvent::External(
                    AuthUpdate::Prompt {
                        text: prompt.text,
                        is_error: prompt.is_error,
                    }
                    .encode(),
                ));
            },
            move || {
                let _ =
                    accepted_proxy.send_event(UserEvent::External(AuthUpdate::Accepted.encode()));
            },
        );
        // `authenticate_and_start` exits the process on success; only the
        // Err path returns here.
        if let Err(error) = result {
            let failure = Failure::from(&error);
            tracing::warn!(?failure, "sign-in did not start a session");
            let _ = proxy.send_event(UserEvent::External(AuthUpdate::Failed(failure).encode()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(u: AuthUpdate) {
        let encoded = u.encode();
        assert_eq!(
            AuthUpdate::decode(&encoded),
            Some(u),
            "roundtrip: {encoded:?}"
        );
    }

    #[test]
    fn auth_frames_roundtrip_and_garbage_decodes_to_none() {
        // Phase `prompt_roundtrips`.
        {
            roundtrip(AuthUpdate::Prompt {
                text: "Place your finger on the reader".to_string(),
                is_error: false,
            });
            roundtrip(AuthUpdate::Prompt {
                text: "Swipe didn't match, try again".to_string(),
                is_error: true,
            });
        }
        // Phase `every_failure_roundtrips`.
        for failure in Failure::ALL {
            roundtrip(AuthUpdate::Failed(failure));
        }
        // Phase `prompt_text_with_separator_survives`.
        {
            // Defensive: even if a prompt contained the separator, splitn(3)
            // keeps the remainder intact so the text isn't truncated.
            roundtrip(AuthUpdate::Prompt {
                text: format!("weird{SEP}prompt"),
                is_error: false,
            });
        }
        // Phase `garbage_decodes_to_none`.
        {
            assert_eq!(AuthUpdate::decode(""), None);
            assert_eq!(AuthUpdate::decode("hello world"), None);
            assert_eq!(AuthUpdate::decode("X\u{1f}1\u{1f}foo"), None);
            assert_eq!(
                AuthUpdate::decode("F\u{1f}pam_authenticate: AUTH_ERR"),
                None
            );
        }
    }

    #[test]
    fn every_greetd_outcome_maps_to_a_failure_the_screen_can_explain() {
        let denied = |description: &str| AuthError::Denied {
            reason: Denial::from_description(description),
            detail: description.to_string(),
        };
        for (error, failure) in [
            (denied("pam_authenticate: AUTH_ERR"), Failure::Credentials),
            (
                denied("pam_authenticate: USER_UNKNOWN"),
                Failure::Credentials,
            ),
            (denied("pam_authenticate: MAXTRIES"), Failure::Locked),
            (
                denied("pam_acct_mgmt: ACCT_EXPIRED"),
                Failure::AccountExpired,
            ),
            (
                denied("pam_acct_mgmt: NEW_AUTHTOK_REQD"),
                Failure::PasscodeExpired,
            ),
            (denied("pam_acct_mgmt: PERM_DENIED"), Failure::NotPermitted),
            (
                denied("pam_authenticate: AUTHINFO_UNAVAIL"),
                Failure::Unavailable,
            ),
            (
                AuthError::SessionStart("pam_open_session: SESSION_ERR".into()),
                Failure::SessionStart,
            ),
            (
                AuthError::Greetd("a session is already being configured".into()),
                Failure::Unavailable,
            ),
            (
                AuthError::Transport("cannot connect to greetd: refused".into()),
                Failure::Service,
            ),
        ] {
            assert_eq!(Failure::from(&error), failure, "{error:?}");
        }
    }
}
