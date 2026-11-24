//! A search scoped to a deck finds its slides, their speaker notes and their text boxes, and
//! choosing one opens the deck on the slide it found, the notes editor bound to that slide.

use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::model::{OPEN_ANCHOR, Source as _, Target};
use tilo_render_protocol::patch::local::{End, pair};
use tilo_render_protocol::patch::{self as ui, ToUi, Transport as _};
use tilo_resource::search_index::{IndexState, Publisher};
use tilo_workspace_service::search::{AppContentIndex, IndexSearch};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

const SLIDES: (&str, &str) = (
    include_str!("../../../../apps/tl/slides/app.tl"),
    include_str!("../../../../apps/tl/slides/app.ids"),
);
const APP: &str = "so.tilo.slides";
const WORKSPACE: &str = "workspace";
const OUTLINE: &str = r#"[
  {"title": "The route", "body": "Lunch is at the harbour."},
  {"title": "Budget", "notes": "Bread and milk.\n\nThe budget is for bread."}
]"#;

struct Slides;

impl Platform for Slides {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SLIDES.0, SLIDES.1).unwrap();
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

/// Ada's Slides window, and the window's end of what it sends.
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
        Rc::new(Slides),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    (instance, window_end)
}

/// What a slides action answered, run by Ada.
fn call(host: &Host, action: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the slides package")
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

/// The places the window's editors were sent to show (`reveal`), each as the editor reads it.
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

/// The resource the window's editor of the `field` of a resource is bound to, as the tree it
/// was sent holds it: the published id of each slide whose notes an editor edits.
fn notes_bound_to(host: &Host, tree: &tilolang_runtime::UiNode) -> Vec<String> {
    let field = host
        .package(APP)
        .expect("the slides package")
        .meta
        .types
        .iter()
        .find(|declared| declared.name == "Slide")
        .expect("the Slide type")
        .fields
        .iter()
        .find(|field| field.name == "notes")
        .expect("a slide's notes field")
        .id
        .clone();
    let mut out = Vec::new();
    let mut pending = vec![tree];
    while let Some(node) = pending.pop() {
        if let Some((target, name)) = node.binding.as_ref().and_then(|binding| binding.field())
            && *name == field
        {
            out.push(tilolang_tilo::publish::resource_id(APP, &target.id_string()));
        }
        pending.extend(&node.children);
    }
    out
}

/// The deck Ada's window shows, and the slides it holds after the outline's two.
fn deck(host: &Host) -> (ResourceRef, Vec<ResourceRef>) {
    let deck = host.root_of(APP).expect("the deck");
    call(
        host,
        "add_slides",
        vec![Value::resource(&deck), Value::text(OUTLINE), Value::none()],
    );
    let slides = match host.runtime().read(&deck, "slides").expect("the deck's slides") {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(held) => Some((*held.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("a deck's slides are a list: {other:?}"),
    };
    (deck, slides)
}

/// The slide of `slides` whose speaker notes say `words`.
fn slide_saying(host: &Host, slides: &[ResourceRef], words: &str) -> ResourceRef {
    slides
        .iter()
        .find(|slide| match host.runtime().read(slide, "notes") {
            Ok(Value::Rich(rich)) => rich.written_text().contains(words),
            _ => false,
        })
        .cloned()
        .unwrap_or_else(|| panic!("a slide whose notes say {words}"))
}

/// The slide of `slides` that holds a text box saying `words`.
fn slide_holding(host: &Host, slides: &[ResourceRef], words: &str) -> ResourceRef {
    slides
        .iter()
        .find(|slide| match host.runtime().read(slide, "elements") {
            Ok(Value::List(items)) => items.iter().any(|item| match item {
                Value::Ref(element) => match host.runtime().read(&element.target, "body") {
                    Ok(Value::Rich(rich)) => rich.written_text().contains(words),
                    _ => false,
                },
                _ => false,
            }),
            _ => false,
        })
        .cloned()
        .unwrap_or_else(|| panic!("a slide holding a text box saying {words}"))
}

/// What system search lists for `needle` of what `host` publishes to the session's index, kept
/// and searched as the session does, narrowed to the deck a window's search button scopes to.
fn search(host: &Host, needle: &str, deck: &ResourceRef) -> Vec<tilo_desktop_search::Hit> {
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
            .publish(WORKSPACE, APP, "deck", delta)
            .expect("the index takes what the window publishes");
    }
    let search = IndexSearch::new(Arc::new(index), Arc::new(|_, _| true));
    let content = IndexedContent::new(
        Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        str::to_owned,
        "ada",
        Arc::new(|| {}),
    );
    let scope = tilolang_tilo::publish::resource_id(APP, &deck.id_string());
    content.for_app(APP, Some(scope)).query(needle)
}

/// The anchor a hit opens, and the place its row stands.
fn anchor_of(hit: &tilo_desktop_search::Hit) -> String {
    let Target::Invoke(open) = &hit.target else {
        panic!("a hit opens its deck: {:?}", hit.target);
    };
    assert_eq!(open.address.action, "open");
    open.arguments
        .get(OPEN_ANCHOR)
        .and_then(serde_json::Value::as_str)
        .expect("the row names what it opens")
        .to_owned()
}

/// What the window was told to show in the settle after `anchor` was opened.
fn open(window: &mut Instance, window_end: &mut End, anchor: &str) -> Vec<String> {
    window
        .open_requests()
        .borrow_mut()
        .push(OpenEffect::Open {
            anchor: anchor.to_owned(),
        });
    let mut asked = Vec::new();
    for _ in 0..4 {
        window.update();
        window.sync();
        asked.extend(reveals(&sent(window_end)));
        if !asked.is_empty() {
            break;
        }
    }
    asked
}

#[test]
fn a_search_inside_one_deck_finds_its_slides_and_opens_the_one_chosen() {
    let (mut window, mut window_end) = window();
    let host = window.host().expect("the window opened");
    let (deck, slides) = deck(&host);
    window.update();
    window.sync();
    sent(&mut window_end);
    let budget = slide_saying(&host, &slides, "budget");

    // The speaker notes' paragraph is a row of its own, anchored at the slide it is on.
    let hits = search(&host, "budget", &deck);
    let paragraph = hits
        .iter()
        .find(|hit| hit.title == "The budget is for bread.")
        .unwrap_or_else(|| panic!("the note's paragraph is a row: {hits:#?}"));
    let anchor = anchor_of(paragraph);
    assert!(
        anchor.contains('#'),
        "the hit is anchored at the paragraph, not the slide: {anchor}"
    );
    assert!(
        !hits
            .iter()
            .any(|hit| hit.title.contains("harbour")),
        "a text box of another slide is not this sentence: {hits:#?}"
    );

    // Choosing it opens the deck on the slide the paragraph is on, the notes editor bound to
    // that slide's notes, showing the paragraph.
    let told = open(&mut window, &mut window_end, &anchor);
    let notes = match host.runtime().read(&budget, "notes").expect("the notes") {
        Value::Rich(rich) => rich,
        other => panic!("a slide's notes are rich text: {other:?}"),
    };
    let place = notes
        .blocks
        .iter()
        .position(|block| block.text.contains("budget"))
        .expect("the paragraph is in the notes");
    assert_eq!(
        told,
        [format!("¶{place}:1")],
        "the notes editor shows the paragraph the hit named"
    );
    let budget_id = tilolang_tilo::publish::resource_id(APP, &budget.id_string());
    assert_eq!(
        notes_bound_to(&host, &window.root_tree().expect("the window's tree")),
        [budget_id],
        "the notes editor is bound to the slide the hit opened, not the one shown before"
    );
}

#[test]
fn a_text_box_is_found_and_opening_it_shows_its_slide() {
    let (mut window, mut window_end) = window();
    let host = window.host().expect("the window opened");
    let (deck, slides) = deck(&host);
    window.update();
    window.sync();
    sent(&mut window_end);
    let route = slide_holding(&host, &slides, "harbour");

    let hits = search(&host, "harbour", &deck);
    let element = hits
        .iter()
        .find(|hit| hit.title == "Lunch is at the harbour.")
        .unwrap_or_else(|| panic!("the text box is a row: {hits:#?}"));
    let anchor = anchor_of(element);
    assert!(
        !anchor.contains('#'),
        "the hit is anchored at the text box itself: {anchor}"
    );

    open(&mut window, &mut window_end, &anchor);
    let route_id = tilolang_tilo::publish::resource_id(APP, &route.id_string());
    assert_eq!(
        notes_bound_to(&host, &window.root_tree().expect("the window's tree")),
        [route_id],
        "opening the text box shows the slide it stands on"
    );
}
