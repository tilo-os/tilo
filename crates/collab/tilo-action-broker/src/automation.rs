//! Shortcuts on the action door.
//!
//! A workspace's saved [`Shortcut`]s are published by the action broker next
//! to the workspace's own governance actions, so every caller of the door —
//! the palette, `tilo-control`, the MCP bridge, an agent, a peer — finds and
//! runs them the same way:
//!
//! - one resource per shortcut, `shortcuts.shortcut:<id>`, carrying the
//!   action `run`, whose parameters are the shortcut's inputs and whose
//!   result is the shortcut's output;
//! - one library resource, `shortcuts`, carrying `create`, `update`,
//!   `delete` and `list`, which take and return the shortcut's JSON.
//!
//! A shortcut is saved only after [`tilo_automation::check()`] accepts it
//! against the catalogue the saving person is offered at that moment, and
//! the host stores it through its [`ShortcutLibraryServices`]. It runs as the
//! person who invoked it, inside its own grant, through the same broker;
//! each step's change carries an [`ActionContext`] grouping it under the run,
//! from which the host records one run that Activity shows and can undo as a
//! whole.
//!
//! [`ShortcutLibraryServices`]: crate::services::ShortcutLibraryServices

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Map, Value};
use settings_store::hardware::{ActionChord, ActionChordTarget, KeyChord, ShortcutConfig};
use tilo_actions::context::ActionContext;
use tilo_actions::contract::{ActionContract, ActionKind, ResultSpec, RiskLevel};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::outcome::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ParamType};
use tilo_actions::remote::workspace_provider;
use tilo_actions::remote::{Confirmation, Pending, pending};
use tilo_actions::{ExecutionLocus, OperationKind};
use tilo_automation::{
    ActionCatalog, AskRequest, ConfirmationRequest, FOCUS_DO_NOT_DISTURB, FOCUS_SCHEDULE, RunHooks,
    RunId, RunMode, RunOutcome, RunReport, RunTrigger, SHORTCUT_RESOURCE_KIND, SHORTCUT_RUN_ACTION,
    SHORTCUTS_APP_ID, Schedule, Shortcut, StopWatch, TriggerSpec, shortcut_id_of,
    shortcut_resource_id,
};
use tilo_desktop_model::Workspace;

/// The resource carrying the shortcut library's actions.
pub const LIBRARY_RESOURCE: &str = "shortcuts";
pub const LIBRARY_RESOURCE_KIND: &str = "shortcuts.library";
/// Save a new shortcut.
pub const CREATE_ACTION: &str = "create";
/// Replace a saved shortcut.
pub const UPDATE_ACTION: &str = "update";
/// Delete a saved shortcut.
pub const DELETE_ACTION: &str = "delete";
/// Every shortcut saved in the workspace.
pub const LIST_ACTION: &str = "list";
/// Stop a run of a saved shortcut that is still going.
pub const STOP_ACTION: &str = "stop";
/// Stop every run going in the workspace.
pub const STOP_ALL_ACTION: &str = "stop-all";

/// How long a running step is left before it is observed again, when its
/// host cannot say when it changes. The broker's own door can, so this is
/// only a fallback.
const SETTLE_INTERVAL: Duration = Duration::from_millis(250);

/// Finished runs kept for the history recorder; the oldest go first.
const MAX_FINISHED_RUNS: usize = 64;

/// Every descriptor `workspace` publishes for its shortcuts: the library's
/// actions, then `run` on each saved shortcut.
#[must_use]
pub fn descriptors(workspace: &Workspace) -> Vec<ActionDescriptor> {
    [
        CREATE_ACTION,
        UPDATE_ACTION,
        DELETE_ACTION,
        LIST_ACTION,
        STOP_ACTION,
        STOP_ALL_ACTION,
    ]
    .into_iter()
    .filter_map(library_descriptor)
    .chain(workspace.shortcuts.iter().map(run_descriptor))
    .collect()
}

#[must_use]
pub fn library_descriptor(action: &str) -> Option<ActionDescriptor> {
    let shortcut_param = || {
        let mut param = ActionParam::required(
            "shortcut",
            ParamType::Object,
            "The shortcut, as JSON: id, name, inputs, steps, output and grant",
        );
        param.schema = Some(
            serde_json::json!({
                "type": "object",
                "required": ["id", "name", "steps"]
            })
            .into(),
        );
        param
    };
    let saved = ResultSpec::Resource {
        resource_kind: SHORTCUT_RESOURCE_KIND.into(),
    };
    let (label, description, params, permission, result) = match action {
        CREATE_ACTION => (
            "New shortcut",
            "Save a shortcut after checking every step against the actions you are offered",
            vec![shortcut_param()],
            ActionKind::Undoable,
            saved,
        ),
        UPDATE_ACTION => (
            "Change shortcut",
            "Replace a saved shortcut after checking it again",
            vec![shortcut_param()],
            ActionKind::Undoable,
            saved,
        ),
        DELETE_ACTION => (
            "Delete shortcut",
            "Delete a saved shortcut",
            vec![ActionParam::required(
                "id",
                ParamType::Text,
                "Id of the shortcut to delete",
            )],
            ActionKind::Undoable,
            ResultSpec::None,
        ),
        LIST_ACTION => (
            "List shortcuts",
            "Every shortcut saved in this workspace, as JSON",
            Vec::new(),
            ActionKind::Read,
            ResultSpec::Json {
                schema: serde_json::json!({"type": "array", "items": {"type": "object"}}),
            },
        ),
        STOP_ACTION => (
            "Stop a run",
            "Stop a running shortcut before its next step; what it already did is kept and can be undone",
            vec![ActionParam::required(
                "id",
                ParamType::Text,
                "Id of the shortcut whose run to stop",
            )],
            ActionKind::Read,
            ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
        ),
        STOP_ALL_ACTION => (
            "Stop every run",
            "Stop every running shortcut before its next step; what they already did is kept and can be undone",
            Vec::new(),
            ActionKind::Read,
            ResultSpec::Json {
                schema: serde_json::json!({"type": "array", "items": {"type": "object"}}),
            },
        ),
        _ => return None,
    };
    Some(ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(LIBRARY_RESOURCE, action),
        resource_kind: LIBRARY_RESOURCE_KIND.into(),
        resource_label: "Shortcuts".into(),
        action_label: label.into(),
        description: Some(description.into()),
        locus: ExecutionLocus::default(),
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: permission,
            result,
            risk: if action == DELETE_ACTION {
                RiskLevel::Destructive
            } else {
                RiskLevel::Low
            },
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params,
        view: None,
    })
}

