//! The notes app's documents: a note exported as Markdown and imported again is the same note,
//! the file a note came from is remembered, and a Notion export becomes notes in one change.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::AutoCommit;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{InvokeOutcome, Principal, ResourceRef, Value};
use tilolang_tilo::host::Package;

use crate::fixtures::Replica;

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.lang.notes";

/// A note that says everything a Markdown document may: a heading, a checklist, a fence, a
/// table and a picture.
const DOCUMENT: &str = concat!(
    "# Groceries\n",
    "\n",
    "- [ ] oat milk\n",
    "- [x] eggs\n",
    "\n",
    "Intro with **bold** and a [link](https://example.test).\n",
    "\n",
    "```rust\n",
    "fn main() {}\n",
    "```\n",
    "\n",
    "| a | b |\n",
    "| --- | --- |\n",
    "| 1 | 2 |\n",
    "\n",
    "![a fox](tilo-blob://picture)\n",
    "\n",
    "> remember the bags\n",
    "\n",
    "---\n",
);

/// The device the test runs on: its content store, of the files it was handed.
#[derive(Clone, Default)]
struct Device {
    content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
}

impl Device {
    /// `bytes` kept as a file called `name`: what a person choosing it would answer.
    fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
        self.keep(name, media, bytes).expect("the bytes are kept")
    }
}

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
        Ok(None)
    }

    /// Nothing a test exports leaves this device: what an export writes is the person's own
    /// chooser's, and the tests read the text the action made instead.
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

/// Ada's workspace with its notebook, on `device` for its files.
fn notebook(device: &Device) -> (Replica, Vec<Package>) {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, NOTES]);
    ada.host.set_clock(|| "2026-11-02T09:00:00Z".to_owned());
    ada.host.runtime().set_blobs(device.clone());
    ada.host.root_of(APP).unwrap();
    ada.commit();
    (ada, loaded)
}

fn action_id(meta: &tilolang_runtime::Meta, name: &str) -> String {
    meta.actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the action `{name}`"))
        .id
        .clone()
}

