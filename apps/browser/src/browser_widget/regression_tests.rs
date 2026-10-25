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

#[test]
fn option_arrow_moves_by_word_on_macos_and_goes_back_elsewhere() {
    let mut widget = BrowserWidget::new_headless();
    let alt = winit::event::Modifiers::from(winit::keyboard::ModifiersState::ALT);
    let goes_back =
        widget.handle_browser_shortcut(&architecture_tests::key(WNamedKey::ArrowLeft), alt);
    let mac =
        ui::platform::PlatformConventions::default().platform() == ui::platform::Platform::MacOs;
    assert_eq!(
        goes_back, !mac,
        "Option+Left is the caret's on macOS, Alt+Left is Back elsewhere"
    );
    assert_eq!(
        widget
            .pending_controls
            .iter()
            .any(|invocation| invocation.address.action == "back"),
        !mac
    );
}

#[test]
fn an_address_abandoned_in_the_bar_is_not_suggested_for_again() {
    let mut widget = BrowserWidget::new_headless();
    widget.current_uri = "https://page.test/".into();
    widget.run_window_toolbar_event(ui::window_toolbar::ToolbarEvent::Edit {
        text: "secret words".into(),
    });
    assert_eq!(widget.bar_query.as_deref(), Some("secret words"));
    // Esc in the bar: the field holds the page's address again.
    widget.run_window_toolbar_event(ui::window_toolbar::ToolbarEvent::Edit {
        text: "https://page.test/".into(),
    });
    assert_eq!(widget.bar_query, None);
    assert!(widget.bar_suggestions().is_empty());
}

#[test]
fn a_page_whose_renderer_died_says_so_and_reloads() {
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    widget.current_uri = "https://heavy.test/".into();
    widget.set_loading_progress(0.4);
    engine.push_event(WebViewEvent::RendererTerminated);
    widget.apply_engine_events();
    assert_eq!(
        widget.load_error.as_deref(),
        Some("This page stopped working")
    );
    assert!((widget.progress - 1.0).abs() < f64::EPSILON, "no load runs");
    // Try again opens the page afresh.
    widget
        .perform_browser_operation(BrowserOperation::Reload)
        .unwrap();
    assert_eq!(
        engine.commands().last(),
        Some(&WebViewCommand::Navigate("https://heavy.test/".into()))
    );
}

#[test]
fn escape_in_the_address_is_the_addresss_not_the_loads() {
    let mut widget = BrowserWidget::new_headless();
    architecture_tests::compose(&mut widget);
    widget.set_loading_progress(0.4);
    widget
        .daily
        .act(crate::daily_controls::Action::Open, &widget.engine);
    architecture_tests::compose(&mut widget);
    assert!(widget.focus_address_input());
    let escape = architecture_tests::key(WNamedKey::Escape);
    assert!(
        !widget.handle_browser_shortcut(&escape, winit::event::Modifiers::default()),
        "the field takes it"
    );
    assert!(widget.daily.open, "Find stays open");
    assert!(
        !widget
            .pending_controls
            .iter()
            .any(|invocation| invocation.address.action == "stop"),
        "the page goes on loading"
    );
}

#[test]
fn the_active_download_notice_goes_once_the_download_does() {
    let mut widget = BrowserWidget::new_headless();
    let download = |state: &str| WebViewEvent::DownloadUpdated {
        id: 7,
        state: state.into(),
        path: "/home/person/Downloads/file.zip".into(),
        name: "file.zip".into(),
        url: "https://files.test/file.zip".into(),
        retryable: false,
        received: 10,
        total: 100,
        error: String::new(),
    };
    widget.capabilities.apply(download("progress"));
    architecture_tests::compose(&mut widget);
    // The tab was closed while it downloaded: it stayed, and says why.
    widget.close_blocked = true;
    architecture_tests::compose(&mut widget);
    assert!(widget.close_blocked, "still downloading");
    widget.capabilities.apply(download("complete"));
    architecture_tests::compose(&mut widget);
    assert!(!widget.close_blocked, "nothing keeps the tab open now");
}