/// The descriptor of `run` on `shortcut`. Running needs no authority of its
/// own: each step is checked against the invoker's authority as it runs.
#[must_use]
pub fn run_descriptor(shortcut: &Shortcut) -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: shortcut.action_address(),
        resource_kind: SHORTCUT_RESOURCE_KIND.into(),
        resource_label: shortcut.name.clone(),
        action_label: format!("Run {}", shortcut.name),
        description: Some(run_description(shortcut)),
        locus: ExecutionLocus::default(),
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Read,
            result: if shortcut.output.is_some() {
                ResultSpec::Json {
                    schema: serde_json::json!({}),
                }
            } else {
                ResultSpec::None
            },
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params: shortcut.inputs.clone(),
        view: None,
    }
}

/// What `run` on `shortcut` says about itself: its steps, what starts it
/// on its own, and which confirmations its saver gave in advance.
#[must_use]
pub fn run_description(shortcut: &Shortcut) -> String {
    let mut description = format!(
        "Run the shortcut “{}”: {} step{}",
        shortcut.name,
        shortcut.steps.len(),
        if shortcut.steps.len() == 1 { "" } else { "s" }
    );
    let triggers: Vec<String> = shortcut.triggers.iter().map(trigger_summary).collect();
    if !triggers.is_empty() {
        description.push_str(". Runs by itself ");
        description.push_str(&triggers.join(" and "));
    }
    let consents: Vec<String> = tilo_automation::grant::advance_consents(&shortcut.grant)
        .into_iter()
        .map(|(resource, action)| format!("`{action}` on {resource}"))
        .collect();
    if !consents.is_empty() {
        description.push_str(". Confirmed in advance: ");
        description.push_str(&consents.join(", "));
    }
    description
}

/// A trigger in words, e.g. "on Super+Shift+N" or "every day at 09:00".
#[must_use]
pub fn trigger_summary(trigger: &TriggerSpec) -> String {
    match trigger {
        TriggerSpec::Keyboard { chord } => format!(
            "on {}",
            KeyChord::parse(chord).map_or_else(|_| chord.clone(), |chord| chord.display())
        ),
        TriggerSpec::Schedule { schedule, .. } => match schedule {
            Schedule::Interval { seconds } if seconds % 3600 == 0 => {
                format!("every {} h", seconds / 3600)
            }
            Schedule::Interval { seconds } if seconds % 60 == 0 => {
                format!("every {} min", seconds / 60)
            }
            Schedule::Interval { seconds } => format!("every {seconds} s"),
            Schedule::Calendar { at, weekdays } => {
                let days = if weekdays.is_empty() {
                    "every day".to_string()
                } else {
                    let names: Vec<String> =
                        weekdays.iter().map(|day| format!("{day:?}")).collect();
                    format!("on {}", names.join(", "))
                };
                format!("{days} at {:02}:{:02}", at.hour, at.minute)
            }
        },
        other => format!("on {}", other.kind()),
    }
}

/// The action-door address a chord runs `shortcut_id` in `room` through.
#[must_use]
pub fn chord_target(room: &str, shortcut_id: &str) -> ActionChordTarget {
    ActionChordTarget {
        provider: workspace_provider(room),
        resource: shortcut_resource_id(shortcut_id),
        action: SHORTCUT_RUN_ACTION.into(),
    }
}

/// Whether `target` runs a saved shortcut.
#[must_use]
pub fn is_shortcut_target(target: &ActionChordTarget) -> bool {
    target.action == SHORTCUT_RUN_ACTION && shortcut_id_of(&target.resource).is_some()
}

/// Replace the shortcut chords in `keymap` with `chords`, keeping every
/// other binding. A chord that has meanwhile become taken (the person bound
/// it to a desktop action on this device) is left out rather than taking
/// it back. Whether anything changed.
pub fn replace_trigger_chords(keymap: &mut ShortcutConfig, chords: Vec<ActionChord>) -> bool {
    let before = keymap.actions.clone();
    keymap
        .actions
        .retain(|bound| !is_shortcut_target(&bound.target));
    for bound in chords {
        let _ = keymap.set_action(bound.chord, bound.target, bound.label);
    }
    keymap.actions != before
}

