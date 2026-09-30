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

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::caps::Verb;
use tilo_clock::unix_now;

use crate::audit::{AuditLog, AuditRecord};
use crate::policy::{Policy, is_trustable};
use crate::profile::Profile;

/// What a guest can ask for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// Update one installed app, or every installed app when absent.
    AppUpdate {
        package: Option<String>,
    },
    /// Stage the signed system image update.
    SystemUpdate,
    /// Update apps and stage the system image, matching `tilo update`.
    AllUpdates,
    /// Read-only app discovery operations.
    AppList {
        json: bool,
    },
    AppSearch {
        query: String,
    },
    AppRepoList,
    /// Read-only system status.
    SystemStatus {
        json: bool,
    },
    /// `perm trust-dir <dir>` — read and write, for good.
    TrustDir {
        path: PathBuf,
    },
    /// `perm list` — the decisions in force.
    ListPerms,
    /// `perm revoke <id>`.
    Revoke {
        id: String,
    },
    /// May this path be read / written? Answers `scan`.
    Check {
        path: PathBuf,
        write: bool,
    },
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
    LayerDiff {
        limit: usize,
    },
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
    Command {
        code: i32,
        stdout: String,
        stderr: String,
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
    /// Boxed: `SessionStatus` is several strings and vecs wide, dwarfing
    /// every other variant here — every `Response` would otherwise pay for
    /// its size even when it's an `Ok` or a `Check`.
    Status {
        status: Box<SessionStatus>,
    },
}

/// Socket-only framing for commands whose output may take a while. The public
/// [`ask`] API still returns one complete [`Response`]; clients which opt in to
/// [`ask_streaming`] additionally receive each output chunk as it arrives.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
enum WireFrame {
    Output { stderr: bool, data: Vec<u8> },
    Heartbeat,
    Complete { response: Response },
}

const MAX_ARG_BYTES: usize = 4096;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
struct HostCommand {
    operation: &'static str,
    program: &'static str,
    args: Vec<String>,
    binary: PathBuf,
    mutating: bool,
}