#[test]
fn a_jump_within_the_page_keeps_find_and_reader_view() {
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    engine.push_event(WebViewEvent::UriChanged("https://docs.test/guide".into()));
    engine.push_event(WebViewEvent::TitleChanged("Guide".into()));
    widget.apply_engine_events();
    widget
        .daily
        .act(crate::daily_controls::Action::Open, &widget.engine);
    widget.reader_view = true;
    // A footnote link, or the page marking the section scrolled to.
    engine.push_event(WebViewEvent::UriChanged(
        "https://docs.test/guide#install".into(),
    ));
    widget.apply_engine_events();
    assert!(widget.daily.open, "Find stays open on the same page");
    assert!(widget.reader_view);
    assert_eq!(widget.title, "Guide");
    // Another page is another document.
    engine.push_event(WebViewEvent::UriChanged("https://docs.test/api".into()));
    widget.apply_engine_events();
    assert!(!widget.reader_view);
    assert!(widget.title.is_empty());
}

#[test]
fn an_engine_download_is_kept_where_the_library_search_and_agents_read() {
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let update = |state: &str, received: u64| WebViewEvent::DownloadUpdated {
        id: 1,
        state: state.into(),
        path: "/home/person/Downloads/report.pdf".into(),
        name: "report.pdf".into(),
        url: "https://files.test/report.pdf".into(),
        retryable: true,
        received,
        total: 100,
        error: String::new(),
    };
    engine.push_event(update("progress", 40));
    widget.apply_engine_events();
    let key = widget.native_download_key(1);
    assert_eq!(
        widget.downloads.get(&key).map(|entry| entry.status.clone()),
        Some(crate::downloads::DownloadStatus::Downloading)
    );
    // Cancelling it from its row, or by an agent, reaches the engine's download.
    widget
        .perform_browser_operation(BrowserOperation::CancelDownload { id: key.clone() })
        .unwrap();
    assert_eq!(
        engine.commands().last(),
        Some(&WebViewCommand::CancelNativeDownload { id: 1 })
    );
    engine.push_event(update("complete", 100));
    widget.apply_engine_events();
    let entry = widget.downloads.get(&key).unwrap();
    assert_eq!(entry.status, crate::downloads::DownloadStatus::Completed);
    assert_eq!(
        entry.destination.as_deref(),
        Some(std::path::Path::new("/home/person/Downloads/report.pdf"))
    );
    // A page's own export has no address to fetch again: not kept.
    engine.push_event(WebViewEvent::DownloadUpdated {
        id: 2,
        state: "complete".into(),
        path: "/home/person/Downloads/chart.png".into(),
        name: "chart.png".into(),
        url: String::new(),
        retryable: false,
        received: 10,
        total: 10,
        error: String::new(),
    });
    widget.apply_engine_events();
    assert!(
        widget
            .downloads
            .get(&widget.native_download_key(2))
            .is_none()
    );
}

#[test]
fn a_page_a_web_app_moves_to_is_named_in_history_once_its_title_comes() {
    let mut app = ui::framework::testing::TestApp::<crate::BrowserShell>::capture(None).unwrap();
    let record = app
        .person::<crate::history::BrowsingHistory>()
        .expect("a test profile keeps person records");
    let engine = TestEngine::new();
    let mut w = BrowserWidget::with_engine(engine.clone(), true);
    w.attach_history(record.clone());
    let title = |url: &str| {
        record.with(|history| history.visits.get(url).map(|visit| visit.title.0.clone()))
    };
    // The real engine's order for a page a web app moves to: its
    // address, its load, then its title.
    for event in [
        WebViewEvent::UriChanged("https://app.test/watch".into()),
        WebViewEvent::LoadFinishedAt {
            uri: "https://app.test/watch".into(),
        },
        WebViewEvent::TitleChanged("A video".into()),
    ] {
        engine.push_event(event);
    }
    w.apply_engine_events();
    assert_eq!(title("https://app.test/watch").as_deref(), Some("A video"));

    // A title it changes to later keeps the name it was recorded with.
    engine.push_event(WebViewEvent::TitleChanged("(1) A video".into()));
    w.apply_engine_events();
    assert_eq!(title("https://app.test/watch").as_deref(), Some("A video"));

    // A title arriving for another page does not name this one.
    for event in [
        WebViewEvent::UriChanged("https://app.test/next".into()),
        WebViewEvent::LoadFinishedAt {
            uri: "https://app.test/next".into(),
        },
        WebViewEvent::UriChanged("https://app.test/other".into()),
        WebViewEvent::TitleChanged("Other".into()),
    ] {
        engine.push_event(event);
    }
    w.apply_engine_events();
    assert_eq!(title("https://app.test/next").as_deref(), Some(""));
}

