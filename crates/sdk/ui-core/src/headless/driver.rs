//! One drive, run in-process: the steps a scenario names, performed through
//! the shipped [`FrameHarness`] path, answering what the app showed rather
//! than writing it to files.
//!
//! [`drive`] runs the steps `--headless-drive` runs, on the same
//! [`FrameHarness`] and through the same [`FrameHarness::perform_bound`] — no
//! test copy — and answers the state the app is in afterwards where the CLI
//! writes it to files. A scenario that passes here therefore passes in the
//! app's own process, on the same frames; what changes is only who reads the
//! answer.

use super::{Action, FrameHarness};
use tilo_sort::CompactSort as _;

/// One published resource, its actions (address, kind, availability and
/// parameters) and its children, as the `resources` answer lists them.
pub(super) fn resource_json(resource: &tilo_actions::Resource) -> serde_json::Value {
    let actions: Vec<serde_json::Value> = resource
        .actions
        .iter()
        .map(|action| {
            let unavailable = match &action.contract.availability {
                tilo_actions::Availability::Unavailable { reason, .. } => Some(reason.clone()),
                tilo_actions::Availability::Available => None,
            };
            serde_json::json!({
                "action": action.id,
                "label": action.label,
                "params": action.params(),
                "unavailable": unavailable,
            })
        })
        .collect();
    serde_json::json!({
        "resource": resource.id.0,
        "kind": resource.kind,
        "label": resource.label,
        "actions": actions,
        "children": resource.children.iter().map(resource_json).collect::<Vec<_>>(),
    })
}

/// The answers earlier steps bound, by the name their step kept them under
/// (`{"name": {"value": ..., "resources": [...]}}`): what a later invocation's
/// resource and arguments fill their `{{name.path}}` placeholders from.
pub type Bindings = std::collections::BTreeMap<String, serde_json::Value>;

/// One step of a drive. Every action `--headless-drive` accepts is a step,
/// and so is a question about the app's state that no action asks.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// One action from the headless grammar, run through
    /// [`FrameHarness::perform`] as `--headless-drive` runs it.
    Action(Action),
    /// The document as it stands now is exactly this JSON: the app's own
    /// state as a value, which no widget's text can stand in for.
    Document(serde_json::Value),
    /// The named resource is published now — `notes`, an app's `library`,
    /// the host's `history` — and, when `action` is named, it publishes that
    /// action too. What a scenario needs to `invoke` it next.
    Resource { id: String, action: Option<String> },
}

impl Step {
    /// The action this step runs, when it runs one.
    #[must_use]
    pub fn action(&self) -> Option<&Action> {
        match self {
            Self::Action(action) => Some(action),
            Self::Document(_) | Self::Resource { .. } => None,
        }
    }

    /// This step as a report names it.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Action(action) => super::action_description(action),
            Self::Document(_) => "expect the document".into(),
            Self::Resource { id, action: None } => format!("expect resource {id:?}"),
            Self::Resource {
                id,
                action: Some(action),
            } => format!("expect resource {id:?} to publish {action:?}"),
        }
    }
}

/// The steps a `--headless-drive` payload names, in order: its actions, as
/// the steps [`drive`] runs them.
#[must_use]
pub fn steps_of(spec: &super::Spec) -> Vec<Step> {
    spec.actions.iter().cloned().map(Step::Action).collect()
}

impl From<Action> for Step {
    fn from(action: Action) -> Self {
        Self::Action(action)
    }
}

/// What driving one step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStatus {
    /// The step did what it names.
    Passed,
    /// The step, or waiting for what it names, failed.
    Failed,
}

/// One step, and what it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepRecord {
    /// The step's place in the drive, from zero.
    pub index: usize,
    pub status: StepStatus,
    /// What went wrong, in words a report prints.
    pub error: Option<String>,
}

impl StepRecord {
    #[must_use]
    pub const fn passed(index: usize) -> Self {
        Self {
            index,
            status: StepStatus::Passed,
            error: None,
        }
    }

    #[must_use]
    pub fn failed(index: usize, error: impl Into<String>) -> Self {
        Self {
            index,
            status: StepStatus::Failed,
            error: Some(error.into()),
        }
    }

    /// Whether the step did what it names.
    #[must_use]
    pub const fn is_pass(&self) -> bool {
        matches!(self.status, StepStatus::Passed)
    }
}

