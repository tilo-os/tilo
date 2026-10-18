//! The broker.
//!
//! Starts with the session, runs on the host, outlives nothing. Every
//! request from inside the guest that reaches past it arrives here as a line
//! of JSON on a Unix socket, is decided, and is written to [`crate::audit`]
//! whichever way it went.
//!
//! # Why a broker at all
//!
//! The guest cannot reach the environment record or the store: neither is
//! bound into the namespace at any path. The broker is the only route to the
//! record that decides what the *next* session is — its tools and its
//! grants — and the only writer of the record of what was asked for.
//!
//! A new tool or a new bind needs a new mount namespace, so `pkg install`
//! and `perm trust-dir` answer with [`Response::NeedsRestart`] and the user
//! runs `restart` — pretending otherwise would mean a change that appears
//! made and has not. A host grant is different: the session's DNS stub and
//! filter take it at once, so `perm allow-host` (and revoking one) answers
//! [`Response::Applied`].

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bounded_command::WakeFd;
use collab_types::caps::Access;
use tilo_clock::unix_now;
use tilo_environment::{Environment, ToolBuilder, host_arch};

use crate::audit::{AuditLog, AuditRecord};
use crate::policy::Policy;
use crate::records::Records;

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
    /// `perm allow-host <host>` — let the guest reach a host, for good.
    AllowHost {
        host: String,
    },
    /// `perm list` — the grants in force.
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
    /// `pkg install` — build a tool image for the packages and add it.
    AddTool {
        packages: Vec<String>,
    },
    /// `pkg remove` — take the tools providing the packages out.
    RemoveTool {
        packages: Vec<String>,
    },
    /// `pkg list` — the runtime and the tools.
    ListTools,
    /// Which image a guest path comes from. Answers `scan`.
    Provenance {
        path: PathBuf,
    },
    /// `guard status`.
    Status,
    /// `restart` — rebuild the guest in place.
    Restart,
    /// A legacy app's command shim: run `app` in the user's legacy VM, in
    /// `cwd` when the VM can see it. Its windows open in the current strip.
    LegacyLaunch {
        app: String,
        args: Vec<String>,
        cwd: PathBuf,
    },
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
    /// Allowed, and already in force in this session.
    Applied {
        detail: String,
    },
    Denied {
        reason: String,
    },
    /// A host command's exit status. Its output reached the guest as it was
    /// produced, in output frames ahead of this answer.
    Command {
        code: i32,
    },
    Perms {
        entries: Vec<PermEntry>,
    },
    Check {
        allowed: bool,
    },
    Tools {
        runtime: String,
        tools: Vec<ToolEntry>,
    },
    Provenance {
        image: Option<String>,
    },
    /// Boxed: `SessionStatus` is several strings and vecs wide, dwarfing
    /// every other variant here — every `Response` would otherwise pay for
    /// its size even when it's an `Ok` or a `Check`.
    Status {
        status: Box<SessionStatus>,
    },
}

/// Socket-only framing for requests whose output may take a while (host
/// management commands, tool builds): each output chunk as it arrives, then
/// the [`Response`]. Output is never collected into the response, so a slow
/// `app update` shows its progress line by line rather than all at the end.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
enum WireFrame {
    Output { stderr: bool, data: Vec<u8> },
    Heartbeat,
    Complete { response: Response },
}

const MAX_ARG_BYTES: usize = 4096;
/// The most output one host command forwards to the guest, stdout and stderr
/// together. Past it the command keeps running to completion (stopping an
/// update halfway over its chatter would be worse) and only its exit status
/// follows.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// How long an update may run before the broker stops it. Generous: a system
/// image or every app over a slow link legitimately takes many minutes; the
/// limit only keeps a wedged host command from holding the update lock for
/// ever.
const UPDATE_TIME_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How long a read-only query (`app list`, `system status`, ...) may run.
const QUERY_TIME_LIMIT: Duration = Duration::from_secs(5 * 60);
/// A silent host command gets a heartbeat this often, so the guest can tell
/// the person it is still working.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// The answer to a host command that reached [`BrokerState::handle`]
/// directly: host commands stream their output, so they run only over the
/// socket.
const HOST_COMMAND_NEEDS_SOCKET: &str = "host commands run only over the broker socket";

#[derive(Debug)]
struct HostCommand {
    operation: &'static str,
    program: &'static str,
    args: Vec<String>,
    binary: PathBuf,
    mutating: bool,
    /// `None` for a legacy app launch, which lasts as long as the person
    /// keeps the app open.
    time_limit: Option<Duration>,
}

fn host_command_for(host_root: &Path, policy: &Policy, req: &Request) -> Option<HostCommand> {
    let (operation, program, args, mutating) = match req {
        // On a relay worker the launch is a request to the relay, answered
        // at once by `BrokerState::handle`, not a host command.
        Request::LegacyLaunch { .. } if crate::legacy_relay::socket_from_env().is_some() => {
            return None;
        }
        Request::LegacyLaunch { app, args, cwd } => (
            "legacy-launch",
            LEGACY_PROGRAM,
            legacy_launch_args(policy, app, args, cwd)?,
            false,
        ),
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
    let time_limit = match operation {
        "legacy-launch" => None,
        _ if mutating => Some(UPDATE_TIME_LIMIT),
        _ => Some(QUERY_TIME_LIMIT),
    };
    Some(HostCommand {
        operation,
        program,
        args,
        binary: host_root.join("usr/bin").join(program),
        mutating,
        time_limit,
    })
}

/// The host side of every legacy app shim.
const LEGACY_PROGRAM: &str = "tilo-legacy";

/// `tilo-legacy launch <app> --cwd <cwd> [--share-cwd] -- <args…>`.
///
/// The directory a command is typed in is shared with the VM for that launch
/// only when the user trusted it to this runtime for writing: a guest cannot
/// widen what the VM sees beyond what the person already granted here.
/// `None` for an app name that is not a plain identifier or a relative cwd.
fn legacy_launch_args(
    policy: &Policy,
    app: &str,
    args: &[String],
    cwd: &Path,
) -> Option<Vec<String>> {
    let plain = !app.is_empty()
        && !app.starts_with(['-', '.'])
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if !plain || !cwd.is_absolute() {
        return None;
    }
    let mut out = vec![
        "launch".to_string(),
        app.to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
    ];
    if policy.permits(cwd, Access::Write, Some(unix_now())) {
        out.push("--share-cwd".to_string());
    }
    out.push("--".to_string());
    out.extend(args.iter().cloned());
    Some(out)
}

/// One line of `pkg list`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolEntry {
    pub id: String,
    pub packages: Vec<String>,
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
    pub environment: String,
    pub record: String,
    pub runtime: String,
    pub tools: Vec<String>,
    pub seccomp: String,
    pub landlock: String,
    pub trusted_dirs: Vec<String>,
    pub readable_dirs: Vec<String>,
    pub audit_records: usize,
    pub network: NetworkStatus,
}

/// What the session's egress enforces, for `guard status` — which tests it
/// from inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum NetworkStatus {
    /// A `*` grant: every host, through the guest's own namespace.
    Unrestricted,
    /// Only the addresses the DNS stub answered for these hosts.
    Filtered { hosts: Vec<String> },
    /// No route out of the namespace at all.
    Offline,
}

impl NetworkStatus {
    fn of(egress: Option<&tilo_egress::Egress>) -> Self {
        match egress.map(tilo_egress::Egress::enforcement) {
            Some(tilo_egress::Enforcement::Unrestricted) => Self::Unrestricted,
            Some(tilo_egress::Enforcement::Filtered(hosts)) => Self::Filtered { hosts },
            Some(tilo_egress::Enforcement::Offline) | None => Self::Offline,
        }
    }
}

/// The environment a session was built from, and the way back to its record.
#[derive(Debug, Clone)]
pub struct EnvironmentHandle {
    pub records: Records,
    pub key: String,
    /// The record as the session was built from it.
    pub built_from: Environment,
}

impl EnvironmentHandle {
    /// The record as it stands now — another device or Terminal may have
    /// changed it since the session started. Falls back to what the session
    /// was built from when the record cannot be read.
    pub fn current(&self) -> Environment {
        match self.records.load(&self.key) {
            Ok(Some(env)) => env,
            Ok(None) => self.built_from.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "environment record unreadable; using the session's copy");
                self.built_from.clone()
            }
        }
    }

    fn policy(&self) -> Policy {
        Policy::new(self.current().grants)
    }
}

