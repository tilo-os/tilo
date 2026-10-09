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
//! A shortcut is saved only after [`tilo_automation::check`] accepts it
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
use tilo_actions::contract::{ActionContract, PermissionRequirement, ResultSpec, RiskLevel};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::outcome::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ParamType};
use tilo_actions::remote::workspace_provider;
use tilo_actions::remote::{Confirmation, Pending, pending};
use tilo_actions::{ExecutionLocus, OperationKind};
use tilo_automation::{
    ActionCatalog, AskRequest, ConfirmationRequest, FOCUS_DO_NOT_DISTURB, FOCUS_SCHEDULE, RunHooks,
    RunId, RunMode, RunOutcome, RunReport, RunTrigger, SHORTCUT_RESOURCE_KIND, SHORTCUT_RUN_ACTION,
    SHORTCUTS_APP_ID, Schedule, Shortcut, TriggerSpec, shortcut_id_of, shortcut_resource_id,
};
use tilo_desktop_model::Workspace;

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
        semantic_identity: None,
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
        semantic_identity: None,
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
    let room = workspace.room.to_string();
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
    /// What the desktop calls a fixed shortcut, in the person's language.
    pub fixed_label: &'a dyn Fn(&settings_store::keyboard_shortcuts::FixedShortcut) -> String,
}

/// A shortcut to store, or remove, in one workspace.
#[derive(Clone, Debug, PartialEq)]
pub enum ShortcutChange {
    /// Save this shortcut, replacing a saved shortcut with the same id.
    Save(Box<Shortcut>),
    /// Remove the saved shortcut with this id.
    Delete(String),
}

/// What one library invocation does: answer from what is saved, or change it.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryPlan {
    /// Answer without changing anything.
    Answer(ActionOutput),
    /// Store `change`, then answer with `output`.
    Change {
        change: ShortcutChange,
        output: ActionOutput,
    },
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
}

impl DoorHooks {
    #[must_use]
    pub fn new(confirmation: Option<Confirmation>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            confirmation,
            runtime,
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
}

impl SheetHooks {
    #[must_use]
    pub fn new(
        prompts: StepPrompts,
        shortcut: impl Into<String>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            prompts,
            shortcut: shortcut.into(),
            runtime,
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

    fn settle(&self, _invocation: InvocationId) -> Pending<()> {
        settle_later(&self.runtime)
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
        TriggerSpec::AppOpened { .. }
        | TriggerSpec::WorkspaceJoined { .. }
        | TriggerSpec::FileChanged { .. }
        | TriggerSpec::FocusMode { .. } => Ok(()),
        other => Err(format!(
            "`{}` triggers cannot be saved yet; keyboard, schedule, app-opened, \
             workspace-joined, file-changed and focus-mode triggers can",
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
    use crate::policy::WorkspacePolicy as _;

    let granted = workspace
        .capabilities
        .iter()
        .find(|granted| granted.id == capability)
        .ok_or_else(|| format!("the workspace has no capability `{capability}`"))?;
    let collab_types::Scope::FsPath(path) = &granted.scope else {
        return Err(format!("capability `{capability}` does not share a folder"));
    };
    let role = workspace
        .role_of(&saver.to_string())
        .ok_or("the saver is not a member of the workspace")?;
    let actor = collab_types::Actor::person(Some(role), saver).in_room(workspace.room.to_string());
    if !workspace.allows(
        &actor,
        &collab_types::ResourceRef::FsPath(path.clone()),
        collab_types::Verb::Read,
    ) {
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
        assert!(decode_library("rename", None).is_err());
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
                "shortcuts.shortcut:tidy#run",
            ]
        );
        let run = published.last().expect("the shortcut's run");
        assert_eq!(run.params, workspace.shortcuts[0].inputs);
        assert!(matches!(run.contract.result, ResultSpec::Json { .. }));
        assert_eq!(run_target(&run.address), Some("tidy"));
    }
}
