use std::rc::Rc;

use tilo_actions::remote::{
    InvocationReceipt, InvocationState, InvokeArgument, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionFailure, InvocationId, InvocationUpdate};
use ui_core::collaboration::{CollabEvent, CollabRequest};
use ui_core::widget::Collaborative as _;

use ui_core::arriving::Arrival;

use super::{Query, ReadFailure};
use crate::framework::reference::Opener;

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct Count {
    count: u64,
}

fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: "test.live".into(),
        address: ActionAddress::new("counter", "read"),
    }
}

/// What the view's door sends next.
fn sent(opener: &Opener) -> Vec<RemoteActionMessage> {
    opener
        .door()
        .map(|mut door| door.take_collab_requests())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction { message } => Some(message),
            _ => None,
        })
        .collect()
}

fn invoked(messages: &[RemoteActionMessage]) -> InvocationId {
    messages
        .iter()
        .find_map(|message| match message {
            RemoteActionMessage::Invoke(request) => Some(request.invocation),
            _ => None,
        })
        .expect("the read is invoked")
}

fn answer(opener: &Opener, invocation: InvocationId, update: InvocationUpdate) {
    let mut door = opener.door().expect("the read has a door");
    door.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::Update {
            invocation,
            state: Some(InvocationState::new(invocation, update)),
        },
    });
}

fn item(invocation: InvocationId, sequence: u64, count: u64) -> InvocationUpdate {
    InvocationUpdate::Item {
        invocation,
        sequence,
        value: serde_json::json!({ "count": count }),
    }
}

#[test]
fn a_query_follows_each_new_result_and_a_new_argument_replaces_the_read() {
    let opener = Opener::default();
    let alive = Rc::new(());
    let query = Query::<Count>::start(
        &opener,
        &Rc::downgrade(&alive),
        target(),
        InvokeArgument::None,
    );
    assert!(
        query.answer().get().is_pending(),
        "nothing has answered yet"
    );
    let first = invoked(&sent(&opener));
    let mut door = opener.door().unwrap();
    door.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: first,
            status: ReceiptStatus::Accepted,
        }),
    });
    answer(&opener, first, item(first, 1, 1));
    assert_eq!(query.answer().get(), Arrival::Ready(Count { count: 1 }));
    answer(&opener, first, item(first, 2, 2));
    assert_eq!(query.answer().get(), Arrival::Ready(Count { count: 2 }));

    query.set_argument(InvokeArgument::Json(serde_json::Map::from_iter([(
        "step".to_owned(),
        serde_json::json!(10),
    )])));
    let messages = sent(&opener);
    assert!(
        messages.iter().any(|message| matches!(
            message,
            RemoteActionMessage::Cancel { invocation } if *invocation == first
        )),
        "the replaced read is cancelled: {messages:?}"
    );
    let second = invoked(&messages);
    assert_eq!(
        query.answer().get(),
        Arrival::Ready(Count { count: 2 }),
        "the last result shows until the new one lands"
    );
    answer(&opener, first, item(first, 3, 99));
    assert_eq!(
        query.answer().get(),
        Arrival::Ready(Count { count: 2 }),
        "a late answer of the replaced read is not shown"
    );
    answer(&opener, second, item(second, 1, 20));
    assert_eq!(query.answer().get(), Arrival::Ready(Count { count: 20 }));
}

#[test]
fn a_refused_read_is_denied_and_a_gone_scope_cancels_it() {
    let opener = Opener::default();
    let alive = Rc::new(());
    let query = Query::<Count>::start(
        &opener,
        &Rc::downgrade(&alive),
        target(),
        InvokeArgument::None,
    );
    let read = invoked(&sent(&opener));
    answer(&opener, read, item(read, 1, 4));
    answer(
        &opener,
        read,
        InvocationUpdate::Failed {
            invocation: read,
            error: ActionFailure {
                code: "not_permitted".into(),
                message: "Not permitted".into(),
                details: None,
                retryable: false,
            },
        },
    );
    assert_eq!(
        query.answer().get(),
        Arrival::Failed(ReadFailure::Denied("Not permitted".into())),
        "a withdrawn permission shows as denied, not as the last data"
    );
    let shown = query.answer();
    assert!(shown.can_retry(), "a failed read offers to read again");
    shown.retry();
    assert!(
        shown.get().is_pending(),
        "asked again, it waits for the answer"
    );
    let again = invoked(&sent(&opener));
    assert_ne!(again, read, "a retry reads afresh");
    answer(&opener, again, item(again, 1, 5));
    assert_eq!(query.answer().get(), Arrival::Ready(Count { count: 5 }));

    let query = Query::<Count>::start(
        &opener,
        &Rc::downgrade(&alive),
        target(),
        InvokeArgument::None,
    );
    let read = invoked(&sent(&opener));
    drop(query);
    assert!(
        sent(&opener).is_empty(),
        "the scope still follows it after its handle is gone"
    );
    drop(alive);
    let messages = sent(&opener);
    assert!(
        messages.iter().any(|message| matches!(
            message,
            RemoteActionMessage::Cancel { invocation } if *invocation == read
        )),
        "a read whose scope is gone is cancelled: {messages:?}"
    );
}
