//! A notebook the native Notes app wrote, opened by the tilolang Notes: its migration carries
//! every page over once, as one change of the notebook, with its title, its body, its place in
//! the tree and its tags, each note keeping the id its page had, its pictures in this device's
//! content store and its links to other pages the links to their notes. The native app's
//! documents are read, never written.
//!
//! `legacy/rust_notes.hex` is what the native app's own actions wrote: its notebook record and the
//! part of the chain its pages are kept in, saved as that app stores them. The test that wrote it
//! was deleted with the app; the bytes stay, so the conversion keeps proving itself against what
//! the app really wrote.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ReadDoc as _};
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, target};

const NOTES_APP: &str = "so.tilo.notes";
const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);

/// The pages the fixture's notebook holds, by the ids the native app gave them.
const HANDBOOK: &str = "00000000-0000-4000-8000-000000000001";
const SETUP: &str = "00000000-0000-4000-8000-000000000002";
const LAPTOP: &str = "00000000-0000-4000-8000-000000000003";
const SECURITY: &str = "00000000-0000-4000-8000-000000000004";
const INBOX: &str = "00000000-0000-4000-8000-000000000005";

/// Where a workspace keeps the native notebook's record: its default document, the record the
/// chain of pages belongs to. The native app's test harness wrote it under the package's own
/// primary id instead.
const RECORD: &str = "derived:default@tab:0000000000000000";

/// The documents of the fixture, each by its document id in a workspace.
fn native_documents() -> Vec<(String, AutoCommit)> {
    include_str!("legacy/rust_notes.hex")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, hex) = line.split_once('\t').expect("an id and the document");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let id = if id == "@tilo:package-primary" {
                RECORD
            } else {
                id
            };
            (id.to_owned(), AutoCommit::load(&bytes).unwrap())
        })
        .collect()
}

/// The device the test runs on: its content store, which the pictures are carried into.
#[derive(Clone, Default)]
struct Device {
    content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
}

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
        Ok(None)
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        // The content's own address: the same bytes are one file, as a content store keeps them.
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
        self.content
            .borrow_mut()
            .insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// The bytes of the one picture the fixture's pages show, as the native app kept them beside its
/// pages.
fn desk_picture(documents: &[(String, AutoCommit)]) -> Vec<u8> {
    let (_, part) = documents
        .iter()
        .find(|(id, _)| id.starts_with("derived:part."))
        .unwrap();
    let (_, images) = part.get(automerge::ROOT, "images").unwrap().unwrap();
    let keys: Vec<String> = part.keys(&images).collect();
    assert_eq!(keys.len(), 1, "the fixture shows one picture: {keys:?}");
    match part.get(&images, keys[0].as_str()).unwrap().unwrap() {
        (automerge::Value::Scalar(s), _) => match s.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => bytes.clone(),
            other => panic!("a picture is bytes: {other:?}"),
        },
        other => panic!("a picture is bytes: {other:?}"),
    }
}

/// The fixture's notebook with the reactions the native app kept on one of its pages, keyed as
/// its `Reactions` field keys them: the emoji, who reacted's persona, and their id in hex.
fn with_reactions(documents: &[(String, AutoCommit)]) -> Vec<(String, AutoCommit)> {
    documents
        .iter()
        .map(|(id, doc)| {
            let mut doc = doc.clone();
            if id.starts_with("derived:part.") {
                let (_, pages) = doc.get(automerge::ROOT, "pages").unwrap().unwrap();
                let (_, page) = doc.get(&pages, HANDBOOK).unwrap().unwrap();
                let (_, fields) = doc.get(&page, "v").unwrap().unwrap();
                let reactions = doc.put_object(&fields, "reactions", ObjType::Map).unwrap();
                for (emoji, who) in [("🎉", "ada"), ("🎉", "bob"), ("👍", "ada")] {
                    let hex = automerge::ActorId::from(who.as_bytes()).to_hex_string();
                    let key = format!("{emoji}\u{1f}\u{1f}{hex}");
                    let reaction = doc
                        .put_object(&reactions, key.as_str(), ObjType::Map)
                        .unwrap();
                    doc.put(&reaction, "emoji", emoji).unwrap();
                    doc.put(&reaction, "by", who).unwrap();
                }
                doc.commit();
            }
            (id.clone(), doc)
        })
        .collect()
}

