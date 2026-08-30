//! The permission broker.
//!
//! Starts with the session, runs on the host, outlives nothing. Every
//! privileged request from inside the guest arrives here as a line of JSON
//! on a Unix socket, is decided against [`crate::policy`], and is written
//! to [`crate::audit`] whichever way it went.
//!
//! # Why a broker at all, when the guest is uid 0
//!
//! The guest is root *in its own user namespace*, so it can already write
//! its own layer without asking — what it cannot do is reach the store:
//! `policy.json` and `audit/` are not bound into the namespace at any path.
//! The broker is the only route to the two files that decide what the
//! *next* session can see, and the only writer of the record of what was
//! asked for.
//!
//! A decision here does not take effect in the running guest: a new bind
//! needs a new mount namespace, so `perm trust-dir` answers with
//! [`Response::NeedsRestart`] and the user runs `restart` — pretending
//! otherwise would mean a grant that appears made and has not.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use collab_types::caps::Verb;

use crate::audit::{AuditLog, AuditRecord};
use crate::policy::{Policy, is_trustable};
use crate::profile::Profile;

/// What a guest can ask for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// `perm trust-dir <dir>` — read and write, for good.
    TrustDir { path: PathBuf },
    /// `perm list` — the decisions in force.
    ListPerms,
    /// `perm revoke <id>`.
    Revoke { id: String },
    /// May this path be read / written? Answers `scan`.
    Check { path: PathBuf, write: bool },
    /// A package transaction happened in the guest. Recorded, and — when it was
    /// `--one-time` — remembered so the session can undo it on the way out.
    PkgRecorded {
        package: String,
        action: String,
        one_time: bool,
    },
    /// `pkg freeze` — keep the one-time installs.
    Freeze,
    /// `pkg diff` — what the layer changed relative to the base. Answered on
    /// the host: from inside the guest the two are one merged view.
    LayerDiff { limit: usize },
    /// `pkg burn` — destroy the layer's files, deferred to session end since an
    /// overlay `upperdir` can't be removed from under a live mount.
    Burn,
    /// `guard status`.
    Status,
    /// `restart` — rebuild the guest in place.
    Restart,
}

/// What the broker answers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Response {
    Ok,
    /// Allowed, and it will be in force after `restart`.
    NeedsRestart {
        detail: String,
    },
    Denied {
        reason: String,
    },
    Perms {
        entries: Vec<PermEntry>,
    },
    Check {
        allowed: bool,
    },
    Diff {
        entries: Vec<DiffEntry>,
        truncated: usize,
    },
    Status {
        status: SessionStatus,
    },
}