/// Why `shortcut`'s triggers cannot be saved in `workspace` by
/// `context.saver`, with `context.keymap` the chords already taken on this
/// device.
///
/// An input a trigger's payload ([`TriggerSpec::payload`]) names must have
/// the payload's type, and an input the shortcut requires must be given by
/// every one of its triggers: nobody is there to give it.
///
/// # Errors
///
/// The first problem, for the person saving it.
pub fn check_triggers(
    shortcut: &Shortcut,
    workspace: &Workspace,
    context: &SaveContext<'_>,
) -> Result<(), String> {
    if shortcut.triggers.is_empty() {
        return Ok(());
    }
    for input in &shortcut.inputs {
        let given: Vec<_> = shortcut
            .triggers
            .iter()
            .map(|trigger| {
                trigger
                    .payload()
                    .iter()
                    .find(|value| value.name == input.name)
            })
            .collect();
        if let Some(value) = given.iter().flatten().find(|value| value.ty != input.ty) {
            return Err(format!(
                "input `{}` is {} but a trigger gives it as {}",
                input.name,
                input.ty.label(),
                value.ty.label()
            ));
        }
        if input.required && given.iter().any(Option::is_none) {
            return Err(format!(
                "a shortcut with triggers cannot require input `{}` unless every trigger gives it: \
                 nobody is there to give it",
                input.name
            ));
        }
    }
    let room = workspace.room().to_string();
    let others = &workspace.shortcuts;
    let target = chord_target(&room, &shortcut.id);
    let mut keyboard = None;
    for trigger in &shortcut.triggers {
        validate_trigger(trigger)?;
        if let TriggerSpec::FileChanged { capability } = trigger {
            readable_folder(workspace, capability, context.saver)?;
        }
        let TriggerSpec::Keyboard { chord: text } = trigger else {
            continue;
        };
        let chord = KeyChord::parse(text).map_err(|reason| format!("`{text}`: {reason}"))?;
        if keyboard.replace(chord.clone()).is_some() {
            return Err("a shortcut takes at most one keyboard trigger".into());
        }
        context
            .keymap
            .check_action_chord(&chord, Some(&target))
            .map_err(|error| match error {
                settings_store::hardware::ShortcutConfigError::Fixed(id) => format!(
                    "{} cannot run the shortcut: the desktop uses it for “{}”",
                    chord.display(),
                    settings_store::keyboard_shortcuts::fixed_shortcut(id)
                        .map_or_else(|| id.to_string(), context.fixed_label)
                ),
                error => format!("{} cannot run the shortcut: {error}", chord.display()),
            })?;
        if let Some(other) = others.iter().find(|other| {
            other.id != shortcut.id
                && other.triggers.iter().any(|trigger| {
                    matches!(trigger, TriggerSpec::Keyboard { chord: bound }
                        if KeyChord::parse(bound).as_ref() == Ok(&chord))
                })
        }) {
            return Err(format!(
                "{} already runs the shortcut “{}”",
                chord.display(),
                other.name
            ));
        }
    }
    Ok(())
}

/// The shortcut id `address` runs, when it is a shortcut's `run`.
#[must_use]
pub fn run_target(address: &ActionAddress) -> Option<&str> {
    (address.action == SHORTCUT_RUN_ACTION)
        .then(|| shortcut_id_of(&address.resource))
        .flatten()
}

/// One decoded library invocation.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryOperation {
    Create(Shortcut),
    Update(Shortcut),
    Delete(String),
    /// Stop the runs of shortcut `id` that are still going.
    Stop(String),
    /// Stop every run going in the workspace.
    StopAll,
    List,
}

/// Decode an invocation of library action `action` from its argument slot.
///
/// # Errors
///
/// Why the arguments do not satisfy the action's parameters.
pub fn decode_library(action: &str, argument: Option<&str>) -> Result<LibraryOperation, String> {
    let descriptor =
        library_descriptor(action).ok_or_else(|| format!("unknown shortcuts action `{action}`"))?;
    let supplied = match argument {
        None | Some("") => Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(ARGUMENTS_JSON_PREFIX)
                .ok_or("shortcut actions take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    let bound =
        ActionArgs::bind_json(&descriptor.params, &supplied).map_err(|error| error.to_string())?;
    let shortcut = || {
        bound
            .to_json()
            .get("shortcut")
            .cloned()
            .ok_or_else(|| "`shortcut` is required".to_string())
            .and_then(|value| {
                serde_json::from_value::<Shortcut>(value)
                    .map_err(|error| format!("`shortcut` is not a shortcut: {error}"))
            })
    };
    Ok(match action {
        CREATE_ACTION => LibraryOperation::Create(shortcut()?),
        UPDATE_ACTION => LibraryOperation::Update(shortcut()?),
        DELETE_ACTION => LibraryOperation::Delete(
            bound
                .text("id")
                .ok_or("`id` is required")?
                .trim()
                .to_string(),
        ),
        STOP_ACTION => LibraryOperation::Stop(
            bound
                .text("id")
                .ok_or("`id` is required")?
                .trim()
                .to_string(),
        ),
        STOP_ALL_ACTION => LibraryOperation::StopAll,
        _ => LibraryOperation::List,
    })
}

/// Who is saving a shortcut, and what it is checked against.
pub struct SaveContext<'a> {
    /// What the saving person is offered now.
    pub catalog: &'a ActionCatalog,
    /// The saving person's policy id; recorded as the shortcut's saver.
    pub saver: &'a str,
    /// The chords already taken on this device.
    pub keymap: &'a ShortcutConfig,
    /// What the desktop calls a fixed shortcut, in the person's language.
    pub fixed_label: &'a dyn Fn(&settings_store::keyboard_shortcuts::FixedShortcut) -> String,
}

pub use tilo_automation::ShortcutChange;

/// What one library invocation does: answer from what is saved, or change it.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryPlan {
    /// Answer without changing anything.
    Answer(ActionOutput),
    /// Stop the runs the operation names, then answer.
    Stop(StopRequest),
    /// Store `change`, then answer with `output`.
    Change {
        change: ShortcutChange,
        output: ActionOutput,
    },
}

