//! The notes app's documents: a note exported as Markdown and imported again is the same note,
//! the file a note came from is remembered, a notebook exported as a zip comes back as the tree
//! it is, with its pictures, and a Notion export becomes notes in one change.

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

/// A note's body, as the words of each block that holds any: what a person reads, and what being
/// a blank line first or last does not change.
fn said(r: &Replica, note: &ResourceRef) -> Vec<String> {
    body(r, note)
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .filter(|text| !text.is_empty())
        .collect()
}

/// A note's body, as the words of each block.
fn words(r: &Replica, note: &ResourceRef) -> Vec<String> {
    body(r, note)
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

fn title(r: &Replica, note: &ResourceRef) -> String {
    match r.host.runtime().read(note, "title") {
        Ok(Value::Text(title)) => title,
        other => panic!("a note's title: {other:?}"),
    }
}

/// The note `note` stands under, or nothing at the top of the tree.
fn parent_of(r: &Replica, note: &ResourceRef) -> Option<ResourceRef> {
    match r.host.runtime().read(note, "parent") {
        Ok(Value::Opt(Some(held))) => match *held {
            Value::Ref(at) => Some((*at.target).clone()),
            other => panic!("a note: {other:?}"),
        },
        Ok(Value::Opt(None)) => None,
        other => panic!("a note's parent: {other:?}"),
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

/// A note the app makes, under `parent`, titled `title`.
fn note(r: &mut Replica, parent: Option<&ResourceRef>, titled: &str) -> ResourceRef {
    let nb = r.host.root_of(APP).unwrap();
    let made = match parent {
        Some(p) => call(
            r,
            "new_child",
            vec![Value::resource(&nb), Value::resource(p)],
        ),
        None => call(r, "new_note", vec![Value::resource(&nb)]),
    };
    let made = match made {
        Value::Ref(at) => (*at.target).clone(),
        other => panic!("a note: {other:?}"),
    };
    call(
        r,
        "retitle",
        vec![Value::resource(&made), Value::text(titled)],
    );
    r.commit();
    made
}

/// `bytes` put in `note` as the picture `name`, as a person choosing it would.
fn picture_in(r: &mut Replica, device: &Device, note: &ResourceRef, name: &str, bytes: &[u8]) {
    let file = Value::file(device.keeping(name, "image/png", bytes));
    call(r, "insert_image", vec![Value::resource(note), file]);
    r.commit();
}

/// What `action` made of `args`, as the file it is.
fn made_file(r: &Replica, action: &str, args: Vec<Value>) -> FileValue {
    match call(r, action, args) {
        Value::File(file) => (*file).clone(),
        other => panic!("`{action}` makes a file, not {other:?}"),
    }
}

/// The Markdown the app exports for `note`.
fn exported(r: &Replica, note: &ResourceRef) -> String {
    match call(r, "export_markdown", vec![Value::resource(note)]) {
        Value::Text(text) => text,
        other => panic!("an export: {other:?}"),
    }
}

/// The files an archive holds, in the order it holds them: what a person who opens the export in
/// their own file manager finds.
fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    bounded_zip::read_zip_parts(bytes, &bounded_zip::ZipLimits::default())
        .expect("the archive reads")
}

/// What an exported archive `file` holds, from the device's content store.
fn held(device: &Device, file: &FileValue) -> Vec<(String, Vec<u8>)> {
    let bytes = device.read(&file.hash).expect("the export's bytes");
    entries(&bytes)
}

/// The content of the entry `name` of an archive.
fn entry(archive: &[(String, Vec<u8>)], name: &str) -> Vec<u8> {
    archive.iter().find(|(held, _)| held == name).map_or_else(
        || {
            panic!(
                "`{name}` among {:?}",
                archive.iter().map(|(held, _)| held).collect::<Vec<_>>()
            )
        },
        |(_, bytes)| bytes.clone(),
    )
}

/// The content of the entry `name` as text.
fn entry_text(archive: &[(String, Vec<u8>)], name: &str) -> String {
    String::from_utf8(entry(archive, name)).expect("a text file")
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
    call(&ada, "import_archive", vec![Value::resource(&nb), file]);
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

const ARCH: &[u8] = b"\x89PNG\r\n\x1a\n-an-arch";

/// The notebook's export: the file the app made of it.
fn exported_notebook(r: &Replica) -> FileValue {
    let nb = r.host.root_of(APP).unwrap();
    let file = made_file(r, "export_notebook", vec![Value::resource(&nb)]);
    assert_eq!(file.name, "Notes.zip");
    assert_eq!(file.media, "application/zip");
    file
}

/// The notebook's export, read back: the files it holds, in the order it holds them.
fn notebook_export(r: &Replica, device: &Device) -> Vec<(String, Vec<u8>)> {
    held(device, &exported_notebook(r))
}

/// A notebook with notes three deep and pictures in two of them: exported as a zip, the zip is a
/// folder of Markdown a reader follows, and importing it gives the notebook back — the tree, the
/// titles, the words and the pictures, their bytes equal.
#[test]
fn a_notebook_exported_and_imported_again_is_the_same_notebook() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let plan = note(&mut ada, None, "Q3 plan");
    picture_in(&mut ada, &device, &plan, "arch.png", ARCH);
    let standups = note(&mut ada, Some(&plan), "Standups");
    let monday = note(&mut ada, Some(&standups), "Monday");
    picture_in(&mut ada, &device, &monday, "fox.png", PIXELS);
    let _home = note(&mut ada, None, "Home");

    let file = exported_notebook(&ada);
    let exported = held(&device, &file);
    let names: Vec<&str> = exported.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Q3 plan.md",
            "Q3 plan/images/arch.png",
            "Q3 plan/Standups.md",
            "Q3 plan/Standups/Monday.md",
            "Q3 plan/Standups/Monday/images/fox.png",
            "Home.md",
        ],
        "a note under the folder of the note it stands in, its pictures beside it"
    );
    for name in &names {
        assert!(
            bounded_zip::is_safe_name(name),
            "`{name}` names a file inside the archive, nowhere else"
        );
    }
    // Each page reaches its picture by the path its own folder gives it, which a reader of the
    // file follows.
    assert!(
        entry_text(&exported, "Q3 plan.md").contains("](Q3 plan/images/arch.png)"),
        "{}",
        entry_text(&exported, "Q3 plan.md")
    );
    assert!(
        entry_text(&exported, "Q3 plan/Standups/Monday.md").contains("](Monday/images/fox.png)"),
        "{}",
        entry_text(&exported, "Q3 plan/Standups/Monday.md")
    );
    assert_eq!(
        entry(&exported, "Q3 plan/Standups/Monday/images/fox.png"),
        PIXELS,
        "a picture's bytes are in the export as they were taken in"
    );

    // The notebook the export makes, imported into an empty one: the tree comes back.
    let (mut ben, _loaded) = notebook(&device);
    let nb = ben.host.root_of(APP).unwrap();
    call(
        &ben,
        "import_archive",
        vec![Value::resource(&nb), Value::file(file)],
    );
    ben.commit();

    let all = written(&ben, &nb);
    assert_eq!(all.len(), 4, "a note for each page");
    let titles: Vec<String> = all.iter().map(|n| title(&ben, n)).collect();
    assert_eq!(titles, ["Q3 plan", "Standups", "Monday", "Home"]);
    assert_eq!(parent_of(&ben, &all[0]), None);
    assert_eq!(
        parent_of(&ben, &all[1]).map(|p| p.id_string()),
        Some(all[0].id_string()),
        "Standups stands under Q3 plan"
    );
    assert_eq!(
        parent_of(&ben, &all[2]).map(|p| p.id_string()),
        Some(all[1].id_string()),
        "Monday stands under Standups"
    );
    assert_eq!(parent_of(&ben, &all[3]), None);
    for (was, now) in [(&plan, &all[0]), (&monday, &all[2])] {
        assert_eq!(
            said(&ada, was),
            said(&ben, now),
            "a body comes back as it went in"
        );
    }

    // The pictures come back too: each body names the address of its bytes again, and the
    // device holds them.
    let shown = words(&ben, &all[2]).join("\n");
    let kept = device.keeping("fox.png", "image/png", PIXELS);
    assert!(
        shown.contains(&kept.url()),
        "the imported page shows its picture: {shown}"
    );
    assert_eq!(device.read(&kept.hash).as_deref(), Some(PIXELS));

    // Exporting what was imported gives the same files again, under the same names.
    assert_eq!(
        notebook_export(&ben, &device),
        exported,
        "the notebook exports as it imported"
    );
}

