//! A scenario as a program of steps, the same way a Shortcut is.
//!
//! A test that drives a window and a Shortcut that a person replays are the
//! same shape: ordered steps, each one an action the app publishes (invoked
//! through the one door), a few steps that touch the surface itself, and
//! assertions on what the run left behind. Defining them here, over the same
//! [`Shortcut`], is what keeps them one vocabulary: a saved Shortcut runs as a
//! scenario and a scenario's action steps are the Shortcut's own
//! [`StepKind::Invoke`], never a second copy that drifts from it.
//!
//! ```ignore
//! let scenario = Scenario {
//!     format: SCENARIO_FORMAT_VERSION,
//!     id: "rename-a-note".into(),
//!     name: "Rename a note".into(),
//!     steps: vec![
//!         ScenarioStep::Act(Box::new(Invocation {
//!             step: "rename".into(),
//!             action: ActionKey::new("so.tilo.notes", "notes.note", "rename"),
//!             resource: Selector::Fixed("notes.note:1".into()),
//!             args: BTreeMap::from([("title".into(), Binding::Literal("Trip".into()))]),
//!             appearance: None,
//!             expect_error: None,
//!             bind: None,
//!         })),
//!         ScenarioStep::Expect(Assert::Resource {
//!             resource: "notes.note:1".into(),
//!             state: StateQuery::present(),
//!             holds: Holds::fields([("title", serde_json::json!("Trip"))]),
//!         }),
//!     ],
//! };
//! let text = scenario.to_json().expect("a scenario is data");
//! assert_eq!(Scenario::from_json(&text), Ok(scenario));
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_actions::ActionKey;
use tilo_actions::params::ActionParam;

use crate::StepPath;
use crate::shortcut::{Binding, Selector, Shortcut, Step, StepKind};

pub mod tree;

/// The version of the scenario text format. Written beside the steps and
/// refused when it is not this one, so a golden file never reads as a newer
/// format's.
pub const SCENARIO_FORMAT_VERSION: u32 = 1;

/// Every door code a scenario may expect a step to be refused with, spelled
/// as the door spells them: one list, so a step that expects a refusal names
/// a code the door can actually answer.
pub const DOOR_CODES: &[&str] = &[
    "unknown_action",
    "invalid_arguments",
    "action_unavailable",
    "action_confirmation_required",
    "action_failed",
];

/// Whether `code` is one the door answers a refusal with
/// ([`DOOR_CODES`]), so a step's `expect_error` can be checked when the
/// scenario is read rather than only when it runs.
#[must_use]
pub fn is_door_code(code: &str) -> bool {
    DOOR_CODES.contains(&code)
}

/// One scenario: a named program over the Shortcut step model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The format this text is written in. Always
    /// [`SCENARIO_FORMAT_VERSION`] for the one this build writes; a reader
    /// refuses any other, so an old golden file never reads as a new format's.
    #[serde(default = "format_version")]
    pub format: u32,
    /// Stable id: what a report names, and what a Shortcut's run id is
    /// derived from when a saved shortcut runs as a scenario.
    pub id: String,
    /// What the scenario is called in a report.
    pub name: String,
    /// What a run is started with, in the same typed vocabulary actions use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ActionParam>,
    pub steps: Vec<ScenarioStep>,
}

fn format_version() -> u32 {
    SCENARIO_FORMAT_VERSION
}