/// Which runs a stop names: one shortcut's, or every one going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopRequest {
    Shortcut(String),
    All,
}

/// Plan `operation` against the shortcuts saved in `workspace`.
/// A saved shortcut is first checked against what the saving person is
/// offered now, and its triggers against the chords already taken; it is
/// stored as saved by them.
///
/// # Errors
///
/// Every problem the checker found, or why the workspace cannot take the
/// change.
pub fn plan_library(
    workspace: &Workspace,
    operation: LibraryOperation,
    context: &SaveContext<'_>,
) -> Result<LibraryPlan, String> {
    let saved = &workspace.shortcuts;
    let (shortcut, replacing) = match operation {
        LibraryOperation::List => {
            return Ok(LibraryPlan::Answer(ActionOutput {
                value: Some(serde_json::to_value(saved).map_err(|error| error.to_string())?),
                resources: Vec::new(),
            }));
        }
        LibraryOperation::Delete(id) => {
            if !saved.iter().any(|shortcut| shortcut.id == id) {
                return Err(format!("there is no shortcut `{id}`"));
            }
            return Ok(LibraryPlan::Change {
                change: ShortcutChange::Delete(id),
                output: ActionOutput::default(),
            });
        }
        LibraryOperation::Stop(id) => {
            if !saved.iter().any(|shortcut| shortcut.id == id) {
                return Err(format!("there is no shortcut `{id}`"));
            }
            return Ok(LibraryPlan::Stop(StopRequest::Shortcut(id)));
        }
        LibraryOperation::StopAll => return Ok(LibraryPlan::Stop(StopRequest::All)),
        LibraryOperation::Create(shortcut) => (shortcut, false),
        LibraryOperation::Update(shortcut) => (shortcut, true),
    };
    let mut shortcut = shortcut;
    shortcut.saved_by = Some(context.saver.to_string());
    // The device that stores it signs it; whatever proof came in is not its.
    shortcut.saved_proof = None;
    let exists = saved.iter().any(|other| other.id == shortcut.id);
    if exists && !replacing {
        return Err(format!(
            "a shortcut `{}` already exists; update it instead",
            shortcut.id
        ));
    }
    if !exists && replacing {
        return Err(format!("there is no shortcut `{}`", shortcut.id));
    }
    check_triggers(&shortcut, workspace, context)?;
    let resolver = |id: &str| saved.iter().find(|other| other.id == id).cloned();
    tilo_automation::check(&shortcut, context.catalog, &resolver).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let resource = shortcut_resource_id(&shortcut.id);
    Ok(LibraryPlan::Change {
        change: ShortcutChange::Save(Box::new(shortcut)),
        output: ActionOutput {
            value: None,
            resources: vec![resource],
        },
    })
}

/// The provenance a library invocation leaves in workspace activity.
#[must_use]
pub fn library_context(descriptor: &ActionDescriptor) -> ActionContext {
    ActionContext::for_resource(LIBRARY_RESOURCE, LIBRARY_RESOURCE_KIND, "Shortcuts")
        .with_app(SHORTCUTS_APP_ID, "Shortcuts")
        .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(
                SHORTCUTS_APP_ID,
                LIBRARY_RESOURCE_KIND,
                descriptor.address.action.clone(),
            ),
            [],
        )))
}

/// A broker's shortcut runs that are not over: the steps waiting on their
/// person, and the runs going so one can be stopped between two steps.
#[derive(Clone, Default)]
pub struct RunLedger {
    pub prompts: StepPrompts,
    pub running: RunningRuns,
}

/// The runs this broker has going, by the room and the shortcut each runs.
///
/// A run registers itself before its first step and leaves when it ends;
/// stopping one marks it, and the interpreter asks before every step that
/// would invoke something. What a stopped run already did stands, and is
/// recorded and undone like a completed run's.
#[derive(Clone, Default)]
pub struct RunningRuns {
    running: std::sync::Arc<Mutex<Vec<RunningRun>>>,
}

/// One run going, as a stop names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningRun {
    pub room: String,
    pub shortcut: String,
    /// What the shortcut is called, for a person reading the answer.
    pub name: String,
    pub run: RunId,
    /// The step the run has reached, as it last said where it was.
    pub at: Option<String>,
    /// Why it is being stopped, once someone has.
    pub stopped: Option<String>,
}

impl RunningRuns {
    /// Note a run as going, before its first step.
    pub fn register(&self, run: RunningRun) {
        if let Ok(mut running) = self.running.lock() {
            running.retain(|other| other.run != run.run);
            running.push(run);
        }
    }

