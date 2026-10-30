//! The desktop's layout actions, served to agents and apps through this
//! device's action broker.
//!
//! `so.tilo.desktop`'s `open`, `create-strip`, `rename-strip`, `move-group`,
//! `arrange`, `list-windows` and `undo` on [`RESOURCE`] name windows, groups
//! and strips by id, so an agent can lay out what it opened without a
//! focused window to act on. The broker admits each like any action — the
//! caller's grants decide, its receipt goes to the workspace's history under
//! the agent and the person it acts for — and this door runs it on the
//! thread that draws the desktop when the loop next pumps ([`take_jobs`]),
//! through the same functions the command palette's window actions use
//! ([`crate::shell::desktop_layout`]).
//!
//! [`RESOURCE`]: tilo_desktop_model::commands::RESOURCE

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde_json::{Map, Value, json};
use tilo_action_broker::services::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionParam,
    ExecutionLocus, InputSpec, OperationKind, ParamType,
};
use tilo_desktop_model::commands::{APP_ID, RESOURCE, layout};

use crate::shell::desktop_layout::{Actor, Layout, Place, Target};
use crate::shell::i18n::t;
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{RoomId, StripId};

/// One request, run on the desktop's thread.
pub(crate) type Job = Box<dyn FnOnce(&mut ShellOverlayHost) + Send>;

/// How a request ran: answered now, or once the window it opened is up.
pub(crate) enum Served {
    Now(HostStepOutcome),
    /// The answer is `value`, with the provider the window `tab` publishes
    /// its own actions under, once it does.
    WhenUp {
        tab: u64,
        value: Value,
    },
}

/// An `open` waiting for its window: answered by the window publishing its
/// actions (and, for a resource, being opened at it), its place closing, or
/// its launch failing ([`take_parked`]).
pub(crate) struct Parked {
    pub tab: u64,
    pub value: Value,
    pub answer: tilo_actions::remote::Completer<HostStepOutcome>,
}

/// How the broker's threads reach the desktop.
struct Port {
    jobs: Mutex<Vec<Job>>,
    parked: Mutex<Vec<Parked>>,
    /// Wakes the thread drawing the desktop to run them.
    wake: Mutex<Option<crate::session_runtime::Wake>>,
}

fn port() -> &'static Port {
    static PORT: OnceLock<Port> = OnceLock::new();
    PORT.get_or_init(|| Port {
        jobs: Mutex::new(Vec::new()),
        parked: Mutex::new(Vec::new()),
        wake: Mutex::new(None),
    })
}

/// Wake the thread drawing the desktop with `wake` whenever the broker asks
/// it something.
pub(crate) fn wake_with(wake: crate::session_runtime::Wake) {
    *port().wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
}

/// What the broker asked of the desktop since the last pump.
pub(crate) fn take_jobs() -> Vec<Job> {
    std::mem::take(&mut *port().jobs.lock().unwrap_or_else(PoisonError::into_inner))
}

/// The `open`s waiting for their windows; whoever takes them puts back
/// those still waiting ([`park`]).
pub(crate) fn take_parked() -> Vec<Parked> {
    std::mem::take(&mut *port().parked.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Wait for `parked`'s window again.
pub(crate) fn park(parked: Parked) {
    port()
        .parked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(parked);
}

/// Run `work` on the desktop's thread; its answer. Dropped unrun (the
/// session ending), the broker hears the step stopped without one.
fn ask(
    work: impl FnOnce(&mut ShellOverlayHost) -> Served + Send + 'static,
) -> Pending<HostStepOutcome> {
    let (answer, answered) = pending();
    port()
        .jobs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Box::new(move |host| match work(host) {
            Served::Now(outcome) => answer.complete(outcome),
            Served::WhenUp { tab, value } => park(Parked { tab, value, answer }),
        }));
    let wake = port()
        .wake
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(wake) = wake {
        wake();
    }
    answered
}

/// A refused layout change, as the caller reads it.
#[allow(
    clippy::unnecessary_box_returns,
    reason = "the broker carries a step's failure boxed"
)]
pub(crate) fn refused(message: impl Into<String>) -> Box<ActionFailure> {
    Box::new(ActionFailure {
        code: "desktop_refused".into(),
        message: message.into(),
        details: None,
        retryable: false,
    })
}