impl Scenario {
    /// A saved Shortcut as a scenario: its input steps are the scenario's
    /// inputs and its `Invoke` steps are the scenario's action steps.
    ///
    /// `None` for a Shortcut that is not only action steps. A plan, a loop, a
    /// value the run asks a person for or the agent transforms is a program
    /// with branches; a scenario is a fixed sequence, and saying so is the
    /// point — it either runs exactly as written or fails. The caller says
    /// which of its steps it could not take.
    pub fn from_shortcut(shortcut: &Shortcut) -> Result<Self, UnrunnableStep> {
        let mut steps = Vec::with_capacity(shortcut.steps.len());
        for (index, step) in shortcut.steps.iter().enumerate() {
            let path = StepPath::root().child(index);
            match &step.kind {
                StepKind::Invoke {
                    action,
                    resource,
                    args,
                    appearance,
                } => steps.push(ScenarioStep::Act(Box::new(Invocation {
                    path: path.as_str().to_owned(),
                    step: step.id.clone(),
                    action: action.clone(),
                    resource: resource.clone(),
                    args: args.clone(),
                    appearance: appearance.clone(),
                    expect_error: None,
                    bind: None,
                }))),
                StepKind::If { .. }
                | StepKind::ForEach { .. }
                | StepKind::SetVar { .. }
                | StepKind::Ask { .. }
                | StepKind::Ai { .. } => {
                    return Err(UnrunnableStep {
                        path: path.as_str().to_owned(),
                        step: step.id.clone(),
                        kind: step.kind.name(),
                    });
                }
            }
        }
        Ok(Self {
            format: SCENARIO_FORMAT_VERSION,
            id: shortcut.id.clone(),
            name: shortcut.name.clone(),
            inputs: shortcut.inputs.clone(),
            steps,
        })
    }

    /// The steps a whole `Shortcut` holds, flattened: the scenario the run
    /// interpreter's report describes, which it can assert on.
    ///
    /// A nested program keeps its Shortcut id and name, so the scenario says
    /// where it came from. A run's own loops and branches run once per step
    /// of the program as written: see [`Self::from_shortcut`] for the
    /// narrower promise a scenario makes of itself.
    #[must_use]
    pub fn of_shortcut_program(shortcut: &Shortcut) -> Self {
        let mut steps = Vec::new();
        collect(&shortcut.steps, &StepPath::root(), &mut steps);
        Self {
            format: SCENARIO_FORMAT_VERSION,
            id: shortcut.id.clone(),
            name: shortcut.name.clone(),
            inputs: shortcut.inputs.clone(),
            steps,
        }
    }

    /// The canonical text of the scenario: keys in byte order at every depth
    /// and no insignificant whitespace, exactly as [`crate::program`] writes a
    /// program, so a golden file diffs to the step that changed.
    ///
    /// # Errors
    /// [`FormatError::Malformed`] when the value does not serialize.
    pub fn to_json(&self) -> Result<String, FormatError> {
        let value = serde_json::to_value(self)
            .map_err(|error| FormatError::Malformed(error.to_string()))?;
        let text = crate::program::canonical_json(&value);
        String::from_utf8(text).map_err(|error| FormatError::Malformed(error.to_string()))
    }

    /// Read [`Self::to_json`]'s text back, in the same spelling: anything
    /// else is refused rather than quietly re-encoded, so a golden file's
    /// bytes and the scenario they mean cannot drift apart.
    ///
    /// # Errors
    /// [`FormatError`] for text that is not this format's canonical spelling.
    pub fn from_json(text: &str) -> Result<Self, FormatError> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| FormatError::Malformed(error.to_string()))?;
        let version = value
            .get("format")
            .and_then(serde_json::Value::as_u64)
            .map_or(SCENARIO_FORMAT_VERSION, |version| {
                u32::try_from(version).unwrap_or(u32::MAX)
            });
        // A version this build does not write is refused by number, never by
        // whatever the fields happen to deserialize to.
        if version != SCENARIO_FORMAT_VERSION {
            return Err(FormatError::UnsupportedVersion(version));
        }
        let scenario: Self = serde_json::from_value(value)
            .map_err(|error| FormatError::Malformed(error.to_string()))?;
        scenario.check_codes()?;
        if scenario.to_json()?.as_str() != text {
            return Err(FormatError::NotCanonical);
        }
        Ok(scenario)
    }

    /// Every code this scenario expects a step to be refused with, refused
    /// here when the door has no such code: a scenario that expects a
    /// refusal that cannot happen passes only by the step not being refused,
    /// which is the opposite of what it was written to prove.
    ///
    /// # Errors
    /// [`FormatError::UnknownDoorCode`] naming the step and the code.
    pub fn check_codes(&self) -> Result<(), FormatError> {
        for (index, step) in self.steps.iter().enumerate() {
            let codes: Vec<&str> = match step {
                ScenarioStep::Act(invocation) => {
                    invocation.expect_error.iter().map(String::as_str).collect()
                }
                ScenarioStep::Expect(Assert::Error { code }) => vec![code.as_str()],
                ScenarioStep::Expect(_) | ScenarioStep::Input(_) | ScenarioStep::Advance { .. } => {
                    Vec::new()
                }
            };
            for code in codes {
                if !is_door_code(code) {
                    return Err(FormatError::UnknownDoorCode {
                        step: index,
                        code: code.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A step of a Shortcut that a scenario cannot hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnrunnableStep {
    /// Where it sits, as a report names it (`0.2`).
    pub path: String,
    pub step: String,
    /// Its kind, as [`StepKind::name`] spells it (`if`, `for_each`, `ai`).
    pub kind: &'static str,
}

impl std::fmt::Display for UnrunnableStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "step `{}` ({}) is a `{}` step, and a scenario is a fixed sequence of actions",
            self.step, self.path, self.kind
        )
    }
}

/// Why a scenario's text was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    #[error("the scenario is not in canonical form")]
    NotCanonical,
    #[error(
        "the scenario was written in format {0}, and this reads format {SCENARIO_FORMAT_VERSION}"
    )]
    UnsupportedVersion(u32),
    #[error("step {step} expects `{code}`, and the door refuses with no such code")]
    UnknownDoorCode { step: usize, code: String },
    #[error("the scenario is not readable: {0}")]
    Malformed(String),
}