/// One line of `pkg diff`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffEntry {
    pub path: String,
    pub kind: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PermEntry {
    pub id: String,
    pub scope: String,
    pub allows: String,
    pub expiry: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionStatus {
    pub session: String,
    pub profile: String,
    pub posture: String,
    pub layer: String,
    pub layer_kind: String,
    pub seccomp: String,
    pub landlock: String,
    pub trusted_dirs: Vec<String>,
    pub readable_dirs: Vec<String>,
    pub one_time_packages: Vec<String>,
    pub audit_records: usize,
}

/// The state one broker owns for the life of a session.
#[derive(Debug)]
pub struct BrokerState {
    pub session_id: String,
    pub profile: Profile,
    pub policy_path: PathBuf,
    pub policy: Policy,
    pub audit: AuditLog,
    /// Packages installed this session with `--one-time`, to be removed from
    /// the layer when the session ends.
    pub one_time: Vec<String>,
    /// Set by `restart`; read by the supervisor after the guest exits.
    pub restart_requested: bool,
    /// Set by `pkg burn`; acted on once the overlay is unmounted.
    pub burn_requested: bool,
    /// The host root the layer shadows, for `pkg diff`.
    pub base: PathBuf,
    /// For `guard status`, filled in by the supervisor at start.
    pub layer: PathBuf,
    pub layer_kind: String,
    pub landlock: String,
}

impl BrokerState {
    /// Decide one request and record it. The whole policy surface is here, so
    /// it can be tested without a socket.
    pub fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::TrustDir { path } => self.trust_dir(&path),
            Request::ListPerms => Response::Perms {
                entries: self.perm_entries(),
            },
            Request::Revoke { id } => self.revoke(&id),
            Request::Check { path, write } => {
                let verb = if write { Verb::Write } else { Verb::Read };
                Response::Check {
                    allowed: self
                        .policy
                        .permits(&path, verb, Some(crate::audit::now_unix())),
                }
            }
            Request::PkgRecorded {
                package,
                action,
                one_time,
            } => self.record_pkg(&package, &action, one_time),
            Request::Freeze => self.freeze(),
            Request::LayerDiff { limit } => self.layer_diff(limit),
            Request::Burn => {
                self.burn_requested = true;
                self.restart_requested = true;
                let layer = self.layer.display().to_string();
                self.log("pkg-burn", Some(&layer), true, None);
                Response::NeedsRestart {
                    detail: format!(
                        "{layer} is destroyed when this session ends, and a fresh \
                         layer built in its place."
                    ),
                }
            }
            Request::Status => Response::Status {
                status: self.status(),
            },
            Request::Restart => {
                self.restart_requested = true;
                self.log("restart", None, true, None);
                Response::Ok
            }
        }
    }

    fn trust_dir(&mut self, path: &Path) -> Response {
        // Resolve first: a symlink under a guest-controlled directory could name a
        // path the user never saw. `canonicalize` failing on a missing path is
        // also correct — trusting nothing there.
        let resolved = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                let reason = format!("{}: {e}", path.display());
                self.log(
                    "trust-dir",
                    Some(&path.display().to_string()),
                    false,
                    Some(&reason),
                );
                return Response::Denied { reason };
            }
        };
        if !resolved.is_dir() {
            let reason = format!("{} is not a directory", resolved.display());
            self.log(
                "trust-dir",
                Some(&resolved.display().to_string()),
                false,
                Some(&reason),
            );
            return Response::Denied { reason };
        }
        if !is_trustable(&resolved) {
            let reason = format!(
                "{} is part of the base the overlay owns; binding it would put \
                 the host's copy back on top of your layer",
                resolved.display()
            );
            self.log(
                "trust-dir",
                Some(&resolved.display().to_string()),
                false,
                Some(&reason),
            );
            return Response::Denied { reason };
        }
        let Some(id) = self.policy.trust_dir(&resolved) else {
            let reason = format!("{} cannot be trusted", resolved.display());
            self.log(
                "trust-dir",
                Some(&resolved.display().to_string()),
                false,
                Some(&reason),
            );
            return Response::Denied { reason };
        };
        if let Err(e) = self.policy.save(&self.policy_path) {
            let reason = format!("could not save the decision: {e}");
            self.log(
                "trust-dir",
                Some(&resolved.display().to_string()),
                false,
                Some(&reason),
            );
            return Response::Denied { reason };
        }
        self.log(
            "trust-dir",
            Some(&resolved.display().to_string()),
            true,
            Some(&id),
        );
        Response::NeedsRestart {
            detail: format!(
                "{} is trusted (capability {id}). Run `restart` to bring it into this session.",
                resolved.display()
            ),
        }
    }

    fn revoke(&mut self, id: &str) -> Response {
        if !self.policy.revoke(id) {
            let reason = format!("no decision with id {id}");
            self.log("revoke", Some(id), false, Some(&reason));
            return Response::Denied { reason };
        }
        if let Err(e) = self.policy.save(&self.policy_path) {
            let reason = format!("could not save the decision: {e}");
            self.log("revoke", Some(id), false, Some(&reason));
            return Response::Denied { reason };
        }
        self.log("revoke", Some(id), true, None);
        Response::NeedsRestart {
            detail: format!("{id} revoked. Run `restart` to drop it from this session."),
        }
    }

    fn record_pkg(&mut self, package: &str, action: &str, one_time: bool) -> Response {
        if one_time && action == "install" && !self.one_time.iter().any(|p| p == package) {
            self.one_time.push(package.to_string());
        }
        if action == "remove" {
            self.one_time.retain(|p| p != package);
        }
        self.log(
            &format!("pkg-{action}"),
            Some(package),
            true,
            one_time.then(|| "one-time".to_string()).as_deref(),
        );
        Response::Ok
    }

    fn freeze(&mut self) -> Response {
        let kept = std::mem::take(&mut self.one_time);
        self.log(
            "pkg-freeze",
            (!kept.is_empty()).then(|| kept.join(",")).as_deref(),
            true,
            None,
        );
        Response::Ok
    }

    fn layer_diff(&self, limit: usize) -> Response {
        let kind = if self.layer_kind == "ephemeral" {
            crate::layer::LayerKind::Ephemeral
        } else {
            crate::layer::LayerKind::Persistent
        };
        let all = crate::layer::Layer::new(self.layer.clone(), kind).diff(&self.base);
        let total = all.len();
        let entries: Vec<DiffEntry> = all
            .into_iter()
            .take(limit)
            .map(|c| DiffEntry {
                path: c.path.display().to_string(),
                kind: c.kind.as_str().to_string(),
                bytes: c.bytes,
            })
            .collect();
        Response::Diff {
            truncated: total.saturating_sub(entries.len()),
            entries,
        }
    }

    fn perm_entries(&self) -> Vec<PermEntry> {
        use collab_types::caps::{Effect, Expiry, Scope};
        self.policy
            .capabilities
            .iter()
            .map(|c| PermEntry {
                id: c.id.clone(),
                scope: match &c.scope {
                    Scope::FsPath(p) => p.clone(),
                    other => format!("{other:?}"),
                },
                allows: format!(
                    "{} {:?}",
                    match c.effect {
                        Effect::Grant => "grant",
                        Effect::Restrict => "restrict",
                    },
                    c.verb
                ),
                expiry: match c.expiry {
                    Expiry::Never => "never".into(),
                    Expiry::AtUnix(t) => format!("at {t}"),
                    Expiry::EndOfSession => "end of session".into(),
                    Expiry::AfterUse => "after use".into(),
                },
            })
            .collect()
    }

    fn status(&self) -> SessionStatus {
        let now = Some(crate::audit::now_unix());
        SessionStatus {
            session: self.session_id.clone(),
            profile: self.profile.as_str().to_string(),
            posture: self.profile.posture_summary().to_string(),
            layer: self.layer.display().to_string(),
            layer_kind: self.layer_kind.clone(),
            seccomp: if self.profile.confines_namespaces() {
                "enforcing: namespace and mount syscalls denied".into()
            } else {
                "relaxed: namespace and mount syscalls allowed (developer profile)".into()
            },
            landlock: self.landlock.clone(),
            trusted_dirs: self
                .policy
                .trusted_dirs(now)
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            readable_dirs: self
                .policy
                .readable_dirs(now)
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            one_time_packages: self.one_time.clone(),
            audit_records: self.audit.read_all().len(),
        }
    }

    fn log(&self, request: &str, subject: Option<&str>, allowed: bool, detail: Option<&str>) {
        let mut rec = AuditRecord::new(&self.session_id, self.profile.as_str(), request, allowed);
        if let Some(s) = subject {
            rec = rec.about(s);
        }
        if let Some(d) = detail {
            rec = rec.because(d);
        }
        self.audit.append(&rec);
    }
}

