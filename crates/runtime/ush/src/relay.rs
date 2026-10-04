//! Environments on a relay, carried from this device.
//!
//! A Terminal tab whose target is an environment on a relay still gets its
//! pty from this supervisor. What runs on the pty's other end is not a guest
//! but `tilo-relay-environment`, which carries the terminal to the session on
//! the relay (`crates/hosting/tilo-relay/src/environment_cli.rs`) and reports
//! on a status descriptor: the session's id first — the tab keeps it to
//! re-attach — then any tool the relay built, then the end.
//!
//! Which relays a person has is what this device enrolled for them: the
//! home-peer registrations beside the identity keys (`home-peers.json`), for
//! this device's user and their personal workspace. A request names a relay
//! by its endpoint id; one that is not among them is refused.

use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tilo_user_identity::IdentityPaths;

/// Largest home-peer registry read.
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

/// How long a relay session may take to open before the tab is told why not.
pub const OPEN_PATIENCE: Duration = Duration::from_secs(30);

/// The descriptor `tilo-relay-environment` writes its status lines to.
const STATUS_FD: RawFd = 3;

/// One of the person's relays, as this device enrolled it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelayPeer {
    /// The relay's endpoint id (hex) — what a target names.
    pub relay: String,
    /// What the person calls it.
    pub label: String,
    /// The relay's user root (hex).
    #[serde(skip)]
    pub user: String,
    /// The workspace the device's environments live in on the relay.
    #[serde(skip)]
    pub workspace: u64,
    /// The relay's last advertised endpoint address, as registered (JSON);
    /// without one the relay is found by its id.
    #[serde(skip)]
    pub address: Option<String>,
    /// Whether the relay last said a worker runs environment sessions
    /// (the registration's `environments.sessions`).
    #[serde(skip)]
    pub runs_environments: bool,
}

/// The part of a home-peer registration a relay session needs.
#[derive(serde::Deserialize)]
struct Registration {
    relay_service_node_id: String,
    #[serde(default)]
    label: String,
    user_public_key: String,
    #[serde(default)]
    relay_workspace_ids: Vec<u64>,
    #[serde(default)]
    relay_endpoint: Option<serde_json::Value>,
    #[serde(default)]
    environments: tilo_relay_protocol::RelayEnvironments,
}

/// The identity this supervisor's user has on this device.
pub fn identity_paths() -> IdentityPaths {
    std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|root| !root.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under)
}

