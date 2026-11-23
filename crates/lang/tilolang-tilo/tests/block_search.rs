//! A paragraph of a note found by search: system search lists it as a row of its own, said to be
//! in the note, and choosing it opens the note with its editor showing that paragraph.

use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::model::{OPEN_ANCHOR, Source as _, Target};
use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as ui, ToUi, Transport as _};
use tilo_resource::search_index::{IndexState, Publisher};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.notes";
const WORKSPACE: &str = "workspace";

struct Notes;

impl Platform for Notes {
    fn load(&self, host: &Host) -> u64 {
        for (source, ids) in [DISCUSSION, NOTES] {
            host.load_builtin(source, ids).unwrap();
        }
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-23T09:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// Ada's notebook window, and the window's end of what it sends.
fn window() -> (Instance, End) {
    let (app_end, window_end) = pair();
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Notes),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    (instance, window_end)
}

/// What the notes action `action` answered, run by Ada.
fn call(host: &Host, action: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the notes package")
        .meta
        .actions
        .iter()
        .find(|held| held.name == action)
        .unwrap_or_else(|| panic!("the action `{action}`"))
        .id
        .clone();
    let ada = Actor::from(MemberId::person("ada"));
    match host.runtime().invoke_action(&ada, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// Everything the window was sent since it was last read.
fn sent(window: &mut End) -> Vec<ToUi> {
    let mut out = Vec::new();
    while let Some(batch) = window.recv().expect("the window reads its batches") {
        out.extend(ui::decode_batch::<ToUi>(&batch).expect("a batch of the window's"));
    }
    out
}

/// The asks the window's editors were sent to show a block (`reveal`).
fn reveals(messages: &[ToUi]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            ToUi::Patch { patches, .. } => Some(patches),
            _ => None,
        })
        .flatten()
        .filter_map(|patch| match patch {
            ui::Patch::SetProp {
                name,
                value: Some(ui::Value::Opt(Some(ask))),
                ..
            } if name == "reveal" => match ask.as_ref() {
                ui::Value::Text(ask) => Some(ask.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A note Ada writes, titled `title`, of the paragraphs `paragraphs`.
fn note(host: &Host, title: &str, paragraphs: &[&str]) -> ResourceRef {
    let notebook = host.root_of(APP).expect("the notebook");
    match call(
        host,
        "new_note",
        vec![
            Value::resource(&notebook),
            Value::text(title),
            Value::text(paragraphs.join("\n\n")),
        ],
    ) {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("a note: {other:?}"),
    }
}

/// Ada's note "Offsite" of three paragraphs, the last about the budget, beside notes that say
/// it in one paragraph and in a paragraph of another.
fn offsite(host: &Host) -> ResourceRef {
    note(host, "Budget", &["Bread and milk"]);
    note(
        host,
        "Groceries",
        &["Bread and milk", "The budget is for bread."],
    );
    note(
        host,
        "Offsite",
        &[
            "We meet on the Thursday.",
            "Lunch is at the harbour.",
            "The budget covers two nights.",
        ],
    )
}

/// What system search lists for `needle` of what `host` publishes to the session's index, kept
/// and searched as the session does.
fn search(host: &Host, needle: &str) -> Vec<tilo_desktop_search::Hit> {
    let index = AppContentIndex::in_memory();
    let published = tilolang_tilo::search::publish_whole(
        Publisher::default(),
        IndexState {
            generation: 1,
            secrets_unlocked: None,
        },
        tilolang_tilo::search::whole(host),
    );
    for delta in &published.deltas {
        index
            .publish(WORKSPACE, APP, "notebook", delta)
            .expect("the index takes what the window publishes");
    }
    IndexedContent::new(Arc::new(index), str::to_owned, "ada").query(needle)
}

#[test]
fn a_paragraph_search_finds_opens_its_note_at_that_paragraph() {
    let (mut window, mut window_end) = window();
    let host = window.host().expect("the window opened");
    let note = offsite(&host);
    window.update();
    window.sync();
    sent(&mut window_end);

    let hits = search(&host, "budget");
    let paragraphs: Vec<_> = hits
        .iter()
        .filter(|hit| hit.subtitle.as_deref() == Some("Offsite"))
        .collect();
    assert_eq!(
        paragraphs
            .iter()
            .map(|hit| hit.title.as_str())
            .collect::<Vec<_>>(),
        ["The budget covers two nights."],
        "the paragraph that says it is one row, said to be in its note: {hits:#?}"
    );
    assert!(
        hits.iter().any(|hit| hit.title == "Budget")
            && !hits
                .iter()
                .any(|hit| hit.subtitle.as_deref() == Some("Budget")),
        "a note of one paragraph is found as itself, with no row for its paragraph: {hits:#?}"
    );

    // Choosing it opens the note at that paragraph.
    let Target::Invoke(open) = &paragraphs[0].target else {
        panic!(
            "a paragraph's row opens its note: {:?}",
            paragraphs[0].target
        );
    };
    assert_eq!(open.address.action, "open");
    let anchor = open
        .arguments
        .get(OPEN_ANCHOR)
        .and_then(serde_json::Value::as_str)
        .expect("the row names what it opens")
        .to_owned();
    window
        .open_requests()
        .borrow_mut()
        .push(OpenEffect::Open { anchor });
    window.update();
    window.sync();
    let told = sent(&mut window_end);
    let Value::Rich(body) = host.runtime().read(&note, "body").expect("the note's body") else {
        panic!("a note's body is rich text");
    };
    let third = body
        .blocks
        .iter()
        .position(|block| block.text.contains("budget"))
        .expect("the paragraph is in the note");
    let reveals = reveals(&told);
    assert_eq!(
        reveals.len(),
        1,
        "the note's editor is asked to show one block: {reveals:?}"
    );
    assert!(
        reveals[0].starts_with(&format!("¶{third}:")),
        "the editor shows the paragraph the row named: {reveals:?}"
    );
    assert!(
        window
            .root_tree()
            .is_some_and(|tree| format!("{tree:?}").contains("Offsite")),
        "the window shows the note"
    );
}