/// Two notes of one title under one note are two files, and no title can name a file outside the
/// folder the export is read into.
#[test]
fn an_export_names_its_files_safely_wherever_the_titles_point() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let plan = note(&mut ada, None, "Plan");
    note(&mut ada, Some(&plan), "Monday");
    note(&mut ada, Some(&plan), "Monday");
    note(&mut ada, None, "../../etc/passwd");
    note(&mut ada, None, "..");

    let exported = notebook_export(&ada, &device);
    let names: Vec<&str> = exported.iter().map(|(name, _)| name.as_str()).collect();
    for name in &names {
        assert!(
            bounded_zip::is_safe_name(name),
            "`{name}` names a file inside the archive, nowhere else: {names:?}"
        );
        assert!(
            !name.starts_with('/') && name.split('/').all(|part| part != ".." && part != "."),
            "`{name}` does not lead out of the folder it is read into: {names:?}"
        );
    }
    assert!(names.contains(&"Plan/Monday.md"), "{names:?}");
    assert!(
        names.contains(&"Plan/Monday (2).md"),
        "the second note of one title is a file of its own: {names:?}"
    );
}

/// A note's own export: its Markdown while it shows no picture, and a zip of the Markdown and its
/// pictures when it does.
#[test]
fn a_note_exports_as_markdown_and_its_pictures_go_with_it() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let groceries = note(&mut ada, None, "Groceries");

    let plain = made_file(&ada, "export_note", vec![Value::resource(&groceries)]);
    assert_eq!(plain.name, "Groceries.md");
    assert_eq!(plain.media, "text/markdown");
    assert_eq!(
        device.read(&plain.hash).as_deref(),
        Some(b"# Groceries".as_slice()),
        "a note of words alone is the Markdown itself"
    );

    picture_in(&mut ada, &device, &groceries, "fox.png", PIXELS);
    let zipped = made_file(&ada, "export_note", vec![Value::resource(&groceries)]);
    assert_eq!(zipped.name, "Groceries.zip");
    assert_eq!(zipped.media, "application/zip");
    let files = held(&device, &zipped);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["Groceries.md", "Groceries/images/fox.png"]);
    assert!(
        entry_text(&files, "Groceries.md").contains("](Groceries/images/fox.png)"),
        "{}",
        entry_text(&files, "Groceries.md")
    );
    assert_eq!(entry(&files, "Groceries/images/fox.png"), PIXELS);
}

