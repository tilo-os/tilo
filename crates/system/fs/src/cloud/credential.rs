//! The credential handoff between the account service and a mount.
//!
//! The service is the only process that opens the account vault. It reads
//! one account's secret the way Files' former browser did
//! (`SecretVault::get` for that account id, nothing else) and hands the mount
//! process just the [`Credential`] its provider needs, as a line on the
//! mount's stdin. It never appears in arguments, the environment or logs.
//!
//! When the server rejects a credential the mount says so on stdout; the
//! service re-reads the vault (Settings may have refreshed a token or the
//! person may have signed in again) and sends the new credential, or marks
//! the account as needing sign-in. Closing the mount's stdin tells it to
//! unmount.

use std::io::{BufRead, Write};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cloud_source::{Credential, CredentialSource, Error};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::providers::MountSpec;

/// Printed by a mount once the filesystem is serving.
pub const READY_LINE: &str = "TILO_CLOUD_MOUNT_READY_V1";
/// Printed by a mount when the server rejected its credential.
pub const REJECTED_LINE: &str = "credential-rejected";
/// Prefix of a health line (`health <state>`).
pub const HEALTH_PREFIX: &str = "health ";

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
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    pub fn pump(&self, input: impl BufRead, on_close: impl FnOnce()) {
        for line in input.lines() {
            let Ok(line) = line else { break };
            let line = Zeroizing::new(line);
            if let Ok(ToMount::Credential(wire)) = serde_json::from_str::<ToMount>(&line) {
                self.update(wire.into());
            }
        }
        self.close();
        on_close();
    }
}

impl CredentialSource for PipeCredential {
    fn credential(&self) -> cloud_source::Result<Credential> {
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
        let first = {
            let mut state = self.lock();
            let first = state.rejected != Some(state.generation);
            state.rejected = Some(state.generation);
            first
        };
        if first && let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{REJECTED_LINE}");
            let _ = out.flush();
        }
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
}
