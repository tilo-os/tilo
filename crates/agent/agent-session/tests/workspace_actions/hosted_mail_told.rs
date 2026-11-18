//! A person tells their assistant to sign in to their mail, and the assistant's first tool
//! call is the one that does it.
//!
//! The model here knows of its workspace only what its turn was told at its start: the
//! paragraph the server puts after its person's words, which names every app with every
//! action it offers, by name, and a thing an app holds once by the path a call takes. It
//! reads no tool's own words, and it looks nothing up once it was shown the action: where
//! that paragraph names the action that signs a mail account in on the line of the thing that
//! keeps a person's mail accounts, it writes the call at once, by the path that line gives,
//! with the address its person wrote and nothing else. Where it was not shown the action it
//! looks, as a model does that is told nothing: for the apps, into the app, into the thing
//! there. Every such look is one more ask of a real model, and many seconds for the person
//! who waits.
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

/// What the tools answered in `request`, in the order the calls were made.
fn answers_in(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result.content.clone()),
            _ => None,
        })
        .collect()
}

/// The path a call takes to sign a mail account in, as `told` shows it at a turn's start: the
/// app itself where the app's own line names [`ACTION`] (`accounts: #list, #connect`), and a
/// thing in the app where that thing's line does (`  accounts/Mail accounts: #connect`) and its
/// name says it keeps mail, as her sentence does. The line of a kind of thing the app holds
/// several of shows none: a call on such a thing takes the thing's name, which that line does
/// not tell.
fn path_shown(told: &str) -> Option<String> {
    let with_its_label = format!("{APP} (");
    let of_a_thing = format!("{APP}/");
    told.lines().find_map(|line| {
        let (named, offered) = line.split_once(": ")?;
        if !offered.split(", ").any(|name| name == ACTION) {
            return None;
        }
        if named == APP || named.starts_with(&with_its_label) {
            return Some(APP.to_owned());
        }
        let thing = named.trim_start();
        (thing.starts_with(&of_a_thing) && thing.to_lowercase().contains("mail"))
            .then(|| thing.to_owned())
    })
}

/// The thing in [`APP`] that keeps mail, as the app's own listing `listed` names it: the path
/// of the row whose name says so.
fn thing_listed(listed: &str) -> Option<String> {
    listed
        .lines()
        .filter_map(|row| row.split("  ").next())
        .find(|name| name.to_lowercase().contains("mail"))
        .map(|name| format!("{APP}/{}", name.trim_end_matches('/')))
}

/// Whether `listed`, the listing of a thing, names [`ACTION`] on a line of its own
/// (`#connect(address)  Sign in to mail account`).
fn lists_it(listed: &str) -> bool {
    listed.lines().any(|line| {
        line.strip_prefix(ACTION)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['(', ' ']))
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
    // The call takes the address its person wrote, and nothing else.
    let signs_in_at = |path: &str| {
        let address = theirs
            .split_whitespace()
            .find(|word| word.contains('@'))
            .map(|word| word.trim_matches(|at: char| !(at.is_ascii_alphanumeric() || at == '@')))
            .unwrap_or_default();
        let call = serde_json::json!({
            "path": format!("{path}{ACTION}"),
            "args": {"address": address},
        });
        calls_tool("tilo_do", calls.len(), call)
    };
    // Told at its start where the action is, it writes the call at once.
    if let Some(path) = path_shown(told) {
        return signs_in_at(&path);
    }
    // It was not shown the action: it looks, as a model does that is told nothing, for the
    // apps, into the app, then into the thing there that keeps mail. Each look is answered in
    // the order it was made.
    let answers = answers_in(request);
    let looked = |into: Option<&str>| {
        calls.iter().position(|(name, given)| {
            name == "tilo_ls" && given.get("path").and_then(serde_json::Value::as_str) == into
        })
    };
    let looks = |into: &str| {
        let into = serde_json::json!({"path": into, "limit": 200});
        calls_tool("tilo_ls", calls.len(), into)
    };
    if looked(None).is_none() {
        return calls_tool("tilo_ls", calls.len(), serde_json::json!({}));
    }
    let Some(in_the_app) = looked(Some(APP)) else {
        return looks(APP);
    };
    let thing = answers
        .get(in_the_app)
        .map(String::as_str)
        .and_then(thing_listed);
    if let Some(thing) = thing {
        let Some(in_the_thing) = looked(Some(thing.as_str())) else {
            return looks(&thing);
        };
        if answers
            .get(in_the_thing)
            .map(String::as_str)
            .is_some_and(lists_it)
        {
            return signs_in_at(&thing);
        }
    }
    says("I could not find how to sign in to a mail account here.")
}

/// She writes one sentence. Her assistant's turn is told at its start what her workspace has,
/// so its first tool call is the one that signs her mail account in: one tool call from her
/// sentence to the question for her password, and two asks of its model, the call and its
/// closing words. A turn that is told nothing, or told the action without the thing it is on,
/// looks three times first: four tool calls and five asks.
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
    // Its first tool call is the one that signs the account in, on the thing in the app that
    // keeps her mail accounts, and it makes no other.
    assert_eq!(calls.len(), 1, "{seen}");
    assert!(
        calls[0].starts_with(&format!("tilo_do {APP}/")) && calls[0].ends_with(ACTION),
        "{seen}"
    );
    // The call carries her address and nothing else.
    assert_eq!(
        made[0].1["args"],
        serde_json::json!({"address": ADDRESS}),
        "{seen}"
    );
    // The one answer says that she is asked first, in a field her assistant cannot read, and
    // names both servers that were found. The tools carry "your person is asked first" as a
    // call that did not finish, so the answer says so before its sentences.
    let answers = answers_in(last);
    assert_eq!(answers.len(), 1, "{answers:#?}\n{seen}");
    let answer = &answers[0];
    assert!(
        answer.contains("asked_person_first")
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
