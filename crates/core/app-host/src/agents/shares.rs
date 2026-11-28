//! A turn answers with the thing it worked on, shown live: a message referring to that document
//! beside its reply; or with a view it wrote over what the workspace keeps, sent into the
//! conversation through Chat's `send_view`, which each reader may add to their own views.
//!
//! The card stands in the reply after its body, which shows the document in its own app where
//! the reply is drawn and opens it: the person who asked sees the document
//! their agent wrote and works in it. The turn's action for it ([`super::turn_tools`]) takes
//! the thing by the path the model worked on it with and what its card is called, and nothing
//! else:
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

use serde_json::json;
use tilo_mcp::McpServer;
use tilo_reference::Embed as Reference;

use super::{Reply, Turn, WorkspaceActionProvider};

/// The longest a view's source the model gives is taken, in bytes.
const MOST_SOURCE_BYTES: usize = 32 * 1024;

/// The longest a card's name the model gives is kept, in characters.
const MOST_TITLE_CHARS: usize = 120;

/// The kind a card of an app's document is shown under: a window of that app on it.
const WINDOW_KIND: &str = "tilo.window";

/// Whether `turn` answers with a reply that can carry a card: one made from an ask.
pub(super) fn answers_with_a_reply(turn: &Turn) -> bool {
    turn.ask.message().is_some()
}

/// The view the model wrote: what it is called, in one line, and its source.
fn view_asked(title: &str, source: &str) -> Result<(String, String), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("`source` is the view, as text".to_owned());
    }
    if source.len() > MOST_SOURCE_BYTES {
        return Err(format!(
            "a view is at most {MOST_SOURCE_BYTES} bytes; this one is {}: write a smaller one",
            source.len()
        ));
    }
    Ok((one_line(Some(title)).unwrap_or_default(), source.to_owned()))
}

/// What the model asked to show: the path it worked on the thing with, without an action, and
/// what its card is called, in one line: the first it gave, or the path.
fn asked(path: &str, title: Option<&str>) -> Result<(String, String), String> {
    let path = path.trim();
    if path.is_empty() || path.contains('#') {
        return Err("`path` names the thing you worked on, without an action".to_owned());
    }
    let title = one_line(title).unwrap_or_else(|| path.chars().take(MOST_TITLE_CHARS).collect());
    Ok((path.to_owned(), title))
}

/// The first line of `text` that says something, within [`MOST_TITLE_CHARS`].
fn one_line(text: Option<&str>) -> Option<String> {
    let line = text?.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(MOST_TITLE_CHARS).collect())
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
    pub(super) async fn view(&self, title: &str, source: &str) -> (String, bool) {
        let (title, source) = match view_asked(title, source) {
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
        path: &str,
        title: Option<&str>,
    ) -> (String, bool) {
        let (path, title) = match asked(path, title) {
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
        .await
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
    use super::{MOST_SOURCE_BYTES, Reference, asked, card, names_app, view_asked};

    /// A view is taken by its source and a title as its first line; a document by its path
    /// without an action, called by the path where no title is given.
    #[test]
    fn a_view_and_a_document_are_taken_by_what_the_model_gave() {
        let source = "view open() { text(\"3\") }";
        assert_eq!(
            view_asked("\n Open \nissues", source),
            Ok(("Open".to_owned(), source.to_owned()))
        );
        assert!(view_asked("Open", " ").is_err());
        assert!(view_asked("", &"x".repeat(MOST_SOURCE_BYTES + 1)).is_err());
        assert_eq!(
            asked(" word-processor ", None),
            Ok(("word-processor".to_owned(), "word-processor".to_owned()))
        );
        assert!(asked("word-processor#open", Some("Poem")).is_err());
        assert!(asked(" ", None).is_err());
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
