//! An app each of whose windows shows one item of the workspace's shared library (`document:
//! shelf.books`): a search hit on a page of a book opens the book, told the page, and the
//! window showing the book turns to that page as a press of the page's row does.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::model::{OPEN_ANCHOR, Source as _, Target};
use tilo_render_protocol::patch::local::pair;
use tilo_resource::search_index::{IndexState, Publisher};
use tilo_workspace_search::{AppContentIndex, IndexSearch};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::publish::resource_id;
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

const APP: &str = "t.shelf";
const WORKSPACE: &str = "workspace";

/// A shelf of books, each window showing one book, its pages down the window.
const SOURCE: &str = r#"app t.shelf "Shelf" (
    document: shelf.books,
)

/// A book of pages.
type Book (title: name) {
    name: Text
    pages: Collection<Leaf>
    reading: Option<Leaf>
}

/// One page of a book, found by its words.
type Leaf (title: words) {
    words: Text
}

/// The workspace's books.
type Shelf {
    books: Collection<Book>
}

/// Puts a book on the shelf.
action add_book(s: Shelf, name: Text) -> Book {
    s.books.insert(Book(name: name))
}

/// Writes a page at the end of a book.
action add_leaf(b: Book, words: Text) -> Leaf {
    b.pages.insert(Leaf(words: words))
}

/// The page the reader of a book is on.
action turn(b: Book, l: Leaf) {
    b.reading = .some(l)
}

root shelf: Shelf

default view shelf for Shelf

view shelf(s: Shelf) {
    page { text("{s.books.len()}") }
}

default view book for Book

view book(b: Book) {
    page {
        ui.VirtualList(fill) {
            on select(l: Leaf): turn(b, l)
            for l in b.pages {
                ui.ListItem(item: l) {
                    text(l.words)
                }
            }
        }
    }
}
"#;

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(17),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

struct Shelf;

impl Platform for Shelf {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-24T09:00:00Z".to_owned()
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

/// Ada's window of the shelf, over a store of its own.
fn window() -> Instance {
    let (app_end, _window) = pair();
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
        Rc::new(Shelf),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    instance
}

/// What the shelf's action `action` answered, run by Ada.
fn call(host: &Host, action: &str, args: Vec<Value>) -> ResourceRef {
    let id = host
        .package(APP)
        .expect("the shelf package")
        .meta
        .actions
        .iter()
        .find(|held| held.name == action)
        .unwrap_or_else(|| panic!("the action `{action}`"))
        .id
        .clone();
    let ada = Actor::from(MemberId::person("ada"));
    match host.runtime().invoke_action(&ada, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(Value::Ref(made))) => (*made.target).clone(),
        other => panic!("`{action}` answered {other:?}"),
    }
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
            .publish(WORKSPACE, APP, "shelf", delta)
            .expect("the index takes what the window publishes");
    }
    let search = IndexSearch::new(
        Arc::new(index),
        tilo_workspace_search::deciding(|_, _| true),
    );
    IndexedContent::new(
        Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        str::to_owned,
        "ada",
        Arc::new(|| {}),
    )
    .query(needle)
}

/// Ask the window to show `anchor`, as an `open` of it does, and let it draw.
fn open(window: &mut Instance, anchor: &str) {
    window.open_requests().borrow_mut().push(OpenEffect::Open {
        anchor: anchor.to_owned(),
    });
    for _ in 0..4 {
        window.update();
        window.sync();
    }
}

#[test]
fn a_hit_on_a_page_opens_its_book_turned_to_the_page() {
    let mut window = window();
    let host = window.host().expect("the window opened");
    let shelf = host.root_of(APP).expect("the shelf");
    let atlas = call(
        &host,
        "add_book",
        vec![Value::resource(&shelf), Value::text("Atlas")],
    );
    let opening = call(
        &host,
        "add_leaf",
        vec![Value::resource(&atlas), Value::text("An opening page")],
    );
    let harbour = call(
        &host,
        "add_leaf",
        vec![Value::resource(&atlas), Value::text("The harbour route")],
    );
    call(
        &host,
        "add_book",
        vec![Value::resource(&shelf), Value::text("Almanac")],
    );
    window.sync();
    let book = resource_id(APP, &atlas.id_string());
    let page = resource_id(APP, &harbour.id_string());

    // The hit on the page's words is an `open` of the book it is in, told the page: the book is
    // the item a window shows, so the session opens the book's window, at the page.
    let hits = search(&host, "harbour");
    let hit = hits
        .iter()
        .find(|hit| hit.title.contains("harbour"))
        .unwrap_or_else(|| panic!("the page is found: {hits:#?}"));
    let Target::Invoke(chosen) = &hit.target else {
        panic!("the hit opens its book: {:?}", hit.target);
    };
    assert_eq!(chosen.address.action, "open");
    assert_eq!(
        chosen.address.resource, book,
        "the hit opens the book it is in"
    );
    assert_eq!(
        chosen
            .arguments
            .get(OPEN_ANCHOR)
            .and_then(|anchor| anchor.as_str()),
        Some(page.as_str()),
        "told the page"
    );
    // A hit on the book itself opens the book.
    let titles = search(&host, "Atlas");
    let found = titles
        .iter()
        .find(|hit| hit.title == "Atlas")
        .unwrap_or_else(|| panic!("the book is found: {titles:#?}"));
    let Target::Invoke(chosen) = &found.target else {
        panic!("the hit opens the book: {:?}", found.target);
    };
    assert_eq!(chosen.address.resource, book);

    // The window on the book turns to the page the hit names, as a press of its row does, and
    // a hit on another page turns it there.
    open(&mut window, &book);
    open(&mut window, &page);
    assert_eq!(
        host.runtime().read(&atlas, "reading").unwrap(),
        Value::some(Value::resource(&harbour)),
        "the window turned to the page the hit names"
    );
    open(&mut window, &resource_id(APP, &opening.id_string()));
    assert_eq!(
        host.runtime().read(&atlas, "reading").unwrap(),
        Value::some(Value::resource(&opening)),
        "and to the next one asked for"
    );
}
