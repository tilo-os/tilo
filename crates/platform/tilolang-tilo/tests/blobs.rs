//! A file an action takes in: its bytes are kept in the device's content store, and what the
//! record, the history and a replay carry is the address of them, never the bytes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::AutoCommit;
#[cfg(feature = "record")]
use tilolang_runtime::ViewEvent;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, MAX_CHOSEN, SaveRequest};
use tilolang_runtime::run::WebRequest;
use tilolang_runtime::web::{WebReply, WebTransport};
use tilolang_runtime::{InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::{Replica, action_id, target};

const APP: &str = "so.tilo.lang.pictures";

const PICTURES: &str = r##"app so.tilo.lang.pictures "Pictures" (web: ["example.test"])

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

/// The page's words, as a file an action makes of them.
action make_text(p: Page) -> File {
    File.of_text("made.md", "# {p.words}")
}

/// An archive an action makes of two files it holds, one of them under a folder.
action make_zip(p: Page) -> File {
    File.zip(
        "made.zip",
        [File.of_text("words.md", "# {p.words}"), File.of_text("images/fox.png", "{p.bytes}")],
    )
}

/// An archive holding a file no folder may have.
action make_unsafe(p: Page) -> File {
    File.zip("bad.zip", [File.of_text("../outside.md", p.words)])
}

/// The picture the page holds, by the address its own record names.
action again(p: Page) -> File {
    File.at("again.png", p.picture?.url ?? "")
}

/// A file of the content an address no device holds.
action missing(p: Page) -> File {
    File.at("missing.png", "tilo-blob://not-a-hash-anyone-stored")
}

/// The answer a service gave, as a file.
action fetch(p: Page) -> File uses web {
    File.of_bytes("page.html", web.get("https://example.test/page").body)
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

    /// `bytes` kept as a file called `name`: what an action that makes a file of what it holds
    /// leaves in the content store, under the content's own address.
    fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
        self.content
            .borrow_mut()
            .insert(hash.clone(), bytes.to_vec());
        FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        )
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

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        Ok(self.keeping(name, media, bytes))
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

/// The keys of the buttons saying `label`, in the order the view draws them.
fn buttons(view: &ViewInstance, label: &str) -> Vec<String> {
    nodes(view.tree())
        .into_iter()
        .filter(|node| {
            node.kind == "ui.Button"
                && matches!(node.props.get("label"), Some(Value::Text(said)) if said == label)
        })
        .map(|node| node.key.clone())
        .collect()
}

/// The key of the button saying `label`.
fn button(view: &ViewInstance, label: &str) -> String {
    let keys = buttons(view, label);
    keys.first()
        .cloned()
        .unwrap_or_else(|| panic!("a `{label}` button: {:#?}", shown(view)))
}

/// The key of the last button saying `label`: the newest of a list's rows.
#[cfg(feature = "record")]
fn last_button(view: &ViewInstance, label: &str) -> String {
    let keys = buttons(view, label);
    keys.last()
        .cloned()
        .unwrap_or_else(|| panic!("a `{label}` button: {:#?}", shown(view)))
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

/// What `action` of the pictures app answered, on the page at `at`.
fn asked(r: &Replica, action: &str, at: &tilolang_runtime::ResourceRef) -> Result<Value, String> {
    let id = action_id(
        &r.host.package(APP).expect("the pictures package").meta,
        action,
    );
    let who = Principal::new(r.name.clone());
    let outcome = r
        .host
        .runtime()
        .invoke_action(&who, (id.as_str(), vec![Value::resource(at)]))
        .map_err(|error| error.to_string())?;
    match outcome {
        InvokeOutcome::Done(value) => Ok(value),
        other => Err(format!("`{action}` answered {other:?}")),
    }
}

/// The file `action` made of the page at `at`.
fn made(r: &Replica, action: &str, at: &tilolang_runtime::ResourceRef) -> FileValue {
    match asked(r, action, at) {
        Ok(Value::File(file)) => (*file).clone(),
        other => panic!("`{action}` makes a file: {other:?}"),
    }
}

/// An answer from outside Tilo, as a host's transport hands it on: the page `fetch` asks for.
struct Page(Vec<u8>);

impl WebTransport for Page {
    fn fetch(&self, request: &WebRequest) -> WebReply {
        assert_eq!(request.url, "https://example.test/page");
        WebReply {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            body: self.0.clone(),
        }
    }
}

/// An action makes a file of what it holds — the text, the bytes an answer brought — and the
/// device keeps its bytes under the name and media type it is made as.
#[test]
fn an_action_makes_a_file_of_the_text_and_the_bytes_it_holds() {
    let device = Device::default();
    let (mut ada, _loaded) = page(&device);
    ada.host
        .runtime()
        .set_web(Page(b"<h1>a page</h1>".to_vec()));
    let at = target(&Value::resource(&ada.host.root_of(APP).unwrap()));

    let text = made(&ada, "make_text", &at);
    assert_eq!(text.name, "made.md");
    assert_eq!(text.media, "text/markdown");
    assert_eq!(text.size, 2);
    assert_eq!(
        device.content(&text.hash).as_deref(),
        Some(b"# ".as_slice())
    );

    let bytes = made(&ada, "fetch", &at);
    assert_eq!(bytes.name, "page.html");
    assert_eq!(bytes.media, "text/html");
    assert_eq!(
        device.content(&bytes.hash).as_deref(),
        Some(b"<h1>a page</h1>".as_slice()),
        "the answer's bytes are kept as they came"
    );
    ada.commit();
}

/// An action makes an archive of the files it holds, each under its own name, and a name no
/// folder may have is refused rather than written.
#[test]
fn an_action_makes_an_archive_of_the_files_it_holds() {
    let device = Device::default();
    let (mut ada, _loaded) = page(&device);
    let at = target(&Value::resource(&ada.host.root_of(APP).unwrap()));

    let zip = made(&ada, "make_zip", &at);
    assert_eq!(zip.name, "made.zip");
    assert_eq!(zip.media, "application/zip");
    let bytes = device.content(&zip.hash).expect("the archive is kept");
    let files = bounded_zip::read_zip_parts(&bytes, &bounded_zip::ZipLimits::default())
        .expect("the archive reads");
    assert_eq!(
        files,
        [
            ("words.md".to_owned(), b"# ".to_vec()),
            ("images/fox.png".to_owned(), b"0".to_vec()),
        ],
        "each file under the name it was made as, in the order it was given"
    );

    let refused = asked(&ada, "make_unsafe", &at).expect_err("a name outside the folder");
    assert!(
        refused.contains("../outside.md"),
        "it says which name: {refused}"
    );
    ada.commit();
}

/// An action names the content an address holds: the picture its record shows, and none a device
/// does not hold.
#[test]
fn an_action_names_the_content_an_address_holds() {
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

    let again = made(&ada, "again", &at);
    assert_eq!(again.name, "again.png");
    assert_eq!(again.media, "image/png");
    assert_eq!(again.size, i64::try_from(PIXELS.len()).expect("a size"));
    assert_eq!(device.content(&again.hash).as_deref(), Some(PIXELS));

    let missing = asked(&ada, "missing", &at).expect_err("content no device holds");
    assert!(
        missing.contains("is not on this device"),
        "it says what is missing: {missing}"
    );
}
