//! What a program's steps ask of the host that runs it.
//!
//! A program of a person's own (a reminder, an event their assistant set
//! up) does what no record of the workspace does for it: it hands an
//! instruction to that person's assistant, and it asks that person's model
//! for one judgement (does this matter, which of these is it). In the
//! Shortcut language these are the agent's transforms, `task`, `classify`
//! and `freeform`, which the agent app answers on a device. On a host, with
//! no device online, the bot's API offers the same actions and the host
//! answers them through [`HostSteps`], so one program reads the same
//! wherever it runs.
//!
//! Only a program that runs for a person is offered these steps
//! ([`super::api::BotApi::runs_for`]): an operator's bot acts for nobody,
//! and has no assistant to instruct and no model of its own to ask.
//!
//! A program also keeps things between its runs ([`super::kept`]): counts,
//! and its note. Two actions on the program's own resource reach them,
//! `count` and `keep`, answered through [`Keeps`] by the host that keeps
//! them ([`super::api::BotApi::keeps`]). The note goes with every
//! instruction and judgement, so whoever answers reads what the program is
//! for and what its last run left. A judgement is the agent's transform over
//! whatever model the host reaches for its person ([`super::judging`]).

use crate::transform::{MAX_INPUT_BYTES, MAX_PROMPT_BYTES};
use serde_json::{Map, Value, json};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::contract::{
    ActionContract, ActionKind, Audience, ConfirmationPolicy, ExecutionMode, ResultSpec,
    RetryPolicy, RiskLevel,
};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::policy::{Answer, Rule, TreePath};
use tilo_actions::remote::{CatalogueResource, CatalogueRow};
use tilo_actions::{ExecutionLocus, InvocationId, OperationKind};
use tilo_automation::{
    AGENT_APP_ID, AGENT_CLASSIFY_ACTION, AGENT_FREEFORM_ACTION, AGENT_TASK_ACTION,
    AGENT_TRANSFORM_KIND, AGENT_TRANSFORM_RESOURCE, SHORTCUT_RESOURCE_KIND, SHORTCUTS_APP_ID,
};

use super::api::AutomationStep;
use super::kept::NoteChange;

/// The provider the host's own steps are offered under.
pub const PROVIDER: &str = "tilo.host.steps";

/// The failure code of a step the host refused for good.
pub const REFUSED: &str = "host_step_refused";

/// Advancing one of the program's counts: an action of the program's own
/// resource.
pub const COUNT_ACTION: &str = "count";

/// Changing the program's note: an action of the program's own resource.
pub const KEEP_ACTION: &str = "keep";

/// The agent's transforms a host answers.
const ANSWERED: [&str; 3] = [
    AGENT_TASK_ACTION,
    AGENT_CLASSIFY_ACTION,
    AGENT_FREEFORM_ACTION,
];

/// What caused a host step: the program, the step and the event that fired
/// its run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepCause {
    /// The program's installation.
    pub installation: String,
    /// The step's own invocation: the same one when the run takes the step
    /// again after a restart, so the host answers it once.
    pub invocation: InvocationId,
    /// The event that fired the run, by its identity
    /// ([`tilo_automation::dispatch::Event::identity`]), when a trigger did.
    pub event: Option<String>,
}

/// An instruction a program hands to the assistant of the person it runs
/// for.
#[derive(Clone, Debug, PartialEq)]
pub struct Instruction {
    /// The person whose assistant takes it: their root key, hex.
    pub person: String,
    /// The workspace the program runs in.
    pub workspace: String,
    /// What to do, as the program carries it.
    pub instruction: String,
    /// The actions the step lets the work use, one grant tree node each;
    /// empty when it names none. They narrow what the assistant may do for
    /// this instruction below what it may do anyway, and never widen it.
    pub allow: Vec<Rule>,
    /// The actions that leave the workspace which the program's person let
    /// its work take without asking ([`super::kept::Kept::permit`]): rules
    /// that each name one action. Everything else the work would send or
    /// change outside still asks ([`Self::lets_through`]).
    pub permits: Vec<Rule>,
    /// The program's note as it stands ([`super::kept::Note`]): what the
    /// program is for, how its person wants it done, the examples kept and
    /// what its last run left. Null for a program that keeps none.
    pub note: Value,
    pub cause: StepCause,
    /// The native look's original assistant grant; programs cannot set it.
    pub look_grant: Option<String>,
}

