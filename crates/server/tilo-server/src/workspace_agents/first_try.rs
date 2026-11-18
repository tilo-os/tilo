//! What makes a turn's first try work.
//!
//! Every ask of a turn's model costs its person many seconds, and a model that must look
//! before it works, or that is refused for a name it guessed, asks more often. So a turn is
//! told at its start what its workspace has for its caller and what a server does not have,
//! and a view's list of apps is read as the view itself writes them.
//!
//! What a turn is told of its workspace is what its own tools would answer its caller
//! ([`McpServer::told_at_start`]): nothing is named to it that it may not use. It changes when the
//! workspace's apps do, so it stands after the instruction, after the person's own words.
//! The instruction itself is what a provider keeps of a prompt's beginning and what a
//! conversation's key is made of: it must be the same byte for byte from turn to turn, and only
//! what is the same for every turn is added to it: what a server does not have, how a secret
//! reaches an action, and that a turn states nothing it did not read.

use std::fmt::Write as _;

use serde_json::Value;

use super::{Content, McpServer, Message, WorkspaceActionProvider};

/// What no turn on a server has, whatever its tools' own words offer: said before a model
/// spends an ask to be refused.
fn not_here() -> String {
    format!(
        "Not on this server: no program is carried out here, so the tool `tilo` takes only \
         `undo task` and `history task`, and the work is done with tilo_do and tilo_read. \
         Nothing is read by a path that begins with `{}:`; read through the app's own path \
         (`notes/Todo`). tilo_view reads a window a person has open; where none shows a path, \
         read it with tilo_read and give the person tilo_link.",
        tilo_actions::remote::RESOURCE_PROVIDER
    )
}

/// How a secret reaches an action, said to every turn: never through the conversation.
const A_SECRET: &str = "A password, a key or a code is never asked for in words or typed into \
    the chat: call the action that needs it; it asks your person in a protected field you \
    cannot read. Where no action asks for it by itself, put the question with Chat's `ask` of \
    the kind `secret`.";
/// What a turn never does, said to every turn: state what no tool's answer told it.
const NOT_READ: &str = "What no tool's answer told you, you do not know: never state it as a \
    fact about your person's things, such as that something is missing, empty or not set up. \
    Where a call was refused or failed, tell your person in plain words what you tried and \
    what was refused.";

/// What stands before what a turn is told of its workspace, after its person's words, in a
/// paragraph of its own.
const FROM_THE_SERVER: &str =
    "From the workspace's server, for this turn (your person did not write what follows):\n";

/// Tell the turn that begins with `messages`, its instruction and its person's message, what
/// a server has not and what its caller has through `server`.
pub(super) async fn tell(messages: &mut [Message], server: &McpServer<WorkspaceActionProvider>) {
    // No clock ends this and no count of bytes cuts it: it is the look the turn's model would
    // ask for first, and a turn told nothing, or half, would ask for that look all the same.
    let has = server.told_at_start().await;
    let mut each = messages.iter_mut();
    // What no server has is the same for every turn: it stands at the end of the instruction.
    if let Some(Content::Text { text }) = each.next().and_then(|first| first.content.first_mut()) {
        text.push_str("\n\n");
        text.push_str(&not_here());
        text.push_str("\n\n");
        text.push_str(A_SECRET);
        text.push_str("\n\n");
        text.push_str(NOT_READ);
    }
    // What the workspace has changes as its apps do: it stands after the instruction, in the
    // person's message, and the instruction stays the same byte for byte. It follows the
    // person's own words as a paragraph of its own: what a turn was asked with begins with
    // what its person wrote, and whoever reads its first paragraph reads theirs.
    if !has.is_empty()
        && let Some(asked) = each.next()
    {
        let told = format!("{FROM_THE_SERVER}{}", has.trim_end());
        match asked.content.last_mut() {
            Some(Content::Text { text }) => {
                text.push_str("\n\n");
                text.push_str(&told);
            }
            _ => asked.content.push(Content::Text { text: told }),
        }
    }
}

/// A name as names are compared: in lower case, without the hyphens, underscores and blanks
/// that part its words.
fn folded(name: &str) -> String {
    name.chars()
        .filter(|character| !matches!(character, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// `reads` as the view `source` writes its apps: an app the model listed in another spelling
/// than its view uses (`word-processor`, or the app's whole id, where the view writes
/// `word_processor`) is listed as the view writes it. A name the view does not write is left
/// as it is, for Chat to refuse.
pub(super) fn reads_as_written(source: &Value, reads: &Value) -> Value {
    let (Some(written), Some(listed)) = (source.as_str(), reads.as_array()) else {
        return reads.clone();
    };
    let named = chat::message_view::named(written);
    Value::Array(
        listed
            .iter()
            .map(|read| {
                let Some(app) = read.as_str() else {
                    return read.clone();
                };
                if named.iter().any(|name| name == app) {
                    return read.clone();
                }
                let wanted = folded(app.rsplit('.').next().unwrap_or(app));
                named
                    .iter()
                    .find(|name| folded(name) == wanted)
                    .map_or_else(|| read.clone(), |name| Value::String(name.clone()))
            })
            .collect(),
    )
}

/// What the model reads of a view that was not posted for `why`: where the view's list of apps
/// was refused, the names the view itself writes before a dot, so its next call is the one
/// that works.
pub(super) fn not_posted(why: &str, source: &Value) -> String {
    let mut said = format!("The view was not posted: {why}");
    if why.contains("does not name")
        && let Some(written) = source.as_str()
    {
        let named = chat::message_view::named(written);
        if named.is_empty() {
            said.push_str(". This view writes no name before a dot, so `reads` is []");
        } else {
            let _ = write!(
                said,
                ". Before a dot this view writes: {}. `reads` lists the apps among those, each \
                 spelled as there, and nothing else",
                named.join(", ")
            );
        }
    }
    said
}

#[cfg(test)]
mod tests {
    use super::{not_posted, reads_as_written};
    use serde_json::json;

    const POEM: &str = "view poem() {\n    text(word_processor.document.title)\n}";

    #[test]
    fn a_views_apps_are_listed_as_the_view_writes_them() {
        for listed in [
            "word_processor",
            "word-processor",
            "Word Processor",
            "so.tilo.word-processor",
        ] {
            assert_eq!(
                reads_as_written(&json!(POEM), &json!([listed])),
                json!(["word_processor"]),
                "{listed}"
            );
        }
        // A name the view does not write stays, for Chat to refuse; so does what is no list.
        assert_eq!(
            reads_as_written(&json!(POEM), &json!(["tasks"])),
            json!(["tasks"])
        );
        assert_eq!(
            reads_as_written(&json!(POEM), &json!("tasks")),
            json!("tasks")
        );
    }

    #[test]
    fn a_refused_list_of_apps_is_answered_with_the_names_the_view_writes() {
        let said = not_posted(
            "the view does not name tasks: list the apps it reads",
            &json!(POEM),
        );
        assert!(
            said.starts_with("The view was not posted: the view does not name tasks")
                && said.contains("this view writes: word_processor, document."),
            "{said}"
        );
        let other = not_posted("the view's definition is too long", &json!(POEM));
        assert_eq!(
            other,
            "The view was not posted: the view's definition is too long"
        );
    }
}
