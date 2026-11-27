//! A search scoped to a Typst document finds the lines of its source, and choosing one shows
//! that line in the editor; a line of another document is not in its scope.

use std::rc::Rc;
use std::sync::Arc;

use crate::window_reads::reveals;
use crate::window_reads::sent;
use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::model::{OPEN_ANCHOR, Source as _, Target};
use tilo_render_protocol::patch as ui;
use tilo_render_protocol::patch::local::{End, pair};
use tilo_resource::search_index::{IndexState, Publisher};
use tilo_workspace_search::{AppContentIndex, IndexSearch};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

const TYPST: (&str, &str) = (
    include_str!("../../../../apps/tl/typst-editor/app.tl"),
    include_str!("../../../../apps/tl/typst-editor/app.ids"),
);
const APP: &str = "so.tilo.typst-editor";
const WORKSPACE: &str = "workspace";
const ROUTE: &str = "= The route\n\nThe figure shows the way.\n";
const NOTES: &str = "The harbour is quiet.\nAnother line.\n";

struct Typst;

impl Platform for Typst {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(TYPST.0, TYPST.1).unwrap();
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

/// Ada's Typst window, and the window's end of what it sends.
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
        Rc::new(Typst),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    (instance, window_end)
}

/// What the typst action `action` answered, run by Ada.
fn call(host: &Host, action: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the typst package")
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

/// The document Ada makes from `name` and `source`.
fn document(host: &Host, name: &str, source: &str) -> ResourceRef {
    let lib = host.root_of(APP).expect("the library");
    match call(
        host,
        "new_document",
        vec![
            Value::resource(&lib),
            Value::text(name),
            Value::text(source),
        ],
    ) {
        Value::Ref(made) => (*made.target).clone(),
        other => panic!("a document: {other:?}"),
    }
}

/// What system search lists for `needle` of what `host` publishes to the session's index, kept
/// and searched as the session does, narrowed to `within` when it names the resource a window's
/// search button scopes to.
fn search(host: &Host, needle: &str, within: Option<String>) -> Vec<tilo_desktop_search::Hit> {
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
            .publish(WORKSPACE, APP, "library", delta)
            .expect("the index takes what the window publishes");
    }
    let search = IndexSearch::new(
        Arc::new(index),
        tilo_workspace_search::deciding(|_, _| true),
    );
    let content = IndexedContent::new(
        Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        str::to_owned,
        "ada",
        Arc::new(|| {}),
    );
    match within {
        Some(anchor) => content.for_app(APP, Some(anchor)),
        None => content,
    }
    .query(needle)
}

#[test]
fn a_search_inside_one_document_finds_its_lines_and_shows_the_one_chosen() {
    let (mut window, mut window_end) = window();
    let host = window.host().expect("the window opened");
    let route = document(&host, "route.typ", ROUTE);
    document(&host, "notes.typ", NOTES);
    window.update();
    window.sync();
    sent(&mut window_end);

    // The document a window shows is the scope its search button opens on: what the index
    // publishes for it is the address every line of its source lives in.
    let scope = tilolang_tilo::naming::resource_id(APP, &route.id_string());
    let hits = search(&host, "figure", Some(scope.clone()));
    let line = hits
        .iter()
        .find(|hit| hit.title == "The figure shows the way.")
        .unwrap_or_else(|| panic!("the line that says it is a row: {hits:#?}"));
    assert!(
        !hits.iter().any(|hit| hit.title.contains("harbour")),
        "a line of another document is not in this one's scope: {hits:#?}"
    );

    // Choosing it opens the document at that line, not at its start.
    let Target::Invoke(open) = &line.target else {
        panic!("a line's row opens its document: {:?}", line.target);
    };
    assert_eq!(open.address.action, "open");
    let anchor = open
        .arguments
        .get(OPEN_ANCHOR)
        .and_then(serde_json::Value::as_str)
        .expect("the row names what it opens")
        .to_owned();
    assert!(
        anchor.contains('#'),
        "the hit is anchored at the line, not the document: {anchor}"
    );
    assert_eq!(
        search(&host, "harbour", Some(scope)).len(),
        0,
        "a search inside this document finds nothing of another one"
    );

    window
        .open_requests()
        .borrow_mut()
        .push(OpenEffect::Open { anchor });
    let mut told = Vec::new();
    for _ in 0..4 {
        window.update();
        window.sync();
        told.extend(reveals(&sent(&mut window_end)));
        if !told.is_empty() {
            break;
        }
    }
    assert_eq!(
        told,
        ["3:1:1"],
        "the code editor shows the line the hit named"
    );
}

#[test]
fn the_code_editor_is_an_editor_a_place_is_revealed_in() {
    let described = ui::description("ui.CodeEditor").expect("the code editor is described");
    assert!(
        described.prop("reveal").is_some(),
        "search tells a code editor the line a hit found: {described:?}"
    );
}