fn host_command_for(base: &Path, req: &Request) -> Option<HostCommand> {
    let (operation, program, args, mutating) = match req {
        Request::AppUpdate { package } => {
            let mut args = vec!["update".into()];
            args.extend(package.iter().cloned());
            ("app-update", "app", args, true)
        }
        Request::SystemUpdate => (
            "system-update",
            "tilo",
            vec!["system".into(), "update".into(), "--yes".into()],
            true,
        ),
        Request::AllUpdates => (
            "all-updates",
            "tilo",
            vec!["update".into(), "--yes".into()],
            true,
        ),
        Request::AppList { json } => {
            let mut args = vec!["list".into()];
            if *json {
                args.push("--json".into());
            }
            ("app-list", "app", args, false)
        }
        Request::AppSearch { query } => (
            "app-search",
            "app",
            vec!["search".into(), query.clone()],
            false,
        ),
        Request::AppRepoList => (
            "app-repo-list",
            "app",
            vec!["repo".into(), "list".into()],
            false,
        ),
        Request::SystemStatus { json } => {
            let mut args = vec!["system".into(), "status".into()];
            if *json {
                args.push("--json".into());
            }
            ("system-status", "tilo", args, false)
        }
        _ => return None,
    };
    Some(HostCommand {
        operation,
        program,
        args,
        binary: base.join("usr/bin").join(program),
        mutating,
    })
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
            Request::AppUpdate { package } => {
                let mut args = vec!["update".to_string()];
                if let Some(package) = package {
                    args.push(package);
                }
                self.host_command("app-update", "app", &args)
            }
            Request::SystemUpdate => self.host_command(
                "system-update",
                "tilo",
                &["system".into(), "update".into(), "--yes".into()],
            ),
            Request::AllUpdates => {
                self.host_command("all-updates", "tilo", &["update".into(), "--yes".into()])
            }
            Request::AppList { json } => {
                let args = if json {
                    vec!["list".into(), "--json".into()]
                } else {
                    vec!["list".into()]
                };
                self.host_command("app-list", "app", &args)
            }
            Request::AppSearch { query } => {
                self.host_command("app-search", "app", &["search".into(), query])
            }
            Request::AppRepoList => {
                self.host_command("app-repo-list", "app", &["repo".into(), "list".into()])
            }
            Request::SystemStatus { json } => {
                let mut args = vec!["system".into(), "status".into()];
                if json {
                    args.push("--json".into());
                }
                self.host_command("system-status", "tilo", &args)
            }
            Request::TrustDir { path } => self.trust_dir(&path),
            Request::ListPerms => Response::Perms {
                entries: self.perm_entries(),
            },
            Request::Revoke { id } => self.revoke(&id),
            Request::Check { path, write } => {
                let verb = if write { Verb::Write } else { Verb::Read };
                Response::Check {
                    allowed: self.policy.permits(&path, verb, Some(unix_now())),
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
                status: Box::new(self.status()),
            },
            Request::Restart => {
                self.restart_requested = true;
                self.log("restart", None, true, None);
                Response::Ok
            }
        }
    }

    fn host_command(&self, operation: &str, program: &str, args: &[String]) -> Response {
        if args.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES {
            let reason = "management command arguments are too large".to_string();
            self.log(operation, Some(program), false, Some(&reason));
            return Response::Denied { reason };
        }
        let binary = self.base.join("usr/bin").join(program);
        let output = match std::process::Command::new(&binary).args(args).output() {
            Ok(output) => output,
            Err(error) => {
                let reason = format!("could not run {}: {error}", binary.display());
                self.log(operation, Some(program), false, Some(&reason));
                return Response::Denied { reason };
            }
        };
        if output.stdout.len() > MAX_OUTPUT_BYTES || output.stderr.len() > MAX_OUTPUT_BYTES {
            let reason = "management command produced too much output".to_string();
            self.log(operation, Some(program), false, Some(&reason));
            return Response::Denied { reason };
        }
        let code = output.status.code().unwrap_or(1);
        self.log(
            operation,
            Some(program),
            true,
            Some(&format!("exit {code}")),
        );
        Response::Command {
            code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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
        let now = Some(unix_now());
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
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    connections: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    /// Anything connected. Only the guest can reach the socket, and its first
    /// process asks for status before it becomes the shell.
    connected: Arc<AtomicBool>,
}

impl Broker {
    /// Bind `socket` and start serving.
    ///
    /// # Errors
    ///
    /// Returns an error if `socket`'s parent directory can't be created,
    /// the socket can't be bound, or its permissions can't be restricted
    /// to owner-only.
    pub fn start(socket: PathBuf, state: BrokerState) -> std::io::Result<Self> {
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        restrict_socket(&socket)?;

        let state = Arc::new(Mutex::new(state));
        let thread_state = Arc::clone(&state);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let connections = Arc::new(Mutex::new(Vec::new()));
        let thread_connections = Arc::clone(&connections);
        let host_mutation_gate = Arc::new(Mutex::new(()));
        let connected = Arc::new(AtomicBool::new(false));
        let thread_connected = Arc::clone(&connected);
        let thread = std::thread::Builder::new()
            .name("tilo-ush-accept".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) if thread_stop.load(Ordering::Acquire) => {
                            drop(stream);
                            break;
                        }
                        Ok((stream, _)) => {
                            thread_connected.store(true, Ordering::Release);
                            let connection_state = Arc::clone(&thread_state);
                            let connection_stop = Arc::clone(&thread_stop);
                            let connection_mutation_gate = Arc::clone(&host_mutation_gate);
                            let handle = std::thread::spawn(move || {
                                serve_connection(
                                    stream,
                                    &connection_state,
                                    &connection_stop,
                                    &connection_mutation_gate,
                                );
                            });
                            if let Ok(mut handles) = thread_connections.lock() {
                                let mut index = 0;
                                while index < handles.len() {
                                    if handles
                                        .get(index)
                                        .is_some_and(std::thread::JoinHandle::is_finished)
                                    {
                                        let finished = handles.swap_remove(index);
                                        let _ = finished.join();
                                    } else {
                                        index = index.saturating_add(1);
                                    }
                                }
                                handles.push(handle);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })?;

        Ok(Self {
            state,
            socket,
            stop,
            thread: Some(thread),
            connections,
            connected,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn state(&self) -> &Arc<Mutex<BrokerState>> {
        &self.state
    }

    /// Did the guest's first process run? `false` after the guest exits means
    /// `bwrap` failed to build the sandbox, before anything of the guest ran.
    pub fn guest_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
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
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && UnixStream::connect(&self.socket).is_ok()
        {
            let _ = thread.join();
        }
        if let Ok(mut connections) = self.connections.lock() {
            for connection in connections.drain(..) {
                let _ = connection.join();
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn restrict_socket(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Owner-only: the socket is the route to the policy file. It lives in the
    // 0700 store already, but a mode on the socket itself costs nothing.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

fn serve_connection(
    stream: UnixStream,
    state: &Arc<Mutex<BrokerState>>,
    stop: &Arc<AtomicBool>,
    host_mutation_gate: &Arc<Mutex<()>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = write_half;
    loop {
        let mut line = String::new();
        let read = match reader.read_line(&mut line) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && !stop.load(Ordering::Acquire) =>
            {
                continue;
            }
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let command = state
                    .lock()
                    .ok()
                    .and_then(|s| host_command_for(&s.base, &req));
                if let Some(command) = command {
                    if serve_host_command(
                        &command,
                        state,
                        &mut writer,
                        &reader,
                        stop,
                        host_mutation_gate,
                    ) {
                        continue;
                    }
                    break;
                }
                match state.lock() {
                    Ok(mut s) => s.handle(req),
                    Err(_) => Response::Denied {
                        reason: "the broker is in a failed state".into(),
                    },
                }
            }
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

#[derive(Debug)]
enum PipeEvent {
    Data { stderr: bool, data: Vec<u8> },
    Eof,
}

fn pipe_reader<R: Read + Send + 'static>(
    mut pipe: R,
    stderr: bool,
    tx: std::sync::mpsc::Sender<PipeEvent>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let Some(data) = buffer.get(..n) else {
                        break;
                    };
                    if tx
                        .send(PipeEvent::Data {
                            stderr,
                            data: data.to_vec(),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(PipeEvent::Eof);
    });
}

fn peer_disconnected(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;

    let mut byte = 0_u8;
    // SAFETY: `byte` is valid for the one-byte non-consuming recv, and the fd
    // belongs to `stream` for the duration of this call.
    let result = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    result == 0
}

fn stop_child(child: &mut Child) {
    if let Some(group) = i32::try_from(child.id()).ok().and_then(i32::checked_neg) {
        // The command is its process-group leader, so this also terminates any
        // verifier/helper descendants which inherited its stdout or stderr.
        // SAFETY: a negative pid targets exactly that process group.
        let _ = unsafe { libc::kill(group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn start_host_command(
    command: &HostCommand,
    state: &Arc<Mutex<BrokerState>>,
) -> Result<Child, Response> {
    if command.args.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES {
        let reason = "management command arguments are too large".to_string();
        if let Ok(s) = state.lock() {
            s.log(
                command.operation,
                Some(command.program),
                false,
                Some(&reason),
            );
        }
        return Err(Response::Denied { reason });
    }
    let mut process = std::process::Command::new(&command.binary);
    process
        .args(&command.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    process.spawn().map_err(|error| {
        let reason = format!("could not run {}: {error}", command.binary.display());
        if let Ok(s) = state.lock() {
            s.log(
                command.operation,
                Some(command.program),
                false,
                Some(&reason),
            );
        }
        Response::Denied { reason }
    })
}

fn acquire_host_mutation<'a>(
    command: &HostCommand,
    gate: &'a Mutex<()>,
) -> Option<std::sync::MutexGuard<'a, ()>> {
    command.mutating.then(|| {
        gate.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    })
}

/// Run a host management command without retaining the state lock. Returns
/// true when the complete response reached the guest.
fn serve_host_command(
    command: &HostCommand,
    state: &Arc<Mutex<BrokerState>>,
    writer: &mut UnixStream,
    reader: &BufReader<UnixStream>,
    stop: &Arc<AtomicBool>,
    host_mutation_gate: &Arc<Mutex<()>>,
) -> bool {
    let _mutation_guard = acquire_host_mutation(command, host_mutation_gate);
    let mut child = match start_host_command(command, state) {
        Ok(child) => child,
        Err(response) => {
            return write_frame(writer, &WireFrame::Complete { response });
        }
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let (Some(child_stdout), Some(child_stderr)) = (child.stdout.take(), child.stderr.take())
    else {
        stop_child(&mut child);
        return false;
    };
    pipe_reader(child_stdout, false, tx.clone());
    pipe_reader(child_stderr, true, tx);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut open_pipes: usize = 2;
    let mut last_activity = Instant::now();
    while open_pipes > 0 {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(PipeEvent::Data {
                stderr: is_stderr,
                data,
            }) => {
                let collected = if is_stderr { &mut stderr } else { &mut stdout };
                if collected.len().saturating_add(data.len()) > MAX_OUTPUT_BYTES {
                    stop_child(&mut child);
                    let reason = "management command produced too much output".to_string();
                    if let Ok(s) = state.lock() {
                        s.log(
                            command.operation,
                            Some(command.program),
                            false,
                            Some(&reason),
                        );
                    }
                    return write_frame(
                        writer,
                        &WireFrame::Complete {
                            response: Response::Denied { reason },
                        },
                    );
                }
                collected.extend_from_slice(&data);
                if !write_frame(
                    writer,
                    &WireFrame::Output {
                        stderr: is_stderr,
                        data,
                    },
                ) {
                    stop_child(&mut child);
                    log_disconnected(state, command);
                    return false;
                }
                last_activity = Instant::now();
            }
            Ok(PipeEvent::Eof) => open_pipes = open_pipes.saturating_sub(1),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Acquire) || peer_disconnected(reader.get_ref()) {
                    stop_child(&mut child);
                    log_disconnected(state, command);
                    return false;
                }
                if last_activity.elapsed() >= Duration::from_secs(10) {
                    if !write_frame(writer, &WireFrame::Heartbeat) {
                        stop_child(&mut child);
                        log_disconnected(state, command);
                        return false;
                    }
                    last_activity = Instant::now();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let code = child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1);
    if let Ok(s) = state.lock() {
        s.log(
            command.operation,
            Some(command.program),
            true,
            Some(&format!("exit {code}")),
        );
    }
    write_frame(
        writer,
        &WireFrame::Complete {
            response: Response::Command {
                code,
                stdout: String::new(),
                stderr: String::new(),
            },
        },
    )
}

fn log_disconnected(state: &Arc<Mutex<BrokerState>>, command: &HostCommand) {
    if let Ok(s) = state.lock() {
        s.log(
            command.operation,
            Some(command.program),
            false,
            Some("guest disconnected; command cancelled"),
        );
    }
}

fn write_frame(writer: &mut UnixStream, frame: &WireFrame) -> bool {
    let Ok(mut bytes) = serde_json::to_vec(frame) else {
        return false;
    };
    bytes.push(b'\n');
    writer.write_all(&bytes).is_ok() && writer.flush().is_ok()
}

/// Ask the broker one question, from inside a guest.
///
/// # Errors
///
/// Returns an error if the socket can't be reached, the request can't be
/// serialized, the write/read fails, or the reply isn't valid JSON.
pub fn ask(socket: &Path, req: &Request) -> std::io::Result<Response> {
    ask_streaming(socket, req, &mut std::io::sink(), &mut std::io::sink())
}

/// Ask the broker while copying host-command output to the supplied streams as
/// soon as it is produced. The returned response remains fully populated so
/// existing callers can continue to inspect or replay the captured output.
///
/// # Errors
///
/// Returns an error when the broker cannot be reached, the socket protocol is
/// invalid, or streamed output cannot be written.
pub fn ask_streaming(
    socket: &Path,
    req: &Request,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> std::io::Result<Response> {
    ask_streaming_with_progress(socket, req, stdout, stderr, false)
}

/// Streaming variant which also renders a sparse liveness message on stderr
/// while a host command is silent. Callers should enable this only for a TTY.
///
/// # Errors
///
/// Returns an error when the broker cannot be reached, the socket protocol is
/// invalid, or streamed output cannot be written.
pub fn ask_streaming_with_progress(
    socket: &Path,
    req: &Request,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    show_progress: bool,
) -> std::io::Result<Response> {
    let stream = UnixStream::connect(socket)?;
    let mut writer = stream.try_clone()?;
    let mut bytes = serde_json::to_vec(req)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut captured_stdout = Vec::new();
    let mut captured_stderr = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "broker closed the connection before replying",
            ));
        }
        // Non-command requests retain the original one-line wire format.
        if let Ok(response) = serde_json::from_str::<Response>(&line) {
            return Ok(response);
        }
        match serde_json::from_str::<WireFrame>(&line)? {
            WireFrame::Output { stderr: true, data } => {
                stderr.write_all(&data)?;
                stderr.flush()?;
                captured_stderr.extend_from_slice(&data);
            }
            WireFrame::Output {
                stderr: false,
                data,
            } => {
                stdout.write_all(&data)?;
                stdout.flush()?;
                captured_stdout.extend_from_slice(&data);
            }
            WireFrame::Heartbeat => {
                if show_progress {
                    stderr.write_all(b"Still working...\n")?;
                    stderr.flush()?;
                }
            }
            WireFrame::Complete { mut response } => {
                if let Response::Command { stdout, stderr, .. } = &mut response {
                    *stdout = String::from_utf8_lossy(&captured_stdout).into_owned();
                    *stderr = String::from_utf8_lossy(&captured_stderr).into_owned();
                }
                return Ok(response);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: Duration = Duration::from_secs(30);

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
    fn host_management_commands_run_on_the_base_and_nothing_else() {
        // phase: management_commands_run_on_the_host_base_and_preserve_status
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("base/usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let tilo = bin.join("tilo");
            std::fs::write(&tilo, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 7\n").unwrap();
            std::fs::set_permissions(&tilo, std::fs::Permissions::from_mode(0o755)).unwrap();
            let app = bin.join("app");
            std::fs::write(&app, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 6\n").unwrap();
            std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();

            let response = state(dir.path()).handle(Request::AppUpdate {
                package: Some("browser".into()),
            });
            assert_eq!(
                response,
                Response::Command {
                    code: 6,
                    stdout: "update\nbrowser\n".into(),
                    stderr: String::new(),
                }
            );

            let response = state(dir.path()).handle(Request::AllUpdates);
            assert_eq!(
                response,
                Response::Command {
                    code: 7,
                    stdout: "update\n--yes\n".into(),
                    stderr: String::new(),
                }
            );
        }
        // phase: signed_host_update_round_trips_over_the_guest_broker_socket
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("base/usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let tilo = bin.join("tilo");
            std::fs::write(&tilo, "#!/bin/sh\nprintf 'host:%s\\n' \"$*\"\n").unwrap();
            std::fs::set_permissions(&tilo, std::fs::Permissions::from_mode(0o755)).unwrap();
            let app = bin.join("app");
            std::fs::write(&app, "#!/bin/sh\nprintf 'host:%s\\n' \"$*\"\n").unwrap();
            std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
            let socket = dir.path().join("broker.sock");
            let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

            assert_eq!(
                ask(
                    &socket,
                    &Request::AppUpdate {
                        package: Some("browser".into()),
                    }
                )
                .unwrap(),
                Response::Command {
                    code: 0,
                    stdout: "host:update browser\n".into(),
                    stderr: String::new(),
                }
            );
            assert_eq!(
                ask(&socket, &Request::SystemUpdate).unwrap(),
                Response::Command {
                    code: 0,
                    stdout: "host:system update --yes\n".into(),
                    stderr: String::new(),
                }
            );
            drop(broker);
        }
        // phase: management_protocol_has_no_arbitrary_program_variant
        {
            assert!(
                serde_json::from_str::<Request>(
                    r#"{"op":"host-command","program":"sh","args":["-c","touch /tmp/nope"]}"#
                )
                .is_err()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn host_commands_stream_and_serialise_without_blocking_status() {
        // phase: host_output_streams_while_other_broker_requests_remain_responsive
        {
            use std::os::unix::fs::PermissionsExt;

            struct ChannelWriter(std::sync::mpsc::Sender<Vec<u8>>);
            impl Write for ChannelWriter {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.0.send(bytes.to_vec()).unwrap();
                    Ok(bytes.len())
                }

                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }

            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("base/usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let app = bin.join("app");
            std::fs::write(
                &app,
                "#!/bin/sh\nprintf 'started\\n'\nsleep 1\nprintf 'finished\\n'\n",
            )
            .unwrap();
            std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
            let socket = dir.path().join("broker.sock");
            let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

            let command_socket = socket.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let command = std::thread::spawn(move || {
                let mut stdout = ChannelWriter(tx);
                ask_streaming(
                    &command_socket,
                    &Request::AppUpdate { package: None },
                    &mut stdout,
                    &mut std::io::sink(),
                )
                .unwrap()
            });

            assert_eq!(rx.recv_timeout(HANG_BOUND).unwrap(), b"started\n");
            let before = Instant::now();
            assert!(matches!(
                ask(&socket, &Request::Status),
                Ok(Response::Status { .. })
            ));
            assert!(
                before.elapsed() < Duration::from_millis(500),
                "a management command must not retain the BrokerState lock"
            );
            assert_eq!(
                command.join().unwrap(),
                Response::Command {
                    code: 0,
                    stdout: "started\nfinished\n".into(),
                    stderr: String::new(),
                }
            );
            drop(broker);
        }
        // phase: host_updates_serialize_without_blocking_status
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("base/usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let events = dir.path().join("events");
            let app = bin.join("app");
            std::fs::write(
                &app,
                format!(
                    "#!/bin/sh\nprintf '%s-start\\n' \"$2\" >> '{}'\n\
                 if [ \"$2\" = first ]; then sleep 1; fi\n\
                 printf '%s-end\\n' \"$2\" >> '{}'\n",
                    events.display(),
                    events.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
            let socket = dir.path().join("broker.sock");
            let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

            let first_socket = socket.clone();
            let first = std::thread::spawn(move || {
                ask(
                    &first_socket,
                    &Request::AppUpdate {
                        package: Some("first".into()),
                    },
                )
                .unwrap()
            });
            let deadline = Instant::now() + HANG_BOUND;
            while Instant::now() < deadline {
                if std::fs::read_to_string(&events).is_ok_and(|text| text.contains("first-start")) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            let second_socket = socket.clone();
            let second = std::thread::spawn(move || {
                ask(
                    &second_socket,
                    &Request::AppUpdate {
                        package: Some("second".into()),
                    },
                )
                .unwrap()
            });
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(
                std::fs::read_to_string(&events).unwrap(),
                "first-start\n",
                "the second update started before the first update exited"
            );

            let before = Instant::now();
            assert!(matches!(
                ask(&socket, &Request::Status),
                Ok(Response::Status { .. })
            ));
            assert!(before.elapsed() < Duration::from_millis(500));
            assert!(matches!(
                first.join().unwrap(),
                Response::Command { code: 0, .. }
            ));
            assert!(matches!(
                second.join().unwrap(),
                Response::Command { code: 0, .. }
            ));
            assert_eq!(
                std::fs::read_to_string(&events).unwrap(),
                "first-start\nfirst-end\nsecond-start\nsecond-end\n"
            );
            drop(broker);
        }
    }

    /// Whether `pid` no longer runs. The killed helper is reparented away from
    /// the broker, so under an init that does not reap orphans it lingers as a
    /// zombie: terminated, yet still answering signal 0.
    fn has_terminated(pid: i32) -> bool {
        // SAFETY: signal 0 only probes whether a test-recorded pid exists.
        if (unsafe { libc::kill(pid, 0) }) == -1 {
            return true;
        }
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with(['Z', 'X']))
        })
    }

    #[test]
    fn disconnecting_the_guest_cancels_the_host_command() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("base/usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let pid_file = dir.path().join("child.pid");
        let app = bin.join("app");
        std::fs::write(
            &app,
            format!(
                "#!/bin/sh\nsleep 30 &\nhelper=$!\nprintf '%s %s' $$ \"$helper\" > '{}'\nwait\n",
                pid_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = dir.path().join("broker.sock");
        let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

        let mut stream = UnixStream::connect(&socket).unwrap();
        serde_json::to_writer(&mut stream, &Request::AppUpdate { package: None }).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        let deadline = Instant::now() + HANG_BOUND;
        while Instant::now() < deadline {
            if pid_file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let pids: Vec<i32> = std::fs::read_to_string(&pid_file)
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        drop(stream);

        let mut stopped = Vec::new();
        let stop_deadline = Instant::now() + HANG_BOUND;
        while Instant::now() < stop_deadline {
            stopped = pids
                .iter()
                .copied()
                .filter(|pid| has_terminated(*pid))
                .collect();
            if stopped.len() == pids.len() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            stopped, pids,
            "the command process group survived its guest connection"
        );
        let audit = broker.state().lock().unwrap().audit.read_all();
        assert!(audit.iter().any(|record| {
            !record.allowed
                && record
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("disconnected"))
        }));
        drop(broker);
    }

    #[test]
    fn trust_requests_resolve_validate_and_persist_grants() {
        // phase: trusting_a_directory_saves_the_decision_and_asks_for_a_restart
        {
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
        // phase: trusting_a_base_subtree_is_refused_and_recorded
        {
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
        // phase: a_symlink_is_resolved_before_it_is_trusted
        // A symlink inside a directory the guest controls must not be able to
        // name a path the user never saw in the prompt.
        {
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
        // phase: trusting_a_path_that_is_not_there_is_refused
        {
            let dir = tempfile::tempdir().unwrap();
            let mut s = state(dir.path());
            let resp = s.handle(Request::TrustDir {
                path: dir.path().join("absent"),
            });
            assert!(matches!(resp, Response::Denied { .. }), "{resp:?}");
        }
        // phase: check_answers_from_the_saved_policy
        {
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
    }

    #[test]
    fn session_state_tracks_one_time_installs_restart_and_posture() {
        // phase: a_one_time_install_is_remembered_and_freeze_forgets_it
        {
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
        // phase: removing_a_package_clears_its_one_time_record
        {
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
        // phase: restart_is_a_flag_the_supervisor_reads
        {
            let dir = tempfile::tempdir().unwrap();
            let mut s = state(dir.path());
            assert!(!s.restart_requested);
            assert_eq!(s.handle(Request::Restart), Response::Ok);
            assert!(s.restart_requested);
        }
        // phase: status_reports_the_real_posture
        {
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
    }

    #[test]
    fn the_broker_socket_is_owner_only_and_answers_every_request() {
        // phase: a_malformed_request_is_refused_rather_than_dropping_the_connection
        {
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
        // phase: a_request_round_trips_over_the_socket
        {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("src");
            std::fs::create_dir_all(&target).unwrap();
            let sock = dir.path().join("broker.sock");
            let broker = Broker::start(sock.clone(), state(dir.path())).unwrap();
            assert!(!broker.guest_connected());

            let resp = ask(
                &sock,
                &Request::TrustDir {
                    path: target.clone(),
                },
            )
            .unwrap();
            assert!(matches!(resp, Response::NeedsRestart { .. }), "{resp:?}");
            assert!(broker.guest_connected());

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
        // phase: the_socket_is_owner_only
        {
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

    fn thread_ids() -> std::collections::BTreeSet<String> {
        std::fs::read_dir("/proc/self/task")
            .expect("/proc/self/task")
            .map(|entry| entry.expect("task").file_name().into_string().expect("tid"))
            .collect()
    }

    fn context_switches(tid: &str) -> u64 {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
            .expect("thread status")
            .lines()
            .filter(|line| line.contains("ctxt_switches"))
            .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
            .sum()
    }

    fn spawned_thread<T>(comm: &str, start: impl FnOnce() -> T) -> (T, String) {
        let before = thread_ids();
        let started = start();
        let waiting = Instant::now();
        loop {
            let named = thread_ids()
                .difference(&before)
                .find(|tid| {
                    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                        .is_ok_and(|name| name.trim() == comm)
                })
                .cloned();
            if let Some(tid) = named {
                return (started, tid);
            }
            assert!(
                waiting.elapsed() < Duration::from_secs(5),
                "no new thread named {comm}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_stays_asleep(tid: &str, what: &str) {
        std::thread::sleep(Duration::from_millis(50));
        let settled = context_switches(tid);
        std::thread::sleep(Duration::from_millis(400));
        let woke = context_switches(tid).saturating_sub(settled);
        assert_eq!(woke, 0, "idle {what} woke {woke} times in 400 ms");
    }

    #[test]
    fn idle_accept_thread_sleeps_without_clients() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("broker.sock");
        let (broker, tid) = spawned_thread("tilo-ush-accept", || {
            Broker::start(sock.clone(), state(dir.path())).unwrap()
        });
        assert_stays_asleep(&tid, "ush broker accept");
        drop(broker);
    }
}