/// A running broker: a listener and the thread serving it.
pub struct Broker {
    state: Arc<Mutex<BrokerState>>,
    socket: PathBuf,
    _thread: std::thread::JoinHandle<()>,
}

impl Broker {
    /// Bind `socket` and start serving.
    pub fn start(socket: PathBuf, state: BrokerState) -> std::io::Result<Self> {
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        restrict_socket(&socket)?;

        let state = Arc::new(Mutex::new(state));
        let thread_state = Arc::clone(&state);
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => serve_connection(stream, &thread_state),
                    // The listener is closed when the session ends; that is the
                    // normal way this loop stops.
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            state,
            socket,
            _thread: thread,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn state(&self) -> &Arc<Mutex<BrokerState>> {
        &self.state
    }

    /// Did the guest ask for a restart?
    pub fn restart_requested(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.restart_requested)
    }

    /// Did the guest ask for its layer to be destroyed?
    pub fn burn_requested(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.burn_requested)
    }

    /// Packages installed with `--one-time` that the session must undo.
    pub fn one_time_packages(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.one_time.clone())
            .unwrap_or_default()
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn restrict_socket(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Owner-only: the socket is the route to the policy file. It lives in the
    // 0700 store already, but a mode on the socket itself costs nothing.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

fn serve_connection(stream: UnixStream, state: &Arc<Mutex<BrokerState>>) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(stream);
    let mut writer = write_half;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => match state.lock() {
                Ok(mut s) => s.handle(req),
                Err(_) => Response::Denied {
                    reason: "the broker is in a failed state".into(),
                },
            },
            Err(e) => Response::Denied {
                reason: format!("malformed request: {e}"),
            },
        };
        let Ok(mut bytes) = serde_json::to_vec(&response) else {
            break;
        };
        bytes.push(b'\n');
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

