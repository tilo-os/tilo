//! From a task to the app actions that carry it out.
//!
//! A [`Planner`] turns a task into [`Step`]s, each one action an app
//! publishes or one input to an on-screen control. The planner sees only
//! inert descriptors ([`PlanContext`]); it can name an action or a control
//! but never perform one, and the participant checks every step against the
//! delegation and the app's door before it runs.
//!
//! [`DirectPlanner`] reads steps written out in the task itself (JSON), for
//! scripts, tests and callers that already know the actions.
//! [`crate::model::ModelPlanner`] asks a model to choose from the same
//! descriptors.

use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::DocumentRoute;
use ui::headless::{Control, ControlInput};

/// What one step does.
#[derive(Clone, Debug, PartialEq)]
pub enum StepAction {
    /// Invoke a published action: one the app declared, or a projected
    /// control's (`ui:<node>#activate`).
    Invoke {
        /// The action.
        address: ActionAddress,
        /// Its typed arguments.
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// Operate an on-screen control as a person's input does.
    Operate {
        /// The control's semantic id.
        control: String,
        /// What to do to it.
        input: ControlInput,
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
            StepAction::Operate { control, input } => {
                (control_resource(control), input_name(input).to_owned())
            }
            StepAction::Open => (format!("app.{}", self.app), "open".to_owned()),
            StepAction::Continue(_) => (String::new(), "continue".to_owned()),
        }
    }
}

/// What a planner may look at.
pub struct PlanContext {
    /// Records the agent has open, with every action each publishes.
    pub open: Vec<(DocumentRoute, Vec<ActionDescriptor>)>,
    /// The on-screen controls of each open record.
    pub controls: Vec<(DocumentRoute, Vec<Control>)>,
    /// Records of the room the agent has seen but not opened.
    pub known: Vec<DocumentRoute>,
    /// Apps the agent may open.
    pub apps: Vec<String>,
}

/// Turns a task into steps.
pub trait Planner {
    /// The steps for `task`, or why there are none.
    ///
    /// # Errors
    ///
    /// Returns a reason shown to the person who gave the task.
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String>;
}

/// Reads the steps from the task: one JSON object or an array of them.
///
/// - `{"app", "record"?, "resource" | "control", "action", "arguments"?}`
///   invokes a published action. `control` names a control by its semantic
///   id (what a person sees and clicks), `resource` an app-declared
///   resource.
/// - `{"app", "record"?, "control", "input", "text"? | "value"? | "dx"?,
///   "dy"?}` operates a control directly; `input` is one of
///   [`INPUT_NAMES`].
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

/// The control inputs a step may name.
pub const INPUT_NAMES: &[&str] = &[
    "press",
    "focus",
    "set-value",
    "type",
    "scroll",
    "toggle",
    "increment",
    "decrement",
    "expand",
    "collapse",
];

/// The name a step and the task log give `input`.
#[must_use]
pub const fn input_name(input: &ControlInput) -> &'static str {
    match input {
        ControlInput::Press => "press",
        ControlInput::Focus => "focus",
        ControlInput::SetValue(_) => "set-value",
        ControlInput::TypeText(_) => "type",
        ControlInput::Scroll { .. } => "scroll",
        ControlInput::Toggle => "toggle",
        ControlInput::Increment => "increment",
        ControlInput::Decrement => "decrement",
        ControlInput::Expand => "expand",
        ControlInput::Collapse => "collapse",
    }
}

fn step(item: &serde_json::Value, context: &PlanContext) -> Result<Step, String> {
    let text = |key: &str| item.get(key).and_then(serde_json::Value::as_str);
    let app = text("app").ok_or("a step names its app")?.to_owned();
    if !context.apps.contains(&app) {
        return Err(format!("{app} cannot be opened by this agent"));
    }
    let record = text("record").map(str::to_owned);
    if let Some(input) = text("input") {
        let control = text("control")
            .ok_or("a step with an input names its control")?
            .to_owned();
        let number = |key: &str| item.get(key).and_then(serde_json::Value::as_f64);
        let string = |key: &str| {
            text(key)
                .map(str::to_owned)
                .ok_or_else(|| format!("a {input} step carries \"{key}\""))
        };
        let input = match input {
            "press" => ControlInput::Press,
            "focus" => ControlInput::Focus,
            "set-value" => ControlInput::SetValue(string("value")?),
            "type" => ControlInput::TypeText(string("text")?),
            "scroll" => ControlInput::Scroll {
                dx: number("dx").unwrap_or(0.0),
                dy: number("dy").unwrap_or(0.0),
            },
            "toggle" => ControlInput::Toggle,
            "increment" => ControlInput::Increment,
            "decrement" => ControlInput::Decrement,
            "expand" => ControlInput::Expand,
            "collapse" => ControlInput::Collapse,
            other => return Err(format!("{other} is not a control input")),
        };
        return Ok(Step {
            app,
            record,
            action: StepAction::Operate { control, input },
        });
    }
    let action = text("action").ok_or("a step names its action")?;
    let resource = match (text("resource"), text("control")) {
        (Some(resource), None) => resource.to_owned(),
        (None, Some(control)) => control_resource(control),
        _ => return Err("a step names either a resource or a control".into()),
    };
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

/// The resource id the semantic tree gives the control `node`.
#[must_use]
pub fn control_resource(node: &str) -> String {
    format!(
        "{}{}",
        crate::apps::SEMANTIC_RESOURCE_PREFIX,
        percent_encoding::utf8_percent_encode(node, percent_encoding::NON_ALPHANUMERIC)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PlanContext {
        PlanContext {
            open: Vec::new(),
            controls: Vec::new(),
            known: Vec::new(),
            apps: vec!["so.tilo.scratchpad".into()],
        }
    }

    #[test]
    fn steps_name_controls_or_declared_resources() {
        let steps = DirectPlanner
            .plan(
                r#"[{"app":"so.tilo.scratchpad","control":"note-editor","action":"set-value","arguments":{"value":"hi"}},
                    {"app":"so.tilo.scratchpad","record":"r1","resource":"scratchpad.note","action":"edit"},
                    {"app":"so.tilo.scratchpad","control":"note-editor","input":"type","text":"more"}]"#,
                &context(),
            )
            .unwrap();
        let StepAction::Invoke { address, .. } = &steps[0].action else {
            panic!("{:?}", steps[0]);
        };
        assert_eq!(address.resource, "ui:note%2Deditor");
        assert_eq!(
            crate::apps::semantic_node(&address.resource).as_deref(),
            Some("note-editor")
        );
        assert_eq!(steps[1].record.as_deref(), Some("r1"));
        assert_eq!(
            steps[2].action,
            StepAction::Operate {
                control: "note-editor".into(),
                input: ControlInput::TypeText("more".into())
            }
        );
        assert_eq!(
            steps[2].log_names(),
            ("ui:note%2Deditor".into(), "type".into())
        );
        assert!(DirectPlanner.plan("tidy up", &context()).is_err());
        assert!(
            DirectPlanner
                .plan(
                    r#"{"app":"so.tilo.scratchpad","control":"x","input":"type"}"#,
                    &context()
                )
                .is_err(),
            "typing needs text"
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
