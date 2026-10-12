use super::*;
use browser_continuity::{Profile, Provider};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use ui::framework::testing::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::widget::Element;

struct TestBrowser;
impl ResourceApp for TestBrowser {
    type Resource = crate::Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    fn view(cx: &mut ViewCx<crate::Browser>) -> Element {
        let engine = cx
            .take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>()
            .unwrap();
        cx.surface(crate::BrowserWindow::new(
            crate::BrowserWidget::with_engine(engine, true),
            false,
        ))
    }
}
struct Fixture {
    root: tempfile::TempDir,
    state: tempfile::TempDir,
    plan: Plan,
    connection: rusqlite::Connection,
}
impl Fixture {
    fn new(history: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let profile = Profile::new(
            "a".repeat(64),
            Provider::Chromium,
            vec!["Default".into()],
            true,
            history,
        )
        .unwrap();
        let plan = Plan::new("authenticated-install".into(), [0x68; 32], vec![profile]).unwrap();
        let directory = root.path().join("profiles").join(&plan.profiles[0].id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Bookmarks"),r#"{"roots":{"bookmark_bar":{"id":"1","type":"folder","name":"Bookmarks bar","children":[{"id":"2","type":"url","name":"Private bookmark","url":"https://migration.test/bookmark"}]}}}"#).unwrap();
        let connection = rusqlite::Connection::open(directory.join("History")).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection.execute_batch("CREATE TABLE urls(url TEXT,title TEXT,last_visit_time INTEGER,visit_count INTEGER); INSERT INTO urls VALUES('https://migration.test/history','Private history',11644473607000000,7);").unwrap();
        let held = StorageDirectory::open(root.path()).unwrap();
        held.publish(
            "plan.json".as_ref(),
            &plan.encode().unwrap(),
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        )
        .unwrap();
        Self {
            root,
            state,
            plan,
            connection,
        }
    }
    fn prepared(&self, person: &[u8; 32]) -> io::Result<Prepared> {
        prepare_given(
            File::open(self.root.path())?,
            Arc::new(StorageDirectory::open(self.state.path())?),
            person,
        )
    }
    fn source_bytes(&self) -> Vec<(std::path::PathBuf, Vec<u8>, u32)> {
        let profile = self
            .root
            .path()
            .join("profiles")
            .join(&self.plan.profiles[0].id);
        fs::read_dir(profile)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.clone(),
                    fs::read(&path).unwrap(),
                    fs::metadata(&path).unwrap().permissions().mode(),
                )
            })
            .collect()
    }
}
fn live() -> TestApp<TestBrowser> {
    TestApp::for_person_with_lent(
        &"68".repeat(32),
        ui::webview::test_engine::TestEngine::new(),
    )
    .unwrap()
}
#[test]
fn authenticated_subset_commits_actual_personal_bookmarks_and_full_history_and_retries_without_recounting()
 {
    let fixture = Fixture::new(true);
    let before = fixture.source_bytes();
    let mut app = live();
    let bookmarks = app.person::<PersonalBookmarks>().unwrap();
    let history = app.person::<BrowsingHistory>().unwrap();
    bookmarks
        .apply(&PersonalBookmarkAction::Add {
            url: "https://existing.test/".into(),
            title: "Already mine".into(),
            tag: "native-existing".into(),
        })
        .unwrap();
    history
        .invoke(
            &BrowsingHistory::VISIT.with(crate::history::browsing_history::Visit {
                url: "https://existing.test/".into(),
                title: "Existing visit".into(),
                at: 1,
            }),
        )
        .unwrap();
    let prepared = fixture.prepared(&fixture.plan.identity_public_key).unwrap();
    assert!(!prepared.complete);
    prepared.capture.apply(&bookmarks, &history).unwrap();
    assert!(bookmarks.get().bookmarks.contains("https://existing.test/"));
    assert!(
        bookmarks
            .get()
            .bookmarks
            .contains("https://migration.test/bookmark")
    );
    assert_eq!(bookmarks.get().imported_trees[0].nodes.len(), 2);
    assert_eq!(
        history
            .get()
            .entries()
            .iter()
            .find(|row| row.url == "https://migration.test/history")
            .unwrap()
            .visit_count,
        7
    );
    assert!(app.state().bookmarks.is_empty());
    for (path, bytes, mode) in before {
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().permissions().mode(), mode);
    }
    // A source changes between a partial commit and retry: the captured checkpoint wins.
    fixture
        .connection
        .execute("UPDATE urls SET title='Changed source',visit_count=99", [])
        .unwrap();
    let retry = fixture.prepared(&fixture.plan.identity_public_key).unwrap();
    retry.capture.apply(&bookmarks, &history).unwrap();
    assert_eq!(
        history
            .get()
            .entries()
            .iter()
            .find(|row| row.url == "https://migration.test/history")
            .unwrap()
            .visit_count,
        7
    );
    retry.capture.complete(&retry.state).unwrap();
    assert!(
        fixture
            .prepared(&fixture.plan.identity_public_key)
            .unwrap()
            .complete
    );
    history
        .invoke(&BrowsingHistory::FORGET.with("https://migration.test/history".to_owned()))
        .unwrap();
    assert!(
        fixture
            .prepared(&fixture.plan.identity_public_key)
            .unwrap()
            .complete
    );
    assert!(
        !history
            .get()
            .entries()
            .iter()
            .any(|row| row.url == "https://migration.test/history")
    );
}
#[test]
fn history_off_does_not_parse_or_import_history_and_wrong_person_writes_nothing() {
    let fixture = Fixture::new(false);
    let mut app = live();
    let state_before = fs::read_dir(fixture.state.path()).unwrap().count();
    assert!(fixture.prepared(&[9; 32]).is_err());
    assert_eq!(
        fs::read_dir(fixture.state.path()).unwrap().count(),
        state_before
    );
    fixture
        .connection
        .execute_batch("DROP TABLE urls;")
        .unwrap();
    let before = fixture.source_bytes();
    let prepared = fixture.prepared(&fixture.plan.identity_public_key).unwrap();
    let history = app.person::<BrowsingHistory>().unwrap();
    let bookmarks = app.person::<PersonalBookmarks>().unwrap();
    prepared.capture.apply(&bookmarks, &history).unwrap();
    assert!(history.get().entries().is_empty());
    assert!(
        bookmarks
            .get()
            .bookmarks
            .contains("https://migration.test/bookmark")
    );
    let bytes = prepared.capture.encode().unwrap();
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("Private history")
    );
    for (path, bytes, mode) in before {
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().permissions().mode(), mode);
    }
}
#[test]
fn actual_browser_worker_and_semantic_status_finish_without_touching_shared_records() {
    let fixture = Fixture::new(true);
    let mut app = live();
    app.with_surface::<crate::BrowserWindow, _>(|window| {
        window.page_mut().continuity_grant_for_test(
            File::open(fixture.root.path()).unwrap(),
            StorageDirectory::open(fixture.state.path()).unwrap(),
        )
    })
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let result = app
            .call_on(
                "browser.continuity",
                "read-continuity",
                serde_json::json!({}),
            )
            .unwrap();
        if result.as_ref().unwrap()["state"] == "complete" {
            break;
        }
        assert_ne!(result.as_ref().unwrap()["state"], "failed");
        assert!(
            std::time::Instant::now() < deadline,
            "actual browser worker did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        app.person::<PersonalBookmarks>()
            .unwrap()
            .get()
            .bookmarks
            .contains("https://migration.test/bookmark")
    );
    assert_eq!(
        app.person::<BrowsingHistory>().unwrap().get().entries()[0].visit_count,
        7
    );
    assert!(app.state().bookmarks.is_empty());
    assert!(
        fixture
            .prepared(&fixture.plan.identity_public_key)
            .unwrap()
            .complete
    );
}