/// Ask the broker one question, from inside a guest.
pub fn ask(socket: &Path, req: &Request) -> std::io::Result<Response> {
    let stream = UnixStream::connect(socket)?;
    let mut writer = stream.try_clone()?;
    let mut bytes = serde_json::to_vec(req)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &Path) -> BrokerState {
        BrokerState {
            session_id: "s1".into(),
            profile: Profile::Ush,
            policy_path: dir.join("policy.json"),
            policy: Policy::default(),
            audit: AuditLog::at(dir.join("audit")),
            one_time: Vec::new(),
            restart_requested: false,
            burn_requested: false,
            base: dir.join("base"),
            layer: dir.join("layers/persistent"),
            layer_kind: "persistent".into(),
            landlock: "not attempted".into(),
        }
    }

    #[test]
    fn trusting_a_directory_saves_the_decision_and_asks_for_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("src");
        std::fs::create_dir_all(&target).unwrap();
        let mut s = state(dir.path());

        let resp = s.handle(Request::TrustDir { path: target });
        assert!(
            matches!(resp, Response::NeedsRestart { .. }),
            "a new bind needs a new mount namespace, so the answer has to say so: {resp:?}"
        );
        // Saved, so the next session picks it up.
        let saved = Policy::load(&s.policy_path);
        assert_eq!(saved.trusted_dirs(Some(1_000)).len(), 1);
    }

    #[test]
    fn trusting_a_base_subtree_is_refused_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        let resp = s.handle(Request::TrustDir {
            path: PathBuf::from("/usr"),
        });
        assert!(matches!(resp, Response::Denied { .. }), "{resp:?}");
        assert!(Policy::load(&s.policy_path).capabilities.is_empty());

        let log = s.audit.read_all();
        assert_eq!(log.len(), 1);
        assert!(
            !log[0].allowed,
            "a refusal must be recorded, not only a grant"
        );
        assert_eq!(log[0].request, "trust-dir");
    }

    /// A symlink inside a directory the guest controls must not be able to
    /// name a path the user never saw in the prompt.
    #[test]
    fn a_symlink_is_resolved_before_it_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-secrets");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("innocent");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut s = state(dir.path());
        s.handle(Request::TrustDir { path: link });

        let saved = Policy::load(&s.policy_path);
        let trusted = saved.trusted_dirs(Some(1_000));
        assert_eq!(
            trusted,
            vec![real.canonicalize().unwrap()],
            "the grant must name the resolved directory, not the symlink"
        );
    }

    #[test]
    fn trusting_a_path_that_is_not_there_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        let resp = s.handle(Request::TrustDir {
            path: dir.path().join("absent"),
        });
        assert!(matches!(resp, Response::Denied { .. }), "{resp:?}");
    }

    #[test]
    fn check_answers_from_the_saved_policy() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("src");
        std::fs::create_dir_all(&target).unwrap();
        let mut s = state(dir.path());
        s.handle(Request::TrustDir {
            path: target.clone(),
        });

        let inside = target.canonicalize().unwrap().join("main.rs");
        assert_eq!(
            s.handle(Request::Check {
                path: inside,
                write: true
            }),
            Response::Check { allowed: true }
        );
        assert_eq!(
            s.handle(Request::Check {
                path: dir.path().join("elsewhere"),
                write: false
            }),
            Response::Check { allowed: false }
        );
    }

    #[test]
    fn a_one_time_install_is_remembered_and_freeze_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        s.handle(Request::PkgRecorded {
            package: "sl".into(),
            action: "install".into(),
            one_time: true,
        });
        s.handle(Request::PkgRecorded {
            package: "htop".into(),
            action: "install".into(),
            one_time: false,
        });
        assert_eq!(s.one_time, vec!["sl".to_string()]);

        s.handle(Request::Freeze);
        assert!(
            s.one_time.is_empty(),
            "freeze promotes the session's installs, so nothing is left to undo"
        );
    }

    #[test]
    fn removing_a_package_clears_its_one_time_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        s.handle(Request::PkgRecorded {
            package: "sl".into(),
            action: "install".into(),
            one_time: true,
        });
        s.handle(Request::PkgRecorded {
            package: "sl".into(),
            action: "remove".into(),
            one_time: true,
        });
        assert!(s.one_time.is_empty());
    }

    #[test]
    fn restart_is_a_flag_the_supervisor_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        assert!(!s.restart_requested);
        assert_eq!(s.handle(Request::Restart), Response::Ok);
        assert!(s.restart_requested);
    }

    #[test]
    fn status_reports_the_real_posture() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        let Response::Status { status } = s.handle(Request::Status) else {
            panic!("expected a status");
        };
        assert_eq!(status.profile, "ush");
        assert!(status.seccomp.starts_with("enforcing"));

        s.profile = Profile::Dsh;
        let Response::Status { status } = s.handle(Request::Status) else {
            panic!("expected a status");
        };
        assert!(
            status.seccomp.starts_with("relaxed"),
            "dsh must not report itself as enforcing: {}",
            status.seccomp
        );
        assert!(status.posture.contains("NOT a sandbox"));
    }

    #[test]
    fn a_malformed_request_is_refused_rather_than_dropping_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("broker.sock");
        let broker = Broker::start(sock.clone(), state(dir.path())).unwrap();

        let stream = UnixStream::connect(&sock).unwrap();
        let mut w = stream.try_clone().unwrap();
        w.write_all(b"{not json}\n").unwrap();
        w.flush().unwrap();
        let mut r = BufReader::new(stream);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("denied"), "{line}");
        drop(broker);
    }

    #[test]
    fn a_request_round_trips_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("src");
        std::fs::create_dir_all(&target).unwrap();
        let sock = dir.path().join("broker.sock");
        let broker = Broker::start(sock.clone(), state(dir.path())).unwrap();

        let resp = ask(
            &sock,
            &Request::TrustDir {
                path: target.clone(),
            },
        )
        .unwrap();
        assert!(matches!(resp, Response::NeedsRestart { .. }), "{resp:?}");

        let resp = ask(&sock, &Request::ListPerms).unwrap();
        let Response::Perms { entries } = resp else {
            panic!("expected perms");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].scope,
            target.canonicalize().unwrap().display().to_string()
        );

        assert!(!broker.restart_requested());
        ask(&sock, &Request::Restart).unwrap();
        assert!(broker.restart_requested());
    }

    #[test]
    fn the_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("broker.sock");
        let broker = Broker::start(sock.clone(), state(dir.path())).unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "broker socket is reachable by other users: {mode:o}"
        );
        drop(broker);
        assert!(!sock.exists(), "the socket must not outlive the session");
    }
}
