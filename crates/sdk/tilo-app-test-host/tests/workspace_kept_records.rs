//! A record one app keeps for every app of the workspace
//! (`collab_types::WORKSPACE_DOCUMENTS`): one record in the workspace for
//! every member, not one per person.

use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;
use tilo_permissions::Role;

/// The app keeping the record: Browser, whose bookmarks the workspace keeps.
const BROWSER: &str = "so.tilo.browser";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub items: Vec<String>,
}

#[resource(
    kind = "test.shelf",
    app = "so.tilo.browser",
    catalogue = "tests/locales"
)]
impl Shelf {}

/// What the workspace bookmarked.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bookmarks {
    pub pages: Collection<String>,
}

#[resource(
    kind = "browser.personal-bookmarks",
    app = "so.tilo.browser",
    catalogue = "tests/locales"
)]
impl Bookmarks {
    /// Keep a page.
    #[action]
    pub fn keep(&mut self, cx: &mut ActionCx, page: String) {
        self.pages.push(cx.id(), page);
    }
}

impl tilo_app::tilo_resource::SharedResource for Bookmarks {
    const OWNER: &'static str = BROWSER;
}

struct BrowserApp;

impl ResourceApp for BrowserApp {
    type Resource = Shelf;
    const ID: &'static str = BROWSER;
    const TITLE: &'static str = "Browser";
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::shared::<Bookmarks>()];

    fn view(_cx: &mut ViewCx<Shelf>) -> Element {
        Box::new(Column::new())
    }
}

fn ada() -> String {
    "ad".repeat(32)
}

fn bob() -> String {
    "b0".repeat(32)
}

fn pages(record: &WorkspaceResource<Bookmarks>) -> Vec<String> {
    let mut pages: Vec<String> = record.with(|state| state.pages.values().cloned().collect());
    pages.sort();
    pages
}

tilo_test_env::scenario! {
    fn a_record_the_workspace_keeps_is_one_record_for_every_member;

    phase two_members_open_the_same_record_of_the_workspace;
    {
        let mut adas = TestApp::<BrowserApp>::for_person(&ada()).unwrap();
        let mut bobs = TestApp::<BrowserApp>::for_person_with_role(&bob(), Role::Editor).unwrap();
        let on_adas = adas.shared_workspace::<Bookmarks>(BROWSER).unwrap();
        let on_bobs = bobs.shared_workspace::<Bookmarks>(BROWSER).unwrap();
        let route = on_adas.record().route().expect("the workspace's record syncs");
        assert_eq!(route, on_bobs.record().route().unwrap(), "one record for both of them");
        assert_eq!(route.app_id, BROWSER);
        assert_eq!(
            route.cluster_id.to_string(),
            tilo_app::framework::headless::TEST_WORKSPACE,
            "it is the workspace's, not a person's own store's"
        );
        assert_eq!(
            route.document.as_deref(),
            Some(collab_types::workspace_document_id(Bookmarks::KIND).as_str())
        );

        on_adas.invoke(&Bookmarks::KEEP.with("from-ada")).unwrap();
        on_bobs.invoke(&Bookmarks::KEEP.with("from-bob")).unwrap();
        on_bobs
            .record()
            .merge_peer_document(on_adas.record().document().unwrap())
            .unwrap();
        on_adas
            .record()
            .merge_peer_document(on_bobs.record().document().unwrap())
            .unwrap();
        assert_eq!(pages(&on_adas), ["from-ada", "from-bob"]);
        assert_eq!(pages(&on_adas), pages(&on_bobs), "each sees what the other kept");
    }

    phase the_record_is_published_as_the_workspaces;
    {
        let report = tilo_app::framework::headless::surface_report::<BrowserApp>();
        let declared = report["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .find(|resource| resource["kind"] == Bookmarks::KIND)
            .cloned()
            .expect("the app declares the record");
        assert_ne!(
            declared["person"],
            serde_json::json!(true),
            "the workspace keeps it, so it is not the person's own"
        );
    }
}

/// Browser as a window that reads the workspace's bookmarks once, when its view is built.
struct ReadsOnce;

impl ResourceApp for ReadsOnce {
    type Resource = Shelf;
    const ID: &'static str = BROWSER;
    const TITLE: &'static str = "Browser";
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::shared::<Bookmarks>()];

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        let said = match cx.shared_workspace::<Bookmarks>(BROWSER) {
            Some(bookmarks) if pages(&bookmarks).is_empty() => "nothing bookmarked".to_owned(),
            Some(bookmarks) => pages(&bookmarks).join(", "),
            None => "the bookmarks are not here".to_owned(),
        };
        // A field the person types into once the view stands.
        let note = TextInput::from_state(&LocalState::new(String::new())).with_id("note");
        Box::new(
            Column::new()
                .push(Box::new(Text::new(&said)))
                .push(Box::new(note)),
        )
    }
}