#[test]
fn failed_browser_import_is_visible_in_empty_personal_library_and_bound_retry_finishes() {
    let fixture = Fixture::new(true);
    let mut app = live();
    // Ordinary empty startup has no migration notice or retry control.
    app.call_on("browser.bookmarks", "show-bookmarks", serde_json::json!({}))
        .unwrap();
    app.settle();
    let inspection = app.harness().inspection_json().unwrap();
    assert!(!inspection.contains("browser.continuity.failure"));
    assert!(!inspection.contains("browser.continuity.retry"));
    let bookmarks_path = fixture
        .root
        .path()
        .join("profiles")
        .join(&fixture.plan.profiles[0].id)
        .join("Bookmarks");
    let original = fs::read(&bookmarks_path).unwrap();
    fs::write(&bookmarks_path, b"invalid selected bookmark source").unwrap();
    app.with_surface::<crate::BrowserWindow, _>(|window| {
        window.page_mut().continuity_grant_for_test(
            File::open(fixture.root.path()).unwrap(),
            StorageDirectory::open(fixture.state.path()).unwrap(),
        );
    })
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let result = app
            .call_on(
                "browser.continuity",
                "read-continuity",
                serde_json::json!({}),
            )
            .unwrap();
        if result.as_ref().unwrap()["state"] == "failed" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual selected source did not fail"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        app.person::<PersonalBookmarks>()
            .unwrap()
            .get()
            .bookmarks
            .entries()
            .is_empty()
    );
    assert!(
        app.person::<BrowsingHistory>()
            .unwrap()
            .get()
            .entries()
            .is_empty()
    );
    for (resource, action) in [
        ("browser.bookmarks", "show-bookmarks"),
        ("browser.history", "show-history"),
    ] {
        app.call_on(resource, action, serde_json::json!({}))
            .unwrap();
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(
            inspection.contains("browser.continuity.failure"),
            "missing failed-import notice in {resource}"
        );
        assert!(inspection.contains("We couldn’t finish bringing your browsing over."));
        assert!(inspection.contains("browser.continuity.retry"));
        assert!(!inspection.contains("invalid selected bookmark source"));
    }
    // Repair the owned fixture, then activate the actual shared button rather
    // than calling the worker directly. It must dispatch the published effect.
    fs::write(&bookmarks_path, original).unwrap();
    // Retry must use the retained grant even after its ambient name changes.
    // This remains entirely inside owned temporary directories, with no fixed
    // host projection mount or replacement of the canonical retry operation.
    let relocated = tempfile::tempdir().unwrap();
    fs::rename(
        fixture.root.path(),
        relocated.path().join("selected-profiles"),
    )
    .unwrap();
    assert!(!fixture.root.path().exists());
    app.harness()
        .perform_named("browser.continuity.retry", &ui::a11y::Action::Activate)
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let result = app
            .call_on(
                "browser.continuity",
                "read-continuity",
                serde_json::json!({}),
            )
            .unwrap();
        assert_ne!(result.as_ref().unwrap()["state"], "failed");
        if result.as_ref().unwrap()["state"] == "complete" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "bound migration retry did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        app.person::<PersonalBookmarks>()
            .unwrap()
            .get()
            .bookmarks
            .contains("https://migration.test/bookmark")
    );
    assert_eq!(
        app.person::<BrowsingHistory>().unwrap().get().entries()[0].visit_count,
        7
    );
    let inspection = app.harness().inspection_json().unwrap();
    assert!(!inspection.contains("browser.continuity.failure"));
    assert!(!inspection.contains("browser.continuity.retry"));
    assert!(app.state().bookmarks.is_empty());
}

