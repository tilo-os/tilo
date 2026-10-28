#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! What History answers for one version, per resource type, through the read
//! door every app publishes: the type's own adapter when it renders text, the
//! exact bytes it holds when it does not, and the three distinct refusals —
//! unsupported, unavailable, incompatible — never a silent empty.
//!
//! The app's own history machinery is the one under test: the same root,
//! dispatch, storage and history verbs a person's window uses.

use serde_json::{Value, json};
use tilo_app::automerge::transaction::Transactable as _;
use tilo_app::prelude::*;
use tilo_resource::Bytes;
use tilo_resource::evolution::VERSION_KEY;
use ui_core::automerge::ROOT;

use super::history_actions::BoardApp;

/// A file: bytes with no text rendering of their own, and a name beside them.
/// Its read adapter answers the metadata; the bytes are named by content
/// address, because a type that declares no text rendering has none to show.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Blob {
    pub name: String,
    pub content: Bytes,
}

/// The type's own read of the record: the name, never the bytes as text.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a read adapter answers a Result, as every type's must"
)]
fn read_blob(blob: &Blob, _item: Option<&Id>) -> Result<Value, String> {
    Ok(json!({ "name": blob.name }))
}

#[resource(kind = "test.blob", label = "File", read_content = read_blob)]
impl Blob {
    /// Keep the bytes under a new name.
    #[action]
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }
}

struct BlobApp;

impl ResourceApp for BlobApp {
    type Resource = Blob;
    const ID: &'static str = "so.tilo.test-blob";
    const TITLE: &'static str = "File";

    fn view(cx: &mut ViewCx<Blob>) -> Element {
        let p = cx.paths();
        Box::new(tilo_app::column![
            cx.input(p.name).with_id("name"),
            cx.text(|blob: &Blob| format!("{} bytes", blob.content.len())),
        ])
    }
}

/// The read door's answer for a version, through the app's own verbs.
fn read_at<A: ResourceApp>(app: &mut TestApp<A>, version: &str) -> Result<Value, String> {
    app.call("read", json!({ "version": version }))
        .map(|output| output.unwrap())
        .map_err(|error| error.to_string())
}

/// The newest version's name, as history lists it.
fn newest<A: ResourceApp>(app: &TestApp<A>) -> String {
    app.versions()
        .last()
        .map(|version| version.hash.to_string())
        .expect("an edit is a version")
}

#[test]
fn a_text_type_reads_each_version_through_its_own_adapter() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "Draft"})).unwrap();
    app.call("checkpoint", json!({"name": "draft"})).unwrap();
    app.call("rename", json!({"title": "Final"})).unwrap();

    // The past version is the type's own read of the record as it stood,
    // never a dump of the document: the bookkeeping the history machinery
    // writes stays out of it.
    let past = read_at(&mut app, "draft").unwrap();
    assert_eq!(past["content"]["title"], "Draft");
    assert!(
        !past.to_string().contains(VERSION_KEY),
        "a typed read never shows the raw record: {past}"
    );
    assert!(
        past["heads"]
            .as_array()
            .is_some_and(|heads| !heads.is_empty())
    );

    // Reading the same version twice answers the same thing, and never
    // moves the live record.
    assert_eq!(read_at(&mut app, "draft").unwrap(), past);
    assert_eq!(app.state().title, "Final");
}

#[test]
fn a_missing_version_is_unavailable_which_is_not_unsupported() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "only"})).unwrap();
    let missing = read_at(&mut app, "0123456789abcdef").unwrap_err();
    assert!(
        missing.contains("There is no version"),
        "a version that is not in the record says so: {missing}"
    );
    // The same app reads its own version: unavailable is this version, and
    // says nothing about the type.
    let newest = newest(&app);
    assert!(read_at(&mut app, &newest).is_ok());
}

#[test]
fn a_version_written_by_another_storage_schema_is_incompatible_not_empty() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "current"})).unwrap();
    // A collaborator runs a newer build of the app and bumps the storage
    // version; its change syncs here.
    let mut peer = app.peer_document();
    peer.put(ROOT, VERSION_KEY, 9_u64).unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();
    let version = newest(&app);
    let refused = read_at(&mut app, &version).unwrap_err();
    assert!(
        refused.contains("newer version of the app"),
        "a schema mismatch says so: {refused}"
    );
    assert!(
        !refused.contains("could not find") && !refused.contains("not in this document"),
        "it is not reported as a missing version: {refused}"
    );
}

#[test]
fn a_delete_is_a_version_like_any_other_and_keeps_its_identity() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "kept"})).unwrap();
    app.call("checkpoint", json!({"name": "before delete"}))
        .unwrap();
    // A collaborator clears the title.
    let mut peer = app.peer_document();
    peer.put(ROOT, "title", "").unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();

    let before = read_at(&mut app, "before delete").unwrap();
    assert_eq!(before["content"]["title"], "kept");
    // Restoring the version brings it back as one step the person can undo.
    app.call("restore", json!({"version": "before delete"}))
        .unwrap()
        .unwrap();
    assert_eq!(app.state().title, "kept");
    app.call("undo", json!({})).unwrap();
    assert_eq!(app.state().title, "");
}

#[test]
fn a_binary_type_names_the_exact_bytes_a_version_holds() {
    let mut app = TestApp::<BlobApp>::new().unwrap();
    let first = vec![0u8, 159, 146, 150, 0, 1, 2];
    let second = b"a later picture".to_vec();
    app.invoke(Invoke::edit(&Blob::paths().content.set(Bytes::of(&first))).unwrap())
        .unwrap();
    app.call("checkpoint", json!({"name": "original"}))
        .unwrap()
        .unwrap();
    app.invoke(Invoke::edit(&Blob::paths().content.set(Bytes::of(&second))).unwrap())
        .unwrap();

    let past = read_at(&mut app, "original").expect("a binary version is readable");
    let binary = &past["content"]["binary"];
    let address = binary["address"].as_str().expect("a content address");
    assert_eq!(binary["len"], json!(first.len()));
    // The address is the bytes themselves: the storage layer's own hash of
    // what the version held, so a restore fetches exactly that content.
    assert!(address.starts_with("blake3:"), "{address}");
    let (hash, len) = address
        .trim_start_matches("blake3:")
        .split_once(':')
        .expect("hash and length");
    assert_eq!(len, first.len().to_string());
    assert_eq!(hash.len(), 64, "a BLAKE3 hash names the bytes: {address}");
    assert!(
        past["content"].get("content").is_none(),
        "binary content is never rendered as text: {past}"
    );

    // The live record still holds the later bytes: history never moves it.
    assert_eq!(app.state().content, Bytes::of(&second));
}

#[test]
fn the_same_type_surface_answers_for_every_app() {
    // Both apps go through one door: the read payload names a version, and
    // what comes back is the type's own answer, not the app's.
    let mut board = TestApp::<BoardApp>::new().unwrap();
    board.call("rename", json!({"title": "text"})).unwrap();
    let version = newest(&board);
    let textual = read_at(&mut board, &version).unwrap();
    assert_eq!(textual["content"]["title"], "text");

    let mut blob = TestApp::<BlobApp>::new().unwrap();
    let bytes = b"bytes".to_vec();
    blob.invoke(Invoke::edit(&Blob::paths().content.set(Bytes::of(&bytes))).unwrap())
        .unwrap();
    let version = newest(&blob);
    let binary = read_at(&mut blob, &version).unwrap();
    assert_eq!(binary["content"]["binary"]["len"], json!(bytes.len()));
}
