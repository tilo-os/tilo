//! `BrowserWidget`'s own tests: the window a person sees — the install it
//! offers on first use, the layout it holds at every width, the address bar,
//! the panels and the page surface — driven the way a person drives them, and
//! the helpers the other test modules share.

use super::*;
use ui::framework::HostDelivery as _;
use ui::tilo_actions::OperationKind;

/// What the window does for the effect an encoded invocation names.
pub(super) fn effect_operation(payload: &[u8]) -> BrowserOperation {
    use ui::tilo_resource::Effect as _;
    crate::effects::BrowserEffect::from_payload(payload)
        .expect("a browser effect")
        .operation()
}
use std::sync::Arc;
use ui::webview::WebViewEngine;

pub(super) fn synced(widget: &mut BrowserWidget) -> (AutoCommit, ObjId) {
    let mut state = AutoCommit::new();
    let root = automerge::ROOT;
    widget.sync(&state, &root);
    (state, root)
}

pub(super) fn layout_at(widget: &mut BrowserWidget, width: f32, height: f32) {
    layout_with_theme(widget, width, height, &Theme::dark());
}

/// Draw the widget as a window at device scale `scale` draws it.
pub(super) fn draw_at(widget: &mut BrowserWidget, scale: f64) -> Scene {
    let mut scene = Scene::new();
    widget.draw(
        &mut scene,
        Affine::scale(scale),
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::default(),
    );
    scene
}

#[test]
fn the_page_renders_at_the_window_scale_and_hovering_never_rescales_it() {
    for scale in [2.0_f64, 1.5, 1.75, 1.25] {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let theme = Theme::dark();
        let (tree, node) = layout_keeping_tree(&mut widget, 800.0, 600.0, &theme);
        draw_at(&mut widget, scale);
        let page = widget.find_rect("browser.surface").expect("the page");
        let physical = (
            (page.width() * scale).round(),
            (page.height() * scale).round(),
            ui::num::f32_of_f64(scale),
        );
        let first = engine.commands().len();
        for step in 0..24 {
            let position = winit::dpi::PhysicalPosition::new(
                (page.x0 + 12.0 + f64::from(step) * 9.0) * scale,
                (page.y0 + 48.0 + f64::from(step % 5) * 7.0) * scale,
            );
            ui::testing::Dispatch::new()
                .layout(&tree, node)
                .scale_factor(scale)
                .send(
                    &mut widget,
                    &WindowEvent::CursorMoved {
                        device_id: winit::event::DeviceId::dummy(),
                        position,
                    },
                );
            // Each hover repaints the page, and the window draws it.
            draw_at(&mut widget, scale);
        }
        let commands = engine.commands();
        assert!(
            commands[first..]
                .iter()
                .any(|command| matches!(command, WebViewCommand::MouseMove { .. })),
            "the pointer moves reach the page at {scale}"
        );
        let resizes: Vec<_> = commands
            .iter()
            .filter_map(|command| match command {
                WebViewCommand::Resize {
                    width,
                    height,
                    scale_factor,
                } => Some((f64::from(*width), f64::from(*height), *scale_factor)),
                _ => None,
            })
            .collect();
        assert_eq!(
            resizes.last(),
            Some(&physical),
            "the page renders at the window's {scale}, one frame pixel per physical pixel"
        );
        assert!(
            commands[first..]
                .iter()
                .all(|command| !matches!(command, WebViewCommand::Resize { .. })),
            "hovering re-rendered the page at {scale}: {resizes:?}"
        );
    }
}

fn layout_with_theme(widget: &mut BrowserWidget, width: f32, height: f32, theme: &Theme) {
    layout_keeping_tree(widget, width, height, theme);
}

#[test]
fn a_load_never_moves_the_page() {
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine, true);
    layout_at(&mut widget, 1000.0, 700.0);
    let idle = widget.find_rect("browser.surface").unwrap();
    widget
        .perform_browser_operation(BrowserOperation::Navigate {
            url: "https://a.test".into(),
        })
        .unwrap();
    assert!(widget.loading_face().is_some(), "the load is shown");
    let _ = widget.take_structure_dirty();
    layout_at(&mut widget, 1000.0, 700.0);
    let loading = widget.find_rect("browser.surface").unwrap();
    assert_eq!(
        idle, loading,
        "the loading line sits on the toolbar's edge; the page stays put"
    );
}

/// Lay the widget out and hand back the tree and the widget's node, for
/// tests that dispatch events against it.
pub(super) fn layout_keeping_tree(
    widget: &mut BrowserWidget,
    width: f32,
    height: f32,
    theme: &Theme,
) -> (TaffyTree<()>, NodeId) {
    let mut state = AutoCommit::new();
    let root = automerge::ROOT;
    let mut text = TextRenderer::new();
    let mut tree = TaffyTree::<()>::new();
    let mut nodes = std::collections::HashMap::new();
    widget.sync(&state, &root);
    let child = widget.build_layout(&mut tree, theme, &mut nodes);
    widget.sync(&state, &root);
    let layout_root = tree
        .new_with_children(
            Style {
                size: Size::from_lengths(width, height),
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let measure_nodes = nodes.clone();
    tree.compute_layout_with_measure(
        layout_root,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::Definite(height),
        },
        |known, available, node, _, _| {
            measure_nodes
                .get(&node)
                .map_or(taffy::Size::ZERO, |ptr| unsafe {
                    (**ptr).measure(known, available, &mut text)
                })
        },
    )
    .unwrap();
    widget.apply_layout(&tree, child, Point::ORIGIN);
    (tree, child)
}

/// Press the left button at `point`, as a person would.
fn click_at(widget: &mut BrowserWidget, tree: &TaffyTree<()>, node: NodeId, point: Point) -> bool {
    let mut state = AutoCommit::new();
    let mut at = ui::testing::Dispatch::new()
        .doc(&mut state)
        .layout(tree, node);
    let mut release_handled = false;
    for event in [
        ui::synthetic_input::cursor_moved(point.x, point.y),
        ui::synthetic_input::mouse_press(),
        ui::synthetic_input::mouse_release(),
    ] {
        release_handled = at.send(widget, &event).handled;
    }
    release_handled
}

#[test]
fn a_page_waiting_on_permission_names_the_request_in_what_it_reads() {
    let mut widget = BrowserWidget::new_headless();
    widget.current_uri = "https://site.test/form".into();
    widget
        .capabilities
        .apply(WebViewEvent::PermissionRequested {
            id: 3,
            origin: "https://site.test".into(),
            permissions: "location".into(),
        });
    let reply = PageReply {
        url: widget.current_uri.clone(),
        message: "Read 2 lines".into(),
        outline: Some("- heading \"Where are you?\"\n".into()),
        data: None,
        image: None,
        navigation: ui::webview::page::PageNavigation::None,
        loading: false,
    };
    let update = widget.page_success(
        ui::tilo_actions::InvocationId::new(),
        &reply,
        &ui::webview::PageRequest::Outline { frame: None },
        None,
    );
    let ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
        panic!("a page request that worked succeeds");
    };
    let value = output.value.as_ref().unwrap();
    let outline = value["outline"].as_str().unwrap();
    // The site's own question is part of the page it waits on, and names
    // the request the page's answers belong to.
    assert!(
        outline.contains("- heading \"Where are you?\""),
        "{outline}"
    );
    assert!(
        outline.contains("asks to use your location") && outline.contains("request=3"),
        "{outline}"
    );
    assert_eq!(value["permission"]["request"], 3);
    assert_eq!(value["permission"]["rights"], "location");
    assert!(
        value["message"]
            .as_str()
            .unwrap()
            .ends_with("the site asks for permission"),
        "{}",
        value["message"]
    );
    // Nothing waits once the request is answered: the page reads plainly.
    widget
        .capabilities
        .apply(WebViewEvent::PermissionClosed { id: 3 });
    let update = widget.page_success(
        ui::tilo_actions::InvocationId::new(),
        &reply,
        &ui::webview::PageRequest::Outline { frame: None },
        None,
    );
    let ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
        panic!("a page request that worked succeeds");
    };
    assert!(
        output.value.as_ref().unwrap().get("permission").is_none(),
        "an answered request is not published again"
    );
}