/// Two notes of one title under one note are two files: the first keeps the title, the one after
/// it takes its place among the notes of that title, and each title a file name cannot keep is
/// named the same way, so no file of a folder stands over another.
#[test]
fn two_notes_of_one_title_are_two_files() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let plan = note(&mut ada, None, "Q3 plan");
    let monday = note(&mut ada, Some(&plan), "Monday");
    let _tuesday = note(&mut ada, Some(&plan), "Tuesday");
    let again = note(&mut ada, Some(&plan), "Monday");
    // Two titles that a file name cannot tell apart, `/` standing for `-`.
    let _slash = note(&mut ada, Some(&plan), "a/b");
    let _dash = note(&mut ada, Some(&plan), "a-b");
    picture_in(&mut ada, &device, &monday, "fox.png", PIXELS);
    picture_in(&mut ada, &device, &again, "fox.png", ARCH);

    let exported = notebook_export(&ada, &device);
    let names: Vec<&str> = exported.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Q3 plan.md",
            "Q3 plan/Monday.md",
            "Q3 plan/Monday/images/fox.png",
            "Q3 plan/Tuesday.md",
            "Q3 plan/Monday (2).md",
            "Q3 plan/Monday (2)/images/fox.png",
            "Q3 plan/a-b.md",
            "Q3 plan/a-b (2).md",
        ],
        "a note of a title another takes under the same note is offered as its place among them"
    );
    // Each note reaches its own picture by the folder its own name gives it.
    assert!(
        entry_text(&exported, "Q3 plan/Monday (2).md").contains("](Monday (2)/images/fox.png)"),
        "{}",
        entry_text(&exported, "Q3 plan/Monday (2).md")
    );
    assert_eq!(entry(&exported, "Q3 plan/Monday (2)/images/fox.png"), ARCH);
}

/// The notes `note`'s body refers to, by their ids: what its links name.
fn refers_to(r: &Replica, note: &ResourceRef) -> Vec<String> {
    body(r, note)
        .references()
        .iter()
        .filter_map(tilolang_runtime::rich::resource_id)
        .map(str::to_owned)
        .collect()
}

/// A notebook an archive made: two `Ideas` under one `Home`, each with a note under it, and a
/// link from the first to the second — which names it by the file its own title gave it.
const LINKED: &[(&str, &[u8])] = &[
    ("Home.md", b"# Home\n\nWhat the flat needs.\n"),
    (
        "Home/Ideas.md",
        b"# Ideas\n\nSee [the other ideas](<Ideas (2).md>).\n",
    ),
    ("Home/Ideas/Monday.md", b"# Monday\n\nStandup.\n"),
    ("Home/Ideas (2).md", b"# Ideas\n\nThe second one.\n"),
    (
        "Home/Ideas (2)/Tuesday.md",
        b"# Tuesday\n\nSecond branch.\n",
    ),
];

