//! What a turn says it is doing, in the person's words.
//!
//! A turn narrates each step it takes: the reply's note says what the agent is at ("Editing
//! Notes…", "Reading the plan…") until the next one, so a person reads what is happening rather
//! than the name of the tool the agent called. The steps are [`TaskStep`]'s small vocabulary,
//! said in this host's catalogue ([`crate::i18n`]); what a step is about is bounded to one short
//! line, as a note is a line.

use serde_json::Value;
use tilo_actions::task_step::TaskStep;
use tilo_history::FluentArgs;
use tilo_model_gateway::host_backend::protocol::ToolCall;

/// What the call of `call` is, in the words a person reads on its reply.
pub(super) fn said(call: &ToolCall) -> String {
    words(&step_of(call))
}

/// The step `call` is: reading, changing or finding out what an app can do, and what about.
fn step_of(call: &ToolCall) -> TaskStep {
    let field = |name: &str| {
        call.input
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let app = |path: &str| {
        let first = path.split(['/', '#', '@']).next().unwrap_or(path);
        // An app is named as people read it: `notes` is Notes beside a person.
        ui::format::capitalize_first(first)
    };
    match call.name.as_str() {
        "tilo_find" => TaskStep::Reading(field("query")),
        "tilo_read" | "tilo_view" | "tilo_watch" | "tilo_link" => TaskStep::Reading(field("path")),
        "tilo_ls" => TaskStep::Exploring(
            Some(field("path"))
                .filter(|path| !path.is_empty())
                .map_or_else(|| "the workspace".to_owned(), |path| app(&path)),
        ),
        "tilo_do" => {
            let path = field("path");
            TaskStep::Editing(if path.is_empty() {
                "the workspace".to_owned()
            } else {
                app(&path)
            })
        }
        _ => TaskStep::Editing("the workspace".to_owned()),
    }
}

/// `step` as the person reads it now: what is being done, and to what.
fn words(step: &TaskStep) -> String {
    let (key, what) = match step {
        TaskStep::Searching(what) => ("agent-step-searching", what),
        TaskStep::Reading(what) => ("agent-step-reading", what),
        TaskStep::Editing(what) => ("agent-step-editing", what),
        TaskStep::Exploring(what) => ("agent-step-exploring", what),
    };
    let what = bounded(what);
    let mut args = FluentArgs::new();
    args.set("what", what.as_str());
    crate::i18n::message(None, key, &args)
}

/// The most characters of a path or a query a note carries: enough to say what the step is
/// about, short enough to read as one line.
const MOST_WHAT_CHARS: usize = 80;

/// `text` on one line (its runs of whitespace as one space each) of at most
/// [`MOST_WHAT_CHARS`], with an ellipsis where it was cut.
fn bounded(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut bounded: String = one_line.chars().take(MOST_WHAT_CHARS).collect();
    if one_line.chars().count() > MOST_WHAT_CHARS {
        bounded.push('\u{2026}');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            input,
            extra_content: None,
        }
    }

    /// A person reads what the agent is at, not the tool it called: changing Notes says so, and a
    /// read says what it reads.
    #[test]
    fn a_step_says_what_is_being_done_and_to_what() {
        assert_eq!(
            said(&call(
                "tilo_do",
                serde_json::json!({"path": "notes/Todo#append-paragraph"})
            )),
            "Editing Notes\u{2026}"
        );
        assert_eq!(
            said(&call("tilo_read", serde_json::json!({"path": "chat/Chat"}))),
            "Reading chat/Chat\u{2026}"
        );
        assert_eq!(
            said(&call("tilo_find", serde_json::json!({"query": "launch"}))),
            "Reading launch\u{2026}"
        );
        assert_eq!(
            said(&call("tilo_ls", serde_json::json!({"path": "notes"}))),
            "Looking at what Notes can do\u{2026}"
        );
        // Nothing a step is about is longer than one line.
        let long = "a".repeat(200);
        let note = said(&call("tilo_do", serde_json::json!({"path": long})));
        assert!(note.chars().count() <= MOST_WHAT_CHARS + "Editing \u{2026}".chars().count() + 1);
        assert!(note.ends_with('\u{2026}'), "{note}");
    }
}
