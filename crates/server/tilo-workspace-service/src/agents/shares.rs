//! A turn answers with the thing it worked on, shown live: Chat's own `share-window` of that
//! document, in the conversation of the turn.
//!
//! A message that shares a window stands in its conversation as the document's own app, live,
//! drawn for each reader from their own copy with what they hold: the person who asked sees the
//! document their agent wrote and writes in it. The tool the model has for it,
//! [`ANSWER_WITH_DOCUMENT`], takes the thing by the path the model worked on it with and what
//! its card is called, and nothing else:
//!
//! - **Where it is shown is the turn's, never the model's.** It is the conversation the turn
//!   answers in ([`shows_in`]); no argument names another, and none names whose share it is.
//! - **Only where nobody else reads.** A turn shows a document only where it knows and keeps
//!   what it learns ([`learned::keeps`]): in its keeper's own conversation with the agent,
//!   which the agent takes part in and no person but the keeper. Anywhere else the name and
//!   the address of a document would reach other members by the agent's choice; that takes
//!   its person's own word, and is refused here.
//! - **Only what its person may read, and the one the turn worked on.** The path is read as
//!   the turn itself, by the turn's own reading tool in the turn's own session of its tools:
//!   the document that read resolved is the one the model's own calls of that path worked in,
//!   and a path the turn may not read names none.
//! - **A turn shares nowhere else.** Chat's own sharing verbs take the conversation as an
//!   argument, which through the turn's other tools is the model's to give. A turn's call of
//!   one is taken only for the conversation the turn answers in ([`refuses`]).
//! - **It gives nobody anything.** What a share grants is asked for by its sharer's own Chat;
//!   no Chat is the agent's, so no grant is written, and whoever opens the document opens it
//!   with what they hold.

use serde_json::{Map, Value, json};
use tilo_mcp::McpServer;

use super::{
    Ask, CHAT, Caller, DocumentRoute, InvokeArgument, InvokeRequest, Reply, ToolDefinition, Turn,
    WorkspaceActionProvider, invoke_named, learned,
};

/// The tool a turn in a conversation answers through with the thing it worked on.
pub(super) const ANSWER_WITH_DOCUMENT: &str = "answer_with_document";

/// When the tool is the answer and what it takes, as the model is told it.
const GUIDE: &str = "Answer with the thing itself that you worked on, shown live in the \
conversation in its own app: the document, the sheet, the board. The person sees it change \
while you or they work on it, and can work in it right there. Use it when they ask to see what \
you made or changed, or to work on it with you. `path` is the path you worked on it with, \
without an action: `word-processor`. `title` is what its card is called. Where it is shown is \
never an argument: it is the conversation you are answering in, and only your keeper's own \
conversation with you. For a count, a list or a status over what apps keep, write a view with \
`answer_with_view` instead. Afterwards say in a sentence what it is.";

/// What the model is told in a conversation that is not its keeper's own with it.
const NOT_THEIR_OWN: &str = "The document was not shown: you show a document only in your \
keeper's own conversation with you, and this is not it, or others read it too. Say where the \
document is (`tilo_link` gives a link only the workspace's members can open); the person \
shares it here themselves if they want to.";

/// Chat's verbs that share something into a conversation, and what they call the conversation.
const SHARING: [&str; 2] = ["share-window", "share-resource"];
const IN: &str = "channel";

/// The longest a card's name the model gives is kept, in characters.
const MOST_TITLE_CHARS: usize = 120;

/// The kind a shared window's resource is posted under where its own kind is not known: the
/// one Chat posts a window under whose app names none.
const WINDOW_KIND: &str = "tilo.window";

/// What a turn's instructions say of the tool: when the thing itself is the answer. Nothing for
/// a turn whose answer is no reply in a conversation.
pub(super) fn told(turn: &Turn) -> &'static str {
    if matches!(turn.ask, Ask::Message { .. }) {
        " When the person asks to see the thing you made or changed, or to work in it with \
         you, show it live through `answer_with_document`, then say in a sentence what it is."
    } else {
        ""
    }
}