#[test]
fn panels_open_below_their_control_and_dismissing_a_question_sets_it_aside() {
    // Phase `the_site_panel_opens_below_the_toolbar_at_the_site_button`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_set_viewport_width(1000);
        layout_at(&mut widget, 1000.0, 700.0);
        widget.site.open = true;
        layout_at(&mut widget, 1000.0, 700.0);
        layout_at(&mut widget, 1000.0, 700.0);
        let panel = widget.anchored_panel.as_ref().unwrap().get_rect();
        let toolbar = widget.find_rect("browser.toolbar").unwrap();
        let trigger = widget.find_rect("browser.site.info").unwrap();
        assert!(
            panel.y0 >= toolbar.y1,
            "the panel {panel:?} leaves the toolbar {toolbar:?} uncovered"
        );
        assert!((panel.x0 - trigger.x0).abs() < 1.0, "{panel:?} {trigger:?}");
        assert!(panel.y1 <= 700.0);
    }

    // Phase `an_autofill_panel_opens_at_its_settings_row`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_autofill(false);
        layout_at(&mut widget, 1000.0, 800.0);
        layout_at(&mut widget, 1000.0, 800.0);
        layout_at(&mut widget, 1000.0, 800.0);
        assert!(widget.credentials.open);
        let panel = widget.anchored_panel.as_ref().unwrap().get_rect();
        let row = widget.find_rect("browser.credentials.open").unwrap();
        assert!(
            (panel.x1 - row.x1).abs() < 1.0,
            "the panel aligns with its trailing management control: {panel:?} {row:?}"
        );
        assert!(
            panel.y1 <= row.y0 || panel.y0 >= row.y1,
            "the panel {panel:?} must not cover its row {row:?}"
        );
    }

    // Phase `dismissing_a_permission_question_declines_it`.
    {
        fn within(widget: &dyn Widget, id: &str) -> bool {
            ui::widget::find_test_id(widget, id).is_some()
        }
        let capture = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(capture.clone(), true);
        widget.debug_request_permission();
        layout_at(&mut widget, 1000.0, 700.0);
        let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &Theme::dark());
        let panel = widget.anchored_panel.as_ref().unwrap().get_rect();
        assert!(panel.width() > 0.0);
        // The question names its two answers, inside the popover.
        let popover = widget.anchored_panel.as_ref().unwrap();
        assert!(within(popover.as_ref(), "browser.decision.true.1.true"));
        assert!(within(popover.as_ref(), "browser.decision.true.1.false"));
        click_at(&mut widget, &tree, node, Point::new(900.0, 650.0));
        layout_at(&mut widget, 1000.0, 700.0);
        // Set aside, not refused: the site may ask again.
        assert!(
            capture
                .commands()
                .contains(&WebViewCommand::PermissionDismissed { id: 1 })
        );
    }
}

#[test]
fn library_downloads_show_a_progress_bar_and_search_is_offered_only_where_it_scopes() {
    fn count(widget: &dyn Widget, matches: &dyn Fn(&dyn Widget) -> bool) -> usize {
        usize::from(matches(widget))
            + widget
                .children()
                .iter()
                .map(|child| count(*child, matches))
                .sum::<usize>()
    }
    let bars = |widget: &BrowserWidget| count(widget, &|node| node.name() == "ProgressBar");
    let searches = |widget: &BrowserWidget| {
        count(widget, &|node| {
            node.test_id()
                .is_some_and(|id| id.starts_with("search-open"))
        })
    };

    let mut downloads = BrowserWidget::new_headless();
    downloads.debug_show_library("downloads");
    downloads.debug_show_downloads("progress");
    layout_at(&mut downloads, 1280.0, 800.0);
    assert_eq!(bars(&downloads), 1, "one download, one bar");
    assert_eq!(searches(&downloads), 1);
    let entry = downloads.downloads.entries().first().unwrap().clone();
    assert!(
        !download_status_text(&entry).contains('%'),
        "the bar carries the percentage"
    );

    let mut extensions = BrowserWidget::new_headless();
    extensions.debug_show_library("extensions");
    layout_at(&mut extensions, 1280.0, 800.0);
    assert_eq!(
        searches(&extensions),
        0,
        "extensions are not a search scope"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn engine_navigation_drives_address_bar_and_history_controls() {
    // Phase `nav_gesture_drives_engine_history`.
    {
        use ui::gesture::NavDirection;
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);
        // Build the composed tree so the gesture forwards down to the
        // WebView — this exercises the real trait-forwarding path
        // (BrowserWidget → Column → WebView), not a shortcut.
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        w.build_layout(&mut tree, &theme, &mut node_map);

        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        assert!(
            w.on_nav_gesture(NavDirection::Back, &state, &root),
            "a back gesture is consumed by the embedded WebView",
        );
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        w.on_nav_gesture(NavDirection::Forward, &state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
    }

    // Phase `bootstrap_seeds_address_bar`.
    {
        let engine = TestEngine::new();
        // Headless so the test never spawns the network filter loader.
        let mut w = BrowserWidget::with_engine(engine, true);
        let (state, root) = synced(&mut w);
        assert_eq!(w.address.with(Clone::clone), DEFAULT_URL);
    }

    // Phase `engine_uri_updates_address_bar_text`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        engine.send(WebViewCommand::Navigate("https://example.test".into()));
        w.sync(&state, &root);

        assert_eq!(w.address.with(Clone::clone), "https://example.test");
        assert_eq!(w.app_title(), "https://example.test — test page");
    }

    // Phase `history_events_drive_button_enablement`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        w.sync(&state, &root);

        assert!(w.can_go_back);
        assert!(!w.can_go_forward);
    }

    // Phase `canonical_navigation_operations_translate_to_engine_commands`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        // History so back/forward are meaningful.
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        w.perform_browser_operation(BrowserOperation::Back).unwrap();
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        w.perform_browser_operation(BrowserOperation::Forward)
            .unwrap();
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));

        w.perform_browser_operation(BrowserOperation::Reload)
            .unwrap();
        assert!(
            engine.commands().contains(&WebViewCommand::Reload),
            "canonical reload reaches the engine",
        );
    }
}

/// The person's browsing history, as a capture's profile keeps it.
/// A browser document bookmarking each `(url, title)`, as items
/// `b0`, `b1`, ….
pub(super) fn bookmarked(pages: &[(&str, &str)]) -> crate::resource::Browser {
    let mut browser = crate::resource::Browser::default();
    for (at, (url, title)) in pages.iter().enumerate() {
        browser.bookmarks.push(
            ui::tilo_resource::Id::from_raw(format!("b{at}")),
            crate::library::Bookmark {
                url: (*url).into(),
                title: (*title).into(),
            },
        );
    }
    browser
}

pub(super) fn person_history() -> (
    ui::framework::testing::TestApp<crate::BrowserShell>,
    ui::framework::PersonRecord<crate::history::BrowsingHistory>,
) {
    let mut app = ui::framework::testing::TestApp::<crate::BrowserShell>::capture(None).unwrap();
    let record = app
        .person::<crate::history::BrowsingHistory>()
        .expect("a test profile keeps person records");
    (app, record)
}

#[test]
fn visits_are_the_persons_history_and_workspace_exit_clears_it() {
    let (_app, record) = person_history();
    let engine = TestEngine::new();
    let mut w = BrowserWidget::with_engine(engine.clone(), true);
    w.attach_history(record.clone());
    let (mut doc, root) = synced(&mut w);
    engine.send(WebViewCommand::Navigate("https://visited.test/".into()));
    w.sync(&doc, &root);
    assert!(
        record.with(|history| {
            history
                .visits
                .keys()
                .any(|url| url.starts_with("https://visited.test"))
        }),
        "a finished page is a visit in the person's history"
    );
    assert!(
        !w.history.is_empty(),
        "the window shows the history it records"
    );

    let mut paused = BrowserWidget::with_engine(engine.clone(), true);
    paused.attach_history(record.clone());
    paused.history_paused = w.history_paused.clone();
    w.history_paused
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let before = record.with(|history| history.visits.len());
    engine.send(WebViewCommand::Navigate("https://paused.test/".into()));
    paused.sync(&doc, &root);
    assert_eq!(record.with(|history| history.visits.len()), before);

    w.settings.clear_on_exit = true;
    drop(w);
    assert!(
        !record.with(|history| history.visits.is_empty()),
        "closing one page keeps the history"
    );
    // Two Browser windows of one profile: tabs, each its own window.
    let profile = tempfile::tempdir().unwrap();
    let window = || {
        let mut page = BrowserWidget::with_engine(TestEngine::new(), true);
        page.attach_history(record.clone());
        page.settings.clear_on_exit = true;
        crate::BrowserWindow::new(page, false).counted_in(profile.path())
    };
    let (first, last) = (window(), window());
    drop(first);
    assert!(
        !record.with(|history| history.visits.is_empty()),
        "closing a tab while another is open keeps the history"
    );
    drop(last);
    assert!(
        record.with(|history| history.visits.is_empty()),
        "closing the browser's last window clears the history"
    );
}

