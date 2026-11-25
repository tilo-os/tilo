//! What a server's person, and the administrators of the workspaces it
//! serves, do to the machine from their own devices.
//!
//! A server is headless: its controls are the `server` device actions its
//! daemon answers ([`Controls`]), never ssh. Each control is one
//! [`ServerOperation`] ([`requested`] reads an action's name and arguments as
//! one), and each answers with a typed [`ServerOperationResult`]: read-only
//! ones with the live answer (the server's health, what it holds and how its
//! copies stand, a service's journal), and the rest with what it asked a root
//! helper to do.
//!
//! The daemon is unprivileged, so anything that changes the machine
//! itself — restarting a unit, rebooting, staging an update, pausing updates,
//! going back to the previous system or making Tilo the default start — is
//! written at one fixed path
//! ([`CONTROL_REQUEST`]) for a root unit to carry out
//! (`os/usr/libexec/tilo-server-control`), the way the update policy is
//! ([`super::policy`]). The helper answers at [`CONTROL_RESULT`]. Nothing the
//! daemon writes is passed to a shell, and the helper checks it again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tilo_server_protocol::transport::{
    HeldCopy, HeldWorkspace, ServerAnswer, ServerHardware, ServerOperation, ServerOperationResult,
    ServerStatus,
};
use tilo_unlock_protocol::opened::{OPENED_PATH, Opening};
use tilo_user_identity::DeviceCert;

use crate::workspaces::Workspaces;

/// The file the daemon writes what its owner asked it to do, for the
/// root helper to carry out.
pub const CONTROL_REQUEST: &str = "control.request";
/// The file the root helper answers the request at.
pub const CONTROL_RESULT: &str = "control.result";
/// How long a mutating operation waits for the root helper to answer.
pub const CONTROL_WAIT: Duration = Duration::from_secs(60);
/// How often the helper's answer is looked for.
const POLL: Duration = Duration::from_millis(100);
/// The most journal lines one `Logs` answers with.
pub const MAX_LOG_LINES: u32 = 500;
/// How many journal lines a `journal` reads when it names no number.
const LOG_LINES: u32 = 200;

/// The actions of a server's `server` resource, as its daemon publishes them:
/// each is one [`ServerOperation`] ([`requested`]).
pub const ACTIONS: [&str; 10] = [
    "status",
    "journal",
    "restart",
    "reboot",
    "stage-update",
    "pause-updates",
    "resume-updates",
    "rollback",
    "drain",
    "set-default-start",
];

/// The operation the `server` action `action` asks for with `args`: a
/// `restart` and a `journal` name their systemd unit as `service`, and a
/// `journal` how many lines it reads as `lines`.
///
/// # Errors
/// `action` is no control of a server's, or names no unit where it needs one.
pub fn requested(action: &str, args: &Value) -> Result<ServerOperation, String> {
    let service = || {
        args.get("service")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|service| !service.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("Name the service to {action}, such as tilo-runtime.service."))
    };
    Ok(match action {
        "status" => ServerOperation::Status,
        "journal" => ServerOperation::Logs {
            service: service()?,
            lines: args
                .get("lines")
                .and_then(Value::as_u64)
                .map_or(LOG_LINES, |lines| {
                    u32::try_from(lines).unwrap_or(MAX_LOG_LINES)
                }),
        },
        "restart" => ServerOperation::Restart {
            service: service()?,
        },
        "reboot" => ServerOperation::Reboot,
        "stage-update" => ServerOperation::Update,
        "pause-updates" => ServerOperation::PauseUpdates,
        "resume-updates" => ServerOperation::ResumeUpdates,
        "rollback" => ServerOperation::Rollback,
        "drain" => ServerOperation::Drain,
        "set-default-start" => ServerOperation::SetDefaultStart,
        other => return Err(format!("A server has no control called {other}.")),
    })
}

/// The controls of this machine, as its daemon answers its `server` actions:
/// the workspaces it serves, where its state is kept (and its root helper
/// reads and answers), and how long that helper is waited for.
#[derive(Clone)]
pub struct Controls {
    workspaces: Workspaces,
    state_root: PathBuf,
    wait: Duration,
}

