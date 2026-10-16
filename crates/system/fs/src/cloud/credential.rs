//! The pipe between the account service and a mount: the credential
//! handoff, and the service's control of the mount.
//!
//! Neither side opens Settings' account vault. The service reads one
//! account's short-lived credential from the handoff store through
//! [`accounts::HandoffCredentialSource`] (an access token, session headers,
//! or the Basic credential a `WebDAV` server itself needs) and hands the
//! mount process just that [`Credential`], as a line on the mount's stdin.
//! It never appears in arguments, the environment or logs.
//!
//! When the server rejects a credential, or the one the mount holds has
//! expired, the mount says so on stdout; the service asks the handoff for a
//! fresh one (which runs Settings' credential helper when needed) and sends
//! it, or marks the account as needing sign-in. Closing the mount's stdin
//! tells it to unmount.
//!
//! The same pipe carries the control the service serves on its socket
//! (`runtime_fs::cloud`): sync now, pause, resume and file requests
//! (revisions, restore, share link). The mount answers a file request with
//! a [`REPLY_PREFIX`] line; neither direction carries a credential beside
//! the [`ToMount::Credential`] line itself.

use std::io::{BufRead, Write};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cloud_source::{Credential, CredentialSource, Error};
use runtime_fs::cloud::{CloudCapabilities, FileReply, FileRequest};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::providers::MountSpec;
use tilo_lock::MutexRecover as _;

/// Printed by a mount once the filesystem is serving.
pub const READY_LINE: &str = "TILO_CLOUD_MOUNT_READY_V1";
/// Printed by a mount when the server rejected its credential.
pub const REJECTED_LINE: &str = "credential-rejected";
/// Printed by a mount when the credential it holds has expired.
pub const EXPIRED_LINE: &str = "credential-expired";
/// A credential this close to its expiry counts as expired.
const EXPIRY_SKEW: Duration = Duration::from_secs(30);
/// Prefix of a health line (`health <state>`).
pub const HEALTH_PREFIX: &str = "health ";
/// Prefix of the line a mount prints before [`READY_LINE`]: its provider's
/// [`CloudCapabilities`] as JSON.
pub const CAPABILITIES_PREFIX: &str = "capabilities ";
/// Prefix of the answer to a [`ToMount::Request`]: a [`MountReply`] as JSON.
pub const REPLY_PREFIX: &str = "reply ";

/// A mount's answer to [`ToMount::Request`] `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountReply {
    pub id: u64,
    pub result: Result<FileReply, String>,
}

/// The [`CAPABILITIES_PREFIX`] line for `capabilities`.
pub fn capabilities_line(capabilities: CloudCapabilities) -> String {
    format!(
        "{CAPABILITIES_PREFIX}{}",
        serde_json::to_string(&capabilities).unwrap_or_default()
    )
}

/// The [`REPLY_PREFIX`] line for `reply`.
pub fn reply_line(reply: &MountReply) -> String {
    format!(
        "{REPLY_PREFIX}{}",
        serde_json::to_string(reply).unwrap_or_default()
    )
}

/// A credential on the wire. `Debug` never shows secrets.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireCredential {
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
        expires_unix: Option<u64>,
    },
    Session {
        headers: Vec<(String, String)>,
        expires_unix: Option<u64>,
    },
}

impl std::fmt::Debug for WireCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireCredential(..)")
    }
}

fn to_unix(time: Option<SystemTime>) -> Option<u64> {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn from_unix(secs: Option<u64>) -> Option<SystemTime> {
    secs.and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)))
}

impl From<&Credential> for WireCredential {
    fn from(credential: &Credential) -> Self {
        match credential {
            Credential::Basic { username, password } => Self::Basic {
                username: username.clone(),
                password: password.as_str().to_owned(),
            },
            Credential::Bearer { token, expires } => Self::Bearer {
                token: token.as_str().to_owned(),
                expires_unix: to_unix(*expires),
            },
            Credential::Session { headers, expires } => Self::Session {
                headers: headers
                    .iter()
                    .map(|(n, v)| (n.clone(), v.as_str().to_owned()))
                    .collect(),
                expires_unix: to_unix(*expires),
            },
        }
    }
}