/// The state one broker owns for the life of a session.
pub struct BrokerState {
    pub session_id: String,
    pub environment: EnvironmentHandle,
    pub audit: AuditLog,
    /// Set by `restart`; read by the supervisor after the guest exits.
    pub restart_requested: bool,
    /// The machine's root, where the management commands live.
    pub host_root: PathBuf,
    /// The images the guest was built from, `(label, root)`, topmost first —
    /// what `scan` reports provenance against.
    pub images: Vec<(String, PathBuf)>,
    /// How `pkg install` builds a tool image.
    pub builder: Arc<dyn ToolBuilder>,
    /// For `guard status`, filled in by the supervisor at start.
    pub landlock: String,
    /// The session's egress, which takes host grants at once. `None` where
    /// no guest network was built (tests of the policy surface).
    pub egress: Option<Arc<tilo_egress::Egress>>,
}

impl std::fmt::Debug for BrokerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerState")
            .field("session_id", &self.session_id)
            .field("environment", &self.environment)
            .field("restart_requested", &self.restart_requested)
            .finish_non_exhaustive()
    }
}

impl BrokerState {
    /// Decide one request and record it. The whole policy surface is here, so
    /// it can be tested without a socket.
    pub fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::AppUpdate { .. }
            | Request::SystemUpdate
            | Request::AllUpdates
            | Request::AppList { .. }
            | Request::AppSearch { .. }
            | Request::AppRepoList
            | Request::SystemStatus { .. } => Response::Denied {
                reason: HOST_COMMAND_NEEDS_SOCKET.into(),
            },
            Request::TrustDir { path } => self.trust_dir(&path),
            Request::AllowHost { host } => self.allow_host(&host),
            Request::ListPerms => Response::Perms {
                entries: self.perm_entries(),
            },
            Request::Revoke { id } => self.revoke(&id),
            Request::Check { path, write } => {
                let verb = if write { Access::Write } else { Access::Read };
                Response::Check {
                    allowed: self
                        .environment
                        .policy()
                        .permits(&path, verb, Some(unix_now())),
                }
            }
            Request::AddTool { packages } => {
                let plan = match self.plan_add_tool(&packages) {
                    Ok(plan) => plan,
                    Err(response) => return response,
                };
                let builder = Arc::clone(&self.builder);
                let built = run_build(&*builder, &plan, &mut std::io::sink());
                self.finish_add_tool(&plan, built)
            }
            Request::RemoveTool { packages } => self.remove_tools(&packages),
            Request::ListTools => {
                let env = self.environment.current();
                Response::Tools {
                    runtime: runtime_label(&env),
                    tools: env
                        .tools
                        .iter()
                        .map(|t| ToolEntry {
                            id: t.images.get(host_arch()).cloned().unwrap_or_default(),
                            packages: t.packages.clone(),
                        })
                        .collect(),
                }
            }
            Request::Provenance { path } => Response::Provenance {
                image: crate::guard::provenance(&path, &self.images),
            },
            Request::Status => Response::Status {
                status: Box::new(self.status()),
            },
            Request::Restart => {
                self.restart_requested = true;
                self.log("restart", None, true, None);
                Response::Ok
            }
            Request::LegacyLaunch { app, args, cwd } => self.legacy_launch(&app, args, &cwd),
        }
    }

    /// A legacy app's shim: on a device, the user's own legacy VM; on a
    /// relay worker, a launch the relay routes to a worker with KVM.
    fn legacy_launch(&self, app: &str, args: Vec<String>, cwd: &Path) -> Response {
        if let Some(socket) = crate::legacy_relay::socket_from_env() {
            return self.relay_legacy_launch(&socket, app, args, cwd);
        }
        if legacy_launch_args(&self.environment.policy(), app, &args, cwd).is_some() {
            Response::Denied {
                reason: HOST_COMMAND_NEEDS_SOCKET.into(),
            }
        } else {
            let reason = format!("{app:?} is not a legacy app name");
            self.log("legacy-launch", Some(app), false, Some(&reason));
            Response::Denied { reason }
        }
    }

    /// A legacy launch from an environment on a relay worker: the relay
    /// runs the app on a worker with KVM and its windows open on the
    /// member's devices.
    fn relay_legacy_launch(
        &self,
        socket: &Path,
        app: &str,
        args: Vec<String>,
        cwd: &Path,
    ) -> Response {
        use crate::legacy_relay::{LaunchReply, LaunchRequest, request};
        let asked = LaunchRequest {
            app: app.to_string(),
            args,
            cwd: cwd.display().to_string(),
        };
        match request(socket, &asked) {
            Ok(LaunchReply::Launched { launch }) => {
                self.log(
                    "legacy-launch",
                    Some(app),
                    true,
                    Some(&format!("relay {launch}")),
                );
                Response::Applied {
                    detail: format!("{app} runs on the relay; its windows open on your devices."),
                }
            }
            Ok(LaunchReply::Refused { detail }) => {
                self.log("legacy-launch", Some(app), false, Some(&detail));
                Response::Denied { reason: detail }
            }
            Err(error) => {
                let reason = format!("the relay worker did not take the launch: {error}");
                self.log("legacy-launch", Some(app), false, Some(&reason));
                Response::Denied { reason }
            }
        }
    }

    /// Apply one action to the session's environment record, logging it.
    fn apply(
        &self,
        operation: &str,
        subject: &str,
        invoke: tilo_resource::Invoke<Environment>,
    ) -> Result<Environment, String> {
        let key = self.environment.key.clone();
        match self.environment.records.apply(&key, invoke) {
            Ok(env) => {
                self.log(operation, Some(subject), true, None);
                Ok(env)
            }
            Err(e) => {
                let reason = e.to_string();
                self.log(operation, Some(subject), false, Some(&reason));
                Err(reason)
            }
        }
    }

    fn trust_dir(&self, path: &Path) -> Response {
        // Resolve first: a symlink under a guest-controlled directory could name a
        // path the user never saw. `canonicalize` failing on a missing path is
        // also correct — trusting nothing there.
        let resolved = match path.canonicalize() {
            Ok(p) if p.is_dir() => p,
            Ok(p) => {
                let reason = format!("{} is not a directory", p.display());
                self.log(
                    "trust-dir",
                    Some(&p.display().to_string()),
                    false,
                    Some(&reason),
                );
                return Response::Denied { reason };
            }
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
        let shown = resolved.display().to_string();
        match self.apply(
            "trust-dir",
            &shown,
            Environment::GRANT_DIR.with((shown.clone(), true)),
        ) {
            Ok(_) => Response::NeedsRestart {
                detail: format!(
                    "{shown} is granted (grant {}). Run `restart` to bring it into this session.",
                    tilo_environment::grant_id(&resolved)
                ),
            },
            Err(reason) => Response::Denied { reason },
        }
    }

    fn allow_host(&self, host: &str) -> Response {
        let normalized = match tilo_environment::normalize_host(host) {
            Ok(h) => h,
            Err(reason) => {
                self.log("allow-host", Some(host), false, Some(&reason));
                return Response::Denied { reason };
            }
        };
        if let Err(reason) = self.apply(
            "allow-host",
            &normalized,
            Environment::ALLOW_HOST.with(normalized.clone()),
        ) {
            return Response::Denied { reason };
        }
        let id = tilo_environment::host_grant_id(&normalized);
        let shown = if normalized == "*" {
            "every host".to_owned()
        } else if let Some(domain) = normalized.strip_prefix('.') {
            format!("{domain} and every host under it")
        } else {
            normalized
        };
        match self.sync_network() {
            Ok(()) => Response::Applied {
                detail: format!("{shown} is allowed (grant {id}), in this session now."),
            },
            Err(reason) => Response::NeedsRestart {
                detail: format!(
                    "{shown} is allowed (grant {id}). {reason}: run `restart` to bring it \
                     into this session."
                ),
            },
        }
    }

    /// Give the session's egress the record's host grants as they stand.
    fn sync_network(&self) -> Result<(), String> {
        let Some(egress) = &self.egress else {
            return Err("this session has no network of its own".into());
        };
        let policy = self.environment.policy().egress(Some(unix_now()));
        egress.update_policy(policy)
    }

    fn revoke(&self, id: &str) -> Response {
        let was_host = self
            .environment
            .current()
            .grants
            .iter()
            .any(|c| c.id == id && matches!(c.scope, collab_types::caps::Scope::NetHost(_)));
        if let Err(reason) = self.apply("revoke", id, Environment::REVOKE_GRANT.with(id.to_owned()))
        {
            return Response::Denied { reason };
        }
        if was_host && self.sync_network().is_ok() {
            return Response::Applied {
                detail: format!("{id} revoked, in this session now."),
            };
        }
        Response::NeedsRestart {
            detail: format!("{id} revoked. Run `restart` to drop it from this session."),
        }
    }

    /// What `pkg install` will build, or the answer when there is nothing to.
    fn plan_add_tool(&self, packages: &[String]) -> Result<ToolPlan, Response> {
        let packages: Vec<String> = packages
            .iter()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        if packages.is_empty() {
            return Err(Response::Denied {
                reason: "pkg install needs a package name".into(),
            });
        }
        if packages.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES {
            return Err(Response::Denied {
                reason: "too many package names".into(),
            });
        }
        let env = self.environment.current();
        let (present, wanted): (Vec<String>, Vec<String>) = packages.into_iter().partition(|p| {
            env.tool_providing(p)
                .is_some_and(|tool| tool.images.contains_key(host_arch()))
        });
        if wanted.is_empty() {
            return Err(Response::NeedsRestart {
                detail: format!(
                    "{} {} already in this environment; nothing to build.",
                    present.join(", "),
                    if present.len() == 1 { "is" } else { "are" }
                ),
            });
        }
        let runtime = env
            .runtime_image(host_arch())
            .map_err(|reason| Response::Denied { reason })?;
        Ok(ToolPlan { runtime, wanted })
    }

    fn finish_add_tool(
        &self,
        plan: &ToolPlan,
        built: Result<tilo_environment::PackageId, String>,
    ) -> Response {
        let subject = plan.wanted.join(",");
        let id = match built {
            Ok(id) => id,
            Err(reason) => {
                self.log("pkg-install", Some(&subject), false, Some(&reason));
                return Response::Denied { reason };
            }
        };
        match self.apply(
            "pkg-install",
            &subject,
            Environment::ADD_TOOL.with((
                plan.wanted.clone(),
                host_arch().to_owned(),
                id.to_string(),
            )),
        ) {
            Ok(_) => Response::NeedsRestart {
                detail: format!(
                    "{} added to this environment ({id}). Run `restart` to use it here; \
                     new sessions have it already.",
                    plan.wanted.join(", ")
                ),
            },
            Err(reason) => Response::Denied { reason },
        }
    }

    fn remove_tools(&self, packages: &[String]) -> Response {
        let env = self.environment.current();
        let mut tools: Vec<&tilo_environment::ToolRef> = Vec::new();
        for package in packages {
            let Some(tool) = env.tool_providing(package) else {
                let reason = format!(
                    "{package} is not a tool in this environment; `pkg list` shows what is"
                );
                self.log("pkg-remove", Some(package), false, Some(&reason));
                return Response::Denied { reason };
            };
            if !tools.iter().any(|t| t.packages == tool.packages) {
                tools.push(tool);
            }
        }
        if tools.is_empty() {
            return Response::Denied {
                reason: "pkg remove needs a package name".into(),
            };
        }
        // A tool image is one build; taking out part of it would mean another
        // build. Say which packages travel together instead.
        for tool in &tools {
            let missing: Vec<&String> = tool
                .packages
                .iter()
                .filter(|p| !packages.contains(p))
                .collect();
            if !missing.is_empty() {
                let reason = format!(
                    "{} was installed together with {}; remove them together: pkg remove {}",
                    packages.join(", "),
                    missing
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    tool.packages.join(" ")
                );
                self.log(
                    "pkg-remove",
                    Some(&packages.join(",")),
                    false,
                    Some(&reason),
                );
                return Response::Denied { reason };
            }
        }
        let sets: Vec<Vec<String>> = tools.iter().map(|t| t.packages.clone()).collect();
        for set in sets {
            if let Err(reason) = self.apply(
                "pkg-remove",
                &packages.join(","),
                Environment::REMOVE_TOOL.with(set),
            ) {
                return Response::Denied { reason };
            }
        }
        Response::NeedsRestart {
            detail: format!(
                "{} removed from this environment. Run `restart` to drop it here.",
                packages.join(", ")
            ),
        }
    }

    fn perm_entries(&self) -> Vec<PermEntry> {
        use collab_types::caps::{Effect, Expiry, Scope};
        self.environment
            .current()
            .grants
            .iter()
            .map(|c| PermEntry {
                id: c.id.clone(),
                scope: match &c.scope {
                    Scope::FsPath(p) => p.clone(),
                    Scope::NetHost(h) => format!("net {h}"),
                    other => format!("{other:?}"),
                },
                allows: format!(
                    "{} {:?}",
                    match c.effect {
                        Effect::Grant => "grant",
                        Effect::Restrict => "restrict",
                    },
                    c.access
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
        let env = self.environment.current();
        let policy = Policy::new(env.grants.clone());
        SessionStatus {
            session: self.session_id.clone(),
            environment: env.name.clone(),
            record: self.environment.key.clone(),
            runtime: runtime_label(&env),
            tools: env
                .tools
                .iter()
                .map(|t| {
                    format!(
                        "{} ({})",
                        t.packages.join(", "),
                        t.images
                            .get(host_arch())
                            .map_or("not built for this architecture yet", String::as_str)
                    )
                })
                .collect(),
            seccomp: "enforcing: namespace and mount syscalls denied".into(),
            landlock: self.landlock.clone(),
            trusted_dirs: policy
                .trusted_dirs(now)
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            readable_dirs: policy
                .readable_dirs(now)
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            audit_records: self.audit.read_all().len(),
            network: NetworkStatus::of(self.egress.as_deref()),
        }
    }

    fn log(&self, request: &str, subject: Option<&str>, allowed: bool, detail: Option<&str>) {
        let mut rec = AuditRecord::new(&self.session_id, &self.environment.key, request, allowed);
        if let Some(s) = subject {
            rec = rec.about(s);
        }
        if let Some(d) = detail {
            rec = rec.because(d);
        }
        self.audit.append(&rec);
    }
}

/// The runtime as `pkg list` and `guard status` show it: the release and
/// this node's image.
fn runtime_label(env: &Environment) -> String {
    match env.runtime.images.get(host_arch()) {
        Some(id) => format!("{} ({id})", env.runtime.release),
        None => format!("{} (no image for {})", env.runtime.release, host_arch()),
    }
}

/// A tool build the broker has decided to run.
#[derive(Debug, Clone)]
struct ToolPlan {
    runtime: tilo_environment::PackageId,
    wanted: Vec<String>,
}

fn run_build(
    builder: &dyn ToolBuilder,
    plan: &ToolPlan,
    progress: &mut dyn Write,
) -> Result<tilo_environment::PackageId, String> {
    builder
        .build(&plan.runtime, &plan.wanted, progress)
        .map_err(|e| e.to_string())
}

/// A kernel pathname alias for sockets under a long data directory.
/// The directory descriptor stays open for every use of its alias. Only the
/// original filesystem path is published to the guest; permissions and socket
/// identity are unchanged, and the descriptor is closed on exec.
struct SocketAddress {
    path: PathBuf,
    _directory: Option<std::fs::File>,
}

impl SocketAddress {
    fn new(socket: &Path) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        if std::os::unix::net::SocketAddr::from_pathname(socket).is_ok() {
            return Ok(Self {
                path: socket.to_path_buf(),
                _directory: None,
            });
        }
        let name = socket.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "socket requires a filename",
            )
        })?;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(socket.parent().unwrap_or_else(|| Path::new(".")))?;
        let path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
        // A long filename or invalid byte is still an error, never truncated.
        std::os::unix::net::SocketAddr::from_pathname(&path)?;
        Ok(Self {
            path,
            _directory: Some(directory),
        })
    }
}

