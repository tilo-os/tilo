//! The shortcut a new agent comes with, which starts its turn on the changes it answers: the
//! same triggers any shortcut uses, saved by the person who made the agent and named after it.
//! The workspace's record of its agents saves and removes it where an agent is made and
//! retired; what answers the shortcut's step is the agents' own.

use serde_json::{Value, json};
use tilo_actions::ActionKey;
use tilo_actions::api::ActionAddress;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_automation::shortcut::Test;
use tilo_automation::{
    ActorFilter, Binding, ChangeFilter, EVENT_INPUT, Selector, Shortcut, Step, StepKind,
    TriggerSpec,
};

/// The provider the agents' answers are taken under in the daemon's dispatch.
pub const PROVIDER: &str = "tilo.agents";
/// The kind of resource an agent is published as.
pub const AGENT_KIND: &str = "agents.agent";
/// The action that starts an agent's turn on what triggered it.
pub const ANSWER: &str = "answer";
/// The argument that names the change that started the turn ([`crate::work::Where`]).
pub const CAUSE: &str = "cause";

/// tilolang Chat, whose messages an agent is mentioned and written to in.
pub const CHAT: &str = "so.tilo.chat";
/// The kind of a message of [`CHAT`].
pub const MESSAGE: &str = "tl.so.tilo.chat.Message";
/// tilolang Tasks, whose tasks are assigned to an agent.
pub const TASKS: &str = "so.tilo.tasks";
/// The kind of a task of [`TASKS`].
pub const TASK: &str = "tl.so.tilo.tasks.Task";
/// tilolang Mail, whose received messages an agent answers.
pub const MAIL: &str = "so.tilo.mail";
/// The kind of a message of [`MAIL`].
pub const MAIL_MESSAGE: &str = "tl.so.tilo.mail.Message";
/// The workspace's moderation record, whose reports an agent watching chat also sees.
pub const MODERATION: &str = "tilo.moderation";
/// The verbs that file a report on it.
pub const REPORTS: [&str; 2] = ["report-message", "report-person"];

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

/// The id of the shortcut that makes `agent` watch a workspace's chat.
#[must_use]
pub fn moderation_shortcut_id(agent: &str) -> String {
    format!("moderation-{agent}")
}

/// tilolang Discussion, whose comments an agent answers a task with.
pub(crate) const DISCUSSION: &str = "so.tilo.lang.discussion";

/// The agent `agent` of `person` as a direct conversation's `people` holds it: a value, not the
/// JSON text a field keeps.
fn member_of(agent: &str, person: &str) -> Value {
    json!({"member": tilo_action_policy::MemberId::agent(agent, person)})
}

/// What names the agent `agent` of `person` wherever a resource holds it: inside a mention in
/// rich text (the mark's value is the member's id as JSON), and inside an actor a field holds (a
/// task's assignee, the person of a direct conversation), each kept as JSON text.
fn named(agent: &str, person: &str) -> String {
    names(&tilo_action_policy::MemberId::agent(agent, person))
}