impl Controls {
    /// The controls of the server that serves `workspaces` and keeps its
    /// state at `state_root`.
    #[must_use]
    pub const fn new(workspaces: Workspaces, state_root: PathBuf) -> Self {
        Self {
            workspaces,
            state_root,
            wait: CONTROL_WAIT,
        }
    }

    /// These controls, waiting at most `wait` for the root helper: what a
    /// test gives a stand-in helper.
    #[must_use]
    pub const fn waiting(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Carry out the `server` action `action` with `args`.
    ///
    /// # Errors
    /// As [`requested`] and [`operate`].
    pub async fn act(&self, action: &str, args: &Value) -> Result<ServerOperationResult, String> {
        let operation = requested(action, args)?;
        carry_out(
            &self.state_root,
            Some(&self.workspaces),
            self.workspaces.device_cert(),
            operation,
            self.wait,
        )
        .await
    }

    /// The workspaces this server serves, as this machine's person's copies keep them: what a
    /// caller's grant to control the machine is decided from.
    #[must_use]
    pub const fn workspaces(&self) -> &Workspaces {
        &self.workspaces
    }
}
/// What the daemon writes for the root helper: one `key=value` line per
/// field, which a shell reads without a JSON parser on the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlRequest {
    /// Names this request, so a stale answer is never taken for it.
    id: String,
    /// What to do: `restart`, `reboot`, `update`, `pause-updates`,
    /// `resume-updates`, `rollback`, `set-default-start` or `logs`.
    verb: String,
    /// The systemd unit a `restart` or `logs` names.
    service: String,
    /// How many lines a `logs` reads.
    lines: u32,
}

impl ControlRequest {
    /// The file the helper reads.
    fn document(&self) -> String {
        format!(
            "id={}\nverb={}\nservice={}\nlines={}\n",
            self.id, self.verb, self.service, self.lines
        )
    }
}

/// What the root helper answers: its first line names the request and
/// whether it worked, and what follows is what it read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlAnswer {
    id: String,
    ok: bool,
    text: String,
}

/// Do `operation` on the server whose state root is `state_root`, held by
/// `workspaces` when this server keeps its person's admin workspace, answering
/// with what it found or did.
///
/// # Errors
/// The operation asked for something this server does not serve, or the helper
/// it asked did not answer in time.
pub async fn operate(
    state_root: &Path,
    workspaces: Option<&Workspaces>,
    own: &DeviceCert,
    operation: ServerOperation,
) -> Result<ServerOperationResult, String> {
    operate_within(state_root, workspaces, own, operation, CONTROL_WAIT).await
}

/// [`operate`], waiting at most `wait` for a root helper to answer: what
/// tests give a stand-in helper, and what a slow machine may need longer for.
///
/// # Errors
/// As [`operate`].
pub async fn operate_within(
    state_root: &Path,
    workspaces: Option<&Workspaces>,
    own: &DeviceCert,
    operation: ServerOperation,
    wait: Duration,
) -> Result<ServerOperationResult, String> {
    carry_out(state_root, workspaces, own, operation, wait).await
}