    /// Note a run as over.
    pub fn leave(&self, run: RunId) {
        if let Ok(mut running) = self.running.lock() {
            running.retain(|other| other.run != run);
        }
    }

    /// Note where run `run` has reached. A stop only lands while the run is
    /// between two answers, so the last place it named is where it ends.
    fn moved(&self, run: RunId, at: &str) {
        if let Ok(mut running) = self.running.lock()
            && let Some(running) = running.iter_mut().find(|other| other.run == run)
        {
            running.at = Some(at.to_owned());
        }
    }

    /// Whether run `run` has been told to stop, and why.
    fn stopped(&self, run: RunId) -> Option<String> {
        self.running
            .lock()
            .ok()?
            .iter()
            .find(|other| other.run == run)?
            .stopped
            .clone()
    }

    /// Stop the runs `request` names, in `room`. Returns those it stopped;
    /// a run that is not going is not an error, since stopping one that has
    /// just finished is what someone asking for it wanted anyway.
    pub fn stop(&self, room: &str, request: &StopRequest, reason: &str) -> Vec<RunningRun> {
        let Some(mut running) = self.running.lock().ok() else {
            return Vec::new();
        };
        let named = |run: &RunningRun| {
            run.room == room
                && match request {
                    StopRequest::Shortcut(id) => run.shortcut == *id,
                    StopRequest::All => true,
                }
        };
        // Only a run still going is stopped; one already told to stop is
        // left to end, and asking again names nothing new.
        let mut stopping = Vec::new();
        for run in running.iter_mut().filter(|run| named(run)) {
            if run.stopped.is_none() {
                run.stopped = Some(reason.to_owned());
                stopping.push(run.clone());
            }
        }
        stopping
    }

    /// Every run going in `room`.
    pub fn going(&self, room: &str) -> Vec<RunningRun> {
        self.running
            .lock()
            .map(|running| {
                running
                    .iter()
                    .filter(|run| run.room == room)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// What stopping says it stopped, for whoever asked: one entry per run it
/// reached, with where each had got to.
#[must_use]
pub fn stopped_output(stopped: &[RunningRun]) -> ActionOutput {
    let runs: Vec<Value> = stopped
        .iter()
        .map(|run| {
            serde_json::json!({
                "shortcut": run.shortcut,
                "name": run.name,
                "reached": run.at,
            })
        })
        .collect();
    ActionOutput {
        value: Some(Value::Array(runs)),
        resources: Vec::new(),
    }
}

/// How a run started through the door treats a person-shaped question.
///
/// Whoever invokes `run` answers for its confirmations up front: evidence
/// supplied with the invocation is offered to each step that asks for
/// confirmation, and makes the run interactive. Without it the run is
/// unattended, so a step that needs confirmation or asks a question stops
/// the run with a failure that says so rather than guessing.
#[must_use]
pub fn run_mode(confirmation: Option<&Confirmation>) -> RunMode {
    if confirmation.is_some() {
        RunMode::Interactive
    } else {
        RunMode::Unattended
    }
}

/// How one run starts.
#[derive(Clone, Debug)]
pub struct RunStart {
    pub run: RunId,
    pub trigger: RunTrigger,
    pub mode: RunMode,
    /// Values for the shortcut's inputs.
    pub inputs: Map<String, Value>,
}

/// [`RunHooks`] for a run started through the door.
pub struct DoorHooks {
    confirmation: Option<Confirmation>,
    /// The runtime the run's poll fallback waits on.
    runtime: tokio::runtime::Handle,
    /// The runs going, so one of them can be stopped from elsewhere.
    runs: RunningRuns,
    run: RunId,
}

impl DoorHooks {
    #[must_use]
    pub fn new(
        confirmation: Option<Confirmation>,
        runtime: tokio::runtime::Handle,
        runs: RunningRuns,
        run: RunId,
    ) -> Self {
        Self {
            confirmation,
            runtime,
            runs,
            run,
        }
    }
}

impl RunHooks for DoorHooks {
    fn confirm(&self, _request: ConfirmationRequest) -> Pending<Option<Confirmation>> {
        Pending::ready(self.confirmation.clone())
    }

    fn ask(&self, _request: AskRequest) -> Pending<Option<Value>> {
        // Nobody on the door can be asked mid-run; declining stops the run
        // at the question instead of inventing an answer.
        Pending::ready(None)
    }

    fn settle(&self, _invocation: InvocationId) -> Pending<()> {
        settle_later(&self.runtime)
    }

    fn stopped(&self, what: StopWatch<'_>) -> Pending<Result<(), String>> {
        self.runs.moved(self.run, what.path.as_str());
        Pending::ready(match self.runs.stopped(self.run) {
            Some(reason) => Err(reason),
            None => Ok(()),
        })
    }
}

/// Resolve after [`SETTLE_INTERVAL`] on `runtime`'s timer, so a running step
/// is observed again. Waiting takes a timer, never a thread.
fn settle_later(runtime: &tokio::runtime::Handle) -> Pending<()> {
    let (completer, answer) = pending();
    runtime.spawn(async move {
        tilo_rt::sleep(SETTLE_INTERVAL).await;
        completer.complete(());
    });
    answer
}

/// A step of an interactive run waiting for the person who started it.
pub enum StepPrompt {
    /// An action that needs confirmation before it runs.
    Confirm(Box<ConfirmationRequest>),
    /// An [`Ask`](tilo_automation::StepKind::Ask) step's question.
    Ask(AskRequest),
}

/// The person's answer to a [`StepPrompt`].
#[derive(Clone, Debug, PartialEq)]
pub enum StepAnswer {
    Confirmed(Confirmation),
    Answered(Value),
    /// Cancelled: the run stops at this step and nothing after it runs.
    Declined,
}

/// One prompt, with the run's shortcut and the way back into the run.
pub struct PromptedStep {
    /// The name of the shortcut asking.
    pub shortcut: String,
    pub prompt: StepPrompt,
    answer: tilo_actions::remote::Completer<StepAnswer>,
}

impl PromptedStep {
    /// Answer the prompt; the run continues (or stops, when declined).
    pub fn answer(self, answer: StepAnswer) {
        self.answer.complete(answer);
    }
}

impl std::fmt::Debug for PromptedStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptedStep")
            .field("shortcut", &self.shortcut)
            .finish_non_exhaustive()
    }
}

/// Prompts interactive runs are waiting on, in the order they asked, until
/// the compositor shows them.
#[derive(Clone, Default)]
pub struct StepPrompts {
    waiting: std::sync::Arc<Mutex<VecDeque<PromptedStep>>>,
}

impl StepPrompts {
    fn push(&self, prompt: PromptedStep) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.push_back(prompt);
        }
    }

