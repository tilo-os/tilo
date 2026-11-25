//! Shortcuts on the action api.
//!
//! A workspace's saved [`Shortcut`]s are published by the action broker next
//! to the workspace's own governance actions, so every caller of the API —
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
//! [`ShortcutLibraryServices`]: crate::apps::services::ShortcutLibraryServices

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Map, Value};
use settings_store::hardware::{ActionChord, ActionChordTarget, KeyChord, ShortcutConfig};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::context::ActionContext;
use tilo_actions::contract::{ActionContract, ActionKind, ResultSpec, RiskLevel};
use tilo_actions::outcome::{
    ActionFailure, ActionOutput, InvocationId as ActionInvocationId, InvocationUpdate,
};
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ParamType};
use tilo_actions::remote::asked::Approval;
use tilo_actions::remote::workspace_provider;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ExecutionLocus, OperationKind};
use tilo_automation::{
    ActionCatalog, AskRequest, AutomationInvocationId, Binding, FOCUS_DO_NOT_DISTURB,
    FOCUS_SCHEDULE, InvocationHooks, InvocationMode, InvocationOutcome, InvocationReport,
    InvocationTrigger, ResourceTarget, SHORTCUT_RESOURCE_KIND, SHORTCUT_RUN_ACTION,
    SHORTCUTS_APP_ID, Schedule, Selector, Shortcut, Step, StepKind, StepPath, StopWatch,
    TriggerSpec, shortcut_id_of, shortcut_resource_id,
};
use tilo_desktop_model::Workspace;

pub use tilo_automation::{COUNT_ACTION, LIBRARY_RESOURCE, LIBRARY_RESOURCE_KIND};
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
/// Keep a saved shortcut's triggers from firing it, until it is resumed.
pub const PAUSE_ACTION: &str = "pause";
/// Let a paused shortcut's triggers fire it again.
pub const RESUME_ACTION: &str = "resume";
/// What a draft would save as: its least grant and what the checker finds wrong with it.
pub const INSPECT_ACTION: &str = "inspect";
/// What one argument of a draft's step may be bound to, as the checker accepts it.
pub const CHOICES_ACTION: &str = "choices";

/// How long a running step is left before it is observed again, when its
/// host cannot say when it changes. The broker's own api can, so this is
/// only a fallback.
const SETTLE_INTERVAL: Duration = Duration::from_millis(250);

/// Finished runs kept for the history recorder; the oldest go first.
const MAX_FINISHED_RUNS: usize = 64;

/// Every descriptor `workspace` publishes for its shortcuts: the library's
/// actions, then `run` on each saved shortcut.
#[must_use]
pub fn descriptors(workspace: &Workspace) -> Vec<ActionDescriptor> {
    let saved = tilo_automation::decode_saved(&workspace.shortcuts);
    [
        CREATE_ACTION,
        UPDATE_ACTION,
        DELETE_ACTION,
        LIST_ACTION,
        STOP_ACTION,
        STOP_ALL_ACTION,
        PAUSE_ACTION,
        RESUME_ACTION,
        COUNT_ACTION,
        INSPECT_ACTION,
        CHOICES_ACTION,
    ]
    .into_iter()
    .filter_map(library_descriptor)
    .chain(saved.iter().map(run_descriptor))
    .collect()
}

/// The one parameter saving a shortcut takes: the shortcut itself.
fn shortcut_param() -> ActionParam {
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
}

#[must_use]
pub fn library_descriptor(action: &str) -> Option<ActionDescriptor> {
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
        PAUSE_ACTION | RESUME_ACTION => (
            if action == PAUSE_ACTION {
                "Pause shortcut"
            } else {
                "Resume shortcut"
            },
            if action == PAUSE_ACTION {
                "Keep a saved shortcut's triggers from running it; what happens meanwhile is not run later"
            } else {
                "Let a paused shortcut's triggers run it again"
            },
            vec![ActionParam::required(
                "id",
                ParamType::Text,
                "Id of the shortcut",
            )],
            ActionKind::Undoable,
            saved,
        ),
        COUNT_ACTION => (
            "Count a run",
            "How many of a shortcut's triggered runs counted, this one included, and whether that reached a multiple of every",
            vec![
                ActionParam::required("id", ParamType::Text, "Id of the shortcut"),
                ActionParam::required(
                    "every",
                    ParamType::Integer,
                    "Reached at every this many runs",
                ),
            ],
            ActionKind::Read,
            ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["count", "reached"],
                    "properties": {
                        "count": {"type": "integer"},
                        "reached": {"type": "boolean"}
                    }
                }),
            },
        ),
        INSPECT_ACTION => (
            "Check a shortcut draft",
            "What this draft would save as: the least grant its steps need, and every problem the checker finds before saving",
            vec![shortcut_param()],
            ActionKind::Read,
            ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["grant", "permissions", "problems"],
                    "properties": {
                        "grant": {"type": "array", "items": {"type": "object"}},
                        "permissions": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["label", "kind"],
                                "properties": {
                                    "label": {"type": "string"},
                                    "kind": {"type": "string"}
                                }
                            }
                        },
                        "problems": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["path", "message"],
                                "properties": {
                                    "path": {"type": "string"},
                                    "message": {"type": "string"}
                                }
                            }
                        }
                    }
                }),
            },
        ),
        CHOICES_ACTION => (
            "What a step argument may be bound to",
            "Every input, variable and earlier step output the checker accepts for one parameter of a draft's step",
            vec![
                shortcut_param(),
                ActionParam::required(
                    "path",
                    ParamType::Text,
                    "The step's position, like 0 or 2.then.1",
                ),
                ActionParam::required("param", ParamType::Text, "The parameter to bind"),
            ],
            ActionKind::Read,
            ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["binding", "describes"],
                        "properties": {
                            "binding": {},
                            "describes": {"type": "string"}
                        }
                    }
                }),
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
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(LIBRARY_RESOURCE, action),
        resource_kind: LIBRARY_RESOURCE_KIND.into(),
        resource_label: "Shortcuts".into(),
        action_label: label.into(),
        keywords: Vec::new(),
        icon: None,
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
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: shortcut.action_address(),
        resource_kind: SHORTCUT_RESOURCE_KIND.into(),
        resource_label: shortcut.name.clone(),
        action_label: format!("Run {}", shortcut.name),
        // A shortcut is found by the words its inputs and its name use, so a
        // caller choosing a tool among many sees what it asks for.
        keywords: shortcut
            .inputs
            .iter()
            .map(|input| input.name.to_string())
            .collect(),
        icon: None,
        description: Some(run_description(shortcut)),
        locus: ExecutionLocus::default(),
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Read,
            result: output_result(shortcut),
            ..ActionContract::default()
        },
        input: InputSpec::None,
        // The inputs are the parameters; what the run answers is `output`.
        params: shortcut.inputs.clone(),
        view: None,
    }
}

