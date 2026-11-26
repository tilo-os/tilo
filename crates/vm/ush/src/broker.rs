//! The broker: the one route from inside a guest to the environment record.
//!
//! It starts with the session and runs on the host. A request from the guest that
//! reaches past it arrives as a line of JSON on a Unix socket, is decided, and is
//! written to [`crate::audit`] whichever way it went. Neither the record nor the store
//! is bound into the namespace, so this is also the only writer of what was asked for.
//!
//! A new tool or bind needs a new mount namespace, so `pkg install` and `perm trust-dir`
//! answer [`Response::NeedsRestart`] rather than claim a change they have not made. A
//! host grant is different: the DNS stub and filter take it at once, so
//! `perm allow-host` and its revocation answer [`Response::Applied`].

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

use crate::audit::{AuditEntry, AuditLog};
use crate::policy::Policy;
use crate::records::Records;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    AppUpdate {
        package: Option<String>,
    },
    SystemUpdate,
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
    TrustDir {
        path: PathBuf,
    },
    AllowHost {
        host: String,
    },
    ListPerms,
    Revoke {
        id: String,
    },
    Check {
        path: PathBuf,
        write: bool,
    },
    AddTool {
        packages: Vec<String>,
    },
    RemoveTool {
        packages: Vec<String>,
    },
    ListTools,
    Provenance {
        path: PathBuf,
    },
    Status,
    Restart,
    LinuxVmLaunch {
        app: String,
        args: Vec<String>,
        cwd: PathBuf,
    },
    FileAction {
        action: crate::file_actions::FileAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<RunGroup>,
    },
}

