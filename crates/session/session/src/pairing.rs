//! Pairing a nearby device with the person's identity, run by the session.
//! Discovery listens on the LAN for an unprovisioned device announcing itself (`tilo-enroll
//! discover[-device]`) and answers what it found, with the verification code both screens show.
//! Approval runs the polkit-gated helper (`tilo-home-peer-control approve-…`), which asks an
//! administrator to authenticate before it admits the device. The helper and its policy are
//! unchanged; only their caller is the session now, so no sandboxed application starts a privileged
//! process.
//! Both steps run on a worker thread and answer through a [`Pending`], so whoever waits — the
//! session's loop and the action broker for an application — never blocks on a LAN window that may
//! stay open for minutes. One step runs at a time: discovery and approval listen on the same LAN
//! port.
//! An approval names a device this session discovered, with the relay service it announced: a
//! caller cannot admit a key it merely names. Rejecting forgets the candidate and stops waiting on
//! its approval; the helper's own LAN window bounds the privileged child.
//! Who may pair is decided by the door the step arrives through, before it reaches this engine:
//! only someone who administers the workspace's identity, and approval needs explicit confirmation.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use crate::platform::fabric::SharedLink;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use tilo_identity_host::device_admin::{
    Approval, Candidate, ENROLL, KeyHolding, PairRole, PersonPaths, approval_args, command_error,
    node_id,
};
use tilo_lock::MutexRecover as _;

const PKEXEC: &str = "/usr/bin/pkexec";
/// How long `tilo-enroll` listens for an announcing device.
const LISTEN_SECONDS: &str = "15";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// The helper opens its own LAN window and waits for the administrator's authentication; this only
/// bounds a child that never ends.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
const OUTPUT_LIMIT: usize = 1024 * 1024;
/// How often a running step checks whether it was rejected.
const STOP_POLL: Duration = Duration::from_millis(100);

/// What a step answers: its result, or why it did not happen.
pub(crate) type Answer = Result<serde_json::Value, String>;

/// The resource every door offers the steps on, and its actions.
pub(crate) use tilo_actions::remote::device_pairing::{
    ANSWER, APPROVE, ASKING, DISCOVER, REJECT, RESOURCE,
};
pub(crate) const STEPS: [&str; 5] = [DISCOVER, APPROVE, REJECT, ASKING, ANSWER];

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
pub(crate) const ASKING_PARAMS: &[ActionParam] = &[];
pub(crate) const ANSWER_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "ask",
        ParamType::Text,
        "The `ask` the asking step listed the device under, 64 hex characters.",
    ),
    ActionParam::required(
        "approve",
        ParamType::Boolean,
        "true to let the device be you, false to decline it.",
    ),
    ActionParam::optional(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation, needed to approve, that the new device shows the code listed with it.",
    ),
];

/// What `step` takes.
pub(crate) fn params(step: &str) -> &'static [ActionParam] {
    match step {
        DISCOVER => DISCOVER_PARAMS,
        APPROVE => APPROVE_PARAMS,
        ASKING => ASKING_PARAMS,
        ANSWER => ANSWER_PARAMS,
        _ => REJECT_PARAMS,
    }
}

/// What `step` may do and answers, the same through every door. Approval admits a device to the
/// person's identity through an administrator's authentication: privileged, and asked first.
pub(crate) fn contract(step: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{
        ActionContract, ActionKind, ConfirmationPolicy, ExecutionMode, ResultSpec, RetryPolicy,
        RiskLevel,
    };
    // Pairing listens on this machine's LAN and asks its administrator.
    let base = ActionContract {
        kind: ActionKind::Outside,
        retry: RetryPolicy::AtMostOnce,
        result: ResultSpec::Json {
            schema: result_schema(step),
        },
        ..ActionContract::default()
    };
    match step {
        DISCOVER => ActionContract {
            execution: ExecutionMode::Background { cancellable: false },
            ..base
        },
        APPROVE | ANSWER => ActionContract {
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            execution: ExecutionMode::Background { cancellable: false },
            ..base
        },
        ASKING => ActionContract {
            kind: ActionKind::Read,
            ..base
        },
        _ => base,
    }
}

