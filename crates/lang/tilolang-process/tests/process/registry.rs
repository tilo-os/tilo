//! `tilo.actions`: the workspace's catalogue read and an action called by its address, from an
//! app's code, through the dispatch as the running invocation's child.

use super::*;

use tilo_actions::remote::{
    ActionCallError, InvocationReceipt, ReceiptStatus, RemoteActionMessage,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilolang_tilo::bundle::Bundle;

/// An app that reads the catalogue and calls one action by its address.
const REGISTRY: &str = r#"app test.registry "Registry"

use tilo.actions as actions
use tilo.invocations as invocations

type Counter {
    value: Int merge counter
}

root Counter

default view counter for Counter

view counter(c: Counter) {
    text("{c.value}")
}

action look() -> Int {
    let found = actions.catalogue(query: .none, app: .none)
    found.len()
}

action poke(address: Text, arguments: json.Value) -> Text {
    let made = actions.call(address: address, arguments: arguments)
    if made.status == .ended {
        "ended"
    } else if made.status == .waiting {
        "waiting"
    } else {
        made.status.label()
    }
}
"#;

const APP: &str = "test.registry";

fn bundle() -> Bundle {
    let used = tilolang_tilo::platform_apps::platform_packages_of(
        ["tilo.actions", "tilo.invocations"].into_iter(),
    )
    .expect("the platform's registry packages read");
    let compiled = tilolang::compile_package(REGISTRY, &tilolang::IdsLock::new(), &used)
        .unwrap_or_else(|diagnostics| panic!("the registry app compiles: {diagnostics:#?}"));
    Bundle::new(&compiled, [])
}

/// The address the app publishes `action` at.
fn published(host: &TilolangProcess, action: &str) -> tilo_actions::ActionAddress {
    host.instances()
        .next()
        .expect("the app runs")
        .published()
        .iter()
        .find(|published| published.address.action == action)
        .unwrap_or_else(|| panic!("{action} is published"))
        .address
        .clone()
}

/// Deliver `address` to the app as ada, waiting until it answers, and answer with the update.
fn delivered(
    host: &mut TilolangProcess,
    window: &mut Window,
    sent: &Sent,
    inbox: &Inbox,
    id: u64,
    address: &tilo_actions::ActionAddress,
    arguments: Option<serde_json::Value>,
) -> tilo_actions::InvocationUpdate {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: address.resource.clone(),
        action: address.action.clone(),
        argument: arguments
            .map(|arguments| format!("{}{arguments}", tilo_actions::ARGUMENTS_JSON_PREFIX)),
        actor: tilo_actions::MemberId::person("ada").into(),
        context: None,
    });
    settle(host, window);
    answered(sent, id).unwrap_or_else(|| panic!("the app ends the invocation {id}"))
}

/// The child call the instance sent, as the dispatch reads it.
fn child_sent(sent: &Sent) -> Option<tilo_actions::remote::InvokeRequest> {
    sent.borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                ..
            } => Some(request.clone()),
            _ => None,
        })
}

/// Answer the child call `invocation` with `status`.
fn receipt(inbox: &Inbox, invocation: InvocationId, status: ReceiptStatus) {
    inbox.borrow_mut().push_back(CollabEvent::RemoteAction {
        message: RemoteActionMessage::Receipt(InvocationReceipt { invocation, status }),
    });
}

fn completed(value: serde_json::Value) -> ReceiptStatus {
    ReceiptStatus::Completed {
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

/// The value an invocation that succeeded answered.
fn succeeded(update: &tilo_actions::InvocationUpdate) -> serde_json::Value {
    match update {
        InvocationUpdate::Succeeded { output, .. } => output.value.clone().unwrap_or_default(),
        other => panic!("the invocation succeeded: {other:?}"),
    }
}

/// Deliver `poke`, asking to write in a note, answer its child call with `answer`, and answer
/// what the app read of it.
fn poked(
    host: &mut TilolangProcess,
    window: &mut Window,
    sent: &Sent,
    inbox: &Inbox,
    id: u64,
    poke: &tilo_actions::ActionAddress,
    answer: serde_json::Value,
) -> String {
    let served = InvocationId::new();
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation: served,
        resource: poke.resource.clone(),
        action: poke.action.clone(),
        argument: Some(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({
                "address": "so.tilo.notes/notes.note:1#write",
                "arguments": {"title": "Take the ferry"},
            })
        )),
        actor: tilo_actions::MemberId::person("ada").into(),
        context: None,
    });
    settle(host, window);
    let child = child_sent(sent).expect("the call went through the dispatch");
    assert_eq!(child.target.provider, "tilo.actions");
    assert_eq!(child.target.address.action, "call");
    assert_eq!(child.on_behalf_of, Some(served));
    let given = match &child.argument {
        tilo_actions::remote::InvokeArgument::Json(given) => given.clone(),
        other => panic!("the address and arguments were sent: {other:?}"),
    };
    assert_eq!(given["address"], "so.tilo.notes/notes.note:1#write");
    assert_eq!(given["arguments"]["title"], "Take the ferry");
    receipt(inbox, child.invocation, completed(answer));
    settle(host, window);
    match succeeded(&answered(sent, id).expect("it ends")) {
        serde_json::Value::String(text) => text,
        other => panic!("the status read as text: {other:?}"),
    }
}

