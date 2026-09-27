//! Shortcuts on the action door.
//!
//! A workspace's saved [`Shortcut`]s are published by the session's action
//! broker next to the workspace's own governance actions, so every caller of
//! the door — the palette, `tilo-control`, the MCP bridge, an agent, a peer —
//! finds and runs them the same way:
//!
//! - one resource per shortcut, `shortcuts.shortcut:<id>`, carrying the
//!   action `run`, whose parameters are the shortcut's inputs and whose
//!   result is the shortcut's output;
//! - one library resource, `shortcuts`, carrying `create`, `update`,
//!   `delete` and `list`, which take and return the shortcut's JSON.
//!
//! A shortcut is saved only after [`tilo_automation::check`] accepts it
//! against the catalogue the saving person is offered at that moment. It
//! runs as the person who invoked it, inside its own grant, through the same
//! broker; each step's change carries an [`ActionContext`] grouping it under
//! the run, from which [`record_shortcut_run`] later records one
//! [`AgentRun`](ui::AgentRun) that Activity shows and can undo as a whole.
//!
//! The broker glue that serves these rows and admits these invocations lives
//! beside the rest of the broker's door (`collab_broker_capnp::action_host`);
//! this module holds what is specific to shortcuts.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use automerge::AutoCommit;
use serde_json::{Map, Value};
use settings_store::hardware::{ActionChord, ActionChordTarget, KeyChord, ShortcutConfig};
use tilo_actions::context::ActionContext;
use tilo_actions::contract::{ActionContract, PermissionRequirement, ResultSpec, RiskLevel};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::outcome::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ParamType};
use tilo_actions::remote::workspace_provider;
use tilo_actions::remote::{Confirmation, Pending, pending};
use tilo_actions::{ExecutionLocus, OperationKind};
use tilo_automation::{
    ActionCatalog, AskRequest, ConfirmationRequest, RunHooks, RunId, RunMode, RunOutcome,
    RunReport, RunTrigger, SHORTCUT_RESOURCE_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, Schedule,
    Shortcut, TriggerSpec, shortcut_id_of, shortcut_resource_id,
};

use crate::shell::automation_triggers::{Armed, readable_folder, validate_trigger};

use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

/// The resource carrying the shortcut library's actions.
pub const LIBRARY_RESOURCE: &str = "shortcuts";
/// Kind of [`LIBRARY_RESOURCE`].
pub const LIBRARY_RESOURCE_KIND: &str = "shortcuts.library";
/// Save a new shortcut.
pub const CREATE_ACTION: &str = "create";
/// Replace a saved shortcut.
pub const UPDATE_ACTION: &str = "update";
/// Delete a saved shortcut.
pub const DELETE_ACTION: &str = "delete";
/// Every shortcut saved in the workspace.
pub const LIST_ACTION: &str = "list";

/// How long a running step is left before it is observed again, when the
/// broker has nothing to push.
const SETTLE_INTERVAL: Duration = Duration::from_millis(25);

/// Finished runs kept for the history recorder; the oldest go first.
const MAX_FINISHED_RUNS: usize = 64;

/// Every descriptor `workspace` publishes for its shortcuts: the library's
/// actions, then `run` on each saved shortcut.
#[must_use]
pub fn descriptors(workspace: &Workspace) -> Vec<ActionDescriptor> {
    [CREATE_ACTION, UPDATE_ACTION, DELETE_ACTION, LIST_ACTION]
        .into_iter()
        .filter_map(library_descriptor)
        .chain(workspace.shortcuts.iter().map(run_descriptor))
        .collect()
}