/// The JSON `step` answers with.
fn result_schema(step: &str) -> serde_json::Value {
    match step {
        DISCOVER => serde_json::json!({
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
        APPROVE => serde_json::json!({
            "type": "object",
            "required": ["role", "device", "label"],
            "properties": {
                "role": { "type": "string", "enum": ["personal-device", "home-peer"] },
                "device": { "type": "string" },
                "label": { "type": "string" }
            }
        }),
        ASKING => serde_json::json!({
            "type": "object",
            "required": ["asking"],
            "properties": {
                "asking": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["ask", "device", "label", "code", "via", "asked_at_ms"],
                        "properties": {
                            "ask": { "type": "string" },
                            "device": { "type": "string" },
                            "label": { "type": "string" },
                            "code": { "type": "string" },
                            "via": { "type": "string" },
                            "asked_at_ms": { "type": "integer" }
                        }
                    }
                }
            }
        }),
        ANSWER => serde_json::json!({
            "type": "object",
            "required": ["ask", "label", "approved"],
            "properties": {
                "ask": { "type": "string" },
                "label": { "type": "string" },
                "approved": { "type": "boolean" }
            }
        }),
        _ => serde_json::json!({
            "type": "object",
            "required": ["device", "forgotten", "stopped"],
            "properties": {
                "device": { "type": "string" },
                "forgotten": { "type": "boolean" },
                "stopped": { "type": "boolean" }
            }
        }),
    }
}

/// How a step started: answered at once, or answering later.
pub(crate) enum Started {
    Answered(serde_json::Value),
    Later(Pending<Answer>),
}

/// One program the session runs for a pairing step.
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

/// Runs a [`HostCommand`] to its end on the calling worker thread, giving up (and stopping the
/// child) once `stop` is raised.
pub(crate) type Runner =
    Arc<dyn Fn(&HostCommand, &AtomicBool) -> Result<Finished, String> + Send + Sync>;

#[derive(Default)]
struct Flights {
    discovering: bool,
    /// What discovery found, by device key, until approved or rejected.
    candidates: HashMap<String, (PairRole, Candidate)>,
    /// The device whose approval runs, and what stops waiting on it.
    approving: Option<(String, Arc<AtomicBool>)>,
}

/// What of the identity's key the device holds, read from its identity.
pub(crate) type Holding = Arc<dyn Fn(&PersonPaths) -> Result<KeyHolding, String> + Send + Sync>;

/// The host's pairing steps and what they have found.
pub(crate) struct Pairing {
    runner: Runner,
    holding: Holding,
    flights: Arc<Mutex<Flights>>,
}

static SESSION: LazyLock<Pairing> = LazyLock::new(|| {
    Pairing::with_runner(Arc::new(run_bounded))
        .holding(Arc::new(tilo_identity_host::device_admin::key_holding))
});

