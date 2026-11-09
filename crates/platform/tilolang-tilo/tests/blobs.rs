//! A file an action takes in: its bytes are kept in the device's content store, and what the
//! record, the history and a replay carry is the address of them, never the bytes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::AutoCommit;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, MAX_CHOSEN, SaveRequest};
use tilolang_runtime::{UiNode, Value, ViewEvent, ViewInstance};

use crate::fixtures::Replica;

const APP: &str = "so.tilo.lang.pictures";

const PICTURES: &str = r##"app so.tilo.lang.pictures "Pictures"

type Page {
    words: Text
    picture: Option<File>
    bytes: Int = 0
}

root Page

default view sheet for Page

/// Puts a picture on the page.
action put(p: Page, picture: File) (did: "added an image") {
    p.picture = picture
    p.bytes = picture.size
}

/// Takes the words of a text file.
action take(p: Page, file: File) {
    match file.text() {
        .ok(words) => { p.words = words },
        .err(why) => { p.words = why },
    }
}

/// The page's words, as a Markdown file.
action export(p: Page) -> Text {
    "# {p.words}"
}

view sheet(p: Page) {
    stack {
        text(p.words)
        if let picture = p.picture {
            text(picture.name)
            text(picture.url)
            text("{picture.size}")
        }
        for v in history(p) {
            text(v.did)
            button("Undo the last change", on: undo(v))
        }
        button("Choose a picture", on: file.choose(put(p), accept: ["image/png"]))
        button("Take a text file", on: file.choose(take(p)))
        button("Save the words", on: file.save(export(p), name: "page.md"))
    }
}
"##;

/// A file the person picks: its name, its media type and its bytes.
#[derive(Clone, Debug)]
struct Pick {
    name: &'static str,
    media: &'static str,
    bytes: Vec<u8>,
}

/// A file written out: the name it was asked for and the bytes.
type Written = (String, Vec<u8>);

/// The device the test runs on: a content store of its own, the files the person picks next,
/// and what was written out.
#[derive(Clone, Default)]
struct Device {
    content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
    picks: Rc<RefCell<Vec<Pick>>>,
    saved: Rc<RefCell<Vec<Written>>>,
    /// What each chooser said it was for, as the runtime asked.
    asked: Rc<RefCell<Vec<String>>>,
}

impl Device {
    /// The device with `picks` waiting for the person to choose them, first one first.
    fn with(picks: Vec<Pick>) -> Self {
        let device = Self::default();
        *device.picks.borrow_mut() = picks;
        device
    }

    /// What it holds at `hash`.
    fn content(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    /// What it wrote out, by name.
    fn saved(&self) -> Vec<(String, Vec<u8>)> {
        self.saved.borrow().clone()
    }
}

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content(hash)
    }

    fn choose(&self, request: &FileRequest) -> Result<Option<FileValue>, String> {
        self.asked.borrow_mut().push(request.title.clone());
        if self.picks.borrow().is_empty() {
            return Ok(None);
        }
        let pick = self.picks.borrow_mut().remove(0);
        let hash = format!("h{}", self.content.borrow().len().saturating_add(1));
        let size = i64::try_from(pick.bytes.len()).unwrap_or(i64::MAX);
        self.content
            .borrow_mut()
            .insert(hash.clone(), pick.bytes.clone());
        Ok(Some(FileValue::new(pick.name, pick.media, size, hash)))
    }

    fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
        self.saved
            .borrow_mut()
            .push((request.name.clone(), content.to_vec()));
        Ok(())
    }
}

/// A page of this app, on a replica with `device` for its files, and the package it loaded.
fn page(device: &Device) -> (Replica, Vec<tilolang_tilo::host::Package>) {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(PICTURES, "")]);
    ada.host.set_clock(|| "2026-11-02T09:00:00Z".to_owned());
    ada.host.runtime().set_blobs(device.clone());
    ada.host.root_of(APP).unwrap();
    ada.commit();
    (ada, loaded)
}

fn nodes(node: &UiNode) -> Vec<&UiNode> {
    let mut out = vec![node];
    for child in &node.children {
        out.extend(nodes(child));
    }
    out
}

