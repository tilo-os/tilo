use super::*;
use crate::prelude::*;
use std::cell::RefCell;
use tilo_resource::ScopeDecl;
use ui_core::search::{PagePoll, SearchQuery};

thread_local! {
    static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[derive(Model, Default)]
pub struct Mailbox {
    pub subject: String,
}

fn entries(_: &Mailbox) -> Vec<SearchEntry> {
    vec![
        SearchEntry {
            anchor: Some("folder:inbox/msg:1".into()),
            title: "Invoice from Ada".into(),
            text: "March".into(),
            ..Default::default()
        },
        SearchEntry {
            anchor: Some("folder:sent/msg:2".into()),
            title: "Invoice to Grace".into(),
            text: "April".into(),
            ..Default::default()
        },
        SearchEntry {
            anchor: Some("msg:3".into()),
            title: "Invoice reminder".into(),
            text: "May".into(),
            within: Some("folder:inbox".into()),
            row: Some(
                tilo_resource::SearchRow::shaped(tilo_resource::SearchRowShape::Message {
                    sender: "Ada".into(),
                    snippet: None,
                    date: None,
                    unread: true,
                })
                .icon("mail"),
            ),
            ..Default::default()
        },
    ]
}

fn scopes(_: &Mailbox) -> Vec<ScopeDecl> {
    vec![ScopeDecl::new("folder:inbox", "Inbox")]
}

#[resource(kind = "test.mailbox", label = "Mailbox", search = entries, scopes = scopes)]
impl Mailbox {
    /// Set the subject.
    #[action]
    pub fn set_subject(&mut self, subject: String) {
        self.subject = subject;
    }
}

struct MailApp;

impl ResourceApp for MailApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-mailbox";
    const TITLE: &'static str = "Mailbox";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.on_open(|anchor| OPENED.with_borrow_mut(|opened| opened.push(anchor.to_owned())));
        Box::new(Column::new().push(Box::new(
            cx.search_button(ui_core::search::SearchRequest::this_app()),
        )))
    }
}

thread_local! {
    static QUERIES: RefCell<Vec<SearchQuery>> = const { RefCell::new(Vec::new()) };
}

/// A view looking queries up beside its content.
struct ViewQueryApp;
impl ResourceApp for ViewQueryApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-search-query";
    const TITLE: &'static str = "Query";
    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.publishing(
            crate::framework::Publishes::new().search_query(|query| {
                QUERIES.with_borrow_mut(|queries| queries.push(query.clone()));
            }),
            Box::new(ui_core::widget::Text::new("Results")),
        )
    }
}

#[test]
fn platform_search_delivers_only_the_latest_bounded_query_to_a_view() {
    QUERIES.with_borrow_mut(Vec::clear);
    let mut app = TestApp::<ViewQueryApp>::new().unwrap();
    QUERIES.with_borrow_mut(Vec::clear);
    app.harness()
        .with_widget_mut(|root: &mut ResourceRoot| {
            let providers = RootSearch::search_providers(root);
            assert_eq!(providers.len(), 1, "one canonical resource source");
            let search = &providers[0];
            search.search("old");
            let query = SearchQuery::new("invoice").within("folder:inbox");
            search.search_page(&query, None, 10, 10);
            search.search_page(&query, None, 10, 10);
            search.search(&"x".repeat(ui_core::search::MAX_SEARCH_QUERY_SCALARS + 1));
            assert!(
                QUERIES.with_borrow(Vec::is_empty),
                "delivery waits for UI sync"
            );
        })
        .unwrap();
    app.settle();
    QUERIES.with_borrow(|queries| {
        assert_eq!(
            queries,
            &[SearchQuery::new("invoice").within("folder:inbox")]
        );
    });
    app.settle();
    assert_eq!(
        QUERIES.with_borrow(Vec::len),
        1,
        "a settled query is not repeated"
    );
}

thread_local! {
    static LOOKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FOUND: RefCell<Vec<SearchEntry>> = const { RefCell::new(Vec::new()) };
}

/// A view asking a server what the search asks, which answers later.
struct ViewServerApp;
impl ResourceApp for ViewServerApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-server-search";
    const TITLE: &'static str = "Server";
    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.publishing(
            crate::framework::Publishes::new()
                .search_query(|_| LOOKING.with(|looking| looking.set(true)))
                .search_pending(|| LOOKING.with(std::cell::Cell::get))
                .search_entries(|| FOUND.with_borrow(Clone::clone)),
            Box::new(ui_core::widget::Text::new("Results")),
        )
    }
}

