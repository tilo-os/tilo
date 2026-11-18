//! A refused call ends with the call that works, written as its caller writes one, and that
//! call works when it is made; a refusal names nothing its caller may not use.
use super::*;

/// The calls `text` writes out, each as the tool and its arguments.
fn taught(text: &str) -> Vec<(String, Value)> {
    text.split("tilo_")
        .skip(1)
        .filter_map(|after| {
            let (name, written) = after.split_once(' ')?;
            if !written.starts_with('{') {
                return None;
            }
            let arguments = serde_json::Deserializer::from_str(written)
                .into_iter::<Value>()
                .next()?
                .ok()?;
            Some((format!("tilo_{name}"), arguments))
        })
        .collect()
}

/// A call refused for its arguments ends with the same call as the action takes it: a named
/// blank where an argument is missing. Made with the blank filled, it is carried out.
#[tokio::test]
async fn a_call_refused_for_its_arguments_ends_with_the_call_that_works() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let (text, failed) = tool(&server, "tilo_do", json!({"path": "notes/Todo#rename"})).await;
    assert!(failed, "{text}");
    let wanted = json!({"path": "notes/Todo#rename", "args": {"title": "<title>"}});
    assert_eq!(
        taught(&text),
        [("tilo_do".to_owned(), wanted.clone())],
        "{text}"
    );
    let mut filled = wanted;
    filled["args"]["title"] = json!("Plums");
    let (text, failed) = tool(&server, "tilo_do", filled).await;
    assert!(!failed, "{text}");
}

/// An action named as none is offered ends with the one whose name comes close, as a call
/// with what the caller gave; where none comes close, with the listing of what there is. Each
/// is carried out when it is made.
#[tokio::test]
async fn an_action_named_wrongly_ends_with_the_call_that_was_meant() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    // As a model writes it that looks for the shape of one argument.
    let wrong = json!({"path": "notes/Todo#rename/title", "args": {"title": "Plums"}});
    let (text, failed) = tool(&server, "tilo_do", wrong).await;
    assert!(
        failed && text.contains("has no action rename/title"),
        "{text}"
    );
    let meant = json!({"path": "notes/Todo#rename", "args": {"title": "Plums"}});
    assert_eq!(
        taught(&text),
        [("tilo_do".to_owned(), meant.clone())],
        "{text}"
    );
    let (text, failed) = tool(&server, "tilo_do", meant).await;
    assert!(!failed, "{text}");
    // None comes close: the listing, which answers.
    let nothing_close = json!({"path": "notes/Todo#explode"});
    let (text, failed) = tool(&server, "tilo_do", nothing_close).await;
    assert!(failed, "{text}");
    let listing = json!({"path": "notes/Todo"});
    assert_eq!(
        taught(&text),
        [("tilo_ls".to_owned(), listing.clone())],
        "{text}"
    );
    let (text, failed) = tool(&server, "tilo_ls", listing).await;
    assert!(!failed, "{text}");
}

/// An action still going says the call that answers how it went, with its own id in it.
#[tokio::test]
async fn an_action_still_going_names_the_call_that_follows_it() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let slow = json!({"path": "notes/Todo#slow", "wait_ms": 30});
    let (text, failed) = tool(&server, "tilo_do", slow).await;
    assert!(failed && text.starts_with("pending "), "{text}");
    let calls = taught(&text);
    let [(name, arguments)] = calls.as_slice() else {
        panic!("one call that follows it: {text}");
    };
    assert_eq!(name, "tilo_status", "{text}");
    let id = arguments["invocation_id"].as_str().unwrap_or_default();
    assert!(uuid::Uuid::parse_str(id).is_ok(), "{text}");
    let (status, _) = tool(&server, "tilo_status", arguments.clone()).await;
    assert!(status.starts_with("pending "), "{status}");
}