#[test]
fn typing_in_the_address_bar_suggests_and_completes_visited_pages() {
    let (_app, record) = person_history();
    for (url, title, at) in [
        ("https://www.youtube.com/watch?v=1", "Cats - YouTube", 10),
        (
            "https://blog.example.org/why-you-should",
            "Why you should",
            11,
        ),
    ] {
        record
            .invoke(&crate::history::BrowsingHistory::VISIT.with(
                crate::history::browsing_history::Visit {
                    url: url.parse().unwrap(),
                    title: title.into(),
                    at,
                },
            ))
            .unwrap();
    }
    // A later window of the same person sees the same visits.
    let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
    w.attach_history(record);
    let (doc, root) = synced(&mut w);
    let mut tree = TaffyTree::new();
    w.build_layout(
        &mut tree,
        &Theme::dark(),
        &mut std::collections::HashMap::default(),
    );
    assert!(w.focus_address_input());
    w.address.set("you".into());
    w.sync(&doc, &root);
    assert!(w.suggestions.is_visible(), "typing shows suggestions");
    let rows = w.suggestions.items().to_vec();
    assert_eq!(
        rows[0].kind,
        omnibox::RowKind::Go,
        "Enter opens the bar's text"
    );
    assert_eq!(rows[0].url, "youtube.com");
    assert_eq!(rows[0].detail, "https://youtube.com");
    assert!(
        rows.iter()
            .any(|row| row.kind == omnibox::RowKind::History && row.title == "Cats - YouTube"),
        "visited pages show with their titles: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.kind == omnibox::RowKind::Search),
        "searching the typed words is always offered"
    );
    assert_eq!(
        w.address.with(Clone::clone),
        "youtube.com",
        "the host is completed inline"
    );
    // The echo of the completion is not new typing.
    w.sync(&doc, &root);
    assert_eq!(w.address.with(Clone::clone), "youtube.com");
    assert_eq!(w.suggestions.items().len(), rows.len());
}