/// Whether `app`'s window says `text`, to a screen reader or in what it draws.
fn says(app: &mut TestApp<ReadsOnce>, text: &str) -> bool {
    let read = app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    });
    read || app
        .harness()
        .inspection_json()
        .is_ok_and(|drawn| drawn.contains(text))
}

/// A window opened on a device the workspace's record has not reached yet, another member
/// having made it, waits for it before it shows its view, and shows what arrived: a view built
/// once while the record was on its way would go on reading that the workspace has none, and
/// what the person then kept would stay on this device.
#[test]
fn a_window_shows_the_workspaces_record_once_it_arrived() {
    let mut app = TestApp::<ReadsOnce>::awaiting_the_workspace(
        tilo_app::framework::headless::TEST_WORKSPACE,
        BROWSER,
    )
    .unwrap();
    app.frame();
    assert!(
        !says(&mut app, "the bookmarks are not here"),
        "on their way, the bookmarks are not read as missing"
    );
    assert!(!says(&mut app, "nothing bookmarked"), "nor as none");

    app.workspace_delivers::<Bookmarks>(BROWSER, |bookmarks| {
        bookmarks
            .invoke(&Bookmarks::KEEP.with("from-bob"))
            .map_err(|error| error.to_string())
    })
    .unwrap();
    for _ in 0..4 {
        app.frame();
    }
    assert!(
        says(&mut app, "from-bob"),
        "the window shows what the other member kept, with no reload"
    );
}

/// A window waiting for the workspace's record shows its view once the workspace answers that it
/// holds none of it: the record is new here, empty, and the view reads it as such.
#[test]
fn a_window_shows_the_workspaces_record_empty_once_the_workspace_holds_none() {
    use tilo_app::resource::WorkspaceAnswer;
    let mut app = TestApp::<ReadsOnce>::awaiting_the_workspace(
        tilo_app::framework::headless::TEST_WORKSPACE,
        BROWSER,
    )
    .unwrap();
    app.frame();
    assert!(
        !says(&mut app, "nothing bookmarked"),
        "on their way, not read as none"
    );
    let wanted = app.wanted_resources();
    assert!(
        !wanted.is_empty(),
        "the window asks the workspace for its bookmarks"
    );
    for record in wanted {
        app.workspace_answered(record, WorkspaceAnswer::HoldsNone);
    }
    for _ in 0..4 {
        app.frame();
    }
    assert!(
        says(&mut app, "nothing bookmarked"),
        "the workspace holds none: the window shows its bookmarks, empty"
    );
}

