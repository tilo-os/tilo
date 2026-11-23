//! What starts an agent's turn: the triggers its person keeps for it, the same triggers any
//! shortcut uses, and the one action they invoke, the agent's `answer`.
//!
//! A new agent comes with the triggers a member would expect ([`triggers`]): a message that
//! mentions it, a message in its direct conversation, a task assigned to it, and a message that
//! arrived in the workspace's mailbox. They are one shortcut, saved by the person who made the
//! agent, named after the agent, and its person changes or removes them like any shortcut's.
//! None of them fires on the agent's own work or a copy of it carried back from elsewhere: each
//! leaves out a change the agent made (`by`) and one that copies the agent's change (`origin`).
//!
//! The shortcut's one step invokes `agents.agent:<id>#answer` with the change that fired it
//! ([`ANSWER`], [`Answers`]). The agent engine takes the turn from that change alone: who asked
//! is whoever made the change (the person an agent or a service acted for, or for a message
//! carried in for someone outside the workspace, the person whose trigger fired), and the agent
//! answers where the change was made, with an ordinary write there. There is no record of
//! asking and no queue beside the turn's own claim ([`crate::work::takes_turn`]).

use serde_json::{Map, Value, json};
use tilo_actions::ActionDescriptor;
use tilo_actions::api::ActionAddress;
use tilo_actions::contract::ActionContract;
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionParam, ParamType};
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionKey, ActionOutput, ExecutionLocus, InputSpec, OperationKind};
use tilo_automation::dispatch::DocumentEvent;
use tilo_automation::shortcut::Test;
use tilo_automation::{
    ActorFilter, Binding, ChangeFilter, EVENT_INPUT, Selector, Shortcut, Step, StepKind,
    TriggerSpec,
};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};

use crate::service::ServiceSlot;

/// The provider the agents' answers are taken under in the daemon's dispatch.
pub const PROVIDER: &str = "tilo.agents";
/// The kind of resource an agent is published as.
pub const AGENT_KIND: &str = "agents.agent";
/// The action that starts an agent's turn on what triggered it.
pub const ANSWER: &str = "answer";

/// tilolang Chat, whose messages an agent is mentioned and written to in.
pub const CHAT: &str = "so.tilo.lang.chat";
/// The kind of a message of [`CHAT`].
pub const MESSAGE: &str = "tl.so.tilo.lang.chat.Message";
/// tilolang Tasks, whose tasks are assigned to an agent.
pub const TASKS: &str = "so.tilo.lang.tasks";
/// The kind of a task of [`TASKS`].
pub const TASK: &str = "tl.so.tilo.lang.tasks.Task";
/// tilolang Mail, whose received messages an agent answers.
pub const MAIL: &str = "so.tilo.mail";
/// The kind of a message of [`MAIL`].
pub const MAIL_MESSAGE: &str = "tl.so.tilo.mail.Message";

/// The resource `agent` is published as: `agents.agent:<id>`.
#[must_use]
pub fn agent_resource(agent: &str) -> String {
    format!("{AGENT_KIND}:{agent}")
}