include!("retained_address_tests.rs");

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn address_focus_selects_tracks_navigation_and_preserves_drafts() {
    // Phase `focused_address_tracks_committed_navigation_but_preserves_new_drafts`.
    {
        let setup = || {
            let engine = TestEngine::new();
            for url in ["https://a.test", "https://b.test"] {
                engine.send(WebViewCommand::Navigate(url.into()));
            }
            let mut widget = BrowserWidget::with_engine(engine.clone(), true);
            layout_at(&mut widget, 900.0, 700.0);
            widget.apply_engine_events();
            assert!(widget.focus_address_input());
            (engine, widget)
        };
        let (_, mut widget) = setup();
        widget.address.set("unsent draft".into());
        widget
            .perform_browser_operation(BrowserOperation::Back)
            .unwrap();
        widget.apply_engine_events();
        assert_eq!(widget.address.with(Clone::clone), "https://a.test");
        widget
            .perform_browser_operation(BrowserOperation::Forward)
            .unwrap();
        widget.apply_engine_events();
        assert_eq!(widget.address.with(Clone::clone), "https://b.test");

        let (_, mut widget) = setup();
        widget
            .perform_browser_operation(BrowserOperation::Back)
            .unwrap();
        widget.address.set("new draft after navigation".into());
        widget.apply_engine_events();
        assert_eq!(
            widget.address.with(Clone::clone),
            "new draft after navigation"
        );

        let (engine, mut widget) = setup();
        widget.address.set("unsent draft".into());
        engine.send(WebViewCommand::Navigate("https://redirect.test".into()));
        widget.apply_engine_events();
        assert_eq!(widget.address.with(Clone::clone), "unsent draft");

        let (engine, mut widget) = setup();
        widget
            .perform_browser_operation(BrowserOperation::Navigate {
                url: "https://start.test".into(),
            })
            .unwrap();
        engine.send(WebViewCommand::Navigate("https://redirect.test".into()));
        widget.apply_engine_events();
        assert_eq!(widget.address.with(Clone::clone), "https://redirect.test");
    }

    // Phase `focused_address_history_menu_selection_replaces_older_draft`.
    {
        let engine = TestEngine::new();
        for url in ["https://a.test", "https://b.test"] {
            engine.send(WebViewCommand::Navigate(url.into()));
        }
        let mut widget = BrowserWidget::with_engine(engine, true);
        layout_at(&mut widget, 900.0, 700.0);
        widget.apply_engine_events();
        assert!(widget.focus_address_input());
        widget.address.set("older unsent draft".into());
        widget.request_navigation_history(NavigationDirection::Back);
        widget.apply_engine_events();
        assert_eq!(widget.back_entries.len(), 1);
        widget.choose_navigation_entry(0);
        widget.apply_engine_events();
        assert_eq!(widget.address.with(Clone::clone), "https://a.test");
    }

    // Phase `address_focus_selects_all_and_survives_chrome_rebuild`.
    {
        fn state(widget: &dyn Widget) -> Option<String> {
            if widget.test_id() == Some(URL_INPUT_ID) {
                assert!(widget.is_focused());
                return ui::undo::get_state(widget);
            }
            widget.children().iter().find_map(|child| state(*child))
        }
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let _ = synced(&mut w);
        let mut tree = TaffyTree::new();
        w.build_layout(
            &mut tree,
            &Theme::dark(),
            &mut std::collections::HashMap::default(),
        );
        assert!(w.focus_address_input());

        let selected = format!("{}:b:0", DEFAULT_URL.len());
        assert_eq!(state(&w).as_deref(), Some(selected.as_str()));
        let mut tree = TaffyTree::new();
        w.build_layout(
            &mut tree,
            &Theme::dark(),
            &mut std::collections::HashMap::default(),
        );
        assert_eq!(state(&w).as_deref(), Some(selected.as_str()));
    }

    // Phase `address_shortcut_focuses_the_chrome_input_after_composition`.
    {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        let _ = synced(&mut w);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        w.build_layout(&mut tree, &Theme::dark(), &mut node_map);

        assert!(w.focus_address_input());
        assert!(w.url_input_focused());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn library_rows_stay_readable_at_every_width() {
    // Matching and bounds retain the same semantics as the local view action.
    assert!(library_filter_matches(
        "rust learn",
        &["Rust", "https://rust-lang.org/learn"]
    ));
    assert!(!library_filter_matches(
        "rust missing",
        &["Rust", "https://rust-lang.org/learn"]
    ));
    let bounded =
        bounded_library_filter(&format!("rust {}", "🦀".repeat(MAX_LIBRARY_FILTER_BYTES)));
    assert!(bounded.len() <= MAX_LIBRARY_FILTER_BYTES);
    assert!(bounded.is_char_boundary(bounded.len()));
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("history");
        widget
            .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                query: "rust learn".into(),
            })
            .unwrap();
        assert_eq!(widget.library_filter.get(), "rust learn");
        assert!(widget.show_library);
        assert!(
            widget
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "x".repeat(MAX_LIBRARY_FILTER_BYTES + 1)
                })
                .is_err()
        );
        layout_at(&mut widget, 390.0, 844.0);
        assert!(
            widget
                .find_rect(&format!(
                    "{}.open",
                    history_resource_id("https://www.rust-lang.org/learn")
                ))
                .is_some()
        );
        assert!(
            widget
                .find_rect(&format!(
                    "{}.open",
                    history_resource_id("https://example.org/long-form")
                ))
                .is_none()
        );
        super::architecture_tests::assert_action_binding(
            &widget,
            "browser.library.clear-filter",
            "browser.library",
            "clear-library-filter",
        );
        widget
            .perform_browser_operation(BrowserOperation::ClearLibraryFilter)
            .unwrap();
        assert!(widget.library_filter.get().is_empty());
        widget
            .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                query: "rust learn".into(),
            })
            .unwrap();
        assert_eq!(
            widget.library_filter.get(),
            "rust learn",
            "the framework filters the production Library view"
        );
    }

    // Phase `compact_history_library_retains_the_hidden_toolbar_privacy_action`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("history");
        widget.debug_set_viewport_width(390);
        layout_at(&mut widget, 390.0, 844.0);
        assert!(
            ui::widget::controls::control_action_bindings(&widget)
                .iter()
                .any(|binding| {
                    binding.control_id == "browser.library.history-pause"
                        && matches!(
                            &binding.kind,
                            ui::widget::ControlBindingKind::Action(address)
                                if address.resource == "browser.session"
                                    && address.action == "toggle-pause"
                        )
                })
        );
    }

    // Phase `populated_library_rows_have_a_bounded_compact_measure_at_all_widths`.
    {
        let mut wide = BrowserWidget::new_headless();
        wide.debug_show_library("history");
        layout_at(&mut wide, 1_200.0, 720.0);
        let first = wide
            .find_rect(&format!(
                "{}.open",
                history_resource_id("https://www.rust-lang.org/learn")
            ))
            .unwrap();
        let second = wide
            .find_rect(&format!(
                "{}.open",
                history_resource_id("https://developer.mozilla.org/en-US/docs/Web")
            ))
            .unwrap();
        let history_tab = wide.find_rect("browser.library.sections").unwrap();
        let theme = ui::Theme::dark();
        let gutter = f64::from(theme.tokens.spacing.rhythm.group);
        let readable = f64::from(theme.tokens.metrics.readable_width);
        assert!(
            (history_tab.x0 - gutter).abs() < 1.0,
            "desktop title, tabs, and rows share the page's leading edge: {history_tab:?}"
        );
        assert!(
            first.x1 <= gutter + readable + 1.0,
            "desktop row actions stay inside the readable Library measure: {first:?}"
        );
        assert!(first.height() <= 60.0);
        assert!(
            second.y0 - first.y0 < 80.0,
            "two-line metadata rows must not expand into oversized slabs"
        );

        let mut narrow = BrowserWidget::new_headless();
        narrow.debug_show_library("bookmarks");
        narrow.present_state(&bookmarked(&[(
            "https://www.rust-lang.org/learn",
            "Learn Rust",
        )]));
        narrow.debug_set_viewport_width(390);
        layout_at(&mut narrow, 390.0, 844.0);
        let open = narrow
            .find_rect(&format!(
                "{}.open",
                bookmark_resource_id(&ui::tilo_resource::Id::from_raw("b0"))
            ))
            .unwrap();
        let remove = narrow
            .find_rect(&format!(
                "{}.remove",
                bookmark_resource_id(&ui::tilo_resource::Id::from_raw("b0"))
            ))
            .unwrap();
        assert!(open.width() > 0.0 && remove.width() > 0.0);
        assert!(
            remove.x1 <= 390.0,
            "row actions stay reachable on narrow layouts"
        );
        let sections = narrow.find_rect("browser.library.sections").unwrap();
        assert!(
            sections.x0 >= 0.0 && sections.x1 <= 390.0,
            "every Library destination stays visible instead of overflowing horizontally"
        );
    }

    // Phase `large_text_desktop_library_keeps_a_centered_reading_measure`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("history");
        widget.debug_set_viewport_width(1_200);
        layout_with_theme(
            &mut widget,
            1_200.0,
            800.0,
            &Theme::dark().with_text_scale(2.0),
        );

        let history_tab = widget.find_rect("browser.library.sections").unwrap();
        let open = widget
            .find_rect(&format!(
                "{}.open",
                history_resource_id("https://www.rust-lang.org/learn")
            ))
            .unwrap();
        let theme = Theme::dark();
        let gutter = f64::from(theme.tokens.spacing.rhythm.group);
        let readable = f64::from(theme.tokens.metrics.readable_width);
        assert!(
            (history_tab.x0 - gutter).abs() < 1.0,
            "large text keeps the Library on the page's leading edge: {history_tab:?}"
        );
        assert!(
            open.x1 <= gutter + readable + 1.0,
            "large-text row actions stay inside the readable Library measure: {open:?}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn address_validation_and_suggestions_dispatch_only_web_navigation() {
    // Phase `address_validation_never_dispatches_non_web_inputs_and_keeps_a_recoverable_error`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (_, _) = synced(&mut w);
        let commands_before = engine.commands().len();
        let overlong = "x".repeat(8 * 1024 + 1);

        for input in [
            "",
            overlong.as_str(),
            "https://",
            "https://bad host",
            "file:///tmp/private.html",
            "about:blank",
            "data:text/html,hello",
            "javascript:alert(1)",
            "custom:opaque",
            "https://example.test/\u{0007}",
        ] {
            let error = w.navigation_target(input).expect_err(input);
            assert!(!error.is_empty());
            assert_eq!(w.load_error.as_deref(), Some(error.as_str()));
            assert!(w.address_error_active);
            assert_eq!(engine.commands().len(), commands_before, "{input}");
        }
    }

    // Phase `address_validation_accepts_local_web_hosts_but_not_local_files`.
    {
        let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
        for (input, expected) in [
            ("localhost:3000", "http://localhost:3000"),
            ("127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("[::1]:8443", "http://[::1]:8443"),
            ("example.com", "https://example.com"),
        ] {
            assert_eq!(w.navigation_target(input).unwrap(), expected);
        }
        assert!(w.navigation_target("file:///tmp/index.html").is_err());
    }

    // Phase `stale_engine_completion_cannot_clear_a_rejected_address_error`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        // Simulate events from an earlier navigation arriving after the user
        // entered a prohibited address. The error must remain actionable,
        // rather than disappearing behind that stale successful page.
        engine.send(WebViewCommand::Navigate("https://earlier.example".into()));
        assert!(w.navigation_target("file:///tmp/not-allowed.html").is_err());
        w.apply_engine_events();
        assert!(w.address_error_active);
        assert_eq!(
            w.load_error.as_deref(),
            Some(AddressProblem::NotWeb.message().as_str())
        );

        assert!(
            w.perform_browser_operation(BrowserOperation::Navigate {
                url: "https://recovery.example".into(),
            })
            .is_ok()
        );
        assert!(
            !w.address_error_active,
            "an allowed retry recovers the page"
        );
    }

    // Phase `address_suggestions_are_modal_and_a11y_actions_queue_canonical_navigation`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.attach_dispatcher(ui::framework::testing::dispatcher("so.tilo.browser"));
        widget.debug_force_suggestions(&[crate::history::HistoryEntry {
            url: "https://accessible.example/reading".into(),
            title: "Accessible reading".into(),
            last_visit: 1,
            visit_count: 1,
        }]);
        layout_at(&mut widget, 390.0, 844.0);

        let mut semantic = ui::a11y_tree::SemanticTree::new();
        semantic.refresh(&widget, &std::collections::HashMap::new(), None, &[]);
        let suggestion = semantic
            .iter()
            .find(|node| node.name.as_deref() == Some("Accessible reading"))
            .expect("floating address suggestions must reach the semantic tree");
        assert!(suggestion.action("activate").is_some());
        assert!(Widget::modal_capture(&widget).is_some());

        let target = format!(
            "browser.address-suggestion:{}",
            blake3::hash(b"https://accessible.example/reading").to_hex()
        );
        assert!(Widget::perform_semantic_action(
            &mut widget,
            &target,
            &ui::a11y::Action::Activate,
            &mut Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        let invocation = ui::widget::controls::take_control_invocation(&mut widget)
            .expect("accessibility activation must queue Browser's action binding");
        assert_eq!(invocation.address.resource, "browser.page");
        assert_eq!(invocation.address.action, "navigate");
        assert!(Widget::modal_capture(&widget).is_none());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn narrow_chrome_keeps_settings_toolbar_and_page_viewport_usable() {
    // Phase `address_editor_opens_and_requires_opt_in_through_narrow_settings`.
    {
        let (_app, book) = crate::autofill::tests::address_book();
        let mut widget = BrowserWidget::new_headless();
        widget.attach_address_book(book.clone());
        let (mut doc, root) = synced(&mut widget);
        widget.show_settings = true;
        widget.structure_dirty = true;
        layout_at(&mut widget, 390.0, 700.0);
        for id in [
            "browser.address.open",
            "browser.address.enable",
            "browser.address.new",
            "browser.address.cancel",
            "browser.credentials.close",
        ] {
            let result = widget
                .start_encoded(
                    OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec(id).unwrap(),
                    &mut doc,
                    &root,
                )
                .unwrap();
            assert!(
                result.is_some(),
                "address editor control must be reachable: {id}"
            );
            assert!(widget.credentials.sync(&widget.engine));
            widget.structure_dirty = true;
            layout_at(&mut widget, 390.0, 700.0);
            if id == "browser.address.open" {
                assert!(
                    !crate::autofill::AutofillStore::open(BrowsingMode::Normal, Some(book.clone()))
                        .enabled(),
                    "opening does not opt in"
                );
            }
            if id == "browser.address.new" {
                let theme = Theme::dark();
                let mut text = TextRenderer::new();
                let mut scene = Scene::new();
                let presence = PresenceCtx::default();
                widget.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
                if let Some(directory) = std::env::var_os("TILO_CREDENTIAL_RENDER_EVIDENCE") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    ui::offscreen::OffscreenRenderer::new(390, 700).render_png(
                        &scene,
                        theme.tokens.colors.surfaces.content,
                        directory.join("address-editor-narrow.png"),
                    );
                }
            }
        }
        assert!(!widget.credentials.open);
        let store = crate::autofill::AutofillStore::open(BrowsingMode::Normal, Some(book));
        assert!(store.enabled());
        assert!(
            store.profiles().is_empty(),
            "discarded editor must not save a profile"
        );
    }

    // Phase `credentials_settings_launcher_and_overlay_actions_work_at_narrow_width`.
    {
        let mut widget = BrowserWidget::new_headless();
        let (mut doc, root) = synced(&mut widget);
        widget.show_settings = true;
        widget.structure_dirty = true;
        layout_at(&mut widget, 390.0, 700.0);
        for id in ["browser.credentials.open", "browser.credentials.close"] {
            let result = widget
                .start_encoded(
                    OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec(id).unwrap(),
                    &mut doc,
                    &root,
                )
                .unwrap();
            assert!(
                result.is_some(),
                "credential action must reach its visible control"
            );
            assert!(widget.credentials.sync(&widget.engine));
            widget.structure_dirty = true;
            layout_at(&mut widget, 390.0, 700.0);
            if id.ends_with("open") {
                assert!(widget.credentials.open);
                let panel = widget.anchored_panel.as_ref().unwrap();
                assert!(
                    panel.traps_focus(),
                    "modal must enable scoped host Tab navigation"
                );
                assert!(panel.get_rect().x1 <= widget.rect.x1);
                assert!(widget.modal_capture().is_some());
            }
        }
        assert!(!widget.credentials.open);
        assert!(widget.modal_capture().is_none());
    }

    // Phase `narrow_chrome_keeps_a_nonzero_page_viewport`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_set_viewport_width(390);
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.sync(&state, &root);
        let child = widget.build_layout(&mut tree, &theme, &mut nodes);
        widget.sync(&state, &root);
        let layout_root = tree
            .new_with_children(
                Style {
                    size: Size::from_lengths(390.0, 844.0),
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_nodes = nodes.clone();
        tree.compute_layout_with_measure(
            layout_root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(390.0),
                height: taffy::AvailableSpace::Definite(844.0),
            },
            |known, available, node, _, _| {
                measure_nodes
                    .get(&node)
                    .map_or(taffy::Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, &mut text)
                    })
            },
        )
        .unwrap();
        widget.apply_layout(&tree, child, Point::ORIGIN);
        let viewport = widget.find_rect("browser.surface").unwrap();
        let address = widget.find_rect(URL_INPUT_ID).unwrap();
        assert!(viewport.width() > 0.0 && viewport.height() > 0.0);
        assert!(viewport.y0 < 100.0, "viewport starts beneath the toolbar");
        assert!(
            address.width() >= 140.0,
            "compact chrome preserves a usable address/search field: {}",
            address.width()
        );
    }

    // Phase `toolbar_surface_controls_advertise_the_action_their_labels_promise`.
    {
        fn address_for(widget: &BrowserWidget, control: &str) -> ui::tilo_actions::ActionAddress {
            ui::widget::controls::control_action_bindings(widget)
                .into_iter()
                .find_map(|binding| match binding.kind {
                    ui::widget::ControlBindingKind::Action(address)
                        if binding.control_id == control =>
                    {
                        Some(address)
                    }
                    _ => None,
                })
                .unwrap()
        }

        let mut widget = BrowserWidget::new_headless();
        let doc = AutoCommit::new();
        widget.sync(&doc, &automerge::ROOT);
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert_eq!(
            address_for(&widget, "browser.library").action,
            "show-history"
        );
        super::architecture_tests::activate_overflow_command(&mut widget, "Settings");
        assert!(widget.run_pending_commands());
        assert!(
            widget
                .pending_controls
                .iter()
                .any(
                    |invocation| invocation.address.resource == crate::resource::Browser::KIND
                        && invocation.address.action == "show-settings"
                ),
            "the actual Settings menu command queues its canonical action"
        );

        widget.show_library = true;
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert_eq!(
            address_for(&widget, "browser.library").action,
            "show-page",
            "a control labelled Close browser library must close it"
        );

        widget.show_library = false;
        widget.show_settings = true;
        widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        assert_eq!(address_for(&widget, "settings.close").action, "show-page");
    }

    // Phase `loading_progress_keeps_toolbar_above_the_page_surface`.
    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_loading();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut tree = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        widget.sync(&state, &root);
        let child = widget.build_layout(&mut tree, &theme, &mut nodes);
        widget.sync(&state, &root);
        let layout_root = tree
            .new_with_children(
                Style {
                    size: Size::from_lengths(900.0, 620.0),
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_nodes = nodes.clone();
        tree.compute_layout_with_measure(
            layout_root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(900.0),
                height: taffy::AvailableSpace::Definite(620.0),
            },
            |known, available, node, _, _| {
                measure_nodes
                    .get(&node)
                    .map_or(taffy::Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, &mut text)
                    })
            },
        )
        .unwrap();
        widget.apply_layout(&tree, child, Point::ORIGIN);
        let toolbar = widget.find_rect("browser.stop").unwrap();
        let viewport = widget.find_rect("browser.surface").unwrap();
        assert!(toolbar.height() > 0.0);
        assert!(
            viewport.y0 > toolbar.y1,
            "loading must reserve a strip below the toolbar, not let WebView cover it"
        );
    }
}

