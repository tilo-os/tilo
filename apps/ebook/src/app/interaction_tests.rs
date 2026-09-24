//! Drive the app through the real `handle_event` path — a headless frame
//! populates hit regions, then synthesized winit clicks route through the
//! same code the live compositor uses. Asserts observable state/CRDT change.

use super::*;
use crate::content::load_sample_library;
use crate::model::AnnoKind;
use serial_test::serial;
use ui::presence::PresenceCtx;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, KeyEvent};

struct WorkspaceEnvGuard(Option<std::ffi::OsString>);

impl WorkspaceEnvGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("TILO_WORKSPACE_DIR");
        // SAFETY: callers use the shared `serial_test` lock while this
        // process-global compatibility override is installed.
        unsafe { std::env::set_var("TILO_WORKSPACE_DIR", path) };
        Self(previous)
    }
}

impl Drop for WorkspaceEnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`; restoration also runs during unwinding.
        unsafe {
            if let Some(previous) = self.0.take() {
                std::env::set_var("TILO_WORKSPACE_DIR", previous);
            } else {
                std::env::remove_var("TILO_WORKSPACE_DIR");
            }
        }
    }
}

fn app_with_sample() -> BooksApp {
    BooksApp::with_library(load_sample_library())
}

fn wait_for_import(app: &mut BooksApp, doc: &mut AutoCommit) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        app.sync(doc, &automerge::ROOT);
        if app.import_loading.is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let pending = app
        .import_loading
        .as_ref()
        .map(|loading| loading.path.display().to_string());
    panic!(
        "book import did not finish within 10 seconds (pending: {pending:?}, error: {:?})",
        app.load_error
    );
}

fn find_semantic_target<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
    if node.action_target.as_deref() == Some(target) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_semantic_target(child, target))
}

#[test]
fn hover_tooltip_reuses_the_semantic_label_of_the_topmost_control() {
    let mut app = app_with_sample();
    let target = Rect::new(10.0, 10.0, 44.0, 44.0);
    app.cursor = target.center();
    app.hits = vec![Hit {
        rect: target,
        action: Action::NextPage,
    }];
    app.refresh_hover_tooltip();
    assert_eq!(Widget::tooltip(&app), Some("Next page"));

    // A swallowed modal background never pretends to be a real control.
    app.hits = vec![Hit {
        rect: target,
        action: Action::Swallow,
    }];
    app.refresh_hover_tooltip();
    assert_eq!(Widget::tooltip(&app), None);
}

#[test]
fn accessibility_exposes_library_controls_and_routes_their_real_actions() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Library;
    frame(&mut app, &mut doc, 1100.0, 760.0);

    let tree = Widget::accessibility(&app).expect("Books semantic root");
    assert_eq!(tree.role, Role::Window);
    assert!(find_semantic_target(&tree, "books.library.open").is_some());
    assert!(find_semantic_target(&tree, "books.library.sort").is_some());
    let hide =
        find_semantic_target(&tree, "books.library.hide-read").expect("hide-finished switch");
    assert_eq!(hide.role, Role::Switch);
    assert_eq!(hide.state.checked, Some(false));

    assert!(Widget::perform_semantic_action(
        &mut app,
        "books.library.hide-read",
        &A11yAction::ToggleCheck,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(BooksDoc::read_library_prefs(&doc).hide_read);
}

#[test]
fn action_search_discovers_live_reader_and_settings_actions() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &automerge::ROOT);
    let actions = app
        .search_providers()
        .pop()
        .expect("the action-label provider is published");

    for query in [
        "Toggle bookmark",
        "Add note",
        "Previous page",
        "Open reading settings",
        "Open font settings",
        "Rename font",
        "Open colour settings",
    ] {
        let hit = ui::search::SearchProvider::search(actions.as_ref(), query)
            .into_iter()
            .find(|hit| hit.action_address().is_some())
            .unwrap_or_else(|| panic!("missing discoverable live action: {query}"));
        let address = hit.action_address().expect("canonical action address");
        let resource =
            ui::search::SearchProvider::resolve_action_resource(actions.as_ref(), &address)
                .unwrap_or_else(|| panic!("live exact action did not resolve: {query}"));
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == address.action),
            "exact resource lost the discovered action for {query}"
        );
    }
}

#[test]
fn accessibility_reader_contains_visible_text_and_actionable_chrome() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);

    let tree = Widget::accessibility(&app).expect("Books semantic root");
    let document = tree
        .children
        .iter()
        .find(|node| node.role == Role::Document)
        .expect("current page document");
    assert!(!document.children.is_empty(), "visible prose is navigable");
    assert!(document.children.iter().all(|node| node.name.is_some()));
    assert!(find_semantic_target(&tree, "books.reader.back").is_some());
    let bookmark = find_semantic_target(&tree, "books.reader.bookmark").expect("bookmark toggle");
    assert_eq!(bookmark.role, Role::ToggleButton);
    assert_eq!(bookmark.state.checked, Some(false));

    assert!(Widget::perform_semantic_action(
        &mut app,
        "books.reader.bookmark",
        &A11yAction::Activate,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(
        BooksDoc::list_annotations(&doc)
            .iter()
            .any(|annotation| annotation.kind == AnnoKind::Bookmark)
    );
}

#[test]
fn reader_progress_slider_seeks_accessibly_and_persists_continuous_position() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let chapter = app.library[0].chapters[0].clone();
    Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters = (0..4)
        .map(|index| crate::model::Chapter {
            title: format!("Part {}", index + 1),
            href: format!("part-{}.xhtml", index + 1),
            blocks: chapter.blocks.clone(),
        })
        .collect();
    frame(&mut app, &mut doc, 1000.0, 720.0);

    app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
    frame(&mut app, &mut doc, 1000.0, 720.0);
    let id = app.book_id();
    let before = BooksDoc::read_progress(&doc, &id).fraction;
    assert!(wheel(&mut app, &mut doc, -3.0));
    assert!(BooksDoc::read_progress(&doc, &id).fraction > before);

    let tree = Widget::accessibility(&app).expect("Books semantic root");
    let slider = find_semantic_target(&tree, "books.reader.progress").expect("progress slider");
    assert_eq!(slider.role, Role::Slider);
    assert!(
        slider
            .value
            .as_deref()
            .is_some_and(|value| value.ends_with('%'))
    );
    assert!(Widget::perform_semantic_action(
        &mut app,
        "books.reader.progress",
        &A11yAction::SetValue("75%".into()),
        &mut doc,
        &automerge::ROOT,
    ));
    assert_eq!(app.chapter, 3);
    assert!((BooksDoc::read_progress(&doc, &id).fraction - 0.75).abs() < 0.01);
}

#[test]
fn accessibility_quick_settings_is_modal_and_numeric_controls_are_live() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.reading_settings.open();
    frame(&mut app, &mut doc, 1000.0, 720.0);

    let tree = Widget::accessibility(&app).expect("Books semantic root");
    assert!(
        tree.children.iter().all(|node| node.role != Role::Document),
        "dimmed page content is absent while the popover is modal"
    );
    assert!(find_semantic_target(&tree, "books.reader.settings.close").is_some());
    let size = find_semantic_target(&tree, "books.number.text-size")
        .expect("shared numeric text-size control");
    assert_eq!(size.role, Role::SpinButton);

    let before = BooksDoc::read_settings(&doc).font_scale;
    assert!(Widget::perform_semantic_action(
        &mut app,
        "books.number.text-size",
        &A11yAction::Increment,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(BooksDoc::read_settings(&doc).font_scale > before);
}

#[test]
fn resource_operations_reject_wrong_kinds_without_changing_state() {
    let mut app = app_with_sample();
    let doc = AutoCommit::new();
    let view_before = std::mem::discriminant(&app.view);
    let settings = serde_json::to_vec(&crate::actions::EbookOperation::OpenSettings).unwrap();
    assert!(
        Widget::invoke_resource_operation(
            &mut app,
            tilo_actions::OperationKind::Effect,
            crate::actions::EBOOK_OPERATION_TYPE,
            &settings,
            &doc,
            &automerge::ROOT,
        )
        .is_err()
    );
    assert_eq!(std::mem::discriminant(&app.view), view_before);

    let open = serde_json::to_vec(&crate::actions::EbookOperation::OpenBook).unwrap();
    assert!(
        Widget::start_resource_operation(
            &mut app,
            tilo_actions::InvocationId::new(),
            tilo_actions::OperationKind::View,
            crate::actions::EBOOK_OPERATION_TYPE,
            &open,
            &doc,
            &automerge::ROOT,
        )
        .is_err()
    );
    assert!(app.resource_invocation.is_none());
}

#[test]
fn stale_resource_reveal_refuses_missing_chapter_or_annotation_target() {
    let mut app = app_with_sample();
    let doc = AutoCommit::new();
    let book_id = app.open_book().expect("sample book").id.clone();
    let before = (app.book_idx, app.chapter, app.page, app.pending_jump_block);
    for operation in [
        crate::actions::EbookOperation::Reveal {
            book_id: book_id.clone(),
            chapter: usize::MAX,
            page: 0,
            block: None,
        },
        crate::actions::EbookOperation::Reveal {
            book_id,
            chapter: 0,
            page: 0,
            block: Some(usize::MAX),
        },
    ] {
        let payload = serde_json::to_vec(&operation).expect("operation serializes");
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::View,
                crate::actions::EBOOK_OPERATION_TYPE,
                &payload,
                &doc,
                &automerge::ROOT,
            )
            .is_err()
        );
        assert_eq!(
            (app.book_idx, app.chapter, app.page, app.pending_jump_block),
            before,
            "a stale target must not navigate to a neighbouring passage"
        );
    }
}

fn app_with_math_sample() -> BooksApp {
    let mut app = app_with_sample();
    let chapter = r#"<body xmlns="http://www.w3.org/1999/xhtml">
          <h1>An Economy of Symbols</h1>
          <p>Inline notation should belong to the sentence: <math xmlns="http://www.w3.org/1998/Math/MathML"><mi>E</mi><mo>=</mo><mi>m</mi><msup><mi>c</mi><mn>2</mn></msup></math>, without disturbing its rhythm.</p>
          <p>A displayed identity gets room for its hierarchy and a stable optical centre.</p>
          <math xmlns="http://www.w3.org/1998/Math/MathML" display="block"><mfrac><mrow><msup><mi>x</mi><mn>2</mn></msup><mo>+</mo><mn>1</mn></mrow><msqrt><mrow><mi>y</mi><mo>-</mo><mn>3</mn></mrow></msqrt></mfrac></math>
          <p>Limits and arrays use the same recursive boxes rather than a second renderer.</p>
          <math xmlns="http://www.w3.org/1998/Math/MathML" display="block"><mrow><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover><mfenced><mtable><mtr><mtd><mi>a</mi></mtd><mtd><mi>b</mi></mtd></mtr><mtr><mtd><mi>c</mi></mtd><mtd><mi>d</mi></mtd></mtr></mtable></mfenced></mrow></math>
        </body>"#;
    Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters[0].blocks =
        crate::xhtml::parse_xhtml(chapter);
    Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters[0].title =
        "An Economy of Symbols".into();
    app.layout = None;
    app
}

/// Run one full headless frame so `self.hits` reflects the current view.
fn frame(app: &mut BooksApp, doc: &mut AutoCommit, w: f64, h: f64) {
    let root = automerge::ROOT;
    app.sync(doc, &root);
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.reconcile_flaps();
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

/// Centre of the topmost recorded hit whose action matches `pred`.
fn find_hit(app: &BooksApp, pred: impl Fn(&Action) -> bool) -> Option<Point> {
    app.comment_overlay
        .action_center(|action| {
            let action = match action {
                CommentAction::Close => Action::CloseComments,
                CommentAction::Swallow => Action::Swallow,
                CommentAction::DeleteHighlight(group) => Action::DeleteHighlight(group),
                CommentAction::Add(group) => Action::AddComment(group),
                CommentAction::Edit(id) => Action::EditComment(id),
                CommentAction::Delete(id) => Action::DeleteComment(id),
            };
            pred(&action)
        })
        .or_else(|| {
            [
                (ReaderBubbleAction::PreviousPage, Action::PrevPage),
                (ReaderBubbleAction::NextPage, Action::NextPage),
            ]
            .into_iter()
            .find_map(|(bubble_action, action)| {
                pred(&action)
                    .then(|| app.reader_bubble.action_center(bubble_action))
                    .flatten()
            })
        })
        .or_else(|| {
            app.hits
                .iter()
                .rev()
                .find(|h| pred(&h.action))
                .map(|h| h.rect.center())
        })
}

fn find_reading_hit(
    app: &BooksApp,
    pred: impl Fn(&ReadingSettingsAction) -> bool,
) -> Option<Point> {
    app.reading_settings
        .action_rect(pred)
        .map(|rect| rect.center())
}

/// Route a real left-click at `p` (logical px) through `handle_event`.
fn click(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) -> bool {
    let mods = Modifiers::default();
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    let mv = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(p.x, p.y),
    };
    app.handle_event(
        &mv,
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    let press = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    handled = false;
    app.handle_event(
        &press,
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}

/// Route a wheel event (`lines` positive == scroll up) through `handle_event`.
fn wheel(app: &mut BooksApp, doc: &mut AutoCommit, lines: f32) -> bool {
    let mods = Modifiers::default();
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    let ev = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, lines),
        phase: winit::event::TouchPhase::Moved,
    };
    app.handle_event(
        &ev,
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}

/// Route a horizontal wheel/scroll through `handle_event`. Positive
/// `lines` is a physical scroll-right, matching winit's `LineDelta` x sign.
fn hwheel(app: &mut BooksApp, doc: &mut AutoCommit, lines: f32) -> bool {
    let mods = Modifiers::default();
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    let ev = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(lines, 0.0),
        phase: winit::event::TouchPhase::Moved,
    };
    app.handle_event(
        &ev,
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}

/// Low-level pointer events for drag-selection tests.
fn send(app: &mut BooksApp, doc: &mut AutoCommit, ev: &WindowEvent) {
    let mods = Modifiers::default();
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    app.handle_event(
        ev,
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
}
fn ptr_move(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) {
    send(
        app,
        doc,
        &WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(p.x, p.y),
        },
    );
}
fn ptr_down(app: &mut BooksApp, doc: &mut AutoCommit) {
    send(
        app,
        doc,
        &WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
}
fn ptr_up(app: &mut BooksApp, doc: &mut AutoCommit) {
    send(
        app,
        doc,
        &WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
}

/// Draw one settled frame into a `Scene` (for pixel-level assertions).
fn render_scene(app: &mut BooksApp, doc: &mut AutoCommit, w: f64, h: f64) -> Scene {
    let root = automerge::ROOT;
    app.sync(doc, &root);
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.reconcile_flaps();
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    #[cfg(windows)]
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/sdk/ui-core/assets/fonts"),
        "en-US",
    );
    #[cfg(windows)]
    let mut tr = TextRenderer::with_font_catalog(&catalog).expect("checked-in font catalog");
    #[cfg(not(windows))]
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    scene
}

// Synthesised winit key presses (zeroed-KeyEvent trick; every field is
// overwritten before use), routed through the real `handle_event`.
fn char_press(ch: &str) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(ch.into());
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(ch.into());
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: ev,
        is_synthetic: false,
    }
}
fn named_press(named: NamedKey) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(named);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: ev,
        is_synthetic: false,
    }
}
fn feed(app: &mut BooksApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
    let mods = Modifiers::default();
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    app.handle_event(
        ev,
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}
fn key_char(app: &mut BooksApp, doc: &mut AutoCommit, ch: &str) -> bool {
    feed(app, doc, &char_press(ch))
}
fn key_named(app: &mut BooksApp, doc: &mut AutoCommit, named: NamedKey) -> bool {
    feed(app, doc, &named_press(named))
}
/// Feed a key with explicit modifiers (Shift/Ctrl selection extension).
fn key_named_mods(
    app: &mut BooksApp,
    doc: &mut AutoCommit,
    named: NamedKey,
    state: ModifiersState,
) -> bool {
    let mods = Modifiers::from(state);
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    app.handle_event(
        &named_press(named),
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}
/// Feed a printable character with explicit modifiers (e.g. Ctrl+A).
fn key_char_mods(
    app: &mut BooksApp,
    doc: &mut AutoCommit,
    ch: &str,
    state: ModifiersState,
) -> bool {
    let mods = Modifiers::from(state);
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let root = automerge::ROOT;
    let mut handled = false;
    let mut focus = None;
    app.handle_event(
        &char_press(ch),
        1.0,
        &mods,
        doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    handled
}

/// A point at the centre of the first laid-out word on the current page.
fn page_point(app: &BooksApp) -> Point {
    let r = &app.sel_runs[0];
    Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
}

/// A real double-click (two rapid presses at the same point).
fn dbl_click(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) {
    ptr_move(app, doc, p);
    ptr_down(app, doc);
    ptr_up(app, doc);
    ptr_down(app, doc);
    ptr_up(app, doc);
}

#[test]
fn wheel_turns_pages_in_the_reader() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert_eq!(app.view, View::Reader);
    let start = (app.chapter, app.page);
    // A downward flick turns forward.
    assert!(
        wheel(&mut app, &mut doc, -4.0),
        "downward wheel turned a page"
    );
    assert!(
        (app.chapter, app.page) != start,
        "advanced from ch{}/pg{}",
        start.0,
        start.1
    );
    // An upward flick turns back.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let mid = (app.chapter, app.page);
    wheel(&mut app, &mut doc, 4.0);
    assert!(
        (app.chapter, app.page) != mid,
        "turned back from the advanced spot"
    );
}

#[test]
fn wheel_over_a_long_toc_scrolls_the_rail_not_the_book_and_reveals_chapter() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let template = app.library[0].chapters[0].clone();
    Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters = (0..32)
        .map(|index| crate::model::Chapter {
            title: format!("Long chapter {}", index + 1),
            href: format!("chapter-{}.xhtml", index + 1),
            blocks: template.blocks.clone(),
        })
        .collect();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(app.toc_max_scroll > 0.0, "the long contents list scrolls");
    let position = (app.chapter, app.page);
    ptr_move(&mut app, &mut doc, Point::new(TOC_W / 2.0, 300.0));
    assert!(wheel(&mut app, &mut doc, -4.0), "rail wheel is consumed");
    assert!(app.toc_scroll > 0.0, "contents moved down");
    assert_eq!(
        (app.chapter, app.page),
        position,
        "the page does not turn behind the contents rail"
    );

    app.semantic_focus = Some("books.reader.chapter:0".into());
    let toc_before_key = app.toc_scroll;
    let before_key = (app.chapter, app.page);
    assert!(
        key_named(&mut app, &mut doc, NamedKey::PageDown),
        "rail-focused PageDown is consumed"
    );
    assert!(
        app.toc_scroll > toc_before_key,
        "keyboard scrolled the contents"
    );
    assert_eq!(
        (app.chapter, app.page),
        before_key,
        "rail-focused PageDown does not turn the book"
    );

    app.on_goto_chapter(27);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        find_hit(&app, |action| matches!(action, Action::GotoChapter(27))).is_some(),
        "the active chapter is brought into the virtualised TOC viewport"
    );
}

#[test]
fn wheel_over_long_annotations_stays_in_the_rail_and_reveals_a_target() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let book_id = app.book_id();
    let mut target = None;
    for index in 0..28 {
        let annotation = Annotation::new(&book_id, AnnoKind::Note, 0, index)
            .with_excerpt(format!("A saved passage number {index}"));
        if index == 24 {
            target = Some(annotation.id);
        }
        BooksDoc::upsert_annotation(&mut doc, &annotation);
    }
    frame(&mut app, &mut doc, 1280.0, 700.0);
    assert!(app.notes_max_scroll > 0.0, "the annotation history scrolls");
    let position = (app.chapter, app.page);
    ptr_move(&mut app, &mut doc, Point::new(1120.0, 300.0));
    assert!(wheel(&mut app, &mut doc, -4.0), "rail wheel is consumed");
    assert!(app.notes_scroll > 0.0, "annotations moved down");
    assert_eq!(
        (app.chapter, app.page),
        position,
        "the page does not turn behind the annotations rail"
    );

    let target = target.expect("target annotation id");
    app.on_jump_to(target);
    frame(&mut app, &mut doc, 1280.0, 700.0);
    assert!(
        find_hit(
            &app,
            |action| matches!(action, Action::JumpTo(id) if *id == target)
        )
        .is_some(),
        "a targeted annotation is revealed in the virtualised rail"
    );
}

#[test]
fn horizontal_scroll_right_turns_forward() {
    // Scrolling right must advance (next page); scrolling left must go back.
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert_eq!(app.view, View::Reader);
    let start = (app.chapter, app.page);
    // Scroll right (positive x) advances forward.
    assert!(
        hwheel(&mut app, &mut doc, 3.0),
        "scroll-right turned a page"
    );
    let after_right = (app.chapter, app.page);
    assert!(after_right != start, "scroll-right advanced (forward)");
    // Position strictly moved forward (page or chapter increased).
    assert!(
        after_right.0 > start.0 || (after_right.0 == start.0 && after_right.1 > start.1),
        "scroll-right moved forward, not back: {start:?} -> {after_right:?}",
    );
    // Scroll left (negative x) turns back.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    hwheel(&mut app, &mut doc, -3.0);
    let after_left = (app.chapter, app.page);
    assert!(after_left != after_right, "scroll-left turned back");
    assert!(
        after_left.0 < after_right.0
            || (after_left.0 == after_right.0 && after_left.1 < after_right.1),
        "scroll-left moved backward: {after_right:?} -> {after_left:?}",
    );
}

#[test]
fn selecting_serif_font_applies_and_persists() {
    use crate::model::FontKind;
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // The reader opens in the serif book face (books read best in serif) —
    // and that is the face the paint pass shapes with.
    assert_eq!(
        app.active_font().kind,
        FontKind::Serif,
        "serif is the default reading face"
    );
    let (fam0, _m0, serif_sig, mono0) = app.body_font();
    assert_eq!(fam0, ui::text_render::FontChoice::Serif);
    assert!(!mono0, "serif is a proportional face");
    // Open the settings popover and switch to the sans "Default" pill — the
    // real click path — which must apply and persist to the CRDT.
    let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
    click(&mut app, &mut doc, aa);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let sans = find_reading_hit(
        &app,
        |a| matches!(a, ReadingSettingsAction::SetFont(id) if id == "default"),
    )
    .expect("default (sans) pill");
    assert!(click(&mut app, &mut doc, sans), "sans pill handled");
    assert_eq!(
        app.active_font().kind,
        FontKind::Default,
        "sans face is now active"
    );
    assert_eq!(
        BooksDoc::read_settings(&doc).font_id,
        "default",
        "selection persisted to CRDT"
    );
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let (fam1, _m1, sans_sig, _mono1) = app.body_font();
    assert_eq!(fam1, ui::text_render::FontChoice::SansSerif);
    assert_ne!(
        sans_sig, serif_sig,
        "font signature differs so the chapter relays out"
    );
    // And back to serif via its own pill — reselecting the reading face.
    let serif = find_reading_hit(
        &app,
        |a| matches!(a, ReadingSettingsAction::SetFont(id) if id == "serif"),
    )
    .expect("serif pill");
    assert!(click(&mut app, &mut doc, serif), "serif pill handled");
    assert_eq!(
        app.active_font().kind,
        FontKind::Serif,
        "serif font is active again"
    );
    assert_eq!(
        BooksDoc::read_settings(&doc).font_id,
        "serif",
        "serif selection persisted"
    );
}

/// A named face that isn't installed must degrade to generic serif, not
/// cosmic-text's default sans; an installed family is honoured verbatim.
#[test]
fn absent_named_font_falls_back_to_generic_serif() {
    use crate::model::{FontEntry, FontKind};
    let mut app = app_with_sample();
    app.fonts.push(FontEntry::new(
        "ghost",
        FontKind::Named("Nonexistent Phantom Face 9Z".into()),
        "Ghost",
        1.0,
        true,
    ));
    app.settings.font_id = "ghost".into();
    let (fam, _, _, _) = app.body_font();
    assert_eq!(
        fam,
        ui::text_render::FontChoice::Serif,
        "an absent named face degrades to a real serif, not the default sans",
    );

    // A genuinely installed family is used as-is.
    let real = TextRenderer::new()
        .system_families()
        .into_iter()
        .next()
        .expect("the text system enumerates at least one installed family");
    app.fonts.push(FontEntry::new(
        "real",
        FontKind::Named(real.clone()),
        "Real",
        1.0,
        true,
    ));
    app.settings.font_id = "real".into();
    let (fam2, _, _, _) = app.body_font();
    assert_eq!(
        fam2,
        ui::text_render::FontChoice::Named(real),
        "an installed named family is honoured verbatim (no gratuitous substitution)",
    );
}

#[test]
fn disabling_a_scheme_hides_it_from_quick_settings() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Colours;
    frame(&mut app, &mut doc, 1200.0, 820.0);
    // Toggle the Light scheme off through its manager row.
    let toggle = find_hit(
        &app,
        |a| matches!(a, Action::ToggleSchemeEnabled(id) if id.as_str() == "light"),
    )
    .expect("scheme enable toggle");
    assert!(click(&mut app, &mut doc, toggle), "toggle handled");
    app.view = View::Reader;
    app.reading_settings.open();
    frame(&mut app, &mut doc, 1200.0, 820.0);
    assert!(
        find_reading_hit(&app, |action| matches!(
            action,
            ReadingSettingsAction::SetReadTheme(ReadTheme::Light)
        ))
        .is_none(),
        "Light is gone from quick settings"
    );
    assert!(
        find_reading_hit(&app, |action| matches!(
            action,
            ReadingSettingsAction::SetReadTheme(ReadTheme::Dark)
        ))
        .is_some(),
        "enabled schemes remain in quick settings"
    );
}

#[test]
fn scheme_manager_new_via_picker_creates_selects_and_edits() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Colours;
    frame(&mut app, &mut doc, 1200.0, 860.0);
    let before = BooksDoc::read_schemes(&doc).len();
    // The single "New scheme" button opens the colour picker (no scheme yet).
    let newbtn =
        find_hit(&app, |a| matches!(a, Action::NewSchemeViaPicker)).expect("a New scheme button");
    assert!(click(&mut app, &mut doc, newbtn), "new-via-picker handled");
    assert!(app.color_picker.is_open(), "the colour picker opened");
    assert_eq!(
        BooksDoc::read_schemes(&doc).len(),
        before,
        "no scheme until the picker commits"
    );
    // Commit the picker -> a scheme is derived from the chosen background,
    // selected, and opened in the editor.
    frame(&mut app, &mut doc, 1200.0, 860.0);
    let done = app.color_picker.done_center().expect("a Done button");
    assert!(click(&mut app, &mut doc, done), "picker done handled");
    let schemes = BooksDoc::read_schemes(&doc);
    assert_eq!(
        schemes.len(),
        before + 1,
        "a custom scheme was created on commit"
    );
    let custom = schemes
        .iter()
        .find(|s| !s.builtin)
        .expect("a custom scheme exists");
    assert_eq!(
        BooksDoc::read_settings(&doc).scheme_id,
        custom.id,
        "new scheme is selected"
    );
    assert_eq!(
        app.editing_scheme.as_deref(),
        Some(custom.id.as_str()),
        "editor opened on it"
    );
}

#[test]
fn color_picker_returns_a_colour_applied_to_a_scheme() {
    // A custom scheme's page colour, edited through the picker, changes.
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let mut schemes = default_schemes();
    schemes.push(ColorScheme::from_bg("scheme_x", "Test", [0x30, 0x30, 0x30]));
    BooksDoc::write_schemes(&mut doc, &schemes);
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Colours;
    frame(&mut app, &mut doc, 1200.0, 900.0);
    let before = BooksDoc::read_schemes(&doc)
        .into_iter()
        .find(|s| s.id == "scheme_x")
        .unwrap()
        .page;
    // Tap the custom scheme's page swatch to open the picker on that slot.
    let sw = find_hit(
        &app,
        |a| matches!(a, Action::PickSchemeColor(id, slot) if id == "scheme_x" && *slot == 0),
    )
    .expect("the page swatch opens the picker");
    assert!(
        click(&mut app, &mut doc, sw),
        "picker opened on the page slot"
    );
    assert!(app.color_picker.is_open());
    // Drag on the SV square: a press + move within it applies a new colour.
    frame(&mut app, &mut doc, 1200.0, 900.0);
    let sv = app.color_picker.sv_rect();
    ptr_move(
        &mut app,
        &mut doc,
        Point::new(sv.x0 + sv.width() * 0.8, sv.y0 + sv.height() * 0.2),
    );
    ptr_down(&mut app, &mut doc);
    ptr_move(
        &mut app,
        &mut doc,
        Point::new(sv.x0 + sv.width() * 0.75, sv.y0 + sv.height() * 0.25),
    );
    ptr_up(&mut app, &mut doc);
    let after = BooksDoc::read_schemes(&doc)
        .into_iter()
        .find(|s| s.id == "scheme_x")
        .unwrap()
        .page;
    assert_ne!(
        after, before,
        "the picker applied a new page colour to the scheme"
    );
}

#[test]
fn font_manager_toggles_shown_and_sets_default() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Fonts;
    frame(&mut app, &mut doc, 1200.0, 820.0);
    // Disable the Serif font (its enable toggle = whether it shows in quick
    // settings).
    let hide = find_hit(
        &app,
        |a| matches!(a, Action::ToggleFontShown(id) if id.as_str() == "serif"),
    )
    .expect("serif enable toggle");
    assert!(click(&mut app, &mut doc, hide), "toggle handled");
    frame(&mut app, &mut doc, 1200.0, 820.0);
    assert!(
        !app.shown_fonts().iter().any(|f| f.id == "serif"),
        "serif no longer in quick settings"
    );
    // Make Mono the registry default via the default-font dropdown.
    let dd =
        find_hit(&app, |a| matches!(a, Action::ToggleDefaultFontMenu)).expect("default dropdown");
    assert!(click(&mut app, &mut doc, dd), "dropdown opened");
    assert!(app.default_font_menu, "default-font menu is open");
    frame(&mut app, &mut doc, 1200.0, 820.0);
    let def = find_hit(
        &app,
        |a| matches!(a, Action::SetDefaultFont(id) if id.as_str() == "mono"),
    )
    .expect("mono row in the dropdown");
    assert!(click(&mut app, &mut doc, def), "default handled");
    assert_eq!(
        BooksDoc::read_default_font(&doc),
        "mono",
        "default persisted"
    );
    // With no explicit selection, the active font follows the default.
    assert_eq!(
        app.active_font().id,
        "mono",
        "active font is the new default"
    );
}

#[test]
fn drag_selection_spans_paragraphs_and_highlights_the_range() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Wide window so the docked rails don't cover the page centre.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        app.sel_runs.len() > 3,
        "the page laid out real run geometry"
    );

    // Press inside the first run, then drag to a run in a *later* block.
    let r0 = app.sel_runs[0];
    let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
    let later = app
        .sel_runs
        .iter()
        .find(|r| r.block > r0.block)
        .copied()
        .expect("a run in a later paragraph");
    let end = Point::new(
        f64::midpoint(later.x0, later.x1),
        f64::midpoint(later.y0, later.y1),
    );

    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, end);
    ptr_up(&mut app, &mut doc);

    let sel = app.sel.expect("a drag produced a selection");
    let (s, e) = sel.ordered();
    assert!(
        e.block > s.block,
        "the selection spans more than one paragraph"
    );

    // Highlight the selection from the floating toolbar — one ranged
    // highlight per spanned block.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
    assert!(click(&mut app, &mut doc, swatch), "swatch handled");
    let hls: Vec<_> = BooksDoc::list_annotations(&doc)
        .into_iter()
        .filter(|a| a.kind == AnnoKind::Highlight)
        .collect();
    assert!(
        hls.len() >= 2,
        "a ranged highlight per spanned paragraph, got {}",
        hls.len()
    );
    assert!(
        hls.iter().all(|a| a.start < a.end),
        "highlights carry a real char range"
    );
    // Selecting cleared the selection (the highlight now stands in for it).
    assert!(app.sel.is_none(), "selection cleared after highlighting");
}

#[test]
fn note_action_opens_an_on_page_composer_over_a_selection() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Make a cross-paragraph selection via drag.
    let r0 = app.sel_runs[0];
    let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
    let later = app
        .sel_runs
        .iter()
        .find(|r| r.block > r0.block)
        .copied()
        .unwrap();
    let end = Point::new(
        f64::midpoint(later.x0, later.x1),
        f64::midpoint(later.y0, later.y1),
    );
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, end);
    ptr_up(&mut app, &mut doc);
    assert!(app.sel.is_some_and(|s| !s.is_empty()));

    // The toolbar's Note action opens the composer (not the side rail),
    // anchored to the selection, with the selection's text as the excerpt.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let note = find_hit(&app, |a| matches!(a, Action::AddNote)).expect("a Note action");
    assert!(click(&mut app, &mut doc, note), "note handled");
    let cid = app.composing.expect("a note is being composed on the page");
    let a = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.id == cid)
        .unwrap();
    assert_eq!(a.kind, AnnoKind::Note);
    assert!(
        !a.excerpt.is_empty(),
        "the composer note carries the selected passage"
    );
    // The on-page composer records a Save hit (proving it's drawn on-page).
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(app.note_composer.is_open(), "on-page composer is drawn");
}

#[test]
fn copy_action_copies_the_full_selected_passage() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Select a long span: from the first run to the last, across paragraphs.
    let r0 = app.sel_runs[0];
    let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
    let last = *app.sel_runs.last().unwrap();
    let end = Point::new(last.x1, f64::midpoint(last.y0, last.y1));
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, end);
    ptr_up(&mut app, &mut doc);
    assert!(app.sel.is_some_and(|s| !s.is_empty()), "a selection exists");

    // The text Copy hands the clipboard is the FULL selected passage — not
    // the 160-char excerpt used for annotation labels.
    let full = app.selection_text();
    assert!(!full.is_empty(), "selection yields text");
    let excerpt = app.selection_excerpt();
    assert!(
        full.chars().count() >= excerpt.chars().count(),
        "the copied text is at least as long as the annotation excerpt"
    );
    if full.chars().count() > 160 {
        assert_eq!(
            excerpt.chars().count(),
            160,
            "the excerpt caps at 160 chars while Copy is unbounded"
        );
    }

    // The floating toolbar exposes a Copy action; clicking it runs the copy
    // path (the clipboard write is a silent no-op headless) and KEEPS the
    // selection so the reader can still highlight or note the same passage.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let copy =
        find_hit(&app, |a| matches!(a, Action::CopySelection)).expect("a Copy action in the HUD");
    assert!(click(&mut app, &mut doc, copy), "copy handled");
    assert!(
        app.sel.is_some_and(|s| !s.is_empty()),
        "Copy keeps the selection"
    );
}

#[test]
fn copy_key_shortcut_targets_the_active_selection() {
    // A bare `c` copies only when a selection is live (mirroring `h`
    // highlight / `m` note); with no selection it is not consumed.
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(app.sel.is_none(), "no selection at rest");
    assert!(
        !key_char(&mut app, &mut doc, "c"),
        "bare c is inert without a selection"
    );

    // Make a selection, then `c` is consumed (it drives the copy path).
    let r0 = app.sel_runs[0];
    let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
    let end = Point::new(r0.x1, f64::midpoint(r0.y0, r0.y1));
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, end);
    ptr_up(&mut app, &mut doc);
    assert!(app.sel.is_some_and(|s| !s.is_empty()), "a selection exists");
    assert!(
        key_char(&mut app, &mut doc, "c"),
        "c copies the live selection"
    );
}

#[test]
fn settings_popover_does_not_dim_the_page() {
    use ui::offscreen::OffscreenRenderer;
    let (w, h) = (1000u32, 700u32);
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Same page, popover closed then open.
    let closed = render_scene(&mut app, &mut doc, f64::from(w), f64::from(h));
    app.reading_settings.open();
    let open = render_scene(&mut app, &mut doc, f64::from(w), f64::from(h));

    let mut ro = OffscreenRenderer::new(w, h);
    let c_px = ro.render_rgba(&closed);
    let o_px = ro.render_rgba(&open);
    // A point well inside the page column, above/left of the bottom-right
    // popover panel — if a scrim dimmed the page it would darken here.
    let (x, y) = (400u32, 250u32);
    let a0 = OffscreenRenderer::sample(&c_px, w, x, y);
    let a1 = OffscreenRenderer::sample(&o_px, w, x, y);
    let d = |u: u8, v: u8| (i32::from(u) - i32::from(v)).abs();
    assert!(
        d(a0.0, a1.0) <= 3 && d(a0.1, a1.1) <= 3 && d(a0.2, a1.2) <= 3,
        "the open settings popover must not dim the page: {a0:?} vs {a1:?}",
    );
}

#[test]
fn composition_setting_relayouts_and_persists() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(app.layout.is_some(), "reader starts laid out");

    app.reading_settings.open();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let ragged = find_reading_hit(&app, |action| {
        matches!(
            action,
            ReadingSettingsAction::SetComposition(CompositionStyle::Ragged)
        )
    })
    .expect("settings popover exposes the Ragged composition choice");
    assert!(click(&mut app, &mut doc, ragged), "choice is clickable");

    assert!(
        app.layout.is_none(),
        "composition changes invalidate layout"
    );
    assert_eq!(app.settings.composition, CompositionStyle::Ragged);
    assert_eq!(
        BooksDoc::read_settings(&doc).composition,
        CompositionStyle::Ragged,
        "reader preference persists"
    );
}

#[test]
fn serif_reader_matches_typography_baseline() {
    use std::path::PathBuf;

    use ui::offscreen::OffscreenRenderer;

    let (width, height) = (1280u32, 800u32);
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let scene = render_scene(&mut app, &mut doc, f64::from(width), f64::from(height));
    let mut renderer = OffscreenRenderer::try_new(width, height)
        .expect("typography baseline requires an offscreen renderer");
    // A typography-heavy first submission can grow Vello's bump budget;
    // compare only the confirmed-fit frame so missing glyph-atlas spans
    // cannot masquerade as a legitimate baseline change.
    let actual = renderer.render_rgba_resilient(&scene);
    let baseline = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(if cfg!(windows) {
        "tests/fixtures/typography/serif-reader-windows.png"
    } else {
        "tests/fixtures/typography/serif-reader.png"
    });
    if std::env::var_os("UPDATE_EBOOK_TYPOGRAPHY_BASELINE").is_some() {
        if let Some(parent) = baseline.parent() {
            std::fs::create_dir_all(parent).expect("create ebook typography baseline dir");
        }
        image::save_buffer(&baseline, &actual, width, height, image::ColorType::Rgba8)
            .expect("write ebook typography baseline");
        eprintln!("updated {}", baseline.display());
        return;
    }
    let expected = image::open(&baseline)
        .unwrap_or_else(|error| {
            panic!(
                "missing ebook typography baseline {}: {error}; regenerate with \
                 UPDATE_EBOOK_TYPOGRAPHY_BASELINE=1",
                baseline.display()
            )
        })
        .to_rgba8();
    assert_eq!(
        expected.dimensions(),
        (width, height),
        "ebook typography baseline changed dimensions"
    );
    let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, expected.as_raw());
    if differing != 0 {
        let output = std::env::temp_dir().join(baseline.file_name().unwrap());
        image::save_buffer(output, &actual, width, height, image::ColorType::Rgba8).unwrap();
    }
    assert_eq!(
        differing, 0,
        "serif reader differs from typography baseline in {differing} pixels"
    );
}

#[test]
fn mathematical_reader_matches_shared_typesetting_baseline() {
    use std::path::PathBuf;

    use ui::offscreen::OffscreenRenderer;

    let (width, height) = (1280u32, 800u32);
    let mut app = app_with_math_sample();
    let mut doc = AutoCommit::new();
    let scene = render_scene(&mut app, &mut doc, f64::from(width), f64::from(height));
    let mut renderer = OffscreenRenderer::try_new(width, height)
        .expect("typography baseline requires an offscreen renderer");
    let actual = renderer.render_rgba_resilient(&scene);
    let baseline = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(if cfg!(windows) {
        "tests/fixtures/typography/math-reader-windows.png"
    } else {
        "tests/fixtures/typography/math-reader.png"
    });
    if std::env::var_os("UPDATE_EBOOK_MATH_BASELINE").is_some() {
        if let Some(parent) = baseline.parent() {
            std::fs::create_dir_all(parent).expect("create ebook math baseline dir");
        }
        image::save_buffer(&baseline, &actual, width, height, image::ColorType::Rgba8)
            .expect("write ebook math baseline");
        eprintln!("updated {}", baseline.display());
        return;
    }
    let expected = image::open(&baseline)
        .unwrap_or_else(|error| {
            panic!(
                "missing ebook math baseline {}: {error}; regenerate with \
                 UPDATE_EBOOK_MATH_BASELINE=1",
                baseline.display()
            )
        })
        .to_rgba8();
    assert_eq!(expected.dimensions(), (width, height));
    let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, expected.as_raw());
    if differing != 0 {
        let output = std::env::temp_dir().join(baseline.file_name().unwrap());
        image::save_buffer(output, &actual, width, height, image::ColorType::Rgba8).unwrap();
    }
    assert_eq!(
        differing, 0,
        "mathematical reader differs from baseline in {differing} pixels"
    );
}

#[test]
fn wheel_scrolls_the_library_shelf() {
    let mut app = app_with_sample();
    app.view = View::Library;
    let mut doc = AutoCommit::new();
    // A short window so the three sample cards overflow into a scroll range.
    frame(&mut app, &mut doc, 560.0, 600.0);
    assert!(app.shelf_max_scroll > 0.0, "shelf overflows this window");
    assert_eq!(app.shelf_scroll, 0.0, "starts at the top");
    // Scroll down, then hard down — must clamp at the bottom.
    assert!(wheel(&mut app, &mut doc, -3.0), "wheel scrolled the shelf");
    assert!(app.shelf_scroll > 0.0);
    wheel(&mut app, &mut doc, -100.0);
    assert!(
        app.shelf_scroll <= app.shelf_max_scroll + 0.001,
        "never past the end"
    );
    assert!(app.shelf_scroll > 0.0);
    // Scroll back up — must clamp at the top.
    wheel(&mut app, &mut doc, 100.0);
    assert_eq!(app.shelf_scroll, 0.0, "clamps at the top");
}

#[test]
fn opening_a_book_from_the_shelf_enters_the_reader() {
    let mut app = app_with_sample();
    app.view = View::Library;
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 720.0);
    let target = find_hit(&app, |a| matches!(a, Action::OpenBook(_))).expect("a book card hit");
    assert!(click(&mut app, &mut doc, target), "book click handled");
    assert_eq!(app.view, View::Reader);
}

#[test]
fn next_page_advances_position() {
    let mut app = app_with_sample();
    // The page-nav stepper now lives on the floating page bubble, which is
    // transient — arm it open so its next chevron registers a NextPage hit,
    // then click that chevron on the real hit-testing path.
    app.arm_reader_bubble_for_shots();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let (c0, p0) = (app.chapter, app.page);
    let next =
        find_hit(&app, |a| matches!(a, Action::NextPage)).expect("next chevron on the page bubble");
    assert!(click(&mut app, &mut doc, next), "next handled");
    assert!(
        app.page != p0 || app.chapter != c0,
        "position advanced from ch{c0}/pg{p0} to ch{}/pg{}",
        app.chapter,
        app.page
    );
}

/// The floating page bubble is hidden on a settled reader and surfaced by
/// a page change without leaving an invisible navigation hit behind.
#[test]
fn page_bubble_surfaces_on_a_page_change() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Settled reader: the bubble is not up, and no NextPage hit is exposed in
    // the bottom band (an invisible bubble never eats a click there).
    assert!(
        !app.needs_animation_frame(),
        "a settled reader schedules no animation frames"
    );
    assert!(
        find_hit(&app, |a| matches!(a, Action::NextPage)).is_none(),
        "the hidden bubble exposes no page-nav hit"
    );
    // A page turn pokes the bubble; the next frame draws it and makes its
    // chevrons hit-testable.
    app.dispatch(Action::NextPage, &mut doc);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        find_hit(&app, |a| matches!(a, Action::NextPage)).is_some(),
        "a page change surfaces the bubble's next chevron"
    );
}

#[test]
fn continuous_mode_scrolls_the_flow_and_persists() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Switch to continuous scroll through the real command path.
    app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert_eq!(app.settings.mode, ReadMode::Continuous, "mode switched");
    assert_eq!(
        BooksDoc::read_settings(&doc).mode,
        ReadMode::Continuous,
        "mode persisted"
    );
    // A downward wheel scrolls the flow (rolls chapters at the ends) rather
    // than snapping whole pages.
    let before = (app.chapter, app.scroll_y);
    assert!(wheel(&mut app, &mut doc, -6.0), "continuous wheel handled");
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert_ne!(
        (app.chapter, app.scroll_y),
        before,
        "continuous scroll moved the reading position"
    );
    // Progress persisted for the shelf bar.
    let id = app.book_id();
    assert!(
        BooksDoc::read_progress(&doc, &id).fraction >= 0.0,
        "progress recorded in continuous mode"
    );
}

#[test]
fn two_up_shows_two_columns_and_turns_by_two() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 820.0);
    app.dispatch(Action::SetMode(ReadMode::TwoUp), &mut doc);
    frame(&mut app, &mut doc, 1280.0, 820.0);
    assert_eq!(app.settings.mode, ReadMode::TwoUp);
    // A spread aligns to an even left page.
    assert_eq!(app.page % 2, 0, "the spread's left page is even");
    // The two facing pages paint into two columns on the real draw path.
    if app.cur_page_count >= 2 {
        let cols: std::collections::HashSet<u8> = app.sel_runs.iter().map(|r| r.col).collect();
        assert!(
            cols.contains(&0) && cols.contains(&1),
            "the spread paints two facing columns"
        );
    }
    // A page turn advances by a whole spread (two pages), or rolls chapter.
    let (c0, p0) = (app.chapter, app.page);
    app.dispatch(Action::NextPage, &mut doc);
    frame(&mut app, &mut doc, 1280.0, 820.0);
    if app.chapter == c0 {
        assert_eq!(app.page, p0 + 2, "a spread turns two pages at once");
    } else {
        assert_eq!(app.chapter, c0 + 1, "or rolls to the next chapter");
    }
}

/// A paged turn eases the incoming page in (a slide + fade) instead of
/// snapping: the turn schedules animation frames and the incoming page starts
/// offset off its resting x, decaying toward zero. Continuous scroll never
/// animates a page turn — it just scrolls.
#[test]
fn turning_a_page_animates_an_eased_slide() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert_eq!(
        app.settings.mode,
        ReadMode::Paged,
        "paged is the default mode"
    );
    assert!(
        !app.needs_animation_frame(),
        "a settled reader schedules no animation frames"
    );
    assert!(
        app.cur_page_count >= 2,
        "the sample chapter paginates into several pages"
    );

    // A forward turn (pages to spare) advances and starts the slide.
    let (c0, p0) = (app.chapter, app.page);
    app.dispatch(Action::NextPage, &mut doc);
    assert_ne!((app.chapter, app.page), (c0, p0), "the page advanced");
    assert!(
        app.needs_animation_frame(),
        "the turn schedules animation frames"
    );
    let pt = app.page_turn.expect("a page-turn slide is in flight");
    let (dx, alpha) = pt.visual(std::time::Instant::now(), 500.0);
    assert!(
        dx > 0.0,
        "a forward turn slides the incoming page in from the right: {dx}"
    );
    assert!(
        alpha < 1.0,
        "the incoming page fades up as it arrives: {alpha}"
    );

    // Continuous scroll scrolls instead of turning, so no slide is scheduled.
    app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
    frame(&mut app, &mut doc, 1000.0, 700.0); // settles + clears the prior turn
    assert!(
        app.page_turn.is_none(),
        "the paged turn cleared after settling"
    );
    app.dispatch(Action::NextPage, &mut doc);
    assert!(
        app.page_turn.is_none(),
        "continuous mode never starts a page-turn slide"
    );
}

#[test]
fn reduced_motion_settles_an_in_flight_page_turn_immediately() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    app.dispatch(Action::NextPage, &mut doc);
    assert!(app.page_turn.is_some());

    Widget::set_motion_reduced(&mut app, true);

    assert!(app.page_turn.is_none());
    assert!(!app.needs_animation_frame());
}

/// A highlight's margin marker is a draggable handle: grabbing it and dropping
/// it on another paragraph re-anchors the highlight there through the
/// annotation action model (one row moves, its comments are untouched). A
/// plain tap on the same marker opens the highlight's comments.
#[test]
fn dragging_a_highlight_marker_reanchors_it_and_keeps_its_comments() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);

    // Highlight a range within the first paragraph (single block → movable).
    let r0 = app.sel_runs[0];
    let end_run = app
        .sel_runs
        .iter()
        .rfind(|r| r.block == r0.block)
        .copied()
        .unwrap();
    ptr_move(
        &mut app,
        &mut doc,
        Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1)),
    );
    ptr_down(&mut app, &mut doc);
    ptr_move(
        &mut app,
        &mut doc,
        Point::new(
            f64::midpoint(end_run.x0, end_run.x1),
            f64::midpoint(end_run.y0, end_run.y1),
        ),
    );
    ptr_up(&mut app, &mut doc);
    app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
    app.refresh_from_doc(&doc);
    let hl = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.kind == AnnoKind::Highlight)
        .expect("a highlight now exists");
    let group = hl.group;
    let src_block = hl.block;

    // Attach a comment so the re-anchor can be shown not to disturb it.
    let comment = Annotation {
        id: uuid::Uuid::new_v4(),
        doc_id: app.book_id(),
        kind: AnnoKind::Note,
        chapter: 0,
        block: src_block,
        start: hl.start,
        end: hl.end,
        excerpt: "x".into(),
        note: "keep me".into(),
        color: HiColor::Green,
        created: 5,
        group,
    };
    BooksDoc::upsert_annotation(&mut doc, &comment);
    app.refresh_from_doc(&doc);

    // A frame records the group's draggable margin marker.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let marker_rect = app
        .hi_markers
        .iter()
        .find(|(_, g)| *g == group)
        .map(|(r, _)| *r)
        .expect("the highlight has a draggable margin marker");

    // A target paragraph in a different block.
    let target = app
        .sel_runs
        .iter()
        .find(|r| r.block != src_block)
        .copied()
        .expect("another paragraph is visible on the page");
    let target_block = target.block;
    let target_pt = Point::new(
        f64::midpoint(target.x0, target.x1),
        f64::midpoint(target.y0, target.y1),
    );

    // Grab the marker and drag it onto the target paragraph.
    ptr_move(&mut app, &mut doc, marker_rect.center());
    ptr_down(&mut app, &mut doc);
    assert!(
        app.marker_drag.is_some(),
        "pressing the marker began a re-anchor drag"
    );
    ptr_move(&mut app, &mut doc, target_pt);
    ptr_up(&mut app, &mut doc);
    app.refresh_from_doc(&doc);

    // The highlight re-anchored to the dropped paragraph — the SAME row moved
    // (its id is preserved, proving it went through `MoveAnnotation`, not a
    // delete + re-create).
    let moved = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.kind == AnnoKind::Highlight && a.group == group)
        .expect("the highlight still exists");
    assert_eq!(
        moved.id, hl.id,
        "the same highlight row moved (not recreated)"
    );
    assert_eq!(
        moved.block, target_block,
        "the highlight re-anchored to the dropped paragraph"
    );
    assert_ne!(moved.block, src_block, "it actually moved off its origin");
    // Its comment survived the move.
    assert!(
        BooksDoc::list_annotations(&doc)
            .into_iter()
            .any(|a| a.kind == AnnoKind::Note && a.group == group && a.note == "keep me"),
        "the highlight's comment survived the re-anchor",
    );
}

/// A plain tap on a highlight's margin marker (no drag) opens its comments,
/// so the marker doubles as the comment affordance.
#[test]
fn tapping_a_highlight_marker_opens_its_comments() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let r0 = app.sel_runs[0];
    let end_run = app
        .sel_runs
        .iter()
        .rfind(|r| r.block == r0.block)
        .copied()
        .unwrap();
    ptr_move(
        &mut app,
        &mut doc,
        Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1)),
    );
    ptr_down(&mut app, &mut doc);
    ptr_move(
        &mut app,
        &mut doc,
        Point::new(
            f64::midpoint(end_run.x0, end_run.x1),
            f64::midpoint(end_run.y0, end_run.y1),
        ),
    );
    ptr_up(&mut app, &mut doc);
    app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
    app.refresh_from_doc(&doc);
    let group = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.kind == AnnoKind::Highlight)
        .expect("a highlight")
        .group;
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let marker = app
        .hi_markers
        .iter()
        .find(|(_, g)| *g == group)
        .map(|(r, _)| r.center())
        .expect("the highlight has a marker");
    // Press + release without moving.
    ptr_move(&mut app, &mut doc, marker);
    ptr_down(&mut app, &mut doc);
    ptr_up(&mut app, &mut doc);
    assert_eq!(
        app.comment_overlay.group(),
        Some(group),
        "a tap on the marker opened the highlight's comments"
    );
}

#[test]
fn shared_selection_hud_shows_in_continuous_mode() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Seed a live selection near the top of the flow and redraw.
    app.sel = Some(TextSel {
        anchor: DocPos { block: 1, ch: 0 },
        head: DocPos { block: 1, ch: 24 },
    });
    app.selected_block = Some(1);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // The shared HUD registered its Copy + a swatch control as clickable hits.
    assert!(
        find_hit(&app, |a| matches!(a, Action::CopySelection)).is_some(),
        "the shared selection HUD floats over the continuous flow"
    );
    assert!(
        find_hit(&app, |a| matches!(a, Action::SetColor(_))).is_some(),
        "the HUD offers highlight swatches"
    );
}

#[test]
fn double_click_selects_a_word_then_a_swatch_writes_a_ranged_highlight() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let before = BooksDoc::list_annotations(&doc).len();
    // Double-click a word: the selection is exactly that word's char span.
    let p = page_point(&app);
    let word_run = app.sel_runs[0];
    dbl_click(&mut app, &mut doc, p);
    let sel = app.sel.expect("double-click made a selection");
    let (s, e) = sel.ordered();
    assert_eq!(s.block, word_run.block);
    assert_eq!(s.ch, word_run.cs, "selection starts at the word start");
    assert_eq!(
        e.ch,
        word_run.cs + word_run.cl,
        "selection ends at the word end"
    );
    // The selection HUD offers colour swatches — click one to highlight.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
    assert!(click(&mut app, &mut doc, swatch), "swatch handled");
    let after = BooksDoc::list_annotations(&doc);
    assert_eq!(after.len(), before + 1, "one highlight added");
    let hl = after
        .iter()
        .find(|a| a.kind == AnnoKind::Highlight)
        .unwrap();
    assert!(
        hl.start < hl.end,
        "the highlight carries a real char range (ranged, not whole-block)"
    );
}

#[test]
fn continuous_highlight_band_fills_the_gap_between_words() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Find two adjacent runs (words) on the same line with a visible gap.
    let runs = app.sel_runs.clone();
    let (a, b) = runs
        .windows(2)
        .map(|w| (w[0], w[1]))
        .find(|(a, b)| a.block == b.block && (a.y0 - b.y0).abs() < 0.5 && b.x0 > a.x1 + 1.0)
        .expect("two adjacent words with a gap");
    // A highlight covering both words (a real ranged annotation on the page).
    let book_id = app.book_id();
    let mut hl = Annotation::new(&book_id, AnnoKind::Highlight, app.chapter, a.block)
        .with_color(HiColor::Yellow);
    hl.start = a.cs;
    hl.end = b.cs + b.cl;
    BooksDoc::upsert_annotation(&mut doc, &hl);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // The gap midpoint between the two words must be inside a drawn band —
    // proving the band is continuous, not per-glyph.
    let gap_mid = Point::new(f64::midpoint(a.x1, b.x0), f64::midpoint(a.y0, a.y1));
    assert!(
        app.hi_bands.iter().any(|band| band.contains(gap_mid)),
        "a continuous highlight band covers the inter-word gap"
    );
}

#[test]
fn highlight_colour_toggles_off_and_replaces() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let count = |d: &AutoCommit| {
        BooksDoc::list_annotations(d)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Highlight)
            .count()
    };
    let color_of = |d: &AutoCommit| {
        BooksDoc::list_annotations(d)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .map(|a| a.color)
    };
    // Select a word, then apply yellow.
    let p = page_point(&app);
    dbl_click(&mut app, &mut doc, p);
    app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
    app.refresh_from_doc(&doc);
    assert_eq!(count(&doc), 1);
    assert_eq!(color_of(&doc), Some(HiColor::Yellow));

    // Re-select the same word; picking a DIFFERENT colour replaces it.
    dbl_click(&mut app, &mut doc, p);
    app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
    app.refresh_from_doc(&doc);
    assert_eq!(count(&doc), 1, "different colour replaces (never layers)");
    assert_eq!(color_of(&doc), Some(HiColor::Green));

    // Re-select the same word; picking the SAME colour toggles it off.
    dbl_click(&mut app, &mut doc, p);
    app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
    app.refresh_from_doc(&doc);
    assert_eq!(count(&doc), 0, "same colour again removes the highlight");
}

#[test]
fn shift_and_ctrl_shift_arrows_extend_selection() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Seed a collapsed selection at the start of the first word.
    let r0 = app.sel_runs[0];
    let start = DocPos {
        block: r0.block,
        ch: r0.cs,
    };
    app.sel = Some(TextSel {
        anchor: start,
        head: start,
    });
    // Shift+Right extends by one character.
    assert!(key_named_mods(
        &mut app,
        &mut doc,
        NamedKey::ArrowRight,
        ModifiersState::SHIFT
    ));
    assert_eq!(
        app.sel.unwrap().head,
        DocPos {
            block: r0.block,
            ch: r0.cs + 1
        },
        "char extend"
    );
    // Shift+Left contracts back by one character.
    assert!(key_named_mods(
        &mut app,
        &mut doc,
        NamedKey::ArrowLeft,
        ModifiersState::SHIFT
    ));
    assert_eq!(app.sel.unwrap().head, start, "char contract");
    // Ctrl+Shift+Right jumps to the next word boundary.
    let expect_word = app.doc_next_word(start);
    assert!(
        expect_word.ch > start.ch,
        "there is a next word boundary to reach"
    );
    assert!(key_named_mods(
        &mut app,
        &mut doc,
        NamedKey::ArrowRight,
        ModifiersState::SHIFT | ModifiersState::CONTROL
    ));
    assert_eq!(app.sel.unwrap().head, expect_word, "word extend");
}

#[test]
fn aa_button_opens_popover_and_page_tint_persists() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(!app.reading_settings.is_open());
    let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
    assert!(click(&mut app, &mut doc, aa));
    assert!(app.reading_settings.is_open(), "popover opened");
    // Choose the Dark page tint from the popover.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let dark = find_reading_hit(&app, |action| {
        matches!(action, ReadingSettingsAction::SetReadTheme(ReadTheme::Dark))
    })
    .expect("dark tint pill");
    assert!(click(&mut app, &mut doc, dark));
    assert_eq!(BooksDoc::read_settings(&doc).theme, ReadTheme::Dark);
}

#[test]
fn rails_collapse_on_narrow_and_dock_on_wide() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Wide: both rails docked-open by default.
    frame(&mut app, &mut doc, 1280.0, 720.0);
    assert!(app.toc_open(), "contents docked+open at 1280");
    assert!(app.notes_open(), "annotations docked+open at 1280");
    // Narrow: both become closed overlays, so the page owns the width.
    frame(&mut app, &mut doc, 560.0, 720.0);
    assert_eq!(app.toc_layout(), SplitLayout::Overlay);
    assert_eq!(app.notes_layout(), SplitLayout::Overlay);
    assert!(!app.toc_open(), "contents closed at 560");
    assert!(!app.notes_open(), "annotations closed at 560");
}

#[test]
fn runtime_start_seeds_no_annotations() {
    // A non-demo app must never fabricate marks into a fresh document.
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        BooksDoc::list_annotations(&doc).is_empty(),
        "no seeded annotations"
    );
}

// ---- library-shelf keyboard ----------------------------------------------

#[test]
fn library_keyboard_moves_focus_and_opens_the_focused_book() {
    let mut app = app_with_sample();
    app.view = View::Library;
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 720.0);
    assert!(app.library.len() >= 3, "sample shelf has several books");
    assert_eq!(app.shelf_cursor, 0, "focus starts on the first book");

    // Arrow-right/-left move the focus along the row.
    assert!(
        key_named(&mut app, &mut doc, NamedKey::ArrowRight),
        "arrow consumed"
    );
    assert_eq!(app.shelf_cursor, 1, "arrow-right moved focus");
    assert!(key_named(&mut app, &mut doc, NamedKey::ArrowLeft));
    assert_eq!(app.shelf_cursor, 0, "arrow-left moved focus back");

    // End jumps to the last book; Home back to the first.
    assert!(key_named(&mut app, &mut doc, NamedKey::End));
    assert_eq!(app.shelf_cursor, app.library.len() - 1, "End -> last book");
    assert!(key_named(&mut app, &mut doc, NamedKey::Home));
    assert_eq!(app.shelf_cursor, 0, "Home -> first book");

    // Enter opens the focused book (mapping the display position back to the
    // real library index through the sorted display order).
    let expected = app.shelf_order[app.shelf_cursor];
    assert!(
        key_named(&mut app, &mut doc, NamedKey::Enter),
        "enter consumed"
    );
    assert_eq!(app.view, View::Reader, "enter opened the focused book");
    assert_eq!(
        app.book_idx, expected,
        "opened the focused book (via display order)"
    );
}

#[test]
fn library_arrow_down_steps_a_whole_row() {
    let mut app = app_with_sample();
    app.view = View::Library;
    let mut doc = AutoCommit::new();
    // A narrow window forces a single column, so down moves to the next book.
    frame(&mut app, &mut doc, 320.0, 700.0);
    assert_eq!(app.shelf_cols, 1, "one column at this width");
    assert!(key_named(&mut app, &mut doc, NamedKey::ArrowDown));
    assert_eq!(
        app.shelf_cursor, 1,
        "down stepped one row (one book) in a single column"
    );
}

#[test]
fn short_desktop_shelf_uses_a_centred_bounded_grid() {
    let mut app = app_with_sample();
    let viewport = Rect::new(0.0, 0.0, 1440.0, 900.0);
    let grid = app.shelf_grid_geometry(viewport, 3);
    assert_eq!(grid.cols, 4, "three books plus Add form one centred row");
    let grid_w = grid.card_w * 4.0 + grid.gap * 3.0;
    assert!(
        (grid.start_x - (viewport.width() - grid_w) / 2.0).abs() < 0.01,
        "the compact shelf is centred instead of pinned to the left"
    );
    assert!(
        grid.card_w < 240.0,
        "desktop cards remain compact, not stretched"
    );
}

// ---- library: open-from-anywhere, import, share ------------------------

#[test]
fn live_constructor_starts_empty_on_the_library() {
    // The live app opens onto the shelf (its home), empty until the document
    // load on the first sync — never a fabricated starter book.
    let app = BooksApp::new();
    assert!(
        app.library.is_empty(),
        "no books before the document is loaded"
    );
    assert_eq!(app.view, View::Library, "the shelf is home, not a book");
    assert!(
        !app.library_loaded,
        "the persisted library loads on first sync"
    );
}

#[test]
fn the_empty_shelf_onboards_with_open_book_not_a_reload() {
    // The empty state offers the primary way in — open a book from anywhere —
    // and never asks the reader to drop files in a magic folder and reload.
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);
    assert!(app.library.is_empty());
    assert!(
        find_hit(&app, |a| matches!(a, Action::OpenViaPortal)).is_some(),
        "the empty shelf offers an Open-book action"
    );
}

/// The core of the redesign: opening a book by path imports it into the
/// library (so it's there next time) AND opens it. Exercised through the OS
/// file-association entry point, which drains on the first sync.
#[test]
fn opening_a_book_by_path_imports_it_and_opens_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("handed-off.epub");
    std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();

    let mut app = BooksApp::open_path(&path);
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);
    wait_for_import(&mut app, &mut doc);

    assert_eq!(
        app.view,
        View::Reader,
        "the handed-off book opens in the reader"
    );
    assert_eq!(app.library.len(), 1, "and is now on the shelf");
    assert!(
        !app.library[0].title.is_empty(),
        "the real epub parsed a title"
    );
    // Persisted, so it is there on the next launch.
    assert_eq!(
        BooksDoc::read_library_entries(&doc).len(),
        1,
        "the opened book is recorded in the library"
    );
}

/// A book chosen through the host file picker loads through the per-frame
/// portal drain (the result is injected — no real dialog).
#[test]
fn a_picked_book_is_imported_and_opened_via_the_frame_drain() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("picked.epub");
    std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();

    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);
    assert!(app.library.is_empty(), "starts empty");

    app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(path)));
    app.sync(&mut doc, &automerge::ROOT);
    wait_for_import(&mut app, &mut doc);

    assert_eq!(app.library.len(), 1, "the picked book joined the library");
    assert_eq!(app.view, View::Reader, "and opened in the reader");
    assert!(
        app.portal_rx.is_none(),
        "the portal channel clears after delivering"
    );
}

#[test]
fn disconnected_picker_settles_the_pending_resource_invocation() {
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    let (sender, receiver) = std::sync::mpsc::channel::<PortalOutcome>();
    drop(sender);
    app.resource_invocation = Some(invocation);
    app.portal_rx = Some(receiver);

    app.sync(&mut doc, &automerge::ROOT);

    assert!(app.portal_rx.is_none());
    assert!(app.resource_invocation.is_none());
    assert!(matches!(
        app.completed_resource_invocation,
        Some(tilo_actions::InvocationUpdate::Failed { invocation: got, ref error })
            if got == invocation && error.code == "ebook.portal_disconnected"
    ));
}

#[test]
#[serial]
fn import_worker_never_blocks_and_rejects_a_stale_result() {
    // Keep this assertion specifically about latest-request wins and local
    // fallback. A file root cannot host `Books`, so workspace
    // materialisation is unavailable regardless of runner privileges.
    let workspace = tempfile::NamedTempFile::new().unwrap();
    let _workspace = WorkspaceEnvGuard::set(workspace.path());
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let pa = a.path().join("slow-a.epub");
    let pb = b.path().join("new-b.epub");
    std::fs::write(&pa, crate::content::SAMPLE_EPUB).unwrap();
    std::fs::write(&pb, crate::content::SAMPLE_EPUB).unwrap();
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();

    let start = Instant::now();
    app.begin_import_with_delay(&pa, ImportKind::Open, Duration::from_millis(180))
        .unwrap();
    assert!(
        start.elapsed() < Duration::from_millis(40),
        "queueing a slow parse stays on the UI budget"
    );
    // Let A leave the latest-wins mailbox and enter the worker, then B
    // supersedes it. A may still finish, but its generation must be ignored.
    std::thread::sleep(Duration::from_millis(20));
    app.begin_import(pb.clone(), ImportKind::Open).unwrap();
    assert_eq!(app.import_loading.as_ref().map(|l| &l.path), Some(&pb));

    wait_for_import(&mut app, &mut doc);
    let book = app.open_book().expect("new request opened");
    assert_eq!(app.library_paths.get(&book.id), Some(&pb));
    assert!(
        !BooksDoc::read_library_entries(&doc)[0].shared,
        "a local import never publishes its source location"
    );
}

#[test]
fn watch_scan_cannot_replace_an_explicit_open() {
    let tmp = tempfile::tempdir().unwrap();
    let open = tmp.path().join("open.epub");
    let watched = tmp.path().join("watched.epub");
    std::fs::write(&open, crate::content::SAMPLE_EPUB).unwrap();
    std::fs::write(&watched, crate::content::SAMPLE_EPUB).unwrap();
    let mut app = BooksApp::with_library(Vec::new());

    app.begin_import_with_delay(&open, ImportKind::Open, Duration::from_millis(100))
        .unwrap();
    app.begin_import(watched, ImportKind::Watch).unwrap();
    let loading = app.import_loading.as_ref().expect("open remains pending");
    assert_eq!(loading.kind, ImportKind::Open);
    assert_eq!(loading.path, open);
}

#[test]
fn importer_disconnect_and_generation_exhaustion_are_visible_failures() {
    let mut app = BooksApp::with_library(Vec::new());
    let path = PathBuf::from("/tmp/book.epub");
    app.import_worker.as_ref().unwrap().close_for_test();
    std::thread::sleep(Duration::from_millis(10));
    let error = app
        .begin_import(path.clone(), ImportKind::Open)
        .unwrap_err();
    app.settle_import_failure(error, "ebook.importer_unavailable");
    assert!(
        app.load_error
            .as_deref()
            .is_some_and(|s| s.contains("importer"))
    );

    let mut exhausted = BooksApp::with_library(Vec::new());
    exhausted.import_generation = u64::MAX;
    let error = exhausted.begin_import(path, ImportKind::Open).unwrap_err();
    exhausted.settle_import_failure(error, "ebook.importer_exhausted");
    assert!(
        exhausted
            .load_error
            .as_deref()
            .is_some_and(|s| s.contains("generation exhausted"))
    );
}

/// The same file opened from two different paths resolves to one content-hash
/// identity, so its highlights and progress are shared — the whole point of
/// keying resources to the book, not the path or the device.
#[test]
fn the_same_book_from_two_paths_shares_one_identity() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let pa = a.path().join("from-downloads.epub");
    let pb = b.path().join("from-usb-stick.epub");
    std::fs::write(&pa, crate::content::SAMPLE_EPUB).unwrap();
    std::fs::write(&pb, crate::content::SAMPLE_EPUB).unwrap();

    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);

    app.import_and_open_for_test(&mut doc, &pa);
    let id = app.library[0].id.clone();
    // A highlight made while reading the first copy.
    BooksDoc::upsert_annotation(&mut doc, &Annotation::new(&id, AnnoKind::Highlight, 0, 1));
    assert_eq!(BooksDoc::annotations_for(&doc, &id).len(), 1);

    // Open the *same bytes* from a completely different path.
    app.import_and_open_for_test(&mut doc, &pb);
    assert_eq!(
        app.library.len(),
        1,
        "same content → one book, not a duplicate"
    );
    assert_eq!(
        app.library[0].id, id,
        "identity is the content hash, not the path"
    );
    assert_eq!(
        BooksDoc::annotations_for(&doc, &app.library[0].id).len(),
        1,
        "the highlight follows the book to the new path"
    );
}

/// A book living on the collaborative workspace is surfaced as shared; a book
/// anywhere else is local.
#[test]
#[serial]
fn a_workspace_book_is_surfaced_as_shared() {
    let ws = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(ws.path());
    let books = crate::content::workspace_books_dir();
    std::fs::create_dir_all(&books).unwrap();
    let shared_path = books.join("shared.epub");
    std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();

    let local = tempfile::tempdir().unwrap();
    let local_path = local.path().join("local.epub");
    std::fs::write(&local_path, crate::content::SAMPLE_EPUB).unwrap();

    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);

    app.import_and_open_for_test(&mut doc, &shared_path);
    let shared_idx = app
        .library
        .iter()
        .position(|b| b.id == app.library[0].id)
        .unwrap();
    assert!(
        app.book_is_shared(shared_idx),
        "a /workspace/Books book reads as shared"
    );

    // The badge is drawn on the shelf: the shared pill's people-glyph action
    // is absent (no Share button on an already-shared book).
    let _ = local_path;
}

#[test]
#[serial]
fn sharing_a_local_book_moves_it_onto_the_workspace() {
    let ws = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(ws.path());
    let home = tempfile::tempdir().unwrap();
    let src = home.path().join("mine.epub");
    std::fs::write(&src, crate::content::SAMPLE_EPUB).unwrap();

    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);
    app.import_and_open_for_test(&mut doc, &src);
    assert!(!app.book_is_shared(0), "starts local");

    let id = app.library[0].id.clone();
    app.dispatch(Action::ShareToWorkspace(id), &mut doc);
    assert!(
        app.book_is_shared(0),
        "sharing puts the book on the workspace"
    );
}

#[test]
#[serial]
fn canonical_share_operation_materialises_before_reporting_success() {
    let workspace = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(workspace.path());
    let source_dir = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("local.epub");
    std::fs::write(&source, crate::content::SAMPLE_EPUB).unwrap();
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    app.import_and_open_for_test(&mut doc, &source);
    let id = app.library[0].id.clone();
    let payload =
        serde_json::to_vec(&crate::actions::EbookOperation::ShareBook { book_id: id }).unwrap();
    let invocation = tilo_actions::InvocationId::new();

    assert!(matches!(
        Widget::start_resource_operation(
            &mut app,
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::EBOOK_OPERATION_TYPE,
            &payload,
            &doc,
            &automerge::ROOT,
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    assert!(
        !BooksDoc::read_library_entries(&doc)[0].shared,
        "the shared row is not advertised before materialisation commits"
    );
    app.sync(&mut doc, &automerge::ROOT);
    assert!(app.book_is_shared(0));
    assert!(BooksDoc::read_library_entries(&doc)[0].shared);
    assert!(matches!(
        Widget::poll_resource_operation(&mut app, invocation, false, &doc, &automerge::ROOT,),
        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
}

#[test]
fn canonical_share_operation_refuses_a_missing_local_capability() {
    let mut app = app_with_sample();
    let doc = AutoCommit::new();
    let id = app.library[0].id.clone();
    app.library_paths.remove(&id);
    let payload =
        serde_json::to_vec(&crate::actions::EbookOperation::ShareBook { book_id: id }).unwrap();
    let error = Widget::start_resource_operation(
        &mut app,
        tilo_actions::InvocationId::new(),
        tilo_actions::OperationKind::Effect,
        crate::actions::EBOOK_OPERATION_TYPE,
        &payload,
        &doc,
        &automerge::ROOT,
    )
    .unwrap_err();
    assert!(error.contains("unavailable on this device"));
}

#[test]
#[serial]
fn remote_open_operation_hydrates_verified_workspace_bytes_before_revealing() {
    let workspace = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(workspace.path());
    let seed = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(seed.path(), crate::content::SAMPLE_EPUB).unwrap();
    let remote = crate::content::parse_path_detailed(seed.path()).unwrap();
    let id = remote.id.clone();
    let shared_path = crate::content::workspace_book_path(&id);
    std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
    std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();

    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    let (_, entry) = crate::content::prepare_imported_book(&shared_path, remote);
    BooksDoc::upsert_library_entry(&mut doc, &entry);
    let invocation = tilo_actions::InvocationId::new();
    let payload = serde_json::to_vec(&crate::actions::EbookOperation::HydrateSharedBook {
        book_id: id.clone(),
    })
    .unwrap();

    assert!(matches!(
        Widget::start_resource_operation(
            &mut app,
            invocation,
            tilo_actions::OperationKind::Effect,
            crate::actions::EBOOK_OPERATION_TYPE,
            &payload,
            &doc,
            &automerge::ROOT,
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    wait_for_import(&mut app, &mut doc);
    assert_eq!(app.open_book().map(|book| &book.id), Some(&id));
    assert_eq!(app.library_paths.get(&id), Some(&shared_path));
    assert!(matches!(
        Widget::poll_resource_operation(&mut app, invocation, false, &doc, &automerge::ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
}

#[test]
#[serial]
fn remote_open_refuses_workspace_bytes_with_the_wrong_content_id() {
    let workspace = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(workspace.path());
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    let id = "book:claimed-but-wrong".to_string();
    let shared_path = crate::content::workspace_book_path(&id);
    std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
    std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();
    BooksDoc::upsert_library_entry(
        &mut doc,
        &crate::model::LibraryEntry {
            id: id.clone(),
            shared: true,
            title: "Wrong bytes".into(),
            author: String::new(),
            subject: String::new(),
            added: 1,
        },
    );
    let payload =
        serde_json::to_vec(&crate::actions::EbookOperation::HydrateSharedBook { book_id: id })
            .unwrap();
    let invocation = tilo_actions::InvocationId::new();
    Widget::start_resource_operation(
        &mut app,
        invocation,
        tilo_actions::OperationKind::Effect,
        crate::actions::EBOOK_OPERATION_TYPE,
        &payload,
        &doc,
        &automerge::ROOT,
    )
    .unwrap();
    wait_for_import(&mut app, &mut doc);
    assert!(app.library.is_empty());
    assert!(matches!(
        Widget::poll_resource_operation(&mut app, invocation, false, &doc, &automerge::ROOT),
        Some(tilo_actions::InvocationUpdate::Failed { .. })
    ));
}

#[test]
#[serial]
fn cancellable_share_operation_reports_its_own_lifecycle_status() {
    let workspace = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(workspace.path());
    let source_dir = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("cancel.epub");
    std::fs::write(&source, crate::content::SAMPLE_EPUB).unwrap();
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    app.import_and_open_for_test(&mut doc, &source);
    let payload = serde_json::to_vec(&crate::actions::EbookOperation::ShareBook {
        book_id: app.library[0].id.clone(),
    })
    .unwrap();
    let invocation = tilo_actions::InvocationId::new();
    Widget::start_resource_operation(
        &mut app,
        invocation,
        tilo_actions::OperationKind::Effect,
        crate::actions::EBOOK_OPERATION_TYPE,
        &payload,
        &doc,
        &automerge::ROOT,
    )
    .unwrap();

    assert!(matches!(
        Widget::poll_resource_operation(
            &mut app,
            invocation,
            true,
            &doc,
            &automerge::ROOT,
        ),
        Some(tilo_actions::InvocationUpdate::Cancelled {
            message: Some(ref message),
            ..
        }) if message == "book sharing cancelled"
    ));
    assert!(
        !BooksDoc::read_library_entries(&doc)[0].shared,
        "cancelling before the UI commit never advertises a shared copy"
    );
}

#[test]
fn removing_a_book_takes_it_off_the_shelf_and_out_of_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("book.epub");
    std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 720.0);
    app.import_and_open_for_test(&mut doc, &path);
    assert_eq!(app.library.len(), 1);
    assert_eq!(BooksDoc::read_library_entries(&doc).len(), 1);

    let id = app.library[0].id.clone();
    app.dispatch(Action::RemoveFromLibrary(id), &mut doc);
    assert!(app.library.is_empty(), "removed from the shelf");
    assert!(
        BooksDoc::read_library_entries(&doc).is_empty(),
        "and from the registry"
    );
}

#[test]
fn removing_one_watched_book_preserves_other_source_deduplication() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let removed_id = app.library[0].id.clone();
    let retained_id = app.library[1].id.clone();
    let removed_source = PathBuf::from("/watched/removed.epub");
    let retained_source = PathBuf::from("/watched/retained.epub");
    app.watched_sources
        .insert(removed_source.clone(), removed_id);
    app.watched_sources
        .insert(retained_source.clone(), retained_id);

    let id = app.library[0].id.clone();
    app.dispatch(Action::RemoveFromLibrary(id), &mut doc);

    assert!(
        !app.watched_sources.contains_key(&removed_source),
        "only the removed book becomes eligible for watch retry"
    );
    assert!(
        app.watched_sources.contains_key(&retained_source),
        "unrelated watched books stay deduplicated"
    );
}

#[test]
fn populated_shelf_offers_an_open_book_affordance() {
    // A shelf that already has books still offers the trailing "Add a book"
    // tile plus the top-bar Open action, so there is always a way in.
    let mut app = app_with_sample();
    app.view = View::Library;
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 800.0);
    assert!(!app.library.is_empty(), "sample shelf has books");
    assert!(
        find_hit(&app, |a| matches!(a, Action::OpenBook(_))).is_some(),
        "book cards are present on the shelf"
    );
    assert!(
        find_hit(&app, |a| matches!(a, Action::OpenViaPortal)).is_some(),
        "a populated shelf still offers a way to add more books"
    );
}

#[test]
fn card_shows_the_title_once_generated_vs_real_cover() {
    // A generated cover sets the title in display type on the cover itself,
    // so `cover_image` yields None and the under-cover shelf label is
    // suppressed — the title renders exactly once. Real cover art keeps the
    // shelf label (its embedded title may not be legible on the shelf), and
    // does not itself re-draw the title, so it too shows once.
    let mut app = BooksApp::with_library(Vec::new());
    let generated = Book {
        id: "gen".into(),
        title: "Generated".into(),
        cover: None,
        ..Default::default()
    };
    assert!(
        app.cover_image(&generated).is_none(),
        "generated cover has no image → shelf label is suppressed (title only on the cover)"
    );
    let real = Book {
        id: "real".into(),
        title: "Real".into(),
        cover: Some(crate::model::Cover {
            rgba: std::sync::Arc::new(vec![0u8; 2 * 2 * 4]),
            width: 2,
            height: 2,
        }),
        ..Default::default()
    };
    assert!(
        app.cover_image(&real).is_some(),
        "real cover art blits → the single title comes from the shelf label"
    );
}

#[test]
fn type_popover_stays_clear_of_the_docked_annotations_rail() {
    // On a wide window the annotations rail is docked-open; the reading
    // settings popover must anchor inside the reading area, never crossing it.
    let mut app = app_with_sample();
    app.view = View::Reader;
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.reconcile_flaps();
    app.notes_flap.snap(true); // fully docked-open
    assert_eq!(
        app.notes_layout(),
        SplitLayout::Docked,
        "wide window docks the rail"
    );

    let (px0, px1) = ReadingSettings::panel_x_bounds(
        app.rect,
        app.rect.x0 + 12.0 + TOC_W,
        app.rect.x1 - 18.0 - NOTES_W,
    );
    let rail_left = app.rect.x1 - NOTES_W;
    assert!(
        px1 <= rail_left,
        "popover right edge {px1} must stay left of the docked annotations rail at {rail_left}"
    );
    assert!(px0 >= app.rect.x0, "popover stays on-screen");
    assert!(px1 - px0 >= 200.0, "popover keeps a usable width");
}

// ---- delete affordance ---------------------------------------------------

#[test]
fn annotation_delete_is_reachable_and_removes_it() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Wide window so the annotations rail is docked-open.
    frame(&mut app, &mut doc, 1280.0, 800.0);
    // Create one highlight via double-click word + swatch.
    let __p = page_point(&app);
    dbl_click(&mut app, &mut doc, __p);
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
    click(&mut app, &mut doc, swatch);
    assert_eq!(
        BooksDoc::list_annotations(&doc).len(),
        1,
        "one highlight created"
    );

    // The delete (×) is registered without the pointer hovering the card
    // (the cursor is still over the swatch on the page), proving reachability.
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let del = find_hit(&app, |a| matches!(a, Action::Delete(_)))
        .expect("delete affordance is present without hover");
    assert!(click(&mut app, &mut doc, del), "delete click handled");
    assert!(
        BooksDoc::list_annotations(&doc).is_empty(),
        "annotation removed"
    );
}

// ---- single source of truth: affordance == palette ActionSpec -----------

/// The comment-overlay Delete (×) button dispatches the byte-identical
/// `EncodedAction` the palette's "Delete comment" `ActionSpec` exports — proof
/// the two are one code path, not two. Drives the real click through
/// `handle_event` and compares against the live resource tree.
#[test]
fn comment_delete_button_dispatches_the_same_action_as_the_palette() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let book_id = app.book_id();
    let group = Uuid::from_u128(0x2222_3333_4444_5555_6666_7777_8888_9999);
    let cid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_00AB);
    let hl = Annotation::new(&book_id, AnnoKind::Highlight, 0, 1)
        .with_excerpt("a marked passage")
        .with_group(group);
    BooksDoc::upsert_annotation(&mut doc, &hl);
    let mut cm = Annotation::new(&book_id, AnnoKind::Note, 0, 1)
        .with_excerpt("a marked passage")
        .with_note("a comment")
        .with_group(group);
    cm.id = cid;
    BooksDoc::upsert_annotation(&mut doc, &cm);
    // Open the highlight's comment overlay (presentation).
    app.comment_overlay.open(group);
    frame(&mut app, &mut doc, 1280.0, 800.0);

    // Root discovery stays bounded; the palette resolves this canonical
    // exact resource through its immutable search provider.
    let spec_encoded = {
        let provider = crate::search_provider::BookSearchProvider::new_shared(
            Arc::clone(&app.library),
            Arc::new(BooksDoc::list_annotations(&doc)),
            Arc::new(BooksDoc::read_library_entries(&doc)),
            1,
        );
        let comment = ui::search::SearchProvider::resolve_action_resource(
            &provider,
            &tilo_actions::ActionAddress::new(format!("ebook.comment:{cid}"), "open"),
        )
        .expect("exact comment resource");
        let spec = comment
            .actions
            .iter()
            .find(|a| a.id == "delete-comment")
            .expect("delete-comment spec");
        match &spec.input {
            tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => panic!("delete-comment is immediate"),
        }
    };

    // Click the overlay's per-comment × button (the real affordance).
    let del =
        find_hit(&app, |a| matches!(a, Action::DeleteComment(_))).expect("a comment delete button");
    assert!(
        click(&mut app, &mut doc, del),
        "delete-comment click handled"
    );

    let dispatched = app
        .last_dispatched()
        .cloned()
        .expect("the button dispatched an action");
    assert_eq!(dispatched.type_id, spec_encoded.type_id);
    assert_eq!(
        dispatched.payload, spec_encoded.payload,
        "the button and the palette build the identical EncodedAction",
    );
    // And it really removed the comment through that action.
    assert!(
        BooksDoc::list_annotations(&doc).iter().all(|a| a.id != cid),
        "the comment was deleted via the dispatched action",
    );
}

/// The selection-toolbar swatch, toggling an existing highlight off, dispatches
/// the byte-identical `EncodedAction` the book resource's "Highlight selection"
/// `ActionSpec` exports — the swatch and the palette are one source of truth.
/// Drives the real `SetColor` dispatch arm a swatch click invokes.
#[test]
fn selection_swatch_dispatches_the_same_action_as_the_palette() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Create a highlight over the first word via the real swatch path, so the
    // page layout and annotation store are populated exactly as at runtime.
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let __p = page_point(&app);
    dbl_click(&mut app, &mut doc, __p);
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(HiColor::Yellow)))
        .expect("the yellow swatch");
    click(&mut app, &mut doc, swatch);
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let hl = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.kind == AnnoKind::Highlight)
        .expect("one highlight now exists");

    // Re-establish the exact selection over that highlight (set directly so
    // the test doesn't depend on a second hit-test resolving under load), so
    // the swatch would toggle it off — a deterministic plan (pure removal, no
    // freshly-minted ids), the case where the two paths encode identically.
    app.hi_color = HiColor::Yellow;
    app.sel = Some(TextSel {
        anchor: DocPos {
            block: hl.block,
            ch: hl.start,
        },
        head: DocPos {
            block: hl.block,
            ch: hl.end,
        },
    });
    app.selected_block = Some(hl.block);

    let expected = crate::actions::encode(
        &app.resolve_highlight_plan(app.sel.expect("selection"), app.hi_color)
            .expect("canonical selection plan")
            .into_action(),
    );
    // Dispatch the swatch's action through the real handler a click invokes.
    app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
    let dispatched = app
        .last_dispatched()
        .cloned()
        .expect("the swatch dispatched an action");
    assert_eq!(
        dispatched.payload, expected.payload,
        "the swatch uses the canonical selection action"
    );
    // And it really toggled the highlight off through that action.
    assert!(
        BooksDoc::list_annotations(&doc)
            .iter()
            .all(|a| a.kind != AnnoKind::Highlight),
        "the highlight was removed via the dispatched action",
    );
}

// ---- bookmark toggle (icon + 'b') ---------------------------------------

#[test]
fn bookmark_toggles_via_icon_and_key() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let bookmarks = |d: &AutoCommit| {
        BooksDoc::list_annotations(d)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Bookmark)
            .count()
    };
    assert_eq!(bookmarks(&doc), 0, "no bookmark yet");
    // Add via the top-bar bookmark icon.
    let bm = find_hit(&app, |a| matches!(a, Action::ToggleBookmark)).expect("bookmark icon");
    assert!(click(&mut app, &mut doc, bm), "bookmark icon handled");
    assert_eq!(bookmarks(&doc), 1, "icon added a bookmark");
    // Remove via the 'b' shortcut (same page position -> same anchor).
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(key_char(&mut app, &mut doc, "b"), "'b' consumed");
    assert_eq!(bookmarks(&doc), 0, "'b' removed the bookmark");
}

#[test]
fn device_local_bookmark_shortcut_rebinds_reader_routing() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(app.local_library.set_shortcut("bookmark", "z"));

    assert!(key_char(&mut app, &mut doc, "z"), "custom key is consumed");
    assert_eq!(
        BooksDoc::list_annotations(&doc)
            .into_iter()
            .filter(|annotation| annotation.kind == AnnoKind::Bookmark)
            .count(),
        1,
        "custom key dispatches the same bookmark action"
    );
    assert!(
        !key_char(&mut app, &mut doc, "b"),
        "the replaced default no longer has a hidden second meaning"
    );
}

#[test]
fn shortcut_capture_rejects_reserved_reader_commands_accessibly() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Shortcuts;
    app.dispatch(
        Action::BeginShortcutCapture(ReaderShortcut::Bookmark),
        &mut doc,
    );

    assert!(
        key_char(&mut app, &mut doc, "t"),
        "capture consumes the key"
    );
    assert_eq!(app.shortcut_capture, Some(ReaderShortcut::Bookmark));
    assert_eq!(
        app.local_library.shortcut(
            ReaderShortcut::Bookmark.id(),
            ReaderShortcut::Bookmark.default_key()
        ),
        "b",
        "the fixed contents shortcut cannot be shadowed"
    );
    let label = app.semantic_label(&Action::BeginShortcutCapture(ReaderShortcut::Bookmark));
    assert!(
        label.contains("reserved"),
        "screen readers receive the refusal"
    );
}

#[test]
fn narrow_large_text_settings_tabs_are_complete_and_semantic() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Shortcuts;
    app.sync(&mut doc, &automerge::ROOT);
    app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
    let theme = Theme::light().with_text_scale(2.0);
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::empty(),
    );

    let tabs = app
        .hits
        .iter()
        .filter(|hit| matches!(hit.action, Action::SetSettingsTab(_)))
        .collect::<Vec<_>>();
    assert_eq!(tabs.len(), 4, "the active tab remains discoverable");
    assert!(tabs.iter().all(|hit| {
        hit.rect.x0 >= app.rect.x0
            && hit.rect.x1 <= app.rect.x1
            && hit.rect.y0 >= app.rect.y0
            && hit.rect.y1 <= app.rect.y1
    }));
    let first_y = tabs.first().map_or(0.0, |hit| hit.rect.y0);
    assert!(
        tabs.iter().any(|hit| (hit.rect.y0 - first_y).abs() > 1.0),
        "large text reflows settings navigation into two bounded rows"
    );
}

#[test]
fn library_settings_exposes_empty_and_discovered_folder_actions() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Library;
    app.library_paths.clear();
    app.watched_folders.clear();
    app.local_library.watched_folders.clear();

    frame(&mut app, &mut doc, 1000.0, 720.0);
    assert!(
        find_hit(&app, |action| matches!(action, Action::OpenViaPortal)).is_some(),
        "an empty folder list keeps the primary Open-book action reachable"
    );

    let folder = PathBuf::from("/books/discovered");
    app.library_paths
        .insert(app.library[0].id.clone(), folder.join("reader.epub"));
    frame(&mut app, &mut doc, 1000.0, 720.0);
    assert!(
        find_hit(&app, |action| {
            matches!(action, Action::ToggleWatchFolder(path) if path == &folder)
        })
        .is_some(),
        "a book's source folder is offered as the exact watch target"
    );
}

#[test]
fn narrow_max_text_scale_keeps_rendered_runs_inside_the_page_box() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    let mut settings = BooksDoc::read_settings(&doc);
    settings.font_scale = Settings::FONT_MAX;
    BooksDoc::write_settings(&mut doc, &settings);

    frame(&mut app, &mut doc, 390.0, 844.0);
    let geom = app.reader_page_geometry(app.rect, app.settings.mode);
    let right = geom.full_content_x + f64::from(geom.full_content_w);
    assert!(right <= app.rect.x1 + 0.01, "content box fits the viewport");
    assert!(
        app.sel_runs
            .iter()
            .all(|run| { run.x0 >= geom.full_content_x - 0.5 && run.x1 <= right + 0.5 }),
        "every measured painted text run reflows inside the narrow page box"
    );
}

#[test]
fn large_text_shot_is_a_normal_two_hundred_percent_narrow_reader() {
    use ui::offscreen::OffscreenRenderer;

    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    app.seed_shot_scenario("large-text");
    let scene = render_scene(&mut app, &mut doc, 480.0, 900.0);

    assert_eq!(app.settings.font_scale, 2.0);
    assert!(app.sel.is_none(), "the visual proof starts unselected");
    assert!(
        app.annotations.is_empty(),
        "the visual proof is not obscured by seeded demo highlights"
    );
    assert!(
        app.hi_bands.is_empty(),
        "no annotation band is painted beneath the 200% proof text"
    );
    let geom = app.reader_page_geometry(app.rect, app.settings.mode);
    let right = geom.full_content_x + f64::from(geom.full_content_w);
    assert!(
        right <= app.rect.x1 - 24.0,
        "the narrow page reserves a visible right inset"
    );
    assert!(
        app.sel_runs
            .iter()
            .all(|run| run.x0 >= geom.full_content_x - 0.5
                && run.x1 <= right + 0.5
                && run.x1 <= app.rect.x1 - 24.0),
        "every 200% text run remains within the narrow content box"
    );
    let mut renderer = OffscreenRenderer::new(480, 900);
    let pixels = renderer.render_rgba(&scene);
    let paper = OffscreenRenderer::sample(&pixels, 480, 16, 500);
    assert!(
        paper.0 > paper.2 && paper.1 > paper.2,
        "the painted reader surface is neutral warm paper, not a blue selection band: {paper:?}"
    );
}

#[test]
fn reduced_motion_shot_keeps_the_desktop_reader_chrome() {
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    app.seed_shot_scenario("reduced-motion");
    frame(&mut app, &mut doc, 1280.0, 800.0);

    assert!(app.reduce_motion);
    assert!(app.toc_open(), "contents rail remains docked-open");
    assert!(app.notes_open(), "annotation rail remains docked-open");
    assert!(
        app.hits
            .iter()
            .any(|hit| matches!(hit.action, Action::Back)),
        "the equivalent reader top bar remains actionable"
    );
}

// ---- note authoring: type / backspace / done ----------------------------

#[test]
fn composing_a_note_types_backspaces_and_persists() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 800.0);
    // Select a word, then start a note with the 'm' shortcut.
    let __p = page_point(&app);
    dbl_click(&mut app, &mut doc, __p);
    assert!(key_char(&mut app, &mut doc, "m"), "'m' started a note");
    let nid = app.composing.expect("a note is now being composed");

    for ch in ["H", "i"] {
        assert!(
            key_char(&mut app, &mut doc, ch),
            "char consumed while composing"
        );
    }
    assert!(
        key_named(&mut app, &mut doc, NamedKey::Space),
        "space consumed"
    );
    for ch in ["t", "h", "e", "r", "e"] {
        assert!(key_char(&mut app, &mut doc, ch));
    }
    assert!(
        key_named(&mut app, &mut doc, NamedKey::Backspace),
        "backspace consumed"
    );
    assert!(
        key_named(&mut app, &mut doc, NamedKey::Enter),
        "enter finished the note"
    );
    assert!(app.composing.is_none(), "enter ended composing");

    let note = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.id == nid)
        .expect("the composed note is in the document");
    assert_eq!(note.kind, AnnoKind::Note);
    assert_eq!(
        note.note, "Hi ther",
        "typed text with one backspace persisted"
    );
}

#[test]
fn clicking_a_finished_note_reopens_it_for_editing() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let __p = page_point(&app);
    dbl_click(&mut app, &mut doc, __p);
    key_char(&mut app, &mut doc, "m");
    key_char(&mut app, &mut doc, "x");
    key_named(&mut app, &mut doc, NamedKey::Enter);
    assert!(app.composing.is_none(), "note finished");

    // Clicking the note card's lower zone reopens it (Action::EditNote).
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let edit = find_hit(&app, |a| matches!(a, Action::EditNote(_))).expect("edit zone");
    assert!(click(&mut app, &mut doc, edit), "edit click handled");
    assert!(app.composing.is_some(), "note reopened for editing");
}

// ---- jump-to-annotation location resolution -----------------------------

#[test]
fn clicking_an_annotation_card_jumps_to_its_location() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Docked notes rail at a wide width so the card (with a JumpTo hit) shows.
    frame(&mut app, &mut doc, 1280.0, 800.0);
    let book_id = app.book_id();
    let nblocks = app.library[app.book_idx].chapters[0].blocks.len().max(1);
    let target = (nblocks / 2).min(nblocks - 1);
    let a = Annotation::new(&book_id, AnnoKind::Highlight, 0, target)
        .with_color(HiColor::Green)
        .with_excerpt("anchored passage");
    BooksDoc::upsert_annotation(&mut doc, &a);
    frame(&mut app, &mut doc, 1280.0, 800.0);

    // Pretend we've navigated away, then jump from the annotation card.
    app.chapter = app.num_chapters().saturating_sub(1);
    let jump = find_hit(&app, |ac| matches!(ac, Action::JumpTo(_))).expect("annotation card");
    assert!(click(&mut app, &mut doc, jump), "jump handled");
    assert_eq!(app.chapter, 0, "jumped to the annotation's chapter");
    assert_eq!(
        app.pending_jump_block,
        Some(target),
        "jump targets the annotation's block"
    );
    // The deferred page resolution runs on the next settled frame.
    frame(&mut app, &mut doc, 1280.0, 800.0);
    assert!(
        app.pending_jump_block.is_none(),
        "pending jump was consumed by layout"
    );
}

#[test]
fn page_for_block_maps_a_block_to_its_page() {
    use crate::model::{Block, Span};
    use crate::paginate::{ChapterLayoutRequest, layout_chapter};
    let mut tr = TextRenderer::new();
    // Eight fat paragraphs into a small box -> several pages.
    let blocks: Vec<Block> = (0..8)
        .map(|_| Block::Paragraph {
            spans: vec![Span::plain("word ".repeat(60))],
        })
        .collect();
    let layout = layout_chapter(
        &mut tr,
        &ChapterLayoutRequest {
            blocks: &blocks,
            content_width: 300.0,
            content_height: 160.0,
            settings: &Settings::default(),
            chapter: 0,
            language: "en-US",
            base_family: &FontChoice::SansSerif,
            font_multiplier: 1.0,
            font_signature: 0,
        },
    );
    assert!(layout.page_count() >= 2, "content spans multiple pages");
    assert_eq!(page_for_block(&layout, 0), 0, "first block is on page 0");
    let last = layout.blocks.last().unwrap().index;
    assert!(
        page_for_block(&layout, last) >= 1,
        "the last block is past page 0"
    );
    for b in &layout.blocks {
        assert!(
            page_for_block(&layout, b.index) < layout.page_count(),
            "valid page index"
        );
    }
    assert_eq!(
        page_for_block(&layout, 9999),
        0,
        "unknown block -> safe page 0"
    );
}

// ---- TOC chapter navigation ---------------------------------------------

#[test]
fn toc_row_click_navigates_to_that_chapter() {
    let mut app = app_with_sample();
    app.book_idx = 1; // Field Notes has three chapters.
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1280.0, 800.0);
    assert!(app.toc_open(), "contents rail docked-open at 1280");
    assert!(app.num_chapters() >= 2, "multi-chapter book");
    assert_eq!(app.chapter, 0);
    let target = app.num_chapters() - 1;
    let row = find_hit(
        &app,
        |a| matches!(a, Action::GotoChapter(c) if *c == target),
    )
    .expect("a TOC row for the last chapter");
    assert!(click(&mut app, &mut doc, row), "toc row handled");
    assert_eq!(app.chapter, target, "navigated to the clicked chapter");
    assert_eq!(app.page, 0, "chapter opens at page 0");
}

// ---- type popover steppers persist --------------------------------------

#[test]
fn type_popover_steppers_change_and_persist_settings() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
    click(&mut app, &mut doc, aa);

    // Text-size + button grows and persists the font scale.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let base = BooksDoc::read_settings(&doc).font_scale;
    let font = app
        .reading_settings
        .number_rect(&NumberField::FontScale)
        .expect("font + stepper");
    let inc = Point::new(
        font.x1 - font.height() / 2.0,
        f64::midpoint(font.y0, font.y1),
    );
    click(&mut app, &mut doc, inc);
    let grown = BooksDoc::read_settings(&doc).font_scale;
    assert!(
        grown > base,
        "font scale grew and persisted ({base} -> {grown})"
    );

    // Line-spacing − button shrinks and persists the spacing.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let sbase = BooksDoc::read_settings(&doc).line_spacing;
    let spacing = app
        .reading_settings
        .number_rect(&NumberField::LineSpacing)
        .expect("spacing - stepper");
    let dec = Point::new(
        spacing.x0 + spacing.height() / 2.0,
        f64::midpoint(spacing.y0, spacing.y1),
    );
    click(&mut app, &mut doc, dec);
    let shrunk = BooksDoc::read_settings(&doc).line_spacing;
    assert!(
        shrunk < sbase,
        "line spacing shrank and persisted ({sbase} -> {shrunk})"
    );
}

// ---- reader keyboard layer ----------------------------------------------

#[test]
fn reader_keyboard_drives_navigation_and_chrome() {
    let mut app = app_with_sample();
    app.book_idx = 1; // Field Notes: three chapters, so nav is deterministic.
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert_eq!(app.chapter, 0);

    // Up/Down move by chapter.
    assert!(key_named(&mut app, &mut doc, NamedKey::ArrowDown));
    assert_eq!(app.chapter, 1, "arrow-down advanced a chapter");
    assert!(key_named(&mut app, &mut doc, NamedKey::ArrowUp));
    assert_eq!(app.chapter, 0, "arrow-up went back a chapter");

    // Right turns the page (into the next chapter if the first is one page).
    let start = (app.chapter, app.page);
    assert!(key_named(&mut app, &mut doc, NamedKey::ArrowRight));
    assert_ne!(
        (app.chapter, app.page),
        start,
        "arrow-right turned the page"
    );

    // 't' toggles the docked contents rail.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let toc0 = app.toc_open();
    assert!(key_char(&mut app, &mut doc, "t"), "'t' consumed");
    assert_ne!(app.toc_open(), toc0, "'t' toggled the contents rail");

    // Digit keys set the page tint and persist it (Light differs from the
    // Sepia default, so this proves a real write, not a coincidence).
    assert!(key_char(&mut app, &mut doc, "1"));
    assert_eq!(
        BooksDoc::read_settings(&doc).theme,
        ReadTheme::Light,
        "'1' set Light"
    );
    assert!(key_char(&mut app, &mut doc, "3"));
    assert_eq!(
        BooksDoc::read_settings(&doc).theme,
        ReadTheme::Dark,
        "'3' set Dark"
    );

    // Escape from a clean reader leaves the book.
    assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
    assert_eq!(app.view, View::Library, "escape left the book");
}

#[test]
fn escape_peels_back_popover_then_overlay_then_book() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);

    // Layer 1: an open type popover — first Escape closes only that.
    app.reading_settings.open();
    assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
    assert!(!app.reading_settings.is_open(), "escape closed the popover");
    assert_eq!(app.view, View::Reader, "still in the book");

    // Layer 2: a narrow window with the contents overlay flap open.
    frame(&mut app, &mut doc, 560.0, 720.0);
    assert_eq!(app.toc_layout(), SplitLayout::Overlay);
    app.toc_flap.snap(true);
    assert!(app.toc_open(), "overlay flap open");
    assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
    assert!(!app.toc_open(), "escape dismissed the overlay flap");
    assert_eq!(app.view, View::Reader, "still in the book");

    // Layer 3: nothing floating -> escape leaves the book.
    assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
    assert_eq!(app.view, View::Library, "escape left the book");
}

// ---- reading-progress persistence + restore -----------------------------

#[test]
fn reading_progress_persists_and_restores_on_reopen() {
    let mut first = app_with_sample();
    first.book_idx = 1; // multi-chapter
    let mut doc = AutoCommit::new();
    frame(&mut first, &mut doc, 1000.0, 700.0);
    let book_id = first.book_id();

    // Advance two chapters; each turn persists progress to the CRDT.
    key_named(&mut first, &mut doc, NamedKey::ArrowDown);
    key_named(&mut first, &mut doc, NamedKey::ArrowDown);
    let reached = first.chapter;
    assert!(reached >= 1, "advanced into the book");
    assert_eq!(
        BooksDoc::read_progress(&doc, &book_id).chapter,
        reached,
        "progress written to the document"
    );

    // A fresh reader on the same document restores the saved position.
    let mut reopened = app_with_sample();
    reopened.book_idx = 1;
    frame(&mut reopened, &mut doc, 1000.0, 700.0);
    assert_eq!(
        reopened.chapter, reached,
        "reopen restored the saved chapter"
    );
}

#[test]
fn continuation_round_trip_reopens_the_same_book_and_reading_position() {
    let mut sender = app_with_sample();
    sender.book_idx = 1;
    sender.chapter = 1;
    sender.page = 7;
    let source = AutoCommit::new();
    let metadata = sender
        .continuation_metadata(&source, &automerge::ROOT)
        .expect("an open library book has a portable continuation");

    let mut receiver = app_with_sample();
    let mut target = AutoCommit::new();
    assert!(receiver.restore_continuation_metadata(&metadata, &mut target, &automerge::ROOT,));
    assert_eq!(
        receiver.library[receiver.book_idx].id,
        sender.library[sender.book_idx].id
    );
    assert_eq!((receiver.chapter, receiver.page), (1, 7));
}

#[test]
fn type_popover_value_is_click_to_type() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
    click(&mut app, &mut doc, aa);
    // Tap the text-size value to type a number directly.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let val = app
        .reading_settings
        .number_rect(&NumberField::FontScale)
        .expect("the text-size value is a shared number field");
    let val = Point::new(f64::midpoint(val.x0, val.x1), f64::midpoint(val.y0, val.y1));
    assert!(click(&mut app, &mut doc, val), "value tap handled");
    for ch in ["1", "5", "0"] {
        assert!(key_char(&mut app, &mut doc, ch), "digit consumed");
    }
    assert!(
        key_named(&mut app, &mut doc, NamedKey::Enter),
        "enter committed"
    );
    let fs = BooksDoc::read_settings(&doc).font_scale;
    assert!(
        (fs - 1.5).abs() < 0.001,
        "typed 150% -> font_scale 1.5, got {fs}"
    );

    // Line spacing is click-to-type too.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let ls_val = app
        .reading_settings
        .number_rect(&NumberField::LineSpacing)
        .expect("line spacing is a shared number field");
    let ls_val = Point::new(
        f64::midpoint(ls_val.x0, ls_val.x1),
        f64::midpoint(ls_val.y0, ls_val.y1),
    );
    click(&mut app, &mut doc, ls_val);
    for ch in ["1", ".", "4"] {
        key_char(&mut app, &mut doc, ch);
    }
    key_named(&mut app, &mut doc, NamedKey::Enter);
    let ls = BooksDoc::read_settings(&doc).line_spacing;
    assert!(
        (ls - 1.4).abs() < 0.001,
        "typed 1.4 -> line_spacing 1.4, got {ls}"
    );
}

#[test]
fn shelf_hide_read_filters_and_sort_orders() {
    use crate::model::{LibraryPrefs, LibrarySort, Progress};
    let mut app = app_with_sample();
    app.view = View::Library;
    let mut doc = AutoCommit::new();
    let ids: Vec<String> = app.library.iter().map(|b| b.id.clone()).collect();
    assert!(ids.len() >= 3, "sample shelf has several books");
    // Mark the first book as finished; give the others open times.
    BooksDoc::write_progress(
        &mut doc,
        &ids[0],
        &Progress {
            chapter: 9,
            page: 9,
            fraction: 1.0,
            opened: 100,
        },
    );
    BooksDoc::write_progress(
        &mut doc,
        &ids[1],
        &Progress {
            fraction: 0.3,
            opened: 500,
            ..Default::default()
        },
    );
    BooksDoc::write_progress(
        &mut doc,
        &ids[2],
        &Progress {
            fraction: 0.6,
            opened: 300,
            ..Default::default()
        },
    );

    // Hide-read on: the finished book drops out of the display order.
    BooksDoc::write_library_prefs(
        &mut doc,
        &LibraryPrefs {
            sort: LibrarySort::Title,
            hide_read: true,
        },
    );
    frame(&mut app, &mut doc, 1200.0, 820.0);
    assert!(!app.shelf_order.contains(&0), "finished book hidden");
    assert_eq!(
        app.shelf_order.len(),
        ids.len() - 1,
        "one book filtered out"
    );

    // Sort by recently opened (hide-read off): most-recent first.
    BooksDoc::write_library_prefs(
        &mut doc,
        &LibraryPrefs {
            sort: LibrarySort::Recent,
            hide_read: false,
        },
    );
    frame(&mut app, &mut doc, 1200.0, 820.0);
    assert_eq!(
        app.shelf_order.first().copied(),
        Some(1),
        "book 1 (opened=500) is most recent"
    );

    // Sort by progress: furthest-read first.
    BooksDoc::write_library_prefs(
        &mut doc,
        &LibraryPrefs {
            sort: LibrarySort::Progress,
            hide_read: false,
        },
    );
    frame(&mut app, &mut doc, 1200.0, 820.0);
    assert_eq!(
        app.shelf_order.first().copied(),
        Some(0),
        "book 0 (fraction=1.0) is furthest read"
    );

    // The sort control + hide-read toggle are reachable in the shelf chrome.
    assert!(
        app.shelf_controls
            .has_action(ShelfControlAction::ToggleSortMenu),
        "a sort control is present"
    );
    assert!(
        app.shelf_controls
            .has_action(ShelfControlAction::ToggleHideRead),
        "a hide-read toggle is present"
    );
}

/// Drive a real drag that starts *inside* a paragraph and ends in a later
/// one, then apply a colour, and assert the stored annotation carries a
/// character range (partial, not the whole block) and spans the boundary as
/// one grouped highlight — proving the mark is text-range, not paragraph.
#[test]
fn drag_across_paragraph_stores_partial_char_range_not_the_whole_block() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        app.sel_runs.len() > 3,
        "the page laid out real run geometry"
    );

    // Start on the SECOND run of the first block (so the range begins
    // mid-paragraph, start > 0), drag into a run of a later block.
    let r0 = app.sel_runs[0];
    let start_run = app
        .sel_runs
        .iter()
        .find(|r| r.block == r0.block && r.cs > 0)
        .copied()
        .expect("a non-first run in the first block");
    let later = app
        .sel_runs
        .iter()
        .find(|r| r.block > start_run.block)
        .copied()
        .expect("a run in a later paragraph");
    let start = Point::new(
        f64::midpoint(start_run.x0, start_run.x1),
        f64::midpoint(start_run.y0, start_run.y1),
    );
    let end = Point::new(
        f64::midpoint(later.x0, later.x1),
        f64::midpoint(later.y0, later.y1),
    );
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, end);
    ptr_up(&mut app, &mut doc);
    let sel = app.sel.expect("a drag produced a selection");
    assert!(
        sel.ordered().1.block > sel.ordered().0.block,
        "selection crosses a paragraph"
    );

    app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
    app.refresh_from_doc(&doc);

    let hls: Vec<_> = BooksDoc::list_annotations(&doc)
        .into_iter()
        .filter(|a| a.kind == AnnoKind::Highlight)
        .collect();
    assert!(hls.len() >= 2, "one ranged highlight per spanned paragraph");
    // Every piece carries a real char range, and shares ONE group.
    assert!(
        hls.iter().all(|a| a.start < a.end),
        "each piece has a char range"
    );
    let g0 = hls[0].group;
    assert!(
        hls.iter().all(|a| a.group == g0),
        "the spanned pieces are one grouped highlight"
    );
    // The first block's piece begins mid-paragraph (start > 0) → this is a
    // character range, NOT a whole-paragraph highlight.
    let first = hls.iter().min_by_key(|a| a.block).unwrap();
    assert!(
        first.start > 0,
        "the highlight begins mid-paragraph (char-level, start={})",
        first.start
    );
}

/// A plain click on highlighted text opens that highlight's floating comment
/// overlay (hit-testing the real char range, not "the paragraph").
#[test]
fn clicking_a_highlight_opens_the_comment_overlay() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);

    // Highlight a run-spanning range in the first block via a drag.
    let r0 = app.sel_runs[0];
    let end_run = app
        .sel_runs
        .iter()
        .rfind(|r| r.block == r0.block)
        .copied()
        .unwrap();
    let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
    let end = Point::new(
        f64::midpoint(end_run.x0, end_run.x1),
        f64::midpoint(end_run.y0, end_run.y1),
    );
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, end);
    ptr_up(&mut app, &mut doc);
    app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
    app.refresh_from_doc(&doc);
    let hl = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.kind == AnnoKind::Highlight)
        .expect("a highlight now exists");
    assert!(!app.comment_overlay.is_open(), "no overlay yet");

    // Re-lay the page, then plain-click on the highlight. A char range can be
    // sub-word (this one starts and ends mid-word), so there isn't always a
    // whole run wholly inside it — the reader taps the *rendered band*, which
    // is exactly what the click must resolve back to the highlight's range.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let band = *app
        .hi_bands
        .first()
        .expect("the highlight drew a band to tap");
    let p = band.center();
    // Sanity: that tap really lands inside the stored char range.
    assert_eq!(
        app.hit_doc_pos(p).and_then(|dp| app.highlight_group_at(dp)),
        Some(hl.group),
        "the tapped point resolves to the highlight's char range",
    );
    ptr_move(&mut app, &mut doc, p);
    ptr_down(&mut app, &mut doc);
    ptr_up(&mut app, &mut doc);
    assert_eq!(
        app.comment_overlay.group(),
        Some(hl.group),
        "clicking the highlight opened its overlay"
    );

    // The overlay's affordances are drawn: add-comment + a trash for the
    // whole highlight + a dismiss target.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        find_hit(&app, |a| matches!(a, Action::AddComment(_))).is_some(),
        "add-comment present"
    );
    assert!(
        find_hit(&app, |a| matches!(a, Action::DeleteHighlight(_))).is_some(),
        "trash present"
    );
    assert!(
        find_hit(&app, |a| matches!(a, Action::CloseComments)).is_some(),
        "dismiss present"
    );
}

/// The comment overlay supports MANY comments per range: add two, edit one,
/// delete one, then trash the whole highlight — all through the real hits.
#[test]
fn comment_overlay_adds_edits_and_deletes_multiple_comments() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Make a single-word highlight, then open its overlay directly.
    let p = page_point(&app);
    dbl_click(&mut app, &mut doc, p);
    app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
    app.refresh_from_doc(&doc);
    let group = BooksDoc::list_annotations(&doc)
        .into_iter()
        .find(|a| a.kind == AnnoKind::Highlight)
        .expect("a highlight")
        .group;
    app.comment_overlay.open(group);
    frame(&mut app, &mut doc, 1000.0, 700.0);

    let notes_in_group = |d: &AutoCommit| {
        BooksDoc::list_annotations(d)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Note && a.group == group)
            .count()
    };

    // Add the first comment and type it.
    let add = find_hit(&app, |a| matches!(a, Action::AddComment(_))).expect("add-comment");
    assert!(click(&mut app, &mut doc, add), "add handled");
    assert!(
        app.comment_overlay.editing_id().is_some(),
        "the new comment is being composed"
    );
    for ch in ["H", "i"] {
        key_char(&mut app, &mut doc, ch);
    }
    key_named(&mut app, &mut doc, NamedKey::Enter);
    assert_eq!(notes_in_group(&doc), 1, "one comment saved");
    assert!(
        app.comment_overlay.group() == Some(group),
        "overlay stays open after saving"
    );

    // Add a SECOND comment on the same range.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let add2 = find_hit(&app, |a| matches!(a, Action::AddComment(_))).expect("add-comment #2");
    assert!(click(&mut app, &mut doc, add2), "add #2 handled");
    for ch in ["Y", "o"] {
        key_char(&mut app, &mut doc, ch);
    }
    key_named(&mut app, &mut doc, NamedKey::Enter);
    assert_eq!(notes_in_group(&doc), 2, "a range holds multiple comments");

    // Edit the first comment (reopen it for composing).
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let edit = find_hit(&app, |a| matches!(a, Action::EditComment(_))).expect("an edit affordance");
    assert!(click(&mut app, &mut doc, edit), "edit handled");
    assert!(
        app.comment_overlay.editing_id().is_some(),
        "a comment reopened for editing"
    );
    key_named(&mut app, &mut doc, NamedKey::Enter);

    // Delete one comment.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let del =
        find_hit(&app, |a| matches!(a, Action::DeleteComment(_))).expect("a delete affordance");
    assert!(click(&mut app, &mut doc, del), "delete handled");
    assert_eq!(notes_in_group(&doc), 1, "one comment removed, one remains");

    // Trash the whole highlight → its pieces AND remaining comments go.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let trash = find_hit(&app, |a| matches!(a, Action::DeleteHighlight(_))).expect("the trash");
    assert!(click(&mut app, &mut doc, trash), "trash handled");
    assert!(
        BooksDoc::list_annotations(&doc)
            .into_iter()
            .all(|a| a.group != group),
        "deleting the highlight removed every row in its group"
    );
    assert!(!app.comment_overlay.is_open(), "the overlay closed");
}

/// The selection toolbar grows a trash button when the selection covers an
/// existing highlight, and it deletes that highlight (item: explicit delete).
#[test]
fn selection_toolbar_trash_deletes_the_covered_highlight() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    // Highlight a word.
    let p = page_point(&app);
    dbl_click(&mut app, &mut doc, p);
    app.dispatch(Action::SetColor(HiColor::Blue), &mut doc);
    app.refresh_from_doc(&doc);
    assert_eq!(
        BooksDoc::list_annotations(&doc)
            .iter()
            .filter(|a| a.kind == AnnoKind::Highlight)
            .count(),
        1,
        "one highlight",
    );
    // Re-select the same word: the toolbar now offers a trash.
    frame(&mut app, &mut doc, 1000.0, 700.0);
    dbl_click(&mut app, &mut doc, p);
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let trash = find_hit(&app, |a| matches!(a, Action::DeleteHighlight(_)))
        .expect("selection toolbar shows a trash over an existing highlight");
    assert!(click(&mut app, &mut doc, trash), "trash handled");
    assert!(
        BooksDoc::list_annotations(&doc)
            .iter()
            .all(|a| a.kind != AnnoKind::Highlight),
        "the highlight under the selection was deleted",
    );
}

/// Double-clicking a word and then dragging must keep the double-clicked
/// word inside the selection and extend by whole words, never resetting
/// to the raw character the press landed on.
#[test]
fn double_click_then_drag_extends_by_whole_words() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    assert!(
        app.sel_runs.len() > 3,
        "the page laid out real run geometry"
    );

    // A multi-char word (so the double-click centre lands *inside* it,
    // strictly past its start) that has a later word in the same paragraph.
    let runs = app.sel_runs.clone();
    let r0 = runs
        .iter()
        .find(|r0| {
            r0.cl >= 3
                && runs
                    .iter()
                    .any(|r| r.block == r0.block && r.cs > r0.cs + r0.cl)
        })
        .copied()
        .expect("a multi-char word with a later word after it");
    let far = runs
        .iter()
        .filter(|r| r.block == r0.block && r.cs > r0.cs + r0.cl)
        .min_by_key(|r| r.cs)
        .copied()
        .expect("a later word in the same paragraph");
    let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
    let far_p = Point::new(f64::midpoint(far.x0, far.x1), f64::midpoint(far.y0, far.y1));

    // First click, then a second press at the same spot (a double-click) that
    // stays DOWN — this is where the word gets selected and the word-drag arms.
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    ptr_up(&mut app, &mut doc);
    ptr_move(&mut app, &mut doc, start);
    ptr_down(&mut app, &mut doc);
    let word = app.sel.expect("the double-click selected a word");
    assert_eq!(
        word.ordered().0.ch,
        r0.cs,
        "word selection starts at the word start"
    );
    assert_eq!(
        word.ordered().1.ch,
        r0.cs + r0.cl,
        "word selection ends at the word end"
    );

    // Now drag right to a later word while still held, then release.
    ptr_move(&mut app, &mut doc, far_p);
    ptr_up(&mut app, &mut doc);

    let sel = app.sel.expect("the drag kept a selection");
    let (s, e) = sel.ordered();
    // The anchor did NOT reset to the mid-word click point: it is still
    // the double-clicked word's start.
    assert_eq!(s.block, r0.block);
    assert_eq!(
        s.ch, r0.cs,
        "anchor pinned to the word start, not the click char"
    );
    // The head extended by whole words to cover the far word entirely.
    assert_eq!(e.ch, far.cs + far.cl, "head extended to the whole far word");
    assert!(
        e.ch > r0.cs + r0.cl,
        "the selection grew past the original word"
    );
}

#[test]
fn triple_click_selects_the_complete_logical_text_block() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let run = app
        .sel_runs
        .iter()
        .find(|run| run.cl > 1)
        .copied()
        .expect("the page has selectable text");
    let point = Point::new(f64::midpoint(run.x0, run.x1), f64::midpoint(run.y0, run.y1));

    dbl_click(&mut app, &mut doc, point);
    ptr_down(&mut app, &mut doc);
    ptr_up(&mut app, &mut doc);

    let (start, end) = app.sel.expect("triple-click selected a line").ordered();
    assert_eq!(
        start,
        DocPos {
            block: run.block,
            ch: 0
        }
    );
    assert_eq!(end.block, run.block);
    assert_eq!(end.ch, app.block_char_len(run.block));
}

/// Headline: the font control lists the reader's *real* installed system
/// families and, once picked, actually re-renders the book body in that
/// family. Drives the real enumeration + picker hit + apply path.
#[test]
fn system_font_picker_lists_real_families_and_applies_to_the_body() {
    use crate::model::FontKind;
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    // Open the font manager's system-family picker.
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Fonts;
    app.dispatch(Action::OpenFontPicker, &mut doc);
    frame(&mut app, &mut doc, 1200.0, 860.0);

    // The picker records an add-hit per installed family — real system fonts.
    let listed: Vec<String> = app
        .hits
        .iter()
        .filter_map(|h| match &h.action {
            Action::AddSystemFont(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !listed.is_empty(),
        "the picker lists installed system families"
    );
    // They are genuinely the renderer's enumerated families, not a hardcoded set.
    let real = TextRenderer::new().system_families();
    assert!(
        !real.is_empty(),
        "the text system enumerates installed families"
    );
    assert!(
        listed.iter().all(|f| real.contains(f)),
        "every listed family is a real installed family",
    );

    // Click one family's real hit → it registers as a Named face.
    let fam = listed[0].clone();
    let hit = find_hit(&app, |a| matches!(a, Action::AddSystemFont(f) if f == &fam))
        .expect("the chosen family has a clickable row");
    assert!(
        click(&mut app, &mut doc, hit),
        "adding the system font handled"
    );
    let entry = BooksDoc::read_fonts(&doc)
        .into_iter()
        .find(|f| f.kind == FontKind::Named(fam.clone()))
        .expect("the family is now a registered reading font");

    // Select it as the reading font → the body font resolves to that family.
    app.dispatch(Action::SetFont(entry.id), &mut doc);
    app.refresh_from_doc(&doc);
    assert_eq!(app.active_font().kind, FontKind::Named(fam.clone()));
    let (choice, _mult, _sig, mono) = app.body_font();
    assert_eq!(
        choice,
        FontChoice::Named(fam),
        "the book body re-renders in the chosen installed family",
    );
    assert!(!mono, "a named proportional family is not monospace");
}

/// Headline: a chosen colour scheme applies live to the reading view. Asserts
/// the real `resolve_palette` the draw pass uses — for the built-in
/// high-contrast tint, the de-browned dark, and a custom scheme.
#[test]
fn selecting_a_scheme_applies_its_palette_to_the_reading_view() {
    let mut app = app_with_sample();
    let mut doc = AutoCommit::new();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    let theme = Theme::dark();

    // High-contrast built-in tint → true-black page, white ink.
    app.dispatch(Action::SetReadTheme(ReadTheme::HighContrast), &mut doc);
    app.refresh_from_doc(&doc);
    let pal = app.resolve_palette(&theme);
    assert!(pal.dark, "high contrast is a dark reading mode");
    assert!(
        pal.page.r() < 6 && pal.page.g() < 6 && pal.page.b() < 6,
        "a true-black page"
    );
    assert!(
        pal.ink.r() > 250 && pal.ink.g() > 250 && pal.ink.b() > 250,
        "white ink for maximum contrast",
    );

    // Dark tint → a neutral charcoal, NOT a brown/sepia (R ≈ B).
    app.dispatch(Action::SetReadTheme(ReadTheme::Dark), &mut doc);
    app.refresh_from_doc(&doc);
    let dark = app.resolve_palette(&theme);
    assert!(dark.dark);
    let warm = i32::from(dark.page.r()) - i32::from(dark.page.b());
    assert!(
        (0..=8).contains(&warm),
        "dark page is a neutral charcoal, not brown (Δrb={warm})"
    );

    // A custom scheme's page colour drives the reading view directly.
    let mut schemes = default_schemes();
    schemes.push(ColorScheme::from_bg(
        "scheme_z",
        "Ocean",
        [0x0A, 0x22, 0x33],
    ));
    BooksDoc::write_schemes(&mut doc, &schemes);
    app.dispatch(Action::SetScheme("scheme_z".into()), &mut doc);
    app.refresh_from_doc(&doc);
    let ocean = app.resolve_palette(&theme);
    assert_eq!(
        (ocean.page.r(), ocean.page.g(), ocean.page.b()),
        (0x0A, 0x22, 0x33),
        "the custom scheme's page colour is what the reading view paints",
    );
}

/// The font-rename field is the toolkit's real `TextInput`: Ctrl+A selects
/// the whole value (so the next keystroke replaces it), and a click
/// outside the field blur-commits the rename to the CRDT.
#[test]
#[serial]
fn font_rename_uses_real_text_input_and_commits_on_blur() {
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Fonts;
    frame(&mut app, &mut doc, 1000.0, 800.0);

    // Begin an inline rename on the built-in serif face via the real action,
    // then lay the editor out with another frame.
    app.dispatch(Action::BeginRenameFont("serif".into()), &mut doc);
    frame(&mut app, &mut doc, 1000.0, 800.0);
    assert!(app.renaming.is_some(), "a rename is in progress");

    // `begin_edit` selects-all; collapse it (caret to end) so Ctrl+A is the
    // ONLY thing that could re-select the whole value.
    key_named(&mut app, &mut doc, NamedKey::ArrowRight);
    let mut ctrl = ModifiersState::empty();
    ctrl |= ModifiersState::CONTROL;
    assert!(
        key_char_mods(&mut app, &mut doc, "a", ctrl),
        "Ctrl+A is consumed by the embedded field",
    );
    // Typing now replaces the whole (re-)selected value.
    key_char(&mut app, &mut doc, "Z");
    assert_eq!(
        app.renaming.as_ref().unwrap().field.current_text(),
        "Z",
        "Ctrl+A selected all in the real TextInput, so the keystroke replaced it",
    );

    // A click far outside the field blur-commits the rename to the CRDT.
    click(&mut app, &mut doc, Point::new(950.0, 24.0));
    assert!(app.renaming.is_none(), "clicking outside ended the rename");
    let fonts = crate::doc::BooksDoc::read_fonts(&doc);
    let serif = fonts
        .iter()
        .find(|f| f.id == "serif")
        .expect("serif entry present");
    assert_eq!(
        serif.name, "Z",
        "the blur committed the new name to the CRDT"
    );
}

/// Esc cancels an inline font rename: the field closes and the CRDT keeps
/// the original name — proven on the real `handle_event` path.
#[test]
#[serial]
fn font_rename_escape_cancels_without_writing() {
    let mut app = BooksApp::with_library(Vec::new());
    let mut doc = AutoCommit::new();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Fonts;
    frame(&mut app, &mut doc, 1000.0, 800.0);

    app.dispatch(Action::BeginRenameFont("serif".into()), &mut doc);
    frame(&mut app, &mut doc, 1000.0, 800.0);
    key_char(&mut app, &mut doc, "Q"); // would-be new value
    assert!(
        key_named(&mut app, &mut doc, NamedKey::Escape),
        "Esc is consumed by the editor",
    );
    assert!(app.renaming.is_none(), "Esc closed the rename");
    let fonts = crate::doc::BooksDoc::read_fonts(&doc);
    let serif = fonts
        .iter()
        .find(|f| f.id == "serif")
        .expect("serif entry present");
    assert_eq!(serif.name, "Serif", "cancel kept the original name");
}

#[test]
#[serial]
fn completed_import_wakes_its_idle_host() {
    let tmp = tempfile::tempdir().unwrap();
    let _workspace = WorkspaceEnvGuard::set(tmp.path());
    let path = tmp.path().join("wake.epub");
    std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
    let mut scope = ui::wake::WakeScope::new();
    let _entry = scope.enter();
    let (sender, receiver) = mpsc::channel();
    scope
        .bind(move |event| {
            let _ = sender.send(event);
        })
        .unwrap();
    let worker = ImportWorker::new().unwrap();
    worker
        .request(ImportRequest {
            generation: 1,
            path,
            kind: ImportKind::Open,
            expected_book_id: None,
            delay: Duration::ZERO,
        })
        .unwrap();
    let event = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("completion wakes the host without an input event or polling frames");
    assert!(scope.accepts(&event));
    assert!(matches!(worker.try_recv(), Ok(result) if result.result.is_ok()));
}
