//! A workspace the native Tube wrote, opened by the tilolang Tube: its migration carries the
//! channels subscribed to, the videos liked and kept for later, the comment liked, and the watch
//! history over once, each watch at the time it was watched and where an unfinished video was
//! left. The native app's documents are read, never written.
//!
//! `legacy/rust_tube.hex` is what the native app's own model wrote: its library (two channels
//! subscribed to and one taken back, a video liked and one kept for later, a comment liked) and
//! its watch history (three videos watched, one forgotten again, one left partway through), one
//! document a line (its id, a tab, the saved document as hex), made by its verbs and edits.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{ResourceRef, Value};

const TUBE: &str = "so.tilo.tube";

fn package() -> (&'static str, &'static str) {
    (
        include_str!("../../../../apps/tl/tube/app.tl"),
        include_str!("../../../../apps/tl/tube/app.ids"),
    )
}

/// The documents the native Tube kept, each by its document id in a workspace.
fn native_documents() -> Vec<(String, automerge::AutoCommit)> {
    include_str!("legacy/rust_tube.hex")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, hex) = line.split_once('\t').expect("an id and the document");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
            // The workspace stamps every document it keeps with the record it holds; the model
            // wrote these outside one.
            automerge::transaction::Transactable::put_object(
                &mut doc,
                automerge::ROOT,
                "tilo:record",
                automerge::ObjType::Map,
            )
            .unwrap();
            (id.to_owned(), doc)
        })
        .collect()
}

/// Ada's workspace with the tilolang Tube, the native app's `documents` beside it, migrated as
/// the workspace's origin migrates it when the app starts.
fn opened(documents: &[(String, automerge::AutoCommit)]) -> Host {
    let host = Host::new(
        tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person("Ada")),
        || None,
    )
    .unwrap();
    // The session the app plays through, which its `use` names.
    host.load_builtin(
        include_str!("../../../../apps/tl/playback/app.tl"),
        include_str!("../../../../apps/tl/playback/app.ids"),
    )
    .unwrap();
    host.load_builtin(package().0, package().1).unwrap();
    host.runtime().set_earlier(TUBE, documents);
    assert_eq!(host.migrate(), 1, "the migration runs");
    host
}

/// The id of the field `field` of the package's type `ty`.
fn field_id(host: &Host, ty: &str, field: &str) -> String {
    let packages = host.packages();
    let package = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client");
    package
        .meta
        .types
        .iter()
        .find(|t| t.name == ty)
        .and_then(|t| t.fields.iter().find(|f| f.name == field))
        .unwrap_or_else(|| panic!("{ty}.{field}"))
        .id
        .clone()
}

/// The items of the root's collection `collection` (of the type `ty`), each as its fields
/// `fields` read.
fn items(host: &Host, collection: &str, ty: &str, fields: &[&str]) -> Vec<Vec<Value>> {
    let root = host.root_of(TUBE).expect("the root");
    let Ok(Value::List(held)) = host
        .runtime()
        .read(&root, &field_id(host, "YouTube", collection))
    else {
        panic!("the root holds its {collection}");
    };
    held.into_iter()
        .map(|item| {
            let Value::Ref(at) = item else {
                panic!("an item of {collection} is a resource");
            };
            let at: ResourceRef = (*at.target).clone();
            fields
                .iter()
                .map(|field| {
                    host.runtime()
                        .read(&at, &field_id(host, ty, field))
                        .unwrap()
                })
                .collect()
        })
        .collect()
}

fn text(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn the_native_tubes_subscriptions_likes_queue_and_history_are_carried_over_once() {
    let documents = native_documents();
    let before: Vec<Vec<u8>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().save())
        .collect();
    let host = opened(&documents);

    let mut subscribed: Vec<String> = items(&host, "subscriptions", "Subscription", &["key"])
        .iter()
        .map(|item| text(&item[0]))
        .collect();
    subscribed.sort();
    assert_eq!(
        subscribed,
        ["UCBJycsmduvYEL83R_U4JriQ", "UCHnyfMqiRRG1u-2MsSQLbXA"],
        "the channels still subscribed to, and not the one taken back"
    );

    let videos = items(&host, "videos", "Video", &["key", "liked"]);
    let video = |key: &str| {
        videos
            .iter()
            .find(|item| text(&item[0]) == key)
            .unwrap_or_else(|| panic!("the video {key} is carried: {videos:?}"))
    };
    assert_eq!(
        video("jfKfPfyJRdk")[1],
        Value::Bool(true),
        "the liked video"
    );
    assert_eq!(video("aircAruvnKk")[1], Value::Bool(false));
    assert_eq!(videos.len(), 2, "one video each, liked or kept: {videos:?}");
    let queue = items(&host, "queue", "QueuedVideo", &["key"]);
    assert_eq!(
        queue.iter().map(|item| text(&item[0])).collect::<Vec<_>>(),
        ["aircAruvnKk"],
        "the video kept for later went on the watch queue"
    );

    let history = items(&host, "history", "WatchEntry", &["key", "watched"]);
    let watched: Vec<(String, String)> = history
        .iter()
        .map(|item| (text(&item[0]), format!("{:?}", item[1])))
        .collect();
    assert_eq!(
        watched.len(),
        2,
        "the two videos watched, and not the one taken out of the history: {watched:?}"
    );
    for (key, at) in [
        ("jfKfPfyJRdk", "2025-11-01T12:26:40"),
        ("aircAruvnKk", "2025-11-02T16:13:20"),
    ] {
        let (_, when) = watched
            .iter()
            .find(|(watched, _)| watched == key)
            .unwrap_or_else(|| panic!("{key} is in the history: {watched:?}"));
        assert!(when.contains(at), "{key} was watched at {at}: {when}");
    }

    // Opened again, nothing is carried twice: the migration ran once.
    host.migrate();
    assert_eq!(
        items(&host, "subscriptions", "Subscription", &["key"]).len(),
        2
    );
    assert_eq!(items(&host, "history", "WatchEntry", &["key"]).len(), 2);

    let after: Vec<Vec<u8>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().save())
        .collect();
    assert_eq!(
        before, after,
        "the native app's documents are left as they were"
    );
}

#[test]
fn the_comments_a_person_liked_are_carried() {
    let host = opened(&native_documents());
    let root = host.root_of(TUBE).expect("the root");
    let Ok(Value::List(liked)) = host
        .runtime()
        .read(&root, &field_id(&host, "YouTube", "liked_comments"))
    else {
        panic!("the root holds the comments it liked");
    };
    assert_eq!(liked, vec![Value::text("v1#c1")], "the comment liked there");
}

#[test]
fn the_place_a_native_video_was_left_at_is_carried() {
    let host = opened(&native_documents());
    let history = items(&host, "history", "WatchEntry", &["key", "left_ms"]);
    let entry = history
        .iter()
        .find(|item| text(&item[0]) == "jfKfPfyJRdk")
        .unwrap_or_else(|| panic!("the video is in the history: {history:?}"));
    assert_eq!(
        entry[1],
        Value::Int(92_000),
        "where the native app left the video, in milliseconds"
    );
    let untouched = history
        .iter()
        .find(|item| text(&item[0]) == "aircAruvnKk")
        .expect("the other watched video");
    assert_eq!(
        untouched[1],
        Value::Int(0),
        "a video it left nowhere keeps none"
    );
}