/// What one step does.
///
/// The step itself is the `kind`: an action, a press, an assertion or a wait.
/// A step does not carry a second `step` tag of its own, so the action step's
/// `action` key is never shadowed by a variant tag of the same name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioStep {
    /// Invoke a published action: a Shortcut's own action step.
    Act(Box<Invocation>),
    /// A press, a key or a wait on the surface, for what only the surface
    /// can prove.
    Input(Input),
    /// Require something at this point: on the model, on the surface, or
    /// that the action was refused.
    Expect(Assert),
    /// Wait for the surface to settle, bounded.
    Advance { millis: u64 },
}

/// One action the scenario invokes, as a Shortcut invokes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    // The fields are the Shortcut's `StepKind::Invoke` ones, so a step that
    // moved between a Shortcut and a scenario moved nothing else.
    /// Where the step sits, `StepPath`'s notation (`0`, `2.then.1`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// The step's name, so a later step can bind what it answered.
    pub step: String,
    /// The action's typed address: its app, its resource kind and its id.
    pub action: ActionKey,
    /// Which resource it acts on.
    pub resource: Selector,
    /// Typed arguments.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, Binding>,
    /// The name and avatar a bot shows for this one action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Binding>,
    /// Expect the door to refuse with this code instead of running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_error: Option<String>,
    /// Keep what the action answered under this name, for a later step's
    /// `{{step.ID/output/...}}` placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
}

impl Invocation {
    /// Whether this step expects the door to refuse it.
    #[must_use]
    pub fn expects_refusal(&self) -> bool {
        self.expect_error.is_some()
    }
}

/// One thing done to the surface rather than to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    /// Click the centre of the widget a test id or path names.
    ClickNamed { target: String },
    /// Click at logical coordinates.
    ClickAt { x: f64, y: f64 },
    /// Direct-touch tap on a named widget.
    TapNamed { target: String },
    /// Direct-touch tap at logical coordinates.
    TapAt { x: f64, y: f64 },
    /// Drag one touch from start to end.
    SwipeAt { x0: f64, y0: f64, x1: f64, y1: f64 },
    /// Press and release one named key or one character.
    Key { key: String },
    /// Type a string, one press/release pair per character.
    Type { text: String },
    /// Hold these modifiers for the steps after (`ctrl`, `shift`, `alt`,
    /// `logo`, `command`); none clears them.
    Modifiers { modifiers: Vec<String> },
    /// Wait until a named widget appears, bounding the wait.
    WaitForNamed { target: String, millis: u64 },
    /// Ask a named widget to perform an accessibility action: the same path
    /// a screen reader's request takes.
    Perform {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        semantic_target: Option<String>,
        action: SemanticAction,
    },
}