/// What names `member` inside an actor a field holds, kept as JSON text: its id without the
/// braces of whatever wraps it.
#[must_use]
pub fn names(member: &tilo_action_policy::MemberId) -> String {
    let member = serde_json::to_string(member).unwrap_or_default();
    // `{"agent":{"id":"<id>","person":"<person>"}}` without its braces: the same within any
    // actor that wraps it.
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
pub fn triggers(agent: &str, person: &str, name: &str) -> Shortcut {
    let who = (agent, person);
    answering(
        who,
        shortcut_id(agent),
        name.to_owned(),
        vec![
            // A message whose text mentions it, in any conversation.
            watch(
                who,
                MESSAGE,
                "item.body",
                Test::Has(json!(named(agent, person))),
            ),
            // A message in its direct conversation: it is one of the conversation's people.
            watch(
                who,
                MESSAGE,
                "item.$in.people",
                Test::Has(member_of(agent, person)),
            ),
            // A reply in a thread of its direct conversation: the thread is nested in the
            // conversation, whose people it is among.
            watch(
                who,
                MESSAGE,
                "item.$in.$in.people",
                Test::Has(member_of(agent, person)),
            ),
            // A task given to it.
            watch(
                who,
                TASK,
                "item.assignee",
                Test::Has(json!(named(agent, person))),
            ),
            // A message that arrived in the workspace's mailbox: written by Mail's outside
            // origin, not by anyone here, and not the account's own copy of what it sent, which
            // an answer of the agent's comes back as once the server keeps it.
            watching(
                who,
                MAIL_MESSAGE,
                Vec::new(),
                vec![
                    ("incoming_message", Test::Is(json!(true))),
                    ("item.mailbox", Test::IsNot(json!("sent"))),
                ],
            ),
        ],
    )
}

/// What a Moderator saves to put `agent` to watching a workspace's chat: one shortcut named
/// after it, whose one step starts the agent's turn on a message a member posted, answered
/// where the message was posted, and on a report filed against a message or a person, answered
/// in the moderation record. It fires on every message and report like the agent's own triggers
/// do, leaving out the agent's own changes and a copy of one carried back from elsewhere, so
/// the agent's removal or reply never starts it again. Saved as an ordinary shortcut of the
/// person who keeps the agent ([`triggers`] beside it), the same runner fires it and the same
/// answer takes its turn; removing that shortcut turns the watch off again.
#[must_use]
pub fn moderates(agent: &str, person: &str, name: &str) -> Shortcut {
    let who = (agent, person);
    answering(
        who,
        moderation_shortcut_id(agent),
        format!("{name} moderates"),
        vec![
            watch_all(who, MESSAGE),
            watch_actions(who, MODERATION, REPORTS.to_vec()),
        ],
    )
}

/// The one step of the agent `agent` of `person` (`who`) that starts its turn on the change a
/// trigger hands it, as the shortcut `id`, named `name`, whose triggers are `triggers`.
fn answering(
    (agent, _): (&str, &str),
    id: String,
    name: String,
    triggers: Vec<TriggerSpec>,
) -> Shortcut {
    let mut step_args = std::collections::BTreeMap::new();
    step_args.insert(
        EVENT_INPUT.to_owned(),
        Binding::Input(EVENT_INPUT.to_owned()),
    );
    // The cause as it starts the turn: the app, the kind and id of the item the change made,
    // the resource it is nested in, and the change itself.
    step_args.insert(
        CAUSE.to_owned(),
        Binding::Object(std::collections::BTreeMap::from([
            (
                "app".to_owned(),
                Binding::Input(format!("{EVENT_INPUT}/app")),
            ),
            (
                "kind".to_owned(),
                Binding::Input(format!("{EVENT_INPUT}/kind")),
            ),
            (
                "resource".to_owned(),
                Binding::Input(format!("{EVENT_INPUT}/record")),
            ),
            (
                "in".to_owned(),
                Binding::Input(format!("{EVENT_INPUT}/after/$in/$id")),
            ),
            (
                "change".to_owned(),
                Binding::Input(format!("{EVENT_INPUT}/change")),
            ),
        ])),
    );
    Shortcut {
        id,
        name,
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
        grant: answering_grant(agent, &triggers),
        triggers,
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// What the shortcut that starts `agent`'s turn may do: start that agent's answer, and read
/// the changes its `triggers` watch. A shortcut does nothing its grant leaves out, so one with
/// none would fire and start no turn.
fn answering_grant(agent: &str, triggers: &[TriggerSpec]) -> Vec<tilo_action_policy::Rule> {
    let mut grant = vec![tilo_automation::grant::action_rule(
        tilo_workspace_agent::OWNER,
        AGENT_KIND,
        Some(&agent_resource(agent)),
        ANSWER,
        tilo_action_policy::KindSet::UNDOABLE,
        tilo_action_policy::Answer::Allow,
    )];
    grant.extend(tilo_automation::grant::trigger_rules(triggers));
    grant
}

/// A change of `kind` that fires for the agent `who` (its id and its person), narrowed by
/// `narrowing`, and never by the agent's own change or a copy of one carried back from elsewhere.
fn watch(who: (&str, &str), kind: &str, field: &str, test: Test) -> TriggerSpec {
    watching(who, kind, Vec::new(), vec![(field, test)])
}

/// A change of `kind` that fires for the agent `who` on any change of it, and never by the
/// agent's own change or a copy of one carried back from elsewhere.
fn watch_all(who: (&str, &str), kind: &str) -> TriggerSpec {
    watching(who, kind, Vec::new(), Vec::new())
}

/// A change of `kind` made by one of the `actions` that fires for the agent `who`, and never by
/// the agent's own change or a copy of one carried back from elsewhere.
fn watch_actions(who: (&str, &str), kind: &str, actions: Vec<&str>) -> TriggerSpec {
    watching(
        who,
        kind,
        actions.into_iter().map(str::to_owned).collect(),
        Vec::new(),
    )
}

/// A change of `kind` that fires for the agent `who` (its id and its person), made by one of
/// `actions` where they are named, narrowed by `narrowing`, and never by the agent's own change
/// or a copy of one carried back from elsewhere.
fn watching(
    (agent, person): (&str, &str),
    kind: &str,
    actions: Vec<String>,
    narrowing: Vec<(&str, Test)>,
) -> TriggerSpec {
    let member =
        String::from_utf8_lossy(&tilo_action_policy::MemberId::agent(agent, person).actor_bytes())
            .into_owned();
    TriggerSpec::ResourceChanged {
        kind: kind.to_owned(),
        actions,
        fields: Vec::new(),
        by: ActorFilter::Anyone,
        filter: ChangeFilter::of(
            [
                ("by", Test::IsNot(json!(member.clone()))),
                ("origin", Test::IsNot(json!(member))),
            ]
            .into_iter()
            .chain(narrowing),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{ActionKind, Answer};
    use tilo_automation::{StepKind, grant_answer, step_node};

    /// The shortcut an agent comes with may start that agent's answer, the one step it has, and
    /// nothing of another agent's: one that fires but whose step its grant leaves out starts
    /// no turn.
    #[test]
    fn an_agents_shortcut_may_start_its_answer_and_no_other_agents() {
        for shortcut in [
            triggers("scout", "person", "Scout"),
            moderates("scout", "person", "Scout"),
        ] {
            let [step] = shortcut.steps.as_slice() else {
                panic!("one step: {:?}", shortcut.steps);
            };
            let StepKind::Invoke { action, .. } = &step.kind else {
                panic!("the step invokes the answer: {:?}", step.kind);
            };
            let at = |agent: &str| {
                step_node(
                    Some("studio"),
                    &action.app,
                    &action.kind,
                    &answer_address(agent),
                )
            };
            assert_eq!(
                grant_answer(&shortcut.grant, &at("scout"), ActionKind::Undoable).answer,
                Answer::Allow,
                "{}: its own answer",
                shortcut.id
            );
            assert_eq!(
                grant_answer(&shortcut.grant, &at("other"), ActionKind::Undoable).answer,
                Answer::Deny,
                "{}: another agent's answer",
                shortcut.id
            );
            assert!(
                tilo_automation::grant::unused_rules(&shortcut).is_empty(),
                "{}: no rule its step and triggers do not use",
                shortcut.id
            );
        }
    }
}