#[test]
fn a_showing_search_waits_for_what_a_views_server_finds() {
    LOOKING.with(|looking| looking.set(false));
    FOUND.with_borrow_mut(Vec::clear);
    let mut app = TestApp::<ViewServerApp>::new().unwrap();
    let query = SearchQuery::new("archive");
    let (search, first) = app
        .harness()
        .with_widget_mut(|root: &mut ResourceRoot| {
            let mut providers = RootSearch::search_providers(root);
            let search = providers.remove(0);
            let first = search.search_page(&query, None, 10, 10);
            (search, first)
        })
        .unwrap();
    assert!(first.results.is_empty(), "nothing held matches yet");
    let token = first.pending.expect("the view has yet to answer");
    assert!(
        matches!(search.poll_page(&token), PagePoll::Pending),
        "not yet asked"
    );
    app.settle();
    assert!(LOOKING.with(std::cell::Cell::get), "the server was asked");
    assert!(
        matches!(search.poll_page(&token), PagePoll::Pending),
        "the server is still looking"
    );

    FOUND.with_borrow_mut(|found| {
        found.push(SearchEntry::new("From the archive", "an old letter").anchored("msg:9"));
    });
    LOOKING.with(|looking| looking.set(false));
    app.settle();
    let PagePoll::Answered(answered) = search.poll_page(&token) else {
        panic!("the server answered");
    };
    assert!(answered.pending.is_none(), "nothing more is coming");
    let titles: Vec<_> = answered
        .results
        .iter()
        .map(|hit| hit.title.as_str())
        .collect();
    assert_eq!(titles, ["From the archive"], "what it found shows");
    assert_eq!(
        search.search_page(&query, None, 10, 10).results.len(),
        1,
        "and stays found for the same search"
    );
}

#[test]
fn a_hit_opens_the_app_at_its_anchor_and_scopes_narrow_the_search() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    let (titles, mut scoped, declared, reminder) = app
        .harness()
        .with_widget_mut(|root: &mut ResourceRoot| {
            let providers = RootSearch::search_providers(root);
            let search = providers.first().unwrap();
            let titles = |query: &SearchQuery| {
                search
                    .search_page(query, None, 10, 10)
                    .results
                    .into_iter()
                    .map(|hit| hit.title)
                    .collect::<Vec<_>>()
            };
            let reminder = search
                .search_page(&SearchQuery::new("reminder"), None, 10, 10)
                .results
                .into_iter()
                .next();
            (
                titles(&SearchQuery::new("invoice")),
                titles(&SearchQuery::new("invoice").within("folder:inbox")),
                search.search_scopes(),
                reminder,
            )
        })
        .unwrap();
    assert_eq!(titles.len(), 3);
    scoped.sort();
    assert_eq!(scoped, ["Invoice from Ada", "Invoice reminder"]);
    let reminder = reminder.expect("the flat-id entry is found");
    assert_eq!(reminder.anchor(), "folder:inbox");
    assert_eq!(reminder.icon, Some(ui_core::icons::Icon::Mail));
    assert!(matches!(
        reminder.row,
        Some(ui_core::search::SearchRow::Message { unread: true, .. })
    ));
    assert_eq!(declared, [ScopeDecl::new("folder:inbox", "Inbox")]);

    app.harness()
        .with_widget_mut(|root: &mut ResourceRoot| {
            let mut doc = ui_core::automerge::AutoCommit::new();
            RootSearch::activate_search_result(
                root,
                "folder:inbox/msg:1",
                &mut ui_core::widget::Edits::lend(&mut doc),
                &ui_core::automerge::ROOT,
            );
        })
        .unwrap();
    OPENED.with_borrow(|opened| assert_eq!(opened.as_slice(), ["folder:inbox/msg:1"]));

    app.call("open", serde_json::json!({ "anchor": "folder:sent/msg:2" }))
        .unwrap();
    OPENED.with_borrow(|opened| {
        assert_eq!(opened.last().map(String::as_str), Some("folder:sent/msg:2"));
    });
}

/// How many indexes of its entries `app`'s resource has started.
fn index_builds<A: ResourceApp>(app: &mut TestApp<A>) -> usize {
    app.harness()
        .with_widget_mut(|root: &mut ResourceRoot| root.search.borrow().builds)
        .unwrap()
}

