//! From a task to the app actions that carry it out.
//!
//! A [`Planner`] turns a task into [`Step`]s, each one operation a record
//! publishes. The planner sees only inert descriptors ([`PlanContext`]); it
//! can name an operation but never perform one, and the participant checks
//! every step against the delegation and the resource's door before it
//! runs.
//!
//! [`DirectPlanner`] reads steps written out in the task itself (JSON), for
//! scripts, tests and callers that already know the actions.
//! [`crate::model::ModelPlanner`] asks a model to choose from the same
//! descriptors.

use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::DocumentRoute;

/// What one step does.
#[derive(Clone, Debug, PartialEq)]
pub enum StepAction {
    /// Invoke an operation the record publishes.
    Invoke {
        address: ActionAddress,
        /// Its typed arguments.
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// Open the step's app: on the step's record, one the room already
    /// has, when it names one; otherwise on a new record.
    Open,
    /// Plan the rest of the task again from here, once the steps before
    /// have changed what is published (an app opened, say).
    Continue(String),
    /// Hold the task until a message it needs arrives or the wait runs
    /// out, then plan the rest again with what arrived.
    Wait(Wait),
}

/// The apps whose records carry messages to an agent's team: a change
/// another member makes to one while a task runs is news the task may have
/// to follow (a correction, a decision, a code it waits for).
pub const MESSAGE_APPS: &[&str] = &["so.tilo.mail", "so.tilo.chat", "so.tilo.messages"];

/// The app of [`MESSAGE_APPS`] `name` stands for: its id, or its last part
/// (`chat`), in any case.
#[must_use]
pub fn message_app(name: &str) -> Option<&'static str> {
    let name = name.trim();
    MESSAGE_APPS.iter().copied().find(|app| {
        app.eq_ignore_ascii_case(name)
            || app
                .rsplit('.')
                .next()
                .is_some_and(|short| short.eq_ignore_ascii_case(name))
    })
}

/// The names a plan's `wait_for.app` takes, as the model is told them: the
/// last part of each of [`MESSAGE_APPS`]. Pass them to
/// [`tilo_agent::tools::typed_waiting_definitions`].
#[must_use]
pub fn wait_places() -> Vec<&'static str> {
    MESSAGE_APPS
        .iter()
        .filter_map(|app| app.rsplit('.').next())
        .collect()
}

/// How many times one task waits for a message ([`StepAction::Wait`]):
/// each wait ends in a plan, so a model that keeps waiting cannot hold the
/// task open for ever. The model is told it, so it knows it may wait again.
pub const MOST_WAITS: u32 = 4;

/// What a waiting task waits for: a change another member makes to a
/// record that carries messages (a delivered SMS, a mail, a chat post).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The apps whose records it waits on; empty for every one of
    /// [`MESSAGE_APPS`].
    pub apps: Vec<String>,
    /// Text the message holds, matched without regard to case; `None` for
    /// any new message.
    pub contains: Option<String>,
    /// The longest it waits.
    pub within: std::time::Duration,
    /// What the plan left for after the wait (its `then`), if anything:
    /// the request itself is shown beside the re-plan regardless.
    pub rest: Option<String>,
}

impl Wait {
    /// The wait a plan's `wait_for` asks for, its place resolved to one of
    /// [`MESSAGE_APPS`] and its seconds kept within
    /// [`tilo_agent::tools::MOST_WAIT_SECONDS`]; `rest` is the plan's
    /// `then`.
    ///
    /// # Errors
    ///
    /// A place no message arrives in, in words for the model.
    pub fn planned(asked: tilo_agent::tools::Wait, rest: Option<String>) -> Result<Self, String> {
        let apps = match asked.app.as_deref() {
            None => Vec::new(),
            Some(place) => vec![message_app(place).map(str::to_owned).ok_or_else(|| {
                format!(
                    "`{}.app` {place:?} is not one of {}; omit it to wait on all of them",
                    tilo_agent::tools::WAIT_FOR,
                    wait_places().join(", ")
                )
            })?],
        };
        Ok(Self {
            apps,
            contains: asked.contains,
            within: std::time::Duration::from_secs(
                asked.seconds.clamp(1, tilo_agent::tools::MOST_WAIT_SECONDS),
            ),
            rest,
        })
    }
}

