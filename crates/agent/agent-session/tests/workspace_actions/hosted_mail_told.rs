//! A person tells their assistant to sign in to their mail, and the assistant's first tool
//! call is the one that does it.
//!
//! The model here knows of its workspace only what its turn was told at its start: the
//! paragraph the server puts after its person's words, which names every app with every
//! action it offers, by name. It reads no tool's own words, and it looks nothing up once it
//! was shown the action: where that paragraph names the action that signs a mail account in
//! on the line of the app that keeps a person's accounts, it writes the call at once, as the
//! paragraph says a call is written, with the address its person wrote and nothing else.
//! Where it was not shown the action it looks, as a model does that is told nothing: for the
//! apps, then into the app. Every such look is one more ask of a real model, and many seconds
//! for the person who waits.
//!
//! A stand-in shows that the way is there and how long it is, not that a real model takes it.
use super::*;
use tilo_server::workspaces::hosted_use::{MailFound, MailServer, MailServers};
use tilo_server::workspaces::person_model::{
    Events, ModelProvider, ProviderCall, ProviderFuture,
    protocol::{Content, Message, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage},
};

/// Her mail address, and the two servers her provider keeps it on: made up for this file.
const ADDRESS: &str = "ada@example.test";
const READS: &str = "imap.provider.test";
const SENDS: &str = "smtp.provider.test";
/// What she writes to her assistant.
const SENTENCE: &str = "Please log in to my mail account ada@example.test.";
/// The app that keeps a person's accounts, as a path names it.
const APP: &str = "accounts";
/// The action that signs a mail account in from its address alone, as a path ends with it.
const ACTION: &str = "#connect";
/// Where what a turn is told of its workspace begins, after its person's words.
const FROM_THE_SERVER: &str = "From the workspace's server, for this turn";

/// Where an address's mail lives, as the stand-in for the network works it out: her address
/// lives at the two servers her provider publishes, and nothing is found for any other.
fn finds(address: &str, allowed: &dyn Fn(&str) -> bool) -> Result<MailServers, String> {
    if address == ADDRESS && allowed(READS) && allowed(SENDS) {
        Ok(MailServers {
            incoming: MailServer::new(READS, 993),
            outgoing: MailServer::new(SENDS, 465),
            found: MailFound::Published,
            username: None,
            own_sign_in: None,
        })
    } else {
        Err("No mail server was found for that address".to_owned())
    }
}

/// A model that knows of its workspace only what its turn was told at its start. Every
/// request it was sent is kept.
#[derive(Default)]
struct ToldAtItsStart {
    asked: Mutex<Vec<ModelRequest>>,
}

impl ModelProvider for ToldAtItsStart {
    fn answer<'a>(&'a self, call: ProviderCall<'a>, _: Events<'a>) -> ProviderFuture<'a> {
        self.asked.lock().unwrap().push(call.request.clone());
        let answered = goes_about_it(call.request);
        Box::pin(async move { Ok(answered) })
    }
}

/// What the turn of `request` was asked with: its person's words, and after them what the
/// server told it.
fn asked_with(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .find(|message| matches!(message.role, Role::User))
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// The tool calls `request` carries, in the order they were made: each tool's name and what
/// it was given.
fn calls_in(request: &ModelRequest) -> Vec<(String, serde_json::Value)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolCall(call) => Some((call.name.clone(), call.input.clone())),
            _ => None,
        })
        .collect()
}

/// What the tools answered in `request`, in order: each answer, and whether it refuses or
/// fails.
fn answers_in(request: &ModelRequest) -> Vec<(String, bool)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}

/// Whether `shown` names [`ACTION`] as one [`APP`] itself offers: on the app's own line of
/// what a turn is told at its start (`accounts: #list, #connect`), or on a line of a listing
/// that begins with it (`#connect(address)  Sign in to mail account`). The line of a kind of
/// thing in the app does not show it: a call on such a thing takes the thing's name, which
/// that line does not tell.
fn shows_it(shown: &str) -> bool {
    let names_it = |word: &str| {
        word.strip_prefix(ACTION).is_some_and(|rest| {
            !rest.starts_with(|next: char| next.is_ascii_alphanumeric() || next == '-')
        })
    };
    shown.lines().any(|line| {
        let the_apps_own = line
            .strip_prefix(APP)
            .is_some_and(|rest| rest.starts_with([':', ' ']));
        (the_apps_own || line.trim_start().starts_with(ACTION))
            && line.split([' ', ',']).any(names_it)
    })
}

/// One answer of the model: `content`, ending as `stop_reason` says.
fn answers(content: Content, stop_reason: StopReason) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![content],
        },
        stop_reason,
        usage: Usage::default(),
        cost_micros: 0,
    }
}

/// The model ends its turn with `words`.
fn says(words: &str) -> ModelResponse {
    answers(
        Content::Text {
            text: words.to_owned(),
        },
        StopReason::EndTurn,
    )
}

/// The model calls the tool `name` with `given`, after `made` calls of this turn.
fn calls_tool(name: &str, made: usize, given: serde_json::Value) -> ModelResponse {
    answers(
        Content::ToolCall(ToolCall {
            id: format!("call-{made}"),
            name: name.to_owned(),
            input: given,
            extra_content: None,
        }),
        StopReason::ToolCalls,
    )
}

