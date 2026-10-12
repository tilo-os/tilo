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
        /// The action.
        address: ActionAddress,
        /// Its typed arguments.
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// Open the step's app on a new record.
    Open,
    /// Plan the rest of the task again from here, once the steps before
    /// have changed what is published (an app opened, say).
    Continue(String),
}

/// One action to perform.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The app.
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
        }
    }
}

/// What a planner may look at.
pub struct PlanContext {
    /// Records the agent has open, with every operation each publishes.
    pub open: Vec<(DocumentRoute, Vec<ActionDescriptor>)>,
    /// Records of the room the agent has seen but not opened.
    pub known: Vec<DocumentRoute>,
    /// Apps the agent may open.
    pub apps: Vec<String>,
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
    ///
    /// # Errors
    ///
    /// Returns a reason shown to the person who gave the task.
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
///
/// # Errors
///
/// Returns why a step is malformed or names an app outside the context.
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
            open: Vec::new(),
            known: Vec::new(),
            apps: vec!["so.tilo.scratchpad".into()],
        }
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