/// The text of a node, when it shows one.
fn text(node: &UiNode) -> Option<&str> {
    match node.props.get("value") {
        Some(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// What the view shows, its text nodes in order.
fn shown(view: &ViewInstance) -> Vec<String> {
    nodes(view.tree())
        .into_iter()
        .filter(|node| node.kind == "ui.Text")
        .filter_map(|node| text(node).map(str::to_owned))
        .collect()
}

/// The key of the button saying `label`.
fn button(view: &ViewInstance, label: &str) -> String {
    nodes(view.tree())
        .into_iter()
        .find(|node| {
            node.kind == "ui.Button"
                && matches!(node.props.get("label"), Some(Value::Text(said)) if said == label)
        })
        .map_or_else(
            || panic!("a `{label}` button: {:#?}", shown(view)),
            |node| node.key.clone(),
        )
}

/// The key of the last button saying `label`: the newest of a list's rows.
fn last_button(view: &ViewInstance, label: &str) -> String {
    nodes(view.tree())
        .into_iter()
        .rfind(|node| {
            node.kind == "ui.Button"
                && matches!(node.props.get("label"), Some(Value::Text(said)) if said == label)
        })
        .map_or_else(
            || panic!("a `{label}` button: {:#?}", shown(view)),
            |node| node.key.clone(),
        )
}

fn target(v: &Value) -> tilolang_runtime::ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// The page's `picture`, as the store reads it.
fn picture(r: &Replica, at: &tilolang_runtime::ResourceRef) -> Option<FileValue> {
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Page", "picture");
    match r.host.runtime().read(at, &id).unwrap() {
        Value::Opt(Some(held)) => match *held {
            Value::File(file) => Some((*file).clone()),
            other => panic!("a file: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("an option: {other:?}"),
    }
}

fn field(r: &Replica, at: &tilolang_runtime::ResourceRef, name: &str) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = crate::fixtures::field_id(&meta, "Page", name);
    r.host.runtime().read(at, &id).unwrap()
}

const PIXELS: &[u8] = b"\x89PNG\r\n\x1a\n-a-picture-nobody-should-store-as-a-register";

/// A person picks a picture: the bytes go to the content store, the record keeps its address,
/// history says what the action did, and a replica that syncs reads the same picture.
#[test]
fn a_chosen_file_is_kept_by_address_and_never_in_the_record() {
    let device = Device::with(vec![Pick {
        name: "Ferns.png",
        media: "image/png",
        bytes: PIXELS.to_vec(),
    }]);
    let (mut ada, loaded) = page(&device);
    let at = target(&Value::resource(&ada.host.root_of(APP).unwrap()));
    let view = ada.view(APP);
    assert_eq!(picture(&ada, &at), None, "a page starts with no picture");

    view.press(&button(&view, "Choose a picture")).unwrap();
    ada.commit();

    let held = picture(&ada, &at).expect("the picture the person chose");
    assert_eq!(held.name, "Ferns.png");
    assert_eq!(held.media, "image/png");
    assert_eq!(held.size, i64::try_from(PIXELS.len()).expect("a size"));
    assert_eq!(
        device.content(&held.hash).as_deref(),
        Some(PIXELS),
        "the bytes are in the content store, at the address the file names"
    );
    assert_eq!(
        field(&ada, &at, "bytes"),
        Value::Int(i64::try_from(PIXELS.len()).expect("a size"))
    );

    // What is recorded is the address: the record's own bytes hold the hash and none of the
    // picture.
    let recorded = String::from_utf8_lossy(&ada.doc.save()).to_string();
    assert!(
        recorded.contains(&held.hash)
            || ada.log.iter().any(|envelope| {
                let bytes = String::from_utf8_lossy(&envelope.action.payload).to_string();
                bytes.contains(&held.hash)
            }),
        "the change names the content address"
    );
    assert!(
        !recorded.contains("nobody-should-store-as-a-register"),
        "no byte of the picture is in the record"
    );

    // A replica that syncs reads the same file, and a view shows its name and address.
    let mut ben = ada.fork("ben", &[(PICTURES, &loaded[0].ids)]);
    ada.sync_with(&mut ben);
    assert_eq!(picture(&ben, &at), Some(held.clone()));
    let view = ada.view(APP);
    assert_eq!(
        shown(&view),
        [
            String::new(),
            "Ferns.png".to_owned(),
            held.url(),
            held.size.to_string(),
            "created it".to_owned(),
            "added an image".to_owned(),
        ],
        "the page shows the picture by name and address, and history says what the action did"
    );
    assert_eq!(
        device.asked.borrow().first().map(String::as_str),
        Some("Put"),
        "the chooser is titled with the action's own label"
    );
}

/// A file larger than a chosen file may be is refused as a value: nothing is written.
#[test]
fn a_file_too_large_to_choose_is_refused() {
    let big = vec![0u8; MAX_CHOSEN + 1];
    let device = Device::with(vec![Pick {
        name: "Huge.png",
        media: "image/png",
        bytes: big,
    }]);
    let (mut ada, _loaded) = page(&device);
    let at = target(&Value::resource(&ada.host.root_of(APP).unwrap()));
    let view = ada.view(APP);
    let refused = view.press(&button(&view, "Choose a picture"));
    let error = refused.expect_err("a file too large to keep is refused");
    assert!(
        error.to_string().contains("more than the"),
        "it says how large a file may be: {error}"
    );
    ada.commit();
    assert_eq!(picture(&ada, &at), None, "nothing was written");
}

/// A file whose media type the chooser did not ask for is refused the same way.
#[test]
fn a_file_of_another_type_than_asked_for_is_refused() {
    let device = Device::with(vec![Pick {
        name: "Notes.txt",
        media: "text/plain",
        bytes: b"not a picture".to_vec(),
    }]);
    let (ada, _loaded) = page(&device);
    let view = ada.view(APP);
    let error = view
        .press(&button(&view, "Choose a picture"))
        .expect_err("the chooser takes pictures");
    assert!(
        error.to_string().contains("image/png"),
        "it says what is asked for: {error}"
    );
}

/// An action reads a text file's content, and a file that is not text, or is more than an
/// action reads as text, answers why instead.
#[test]
fn an_action_reads_a_text_file_and_says_why_when_it_cannot() {
    let device = Device::with(vec![
        Pick {
            name: "plan.md",
            media: "text/markdown",
            bytes: b"# Plan\n\nShip it.".to_vec(),
        },
        Pick {
            name: "scan.pdf",
            media: "application/pdf",
            bytes: vec![0xff, 0xfe, 0x00],
        },
        Pick {
            name: "book.md",
            media: "text/markdown",
            bytes: vec![b'a'; tilolang_runtime::blob::MAX_TEXT + 1],
        },
    ]);
    let (mut ada, _loaded) = page(&device);
    let at = target(&Value::resource(&ada.host.root_of(APP).unwrap()));

    let view = ada.view(APP);
    view.press(&button(&view, "Take a text file")).unwrap();
    ada.commit();
    assert_eq!(field(&ada, &at, "words"), Value::text("# Plan\n\nShip it."));

    let view = ada.view(APP);
    view.press(&button(&view, "Take a text file")).unwrap();
    ada.commit();
    assert_eq!(
        field(&ada, &at, "words"),
        Value::text("`scan.pdf` is not text")
    );

    let view = ada.view(APP);
    view.press(&button(&view, "Take a text file")).unwrap();
    ada.commit();
    let said = match field(&ada, &at, "words") {
        Value::Text(words) => words,
        other => panic!("text: {other:?}"),
    };
    assert!(
        said.contains("more than the") && said.contains("reads as text"),
        "a file too large to read says so: {said}"
    );
}

/// What an action made is put where the person says, under the name the app asks for.
#[test]
fn an_action_saves_what_it_made_as_the_file_the_person_names() {
    let device = Device::with(vec![Pick {
        name: "two-ferns.md",
        media: "text/markdown",
        bytes: b"Two ferns".to_vec(),
    }]);
    let (mut ada, _loaded) = page(&device);

    let view = ada.view(APP);
    view.press(&button(&view, "Take a text file")).unwrap();
    ada.commit();
    let view = ada.view(APP);
    view.press(&button(&view, "Save the words")).unwrap();
    assert_eq!(
        device.saved(),
        vec![("page.md".to_owned(), b"# Two ferns".to_vec())],
        "the file the action made is written under the name it asked for"
    );
}

/// A picture put in a note is taken back like any other change: the field it wrote goes back
/// to what it held, and history no longer says it.
#[cfg(feature = "record")]
#[test]
fn undoing_the_step_takes_the_picture_back_out() {
    use tilolang_runtime::UndoAsk;
    use tilolang_tilo::instance::HistoryAsk;

    let device = Device::with(vec![Pick {
        name: "Ferns.png",
        media: "image/png",
        bytes: PIXELS.to_vec(),
    }]);
    let (mut ada, _loaded) = page(&device);
    let at = target(&Value::resource(&ada.host.root_of(APP).unwrap()));
    let view = ada.view(APP);
    view.press(&button(&view, "Choose a picture")).unwrap();
    ada.commit();
    assert!(picture(&ada, &at).is_some(), "the picture went in");

    let mut view = ada.view(APP);
    view.fire(&last_button(&view, "Undo the last change"), "press")
        .unwrap();
    let ask = view
        .poll_events()
        .into_iter()
        .find_map(|event| match event {
            ViewEvent::Undo(ask) => Some(ask),
            _ => None,
        })
        .expect("the control asks the history to undo");
    let asked = match &ask {
        UndoAsk::Step { change, .. } => HistoryAsk::UndoStep(change.clone()),
        UndoAsk::Run { run } => HistoryAsk::UndoRun(run.clone()),
    };
    ada.history(&asked.edit());
    assert_eq!(
        picture(&ada, &at),
        None,
        "the step took the picture back out"
    );
    assert_eq!(field(&ada, &at, "bytes"), Value::Int(0));
}
