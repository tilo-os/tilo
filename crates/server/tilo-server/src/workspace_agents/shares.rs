//! A turn answers with the thing it worked on, shown live: a card of that document in the
//! reply to the ask ([`asks::show`]).
//!
//! The card stands in the reply's body as a paragraph of its own, which shows the document in
//! its own app where the reply is drawn and opens it: the person who asked sees the document
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
//!
//! [`asks::show`]: tilo_workspace_service::asks::show

use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_workspace_service::asks::Reference;

use super::{Ask, Reply, ToolDefinition, Turn, WorkspaceActionProvider};

/// The tool a turn answers through with the thing it worked on.
pub(super) const ANSWER_WITH_DOCUMENT: &str = "answer_with_document";

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
    matches!(turn.ask, Ask::Asked { .. })
}

/// What a turn's instructions say of the tool: when the thing itself is the answer. Nothing for
/// a turn whose answer is no reply to an ask.
pub(super) fn told(turn: &Turn) -> &'static str {
    if answers_with_a_reply(turn) {
        " When the person asks to see the thing you made or changed, or to work in it with \
         you, show it live through `answer_with_document`, then say in a sentence what it is."
    } else {
        ""
    }
}

/// The tool as the model is told it. None for a turn whose answer is no reply to an ask, as an
/// issue's comment is.
pub(super) fn tool(turn: &Turn) -> Option<ToolDefinition> {
    answers_with_a_reply(turn).then(|| ToolDefinition {
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
    })
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
    /// Show the thing the model worked on live in the turn's reply, as a card of its own: what
    /// the model reads back, and whether it failed. Nothing the model says names where it is
    /// shown or whose card it is.
    pub(super) async fn document(
        &self,
        tools: &McpServer<WorkspaceActionProvider>,
        input: &Value,
    ) -> (String, bool) {
        let Some(replying) = &self.replying else {
            return ("only an answer to an ask shows a document".to_owned(), true);
        };
        let (path, title) = match asked(input) {
            Ok(asked) => asked,
            Err(why) => return (why, true),
        };
        let (app, document) = match read_as_the_turn(tools, &path).await {
            Ok(read) => read,
            Err(why) => return (format!("The document was not shown: {why}"), true),
        };
        match replying.show(&title, card((&app, &document))).await {
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
    use super::{Reference, card, names_app};

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