/// Everything one drive answers: the state the app is in once the steps ran,
/// step by step, and the first step that failed.
///
/// The answers are values, so a test asserts on them and a CLI serialises
/// them; nothing here names a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The document as it stands at the end, the JSON `dump` writes.
    pub document: serde_json::Value,
    /// The inspection tree `inspect` writes: each widget, its bounds and what
    /// it shows.
    pub inspection: serde_json::Value,
    /// Every addressable widget with its `(x, y, width, height)` in the window,
    /// by the name a click or an expectation addresses it with, sorted by name:
    /// what `targets` writes.
    pub targets: Vec<(String, [f64; 4])>,
    /// The resources the app publishes, each action's address among them.
    pub resources: Vec<serde_json::Value>,
    /// One record per step that ran, in order.
    pub steps: Vec<StepRecord>,
}

impl Outcome {
    /// The first step that failed, if one did.
    #[must_use]
    pub fn failure(&self) -> Option<&StepRecord> {
        self.steps.iter().find(|step| !step.is_pass())
    }

    /// Whether every step passed.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.failure().is_none()
    }
}

/// Run `steps` against the app `harness` hosts and answer what it shows.
///
/// Each step runs through the path `--headless-drive` runs it on, and the
/// harness is left where the last step put it, so the caller keeps driving it
/// or closes it as it would after a CLI run. A step that fails ends the drive:
/// the steps after it would run against state the failed one never produced,
/// and the record names the one that stopped it.
pub fn drive(harness: &mut FrameHarness, steps: &[Step]) -> Outcome {
    let (records, _bindings) = run_steps(harness, steps);
    Outcome {
        document: read_document(harness),
        inspection: read_inspection(harness),
        targets: read_targets(harness),
        resources: read_resources(harness),
        steps: records,
    }
}

/// Run each step, keeping the answers every step bound for the ones after
/// it. Answers what each step did, stopping at the first failure.
pub(crate) fn run_steps(harness: &mut FrameHarness, steps: &[Step]) -> (Vec<StepRecord>, Bindings) {
    let mut records = Vec::with_capacity(steps.len());
    let mut bindings = Bindings::new();
    refresh(harness);
    for (index, step) in steps.iter().enumerate() {
        match run_step(harness, step, &mut bindings) {
            Ok(()) => records.push(StepRecord::passed(index)),
            Err(error) => {
                records.push(StepRecord::failed(index, error));
                break;
            }
        }
        // A step's edits reach the writer as an idle pass offers them, so the
        // next step reads the state the frame shows, not the one before it.
        refresh(harness);
    }
    (records, bindings)
}

/// Draw a frame and let its edits land, as a live session does between two
/// things a person does: the steps after it judge the state that frame shows.
fn refresh(harness: &mut FrameHarness) {
    harness.relayout();
    harness.persist_tick();
}

fn run_step(
    harness: &mut FrameHarness,
    step: &Step,
    bindings: &mut Bindings,
) -> Result<(), String> {
    match step {
        Step::Action(_) => harness.perform_bound(step, bindings),
        Step::Document(expected) => {
            let observed = read_document(harness);
            if observed == *expected {
                return Ok(());
            }
            Err(format!(
                "document expected {}, observed {}",
                compact(expected),
                compact(&observed)
            ))
        }
        Step::Resource { id, action } => {
            let resources = read_resources(harness);
            let found = resources
                .iter()
                .find(|resource| resource.get("resource").and_then(|r| r.as_str()) == Some(id));
            let Some(found) = found else {
                return Err(format!("no resource {id:?} is published"));
            };
            let Some(action) = action else {
                return Ok(());
            };
            let publishes = found
                .get("actions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|actions| {
                    actions.iter().any(|published| {
                        published.get("action").and_then(|a| a.as_str()) == Some(action.as_str())
                    })
                });
            if publishes {
                Ok(())
            } else {
                Err(format!("resource {id:?} publishes no action {action:?}"))
            }
        }
    }
}

fn compact(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| format!("<{error}>"))
}

fn read_document(harness: &mut FrameHarness) -> serde_json::Value {
    serde_json::from_str(&harness.dump_json()).unwrap_or_else(
        |error| serde_json::json!({ "error": format!("the document is not JSON: {error}") }),
    )
}

fn read_inspection(harness: &FrameHarness) -> serde_json::Value {
    harness.inspection_json().map_or_else(
        |error| serde_json::json!({ "error": format!("the inspection is not JSON: {error}") }),
        |json| serde_json::from_str(&json).unwrap_or(serde_json::Value::Null),
    )
}

/// Every addressable widget and its rect, sorted by name so two drives of one
/// app answer the same list.
fn read_targets(harness: &FrameHarness) -> Vec<(String, [f64; 4])> {
    let mut targets: Vec<(String, [f64; 4])> = harness
        .targets()
        .into_iter()
        .map(|(name, rect)| (name, harness.window_rect(rect)))
        .map(|(name, rect)| (name, [rect.x0, rect.y0, rect.width(), rect.height()]))
        .collect();
    // Sorted, so two drives of one app answer the same list whatever order
    // the widget walk found the targets in.
    targets.compact_sort_by(|a, b| a.0.cmp(&b.0));
    targets
}