/// A running broker: a listener and the thread serving it.
pub struct Broker {
    state: Arc<Mutex<BrokerState>>,
    socket: PathBuf,
    address: SocketAddress,
    shared: Arc<Shared>,
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
        let address = SocketAddress::new(&socket)?;
        // Owner-only: the socket is the route to the policy file. It lives in
        // the 0700 store already, but a mode on the socket itself costs nothing.
        let listener = tilo_files::native::private::bind_socket(&address.path)?;

        let state = Arc::new(Mutex::new(state));
        let thread_state = Arc::clone(&state);
        let shared = Arc::new(Shared::new()?);
        let thread_shared = Arc::clone(&shared);
        let connections = Arc::new(Mutex::new(Vec::new()));
        let thread_connections = Arc::clone(&connections);
        let connected = Arc::new(AtomicBool::new(false));
        let thread_connected = Arc::clone(&connected);
        let thread = std::thread::Builder::new()
            .name("tilo-ush-accept".into())
            .spawn(move || {
                while !thread_shared.stopped() {
                    match listener.accept() {
                        Ok((stream, _)) if thread_shared.stopped() => {
                            drop(stream);
                            break;
                        }
                        Ok((stream, _)) => {
                            thread_connected.store(true, Ordering::Release);
                            let connection_state = Arc::clone(&thread_state);
                            let connection_shared = Arc::clone(&thread_shared);
                            // Named, so an idle session's threads can be told apart.
                            let Ok(handle) = std::thread::Builder::new()
                                .name("tilo-ush-broker".into())
                                .spawn(move || {
                                    serve_connection(stream, &connection_state, &connection_shared);
                                })
                            else {
                                continue;
                            };
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
            address,
            shared,
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
}

impl Drop for Broker {
    fn drop(&mut self) {
        // Wakes every connection, idle or serving a host command, at once.
        self.shared.stop();
        if let Some(thread) = self.thread.take()
            && UnixStream::connect(&self.address.path).is_ok()
        {
            let _ = thread.join();
        }
        if let Ok(mut connections) = self.connections.lock() {
            for connection in connections.drain(..) {
                let _ = connection.join();
            }
        }
        let _ = std::fs::remove_file(&self.address.path);
    }
}

fn serve_connection(stream: UnixStream, state: &Arc<Mutex<BrokerState>>, shared: &Shared) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = write_half;
    while let Some(line) = next_line(&mut reader, shared) {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                if let Request::AddTool { packages } = &req {
                    if serve_add_tool(packages, state, &mut writer) {
                        continue;
                    }
                    break;
                }
                let command = state
                    .lock()
                    .ok()
                    .and_then(|s| host_command_for(&s.host_root, &s.environment.policy(), &req));
                if let Some(command) = command {
                    if serve_host_command(&command, state, &mut writer, shared) {
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

/// What the broker's connections share: whether it is stopping, and the lock
/// that keeps host updates one at a time.
///
/// Every wait of a connection is one `poll(2)` on the descriptors that can end
/// it (its guest's connection, a command's pipes and exit, `stopping`), so an
/// idle connection, a silent host command and an update waiting for another
/// all sleep until something happens.
struct Shared {
    stop: AtomicBool,
    /// Rung once, when the broker stops, and never drained.
    stopping: WakeFd,
    host_updates: HostUpdateGate,
}

impl Shared {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            stop: AtomicBool::new(false),
            stopping: WakeFd::new()?,
            host_updates: HostUpdateGate::new()?,
        })
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.stopping.ring();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Keeps host updates one at a time. A waiting update sleeps until the
/// running one lets go (`released` rings) or its own guest leaves.
struct HostUpdateGate {
    lock: Mutex<()>,
    released: WakeFd,
}

/// A held [`HostUpdateGate`]; dropping it lets the next update start.
struct HostUpdateGuard<'a> {
    held: Option<std::sync::MutexGuard<'a, ()>>,
    released: &'a WakeFd,
}

impl Drop for HostUpdateGuard<'_> {
    fn drop(&mut self) {
        drop(self.held.take());
        self.released.ring();
    }
}

impl HostUpdateGate {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            lock: Mutex::new(()),
            released: WakeFd::new()?,
        })
    }

    fn try_acquire(&self) -> Option<HostUpdateGuard<'_>> {
        let held = match self.lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(HostUpdateGuard {
            held: Some(held),
            released: &self.released,
        })
    }
}

const fn pollfd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

/// One `poll(2)`. A signal returns early with nothing ready; the caller's
/// loop looks again.
fn poll_fds(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> std::io::Result<()> {
    let count = libc::nfds_t::try_from(fds.len()).map_err(std::io::Error::other)?;
    // Round up, so a timed-out poll means the time really passed.
    let millis = timeout.map_or(-1, |timeout| {
        i32::try_from(timeout.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
    });
    // SAFETY: `fds` is a valid, exclusively borrowed array of `count` pollfd
    // entries for the duration of the call.
    if unsafe { libc::poll(fds.as_mut_ptr(), count, millis) } >= 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() != std::io::ErrorKind::Interrupted {
        return Err(error);
    }
    for fd in fds {
        fd.revents = 0;
    }
    Ok(())
}

/// What to wait for on a guest's connection while serving it: its end closing
/// or being shut for writing (Ctrl-C ends the client). Bytes the guest sends
/// meanwhile are not among them, so they cannot keep the broker awake.
const GUEST_GONE: libc::c_short = libc::POLLRDHUP;

/// Whether `revents` of a [`GUEST_GONE`] wait say the guest left, or its
/// connection failed.
const fn guest_left(revents: libc::c_short) -> bool {
    revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// The guest's next request line, or `None` at its end, on an error or once
/// the broker stops. Waits in `poll(2)` on the connection and the broker's
/// stop, never on a timer.
fn next_line(reader: &mut BufReader<UnixStream>, shared: &Shared) -> Option<String> {
    let mut line = Vec::new();
    loop {
        if reader.buffer().is_empty() {
            let mut fds = [
                pollfd(reader.get_ref().as_raw_fd(), libc::POLLIN),
                pollfd(shared.stopping.fd().as_raw_fd(), libc::POLLIN),
            ];
            poll_fds(&mut fds, None).ok()?;
            let [connection, stopping] = fds;
            if stopping.revents != 0 || shared.stopped() {
                return None;
            }
            if connection.revents == 0 {
                continue;
            }
        }
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if available.is_empty() {
            // A last line without its newline still counts, as `read_line`
            // returns it.
            if line.is_empty() {
                return None;
            }
            return String::from_utf8(line).ok();
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let taken = newline.map_or(available.len(), |at| at.saturating_add(1));
        line.extend_from_slice(available.get(..taken).unwrap_or_default());
        reader.consume(taken);
        if newline.is_some() {
            return String::from_utf8(line).ok();
        }
    }
}

/// A descriptor that becomes readable when `child` exits, or `None` where the
/// kernel has no `pidfd_open`.
fn exit_fd(child: &Child) -> Option<OwnedFd> {
    let pid = libc::pid_t::try_from(child.id()).ok()?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or
    // -1. The child is unreaped, so its pid still names exactly it.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let fd = RawFd::try_from(fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: the kernel returned a fresh descriptor this call now owns.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Without a pidfd, a command whose pipes closed is looked at this often until
/// it exits. Only kernels older than Linux 5.3 take this path.
const EXIT_POLL_WITHOUT_PIDFD: Duration = Duration::from_millis(50);

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

/// The guest hung up (Ctrl-C ends the client) or the broker is stopping.
#[derive(Debug)]
struct Cancelled;

/// Take the lock that keeps host updates one at a time. A second update
/// says why it is not starting instead of sitting silent, and gives up when
/// its guest does. `writer` is the guest's connection.
fn acquire_host_mutation<'a>(
    command: &HostCommand,
    shared: &'a Shared,
    writer: &mut UnixStream,
) -> Result<Option<HostUpdateGuard<'a>>, Cancelled> {
    if !command.mutating {
        return Ok(None);
    }
    let gate = &shared.host_updates;
    let mut told = false;
    loop {
        // Drain before trying: a release racing the try rings again and
        // wakes the wait below.
        gate.released.drain();
        if let Some(guard) = gate.try_acquire() {
            return Ok(Some(guard));
        }
        if !told {
            told = true;
            let notice = WireFrame::Output {
                stderr: true,
                data: b"Waiting for another host update to finish...\n".to_vec(),
            };
            if !write_frame(writer, &notice) {
                return Err(Cancelled);
            }
        }
        let mut fds = [
            pollfd(writer.as_raw_fd(), GUEST_GONE),
            pollfd(shared.stopping.fd().as_raw_fd(), libc::POLLIN),
            pollfd(gate.released.fd().as_raw_fd(), libc::POLLIN),
        ];
        if poll_fds(&mut fds, None).is_err() {
            return Err(Cancelled);
        }
        let [guest, stopping, _] = fds;
        if guest_left(guest.revents) || stopping.revents != 0 || shared.stopped() {
            return Err(Cancelled);
        }
    }
}

/// The frames one chunk of command output becomes: the part still within
/// the output cap, and the notice when this chunk crosses it. `forwarded`
/// counts every byte the command printed so far.
fn capped_output(
    command: &HostCommand,
    stderr: bool,
    data: &[u8],
    forwarded: &mut usize,
) -> Vec<WireFrame> {
    let room = MAX_OUTPUT_BYTES.saturating_sub(*forwarded);
    let shown = data.get(..room.min(data.len())).unwrap_or_default();
    let was_within_cap = *forwarded <= MAX_OUTPUT_BYTES;
    *forwarded = forwarded.saturating_add(data.len());
    let mut frames = Vec::new();
    if !shown.is_empty() {
        frames.push(WireFrame::Output {
            stderr,
            data: shown.to_vec(),
        });
    }
    if was_within_cap && *forwarded > MAX_OUTPUT_BYTES {
        frames.push(WireFrame::Output {
            stderr: true,
            data: format!(
                "\n{} printed more than {} KiB; the rest of its output is not shown. \
                 It keeps running; its exit status follows.\n",
                command.program,
                MAX_OUTPUT_BYTES / 1024
            )
            .into_bytes(),
        });
    }
    frames
}

/// Record, and answer, a command the broker stopped at its time limit.
fn past_time_limit(command: &HostCommand, state: &Arc<Mutex<BrokerState>>) -> Response {
    let reason = format!(
        "`{} {}` did not finish within {} and was stopped",
        command.program,
        command.args.join(" "),
        limit_label(command.time_limit.unwrap_or_default())
    );
    if let Ok(s) = state.lock() {
        s.log(
            command.operation,
            Some(command.program),
            false,
            Some(&reason),
        );
    }
    Response::Denied { reason }
}

/// Run a host management command without retaining the state lock,
/// forwarding its output as it is produced. `writer` is the guest's
/// connection. Returns true when the complete response reached the guest.
fn serve_host_command(
    command: &HostCommand,
    state: &Arc<Mutex<BrokerState>>,
    writer: &mut UnixStream,
    shared: &Shared,
) -> bool {
    let Ok(mutation_guard) = acquire_host_mutation(command, shared, writer) else {
        log_disconnected(state, command);
        return false;
    };
    let mut child = match start_host_command(command, state) {
        Ok(child) => child,
        Err(response) => {
            return complete_host_command(writer, response, mutation_guard);
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        bounded_command::terminate_tree(&mut child, Duration::ZERO);
        return false;
    };
    let pipes = [
        std::fs::File::from(OwnedFd::from(stdout)),
        std::fs::File::from(OwnedFd::from(stderr)),
    ];
    let (code, forwarded) = match pump_host_command(command, &mut child, pipes, writer, shared) {
        Pumped::Exited { code, forwarded } => (code, forwarded),
        Pumped::Disconnected => {
            bounded_command::terminate_tree(&mut child, Duration::ZERO);
            log_disconnected(state, command);
            return false;
        }
        Pumped::TimedOut => {
            bounded_command::terminate_tree(&mut child, Duration::ZERO);
            let response = past_time_limit(command, state);
            return complete_host_command(writer, response, mutation_guard);
        }
    };

    if let Ok(s) = state.lock() {
        let detail = if forwarded > MAX_OUTPUT_BYTES {
            format!(
                "exit {code}; output past {} KiB not shown",
                MAX_OUTPUT_BYTES / 1024
            )
        } else {
            format!("exit {code}")
        };
        s.log(
            command.operation,
            Some(command.program),
            true,
            Some(&detail),
        );
    }
    complete_host_command(writer, Response::Command { code }, mutation_guard)
}

/// How serving a running host command ended.
enum Pumped {
    /// It exited and closed its output, having printed `forwarded` bytes.
    Exited { code: i32, forwarded: usize },
    /// The guest left or the broker is stopping.
    Disconnected,
    /// Its time limit passed.
    TimedOut,
}

/// A running host command's output on its way to the guest.
struct HostOutput<'a> {
    command: &'a HostCommand,
    /// stdout, then stderr; each `None` once it closed.
    pipes: [Option<std::fs::File>; 2],
    /// Bytes the command printed so far, shown or not.
    forwarded: usize,
    /// When the guest was last sent something.
    last_activity: Instant,
}

impl HostOutput<'_> {
    fn complete(&self) -> bool {
        self.pipes.iter().all(Option::is_none)
    }

    fn fds(&self) -> [RawFd; 2] {
        self.pipes
            .each_ref()
            .map(|pipe| pipe.as_ref().map_or(-1, std::os::fd::AsRawFd::as_raw_fd))
    }

    /// Forward what the pipes `ready` names hold. False when the guest
    /// stopped reading.
    fn forward(&mut self, ready: [bool; 2], writer: &mut UnixStream) -> bool {
        let mut buffer = [0_u8; 8192];
        for ((pipe, ready), stderr) in self.pipes.iter_mut().zip(ready).zip([false, true]) {
            let Some(file) = pipe.as_mut().filter(|_| ready) else {
                continue;
            };
            let read = match file.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Ok(0) | Err(_) => {
                    *pipe = None;
                    continue;
                }
                Ok(read) => read,
            };
            let data = buffer.get(..read).unwrap_or_default();
            let frames = capped_output(self.command, stderr, data, &mut self.forwarded);
            for frame in &frames {
                if !write_frame(writer, frame) {
                    return false;
                }
            }
            if !frames.is_empty() {
                self.last_activity = Instant::now();
            }
        }
        true
    }
}

/// Forward `child`'s output to the guest until it is complete and the child
/// has exited. Sleeps in one `poll(2)` on the output pipes, the guest's
/// connection, the broker's stop and, once the pipes closed, the child's
/// exit, with the time limit or the next heartbeat as its timeout.
fn pump_host_command(
    command: &HostCommand,
    child: &mut Child,
    pipes: [std::fs::File; 2],
    writer: &mut UnixStream,
    shared: &Shared,
) -> Pumped {
    let deadline = command
        .time_limit
        .and_then(|limit| Instant::now().checked_add(limit));
    let exited = exit_fd(child);
    let mut output = HostOutput {
        command,
        pipes: pipes.map(Some),
        forwarded: 0,
        last_activity: Instant::now(),
    };
    loop {
        // Output is complete only once both pipes closed; a helper which
        // inherited them can outlive the command itself.
        let complete = output.complete();
        if complete {
            let forwarded = output.forwarded;
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = status.code().unwrap_or(1);
                    return Pumped::Exited { code, forwarded };
                }
                Ok(None) => {}
                Err(_) => return Pumped::Exited { code: 1, forwarded },
            }
        }
        let heartbeat = output
            .last_activity
            .checked_add(HEARTBEAT_INTERVAL)
            .unwrap_or(output.last_activity);
        let wake_at = deadline.map_or(heartbeat, |deadline| deadline.min(heartbeat));
        let mut timeout = wake_at.saturating_duration_since(Instant::now());
        let exit_watch = match (&exited, complete) {
            (_, false) => -1,
            (Some(exited), true) => exited.as_raw_fd(),
            (None, true) => {
                timeout = timeout.min(EXIT_POLL_WITHOUT_PIDFD);
                -1
            }
        };
        let [out, err] = output.fds();
        let mut fds = [
            pollfd(out, libc::POLLIN),
            pollfd(err, libc::POLLIN),
            pollfd(writer.as_raw_fd(), GUEST_GONE),
            pollfd(shared.stopping.fd().as_raw_fd(), libc::POLLIN),
            pollfd(exit_watch, libc::POLLIN),
        ];
        if poll_fds(&mut fds, Some(timeout)).is_err() {
            return Pumped::Disconnected;
        }
        let [out, err, guest, stopping, _] = fds;
        if !output.forward([out.revents != 0, err.revents != 0], writer)
            || guest_left(guest.revents)
            || stopping.revents != 0
            || shared.stopped()
        {
            return Pumped::Disconnected;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Pumped::TimedOut;
        }
        if output.last_activity.elapsed() >= HEARTBEAT_INTERVAL {
            if !write_frame(writer, &WireFrame::Heartbeat) {
                return Pumped::Disconnected;
            }
            output.last_activity = Instant::now();
        }
    }
}