/// A `plan` call's input read as the plan: one of no steps is an answer
/// only where `context` allows it ([`PlanContext::may_plan_nothing`]), and
/// a `wait_for` must name a place messages arrive in.
///
/// # Errors
///
/// Why the plan is refused, as [`tilo_agent::tools::read_plan`] says, or
/// the unknown place, for the model to correct.
pub fn read_answer(
    input: &serde_json::Value,
    revealed: &tilo_agent::tools::Revealed,
    most_steps: usize,
    context: &PlanContext,
) -> Result<tilo_agent::tools::Answer, String> {
    let answer = if context.may_plan_nothing {
        tilo_agent::tools::read_plan_allowing_none(input, revealed, most_steps)
    } else {
        tilo_agent::tools::read_plan(input, revealed, most_steps)
    }?;
    if let Some(wait) = &answer.wait {
        Wait::planned(wait.clone(), None)?;
    }
    Ok(answer)
}

/// One action to perform.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub app: String,
    /// The record; `None` uses the record already open in `app`, or opens a
    /// new one.
    pub record: Option<String>,
    /// What to do there.
    pub action: StepAction,
    /// The parameter under which the operation takes what the step before
    /// it produced: bound to that step's one content result when it runs,
    /// never to a value the plan wrote
    /// ([`tilo_agent::plan::carried_arguments`]). `None` carries nothing.
    pub carries: Option<String>,
}

impl Step {
    /// The resource and action names the task log records for this step.
    #[must_use]
    pub fn log_names(&self) -> (String, String) {
        match &self.action {
            StepAction::Invoke { address, .. } => {
                (address.resource.clone(), address.action.clone())
            }
            StepAction::Open => match &self.record {
                Some(record) => (format!("record.{}.{record}", self.app), "open".to_owned()),
                None => (format!("app.{}", self.app), "open".to_owned()),
            },
            StepAction::Continue(_) => (String::new(), "continue".to_owned()),
            StepAction::Wait(_) => (String::new(), "wait".to_owned()),
        }
    }
}

/// How many model calls a task's plans may still ask again after losing
/// them to the network ([`MOST_TURN_RETRIES`]). Every plan of one task
/// shares it, so a flaky network costs a task a few calls, not one per
/// turn of every plan.
#[derive(Clone, Debug)]
pub struct TurnRetries(std::sync::Arc<std::sync::atomic::AtomicU32>);

/// The most lost calls one task asks again.
pub const MOST_TURN_RETRIES: u32 = 3;

impl Default for TurnRetries {
    fn default() -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            MOST_TURN_RETRIES,
        )))
    }
}

/// Budgets are equal when as much is left of them.
impl PartialEq for TurnRetries {
    fn eq(&self, other: &Self) -> bool {
        self.left() == other.left()
    }
}

impl Eq for TurnRetries {}

impl TurnRetries {
    /// How many calls may still be asked again.
    #[must_use]
    pub fn left(&self) -> u32 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Spend one retry; `false` when none is left.
    pub fn take(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        self.0
            .fetch_update(Relaxed, Relaxed, |left| left.checked_sub(1))
            .is_ok()
    }
}

