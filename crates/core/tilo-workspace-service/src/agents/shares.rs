//! A turn answers with the thing it worked on, shown live: a message referring to that document
//! beside its reply; or with a view it wrote over what the workspace keeps, sent into the
//! conversation through Chat's `send_view`, which each reader may add to their own views.
//!
//! The card stands in the reply after its body, which shows the document in its own app where
//! the reply is drawn and opens it: the person who asked sees the document
//! their agent wrote and works in it. The tool the model has for it, [`ANSWER_WITH_DOCUMENT`],
//! takes the thing by the path the model worked on it with and what its card is called, and
//! nothing else:
//!
//! - **Where it is shown is the turn's, never the model's.** It is the reply the turn writes;
//!   no argument names another place, and none names whose card it is.
//! - **Only what its person may read, and the one the turn worked on.** The path is read as
//!   the turn itself, by the turn's own reading tool in the turn's own session of its tools:
//!   the document that read resolved is the one the model's own calls of that path worked in,
//!   and a path the turn may not read names none.
//! - **It gives nobody anything.** A card is a reference, and each reader of the reply reads it
//!   with what they hold: one the workspace lets read the document sees it, anyone else reads
//!   only that something is there, never what it is called.

use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_model_gateway::host_backend::protocol::ToolDefinition;
use tilo_reference::Embed as Reference;

use super::{Reply, Turn, WorkspaceActionProvider};

/// The tool a turn answers through with the thing it worked on.
pub(super) const ANSWER_WITH_DOCUMENT: &str = "answer_with_document";

/// The tool a turn answers through with a view it wrote.
pub(super) const ANSWER_WITH_VIEW: &str = "answer_with_view";

/// When a view is the answer and how one is written, as the model is told it.
pub(super) const VIEW_GUIDE: &str = "Answer with a view: a small piece of interface you write in Tilo's \
app language over what the workspace's apps keep. It goes with your reply, and everyone who \
reads it can add it to their own views, where it is drawn as that reader, with what they may \
read, and again whenever what it reads changes. Use it when what was asked for keeps changing \
or is easier seen than read: a count, a list, a status, things someone wants to keep an eye on. \
Answer in words alone when the answer is one fact that will not move. `title` is what the view \
is called. `source` is one `view` declaration and the functions it calls, and nothing else: no \
types, no actions, no `use`. It names an app by the last part of its id (`tasks` for \
`so.tilo.tasks`) and reads what that app keeps for the workspace: `view projects() { for a in \
tasks.tracker.areas { text(a.name) } }`. A control invokes one of the app's own actions, named \
as the app names it with `_` for `-`: `button(\"Delete\", on: tasks.delete_area(tasks.tracker, \
a.id))`; in a view a press only suggests, and the reader allows that one thing or not. Numbers \
that are compared are a chart: `ui.BarChart([.bars(values: [3.0, 5.0], label: \"Closed\")], \
categories: [\"Mon\", \"Tue\"])` for amounts side by side, `ui.PieChart([.slice(label: \
\"Open\", value: 3.0)])` for the parts of one whole, `ui.Chart([.series(values: [1.0, 2.0], \
label: \"Opened\")])` for a history. A view reads a field by its type: an option with `??` or \
`if let v = x {}`; a list or a collection with `for i in xs {}`, `.len()` or `.map()`; a plain \
case by `== .name`. After it, say in a sentence or two what the view shows: those words are all \
that search, notifications and a reader that cannot show a view have.";

/// The longest a view's source the model gives is taken, in bytes.
const MOST_SOURCE_BYTES: usize = 32 * 1024;

/// When the tool is the answer and what it takes, as the model is told it.
const GUIDE: &str = "Answer with the thing itself that you worked on, shown live in your reply \
in its own app: the document, the sheet, the board. The person sees it change while you or they \
work on it, and opens it from there to work in it. Use it when they ask to see what you made or \
changed, or to work on it with you. `path` is the path you worked on it with, without an \
action: `word-processor`. `title` is what its card is called. Where it is shown is never an \
argument: it is your reply. For a count, a list or a status over what apps keep, write a view \
with `answer_with_view` instead. Afterwards say in a sentence what it is.";

/// The longest a card's name the model gives is kept, in characters.
const MOST_TITLE_CHARS: usize = 120;

/// The kind a card of an app's document is shown under: a window of that app on it.
const WINDOW_KIND: &str = "tilo.window";

/// Whether `turn` answers with a reply that can carry a card: one made from an ask.
fn answers_with_a_reply(turn: &Turn) -> bool {
    turn.ask.message().is_some()
}

