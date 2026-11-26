//! What a turn is offered beside the agent tools and the web's: published actions, each told to
//! its model through [`tilo_mcp::tool::of_action`] from the descriptor its calls are bound by, so
//! there is no second model of a tool beside the actions.
//!
//! - **How it answers.** A turn whose answer is a reply answers with a view, or with the
//!   document it worked on shown live, through actions on its own invocation record
//!   ([`answering`]); where it goes and whose it is are the turn's, never an argument
//!   ([`super::shares`]).
//! - **What it keeps about its person.** A turn that knows and keeps facts ([`super::learned`])
//!   is offered the actions of what the workspace's agents learned
//!   ([`tilo_person_services::person_learned`]), carried out for the person who asked it.

use serde_json::Value;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::workspace_invocations::{INVOCATION_TYPE, PROVIDER as INVOCATIONS};
use tilo_actions::{ActionArgs, ActionDescriptor, ActionKind, ActionParam, ParamType};

use tilo_model_gateway::host_backend::protocol::ToolDefinition;

use super::{Turn, shares};
use tilo_person_services::person_learned;

/// The invocation a turn of `agent` is: of the agent's answer action, in the workspace's
/// invocations, whose record holds the turn's state, steps and output.
fn answering(agent: &str) -> String {
    format!("agents.agent:{agent}#answer")
}

/// Answer with a view on the reply.
const ANSWER_WITH_VIEW: &str = "answer-with-view";
/// Answer with the document the turn worked on, shown live in its conversation.
const ANSWER_WITH_DOCUMENT: &str = "answer-with-document";

/// The names the model calls the actions by, each with the provider and the action it is. A
/// tool's name is identity, not description: it never moves with the action's label.
const TOOLS: [(&str, &str, &str); 6] = [
    ("answer_with_view", INVOCATIONS, ANSWER_WITH_VIEW),
    ("answer_with_document", INVOCATIONS, ANSWER_WITH_DOCUMENT),
    ("learn", person_learned::PROVIDER, person_learned::LEARN),
    ("correct", person_learned::PROVIDER, person_learned::CORRECT),
    ("forget", person_learned::PROVIDER, person_learned::FORGET),
    ("learned", person_learned::PROVIDER, person_learned::LIST),
];

/// When a view is the answer and how one is written.
const VIEW: &str = "A small piece of interface you write in Tilo's app language over what the \
workspace's apps keep. It goes with your reply, and everyone who reads it can add it to their own \
views, where it is drawn as that reader, with what they may read, and again whenever what it \
reads changes. Use it when what was asked for keeps changing or is easier seen than read: a \
count, a list, a status, things someone wants to keep an eye on. Answer in words alone when the \
answer is one fact that will not move. `title` is what the view is called. `source` is one \
`view` declaration and the functions it calls, and nothing else: no types, no actions, no `use`. \
It names an app by the last part of its id (`tasks` for `so.tilo.tasks`) and reads what that app \
keeps for the workspace: `view projects() { for a in tasks.tracker.areas { text(a.name) } }`. A \
control invokes one of the app's own actions, named as the app names it with `_` for `-`: \
`button(\"Delete\", on: tasks.delete_area(tasks.tracker, a.id))`; in a view a press only \
suggests, and the reader allows that one thing or not. Numbers that are compared are a chart: \
`ui.BarChart([.bars(values: [3.0, 5.0], label: \"Closed\")], categories: [\"Mon\", \"Tue\"])` \
for amounts side by side, `ui.PieChart([.slice(label: \"Open\", value: 3.0)])` for the parts of \
one whole, `ui.Chart([.series(values: [1.0, 2.0], label: \"Opened\")])` for a history. A view \
reads a field by its type: an option with `??` or `if let v = x {}`; a list or a collection with \
`for i in xs {}`, `.len()` or `.map()`; a plain case by `== .name`. After it, say in a sentence \
or two what the view shows: those words are all that search, notifications and a reader that \
cannot show a view have.";

/// When the thing itself is the answer, and how it is named.
const DOCUMENT: &str = "The thing itself that you worked on, shown live in your reply in its own \
app: the document, the sheet, the board. The person sees it change while you or they work on it, \
and opens it from there to work in it. Use it when they ask to see what you made or changed, or \
to work on it with you. `path` is the path you worked on it with, without an action: \
`word-processor`. `title` is what its card is called. Where it is shown is never an argument: it \
is your reply. For a count, a list or a status over what apps keep, answer with a view instead. \
Afterwards say in a sentence what it is.";