/// What a planner may look at.
#[derive(Default)]
pub struct PlanContext {
    /// Records the agent has open, with every operation each publishes.
    pub open: Vec<(DocumentRoute, Vec<ActionDescriptor>)>,
    /// Records of the room the agent has seen but not opened, those people
    /// have open first, then the latest changed; each with what its app
    /// calls it (a document's first heading), when it names it and the
    /// record is among those offered to open.
    pub known: Vec<(DocumentRoute, Option<String>)>,
    /// Apps the agent may open.
    pub apps: Vec<String>,
    /// Content addresses (`tilo.content:<hash>`) the task's steps have
    /// produced so far, oldest first.
    pub produced: Vec<String>,
    /// What the task's steps have answered so far, oldest first, each an
    /// [`observation`]: what a view or read found (an artwork's size, a
    /// document's text), for the rest of the task to be planned with.
    pub observed: Vec<String>,
    /// Apps the task is about of which neither the agent nor the room has
    /// a record yet, each with the operations a new record of it
    /// publishes: a step naming one opens a new record of the app first
    /// ([`Step::record`] `None`), so a task that starts a document is
    /// planned without opening it first.
    pub previews: Vec<(String, Vec<ActionDescriptor>)>,
    /// What the open records the task is about hold now, each an
    /// [`observation`] of one of their reads (an artwork's size and marks,
    /// a document's text, a sheet's cells), taken just before planning.
    pub snapshots: Vec<String>,
    /// The task as it was given, when this plan is for the rest of it (a
    /// [`StepAction::Continue`]): what the rest leaves out is still asked.
    pub request: Option<String>,
    /// The apps the agent may use, sorted by app id, each with the
    /// operations a new record of it publishes: what those apps can do,
    /// whatever the room holds, the same for every agent and task granted
    /// them — the stable reference a model's prompt starts with.
    pub reference: Vec<(String, Vec<ActionDescriptor>)>,
    /// The room the task is planned in.
    pub room: String,
    /// Every record of the room the agent may open, with its plain text,
    /// for the planner to find one by what it holds
    /// ([`crate::search::SEARCH_ROOM`]): those people have open first, then
    /// the latest changed.
    pub searchable: Vec<crate::search::SearchDoc>,
    /// Whether this plan may rightly find nothing left to do: the check of
    /// a task's finished work, or the turn after its reads. No steps, with
    /// a reply saying so, is then the answer; a first plan must still name
    /// the work.
    pub may_plan_nothing: bool,
    /// The content addresses of the pictures the task's steps looked at
    /// ([`rendered_picture`]), oldest first: a re-plan's model that sees is
    /// shown the newest with its first turn.
    pub pictures: Vec<String>,
    /// The content addresses apps rendered or made for this task (its
    /// steps' looks and exports): pictures shown to a model that sees,
    /// whatever [`Self::room_pictures`] says.
    pub rendered: Vec<String>,
    /// Whether the agent may show the room's own pictures (a scan, a photo
    /// someone put in the room) to its model's provider. Off unless the
    /// person running it turned it on
    /// ([`crate::vision::ROOM_PICTURES_ENV`]).
    pub room_pictures: bool,
    /// Apps of which the plan is offered a new, empty document although
    /// one is open (`app.<app>#open`): an import into the open one was
    /// refused for the content it holds, and only a new document takes it.
    pub new_documents: Vec<String>,
    /// The task's budget for asking a model call lost to the network again.
    pub retries: TurnRetries,
}

impl PlanContext {
    /// The parameter under which the operation at `address` of `app` — on
    /// `record`, or on the record of `app` the agent has open — takes what
    /// the step before it produced, as its descriptor declares
    /// ([`content_param`]). `None` for an operation of a record that is not
    /// open.
    #[must_use]
    pub fn content_param(
        &self,
        app: &str,
        record: Option<&str>,
        address: &ActionAddress,
    ) -> Option<String> {
        self.open
            .iter()
            .find(|(route, _)| {
                route.app_id == app && record.is_none_or(|record| route.record == record)
            })
            .and_then(|(_, descriptors)| {
                descriptors
                    .iter()
                    .find(|descriptor| descriptor.address == *address)
            })
            .and_then(content_param)
    }

    /// [`Self::content_param`] of the operation at `address` that a new
    /// record of `app` publishes ([`Self::previews`]).
    #[must_use]
    pub fn preview_content_param(&self, app: &str, address: &ActionAddress) -> Option<String> {
        self.previews
            .iter()
            .filter(|(previewed, _)| previewed == app)
            .flat_map(|(_, descriptors)| descriptors)
            .find(|descriptor| descriptor.address == *address)
            .and_then(content_param)
    }
}

/// The parameter content another operation produced is carried under in
/// `descriptor`: its typed content parameter (`#[param(content)]`), or the
/// one input of an operation that collects only content.
#[must_use]
pub fn content_param(descriptor: &ActionDescriptor) -> Option<String> {
    if let Some(param) = descriptor.params.iter().find(|param| param.is_content()) {
        return Some(param.name.to_string());
    }
    descriptor
        .input
        .wants_content()
        .then(|| tilo_actions::PROMPTED_PARAM.to_owned())
}

pub use crate::apps::readable;