/// The mutation has finished before the guest can observe its completion.
/// A slow response reader must not retain the host's update serialization gate.
fn complete_host_command(
    writer: &mut UnixStream,
    response: Response,
    mutation_guard: Option<HostUpdateGuard<'_>>,
) -> bool {
    drop(mutation_guard);
    write_frame(writer, &WireFrame::Complete { response })
}

/// "30 minutes"; seconds for limits that are not whole minutes.
fn limit_label(limit: Duration) -> String {
    let secs = limit.as_secs();
    let (count, unit) = if secs >= 60 && secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        (secs, "second")
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural}")
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

/// Build a tool without holding the state lock, streaming the builder's
/// output to the guest as it comes. Returns true when the complete response
/// reached the guest.
fn serve_add_tool(
    packages: &[String],
    state: &Arc<Mutex<BrokerState>>,
    writer: &mut UnixStream,
) -> bool {
    let planned = match state.lock() {
        Ok(s) => s
            .plan_add_tool(packages)
            .map(|plan| (plan, Arc::clone(&s.builder))),
        Err(_) => Err(Response::Denied {
            reason: "the broker is in a failed state".into(),
        }),
    };
    let (plan, builder) = match planned {
        Ok(planned) => planned,
        Err(response) => return write_frame(writer, &WireFrame::Complete { response }),
    };
    let built = {
        let mut progress = FrameWriter { stream: writer };
        run_build(&*builder, &plan, &mut progress)
    };
    let response = match state.lock() {
        Ok(s) => s.finish_add_tool(&plan, built),
        Err(_) => Response::Denied {
            reason: "the broker is in a failed state".into(),
        },
    };
    write_frame(writer, &WireFrame::Complete { response })
}