/// A call of one of the actions, bound by what it publishes.
pub(super) enum Call {
    /// What the view the model wrote is called, and its source.
    View { title: String, source: String },
    /// The path the model worked on the thing with, and what its card is called.
    Document { path: String, title: Option<String> },
    /// One of what the workspace's agents learned, by its action: its arguments are bound where
    /// it is carried out.
    Learned(&'static str),
}

/// The actions of a turn of `agent`, by the provider each is of.
fn descriptors(agent: &str) -> Vec<(&'static str, ActionDescriptor)> {
    let turn = answering(agent);
    let on_turn = |action, label, description, params| {
        let address = ActionAddress::new(&turn, action);
        let kind = (INVOCATION_TYPE, "This turn");
        person_learned::published(
            address,
            kind,
            label,
            description,
            ActionKind::Undoable,
            params,
        )
    };
    let invocations = [
        on_turn(
            ANSWER_WITH_VIEW,
            "Answer with a view",
            VIEW,
            vec![
                ActionParam::required("title", ParamType::Text, "What the view is called"),
                ActionParam::required("source", ParamType::Text, "The view, as text"),
            ],
        ),
        on_turn(
            ANSWER_WITH_DOCUMENT,
            "Answer with the document",
            DOCUMENT,
            vec![
                ActionParam::required("path", ParamType::Text, "The path you worked on it with"),
                ActionParam::optional("title", ParamType::Text, "What its card is called"),
            ],
        ),
    ];
    invocations
        .into_iter()
        .map(|descriptor| (INVOCATIONS, descriptor))
        .chain(
            person_learned::descriptors()
                .into_iter()
                .map(|descriptor| (person_learned::PROVIDER, descriptor)),
        )
        .collect()
}

/// The actions `turn` is offered, as its model is told them: how it answers where its answer is
/// a reply to an ask, and what it keeps about its person where it `keeps` facts.
pub(super) fn definitions(turn: &Turn, keeps: bool) -> Vec<ToolDefinition> {
    let replies = shares::answers_with_a_reply(turn);
    descriptors(&turn.agent.id.0)
        .iter()
        .filter(|(provider, _)| {
            if *provider == INVOCATIONS {
                replies
            } else {
                keeps
            }
        })
        .filter_map(|(provider, descriptor)| {
            let (name, _, _) = TOOLS
                .iter()
                .find(|(_, of, action)| of == provider && *action == descriptor.address.action)?;
            Some(tilo_mcp::tool::of_action(
                name,
                descriptor,
                tilo_resource::schema::args_schema(&descriptor.params),
            ))
        })
        .collect()
}

/// The call of the tool `name` with `input`, bound by what its action publishes, or why it does
/// not bind. `None` where `name` is none of these actions.
pub(super) fn call(name: &str, input: &Value) -> Option<Result<Call, String>> {
    let (_, provider, action) = TOOLS.iter().find(|(tool, _, _)| *tool == name)?;
    if *provider == person_learned::PROVIDER {
        return Some(Ok(Call::Learned(action)));
    }
    // What a call binds is the same whichever agent's turn it is.
    let (_, descriptor) = descriptors("")
        .into_iter()
        .find(|(of, descriptor)| of == provider && descriptor.address.action == *action)?;
    let bound = input
        .as_object()
        .ok_or_else(|| "the arguments are an object".to_owned())
        .and_then(|given| {
            ActionArgs::bind_json(&descriptor.params, given).map_err(|error| error.to_string())
        });
    Some(bound.map(|bound| {
        let text = |name: &str| bound.text(name).map(str::to_owned);
        if *action == ANSWER_WITH_VIEW {
            Call::View {
                title: text("title").unwrap_or_default(),
                source: text("source").unwrap_or_default(),
            }
        } else {
            Call::Document {
                path: text("path").unwrap_or_default(),
                title: text("title"),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use serde_json::json;

    use super::*;

    /// Every action a turn is offered is told under its name with what it publishes, and a call of
    /// it is bound by the same: what it does not take is refused before anything is done.
    #[test]
    fn a_turns_tools_are_the_actions_it_publishes() {
        assert!(call("tilo_ls", &json!({})).is_none());
        let view = json!({"title": "Open", "source": "view v() {}"});
        assert!(matches!(
            call("answer_with_view", &view),
            Some(Ok(Call::View { title, source })) if title == "Open" && source == "view v() {}"
        ));
        assert!(matches!(
            call("answer_with_document", &json!({"path": "word-processor"})),
            Some(Ok(Call::Document { path, title: None })) if path == "word-processor"
        ));
        for (name, refused) in [
            ("answer_with_view", json!({"source": "view v() {}"})),
            (
                "answer_with_document",
                json!({"path": "notes", "channel": "c"}),
            ),
        ] {
            assert!(matches!(call(name, &refused), Some(Err(_))), "{refused}");
        }
    }
}