/// Runs reads while a task is planned: what a [`Planner`] uses to look at
/// an open record before it decides, in the same planning conversation.
pub trait Reads {
    /// Run the [`readable`] operation at `address` of the open record at
    /// `route` with `arguments`, and answer with its output.
    ///
    /// # Errors
    ///
    /// Why it was not run: the record is not open, the operation is not a
    /// read it publishes, or the record's door refused it.
    fn read(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, String>;

    /// The bytes of the content at `hash` the room keeps: a picture a read
    /// rendered, or one the planning model asks to look at. Content larger
    /// than [`crate::vision::MOST_SOURCE_BYTES`] may be answered with its
    /// first bytes alone, one more than that: enough to refuse it.
    ///
    /// # Errors
    ///
    /// Why it cannot be had: the room does not keep it.
    fn look(&mut self, hash: &str) -> Result<Vec<u8>, String> {
        let _ = hash;
        Err("no content can be looked at while this task is planned".into())
    }
}

/// The content address of the picture `output` rendered (a canvas look, a
/// PDF page): the `content` its value names, which is content the
/// operation made. `None` for any other answer.
#[must_use]
pub fn rendered_picture(output: &ActionOutput) -> Option<String> {
    let content = output
        .value
        .as_ref()?
        .get("content")?
        .as_str()
        .filter(|content| tilo_actions::content_hash(content).is_some())?;
    output
        .resources
        .iter()
        .any(|resource| resource == content)
        .then(|| content.to_owned())
}

/// No reads: what a planner is given where nothing can be read.
pub struct NoReads;

impl Reads for NoReads {
    fn read(
        &mut self,
        _: &DocumentRoute,
        _: &ActionAddress,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, String> {
        Err("nothing can be read while this task is planned".into())
    }
}

/// The most answers one task keeps for its plans, the newest.
pub const MOST_OBSERVED: usize = 6;

/// Strings of an answer longer than this with no space in them are encoded
/// data (a picture inlined as base64, say), left out of an observation.
const MOST_DATA_CHARS: usize = 256;

/// The items of one list of an answer an observation shows; the rest are
/// counted.
const MOST_ITEMS: usize = 24;

/// What the step that ran `address` on `app` answered, as a plan reads it:
/// its value, compacted and bounded. Data inlined in it (a picture) is left
/// out with its size — a picture an operation renders is kept as content,
/// and its address and dimensions are what the value shows — long lists are
/// cut, and the whole is at most [`tilo_agent::tools::MOST_RESULT_CHARS`]
/// characters. `None` when the operation answered no value.
#[must_use]
pub fn observation(app: &str, address: &ActionAddress, output: &ActionOutput) -> Option<String> {
    observation_within(app, address, output, tilo_agent::tools::MOST_RESULT_CHARS)
}

/// [`observation`], its value at most `most` characters.
#[must_use]
pub fn observation_within(
    app: &str,
    address: &ActionAddress,
    output: &ActionOutput,
    most: usize,
) -> Option<String> {
    let value = output.value.as_ref().filter(|value| !value.is_null())?;
    // Dates and amounts as the host reads them, noted beside what the
    // document writes, so the model copies an ISO date rather than
    // working out which way `03/04/2026` goes.
    let text = compact(&crate::values::annotate_json(read_content(value))).to_string();
    let text = if text.chars().count() > most {
        format!("{}… (cut)", text.chars().take(most).collect::<String>())
    } else {
        text
    };
    Some(format!(
        "{app} {}#{} answered {text}",
        address.resource, address.action
    ))
}

/// What a read answered without its envelope: a read's `content`, when
/// the rest is only which record and version it was read from (the plan
/// names the record already, and the version is the app's to check).
fn read_content(value: &serde_json::Value) -> &serde_json::Value {
    const ENVELOPE: [&str; 4] = ["version", "record", "basis", "nextCursor"];
    match value.as_object() {
        Some(object)
            if object.contains_key("content")
                && object
                    .get("nextCursor")
                    .is_none_or(serde_json::Value::is_null)
                && object
                    .keys()
                    .all(|key| key == "content" || ENVELOPE.contains(&key.as_str())) =>
        {
            object.get("content").unwrap_or(value)
        }
        _ => value,
    }
}

/// `value` without inlined data and with long lists cut: what a plan, a
/// repair or a log is shown of an answer or a step's arguments.
pub(crate) fn compact(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => {
            let length = text.chars().count();
            if let Some(kind) = text
                .strip_prefix("data:")
                .and_then(|rest| rest.split([';', ',']).next())
            {
                Value::String(format!("(data:{kind}, {length} characters, left out)"))
            } else if length > MOST_DATA_CHARS && !text.contains(char::is_whitespace) {
                Value::String(format!("({length} characters of encoded data, left out)"))
            } else {
                value.clone()
            }
        }
        Value::Array(items) => {
            let mut kept: Vec<Value> = items.iter().take(MOST_ITEMS).map(compact).collect();
            if items.len() > MOST_ITEMS {
                kept.push(Value::String(format!(
                    "… {} more",
                    items.len().saturating_sub(MOST_ITEMS)
                )));
            }
            Value::Array(kept)
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), compact(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Steps written out in `task` itself (one JSON step or an array of them),
/// which every planner takes as written: `None` when the task is not JSON
/// steps and needs planning.
#[must_use]
pub fn written_steps(task: &str, context: &PlanContext) -> Option<Result<Vec<Step>, String>> {
    let value = serde_json::from_str::<serde_json::Value>(task).ok()?;
    (value.is_array() || value.is_object()).then(|| steps_from_json(&value, context))
}

/// Turns a task into steps. A planner runs on a thread of its own, beside
/// the agent's loop, so it may block (a model call) without holding the
/// agent's session back.
pub trait Planner: Send {
    /// The steps for `task`, or why there are none.
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String>;

    /// [`Self::plan`], with `reads` to look at the open records while
    /// planning. A planner that reads nothing plans as [`Self::plan`] does.
    ///
    /// # Errors
    ///
    /// Returns a reason shown to the person who gave the task.
    fn plan_reading(
        &mut self,
        task: &str,
        context: &PlanContext,
        reads: &mut dyn Reads,
    ) -> Result<Vec<Step>, String> {
        let _ = reads;
        self.plan(task, context)
    }

    /// Whether a task this planner planned is checked against its request
    /// once its steps ran (one more planning turn that fixes what is
    /// missing). A planner that does not say, does not.
    fn checks_work(&self) -> bool {
        false
    }

    /// Whether the plan this planner last made was a model's (not steps
    /// written out, nor one operation the task stated outright): only a
    /// model's plan is checked.
    fn last_planned_by_model(&self) -> bool {
        false
    }
}

/// Reads the steps from the task: one JSON object or an array of them.
///
/// `{"app", "record"?, "resource", "action", "arguments"?, "carries"?}`
/// invokes an operation the record publishes on `resource`. An operation
/// that takes content is given what the step before it produced
/// (`carries`) unless its arguments name the content themselves; `carries`
/// says so explicitly (`true`, or the parameter it fills), for a record
/// that is not open yet.
#[derive(Default)]
pub struct DirectPlanner;

impl Planner for DirectPlanner {
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String> {
        let value: serde_json::Value = serde_json::from_str(task)
            .map_err(|_| "this agent takes tasks as JSON steps".to_string())?;
        steps_from_json(&value, context)
    }
}

/// Steps from one JSON step or an array of them (see [`DirectPlanner`]).
pub fn steps_from_json(
    value: &serde_json::Value,
    context: &PlanContext,
) -> Result<Vec<Step>, String> {
    let items = match value {
        serde_json::Value::Array(items) => items.as_slice(),
        serde_json::Value::Object(_) => std::slice::from_ref(value),
        _ => return Err("a task is a step or a list of steps".into()),
    };
    items.iter().map(|item| step(item, context)).collect()
}

fn step(item: &serde_json::Value, context: &PlanContext) -> Result<Step, String> {
    let text = |key: &str| item.get(key).and_then(serde_json::Value::as_str);
    let app = text("app").ok_or("a step names its app")?.to_owned();
    if !context.apps.contains(&app) {
        return Err(format!("{app} cannot be opened by this agent"));
    }
    let record = text("record").map(str::to_owned);
    let action = text("action").ok_or("a step names its action")?;
    let resource = text("resource")
        .ok_or("a step names its resource")?
        .to_owned();
    let arguments = match item.get("arguments") {
        None => serde_json::Map::new(),
        Some(serde_json::Value::Object(arguments)) => arguments.clone(),
        Some(_) => return Err("arguments are an object".into()),
    };
    let address = ActionAddress::new(resource, action);
    let declared = context.content_param(&app, record.as_deref(), &address);
    let carries = match item.get("carries") {
        None => declared.filter(|param| !arguments.contains_key(param)),
        Some(serde_json::Value::Bool(false)) => None,
        Some(serde_json::Value::Bool(true)) => {
            Some(declared.unwrap_or_else(|| tilo_actions::PROMPTED_PARAM.to_owned()))
        }
        Some(serde_json::Value::String(param)) if !param.is_empty() => Some(param.clone()),
        Some(_) => return Err("carries is true, false or the parameter it fills".into()),
    };
    Ok(Step {
        app,
        record,
        action: StepAction::Invoke { address, arguments },
        carries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PlanContext {
        PlanContext {
            apps: vec!["so.tilo.scratchpad".into()],
            ..PlanContext::default()
        }
    }

    /// A plan's wait resolves its place to the message app it names, keeps
    /// its seconds within the bound and carries the plan's `then`; a place
    /// no message arrives in is refused in words the model can act on.
    #[test]
    fn a_planned_wait_names_a_message_app_and_carries_the_rest() {
        let asked = |app: Option<&str>, seconds| tilo_agent::tools::Wait {
            app: app.map(str::to_owned),
            contains: Some("code".into()),
            seconds,
        };
        let wait = Wait::planned(asked(Some("Messages"), 600), Some("note the code".into()))
            .expect("a message app");
        assert_eq!(
            wait,
            Wait {
                apps: vec!["so.tilo.messages".into()],
                contains: Some("code".into()),
                within: std::time::Duration::from_secs(tilo_agent::tools::MOST_WAIT_SECONDS),
                rest: Some("note the code".into()),
            }
        );
        let anywhere = Wait::planned(asked(None, 0), None).expect("any app");
        assert!(anywhere.apps.is_empty());
        assert_eq!(anywhere.within, std::time::Duration::from_secs(1));
        assert_eq!(
            Wait::planned(asked(Some("so.tilo.chat"), 30), None)
                .expect("an app id")
                .apps,
            ["so.tilo.chat"]
        );
        let refused = Wait::planned(asked(Some("sms"), 30), None).expect_err("no such place");
        assert!(refused.contains("mail, chat, messages"), "{refused}");
        let step = Step {
            app: String::new(),
            record: None,
            action: StepAction::Wait(anywhere),
            carries: None,
        };
        assert_eq!(step.log_names(), (String::new(), "wait".into()));
    }

    /// No steps is an answer only where the context allows it and with a
    /// reply; a wait's place is checked when the plan is read, so the model
    /// is told at once.
    #[test]
    fn a_plan_is_read_with_what_the_context_allows() {
        let revealed = tilo_agent::tools::Revealed::new();
        let mut context = context();
        let done = serde_json::json!({"reply": "Nothing is left.", "steps": [], "why": "",
            "then": ""});
        assert!(read_answer(&done, &revealed, 4, &context).is_err());
        context.may_plan_nothing = true;
        let answer = read_answer(&done, &revealed, 4, &context).expect("a check may find nothing");
        assert!(answer.actions.is_empty());
        let silent = serde_json::json!({"steps": [], "why": "", "then": ""});
        assert!(read_answer(&silent, &revealed, 4, &context).is_err());
        let waits = serde_json::json!({"steps": [], "why": "", "then": "",
            "wait_for": {"app": "chat", "seconds": 30}});
        let answer = read_answer(&waits, &revealed, 4, &context).expect("a wait");
        assert_eq!(
            answer.wait.map(|wait| wait.app),
            Some(Some("chat".to_owned()))
        );
        let elsewhere = serde_json::json!({"steps": [], "why": "", "then": "",
            "wait_for": {"app": "sms", "seconds": 30}});
        let refused = read_answer(&elsewhere, &revealed, 4, &context).expect_err("no such place");
        assert!(
            refused.contains("omit it to wait on all of them"),
            "{refused}"
        );
        assert_eq!(wait_places(), ["mail", "chat", "messages"]);
        assert_eq!(message_app(" CHAT "), Some("so.tilo.chat"));
        assert_eq!(message_app("so.tilo.notes"), None);
    }

    #[test]
    fn an_observation_shows_what_was_found_and_leaves_inlined_data_out() {
        let inspect = ActionAddress::new("canvas.artwork", "inspect");
        let answered = |value: serde_json::Value| ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        };
        let seen = observation(
            "so.tilo.canvas",
            &inspect,
            &answered(serde_json::json!({"artwork": {"width": 640, "height": 480}})),
        )
        .unwrap();
        assert!(
            seen.starts_with("so.tilo.canvas canvas.artwork#inspect answered ")
                && seen.contains(r#""width":640"#)
                && seen.contains(r#""height":480"#),
            "{seen}"
        );
        assert_eq!(
            observation("so.tilo.canvas", &inspect, &ActionOutput::default()),
            None,
            "nothing answered, nothing observed"
        );

        let picture = format!("data:image/png;base64,{}", "A".repeat(200_000));
        let blob = "Q".repeat(5_000);
        let seen = observation(
            "so.tilo.canvas",
            &ActionAddress::new("canvas.artwork", "look"),
            &answered(serde_json::json!({
                "width": 768,
                "height": 512,
                "content": "tilo.content:abc",
                "image": picture,
                "pixels": blob,
                "marks": (0..100).collect::<Vec<_>>(),
            })),
        )
        .unwrap();
        assert!(seen.contains(r#""width":768"#), "{seen}");
        assert!(seen.contains("tilo.content:abc"), "{seen}");
        assert!(
            seen.contains("(data:image/png, 200022 characters, left out)")
                && seen.contains("(5000 characters of encoded data, left out)")
                && seen.contains("… 76 more"),
            "{seen}"
        );
        assert!(!seen.contains("AAAA") && !seen.contains("QQQQ"), "{seen}");

        let long = observation(
            "so.tilo.word-processor",
            &ActionAddress::new("word-processor.document", "read-text"),
            &answered(serde_json::json!({"markdown": "word ".repeat(2_000)})),
        )
        .unwrap();
        assert!(long.ends_with("… (cut)"), "{long}");
        assert!(
            long.chars().count() < tilo_agent::tools::MOST_RESULT_CHARS + 120,
            "bounded"
        );
    }

    /// A record of `app` publishing one content-consuming operation.
    fn place_image(app: &str) -> (DocumentRoute, ActionDescriptor) {
        let route = DocumentRoute {
            app_id: app.into(),
            cluster_id: "room".into(),
            record: "w1".into(),
        };
        let descriptor = ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new("document", "place-image"),
            resource_kind: "document".into(),
            resource_label: "Document".into(),
            action_label: "Place image".into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            params: Vec::new(),
            input: tilo_actions::InputSpec::Text {
                prompt: "Image".into(),
                placeholder: "content hash".into(),
                collects: tilo_actions::Collects::Content,
            },
            view: None,
        };
        assert!(descriptor.input.wants_content());
        (route, descriptor)
    }

    #[test]
    fn a_step_carries_content_where_its_operation_declares_it() {
        let app = "so.tilo.word-processor";
        let (route, descriptor) = place_image(app);
        let mut context = context();
        context.apps.push(app.into());
        let step = |json: serde_json::Value, context: &PlanContext| {
            steps_from_json(&json, context).unwrap().remove(0)
        };
        let place =
            serde_json::json!({"app": app, "resource": "document", "action": "place-image"});
        assert!(
            step(place.clone(), &context).carries.is_none(),
            "a record that is not open declares nothing yet"
        );
        context.open.push((route, vec![descriptor]));
        assert_eq!(
            step(place.clone(), &context).carries.as_deref(),
            Some(tilo_actions::PROMPTED_PARAM)
        );
        let mut named = place.clone();
        named["record"] = "w1".into();
        assert!(step(named.clone(), &context).carries.is_some());
        named["record"] = "w2".into();
        assert!(step(named, &context).carries.is_none(), "another record");
        let mut explicit = place.clone();
        explicit["arguments"] = serde_json::json!({(tilo_actions::PROMPTED_PARAM): "abc"});
        assert!(
            step(explicit, &context).carries.is_none(),
            "content the step names itself is not replaced"
        );
        let mut said = place;
        said["carries"] = false.into();
        assert!(step(said.clone(), &context).carries.is_none());
        said["carries"] = 3.into();
        assert!(steps_from_json(&said, &context).is_err());
    }

    #[test]
    fn a_carried_step_is_given_the_one_content_the_step_before_produced() {
        use tilo_agent::plan::carried_arguments;
        let planned = serde_json::Map::from_iter([
            ("caption".to_owned(), serde_json::Value::from("kept")),
            (
                tilo_actions::PROMPTED_PARAM.to_owned(),
                serde_json::Value::from("model-invented"),
            ),
        ]);
        let produced = tilo_actions::ActionOutput {
            value: None,
            resources: vec![tilo_actions::content_resource("abc123").unwrap()],
        };
        assert_eq!(
            carried_arguments(&planned, None, Some(&produced)).unwrap(),
            planned,
            "a step that does not carry runs as planned"
        );
        let bound = carried_arguments(
            &planned,
            Some(tilo_actions::PROMPTED_PARAM),
            Some(&produced),
        )
        .unwrap();
        assert_eq!(bound[tilo_actions::PROMPTED_PARAM], "abc123");
        assert_eq!(bound["caption"], "kept");

        let refused =
            carried_arguments(&planned, Some(tilo_actions::PROMPTED_PARAM), None).unwrap_err();
        assert!(refused.contains("no preceding action"), "{refused}");
        let nothing = tilo_actions::ActionOutput::default();
        assert!(
            carried_arguments(&planned, Some(tilo_actions::PROMPTED_PARAM), Some(&nothing))
                .is_err()
        );
        let two = tilo_actions::ActionOutput {
            value: None,
            resources: vec!["tilo.content:a".into(), "tilo.content:b".into()],
        };
        assert!(
            carried_arguments(&planned, Some(tilo_actions::PROMPTED_PARAM), Some(&two)).is_err()
        );
        let file = tilo_actions::ActionOutput {
            value: None,
            resources: vec!["files.entry:/tmp/a.png".into()],
        };
        assert!(
            carried_arguments(&planned, Some(tilo_actions::PROMPTED_PARAM), Some(&file)).is_err()
        );
    }

    #[test]
    fn steps_name_published_operations() {
        let steps = DirectPlanner
            .plan(
                r#"[{"app":"so.tilo.scratchpad","resource":"scratchpad.note","action":"edit","arguments":{"text":"hi"}},
                    {"app":"so.tilo.scratchpad","record":"r1","resource":"scratchpad.note","action":"edit"}]"#,
                &context(),
            )
            .unwrap();
        let StepAction::Invoke { address, arguments } = &steps[0].action else {
            panic!("{:?}", steps[0]);
        };
        assert_eq!(address.resource, "scratchpad.note");
        assert_eq!(arguments["text"], "hi");
        assert_eq!(steps[1].record.as_deref(), Some("r1"));
        assert_eq!(
            steps[1].log_names(),
            ("scratchpad.note".into(), "edit".into())
        );
        assert!(DirectPlanner.plan("tidy up", &context()).is_err());
        assert!(
            DirectPlanner
                .plan(
                    r#"{"app":"so.tilo.scratchpad","action":"edit"}"#,
                    &context()
                )
                .is_err(),
            "a step names its resource"
        );
        assert!(
            DirectPlanner
                .plan(
                    r#"{"app":"so.tilo.mail","resource":"x","action":"send"}"#,
                    &context()
                )
                .is_err(),
            "an app outside the catalogue is refused at planning"
        );
    }

    /// A look's answer names the picture it rendered (its address, which a
    /// re-plan shows), and the observation still leaves inlined data out.
    #[test]
    fn a_look_answer_names_the_picture_it_rendered() {
        let look = ActionAddress::new("canvas.artwork", "look");
        let address = format!("tilo.content:{}", "a".repeat(64));
        let output = ActionOutput {
            value: Some(
                serde_json::json!({"width": 768, "height": 512, "content": address,
                "thumbnail": format!("data:image/png;base64,{}", "A".repeat(4_000))}),
            ),
            resources: vec![address.clone()],
        };
        assert_eq!(rendered_picture(&output), Some(address.clone()));
        let seen = observation("so.tilo.canvas", &look, &output).unwrap();
        assert!(seen.contains(&address), "{seen}");
        assert!(
            seen.contains("(data:image/png, 4022 characters, left out)"),
            "{seen}"
        );
        assert!(!seen.contains("AAAA"), "{seen}");

        let named_only = ActionOutput {
            value: output.value,
            resources: Vec::new(),
        };
        assert_eq!(
            rendered_picture(&named_only),
            None,
            "content it did not make"
        );
        assert_eq!(rendered_picture(&ActionOutput::default()), None);
    }
}
