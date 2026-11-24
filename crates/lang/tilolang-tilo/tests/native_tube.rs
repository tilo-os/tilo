//! The library and watch history the native Tube kept, opened by the tilolang YouTube: its
//! migration carries the channels subscribed to, the videos liked, the watch queue, the watch
//! history with when each video was watched, and the replies written to comments over once, as one
//! change. A device that opens the same documents later finds it done. The native app's documents
//! are read, never written.
//!
//! `legacy/rust_tube.hex` is what the native Tube's own model wrote: its library (subscriptions,
//! one taken back again, likes, the queue, a liked comment and two replies) and its watch history
//! (four videos watched, one left halfway, one forgotten again).

use automerge::AutoCommit;
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, Workspace, target};

const TUBE: &str = "so.tilo.tube";
const SOURCE: &str = include_str!("../../../../apps/tl/tube/app.tl");
const IDS: &str = include_str!("../../../../apps/tl/tube/app.ids");
const PLAYBACK_SOURCE: &str = include_str!("../../../../apps/tl/playback/app.tl");
const PLAYBACK_IDS: &str = include_str!("../../../../apps/tl/playback/app.ids");

fn native_documents() -> Vec<(String, AutoCommit)> {
    crate::fixtures::native_documents(include_str!("legacy/rust_tube.hex"))
}

/// `name`'s replica of `doc` with the client, the native app's `documents` beside it, and its
/// root as the window opens it.
fn opened(name: &str, doc: Workspace, documents: &[(String, AutoCommit)]) -> (Replica, ResourceRef) {
    // The shared player comes first: a package compiles against the ones loaded before it.
    let (replica, _) = Replica::new(name, doc, &[(PLAYBACK_SOURCE, PLAYBACK_IDS), (SOURCE, IDS)]);
    replica.host.runtime().set_earlier(TUBE, documents);
    let root = replica.host.root_of(TUBE).expect("the client");
    replica.commit();
    (replica, root)
}

fn listed(r: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match r.host.runtime().read(at, field).unwrap() {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("a collection reads as a list: {other:?}"),
    }
}

fn text(r: &Replica, at: &ResourceRef, field: &str) -> String {
    match r.host.runtime().read(at, field).unwrap() {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

fn keys(r: &Replica, root: &ResourceRef, field: &str) -> Vec<String> {
    let mut keys: Vec<String> = listed(r, root, field)
        .iter()
        .map(|at| text(r, at, "key"))
        .collect();
    keys.sort();
    keys
}

/// What the client holds of what the native app kept: subscriptions, liked videos, the queue,
/// the history (each video, when it was watched and whose it is) and the replies.
#[derive(Debug, PartialEq)]
struct Kept {
    subscriptions: Vec<String>,
    liked: Vec<String>,
    queue: Vec<String>,
    history: Vec<(String, String, Value)>,
    replies: Vec<(String, String, Value)>,
}

fn kept(r: &Replica, root: &ResourceRef) -> Kept {
    let mut history: Vec<(String, String, Value)> = listed(r, root, "history")
        .iter()
        .map(|at| {
            (
                text(r, at, "key"),
                text(r, at, "watched"),
                r.host.runtime().read(at, "member").unwrap(),
            )
        })
        .collect();
    history.sort_by(|a, b| a.0.cmp(&b.0));
    let mut replies: Vec<(String, String, Value)> = listed(r, root, "replies")
        .iter()
        .map(|at| {
            (
                text(r, at, "comment"),
                text(r, at, "text"),
                r.host.runtime().read(at, "member").unwrap(),
            )
        })
        .collect();
    replies.sort_by(|a, b| a.0.cmp(&b.0));
    Kept {
        subscriptions: keys(r, root, "subscriptions"),
        liked: listed(r, root, "videos")
            .iter()
            .filter(|at| r.host.runtime().read(at, "liked").unwrap() == Value::Bool(true))
            .map(|at| text(r, at, "key"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        queue: keys(r, root, "queue"),
        history,
        replies,
    }
}

/// What the native Tube showed: the two channels still subscribed to, the two liked videos, the
/// queue, the three videos still in the history with when each was watched (the forgotten one
/// not), and both replies. The history and the replies came before the app recorded who watched
/// or wrote what, so they name no member: they stay the workspace's.
fn expected() -> Kept {
    let strings = |all: &[&str]| all.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let nobody = Value::Opt(None);
    Kept {
        subscriptions: strings(&["UC_lexfridman", "UC_veritasium"]),
        liked: strings(&["v_cells", "v_rockets"]),
        queue: strings(&["v_bridges", "v_cells"]),
        history: vec![
            ("v_bridges".into(), "2026-06-29T07:12:00Z".into(), nobody.clone()),
            ("v_cells".into(), "2026-06-29T07:07:00Z".into(), nobody.clone()),
            ("v_rockets".into(), "2026-06-29T07:02:00Z".into(), nobody.clone()),
        ],
        replies: vec![
            ("Ugx_cells_9".into(), "Mitochondria, of course.".into(), nobody.clone()),
            (
                "Ugx_rockets_1".into(),
                "The second stage lands too now.".into(),
                nobody,
            ),
        ],
    }
}

#[test]
fn a_native_tube_library_and_history_are_carried_over_once_as_one_change() {
    let documents = native_documents();
    let before: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let (ada, root) = opened("ada", Workspace::new(), &documents);
    assert!(listed(&ada, &root, "subscriptions").is_empty(), "nothing before the migration");

    assert_eq!(ada.host.migrate(), 1, "the client's carrying runs");
    assert_eq!(ada.commit(), 1, "as one change");
    assert_eq!(kept(&ada, &root), expected());
    assert_eq!(
        ada.host.runtime().read(&root, "history_cleared").unwrap(),
        Value::Bool(false),
        "the history was never deleted there"
    );

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[(PLAYBACK_SOURCE, PLAYBACK_IDS), (SOURCE, IDS)]);
    grace.host.runtime().set_earlier(TUBE, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device opening the same documents later finds it done"
    );
    assert_eq!(kept(&grace, &root), expected(), "and nothing twice");

    let after: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's documents are not written");

    assert!(ada.undo_last(), "one step takes the carrying back");
    assert!(listed(&ada, &root, "subscriptions").is_empty(), "undone at once");
    assert!(ada.redo_last());
    assert_eq!(kept(&ada, &root), expected());
}

#[test]
fn a_client_opened_before_the_native_documents_arrived_receives_them_once_they_do() {
    let (ada, root) = opened("ada", Workspace::new(), &[]);
    assert_eq!(ada.host.migrate(), 0, "nothing to carry has arrived");
    assert_eq!(ada.commit(), 0);

    ada.host.runtime().set_earlier(TUBE, &native_documents());
    assert_eq!(ada.host.migrate(), 1, "the carrying waited for them");
    assert_eq!(ada.commit(), 1);
    assert_eq!(kept(&ada, &root), expected());
    assert_eq!(ada.host.migrate(), 0, "and runs only once");
}