impl Instruction {
    /// Whether the work this instruction starts may take the action at
    /// `node`, one that leaves the workspace, without asking its person:
    /// only when a permission the program carries names exactly that
    /// action. Having set a program up gives it no reach of its own.
    #[must_use]
    pub fn lets_through(&self, node: &TreePath) -> bool {
        let granted = tilo_automation::grant_answer(&self.permits, node, ActionKind::Outside);
        granted.answer == Answer::Allow && granted.names_action
    }
}

/// What a judgement asks about its input.
#[derive(Clone, Debug, PartialEq)]
pub enum Question {
    /// Which of `choices` describes the input; with `multi`, every one that
    /// does.
    Labels { choices: Vec<String>, multi: bool },
    /// `prompt`, answered as text, or as a value of `schema` when the step
    /// declares one.
    Prompt {
        prompt: String,
        schema: Option<Value>,
    },
}

impl Question {
    /// The transform that answers this question, held to the bounds the
    /// agent app holds one to on a device: how many labels and how long,
    /// how long a prompt, and a shape an answer can be checked against.
    ///
    /// # Errors
    /// The bound the question is past, in words for whoever reads the run.
    pub fn transform(&self) -> Result<crate::transform::Transform, String> {
        use crate::transform::Transform;
        match self {
            Self::Labels { choices, multi } => Transform::classify(choices.clone(), *multi),
            Self::Prompt { prompt, schema } => Transform::freeform(prompt, schema.clone()),
        }
        .map_err(|failure| failure.message.clone())
    }
}

/// One judgement a program asks of the model of the person it runs for.
#[derive(Clone, Debug, PartialEq)]
pub struct Judgement {
    /// The person whose model answers: their root key, hex.
    pub person: String,
    /// The workspace the program runs in.
    pub workspace: String,
    pub question: Question,
    /// The text judged, when the step binds one: what fired the run, or
    /// what an earlier step read.
    pub input: Option<String>,
    /// The program's note as it stands, as an instruction carries it
    /// ([`Instruction::note`]): what its person told it counts.
    pub note: Value,
    pub cause: StepCause,
}

/// Why the host did not take a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostStepError {
    /// Nothing can take it now and something may later: the run is tried
    /// again.
    Unavailable(String),
    /// It will not be taken however often it is asked, for this reason,
    /// which whoever reads the run is told.
    Refused(String),
}

/// What a host answers for the programs that run for a person. Both are
/// called from the run's own blocking thread, so waiting in them holds no
/// runtime worker.
pub trait HostSteps: Send + Sync {
    /// Hand `instruction` to its person's assistant, which starts a turn
    /// with it. Answers what the assistant said when the host waits for the
    /// turn, and an empty text when it only hands the instruction over.
    ///
    /// A step taken again (the same [`StepCause::invocation`]) is answered
    /// again and never starts a second turn.
    ///
    /// # Errors
    /// [`HostStepError`]: nothing can take it now, or it is refused for good.
    fn instruct(&self, instruction: Instruction) -> Result<String, HostStepError>;

    /// Answer `judgement` with one call of its person's own model, the one
    /// they chose for the kind of work `classifier`. Answers the chosen
    /// label, or every chosen label as an array when the question asks for
    /// all that apply; a prompt's answer is text, or a value of the shape
    /// the step declared.
    ///
    /// A judgement changes nothing, so one asked again is simply answered
    /// again.
    ///
    /// # Errors
    /// [`HostStepError`]: no model can answer now, or the person has none
    /// for this kind of work.
    fn judge(&self, judgement: Judgement) -> Result<Value, HostStepError>;
}

/// What a host keeps for one program between its runs
/// ([`super::kept::Kept`]), and how its steps reach it.
pub trait Keeps: Send + Sync {
    /// Advance the count `name` by one for `step` of its run, and answer
    /// where the count stands. The same step of the same run asked again,
    /// as a run that is tried again asks, is answered where the count
    /// stands and advances nothing.
    ///
    /// # Errors
    /// [`HostStepError`]: the count cannot be kept now, or it is one too
    /// many.
    fn count(&self, step: &AutomationStep, name: &str) -> Result<u64, HostStepError>;