/// Progress output, framed for the guest.
struct FrameWriter<'a> {
    stream: &'a mut UnixStream,
}

impl Write for FrameWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let frame = WireFrame::Output {
            stderr: false,
            data: data.to_vec(),
        };
        if write_frame(self.stream, &frame) {
            Ok(data.len())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the guest stopped listening",
            ))
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn write_frame(writer: &mut UnixStream, frame: &WireFrame) -> bool {
    let Ok(mut bytes) = serde_json::to_vec(frame) else {
        return false;
    };
    bytes.push(b'\n');
    writer.write_all(&bytes).is_ok() && writer.flush().is_ok()
}

/// Ask the broker one question, from inside a guest. A host command's output
/// is discarded; use [`ask_streaming`] to see it.
///
/// # Errors
///
/// Returns an error if the socket can't be reached, the request can't be
/// serialized, the write/read fails, or the reply isn't valid JSON.
pub fn ask(socket: &Path, req: &Request) -> std::io::Result<Response> {
    ask_streaming(socket, req, &mut std::io::sink(), &mut std::io::sink())
}

/// Ask the broker while copying host-command output to the supplied streams as
/// soon as it is produced. The output is not repeated in the response, which
/// carries only the exit status.
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
    let address = SocketAddress::new(socket)?;
    let stream = UnixStream::connect(&address.path)?;
    let mut writer = stream.try_clone()?;
    let mut bytes = serde_json::to_vec(req)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
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
            }
            WireFrame::Output {
                stderr: false,
                data,
            } => {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            WireFrame::Heartbeat => {
                if show_progress {
                    stderr.write_all(b"Still working...\n")?;
                    stderr.flush()?;
                }
            }
            WireFrame::Complete { response } => return Ok(response),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: Duration = Duration::from_secs(30);

    use tilo_environment::DEFAULT_RECORD;

    fn state(dir: &Path) -> BrokerState {
        state_with(dir, Arc::new(tilo_environment::NoToolBuilder))
    }

    fn state_with(dir: &Path, builder: Arc<dyn ToolBuilder>) -> BrokerState {
        let records = Records::at(dir.join("records"));
        let built_from = records
            .create(
                DEFAULT_RECORD,
                &Environment::new("Default", "2026.10", host_arch(), &image(1)),
            )
            .unwrap();
        BrokerState {
            session_id: "s1".into(),
            environment: EnvironmentHandle {
                records,
                key: DEFAULT_RECORD.into(),
                built_from,
            },
            audit: AuditLog::at(dir.join("audit")),
            restart_requested: false,
            host_root: dir.join("base"),
            images: Vec::new(),
            builder,
            landlock: "not attempted".into(),
            egress: None,
        }
    }

    fn trusted(s: &BrokerState) -> Vec<PathBuf> {
        Policy::new(s.environment.current().grants).trusted_dirs(Some(1_000))
    }

    #[test]
    fn legacy_shims_reach_the_vm_only_with_the_users_own_grants() {
        use collab_types::caps::{CapOverride, Principal, Scope, Subject};
        let policy = Policy::new(vec![CapOverride::grant(
            Subject::Principal(Principal::App(crate::RUNTIME_APP_ID.to_string())),
            Scope::FsPath("/var/home/you/src".into()),
            Access::Write,
        )]);
        // phase: a trusted directory is shared for the launch
        {
            let args = legacy_launch_args(
                &policy,
                "code",
                &[".".to_string()],
                Path::new("/var/home/you/src/app"),
            )
            .unwrap();
            assert_eq!(
                args,
                [
                    "launch",
                    "code",
                    "--cwd",
                    "/var/home/you/src/app",
                    "--share-cwd",
                    "--",
                    "."
                ]
            );
            let command = host_command_for(
                Path::new("/"),
                &policy,
                &Request::LegacyLaunch {
                    app: "code".into(),
                    args: Vec::new(),
                    cwd: "/var/home/you/src".into(),
                },
            )
            .unwrap();
            assert_eq!(command.binary, PathBuf::from("/usr/bin/tilo-legacy"));
            assert!(!command.mutating);
            // A legacy app runs for as long as the person keeps it open.
            assert_eq!(command.time_limit, None);
        }
        // phase: an untrusted directory is named but not shared
        {
            let args = legacy_launch_args(&policy, "code", &[], Path::new("/var/home/you/secret"))
                .unwrap();
            assert!(!args.contains(&"--share-cwd".to_string()));
        }
        // phase: option-looking names and relative directories are refused
        {
            for app in ["--image", "../x", "", "a b"] {
                assert!(legacy_launch_args(&policy, app, &[], Path::new("/x")).is_none());
            }
            assert!(legacy_launch_args(&policy, "code", &[], Path::new("src")).is_none());
        }
    }

    /// An executable fake of the host's `/usr/bin/<program>` under `dir`.
    fn fake_host_program(dir: &Path, program: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;

        let bin = dir.join("base/usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join(program);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Ask over the socket; the answer and everything streamed ahead of it.
    fn ask_capturing(socket: &Path, req: &Request) -> (Response, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let response = ask_streaming(socket, req, &mut stdout, &mut stderr).unwrap();
        (
            response,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    /// Output written to it arrives on a channel, as the guest CLI would
    /// print it.
    struct ChannelWriter(std::sync::mpsc::Sender<Vec<u8>>);

    impl Write for ChannelWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(bytes.to_vec());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Wait until `path` exists; only a hang reaches the bound.
    fn wait_for_file(path: &Path) {
        let started = Instant::now();
        while started.elapsed() < HANG_BOUND && !path.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(path.exists(), "{} never appeared", path.display());
    }

    #[test]
    fn host_management_commands_run_on_the_base_and_nothing_else() {
        // phase: commands run on the host base, stream both streams and keep their status
        {
            let dir = tempfile::tempdir().unwrap();
            fake_host_program(
                dir.path(),
                "tilo",
                "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 7\n",
            );
            fake_host_program(
                dir.path(),
                "app",
                "#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf 'note\\n' >&2\nexit 6\n",
            );
            let socket = dir.path().join("broker.sock");
            let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

            assert_eq!(
                ask_capturing(
                    &socket,
                    &Request::AppUpdate {
                        package: Some("browser".into()),
                    }
                ),
                (
                    Response::Command { code: 6 },
                    "update\nbrowser\n".into(),
                    "note\n".into()
                )
            );
            assert_eq!(
                ask_capturing(&socket, &Request::AllUpdates),
                (
                    Response::Command { code: 7 },
                    "update\n--yes\n".into(),
                    String::new()
                )
            );
            assert_eq!(
                ask_capturing(&socket, &Request::SystemUpdate),
                (
                    Response::Command { code: 7 },
                    "system\nupdate\n--yes\n".into(),
                    String::new()
                )
            );
            drop(broker);
        }
        // phase: host commands never run outside the streaming socket path
        {
            let dir = tempfile::tempdir().unwrap();
            let ran = dir.path().join("ran");
            fake_host_program(
                dir.path(),
                "app",
                &format!("#!/bin/sh\ntouch '{}'\n", ran.display()),
            );
            assert_eq!(
                state(dir.path()).handle(Request::AppUpdate { package: None }),
                Response::Denied {
                    reason: HOST_COMMAND_NEEDS_SOCKET.into()
                }
            );
            assert!(!ran.exists());
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
    fn host_output_reaches_the_guest_before_the_command_exits() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        // The second line waits for the test, so the first one can only have
        // arrived while the command was still running.
        fake_host_program(
            dir.path(),
            "app",
            &format!(
                "#!/bin/sh\nprintf 'started\\n'\n\
                 while [ ! -e '{}' ]; do sleep 0.05; done\nprintf 'finished\\n'\n",
                release.display()
            ),
        );
        let socket = dir.path().join("broker.sock");
        let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

        let command_socket = socket.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let command = std::thread::spawn(move || {
            ask_streaming(
                &command_socket,
                &Request::AppUpdate { package: None },
                &mut ChannelWriter(tx),
                &mut std::io::sink(),
            )
            .unwrap()
        });

        assert_eq!(rx.recv_timeout(HANG_BOUND).unwrap(), b"started\n");
        assert!(!command.is_finished());
        // A running command does not retain the state lock.
        let before = Instant::now();
        assert!(matches!(
            ask(&socket, &Request::Status),
            Ok(Response::Status { .. })
        ));
        assert!(before.elapsed() < Duration::from_millis(500));

        std::fs::write(&release, b"").unwrap();
        assert_eq!(rx.recv_timeout(HANG_BOUND).unwrap(), b"finished\n");
        assert_eq!(command.join().unwrap(), Response::Command { code: 0 });
        drop(broker);
    }

    #[test]
    fn completed_host_command_releases_gate_before_a_blocked_completion_frame() {
        let dir = tempfile::tempdir().unwrap();
        fake_host_program(dir.path(), "tilo", "#!/bin/sh\nexit 7\n");
        let state = Arc::new(Mutex::new(state(dir.path())));
        let command = {
            let state = state.lock().unwrap();
            host_command_for(
                &state.host_root,
                &state.environment.policy(),
                &Request::SystemUpdate,
            )
            .unwrap()
        };
        let audit = state.lock().unwrap().audit.clone();
        let shared = Arc::new(Shared::new().unwrap());
        let (mut writer, peer) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        // Fill every remaining byte so the actual Complete write cannot finish.
        loop {
            match writer.write(&[0]) {
                Ok(1) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                other => panic!("failed to fill completion socket: {other:?}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::spawn(move || {
            serve_host_command(&command, &state, &mut writer, &worker_shared)
        });
        let started = Instant::now();
        let mut completed = false;
        let mut released = false;
        while started.elapsed() < Duration::from_secs(2) {
            completed = audit
                .read_all()
                .iter()
                .any(|record| record.detail.as_deref() == Some("exit 7"));
            if completed && shared.host_updates.try_acquire().is_some() {
                released = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let blocked = !worker.is_finished();
        // Always retire the fixture socket and join before asserting, even on failure.
        peer.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(!worker.join().unwrap());
        assert!(completed, "production command did not reach completion");
        assert!(
            blocked,
            "fixture did not block the production completion write"
        );
        assert!(
            released,
            "completed command retained the host mutation gate while sending its response"
        );
    }

    #[test]
    fn host_updates_run_one_at_a_time_and_say_so() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events");
        let release = dir.path().join("release");
        fake_host_program(
            dir.path(),
            "app",
            &format!(
                "#!/bin/sh\nprintf '%s-start\\n' \"$2\" >> '{events}'\n\
                 if [ \"$2\" = first ]; then while [ ! -e '{release}' ]; do sleep 0.05; done; fi\n\
                 printf '%s-end\\n' \"$2\" >> '{events}'\n",
                events = events.display(),
                release = release.display(),
            ),
        );
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
        wait_for_file(&events);

        let second_socket = socket.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            ask_streaming(
                &second_socket,
                &Request::AppUpdate {
                    package: Some("second".into()),
                },
                &mut std::io::sink(),
                &mut ChannelWriter(tx),
            )
            .unwrap()
        });
        let notice = rx.recv_timeout(HANG_BOUND).unwrap();
        assert!(String::from_utf8_lossy(&notice).contains("Waiting for another host update"));
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

        std::fs::write(&release, b"").unwrap();
        assert_eq!(first.join().unwrap(), Response::Command { code: 0 });
        assert_eq!(second.join().unwrap(), Response::Command { code: 0 });
        assert_eq!(
            std::fs::read_to_string(&events).unwrap(),
            "first-start\nfirst-end\nsecond-start\nsecond-end\n"
        );
        drop(broker);
    }

    #[test]
    fn output_past_the_cap_is_dropped_but_the_exit_status_still_arrives() {
        let dir = tempfile::tempdir().unwrap();
        // One stream, so the line after the flood cannot overtake it.
        fake_host_program(
            dir.path(),
            "app",
            &format!(
                "#!/bin/sh\nhead -c {} /dev/zero | tr '\\0' x\nprintf 'after the flood\\n'\nexit 3\n",
                MAX_OUTPUT_BYTES.saturating_mul(2)
            ),
        );
        let socket = dir.path().join("broker.sock");
        let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();

        let (response, stdout, stderr) =
            ask_capturing(&socket, &Request::AppUpdate { package: None });
        assert_eq!(response, Response::Command { code: 3 });
        assert_eq!(stdout.len(), MAX_OUTPUT_BYTES);
        assert!(stdout.bytes().all(|byte| byte == b'x'));
        assert!(
            stderr.contains("the rest of its output is not shown"),
            "{stderr}"
        );
        let audit = broker.state().lock().unwrap().audit.read_all();
        assert!(audit.iter().any(|record| {
            record.allowed
                && record
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("exit 3") && detail.contains("not shown"))
        }));
        drop(broker);
    }

    #[test]
    fn a_host_command_past_its_time_limit_is_stopped_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        fake_host_program(
            dir.path(),
            "app",
            &format!(
                "#!/bin/sh\nsleep 30 &\nhelper=$!\nprintf '%s %s' $$ \"$helper\" > '{}'\n\
                 printf 'working\\n'\nwait\n",
                pid_file.display()
            ),
        );
        let command = HostCommand {
            operation: "app-update",
            program: "app",
            args: vec!["update".into()],
            binary: dir.path().join("base/usr/bin/app"),
            mutating: true,
            time_limit: Some(Duration::from_secs(1)),
        };
        let state = Arc::new(Mutex::new(state(dir.path())));
        let (mut writer, guest_side) = UnixStream::pair().unwrap();

        let started = Instant::now();
        assert!(serve_host_command(
            &command,
            &state,
            &mut writer,
            &Shared::new().unwrap(),
        ));
        assert!(started.elapsed() >= Duration::from_secs(1));
        drop(writer);

        let frames: Vec<WireFrame> = BufReader::new(guest_side)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect();
        assert!(frames.iter().any(|frame| matches!(
            frame,
            WireFrame::Output { stderr: false, data } if data == b"working\n"
        )));
        let Some(WireFrame::Complete {
            response: Response::Denied { reason },
        }) = frames.last()
        else {
            panic!("expected a refusal, got {frames:?}");
        };
        assert_eq!(
            reason,
            "`app update` did not finish within 1 second and was stopped"
        );
        let pids: Vec<i32> = std::fs::read_to_string(&pid_file)
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        let deadline = Instant::now() + HANG_BOUND;
        while Instant::now() < deadline && !pids.iter().all(|pid| has_terminated(*pid)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            pids.iter().all(|pid| has_terminated(*pid)),
            "the command process group outlived its time limit"
        );
        let audit = state.lock().unwrap().audit.read_all();
        assert!(audit.iter().any(|record| {
            !record.allowed
                && record
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("did not finish"))
        }));
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
                "#!/bin/sh\nsleep 30 &\nhelper=$!\nprintf '%s %s' $$ \"$helper\" > '{}.pending'\nmv '{}.pending' '{}'\nwait\n",
                pid_file.display(),
                pid_file.display(),
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
        // Observing a terminated process can precede the broker's wait/reap
        // returning and its subsequent cancellation audit. Join the actual
        // connection worker before observing that final production record.
        let state = broker.state().clone();
        drop(broker);
        let audit = state.lock().unwrap().audit.read_all();
        assert!(audit.iter().any(|record| {
            !record.allowed
                && record
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("disconnected"))
        }));
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
            // In the record, so the next session picks it up.
            assert_eq!(trusted(&s).len(), 1);
        }
        // phase: trusting_a_guest_owned_path_is_refused_and_recorded
        {
            let dir = tempfile::tempdir().unwrap();
            let mut s = state(dir.path());
            let resp = s.handle(Request::TrustDir {
                path: PathBuf::from("/usr"),
            });
            assert!(matches!(resp, Response::Denied { .. }), "{resp:?}");
            assert!(s.environment.current().grants.is_empty());

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

            assert_eq!(
                trusted(&s),
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
    fn restart_is_a_flag_and_status_reports_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        assert!(!s.restart_requested);
        assert_eq!(s.handle(Request::Restart), Response::Ok);
        assert!(s.restart_requested);

        let Response::Status { status } = s.handle(Request::Status) else {
            panic!("expected a status");
        };
        assert_eq!(status.environment, "Default");
        assert_eq!(status.record, DEFAULT_RECORD);
        assert_eq!(status.runtime, format!("2026.10 ({})", image(1)));
        assert!(status.seccomp.starts_with("enforcing"));
    }

    struct FakeBuilder {
        id: tilo_environment::PackageId,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl ToolBuilder for FakeBuilder {
        fn build(
            &self,
            runtime: &tilo_environment::PackageId,
            packages: &[String],
            progress: &mut dyn Write,
        ) -> Result<tilo_environment::PackageId, tilo_environment::ToolBuildError> {
            use tilo_environment::ToolBuildError;
            if *runtime != image(1) {
                return Err(ToolBuildError::Failed(
                    "a tool is built for the environment's runtime".into(),
                ));
            }
            writeln!(progress, "building {}", packages.join(" "))
                .map_err(|e| ToolBuildError::Failed(e.to_string()))?;
            self.calls
                .lock()
                .map_err(|e| ToolBuildError::Failed(e.to_string()))?
                .push(packages.to_vec());
            Ok(self.id)
        }
    }

    fn image(byte: u8) -> tilo_environment::PackageId {
        tilo_environment::PackageId::from_bytes([byte; 32])
    }

    fn fake_builder() -> Arc<FakeBuilder> {
        Arc::new(FakeBuilder {
            id: image(2),
            calls: Mutex::new(Vec::new()),
        })
    }

    #[test]
    fn pkg_install_builds_a_tool_and_adds_it_to_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let builder = fake_builder();
        let mut s = state_with(dir.path(), builder.clone());

        let resp = s.handle(Request::AddTool {
            packages: vec!["pandoc".into()],
        });
        assert!(matches!(resp, Response::NeedsRestart { .. }), "{resp:?}");
        let stored = s.environment.records.load(DEFAULT_RECORD).unwrap().unwrap();
        assert_eq!(stored.tools.len(), 1);
        assert_eq!(
            stored.tools[0].images.get(host_arch()),
            Some(&image(2).to_string())
        );
        assert_eq!(stored.tools[0].packages, vec!["pandoc".to_string()]);

        // Already there: nothing is built.
        let resp = s.handle(Request::AddTool {
            packages: vec!["pandoc".into()],
        });
        assert!(
            matches!(resp, Response::NeedsRestart { ref detail } if detail.contains("already"))
        );
        assert_eq!(builder.calls.lock().unwrap().len(), 1);

        let Response::Tools { runtime, tools } = s.handle(Request::ListTools) else {
            panic!("expected tools");
        };
        assert_eq!(runtime, format!("2026.10 ({})", image(1)));
        assert_eq!(tools[0].packages, vec!["pandoc".to_string()]);

        let audit = s.audit.read_all();
        assert!(
            audit
                .iter()
                .any(|r| r.request == "pkg-install" && r.allowed)
        );
    }

    #[test]
    fn without_a_builder_pkg_install_says_building_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state_with(dir.path(), Arc::new(tilo_environment::NoToolBuilder));
        let Response::Denied { reason } = s.handle(Request::AddTool {
            packages: vec!["pandoc".into()],
        }) else {
            panic!("an install without a builder must be refused");
        };
        assert!(reason.contains("not available"), "{reason}");
        assert!(s.environment.current().tools.is_empty());
        assert!(
            s.audit
                .read_all()
                .iter()
                .any(|r| r.request == "pkg-install" && !r.allowed)
        );
    }

    #[test]
    fn pkg_remove_takes_a_whole_tool_out_or_says_what_travels_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state_with(dir.path(), fake_builder());
        s.handle(Request::AddTool {
            packages: vec!["git".into(), "jq".into()],
        });

        let Response::Denied { reason } = s.handle(Request::RemoveTool {
            packages: vec!["jq".into()],
        }) else {
            panic!("half a tool cannot be removed");
        };
        assert!(reason.contains("pkg remove git jq"), "{reason}");
        assert!(matches!(
            s.handle(Request::RemoveTool {
                packages: vec!["htop".into()]
            }),
            Response::Denied { .. }
        ));

        let resp = s.handle(Request::RemoveTool {
            packages: vec!["jq".into(), "git".into()],
        });
        assert!(matches!(resp, Response::NeedsRestart { .. }), "{resp:?}");
        assert!(s.environment.current().tools.is_empty());
    }

    #[test]
    fn provenance_names_the_image_a_path_comes_from() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(runtime.join("usr/bin")).unwrap();
        std::fs::write(runtime.join("usr/bin/sh"), b"").unwrap();
        let mut s = state(dir.path());
        s.images = vec![("runtime tapp:01".into(), runtime)];
        assert_eq!(
            s.handle(Request::Provenance {
                path: PathBuf::from("/usr/bin/sh")
            }),
            Response::Provenance {
                image: Some("runtime tapp:01".into())
            }
        );
        assert_eq!(
            s.handle(Request::Provenance {
                path: PathBuf::from("/home/you/sh")
            }),
            Response::Provenance { image: None }
        );
    }

    #[test]
    fn a_tool_build_streams_its_progress_to_the_guest() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("broker.sock");
        let broker = Broker::start(socket.clone(), state_with(dir.path(), fake_builder())).unwrap();
        let mut out = Vec::new();
        let resp = ask_streaming(
            &socket,
            &Request::AddTool {
                packages: vec!["pandoc".into()],
            },
            &mut out,
            &mut std::io::sink(),
        )
        .unwrap();
        assert!(matches!(resp, Response::NeedsRestart { .. }), "{resp:?}");
        assert_eq!(String::from_utf8(out).unwrap(), "building pandoc\n");
        drop(broker);
    }

    #[test]
    fn broker_under_long_data_home_round_trips_and_cleans_up() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("a".repeat(90)).join("ush/sessions/session");
        std::fs::create_dir_all(&data).unwrap();
        let socket = data.join("broker.sock");
        assert!(std::os::unix::net::SocketAddr::from_pathname(&socket).is_err());
        let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();
        assert_eq!(broker.socket(), socket);
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o077,
            0
        );
        let response = ask(&socket, &Request::ListPerms).unwrap();
        assert!(matches!(response, Response::Perms { .. }));
        assert!(broker.guest_connected());
        // Reconnect to the same owner-only filesystem socket, not a new alias
        // published to the peer. Renaming its parent cannot invalidate the
        // broker's retained descriptor used to wake and clean up on shutdown.
        let renamed = data.with_file_name("renamed-session");
        std::fs::rename(&data, &renamed).unwrap();
        let moved_socket = renamed.join("broker.sock");
        let response = ask(&moved_socket, &Request::ListPerms).unwrap();
        assert!(matches!(response, Response::Perms { .. }));
        drop(broker);
        assert!(
            !moved_socket.exists(),
            "session socket must be removed on shutdown"
        );
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

    use tilo_test_env::idle::{assert_stays_asleep, spawned_thread};

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

    #[test]
    fn an_idle_guest_connection_sleeps_until_the_guest_asks() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("broker.sock");
        let broker = Broker::start(sock.clone(), state(dir.path())).unwrap();
        let (stream, tid) =
            spawned_thread("tilo-ush-broker", || UnixStream::connect(&sock).unwrap());
        assert_stays_asleep(&tid, "ush broker connection");
        // Still served: a request on the same connection is answered.
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(b"{\"op\":\"status\"}\n").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert!(
            matches!(serde_json::from_str(&line), Ok(Response::Status { .. })),
            "{line}"
        );
        // Stopping the broker ends the connection it still holds.
        drop(broker);
    }

    #[test]
    fn a_silent_host_command_and_an_update_waiting_for_it_sleep() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events");
        let release = dir.path().join("release");
        fake_host_program(
            dir.path(),
            "app",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$2\" >> '{events}'\n\
                 if [ \"$2\" = first ]; then while [ ! -e '{release}' ]; do sleep 0.05; done; fi\n",
                events = events.display(),
                release = release.display(),
            ),
        );
        let socket = dir.path().join("broker.sock");
        let broker = Broker::start(socket.clone(), state(dir.path())).unwrap();
        let ask_update = |package: &str| {
            let socket = socket.clone();
            let request = Request::AppUpdate {
                package: Some(package.into()),
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let asker = std::thread::spawn(move || {
                ask_streaming(
                    &socket,
                    &request,
                    &mut std::io::sink(),
                    &mut ChannelWriter(tx),
                )
                .unwrap()
            });
            (asker, rx)
        };

        let ((first, _), running) = spawned_thread("tilo-ush-broker", || ask_update("first"));
        wait_for_file(&events);
        assert_stays_asleep(&running, "silent host command");

        let ((second, notices), waiting) =
            spawned_thread("tilo-ush-broker", || ask_update("second"));
        let notice = notices.recv_timeout(HANG_BOUND).unwrap();
        assert!(String::from_utf8_lossy(&notice).contains("Waiting for another host update"));
        assert_stays_asleep(&waiting, "host update waiting for another");

        std::fs::write(&release, b"").unwrap();
        assert_eq!(first.join().unwrap(), Response::Command { code: 0 });
        assert_eq!(second.join().unwrap(), Response::Command { code: 0 });
        assert_eq!(std::fs::read_to_string(&events).unwrap(), "first\nsecond\n");
        drop(broker);
    }

    #[test]
    fn allow_host_writes_a_net_grant_that_perm_list_shows_and_revoke_removes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state(dir.path());
        // Without a session network the grant is recorded for the next one.
        let Response::NeedsRestart { detail } = s.handle(Request::AllowHost {
            host: "*.Crates.io".into(),
        }) else {
            panic!("allow-host without a network must say restart");
        };
        assert!(
            detail.contains("crates.io and every host under it"),
            "{detail}"
        );
        let Response::Perms { entries } = s.handle(Request::ListPerms) else {
            panic!("no permission list");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].scope, "net .crates.io");
        assert_eq!(entries[0].id, tilo_environment::host_grant_id(".crates.io"));
        assert_eq!(
            s.environment.policy().net_hosts(Some(unix_now())),
            [".crates.io"]
        );

        let Response::Denied { reason } = s.handle(Request::AllowHost {
            host: "1.1.1.1".into(),
        }) else {
            panic!("an address literal was allowed");
        };
        assert!(reason.contains("address"), "{reason}");

        let resp = s.handle(Request::Revoke {
            id: entries[0].id.clone(),
        });
        assert!(matches!(resp, Response::NeedsRestart { .. }), "{resp:?}");
        assert!(
            s.environment
                .policy()
                .net_hosts(Some(unix_now()))
                .is_empty()
        );
        let Response::Status { status } = s.handle(Request::Status) else {
            panic!("no status");
        };
        assert_eq!(status.network, NetworkStatus::Offline);
    }
}