pub use crate::file_actions::command::{COMMAND_GROUP, RunGroup};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Response {
    Ok,
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
    Command {
        code: i32,
    },
    Perms {
        entries: Vec<PermEntry>,
    },
    FileAction {
        id: String,
        operations: usize,
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

pub(crate) const MAX_ARG_BYTES: usize = 4096;
/// The largest request line the broker accepts from a guest (8 MiB).
///
/// The largest legitimate request is a [`Request::FileAction`] with
/// `MAX_OPERANDS` (256) paths of up to `PATH_MAX` (4096) bytes (1 MiB raw),
/// `MAX_TEXT_BYTES` (64 KiB) of inline text, and a [`RunGroup::label`] bounded
/// to [`MAX_ARG_BYTES`] (4096 bytes). Even if every byte were expanded sixfold
/// by JSON escaping (`\u00xx`), those fields plus the short [`RunGroup::id`]
/// and JSON framing stay under 7 MiB (~6.40 MiB). Anything past 8 MiB is
/// refused and ends that connection.
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024 * 1024;
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
    time_limit: Option<Duration>,
}

fn host_command_for(host_root: &Path, policy: &Policy, req: &Request) -> Option<HostCommand> {
    let (operation, program, args, mutating) = match req {
        Request::LinuxVmLaunch { app, args, cwd } => (
            "linux-vm-launch",
            LINUX_VM_PROGRAM,
            linux_vm_launch_args(policy, app, args, cwd)?,
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
        "linux-vm-launch" => None,
        _ if mutating => Some(UPDATE_TIME_LIMIT),
        _ => Some(QUERY_TIME_LIMIT),
    };
    Some(HostCommand {
        operation,
        program,
        args,
        binary: host_root.join(host_program_dir(program)).join(program),
        mutating,
        time_limit,
    })
}

const LINUX_VM_PROGRAM: &str = "tilo-linux-vm";

fn host_program_dir(program: &str) -> &'static str {
    if program == LINUX_VM_PROGRAM {
        "usr/libexec/tilo"
    } else {
        "usr/bin"
    }
}

/// `tilo-linux-vm launch <app> --cwd <cwd> [--share-cwd] -- <args…>`.
///
/// The directory a command is typed in is shared with the VM for that launch
/// only when the user trusted it to this runtime for writing: a guest cannot
/// widen what the VM sees beyond what the person already granted here.
/// `None` for an app name that is not a plain identifier or a relative cwd.
fn linux_vm_launch_args(
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
    pub audit_entries: usize,
    pub network: NetworkStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum NetworkStatus {
    Unrestricted,
    /// Only the addresses the DNS stub answered for these hosts.
    Filtered {
        hosts: Vec<String>,
    },
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

#[derive(Debug, Clone)]
pub struct EnvironmentHandle {
    pub var_dir: PathBuf,
    pub records: Records,
    pub key: String,
    pub built_from: Environment,
}

impl EnvironmentHandle {
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

    pub fn policy(&self) -> Policy {
        Policy::new(self.current().grants)
    }
}

pub struct BrokerState {
    pub session_id: String,
    pub environment: EnvironmentHandle,
    pub audit: AuditLog,
    pub restart_requested: bool,
    pub host_root: PathBuf,
    pub images: Vec<(String, PathBuf)>,
    pub builder: Arc<dyn ToolBuilder>,
    pub landlock: String,
    pub egress: Option<Arc<tilo_egress::Egress>>,
    pub person: String,
    pub device: String,
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
            Request::LinuxVmLaunch { app, args, cwd } => self.linux_vm_launch(&app, &args, &cwd),
            Request::FileAction { action, run } => {
                if run
                    .as_ref()
                    .is_some_and(|run| run.label.len() > MAX_ARG_BYTES)
                {
                    let reason =
                        format!("command label is too large (at most {MAX_ARG_BYTES} bytes)");
                    self.log(
                        action.verb(),
                        Some(&crate::file_actions::subjects(&action)),
                        false,
                        Some(&reason),
                    );
                    return Response::Denied { reason };
                }
                crate::file_actions::brokered(self, &action, run.as_ref())
            }
        }
    }

    fn linux_vm_launch(&self, app: &str, args: &[String], cwd: &Path) -> Response {
        if linux_vm_launch_args(&self.environment.policy(), app, args, cwd).is_some() {
            Response::Denied {
                reason: HOST_COMMAND_NEEDS_SOCKET.into(),
            }
        } else {
            let reason = format!("{app:?} is not a Linux VM app name");
            self.log("linux-vm-launch", Some(app), false, Some(&reason));
            Response::Denied { reason }
        }
    }

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
        use collab_types::caps::{Effect, Scope};
        use tilo_action_policy::Lifetime;
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
                        Effect::Grant | Effect::WorkspaceFileAccess => "grant",
                        Effect::Restrict | Effect::WorkspaceFileBoundary => "restrict",
                    },
                    c.access
                ),
                expiry: match c.lifetime {
                    Lifetime::Until(at) => format!("until {at} ms"),
                    lifetime => lifetime.key().into(),
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
            audit_entries: self.audit.read_all().len(),
            network: NetworkStatus::of(self.egress.as_deref()),
        }
    }

    fn log(&self, request: &str, subject: Option<&str>, allowed: bool, detail: Option<&str>) {
        let mut rec = AuditEntry::new(&self.session_id, &self.environment.key, request, allowed);
        if let Some(s) = subject {
            rec = rec.about(s);
        }
        if let Some(d) = detail {
            rec = rec.because(d);
        }
        self.audit.append(&rec);
    }
}

fn runtime_label(env: &Environment) -> String {
    match env.runtime.images.get(host_arch()) {
        Some(id) => format!("{} ({id})", env.runtime.release),
        None => format!("{} (no image for {})", env.runtime.release, host_arch()),
    }
}

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

pub struct Broker {
    state: Arc<Mutex<BrokerState>>,
    socket: PathBuf,
    address: SocketAddress,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    files: Option<std::thread::JoinHandle<()>>,
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
        let files = crate::file_actions::watch::spawn(Arc::clone(&state), Arc::clone(&shared))?;
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
            files: Some(files),
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