/// An engine that says only what a test does: the page's answer to a click
/// stays with the page, which is stopped on its question.
struct Asking(
    std::sync::Mutex<Vec<WebViewEvent>>,
    std::sync::Arc<TestEngine>,
);
impl WebViewEngine for Asking {
    fn send(&self, command: WebViewCommand) {
        self.1.send(command);
    }
    fn frames(&self) -> ui::webview::FrameSlot {
        self.1.frames()
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
    fn accept_page_dialogs(&self) {
        self.1.accept_page_dialogs();
    }
}

/// A page that asks waits on its question: the person answers it in the
/// dialog over the page (Escape cancels, Enter takes the default, which is
/// staying when the page asks to be left), and an agent's page action the
/// question stopped answers at once instead of timing out.
#[test]
fn a_question_the_page_asks_is_answered_from_its_dialog() {
    use ui::webview::{PageDialog, PageDialogKind};
    let engine = std::sync::Arc::new(Asking(std::sync::Mutex::new(Vec::new()), TestEngine::new()));
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let theme = Theme::dark();
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(
        engine.1.accepts_page_dialogs(),
        "the page's questions come here"
    );
    let push = |event| engine.0.lock().unwrap().push(event);
    let ask = |id, kind| {
        push(WebViewEvent::PageDialog(PageDialog {
            id,
            kind,
            origin: "https://lists.example".into(),
            message: "Name the list".into(),
            default_text: "Groceries".into(),
        }));
    };
    let answers = || {
        engine
            .1
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::AnswerPageDialog { id, accept, text } => Some((id, accept, text)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    // An agent clicks a button that asks before it acts.
    let click = ui::tilo_actions::InvocationId::new();
    widget
        .admit(
            click,
            BrowserOperation::Page(PageRequest::Click {
                element: "d1e5".into(),
            }),
        )
        .unwrap();
    ask(9, PageDialogKind::Prompt);
    let (tree, node) = tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    // The click happened; the page only asks before it goes on. So the action
    // succeeds, and its answer carries the question for the agent to answer.
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        widget.poll_invocation(click)
    else {
        panic!("the action the question stopped answers at once, as what it did");
    };
    let value = output
        .value
        .expect("the answer carries the question it opened");
    assert!(
        value["message"]
            .as_str()
            .is_some_and(|message| message.contains("Clicked element d1e5")),
        "it says what it did: {value}"
    );
    assert_eq!(value["dialog"]["kind"], "prompt");
    assert_eq!(value["dialog"]["message"], "Name the list");
    assert_eq!(value["dialog"]["default_text"], "Groceries");
    assert_eq!(value["provenance"], "web-content");
    assert!(
        value["outline"]
            .as_str()
            .is_some_and(|line| line.starts_with("- dialog prompt \"Name the list\"")),
        "the outline names the question: {value}"
    );
    assert!(
        widget
            .published()
            .children
            .iter()
            .any(|row| row.id.as_str() == crate::page_dialogs::ROW),
        "the question is the window's row"
    );
    assert!(ui::widget::find_test_id(&widget, crate::page_dialogs::ROW).is_some());
    assert_eq!(
        widget.modal_capture(),
        widget.page_dialogs.widget().unwrap().modal_capture()
    );
    ui::testing::Dispatch::new()
        .layout(&tree, node)
        .send(&mut widget, &architecture_tests::key(WNamedKey::Escape));
    assert_eq!(answers(), [(9, false, None)], "Escape cancels");
    assert!(ui::widget::find_test_id(&widget, crate::page_dialogs::ROW).is_none());

    ask(10, PageDialogKind::Confirm);
    let (tree, node) = tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    ui::testing::Dispatch::new()
        .layout(&tree, node)
        .send(&mut widget, &architecture_tests::key(WNamedKey::Enter));
    assert_eq!(answers()[1], (10, true, None), "Enter allows");

    ask(11, PageDialogKind::Leave { reload: false });
    let (tree, node) = tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    ui::testing::Dispatch::new()
        .layout(&tree, node)
        .send(&mut widget, &architecture_tests::key(WNamedKey::Enter));
    assert_eq!(answers()[2], (11, false, None), "Enter stays on the page");

    // An agent answers through the window's row, with the action it
    // publishes: a prompt takes the text it gives.
    ask(13, PageDialogKind::Prompt);
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    let published = widget.published();
    let row = published
        .children
        .iter()
        .find(|row| row.id.as_str() == crate::page_dialogs::ROW)
        .unwrap();
    let answer = |action: &str, arguments: serde_json::Value| {
        use ui::tilo_resource::Effect as _;
        let spec = row.actions.iter().find(|spec| spec.id == action).unwrap();
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let encoded = spec.build_json(&arguments).unwrap();
        crate::effects::BrowserEffect::from_payload(&encoded.payload)
            .unwrap()
            .operation()
    };
    let send = answer("accept-page-dialog", serde_json::json!({"text": "Errands"}));
    let stale = answer("dismiss-page-dialog", serde_json::json!({}));
    widget
        .admit(ui::tilo_actions::InvocationId::new(), send)
        .unwrap();
    assert_eq!(answers()[3], (13, true, Some("Errands".into())));
    assert!(
        widget
            .admit(ui::tilo_actions::InvocationId::new(), stale)
            .is_err(),
        "an answer to a question no longer asked is refused"
    );
    assert_eq!(answers().len(), 4);

    // The engine stops waiting by itself (the page went elsewhere): the
    // dialog goes, unanswered.
    ask(12, PageDialogKind::Alert);
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(widget.page_dialogs.current().is_some());
    push(WebViewEvent::PageDialogClosed { id: 12 });
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(widget.page_dialogs.current().is_none());
    assert_eq!(answers().len(), 4);
}

/// A page waiting on its question runs nothing, so an agent's page action
/// started then is refused at once with the question to answer, rather than
/// sent to the page to time out 45 seconds later as retryable; once the
/// question is answered, the page takes actions again.
#[test]
fn a_page_action_while_the_page_waits_on_its_question_answers_at_once() {
    use ui::webview::{PageDialog, PageDialogKind};
    let engine = std::sync::Arc::new(Asking(std::sync::Mutex::new(Vec::new()), TestEngine::new()));
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let theme = Theme::dark();
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    engine
        .0
        .lock()
        .unwrap()
        .push(WebViewEvent::PageDialog(PageDialog {
            id: 4,
            kind: PageDialogKind::Confirm,
            origin: "https://shop.example".into(),
            message: "Empty the basket?".into(),
            default_text: String::new(),
        }));
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(widget.page_dialogs.current().is_some());
    let sent = || {
        engine
            .1
            .commands()
            .into_iter()
            .filter(|command| matches!(command, WebViewCommand::Page { .. }))
            .count()
    };
    let click = |element: &str| {
        BrowserOperation::Page(PageRequest::Click {
            element: element.into(),
        })
    };

    let refused = ui::tilo_actions::InvocationId::new();
    widget.admit(refused, click("d1e5")).unwrap();
    let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
        widget.poll_invocation(refused)
    else {
        panic!("a page action while the page waits on its question answers at once");
    };
    assert_eq!(error.code, "browser.page.question");
    assert!(
        !error.retryable,
        "trying again changes nothing until it is answered"
    );
    assert!(
        error
            .message
            .contains("waiting on its confirm dialog \"Empty the basket?\""),
        "the refusal quotes the question: {}",
        error.message
    );
    assert!(
        error.message.contains("answer-page-dialog"),
        "and says how to answer it: {}",
        error.message
    );
    assert_eq!(
        error.details.as_ref().map(|details| &details["kind"]),
        Some(&serde_json::json!("confirm")),
        "with the question as a value"
    );
    assert_eq!(sent(), 0, "nothing is sent to a page that runs nothing");

    widget
        .admit(
            ui::tilo_actions::InvocationId::new(),
            BrowserOperation::AnswerPageDialog {
                dialog: 4,
                accept: false,
                text: None,
            },
        )
        .unwrap();
    let next = ui::tilo_actions::InvocationId::new();
    widget.admit(next, click("d1e5")).unwrap();
    assert!(
        widget.poll_invocation(next).is_none(),
        "answered, the page takes the action"
    );
}

/// The pages the person saved are what the address bar suggests too:
/// typing what a bookmark says recalls it, marked as a saved page.
#[test]
fn typing_in_the_address_bar_suggests_saved_pages() {
    let mut w = BrowserWidget::new_headless();
    w.present_state(&tests::bookmarked(&[(
        "https://reports.example.test/annual",
        "Annual report",
    )]));
    let (doc, root) = tests::synced(&mut w);
    let mut tree = TaffyTree::new();
    w.build_layout(
        &mut tree,
        &Theme::dark(),
        &mut std::collections::HashMap::default(),
    );
    assert!(w.focus_address_input());
    w.address.set("annual".into());
    w.sync(&doc, &root);
    let rows = w.suggestions.items().to_vec();
    let saved = rows
        .iter()
        .find(|row| row.kind == omnibox::RowKind::Bookmark)
        .unwrap_or_else(|| panic!("the saved page is suggested: {rows:?}"));
    assert_eq!(saved.title, "Annual report");
    assert_eq!(saved.detail, "reports.example.test/annual");
    assert_eq!(saved.url, "https://reports.example.test/annual");
    // Choosing it opens the page it names.
    assert_eq!(
        row_destination(saved, &w.settings.search_engine),
        "https://reports.example.test/annual"
    );

    // A bookmark saved in the person's own library is suggested as well.
    let mut app = ui::framework::testing::TestApp::<crate::BrowserShell>::capture(None).unwrap();
    let personal = app
        .person::<crate::personal_bookmarks::PersonalBookmarks>()
        .expect("a test profile keeps person records");
    personal
        .invoke(
            &crate::personal_bookmarks::PersonalBookmarks::ADD_BOOKMARK.with((
                "https://person.example.test/notes".to_owned(),
                "Notes of the year".to_owned(),
            )),
        )
        .unwrap();
    let mut own = BrowserWidget::new_headless();
    own.attach_personal_bookmarks(personal);
    let (doc, root) = tests::synced(&mut own);
    let mut tree = TaffyTree::new();
    own.build_layout(
        &mut tree,
        &Theme::dark(),
        &mut std::collections::HashMap::default(),
    );
    assert!(own.focus_address_input());
    own.address.set("notes".into());
    own.sync(&doc, &root);
    let rows = own.suggestions.items().to_vec();
    assert!(
        rows.iter().any(|row| row.kind == omnibox::RowKind::Bookmark
            && row.url == "https://person.example.test/notes"),
        "the person's own bookmark is suggested: {rows:?}"
    );
}

/// The Library's history rows forget one page each, through the same
/// `forget` verb that clears all of them: the rest stay.
#[test]
fn a_history_row_forgets_only_its_own_page() {
    let (_app, record) = tests::person_history();
    for (url, title) in [
        ("https://one.test/", "One"),
        ("https://two.test/", "Two"),
        ("https://three.test/", "Three"),
    ] {
        record
            .invoke(&crate::history::BrowsingHistory::VISIT.with(
                crate::history::browsing_history::Visit {
                    url: url.into(),
                    title: title.into(),
                    at: 10,
                },
            ))
            .unwrap();
    }
    let mut w = BrowserWidget::new_headless();
    w.attach_history(record.clone());
    w.show_library = true;
    w.library_section = LibrarySection::History;
    let (doc, root) = tests::synced(&mut w);
    architecture_tests::compose(&mut w);

    let forgotten = "https://two.test/";
    let id = history_resource_id(forgotten);
    // The row still opens its page, and now offers its own removal.
    architecture_tests::assert_action_binding(&w, &format!("{id}.open"), &id, "open");
    architecture_tests::assert_action_binding(&w, &format!("{id}.forget"), &id, "forget-visit");

    // What its press runs is the row's own published action.
    let page = w.resource_by_id(&id).expect("the history page is a row");
    let forget = page
        .actions
        .iter()
        .find(|action| action.id == "forget-visit")
        .expect("the row offers forgetting it");
    let encoded = forget.build_json(&serde_json::Map::new()).unwrap();
    assert!(
        matches!(
            w.admit(
                ui::tilo_actions::InvocationId::new(),
                tests::effect_operation(&encoded.payload),
            )
            .map(Some)
            .unwrap(),
            Some(ui::framework::Effected::Done(_))
        ),
        "forgetting one page answers at once"
    );
    w.sync(&doc, &root);

    let shown: Vec<&str> = w
        .history
        .entries()
        .iter()
        .map(|entry| entry.url.as_str())
        .collect();
    assert!(
        !shown.contains(&forgotten),
        "the forgotten page is gone from the list: {shown:?}"
    );
    for kept in ["https://one.test/", "https://three.test/"] {
        assert!(shown.contains(&kept), "{kept} stays: {shown:?}");
    }
    assert!(
        record.with(|history| !history.visits.contains_key(forgotten)
            && history.visits.contains_key("https://one.test/")
            && history.visits.contains_key("https://three.test/")),
        "the record forgets exactly that page"
    );

    // The list the person sees agrees: one row gone, the others there.
    architecture_tests::compose(&mut w);
    assert!(
        w.find_rect(&format!("{id}.open")).is_none(),
        "no row is left for the forgotten page"
    );
    for kept in ["https://one.test/", "https://three.test/"] {
        assert!(
            w.find_rect(&format!("{}.open", history_resource_id(kept)))
                .is_some(),
            "{kept} keeps its row"
        );
    }
}

/// Answering the page's question says what answering did and what to do
/// next, so an agent that never read the question still knows the page goes
/// on: the answer is the action's output, not a bare acknowledgement.
#[test]
fn answering_a_pages_question_says_what_happens_next() {
    use ui::webview::{PageDialog, PageDialogKind};
    let engine = std::sync::Arc::new(Asking(std::sync::Mutex::new(Vec::new()), TestEngine::new()));
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let theme = Theme::dark();
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    let ask = |id, kind| {
        engine
            .0
            .lock()
            .unwrap()
            .push(WebViewEvent::PageDialog(PageDialog {
                id,
                kind,
                origin: "https://forms.example".into(),
                message: "Security code".into(),
                default_text: "0000".into(),
            }));
    };
    let answer =
        |dialog: u32, accept: bool, text: Option<&str>| BrowserOperation::AnswerPageDialog {
            dialog,
            accept,
            text: text.map(str::to_string),
        };

    ask(21, PageDialogKind::Prompt);
    tests::layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    let done = widget
        .admit(
            ui::tilo_actions::InvocationId::new(),
            answer(21, true, Some("7391")),
        )
        .unwrap();
    let ui::framework::Effected::Done(output) = done else {
        panic!("answering the question answers at once");
    };
    let message = output
        .value
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("Answered the page's prompt dialog: OK with \"7391\""),
        "it says what answering took: {message}"
    );
    assert!(
        message.contains("read-page"),
        "and what to do next: {message}"
    );
}
