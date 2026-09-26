//! Background authentication worker + the UI-facing update protocol.
//!
//! The greetd conversation ([`crate::greetd_client`]) is blocking: it
//! waits on socket round-trips and — for fingerprint — on the user
//! physically touching the reader. Running it inline in the `ui` event
//! loop would freeze the greeter (no "Authenticating…" frame, no live
//! fingerprint prompt). So [`spawn_auth`] runs it on a worker thread and
//! pumps progress back into the running [`ui::App`] via
//! [`ui::app::UserEvent::External`] — the same out-of-band channel the
//! installer uses to stream backend output.
//!
//! The payload is a compact, dependency-free string ([`AuthUpdate`]) so it
//! rides the `External(String)` variant without pulling in serde.

use winit::event_loop::EventLoopProxy;
use zeroize::Zeroizing;

use ui::app::UserEvent;

use crate::greetd_client::{AuthPrompt, authenticate_and_start};

/// Field separator for the [`AuthUpdate`] wire form. A control character
/// that never appears in PAM prompt text or our own copy.
const SEP: char = '\u{1f}';

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
    /// Authentication failed — return to the form. `message` is the
    /// user-facing one-liner; `detail` is optional raw secondary text.
    Failed {
        message: String,
        detail: Option<String>,
    },
}

impl AuthUpdate {
    /// Encode to the compact `External` wire form.
    pub fn encode(&self) -> String {
        match self {
            AuthUpdate::Prompt { text, is_error } => {
                format!("P{SEP}{}{SEP}{text}", i32::from(*is_error))
            }
            AuthUpdate::Accepted => "A".to_string(),
            AuthUpdate::Failed { message, detail } => {
                format!("F{SEP}{message}{SEP}{}", detail.as_deref().unwrap_or(""))
            }
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
                let message = it.next()?.to_string();
                let d = it.next().unwrap_or("");
                Some(AuthUpdate::Failed {
                    message,
                    detail: if d.is_empty() {
                        None
                    } else {
                        Some(d.to_string())
                    },
                })
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
/// it sends an [`AuthUpdate::Failed`] so the UI returns to the form.
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
        if let Err(e) = result {
            let _ = proxy.send_event(UserEvent::External(
                AuthUpdate::Failed {
                    message: e.user_message(),
                    detail: e.detail(),
                }
                .encode(),
            ));
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
        // Phase `failed_roundtrips_with_and_without_detail`.
        {
            roundtrip(AuthUpdate::Failed {
                message: "Incorrect username or password".to_string(),
                detail: None,
            });
            roundtrip(AuthUpdate::Failed {
                message: "Could not start the session".to_string(),
                detail: Some("session slot busy".to_string()),
            });
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
        }
    }
}