    pub fn guest_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn restart_requested(&self) -> bool {
        self.state.lock().is_ok_and(|s| s.restart_requested)
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(thread) = self.thread.take()
            && tilo_files::native::socket::connect(&self.address.path).is_ok()
        {
            let _ = thread.join();
        }
        if let Ok(mut connections) = self.connections.lock() {
            for connection in connections.drain(..) {
                let _ = connection.join();
            }
        }
        if let Some(files) = self.files.take() {
            let _ = files.join();
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
pub(crate) struct Shared {
    stop: AtomicBool,
    /// Rung once, when the broker stops, and never drained.
    pub(crate) stopping: WakeFd,
    host_updates: HostUpdateLock,
}

impl Shared {
    pub(crate) fn new() -> std::io::Result<Self> {
        Ok(Self {
            stop: AtomicBool::new(false),
            stopping: WakeFd::new()?,
            host_updates: HostUpdateLock::new()?,
        })
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.stopping.ring();
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Keeps host updates one at a time. A waiting update sleeps until the
/// running one lets go (`released` rings) or its own guest leaves.
struct HostUpdateLock {
    lock: Mutex<()>,
    released: WakeFd,
}

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

impl HostUpdateLock {
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

pub(crate) const fn pollfd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

pub(crate) fn poll_fds(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> std::io::Result<()> {
    let count = libc::nfds_t::try_from(fds.len()).map_err(std::io::Error::other)?;
    // Round up, so a timed-out poll means the time really passed.
    let millis = timeout.map_or(-1, |timeout| {
        i32::try_from(timeout.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
    });
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

const fn guest_left(revents: libc::c_short) -> bool {
    revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// The guest's next request line, or `None` at its end, on an error, if the
/// line exceeds [`MAX_REQUEST_LINE_BYTES`], or once the broker stops. Waits in
/// `poll(2)` on the connection and the broker's stop, never on a timer.
fn next_line(reader: &mut BufReader<UnixStream>, shared: &Shared) -> Option<String> {
    next_line_bounded(reader, shared, MAX_REQUEST_LINE_BYTES)
}

fn next_line_bounded(
    reader: &mut BufReader<UnixStream>,
    shared: &Shared,
    max_bytes: usize,
) -> Option<String> {
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
        let content_len = newline.unwrap_or(available.len());
        let new_len = line.len().checked_add(content_len)?;
        if new_len > max_bytes {
            tracing::warn!(
                max_bytes,
                "guest broker request exceeded maximum line length; closing connection"
            );
            return None;
        }
        if new_len > line.capacity() {
            // Grow geometrically for throughput, without ever allocating past `max_bytes`.
            let target = line
                .capacity()
                .saturating_mul(2)
                .max(new_len)
                .min(max_bytes);
            line.reserve_exact(target.saturating_sub(line.len()));
        }
        line.extend_from_slice(available.get(..content_len).unwrap_or_default());
        let consumed = newline.map_or(available.len(), |at| at.saturating_add(1));
        reader.consume(consumed);
        if newline.is_some() {
            return String::from_utf8(line).ok();
        }
    }
}

fn exit_fd(child: &Child) -> Option<OwnedFd> {
    let pid = libc::pid_t::try_from(child.id()).ok()?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or
    // -1. The child is unreaped, so its pid still names exactly it.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let fd = RawFd::try_from(fd).ok().filter(|fd| *fd >= 0)?;
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
    let check = &shared.host_updates;
    let mut told = false;
    loop {
        check.released.drain();
        if let Some(guard) = check.try_acquire() {
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
            pollfd(check.released.fd().as_raw_fd(), libc::POLLIN),
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

enum Pumped {
    Exited { code: i32, forwarded: usize },
    Disconnected,
    TimedOut,
}

struct HostOutput<'a> {
    command: &'a HostCommand,
    pipes: [Option<std::fs::File>; 2],
    /// Bytes the command printed so far, shown or not.
    forwarded: usize,
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
/// A slow response reader must not retain the host's update serialization check.
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

mod client;
pub use client::{ask, ask_streaming, ask_streaming_with_progress};

/// The broker's state over a scratch directory, for the tests of the modules
/// that act through it.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tilo_environment::DEFAULT_RECORD;

    pub(crate) fn state(dir: &Path) -> BrokerState {
        crate::records::testing::keep_serving(&dir.join("records"));
        let records = Records::at(dir.join("records"));
        let image = tilo_environment::PackageId::from_bytes([1; 32]);
        let built_from = records
            .create(
                DEFAULT_RECORD,
                &Environment::new("Default", "2026.10", host_arch(), &image),
            )
            .unwrap();
        BrokerState {
            session_id: "s1".into(),
            environment: EnvironmentHandle {
                var_dir: dir.join("var"),
                records,
                key: DEFAULT_RECORD.into(),
                built_from,
            },
            audit: AuditLog::at(dir.join("audit")),
            restart_requested: false,
            host_root: dir.join("base"),
            images: Vec::new(),
            builder: Arc::new(tilo_environment::NoToolBuilder),
            landlock: "not attempted".into(),
            egress: None,
            person: "person-ada".into(),
            device: "this-device".into(),
        }
    }
}