/// What a run of `shortcut` answers: the schema its `output` binds a value
/// of, so a caller is handed the shortcut's own typed result rather than an
/// unconstrained one. A shortcut that binds nothing answers nothing.
fn output_result(shortcut: &Shortcut) -> ResultSpec {
    shortcut
        .output
        .as_ref()
        .map_or(ResultSpec::None, |output| ResultSpec::Json {
            schema: output_schema(output, &shortcut.inputs),
        })
}

/// The JSON schema of the value `output` binds.
///
/// The value a binding produces is known type by type; one that reads an
/// earlier step is held to that step's own result when the shortcut is saved,
/// and read as text here.
fn output_schema(output: &Binding, inputs: &[ActionParam]) -> serde_json::Value {
    let of_input = |name: &str| {
        inputs
            .iter()
            .find(|input| input.name.as_ref() == name)
            .map(|input| serde_json::json!({"type": schema_type(input.ty)}))
    };
    match output {
        Binding::Literal(serde_json::Value::Object(_)) | Binding::Object(_) => {
            serde_json::json!({"type": "object"})
        }
        Binding::Literal(serde_json::Value::Array(_)) | Binding::List(_) => {
            serde_json::json!({"type": "array"})
        }
        Binding::Literal(serde_json::Value::Null) => serde_json::json!({"type": "null"}),
        Binding::Literal(serde_json::Value::Bool(_)) => serde_json::json!({"type": "boolean"}),
        Binding::Literal(serde_json::Value::Number(number)) if number.is_i64() => {
            serde_json::json!({"type": "integer"})
        }
        Binding::Literal(serde_json::Value::Number(_)) => {
            serde_json::json!({"type": "number"})
        }
        Binding::Literal(serde_json::Value::String(_)) | Binding::Template(_) => {
            serde_json::json!({"type": "string"})
        }
        Binding::Input(name) => {
            of_input(name).unwrap_or_else(|| serde_json::json!({"type": "string"}))
        }
        // A variable or an earlier step's value is held to its own type where
        // the shortcut is saved; a caller is promised text here.
        Binding::Var(_) | Binding::StepOutput { .. } => serde_json::json!({"type": "string"}),
    }
}

/// A parameter's type in JSON schema's words.
fn schema_type(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Text | ParamType::Secret | ParamType::Resource | ParamType::DateTime => "string",
        ParamType::Integer => "integer",
        ParamType::Number => "number",
        ParamType::Boolean => "boolean",
        ParamType::Object => "object",
        ParamType::Array => "array",
    }
}

/// What `run` on `shortcut` says about itself: its inputs, its steps, what
/// starts it on its own, and which confirmations its saver gave in advance.
#[must_use]
pub fn run_description(shortcut: &Shortcut) -> String {
    let mut description = format!(
        "Run the shortcut “{}”: {} step{}",
        shortcut.name,
        shortcut.steps.len(),
        if shortcut.steps.len() == 1 { "" } else { "s" }
    );
    let inputs: Vec<String> = shortcut
        .inputs
        .iter()
        .map(|input| {
            format!(
                "`{}` ({}{}): {}",
                input.name,
                input.ty.label(),
                if input.required { "" } else { ", optional" },
                input.description
            )
        })
        .collect();
    if !inputs.is_empty() {
        description.push_str(". Takes ");
        description.push_str(&inputs.join(", "));
    }
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

/// The action-api address a chord runs `shortcut_id` in `workspace` through.
#[must_use]
pub fn chord_target(workspace: &str, shortcut_id: &str) -> ActionChordTarget {
    ActionChordTarget {
        provider: workspace_provider(workspace),
        resource: shortcut_resource_id(shortcut_id),
        action: SHORTCUT_RUN_ACTION.into(),
    }
}

/// Whether `target` runs a saved shortcut.
#[must_use]
pub fn is_shortcut_target(target: &ActionChordTarget) -> bool {
    tilo_automation::runs_shortcut(&target.resource, &target.action)
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
    let workspace_id = workspace.id().to_string();
    let others = &tilo_automation::decode_saved(&workspace.shortcuts);
    let target = chord_target(&workspace_id, &shortcut.id);
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

/// The facet a shortcut run's call names the trigger that started it by, when one did: the key
/// the person pressed. Provenance its history shows, never authority.
pub const TRIGGER_FACET: &str = "trigger";

/// The context of the call that runs a shortcut `trigger` started: what its history says
/// started it, with the trigger whole (a key's chord).
#[must_use]
pub fn fired_context(trigger: &TriggerSpec) -> ActionContext {
    let mut context = ActionContext::default();
    if let Some(named) = tilo_failure::ignored!(
        "a trigger is plain data, and always reads as JSON",
        serde_json::to_string(trigger).ok()
    ) {
        context.facets.push(tilo_actions::context::ActionFacet::new(
            TRIGGER_FACET,
            named,
        ));
    }
    context
}

/// What started the run of `shortcut` that `context` belongs to: the trigger it names when the
/// shortcut has that trigger, else the person, by hand.
#[must_use]
pub fn run_trigger(
    context: Option<&ActionContext>,
    shortcut: &Shortcut,
) -> tilo_automation::InvocationTrigger {
    context
        .into_iter()
        .flat_map(|context| &context.facets)
        .filter(|facet| facet.key == TRIGGER_FACET)
        .find_map(|facet| {
            tilo_failure::ignored!(
                "a trigger that does not read is provenance lost, and the run reads as by hand",
                serde_json::from_str::<TriggerSpec>(&facet.value).ok()
            )
        })
        .filter(|trigger| shortcut.triggers.contains(trigger))
        .map_or(
            tilo_automation::InvocationTrigger::Manual,
            tilo_automation::InvocationTrigger::Fired,
        )
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
    /// Pause shortcut `id`, or resume it.
    Pause {
        id: String,
        paused: bool,
    },
    /// Count a run of shortcut `id`, reached at every `every` runs.
    Count {
        id: String,
        every: u64,
    },
    /// What the draft would save as: its least grant, and what the checker finds wrong with it.
    Inspect(Box<Shortcut>),
    /// What the parameter `param` of the step at `path` may be bound to.
    Choices {
        shortcut: Box<Shortcut>,
        path: String,
        param: String,
    },
}

/// Decode an invocation of library action `action` from its argument slot.
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
        INSPECT_ACTION => LibraryOperation::Inspect(Box::new(shortcut()?)),
        CHOICES_ACTION => LibraryOperation::Choices {
            shortcut: Box::new(shortcut()?),
            path: bound
                .text("path")
                .ok_or("`path` is required")?
                .trim()
                .to_string(),
            param: bound
                .text("param")
                .ok_or("`param` is required")?
                .trim()
                .to_string(),
        },
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
        PAUSE_ACTION | RESUME_ACTION => LibraryOperation::Pause {
            id: bound
                .text("id")
                .ok_or("`id` is required")?
                .trim()
                .to_string(),
            paused: action == PAUSE_ACTION,
        },
        COUNT_ACTION => LibraryOperation::Count {
            id: bound
                .text("id")
                .ok_or("`id` is required")?
                .trim()
                .to_string(),
            every: bound
                .to_json()
                .get("every")
                .and_then(Value::as_u64)
                .filter(|every| *every > 0)
                .ok_or("`every` is a whole number above zero")?,
        },
        _ => LibraryOperation::List,
    })
}