#[allow(
    clippy::too_many_lines,
    reason = "one match keeps every operation's request and answer side by side, which is what makes them comparable"
)]
async fn carry_out(
    state_root: &Path,
    workspaces: Option<&Workspaces>,
    own: &DeviceCert,
    operation: ServerOperation,
    wait: Duration,
) -> Result<ServerOperationResult, String> {
    let result = match operation {
        ServerOperation::Status => {
            let status = status(state_root, workspaces, own).await;
            ServerOperationResult {
                operation: "status".into(),
                done: true,
                summary: format!(
                    "up {}s, holding {} workspace(s)",
                    status.uptime_seconds,
                    status.holding.len()
                ),
                answer: ServerAnswer::Status(Box::new(status)),
            }
        }
        ServerOperation::Logs { service, lines } => {
            let lines = lines.min(MAX_LOG_LINES);
            let answer = ask_root(
                state_root,
                ControlRequest {
                    id: request_id(),
                    verb: "logs".into(),
                    service: valid_service(&service)?,
                    lines,
                },
                wait,
            )
            .await?;
            if !answer.ok {
                return Err(answer.text);
            }
            ServerOperationResult {
                operation: "logs".into(),
                done: true,
                summary: format!("the last {lines} lines of {service}"),
                answer: ServerAnswer::Text(answer.text),
            }
        }
        ServerOperation::Restart { service } => {
            let service = valid_service(&service)?;
            ran(
                state_root,
                ControlRequest {
                    id: request_id(),
                    verb: "restart".into(),
                    service: service.clone(),
                    lines: 0,
                },
                wait,
                "restart",
                format!("restarted {service}"),
            )
            .await?
        }
        ServerOperation::Update => {
            ran(
                state_root,
                ControlRequest {
                    id: request_id(),
                    verb: "update".into(),
                    service: String::new(),
                    lines: 0,
                },
                wait,
                "update",
                "fetched and staged the newest update".into(),
            )
            .await?
        }
        ServerOperation::PauseUpdates => {
            ran(
                state_root,
                ControlRequest {
                    id: request_id(),
                    verb: "pause-updates".into(),
                    service: String::new(),
                    lines: 0,
                },
                wait,
                "pause-updates",
                "stopped taking OS updates".into(),
            )
            .await?
        }
        ServerOperation::ResumeUpdates => {
            ran(
                state_root,
                ControlRequest {
                    id: request_id(),
                    verb: "resume-updates".into(),
                    service: String::new(),
                    lines: 0,
                },
                wait,
                "resume-updates",
                "follows OS updates again".into(),
            )
            .await?
        }
        ServerOperation::Reboot => {
            // The machine goes down for this answer, so it does not wait for
            // one: the request is written and the helper reboots.
            asked(
                state_root,
                &ControlRequest {
                    id: request_id(),
                    verb: "reboot".into(),
                    service: String::new(),
                    lines: 0,
                },
                "reboot",
                "the machine is rebooting".into(),
            )?
        }
        ServerOperation::Rollback => asked(
            state_root,
            &ControlRequest {
                id: request_id(),
                verb: "rollback".into(),
                service: String::new(),
                lines: 0,
            },
            "rollback",
            "boots the previous system from now on".into(),
        )?,
        ServerOperation::SetDefaultStart => {
            // The root helper's answer is the sentence: it names the entry
            // Tilo took the place of, or that it restored the order.
            let answer = ask_root(
                state_root,
                ControlRequest {
                    id: request_id(),
                    verb: "set-default-start".into(),
                    service: String::new(),
                    lines: 0,
                },
                wait,
            )
            .await?;
            if !answer.ok {
                return Err(answer.text);
            }
            ServerOperationResult {
                operation: "set-default-start".into(),
                done: true,
                summary: answer.text,
                answer: ServerAnswer::Nothing,
            }
        }
        ServerOperation::Drain => {
            let workspaces = workspaces.ok_or_else(|| {
                "this server keeps no admin workspace, so it has nothing to drain".to_owned()
            })?;
            let held =
                crate::rollout::ServerMachine::new(workspaces.clone(), state_root.to_path_buf())
                    .drain_now()
                    .await?;
            ServerOperationResult {
                operation: "drain".into(),
                done: true,
                summary: format!(
                    "suspended its Linux machines and handed off its work across {held} workspace(s)"
                ),
                answer: ServerAnswer::Nothing,
            }
        }
    };
    Ok(result)
}

/// Carry `request` out through the root helper and answer what it did.
async fn ran(
    state_root: &Path,
    request: ControlRequest,
    wait: Duration,
    operation: &str,
    done: String,
) -> Result<ServerOperationResult, String> {
    let answer = ask_root(state_root, request, wait).await?;
    if !answer.ok {
        return Err(answer.text);
    }
    Ok(ServerOperationResult {
        operation: operation.to_owned(),
        done: true,
        summary: done,
        answer: ServerAnswer::Nothing,
    })
}

/// Write `request` for the root helper without waiting for it: the
/// operation ends or changes a boot, so an answer taken now would not
/// outlive the machine going down.
fn asked(
    state_root: &Path,
    request: &ControlRequest,
    operation: &str,
    summary: String,
) -> Result<ServerOperationResult, String> {
    write_request(state_root, request)?;
    Ok(ServerOperationResult {
        operation: operation.to_owned(),
        done: false,
        summary,
        answer: ServerAnswer::Nothing,
    })
}