/// What the model answers `request` with.
fn goes_about_it(request: &ModelRequest) -> ModelResponse {
    let calls = calls_in(request);
    // Once it made the call that signs in, it says so in a line and its turn ends.
    let signed_in = calls.iter().any(|(name, given)| {
        name == "tilo_do"
            && given["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(ACTION))
    });
    if signed_in {
        return says("I asked you for its password. Type it into the field in our chat.");
    }
    let with = asked_with(request);
    let (theirs, told) = with
        .split_once(FROM_THE_SERVER)
        .unwrap_or((with.as_str(), ""));
    // Everything it was shown of its workspace so far: what its turn was told at its start,
    // and what its own looks answered.
    let mut shown = told.to_owned();
    for (answer, _) in answers_in(request) {
        shown.push('\n');
        shown.push_str(&answer);
    }
    if shows_it(&shown) {
        // The call as what it was told writes one: the app, then the action, and of what the
        // action takes only what its person wrote.
        let address = theirs
            .split_whitespace()
            .find(|word| word.contains('@'))
            .map(|word| word.trim_matches(|at: char| !(at.is_ascii_alphanumeric() || at == '@')))
            .unwrap_or_default();
        let call = serde_json::json!({
            "path": format!("{APP}{ACTION}"),
            "args": {"address": address},
        });
        return calls_tool("tilo_do", calls.len(), call);
    }
    // It was not shown the action: it looks, for the apps, then into the app. Then it gives up.
    let looked = |into: Option<&str>| {
        calls.iter().any(|(name, given)| {
            name == "tilo_ls" && given.get("path").and_then(serde_json::Value::as_str) == into
        })
    };
    if !looked(None) {
        return calls_tool("tilo_ls", calls.len(), serde_json::json!({}));
    }
    if !looked(Some(APP)) {
        let into = serde_json::json!({"path": APP, "limit": 200});
        return calls_tool("tilo_ls", calls.len(), into);
    }
    says("I could not find how to sign in to a mail account here.")
}

/// She writes one sentence. Her assistant's turn is told at its start what her workspace has,
/// so its first tool call is the one that signs her mail account in: one tool call from her
/// sentence to the question for her password, and two asks of its model, the call and its
/// closing words. A turn that is told nothing looks first, and each look is an ask more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn told_at_its_start_a_turn_signs_a_mail_account_in_with_its_first_tool_call() {
    let hosted = Hosted::start().await;
    hosted.workspaces.find_mail_servers_through(finds);
    // The stand-in's servers answer under no name of the network. Its operator lets this
    // server ask what it would not find there.
    hosted.workspaces.call_private_model_addresses(true);
    let model = Arc::new(ToldAtItsStart::default());
    hosted.workspaces.answer_models_with(model.clone());

    hosted.turn(SENTENCE, Vec::new()).await;

    let asked = model.asked.lock().unwrap().clone();
    let first = asked.first().expect("her assistant's model was asked");
    let last = asked.last().expect("her assistant's model was asked");
    let with = asked_with(first);
    let (theirs, told) = with
        .split_once(FROM_THE_SERVER)
        .unwrap_or((with.as_str(), ""));
    let of_the_app: Vec<&str> = told.lines().filter(|line| line.contains(APP)).collect();
    let made = calls_in(last);
    let calls: Vec<String> = made
        .iter()
        .map(|(name, given)| format!("{name} {}", given["path"].as_str().unwrap_or_default()))
        .collect();
    let seen = format!(
        "its tool calls in order: {calls:?}\nits turn was told {} bytes at its start; the \
         lines of them that name `{APP}`: {of_the_app:#?}",
        told.len()
    );
    // Her own words come first in what her assistant's model reads of her message.
    assert_eq!(theirs.trim_end(), SENTENCE, "{with}");
    // Its first tool call is the one that signs the account in, and it makes no other.
    assert_eq!(calls, [format!("tilo_do {APP}{ACTION}")], "{seen}");
    // The call carries her address and nothing else.
    assert_eq!(
        made[0].1["args"],
        serde_json::json!({"address": ADDRESS}),
        "{seen}"
    );
    // Nothing was refused: the one answer says she is asked in a field her assistant cannot
    // read, and names both servers that were found.
    let answers = answers_in(last);
    assert_eq!(answers.len(), 1, "{answers:#?}\n{seen}");
    let (answer, refused) = &answers[0];
    assert!(
        !refused
            && answer.contains("protected field")
            && answer.contains(READS)
            && answer.contains(SENDS),
        "{answer}\n{seen}"
    );
    // One question asks her for the password, and it names her account.
    let questions = hosted.questions(chat::question::SECRET).await;
    assert_eq!(questions.len(), 1, "{seen}");
    assert!(
        questions[0].1.text.contains(ADDRESS),
        "{}",
        questions[0].1.text
    );
    // Two asks of the model from her sentence to that question: the call, and its closing
    // words.
    assert_eq!(asked.len(), 2, "{seen}");
    hosted.page.peer.close();
}