    /// The oldest prompt nobody has taken yet.
    pub fn take_next(&self) -> Option<PromptedStep> {
        self.waiting.lock().ok()?.pop_front()
    }
}

/// [`RunHooks`] for a run the person is present at: every confirmation and
/// question waits in [`StepPrompts`] until the compositor's sheet answers.
pub struct SheetHooks {
    prompts: StepPrompts,
    shortcut: String,
    /// The runtime the run's poll fallback waits on.
    runtime: tokio::runtime::Handle,
    /// The runs going, so one of them can be stopped from elsewhere.
    runs: RunningRuns,
    run: RunId,
}

impl SheetHooks {
    #[must_use]
    pub fn new(
        prompts: StepPrompts,
        shortcut: impl Into<String>,
        runtime: tokio::runtime::Handle,
        runs: RunningRuns,
        run: RunId,
    ) -> Self {
        Self {
            prompts,
            shortcut: shortcut.into(),
            runtime,
            runs,
            run,
        }
    }

    fn ask_person(&self, prompt: StepPrompt) -> Pending<StepAnswer> {
        let (answer, pending) = pending();
        self.prompts.push(PromptedStep {
            shortcut: self.shortcut.clone(),
            prompt,
            answer,
        });
        pending
    }
}

impl RunHooks for SheetHooks {
    fn confirm(&self, request: ConfirmationRequest) -> Pending<Option<Confirmation>> {
        self.ask_person(StepPrompt::Confirm(Box::new(request)))
            .map(|answer| match answer {
                StepAnswer::Confirmed(confirmation) => Some(confirmation),
                StepAnswer::Answered(_) | StepAnswer::Declined => None,
            })
    }

    fn ask(&self, request: AskRequest) -> Pending<Option<Value>> {
        self.ask_person(StepPrompt::Ask(request))
            .map(|answer| match answer {
                StepAnswer::Answered(value) => Some(value),
                StepAnswer::Confirmed(_) | StepAnswer::Declined => None,
            })
    }

    fn stopped(&self, what: StopWatch<'_>) -> Pending<Result<(), String>> {
        self.runs.moved(self.run, what.path.as_str());
        Pending::ready(match self.runs.stopped(self.run) {
            Some(reason) => Err(reason),
            None => Ok(()),
        })
    }

    fn settle(&self, _invocation: InvocationId) -> Pending<()> {
        settle_later(&self.runtime)
    }
}

/// The terminal update the door reports for a finished run.
#[must_use]
pub fn completion(invocation: InvocationId, report: &RunReport) -> InvocationUpdate {
    match &report.outcome {
        // A stopped run did not fail: it ends where it was told to. What it
        // had already done is recorded and can be undone as one step, so it
        // answers like a completed run, with the report saying it stopped.
        RunOutcome::Completed | RunOutcome::Cancelled { .. } => InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: report.output.clone(),
                resources: Vec::new(),
            },
        },
        RunOutcome::Failed { path, failure } => InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: "shortcut_step_failed".into(),
                message: format!("step {path}: {failure}"),
                details: serde_json::to_value(report).ok(),
                retryable: false,
            },
        },
    }
}

/// One run the door finished, waiting to be recorded in history.
#[derive(Clone, Debug)]
pub struct FinishedShortcutRun {
    /// The room of the workspace the shortcut is saved in.
    pub room: String,
    pub shortcut: Shortcut,
    /// Policy id of the person or agent who invoked it.
    pub invoker: String,
    pub report: RunReport,
}

/// Runs the broker finished, until the history recorder takes them.
#[derive(Default)]
pub struct ShortcutRuns {
    finished: Mutex<VecDeque<FinishedShortcutRun>>,
}

impl ShortcutRuns {
    pub fn push(&self, run: FinishedShortcutRun) {
        if let Ok(mut finished) = self.finished.lock() {
            finished.push_back(run);
            while finished.len() > MAX_FINISHED_RUNS {
                finished.pop_front();
            }
        }
    }