/// Ada's workspace with the tilolang Notes on `device`, the native app's `documents` beside it,
/// and its notebook as the window opens it.
fn opened(documents: &[(String, AutoCommit)], device: &Device) -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new(
        "ada",
        crate::fixtures::Workspace::new(),
        &[DISCUSSION, NOTES],
    );
    ada.host.runtime().set_blobs(device.clone());
    ada.host.runtime().set_earlier(NOTES_APP, documents);
    let notebook = ada.host.root_of(NOTES_APP).expect("the notebook");
    ada.commit();
    (ada, notebook)
}

fn read(ada: &Replica, at: &ResourceRef, field: &str) -> Value {
    ada.host.runtime().read(at, field).unwrap()
}

fn listed(ada: &Replica, notebook: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match read(ada, notebook, field) {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("a collection reads as a list: {other:?}"),
    }
}

fn id_of(note: &ResourceRef) -> String {
    note.item.clone().expect("a note is an item")
}

fn text(ada: &Replica, at: &ResourceRef, field: &str) -> String {
    match read(ada, at, field) {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

fn parent(ada: &Replica, note: &ResourceRef) -> Option<String> {
    match read(ada, note, "parent") {
        Value::Opt(Some(held)) => Some(id_of(&target(&held))),
        Value::Opt(None) => None,
        other => panic!("a parent reads {other:?}"),
    }
}

fn tags(ada: &Replica, note: &ResourceRef) -> Vec<String> {
    match read(ada, note, "tags") {
        Value::List(names) => names
            .into_iter()
            .map(|name| match name {
                Value::Text(name) => name,
                other => panic!("a tag reads {other:?}"),
            })
            .collect(),
        other => panic!("the tags read {other:?}"),
    }
}

/// Each paragraph of the note's body: its kind, its words, and what marks or describes it.
fn body(ada: &Replica, note: &ResourceRef) -> Vec<(String, String, String)> {
    match read(ada, note, "body") {
        Value::Rich(rich) => rich
            .blocks
            .iter()
            .map(|block| {
                let mut said = Vec::new();
                if let Some(level) = block.int_attr("level") {
                    said.push(format!("level {level}"));
                }
                if let Some(checked) = block.bool_attr("checked") {
                    said.push(format!("checked {checked}"));
                }
                if block.flag("ordered") {
                    said.push("ordered".to_owned());
                }
                for mark in &block.marks {
                    if mark.name != "author" {
                        said.push(format!(
                            "{} {}..{} {:?}",
                            mark.name, mark.start, mark.end, mark.value
                        ));
                    }
                }
                (block.kind.clone(), block.text.clone(), said.join(", "))
            })
            .collect(),
        other => panic!("a body reads {other:?}"),
    }
}

fn block(kind: &str, text: &str, said: &str) -> (String, String, String) {
    (kind.to_owned(), text.to_owned(), said.to_owned())
}

/// The note's pictures: the address of each one's content, and the name it stands under.
fn images(ada: &Replica, note: &ResourceRef) -> Vec<(String, String)> {
    match read(ada, note, "images") {
        // A map field reads as its entries, each the key and what it holds.
        Value::List(entries) => entries
            .into_iter()
            .map(|entry| match entry {
                Value::Tuple(pair) => match pair.as_slice() {
                    [Value::Text(address), Value::Text(name)] => (address.clone(), name.clone()),
                    other => panic!("a picture reads {other:?}"),
                },
                other => panic!("a picture reads {other:?}"),
            })
            .collect(),
        other => panic!("the pictures read {other:?}"),
    }
}

/// The notebook carried over from the fixture, as the native app showed it, its picture's
/// bytes the content `device` holds.
fn assert_carried(ada: &Replica, notebook: &ResourceRef, device: &Device, picture: &[u8]) {
    let notes = listed(ada, notebook, "notes");
    let ids: Vec<String> = notes.iter().map(id_of).collect();
    assert_eq!(
        ids,
        [HANDBOOK, SETUP, LAPTOP, SECURITY, INBOX],
        "every page, each keeping its id, in the tree's order: a note before the notes under it, \
         siblings in the order of their ranks"
    );
    let note = |id: &str| notes.iter().find(|n| id_of(n) == id).unwrap();
    let titles: Vec<String> = notes.iter().map(|n| text(ada, n, "title")).collect();
    assert_eq!(titles, ["Handbook", "Setup", "Laptop", "Security", "Inbox"]);
    let parents: Vec<Option<String>> = notes.iter().map(|n| parent(ada, n)).collect();
    assert_eq!(
        parents,
        [
            None,
            Some(HANDBOOK.to_owned()),
            Some(SETUP.to_owned()),
            Some(HANDBOOK.to_owned()),
            None,
        ],
        "each under the note its page stood under, two deep"
    );
    assert_eq!(tags(ada, note(HANDBOOK)), ["work"]);
    assert_eq!(tags(ada, note(LAPTOP)), ["work"]);
    assert_eq!(tags(ada, note(INBOX)), ["urgent"]);
    assert!(tags(ada, note(SETUP)).is_empty());

    assert_eq!(
        body(ada, note(HANDBOOK)),
        [
            block("heading", "Getting started", "level 2"),
            block(
                "paragraph",
                "Read the guide first.",
                "link 9..14 Str(\"https://tilo.sh/guide\")"
            ),
            block("list_item", "Ask questions", "level 0"),
            block("list_item", "Write things down", "level 0"),
            block("list_item", "Get your laptop", "level 0, checked false"),
            block(
                "list_item",
                "Join the team workspace",
                "level 0, checked true"
            ),
            block(
                "paragraph",
                "| Name | Role |\n| --- | --- |\n| Ada | Lead |",
                ""
            ),
        ],
        "headings, a link, a list, a checklist and a table, as the native app kept them"
    );
    assert_eq!(
        body(ada, note(SETUP)),
        [
            block("heading", "Accounts", "level 3"),
            block("list_item", "Mail", "level 0, ordered"),
            block("list_item", "Chat", "level 0, ordered"),
        ]
    );
    let laptop = body(ada, note(LAPTOP));
    assert_eq!(laptop.len(), 2, "{laptop:?}");
    assert_eq!(laptop[0].1, "Pick one up at the desk. See Inbox.");
    assert!(
        laptop[0]
            .2
            .starts_with(&format!("{} 29..34 ", tilolang_runtime::rich::MENTION))
            && laptop[0].2.contains(INBOX)
            && !laptop[0].2.contains("#page-"),
        "the link to another page is the link to its note, which opens it: {laptop:?}"
    );
    let shown = images(ada, note(LAPTOP));
    assert_eq!(shown.len(), 1, "the note shows its picture: {shown:?}");
    let (address, name) = &shown[0];
    assert_eq!(name, "The desk.png", "under the name the page gave it");
    assert_eq!(
        laptop[1],
        block("paragraph", &format!("![The desk]({address})"), ""),
        "the picture is where the page showed it, by the address of its content"
    );
    let hash = address
        .strip_prefix(tilolang_runtime::blob::SCHEME)
        .expect("a picture's address is its content's");
    assert_eq!(
        device.read(hash).as_deref(),
        Some(picture),
        "the picture's bytes are this device's to show"
    );
    assert!(images(ada, note(HANDBOOK)).is_empty());
    assert!(listed(ada, notebook, "set_aside").is_empty());
}

#[test]
fn a_native_notebook_is_carried_over_once_as_one_change() {
    let documents = native_documents();
    let picture = desk_picture(&documents);
    let before: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let device = Device::default();
    let (ada, notebook) = opened(&documents, &device);
    assert!(
        listed(&ada, &notebook, "notes").is_empty(),
        "nothing is carried before the migration runs"
    );
    assert_eq!(ada.host.migrate(), 1, "the notebook's migration runs");
    assert_eq!(ada.commit(), 1, "as one change");
    assert_carried(&ada, &notebook, &device, &picture);

    let migration = ada
        .host
        .package(NOTES_APP)
        .unwrap()
        .meta
        .migrations()
        .find(|m| m.name == "carry_native_notebook")
        .unwrap()
        .id
        .clone();
    let versions = ada.host.runtime().versions(&notebook).unwrap();
    assert_eq!(
        versions.last().map(|v| v.action.as_str()),
        Some(migration.as_str()),
        "history names the migration as the change that carried the pages: {versions:?}"
    );
    assert_eq!(
        versions.iter().filter(|v| v.action == migration).count(),
        1,
        "one version of the notebook"
    );

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[DISCUSSION, NOTES]);
    grace.host.runtime().set_blobs(device.clone());
    grace.host.runtime().set_earlier(NOTES_APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device with the same documents finds it done"
    );
    assert_carried(&grace, &notebook, &device, &picture);

    let after: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's documents are not written");

    // Undo takes the whole carrying back as one step, and redo makes it again.
    step(&ada, false);
    assert!(
        listed(&ada, &notebook, "notes").is_empty(),
        "undone at once"
    );
    step(&ada, true);
    assert_carried(&ada, &notebook, &device, &picture);
}

#[test]
fn a_native_page_keeps_the_reactions_it_had_as_words() {
    let documents = with_reactions(&native_documents());
    let device = Device::default();
    let (ada, notebook) = opened(&documents, &device);
    assert_eq!(ada.host.migrate(), 1, "the notebook's migration runs");
    assert_eq!(ada.commit(), 1);
    let notes = listed(&ada, &notebook, "notes");
    let handbook = notes.iter().find(|n| id_of(n) == HANDBOOK).unwrap();
    assert_eq!(
        text(&ada, handbook, "reacted"),
        "🎉 2  👍 1",
        "each emoji and how many reacted with it, who reacted there not carried"
    );
    let setup = notes.iter().find(|n| id_of(n) == SETUP).unwrap();
    assert_eq!(
        text(&ada, setup, "reacted"),
        "",
        "a page nobody reacted to says nothing of reactions"
    );
    assert_eq!(
        read(&ada, handbook, "reactions"),
        Value::List(Vec::new()),
        "the reactions of this notebook are the members' own, and none carried one"
    );
}

/// The person of `replica` takes their last step back, or makes it again.
fn step(replica: &Replica, again: bool) {
    assert!(if again {
        replica.redo_last()
    } else {
        replica.undo_last()
    });
}

/// The fixture's pages as version 2 of the native app kept them: in the notebook's own record,
/// beside its tags, ordered by their place in the collection, before pages had ranks.
fn version_two(documents: &[(String, AutoCommit)]) -> AutoCommit {
    let (_, part) = documents
        .iter()
        .find(|(id, _)| id.starts_with("derived:part."))
        .unwrap();
    let mut doc = part.clone().fork();
    doc.put(automerge::ROOT, "tilo:version", 2_u64).unwrap();
    let (_, pages) = doc.get(automerge::ROOT, "pages").unwrap().unwrap();
    for id in doc.keys(&pages).collect::<Vec<_>>() {
        let (_, entry) = doc.get(&pages, id.as_str()).unwrap().unwrap();
        let (_, fields) = doc.get(&entry, "v").unwrap().unwrap();
        doc.delete(&fields, "rank").unwrap();
    }
    // The collection's order is the tree's: Handbook (Setup (Laptop), Security), Inbox.
    for (id, order) in [
        (HANDBOOK, "a"),
        (SETUP, "b"),
        (LAPTOP, "c"),
        (SECURITY, "d"),
        (INBOX, "e"),
    ] {
        let (_, entry) = doc.get(&pages, id).unwrap().unwrap();
        doc.put(&entry, "k", order).unwrap();
    }
    let tags = doc
        .put_object(automerge::ROOT, "tags", ObjType::Map)
        .unwrap();
    for (id, name, order) in [
        ("00000000-0000-4000-8000-000000000010", "work", "i"),
        ("00000000-0000-4000-8000-000000000011", "urgent", "j"),
    ] {
        let entry = doc.put_object(&tags, id, ObjType::Map).unwrap();
        doc.put(&entry, "k", order).unwrap();
        let fields = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        doc.put(&fields, "name", name).unwrap();
    }
    doc.commit();
    doc
}

#[test]
fn a_notebook_kept_before_pages_had_parts_is_carried_over_too() {
    let documents = native_documents();
    let picture = desk_picture(&documents);
    let record = version_two(&documents);
    let device = Device::default();
    let (ada, notebook) = opened(&[(RECORD.to_owned(), record)], &device);
    assert_eq!(ada.host.migrate(), 1);
    assert_eq!(ada.commit(), 1);
    assert_carried(&ada, &notebook, &device, &picture);
}

#[test]
fn a_page_under_a_deleted_note_is_set_aside_and_a_document_of_no_known_layout_is_left_alone() {
    let mut documents = native_documents();
    // Setup is deleted, as the native app deletes a note: Laptop under it stays in its part,
    // which the native app no longer showed.
    let (_, part) = documents
        .iter_mut()
        .find(|(id, _)| id.starts_with("derived:part."))
        .unwrap();
    let (_, pages) = part.get(automerge::ROOT, "pages").unwrap().unwrap();
    part.delete(&pages, SETUP).unwrap();
    // And the bytes of Laptop's picture are gone from beside the pages.
    part.delete(automerge::ROOT, "images").unwrap();
    part.commit();
    // A notebook from before notes were kept as a resource: no layout this reads.
    let mut older = AutoCommit::new();
    older
        .put(automerge::ROOT, "notes_schema_version", 1_u64)
        .unwrap();
    older.commit();
    documents.push((
        "uuid:4b0c1e9e-8f6d-4a63-9d0e-3d1b8f7a2c55".to_owned(),
        older,
    ));

    let device = Device::default();
    let (ada, notebook) = opened(&documents, &device);
    assert_eq!(ada.host.migrate(), 1);
    assert_eq!(ada.commit(), 1);
    let ids: Vec<String> = listed(&ada, &notebook, "notes").iter().map(id_of).collect();
    assert_eq!(ids, [HANDBOOK, SECURITY, INBOX]);
    let aside = listed(&ada, &notebook, "set_aside");
    assert_eq!(
        aside.len(),
        2,
        "the page, and its picture whose bytes are gone"
    );
    assert_eq!(id_of(&aside[0]), LAPTOP);
    assert_eq!(text(&ada, &aside[0], "title"), "Laptop");
    assert_eq!(tags(&ada, &aside[0]), ["work"]);
    assert!(images(&ada, &aside[0]).is_empty());
    let laptop = body(&ada, &aside[0]);
    assert!(
        laptop[1].1.starts_with("![The desk](asset:"),
        "the page still says where its picture stood: {laptop:?}"
    );
    assert_eq!(text(&ada, &aside[1], "title"), "A picture of Laptop");
    let why = body(&ada, &aside[1]);
    assert!(
        why.len() == 1
            && why[0]
                .1
                .starts_with("The native Notes app kept no bytes for this picture")
            && why[0].1.contains("![The desk](asset:"),
        "the picture is set aside saying why: {why:?}"
    );
    assert!(device.content.borrow().is_empty(), "no bytes, no file");

    // A workspace whose only earlier document is of no layout this reads: nothing is carried
    // and it is not touched, and the notebook starts as a new one, its carrying still to run.
    let only = vec![documents.pop().unwrap()];
    let heads = only[0].1.clone().get_heads();
    let (grace, notebook) = opened(&only, &Device::default());
    assert!(!grace.host.runtime().has_earlier(NOTES_APP));
    assert_eq!(grace.host.migrate(), 0);
    assert!(listed(&grace, &notebook, "notes").is_empty());
    assert_eq!(only[0].1.clone().get_heads(), heads);
}

#[test]
fn a_notebook_another_device_made_first_still_receives_the_native_pages() {
    let documents = native_documents();
    let picture = desk_picture(&documents);
    // Grace opens the tilolang Notes first, on a device that is not the workspace's origin and
    // holds none of the native app's documents: her notebook starts empty, its carrying still to
    // run.
    let (grace, _) = Replica::new(
        "grace",
        crate::fixtures::Workspace::new(),
        &[DISCUSSION, NOTES],
    );
    let notebook = grace.host.root_of(NOTES_APP).expect("the notebook");
    grace.commit();
    assert!(listed(&grace, &notebook, "notes").is_empty());

    // Ada's device, the origin, comes back with the native app's documents and Grace's notebook.
    let device = Device::default();
    let ada = grace.fork("ada", &[DISCUSSION, NOTES]);
    ada.host.runtime().set_blobs(device.clone());
    ada.host.runtime().set_earlier(NOTES_APP, &documents);
    assert_eq!(
        ada.host.root_of(NOTES_APP).expect("the notebook"),
        notebook,
        "one notebook, the one Grace made"
    );
    assert_eq!(
        ada.host.migrate(),
        1,
        "the carrying was not marked done where the notebook was made"
    );
    assert_eq!(ada.commit(), 1);
    assert_carried(&ada, &notebook, &device, &picture);
    assert_eq!(ada.host.migrate(), 0, "and it runs once");
}

#[test]
fn a_notebook_opened_before_the_native_pages_arrived_receives_them_once_they_do() {
    let documents = native_documents();
    let picture = desk_picture(&documents);
    // Ada's device opens the tilolang Notes before the native app's documents reached it: there
    // is nothing to carry, and finding nothing is not the carrying done.
    let device = Device::default();
    let (ada, notebook) = opened(&[], &device);
    assert_eq!(ada.host.migrate(), 0, "nothing to carry yet");
    assert_eq!(ada.commit(), 0, "and nothing recorded");
    assert!(listed(&ada, &notebook, "notes").is_empty());
    assert_eq!(ada.host.migrate(), 0, "still nothing");

    // The documents arrive, and the next start reads them.
    ada.host.runtime().set_earlier(NOTES_APP, &documents);
    assert_eq!(
        ada.host.migrate(),
        1,
        "the carrying runs once they are here"
    );
    assert_eq!(ada.commit(), 1);
    assert_carried(&ada, &notebook, &device, &picture);
    assert_eq!(ada.host.migrate(), 0, "and only once");
    assert_eq!(ada.commit(), 0);
    assert_carried(&ada, &notebook, &device, &picture);
}

/// The fixture's pages kept in two parts of their chain, as the native app keeps a notebook that
/// grew: Handbook and Setup in part 0, Laptop, Security and Inbox (and Laptop's picture) in part
/// 1, its newest.
fn in_two_parts(documents: &[(String, AutoCommit)]) -> Vec<(String, AutoCommit)> {
    let (id, part) = documents
        .iter()
        .find(|(id, _)| id.starts_with("derived:part.0@"))
        .unwrap();
    let mut first = part.clone().fork();
    let mut second = part.clone().fork();
    let (_, pages) = first.get(automerge::ROOT, "pages").unwrap().unwrap();
    for later in [LAPTOP, SECURITY, INBOX] {
        first.delete(&pages, later).unwrap();
    }
    first.delete(automerge::ROOT, "images").unwrap();
    first.commit();
    let (_, pages) = second.get(automerge::ROOT, "pages").unwrap().unwrap();
    for earlier in [HANDBOOK, SETUP] {
        second.delete(&pages, earlier).unwrap();
    }
    second.commit();
    let (_, record) = documents.iter().find(|(id, _)| id == RECORD).unwrap();
    vec![
        (RECORD.to_owned(), record.clone()),
        (id.clone(), first),
        (id.replacen("derived:part.0@", "derived:part.1@", 1), second),
    ]
}

#[test]
fn a_notebook_whose_pages_are_still_arriving_is_carried_once_they_all_are() {
    let documents = native_documents();
    let picture = desk_picture(&documents);
    let parts = in_two_parts(&documents);
    let device = Device::default();
    // The record and the chain's newest part are here, its first part still on its way: what is
    // here is not carried, since the carrying would be done and part 0's pages left behind.
    let arriving = vec![parts[0].clone(), parts[2].clone()];
    let (ada, notebook) = opened(&arriving, &device);
    assert!(!ada.host.runtime().has_earlier(NOTES_APP));
    assert_eq!(ada.host.migrate(), 0, "the carrying waits");
    assert_eq!(ada.commit(), 0);
    assert!(listed(&ada, &notebook, "notes").is_empty());
    // Nor are the parts without the record they belong to, whose tags they name.
    ada.host
        .runtime()
        .set_earlier(NOTES_APP, &[parts[1].clone(), parts[2].clone()]);
    assert!(!ada.host.runtime().has_earlier(NOTES_APP));
    assert_eq!(ada.host.migrate(), 0, "still waiting");

    // The first part arrives, and the next start reads every part.
    ada.host.runtime().set_earlier(NOTES_APP, &parts);
    assert_eq!(ada.host.migrate(), 1, "every page is carried, once");
    assert_eq!(ada.commit(), 1);
    assert_carried(&ada, &notebook, &device, &picture);
    assert_eq!(ada.host.migrate(), 0);
}