/// Site panel retention, composition, narrow rendering and resource actions.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn site_panel_stays_retained_composed_and_actionable() {
    // Phase `site_panel_remains_in_workspace_tree_across_sync`.
    {
        let mut widget = BrowserWidget::new_headless();
        layout_at(&mut widget, 800.0, 588.0);
        widget.site.open = true;
        let window = crate::BrowserWindow::new(widget, false);
        let mut harness =
            ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(window), (800, 632));
        for _ in 0..3 {
            harness.relayout();
        }
        assert!(harness.targets().contains_key("browser.site.pages"));
        assert!(harness.targets().contains_key("browser.site.close"));
    }

    // Phase `site_panel_composes_above_page_across_repeated_layouts`.
    {
        let mut widget = BrowserWidget::new_headless();
        layout_at(&mut widget, 800.0, 560.0);
        widget.site.open = true;
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let presence = PresenceCtx::default();
        for pass in 0..3 {
            layout_at(&mut widget, 800.0, 560.0);
            let panel = widget.anchored_panel.as_ref().unwrap();
            assert!(panel.get_rect().height() > 100.0);
            let mut scene = Scene::new();
            widget.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            let page_tags = scene.encoding().draw_tags.len();
            widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            assert!(
                scene.encoding().draw_tags.len() > page_tags + 5,
                "overlay must emit visible content"
            );
            if let Some(directory) = std::env::var_os("TILO_SITE_RENDER_EVIDENCE") {
                let path = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&path).unwrap();
                let mut renderer = ui::offscreen::OffscreenRenderer::new(800, 560);
                renderer.render_png(
                    &scene,
                    theme.tokens.colors.surfaces.content,
                    path.join(format!("panel-pass-{pass}.png")),
                );
            }
        }
    }

    // Phase `populated_site_panels_render_at_narrow_width`.
    {
        let mut widget = BrowserWidget::new_headless();
        let (mut doc, root) = synced(&mut widget);
        widget.site.navigation("https://example.com");
        widget.site.open = true;
        widget.site.sync(&widget.engine);
        assert!(
            widget
                .site
                .apply(WebViewEvent::SiteStatus(ui::webview::SiteStatus {
                    request_id: 1,
                    origin: "https://example.com".into(),
                    generation: 2,
                    blocked: 4,
                    global_enabled: true,
                    excepted: false,
                    effective: true,
                    security: "secure".into(),
                    certificate_status: 0,
                    tls_version: 6,
                    content_status: 0,
                    subject: "example.com".into(),
                    issuer: "Cloudflare TLS Issuing ECC CA 3".into(),
                    expires: 13_437_613_041_000_000.0,
                    permissions: ["camera", "microphone", "location", "notifications"]
                        .into_iter()
                        .map(|name| (name.into(), "ask".into()))
                        .collect(),
                    can_reset: true,
                }))
        );
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let presence = PresenceCtx::default();
        for information in [false, true] {
            layout_at(&mut widget, 390.0, 700.0);
            // Match the retained app layout after its first parent size is known.
            layout_at(&mut widget, 390.0, 700.0);
            if information {
                assert!(
                    widget
                        .start_encoded(
                            OperationKind::View,
                            "tilo.ui.segmented.select",
                            &serde_json::to_vec(
                                &serde_json::json!({"control":"browser.site.pages", "index":1})
                            )
                            .unwrap(),
                            &mut doc,
                            &root
                        )
                        .unwrap()
                        .is_some()
                );
                widget.site.sync(&widget.engine);
                layout_at(&mut widget, 390.0, 700.0);
            }
            let rect = widget.anchored_panel.as_ref().unwrap().get_rect();
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= 390.0 && rect.height() > 100.0,
                "{rect:?}"
            );
            let mut scene = Scene::new();
            widget.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            if let Some(directory) = std::env::var_os("TILO_SITE_RENDER_EVIDENCE") {
                let path = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&path).unwrap();
                let mut renderer = ui::offscreen::OffscreenRenderer::new(390, 700);
                let name = if information {
                    "narrow-site-info.png"
                } else {
                    "narrow-protection.png"
                };
                renderer.render_png(
                    &scene,
                    theme.tokens.colors.surfaces.content,
                    path.join(name),
                );
            }
        }
    }

    // Phase `site_panel_resource_actions_reach_the_retained_panel`.
    {
        let mut widget = BrowserWidget::new_headless();
        let (mut doc, root) = synced(&mut widget);
        layout_at(&mut widget, 1000.0, 700.0);
        widget.site.open = true;
        layout_at(&mut widget, 1000.0, 700.0);
        assert!(widget.anchored_panel.is_some());
        let result = widget
            .start_encoded(
                OperationKind::Effect,
                "tilo.ui.control.invoke",
                &serde_json::to_vec("browser.site.close").unwrap(),
                &mut doc,
                &root,
            )
            .unwrap();
        assert!(
            result.is_some(),
            "native button operation must reach overlay child"
        );
        widget.site.sync(&widget.engine);
        assert!(!widget.site.open);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the real panel action and ordered asynchronous replies in one regression fixture."
)]
fn site_protection_reload_updates_chrome_only_after_correlated_success() {
    // Keep engine replies under test control: TestEngine records commands,
    // while this queue models the native helper's asynchronous event order.
    struct QueuedEngine {
        events: std::sync::Mutex<Vec<WebViewEvent>>,
        capture: Arc<TestEngine>,
    }
    impl WebViewEngine for QueuedEngine {
        fn send(&self, command: WebViewCommand) {
            self.capture.send(command);
        }
        fn frames(&self) -> ui::webview::FrameSlot {
            self.capture.frames()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    const ORIGIN: &str = "https://protection.test";
    const URL: &str = "https://protection.test/page";
    const GENERATION: u64 = 3;
    for reload_succeeds in [false, true] {
        let capture = TestEngine::new();
        let engine = Arc::new(QueuedEngine {
            events: std::sync::Mutex::new(Vec::new()),
            capture: capture.clone(),
        });
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let (mut doc, root) = synced(&mut widget);
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::UriChanged(URL.into()));
        widget.apply_engine_events();
        widget.set_loading_progress(0.2);
        widget
            .perform_browser_operation(BrowserOperation::Stop)
            .unwrap();
        widget
            .daily
            .act(crate::daily_controls::Action::Open, &widget.engine);
        widget.daily.buffer_text("existing match");
        widget.daily.sync(&widget.engine);
        let find_id = capture
            .commands()
            .into_iter()
            .find_map(|command| match command {
                WebViewCommand::Find {
                    id, next: false, ..
                } => Some(id),
                _ => None,
            })
            .expect("Find has an active native request");
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::FindResult {
                id: find_id,
                count: 4,
                active: 2,
                final_update: true,
            });
        widget.apply_engine_events();

        widget.site.open = true;
        widget.sync(&doc, &root);
        let status_request = capture
            .commands()
            .into_iter()
            .rev()
            .find_map(|command| match command {
                WebViewCommand::RequestSiteStatus { request_id, .. } => Some(request_id),
                _ => None,
            })
            .expect("open panel requests native site status");
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::SiteStatus(ui::webview::SiteStatus {
                request_id: status_request,
                origin: ORIGIN.into(),
                generation: GENERATION,
                blocked: 4,
                global_enabled: true,
                excepted: false,
                effective: true,
                security: "secure".into(),
                certificate_status: 0,
                tls_version: 6,
                content_status: 0,
                subject: String::new(),
                issuer: String::new(),
                expires: 0.0,
                permissions: Vec::new(),
                can_reset: true,
            }));
        widget.apply_engine_events();
        layout_at(&mut widget, 1000.0, 700.0);
        assert!(widget.anchored_panel.is_some());
        assert!(
            widget
                .start_encoded(
                    OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec("browser.site.toggle").unwrap(),
                    &mut doc,
                    &root
                )
                .unwrap()
                .is_some(),
            "the real protection button must accept the action"
        );
        widget.sync(&doc, &root);
        let toggle_request = capture
            .commands()
            .into_iter()
            .rev()
            .find_map(|command| match command {
                WebViewCommand::SiteAction {
                    request_id,
                    origin,
                    generation,
                    action,
                } if action == "disable" => {
                    assert_eq!(origin, ORIGIN);
                    assert_eq!(generation, GENERATION);
                    Some(request_id)
                }
                _ => None,
            })
            .expect("the panel sends an origin-scoped protection change");
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::SiteResult {
                request_id: toggle_request,
                origin: ORIGIN.into(),
                generation: GENERATION,
                action: "disable".into(),
                success: true,
                message: "Changed".into(),
            });
        widget.sync(&doc, &root);
        widget.sync(&doc, &root);
        let reload_request = capture
            .commands()
            .into_iter()
            .rev()
            .find_map(|command| match command {
                WebViewCommand::SiteAction {
                    request_id,
                    origin,
                    generation,
                    action,
                } if action == "reload" => {
                    assert_eq!(origin, ORIGIN);
                    assert_eq!(generation, GENERATION);
                    Some(request_id)
                }
                _ => None,
            })
            .expect("the accepted protection change submits a scoped reload");

        let reply = |request_id, success| WebViewEvent::SiteResult {
            request_id,
            origin: ORIGIN.into(),
            generation: GENERATION,
            action: "reload".into(),
            success,
            message: "Reload result".into(),
        };
        let assert_stopped = |widget: &BrowserWidget| {
            assert!(
                widget.loading_stopped,
                "an unaccepted reload must not resume loading chrome"
            );
            assert!((widget.progress - 1.0).abs() < f64::EPSILON);
            assert!(widget.daily.open, "an unaccepted reload must preserve Find");
        };
        assert_stopped(&widget);
        engine
            .events
            .lock()
            .unwrap()
            .push(reply(reload_request + 1, true));
        widget.apply_engine_events();
        assert_stopped(&widget);
        assert!(!capture.commands().contains(&WebViewCommand::StopFinding));

        engine
            .events
            .lock()
            .unwrap()
            .push(reply(reload_request, reload_succeeds));
        widget.apply_engine_events();
        if reload_succeeds {
            assert!(!widget.loading_stopped);
            assert!(widget.progress.abs() < f64::EPSILON);
            assert!(!widget.daily.open);
            assert!(capture.commands().contains(&WebViewCommand::StopFinding));
            engine.events.lock().unwrap().extend([
                WebViewEvent::UriChanged(URL.into()),
                WebViewEvent::LoadProgress(0.4),
            ]);
            widget.apply_engine_events();
            assert!(
                (widget.progress - 0.4).abs() < f64::EPSILON,
                "same-URL reload progress must resume after Stop"
            );
            engine
                .events
                .lock()
                .unwrap()
                .push(reply(reload_request, true));
            widget.apply_engine_events();
            assert!(
                (widget.progress - 0.4).abs() < f64::EPSILON,
                "a repeated reply must not restart progress"
            );
            assert!(!widget.daily.open);
        } else {
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::LoadProgress(0.4));
            widget.apply_engine_events();
            assert_stopped(&widget);
            assert!(!capture.commands().contains(&WebViewCommand::StopFinding));
        }
        assert!(
            !capture.commands().contains(&WebViewCommand::Reload),
            "chrome reset must not send a second unscoped reload"
        );
    }
}

