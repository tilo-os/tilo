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
    Open,
    /// Plan the rest of the task again from here, once the steps before
    /// have changed what is published (an app opened, say).
    Continue(String),
    /// Hold the task until a message it needs arrives or the wait runs
    /// out, then plan the rest again with what arrived.
    Wait(Wait),
}

/// The apps whose records carry messages to an agent's team: what a task
/// may wait for arrives in one of them.
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
}

impl Step {
    /// The resource and action names the task log records for this step.
    #[must_use]
    pub fn log_names(&self) -> (String, String) {
        match &self.action {
            StepAction::Invoke { address, .. } => {
                (address.resource.clone(), address.action.clone())
            }
            StepAction::Open => (format!("app.{}", self.app), "open".to_owned()),
            StepAction::Continue(_) => (String::new(), "continue".to_owned()),
            StepAction::Wait(_) => (String::new(), "wait".to_owned()),
        }
    }
}

/// What a planner may look at.
#[derive(Default)]
pub struct PlanContext {
    /// Records the agent has open, with every operation each publishes.
    pub open: Vec<(DocumentRoute, Vec<ActionDescriptor>)>,
    /// Records of the room the agent has seen but not opened.
    pub known: Vec<DocumentRoute>,
    /// Apps the agent may open.
    pub apps: Vec<String>,
    /// Whether this plan may rightly find nothing left to do: the check of
    /// a task's finished work, or the turn after its reads. No steps, with
    /// a reply saying so, is then the answer ([`read_answer`]); a first
    /// plan must still name the work.
    pub may_plan_nothing: bool,
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
}

/// Reads the steps from the task: one JSON object or an array of them.
///
/// `{"app", "record"?, "resource", "action", "arguments"?}` invokes an
/// operation the record publishes on `resource`.
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
    Ok(Step {
        app,
        record,
        action: StepAction::Invoke {
            address: ActionAddress::new(resource, action),
            arguments,
        },
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
}
