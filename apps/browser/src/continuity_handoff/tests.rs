use super::*;
use browser_continuity::{Profile, Provider};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use ui::framework::testing::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::widget::Element;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct TestBrowser;
impl ResourceApp for TestBrowser {
    type Resource = crate::Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    fn view(cx: &mut ViewCx<crate::Browser>) -> Element {
        let engine = cx.take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>();
        assert!(engine.is_some(), "test browser requires its lent engine");
        let engine = engine.unwrap_or_else(ui::webview::test_engine::TestEngine::new);
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
    fn new(history: bool) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700))?;
        let profile = Profile::new(
            "a".repeat(64),
            Provider::Chromium,
            vec!["Default".into()],
            true,
            history,
        )?;
        let plan = Plan::new("authenticated-install".into(), [0x68; 32], vec![profile])?;
        let directory = root.path().join("profiles").join(
            &plan
                .profiles
                .first()
                .ok_or("fixture plan has no profile")?
                .id,
        );
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("Bookmarks"),
            r#"{"roots":{"bookmark_bar":{"id":"1","type":"folder","name":"Bookmarks bar","children":[{"id":"2","type":"url","name":"Private bookmark","url":"https://migration.test/bookmark"}]}}}"#,
        )?;
        let connection = rusqlite::Connection::open(directory.join("History"))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch("CREATE TABLE urls(url TEXT,title TEXT,last_visit_time INTEGER,visit_count INTEGER); INSERT INTO urls VALUES('https://migration.test/history','Private history',11644473607000000,7);")?;
        let held = StorageDirectory::open(root.path())?;
        held.publish(
            "plan.json".as_ref(),
            &plan.encode()?,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        )?;
        Ok(Self {
            root,
            state,
            plan,
            connection,
        })
    }
    fn prepared(&self, person: &[u8; 32]) -> io::Result<Prepared> {
        prepare_given(
            File::open(self.root.path())?,
            Arc::new(StorageDirectory::open(self.state.path())?),
            person,
        )
    }
    fn source_bytes(&self) -> TestResult<Vec<(std::path::PathBuf, Vec<u8>, u32)>> {
        let profile = self.root.path().join("profiles").join(
            &self
                .plan
                .profiles
                .first()
                .ok_or("fixture plan has no profile")?
                .id,
        );
        fs::read_dir(profile)?
            .map(|entry| {
                let path = entry?.path();
                Ok((
                    path.clone(),
                    fs::read(&path)?,
                    fs::metadata(&path)?.permissions().mode(),
                ))
            })
            .collect()
    }
}
fn live() -> TestResult<TestApp<TestBrowser>> {
    Ok(TestApp::for_person_with_lent(
        &"68".repeat(32),
        ui::webview::test_engine::TestEngine::new(),
    )?)
}
#[test]
fn authenticated_subset_commits_actual_personal_bookmarks_and_full_history_and_retries_without_recounting()
 {
    let outcome = (|| -> TestResult {
        let fixture = Fixture::new(true)?;
        let before = fixture.source_bytes()?;
        let mut app = live()?;
        let bookmarks = app
            .person::<PersonalBookmarks>()
            .ok_or("personal record is missing")?;
        let history = app
            .person::<BrowsingHistory>()
            .ok_or("personal record is missing")?;
        bookmarks.invoke(&PersonalBookmarks::ADD_BOOKMARK.with((
            "https://existing.test/".to_owned(),
            "Already mine".to_owned(),
        )))?;
        history.invoke(
            &BrowsingHistory::VISIT.with(crate::history::browsing_history::Visit {
                url: "https://existing.test/".into(),
                title: "Existing visit".into(),
                at: 1,
            }),
        )?;
        let prepared = fixture.prepared(&fixture.plan.identity_public_key)?;
        assert!(!prepared.complete);
        prepared.capture.apply(&bookmarks, &history)?;
        assert!(
            bookmarks
                .get()
                .bookmarks()
                .contains("https://existing.test/")
        );
        assert!(
            bookmarks
                .get()
                .bookmarks()
                .contains("https://migration.test/bookmark")
        );
        assert_eq!(
            bookmarks
                .get()
                .imported_trees()
                .first()
                .ok_or("imported bookmark tree is missing")?
                .nodes
                .len(),
            2
        );
        assert_eq!(
            history
                .get()
                .entries()
                .iter()
                .find(|row| row.url == "https://migration.test/history")
                .ok_or("imported history row is missing")?
                .visit_count,
            7
        );
        assert!(app.state().bookmarks.is_empty());
        for (path, bytes, mode) in before {
            assert_eq!(fs::read(&path)?, bytes);
            assert_eq!(fs::metadata(path)?.permissions().mode(), mode);
        }
        // A source changes between a partial commit and retry: the captured checkpoint wins.
        fixture
            .connection
            .execute("UPDATE urls SET title='Changed source',visit_count=99", [])?;
        let retry = fixture.prepared(&fixture.plan.identity_public_key)?;
        retry.capture.apply(&bookmarks, &history)?;
        assert_eq!(
            history
                .get()
                .entries()
                .iter()
                .find(|row| row.url == "https://migration.test/history")
                .ok_or("imported history row is missing")?
                .visit_count,
            7
        );
        retry.capture.complete(&retry.state)?;
        assert!(
            fixture
                .prepared(&fixture.plan.identity_public_key)?
                .complete
        );
        history
            .invoke(&BrowsingHistory::FORGET.with("https://migration.test/history".to_owned()))?;
        assert!(
            fixture
                .prepared(&fixture.plan.identity_public_key)?
                .complete
        );
        assert!(
            !history
                .get()
                .entries()
                .iter()
                .any(|row| row.url == "https://migration.test/history")
        );
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}
#[test]
fn history_off_does_not_parse_or_import_history_and_wrong_person_writes_nothing() {
    let outcome = (|| -> TestResult {
        let fixture = Fixture::new(false)?;
        let mut app = live()?;
        let state_before = fs::read_dir(fixture.state.path())?.count();
        assert!(fixture.prepared(&[9; 32]).is_err());
        assert_eq!(fs::read_dir(fixture.state.path())?.count(), state_before);
        fixture.connection.execute_batch("DROP TABLE urls;")?;
        let before = fixture.source_bytes()?;
        let prepared = fixture.prepared(&fixture.plan.identity_public_key)?;
        let history = app
            .person::<BrowsingHistory>()
            .ok_or("personal record is missing")?;
        let bookmarks = app
            .person::<PersonalBookmarks>()
            .ok_or("personal record is missing")?;
        prepared.capture.apply(&bookmarks, &history)?;
        assert!(history.get().entries().is_empty());
        assert!(
            bookmarks
                .get()
                .bookmarks()
                .contains("https://migration.test/bookmark")
        );
        let bytes = prepared.capture.encode()?;
        assert!(!String::from_utf8(bytes)?.contains("Private history"));
        for (path, bytes, mode) in before {
            assert_eq!(fs::read(&path)?, bytes);
            assert_eq!(fs::metadata(path)?.permissions().mode(), mode);
        }
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}
#[test]
fn actual_browser_worker_and_semantic_status_finish_without_touching_shared_records() {
    let outcome = (|| -> TestResult {
        let fixture = Fixture::new(true)?;
        let mut app = live()?;
        app.with_surface::<crate::BrowserWindow, _>(|window| -> TestResult {
            window.page_mut().continuity_grant_for_test(
                File::open(fixture.root.path())?,
                StorageDirectory::open(fixture.state.path())?,
            );
            Ok(())
        })
        .ok_or("browser surface is missing")??;
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.settle();
            let result = app.call_on(
                "browser.continuity",
                "read-continuity",
                serde_json::json!({}),
            )?;
            if result
                .as_ref()
                .ok_or("continuity action returned no output")?
                .get("state")
                .ok_or("continuity output has no state")?
                == "complete"
            {
                break;
            }
            assert_ne!(
                result
                    .as_ref()
                    .ok_or("continuity action returned no output")?
                    .get("state")
                    .ok_or("continuity output has no state")?,
                "failed"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "actual browser worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            app.person::<PersonalBookmarks>()
                .ok_or("personal record is missing")?
                .get()
                .bookmarks()
                .contains("https://migration.test/bookmark")
        );
        assert_eq!(
            app.person::<BrowsingHistory>()
                .ok_or("personal record is missing")?
                .get()
                .entries()
                .first()
                .ok_or("imported history row is missing")?
                .visit_count,
            7
        );
        assert!(app.state().bookmarks.is_empty());
        assert!(
            fixture
                .prepared(&fixture.plan.identity_public_key)?
                .complete
        );
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[test]
fn failed_browser_import_is_visible_in_empty_personal_library_and_bound_retry_finishes() {
    let outcome = (|| -> TestResult {
        let fixture = Fixture::new(true)?;
        let mut app = live()?;
        // Ordinary empty startup has no migration notice or retry control.
        app.call_on("browser.bookmarks", "show-bookmarks", serde_json::json!({}))?;
        app.settle();
        let inspection = app.harness().inspection_json()?;
        assert!(!inspection.contains("browser.continuity.failure"));
        assert!(!inspection.contains("browser.continuity.retry"));
        let bookmarks_path = fixture
            .root
            .path()
            .join("profiles")
            .join(
                &fixture
                    .plan
                    .profiles
                    .first()
                    .ok_or("fixture plan has no profile")?
                    .id,
            )
            .join("Bookmarks");
        let original = fs::read(&bookmarks_path)?;
        fs::write(&bookmarks_path, b"invalid selected bookmark source")?;
        app.with_surface::<crate::BrowserWindow, _>(|window| -> TestResult {
            window.page_mut().continuity_grant_for_test(
                File::open(fixture.root.path())?,
                StorageDirectory::open(fixture.state.path())?,
            );
            Ok(())
        })
        .ok_or("browser surface is missing")??;
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.settle();
            let result = app.call_on(
                "browser.continuity",
                "read-continuity",
                serde_json::json!({}),
            )?;
            if result
                .as_ref()
                .ok_or("continuity action returned no output")?
                .get("state")
                .ok_or("continuity output has no state")?
                == "failed"
            {
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
                .ok_or("personal record is missing")?
                .get()
                .bookmarks()
                .entries()
                .is_empty()
        );
        assert!(
            app.person::<BrowsingHistory>()
                .ok_or("personal record is missing")?
                .get()
                .entries()
                .is_empty()
        );
        for (resource, action) in [
            ("browser.bookmarks", "show-bookmarks"),
            ("browser.history", "show-history"),
        ] {
            app.call_on(resource, action, serde_json::json!({}))?;
            app.settle();
            let inspection = app.harness().inspection_json()?;
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
        fs::write(&bookmarks_path, original)?;
        // Retry must use the retained grant even after its ambient name changes.
        // This remains entirely inside owned temporary directories, with no fixed
        // host projection mount or replacement of the canonical retry operation.
        let relocated = tempfile::tempdir()?;
        fs::rename(
            fixture.root.path(),
            relocated.path().join("selected-profiles"),
        )?;
        assert!(!fixture.root.path().exists());
        app.harness()
            .perform_named("browser.continuity.retry", &ui::a11y::Action::Activate)?;
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.settle();
            let result = app.call_on(
                "browser.continuity",
                "read-continuity",
                serde_json::json!({}),
            )?;
            assert_ne!(
                result
                    .as_ref()
                    .ok_or("continuity action returned no output")?
                    .get("state")
                    .ok_or("continuity output has no state")?,
                "failed"
            );
            if result
                .as_ref()
                .ok_or("continuity action returned no output")?
                .get("state")
                .ok_or("continuity output has no state")?
                == "complete"
            {
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
                .ok_or("personal record is missing")?
                .get()
                .bookmarks()
                .contains("https://migration.test/bookmark")
        );
        assert_eq!(
            app.person::<BrowsingHistory>()
                .ok_or("personal record is missing")?
                .get()
                .entries()
                .first()
                .ok_or("imported history row is missing")?
                .visit_count,
            7
        );
        let inspection = app.harness().inspection_json()?;
        assert!(!inspection.contains("browser.continuity.failure"));
        assert!(!inspection.contains("browser.continuity.retry"));
        assert!(app.state().bookmarks.is_empty());
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[test]
fn unadmitted_service_effect_refuses_before_changing_the_existing_page() {
    let outcome = (|| -> TestResult {
        let mut app = live()?;
        let before = app
            .with_surface::<crate::BrowserWindow, _>(|window| {
                (
                    window.page_mut().engine_handle(),
                    window.page_mut().observed_page_uri().to_owned(),
                )
            })
            .ok_or("browser surface is missing")?;
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
            ]);
        })
        .ok_or("browser surface is missing")?;
        app.settle();
        let published = app.harness().resources();
        let selected = published
            .iter()
            .find_map(|root| {
                root.children
                    .iter()
                    .find(|resource| resource.id.as_str() == "browser.service.selected-account")
            })
            .ok_or("selected service resource is missing")?;
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
            .err()
            .ok_or("unadmitted service effect unexpectedly succeeded")?;
        assert!(error.to_string().contains("needs to be connected again"));
        app.settle();
        let after = app
            .with_surface::<crate::BrowserWindow, _>(|window| {
                (
                    window.page_mut().engine_handle(),
                    window.page_mut().observed_page_uri().to_owned(),
                )
            })
            .ok_or("browser surface is missing")?;
        assert!(std::sync::Arc::ptr_eq(&before.0, &after.0));
        assert_eq!(before.1, after.1);
        assert!(
            app.person::<PersonalBookmarks>()
                .ok_or("personal record is missing")?
                .get()
                .bookmarks()
                .entries()
                .is_empty()
        );
        assert!(
            app.person::<BrowsingHistory>()
                .ok_or("personal record is missing")?
                .get()
                .entries()
                .is_empty()
        );
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
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
            OPENED_VIEW_ANCHORS.with_borrow_mut(|values| values.push(anchor.to_owned()));
        });
        let engine = cx.take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>();
        assert!(engine.is_some(), "test browser requires its lent engine");
        let engine = engine.unwrap_or_else(ui::webview::test_engine::TestEngine::new);
        cx.surface(crate::BrowserWindow::new(
            crate::BrowserWidget::with_engine(engine, true),
            false,
        ))
    }
}
#[test]
fn static_browser_open_and_registered_framework_opener_publish_once_and_dispatch() {
    let outcome = (|| -> TestResult {
        use ui::tilo_resource::Resource as _;
        OPENED_VIEW_ANCHORS.with_borrow_mut(Vec::clear);
        // This qualifies the same framework opener/static export composition,
        // without pretending this fixture has native service credential custody.
        let mut app = TestApp::<OpeningBrowser>::for_person_with_lent(
            &"68".repeat(32),
            ui::webview::test_engine::TestEngine::new(),
        )?;
        let published = app.harness().resources();
        let root = published
            .iter()
            .find(|resource| resource.id.as_str() == crate::Browser::KIND)
            .ok_or("browser resource is missing")?;
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
            .ok_or("browser open action is missing")?;
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
        )?;
        assert!(
            app.call_on(crate::Browser::KIND, "open", serde_json::json!({}))
                .is_err(),
            "the canonical open contract requires an anchor"
        );
        app.call_on(
            crate::Browser::KIND,
            "open",
            serde_json::json!({"anchor":""}),
        )?;
        OPENED_VIEW_ANCHORS
            .with_borrow(|values| assert_eq!(values.as_slice(), ["service:selected-account", ""]));
        assert!(app.state().bookmarks.is_empty());
        Ok(())
    })();
    assert!(outcome.is_ok(), "{outcome:?}");
}
