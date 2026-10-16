//! Pairing a nearby device with the person's identity, run by the host.
//!
//! Discovery listens on the LAN for an unprovisioned device announcing
//! itself (`tilo-enroll discover[-device]`) and answers what it found,
//! with the verification code both screens show. Approval runs the
//! polkit-gated helper (`tilo-home-peer-control approve-…`), which asks an
//! administrator to authenticate before it admits the device. The helper and
//! its policy are unchanged; only their caller is the host now, so no
//! sandboxed application starts a privileged process.
//!
//! Both steps run on a worker thread and answer through a
//! [`Pending`], so whoever waits — the compositor loop for the palette and
//! the control socket, the action broker for an application — never blocks
//! on a LAN window that may stay open for minutes. One step runs at a time:
//! discovery and approval listen on the same LAN port.
//!
//! An approval names a device this host discovered, with the relay service
//! it announced: a caller cannot admit a key it merely names. Rejecting
//! forgets the candidate and stops waiting on its approval; the helper's own
//! LAN window bounds the privileged child.
//!
//! Who may pair is decided by the door the step arrives through, before it
//! reaches this engine: only someone who administers the workspace's
//! identity, and approval needs explicit confirmation.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_lock::MutexRecover as _;

const ENROLL: &str = "/usr/bin/tilo-enroll";
const PKEXEC: &str = "/usr/bin/pkexec";
const CONTROL_HELPER: &str = "/usr/libexec/tilo-home-peer-control";
const RELAY_CONFIG: &str = "/etc/tilo/relay.toml";
/// How long `tilo-enroll` listens for an announcing device.
const LISTEN_SECONDS: &str = "15";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// The helper opens its own LAN window and waits for the administrator's
/// authentication; this only bounds a child that never ends.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
const OUTPUT_LIMIT: usize = 1024 * 1024;
/// How often a running step checks whether it was rejected.
const STOP_POLL: Duration = Duration::from_millis(100);

/// What a step answers: its result, or why it did not happen.
pub(crate) type Answer = Result<serde_json::Value, String>;

/// The resource every door offers the steps on, and its three actions.
pub(crate) use tilo_actions::remote::device_pairing::{APPROVE, DISCOVER, REJECT, RESOURCE};
pub(crate) const STEPS: [&str; 3] = [DISCOVER, APPROVE, REJECT];

const ROLE: ActionParam = ActionParam::required(
    "role",
    ParamType::Text,
    "personal-device for another of your devices, home-peer for an always-on home server.",
);
const DEVICE: ActionParam = ActionParam::required(
    "device",
    ParamType::Text,
    "The discovered device's key: the `device` discovery answered, 64 hex characters.",
);

pub(crate) const DISCOVER_PARAMS: &[ActionParam] = &[ROLE];
pub(crate) const APPROVE_PARAMS: &[ActionParam] = &[
    ROLE,
    DEVICE,
    ActionParam::required(
        "service",
        ParamType::Text,
        "The relay service node id discovery answered with the device, 64 hex characters.",
    ),
    ActionParam::optional(
        "workspace_ids",
        ParamType::Array,
        "For a home peer, the ids of the workspaces it keeps. Defaults to the relay configuration's.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that both devices show the same verification code and this one may join.",
    ),
];
pub(crate) const REJECT_PARAMS: &[ActionParam] = &[DEVICE];

/// What `step` takes.
pub(crate) fn params(step: &str) -> &'static [ActionParam] {
    match step {
        DISCOVER => DISCOVER_PARAMS,
        APPROVE => APPROVE_PARAMS,
        _ => REJECT_PARAMS,
    }
}

