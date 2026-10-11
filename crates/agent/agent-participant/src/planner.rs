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
        /// The action.
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
    /// The operation takes what the step before it produced: its content
    /// input is bound to that step's one content result when it runs, never
    /// to a value the plan wrote ([`tilo_agent::plan::carried_arguments`]).
    pub carries: bool,
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
        }
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
}

impl PlanContext {
    /// Whether the operation at `address` of `app` — on `record`, or on the
    /// record of `app` the agent has open — takes what the step before it
    /// produced, as its descriptor declares. `false` for an operation of a
    /// record that is not open.
    #[must_use]
    pub fn wants_content(&self, app: &str, record: Option<&str>, address: &ActionAddress) -> bool {
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
            .is_some_and(|descriptor| descriptor.input.wants_content())
    }

    /// Whether the operation at `address` that a new record of `app`
    /// publishes ([`Self::previews`]) takes what the step before it
    /// produced.
    #[must_use]
    pub fn preview_wants_content(&self, app: &str, address: &ActionAddress) -> bool {
        self.previews
            .iter()
            .filter(|(previewed, _)| previewed == app)
            .flat_map(|(_, descriptors)| descriptors)
            .find(|descriptor| descriptor.address == *address)
            .is_some_and(|descriptor| descriptor.input.wants_content())
    }
}

/// Whether `descriptor` only reads its record and answers with what it
/// found — a view, or an effect as observational as one (Word Processor
/// `read-text`, Sheets `read-cells`) — and so may be run while a task is
/// planned: it changes nothing and makes no content of its own.
#[must_use]
pub fn readable(descriptor: &ActionDescriptor) -> bool {
    use tilo_actions::{OperationKind, PermissionRequirement, ResultSpec, RiskLevel};
    let contract = &descriptor.contract;
    let answers = match &contract.result {
        ResultSpec::Json { .. } => true,
        ResultSpec::Resource { resource_kind } => {
            resource_kind != tilo_actions::CONTENT_RESOURCE_KIND
        }
        ResultSpec::None | ResultSpec::Job => false,
    };
    let reads = match descriptor.kind {
        OperationKind::View => true,
        OperationKind::Effect => contract.risk == RiskLevel::None,
        OperationKind::Document => false,
    };
    reads
        && answers
        && PermissionRequirement::Read.covers(contract.permission)
        && contract.confirmation == tilo_actions::ConfirmationPolicy::None
        && !descriptor.input.wants_content()
}

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
    let text = compact(read_content(value)).to_string();
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

/// `value` without inlined data and with long lists cut.
fn compact(value: &serde_json::Value) -> serde_json::Value {
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
    ///
    /// # Errors
    ///
    /// Returns a reason shown to the person who gave the task.
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
}

/// Reads the steps from the task: one JSON object or an array of them.
///
/// `{"app", "record"?, "resource", "action", "arguments"?, "carries"?}`
/// invokes an operation the record publishes on `resource`. An operation
/// that takes content is given what the step before it produced
/// (`carries`) unless its arguments name the content themselves; `carries`
/// says so explicitly, for a record that is not open yet.
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
    let address = ActionAddress::new(resource, action);
    let carries = match item.get("carries") {
        None => {
            !arguments.contains_key(tilo_actions::PROMPTED_PARAM)
                && context.wants_content(&app, record.as_deref(), &address)
        }
        Some(serde_json::Value::Bool(carries)) => *carries,
        Some(_) => return Err("carries is true or false".into()),
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
            !step(place.clone(), &context).carries,
            "a record that is not open declares nothing yet"
        );
        context.open.push((route, vec![descriptor]));
        assert!(step(place.clone(), &context).carries);
        let mut named = place.clone();
        named["record"] = "w1".into();
        assert!(step(named.clone(), &context).carries);
        named["record"] = "w2".into();
        assert!(!step(named, &context).carries, "another record");
        let mut explicit = place.clone();
        explicit["arguments"] = serde_json::json!({(tilo_actions::PROMPTED_PARAM): "abc"});
        assert!(
            !step(explicit, &context).carries,
            "content the step names itself is not replaced"
        );
        let mut said = place;
        said["carries"] = false.into();
        assert!(!step(said.clone(), &context).carries);
        said["carries"] = "yes".into();
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
            carried_arguments(&planned, false, Some(&produced)).unwrap(),
            planned,
            "a step that does not carry runs as planned"
        );
        let bound = carried_arguments(&planned, true, Some(&produced)).unwrap();
        assert_eq!(bound[tilo_actions::PROMPTED_PARAM], "abc123");
        assert_eq!(bound["caption"], "kept");

        let refused = carried_arguments(&planned, true, None).unwrap_err();
        assert!(refused.contains("no preceding action"), "{refused}");
        let nothing = tilo_actions::ActionOutput::default();
        assert!(carried_arguments(&planned, true, Some(&nothing)).is_err());
        let two = tilo_actions::ActionOutput {
            value: None,
            resources: vec!["tilo.content:a".into(), "tilo.content:b".into()],
        };
        assert!(carried_arguments(&planned, true, Some(&two)).is_err());
        let file = tilo_actions::ActionOutput {
            value: None,
            resources: vec!["files.entry:/tmp/a.png".into()],
        };
        assert!(carried_arguments(&planned, true, Some(&file)).is_err());
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