/// One accessibility verb a scenario asks a widget to perform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticAction {
    Activate,
    Focus,
    ToggleCheck,
    Increment,
    Decrement,
    Expand,
    Collapse,
    ScrollIntoView,
    ShowContextMenu,
    SetValue { value: String },
    SetSelection { anchor: usize, focus: usize },
}

/// What a step requires to be true.
///
/// Not `deny_unknown_fields`: a resource assertion's fields are the thing's
/// own ([`Holds`]), and a serde enum cannot both flatten an open object and
/// refuse the names it does not know. What a step or a scenario is written as
/// is still refused when it carries a key this format does not define.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "expect", rename_all = "snake_case")]
pub enum Assert {
    /// The app publishes `resource`, and what it holds matches.
    Resource {
        /// The resource id, or a `{{placeholder}}` naming one.
        resource: String,
        /// Whether it is there.
        #[serde(flatten)]
        state: StateQuery,
        /// What it holds.
        holds: Holds,
    },
    /// What the run so far did: the steps it ran, how each ended, and what
    /// they answered.
    Run {
        /// Whether the run is there, which is to say it ran and finished.
        #[serde(flatten)]
        state: StateQuery,
        /// What it did.
        holds: Holds,
    },
    /// A retained widget's accessibility surface.
    Semantic { target: String },
    /// The door refuses an action with this code.
    Error { code: String },
}

/// What a resource, or a run, holds.
///
/// The fields sit under their own key beside the state they are read at
/// (`{"present":null,"holds":{"title":"Trip"}}`): flattening them into the
/// assertion would make a field named `present` or `resource` unwritable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holds {
    /// Each field equals its JSON value. None of them means anything at this
    /// field's position will do, which is the assertion that the thing is
    /// there at all.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, serde_json::Value>,
}

impl Holds {
    /// Nothing in particular: the thing is asserted to be there, not what it
    /// says.
    #[must_use]
    pub fn any() -> Self {
        Self::default()
    }

    /// These fields equal these values.
    #[must_use]
    pub fn fields<const N: usize>(fields: [(&str, serde_json::Value); N]) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        }
    }

    /// Whether every field this requires is `actual`'s, with that value.
    #[must_use]
    pub fn matches(&self, actual: &BTreeMap<String, serde_json::Value>) -> bool {
        self.fields
            .iter()
            .all(|(name, want)| actual.get(name) == Some(want))
    }

    /// Whether this requires nothing in particular.
    #[must_use]
    pub fn is_any(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Whether a resource, or a run, is there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateQuery {
    /// It is there.
    #[default]
    Present,
    /// It is gone.
    Absent,
}

impl StateQuery {
    /// Require it to be there.
    #[must_use]
    pub const fn present() -> Self {
        Self::Present
    }

    /// Require it to be gone.
    #[must_use]
    pub const fn absent() -> Self {
        Self::Absent
    }

    #[must_use]
    pub const fn is_present(&self) -> bool {
        matches!(self, Self::Present)
    }
}

/// [`StepKind::name`]: a step kind's wire name, for a report that names the
/// step a scenario could not take.
impl StepKind {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Invoke { .. } => "invoke",
            Self::If { .. } => "if",
            Self::ForEach { .. } => "for_each",
            Self::SetVar { .. } => "set_var",
            Self::Ask { .. } => "ask",
            Self::Ai { .. } => "ai",
        }
    }
}