/// What `step` may do and answers, the same through every door. Approval
/// admits a device to the person's identity through an administrator's
/// authentication: privileged, and asked first.
pub(crate) fn contract(step: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{
        ActionContract, ActionKind, ConfirmationPolicy, ExecutionMode, ResultSpec, RetryPolicy,
        RiskLevel,
    };
    // Pairing listens on this machine's LAN and asks its administrator: it
    // runs for the person at this device, never for a remote participant.
    let base = ActionContract {
        kind: ActionKind::Outside,
        retry: RetryPolicy::AtMostOnce,
        tags: vec![tilo_actions::LOCAL_DEVICE_ONLY.into()],
        ..ActionContract::default()
    };
    match step {
        DISCOVER => ActionContract {
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["role", "device", "service", "label", "verification_code"],
                    "properties": {
                        "role": { "type": "string", "enum": ["personal-device", "home-peer"] },
                        "device": { "type": "string" },
                        "service": { "type": "string" },
                        "label": { "type": "string" },
                        "verification_code": { "type": "string" }
                    }
                }),
            },
            ..base
        },
        APPROVE => ActionContract {
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["role", "device", "label"],
                    "properties": {
                        "role": { "type": "string", "enum": ["personal-device", "home-peer"] },
                        "device": { "type": "string" },
                        "label": { "type": "string" }
                    }
                }),
            },
            ..base
        },
        _ => ActionContract {
            result: ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["device", "forgotten", "stopped"],
                    "properties": {
                        "device": { "type": "string" },
                        "forgotten": { "type": "boolean" },
                        "stopped": { "type": "boolean" }
                    }
                }),
            },
            ..base
        },
    }
}

/// How a step started: answered at once, or answering later.
pub(crate) enum Started {
    Answered(serde_json::Value),
    Later(Pending<Answer>),
}

/// The kind of device a pairing admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PairRole {
    /// Another of the person's own devices.
    PersonalDevice,
    /// An always-on home server that keeps the person's workspaces.
    HomePeer,
}

impl PairRole {
    /// The role `text` names: `personal-device` or `home-peer`.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "personal-device" => Ok(Self::PersonalDevice),
            "home-peer" => Ok(Self::HomePeer),
            _ => Err("role must be personal-device or home-peer".into()),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalDevice => "personal-device",
            Self::HomePeer => "home-peer",
        }
    }

    const fn discover_command(self) -> &'static str {
        match self {
            Self::PersonalDevice => "discover-device",
            Self::HomePeer => "discover",
        }
    }

    const fn approve_command(self) -> &'static str {
        match self {
            Self::PersonalDevice => "approve-device",
            Self::HomePeer => "approve-enrollment",
        }
    }
}

/// A device found nearby, as `tilo-enroll discover` reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
struct Candidate {
    device_node_id: String,
    relay_service_node_id: String,
    label: String,
    verification_code: String,
}

impl Candidate {
    fn answer(&self, role: PairRole) -> serde_json::Value {
        serde_json::json!({
            "role": role.as_str(),
            "device": self.device_node_id,
            "service": self.relay_service_node_id,
            "label": self.label,
            "verification_code": self.verification_code,
        })
    }
}

/// One program the host runs for a pairing step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostCommand {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) timeout: Duration,
}

/// How a [`HostCommand`] ended.
#[derive(Clone, Debug, Default)]
pub(crate) struct Finished {
    pub(crate) success: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Runs a [`HostCommand`] to its end on the calling worker thread, giving
/// up (and stopping the child) once `stop` is raised.
pub(crate) type Runner =
    Arc<dyn Fn(&HostCommand, &AtomicBool) -> Result<Finished, String> + Send + Sync>;

/// An approval of a device discovered here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Approval {
    pub(crate) role: PairRole,
    /// The device's key, in lowercase hex.
    pub(crate) device: String,
    /// The relay service node id it announced, in lowercase hex.
    pub(crate) service: String,
    /// For a home peer, the workspaces it keeps; `None` takes the relay
    /// configuration's.
    pub(crate) workspace_ids: Option<Vec<u64>>,
}

/// Where the identity an approved device joins lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IdentityPaths {
    /// The device root (`/var/lib/tilo` on an installed system).
    pub(crate) root: PathBuf,
    /// The person's identity directory.
    pub(crate) dir: PathBuf,
}

impl IdentityPaths {
    /// The identity this session runs as: `$TILO_IDENTITY_ROOT`, else the
    /// system device root when it exists, else the person's data directory;
    /// and the person's identity directory.
    pub(crate) fn from_env() -> Result<Self, String> {
        let root = std::env::var_os("TILO_IDENTITY_ROOT").map_or_else(
            || {
                if Path::new("/var/lib/tilo/device").is_dir() {
                    PathBuf::from("/var/lib/tilo")
                } else {
                    tilo_paths::data_dir()
                        .unwrap_or_else(|| PathBuf::from(".local/share"))
                        .join("tilo")
                }
            },
            PathBuf::from,
        );
        let dir = runtime_identity::identity_dir_from_env()
            .map_err(|error| format!("could not find your identity: {error}"))?;
        Ok(Self { root, dir })
    }
}