/// The tool as the model is told it. None for an issue's turn, whose answer is a comment.
pub(super) fn tool(turn: &Turn) -> Option<ToolDefinition> {
    matches!(turn.ask, Ask::Message { .. }).then(|| ToolDefinition {
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

/// The conversation `turn` may show a document in, by its id: the one it answers in, where
/// that is its agent's keeper's own with the agent ([`learned::keeps`]). None for every other
/// turn: one somebody else asked, one in a conversation another person takes part in or the
/// whole workspace reads, a sub-agent's, an issue's.
pub(super) fn shows_in(turn: &Turn) -> Option<String> {
    learned::keeps(turn)?;
    conversation_id(&turn.conversation)
}

/// The id of the conversation whose address in the workspace is `address`
/// ([`chat::summons::conversation_address`]); none for what is no such address.
fn conversation_id(address: &str) -> Option<String> {
    let prefix = chat::summons::conversation_address(&tilo_resource::Id::from_raw(String::new()));
    address
        .strip_prefix(&prefix)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Why a turn's call of one of Chat's sharing verbs is not taken, when it is not: a turn shares
/// only into the conversation it answers in. Where a share stands is where a document's name
/// and address reach, and the conversation is an argument the model gives, so it is held here
/// to the turn's own, as a sub-agent's words are (`sub_agents::admits`). A call that is no
/// turn's, as a person's own is, is never refused here, and neither is any other action.
pub(super) fn refuses(caller: &Caller, request: &InvokeRequest) -> Option<String> {
    let turn = caller.turn()?;
    let chat = format!("{}:{CHAT}:", tilo_agent_session::RESOURCE_PROVIDER);
    let target = &request.target;
    if !target.provider.starts_with(&chat) || !SHARING.contains(&target.address.action.as_str()) {
        return None;
    }
    let named = match &request.argument {
        InvokeArgument::Json(given) => given.get(IN).and_then(Value::as_str),
        _ => None,
    };
    let own = conversation_id(&turn.conversation);
    (named.is_none() || named != own.as_deref()).then(|| {
        "an agent shares only into the conversation it is answering in: say where the thing \
         is, or show it there with `answer_with_document`"
            .to_owned()
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
             where it is shown and whose share it is are never arguments"
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

/// What Chat's `share-window` takes to show the document `document` of `app` in `conversation`
/// under `title`. An app's own document of the workspace has no id of its own.
fn sharing(conversation: &str, (app, document): (&str, &str), title: &str) -> Map<String, Value> {
    let mut attached = Map::new();
    attached.insert("app".into(), json!(app));
    if document.is_empty() {
        attached.insert("id".into(), json!(app));
    } else {
        attached.insert("record".into(), json!(document));
        attached.insert("id".into(), json!(document));
    }
    attached.insert("kind".into(), json!(WINDOW_KIND));
    attached.insert("label".into(), json!(title));
    let mut arguments = Map::new();
    arguments.insert(IN.into(), json!(conversation));
    arguments.insert("attached".into(), Value::Object(attached));
    // What its card offers a reader: to work in it. It grants nothing: each reader changes
    // what they may change anyway.
    arguments.insert("access".into(), json!("edit"));
    arguments
}

impl Reply {
    /// Show the thing the model worked on live in the turn's conversation, through Chat's own
    /// `share-window`, as the agent acting for the person inside this turn: what the model reads
    /// back, and whether it failed. Nothing the model says names the conversation or the sharer.
    pub(super) async fn document(
        &self,
        tools: &McpServer<WorkspaceActionProvider>,
        input: &Value,
    ) -> (String, bool) {
        if !matches!(self.ask, Ask::Message { .. }) {
            return (
                "only an answer in a conversation shows a document".to_owned(),
                true,
            );
        }
        let (path, title) = match asked(input) {
            Ok(asked) => asked,
            Err(why) => return (why, true),
        };
        let Some(conversation) = self.shows_in.as_deref() else {
            return (NOT_THEIR_OWN.to_owned(), true);
        };
        let (app, document) = match read_as_the_turn(tools, &path).await {
            Ok(read) => read,
            Err(why) => return (format!("The document was not shown: {why}"), true),
        };
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        };
        match invoke_named(
            &self.provider,
            &self.caller,
            &route,
            &collab_types::document::chain_resource_id(&chat::parts::chain()),
            "share-window",
            sharing(conversation, (&app, &document), &title),
        )
        .await
        {
            Ok(_) => (
                "It is shown live in the conversation, in its own app: the person sees it \
                 change and can work in it there. Now say in a sentence what it is."
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
    use super::{conversation_id, names_app};

    /// A turn's conversation is named by its address in the workspace; what Chat is asked to
    /// share in is the conversation's own id, and what is no conversation's address names none.
    #[test]
    fn a_document_is_shown_in_the_conversation_the_turns_address_names() {
        let id = tilo_resource::Id::from_raw("direct:[1,2]".to_owned());
        let address = chat::summons::conversation_address(&id);
        assert_eq!(conversation_id(&address).as_deref(), Some("direct:[1,2]"));
        let nowhere =
            chat::summons::conversation_address(&tilo_resource::Id::from_raw(String::new()));
        for address in [
            "",
            "direct:[1,2]",
            "tasks.tracker/issues/7",
            nowhere.as_str(),
        ] {
            assert_eq!(conversation_id(address), None, "{address:?}");
        }
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