/// The agent a resource id names, when it names one.
#[must_use]
pub fn agent_of(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(AGENT_KIND)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

/// The address of `agent`'s answer: `agents.agent:<id>#answer`.
#[must_use]
pub fn answer_address(agent: &str) -> ActionAddress {
    ActionAddress::new(agent_resource(agent), ANSWER)
}

/// The id of the shortcut that holds `agent`'s triggers.
#[must_use]
pub fn shortcut_id(agent: &str) -> String {
    format!("agent-{agent}")
}

/// tilolang Discussion, whose comments an agent answers a task with.
const DISCUSSION: &str = "so.tilo.lang.discussion";

/// What names `agent` wherever a resource holds it: inside a mention in rich text (the mark's
/// value is the member's id as JSON), and inside an actor a field holds (a task's assignee, the
/// person of a direct conversation), each kept as JSON text.
fn named(agent: &str) -> String {
    let member =
        serde_json::to_string(&tilo_action_policy::MemberId::agent(agent)).unwrap_or_default();
    // `{"agent":"<id>"}` without its braces: the same within any actor that wraps it.
    member
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(&member)
        .to_owned()
}

/// The triggers a new agent comes with, as one shortcut named after it whose one step starts
/// the agent's turn on the change that fired it: a message that mentions it, a message in its
/// direct conversation, a task assigned to it, and a message that arrived in the workspace's
/// mailbox. Each leaves out the agent's own changes and a copy of one carried back from
/// elsewhere, so its answer never starts it again.
#[must_use]
pub fn triggers(agent: &str, name: &str) -> Shortcut {
    let member = format!("agent:{agent}");
    let watch = |kind: &str, narrowing: Vec<(&str, Test)>| TriggerSpec::ResourceChanged {
        kind: kind.to_owned(),
        actions: Vec::new(),
        fields: Vec::new(),
        by: ActorFilter::Anyone,
        filter: ChangeFilter::of(
            [
                ("by", Test::IsNot(json!(member.clone()))),
                ("origin", Test::IsNot(json!(member.clone()))),
            ]
            .into_iter()
            .chain(narrowing),
        ),
    };
    let mut step_args = std::collections::BTreeMap::new();
    step_args.insert(
        EVENT_INPUT.to_owned(),
        Binding::Input(EVENT_INPUT.to_owned()),
    );
    Shortcut {
        id: shortcut_id(agent),
        name: name.to_owned(),
        inputs: vec![ActionParam::required(
            EVENT_INPUT,
            ParamType::Object,
            "The change that started the agent's turn",
        )],
        steps: vec![Step::new(
            ANSWER,
            StepKind::Invoke {
                action: ActionKey::new(tilo_workspace_agent::OWNER, AGENT_KIND, ANSWER),
                resource: Selector::Fixed(agent_resource(agent)),
                args: step_args,
                appearance: None,
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![
            // A message whose text mentions it, in any conversation.
            watch(MESSAGE, vec![("item.body", Test::Has(json!(named(agent))))]),
            // A message in its direct conversation: the conversation it is in is with it.
            watch(
                MESSAGE,
                vec![("item.$in.person", Test::Has(json!(named(agent))))],
            ),
            // A task given to it.
            watch(
                TASK,
                vec![("item.assignee", Test::Has(json!(named(agent))))],
            ),
            // A message that arrived in the workspace's mailbox: written by mail's outside
            // origin, not by anyone here, and not the account's own copy of what it sent, which
            // an answer of the agent's comes back as once the server keeps it.
            watch(
                MAIL_MESSAGE,
                vec![
                    ("incoming_message", Test::Is(json!(true))),
                    ("item.mailbox", Test::IsNot(json!("sent"))),
                ],
            ),
        ],
        saved_by: None,
        saved_proof: None,
    }
}

/// Where an agent answers what triggered it: the app the change was made in, the resource it
/// was made in there (a message's conversation), and the item it made or changed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Where {
    pub app: String,
    pub record: String,
    pub item: String,
}

impl Where {
    /// Where `event` asks to be answered: a message of Chat, a task of Tasks or a message of
    /// Mail. `None` for a change of anything else, which no agent's trigger answers.
    #[must_use]
    pub fn of(event: &DocumentEvent) -> Option<Self> {
        let app = match event.kind.as_str() {
            MESSAGE => CHAT,
            TASK => TASKS,
            MAIL_MESSAGE => MAIL,
            _ => return None,
        };
        let item = event.item.as_ref()?.id.clone();
        // The resource the item is nested in, as the change read it.
        let record = event
            .after
            .as_ref()
            .and_then(|after| after.pointer("/$in/$id"))
            .and_then(Value::as_str)
            .map_or_else(|| event.record.clone(), str::to_owned);
        Some(Self {
            app: app.to_owned(),
            record,
            item,
        })
    }

    /// The inputs the turn that answers here is named by: every device that saw the change
    /// names the same turn.
    #[must_use]
    pub fn inputs(&self) -> Value {
        json!({"record": self.record, "item": self.item})
    }
}

/// The turn of `agent` that answers at `place`, as it is placed: named by the agent's answer and
/// the item that asked, so every device that saw the change names the same one, and about where
/// it was asked.
#[must_use]
pub fn turn_of(agent: &str, place: &Where) -> crate::work::Turn {
    answer_at(agent, place)
        .acting_as(crate::work::Actor::Agent(agent.to_owned()))
        .about(place.app.clone(), place.record.clone())
}

/// The answer of `agent` at `place` as a turn that runs once: what names it, and what keeps it
/// done.
#[must_use]
pub fn answer_at(agent: &str, place: &Where) -> crate::work::Turn {
    crate::work::Turn::once(
        format!("{}#{ANSWER}", agent_resource(agent)),
        place.inputs(),
    )
}

/// [`turn_of`], started on this device now: the device whose trigger fired takes it at once,
/// and another only once this one let it go ([`crate::work::takes_turn`]).
#[must_use]
pub fn started_here(
    workspaces: &crate::workspaces::Workspaces,
    agent: &str,
    place: &Where,
) -> crate::work::Turn {
    turn_of(agent, place).started(
        workspaces.serving_certificate().device_public_key,
        tilo_clock::unix_now_ms_i64(),
    )
}

/// The published id of `id` of the tilolang app `app`.
fn published(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// Write `words`, a turn's answer, where `place` asked for it, as `caller`: a reply to the
/// message in its conversation, an answer to the conversation in Mail, or a comment on the task.
///
/// # Errors
/// The app refused the write.
pub(super) async fn write_answer(
    provider: &super::WorkspaceActionProvider,
    caller: &tilo_actions::remote::Caller,
    workspace: &str,
    place: &Where,
    words: &str,
) -> Result<(), String> {
    let route = |app: &str| tilo_peer_state::document_frame::DocumentRoute {
        app_id: app.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let mut arguments = Map::new();
    let (app, resource, action) = if place.app == CHAT {
        arguments.insert("body".into(), json!(words));
        arguments.insert("reply_to".into(), json!(published(CHAT, &place.item)));
        (CHAT, published(CHAT, &place.record), "send")
    } else if place.app == MAIL {
        // Mail answers a conversation, to whoever wrote its newest message, as a reply.
        arguments.insert("body".into(), json!(words));
        (MAIL, published(MAIL, &place.record), "answer")
    } else {
        arguments.insert("text".into(), json!(words));
        arguments.insert(
            tilo_actions::params::FROM_ARGUMENT.into(),
            tilo_actions::params::MadeFrom {
                resource: published(TASKS, &place.item),
                kind: String::new(),
            }
            .to_json(),
        );
        (DISCUSSION, format!("tl:{DISCUSSION}"), "comment")
    };
    super::invoke_named(provider, caller, &route(app), &resource, action, arguments)
        .await
        .map(drop)
}

/// Who asked, by `event`: the person who made the change, or whom the agent, service or
/// automation that made it acted for. `None` for a change made for nobody who is a member: a
/// message a service carried in for someone outside the workspace.
#[must_use]
pub fn asker(event: &DocumentEvent) -> Option<String> {
    event
        .actor
        .person_id()
        .or_else(|| event.actor.acted_for_person())
        .map(str::to_owned)
}

/// What the change says, as the turn is told it: the text of the item it made.
#[must_use]
pub fn said(event: &DocumentEvent) -> String {
    let mut words = Vec::new();
    if let Some(after) = &event.after {
        for field in ["title", "body", "notes", "text"] {
            if let Some(value) = after.get(field) {
                collect_text(value, &mut words);
            }
        }
    }
    words.join("\n").trim().to_owned()
}

/// Every text `value` holds, in order: a rich text's runs and a plain text alike.
fn collect_text(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.trim().is_empty() => into.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_text(item, into)),
        Value::Object(fields) => {
            for (key, field) in fields {
                if matches!(
                    key.as_str(),
                    "text" | "body" | "spans" | "blocks" | "children"
                ) {
                    collect_text(field, into);
                }
            }
        }
        _ => {}
    }
}

pub use crate::work::Started;

/// Why an agent's turn did not start, as its answer fails with it.
fn not_started(message: String) -> tilo_actions::ActionFailure {
    tilo_actions::ActionFailure {
        code: "agent-not-started".to_owned(),
        message,
        details: None,
        retryable: false,
    }
}

/// The agents' answers in the daemon's dispatch: `agents.agent:<id>#answer`, taken by the agent
/// engine this device runs ([`crate::work::AgentEngine::answer`]).
pub struct Answers {
    service: ServiceSlot,
}

impl Answers {
    /// The answers of the agents of the workspaces `service` holds once it opens.
    #[must_use]
    pub const fn new(service: ServiceSlot) -> Self {
        Self { service }
    }
}

/// The answer of `agent`, named `name`, as a step of a program reaches it.
fn descriptor(agent: &str, name: &str) -> ActionDescriptor {
    let contract = ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        ..ActionContract::default()
    };
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: answer_address(agent),
        resource_kind: AGENT_KIND.to_owned(),
        resource_label: name.to_owned(),
        action_label: "Answer".to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: Some(
            "Start the agent's turn on a change, answered where the change was made.".to_owned(),
        ),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract,
        input: InputSpec::None,
        params: vec![ActionParam::required(
            EVENT_INPUT,
            ParamType::Object,
            "The change that started the turn, as a trigger hands it on",
        )],
        view: None,
    }
}

impl HostStepServices for Answers {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn app_id(&self) -> &str {
        tilo_workspace_agent::OWNER
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        let Some(engine) = self
            .service
            .service()
            .and_then(|service| service.workspaces().agent_keys())
        else {
            return Vec::new();
        };
        engine
            .answering()
            .iter()
            .map(|(agent, name)| descriptor(agent, name))
            .collect()
    }

    fn invocation(
        &self,
        address: &ActionAddress,
        _argument: Option<&str>,
    ) -> Option<ActionDescriptor> {
        let agent = agent_of(&address.resource)?;
        (address.action == ANSWER).then(|| descriptor(agent, agent))
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let agent = agent_of(&request.address.resource)
            .filter(|_| request.address.action == ANSWER)
            .ok_or_else(|| format!("no agent answers `{}`", request.address.resource))?
            .to_owned();
        let engine = self
            .service
            .service()
            .ok_or("this device has not opened its workspaces yet")?
            .workspaces()
            .agent_keys()
            .ok_or("this device runs no agents of its workspaces")?;
        let arguments = request
            .argument
            .and_then(|text| text.strip_prefix(ARGUMENTS_JSON_PREFIX))
            .and_then(|json| serde_json::from_str::<Map<String, Value>>(json).ok())
            .ok_or("an answer takes the change that triggered it")?;
        let event: DocumentEvent = arguments
            .get(EVENT_INPUT)
            .cloned()
            .ok_or("an answer takes the change that triggered it")
            .and_then(|event| {
                serde_json::from_value(event).map_err(|_| "the change does not read as one")
            })?;
        let saver = request
            .actor
            .direct_person()
            .or_else(|| request.actor.acting_for())
            .ok_or("an answer is started for a person")?
            .to_owned();
        let started = engine.answer(request.workspace.to_owned(), agent, event, saver);
        let (done, outcome) = pending();
        started.then(move |started| {
            done.complete(match started {
                Some(Ok(started)) => Ok(ActionOutput {
                    value: Some(match started {
                        Started::Taken => json!({"taken": true}),
                        Started::Elsewhere(why) => json!({"taken": false, "why": why}),
                    }),
                    resources: Vec::new(),
                }),
                Some(Err(why)) => Err(Box::new(not_started(why))),
                None => Err(Box::new(not_started(
                    "this device's agents stopped".to_owned(),
                ))),
            });
        });
        Ok(outcome)
    }
}