/// Picture-in-picture feedback placement and lifetime.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pip_feedback_is_anchored_clamped_and_settles() {
    // The monotonic clock is pinned, so a feedback note cannot lapse while
    // a loaded machine runs the phase.
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        // Phase `narrow_pip_feedback_stays_inside_the_window_beside_the_overflow_menu`.
        {
            let mut widget = BrowserWidget::new_headless();
            widget.debug_set_viewport_width(390);
            widget.debug_show_empty_pip_feedback();
            layout_at(&mut widget, 390.0, 844.0);
            layout_at(&mut widget, 390.0, 844.0);

            let rect = widget.anchored_panel.as_ref().unwrap().get_rect();
            assert!(rect.width() > 0.0);
            assert!(rect.x0 >= 0.0 && rect.x1 <= 390.0, "{rect:?}");
            let overflow = widget.find_rect(toolbar::OVERFLOW_ID).unwrap();
            assert!(
                rect.y0 >= overflow.y1,
                "the note opens below the control that holds the command"
            );

            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            widget.draw_overlay(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert!(
                !scene.encoding().draw_tags.is_empty(),
                "compact feedback must remain visible even without a PiP toolbar button"
            );
        }

        // Phase `pip_feedback_needs_no_animation_frames_and_wakes_at_its_expiry`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now(),
                persistent: false,
            });
            assert!(!w.needs_animation_frame());
            assert!(w.next_wake_deadline().is_some());
            w.pip_feedback = Some(PipFeedback::persistent(PipFeedbackKind::NoVideo));
            assert!(w.next_wake_deadline().is_none());
        }

        // Phase `actionable_pip_feedback_does_not_expire_under_the_pointer`.
        {
            let mut w = BrowserWidget::new_headless();
            w.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now(),
                persistent: false,
            });
            layout_at(&mut w, 800.0, 600.0);
            layout_at(&mut w, 800.0, 600.0);
            let rect = w.anchored_panel.as_ref().unwrap().get_rect();
            w.pip_feedback = Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now()
                    .checked_sub(PIP_FEEDBACK_LIFETIME + std::time::Duration::from_secs(1))
                    .unwrap_or_else(ui::time::Instant::now),
                persistent: false,
            });
            w.cursor = rect.center();
            w.apply_panel_events();
            assert!(w.pip_feedback.is_some());

            w.cursor = Point::ZERO;
            w.apply_panel_events();
            assert!(w.pip_feedback.is_none());
        }
    });
}