/// A window whose record no device that holds it ever answers for does not wait for good: once
/// the wait's bound passed, it says what it waits for, in the person's words, and offers to go
/// on without it. Going on shows the view with the record read as missing, and what the person
/// types reaches it.
#[test]
fn a_window_whose_record_never_arrives_says_so_and_goes_on_without_it() {
    let mut app = TestApp::<ReadsOnce>::awaiting_the_workspace(
        tilo_app::framework::headless::TEST_WORKSPACE,
        BROWSER,
    )
    .unwrap();
    app.frame();
    let waiting = "Bookmarks from Browser hasn\u{2019}t reached this device yet.";
    assert!(
        !says(&mut app, waiting),
        "a moment in, the window still waits"
    );

    // Seconds after the window opened, on the test's clock.
    let opened = tilo_app::time::now();
    let after = |seconds: u64| {
        opened
            .checked_add(tilo_app::time::Duration::from_secs(seconds))
            .expect("a time")
    };
    tilo_app::time::testing::with_monotonic_time(after(16), || {
        for _ in 0..4 {
            app.frame();
        }
    });
    assert!(
        says(&mut app, waiting),
        "past the bound, the window says what it waits for: {:?}",
        app.texts()
    );
    assert!(
        says(&mut app, "Bookmarks hasn\u{2019}t arrived yet"),
        "its title names what has not arrived: {:?}",
        app.texts()
    );
    assert!(
        !says(&mut app, "the bookmarks are not here"),
        "the view is not built yet"
    );

    // The person reads the page a moment, then goes on; the clock moves on with them.
    tilo_app::time::testing::with_monotonic_time(after(17), || {
        app.harness()
            .click_named("arriving.without")
            .expect("the window offers to go on without the bookmarks");
    });
    for seconds in 18..22 {
        tilo_app::time::testing::with_monotonic_time(after(seconds), || app.frame());
    }
    assert!(
        says(&mut app, "the bookmarks are not here"),
        "going on, the view reads the bookmarks as missing: {:?}",
        app.texts()
    );
    assert!(!says(&mut app, waiting), "and no longer says it waits");

    tilo_app::time::testing::with_monotonic_time(after(22), || {
        app.harness().click_named("note").expect("the view's field");
        app.harness().type_text("hello");
        app.frame();
    });
    assert!(
        says(&mut app, "hello"),
        "what the person types reaches the view, not held"
    );
}

/// The workspace's answer that it holds none of the record comes after the window said it
/// waited too long: the late answer wins, and the window shows its view with the record empty,
/// as if it had come in time, with nothing for the person to press.
#[test]
fn a_late_answer_shows_the_view_after_the_window_said_it_waited_too_long() {
    use tilo_app::resource::WorkspaceAnswer;
    let mut app = TestApp::<ReadsOnce>::awaiting_the_workspace(
        tilo_app::framework::headless::TEST_WORKSPACE,
        BROWSER,
    )
    .unwrap();
    app.frame();
    let opened = tilo_app::time::now();
    let after = |seconds: u64| {
        opened
            .checked_add(tilo_app::time::Duration::from_secs(seconds))
            .expect("a time")
    };
    tilo_app::time::testing::with_monotonic_time(after(16), || {
        for _ in 0..4 {
            app.frame();
        }
    });
    assert!(
        says(&mut app, "Continue without it"),
        "past the bound, the window offers to go on: {:?}",
        app.texts()
    );

    let wanted = app.wanted_resources();
    assert!(
        !wanted.is_empty(),
        "the window still asks for its bookmarks"
    );
    for record in wanted {
        app.workspace_answered(record, WorkspaceAnswer::HoldsNone);
    }
    for seconds in 17..21 {
        tilo_app::time::testing::with_monotonic_time(after(seconds), || app.frame());
    }
    assert!(
        says(&mut app, "nothing bookmarked"),
        "the late answer shows the bookmarks, empty: {:?}",
        app.texts()
    );
    assert!(
        !says(&mut app, "Continue without it"),
        "and the window no longer says it waits"
    );
}

/// Browser's record shown as a page: the view a person's browser renders, held outside any
/// window, which whoever holds it tells what the workspace holds.
fn page_of_reads_once() -> tilo_app::framework::AttachedView {
    let app = TestApp::<ReadsOnce>::new().unwrap();
    let definition = tilo_app::framework::definition::<ReadsOnce>();
    tilo_app::framework::AttachedView::open(
        definition,
        tilo_app::framework::testing::TEST_WORKSPACE,
        tilo_app::resource::DocumentCheckout::new(
            app.record().clone(),
            definition.resource_type.clone(),
        ),
        app.read_document(|doc| doc.clone().save()),
        &MemberId::person("ada"),
        tilo_app::framework::harness_device::HarnessDevice::member_of(
            tilo_app::framework::testing::TEST_WORKSPACE,
            &MemberId::person("ada"),
            tilo_app::Role::Owner,
        ),
        false,
    )
    .unwrap()
}