    /// Make `change` to the program's note.
    ///
    /// # Errors
    /// [`HostStepError`]: the note cannot be kept now, or a part of the
    /// change is past its bound.
    fn keep(&self, change: NoteChange) -> Result<(), HostStepError>;

    /// The program's note as it stands, every part present.
    fn note(&self) -> Value;

    /// The actions that leave the workspace which the program's person let
    /// its work take without asking ([`super::kept::Kept::permit`]).
    fn permits(&self) -> Vec<Rule>;
}

/// Whom a step is asked for, and what caused it.
pub(crate) struct AskedFor<'a> {
    /// The person the program runs for, when it runs for one.
    pub person: Option<&'a str>,
    pub workspace: &'a str,
    pub cause: StepCause,
    /// Which step of which run asks, as its context says.
    pub step: Option<AutomationStep>,
    /// The program's note as it stands; null when it keeps none.
    pub note: Value,
    /// The permissions the program carries; none when it keeps none.
    pub permits: Vec<Rule>,
}

/// One step a host answers, read from the action it invokes and its
/// arguments.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Asked {
    Instruct(Instruction),
    Judge(Judgement),
    Count {
        name: String,
        every: Option<u64>,
        step: AutomationStep,
    },
    Keep(NoteChange),
}

impl Asked {
    /// What `action` with `arguments` asks of the host: `None` for an action
    /// no host answers, and for one only a program that runs for a person
    /// is answered when this one runs for nobody.
    ///
    /// # Errors
    /// Why the arguments do not say what is asked, in words for whoever
    /// reads the run.
    pub(crate) fn read(
        action: &str,
        arguments: &Map<String, Value>,
        asked_for: AskedFor<'_>,
    ) -> Result<Option<Self>, String> {
        let arguments = Arguments(arguments);
        match action {
            AGENT_TASK_ACTION => {
                instruction(&arguments, asked_for).map(|read| read.map(Self::Instruct))
            }
            AGENT_CLASSIFY_ACTION | AGENT_FREEFORM_ACTION => {
                judgement(action, &arguments, asked_for).map(|read| read.map(Self::Judge))
            }
            COUNT_ACTION => count(&arguments, asked_for.step).map(Some),
            KEEP_ACTION => serde_json::from_value(Value::Object(arguments.0.clone()))
                .map(|change| Some(Self::Keep(change)))
                .map_err(|error| format!("what to keep does not read: {error}")),
            _ => Ok(None),
        }
    }

    /// The step's value, as the host answers it: `steps` for the person the
    /// program runs for, `keeps` for what the program keeps. `None` when
    /// the host has not the one the step needs. A classification is held to
    /// the labels it gave: an answer that names another is no answer.
    pub(crate) fn answered_by(
        self,
        steps: Option<&dyn HostSteps>,
        keeps: Option<&dyn Keeps>,
    ) -> Option<Result<Option<Value>, HostStepError>> {
        Some(match self {
            Self::Instruct(instruction) => steps?
                .instruct(instruction)
                .map(|answer| Some(Value::String(answer))),
            Self::Judge(judgement) => judged(steps?, judgement).map(Some),
            Self::Count { name, every, step } => keeps?.count(&step, &name).map(|count| {
                Some(json!({
                    "count": count,
                    "reached": every.is_some_and(|every| count.checked_rem(every) == Some(0)),
                }))
            }),
            Self::Keep(change) => keeps?.keep(change).map(|()| None),
        })
    }
}

/// A step's arguments, as its readers ask for them.
struct Arguments<'a>(&'a Map<String, Value>);

impl<'a> Arguments<'a> {
    /// The argument `name`, when it is given and not null.
    fn given(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name).filter(|value| !value.is_null())
    }

    /// The argument `name` as text that says something.
    fn text(&self, name: &str) -> Option<&'a str> {
        self.given(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
}