/// Write `request` and wait for the helper's answer to it.
async fn ask_root(
    state_root: &Path,
    request: ControlRequest,
    wait: Duration,
) -> Result<ControlAnswer, String> {
    write_request(state_root, &request)?;
    let path = state_root.join(CONTROL_RESULT);
    let id = request.id;
    let waited = tokio::time::timeout(wait, async move {
        loop {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Some(answer) = parse_answer(&text)
                && answer.id == id
            {
                return answer;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    waited.map_err(|_| "the server's root helper did not answer in time".to_owned())
}

fn write_request(state_root: &Path, request: &ControlRequest) -> Result<(), String> {
    tilo_files::native::atomic::write_with_mode(
        &state_root.join(CONTROL_REQUEST),
        request.document().as_bytes(),
        0o600,
    )
    .map_err(|error| error.to_string())
}

/// Read the helper's answer: `<id> <ok>` on the first line, then what it read.
fn parse_answer(text: &str) -> Option<ControlAnswer> {
    let (header, body) = text.split_once('\n').unwrap_or((text, ""));
    let mut fields = header.split_whitespace();
    let id = fields.next()?.to_owned();
    let ok = match fields.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    Some(ControlAnswer {
        id,
        ok,
        text: body.to_owned(),
    })
}

/// A systemd unit name, as the helper takes it: letters, digits and the
/// punctuation unit names use, with no path, gap or shell metacharacter.
fn valid_service(service: &str) -> Result<String, String> {
    let service = service.trim();
    let name = service.strip_suffix(".service").unwrap_or(service);
    let mut chars = name.chars();
    let named = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | ':'))
        && service.len() <= 128;
    if !named {
        return Err("a service is a systemd unit name".into());
    }
    Ok(if service.ends_with(".service") {
        service.to_owned()
    } else {
        format!("{service}.service")
    })
}

/// A request id that no earlier request shares, so a stale answer is never
/// taken for this one.
fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// What the server is, as its owner asks.
async fn status(
    state_root: &Path,
    workspaces: Option<&Workspaces>,
    own: &DeviceCert,
) -> ServerStatus {
    let (updates, unlock) = entry_settings(workspaces, own).await;
    let unlock = unlock_with_pending(unlock, state_root);
    ServerStatus {
        hostname: hostname(),
        uptime_seconds: uptime_seconds(),
        booted_image: bootc_field("booted-digest"),
        staged_image: bootc_field("staged-image"),
        updates,
        updates_paused: updates_paused(),
        pending_update: std::fs::read_to_string(state_root.join("update-pending"))
            .map(|target| target.trim().to_owned())
            .unwrap_or_default(),
        unlock,
        unlocked: opening(),
        holding: holding(workspaces).await,
        hardware: hardware(state_root),
    }
}

/// The planned unlock, naming the sibling unlock while the install key is
/// still waiting for first boot to spend it on that enrolment. The marker is
/// the installer's, and first boot removes it with the key.
fn unlock_with_pending(unlock: String, state_root: &Path) -> String {
    if !state_root.join("sibling-unlock-pending").is_file() {
        return unlock;
    }
    if unlock.is_empty() {
        "owner-device (sibling unlock pending)".to_owned()
    } else {
        format!("{unlock} (sibling unlock pending)")
    }
}

/// How this boot's system volume opened, as early boot left it: `None` when
/// this boot left no note, as one whose initrd wrote none does.
fn opening() -> Option<Opening> {
    opening_at(Path::new(OPENED_PATH))
}

/// [`opening`] at `path`.
fn opening_at(path: &Path) -> Option<Opening> {
    Opening::decode(&std::fs::read(path).ok()?)
}

/// The updates the server's entry says it follows and how it unlocks; both
/// empty when it has no entry here.
async fn entry_settings(workspaces: Option<&Workspaces>, own: &DeviceCert) -> (String, String) {
    let none = (String::new(), String::new());
    let Some(workspaces) = workspaces else {
        return none;
    };
    let Ok(servers) = crate::servers::read(workspaces).await else {
        return none;
    };
    servers
        .active(&own.device_public_key)
        .map_or(none, |server| {
            (server.updates.clone(), server.unlock.clone())
        })
}

/// The workspaces this server holds, and how each one's copies stand.
async fn holding(workspaces: Option<&Workspaces>) -> Vec<HeldWorkspace> {
    let Some(workspaces) = workspaces else {
        return Vec::new();
    };
    let mut held = Vec::new();
    for summary in workspaces.list().await {
        let Ok(copies) = workspaces.copies(&summary.workspace).await else {
            continue;
        };
        held.push(HeldWorkspace {
            workspace: summary.workspace,
            name: summary.name,
            holders: copies
                .holders
                .into_iter()
                .map(|holder| HeldCopy {
                    device: hex::encode(holder.device),
                    role: match holder.role {
                        tilo_peer_state::CopyRole::KeyHolder => "key-holder".into(),
                        tilo_peer_state::CopyRole::Store => "store".into(),
                    },
                    current: holder.current,
                })
                .collect(),
            shortfall: copies
                .shortfall
                .into_iter()
                .map(|short| match short {
                    tilo_peer_state::Shortfall::Holder { device, .. } => {
                        format!("{} is catching up", hex::encode(device))
                    }
                    tilo_peer_state::Shortfall::Copies {
                        role,
                        wanted,
                        current,
                    } => format!(
                        "wants {wanted} current {} and keeps {current}",
                        match role {
                            tilo_peer_state::CopyRole::KeyHolder => "key holders",
                            tilo_peer_state::CopyRole::Store => "stores",
                        }
                    ),
                })
                .collect(),
        });
    }
    held
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

fn uptime_seconds() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|uptime| {
            uptime
                .split_whitespace()
                .next()
                .and_then(|seconds| seconds.split('.').next()?.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

/// A field of `bootc status`, as `tilo-image-trust` reads it: empty when this
/// server cannot read its own boot status unprivileged.
fn bootc_field(field: &str) -> String {
    let status = std::process::Command::new("bootc")
        .args(["status", "--format", "json"])
        .output();
    let Ok(status) = status else {
        return String::new();
    };
    if !status.status.success() {
        return String::new();
    }
    let Ok(mut child) = std::process::Command::new("/usr/bin/tilo-image-trust")
        .args(["bootc-status-field", field])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    else {
        return String::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(&status.stdout);
    }
    child
        .wait_with_output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// Whether the timer that stages OS updates is masked, as
/// `PauseUpdates` leaves it.
fn updates_paused() -> bool {
    if !Path::new("/run/systemd/system").exists() {
        return false;
    }
    std::process::Command::new("systemctl")
        .args(["is-enabled", "bootc-fetch-apply-updates.timer"])
        .output()
        .ok()
        .is_some_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "masked")
}

fn hardware(state_root: &Path) -> ServerHardware {
    let (storage_free_gb, storage_total_gb) = host_fs::space(state_root).map_or((0, 0), |space| {
        (
            space.available / 1_000_000_000,
            space.capacity / 1_000_000_000,
        )
    });
    ServerHardware {
        product: read_first(&["/sys/class/dmi/id/product_name", "/proc/device-tree/model"]),
        cpu_model: cpu_model(),
        cpus: std::thread::available_parallelism()
            .map_or(0, |cpus| u32::try_from(cpus.get()).unwrap_or(0)),
        memory_mb: memory_mb(),
        storage_free_gb,
        storage_total_gb,
    }
}

fn read_first(paths: &[&str]) -> String {
    paths
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().trim_end_matches('\0').to_owned())
        .unwrap_or_default()
}

fn cpu_model() -> String {
    let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") else {
        return String::new();
    };
    cpuinfo
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            matches!(key.trim(), "model name" | "Model" | "cpu model")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

fn memory_mb() -> u64 {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    meminfo
        .lines()
        .find_map(|line| {
            let rest = line.strip_prefix("MemTotal:")?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
        .map_or(0, |kilobytes| kilobytes / 1024)
}

#[cfg(test)]
mod tests;