/// Picture-in-picture video probing, no-video fallback and host path.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pip_floats_the_video_or_falls_back_to_shell_selection() {
    // The monotonic clock is pinned, so a feedback note cannot lapse while
    // a loaded machine runs the phase.
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        // Phase `pip_button_probes_and_floats_only_the_video_rect`.
        {
            let engine = TestEngine::new();
            engine.set_media_element(Some(MediaElementInfo {
                element_id: "media:main".into(),
                viewport_rect: [0.1, 0.2, 0.5, 0.4],
                intrinsic_size: [1920, 1080],
                label: "The active film".into(),
            }));
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            let (mut state, root) = synced(&mut w);
            w.rect = Rect::new(100.0, 50.0, 900.0, 650.0);
            *w.content_rect.lock().unwrap() = Some([0.0, 0.1, 1.0, 1.0]);

            w.begin_pip_probe();
            w.sync(&state, &root); // accepts/sends the asynchronous probe
            assert!(
                engine
                    .commands()
                    .iter()
                    .any(|command| matches!(command, WebViewCommand::RequestMediaElement { .. }))
            );
            let request = w
                .take_floating_surface_request()
                .expect("a visible video becomes PiP");
            assert_eq!(request.title, "The active film");
            assert_eq!(request.aspect_ratio, Some(16.0 / 9.0));
            let ui::floating_surface::FloatingSource::AppElement {
                element_id, region, ..
            } = request.source
            else {
                panic!("browser PiP should preserve its semantic media identity");
            };
            assert_eq!(element_id, "media:main");
            let expected = Rect::new(180.0, 218.0, 580.0, 434.0);
            assert!((region.x0 - expected.x0).abs() < 0.001);
            assert!((region.y0 - expected.y0).abs() < 0.001);
            assert!((region.x1 - expected.x1).abs() < 0.001);
            assert!((region.y1 - expected.y1).abs() < 0.001);
            assert_eq!(w.active_floating_surface_ids(), vec![PIP_ID.to_string()]);

            w.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                    id: PIP_ID.into(),
                    rect: [0.0, 0.0, 1.0, 1.0],
                },
            );
            w.rect = Rect::new(200.0, 80.0, 1_200.0, 780.0);
            w.refresh_active_pip_region();
            let updates = w.take_floating_surface_source_updates();
            assert_eq!(
                updates.len(),
                1,
                "a resized browser republishes the video box"
            );
            assert_ne!(updates[0].1, expected);

            engine.set_media_element(Some(MediaElementInfo {
                element_id: "media:main".into(),
                viewport_rect: [0.18, 0.12, 0.46, 0.36],
                intrinsic_size: [1920, 1080],
                label: "The active film".into(),
            }));
            w.next_pip_refresh_at = Some(
                ui::time::now()
                    .checked_sub(std::time::Duration::from_millis(1))
                    .unwrap_or_else(ui::time::Instant::now),
            );
            w.poll_active_pip_element();
            assert!(engine.commands().iter().any(|command| matches!(
                command,
                WebViewCommand::RequestMediaElementById { element_id, .. }
                    if element_id == "media:main"
            )));
            w.apply_engine_events();
            let semantic_updates = w.take_floating_surface_source_updates();
            assert_eq!(semantic_updates.len(), 1);
            assert_ne!(semantic_updates[0].1, updates[0].1);

            w.begin_pip_probe();
            assert!(
                w.active_floating_surface_ids().is_empty(),
                "the toolbar button toggles an adopted PiP off instead of probing twice"
            );

            w.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: PIP_ID.into() },
            );
            assert!(w.active_pip_element.is_none());
        }

        // Phase `pip_without_video_explains_the_shell_selection_fallback`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine, true);
            let (mut state, root) = synced(&mut w);

            w.begin_pip_probe();
            w.sync(&state, &root);
            w.sync(&state, &root);

            assert!(w.take_floating_surface_request().is_none());
            assert_eq!(
                w.pip_feedback.as_ref().map(|feedback| feedback.kind),
                Some(PipFeedbackKind::NoVideo)
            );
        }

        // Phase `no_video_select_area_requests_the_shared_shell_pip_picker_once`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            w.set_pip_feedback(Some(PipFeedback {
                kind: PipFeedbackKind::NoVideo,
                started: ui::time::now(),
                persistent: false,
            }));
            layout_at(&mut w, 1000.0, 700.0);
            let (tree, node) = layout_keeping_tree(&mut w, 1000.0, 700.0, &Theme::dark());
            let choose = w
                .find_rect("browser.pip.select-area")
                .expect("shared selection control");
            assert!(
                click_at(&mut w, &tree, node, choose.center()),
                "the real selection control handles its complete click"
            );
            let invocation = ui::widget::controls::take_control_invocation(&mut w)
                .expect("pointer emits its canonical control invocation");
            assert_eq!(
                invocation.address.resource,
                "tilo.control:browser.pip.select-area"
            );
            assert_eq!(invocation.address.action, "invoke");
            let (mut state, root) = synced(&mut w);
            assert!(
                w.start_encoded(
                    OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec("browser.pip.select-area").unwrap(),
                    &mut state,
                    &root
                )
                .unwrap()
                .is_some()
            );
            w.sync(&state, &root);
            assert_eq!(
                w.take_capture_studio_request(),
                Some(ui::widget::CaptureMode::Pip)
            );
            assert!(w.take_capture_studio_request().is_none());
            assert!(w.take_floating_surface_request().is_none());
            assert!(w.pip_feedback.is_none());
        }

        // Phase `no_video_fallback_floats_and_tracks_the_page_viewport`.
        {
            let engine = TestEngine::new();
            let mut w = BrowserWidget::with_engine(engine.clone(), true);
            w.rect = Rect::new(100.0, 50.0, 900.0, 650.0);
            *w.content_rect.lock().unwrap() = Some([0.0, 0.1, 1.0, 1.0]);
            w.title = "Reference board".into();

            w.float_current_page();
            let request = w
                .take_floating_surface_request()
                .expect("the actionable no-video card creates a real live mirror");
            assert_eq!(request.title, "Reference board");
            let ui::floating_surface::FloatingSource::AppRegion { region, .. } = request.source
            else {
                panic!("page fallback should track the page viewport, not invent a media element");
            };
            let expected = Rect::new(100.0, 110.0, 900.0, 650.0);
            assert!((region.x0 - expected.x0).abs() < 0.001);
            assert!((region.y0 - expected.y0).abs() < 0.001);
            assert!((region.x1 - expected.x1).abs() < 0.001);
            assert!((region.y1 - expected.y1).abs() < 0.001);
            assert!(w.active_pip && w.pip_page_fallback);

            w.rect = Rect::new(200.0, 80.0, 1_200.0, 780.0);
            w.refresh_active_pip_region();
            let updates = w.take_floating_surface_source_updates();
            assert_eq!(
                updates.len(),
                1,
                "a resized page keeps its live crop current"
            );
            assert_ne!(updates[0].1, region);

            w.poll_active_pip_element();
            assert!(
                !engine.commands().iter().any(|command| matches!(
                    command,
                    WebViewCommand::RequestMediaElementById { .. }
                )),
                "page fallback must not be closed by semantic-video polling"
            );
            w.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: PIP_ID.into() },
            );
            assert!(!w.active_pip && !w.pip_page_fallback);
        }

        // Phase `active_pip_closes_when_its_semantic_element_disappears`.
        {
            let mut w = BrowserWidget::with_engine(TestEngine::new(), true);
            let (mut state, root) = synced(&mut w);
            w.active_pip = true;
            w.active_pip_element = Some(MediaElementInfo {
                element_id: "media:gone".into(),
                viewport_rect: [0.1, 0.1, 0.8, 0.6],
                intrinsic_size: [1_280, 720],
                label: "Temporary video".into(),
            });
            w.next_pip_refresh_at = Some(
                ui::time::now()
                    .checked_sub(std::time::Duration::from_millis(1))
                    .unwrap_or_else(ui::time::Instant::now),
            );

            w.poll_active_pip_element();
            w.apply_engine_events();

            assert!(!w.active_pip);
            assert!(w.active_pip_element.is_none());
            assert!(w.active_floating_surface_ids().is_empty());
            assert!(w.pip_feedback.is_some());
        }

        // Phase `picture_in_picture_host_path_waits_for_engine_success_or_error`.
        {
            let engine = TestEngine::new();
            engine.set_media_element(Some(MediaElementInfo {
                element_id: "media:host".into(),
                viewport_rect: [0.1, 0.1, 0.6, 0.5],
                intrinsic_size: [1280, 720],
                label: "Host video".into(),
            }));
            let mut w = BrowserWidget::with_engine(engine, true);
            let (mut doc, root) = synced(&mut w);
            w.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
            let resources = [w.published()];
            let page = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == "browser.page")
                .unwrap();
            let pip = page
                .actions
                .iter()
                .find(|action| action.id == "picture-in-picture")
                .unwrap();
            assert!(matches!(
                pip.contract.execution,
                ui::tilo_actions::ExecutionMode::Background { cancellable: false }
            ));
            let invocation = ui::tilo_actions::InvocationId::new();
            assert_eq!(
                w.admit(invocation, BrowserOperation::TogglePictureInPicture)
                    .map(Some)
                    .unwrap(),
                Some(ui::framework::Effected::Pending)
            );
            assert!(w.poll_invocation(invocation).is_none());
            w.sync(&doc, &root);
            let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
                w.poll_invocation(invocation)
            else {
                panic!("media acceptance must terminally succeed the admitted invocation");
            };

            let mut unavailable = BrowserWidget::with_engine(TestEngine::new(), true);
            let (mut doc, root) = synced(&mut unavailable);
            let invocation = ui::tilo_actions::InvocationId::new();
            assert!(matches!(
                unavailable
                    .admit(invocation, BrowserOperation::TogglePictureInPicture)
                    .map(Some)
                    .unwrap(),
                Some(ui::framework::Effected::Pending)
            ));
            unavailable.sync(&doc, &root);
            assert!(matches!(
                unavailable.poll_invocation(invocation),
                Some(ui::tilo_actions::InvocationUpdate::Failed { .. })
            ));
        }
    });
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn engine_surface_and_session_restore_keep_truthful_page_identity() {
    // Phase `headless_engine_has_deterministic_page`.
    {
        let w = BrowserWidget::new_headless();
        let slot = w.engine.frames();
        let frame = slot.lock().unwrap().clone().expect("pre-rendered frame");
        assert!(frame.width > 0 && frame.height > 0);
    }

    // Phase `default_engine_failure_never_becomes_a_simulated_page`.
    {
        let (engine, failed) =
            engine_or_error(Err("Could not start Chromium at /x: denied".into()));
        assert!(failed);
        let _clone: SharedEngine = Arc::clone(&engine);
        engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
        assert!(engine.frames().lock().unwrap().is_none());
        // The person reads what happened; the cause stays in the log.
        assert!(engine.drain_events().iter().any(|event| matches!(event,
            WebViewEvent::LoadFailed { error, .. }
                if error == ui::webview::chromium::runtime::FAILED_MESSAGE)));
    }

    // Phase `the_page_surface_is_named_after_the_page_title`.
    {
        fn surface_name(widget: &dyn Widget) -> Option<String> {
            if widget.test_id() == Some("browser.surface") {
                return widget.accessibility().and_then(|node| node.name);
            }
            widget
                .children()
                .iter()
                .find_map(|child| surface_name(*child))
        }
        let mut w = BrowserWidget::new_headless();
        layout_at(&mut w, 900.0, 600.0);
        layout_at(&mut w, 900.0, 600.0);
        let composed = w.composed.element().expect("composed browser");
        assert_eq!(
            surface_name(composed).as_deref(),
            Some(format!("{SHOTS_URL} — test page").as_str()),
            "assistive technology reads the loaded page by its title"
        );
    }

    // Phase `an_engine_that_is_not_running_says_so_and_offers_no_download`.
    {
        let engine = unavailable_engine(BrowsingMode::Normal);
        engine.send(WebViewCommand::Navigate("https://tilo.so".into()));
        let message = engine
            .drain_events()
            .into_iter()
            .find_map(|event| match event {
                WebViewEvent::LoadFailed { error, .. } => Some(error),
                _ => None,
            })
            .expect("navigation answers with the problem");
        assert_eq!(message, ui::webview::chromium::runtime::FAILED_MESSAGE);
        for developer_word in ["build-host", "TILO_", "python", "Chromium"] {
            assert!(!message.contains(developer_word), "{message}");
        }

        let mut w = BrowserWidget::with_engine(engine, false);
        w.engine_failed = true;
        let _ = synced(&mut w);
        let resources = [w.published()];
        let page = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == "browser.page")
            .unwrap();
        assert!(
            !page
                .actions
                .iter()
                .any(|action| action.id.contains("engine")),
            "the engine installs with Browser; the page offers no download of it"
        );
        layout_at(&mut w, 900.0, 600.0);
        assert!(
            !ui::widget::controls::control_action_bindings(&w)
                .iter()
                .any(|binding| binding.control_id.contains("engine")),
            "the page area explains, it has no download button"
        );
    }

    // Phase `resource_surface_binds_typed_navigation_and_reports_history_availability`.
    {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut doc, root) = synced(&mut w);
        let resources = [w.published()];
        assert_eq!(resources[0].id.0, crate::resource::Browser::KIND);
        for expected in [
            "browser.page",
            "browser.history",
            "browser.bookmarks",
            "browser.downloads",
            "browser.extensions",
            "browser.library",
            "browser.settings",
        ] {
            assert!(
                resources[0]
                    .children
                    .iter()
                    .any(|child| child.id.0 == expected)
            );
        }
        let page = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.page")
            .unwrap();
        assert!(
            page.actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "the page row offers only what this window does"
        );
        let back = page.actions.iter().find(|a| a.id == "back").unwrap();
        assert!(!back.contract.availability.is_available());
        let navigate = page.actions.iter().find(|a| a.id == "navigate").unwrap();
        assert_eq!(navigate.params()[0].name.as_ref(), "url");
        // Another page is a desktop tab: "Open in new tab" asks the
        // desktop and leaves this window on its own page.
        let open_tab = page.actions.iter().find(|a| a.id == "open-tab").unwrap();
        assert!(
            open_tab
                .params()
                .iter()
                .any(|param| param.name.as_ref() == "url")
        );
        assert!(open_tab.contract.validate_declaration().is_empty());
        let mut args = serde_json::Map::new();
        args.insert("url".into(), serde_json::json!("example.test/next"));
        args.insert("background".into(), serde_json::json!(true));
        let operation = effect_operation(&open_tab.build_json(&args).unwrap().payload);
        assert!(matches!(
            &operation,
            BrowserOperation::OpenInNewTab { url, background: true } if url == "example.test/next"
        ));
        let _ = ui::shell_tabs::take_requests();
        let before = engine.commands().len();
        w.perform_browser_operation(operation).unwrap();
        assert_eq!(
            ui::shell_tabs::take_requests(),
            vec![ui::shell_tabs::TabRequest::OpenBeside {
                target: Some("https://example.test/next".into()),
                background: true,
            }]
        );
        assert!(
            !engine.commands()[before..]
                .iter()
                .any(|command| matches!(command, WebViewCommand::Navigate(_))),
            "this window keeps its page"
        );
        assert_eq!(
            navigate.contract.execution,
            ui::tilo_actions::ExecutionMode::Background { cancellable: true },
            "a caller can stop a navigation it no longer waits for"
        );
        assert!(navigate.contract.validate_declaration().is_empty());
        let settings = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.settings")
            .unwrap();
        let library = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.library")
            .unwrap();
        let filter = library
            .actions
            .iter()
            .find(|action| action.id == "set-library-filter")
            .unwrap();
        assert_eq!(filter.kind, OperationKind::View);
        assert_eq!(filter.params()[0].name.as_ref(), "query");
        assert!(filter.contract.validate_declaration().is_empty());
        assert!(
            settings
                .actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "the preferences are the resource's own document actions"
        );
        let mut args = serde_json::Map::new();
        args.insert("url".into(), serde_json::json!("example.test"));
        let encoded = navigate.build_json(&args).unwrap();
        let invocation = ui::tilo_actions::InvocationId::new();
        assert_eq!(
            w.admit(invocation, effect_operation(&encoded.payload))
                .map(Some)
                .unwrap(),
            Some(ui::framework::Effected::Pending)
        );
        assert_eq!(
            engine.current_uri().as_deref(),
            Some("https://example.test")
        );
        w.sync(&doc, &root);
        assert!(matches!(
            w.poll_invocation(invocation),
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ));

        let mut filter_args = serde_json::Map::new();
        filter_args.insert("query".into(), serde_json::json!("tilo example"));
        let encoded = filter.build_json(&filter_args).unwrap();
        let started = w
            .admit(
                ui::tilo_actions::InvocationId::new(),
                effect_operation(&encoded.payload),
            )
            .map(Some)
            .unwrap();
        assert!(matches!(started, Some(ui::framework::Effected::Done(_))));
        assert_eq!(w.library_filter.get(), "tilo example");
    }

    // Phase `stale_blank_completion_does_not_finish_restored_page`.
    {
        struct Events(
            std::sync::Mutex<Vec<WebViewEvent>>,
            std::sync::Arc<TestEngine>,
        );
        impl WebViewEngine for Events {
            fn send(&self, command: WebViewCommand) {
                self.1.send(command);
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.1.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.0.lock().unwrap())
            }
        }
        let engine = std::sync::Arc::new(Events(
            std::sync::Mutex::new(vec![
                WebViewEvent::UriChanged("https://example.test/saved".into()),
                WebViewEvent::LoadProgress(0.3),
                WebViewEvent::LoadFinishedAt {
                    uri: "about:blank".into(),
                },
            ]),
            TestEngine::new(),
        ));
        let mut widget = BrowserWidget::with_engine(engine.clone(), true)
            .with_initial_uri("https://example.test/saved".into());
        widget.title = "about:blank".into();
        widget.apply_engine_events();
        assert!(
            widget.title.is_empty(),
            "a new document must not inherit the blank page title"
        );
        assert!((widget.progress - 0.3).abs() < f64::EPSILON);
        engine.0.lock().unwrap().push(WebViewEvent::LoadFinishedAt {
            uri: "https://example.test/saved".into(),
        });
        widget.apply_engine_events();
        assert!((widget.progress - 1.0).abs() < f64::EPSILON);
    }

    // Phase `continuation_restore_navigates_the_matching_browser_instance`.
    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let (mut doc, root) = synced(&mut widget);
        let metadata = ui::collaboration::ContinuationMetadata {
            schema_version: ui::collaboration::semantic_schema::WEB_LOCATION_V1,
            bytes: br#"{"url":"https://example.test/reading"}"#.to_vec(),
        };
        assert!(widget.restore_continuation_metadata(&metadata, &mut Edits::lend(&mut doc), &root));
        assert_eq!(
            engine.current_uri().as_deref(),
            Some("https://example.test/reading")
        );
    }
}