/// What the page shows, as its composition carries it.
fn page_says(page: &tilo_app::framework::AttachedView, text: &str) -> bool {
    serde_json::to_string(&page.composition())
        .unwrap()
        .contains(text)
}

/// A page waiting for the workspace's record shows its view once whoever holds it says the
/// workspace holds none: the record is new, empty, and the view reads it as such. The page
/// asks for the record itself, so a holder that answers what the page wants answers it.
#[test]
fn a_page_shows_the_workspaces_record_empty_once_the_workspace_holds_none() {
    use tilo_app::resource::WorkspaceAnswer;
    let mut page = page_of_reads_once();
    assert!(
        !page_says(&page, "nothing bookmarked"),
        "not read as none yet"
    );
    let wanted = page.wanted_resources();
    assert!(!wanted.is_empty(), "the page asks for its bookmarks");
    for record in wanted {
        page.workspace_answered(record, WorkspaceAnswer::HoldsNone);
    }
    assert!(
        page_says(&page, "nothing bookmarked"),
        "the page shows the bookmarks, empty"
    );
}

/// The answer that the workspace holds none comes after the page said it waited too long: the
/// late answer wins, and the page shows its view as if it had come in time.
#[test]
fn a_late_answer_shows_the_page_after_it_said_it_waited_too_long() {
    use tilo_app::resource::WorkspaceAnswer;
    let mut page = page_of_reads_once();
    let opened = tilo_app::time::now();
    let after = |seconds: u64| {
        opened
            .checked_add(tilo_app::time::Duration::from_secs(seconds))
            .expect("a time")
    };
    let offered = tilo_app::time::testing::with_monotonic_time(after(16), || {
        page_says(&page, "Continue without it")
    });
    assert!(offered, "past the bound, the page offers to go on");
    let asked = page.wanted_resources();
    assert!(!asked.is_empty(), "the page still asks for its bookmarks");
    for record in asked {
        page.workspace_answered(record, WorkspaceAnswer::HoldsNone);
    }
    let shown = tilo_app::time::testing::with_monotonic_time(after(17), || {
        (
            page_says(&page, "nothing bookmarked"),
            page_says(&page, "Continue without it"),
        )
    });
    assert_eq!(
        shown,
        (true, false),
        "the late answer shows the bookmarks, empty, and the page no longer says it waits"
    );
}

/// A page whose placeholder showed before the workspace answered (the first page a worker opens
/// is slow to lay out) shows the answer in the read that follows it: a page is a still frame,
/// read when its person's browser asks, and has no later frame that would reveal what landed.
#[test]
fn a_page_answered_while_its_placeholder_shows_shows_the_answer_in_the_next_read() {
    use tilo_app::resource::WorkspaceAnswer;
    let mut page = page_of_reads_once();
    let opened = tilo_app::time::now();
    let after = |millis: u64| {
        opened
            .checked_add(tilo_app::time::Duration::from_millis(millis))
            .expect("a time")
    };
    assert!(
        !page_says(&page, "nothing bookmarked"),
        "not read as none yet"
    );
    let shown = tilo_app::time::testing::with_monotonic_time(after(200), || {
        let asked = page.wanted_resources();
        assert!(!asked.is_empty(), "the page asks for its bookmarks");
        for record in asked {
            page.workspace_answered(record, WorkspaceAnswer::HoldsNone);
        }
        page_says(&page, "nothing bookmarked")
    });
    assert!(
        shown,
        "the read after the answer shows the bookmarks, empty"
    );
    let later = tilo_app::time::testing::with_monotonic_time(after(2000), || {
        page_says(&page, "nothing bookmarked")
    });
    assert!(later, "and every read after it");
}