/// Two notes of one title are two files, each named by the tree's order, and a note's folder
/// follows the name its own note took. A link between them names the file of the note it links
/// to, so the export read back is the notebook it was: the same tree, and a link reaching the
/// same note rather than the first one of that title.
#[test]
fn two_notes_of_one_title_are_two_files_a_link_between_them_reaches() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    let file = Value::file(device.keeping("Notes.zip", "application/zip", &archive(LINKED)));
    let nb = ada.host.root_of(APP).unwrap();
    call(&ada, "import_archive", vec![Value::resource(&nb), file]);
    ada.commit();

    let all = written(&ada, &nb);
    assert_eq!(all.len(), 5, "a note for each page");
    let titles: Vec<String> = all.iter().map(|n| title(&ada, n)).collect();
    assert_eq!(titles, ["Home", "Ideas", "Monday", "Ideas", "Tuesday"]);
    assert_eq!(
        refers_to(&ada, &all[1]),
        [all[3].id_string()],
        "the page's link reaches the second Ideas, the one its path named"
    );

    // Two notes of one title are two files, and each note's folder is the file of the note it
    // stands under: `Ideas (2)` takes its own children with it.
    let export = exported_notebook(&ada);
    let exported = held(&device, &export);
    let names: Vec<&str> = exported.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Home.md",
            "Home/Ideas.md",
            "Home/Ideas/Monday.md",
            "Home/Ideas (2).md",
            "Home/Ideas (2)/Tuesday.md",
        ],
        "a note of a title another took beside it is a file of its own"
    );
    let page = entry_text(&exported, "Home/Ideas.md");
    assert!(
        page.contains("[the other ideas](<Ideas (2).md>)"),
        "the link names the file of the note it links to, as a reader of the file reaches it: \
         {page}"
    );

    // The export imported again is the notebook it was, the link reaching the second `Ideas`
    // again, and exporting that one gives the same files.
    let (mut ben, _loaded) = notebook(&device);
    let nb = ben.host.root_of(APP).unwrap();
    call(
        &ben,
        "import_archive",
        vec![Value::resource(&nb), Value::file(export)],
    );
    ben.commit();
    let all = written(&ben, &nb);
    let titles: Vec<String> = all.iter().map(|n| title(&ben, n)).collect();
    assert_eq!(titles, ["Home", "Ideas", "Monday", "Ideas", "Tuesday"]);
    assert_eq!(parent_of(&ben, &all[1]), Some(all[0].clone()));
    assert_eq!(parent_of(&ben, &all[2]), Some(all[1].clone()));
    assert_eq!(parent_of(&ben, &all[3]), Some(all[0].clone()));
    assert_eq!(
        parent_of(&ben, &all[4]),
        Some(all[3].clone()),
        "the second Ideas keeps its own child"
    );
    assert_eq!(
        refers_to(&ben, &all[1]),
        [all[3].id_string()],
        "the link reaches the second Ideas again, not the first"
    );
    assert_eq!(
        notebook_export(&ben, &device),
        exported,
        "the notebook exports as it imported"
    );
}

/// A title that spells a name the export would give a duplicate keeps a file of its own: the
/// second `Ideas` steps past `Ideas (2)` rather than standing over the note titled that, and
/// two notes without a title are two files as well.
#[test]
fn a_title_that_spells_a_name_the_export_gives_is_a_file_of_its_own() {
    let device = Device::default();
    let (mut ada, _loaded) = notebook(&device);
    note(&mut ada, None, "Ideas");
    note(&mut ada, None, "Ideas (2)");
    note(&mut ada, None, "Ideas");
    note(&mut ada, None, "");
    note(&mut ada, None, "");

    let exported = notebook_export(&ada, &device);
    let names: Vec<&str> = exported.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Ideas.md",
            "Ideas (2).md",
            "Ideas (3).md",
            "Note.md",
            "Note (2).md",
        ],
        "no note's file stands over another's, whatever its title spells"
    );
    // Each file says which note it is, so the notebook read back has every note again.
    let titles: Vec<String> = ["Ideas.md", "Ideas (2).md", "Ideas (3).md"]
        .iter()
        .map(|name| {
            entry_text(&exported, name)
                .lines()
                .next()
                .unwrap_or_default()
                .trim_start_matches("# ")
                .to_owned()
        })
        .collect();
    assert_eq!(titles, ["Ideas", "Ideas (2)", "Ideas"]);
}