/// The directory the identity keys are kept under (`/var/lib/tilo`).
pub fn identity_root(identity: &IdentityPaths) -> Option<PathBuf> {
    identity
        .user_pubkey
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// The person's relays: registrations of this device's user that keep their
/// personal workspace. Anything unreadable is no relay.
pub fn relays(identity: &IdentityPaths) -> Vec<RelayPeer> {
    let Ok(user) = tilo_user_identity::persist::read_public_key(&identity.user_pubkey) else {
        return Vec::new();
    };
    let Some(path) = identity_root(identity).map(|root| root.join("home-peers.json")) else {
        return Vec::new();
    };
    relays_in(&path, &user)
}

/// [`relays`] from one registry file, for `user`.
pub fn relays_in(path: &Path, user: &[u8; 32]) -> Vec<RelayPeer> {
    let readable = std::fs::metadata(path).is_ok_and(|meta| meta.len() <= MAX_REGISTRY_BYTES);
    let Some(registrations) = readable
        .then(|| std::fs::read(path).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Vec<Registration>>(&bytes).ok())
    else {
        return Vec::new();
    };
    let user_hex = hex::encode(user);
    let workspace = tilo_relay_protocol::personal_continuity_workspace_id(user);
    let mut out: Vec<RelayPeer> = registrations
        .into_iter()
        .filter(|r| r.user_public_key.eq_ignore_ascii_case(&user_hex))
        .filter(|r| r.relay_workspace_ids.contains(&workspace))
        .map(|r| RelayPeer {
            label: if r.label.trim().is_empty() {
                r.relay_service_node_id.chars().take(8).collect()
            } else {
                r.label
            },
            relay: r.relay_service_node_id.to_ascii_lowercase(),
            user: r.user_public_key.to_ascii_lowercase(),
            workspace,
            address: r
                .relay_endpoint
                .filter(|address| !address.is_null())
                .map(|address| address.to_string()),
            runs_environments: r.environments.sessions,
        })
        .filter(|peer| {
            tilo_environment::Location::Relay {
                relay: peer.relay.clone(),
            }
            .is_valid()
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out.dedup_by(|a, b| a.relay == b.relay);
    out
}

/// The relays a new environment can be opened on: the person's relays whose
/// workers last said they run environment sessions. A session already on a
/// relay re-attaches through [`relays`] whatever the relay last said.
pub fn offered(identity: &IdentityPaths) -> Vec<RelayPeer> {
    relays(identity)
        .into_iter()
        .filter(|peer| peer.runs_environments)
        .collect()
}

/// Where `tilo-relay-environment` is: `$TILO_RELAY_ENVIRONMENT_BIN`, the
/// installed location, then `PATH`.
pub fn bridge_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("TILO_RELAY_ENVIRONMENT_BIN") {
        return Some(PathBuf::from(explicit));
    }
    let installed = PathBuf::from("/usr/libexec/tilo-relay-environment");
    if installed.exists() {
        return Some(installed);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("tilo-relay-environment"))
        .find(|candidate| candidate.is_file())
}

/// A relay session to open from a record, or to re-attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaySession {
    Open { record: String },
    Attach { session: String },
}

/// Whether `session` is a session id a request may carry: 16 bytes in hex.
pub fn is_session_id(session: &str) -> bool {
    session.len() == 32 && session.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The bridge's command line for `session` on `peer`. Its status lines go to
/// descriptor 3.
pub fn bridge_command(
    binary: &Path,
    peer: &RelayPeer,
    session: &RelaySession,
    records_root: &Path,
    identity_root: &Path,
    term: &str,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--relay")
        .arg(&peer.relay)
        .arg("--relay-user")
        .arg(&peer.user)
        .arg("--workspace")
        .arg(peer.workspace.to_string())
        .arg("--identity-root")
        .arg(identity_root)
        .arg("--status-fd")
        .arg(STATUS_FD.to_string());
    if let Some(address) = &peer.address {
        cmd.arg("--relay-addr").arg(address);
    }
    match session {
        RelaySession::Open { record } => {
            cmd.arg("--record")
                .arg(record)
                .arg("--records-root")
                .arg(records_root);
        }
        RelaySession::Attach { session } => {
            cmd.arg("--session").arg(session);
        }
    }
    cmd.env("TERM", term);
    cmd
}

/// One status line from the bridge.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgeStatus {
    Session {
        session: String,
        #[serde(default)]
        resumed: bool,
    },
    ToolBuilt {
        record: String,
        #[serde(default)]
        packages: Vec<String>,
        #[serde(default)]
        images: std::collections::BTreeMap<String, String>,
    },
    Ended {
        #[serde(default)]
        code: Option<i32>,
        #[serde(default)]
        reason: Option<String>,
    },
}

/// A pipe whose write end the bridge gets as descriptor 3.
///
/// # Errors
/// Returns the pipe's creation error.
pub fn status_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` holds the two descriptors pipe2 creates.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded, so both are new descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Make `write` the child's descriptor 3 (in `pre_exec`).
///
/// # Errors
/// Returns `dup2`'s error.
///
/// # Safety
/// Async-signal-safe: only `dup2`.
pub unsafe fn install_status_fd(write: RawFd) -> std::io::Result<()> {
    // SAFETY: dup2 onto a fixed slot; the new descriptor has no CLOEXEC.
    if unsafe { libc::dup2(write, STATUS_FD) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Read status lines from `read`, the first within [`OPEN_PATIENCE`].
pub struct StatusReader {
    lines: BufReader<std::fs::File>,
}

impl StatusReader {
    pub fn new(read: OwnedFd) -> Self {
        Self {
            lines: BufReader::new(std::fs::File::from(read)),
        }
    }

    /// The next status, waiting at most `patience` when given; `None` at the
    /// end, on a timeout, or for a line that is not one.
    pub fn next(&mut self, patience: Option<Duration>) -> Option<BridgeStatus> {
        if let Some(patience) = patience
            && self.lines.buffer().is_empty()
        {
            let mut poll = libc::pollfd {
                fd: self.lines.get_ref().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = i32::try_from(patience.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: one initialised pollfd.
            if unsafe { libc::poll(&raw mut poll, 1, millis) } <= 0 {
                return None;
            }
        }
        let mut line = String::new();
        match self.lines.read_line(&mut line) {
            Ok(n) if n > 0 => serde_json::from_str(line.trim()).ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_persons_relays_are_their_registrations_that_keep_their_personal_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home-peers.json");
        let me = [7u8; 32];
        let workspace = tilo_relay_protocol::personal_continuity_workspace_id(&me);
        let relay = "ab".repeat(32);
        std::fs::write(
            &path,
            serde_json::json!([
                {"device_node_id": "x", "relay_service_node_id": relay.to_uppercase(),
                 "label": "Home", "user_public_key": hex::encode(me),
                 "relay_workspace_ids": [workspace],
                 "environments": {"sessions": true, "legacy_apps": false}},
                {"device_node_id": "y", "relay_service_node_id": "12".repeat(32),
                 "label": "Files only", "user_public_key": hex::encode(me),
                 "relay_workspace_ids": [workspace]},
                {"device_node_id": "x", "relay_service_node_id": "cd".repeat(32),
                 "label": "Someone's", "user_public_key": hex::encode([8u8; 32]),
                 "relay_workspace_ids": [workspace]},
                {"device_node_id": "x", "relay_service_node_id": "ef".repeat(32),
                 "label": "Other workspace", "user_public_key": hex::encode(me),
                 "relay_workspace_ids": [1]}
            ])
            .to_string(),
        )
        .unwrap();
        let relays = relays_in(&path, &me);
        assert_eq!(relays.len(), 2);
        let home = relays.iter().find(|peer| peer.label == "Home").unwrap();
        assert_eq!(home.relay, relay);
        assert_eq!(home.workspace, workspace);
        // Only a relay whose workers run environments is offered for a new
        // one; the other stays reachable for sessions already there.
        let offered: Vec<_> = relays
            .iter()
            .filter(|peer| peer.runs_environments)
            .collect();
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].label, "Home");
        assert!(relays_in(&dir.path().join("absent.json"), &me).is_empty());
    }

    #[test]
    fn the_bridge_opens_from_the_record_or_re_attaches_by_session() {
        let peer = RelayPeer {
            relay: "ab".repeat(32),
            label: "Home".into(),
            user: "cd".repeat(32),
            workspace: 42,
            address: None,
            runs_environments: true,
        };
        let args = |session: &RelaySession| -> Vec<String> {
            bridge_command(
                Path::new("/bin/bridge"),
                &peer,
                session,
                Path::new("/records"),
                Path::new("/var/lib/tilo"),
                "xterm",
            )
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
        };
        let open = args(&RelaySession::Open {
            record: "work".into(),
        });
        assert!(open.windows(2).any(|w| w == ["--record", "work"]));
        assert!(open.windows(2).any(|w| w == ["--records-root", "/records"]));
        assert!(open.windows(2).any(|w| w == ["--workspace", "42"]));
        let attach = args(&RelaySession::Attach {
            session: "0f".repeat(16),
        });
        assert!(attach.windows(2).any(|w| w[0] == "--session"));
        assert!(!attach.iter().any(|a| a == "--record"));
        assert!(is_session_id(&"0f".repeat(16)));
        assert!(!is_session_id("../x"));
    }

    #[test]
    fn status_lines_are_read_and_a_silent_bridge_is_given_up_on() {
        let (read, write) = status_pipe().unwrap();
        let mut writer = std::fs::File::from(write);
        std::io::Write::write_all(
            &mut writer,
            b"{\"session\":{\"session\":\"00ff\",\"resumed\":false}}\n",
        )
        .unwrap();
        let mut reader = StatusReader::new(read);
        assert_eq!(
            reader.next(Some(Duration::from_secs(1))),
            Some(BridgeStatus::Session {
                session: "00ff".into(),
                resumed: false
            })
        );
        assert_eq!(reader.next(Some(Duration::from_millis(50))), None);
        drop(writer);
        assert_eq!(reader.next(None), None);
    }
}