/// What a turn's instructions say of the tools: when a view or the thing itself is the answer.
/// Nothing for a turn whose answer is no reply to an ask.
pub(super) fn told(turn: &Turn) -> &'static str {
    if answers_with_a_reply(turn) {
        " When what is asked for keeps changing or is easier seen than read (a count, a list, \
         a status), answer with a view through `answer_with_view`, then say in a sentence \
         what it shows. When the person asks to see the thing you made or changed, or to work \
         in it with you, show it live through `answer_with_document`, then say in a sentence \
         what it is."
    } else {
        ""
    }
}

/// The tools as the model is told them. None for a turn whose answer is no reply to an ask, as
/// an issue's comment is.
pub(super) fn tools(turn: &Turn) -> Vec<ToolDefinition> {
    if !answers_with_a_reply(turn) {
        return Vec::new();
    }
    vec![
        ToolDefinition {
            name: ANSWER_WITH_VIEW.to_owned(),
            description: VIEW_GUIDE.to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "source": {"type": "string"},
                },
                "required": ["title", "source"],
                "additionalProperties": false,
            }),
        },
        ToolDefinition {
            name: ANSWER_WITH_DOCUMENT.to_owned(),
            description: GUIDE.to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "title": {"type": "string"},
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
    ]
}

/// The view the model wrote: what it is called, and its source. Anything else it gave is
/// refused with why.
fn view_asked(input: &Value) -> Result<(String, String), String> {
    let given = input.as_object().cloned().unwrap_or_default();
    if let Some(other) = given
        .keys()
        .find(|key| !matches!(key.as_str(), "title" | "source"))
    {
        return Err(format!(
            "{ANSWER_WITH_VIEW} takes `title` and `source` and nothing else, not `{other}`: \
             whose view it is and which reply it goes with are never arguments"
        ));
    }
    let Some(source) = given
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
    else {
        return Err(format!(
            "{ANSWER_WITH_VIEW} takes `source`, the view as text"
        ));
    };
    if source.len() > MOST_SOURCE_BYTES {
        return Err(format!(
            "{ANSWER_WITH_VIEW} takes a view of at most {MOST_SOURCE_BYTES} bytes; this one \
             is {}: write a smaller one",
            source.len()
        ));
    }
    let title = given
        .get("title")
        .and_then(Value::as_str)
        .and_then(|title| title.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or_default()
        .chars()
        .take(MOST_TITLE_CHARS)
        .collect();
    Ok((title, source.to_owned()))
}

/// What the model asked to show: the path it worked on the thing with, and what its card is
/// called. Anything else it gave is refused with why.
fn asked(input: &Value) -> Result<(String, String), String> {
    let given = input.as_object().cloned().unwrap_or_default();
    if let Some(other) = given
        .keys()
        .find(|key| !matches!(key.as_str(), "path" | "title"))
    {
        return Err(format!(
            "{ANSWER_WITH_DOCUMENT} takes `path` and `title` and nothing else, not `{other}`: \
             where it is shown and whose card it is are never arguments"
        ));
    }
    let Some(path) = given
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return Err(format!(
            "{ANSWER_WITH_DOCUMENT} takes `path`, the path you worked on the thing with"
        ));
    };
    if path.contains('#') {
        return Err(format!(
            "{ANSWER_WITH_DOCUMENT} takes the path of a thing, not of an action: leave out \
             what follows `#`"
        ));
    }
    // A card is called by one line: the first the model gave, or the path.
    let title = given
        .get("title")
        .and_then(Value::as_str)
        .and_then(|title| title.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or(path)
        .chars()
        .take(MOST_TITLE_CHARS)
        .collect();
    Ok((path.to_owned(), title))
}

/// The card of the document `document` of `app`: a window of that app on it. An app's own
/// document of the workspace has no id of its own, and is named by its app.
fn card((app, document): (&str, &str)) -> Reference {
    let mut reference = Reference::resource(
        app,
        WINDOW_KIND,
        if document.is_empty() { app } else { document },
    );
    if let Reference::Resource(at) = &mut reference
        && !document.is_empty()
    {
        at.record = Some(document.to_owned());
    }
    reference
}

impl Reply {
    /// Answer with the view the model wrote, on the turn's reply: what the model reads back,
    /// and whether it failed. Nothing the model says names the reply or whose view it is.
    pub(super) async fn view(&self, input: &Value) -> (String, bool) {
        let (title, source) = match view_asked(input) {
            Ok(asked) => asked,
            Err(why) => return (why, true),
        };
        match self.send_view(&title, &source).await {
            Ok(()) => (
                "The view goes with your reply, for everyone who reads it to add to their own \
                 views. Now say in a sentence or two what it shows."
                    .to_owned(),
                false,
            ),
            Err(why) => (format!("The view was not posted: {why}"), true),
        }
    }

    /// Show the thing the model worked on live in the turn's reply, as a card of its own: what
    /// the model reads back, and whether it failed. Nothing the model says names where it is
    /// shown or whose card it is.
    pub(super) async fn document(
        &self,
        tools: &McpServer<WorkspaceActionProvider>,
        input: &Value,
    ) -> (String, bool) {
        let (path, title) = match asked(input) {
            Ok(asked) => asked,
            Err(why) => return (why, true),
        };
        let (app, document) = match read_as_the_turn(tools, &path).await {
            Ok(read) => read,
            Err(why) => return (format!("The document was not shown: {why}"), true),
        };
        match self.show(&title, &card((&app, &document))).await {
            Ok(()) => (
                "It is shown live in your reply, in its own app: the person sees it change \
                 and opens it there to work in it. Now say in a sentence what it is."
                    .to_owned(),
                false,
            ),
            Err(why) => (format!("The document was not shown: {why}"), true),
        }
    }
}

/// The app and the document of it that `path` names, read as the turn itself: by the turn's
/// own reading tool in the turn's own session `tools`, which resolves the path as it did for
/// the model's own calls. A path the turn may not read names none. Nor does one whose read
/// resolved no document of the app it names: the session then still holds the document of an
/// earlier call, which is not shown under another thing's name.
async fn read_as_the_turn(
    tools: &McpServer<WorkspaceActionProvider>,
    path: &str,
) -> Result<(String, String), String> {
    let reading = json!({"path": path, "max_chars": 200});
    let read = tilo_agent_turn::Tools::call(tools, "tilo_read", reading).await;
    if read.is_error {
        return Err(read.text);
    }
    tools
        .acted()
        .map(|acted| (acted.app, acted.record))
        .filter(|(app, _)| names_app(path, app))
        .ok_or_else(|| format!("`{path}` names no document of an app the workspace keeps"))
}

/// Whether `path` begins with the app whose id is `app`, as a model writes one: the last part
/// of its id, in any case, with whatever it puts between the words (`word-processor`,
/// `word_processor`, `Word Processor`).
fn names_app(path: &str, app: &str) -> bool {
    let letters = |text: &str| -> String {
        text.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    let named = path.split(['/', '@']).next().unwrap_or_default();
    let last = app.rsplit('.').next().unwrap_or(app);
    !named.is_empty() && (letters(named) == letters(last) || letters(named) == letters(app))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MOST_SOURCE_BYTES, Reference, card, names_app, view_asked};

    /// A view is taken by its title and source alone, a title as its first line.
    #[test]
    fn a_view_is_taken_by_its_title_and_source_alone() {
        let source = "view open() { text(\"3\") }";
        assert_eq!(
            view_asked(&json!({"title": "\n Open \nissues", "source": source})),
            Ok(("Open".to_owned(), source.to_owned()))
        );
        assert!(view_asked(&json!({"title": "Open", "source": " "})).is_err());
        assert!(view_asked(&json!({"source": source, "reply": "elsewhere"})).is_err());
        let long = "x".repeat(MOST_SOURCE_BYTES + 1);
        assert!(view_asked(&json!({"source": long})).is_err());
    }

    /// A card names the document of the app it shows, and an app's own document of the
    /// workspace by the app.
    #[test]
    fn a_card_names_the_document_it_shows() {
        let Reference::Resource(at) = card(("so.tilo.word-processor", "poem")) else {
            panic!("a card names a resource");
        };
        assert_eq!(at.app, "so.tilo.word-processor");
        assert_eq!(at.record.as_deref(), Some("poem"));
        assert_eq!(at.reference.id, "poem");
        let Reference::Resource(at) = card(("so.tilo.word-processor", "")) else {
            panic!("a card names a resource");
        };
        assert_eq!(at.record, None);
        assert_eq!(at.reference.id, "so.tilo.word-processor");
    }

    /// A path names the app of the document it is shown as, however a model writes the app.
    #[test]
    fn a_document_is_shown_only_under_a_path_that_names_its_app() {
        let app = "so.tilo.word-processor";
        for path in [
            "word-processor",
            "word_processor",
            "Word Processor",
            "word-processor/Poem",
            "word-processor@2/Poem",
            "so.tilo.word-processor",
        ] {
            assert!(names_app(path, app), "{path}");
        }
        for path in [
            "notes",
            "tilo.events/events",
            "",
            "/word-processor",
            "processor",
        ] {
            assert!(!names_app(path, app), "{path:?}");
        }
    }
}