#[test]
fn the_action_directory_names_the_search_source_without_indexing_it() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    let mut named = Vec::new();
    for edit in 0..4 {
        app.invoke(Mailbox::SET_SUBJECT.with(format!("Draft {edit}")))
            .unwrap();
        let (first, again) = app
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                (
                    RootSearch::search_provider_descriptors(root),
                    RootSearch::search_provider_descriptors(root),
                )
            })
            .unwrap();
        assert_eq!(first, again, "an unchanged resource names the same build");
        named.push(first);
    }
    assert_eq!(
        index_builds(&mut app),
        0,
        "nobody searched: nothing indexed"
    );
    assert!(
        named.windows(2).all(|pair| pair[0] != pair[1]),
        "every revision names a new build, so the directory moves with it"
    );

    let (hits, searched, after) = app
        .harness()
        .with_widget_mut(|root: &mut ResourceRoot| {
            let providers = RootSearch::search_providers(root);
            let hits = providers[0].search("invoice").len();
            (
                hits,
                ui_core::search::capability::SearchProviderDescriptor::of(providers[0].as_ref()),
                RootSearch::search_provider_descriptors(root),
            )
        })
        .unwrap();
    assert_eq!(hits, 3);
    assert_eq!(index_builds(&mut app), 1, "a search builds the index once");
    assert_eq!(
        named.last(),
        Some(&vec![searched]),
        "the directory named the build the search answers from"
    );
    assert_eq!(named.last(), Some(&after), "and searching did not move it");
    app.harness()
        .with_widget_mut(|root: &mut ResourceRoot| {
            RootSearch::search_providers(root)[0].search("invoice");
        })
        .unwrap();
    assert_eq!(
        index_builds(&mut app),
        1,
        "an unchanged resource is not reindexed"
    );
}

/// Prose a book's entry carries, which sends its index off the UI thread.
struct Prose(&'static str);

impl tilo_resource::SearchText for Prose {
    fn segment_count(&self) -> usize {
        1
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment == 0)
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<tilo_resource::SearchPiece<'_>> {
        (segment == 0 && piece == 0).then_some(tilo_resource::SearchPiece {
            text: self.0,
            space_before: false,
        })
    }
}

#[derive(Model, Default)]
pub struct Shelf {
    pub title: String,
}

fn shelf_entries(shelf: &Shelf) -> Vec<SearchEntry> {
    vec![
        SearchEntry::new(shelf.title.clone(), "").with_body(tilo_resource::SearchBody::new(
            Arc::new(Prose("The lighthouse keeper counted the waves.")),
        )),
    ]
}

#[resource(kind = "test.shelf", label = "Shelf", search = shelf_entries)]
impl Shelf {
    /// Retitle the book.
    #[action]
    pub fn retitle(&mut self, title: String) {
        self.title = title;
    }
}

struct ShelfApp;

impl ResourceApp for ShelfApp {
    type Resource = Shelf;
    const ID: &'static str = "so.tilo.test-shelf";
    const TITLE: &'static str = "Shelf";

    fn view(_: &mut ViewCx<Shelf>) -> Element {
        Box::new(Column::new())
    }
}

#[test]
fn edits_while_an_index_builds_cost_one_build_after_it() {
    let pumped = ui_core::background::Pumped::new();
    let search = |app: &mut TestApp<ShelfApp>| {
        let _background = pumped.background().enter();
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                RootSearch::search_providers(root)[0]
                    .search("storm")
                    .into_iter()
                    .map(|hit| hit.title)
                    .collect::<Vec<_>>()
            })
            .unwrap()
    };
    let mut app = TestApp::<ShelfApp>::new().unwrap();
    app.invoke(Shelf::RETITLE.with("Storm Tales")).unwrap();
    assert!(
        search(&mut app).is_empty(),
        "a book is indexed off the UI thread"
    );
    assert_eq!(index_builds(&mut app), 1);
    for edit in 0..5 {
        app.invoke(Shelf::RETITLE.with(format!("Storm Tales {edit}")))
            .unwrap();
        assert!(
            search(&mut app).is_empty(),
            "until it arrives, search answers from the index before it"
        );
    }
    assert_eq!(
        index_builds(&mut app),
        1,
        "edits while a build is in flight start no build beside it"
    );

    pumped.run_pending();
    assert!(search(&mut app).contains(&"Storm Tales".to_owned()));
    assert_eq!(
        index_builds(&mut app),
        2,
        "once it arrives, the newest state alone is built"
    );
    pumped.run_pending();
    assert!(search(&mut app).contains(&"Storm Tales 4".to_owned()));
    assert_eq!(index_builds(&mut app), 2);
}