#[test]
fn unadmitted_service_effect_refuses_before_changing_the_existing_page() {
    let mut app = live();
    let before = app
        .with_surface::<crate::BrowserWindow, _>(|window| {
            (
                window.page_mut().engine_handle(),
                window.page_mut().observed_page_uri().to_owned(),
            )
        })
        .unwrap();
    // Public catalog metadata does not grant credentials. The actual selected
    // account resource exposes the canonical effect; absent native client
    // custody must still refuse before it replaces the existing engine.
    app.with_surface::<crate::BrowserWindow, _>(|window| {
        window.page_mut().accept_service_catalog(vec![
            ui::collaboration::browser_service_sessions::BrowserServiceSessionDescriptor {
                id: "selected-account".into(),
                service: browser_continuity::sessions::Service::Slack,
                setup_binding: "public-fixture-binding".into(),
            },
        ])
    })
    .unwrap();
    app.settle();
    let published = app.harness().resources();
    let selected = published
        .iter()
        .find_map(|root| {
            root.children
                .iter()
                .find(|resource| resource.id.as_str() == "browser.service.selected-account")
        })
        .unwrap();
    assert!(
        selected
            .actions
            .iter()
            .any(|action| action.id == "open-service-session")
    );
    let error = app
        .call_on(
            "browser.service.selected-account",
            "open-service-session",
            serde_json::json!({}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("needs to be connected again"));
    app.settle();
    let after = app
        .with_surface::<crate::BrowserWindow, _>(|window| {
            (
                window.page_mut().engine_handle(),
                window.page_mut().observed_page_uri().to_owned(),
            )
        })
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&before.0, &after.0));
    assert_eq!(before.1, after.1);
    assert!(
        app.person::<PersonalBookmarks>()
            .unwrap()
            .get()
            .bookmarks
            .entries()
            .is_empty()
    );
    assert!(
        app.person::<BrowsingHistory>()
            .unwrap()
            .get()
            .entries()
            .is_empty()
    );
}