/// The checker's step path for an editor path such as `2.then.1`.
fn editor_path(path: &str) -> StepPath {
    let mut out = StepPath::root();
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        out = match segment {
            "then" => out.then_branch(),
            "else" => out.else_branch(),
            "each" => out.each(None),
            index => index
                .parse()
                .map_or_else(|_| out.clone(), |index| out.child(index)),
        };
    }
    out
}

/// The block and index of a step path: `2.then.1` is step 1 of block `2.then`; `0` is step 0 of
/// the top level.
fn split_path(path: &str) -> Option<(&str, usize)> {
    let (block, index) = match path.rsplit_once('.') {
        Some((block, index)) => (block, index),
        None => ("", path),
    };
    Some((block, index.parse().ok()?))
}

/// The steps of the block a block address names: `""`, `2.then`, `2.else` or `3.each`.
fn block_at<'a>(steps: &'a [Step], block: &str) -> Option<&'a [Step]> {
    if block.is_empty() {
        return Some(steps);
    }
    let (owner, branch) = block.rsplit_once('.')?;
    let step = step_at(steps, owner)?;
    match (&step.kind, branch) {
        (StepKind::If { then, .. }, "then") => Some(then),
        (StepKind::If { otherwise, .. }, "else") => Some(otherwise),
        (StepKind::ForEach { body, .. }, "each") => Some(body),
        _ => None,
    }
}

/// The step at a path such as `2.then.1`.
fn step_at<'a>(steps: &'a [Step], path: &str) -> Option<&'a Step> {
    let (block, index) = split_path(path)?;
    block_at(steps, block)?.get(index)
}

/// Who is saving a shortcut, and what it is checked against.
pub struct SaveContext<'a> {
    /// What the saving person is offered now.
    pub catalog: &'a ActionCatalog,
    /// Who saves it; a person is recorded as the shortcut's saver.
    pub saver: &'a tilo_action_policy::MemberId,
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
    /// Count a run of shortcut `id` from the firings kept done in the workspace, then answer.
    Count { id: String, every: u64 },
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
pub fn plan_library(
    workspace: &Workspace,
    operation: LibraryOperation,
    context: &SaveContext<'_>,
) -> Result<LibraryPlan, String> {
    let saved = &tilo_automation::decode_saved(&workspace.shortcuts);
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
        LibraryOperation::Count { id, every } => {
            if !saved.iter().any(|shortcut| shortcut.id == id) {
                return Err(format!("there is no shortcut `{id}`"));
            }
            return Ok(LibraryPlan::Count { id, every });
        }
        LibraryOperation::Pause { id, paused } => {
            let mut shortcut = saved
                .iter()
                .find(|shortcut| shortcut.id == id)
                .cloned()
                .ok_or_else(|| format!("there is no shortcut `{id}`"))?;
            // Pausing changes nothing the saver's proof covers: the proof stays, and so does
            // whose shortcut it is.
            shortcut.paused = paused;
            let resource = shortcut_resource_id(&shortcut.id);
            return Ok(LibraryPlan::Change {
                change: ShortcutChange::Save(Box::new(shortcut)),
                output: ActionOutput {
                    value: None,
                    resources: vec![resource],
                },
            });
        }
        LibraryOperation::Inspect(draft) => {
            let draft = *draft;
            let permissions =
                tilo_automation::grant::least_privilege(&draft.steps, context.catalog);
            let grant = tilo_automation::grant::rules(&permissions);
            let candidate = Shortcut { grant, ..draft };
            let resolver = |id: &str| saved.iter().find(|other| other.id == id).cloned();
            let problems: Vec<Value> =
                match tilo_automation::check(&candidate, context.catalog, &resolver) {
                    Ok(()) => Vec::new(),
                    Err(errors) => errors
                        .iter()
                        .map(|error| {
                            serde_json::json!({
                                "path": error.path.as_str(),
                                "message": error.to_string(),
                            })
                        })
                        .collect(),
                };
            let permissions: Vec<Value> = permissions
                .iter()
                .map(|permission| {
                    serde_json::json!({
                        "label": permission.label,
                        "kind": permission.kind.phrase(),
                    })
                })
                .collect();
            return Ok(LibraryPlan::Answer(ActionOutput {
                value: Some(serde_json::json!({
                    "grant": candidate.grant,
                    "permissions": permissions,
                    "problems": problems,
                })),
                resources: Vec::new(),
            }));
        }
        LibraryOperation::Choices {
            shortcut,
            path,
            param,
        } => {
            let shortcut = *shortcut;
            let step = step_at(&shortcut.steps, &path)
                .ok_or_else(|| format!("there is no step `{path}`"))?;
            let StepKind::Invoke {
                action, resource, ..
            } = &step.kind
            else {
                return Err(format!("step `{path}` is not an action"));
            };
            let target = match resource {
                Selector::Fixed(id) => ResourceTarget::Id(id),
                Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
                Selector::FromBinding(_) => ResourceTarget::Any,
            };
            let row = context
                .catalog
                .find(action, target)
                .ok_or("the app no longer offers this action")?;
            let param = row
                .action
                .as_ref()
                .and_then(|descriptor| {
                    descriptor
                        .params
                        .iter()
                        .find(|known| known.name.as_ref() == param.as_str())
                })
                .ok_or_else(|| format!("`{param}` is not one of its parameters"))?;
            let choices: Vec<Value> = tilo_automation::binding_options(
                &shortcut,
                context.catalog,
                &editor_path(&path),
                param,
            )
            .into_iter()
            .map(|option| {
                serde_json::json!({
                    "binding": option.binding,
                    "describes": option.describes,
                })
            })
            .collect();
            return Ok(LibraryPlan::Answer(ActionOutput {
                value: Some(serde_json::json!(choices)),
                resources: Vec::new(),
            }));
        }
        LibraryOperation::Create(shortcut) => (shortcut, false),
        LibraryOperation::Update(shortcut) => (shortcut, true),
    };
    let mut shortcut = shortcut;
    shortcut.saved_by = context.saver.person_id().map(str::to_owned);
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