/// What `action` of the notes app answered, on `args`.
fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let id = {
        let package = r.host.package(APP).expect("the notes package");
        action_id(&package.meta, action)
    };
    let who = Principal::new(r.name.clone());
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// The notebook's notes, in its own order.
fn written(r: &Replica, nb: &ResourceRef) -> Vec<ResourceRef> {
    match r.host.runtime().read(nb, "notes") {
        Ok(Value::List(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(r) => Some((*r.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("the notebook's notes: {other:?}"),
    }
}

/// A note's body, as the blocks it holds.
fn body(r: &Replica, note: &ResourceRef) -> RichValue {
    match r.host.runtime().read(note, "body") {
        Ok(Value::Rich(rich)) => rich,
        other => panic!("a note's body: {other:?}"),
    }
}

fn title(r: &Replica, note: &ResourceRef) -> String {
    match r.host.runtime().read(note, "title") {
        Ok(Value::Text(title)) => title,
        other => panic!("a note's title: {other:?}"),
    }
}

/// `text` imported as the file `name` of the media type a Markdown file has.
fn imported(r: &mut Replica, device: &Device, name: &str, text: &str) -> ResourceRef {
    let file = Value::file(device.keeping(name, "text/markdown", text.as_bytes()));
    let nb = r.host.root_of(APP).unwrap();
    call(r, "import_markdown", vec![Value::resource(&nb), file]);
    r.commit();
    let all = written(r, &nb);
    all.last().cloned().expect("the note it imported")
}

/// The Markdown the app exports for `note`.
fn exported(r: &Replica, note: &ResourceRef) -> String {
    match call(r, "export_markdown", vec![Value::resource(note)]) {
        Value::Text(text) => text,
        other => panic!("an export: {other:?}"),
    }
}

#[test]
fn a_note_exported_and_imported_again_is_the_same_note() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let first = imported(&mut ada, &device, "Groceries.md", DOCUMENT);
    assert_eq!(title(&ada, &first), "Groceries", "its title is its heading");
    let held = body(&ada, &first);
    let kinds: Vec<&str> = held
        .blocks
        .iter()
        .map(|block| block.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "list_item",
            "list_item",
            "paragraph",
            "paragraph",
            "paragraph",
            "paragraph",
            "quote",
            "divider",
        ],
        "the heading that names it is not said again: {kinds:?}"
    );

    let markdown = exported(&ada, &first);
    assert!(
        markdown.starts_with("# Groceries\n\n- [ ] oat milk\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("| a | b |\n| --- | --- |\n| 1 | 2 |"),
        "a table is kept as its source: {markdown}"
    );
    assert!(
        markdown.contains("```rust\nfn main() {}\n```"),
        "a fence keeps its language: {markdown}"
    );
    assert!(
        markdown.contains("![a fox](tilo-blob://picture)"),
        "a picture keeps the address of its bytes: {markdown}"
    );

    let again = imported(&mut ada, &device, "Groceries again.md", &markdown);
    assert_eq!(title(&ada, &again), "Groceries");
    assert_eq!(
        exported(&ada, &again),
        markdown,
        "the note exported and imported again is the same note"
    );
}

#[test]
fn importing_the_same_file_twice_makes_one_note() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let first = imported(&mut ada, &device, "Groceries.md", DOCUMENT);
    let twice = imported(&mut ada, &device, "Groceries.md", DOCUMENT);
    assert_eq!(first.id_string(), twice.id_string(), "the note it made");
    assert_eq!(written(&ada, &ada.host.root_of(APP).unwrap()).len(), 1);
}

#[test]
fn a_whole_file_is_exported_under_the_name_it_asks_for() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let note = imported(&mut ada, &device, "Groceries.md", DOCUMENT);
    let text = exported(&ada, &note);
    assert!(text.contains("# Groceries"), "{text}");
}

/// A zip of stored files, as Notion exports one: a page's Markdown beside its pictures.
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    let mut out: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    for (name, content) in entries {
        let offset = u32::try_from(out.len()).expect("a small archive");
        let crc = crc32(content);
        let size = u32::try_from(content.len()).expect("a small file");
        let named = u16::try_from(name.len()).expect("a short name");
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&named.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(content);
        directory.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        directory.extend_from_slice(&20_u16.to_le_bytes());
        directory.extend_from_slice(&20_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&named.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u32.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let start = u32::try_from(out.len()).expect("a small archive");
    out.extend_from_slice(&directory);
    let count = u16::try_from(entries.len()).expect("few files");
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(directory.len())
            .expect("a small directory")
            .to_le_bytes(),
    );
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

const PIXELS: &[u8] = b"\x89PNG\r\n\x1a\n-a-fox";

#[test]
fn a_notion_export_becomes_notes_in_one_change() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let exported = archive(&[
        ("Reading list.md", b"# Reading list\n\nBooks to read.\n"),
        (
            "Reading list/Books.md",
            b"# Books\n\n![a fox](fox.png)\n\n- [ ] Piranesi\n",
        ),
        ("Reading list/fox.png", PIXELS),
    ]);
    let file = Value::file(device.keeping("Reading list.zip", "application/zip", &exported));
    let nb = ada.host.root_of(APP).unwrap();
    let before = ada.log.len();
    call(&ada, "import_notion", vec![Value::resource(&nb), file]);
    ada.commit();
    assert_eq!(
        ada.log.len(),
        before.saturating_add(1),
        "the whole import is one change"
    );

    let all = written(&ada, &nb);
    assert_eq!(all.len(), 2, "a note for each page");
    assert_eq!(title(&ada, &all[0]), "Reading list");
    assert_eq!(title(&ada, &all[1]), "Books");

    // The picture the export kept is the one the page shows, by the address of its bytes.
    let picture = device.keeping("Reading list/fox.png", "image/png", PIXELS);
    let kept = picture.url();
    let blocks = body(&ada, &all[1]);
    assert!(
        blocks
            .blocks
            .iter()
            .any(|block| block.text == format!("![a fox]({kept})")),
        "the page shows the picture the archive held: {:?}",
        blocks
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        device.read(&picture.hash).as_deref(),
        Some(PIXELS),
        "the picture's bytes are in the content store"
    );
}