/// A device key or relay service node id: 32 bytes as hex, lowercased.
pub(crate) fn node_id(text: &str, what: &str) -> Result<String, String> {
    let text = text.trim();
    if text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(format!("{what} must be 64 hexadecimal characters"))
    }
}

#[derive(Default)]
struct Flights {
    discovering: bool,
    /// What discovery found, by device key, until approved or rejected.
    candidates: HashMap<String, (PairRole, Candidate)>,
    /// The device whose approval runs, and what stops waiting on it.
    approving: Option<(String, Arc<AtomicBool>)>,
}

/// The host's pairing steps and what they have found.
pub(crate) struct Pairing {
    runner: Runner,
    flights: Arc<Mutex<Flights>>,
}

static SESSION: LazyLock<Pairing> = LazyLock::new(|| Pairing::with_runner(Arc::new(run_bounded)));

impl Pairing {
    /// This session's pairing: what every door shares, so a device one
    /// door discovered is approved or rejected through another.
    pub(crate) fn session() -> &'static Self {
        &SESSION
    }

    /// Pairing whose commands `runner` runs.
    pub(crate) fn with_runner(runner: Runner) -> Self {
        Self {
            runner,
            flights: Arc::default(),
        }
    }

    fn flights(&self) -> MutexGuard<'_, Flights> {
        self.flights.lock_recover()
    }

    /// Look for a nearby device of `role`; the answer is the device found,
    /// remembered for approval.
    pub(crate) fn discover(&self, role: PairRole) -> Result<Pending<Answer>, String> {
        {
            let mut flights = self.flights();
            refuse_busy(&flights)?;
            flights.discovering = true;
        }
        let command = HostCommand {
            program: PathBuf::from(ENROLL),
            args: [role.discover_command(), "--timeout", LISTEN_SECONDS]
                .into_iter()
                .map(OsString::from)
                .collect(),
            timeout: DISCOVERY_TIMEOUT,
        };
        let (completer, answer) = pending();
        let runner = Arc::clone(&self.runner);
        let flights = Arc::clone(&self.flights);
        let started = spawn_step(move || {
            let found = runner(&command, &AtomicBool::new(false))
                .and_then(|finished| discovered(&finished));
            let mut flights = flights.lock_recover();
            flights.discovering = false;
            let answer = found.map(|candidate| {
                let value = candidate.answer(role);
                flights
                    .candidates
                    .insert(candidate.device_node_id.clone(), (role, candidate));
                value
            });
            drop(flights);
            completer.complete(answer);
        });
        if let Err(error) = started {
            self.flights().discovering = false;
            return Err(error);
        }
        Ok(answer)
    }

    /// Admit the device `approval` names, which discovery found here, to
    /// the identity at `paths`. The answer arrives once the helper ends:
    /// after the administrator authenticated and the device joined, or
    /// with why it did not.
    pub(crate) fn approve(
        &self,
        approval: &Approval,
        paths: &IdentityPaths,
    ) -> Result<Pending<Answer>, String> {
        let workspace_ids = match approval.role {
            PairRole::HomePeer => approval
                .workspace_ids
                .clone()
                .unwrap_or_else(|| relay_workspaces(Path::new(RELAY_CONFIG))),
            PairRole::PersonalDevice => Vec::new(),
        };
        let (label, stop) = {
            let mut flights = self.flights();
            refuse_busy(&flights)?;
            let (role, candidate) = flights.candidates.get(&approval.device).ok_or_else(|| {
                "no device with that key was discovered here; discover it first".to_string()
            })?;
            if *role != approval.role || candidate.relay_service_node_id != approval.service {
                return Err("that is not the device discovered with this key".into());
            }
            let label = candidate.label.clone();
            let stop = Arc::new(AtomicBool::new(false));
            flights.approving = Some((approval.device.clone(), Arc::clone(&stop)));
            drop(flights);
            (label, stop)
        };
        let command = approval_command(approval, paths, &workspace_ids);
        let (completer, answer) = pending();
        let runner = Arc::clone(&self.runner);
        let flights = Arc::clone(&self.flights);
        let device = approval.device.clone();
        let role = approval.role;
        let started = spawn_step(move || {
            let ran = runner(&command, &stop);
            let mut flights = flights.lock_recover();
            flights.approving = None;
            let answer = if stop.load(Ordering::Acquire) {
                Err("the approval was rejected".to_string())
            } else {
                match ran {
                    Ok(finished) if finished.success => {
                        flights.candidates.remove(&device);
                        Ok(serde_json::json!({
                            "role": role.as_str(),
                            "device": device,
                            "label": label,
                        }))
                    }
                    Ok(finished) => Err(format!(
                        "the device was not paired: {}",
                        command_error(
                            &finished.stderr,
                            "the pairing window closed or administrator approval was not given",
                        )
                    )),
                    Err(error) => Err(error),
                }
            };
            drop(flights);
            completer.complete(answer);
        });
        if let Err(error) = started {
            self.flights().approving = None;
            return Err(error);
        }
        Ok(answer)
    }

    /// Start `step` with `args`, which the door bound to [`params`] after
    /// it decided the caller may pair.
    pub(crate) fn start(&self, step: &str, args: &ActionArgs) -> Result<Started, String> {
        match step {
            DISCOVER => {
                let role = PairRole::parse(args.text("role").unwrap_or_default())?;
                self.discover(role).map(Started::Later)
            }
            APPROVE => {
                if args.boolean("confirm") != Some(true) {
                    return Err("explicit confirmation is required to approve this device".into());
                }
                let approval = approval(args)?;
                let paths = IdentityPaths::from_env()?;
                self.approve(&approval, &paths).map(Started::Later)
            }
            REJECT => self
                .reject(args.text("device").unwrap_or_default())
                .map(Started::Answered),
            _ => Err(format!("`{step}` is not a pairing step")),
        }
    }

    /// Forget the discovered device `device` and stop waiting on its
    /// approval, if one runs.
    pub(crate) fn reject(&self, device: &str) -> Answer {
        let device = node_id(device, "device")?;
        let mut flights = self.flights();
        let forgotten = flights.candidates.remove(&device).is_some();
        let stopped = flights
            .approving
            .as_ref()
            .filter(|(approving, _)| *approving == device)
            .map(|(_, stop)| stop.store(true, Ordering::Release))
            .is_some();
        drop(flights);
        if !forgotten && !stopped {
            return Err("no device with that key was discovered or is being approved".into());
        }
        Ok(serde_json::json!({
            "device": device,
            "forgotten": forgotten,
            "stopped": stopped,
        }))
    }
}