    /// Whether a finished run is waiting to be taken.
    pub fn has_finished(&self) -> bool {
        self.finished
            .lock()
            .is_ok_and(|finished| !finished.is_empty())
    }

    /// Every finished run not taken yet, oldest first.
    pub fn take(&self) -> Vec<FinishedShortcutRun> {
        self.finished
            .lock()
            .map(|mut finished| finished.drain(..).collect())
            .unwrap_or_default()
    }
}

/// The shortest interval a schedule trigger may repeat at.
pub const MIN_INTERVAL_SECONDS: u64 = 60;

/// Why a trigger cannot be saved on a shortcut, when it cannot.
///
/// # Errors
///
/// The reason, for the person saving it.
pub fn validate_trigger(trigger: &TriggerSpec) -> Result<(), String> {
    match trigger {
        TriggerSpec::Keyboard { chord } => KeyChord::parse(chord)
            .map(drop)
            .map_err(|reason| format!("keyboard trigger `{chord}`: {reason}")),
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds },
            ..
        } => {
            if *seconds < MIN_INTERVAL_SECONDS {
                Err(format!(
                    "a schedule repeats at most once every {MIN_INTERVAL_SECONDS} seconds"
                ))
            } else {
                Ok(())
            }
        }
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar { at, .. },
            ..
        } => {
            if at.hour < 24 && at.minute < 60 {
                Ok(())
            } else {
                Err(format!(
                    "{:02}:{:02} is not a time of day",
                    at.hour, at.minute
                ))
            }
        }
        TriggerSpec::AppOpened { app_id } if app_id.trim().is_empty() => {
            Err("an app-opened trigger names the application".into())
        }
        TriggerSpec::FileChanged { capability } if capability.trim().is_empty() => {
            Err("a file-changed trigger names the capability sharing its folder".into())
        }
        TriggerSpec::FocusMode { mode, .. }
            if mode != FOCUS_DO_NOT_DISTURB && mode != FOCUS_SCHEDULE =>
        {
            Err(format!(
                "`{mode}` is not a focus mode; `{FOCUS_DO_NOT_DISTURB}` and `{FOCUS_SCHEDULE}` are"
            ))
        }
        TriggerSpec::RecordChanged { kind, .. } if kind.trim().is_empty() => {
            Err("a record trigger names the record kind it watches".into())
        }
        TriggerSpec::Connector { .. } => Err(
            "connector triggers run only bots, on the room's host; a Shortcut cannot watch one"
                .into(),
        ),
        TriggerSpec::AppOpened { .. }
        | TriggerSpec::WorkspaceJoined { .. }
        | TriggerSpec::FileChanged { .. }
        | TriggerSpec::FocusMode { .. }
        | TriggerSpec::RecordChanged { .. }
        | TriggerSpec::IncomingMessage { .. } => Ok(()),
        other => Err(format!(
            "`{}` triggers cannot be saved yet; keyboard, schedule, app-opened, \
             workspace-joined, file-changed, focus-mode, record-changed and \
             incoming-message triggers can",
            other.kind()
        )),
    }
}