impl Pairing {
    /// This session's pairing: what every door shares, so a device one door discovered is approved
    /// or rejected through another.
    pub(crate) fn session() -> &'static Self {
        &SESSION
    }

    /// Pairing whose commands `runner` runs, on a device holding the identity's whole key.
    pub(crate) fn with_runner(runner: Runner) -> Self {
        Self {
            runner,
            holding: Arc::new(|_| Ok(KeyHolding::Whole)),
            flights: Arc::default(),
        }
    }

    /// This pairing on a device whose key `holding` reads.
    pub(crate) fn holding(self, holding: Holding) -> Self {
        Self { holding, ..self }
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

    /// Admit the device `approval` names, which discovery found here, to the identity at `paths` .
    /// A device holding the identity's key signs its certificate through the helper, after an
    /// administrator authenticated. The answer arrives once the device joined, or with why it did
    /// not.
    pub(crate) fn approve(
        &self,
        approval: &Approval,
        paths: &PersonPaths,
        fabric: Option<&SharedLink>,
    ) -> Result<Pending<Answer>, String> {
        let workspace_ids = match approval.role {
            PairRole::HomePeer => approval
                .workspace_ids
                .clone()
                // A new home server keeps no workspace until one adds it.
                .unwrap_or_default(),
            PairRole::PersonalDevice => Vec::new(),
        };
        let holding = (self.holding)(paths)?;
        if holding != KeyHolding::Whole && approval.role == PairRole::HomePeer {
            return Err("a home server is added from the device you set up first".into());
        }
        let (label, candidate, stop) = {
            let mut flights = self.flights();
            refuse_busy(&flights)?;
            let (role, candidate) = flights.candidates.get(&approval.device).ok_or_else(|| {
                "no device with that key was discovered here; discover it first".to_string()
            })?;
            if *role != approval.role || candidate.relay_service_node_id != approval.service {
                return Err("that is not the device discovered with this key".into());
            }
            let label = candidate.label.clone();
            let candidate = candidate.clone();
            let stop = Arc::new(AtomicBool::new(false));
            flights.approving = Some((approval.device.clone(), Arc::clone(&stop)));
            drop(flights);
            (label, candidate, stop)
        };
        let admission = Admission {
            holding,
            approval: approval.clone(),
            paths: paths.clone(),
            workspace_ids,
            candidate,
            fabric: fabric.cloned(),
        };
        let (completer, answer) = pending();
        let runner = Arc::clone(&self.runner);
        let flights = Arc::clone(&self.flights);
        let device = approval.device.clone();
        let role = approval.role;
        let started = spawn_step(move || {
            let ran = admission.run(&runner, &stop);
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

    /// Start `step` with `args` , which the door bound to [`params`] after it decided the caller
    /// may pair.
    pub(crate) fn start(
        &self,
        step: &str,
        args: &ActionArgs,
        fabric: Option<&SharedLink>,
    ) -> Result<Started, String> {
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
                let paths = PersonPaths::from_env()?;
                self.approve(&approval, &paths, fabric).map(Started::Later)
            }
            REJECT => self
                .reject(args.text("device").unwrap_or_default())
                .map(Started::Answered),
            ASKING => Ok(Started::Answered(crate::certification::asking_step(fabric))),
            ANSWER => {
                let approve = args.boolean("approve").unwrap_or(false);
                if approve && args.boolean("confirm") != Some(true) {
                    return Err("explicit confirmation is required to approve this device".into());
                }
                let ask = args.text("ask").unwrap_or_default().to_owned();
                let fabric = fabric.cloned();
                let (completer, answered) = pending();
                spawn_step(move || {
                    completer.complete(crate::certification::answer_step(
                        fabric.as_ref(),
                        &ask,
                        approve,
                    ));
                })?;
                Ok(Started::Later(answered))
            }
            _ => Err(format!("`{step}` is not a pairing step")),
        }
    }

    /// Forget the discovered device `device` and stop waiting on its approval, if one runs.
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

/// One approval, as its worker runs it.
struct Admission {
    holding: KeyHolding,
    approval: Approval,
    paths: PersonPaths,
    workspace_ids: Vec<u64>,
    candidate: Candidate,
    /// The person's device fabric, where their other devices are asked to certify the device when
    /// this one holds only a share of their key.
    fabric: Option<SharedLink>,
}

impl Admission {
    fn run(&self, runner: &Runner, stop: &AtomicBool) -> Result<Finished, String> {
        let KeyHolding::Part {
            person,
            threshold,
            total,
        } = self.holding
        else {
            return runner(
                &approval_command(&self.approval, &self.paths, &self.workspace_ids),
                stop,
            );
        };
        let fabric = self
            .fabric
            .as_ref()
            .ok_or_else(crate::certification::cannot_certify_here)?;
        let (proposal, service) = tilo_identity_host::device_admin::proposed_certificate(
            person,
            &self.candidate,
            tilo_clock::unix_now_ns(),
        )?;
        let cert = crate::certification::certify(fabric, proposal, service, threshold, total)?;
        if stop.load(Ordering::Acquire) {
            return Err("stopped".into());
        }
        let file = tempfile::NamedTempFile::new()
            .map_err(|error| format!("could not keep the approved certificate: {error}"))?;
        tilo_user_identity::persist::write_device_cert(file.path(), &cert)
            .map_err(|error| format!("could not keep the approved certificate: {error}"))?;
        runner(
            &HostCommand {
                program: PathBuf::from(ENROLL),
                args: tilo_identity_host::device_admin::approved_args(
                    &self.approval,
                    &self.paths,
                    file.path(),
                ),
                timeout: APPROVAL_TIMEOUT,
            },
            stop,
        )
    }
}

/// The helper invocation that admits `approval` 's device.
pub(crate) fn approval_command(
    approval: &Approval,
    paths: &PersonPaths,
    workspace_ids: &[u64],
) -> HostCommand {
    HostCommand {
        program: PathBuf::from(PKEXEC),
        args: approval_args(approval, paths, workspace_ids),
        timeout: APPROVAL_TIMEOUT,
    }
}

/// The device a finished discovery reports.
fn discovered(finished: &Finished) -> Result<Candidate, String> {
    tilo_identity_host::device_admin::discovered(
        finished.success,
        &finished.stdout,
        &finished.stderr,
    )
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

/// The broker's door to pairing: how Settings pairs a device, as the person using it, without
/// starting any process itself.
pub(crate) struct BrokerDoor {
    pub(crate) desk: std::sync::Arc<crate::plane::DeskAuthority>,
    pub(crate) fabric: Option<SharedLink>,
    pub(crate) pairing: &'static Pairing,
}

impl BrokerDoor {
    /// Refuse unless `person` administers `workspace` 's identity: they may grant in the workspace,
    /// and it has an identity selected.
    fn authorize(&self, workspace: &str, person: &str) -> Result<(), String> {
        crate::identity_admin::authorize(&self.desk, workspace, person).map(drop)
    }
}

/// `step` , described for the broker's catalogue.
fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use tilo_desktop_surface::i18n::t;
    let (label, description) = match step {
        DISCOVER => (
            "search-action-identity-device-pair-discover",
            "search-action-identity-device-pair-discover-subtitle",
        ),
        APPROVE => (
            "search-action-identity-device-pair-approve",
            "search-action-identity-device-pair-approve-subtitle",
        ),
        ASKING => (
            "search-action-identity-device-pair-asking",
            "search-action-identity-device-pair-asking-subtitle",
        ),
        ANSWER => (
            "search-action-identity-device-pair-answer",
            "search-action-identity-device-pair-answer-subtitle",
        ),
        _ => (
            "search-action-identity-device-pair-reject",
            "search-action-identity-device-pair-reject-subtitle",
        ),
    };
    tilo_actions::ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, step),
        resource_kind: RESOURCE.into(),
        resource_label: t("search-action-identity-device-pair-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: Some(t(description)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: contract(step),
        input: tilo_actions::InputSpec::None,
        params: params(step).to_vec(),
        view: None,
    }
}

/// `argument` , in its slot form, bound to `params` .
fn bind_slot(params: &[ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .ok_or("pairing steps take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

fn outcome(answer: Answer) -> tilo_broker_core::steps::HostStepOutcome {
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

impl tilo_broker_core::steps::HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        tilo_actions::remote::device_pairing::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_broker_core::steps::HostStepRequest<'_>,
    ) -> Result<Pending<tilo_broker_core::steps::HostStepOutcome>, String> {
        // Admitting a device to a person's identity is that person's own act: an agent or an app
        // acting for nobody never pairs through here.
        let person = request
            .actor
            .direct_person()
            .ok_or("only a person pairs a device")?;
        let step = STEPS
            .into_iter()
            .find(|step| request.address.resource == RESOURCE && request.address.action == *step)
            .ok_or("unknown pairing step")?;
        let args = bind_slot(params(step), request.argument)?;
        self.authorize(request.workspace, person)?;
        Ok(
            match self.pairing.start(step, &args, self.fabric.as_ref())? {
                Started::Answered(value) => Pending::ready(outcome(Ok(value))),
                Started::Later(answer) => answer.map(outcome),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a step answered, once it did.
    fn answering(answer: Pending<Answer>) -> Answer {
        answer
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the step answers")
    }

    /// The value a step answered.
    fn answered(answer: Pending<Answer>) -> serde_json::Value {
        answering(answer).unwrap_or_else(|error| panic!("the step answers: {error}"))
    }

    /// A device announcing itself, as `tilo-enroll discover` prints it.
    fn announced(device: &str, service: &str) -> Vec<u8> {
        serde_json::json!({
            "device_node_id": device,
            "relay_service_node_id": service,
            "label": "Phone",
            "verification_code": "123456",
        })
        .to_string()
        .into_bytes()
    }

    /// The pairing whose commands are recorded and answered as `found` announced itself.
    fn recording(found: (String, String)) -> (Pairing, Arc<Mutex<Vec<HostCommand>>>) {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&commands);
        let (device, service) = found;
        let pairing = Pairing::with_runner(Arc::new(move |command, _| {
            recorded.lock().expect("commands").push(command.clone());
            let stdout = if command.program == std::path::Path::new(ENROLL) {
                announced(&device, &service)
            } else {
                Vec::new()
            };
            Ok(Finished {
                success: true,
                stdout,
                stderr: Vec::new(),
            })
        }));
        (pairing, commands)
    }

    #[test]
    fn a_discovered_device_is_approved_through_the_privileged_helper_and_forgotten_after() {
        let device = "AA".repeat(32);
        let service = "bb".repeat(32);
        let (pairing, commands) = recording((device, service.clone()));
        let paths = PersonPaths {
            root: PathBuf::from("/var/lib/tilo"),
            dir: PathBuf::from("/home/person/.config/tilo/identity"),
        };

        let found = answered(
            pairing
                .discover(PairRole::PersonalDevice)
                .expect("discovery"),
        );
        assert_eq!(
            found,
            serde_json::json!({
                "role": "personal-device",
                "device": "aa".repeat(32),
                "service": service,
                "label": "Phone",
                "verification_code": "123456",
            })
        );

        // Only a device discovery found here can be approved.
        let unknown = Approval {
            role: PairRole::PersonalDevice,
            device: "cc".repeat(32),
            service: service.clone(),
            workspace_ids: None,
        };
        let refused = pairing
            .approve(&unknown, &paths, None)
            .err()
            .expect("an undiscovered device is not approved");
        assert!(refused.contains("discover it first"), "{refused}");

        let approval = Approval {
            role: PairRole::PersonalDevice,
            device: "aa".repeat(32),
            service,
            workspace_ids: None,
        };
        let approved = answered(pairing.approve(&approval, &paths, None).expect("approval"));
        assert_eq!(
            approved,
            serde_json::json!({
                "role": "personal-device",
                "device": "aa".repeat(32),
                "label": "Phone",
            })
        );

        let recorded = commands.lock().expect("commands").clone();
        assert_eq!(
            recorded[0],
            HostCommand {
                program: PathBuf::from(ENROLL),
                args: ["discover-device", "--timeout", LISTEN_SECONDS]
                    .map(OsString::from)
                    .to_vec(),
                timeout: DISCOVERY_TIMEOUT,
            }
        );
        assert_eq!(recorded[1].program, PathBuf::from(PKEXEC), "{recorded:?}");
        let ask = tilo_identity_host::device_admin::approval_args(&approval, &paths, &[]);
        assert_eq!(recorded[1].args, ask);
        assert_eq!(recorded[1].timeout, APPROVAL_TIMEOUT);

        // The device joined: nothing is left to reject.
        assert!(pairing.reject(&approval.device).is_err());
    }

    /// Pairing on a device holding only a share of the person's key.
    fn sharing(found: (String, String)) -> (Pairing, Arc<Mutex<Vec<HostCommand>>>) {
        let (pairing, commands) = recording(found);
        let pairing = pairing.holding(Arc::new(|_| {
            Ok(KeyHolding::Part {
                person: [0x11; 32],
                threshold: 2,
                total: 3,
            })
        }));
        (pairing, commands)
    }

    #[test]
    fn a_home_server_is_only_added_from_the_device_the_person_set_up_first() {
        let (pairing, commands) = sharing(("aa".repeat(32), "bb".repeat(32)));
        let paths = PersonPaths {
            root: PathBuf::from("/var/lib/tilo"),
            dir: PathBuf::from("/home/person/.config/tilo/identity"),
        };
        let _ = answered(pairing.discover(PairRole::HomePeer).expect("discovery"));
        let approval = Approval {
            role: PairRole::HomePeer,
            device: "aa".repeat(32),
            service: "bb".repeat(32),
            workspace_ids: Some(vec![7]),
        };
        let refused = pairing
            .approve(&approval, &paths, None)
            .err()
            .expect("a home server needs the whole key");
        assert!(refused.contains("the device you set up first"), "{refused}");
        assert_eq!(
            commands.lock().expect("commands").len(),
            1,
            "nothing ran for the home server"
        );
    }

    #[test]
    fn a_device_holding_a_share_signs_nothing_without_the_other_devices() {
        let (pairing, commands) = sharing(("aa".repeat(32), "bb".repeat(32)));
        let paths = PersonPaths {
            root: PathBuf::from("/var/lib/tilo"),
            dir: PathBuf::from("/home/person/.config/tilo/identity"),
        };
        let _ = answered(
            pairing
                .discover(PairRole::PersonalDevice)
                .expect("discovery"),
        );
        let approval = Approval {
            role: PairRole::PersonalDevice,
            device: "aa".repeat(32),
            service: "bb".repeat(32),
            workspace_ids: None,
        };
        let refused = answering(
            pairing
                .approve(&approval, &paths, None)
                .expect("the approval starts"),
        )
        .expect_err("this session runs no device fabric");
        assert!(refused.contains("runs no device fabric"), "{refused}");
        assert_eq!(
            commands.lock().expect("commands").len(),
            1,
            "no helper signed for a device this session cannot certify"
        );
    }
}