/// One layout request, its arguments checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Open {
        target: Target,
        place: Place,
        background: bool,
    },
    CreateStrip {
        name: String,
    },
    RenameStrip {
        strip: StripId,
        name: String,
    },
    MoveGroup {
        group: u64,
        strip: StripId,
        index: Option<usize>,
    },
    Arrange {
        strip: StripId,
        layout: Layout,
        order: Option<Vec<u64>>,
        widths: Option<Vec<u32>>,
    },
    ListWindows,
    Undo {
        run: Option<String>,
    },
}

/// A desktop id, as JSON carries it: decimal text (ids exceed what a
/// JavaScript number holds exactly) or a whole number.
fn id(arguments: &Map<String, Value>, name: &str) -> Result<Option<u64>, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => text
            .parse()
            .map(Some)
            .map_err(|_| format!("`{name}` is a window, group or strip id")),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{name}` is a window, group or strip id")),
        Some(_) => Err(format!("`{name}` is a window, group or strip id")),
    }
}

fn required_id(arguments: &Map<String, Value>, name: &str) -> Result<u64, String> {
    id(arguments, name)?.ok_or_else(|| format!("`{name}` is required"))
}

fn text<'a>(arguments: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("`{name}` is text")),
    }
}

/// Check `arguments` for `action`, published with `params`.
///
/// # Errors
///
/// Why they do not fit.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per action, each checking that action's published arguments"
)]
pub(crate) fn bind(
    action: &str,
    arguments: &Map<String, Value>,
    actor_is_agent: bool,
) -> Result<Command, String> {
    let published = params(action);
    if let Some(unknown) = arguments
        .keys()
        .find(|key| !published.iter().any(|param| *param.name == **key))
    {
        return Err(format!("{action} takes no `{unknown}`"));
    }
    Ok(match action {
        layout::OPEN => {
            let target = Target::parse(text(arguments, "target")?.ok_or("`target` is required")?)?;
            let strip = id(arguments, "strip")?.map(StripId);
            let group = id(arguments, "group")?;
            let place = match text(arguments, "place")?.unwrap_or("new-window") {
                "new-window" => {
                    if group.is_some() {
                        return Err("a new window takes `strip`, not `group`".into());
                    }
                    Place::NewWindow { strip }
                }
                "tab-of" | "beside" if strip.is_some() => {
                    return Err("`tab-of` and `beside` go by `group`, not `strip`".into());
                }
                "tab-of" => Place::TabOf(group.ok_or("`tab-of` needs the `group` to join")?),
                "beside" => Place::Beside(group.ok_or("`beside` needs the `group` to open by")?),
                other => {
                    return Err(format!(
                        "`place` is new-window, tab-of or beside, not {other:?}"
                    ));
                }
            };
            let background = match arguments.get("background") {
                None | Some(Value::Null) => actor_is_agent,
                Some(Value::Bool(background)) => *background,
                Some(_) => return Err("`background` is true or false".into()),
            };
            let signed_in = match arguments.get("signed_in") {
                None | Some(Value::Null) => !actor_is_agent,
                Some(Value::Bool(signed_in)) if matches!(target, Target::Url(_)) => *signed_in,
                Some(Value::Bool(_)) => return Err("`signed_in` is for a web address".into()),
                Some(_) => return Err("`signed_in` is true or false".into()),
            };
            // A web address an agent opens is browsed as a visitor would,
            // unless it asks for the person's sign-ins: then every action on
            // its page asks the person first.
            let target = if signed_in {
                target
            } else {
                target.as_visitor()
            };
            Command::Open {
                target,
                place,
                background,
            }
        }
        layout::CREATE_STRIP => Command::CreateStrip {
            name: text(arguments, "name")?
                .ok_or("`name` is required")?
                .to_owned(),
        },
        layout::RENAME_STRIP => Command::RenameStrip {
            strip: StripId(required_id(arguments, "strip")?),
            name: text(arguments, "name")?
                .ok_or("`name` is required")?
                .to_owned(),
        },
        layout::MOVE_GROUP => Command::MoveGroup {
            group: required_id(arguments, "group")?,
            strip: StripId(required_id(arguments, "strip")?),
            index: match arguments.get("index") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .and_then(|index| usize::try_from(index).ok())
                        .ok_or("`index` is a position from 0")?,
                ),
            },
        },
        layout::ARRANGE => Command::Arrange {
            strip: StripId(required_id(arguments, "strip")?),
            layout: match text(arguments, "layout")?.unwrap_or("columns") {
                "columns" => Layout::Columns,
                "rows" => Layout::Rows,
                "grid" => Layout::Grid,
                other => {
                    return Err(format!("`layout` is columns, rows or grid, not {other:?}"));
                }
            },
            order: match arguments.get("order") {
                None | Some(Value::Null) => None,
                Some(Value::Array(order)) => Some(
                    order
                        .iter()
                        .map(|group| match group {
                            Value::String(text) => text.parse().ok(),
                            other => other.as_u64(),
                        })
                        .collect::<Option<_>>()
                        .ok_or("`order` lists the strip's window group ids")?,
                ),
                Some(_) => return Err("`order` lists the strip's window group ids".into()),
            },
            widths: match arguments.get("widths") {
                None | Some(Value::Null) => None,
                Some(Value::Array(widths)) => Some(
                    widths
                        .iter()
                        .map(|width| {
                            width
                                .as_u64()
                                .and_then(|width| u32::try_from(width).ok())
                                .ok_or("each width is a share in per mille")
                        })
                        .collect::<Result<_, _>>()?,
                ),
                Some(_) => return Err("`widths` is a list of shares in per mille".into()),
            },
        },
        layout::LIST_WINDOWS => Command::ListWindows,
        layout::UNDO => Command::Undo {
            run: text(arguments, "run")?.map(str::to_owned),
        },
        other => return Err(format!("the desktop has no `{other}`")),
    })
}

const ID: &str = "^[0-9]+$";

fn id_param(name: &'static str, required: bool, description: &'static str) -> ActionParam {
    let param = if required {
        ActionParam::required(name, ParamType::Text, description)
    } else {
        ActionParam::optional(name, ParamType::Text, description)
    };
    param.with_schema(json!({ "type": "string", "pattern": ID }))
}

/// What `action` takes.
#[allow(
    clippy::too_many_lines,
    reason = "every action's published parameters, side by side"
)]
fn params(action: &str) -> Vec<ActionParam> {
    match action {
        layout::OPEN => vec![
            ActionParam::required(
                "target",
                ParamType::Text,
                "What to open: a resource address an app publishes (as the catalogue names it), \
                 workspace content as tilo.content:<hash>, or an http(s) address, which opens \
                 in Browser. Answered once the window is up, with the `provider` its own \
                 actions are published under (Browser's read-page, capture-page, find and \
                 click on browser.page).",
            )
            .with_schema(json!({ "type": "string", "minLength": 1, "maxLength": 8192 })),
            ActionParam::optional(
                "place",
                ParamType::Text,
                "new-window (default): a window of its own; tab-of: a tab of the window \
                 `group`; beside: a window just right of `group`.",
            )
            .with_schema(json!({ "type": "string", "enum": ["new-window", "tab-of", "beside"] })),
            id_param(
                "group",
                false,
                "The window group to join (tab-of) or open beside (beside), as list-windows \
                 or an earlier open answered it.",
            ),
            id_param(
                "strip",
                false,
                "For a new window: the strip to open it at the end of. Without it, it opens \
                 beside the window in focus.",
            ),
            ActionParam::optional(
                "background",
                ParamType::Boolean,
                "Whether to leave the person's focus where it is. Default true for an agent.",
            ),
            ActionParam::optional(
                "signed_in",
                ParamType::Boolean,
                "For a web address: whether Browser opens it with the person's sign-ins \
                 (their cookies and accounts). Default false for an agent: the page opens \
                 as any visitor sees it, in a window of its own profile, and reading it \
                 (read-page, capture-page, find) needs no confirmation. With true, every \
                 action on the page asks the person first.",
            ),
        ],
        layout::CREATE_STRIP => vec![
            ActionParam::required(
                "name",
                ParamType::Text,
                "The strip's name: 1 to 120 characters without control characters.",
            )
            .with_schema(json!({ "type": "string", "minLength": 1, "maxLength": 120 })),
        ],
        layout::RENAME_STRIP => vec![
            id_param("strip", true, "The strip to rename."),
            ActionParam::required(
                "name",
                ParamType::Text,
                "Its new name: 1 to 120 characters without control characters.",
            )
            .with_schema(json!({ "type": "string", "minLength": 1, "maxLength": 120 })),
        ],
        layout::MOVE_GROUP => vec![
            id_param(
                "group",
                true,
                "The window group to move, with all its tabs.",
            ),
            id_param("strip", true, "The strip to move it to."),
            ActionParam::optional(
                "index",
                ParamType::Integer,
                "Its position among that strip's other windows, from 0. Default: the end.",
            )
            .with_schema(json!({ "type": "integer", "minimum": 0 })),
        ],
        layout::ARRANGE => vec![
            id_param("strip", true, "The strip whose windows to lay out."),
            ActionParam::optional(
                "layout",
                ParamType::Text,
                "columns (default): side by side. A strip has no rows or grid; put windows \
                 that belong apart on strips of their own.",
            )
            .with_schema(json!({ "type": "string", "enum": ["columns", "rows", "grid"] })),
            ActionParam::optional(
                "order",
                ParamType::Array,
                "The strip's window groups left to right, every one once. Default: as they are.",
            )
            .with_schema(json!({ "type": "array", "items": { "type": "string", "pattern": ID } })),
            ActionParam::optional(
                "widths",
                ParamType::Array,
                "Each window's share of the strip's width in per mille, in order. Default: \
                 equal shares.",
            )
            .with_schema(json!({
                "type": "array",
                "items": { "type": "integer", "minimum": 1, "maximum": 1000 }
            })),
        ],
        layout::UNDO => vec![
            ActionParam::optional(
                "run",
                ParamType::Text,
                "The run whose layout changes to take back, by its issue address. Default: \
                 your latest.",
            )
            .with_schema(json!({ "type": "string", "minLength": 1 })),
        ],
        _ => Vec::new(),
    }
}

/// Every action, with its label key, operation and result.
const ACTIONS: [(&str, &str, OperationKind); 7] = [
    (layout::OPEN, "desktop-action-open", OperationKind::Document),
    (
        layout::CREATE_STRIP,
        "desktop-action-create-strip",
        OperationKind::Document,
    ),
    (
        layout::RENAME_STRIP,
        "desktop-action-rename-strip",
        OperationKind::Document,
    ),
    (
        layout::MOVE_GROUP,
        "desktop-action-move-group",
        OperationKind::Document,
    ),
    (
        layout::ARRANGE,
        "desktop-action-arrange",
        OperationKind::Document,
    ),
    (
        layout::LIST_WINDOWS,
        "desktop-action-list-windows",
        OperationKind::View,
    ),
    (layout::UNDO, "desktop-action-undo", OperationKind::Document),
];

fn descriptor(action: &str, label: &str, kind: OperationKind) -> ActionDescriptor {
    let contract = if kind == OperationKind::View {
        ActionContract::view()
    } else {
        // Layout is undone by laying it out again: `undo` takes a run back.
        ActionContract {
            kind: ActionKind::Undoable,
            ..ActionContract::default()
        }
    };
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(RESOURCE, action),
        resource_kind: RESOURCE.into(),
        resource_label: t("desktop-actions-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: Some(t(&format!("{label}-description"))),
        locus: ExecutionLocus::default(),
        kind,
        contract,
        input: InputSpec::None,
        params: params(action),
        view: None,
    }
}

/// The typed arguments an argument slot carries; none for none.
fn arguments_of(argument: Option<&str>) -> Result<Map<String, Value>, String> {
    let Some(argument) = argument else {
        return Ok(Map::new());
    };
    let json = argument
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or("the desktop's actions take typed arguments")?;
    serde_json::from_str(json).map_err(|error| error.to_string())
}

/// The broker's door to the desktop's layout.
pub(crate) fn broker_door() -> Arc<dyn HostStepServices> {
    Arc::new(BrokerDoor)
}

struct BrokerDoor;

impl HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        layout::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        ACTIONS
            .into_iter()
            .map(|(action, label, kind)| descriptor(action, label, kind))
            .collect()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.address.resource != RESOURCE {
            return Err("the desktop publishes its layout on desktop:session".into());
        }
        let room = RoomId::parse(request.room).ok_or("the step names no workspace")?;
        let arguments = arguments_of(request.argument)?;
        let actor = Actor {
            id: request.actor.id().to_owned(),
            run: request.actor.issue().map(str::to_owned),
            is_agent: request.actor.is_agent(),
        };
        let command = bind(&request.address.action, &arguments, actor.is_agent)?;
        Ok(ask(move |host| host.serve_desktop(room, &actor, command)))
    }
}

#[cfg(test)]
#[path = "desktop_door_tests.rs"]
mod tests;