thread_local! {
    static OPENED_VIEW_ANCHORS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}
struct OpeningBrowser;
impl ResourceApp for OpeningBrowser {
    type Resource = crate::Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    fn view(cx: &mut ViewCx<crate::Browser>) -> Element {
        cx.on_open(|anchor| {
            OPENED_VIEW_ANCHORS.with_borrow_mut(|values| values.push(anchor.to_owned()))
        });
        let engine = cx
            .take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>()
            .unwrap();
        cx.surface(crate::BrowserWindow::new(
            crate::BrowserWidget::with_engine(engine, true),
            false,
        ))
    }
}
#[test]
fn static_browser_open_and_registered_framework_opener_publish_once_and_dispatch() {
    use ui::tilo_resource::Resource as _;
    OPENED_VIEW_ANCHORS.with_borrow_mut(Vec::clear);
    // This qualifies the same framework opener/static export composition,
    // without pretending this fixture has native service credential custody.
    let mut app = TestApp::<OpeningBrowser>::for_person_with_lent(
        &"68".repeat(32),
        ui::webview::test_engine::TestEngine::new(),
    )
    .unwrap();
    let published = app.harness().resources();
    let root = published
        .iter()
        .find(|resource| resource.id.as_str() == crate::Browser::KIND)
        .unwrap();
    assert_eq!(
        root.actions
            .iter()
            .filter(|action| action.id == "open")
            .count(),
        1
    );
    let open = root
        .actions
        .iter()
        .find(|action| action.id == "open")
        .unwrap();
    assert_eq!(open.operation_type.as_deref(), Some("tilo.app.open"));
    assert_eq!(
        open.contract.need().kind,
        ui::tilo_actions::ActionKind::Read
    );
    assert!(!open.contract.need().privileged);
    app.call_on(
        crate::Browser::KIND,
        "open",
        serde_json::json!({"anchor":"service:selected-account"}),
    )
    .unwrap();
    assert!(
        app.call_on(crate::Browser::KIND, "open", serde_json::json!({}))
            .is_err(),
        "the canonical open contract requires an anchor"
    );
    app.call_on(
        crate::Browser::KIND,
        "open",
        serde_json::json!({"anchor":""}),
    )
    .unwrap();
    OPENED_VIEW_ANCHORS
        .with_borrow(|values| assert_eq!(values.as_slice(), ["service:selected-account", ""]));
    assert!(app.state().bookmarks.is_empty());
}