tilo_test_env::scenario! {
    fn the_registry_is_read_and_called_by_address_as_the_running_invocations_child;

    phase a_catalogue_read_lists_the_actions_the_dispatch_offers;
    {
        let (mut host, mut window, sent, inbox, _route) =
            running_app("ada", APP, bundle());
        settle(&mut host, &mut window);
        let look = published(&host, "look");
        let served = InvocationId::new();
        inbox.borrow_mut().push_back(CollabEvent::Invocation {
            id: 1,
            invocation: served,
            resource: look.resource.clone(),
            action: look.action.clone(),
            argument: None,
            actor: tilo_actions::MemberId::person("ada").into(),
            context: None,
        });
        settle(&mut host, &mut window);
        let child = child_sent(&sent).expect("the catalogue was read through the dispatch");
        assert_eq!(child.target.provider, "tilo.actions");
        assert_eq!(child.target.address.action, "catalogue");
        assert_eq!(child.on_behalf_of, Some(served), "it is the run's child");
        receipt(
            &inbox,
            child.invocation,
            completed(serde_json::json!([{
                "address": "so.tilo.notes/notes.note:1#write",
                "app": "so.tilo.notes",
                "app_label": "Notes",
                "label": "Write in a note",
                "resource": "Note",
                "kind": "read",
                "params": [{"name": "body", "ty": "text", "required": true}],
                "confirms": false,
            }])),
        );
        settle(&mut host, &mut window);
        assert_eq!(succeeded(&answered(&sent, 1).expect("it ends")), 1);
    }

    phase a_call_answers_the_invocation_it_was_and_waits_when_it_asks;
    {
        let (mut host, mut window, sent, inbox, _route) =
            running_app("ada", APP, bundle());
        settle(&mut host, &mut window);
        let poke = published(&host, "poke");
        // The dispatch kept the call as its asked record: the run reads the invocation waiting.
        assert_eq!(
            poked(
                &mut host,
                &mut window,
                &sent,
                &inbox,
                2,
                &poke,
                serde_json::json!({
                    "id": "inv-1",
                    "status": "waiting",
                    "action": "notes.note:1#write",
                    "started_ms": 1,
                }),
            ),
            "waiting"
        );
        // A call the dispatch answered ended reads as the case the code compares it with: the
        // status the record carries is the one `.{case}` builds, not only what its label says.
        assert_eq!(
            poked(
                &mut host,
                &mut window,
                &sent,
                &inbox,
                4,
                &poke,
                serde_json::json!({
                    "id": "inv-2",
                    "status": "ended",
                    "action": "notes.note:1#write",
                    "started_ms": 1,
                    "ended_ms": 2,
                    "output": {"title": "Take the ferry"},
                }),
            ),
            "ended"
        );
    }

    phase a_call_the_caller_may_not_make_is_refused;
    {
        let (mut host, mut window, sent, inbox, _route) =
            running_app("ada", APP, bundle());
        settle(&mut host, &mut window);
        let poke = published(&host, "poke");
        refused_call(&mut host, &mut window, &sent, &inbox, &poke);
    }
}

/// Deliver `poke` and answer its child with a refusal: the served invocation fails with it.
fn refused_call(
    host: &mut TilolangProcess,
    window: &mut Window,
    sent: &Sent,
    inbox: &Inbox,
    poke: &tilo_actions::ActionAddress,
) {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id: 3,
        invocation: InvocationId::new(),
        resource: poke.resource.clone(),
        action: poke.action.clone(),
        argument: Some(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({
                "address": "so.tilo.vault/items:1#read",
                "arguments": {},
            })
        )),
        actor: tilo_actions::MemberId::person("ada").into(),
        context: None,
    });
    settle(host, window);
    let child = child_sent(sent).expect("the call went through the dispatch");
    receipt(
        inbox,
        child.invocation,
        ReceiptStatus::Rejected {
            error: ActionCallError::NotPermitted,
        },
    );
    settle(host, window);
    match answered(sent, 3) {
        Some(InvocationUpdate::Failed { error, .. }) => {
            assert!(
                error.message.to_lowercase().contains("refus")
                    || error.message.to_lowercase().contains("permit"),
                "{error:?}"
            );
        }
        other => panic!("the call was refused: {other:?}"),
    }
}