/// The approval `args` ask for.
pub(crate) fn approval(args: &ActionArgs) -> Result<Approval, String> {
    let role = PairRole::parse(args.text("role").unwrap_or_default())?;
    let workspace_ids = args
        .array("workspace_ids")
        .map(|ids| {
            ids.iter()
                .map(|id| {
                    id.as_u64()
                        .ok_or_else(|| "workspace_ids must be non-negative integers".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    Ok(Approval {
        role,
        device: node_id(args.text("device").unwrap_or_default(), "device")?,
        service: node_id(args.text("service").unwrap_or_default(), "service")?,
        workspace_ids,
    })
}

fn refuse_busy(flights: &Flights) -> Result<(), String> {
    if flights.discovering {
        Err("nearby discovery is already running".into())
    } else if flights.approving.is_some() {
        Err("a device is being approved; wait for it or reject it first".into())
    } else {
        Ok(())
    }
}

fn spawn_step(step: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name("tilo-device-pairing".into())
        .spawn(step)
        .map(drop)
        .map_err(|error| format!("could not start pairing: {error}"))
}

/// The helper invocation that admits `approval`'s device: the same
/// arguments Settings passed when it ran the helper itself.
pub(crate) fn approval_command(
    approval: &Approval,
    paths: &IdentityPaths,
    workspace_ids: &[u64],
) -> HostCommand {
    let mut args: Vec<OsString> = [CONTROL_HELPER, approval.role.approve_command()]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push("--identity-root".into());
    args.push(paths.root.clone().into_os_string());
    args.push("--identity-dir".into());
    args.push(paths.dir.clone().into_os_string());
    args.push("--expected-device".into());
    args.push(approval.device.clone().into());
    args.push("--expected-service".into());
    args.push(approval.service.clone().into());
    if approval.role == PairRole::HomePeer {
        for id in workspace_ids {
            args.push("--workspace-id".into());
            args.push(id.to_string().into());
        }
    }
    HostCommand {
        program: PathBuf::from(PKEXEC),
        args,
        timeout: APPROVAL_TIMEOUT,
    }
}

/// The workspaces the relay configuration at `path` keeps; none when it
/// cannot be read.
fn relay_workspaces(path: &Path) -> Vec<u64> {
    tilo_relay_protocol::RelayConfig::load(path)
        .map(|config| config.workspaces.iter().map(|binding| binding.id).collect())
        .unwrap_or_default()
}

/// The device a finished discovery reports.
fn discovered(finished: &Finished) -> Result<Candidate, String> {
    if !finished.success {
        return Err(command_error(
            &finished.stderr,
            "no new device was found nearby",
        ));
    }
    let candidate: Candidate = serde_json::from_slice(&finished.stdout)
        .map_err(|_| "nearby discovery returned an invalid identity".to_string())?;
    Ok(Candidate {
        device_node_id: node_id(&candidate.device_node_id, "the discovered device")?,
        relay_service_node_id: node_id(
            &candidate.relay_service_node_id,
            "the discovered relay service",
        )?,
        ..candidate
    })
}

/// A child's diagnostic, safe to show a person, or `fallback` when it said
/// nothing.
pub(crate) fn command_error(stderr: &[u8], fallback: &str) -> String {
    let detail = String::from_utf8_lossy(stderr);
    let detail: String = detail
        .trim()
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(512)
        .collect();
    if detail.is_empty() {
        fallback.to_owned()
    } else {
        detail
    }
}

/// Run `command` under the bounded-command policy, polling for `stop`;
/// dropping the child when stopped cancels its process group.
pub(crate) fn run_bounded(command: &HostCommand, stop: &AtomicBool) -> Result<Finished, String> {
    let mut prepared = std::process::Command::new(&command.program);
    prepared.args(&command.args);
    let mut child = bounded_command::spawn_prepared(prepared, None, OUTPUT_LIMIT, command.timeout)
        .map_err(|error| format!("could not start {}: {error}", command.program.display()))?;
    loop {
        if stop.load(Ordering::Acquire) {
            return Err("stopped".into());
        }
        if let Some(result) = child.try_wait() {
            let output = result.map_err(|error| error.to_string())?;
            return Ok(Finished {
                success: output.status.success(),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        std::thread::sleep(STOP_POLL);
    }
}

/// The application whose Devices & Sync controls pair devices through the
/// broker's door.
const SETTINGS_APP_ID: &str = "so.tilo.settings";

/// The broker's door to pairing: how Settings pairs a device, as the person
/// using it, without starting any process itself.
pub(crate) struct BrokerDoor {
    pub(crate) tree: crate::shell::state::WeakWorkspaceTreeHandle,
    pub(crate) pairing: &'static Pairing,
}

impl BrokerDoor {
    /// Refuse unless `person` administers `room`'s identity: they may grant
    /// in the workspace, and it has an identity selected.
    fn authorize(&self, room: &str, person: &str) -> Result<(), String> {
        crate::shell::identity_admin::authorize(&self.tree, room, person).map(drop)
    }
}

/// `step`, described for the broker's catalogue.
fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use crate::shell::i18n::t;
    let (label, description) = match step {
        DISCOVER => (
            "search-action-identity-device-pair-discover",
            "search-action-identity-device-pair-discover-subtitle",
        ),
        APPROVE => (
            "search-action-identity-device-pair-approve",
            "search-action-identity-device-pair-approve-subtitle",
        ),
        _ => (
            "search-action-identity-device-pair-reject",
            "search-action-identity-device-pair-reject-subtitle",
        ),
    };
    tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, step),
        resource_kind: RESOURCE.into(),
        resource_label: t("search-action-identity-device-pair-resource"),
        action_label: t(label),
        description: Some(t(description)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: contract(step),
        input: tilo_actions::InputSpec::None,
        params: params(step).to_vec(),
        view: None,
    }
}

/// `argument`, in its slot form, bound to `params`.
fn bind_slot(params: &[ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::params::ARGUMENTS_JSON_PREFIX)
                .ok_or("pairing steps take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

fn outcome(answer: Answer) -> tilo_action_broker::services::DevicePairingOutcome {
    answer
        .map(|value| tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|message| {
            Box::new(tilo_actions::ActionFailure {
                code: "pairing_failed".into(),
                message,
                details: None,
                retryable: true,
            })
        })
}

impl tilo_action_broker::services::DevicePairingServices for BrokerDoor {
    fn app_id(&self) -> &str {
        SETTINGS_APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::DevicePairingRequest<'_>,
    ) -> Result<Pending<tilo_action_broker::services::DevicePairingOutcome>, String> {
        // Admitting a device to a person's identity is that person's own
        // act: an agent or an app acting for nobody never pairs through here.
        let person = request
            .actor
            .direct_person()
            .ok_or("only a person pairs a device")?;
        let step = STEPS
            .into_iter()
            .find(|step| request.address.resource == RESOURCE && request.address.action == *step)
            .ok_or("unknown pairing step")?;
        let args = bind_slot(params(step), request.argument)?;
        self.authorize(request.room, person)?;
        Ok(match self.pairing.start(step, &args)? {
            Started::Answered(value) => Pending::ready(outcome(Ok(value))),
            Started::Later(answer) => answer.map(outcome),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const DEVICE: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const SERVICE: &str = "bb00000000000000000000000000000000000000000000000000000000000002";

    fn paths() -> IdentityPaths {
        IdentityPaths {
            root: PathBuf::from("/var/lib/tilo"),
            dir: PathBuf::from("/home/person/.config/tilo/identity"),
        }
    }

    fn found() -> Finished {
        Finished {
            success: true,
            stdout: format!(
                "{{\"device_node_id\":\"{}\",\"relay_service_node_id\":\"{SERVICE}\",\
                 \"label\":\"Kitchen NAS\",\"verification_code\":\"123-456\",\"kind\":\"HomePeer\"}}\n",
                DEVICE.to_ascii_uppercase()
            )
            .into_bytes(),
            stderr: Vec::new(),
        }
    }

    /// A runner that records each command and answers from `script`;
    /// an approval waits until it is stopped or released.
    fn scripted(
        commands: mpsc::Sender<HostCommand>,
        release: Arc<Mutex<mpsc::Receiver<bool>>>,
    ) -> Runner {
        Arc::new(move |command, stop| {
            commands.send(command.clone()).expect("recorded");
            if command.program == Path::new(ENROLL) {
                return Ok(found());
            }
            loop {
                if stop.load(Ordering::Acquire) {
                    return Err("stopped".into());
                }
                let released = lock_receiver(&release).try_recv();
                if let Ok(success) = released {
                    return Ok(Finished {
                        success,
                        stderr: if success {
                            Vec::new()
                        } else {
                            b"Not authorized\n".to_vec()
                        },
                        ..Finished::default()
                    });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    }

    fn lock_receiver(
        receiver: &Mutex<mpsc::Receiver<bool>>,
    ) -> MutexGuard<'_, mpsc::Receiver<bool>> {
        receiver.lock_recover()
    }

    /// The step's answer; a step that never answers reads as that refusal,
    /// which no assertion below expects.
    fn wait(answer: Pending<Answer>) -> Answer {
        answer
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .unwrap_or_else(|| Err("the step never answered".into()))
    }

    #[test]
    fn discovery_answers_and_remembers_the_device_found_and_runs_one_step_at_a_time() {
        let (commands, recorded) = mpsc::channel();
        let (_release, gate) = mpsc::channel();
        let pairing = Pairing::with_runner(scripted(commands, Arc::new(Mutex::new(gate))));

        let answer = wait(pairing.discover(PairRole::HomePeer).expect("starts"))
            .expect("a device was found");
        assert_eq!(
            answer,
            serde_json::json!({
                "role": "home-peer",
                "device": DEVICE,
                "service": SERVICE,
                "label": "Kitchen NAS",
                "verification_code": "123-456",
            }),
            "the key is normalised to lowercase"
        );
        assert_eq!(
            recorded.recv().expect("ran").args,
            ["discover", "--timeout", "15"].map(OsString::from).to_vec()
        );

        let wrong_role = Approval {
            role: PairRole::PersonalDevice,
            device: DEVICE.into(),
            service: SERVICE.into(),
            workspace_ids: None,
        };
        assert!(
            pairing.approve(&wrong_role, &paths()).is_err(),
            "a home server is not approved as a personal device"
        );
        let unknown = Approval {
            role: PairRole::HomePeer,
            device: SERVICE.into(),
            service: SERVICE.into(),
            workspace_ids: Some(Vec::new()),
        };
        assert!(
            pairing.approve(&unknown, &paths()).is_err(),
            "a key nobody discovered is never approved"
        );

        let approval = Approval {
            role: PairRole::HomePeer,
            device: DEVICE.into(),
            service: SERVICE.into(),
            workspace_ids: Some(vec![7, 9]),
        };
        let running = pairing.approve(&approval, &paths()).expect("starts");
        assert!(
            pairing.discover(PairRole::HomePeer).is_err(),
            "discovery waits while an approval holds the LAN port"
        );
        assert!(
            pairing.approve(&approval, &paths()).is_err(),
            "single flight"
        );
        let rejected = pairing.reject(DEVICE).expect("rejects");
        assert_eq!(rejected["stopped"], true);
        assert_eq!(rejected["forgotten"], true);
        assert_eq!(
            wait(running),
            Err("the approval was rejected".into()),
            "rejecting stops waiting on the approval"
        );
        assert!(
            pairing.approve(&approval, &paths()).is_err(),
            "a rejected device is forgotten"
        );
        assert!(pairing.reject(DEVICE).is_err(), "nothing left to reject");
    }

    #[test]
    fn approval_runs_the_polkit_helper_with_the_arguments_settings_passed() {
        let (commands, recorded) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let pairing = Pairing::with_runner(scripted(commands, Arc::new(Mutex::new(gate))));
        wait(pairing.discover(PairRole::HomePeer).expect("starts")).expect("found");
        recorded.recv().expect("discovery ran");

        let approval = Approval {
            role: PairRole::HomePeer,
            device: DEVICE.into(),
            service: SERVICE.into(),
            workspace_ids: Some(vec![7, 9]),
        };
        let first = pairing.approve(&approval, &paths()).expect("starts");
        let command = recorded.recv().expect("approval ran");
        assert_eq!(command.program, PathBuf::from(PKEXEC));
        assert_eq!(command.timeout, APPROVAL_TIMEOUT);
        assert_eq!(
            command.args,
            [
                CONTROL_HELPER,
                "approve-enrollment",
                "--identity-root",
                "/var/lib/tilo",
                "--identity-dir",
                "/home/person/.config/tilo/identity",
                "--expected-device",
                DEVICE,
                "--expected-service",
                SERVICE,
                "--workspace-id",
                "7",
                "--workspace-id",
                "9",
            ]
            .map(OsString::from)
            .to_vec()
        );
        release.send(false).expect("released");
        assert_eq!(
            wait(first),
            Err("the device was not paired: Not authorized".into()),
            "a refused authentication says why and keeps the candidate"
        );

        let second = pairing.approve(&approval, &paths()).expect("retries");
        recorded.recv().expect("approval ran again");
        release.send(true).expect("released");
        assert_eq!(
            wait(second).expect("paired"),
            serde_json::json!({"role": "home-peer", "device": DEVICE, "label": "Kitchen NAS"})
        );
        assert!(
            pairing.reject(DEVICE).is_err(),
            "a paired device is no longer a candidate"
        );

        let personal = approval_command(
            &Approval {
                role: PairRole::PersonalDevice,
                ..approval
            },
            &paths(),
            &[7],
        );
        assert_eq!(
            personal.args.get(1),
            Some(&OsString::from("approve-device"))
        );
        assert!(
            !personal.args.contains(&OsString::from("--workspace-id")),
            "a personal device keeps no workspaces of its own"
        );
    }

    #[test]
    fn keys_and_roles_are_checked_before_anything_runs() {
        assert_eq!(PairRole::parse("home-peer"), Ok(PairRole::HomePeer));
        assert_eq!(
            PairRole::parse(" personal-device "),
            Ok(PairRole::PersonalDevice)
        );
        assert!(PairRole::parse("server").is_err());
        assert_eq!(
            node_id(&DEVICE.to_ascii_uppercase(), "device"),
            Ok(DEVICE.into())
        );
        assert!(node_id("abc", "device").is_err());
        assert!(node_id(&"g".repeat(64), "device").is_err());
        let refused = discovered(&Finished {
            success: false,
            stdout: Vec::new(),
            stderr: b"\x1b[31mno matching device\n".to_vec(),
        });
        assert_eq!(refused, Err("[31mno matching device".into()));
        assert!(
            discovered(&Finished {
                success: true,
                stdout: b"{\"device_node_id\":\"x\"}".to_vec(),
                stderr: Vec::new(),
            })
            .is_err()
        );
        assert!(relay_workspaces(Path::new("/nonexistent/relay.toml")).is_empty());
    }
}