/// What the count step answers for the run counted: `counted` earlier runs counted before it.
#[must_use]
pub fn count_output(counted: usize, every: u64) -> ActionOutput {
    let count = u64::try_from(counted).unwrap_or(u64::MAX).saturating_add(1);
    ActionOutput {
        value: Some(serde_json::json!({"count": count, "reached": count % every == 0})),
        resources: Vec::new(),
    }
}

/// The action a firing of shortcut `id` runs once ([`crate::work::Turn::once`]).
#[must_use]
pub fn run_address(id: &str) -> String {
    format!("shortcuts.shortcut:{id}#run")
}

/// The lease ids of the firings of shortcut `id` all start with this.
#[must_use]
pub fn firings_of(id: &str) -> String {
    let leases = crate::work::once_leases(&run_address(id));
    debug_assert!(
        leases.starts_with(tilo_automation::FIRING_PREFIX),
        "a firing's lease starts with what the automation crate reads it by"
    );
    leases
}

/// Whether the step `step` of `shortcut` is its count step: one that invokes
/// [`COUNT_ACTION`].
#[must_use]
pub fn counts(shortcut: &Shortcut, step: &str) -> bool {
    fn find<'a>(steps: &'a [tilo_automation::Step], id: &str) -> Option<&'a tilo_automation::Step> {
        steps.iter().find_map(|step| {
            if step.id == id {
                return Some(step);
            }
            match &step.kind {
                tilo_automation::StepKind::If {
                    then, otherwise, ..
                } => find(then, id).or_else(|| find(otherwise, id)),
                tilo_automation::StepKind::ForEach { body, .. } => find(body, id),
                _ => None,
            }
        })
    }
    find(&shortcut.steps, step).is_some_and(|step| {
        matches!(&step.kind, tilo_automation::StepKind::Invoke { action, .. }
            if action.app == SHORTCUTS_APP_ID
                && action.kind == LIBRARY_RESOURCE_KIND
                && action.action == COUNT_ACTION)
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

/// A broker's shortcut runs that are not over: the runs going, so one can be stopped between
/// two steps.
#[derive(Clone, Default)]
pub struct OpenInvocations {
    pub running: RunningInvocations,
}

/// The runs this broker has going, by the workspace and the shortcut each runs.
///
/// A run registers itself before its first step and leaves when it ends;
/// A run of a shortcut going on from where an earlier one got to: the run
/// that happened, and the step of it to go on from.
#[derive(Clone, Debug, PartialEq)]
pub struct Continued {
    pub report: InvocationReport,
    pub from: StepPath,
}

/// stopping one marks it, and the interpreter asks before every step that
/// would invoke something. What a stopped run already did stands, and is
/// recorded and undone like a completed run's.
#[derive(Clone, Default)]
pub struct RunningInvocations {
    running: std::sync::Arc<Mutex<Vec<RunningInvocation>>>,
}

/// One run going, as a stop names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningInvocation {
    pub workspace: String,
    pub shortcut: String,
    /// What the shortcut is called, for a person reading the answer.
    pub name: String,
    pub invocation: AutomationInvocationId,
    /// The step the run has reached, as it last said where it was.
    pub at: Option<String>,
    /// Why it is being stopped, once someone has.
    pub stopped: Option<String>,
}

impl RunningInvocations {
    /// Note a run as going, before its first step.
    pub fn register(&self, invocation: RunningInvocation) {
        if let Ok(mut running) = self.running.lock() {
            running.retain(|other| other.invocation != invocation.invocation);
            running.push(invocation);
        }
    }

    /// Note a run as over.
    pub fn leave(&self, invocation: AutomationInvocationId) {
        if let Ok(mut running) = self.running.lock() {
            running.retain(|other| other.invocation != invocation);
        }
    }

    /// Note where run `run` has reached. A stop only lands while the run is
    /// between two answers, so the last place it named is where it ends.
    fn moved(&self, invocation: AutomationInvocationId, at: &str) {
        if let Ok(mut running) = self.running.lock()
            && let Some(running) = running
                .iter_mut()
                .find(|other| other.invocation == invocation)
        {
            running.at = Some(at.to_owned());
        }
    }

    /// Whether run `run` has been told to stop, and why.
    fn stopped(&self, invocation: AutomationInvocationId) -> Option<String> {
        self.running
            .lock()
            .ok()?
            .iter()
            .find(|other| other.invocation == invocation)?
            .stopped
            .clone()
    }

    /// Stop the runs `request` names, in `workspace`. Returns those it stopped;
    /// a run that is not going is not an error, since stopping one that has
    /// just finished is what someone asking for it wanted anyway.
    pub fn stop(
        &self,
        workspace: &str,
        request: &StopRequest,
        reason: &str,
    ) -> Vec<RunningInvocation> {
        let Some(mut running) = self.running.lock().ok() else {
            return Vec::new();
        };
        let named = |invocation: &RunningInvocation| {
            invocation.workspace == workspace
                && match request {
                    StopRequest::Shortcut(id) => invocation.shortcut == *id,
                    StopRequest::All => true,
                }
        };
        // Only a run still going is stopped; one already told to stop is
        // left to end, and asking again names nothing new.
        let mut stopping = Vec::new();
        for invocation in running.iter_mut().filter(|invocation| named(invocation)) {
            if invocation.stopped.is_none() {
                invocation.stopped = Some(reason.to_owned());
                stopping.push(invocation.clone());
            }
        }
        stopping
    }

    /// Every run going in `workspace`.
    pub fn going(&self, workspace: &str) -> Vec<RunningInvocation> {
        self.running
            .lock()
            .map(|running| {
                running
                    .iter()
                    .filter(|invocation| invocation.workspace == workspace)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The shortcuts a run about to start in `workspace` is told are running
    /// above it: the shortcut it is being started for, plus the shortcut of
    /// every run going in the workspace whose own id is the group its steps will
    /// carry (`parent`), which is how a run reads the shortcut that called
    /// it.
    ///
    /// A step's [`InvokeRequest`](tilo_actions::remote::InvokeRequest) names
    /// its group, and a host starts a run called that way with that group as
    /// its `parent`, so the runs going inside the caller's group are the
    /// calls it made. Two shortcuts that call each other are therefore one
    /// run inside the other and the inner one is refused; two runs a person
    /// started side by side carry their own groups and are not each other's
    /// parents, and neither is a run in another workspace.
    #[must_use]
    pub fn above(&self, workspace: &str, parent: Option<&str>, own: &str) -> Vec<String> {
        let Ok(running) = self.running.lock() else {
            return Vec::new();
        };
        let mut above: Vec<String> = running
            .iter()
            .filter(|invocation| invocation.workspace == workspace)
            .filter(|invocation| {
                parent.is_some_and(|parent| parent == invocation.invocation.0.to_string())
            })
            .map(|invocation| invocation.shortcut.clone())
            .collect();
        above.push(own.to_owned());
        above
    }
}

/// What stopping says it stopped, for whoever asked: one entry per run it
/// reached, with where each had got to.
#[must_use]
pub fn stopped_output(stopped: &[RunningInvocation]) -> ActionOutput {
    let runs: Vec<Value> = stopped
        .iter()
        .map(|invocation| {
            serde_json::json!({
                "shortcut": invocation.shortcut,
                "name": invocation.name,
                "reached": invocation.at,
            })
        })
        .collect();
    ActionOutput {
        value: Some(Value::Array(runs)),
        resources: Vec::new(),
    }
}

/// How a run started through the API treats a person-shaped question.
///
/// A run the person approved ([`Approval`], its call's asked record answered) is interactive:
/// their approve is carried to each step that needs their yes, having been shown each one
/// before anything ran ([`tilo_automation::asks_before_running`]). Without it the run is
/// unattended, so a step that needs a yes nobody consented to in advance, or that asks a
/// question, stops the run with a failure that says so rather than guessing.
#[must_use]
pub fn run_mode(approval: Option<&Approval>) -> InvocationMode {
    if approval.is_some() {
        InvocationMode::Interactive
    } else {
        InvocationMode::Unattended
    }
}

/// How one run starts.
#[derive(Clone, Debug)]
pub struct InvocationStart {
    pub invocation: AutomationInvocationId,
    pub trigger: InvocationTrigger,
    pub mode: InvocationMode,
    /// Values for the shortcut's inputs.
    pub inputs: Map<String, Value>,
    /// The work unit this run belongs to when something else asked for it:
    /// a task that called the shortcut, named by its own group. The run's
    /// steps are stamped with it, so the run shows up inside that task rather
    /// than beside it, and undoing the task undoes what it asked for.
    pub parent: Option<tilo_actions::context::WorkGroup>,
    /// The person's approve of the asked record the run's call was kept as.
    pub approval: Option<Approval>,
}

/// [`InvocationHooks`] for a run started through the API.
pub struct ApiHooks {
    /// The runtime the run's poll fallback waits on.
    runtime: tokio::runtime::Handle,
    /// The runs going, so one of them can be stopped from elsewhere.
    invocations: RunningInvocations,
    invocation: AutomationInvocationId,
}

impl ApiHooks {
    #[must_use]
    pub fn new(
        runtime: tokio::runtime::Handle,
        invocations: RunningInvocations,
        invocation: AutomationInvocationId,
    ) -> Self {
        Self {
            runtime,
            invocations,
            invocation,
        }
    }
}

impl InvocationHooks for ApiHooks {
    fn ask(&self, _request: AskRequest) -> Pending<Option<Value>> {
        // Nobody on the API can be asked mid-run; declining stops the run
        // at the question instead of inventing an answer.
        Pending::ready(None)
    }

    fn settle(&self, _invocation: ActionInvocationId) -> Pending<()> {
        settle_later(&self.runtime)
    }

    fn stopped(&self, what: StopWatch<'_>) -> Pending<Result<(), String>> {
        self.invocations.moved(self.invocation, what.path.as_str());
        Pending::ready(match self.invocations.stopped(self.invocation) {
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

/// The terminal update the API reports for a finished run.
#[must_use]
pub fn completion(invocation: ActionInvocationId, report: &InvocationReport) -> InvocationUpdate {
    match &report.outcome {
        // A stopped run did not fail: it ends where it was told to. What it
        // had already done is recorded and can be undone as one step, so it
        // answers like a completed run, with the report saying it stopped.
        // A run that finished degraded still succeeded: the steps that
        // fell short are recorded in the report, and carry the recovery the
        // shortcut asked for, so nobody mistakes it for an untouched run.
        InvocationOutcome::Completed | InvocationOutcome::Cancelled { .. } => {
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: report.reported_value(),
                    resources: Vec::new(),
                },
            }
        }
        InvocationOutcome::Failed { path, failure } => InvocationUpdate::Failed {
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

/// One run the API finished, waiting to be recorded in history.
#[derive(Clone, Debug)]
pub struct FinishedInvocation {
    /// The workspace the shortcut is saved in.
    pub workspace: String,
    pub shortcut: Shortcut,
    /// The person or agent who invoked it.
    pub invoker: tilo_action_policy::MemberId,
    pub report: InvocationReport,
}

/// The shortcut runs the broker holds while they go and once they end: a run going (and the
/// steps waiting on its person) is stopped through here, and an ended one waits here until history
/// takes it.
#[derive(Default)]
pub struct BrokerInvocations {
    pub finished: FinishedInvocations,
    pub open: OpenInvocations,
}

/// Runs the broker finished, until the history recorder takes them.
#[derive(Default)]
pub struct FinishedInvocations {
    finished: Mutex<VecDeque<FinishedInvocation>>,
    /// The same runs, kept by id after they were taken, newest last.
    kept: Mutex<VecDeque<FinishedInvocation>>,
}

impl FinishedInvocations {
    pub fn push(&self, invocation: FinishedInvocation) {
        if let Ok(mut kept) = self.kept.lock() {
            kept.push_back(invocation.clone());
            while kept.len() > MAX_FINISHED_RUNS {
                kept.pop_front();
            }
        }
        if let Ok(mut finished) = self.finished.lock() {
            finished.push_back(invocation);
            while finished.len() > MAX_FINISHED_RUNS {
                finished.pop_front();
            }
        }
    }

    /// The run `run` produced of `shortcut`, when it is one this broker kept.
    #[must_use]
    pub fn report(
        &self,
        invocation: AutomationInvocationId,
        shortcut: &str,
    ) -> Option<InvocationReport> {
        self.kept
            .lock()
            .ok()?
            .iter()
            .rev()
            .find(|finished| {
                finished.report.invocation == invocation && finished.shortcut.id == shortcut
            })
            .map(|finished| finished.report.clone())
    }

    /// Whether a finished run is waiting to be taken.
    pub fn has_finished(&self) -> bool {
        self.finished
            .lock()
            .is_ok_and(|finished| !finished.is_empty())
    }

    /// Every finished run not taken yet, oldest first.
    pub fn take(&self) -> Vec<FinishedInvocation> {
        self.finished
            .lock()
            .map(|mut finished| finished.drain(..).collect())
            .unwrap_or_default()
    }
}

/// The shortest interval a schedule trigger may repeat at.
pub const MIN_INTERVAL_SECONDS: u64 = tilo_automation::schedule::MIN_INTERVAL_SECONDS;

/// Why a trigger cannot be saved on a shortcut, when it cannot.
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
        TriggerSpec::ResourceChanged { kind, .. } if kind.trim().is_empty() => {
            Err("a record trigger names the record kind it watches".into())
        }
        TriggerSpec::AppEvent { app_id, .. } if app_id.trim().is_empty() => {
            Err("an app-event trigger names the application that declares the event".into())
        }
        TriggerSpec::AppEvent { kind, .. } if kind.trim().is_empty() => {
            Err("an app-event trigger names the event the application declares".into())
        }
        TriggerSpec::Connector { app_id, .. } if app_id.trim().is_empty() => {
            Err("a connector trigger names the app whose connector reports the event".into())
        }
        TriggerSpec::Connector { source, .. } if source.trim().is_empty() => {
            Err("a connector trigger names the source the event comes from".into())
        }
        // The times of a person's calendars are kept by the Calendar app and read there, not by
        // the host that runs this shortcut, so a moment counted from one cannot be armed here.
        TriggerSpec::ResourceTime { .. } => Err(
            "a moment counted from a calendar entry is not available here: the Calendar app \
             reads its own entries"
                .into(),
        ),
        TriggerSpec::SourceChanged {
            kind,
            at,
            every_secs,
            ..
        } => {
            use tilo_automation::source_change::{self, WEB_FEED, WEB_PAGE};
            match source_change::refusal(kind, at)
                .or_else(|| source_change::look_refusal(kind, at, *every_secs))
            {
                Some(why) => Err(why),
                // A feed or a page on the web is looked at on a clock; no other source is read.
                None if !source_change::looked_at(kind) => Err(format!(
                    "something new is found in a feed on the web (`when something new is in \
                     \"{WEB_FEED}\" at \"https://example.org/feed.xml\"`) or on a page there \
                     (`when something new is in \"{WEB_PAGE}\" at \"https://example.org/prices\"`)"
                )),
                None => Ok(()),
            }
        }
        TriggerSpec::Once { .. }
        | TriggerSpec::AppOpened { .. }
        | TriggerSpec::WorkspaceJoined { .. }
        | TriggerSpec::FileChanged { .. }
        | TriggerSpec::FocusMode { .. }
        | TriggerSpec::ResourceChanged { .. }
        | TriggerSpec::IncomingMessage { .. }
        | TriggerSpec::AppEvent { .. }
        | TriggerSpec::Connector { .. }
        | TriggerSpec::Webhook { .. } => Ok(()),
        other => Err(format!(
            "`{}` triggers cannot be saved yet; keyboard, schedule, moment, \
             app-opened, workspace-joined, file-changed, focus-mode, record-changed, \
             incoming-message, app-event, connector, feed and page triggers can",
            other.kind()
        )),
    }
}

/// The folder capability `capability` of `workspace` shares, when `saver`
/// may read through it.
pub fn readable_folder(
    workspace: &Workspace,
    capability: &str,
    saver: &tilo_action_policy::MemberId,
) -> Result<std::path::PathBuf, String> {
    let granted = workspace
        .sandbox
        .iter()
        .find(|granted| granted.id == capability)
        .ok_or_else(|| format!("the workspace has no capability `{capability}`"))?;
    let collab_types::Scope::FsPath(path) = &granted.scope else {
        return Err(format!("capability `{capability}` does not share a folder"));
    };
    let Some(saver) = saver.person_id().filter(|saver| {
        workspace.policy.role_of(saver).is_some() && !workspace.policy.is_blocked(saver)
    }) else {
        return Err("the saver is not a member of the workspace".into());
    };
    let acting = tilo_action_policy::Acting::person(saver);
    if collab_types::caps::decide_capability(
        &acting,
        &collab_types::ResourceRef::FsPath(path.clone()),
        collab_types::caps::Access::Read,
        &workspace.sandbox,
        Some(tilo_clock::unix_now().saturating_mul(1000)),
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
    use tilo_actions::remote::{CatalogueResource, CatalogueRow};
    use tilo_automation::SavedShortcut;

    fn shortcut(json: Value) -> Shortcut {
        serde_json::from_value(json).expect("shortcut JSON")
    }

    /// One action a catalogue offers on a resource of an app.
    fn row(
        app: &str,
        resource: &str,
        kind: &str,
        action: &str,
        params: Vec<ActionParam>,
        contract: ActionContract,
    ) -> CatalogueRow {
        CatalogueRow {
            offered: None,
            provider: format!("{app}#1"),
            app_id: app.into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.into(),
                kind: kind.into(),
                label: resource.into(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: ActionAddress::new(resource, action),
                resource_kind: kind.into(),
                resource_label: resource.into(),
                action_label: action.into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: ExecutionLocus::ClientLocal,
                kind: OperationKind::Document,
                contract,
                input: InputSpec::None,
                params,
                view: None,
            }),
            has_children: false,
            visibility: None,
            surface: None,
        }
    }

    fn notes_catalog() -> ActionCatalog {
        ActionCatalog::new(vec![row(
            "so.tilo.notes",
            "notes.note:1",
            "notes.note",
            "write",
            vec![ActionParam::required(
                "body",
                ParamType::Text,
                "What to write",
            )],
            ActionContract {
                kind: ActionKind::Undoable,
                ..ActionContract::default()
            },
        )])
    }

    fn draft() -> Shortcut {
        shortcut(serde_json::json!({
            "id": "draft",
            "name": "Draft",
            "inputs": [{"name": "body", "ty": "text", "required": true, "description": "What to write"}],
            "steps": [{
                "id": "write",
                "step": "invoke",
                "action": {"app": "so.tilo.notes", "kind": "notes.note", "action": "write"},
                "resource": {"fixed": "notes.note:1"},
                "args": {}
            }]
        }))
    }

    tilo_test_env::scenario! {
        fn a_draft_is_inspected_and_what_its_arguments_may_be_is_listed;

        phase inspecting_a_draft_answers_its_least_grant_and_its_problems;
        {
            let slot = |value: Value| format!("{ARGUMENTS_JSON_PREFIX}{value}");
            let draft = draft();
            let operation = decode_library(
                INSPECT_ACTION,
                Some(&slot(serde_json::json!({"shortcut": draft.clone()}))),
            )
            .expect("the draft inspects");
            let catalog = notes_catalog();
            let keymap = ShortcutConfig::default();
            let workspace = Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Office");
            let saver = tilo_action_policy::MemberId::person("ada");
            let plan = plan_library(
                &workspace,
                operation,
                &SaveContext {
                    catalog: &catalog,
                    saver: &saver,
                    keymap: &keymap,
                    fixed_label: &|_| String::new(),
                },
            )
            .expect("the draft is checked");
            let LibraryPlan::Answer(output) = plan else {
                panic!("inspection changes nothing: {plan:?}");
            };
            let value = output.value.expect("its answer");
            assert!(
                value["grant"].as_array().is_some_and(|rules| !rules.is_empty()),
                "the step's least grant: {value}"
            );
            let permissions = value["permissions"].as_array().expect("permissions");
            assert!(
                permissions.iter().any(|permission| permission["label"]
                    .as_str()
                    .is_some_and(|label| label.contains("write"))),
                "the grant as a person reads it: {value}"
            );
            let problems = value["problems"].as_array().expect("problems");
            assert!(
                problems.iter().any(|problem| problem["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("body"))),
                "the unfilled parameter is named: {value}"
            );
        }

        phase the_choices_for_an_argument_are_what_the_checker_accepts;
        {
            let slot = |value: Value| format!("{ARGUMENTS_JSON_PREFIX}{value}");
            let operation = decode_library(
                CHOICES_ACTION,
                Some(&slot(serde_json::json!({
                    "shortcut": draft(),
                    "path": "0",
                    "param": "body",
                }))),
            )
            .expect("the choices are asked");
            let catalog = notes_catalog();
            let keymap = ShortcutConfig::default();
            let workspace = Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Office");
            let saver = tilo_action_policy::MemberId::person("ada");
            let plan = plan_library(
                &workspace,
                operation,
                &SaveContext {
                    catalog: &catalog,
                    saver: &saver,
                    keymap: &keymap,
                    fixed_label: &|_| String::new(),
                },
            )
            .expect("the choices are read");
            let LibraryPlan::Answer(output) = plan else {
                panic!("choices change nothing: {plan:?}");
            };
            let value = output.value.expect("its answer");
            assert!(
                value.as_array().is_some_and(|choices| choices.iter().any(
                    |choice| choice["binding"] == serde_json::json!({"input": "body"})
                )),
                "the shortcut's input is a choice: {value}"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_workspaces_shortcut_library_is_published_typed_and_decoded;

        phase library_arguments_decode_into_operations;
        {
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

        phase a_workspace_publishes_its_library_and_one_run_per_shortcut;
        {
            let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Office");
            workspace.shortcuts.push(SavedShortcut::encode(&shortcut(serde_json::json!({
                "id": "tidy",
                "name": "Tidy",
                "inputs": [{"name": "folder", "ty": "text", "required": true, "description": "Where"}],
                "steps": [],
                "output": {"input": "folder"}
            }))));
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
                    "shortcuts#pause",
                    "shortcuts#resume",
                    "shortcuts#count",
                    "shortcuts#inspect",
                    "shortcuts#choices",
                    "shortcuts.shortcut:tidy#run",
                ]
            );
            let run = published.last().expect("the shortcut's run");
            assert_eq!(run.params, workspace.shortcuts[0].decode().unwrap().inputs);
            assert!(matches!(run.contract.result, ResultSpec::Json { .. }));
            assert_eq!(run_target(&run.address), Some("tidy"));
        }

        phase a_shortcut_reads_as_a_typed_tool_with_its_inputs_and_output;
        // What a task chooses a tool by: the words its inputs and its name use,
        // what it answers, and what it says it takes.
        {
            let folder = ActionParam::required("folder", ParamType::Text, "Where the receipts are");
            let limit = ActionParam::optional("week", ParamType::Integer, "Which week, 0 for this one");
            let tidy = shortcut(serde_json::json!({
                "id": "file-receipts",
                "name": "File receipts",
                "inputs": [&folder, &limit],
                "steps": [],
                "output": {"literal": {"filed": 3}}
            }));
            let run = run_descriptor(&tidy);
            assert_eq!(
                run.params,
                vec![folder.clone(), limit.clone()],
                "its declared inputs, typed"
            );
            assert_eq!(run.keywords, vec!["folder".to_owned(), "week".to_owned()]);
            assert_eq!(
                run.contract.result,
                ResultSpec::Json {
                    schema: serde_json::json!({"type": "object"})
                },
                "a caller is promised the shape the shortcut answers"
            );
            let description = run.description.expect("a shortcut says what it offers");
            assert!(description.contains("`folder` (text)"), "{description}");
            assert!(
                description.contains("Where the receipts are"),
                "{description}"
            );
            assert!(
                description.contains("`week` (integer, optional)"),
                "{description}"
            );

            // An output that hands back one of its inputs answers with that
            // input's own type.
            let echo = shortcut(serde_json::json!({
                "id": "echo",
                "name": "Echo the week",
                "inputs": [&folder, &limit],
                "steps": [],
                "output": {"input": "week"}
            }));
            assert_eq!(
                run_descriptor(&echo).contract.result,
                ResultSpec::Json {
                    schema: serde_json::json!({"type": "integer"})
                }
            );

            let silent = shortcut(serde_json::json!({
                "id": "tidy",
                "name": "Tidy",
                "steps": []
            }));
            assert_eq!(
                run_descriptor(&silent).contract.result,
                ResultSpec::None,
                "a shortcut that binds no output answers none"
            );
        }
    }

    fn going(workspace: &str, shortcut: &str, name: &str) -> RunningInvocation {
        RunningInvocation {
            workspace: workspace.into(),
            shortcut: shortcut.into(),
            name: name.into(),
            invocation: AutomationInvocationId::new(),
            at: None,
            stopped: None,
        }
    }

    tilo_test_env::scenario! {
        fn runs_going_are_named_told_and_stopped;

        phase stopping_names_one_shortcuts_runs_or_every_run_going_in_the_workspace;
        {
            let runs = RunningInvocations::default();
            let tidy = going("office", "tidy", "Tidy");
            let sweep = going("office", "sweep", "Sweep");
            let elsewhere = going("study", "tidy", "Tidy");
            for run in [&tidy, &sweep, &elsewhere] {
                runs.register(run.clone());
            }
            runs.moved(tidy.invocation, "1");
            let stopped = runs.stop("office", &StopRequest::Shortcut("tidy".into()), "asked");
            assert_eq!(stopped.len(), 1);
            assert_eq!(stopped[0].shortcut, "tidy");
            assert_eq!(stopped[0].at.as_deref(), Some("1"));
            // The other workspace's run of the same shortcut is somebody else's.
            assert!(runs.stopped(elsewhere.invocation).is_none());
            assert!(runs.stopped(sweep.invocation).is_none());

            // Stopping everything names what is still going, not the run already
            // told to stop.
            let all = runs.stop("office", &StopRequest::All, "asked");
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].shortcut, "sweep");
            assert!(runs.stopped(sweep.invocation).is_some());
            assert!(runs.stopped(tidy.invocation).is_some());
            // Asking again stops nothing new: a run is told once.
            assert!(
                runs.stop("office", &StopRequest::All, "asked again")
                    .is_empty()
            );
            assert_eq!(runs.going("office").len(), 2);
            runs.leave(tidy.invocation);
            assert_eq!(runs.going("office").len(), 1);
        }

        phase a_run_is_told_the_shortcuts_going_above_it_and_the_ones_beside_it_are_not;
        {
            let runs = RunningInvocations::default();
            // `outer` is going on its own group, as a run with nothing above it
            // does; the run its step calls is stamped with that group.
            let outer = going("office", "outer", "Outer");
            let group = outer.invocation.0.to_string();
            runs.register(outer.clone());
            // The call inside it: its own id is running, and so is `outer`'s.
            assert_eq!(
                runs.above("office", Some(&group), "inner"),
                ["outer", "inner"]
            );
            // Another workspace's outer run above the same group is somebody else's.
            runs.register(going("study", "other", "Other"));
            assert_eq!(
                runs.above("office", Some(&group), "inner"),
                ["outer", "inner"]
            );
            // A run a person started has no parent naming another run, so it is
            // told only of itself: two runs side by side do not block each other.
            assert_eq!(runs.above("office", None, "beside"), ["beside"]);
            assert_eq!(runs.above("office", Some("elsewhere"), "beside"), ["beside"]);
            // Once the outer run is over, a run its group names has nothing
            // above it.
            runs.leave(outer.invocation);
            assert_eq!(runs.above("office", Some(&group), "inner"), ["inner"]);
        }

        phase a_stop_says_which_runs_it_reached_and_where_each_had_got_to;
        {
            let runs = RunningInvocations::default();
            let tidy = going("office", "tidy", "Tidy");
            runs.register(tidy.clone());
            runs.moved(tidy.invocation, "then.2");
            let output = stopped_output(&runs.stop("office", &StopRequest::All, "asked"));
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
    }

    tilo_test_env::scenario! {
        fn a_trigger_is_validated_by_what_it_watches;

        phase an_app_event_trigger_names_the_app_and_the_event;
        {
            assert!(
                validate_trigger(&TriggerSpec::AppEvent {
                    app_id: "so.tilo.mail".into(),
                    kind: "mail.message".into(),
                })
                .is_ok()
            );
            assert!(
                validate_trigger(&TriggerSpec::AppEvent {
                    app_id: " ".into(),
                    kind: "mail.message".into(),
                })
                .unwrap_err()
                .contains("names the application")
            );
            assert!(
                validate_trigger(&TriggerSpec::AppEvent {
                    app_id: "so.tilo.mail".into(),
                    kind: " ".into(),
                })
                .unwrap_err()
                .contains("names the event")
            );
        }

        phase a_persons_shortcut_may_watch_records_messages_and_connectors;
        {
            use tilo_automation::ActorFilter;
            assert!(
                validate_trigger(&TriggerSpec::ResourceChanged {
                    kind: "chat.workspace".into(),
                    actions: vec![],
                    fields: vec![],
                    by: ActorFilter::Anyone,
                    filter: tilo_automation::ChangeFilter::default(),
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
                validate_trigger(&TriggerSpec::ResourceChanged {
                    kind: " ".into(),
                    actions: vec![],
                    fields: vec![],
                    by: ActorFilter::Anyone,
                    filter: tilo_automation::ChangeFilter::default(),
                })
                .unwrap_err()
                .contains("names the record kind")
            );
            assert!(
                validate_trigger(&TriggerSpec::Connector {
                    app_id: "so.tilo.discord".into(),
                    source: "message-created".into(),
                    filter: serde_json::Map::new(),
                })
                .is_ok()
            );
            assert!(
                validate_trigger(&TriggerSpec::Connector {
                    app_id: " ".into(),
                    source: "message-created".into(),
                    filter: serde_json::Map::new(),
                })
                .unwrap_err()
                .contains("names the app")
            );
            assert!(
                validate_trigger(&TriggerSpec::Connector {
                    app_id: "so.tilo.discord".into(),
                    source: " ".into(),
                    filter: serde_json::Map::new(),
                })
                .unwrap_err()
                .contains("names the source")
            );
        }
    }
}
