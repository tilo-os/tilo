//! A message someone received: a record an outside origin makes for a type that says
//! `incoming` carries the incoming-message facet, with the sender its type names, so an
//! automation's incoming-message trigger, or a record change narrowed by
//! `incoming_message`, fires on mail that arrives. A message the app itself writes is no
//! arrival, and neither is the origin syncing a field of a message it already holds.

use serde_json::{Map, json};
use tilo_actions::{Acting, Did};
use tilo_automation::dispatch::{Armed, DocumentEvent, Event, Firer, dispatch};
use tilo_automation::shortcut::{ChangeFilter, Test, TriggerSpec};
use tilo_automation::{ActorFilter, Shortcut};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::runtime::origin::OriginWrite;

use crate::fixtures::{Replica, Workspace, action_id, field_id, type_id};

const APP: &str = "t.inc";
const MESSAGE: &str = "Message";

/// A mailbox whose messages an outside service sends, and that the app also writes one into:
/// its own copy of what someone here wrote is no message anyone received.
const SOURCE: &str = r#"app t.inc "Incoming"

type Inbox {
    messages: Collection<Message>
}

type Message (origin: imap.message, title: subject, incoming: from) {
    key: Text merge register
    subject: Text
    from: Text
    body: Text
}

/// Writes a message as the app itself does: what someone here wrote, not what arrived.
action write(i: Inbox, from: Text, body: Text) {
    i.messages.insert(Message(from: from, subject: "Re: lunch", body: body))
}

root Inbox

default view main for Inbox

view main(i: Inbox) {
    page {
        text("{i.messages.count()}")
    }
}
"#;

/// Ada's mailbox on her device.
fn mailbox() -> (Replica, tilolang_runtime::Meta) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(SOURCE, "")]);
    let meta = packages.last().unwrap().meta.clone();
    ada.host.root_of(APP).unwrap();
    (ada, meta)
}

/// The stamp of the one write that records a received message.
fn received_stamp(ada: &Replica) -> collab_types::Stamp {
    let mut received: Vec<collab_types::Stamp> = ada
        .doc
        .stamps()
        .into_iter()
        .map(|(_, stamp)| stamp)
        .filter(collab_types::Stamp::incoming_message)
        .collect();
    assert!(!received.is_empty(), "a change records a received message");
    received.remove(0)
}

/// A message the origin took in, as the outside service sent it.
fn receive(ada: &Replica, meta: &tilolang_runtime::Meta) -> ResourceRef {
    let writes = vec![OriginWrite::Insert {
        owner: ada.host.root_of(APP).unwrap(),
        field: field_id(meta, "Inbox", "messages"),
        ty: type_id(meta, MESSAGE),
        fields: vec![
            (
                "from".to_owned(),
                Value::Text("grace@example.com".to_owned()),
            ),
            ("subject".to_owned(), Value::Text("Lunch".to_owned())),
            ("body".to_owned(), Value::Text("Thursday works.".to_owned())),
        ],
    }];
    let made = ada
        .host
        .runtime()
        .apply_origin("imap", writes, None)
        .expect("the origin's write stands");
    made.into_iter()
        .flatten()
        .next()
        .expect("the message the origin wrote")
}

/// The event a change of the message kind carrying `stamp` makes for the automation.
fn event(
    meta: &tilolang_runtime::Meta,
    record: &str,
    stamp: &collab_types::Stamp,
) -> DocumentEvent {
    DocumentEvent {
        workspace: "ws-1".to_owned(),
        record: record.to_owned(),
        kind: meta.kind_of(MESSAGE),
        change: format!("change-{}", stamp.invocation),
        actor: Acting::person(""),
        did: Did::verb(
            tilo_actions::ActionKey::new(APP, &meta.kind_of(MESSAGE), &stamp.verb),
            [],
        ),
        arguments: Map::new(),
        item: None,
        before: None,
        after: None,
        caused_by: None,
        origin: None,
        incoming_message: stamp.incoming_message(),
        housekeeping: false,
        shortcut: None,
        invocation: None,
    }
}