/// A refusal names nothing its caller may not use: an app the workspace does not let this
/// agent open is refused without a call into it, whatever the agent tried there, and nothing
/// is asked of the workspace on the agent's behalf.
#[tokio::test]
async fn a_refusal_names_no_call_into_what_the_caller_may_not_open() {
    let notes = Notes::new(true).installed(Installed::WriterClosedToAgent);
    let server = agent(&notes);
    for (name, arguments) in [
        (
            "tilo_do",
            json!({"path": "word-processor#append-paragraf", "args": {"text": "Sloe."}}),
        ),
        (
            "tilo_do",
            json!({"path": "word-processor#append-paragraph"}),
        ),
        ("tilo_do", json!({"path": "word-processor#open"})),
        ("tilo_view", json!({"path": "word-processor"})),
    ] {
        let (text, failed) = tool(&server, name, arguments).await;
        assert!(failed && text.contains("may not open"), "{name}: {text}");
        assert!(
            taught(&text).is_empty() && !text.contains("append-paragraph("),
            "{name} is refused with a call into the app: {text}"
        );
    }
    assert!(
        !notes
            .invoked()
            .iter()
            .any(|(resource, _, _)| resource.contains("word-processor")),
        "nothing was asked of the workspace on the agent's behalf"
    );
}

/// An action as an app offers it on `resource`, with what it takes.
fn offered(resource: &str, name: &str, params: Vec<ActionParam>) -> tilo_actions::ActionDescriptor {
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(resource.to_owned(), name),
        resource_kind: "accounts.mail".into(),
        resource_label: "Mail accounts".into(),
        action_label: name.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract::default(),
        input: tilo_actions::InputSpec::None,
        params,
        view: None,
    }
}

/// A read of what nothing reads ends with the calls that work there: what it offers, as a call
/// with a named blank for each argument it must be given, and the listing of what it holds.
/// Where it offers nothing it says so and names the listing: it never ends in an empty list.
#[test]
fn a_read_of_what_nothing_reads_ends_with_the_calls_that_work_there() {
    let adds = offered(
        "accounts",
        "add-mail-with-item",
        vec![
            ActionParam::required("address", ParamType::Text, "Address"),
            ActionParam::optional("name", ParamType::Text, "Name"),
            ActionParam::required("detail", ParamType::Object, "Detail"),
        ],
    );
    let reply = super::super::instead::not_read("accounts/Mail accounts", &[adds], false);
    assert!(
        reply.failed
            && reply.text.starts_with(
                "accounts/Mail accounts has no read action; its actions: add-mail-with-item"
            ),
        "{}",
        reply.text
    );
    let adding = json!({
        "path": "accounts/Mail accounts#add-mail-with-item",
        "args": {"address": "<address>", "detail": "<detail: object>"},
    });
    assert_eq!(
        taught(&reply.text),
        [("tilo_do".to_owned(), adding)],
        "{}",
        reply.text
    );
    let reply = super::super::instead::not_read("settings", &[], true);
    assert!(
        reply.failed && !reply.text.contains("its actions"),
        "{}",
        reply.text
    );
    assert_eq!(
        taught(&reply.text),
        [("tilo_ls".to_owned(), json!({"path": "settings"}))],
        "{}",
        reply.text
    );
}

/// An object written as the text that holds it is read as the object, and what a refusal
/// repeats of a caller's own is short: a long value and a secret are named, never repeated.
#[test]
fn what_a_caller_gave_is_read_as_declared_and_repeated_only_where_it_is_short() {
    let asks = offered(
        "chat",
        "ask",
        vec![
            ActionParam::required("text", ParamType::Text, "Text"),
            ActionParam::required("detail", ParamType::Object, "Detail"),
            ActionParam::optional("pass", ParamType::Secret, "Pass"),
        ],
    );
    let mut gave = serde_json::Map::new();
    gave.insert("detail".into(), json!("{\"kind\":\"login\"}"));
    gave.insert("text".into(), json!("x".repeat(400)));
    gave.insert("pass".into(), json!("sloe"));
    let read = super::super::instead::as_declared(&asks, gave);
    assert_eq!(read.get("detail"), Some(&json!({"kind": "login"})));
    let again = super::super::instead::Call::does("chat/Messages", &asks, &read, "");
    assert_eq!(
        taught(again.written()),
        [(
            "tilo_do".to_owned(),
            json!({
                "path": "chat/Messages#ask",
                "args": {
                    "text": "<as you gave it>",
                    "detail": {"kind": "login"},
                    "pass": "<as you gave it>",
                },
            })
        )],
        "{}",
        again.written()
    );
    assert!(!again.written().contains("sloe"), "{}", again.written());
}
