//! What a turn answers with a view through, and what its arguments then say.
//!
//! A conversation's answer may carry a view ([`super::ANSWER_WITH_VIEW`]): its source and the
//! apps it reads. Chat keeps such a view as it is given, so a view naming an app it does not
//! read is kept and shown as an error that says nothing of why, and the model that wrote it is
//! told nothing either. The helpers here are what a turn has of that answer: the tool the model
//! is told of ([`tool`]), what a refused call gave read back to it ([`described`]), and the
//! apps a view never calls named before the call is made ([`unread_apps`]).

use serde_json::{Map, Value, json};
use tilo_agent_turn::ToolDefinition;

use super::{ANSWER_WITH_VIEW, Ask, Turn, worked};

/// Why a view written as `source` and reading `reads` is refused here, when it is: `reads` names
/// an app the view never calls before a dot. The view is given the apps it lists and no other
/// ([`chat::message_view::GUIDE`]), and Chat keeps such a view: shown, it stands an error in place
/// of what it shows, and nothing says why. Saying so here costs the step the call stands in,
/// where the same said after the post costs the call, the answer and the step after it.
///
/// One the view calls that the workspace does not have is Chat's to refuse, and that refusal is
/// answered with what the call gave ([`described`]): said here, its words would send the model
/// to an app it guessed at as readily as to one the call cut short.
///
/// No caller in this tree yet: the refusal it answers for is made where a view is written, and
/// until that call site returns it stands here kept for it.
#[allow(dead_code)]
pub(super) fn unread_apps(source: &str, reads: &Value) -> Option<String> {
    let named = chat::message_view::named(source);
    let unused: Vec<&str> = reads
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .filter(|app| !named.iter().any(|name| name == app))
        .collect();
    if unused.is_empty() {
        return None;
    }
    let listed = unused.join("`, `");
    Some(format!(
        "{ANSWER_WITH_VIEW} was not sent: a view is given the apps `reads` lists and no other, \
         and the view calls no app by `{listed}`. Call every app the view reads before a dot, as \
         `tasks.tracker.areas` reads Tasks, and leave out of `reads` each app the view does not \
         call."
    ))
}

/// What a call of `answer_with_view` gave, each argument by its name and what it held, as the
/// model reads it back: `source` as the few characters it began with, `reads` by its entries,
/// and anything else by its value. Nothing but what the model itself wrote is said, and it is
/// cut short, so the sentence stays one step's answer.
pub(super) fn described(given: &Map<String, Value>) -> String {
    let mut said: Vec<String> = Vec::new();
    for (name, value) in given {
        let held = match (name.as_str(), value) {
            (_, Value::Null) => "nothing".to_owned(),
            ("source", Value::String(source)) => {
                let mut began: String = source.trim().chars().take(120).collect();
                if source.trim().chars().count() > 120 {
                    began.push_str("...");
                }
                format!("the text {began:?}")
            }
            (_, Value::Array(reads)) => {
                let apps: Vec<String> = reads
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|app| format!("`{app}`"))
                    .collect();
                if apps.is_empty() {
                    format!("{} entries, none of them an app", reads.len())
                } else {
                    format!("the apps {}", apps.join(", "))
                }
            }
            (_, Value::String(text)) => {
                format!("the text {}", text.chars().take(60).collect::<String>())
            }
            (_, other) => format!("the value {other}"),
        };
        said.push(format!("`{name}`: {held}"));
    }
    if said.is_empty() {
        return "nothing".to_owned();
    }
    said.join("\n")
}

/// The tool a turn in a conversation answers with a view through, as the model is told it: when
/// a view is the right answer and how one is written ([`chat::message_view::GUIDE`]). None for
/// an issue's turn, whose answer is a comment.
pub(super) fn tool(turn: &Turn) -> Option<ToolDefinition> {
    matches!(turn.ask, Ask::Message { .. }).then(|| ToolDefinition {
        name: ANSWER_WITH_VIEW.to_owned(),
        // Chat's own guide of a view, and what a conversation's earlier answers left of one:
        // the view asked for again is shown from its working source rather than written anew.
        description: format!("{} {}", chat::message_view::GUIDE, worked::GUIDE),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {"type": "string"},
                "reads": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["source", "reads"],
            "additionalProperties": false,
        }),
    })
}