fn read_resources(harness: &FrameHarness) -> Vec<serde_json::Value> {
    harness.resources().iter().map(resource_json).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{State, Theme, parse_spec};

    /// A focused field, as a scenario leaves one before it types.
    fn field(value: &str) -> FrameHarness {
        FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(
                crate::widget::TextInput::local()
                    .with_id("field")
                    .with_initial_value(value),
            ),
            (400, 200),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture-user".into()),
        )
    }

    /// The text the field shows, as an app reads back the value it edits.
    fn shown(harness: &FrameHarness) -> String {
        harness
            .access_node_named("field", None)
            .ok()
            .and_then(|node| node.value)
            .unwrap_or_default()
    }

    /// The `Spec` a CLI payload names, parsed by the parser
    /// `--headless-drive` parses it with.
    fn spec(actions: serde_json::Value) -> crate::headless::Spec {
        parse_spec(&serde_json::json!({ "actions": actions }).to_string())
            .expect("the payload is a scenario")
    }

    #[test]
    fn a_drive_runs_the_steps_a_cli_payload_names() {
        let mut harness = field("hello");
        let steps = steps_of(&spec(serde_json::json!([
            { "perform": { "target": "field", "action": "set_value", "value": "typed" } },
            { "expect": { "target": "field", "value": "typed" } }
        ])));
        let outcome = drive(&mut harness, &steps);
        assert!(outcome.is_pass(), "{:?}", outcome.steps);
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(shown(&harness), "typed");
    }

    #[test]
    fn a_drive_answers_the_state_the_app_is_in() {
        let mut harness = field("hello");
        let outcome = drive(&mut harness, &[]);
        assert!(outcome.is_pass());
        // The document read back as a value rather than a file.
        assert_eq!(outcome.document, read_document(&mut harness));
        // Every addressable widget and its rect in the window, sorted by name
        // so two drives of one app answer the same list.
        let (_, [x, y, width, height]) = outcome
            .targets
            .iter()
            .find(|(name, _)| name == "field")
            .expect("the field is addressable");
        assert!(
            *width > 0.0 && *height > 0.0,
            "({x}, {y}, {width}, {height})"
        );
        let mut sorted = outcome.targets.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(outcome.targets, sorted);
        // The inspection tree those names come from, and the resources the app
        // publishes — the field's own among them.
        assert!(outcome.inspection.is_object(), "{}", outcome.inspection);
        assert!(
            outcome
                .resources
                .iter()
                .any(|resource| resource["kind"] == "tilo.ui.control"),
            "{:?}",
            outcome.resources
        );
    }

    #[test]
    fn a_step_naming_a_resource_the_app_does_not_publish_fails() {
        let mut harness = field("hello");
        let outcome = drive(
            &mut harness,
            &[Step::Resource {
                id: "no-such-resource".into(),
                action: None,
            }],
        );
        let failure = outcome.failure().expect("nothing publishes that");
        assert_eq!(failure.index, 0);
        let error = failure.error.clone().unwrap_or_default();
        assert!(error.contains("no-such-resource"), "{failure:?}");
    }

    #[test]
    fn a_drive_stops_at_the_first_step_that_fails_and_names_it() {
        let mut harness = field("hello");
        let empty = read_document(&mut harness);
        let steps = [
            Step::Document(empty.clone()),
            Step::Document(serde_json::json!({ "field": "goodbye" })),
            Step::Action(Action::Type("!".into())),
        ];
        let outcome = drive(&mut harness, &steps);
        let failure = outcome.failure().expect("the second step cannot pass");
        assert_eq!(failure.index, 1);
        let error = failure.error.as_deref().unwrap_or_default();
        // The record says what the step expected and what the app showed.
        assert!(error.contains("goodbye"), "{error}");
        assert!(error.contains(&compact(&empty)), "{error}");
        // The step after the failure never ran: it would judge state the failed
        // step never produced, so there is no record of it.
        assert_eq!(outcome.steps.len(), 2, "{:?}", outcome.steps);
        assert!(outcome.steps[0].is_pass() && !outcome.steps[1].is_pass());
    }

    #[test]
    fn a_failed_step_still_answers_the_state_the_app_is_in() {
        let mut harness = field("hello");
        let steps = steps_of(&spec(serde_json::json!([
            { "expect": { "target": "field", "value": "goodbye" } }
        ])));
        let outcome = drive(&mut harness, &steps);
        assert!(!outcome.is_pass(), "{:?}", outcome.steps);
        // The failure does not cost the caller the state it wants to look at.
        assert_eq!(shown(&harness), "hello");
        assert!(!outcome.targets.is_empty());
        assert_eq!(outcome.document, read_document(&mut harness));
    }
}
