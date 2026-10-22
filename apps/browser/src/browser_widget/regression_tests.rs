//! Regression tests for what people hit in the browser: each names what
//! went wrong and is asserted on the path a person's action takes.

use super::*;
use ui::webview::WebViewEngine;

#[test]
fn a_navigation_while_a_page_loads_replaces_it() {
    struct Events(
        std::sync::Mutex<Vec<WebViewEvent>>,
        std::sync::Arc<TestEngine>,
    );
    impl WebViewEngine for Events {
        fn send(&self, command: WebViewCommand) {
            self.1.send(command);
        }
        fn navigate_owned(
            &self,
            request_id: u32,
            command: NavigationCommand,
        ) -> Result<(), String> {
            self.1.navigate_owned(request_id, command)
        }
        fn frames(&self) -> ui::webview::FrameSlot {
            self.1.frames()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }
    let engine = std::sync::Arc::new(Events(std::sync::Mutex::new(Vec::new()), TestEngine::new()));
    let mut widget = BrowserWidget::with_engine(engine.clone(), false);
    let slow = ui::tilo_actions::InvocationId::new();
    widget
        .admit(
            slow,
            BrowserOperation::Navigate {
                url: "https://slow.test/".into(),
            },
        )
        .unwrap();
    let slow_request = widget.resource_navigation_request.unwrap();
    // Still loading: the person types another address, then goes back.
    let next = ui::tilo_actions::InvocationId::new();
    assert!(matches!(
        widget.admit(
            next,
            BrowserOperation::Navigate {
                url: "https://next.test/".into(),
            },
        ),
        Ok(ui::framework::Effected::Pending)
    ));
    let back = ui::tilo_actions::InvocationId::new();
    assert!(matches!(
        widget.admit(back, BrowserOperation::Back),
        Ok(ui::framework::Effected::Pending)
    ));
    let back_request = widget.resource_navigation_request.unwrap();
    // The engine reports the replaced loads as such, and the last one.
    engine.0.lock().unwrap().extend([
        WebViewEvent::NavigationOutcome {
            request_id: slow_request,
            uri: "https://slow.test/".into(),
            error: Some("Navigation was superseded".into()),
        },
        WebViewEvent::NavigationOutcome {
            request_id: back_request,
            uri: "https://before.test/".into(),
            error: None,
        },
    ]);
    widget.apply_engine_events();
    assert!(matches!(
        widget.poll_invocation(back),
        Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
    for replaced in [slow, next] {
        let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
            widget.poll_invocation(replaced)
        else {
            panic!("a replaced navigation settles");
        };
        assert_eq!(error.code, "browser.navigation_replaced");
    }
    assert!(widget.replaced_navigations.is_empty());
}

#[test]
fn the_newest_history_rows_open_however_long_the_history() {
    let mut widget = BrowserWidget::new_headless();
    // 150 pages in the record's own order, by address, the newest first
    // in the alphabet.
    for n in 0..150_u64 {
        let url = format!("https://{}.test/", 850 + n);
        widget.history.record_at(&url, "", 1_000_000 - n);
    }
    let newest = history_resource_id("https://850.test/");
    assert!(
        widget.unit_binding(&newest, "open").is_some(),
        "the newest page opens from its row"
    );
}

#[test]
fn a_selection_dragged_off_the_page_is_released_on_it() {
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let (tree, node) = tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &Theme::dark());
    let page = widget.find_rect("browser.surface").unwrap();
    let device = winit::event::DeviceId::dummy();
    let send = |widget: &mut BrowserWidget, event: WindowEvent| {
        ui::testing::Dispatch::new()
            .layout(&tree, node)
            .send(widget, &event);
    };
    let left = |state| WindowEvent::MouseInput {
        device_id: device,
        state,
        button: MouseButton::Left,
    };
    send(
        &mut widget,
        WindowEvent::CursorMoved {
            device_id: device,
            position: (page.x0 + 40.0, page.y0 + 40.0).into(),
        },
    );
    send(&mut widget, left(ElementState::Pressed));
    assert!(widget.wants_pointer_capture());
    // Up over the toolbar, and let go there.
    send(
        &mut widget,
        WindowEvent::CursorMoved {
            device_id: device,
            position: (page.x0 + 40.0, page.y0 - 10.0).into(),
        },
    );
    send(&mut widget, left(ElementState::Released));
    assert!(
        engine.commands().iter().any(|command| matches!(
            command,
            WebViewCommand::MouseButton {
                button: ui::webview::MouseButton::Left,
                down: false,
                ..
            }
        )),
        "the page hears the release: {:?}",
        engine.commands()
    );
    assert!(!widget.wants_pointer_capture());
}

#[test]
fn a_narrow_window_bookmarks_into_the_persons_own_library() {
    let mut app = ui::framework::testing::TestApp::<crate::BrowserShell>::capture(None).unwrap();
    let record = app
        .person::<crate::personal_bookmarks::PersonalBookmarks>()
        .expect("a test profile keeps person records");
    let mut page = BrowserWidget::new_headless();
    page.attach_personal_bookmarks(record);
    page.compact_chrome = true;
    architecture_tests::compose(&mut page);
    architecture_tests::activate_overflow_command(&mut page, "Bookmark this page");
    assert!(page.run_pending_commands());
    let reached: Vec<_> = page
        .pending_controls
        .iter()
        .map(|invocation| {
            (
                invocation.address.resource.as_str(),
                invocation.address.action.as_str(),
            )
        })
        .collect();
    assert_eq!(
        reached,
        [("browser.page", "bookmark-personal-page")],
        "the person's library, not the workspace's"
    );
}

#[test]
fn enter_searches_what_the_address_list_offers_as_a_search() {
    let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
    let search = |w: &BrowserWidget, words: &str| search_url(&w.settings.search_engine, words);
    for words in ["U.S.", "Ph.D.", "foo/bar.baz", "?what.is"] {
        assert!(!omnibox::looks_like_address(words), "{words}");
        let expected = search(&w, words);
        assert_eq!(w.navigation_target(words).unwrap(), expected, "{words}");
    }
    assert_eq!(
        w.navigation_target("example.com/a.b").unwrap(),
        "https://example.com/a.b"
    );
}