impl From<WireCredential> for Credential {
    fn from(wire: WireCredential) -> Self {
        match wire {
            WireCredential::Basic { username, password } => Self::Basic {
                username,
                password: Zeroizing::new(password),
            },
            WireCredential::Bearer {
                token,
                expires_unix,
            } => Self::Bearer {
                token: Zeroizing::new(token),
                expires: from_unix(expires_unix),
            },
            WireCredential::Session {
                headers,
                expires_unix,
            } => Self::Session {
                headers: headers
                    .into_iter()
                    .map(|(n, v)| (n, Zeroizing::new(v)))
                    .collect(),
                expires: from_unix(expires_unix),
            },
        }
    }
}

/// One line from the service to a mount.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToMount {
    Start(MountSpec),
    Credential(WireCredential),
    /// Cut the sync back-off short and sync now.
    SyncNow,
    /// Pause background sync and uploads.
    Pause,
    /// Resume them and sync now.
    Resume,
    /// A file request, answered by a [`REPLY_PREFIX`] line carrying `id`.
    Request {
        id: u64,
        request: FileRequest,
    },
}

/// Serialise a message as one line. The caller writes it to the pipe and
/// drops the buffer (it may hold a secret).
pub fn line(message: &ToMount) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(serde_json::to_vec(message).unwrap_or_default());
    bytes.push(b'\n');
    bytes
}

struct State {
    current: Option<Credential>,
    generation: u64,
    rejected: Option<u64>,
    closed: bool,
}

/// The mount side: the latest credential received on stdin.
pub struct PipeCredential {
    state: Mutex<State>,
    changed: Condvar,
    out: Mutex<Box<dyn Write + Send>>,
}

/// How long a request waits for a fresh credential after a rejection
/// before failing as unauthorized.
const REFRESH_WAIT: Duration = Duration::from_secs(5);