/// Every step of `steps`, nested blocks included, addressed by its path.
fn collect(steps: &[Step], path: &StepPath, out: &mut Vec<ScenarioStep>) {
    for (index, step) in steps.iter().enumerate() {
        let here = path.child(index);
        match &step.kind {
            StepKind::Invoke {
                action,
                resource,
                args,
                appearance,
            } => out.push(ScenarioStep::Act(Box::new(Invocation {
                path: here.as_str().to_owned(),
                step: step.id.clone(),
                action: action.clone(),
                resource: resource.clone(),
                args: args.clone(),
                appearance: appearance.clone(),
                expect_error: None,
                bind: None,
            }))),
            StepKind::If {
                then, otherwise, ..
            } => {
                collect(then, &here.then_branch(), out);
                collect(otherwise, &here.else_branch(), out);
            }
            StepKind::ForEach { body, .. } => collect(body, &here.each(None), out),
            StepKind::SetVar { .. } | StepKind::Ask { .. } | StepKind::Ai { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(step: &str, resource: &str, title: &str) -> ScenarioStep {
        ScenarioStep::Act(Box::new(Invocation {
            path: String::new(),
            step: step.into(),
            action: ActionKey::new("so.tilo.notes", "notes.note", "rename"),
            resource: Selector::Fixed(resource.into()),
            args: BTreeMap::from([("title".into(), Binding::Literal(title.into()))]),
            appearance: None,
            expect_error: None,
            bind: None,
        }))
    }

    fn scenario() -> Scenario {
        Scenario {
            format: SCENARIO_FORMAT_VERSION,
            id: "rename-a-note".into(),
            name: "Rename a note".into(),
            inputs: Vec::new(),
            steps: vec![
                invocation("rename", "notes.note:1", "Trip"),
                ScenarioStep::Expect(Assert::Resource {
                    resource: "notes.note:1".into(),
                    state: StateQuery::present(),
                    holds: Holds::fields([("title", serde_json::json!("Trip"))]),
                }),
                ScenarioStep::Input(Input::ClickNamed {
                    target: "note-title".into(),
                }),
                ScenarioStep::Advance { millis: 250 },
            ],
        }
    }

    #[test]
    fn a_scenario_round_trips_through_canonical_text() {
        let scenario = scenario();
        let text = scenario.to_json().unwrap();
        assert_eq!(Scenario::from_json(&text).unwrap(), scenario);
        assert!(
            !text.contains(": ") && !text.contains('\n'),
            "no insignificant whitespace: {text}"
        );
        // Every step is one tagged object, so a diff names the step that
        // changed rather than rewriting an array of anonymous values.
        assert!(
            text.contains(
                r#"{"expect":"resource","holds":{"fields":{"title":"Trip"}},"kind":"expect","present":null,"resource":"notes.note:1"}"#
            ),
            "what it holds is a property of the resource asserted: {text}"
        );
        assert!(text.contains(r#""kind":"act""#), "{text}");
        assert!(text.contains(r#""format":1"#), "{text}");
    }

    #[test]
    fn the_codes_a_step_may_expect_are_the_door_s_own() {
        // Every refusal a scenario can expect is one the door answers with,
        // and every code the door answers with is expectable: a shorter list
        // would make a real refusal unwritable, a longer one would let a step
        // expect something that never happens.
        let address = tilo_actions::ActionAddress::new("r", "a");
        let door = [
            tilo_actions::DoorError::Unknown(address.clone()),
            tilo_actions::DoorError::MissingArgument(address.clone()),
            tilo_actions::DoorError::UnexpectedArgument(address),
            tilo_actions::DoorError::InvalidArguments("x".into()),
            tilo_actions::DoorError::Unavailable {
                code: "x".into(),
                reason: "x".into(),
            },
            tilo_actions::DoorError::ConfirmationRequired,
            tilo_actions::DoorError::UnregisteredType("x".into()),
            tilo_actions::DoorError::Failed("x".into()),
        ];
        for error in &door {
            assert!(
                is_door_code(error.code()),
                "the door answers `{}`, and a step cannot expect it",
                error.code()
            );
        }
        assert!(!is_door_code("ok"));
        assert!(!is_door_code(""));
    }

    #[test]
    fn text_that_is_not_the_canonical_spelling_is_refused() {
        let text = scenario().to_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        assert_eq!(
            Scenario::from_json(&pretty),
            Err(FormatError::NotCanonical),
            "a golden file is one spelling"
        );
        let mut newer = value.clone();
        newer["format"] = serde_json::json!(SCENARIO_FORMAT_VERSION + 1);
        let newer = String::from_utf8(crate::program::canonical_json(&newer)).unwrap();
        assert_eq!(
            Scenario::from_json(&newer),
            Err(FormatError::UnsupportedVersion(SCENARIO_FORMAT_VERSION + 1))
        );
        let mut unknown = value;
        unknown["extra"] = serde_json::json!(1);
        let unknown = String::from_utf8(crate::program::canonical_json(&unknown)).unwrap();
        assert!(matches!(
            Scenario::from_json(&unknown),
            Err(FormatError::Malformed(_))
        ));
        assert!(matches!(
            Scenario::from_json("{"),
            Err(FormatError::Malformed(_))
        ));
    }

    #[test]
    fn a_step_expecting_a_refusal_the_door_cannot_make_is_refused_when_read() {
        let mut scenario = scenario();
        scenario.steps.push(ScenarioStep::Expect(Assert::Error {
            code: "not_our_verb".into(),
        }));
        let text = serde_json::to_value(&scenario).unwrap();
        let text = String::from_utf8(crate::program::canonical_json(&text)).unwrap();
        assert_eq!(
            Scenario::from_json(&text),
            Err(FormatError::UnknownDoorCode {
                step: 4,
                code: "not_our_verb".into(),
            }),
            "a refusal the door cannot answer would make the step unreachable"
        );
        // A code the door does answer is a scenario like any other.
        scenario.steps[4] = ScenarioStep::Expect(Assert::Error {
            code: "action_unavailable".into(),
        });
        let text = scenario.to_json().unwrap();
        assert_eq!(Scenario::from_json(&text).unwrap(), scenario);
    }

    #[test]
    fn a_saved_shortcut_runs_as_a_scenario_of_its_action_steps() {
        let shortcut = Shortcut {
            id: "tidy".into(),
            name: "Tidy".into(),
            inputs: Vec::new(),
            steps: vec![Step::new(
                "rename",
                StepKind::Invoke {
                    action: ActionKey::new("so.tilo.notes", "notes.note", "rename"),
                    resource: Selector::Fixed("notes.note:1".into()),
                    args: BTreeMap::from([("title".into(), Binding::Literal("Trip".into()))]),
                    appearance: None,
                },
            )],
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
        };
        let scenario = Scenario::from_shortcut(&shortcut).unwrap();
        assert_eq!(scenario.id, "tidy");
        assert_eq!(scenario.steps.len(), 1);
        let ScenarioStep::Act(invocation) = &scenario.steps[0] else {
            panic!("an invoked action step is a scenario step");
        };
        assert_eq!(invocation.step, "rename");
        assert_eq!(invocation.resource, Selector::Fixed("notes.note:1".into()));

        // The scenario's step is the Shortcut's step: one definition, so the
        // scenario's own text reads back with the same action step.
        let text = scenario.to_json().unwrap();
        assert!(
            text.contains(r#""action":{"action":"rename","app":"so.tilo.notes""#),
            "{text}"
        );
        assert_eq!(Scenario::from_json(&text).unwrap(), scenario);
    }

    #[test]
    fn a_shortcut_with_a_branch_is_not_a_scenario_and_says_where() {
        let shortcut = Shortcut {
            id: "branchy".into(),
            name: "Branchy".into(),
            inputs: Vec::new(),
            steps: vec![
                Step::new(
                    "note",
                    StepKind::SetVar {
                        name: "note".into(),
                        value: Binding::Literal("notes.note:1".into()),
                    },
                ),
                Step::new(
                    "choose",
                    StepKind::If {
                        cond: crate::Condition::Truthy(Binding::Literal(serde_json::json!(true))),
                        then: Vec::new(),
                        otherwise: Vec::new(),
                    },
                ),
            ],
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
        };
        let refused = Scenario::from_shortcut(&shortcut).unwrap_err();
        assert_eq!(refused.path, "0");
        assert_eq!(refused.kind, "set_var");

        // Flattening the same program names both steps, and the nested
        // blocks none of them hold.
        let flattened = Scenario::of_shortcut_program(&shortcut);
        assert_eq!(flattened.id, "branchy");
        assert!(flattened.steps.is_empty());
    }
}