/// The instruction a task step hands over: `None` for a program that runs
/// for nobody.
fn instruction(
    arguments: &Arguments<'_>,
    asked_for: AskedFor<'_>,
) -> Result<Option<Instruction>, String> {
    let Some(person) = asked_for.person else {
        return Ok(None);
    };
    let instruction = arguments
        .text("instruction")
        .ok_or("an instruction says what to do")?;
    // A host hands the instruction over and answers text: a summary of a
    // declared shape is what a task on a device answers.
    if arguments.given("schema").is_some() {
        return Err("an instruction handed over on a host answers text: declare no output".into());
    }
    let allow = match arguments.given("allow") {
        None => Vec::new(),
        Some(rules) => serde_json::from_value(rules.clone())
            .map_err(|error| format!("the actions the instruction allows do not read: {error}"))?,
    };
    Ok(Some(Instruction {
        look_grant: None,
        person: person.to_owned(),
        workspace: asked_for.workspace.to_owned(),
        instruction: instruction.to_owned(),
        allow,
        permits: asked_for.permits,
        note: asked_for.note,
        cause: asked_for.cause,
    }))
}

/// The judgement a classify or an AI step asks: `None` for a program that
/// runs for nobody.
fn judgement(
    action: &str,
    arguments: &Arguments<'_>,
    asked_for: AskedFor<'_>,
) -> Result<Option<Judgement>, String> {
    let Some(person) = asked_for.person else {
        return Ok(None);
    };
    if arguments.given("resource").is_some() {
        return Err("a host judges the text a step gives it: bind `input`, not `resource`".into());
    }
    let input = match arguments.given("input") {
        None => None,
        Some(Value::String(input)) if input.len() <= MAX_INPUT_BYTES => Some(input.clone()),
        Some(Value::String(_)) => {
            return Err(format!(
                "a judgement reads at most {MAX_INPUT_BYTES} bytes of text"
            ));
        }
        Some(_) => return Err("the input of a judgement is text".into()),
    };
    let question = if action == AGENT_CLASSIFY_ACTION {
        let choices = arguments
            .given("choices")
            .and_then(|choices| serde_json::from_value::<Vec<String>>(choices.clone()).ok())
            .filter(|choices| !choices.is_empty())
            .ok_or("a classification names the labels to choose from")?;
        Question::Labels {
            choices,
            multi: arguments
                .given("multi")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    } else {
        let prompt = arguments
            .text("prompt")
            .filter(|prompt| prompt.len() <= MAX_PROMPT_BYTES)
            .ok_or_else(|| {
                format!("a prompt says what to answer, in at most {MAX_PROMPT_BYTES} bytes")
            })?;
        Question::Prompt {
            prompt: prompt.to_owned(),
            schema: arguments.given("schema").cloned(),
        }
    };
    // Past a bound, a question is refused here, before any model is asked.
    question.transform()?;
    Ok(Some(Judgement {
        person: person.to_owned(),
        workspace: asked_for.workspace.to_owned(),
        question,
        input,
        note: asked_for.note,
        cause: asked_for.cause,
    }))
}

/// The count a step advances, for the step of the run that asks.
fn count(arguments: &Arguments<'_>, step: Option<AutomationStep>) -> Result<Asked, String> {
    let step = step.ok_or("a count is advanced by a step of a run")?;
    let every = match arguments.given("every") {
        None => None,
        Some(every) => Some(
            every
                .as_u64()
                .filter(|every| *every > 0)
                .ok_or("`every` is a whole number from 1 up")?,
        ),
    };
    Ok(Asked::Count {
        name: arguments.text("name").unwrap_or_default().to_owned(),
        every,
        step,
    })
}

/// `judgement` as `steps` answers it, a classification held to its labels.
fn judged(steps: &dyn HostSteps, judgement: Judgement) -> Result<Value, HostStepError> {
    let labels = match &judgement.question {
        Question::Labels { choices, multi } => Some((choices.clone(), *multi)),
        Question::Prompt { .. } => None,
    };
    let answer = steps.judge(judgement)?;
    match labels {
        Some((choices, multi)) if !names_labels(&answer, &choices, multi) => Err(
            HostStepError::Refused("the model chose none of the labels it was given".into()),
        ),
        _ => Ok(answer),
    }
}

/// Whether `answer` is one of `choices`, or with `multi` a list of them.
fn names_labels(answer: &Value, choices: &[String], multi: bool) -> bool {
    let known = |label: &Value| {
        label
            .as_str()
            .is_some_and(|label| choices.iter().any(|choice| choice == label))
    };
    match answer {
        Value::Array(labels) => multi && labels.iter().all(known),
        label => !multi && known(label),
    }
}

/// The rows the host's own steps are offered as: the agent's transforms a
/// host answers, under [`PROVIDER`], with the contract the agent app
/// publishes them with.
pub(crate) fn rows() -> Vec<CatalogueRow> {
    crate::transform::descriptors()
        .into_iter()
        .filter(|descriptor| ANSWERED.contains(&descriptor.address.action.as_str()))
        .map(|mut descriptor| {
            descriptor.locus = ExecutionLocus::ServerPreferred;
            CatalogueRow {
                offered: None,
                provider: PROVIDER.to_owned(),
                app_id: AGENT_APP_ID.to_owned(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: AGENT_TRANSFORM_RESOURCE.to_owned(),
                    kind: AGENT_TRANSFORM_KIND.to_owned(),
                    label: descriptor.resource_label.clone(),
                    presentation: None,
                },
                action: Some(descriptor),
                has_children: false,
                visibility: None,
                surface: None,
            }
        })
        .collect()
}

/// The rows what a program keeps is offered as: `count` and `keep` on the
/// program's own resource, the Shortcut it is, as installation
/// `installation` runs it here.
pub(crate) fn kept_rows(installation: &str) -> Vec<CatalogueRow> {
    let resource = tilo_automation::shortcut_resource_id(installation);
    let counted = ResultSpec::Json {
        schema: json!({
            "type": "object",
            "required": ["count", "reached"],
            "properties": {
                "count": {"type": "integer", "minimum": 1},
                "reached": {"type": "boolean"}
            }
        }),
    };
    [
        (
            COUNT_ACTION,
            "Count",
            "Advance one of the program's counts by one. Answers where it stands, and whether \
             it reached a multiple of `every`.",
            count_params(),
            counted,
        ),
        (
            KEEP_ACTION,
            "Keep",
            "Change the program's note: what it is for, how its person wants it done, an \
             example they accepted or rejected, and what this run keeps for the next.",
            keep_params(),
            ResultSpec::None,
        ),
    ]
    .into_iter()
    .map(|(action, label, description, params, result)| {
        let mut contract = ActionContract {
            kind: ActionKind::Undoable,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Immediate,
            result,
            ..ActionContract::default()
        };
        // What a program asks of itself, never a row a person picks.
        contract.set_audience(Audience::Agents);
        CatalogueRow {
            offered: None,
            provider: PROVIDER.to_owned(),
            app_id: SHORTCUTS_APP_ID.to_owned(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.clone(),
                kind: SHORTCUT_RESOURCE_KIND.to_owned(),
                label: "Shortcut".to_owned(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: ActionAddress::new(resource.clone(), action),
                resource_kind: SHORTCUT_RESOURCE_KIND.to_owned(),
                resource_label: "Shortcut".to_owned(),
                action_label: label.to_owned(),
                keywords: Vec::new(),
                icon: None,
                description: Some(description.to_owned()),
                locus: ExecutionLocus::ServerPreferred,
                kind: OperationKind::Effect,
                contract,
                input: InputSpec::None,
                params,
                view: None,
            }),
            has_children: false,
            visibility: None,
            surface: None,
        }
    })
    .collect()
}

fn count_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "name",
            ParamType::Text,
            "Which count, for a program that keeps several; left out, its one count.",
        ),
        ActionParam::optional(
            "every",
            ParamType::Integer,
            "Say whether the count reached a multiple of this number: 10 for every tenth.",
        ),
    ]
}

fn keep_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "instruction",
            ParamType::Text,
            "What the program is for, in its person's words.",
        ),
        ActionParam::optional(
            "style",
            ParamType::Text,
            "How what it writes should read: a voice, a length, a form.",
        ),
        ActionParam::optional(
            "guidance",
            ParamType::Text,
            "What its person cares about and how the work is done.",
        ),
        ActionParam::optional(
            "accepted",
            ParamType::Text,
            "An answer the person accepted, to keep as an example.",
        ),
        ActionParam::optional(
            "rejected",
            ParamType::Text,
            "An answer the person rejected, to keep as an example.",
        ),
        ActionParam::optional(
            "kept",
            ParamType::Object,
            "What this run keeps for the next: what it saw last, where it stopped. It takes \
             the place of what the last run kept.",
        ),
    ]
}