/// A shortcut saved as the person's, watching what arrived in the mailbox.
fn shortcut(meta: &tilolang_runtime::Meta) -> Shortcut {
    Shortcut {
        id: "answer-mail".to_owned(),
        name: "Answer mail".to_owned(),
        inputs: Vec::new(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![
            TriggerSpec::IncomingMessage {
                app_id: Some(APP.to_owned()),
                from: None,
            },
            TriggerSpec::ResourceChanged {
                kind: meta.kind_of(MESSAGE),
                actions: Vec::new(),
                fields: Vec::new(),
                by: ActorFilter::Anyone,
                filter: ChangeFilter::of([("incoming_message", Test::Is(json!(true)))]),
            },
        ],
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// The triggers `shortcut` fires on `event`.
fn fires(shortcut: &Shortcut, event: &DocumentEvent) -> Vec<usize> {
    dispatch(
        &[Armed {
            firer: Firer {
                shortcut: &shortcut.id,
                saver: "bob",
            },
            shortcut,
        }],
        Event::Record(event),
    )
    .firings
    .into_iter()
    .map(|firing| firing.trigger_index)
    .collect()
}

/// A message the app writes itself, as its own copy of what someone here wrote: the stamp of the
/// change that wrote it.
fn write_own(ada: &Replica, meta: &tilolang_runtime::Meta) -> collab_types::Stamp {
    let inbox = ada.host.root_of(APP).unwrap();
    let arguments = json!({ "from": "ada@example.com", "body": "Thursday works." });
    ada.carry(
        APP,
        &meta.kind_of("Inbox"),
        &action_id(meta, "write"),
        Some(&inbox.id_string()),
        arguments.as_object().expect("an object"),
        None,
        None,
    )
    .expect("the app writes its own message");
    ada.doc
        .stamps()
        .into_iter()
        .map(|(_, stamp)| stamp)
        .find(|stamp| stamp.verb == action_id(meta, "write"))
        .expect("the change the action wrote")
}

#[test]
fn a_message_the_origin_takes_in_carries_the_facet_with_its_sender() {
    let (ada, meta) = mailbox();
    receive(&ada, &meta);
    let stamp = received_stamp(&ada);
    let facet = stamp
        .facets
        .iter()
        .find(|facet| facet.key == tilo_actions::INCOMING_MESSAGE_FACET)
        .expect("a received message says so on its change");
    assert_eq!(
        facet.value, "grace@example.com",
        "the facet names its sender"
    );
    assert_eq!(stamp.verb, collab_types::Stamp::ORIGIN_VERB);
}

#[test]
fn the_origin_syncing_a_field_of_a_message_it_holds_is_no_arrival() {
    let (ada, meta) = mailbox();
    let message = receive(&ada, &meta);
    let before = ada
        .doc
        .stamps()
        .into_iter()
        .filter(|(_, stamp)| stamp.incoming_message())
        .count();
    let set = vec![OriginWrite::Set {
        target: message,
        fields: vec![(
            "subject".to_owned(),
            Value::Text("Lunch on Thursday".to_owned()),
        )],
    }];
    ada.host
        .runtime()
        .apply_origin("imap", set, None)
        .expect("the service's field is taken in");
    let after = ada
        .doc
        .stamps()
        .into_iter()
        .filter(|(_, stamp)| stamp.incoming_message())
        .count();
    assert_eq!(
        after, before,
        "syncing a field of a message already held records no received message"
    );
}

#[test]
fn a_message_the_app_writes_is_no_arrival() {
    let (ada, meta) = mailbox();
    let written = write_own(&ada, &meta);
    assert!(
        !written.incoming_message(),
        "the app's own message is no message anyone received"
    );
    assert!(
        ada.doc
            .stamps()
            .iter()
            .all(|(_, stamp)| !stamp.incoming_message()),
        "no change of the app's records a received message"
    );
}

#[test]
fn a_saved_incoming_message_shortcut_fires_on_what_arrived_and_not_on_what_was_written() {
    let (ada, meta) = mailbox();
    let message = receive(&ada, &meta);
    let arrived = event(&meta, &message.id_string(), &received_stamp(&ada));
    let watch = shortcut(&meta);
    assert_eq!(
        fires(&watch, &arrived),
        [0, 1],
        "the incoming-message trigger and the narrowed record change both fire"
    );

    let (ada, meta) = mailbox();
    let written = event(&meta, "m1", &write_own(&ada, &meta));
    assert!(
        fires(&watch, &written).is_empty(),
        "what the app wrote is no message the workspace received"
    );
}