/// The folder capability `capability` of `workspace` shares, when `saver`
/// may read through it.
///
/// # Errors
///
/// Why it cannot be watched, for the person saving the trigger.
pub fn readable_folder(
    workspace: &Workspace,
    capability: &str,
    saver: &str,
) -> Result<std::path::PathBuf, String> {
    let granted = workspace
        .sandbox
        .iter()
        .find(|granted| granted.id == capability)
        .ok_or_else(|| format!("the workspace has no capability `{capability}`"))?;
    let collab_types::Scope::FsPath(path) = &granted.scope else {
        return Err(format!("capability `{capability}` does not share a folder"));
    };
    if workspace.role_of(saver).is_none() || workspace.is_blocked(saver) {
        return Err("the saver is not a member of the workspace".into());
    }
    let actor = collab_types::Actor::person(saver).in_room(workspace.room().to_string());
    if workspace.sandbox_decide_at(
        &actor,
        &collab_types::ResourceRef::FsPath(path.clone()),
        collab_types::caps::Access::Read,
        tilo_clock::unix_now(),
    ) != collab_types::CapDecision::Allow
    {
        return Err(format!(
            "the saver may not read the folder capability `{capability}` shares"
        ));
    }
    Ok(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(json: Value) -> Shortcut {
        serde_json::from_value(json).expect("shortcut JSON")
    }

    #[test]
    fn library_arguments_decode_into_operations() {
        let slot = |value: Value| format!("{ARGUMENTS_JSON_PREFIX}{value}");
        let saved = serde_json::json!({"id": "tidy", "name": "Tidy", "steps": []});
        assert_eq!(
            decode_library(
                CREATE_ACTION,
                Some(&slot(serde_json::json!({ "shortcut": saved })))
            ),
            Ok(LibraryOperation::Create(shortcut(saved)))
        );
        assert_eq!(
            decode_library(
                DELETE_ACTION,
                Some(&slot(serde_json::json!({ "id": " tidy " })))
            ),
            Ok(LibraryOperation::Delete("tidy".into()))
        );
        assert_eq!(
            decode_library(LIST_ACTION, None),
            Ok(LibraryOperation::List)
        );
        assert!(decode_library(CREATE_ACTION, None).is_err());
        assert!(decode_library(CREATE_ACTION, Some("plain words")).is_err());
        assert!(
            decode_library(
                CREATE_ACTION,
                Some(&slot(serde_json::json!({"shortcut": {"id": "x"}})))
            )
            .is_err()
        );
        assert_eq!(
            decode_library(STOP_ACTION, Some(&slot(serde_json::json!({"id": "tidy"})))),
            Ok(LibraryOperation::Stop("tidy".into()))
        );
        assert_eq!(
            decode_library(STOP_ALL_ACTION, None),
            Ok(LibraryOperation::StopAll)
        );
        assert!(decode_library(STOP_ACTION, None).is_err());
        assert!(decode_library("rename", None).is_err());
    }

    fn going(room: &str, shortcut: &str, name: &str) -> RunningRun {
        RunningRun {
            room: room.into(),
            shortcut: shortcut.into(),
            name: name.into(),
            run: RunId::new(),
            at: None,
            stopped: None,
        }
    }

    #[test]
    fn stopping_names_one_shortcuts_runs_or_every_run_going_in_the_room() {
        let runs = RunningRuns::default();
        let tidy = going("desk", "tidy", "Tidy");
        let sweep = going("desk", "sweep", "Sweep");
        let elsewhere = going("study", "tidy", "Tidy");
        for run in [&tidy, &sweep, &elsewhere] {
            runs.register(run.clone());
        }
        runs.moved(tidy.run, "1");
        let stopped = runs.stop("desk", &StopRequest::Shortcut("tidy".into()), "asked");
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].shortcut, "tidy");
        assert_eq!(stopped[0].at.as_deref(), Some("1"));
        // The other room's run of the same shortcut is somebody else's.
        assert!(runs.stopped(elsewhere.run).is_none());
        assert!(runs.stopped(sweep.run).is_none());

        // Stopping everything names what is still going, not the run already
        // told to stop.
        let all = runs.stop("desk", &StopRequest::All, "asked");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].shortcut, "sweep");
        assert!(runs.stopped(sweep.run).is_some());
        assert!(runs.stopped(tidy.run).is_some());
        // Asking again stops nothing new: a run is told once.
        assert!(
            runs.stop("desk", &StopRequest::All, "asked again")
                .is_empty()
        );
        assert_eq!(runs.going("desk").len(), 2);
        runs.leave(tidy.run);
        assert_eq!(runs.going("desk").len(), 1);
    }

    #[test]
    fn a_stop_says_which_runs_it_reached_and_where_each_had_got_to() {
        let runs = RunningRuns::default();
        let tidy = going("desk", "tidy", "Tidy");
        runs.register(tidy.clone());
        runs.moved(tidy.run, "then.2");
        let output = stopped_output(&runs.stop("desk", &StopRequest::All, "asked"));
        assert_eq!(
            output.value,
            Some(serde_json::json!([{
                "shortcut": "tidy",
                "name": "Tidy",
                "reached": "then.2",
            }]))
        );
        // Nothing running is not a failure: the run just finished.
        assert_eq!(stopped_output(&[]).value, Some(serde_json::json!([])));
    }

    #[test]
    fn a_workspace_publishes_its_library_and_one_run_per_shortcut() {
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId(1), "Desk");
        workspace.shortcuts.push(shortcut(serde_json::json!({
            "id": "tidy",
            "name": "Tidy",
            "inputs": [{"name": "folder", "ty": "text", "required": true, "description": "Where"}],
            "steps": [],
            "output": {"input": "folder"}
        })));
        let published = descriptors(&workspace);
        let addresses: Vec<_> = published
            .iter()
            .map(|descriptor| {
                format!(
                    "{}#{}",
                    descriptor.address.resource, descriptor.address.action
                )
            })
            .collect();
        assert_eq!(
            addresses,
            [
                "shortcuts#create",
                "shortcuts#update",
                "shortcuts#delete",
                "shortcuts#list",
                "shortcuts#stop",
                "shortcuts#stop-all",
                "shortcuts.shortcut:tidy#run",
            ]
        );
        let run = published.last().expect("the shortcut's run");
        assert_eq!(run.params, workspace.shortcuts[0].inputs);
        assert!(matches!(run.contract.result, ResultSpec::Json { .. }));
        assert_eq!(run_target(&run.address), Some("tidy"));
    }

    #[test]
    fn a_persons_shortcut_may_watch_records_and_messages_but_not_connectors() {
        use tilo_automation::ActorFilter;
        assert!(
            validate_trigger(&TriggerSpec::RecordChanged {
                kind: "chat.workspace".into(),
                actions: vec![],
                fields: vec![],
                by: ActorFilter::Anyone,
            })
            .is_ok()
        );
        assert!(
            validate_trigger(&TriggerSpec::IncomingMessage {
                app_id: None,
                from: None
            })
            .is_ok()
        );
        assert!(
            validate_trigger(&TriggerSpec::RecordChanged {
                kind: " ".into(),
                actions: vec![],
                fields: vec![],
                by: ActorFilter::Anyone,
            })
            .unwrap_err()
            .contains("names the record kind")
        );
        let refused = validate_trigger(&TriggerSpec::Connector {
            app_id: "so.tilo.discord".into(),
            source: "message-created".into(),
            filter: serde_json::Map::new(),
        })
        .unwrap_err();
        assert!(refused.contains("only bots"), "{refused}");
    }
}