/// The descriptor of library action `action`, when it is one.
#[must_use]
pub fn library_descriptor(action: &str) -> Option<ActionDescriptor> {
    let shortcut_param = || {
        let mut param = ActionParam::required(
            "shortcut",
            ParamType::Object,
            "The shortcut, as JSON: id, name, inputs, steps, output and grant",
        );
        param.schema = Some(serde_json::json!({
            "type": "object",
            "required": ["id", "name", "steps"]
        }));
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
            PermissionRequirement::Write,
            saved,
        ),
        UPDATE_ACTION => (
            "Change shortcut",
            "Replace a saved shortcut after checking it again",
            vec![shortcut_param()],
            PermissionRequirement::Write,
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
            PermissionRequirement::Write,
            ResultSpec::None,
        ),
        LIST_ACTION => (
            "List shortcuts",
            "Every shortcut saved in this workspace, as JSON",
            Vec::new(),
            PermissionRequirement::Read,
            ResultSpec::Json {
                schema: serde_json::json!({"type": "array", "items": {"type": "object"}}),
            },
        ),
        _ => return None,
    };
    Some(ActionDescriptor {
        address: ActionAddress::new(LIBRARY_RESOURCE, action),
        resource_kind: LIBRARY_RESOURCE_KIND.into(),
        resource_label: "Shortcuts".into(),
        action_label: label.into(),
        description: Some(description.into()),
        locus: ExecutionLocus::default(),
        kind: OperationKind::Effect,
        contract: ActionContract {
            permission,
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
        address: shortcut.action_address(),
        resource_kind: SHORTCUT_RESOURCE_KIND.into(),
        resource_label: shortcut.name.clone(),
        action_label: format!("Run {}", shortcut.name),
        description: Some(run_description(shortcut)),
        locus: ExecutionLocus::default(),
        kind: OperationKind::Effect,
        contract: ActionContract {
            permission: PermissionRequirement::None,
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
    if !shortcut.consents.is_empty() {
        let consents: Vec<String> = shortcut
            .consents
            .iter()
            .map(|consent| format!("`{}` on {}", consent.action, consent.resource))
            .collect();
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

/// The chords the keyboard triggers of `armed` bind, as the keymap lists
/// them.
#[must_use]
pub fn trigger_chords(armed: &[Armed<'_>]) -> Vec<ActionChord> {
    armed
        .iter()
        .flat_map(|armed| {
            armed.shortcut.triggers.iter().filter_map(move |trigger| {
                let TriggerSpec::Keyboard { chord } = trigger else {
                    return None;
                };
                Some(ActionChord {
                    chord: KeyChord::parse(chord).ok()?,
                    target: chord_target(&armed.room, &armed.shortcut.id),
                    label: format!("Run {}", armed.shortcut.name),
                })
            })
        })
        .collect()
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

/// Why `shortcut`'s triggers cannot be saved in `workspace` by `saver`,
/// with `keymap` the chords already taken on this device.
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
    saver: &str,
    keymap: &ShortcutConfig,
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
    let room = workspace.room.to_string();
    let others = &workspace.shortcuts;
    let target = chord_target(&room, &shortcut.id);
    let mut keyboard = None;
    for trigger in &shortcut.triggers {
        validate_trigger(trigger)?;
        if let TriggerSpec::FileChanged { capability } = trigger {
            readable_folder(workspace, capability, saver)?;
        }
        let TriggerSpec::Keyboard { chord: text } = trigger else {
            continue;
        };
        let chord = KeyChord::parse(text).map_err(|reason| format!("`{text}`: {reason}"))?;
        if keyboard.replace(chord.clone()).is_some() {
            return Err("a shortcut takes at most one keyboard trigger".into());
        }
        keymap
            .check_action_chord(&chord, Some(&target))
            .map_err(|error| format!("{} cannot run the shortcut: {error}", chord.display()))?;
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
}

/// Apply `operation` to workspace `workspace_id`. A saved shortcut is first
/// checked against what the saving person is offered now, and its triggers
/// against the chords already taken; it is stored as saved by them.
///
/// # Errors
///
/// Every problem the checker found, or why the workspace cannot take the
/// change.
pub fn apply_library(
    tree: &mut WorkspaceTree,
    workspace_id: WorkspaceId,
    operation: LibraryOperation,
    context: &SaveContext<'_>,
) -> Result<ActionOutput, String> {
    let workspace = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .cloned()
        .ok_or("the workspace is no longer hosted here")?;
    let saved = workspace.shortcuts.clone();
    let (shortcut, replacing) = match operation {
        LibraryOperation::List => {
            return Ok(ActionOutput {
                value: Some(serde_json::to_value(&saved).map_err(|error| error.to_string())?),
                resources: Vec::new(),
            });
        }
        LibraryOperation::Delete(id) => {
            if !tree.remove_workspace_shortcut(workspace_id, &id) {
                return Err(format!("there is no shortcut `{id}`"));
            }
            return Ok(ActionOutput::default());
        }
        LibraryOperation::Create(shortcut) => (shortcut, false),
        LibraryOperation::Update(shortcut) => (shortcut, true),
    };
    let mut shortcut = shortcut;
    shortcut.saved_by = Some(context.saver.to_string());
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
    check_triggers(&shortcut, &workspace, context.saver, context.keymap)?;
    let resolver = |id: &str| saved.iter().find(|other| other.id == id).cloned();
    tilo_automation::check(&shortcut, context.catalog, &resolver).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let resource = shortcut_resource_id(&shortcut.id);
    if !tree.save_workspace_shortcut(workspace_id, &shortcut) {
        return Err("the workspace is no longer hosted here".into());
    }
    Ok(ActionOutput {
        value: None,
        resources: vec![resource],
    })
}

/// The provenance a library invocation leaves in workspace activity.
#[must_use]
pub fn library_context(descriptor: &ActionDescriptor) -> ActionContext {
    ActionContext::for_resource(
        descriptor.action_label.clone(),
        LIBRARY_RESOURCE,
        LIBRARY_RESOURCE_KIND,
        "Shortcuts",
    )
    .with_app(SHORTCUTS_APP_ID, "Shortcuts")
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
}

impl DoorHooks {
    #[must_use]
    pub fn new(confirmation: Option<Confirmation>) -> Self {
        Self { confirmation }
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

    fn settle(&self, invocation: InvocationId) -> Pending<()> {
        settle_later(invocation)
    }
}

/// Resolve after [`SETTLE_INTERVAL`], so a running step is observed again.
fn settle_later(_invocation: InvocationId) -> Pending<()> {
    let (completer, answer) = pending();
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn(async move {
                tokio::time::sleep(SETTLE_INTERVAL).await;
                completer.complete(());
            });
        }
        Err(_) => {
            std::thread::spawn(move || {
                std::thread::sleep(SETTLE_INTERVAL);
                completer.complete(());
            });
        }
    }
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
}

impl SheetHooks {
    #[must_use]
    pub fn new(prompts: StepPrompts, shortcut: impl Into<String>) -> Self {
        Self {
            prompts,
            shortcut: shortcut.into(),
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

    fn settle(&self, invocation: InvocationId) -> Pending<()> {
        settle_later(invocation)
    }
}

/// The terminal update the door reports for a finished run.
#[must_use]
pub fn completion(invocation: InvocationId, report: &RunReport) -> InvocationUpdate {
    match &report.outcome {
        RunOutcome::Completed => InvocationUpdate::Succeeded {
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

    /// Every finished run not taken yet, oldest first.
    pub fn take(&self) -> Vec<FinishedShortcutRun> {
        self.finished
            .lock()
            .map(|mut finished| finished.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Whether a change message says the change was made by run `run`.
fn made_by_run(message: Option<&str>, run: RunId) -> Option<ActionContext> {
    let context = ActionContext::from_change_message(message?)?;
    let group = context.group.as_ref()?;
    (group.kind == "shortcut" && group.id == run.0.to_string()).then_some(context)
}

/// One document's share of a run, with when it was first changed.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentShare {
    /// Wall-clock time of the run's first change here, in milliseconds.
    pub at: i64,
    pub step: ui::RunStep,
}

/// Document `record`'s share of run `run`: the band from its first to its
/// last change grouped under the run. `None` when the run did not change it.
pub fn document_share(run: RunId, record: &str, doc: &mut AutoCommit) -> Option<DocumentShare> {
    let mut first: Option<(i64, String)> = None;
    let mut last = None;
    let mut app = String::new();
    let mut labels: Vec<String> = Vec::new();
    for version in qdoc::history::versions(doc) {
        let Some(context) = made_by_run(version.message.as_deref(), run) else {
            continue;
        };
        if first.is_none() {
            first = Some((version.time_ms, version.change_hash.clone()));
        }
        last = Some(version.change_hash.clone());
        if let Some(owner) = &context.app
            && app.is_empty()
        {
            app.clone_from(&owner.id);
        }
        if !labels.contains(&context.action_label) {
            labels.push(context.action_label.clone());
        }
    }
    let (at, start) = first?;
    let end = last?;
    Some(DocumentShare {
        at,
        step: ui::RunStep {
            app,
            record: record.to_string(),
            run: ui::ChangeRun::new(
                ui::history_source::entry_id_from_hash(&start),
                ui::history_source::entry_id_from_hash(&end),
            ),
            summary: labels.join(", "),
        },
    })
}

/// The steps of a run, in the order its documents were first changed.
#[must_use]
pub fn ordered_steps(mut shares: Vec<DocumentShare>) -> Vec<ui::RunStep> {
    shares.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.step.record.cmp(&right.step.record))
    });
    shares.into_iter().map(|share| share.step).collect()
}

/// Record `finished` in `coordinator` as one run made of `steps`, so
/// Activity lists it under the shortcut's name, attributed to whoever
/// invoked it, and can undo it whole. `None` when there are no steps: the
/// run changed nothing that can be undone. The transaction is left open for
/// the caller to commit.
pub fn record_run_steps(
    coordinator: &mut AutoCommit,
    finished: &FinishedShortcutRun,
    steps: &[ui::RunStep],
) -> Option<String> {
    let decision = format!(
        "Shortcut “{}” · {} run · {}",
        finished.shortcut.name,
        finished.report.trigger,
        match &finished.report.outcome {
            RunOutcome::Completed => "completed".to_string(),
            RunOutcome::Failed { path, failure } => format!("stopped at {path}: {failure}"),
        }
    );
    ui::record_run_with_details(
        coordinator,
        &finished.shortcut.name,
        None,
        Some(&finished.invoker),
        Some(&format!("Run shortcut “{}”", finished.shortcut.name)),
        Some(&decision),
        steps,
    )
}

/// [`record_run_steps`] with the steps found in `documents`.
pub fn record_shortcut_run(
    coordinator: &mut AutoCommit,
    finished: &FinishedShortcutRun,
    documents: &mut [(String, &mut AutoCommit)],
) -> Option<String> {
    let shares = documents
        .iter_mut()
        .filter_map(|(record, doc)| document_share(finished.report.run, record, doc))
        .collect();
    record_run_steps(coordinator, finished, &ordered_steps(shares))
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
        assert!(decode_library("rename", None).is_err());
    }

    #[test]
    fn a_workspace_publishes_its_library_and_one_run_per_shortcut() {
        let mut workspace = Workspace::new(WorkspaceId(1), "Desk");
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
                "shortcuts.shortcut:tidy#run",
            ]
        );
        let run = published.last().expect("the shortcut's run");
        assert_eq!(run.params, workspace.shortcuts[0].inputs);
        assert!(matches!(run.contract.result, ResultSpec::Json { .. }));
        assert_eq!(run_target(&run.address), Some("tidy"));
    }

    #[test]
    fn only_changes_grouped_under_the_run_become_its_steps() {
        let run = RunId::new();
        let context = |label: &str, group: &str| {
            ActionContext::for_resource(label, "notes.note:1", "notes.note", "Note")
                .with_app("so.tilo.notes", "Notes")
                .with_group(group, "Tidy", "shortcut")
                .change_message()
        };
        let mut notes = AutoCommit::new();
        let mut untouched = AutoCommit::new();
        for (key, message) in [
            ("before", None),
            ("title", context("Rename", &run.0.to_string())),
            ("body", context("Rewrite", &run.0.to_string())),
            ("other", context("Rename", "another-run")),
        ] {
            automerge::transaction::Transactable::put(&mut notes, automerge::ROOT, key, "x")
                .expect("write");
            notes.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(message.unwrap_or_else(|| "typed by hand".to_string())),
            );
        }
        automerge::transaction::Transactable::put(&mut untouched, automerge::ROOT, "k", "v")
            .expect("write");
        untouched.commit();

        assert_eq!(
            document_share(run, "file:untouched", &mut untouched),
            None,
            "a document the run did not change is no step"
        );
        let steps = ordered_steps(
            document_share(run, "file:notes", &mut notes)
                .into_iter()
                .collect(),
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].app, "so.tilo.notes");
        assert_eq!(steps[0].record, "file:notes");
        assert_eq!(steps[0].summary, "Rename, Rewrite");
        assert_ne!(steps[0].run.start, steps[0].run.end);
    }
}