impl PipeCredential {
    pub fn new(initial: Option<Credential>, out: Box<dyn Write + Send>) -> Self {
        Self {
            state: Mutex::new(State {
                current: initial,
                generation: 1,
                rejected: None,
                closed: false,
            }),
            changed: Condvar::new(),
            out: Mutex::new(out),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    pub fn update(&self, credential: Credential) {
        let mut state = self.lock();
        state.current = Some(credential);
        state.generation = state.generation.saturating_add(1);
        state.rejected = None;
        drop(state);
        self.changed.notify_all();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    /// Read service messages until stdin closes, then call `on_close`.
    /// Credentials update this source; every other message goes to
    /// `on_control`.
    pub fn pump(
        &self,
        input: impl BufRead,
        mut on_control: impl FnMut(ToMount),
        on_close: impl FnOnce(),
    ) {
        for line in input.lines() {
            let Ok(line) = line else { break };
            let line = Zeroizing::new(line);
            match serde_json::from_str::<ToMount>(&line) {
                Ok(ToMount::Credential(wire)) => self.update(wire.into()),
                Ok(ToMount::Start(_)) | Err(_) => {}
                Ok(control) => on_control(control),
            }
        }
        self.close();
        on_close();
    }
}

fn expired(credential: &Credential) -> bool {
    let expires = match credential {
        Credential::Basic { .. } => None,
        Credential::Bearer { expires, .. } | Credential::Session { expires, .. } => *expires,
    };
    expires.is_some_and(|expires| {
        SystemTime::now()
            .checked_add(EXPIRY_SKEW)
            .is_some_and(|soon| soon >= expires)
    })
}

impl PipeCredential {
    /// Mark the current credential as unusable and tell the service once.
    fn ask(&self, line: &str) {
        let first = {
            let mut state = self.lock();
            let first = state.rejected != Some(state.generation);
            state.rejected = Some(state.generation);
            first
        };
        if first && let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

impl CredentialSource for PipeCredential {
    fn credential(&self) -> cloud_source::Result<Credential> {
        let due = self.lock().current.as_ref().is_some_and(expired);
        if due {
            self.ask(EXPIRED_LINE);
        }
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, REFRESH_WAIT, |s| {
                s.rejected == Some(s.generation) && !s.closed
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.rejected == Some(state.generation) {
            return Err(Error::Unauthorized);
        }
        state.current.clone().ok_or(Error::Unauthorized)
    }

    fn rejected(&self) {
        self.ask(REJECTED_LINE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_credentials_round_trip_and_never_debug_print() {
        let credential = Credential::Basic {
            username: "ada".into(),
            password: Zeroizing::new("hunter2".into()),
        };
        let wire = WireCredential::from(&credential);
        assert!(!format!("{wire:?}").contains("hunter2"));
        let text = line(&ToMount::Credential(wire));
        let parsed: ToMount = serde_json::from_slice(&text).unwrap();
        assert!(!format!("{parsed:?}").contains("hunter2"));
        let ToMount::Credential(wire) = parsed else {
            panic!("credential");
        };
        let Credential::Basic { password, .. } = Credential::from(wire) else {
            panic!("basic");
        };
        assert_eq!(password.as_str(), "hunter2");
    }

    #[test]
    fn the_pump_hands_control_on_and_keeps_credentials() {
        let pipe = PipeCredential::new(None, Box::new(Vec::new()));
        let mut input = Vec::new();
        for message in [
            ToMount::SyncNow,
            ToMount::Credential(WireCredential::Bearer {
                token: "fresh".into(),
                expires_unix: None,
            }),
            ToMount::Pause,
            ToMount::Request {
                id: 7,
                request: FileRequest::Revisions {
                    path: "a/b.txt".into(),
                },
            },
        ] {
            input.extend_from_slice(&line(&message));
        }
        let mut seen = Vec::new();
        let mut closed = false;
        pipe.pump(
            input.as_slice(),
            |control| seen.push(format!("{control:?}")),
            || closed = true,
        );
        assert!(closed);
        assert_eq!(
            seen,
            [
                "SyncNow",
                "Pause",
                "Request { id: 7, request: Revisions { path: \"a/b.txt\" } }"
            ]
        );
        assert!(seen.iter().all(|control| !control.contains("fresh")));
        let Credential::Bearer { token, .. } = pipe.credential().unwrap() else {
            panic!("bearer");
        };
        assert_eq!(token.as_str(), "fresh");

        let reply = MountReply {
            id: 7,
            result: Ok(FileReply::Link("https://example.test/s/1".into())),
        };
        let text = reply_line(&reply);
        let parsed: MountReply =
            serde_json::from_str(text.strip_prefix(REPLY_PREFIX).unwrap()).unwrap();
        assert_eq!(parsed, reply);
    }

    #[test]
    fn a_rejection_asks_once_and_a_fresh_credential_answers() {
        let out: Box<dyn Write + Send> = Box::new(Vec::new());
        let pipe = PipeCredential::new(
            Some(Credential::Bearer {
                token: Zeroizing::new("old".into()),
                expires: None,
            }),
            out,
        );
        assert!(pipe.credential().is_ok());
        pipe.rejected();
        pipe.rejected();
        pipe.update(Credential::Bearer {
            token: Zeroizing::new("new".into()),
            expires: None,
        });
        let Credential::Bearer { token, .. } = pipe.credential().unwrap() else {
            panic!("bearer");
        };
        assert_eq!(token.as_str(), "new");
    }

    /// The mount's stdout as the service reads it: one message per line.
    struct Lines(std::sync::mpsc::Sender<String>);

    impl Write for Lines {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let text = String::from_utf8_lossy(buf);
            for line in text.lines().filter(|line| !line.is_empty()) {
                let _ = self.0.send(line.to_owned());
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_expired_credential_is_asked_for_again_before_it_is_used() {
        let (lines, received) = std::sync::mpsc::channel();
        let pipe = std::sync::Arc::new(PipeCredential::new(
            Some(Credential::Bearer {
                token: Zeroizing::new("expired".into()),
                expires: Some(SystemTime::now()),
            }),
            Box::new(Lines(lines)),
        ));
        let service = {
            let pipe = std::sync::Arc::clone(&pipe);
            std::thread::spawn(move || {
                // The service answers the line with a fresh credential.
                assert_eq!(received.recv().unwrap(), EXPIRED_LINE);
                pipe.update(Credential::Bearer {
                    token: Zeroizing::new("fresh".into()),
                    expires: None,
                });
            })
        };
        let Credential::Bearer { token, .. } = pipe.credential().unwrap() else {
            panic!("bearer");
        };
        service.join().unwrap();
        assert_eq!(token.as_str(), "fresh");
    }
}
