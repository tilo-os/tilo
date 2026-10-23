//! Drive the app through the real `handle_event` path — a headless frame
//! populates hit regions, then synthesized winit clicks route through the
//! same code the live compositor uses. Asserts observable state/CRDT change.

use super::*;
use crate::content::load_sample_library;
use crate::model::{AnnoKind, LibraryPrefs, Progress};
use serial_test::serial;
use ui::presence::PresenceCtx;
use ui::testing::Dispatch;
use ui::time::Duration;
use ui::widget::Edits;

/// Point the compatibility `TILO_WORKSPACE_DIR` override at `path` until the
/// returned scope is dropped, including during unwinding.
fn workspace_env(path: &std::path::Path) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_DIR", path);
    env
}

/// What a test reads of the library its document holds, and the edits it
/// sets the library up with.
struct TestLibrary;

impl TestLibrary {
    fn library(doc: &AutoCommit) -> crate::resource::Ebook {
        crate::resource::read_doc(doc)
    }

    fn read_settings(doc: &AutoCommit) -> Settings {
        Self::library(doc).settings
    }

    fn read_library_prefs(doc: &AutoCommit) -> LibraryPrefs {
        Self::library(doc).prefs
    }

    fn read_progress(doc: &AutoCommit, id: &str) -> Progress {
        Self::library(doc).progress_of(id)
    }

    fn read_schemes(doc: &AutoCommit) -> Vec<ColorScheme> {
        Self::library(doc).schemes.values().cloned().collect()
    }

    fn read_fonts(doc: &AutoCommit) -> Vec<FontEntry> {
        Self::library(doc).fonts.values().cloned().collect()
    }

    fn read_default_font(doc: &AutoCommit) -> String {
        Self::library(doc).default_font.0
    }

    fn read_library_entries(doc: &AutoCommit) -> Vec<crate::model::LibraryEntry> {
        Self::library(doc).entries.values().cloned().collect()
    }

    fn list_annotations(doc: &AutoCommit) -> Vec<Annotation> {
        Self::library(doc).marks()
    }

    fn annotations_for(doc: &AutoCommit, id: &str) -> Vec<Annotation> {
        Self::library(doc).annotations_for(id)
    }

    fn upsert_annotation(doc: &mut AutoCommit, annotation: &Annotation) {
        let annotations = crate::resource::Ebook::paths().annotations;
        if Self::library(doc).annotations.contains(&annotation.key()) {
            crate::resource::edit_doc(doc, &annotations.remove(&annotation.key()));
        }
        let at = Self::library(doc).annotations.len();
        crate::resource::edit_doc(
            doc,
            &annotations.insert_as(&annotation.key(), at, &crate::model::Mark::of(annotation)),
        );
    }

    fn upsert_library_entry(doc: &mut AutoCommit, entry: &crate::model::LibraryEntry) {
        let entries = crate::resource::Ebook::paths().entries;
        if Self::library(doc).entries.contains(&entry.key()) {
            crate::resource::edit_doc(doc, &entries.remove(&entry.key()));
        }
        let at = Self::library(doc).entries.len();
        crate::resource::edit_doc(doc, &entries.insert_as(&entry.key(), at, entry));
    }

    fn write_schemes(doc: &mut AutoCommit, schemes: &[ColorScheme]) {
        let paths = crate::resource::Ebook::paths().schemes;
        for scheme in Self::read_schemes(doc) {
            crate::resource::edit_doc(
                doc,
                &paths.remove(&ui::tilo_resource::Id::from_raw(scheme.id)),
            );
        }
        for (at, scheme) in schemes.iter().enumerate() {
            crate::resource::edit_doc(
                doc,
                &paths.insert_as(
                    &ui::tilo_resource::Id::from_raw(scheme.id.clone()),
                    at,
                    scheme,
                ),
            );
        }
    }

    fn write_progress(doc: &mut AutoCommit, id: &str, progress: &Progress) {
        let mut all = Self::library(doc).progress;
        all.insert(id.to_owned(), *progress);
        crate::resource::edit_doc(doc, &crate::resource::Ebook::paths().progress.set(all));
    }

    fn write_library_prefs(doc: &mut AutoCommit, prefs: LibraryPrefs) {
        let paths = crate::resource::Ebook::paths().prefs;
        crate::resource::edit_doc(doc, &paths.sort.set(prefs.sort));
        crate::resource::edit_doc(doc, &paths.hide_read.set(prefs.hide_read));
    }

    fn write_settings(doc: &mut AutoCommit, settings: &Settings) {
        let before = Self::read_settings(doc);
        for edit in crate::resource::Ebook::paths()
            .settings
            .path()
            .edits(&before, settings)
        {
            crate::resource::edit_doc(doc, &edit);
        }
    }
}

/// A new library document.
fn library_doc() -> AutoCommit {
    crate::resource::doc_with(&[], &[])
}

fn app_with_sample() -> BooksApp {
    BooksApp::with_library(
        crate::resource::library_dispatcher_for_test(),
        load_sample_library(),
    )
}

/// The reader on the sample shelf as screenshot `scenario` stages it: the
/// scenario's actions applied to the library, then its view state.
fn staged(scenario: &str) -> (BooksApp, AutoCommit) {
    let mut library = ui::tilo_resource::Replica::<crate::resource::Ebook>::new();
    for invoke in crate::scenarios::actions(Some(scenario)) {
        library.apply(invoke).unwrap();
    }
    let mut doc = library.doc().clone();
    let mut app = app_with_sample();
    app.read_record(&doc);
    app.settle(&mut Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    app.stage_scenario(scenario);
    (app, doc)
}

fn wait_for_import(app: &mut BooksApp, doc: &mut AutoCommit) {
    let deadline = tilo_test_env::hang_deadline();
    while ui::time::now() < deadline {
        app.read_record(doc);
        app.settle(&mut Edits::lend(doc), &automerge::ROOT);
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

/// Tooltips, library controls and library settings are semantic and route their real actions.
#[test]
fn library_controls_and_actions_are_discoverable_and_accessible() {
    // --- phase: hover_tooltip_reuses_the_semantic_label_of_the_topmost_control ---
    {
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

    // --- phase: accessibility_exposes_library_controls_and_routes_their_real_actions ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
            &mut Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(TestLibrary::read_library_prefs(&doc).hide_read);
    }

    // --- phase: library_settings_exposes_empty_and_discovered_folder_actions ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
}

/// Reader text, progress slider, quick settings and settings tabs are accessible and live.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn reader_controls_are_accessible_and_live() {
    // --- phase: accessibility_reader_contains_visible_text_and_actionable_chrome ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        let bookmark =
            find_semantic_target(&tree, "books.reader.bookmark").expect("bookmark toggle");
        assert_eq!(bookmark.role, Role::ToggleButton);
        assert_eq!(bookmark.state.checked, Some(false));

        assert!(Widget::perform_semantic_action(
            &mut app,
            "books.reader.bookmark",
            &A11yAction::Activate,
            &mut Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            TestLibrary::list_annotations(&doc)
                .iter()
                .any(|annotation| annotation.kind == AnnoKind::Bookmark)
        );
    }

    // --- phase: reader_progress_slider_seeks_accessibly_and_persists_continuous_position ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let chapter = app.library[0].chapters[0].clone();
        Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters = (0..4)
            .map(|index| crate::model::Chapter {
                title: format!("Part {}", index + 1),
                blocks: chapter.blocks.clone(),
            })
            .collect();
        frame(&mut app, &mut doc, 1000.0, 720.0);

        app.dispatch(
            Action::SetMode(ReadMode::Continuous),
            &mut Edits::lend(&mut doc),
        );
        frame(&mut app, &mut doc, 1000.0, 720.0);
        let id = app.book_id();
        let before = TestLibrary::read_progress(&doc, &id).fraction;
        assert!(wheel(&mut app, &mut doc, -3.0));
        assert!(TestLibrary::read_progress(&doc, &id).fraction > before);

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
            &mut Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(app.chapter, 3);
        assert!((TestLibrary::read_progress(&doc, &id).fraction - 0.75).abs() < 0.01);
    }

    // --- phase: accessibility_quick_settings_is_modal_and_numeric_controls_are_live ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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

        let before = TestLibrary::read_settings(&doc).font_scale;
        assert!(Widget::perform_semantic_action(
            &mut app,
            "books.number.text-size",
            &A11yAction::Increment,
            &mut Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(TestLibrary::read_settings(&doc).font_scale > before);
    }

    // --- phase: narrow_large_text_settings_tabs_are_complete_and_semantic ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Shortcuts;
        app.read_record(&doc);
        app.settle(&mut Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
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
}

/// Resource operations refuse unknown effects, missing chapters or annotations, and missing local capabilities without changing state.
#[test]
fn resource_operations_refuse_wrong_or_stale_targets() {
    // --- phase: resource_operations_reject_unknown_or_incomplete_effects_without_changing_state ---
    {
        let mut app = app_with_sample();
        let doc = library_doc();
        let view_before = std::mem::discriminant(&app.view);
        let settings =
            serde_json::to_vec(&serde_json::json!({"effect": "no-such", "args": {}})).unwrap();
        assert!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &settings, &doc)
                .is_err()
        );
        assert_eq!(std::mem::discriminant(&app.view), view_before);

        let open = serde_json::to_vec(&serde_json::json!({"effect": "share", "args": {}})).unwrap();
        assert!(
            app.start_effect(tilo_actions::InvocationId::new(), &open, &doc)
                .is_err()
        );
        assert!(app.resource_invocation.is_none());
    }

    // --- phase: stale_resource_reveal_refuses_missing_chapter_or_annotation_target ---
    {
        let mut app = app_with_sample();
        let doc = library_doc();
        let book_id = app.open_book().expect("sample book").id.clone();
        let before = (app.book_idx, app.chapter, app.page, app.pending_jump_block);
        for operation in [
            crate::resource::BooksEffect::Open {
                book: book_id.clone(),
                chapter: usize::MAX,
                page: 0,
                block: None,
            },
            crate::resource::BooksEffect::Open {
                book: book_id,
                chapter: 0,
                page: 0,
                block: Some(usize::MAX),
            },
        ] {
            let payload = crate::actions::effect_payload(&operation);
            assert!(
                app.start_effect(ui::tilo_actions::InvocationId::new(), &payload, &doc)
                    .is_err()
            );
            assert_eq!(
                (app.book_idx, app.chapter, app.page, app.pending_jump_block),
                before,
                "a stale target must not navigate to a neighbouring passage"
            );
        }
    }

    // --- phase: canonical_share_operation_refuses_a_missing_local_capability ---
    {
        let mut app = app_with_sample();
        let doc = library_doc();
        let id = app.library[0].id.clone();
        app.library_paths.remove(&id);
        let payload =
            crate::actions::effect_payload(&crate::resource::BooksEffect::Share { book: id });
        let error = app
            .start_effect(tilo_actions::InvocationId::new(), &payload, &doc)
            .unwrap_err();
        assert!(error.contains("unavailable on this device"));
    }
}

/// A caller with no screen reads a chapter block by block, by the indices a
/// passage names, and the book it is in; a chapter the book lacks is refused.
#[test]
fn a_chapter_reads_block_by_block() {
    let mut app = app_with_sample();
    let doc = library_doc();
    let book = app.library[0].clone();
    let payload = crate::actions::effect_payload(&crate::resource::BooksEffect::ReadChapter {
        book: book.id.clone(),
        chapter: Some(0),
    });
    let ui::framework::Effected::Done(output) = app
        .start_effect(tilo_actions::InvocationId::new(), &payload, &doc)
        .unwrap()
    else {
        panic!("a read answers at once");
    };
    let read = output.value.unwrap();
    assert_eq!(read["title"], book.title.as_str());
    assert_eq!(read["author"], book.author.as_str());
    let blocks = read["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), book.chapters[0].blocks.len());
    assert_eq!(blocks[0]["block"], 0);
    assert_eq!(blocks[0]["text"], book.chapters[0].blocks[0].plain_text());
    let missing = crate::actions::effect_payload(&crate::resource::BooksEffect::ReadChapter {
        book: book.id.clone(),
        chapter: Some(usize::MAX),
    });
    assert!(
        app.start_effect(tilo_actions::InvocationId::new(), &missing, &doc)
            .is_err()
    );
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
    app.read_record(doc);
    app.settle(&mut Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &root);
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.resolve_drawers();
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    draw_settled(app, &mut scene, &mut tr, &theme, &presence);
}

/// Draw frames until the reader shows its chapter fully laid out, as a
/// window does while the layout worker's results wake it.
fn draw_settled(
    app: &mut BooksApp,
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        scene.reset();
        app.draw(scene, Affine::IDENTITY, tr, theme, presence);
        if app.view != View::Reader || !app.layout_pending() || ui::time::now() > deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
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
    let mv = ui::synthetic_input::cursor_moved(p.x, p.y);
    let moved = Dispatch::new().doc(doc).send(app, &mv);
    let press = ui::synthetic_input::mouse_press();
    Dispatch::new()
        .doc(doc)
        .focus_target(moved.focus_target)
        .send(app, &press)
        .handled
}

/// Route a wheel event (`lines` positive == scroll up) through `handle_event`.
fn wheel(app: &mut BooksApp, doc: &mut AutoCommit, lines: f32) -> bool {
    let ev = ui::synthetic_input::mouse_wheel(0.0, lines);
    Dispatch::new().doc(doc).send(app, &ev).handled
}

/// Route a horizontal wheel/scroll through `handle_event`. Positive
/// `lines` is a physical scroll-right, matching winit's `LineDelta` x sign.
fn hwheel(app: &mut BooksApp, doc: &mut AutoCommit, lines: f32) -> bool {
    let ev = ui::synthetic_input::mouse_wheel(lines, 0.0);
    Dispatch::new().doc(doc).send(app, &ev).handled
}

/// Low-level pointer events for drag-selection tests.
fn send(app: &mut BooksApp, doc: &mut AutoCommit, ev: &WindowEvent) {
    Dispatch::new().doc(doc).send(app, ev);
}
fn ptr_move(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) {
    send(app, doc, &ui::synthetic_input::cursor_moved(p.x, p.y));
}
fn ptr_down(app: &mut BooksApp, doc: &mut AutoCommit) {
    send(app, doc, &ui::synthetic_input::mouse_press());
}
fn ptr_up(app: &mut BooksApp, doc: &mut AutoCommit) {
    send(app, doc, &ui::synthetic_input::mouse_release());
}

/// Draw one settled frame into a `Scene` (for pixel-level assertions).
fn render_scene(app: &mut BooksApp, doc: &mut AutoCommit, w: f64, h: f64) -> Scene {
    let root = automerge::ROOT;
    app.read_record(doc);
    app.settle(&mut Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &root);
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.resolve_drawers();
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    // Only the checked-in fonts: pixels drawn with whatever the host happens to
    // have installed would make every committed baseline machine-specific.
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    let mut tr = TextRenderer::with_font_catalog(&catalog).expect("checked-in font catalog");
    let mut scene = Scene::new();
    draw_settled(app, &mut scene, &mut tr, &theme, &presence);
    scene
}

// Synthesised winit key presses, routed through the real `handle_event`.
fn char_press(ch: &str) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Character(ch.into()),
        Some(ch.into()),
        ElementState::Pressed,
    ))
}
fn named_press(named: NamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(named),
        None,
        ElementState::Pressed,
    ))
}
fn feed(app: &mut BooksApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
    Dispatch::new().doc(doc).send(app, ev).handled
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
    Dispatch::new()
        .modifiers(state)
        .doc(doc)
        .send(app, &named_press(named))
        .handled
}
/// Feed a printable character with explicit modifiers (e.g. Ctrl+A).
fn key_char_mods(
    app: &mut BooksApp,
    doc: &mut AutoCommit,
    ch: &str,
    state: ModifiersState,
) -> bool {
    Dispatch::new()
        .modifiers(state)
        .doc(doc)
        .send(app, &char_press(ch))
        .handled
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

/// A click in the page's trailing margin turns forward and one in its
/// leading margin turns back, as a book's edges do, without the transient
/// bubble having to be up.
#[test]
fn a_click_in_a_page_margin_turns_the_page() {
    let mut app = app_with_sample();
    let mut doc = library_doc();
    frame(&mut app, &mut doc, 1280.0, 720.0);
    assert_eq!(app.view, View::Reader);
    let (leading, trailing) = app.turn_margins;
    let (leading, trailing) = (
        leading.expect("a framed page has a leading margin"),
        trailing.expect("and a trailing one"),
    );
    let (c0, p0) = (app.chapter, app.page);
    assert!(click(&mut app, &mut doc, trailing.center()), "handled");
    ptr_up(&mut app, &mut doc);
    frame(&mut app, &mut doc, 1280.0, 720.0);
    assert_ne!((app.chapter, app.page), (c0, p0), "turned forward");
    assert!(click(&mut app, &mut doc, leading.center()), "handled");
    ptr_up(&mut app, &mut doc);
    frame(&mut app, &mut doc, 1280.0, 720.0);
    assert_eq!((app.chapter, app.page), (c0, p0), "turned back");
}

/// The progress slider leaves room at its end for how far through the book
/// the reader is, and that label follows the turns.
#[test]
fn the_progress_slider_says_how_far_in_the_book_is() {
    let mut app = app_with_sample();
    let mut doc = library_doc();
    frame(&mut app, &mut doc, 1280.0, 720.0);
    let (_, trailing) = app.turn_margins;
    let trailing = trailing.expect("a framed page has a trailing margin");
    assert!(
        app.progress_slider_track.x1 + 20.0 < trailing.x0,
        "the track stops short of the text edge for its label: {:?} vs {:?}",
        app.progress_slider_track,
        trailing
    );
    assert_eq!(super::progress_label(0.0), "0%");
    assert_eq!(super::progress_label(0.424), "42%");
    assert_eq!(super::progress_label(1.0), "100%");
}

#[test]
fn wheel_turns_pages_and_scrolls_the_shelf() {
    // --- phase: wheel_turns_pages_in_the_reader ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.view, View::Reader);
        let start = (app.chapter, app.page);
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
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let mid = (app.chapter, app.page);
        wheel(&mut app, &mut doc, 4.0);
        assert!(
            (app.chapter, app.page) != mid,
            "turned back from the advanced spot"
        );
    }

    // --- phase: horizontal_scroll_right_turns_forward ---
    {
        // Scrolling right must advance (next page); scrolling left must go back.
        let mut app = app_with_sample();
        let mut doc = library_doc();
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

    // --- phase: wheel_scrolls_the_library_shelf ---
    {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
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
}

/// The wheel over long TOC and annotation rails scrolls the rail and reveals targets.
#[test]
fn wheel_over_rails_scrolls_the_rail_not_the_book() {
    // --- phase: wheel_over_a_long_toc_scrolls_the_rail_not_the_book_and_reveals_chapter ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let template = app.library[0].chapters[0].clone();
        Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters = (0..32)
            .map(|index| crate::model::Chapter {
                title: format!("Long chapter {}", index + 1),
                blocks: template.blocks.clone(),
            })
            .collect();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.toc_max_scroll > 0.0, "the long contents list scrolls");
        let position = (app.chapter, app.page);
        let toc = app.toc_panel_rect().expect("the contents sidebar is open");
        ptr_move(&mut app, &mut doc, Point::new(toc.center().x, 300.0));
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

    // --- phase: wheel_over_long_annotations_stays_in_the_rail_and_reveals_a_target ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let book_id = app.book_id();
        let mut target = None;
        for index in 0..28 {
            let annotation = Annotation::new(&book_id, AnnoKind::Note, 0, index)
                .with_excerpt(format!("A saved passage number {index}"));
            if index == 24 {
                target = Some(annotation.id);
            }
            TestLibrary::upsert_annotation(&mut doc, &annotation);
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
}

/// Font choices apply and persist and absent named fonts fall back to generic serif.
#[test]
fn font_choice_applies_persists_and_falls_back() {
    // --- phase: selecting_serif_font_applies_and_persists ---
    {
        use crate::model::FontKind;
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
            TestLibrary::read_settings(&doc).font_id,
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
            TestLibrary::read_settings(&doc).font_id,
            "serif",
            "serif selection persisted"
        );
    }

    // --- phase: absent_named_font_falls_back_to_generic_serif ---
    // A named face that isn't installed must degrade to generic serif, not
    // cosmic-text's default sans; an installed family is honoured verbatim.
    {
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
}

/// Disabled schemes hide from quick settings, new schemes are created and selected, and a selected scheme applies to the reading view.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn schemes_are_listed_created_and_applied() {
    // --- phase: disabling_a_scheme_hides_it_from_quick_settings ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 820.0);
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

    // --- phase: scheme_manager_new_via_picker_creates_selects_and_edits ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 860.0);
        let before = TestLibrary::read_schemes(&doc).len();
        // The single "New scheme" button opens the colour picker (no scheme yet).
        let newbtn = find_hit(&app, |a| matches!(a, Action::NewSchemeViaPicker))
            .expect("a New scheme button");
        assert!(click(&mut app, &mut doc, newbtn), "new-via-picker handled");
        assert!(app.color_picker.is_open(), "the colour picker opened");
        assert_eq!(
            TestLibrary::read_schemes(&doc).len(),
            before,
            "no scheme until the picker commits"
        );
        // Commit the picker -> a scheme is derived from the chosen background,
        // selected, and opened in the editor.
        frame(&mut app, &mut doc, 1200.0, 860.0);
        let done = picker_rect(&app, "color_picker.done").center();
        assert!(click(&mut app, &mut doc, done), "picker done handled");
        assert!(!app.color_picker.is_open(), "Done closes the picker");
        let schemes = TestLibrary::read_schemes(&doc);
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
            TestLibrary::read_settings(&doc).scheme_id,
            custom.id,
            "new scheme is selected"
        );
        assert_eq!(
            app.editing_scheme.as_deref(),
            Some(custom.id.as_str()),
            "editor opened on it"
        );
    }

    // --- phase: selecting_a_scheme_applies_its_palette_to_the_reading_view ---
    // Headline: a chosen colour scheme applies live to the reading view. Asserts
    // the real `resolve_palette` the draw pass uses — for the built-in
    // high-contrast tint, the de-browned dark, and a custom scheme.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let theme = Theme::dark();

        // High-contrast built-in tint → true-black page, white ink.
        app.dispatch(
            Action::SetReadTheme(ReadTheme::HighContrast),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);
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

        // Sepia on an OS high-contrast theme must not keep warm paper.
        app.dispatch(
            Action::SetReadTheme(ReadTheme::Sepia),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);
        let hc = Theme::high_contrast();
        let forced = app.resolve_palette(&hc);
        assert_eq!(
            (forced.page.r(), forced.page.g(), forced.page.b()),
            (
                hc.tokens.colors.surfaces.raised.r(),
                hc.tokens.colors.surfaces.raised.g(),
                hc.tokens.colors.surfaces.raised.b()
            ),
            "high contrast reaches the page even when the reader tint is sepia"
        );

        // Dark tint → the OS theme's raised surface, not a hardcoded charcoal.
        app.dispatch(
            Action::SetReadTheme(ReadTheme::Dark),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);
        let dark = app.resolve_palette(&theme);
        assert!(dark.dark);
        assert_eq!(
            (dark.page.r(), dark.page.g(), dark.page.b()),
            (
                theme.tokens.colors.surfaces.raised.r(),
                theme.tokens.colors.surfaces.raised.g(),
                theme.tokens.colors.surfaces.raised.b()
            ),
            "dark page tracks the OS raised surface"
        );

        let mut schemes = default_schemes();
        schemes.push(ColorScheme::from_bg(
            "scheme_z",
            "Ocean",
            [0x0A, 0x22, 0x33],
        ));
        TestLibrary::write_schemes(&mut doc, &schemes);
        app.dispatch(
            Action::SetScheme("scheme_z".into()),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);
        let ocean = app.resolve_palette(&theme);
        assert_eq!(
            (ocean.page.r(), ocean.page.g(), ocean.page.b()),
            (0x0A, 0x22, 0x33),
            "the custom scheme's page colour is what the reading view paints",
        );
    }
}

/// The colour picker applies a colour, leaves the scheme untouched on cancel and works through semantics.
#[test]
fn colour_picker_edits_or_leaves_a_scheme() {
    // --- phase: color_picker_returns_a_colour_applied_to_a_scheme ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let mut schemes = default_schemes();
        schemes.push(ColorScheme::from_bg("scheme_x", "Test", [0x30, 0x30, 0x30]));
        TestLibrary::write_schemes(&mut doc, &schemes);
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 900.0);
        let before = TestLibrary::read_schemes(&doc)
            .into_iter()
            .find(|s| s.id == "scheme_x")
            .unwrap()
            .page;
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
        // Drag on the SV square: a press + move within it picks a new colour,
        // which stays pending until Done.
        frame(&mut app, &mut doc, 1200.0, 900.0);
        let sv = picker_rect(&app, "color_picker.sv");
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
        assert_eq!(
            page_of(&doc, "scheme_x"),
            before,
            "nothing is written while the pick is pending"
        );
        frame(&mut app, &mut doc, 1200.0, 900.0);
        let done = picker_rect(&app, "color_picker.done").center();
        assert!(click(&mut app, &mut doc, done), "picker done handled");
        let after = TestLibrary::read_schemes(&doc)
            .into_iter()
            .find(|s| s.id == "scheme_x")
            .unwrap()
            .page;
        assert_ne!(
            after, before,
            "the picker applied a new page colour to the scheme"
        );
    }

    // --- phase: cancelling_the_picker_leaves_the_scheme_untouched ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let before = open_page_picker(&mut app, &mut doc);
        let swatch = picker_rect(&app, "color_picker.swatch[0]").center();
        assert!(click(&mut app, &mut doc, swatch), "swatch handled");
        let cancel = picker_rect(&app, "color_picker.cancel").center();
        assert!(click(&mut app, &mut doc, cancel), "cancel handled");
        assert!(!app.color_picker.is_open(), "Cancel closes the picker");
        assert_eq!(page_of(&doc, "scheme_x"), before, "Cancel writes nothing");
    }

    // --- phase: a_swatch_action_and_done_apply_through_semantics ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let before = open_page_picker(&mut app, &mut doc);
        let nodes = app.color_picker.accessibility_nodes();
        let dialog = nodes.first().expect("the sheet is a dialog");
        assert!(dialog.state.modal);
        let group = dialog.children.first().expect("the picker's semantics");
        let red = group
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Red"))
            .and_then(|node| node.action_target.clone())
            .expect("each swatch carries its own action");
        assert!(app.perform_semantic_action(
            &red,
            &A11yAction::Activate,
            &mut Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(
            page_of(&doc, "scheme_x"),
            before,
            "a swatch pick is pending"
        );
        assert!(app.perform_semantic_action(
            "color_picker.done",
            &A11yAction::Activate,
            &mut Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(page_of(&doc, "scheme_x"), [0xEF, 0x44, 0x44]);
    }
}

/// A named part of the open colour picker, in its test-target names.
fn picker_rect(app: &BooksApp, part: &str) -> Rect {
    app.color_picker
        .picker()
        .expect("the colour picker is open")
        .test_rects()
        .into_iter()
        .find(|(name, _)| name == part)
        .map_or_else(|| panic!("the picker lays out {part}"), |(_, rect)| rect)
}

fn page_of(doc: &AutoCommit, id: &str) -> [u8; 3] {
    TestLibrary::read_schemes(doc)
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
        .page
}

/// Open the picker on a custom scheme's page slot; returns the page colour
/// before any pick.
fn open_page_picker(app: &mut BooksApp, doc: &mut AutoCommit) -> [u8; 3] {
    let mut schemes = default_schemes();
    schemes.push(ColorScheme::from_bg("scheme_x", "Test", [0x30, 0x30, 0x30]));
    TestLibrary::write_schemes(doc, &schemes);
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Colours;
    frame(app, doc, 1200.0, 900.0);
    let swatch = find_hit(
        app,
        |a| matches!(a, Action::PickSchemeColor(id, slot) if id == "scheme_x" && *slot == 0),
    )
    .expect("the page swatch opens the picker");
    assert!(click(app, doc, swatch), "picker opened on the page slot");
    frame(app, doc, 1200.0, 900.0);
    page_of(doc, "scheme_x")
}

/// The font manager toggles shown and default fonts and the system picker applies real families.
#[test]
fn font_manager_and_system_picker_set_fonts() {
    // --- phase: font_manager_toggles_shown_and_sets_default ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        assert!(
            app.perform_semantic_action(
                "books.settings.default-font",
                &A11yAction::SetValue("Mono".into()),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "the default-font dropdown takes the choice"
        );
        assert_eq!(
            TestLibrary::read_default_font(&doc),
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

    // --- phase: system_font_picker_lists_real_families_and_applies_to_the_body ---
    // Headline: the font control lists the reader's *real* installed system
    // families and, once picked, actually re-renders the book body in that
    // family. Drives the real enumeration + picker hit + apply path.
    {
        use crate::model::FontKind;
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        app.dispatch(Action::OpenFontPicker, &mut Edits::lend(&mut doc));
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
        let entry = TestLibrary::read_fonts(&doc)
            .into_iter()
            .find(|f| f.kind == FontKind::Named(fam.clone()))
            .expect("the family is now a registered reading font");

        // Select it as the reading font → the body font resolves to that family.
        app.dispatch(Action::SetFont(entry.id), &mut Edits::lend(&mut doc));
        app.read_record(&doc);
        assert_eq!(app.active_font().kind, FontKind::Named(fam.clone()));
        let (choice, _mult, _sig, mono) = app.body_font();
        assert_eq!(
            choice,
            FontChoice::Named(fam),
            "the book body re-renders in the chosen installed family",
        );
        assert!(!mono, "a named proportional family is not monospace");
    }
}

/// Drag, word and block selection span paragraphs and store partial ranges.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn pointer_selection_spans_words_blocks_and_paragraphs() {
    // --- phase: drag_selection_spans_paragraphs_and_highlights_the_range ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        let hls: Vec<_> = TestLibrary::list_annotations(&doc)
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

    // --- phase: drag_across_paragraph_stores_partial_char_range_not_the_whole_block ---
    // Drive a real drag that starts *inside* a paragraph and ends in a later
    // one, then apply a colour, and assert the stored annotation carries a
    // character range (partial, not the whole block) and spans the boundary as
    // one grouped highlight — proving the mark is text-range, not paragraph.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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

        app.dispatch(
            Action::SetColor(HiColor::Yellow),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);

        let hls: Vec<_> = TestLibrary::list_annotations(&doc)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Highlight)
            .collect();
        assert!(hls.len() >= 2, "one ranged highlight per spanned paragraph");
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

    // --- phase: double_click_then_drag_extends_by_whole_words ---
    // Double-clicking a word and then dragging must keep the double-clicked
    // word inside the selection and extend by whole words, never resetting
    // to the raw character the press landed on.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        assert_eq!(e.ch, far.cs + far.cl, "head extended to the whole far word");
        assert!(
            e.ch > r0.cs + r0.cl,
            "the selection grew past the original word"
        );
    }

    // --- phase: triple_click_selects_the_complete_logical_text_block ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
}

/// A note composer opens over a selection, types, backspaces and persists, and a finished note reopens for editing.
#[test]
fn notes_are_composed_reopened_and_persisted() {
    // --- phase: note_action_opens_an_on_page_composer_over_a_selection ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
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
        let a = TestLibrary::list_annotations(&doc)
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

    // --- phase: composing_a_note_types_backspaces_and_persists ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1280.0, 800.0);
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

        let note = TestLibrary::list_annotations(&doc)
            .into_iter()
            .find(|a| a.id == nid)
            .expect("the composed note is in the document");
        assert_eq!(note.kind, AnnoKind::Note);
        assert_eq!(
            note.note, "Hi ther",
            "typed text with one backspace persisted"
        );
    }

    // --- phase: clicking_a_finished_note_reopens_it_for_editing ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
}

/// The Aa popover opens without dimming the page, persists the tint and stays clear of the annotations rail.
#[test]
fn type_popover_opens_undimmed_and_clear_of_the_rail() {
    // --- phase: settings_popover_does_not_dim_the_page ---
    {
        use ui::offscreen::OffscreenRenderer;
        let (w, h) = (1000u32, 700u32);
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let closed = render_scene(&mut app, &mut doc, f64::from(w), f64::from(h));
        app.reading_settings.open();
        let open = render_scene(&mut app, &mut doc, f64::from(w), f64::from(h));

        let mut ro = OffscreenRenderer::new(w, h);
        let c_px = ro.render_rgba(&closed);
        let o_px = ro.render_rgba(&open);
        // A point on the page below the popover, which hangs from its button
        // in the header — if a scrim dimmed the page it would darken here.
        let (x, y) = (400u32, 660u32);
        let a0 = OffscreenRenderer::sample(&c_px, w, x, y);
        let a1 = OffscreenRenderer::sample(&o_px, w, x, y);
        let d = |u: u8, v: u8| (i32::from(u) - i32::from(v)).abs();
        assert!(
            d(a0.0, a1.0) <= 3 && d(a0.1, a1.1) <= 3 && d(a0.2, a1.2) <= 3,
            "the open settings popover must not dim the page: {a0:?} vs {a1:?}",
        );
    }

    // --- phase: aa_button_opens_popover_and_page_tint_persists ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(!app.reading_settings.is_open());
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        assert!(click(&mut app, &mut doc, aa));
        assert!(app.reading_settings.is_open(), "popover opened");
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let dark = find_reading_hit(&app, |action| {
            matches!(action, ReadingSettingsAction::SetReadTheme(ReadTheme::Dark))
        })
        .expect("dark tint pill");
        assert!(click(&mut app, &mut doc, dark));
        assert_eq!(TestLibrary::read_settings(&doc).theme, ReadTheme::Dark);
    }

    // --- phase: type_popover_stays_clear_of_the_docked_annotations_rail ---
    {
        // On a wide window the annotations rail is docked-open; the reading
        // settings popover must anchor inside the reading area, never crossing it.
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Reader;
        frame(&mut app, &mut doc, 1280.0, 800.0);
        assert!(app.notes_drawer.is_docked(), "wide window docks the rail");
        assert!(app.notes_open(), "the annotations rail is open");

        let (px0, px1) = ReadingSettings::panel_x_bounds(
            app.rect,
            app.type_button,
            f64::from(Theme::dark().tokens.metrics.panel_width_utility),
            f64::from(Theme::dark().tokens.spacing.s3),
        );
        let rail_left = app.notes_panel_rect().expect("annotations panel").x0;
        assert!(
            px1 <= rail_left,
            "popover right edge {px1} must stay left of the docked annotations rail at {rail_left}"
        );
        assert!(px0 >= app.rect.x0, "popover stays on-screen");
        assert!(px1 - px0 >= 200.0, "popover keeps a usable width");
    }
}

/// Composition, stepper and click-to-type values in the type popover change and persist.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn type_popover_values_edit_and_persist() {
    // --- phase: composition_setting_relayouts_and_persists ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.layout.is_some(), "reader starts laid out");

        app.reading_settings.open();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let justified = find_reading_hit(&app, |action| {
            matches!(
                action,
                ReadingSettingsAction::SetComposition(CompositionStyle::Book)
            )
        })
        .expect("settings popover exposes the Justified composition choice");
        assert!(click(&mut app, &mut doc, justified), "choice is clickable");

        assert_eq!(app.settings.composition, CompositionStyle::Book);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            app.layout
                .as_ref()
                .is_some_and(|layout| layout.key.composition),
            "composition changes lay the chapter out justified"
        );
        assert_eq!(
            TestLibrary::read_settings(&doc).composition,
            CompositionStyle::Book,
            "reader preference persists"
        );
    }

    // --- phase: type_popover_steppers_change_and_persist_settings ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        click(&mut app, &mut doc, aa);

        frame(&mut app, &mut doc, 1000.0, 700.0);
        let base = TestLibrary::read_settings(&doc).font_scale;
        let font = app
            .reading_settings
            .number_rect(&NumberField::FontScale)
            .expect("font + stepper");
        let inc = Point::new(
            font.x1 - font.height() / 2.0,
            f64::midpoint(font.y0, font.y1),
        );
        click(&mut app, &mut doc, inc);
        let grown = TestLibrary::read_settings(&doc).font_scale;
        assert!(
            grown > base,
            "font scale grew and persisted ({base} -> {grown})"
        );

        frame(&mut app, &mut doc, 1000.0, 700.0);
        let sbase = TestLibrary::read_settings(&doc).line_spacing;
        let spacing = app
            .reading_settings
            .number_rect(&NumberField::LineSpacing)
            .expect("spacing - stepper");
        let dec = Point::new(
            spacing.x0 + spacing.height() / 2.0,
            f64::midpoint(spacing.y0, spacing.y1),
        );
        click(&mut app, &mut doc, dec);
        let shrunk = TestLibrary::read_settings(&doc).line_spacing;
        assert!(
            shrunk < sbase,
            "line spacing shrank and persisted ({sbase} -> {shrunk})"
        );
    }

    // --- phase: type_popover_value_is_click_to_type ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        click(&mut app, &mut doc, aa);
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
        let fs = TestLibrary::read_settings(&doc).font_scale;
        assert!(
            (fs - 1.5).abs() < 0.001,
            "typed 150% -> font_scale 1.5, got {fs}"
        );

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
        let ls = TestLibrary::read_settings(&doc).line_spacing;
        assert!(
            (ls - 1.4).abs() < 0.001,
            "typed 1.4 -> line_spacing 1.4, got {ls}"
        );
    }
}

/// Edge coverage, in premultiplied 8-bit channel steps, that one GPU
/// rasterizer may compute differently from another for the same geometry.
const FOREIGN_RASTERIZER_EDGE_STEPS: u16 = 16;

/// Pixels of `actual` that differ from a reader reference.
///
/// The Linux references are Mesa llvmpipe renders, the adapter Linux CI pins
/// (`WGPU_ADAPTER_NAME=llvmpipe`), and the Windows ones renders of that
/// platform's adapter; there the comparison keeps the ±1 antialiasing
/// tolerance. Another rasterizer (Metal on a Mac, a desktop Vulkan driver)
/// evaluates the same coverage with its own float arithmetic: glyph edges
/// land up to about 15 channel steps away, at thousands of edge pixels,
/// while every stroke, line break and position matches. There the comparison
/// is of premultiplied colour within those edge steps, so near-transparent
/// chrome pixels cannot magnify a coverage difference through their alpha,
/// yet a changed glyph, wrap or offset still moves whole strokes and fails.
fn pixels_differing_from_reader_reference(
    renderer: &ui::offscreen::OffscreenRenderer,
    actual: &[u8],
    expected: &[u8],
) -> usize {
    let adapter = renderer.adapter_info();
    if cfg!(windows)
        || adapter
            .as_ref()
            .is_some_and(|info| info.name.contains("llvmpipe"))
    {
        return ui::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(actual, expected);
    }
    eprintln!(
        "reader reference is an llvmpipe render; comparing a {:?} render within \
         {FOREIGN_RASTERIZER_EDGE_STEPS} premultiplied edge steps",
        adapter.map(|info| info.name)
    );
    let premultiplied = |pixel: &[u8]| {
        let alpha = u16::from(pixel[3]);
        [
            u16::from(pixel[0]).saturating_mul(alpha) / 255,
            u16::from(pixel[1]).saturating_mul(alpha) / 255,
            u16::from(pixel[2]).saturating_mul(alpha) / 255,
            alpha,
        ]
    };
    let beyond_edge_steps = |a: &[u8], b: &[u8]| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(a, b)| {
                premultiplied(a)
                    .iter()
                    .zip(premultiplied(b))
                    .any(|(x, y)| x.abs_diff(y) > FOREIGN_RASTERIZER_EDGE_STEPS)
            })
            .count()
    };
    // The tolerance must still see text move: the reference against itself
    // one pixel to the right differs along every glyph.
    let shifted = beyond_edge_steps(
        &expected[4..],
        &expected[..expected.len().saturating_sub(4)],
    );
    assert!(
        shifted > 1000,
        "a one-pixel shift of the reference must exceed the edge tolerance, got {shifted} pixels"
    );
    beyond_edge_steps(actual, expected)
        .saturating_add(actual.len().abs_diff(expected.len()).div_ceil(4))
}

fn reader_typography_baseline(filename: &str) -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let result = std::process::Command::new("python3")
        .arg(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/test-assets.py"),
        )
        .arg(root.join("tests/fixtures/typography/artifacts.json"))
        .args(["--file", filename])
        .output()
        .expect("run pinned reader typography asset resolver");
    assert!(
        result.status.success(),
        "resolve immutable reader baseline {filename}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    std::path::PathBuf::from(
        String::from_utf8(result.stdout)
            .expect("UTF-8 reader baseline path")
            .trim(),
    )
}

#[test]
fn serif_reader_matches_typography_baseline() {
    use std::path::PathBuf;

    use ui::offscreen::OffscreenRenderer;

    let (width, height) = (1280u32, 800u32);
    let mut app = app_with_sample();
    let mut doc = library_doc();
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
    let baseline = reader_typography_baseline(baseline.file_name().unwrap().to_str().unwrap());
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
    let differing = pixels_differing_from_reader_reference(&renderer, &actual, expected.as_raw());
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
    let mut doc = library_doc();
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
    let baseline = reader_typography_baseline(baseline.file_name().unwrap().to_str().unwrap());
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
    let differing = pixels_differing_from_reader_reference(&renderer, &actual, expected.as_raw());
    if differing != 0 {
        let output = std::env::temp_dir().join(baseline.file_name().unwrap());
        image::save_buffer(output, &actual, width, height, image::ColorType::Rgba8).unwrap();
    }
    assert_eq!(
        differing, 0,
        "mathematical reader differs from baseline in {differing} pixels"
    );
}

/// Opening a book enters the reader; pages advance with a bubble, and blocks and TOC rows jump to their page.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn pages_advance_and_jump_to_blocks_and_chapters() {
    // --- phase: opening_a_book_from_the_shelf_enters_the_reader ---
    {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1280.0, 720.0);
        let target = find_hit(&app, |a| matches!(a, Action::OpenBook(_))).expect("a book card hit");
        assert!(click(&mut app, &mut doc, target), "book click handled");
        assert_eq!(app.view, View::Reader);
    }

    // --- phase: next_page_advances_position ---
    {
        let mut app = app_with_sample();
        // The page-nav stepper now lives on the floating page bubble, which is
        // transient — arm it open so its next chevron registers a NextPage hit,
        // then click that chevron on the real hit-testing path.
        app.arm_reader_bubble_for_shots();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let (c0, p0) = (app.chapter, app.page);
        let next = find_hit(&app, |a| matches!(a, Action::NextPage))
            .expect("next chevron on the page bubble");
        assert!(click(&mut app, &mut doc, next), "next handled");
        assert_eq!(
            (app.chapter, app.page),
            (c0, p0 + 1),
            "the visible chevron turns exactly one page, never seeks underneath"
        );
        assert!(
            !app.progress_dragging,
            "a page turn does not start a seek drag"
        );
    }

    // --- phase: page_bubble_surfaces_on_a_page_change ---
    // The floating page bubble is hidden on a settled reader and surfaced by
    // a page change without leaving an invisible navigation hit behind.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        // A page turn pokes the bubble; the next frame draws it (its chevrons are
        // hit-testable) and it keeps requesting frames so it can fade on its own.
        app.dispatch(Action::NextPage, &mut Edits::lend(&mut doc));
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            find_hit(&app, |a| matches!(a, Action::NextPage)).is_some(),
            "a page change surfaces the bubble's next chevron"
        );
        assert!(
            app.reader_bubble.needs_frame(ui::time::now()),
            "the visible bubble schedules its own fade-out frames"
        );
        // The bubble shares the foot's band with the progress slider:
        // reaching for the slider gives the band back to it at once.
        let track = app.progress_slider_track;
        app.cursor = Point::new(track.x0 + 2.0, track.center().y);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            find_hit(&app, |a| matches!(a, Action::NextPage)).is_none(),
            "the pointer on the slider hides the page bubble"
        );
    }

    // --- phase: page_for_block_maps_a_block_to_its_page ---
    {
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

    // --- phase: toc_row_click_navigates_to_that_chapter ---
    {
        let mut app = app_with_sample();
        app.book_idx = 1; // Field Notes has three chapters.
        let mut doc = library_doc();
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
}

/// Continuous and two-up modes scroll and turn correctly and page turns animate or settle under reduced motion.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn page_modes_and_turn_animation() {
    // --- phase: continuous_mode_scrolls_the_flow_and_persists ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        app.dispatch(
            Action::SetMode(ReadMode::Continuous),
            &mut Edits::lend(&mut doc),
        );
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.settings.mode, ReadMode::Continuous, "mode switched");
        assert_eq!(
            TestLibrary::read_settings(&doc).mode,
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
        let id = app.book_id();
        assert!(
            TestLibrary::read_progress(&doc, &id).fraction >= 0.0,
            "progress recorded in continuous mode"
        );
    }

    // --- phase: two_up_shows_two_columns_and_turns_by_two ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1280.0, 820.0);
        app.dispatch(Action::SetMode(ReadMode::TwoUp), &mut Edits::lend(&mut doc));
        frame(&mut app, &mut doc, 1280.0, 820.0);
        assert_eq!(app.settings.mode, ReadMode::TwoUp);
        // A spread aligns to an even left page.
        assert_eq!(app.page % 2, 0, "the spread's left page is even");
        if app.cur_page_count >= 2 {
            let cols: std::collections::HashSet<u8> = app.sel_runs.iter().map(|r| r.col).collect();
            assert!(
                cols.contains(&0) && cols.contains(&1),
                "the spread paints two facing columns"
            );
        }
        // A page turn advances by a whole spread (two pages), or rolls chapter.
        let (c0, p0) = (app.chapter, app.page);
        app.dispatch(Action::NextPage, &mut Edits::lend(&mut doc));
        frame(&mut app, &mut doc, 1280.0, 820.0);
        if app.chapter == c0 {
            assert_eq!(app.page, p0 + 2, "a spread turns two pages at once");
        } else {
            assert_eq!(app.chapter, c0 + 1, "or rolls to the next chapter");
        }
    }

    // --- phase: turning_a_page_animates_an_eased_slide ---
    // A paged turn eases the incoming page in (a slide + fade) instead of
    // snapping: the turn schedules animation frames and the incoming page starts
    // offset off its resting x, decaying toward zero. Continuous scroll never
    // animates a page turn — it just scrolls.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        app.dispatch(Action::NextPage, &mut Edits::lend(&mut doc));
        assert_ne!((app.chapter, app.page), (c0, p0), "the page advanced");
        assert!(
            app.needs_animation_frame(),
            "the turn schedules animation frames"
        );
        let pt = app.page_turn.expect("a page-turn slide is in flight");
        let (dx, alpha) = pt.visual(ui::time::now(), 500.0);
        assert!(
            dx > 0.0,
            "a forward turn slides the incoming page in from the right: {dx}"
        );
        assert!(
            alpha < 1.0,
            "the incoming page fades up as it arrives: {alpha}"
        );

        // Continuous scroll scrolls instead of turning, so no slide is scheduled.
        app.dispatch(
            Action::SetMode(ReadMode::Continuous),
            &mut Edits::lend(&mut doc),
        );
        frame(&mut app, &mut doc, 1000.0, 700.0); // settles + clears the prior turn
        assert!(
            app.page_turn.is_none(),
            "the paged turn cleared after settling"
        );
        app.dispatch(Action::NextPage, &mut Edits::lend(&mut doc));
        assert!(
            app.page_turn.is_none(),
            "continuous mode never starts a page-turn slide"
        );
    }

    // --- phase: reduced_motion_settles_an_in_flight_page_turn_immediately ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        app.dispatch(Action::NextPage, &mut Edits::lend(&mut doc));
        assert!(app.page_turn.is_some());

        Widget::set_motion_reduced(&mut app, true);

        assert!(app.page_turn.is_none());
        assert!(!app.needs_animation_frame());
    }
}

/// Dragging a highlight marker reanchors it keeping comments; tapping opens its comments.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn highlight_markers_reanchor_and_open_comments() {
    // --- phase: dragging_a_highlight_marker_reanchors_it_and_keeps_its_comments ---
    // A highlight's margin marker is a draggable handle: grabbing it and dropping
    // it on another paragraph re-anchors the highlight there through the
    // annotation action model (the highlight and its comments move together). A
    // plain tap on the same marker opens the highlight's comments.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        app.dispatch(Action::SetColor(HiColor::Green), &mut Edits::lend(&mut doc));
        app.read_record(&doc);
        let hl = TestLibrary::list_annotations(&doc)
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
            blocks: Vec::new(),
        };
        TestLibrary::upsert_annotation(&mut doc, &comment);
        app.read_record(&doc);

        frame(&mut app, &mut doc, 1000.0, 700.0);
        let marker_rect = app
            .hi_markers
            .iter()
            .find(|(_, g)| *g == group)
            .map(|(r, _)| *r)
            .expect("the highlight has a draggable margin marker");

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

        ptr_move(&mut app, &mut doc, marker_rect.center());
        ptr_down(&mut app, &mut doc);
        assert!(
            app.marker_drag.is_some(),
            "pressing the marker began a re-anchor drag"
        );
        ptr_move(&mut app, &mut doc, target_pt);
        ptr_up(&mut app, &mut doc);
        app.read_record(&doc);

        // The highlight re-anchored to the dropped paragraph — the SAME row moved
        // (its id is preserved, proving it went through `MoveAnnotation`, not a
        // delete + re-create).
        let moved = TestLibrary::list_annotations(&doc)
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
        // The comment survives with its identity and text, but follows the
        // passage so clicking its drawer/search result opens the new anchor.
        let moved_comment = TestLibrary::list_annotations(&doc)
            .into_iter()
            .find(|a| a.id == comment.id)
            .expect("the highlight's comment survived the re-anchor");
        assert_eq!(moved_comment.note, "keep me");
        assert_eq!(moved_comment.group, group);
        assert_eq!(moved_comment.kind, AnnoKind::Note);
        assert_eq!(moved_comment.chapter, moved.chapter);
        assert_eq!(moved_comment.block, target_block);
        assert_eq!(moved_comment.start, moved.start);
        assert_eq!(moved_comment.end, moved.end);
        assert_eq!(moved_comment.excerpt, moved.excerpt);
        app.dispatch(Action::JumpTo(comment.id), &mut Edits::lend(&mut doc));
        assert_eq!(app.pending_jump_block, Some(target_block));
    }

    // --- phase: tapping_a_highlight_marker_opens_its_comments ---
    // A plain tap on a highlight's margin marker (no drag) opens its comments,
    // so the marker doubles as the comment affordance.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        app.dispatch(Action::SetColor(HiColor::Green), &mut Edits::lend(&mut doc));
        app.read_record(&doc);
        let group = TestLibrary::list_annotations(&doc)
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
        ptr_move(&mut app, &mut doc, marker);
        ptr_down(&mut app, &mut doc);
        ptr_up(&mut app, &mut doc);
        assert_eq!(
            app.comment_overlay.group(),
            Some(group),
            "a tap on the marker opened the highlight's comments"
        );
    }
}

/// Swatches write ranged highlights from selections, bands fill word gaps, and colours toggle off or replace.
#[test]
fn highlights_are_written_and_recoloured_from_selections() {
    // --- phase: double_click_selects_a_word_then_a_swatch_writes_a_ranged_highlight ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let before = TestLibrary::list_annotations(&doc).len();
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
        let after = TestLibrary::list_annotations(&doc);
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

    // --- phase: continuous_highlight_band_fills_the_gap_between_words ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        TestLibrary::upsert_annotation(&mut doc, &hl);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // The gap midpoint between the two words must be inside a drawn band —
        // proving the band is continuous, not per-glyph.
        let gap_mid = Point::new(f64::midpoint(a.x1, b.x0), f64::midpoint(a.y0, a.y1));
        assert!(
            app.hi_bands.iter().any(|band| band.contains(gap_mid)),
            "a continuous highlight band covers the inter-word gap"
        );
    }

    // --- phase: highlight_colour_toggles_off_and_replaces ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let count = |d: &AutoCommit| {
            TestLibrary::list_annotations(d)
                .into_iter()
                .filter(|a| a.kind == AnnoKind::Highlight)
                .count()
        };
        let color_of = |d: &AutoCommit| {
            TestLibrary::list_annotations(d)
                .into_iter()
                .find(|a| a.kind == AnnoKind::Highlight)
                .map(|a| a.color)
        };
        let p = page_point(&app);
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(
            Action::SetColor(HiColor::Yellow),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);
        assert_eq!(count(&doc), 1);
        assert_eq!(color_of(&doc), Some(HiColor::Yellow));

        // Re-select the same word; picking a DIFFERENT colour replaces it.
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Green), &mut Edits::lend(&mut doc));
        app.read_record(&doc);
        assert_eq!(count(&doc), 1, "different colour replaces (never layers)");
        assert_eq!(color_of(&doc), Some(HiColor::Green));

        // Re-select the same word; picking the SAME colour toggles it off.
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Green), &mut Edits::lend(&mut doc));
        app.read_record(&doc);
        assert_eq!(count(&doc), 0, "same colour again removes the highlight");
    }
}

/// Standard Ebooks' Odyssey, Book I, set by the reader at phone widths: no
/// justified line holds a single word or gapes open next to a tight one.
#[test]
fn em_dash_compounds_set_evenly_in_a_phone_reader() {
    let summary = "A council of the gods\u{feff}—Deliberations concerning Ulysses\u{feff}—Mercury despatched to Calypso, to bid her send Ulysses to Ithaca\u{feff}—Visit of Pallas, in the shape of Mentor, to Telemachus, advising him to repair to Pylos and Sparta in quest of his father, Ulysses\u{feff}—Revels of the suitors of Penelope\u{feff}—Phemius, the minstrel, and his song of the return of the Grecians\u{feff}—The suitors rebuked by Telemachus.";
    let word_joined = summary.replace("\u{feff}—", "\u{2060}—\u{2060}");
    let mut failures = Vec::new();
    for text in [summary, word_joined.as_str()] {
        let mut app = app_with_sample();
        let chapter = format!(
            "<body xmlns=\"http://www.w3.org/1999/xhtml\"><h2>Book I</h2><p>{text}</p></body>"
        );
        Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters[0].blocks =
            crate::xhtml::parse_xhtml(&chapter);
        let mut doc = library_doc();
        for width in (360..=430).step_by(2) {
            app.layout = None;
            frame(&mut app, &mut doc, f64::from(width), 900.0);
            let block = &app.layout.as_ref().expect("laid out").blocks[1];
            let lines = &block.lines;
            for line in &lines[..lines.len() - 1] {
                if line.runs.len() < 2 || line.adjustment_ratio > 7.0 {
                    let words: Vec<_> = line.runs.iter().map(|run| &run.display_text).collect();
                    failures.push(format!(
                        "{width}px r={:.1} {words:?}",
                        line.adjustment_ratio
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Copy keeps verse lines on their own lines and blocks apart, while the
/// annotation excerpt and every char offset keep the single-space text; the
/// verse paragraph's semantic node covers the lines it paints.
#[test]
fn verse_copies_its_line_breaks_and_reports_its_geometry() {
    let mut app = app_with_sample();
    let chapter = r#"<body xmlns="http://www.w3.org/1999/xhtml">
          <div class="verse"><p><span>Tell me, O Muse, of that sagacious man</span><br/>
          <span class="i1">Who, having overthrown the city-towers</span><br/>
          <span>Of Ilium, wandered far</span></p></div>
          <p>And prose follows.</p>
        </body>"#;
    let blocks = crate::xhtml::parse_xhtml(chapter);
    assert!(
        matches!(blocks[0], crate::model::Block::Verse { .. }),
        "{blocks:?}"
    );
    Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0]).chapters[0].blocks = blocks;
    app.layout = None;
    let mut doc = library_doc();
    frame(&mut app, &mut doc, 1000.0, 700.0);
    app.sel = Some(TextSel {
        anchor: DocPos { block: 0, ch: 0 },
        head: DocPos {
            block: 1,
            ch: "And prose".len(),
        },
    });
    assert_eq!(
        app.selection_clipboard_text(),
        "Tell me, O Muse, of that sagacious man\nWho, having overthrown the city-towers\nOf Ilium, wandered far\n\nAnd prose"
    );
    assert_eq!(
        app.selection_text(),
        "Tell me, O Muse, of that sagacious man Who, having overthrown the city-towers Of Ilium, wandered far And prose"
    );

    let tree = Widget::accessibility(&app).expect("Books semantic root");
    let document = tree
        .children
        .iter()
        .find(|node| node.role == Role::Document)
        .expect("current page document");
    let screen = app.rect;
    let verse = document
        .children
        .iter()
        .find(|node| {
            node.name
                .as_deref()
                .is_some_and(|name| name.starts_with("Tell me"))
        })
        .expect("verse paragraph node");
    let bounds = verse.bounds.expect("verse paragraph has bounds").to_rect();
    let runs: Vec<_> = app.sel_runs.iter().filter(|run| run.block == 0).collect();
    assert!(runs.len() >= 3, "every verse line is painted");
    assert!(
        bounds.y0 >= screen.y0 && bounds.y1 <= screen.y1,
        "{bounds:?} on screen"
    );
    assert!(runs.iter().all(|run| {
        bounds.contains(Point::new(
            f64::midpoint(run.x0, run.x1),
            f64::midpoint(run.y0, run.y1),
        ))
    }));
    let document_bounds = document.bounds.expect("page bounds").to_rect();
    assert!(document_bounds.contains(bounds.center()));
}

/// Keyboard selection extends and copy targets the full active selection.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn keyboard_selection_and_copy_target_the_active_passage() {
    // --- phase: shift_and_ctrl_shift_arrows_extend_selection ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowLeft,
            ModifiersState::SHIFT
        ));
        assert_eq!(app.sel.unwrap().head, start, "char contract");
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

    // --- phase: copy_action_copies_the_full_selected_passage ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        let copy = find_hit(&app, |a| matches!(a, Action::CopySelection))
            .expect("a Copy action in the HUD");
        assert!(click(&mut app, &mut doc, copy), "copy handled");
        assert!(
            app.sel.is_some_and(|s| !s.is_empty()),
            "Copy keeps the selection"
        );
    }

    // --- phase: copy_key_shortcut_targets_the_active_selection ---
    {
        // A bare `c` copies only when a selection is live (mirroring `h`
        // highlight / `m` note); with no selection it is not consumed.
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
}

/// A live start seeds no annotations, opens on an empty library and onboards with open-book.
#[test]
fn live_start_is_empty_and_onboards() {
    // --- phase: runtime_start_seeds_no_annotations ---
    {
        // A non-demo app must never fabricate marks into a fresh document.
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            TestLibrary::list_annotations(&doc).is_empty(),
            "no seeded annotations"
        );
    }

    // --- phase: live_constructor_starts_empty_on_the_library ---
    {
        // The live app opens onto the shelf (its home), empty until the document
        // load on the first sync — never a fabricated starter book.
        let app = BooksApp::new(crate::resource::library_dispatcher_for_test(), false);
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

    // --- phase: the_empty_shelf_onboards_with_open_book_not_a_reload ---
    {
        // The empty state offers the primary way in — open a book from anywhere —
        // and never asks the reader to drop files in a magic folder and reload.
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        assert!(app.library.is_empty());
        assert!(
            find_hit(&app, |a| matches!(a, Action::OpenViaPortal)).is_some(),
            "the empty shelf offers an Open-book action"
        );
    }
}

/// The shelf lays out a bounded grid of cards, filters and sorts, and is driven by keyboard focus.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn library_shelf_layout_filtering_and_keyboard() {
    // --- phase: library_keyboard_moves_focus_and_opens_the_focused_book ---
    {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1280.0, 720.0);
        assert!(app.library.len() >= 3, "sample shelf has several books");
        assert_eq!(app.shelf_cursor, 0, "focus starts on the first book");

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

    // --- phase: library_arrow_down_steps_a_whole_row ---
    {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
        // A narrow window forces a single column, so down moves to the next book.
        frame(&mut app, &mut doc, 320.0, 700.0);
        assert_eq!(app.shelf_cols, 1, "one column at this width");
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowDown));
        assert_eq!(
            app.shelf_cursor, 1,
            "down stepped one row (one book) in a single column"
        );
    }

    // --- phase: short_desktop_shelf_uses_a_centred_bounded_grid ---
    {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1440.0, 900.0);
        assert!(
            app.shelf_cols >= 4,
            "three books and the add tile share one row on a desktop window"
        );
        let card = hit_rect(&app, |a| matches!(a, Action::OpenBook(_))).expect("a book card");
        assert!(card.width() < 240.0, "desktop cards keep their media size");
    }

    // --- phase: populated_shelf_offers_an_open_book_affordance ---
    {
        // A shelf that already has books still offers the trailing "Add a book"
        // tile plus the top-bar Open action, so there is always a way in.
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
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

    // --- phase: card_shows_the_title_once_generated_vs_real_cover ---
    {
        // A generated cover sets the title in display type on the cover itself,
        // so `cover_image` yields None and the under-cover shelf label is
        // suppressed — the title renders exactly once. Real cover art keeps the
        // shelf label (its embedded title may not be legible on the shelf), and
        // does not itself re-draw the title, so it too shows once.
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
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

    // --- phase: shelf_hide_read_filters_and_sort_orders ---
    {
        use crate::model::{LibraryPrefs, LibrarySort, Progress};
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = library_doc();
        let ids: Vec<String> = app.library.iter().map(|b| b.id.clone()).collect();
        assert!(ids.len() >= 3, "sample shelf has several books");
        // Mark the first book as finished; give the others open times.
        TestLibrary::write_progress(
            &mut doc,
            &ids[0],
            &Progress {
                chapter: 9,
                block: 9,
                fraction: 1.0,
                opened: 100,
            },
        );
        TestLibrary::write_progress(
            &mut doc,
            &ids[1],
            &Progress {
                fraction: 0.3,
                opened: 500,
                ..Default::default()
            },
        );
        TestLibrary::write_progress(
            &mut doc,
            &ids[2],
            &Progress {
                fraction: 0.6,
                opened: 300,
                ..Default::default()
            },
        );

        // Hide-read on: the finished book drops out of the display order.
        TestLibrary::write_library_prefs(
            &mut doc,
            LibraryPrefs {
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
        TestLibrary::write_library_prefs(
            &mut doc,
            LibraryPrefs {
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
        TestLibrary::write_library_prefs(
            &mut doc,
            LibraryPrefs {
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

        assert!(
            app.shelf_controls
                .accessibility_nodes(app.library_prefs, None)
                .iter()
                .any(|node| node.action_target.as_deref() == Some("books.library.sort")),
            "a sort control is present"
        );
        assert!(
            app.shelf_controls
                .has_action(ShelfControlAction::ToggleHideRead),
            "a hide-read toggle is present"
        );
    }
}

/// Books open by path or picker, failures settle visibly, watch scans never replace explicit opens, one book has one identity and removal keeps dedup.
#[test]
#[serial]
fn importing_and_removing_books_keeps_one_identity() {
    // --- phase: a_picked_book_is_imported_and_opened_through_the_open_book_effect ---
    // A book chosen through the host's file chooser (a recording host answers
    // it — no real dialog) joins the library and opens in the reader.
    {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("picked.epub");
        std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
        let workspace = tempfile::NamedTempFile::new().unwrap();
        let _workspace = workspace_env(workspace.path());

        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = ui::framework::testing::TestApp::<crate::EbookShell>::new().unwrap();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
        app.effect(&crate::resource::BooksEffect::OpenBook).unwrap();
        app.settle();
        assert_eq!(recorder.pick_requests()[0].title(), "Open a book");

        let shelf: Vec<crate::model::LibraryEntry> =
            app.state().entries.values().cloned().collect();
        assert_eq!(shelf.len(), 1, "the picked book joined the library");
        assert_eq!(
            shelf[0].content_hash().as_deref(),
            Some(tilo_content::hash_bytes(crate::content::SAMPLE_EPUB).as_str()),
            "without a workspace its bytes are kept in the content store"
        );
        let chapter = crate::content::bundled_sample().unwrap().chapters[0]
            .title
            .clone();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .a11y_node_named(&chapter)
                    .is_some()),
            "and opened in the reader"
        );
    }

    // --- phase: watch_scan_cannot_replace_an_explicit_open ---
    {
        let tmp = tempfile::tempdir().unwrap();
        let open = tmp.path().join("open.epub");
        let watched = tmp.path().join("watched.epub");
        std::fs::write(&open, crate::content::SAMPLE_EPUB).unwrap();
        std::fs::write(&watched, crate::content::SAMPLE_EPUB).unwrap();
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());

        let (_entered, _release) = app.begin_import_held(&open, ImportKind::Open).unwrap();
        app.begin_import(watched, ImportKind::Watch).unwrap();
        let loading = app.import_loading.as_ref().expect("open remains pending");
        assert_eq!(loading.kind, ImportKind::Open);
        assert_eq!(loading.path, open);
    }

    // --- phase: importer_disconnect_and_generation_exhaustion_are_visible_failures ---
    {
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let path = PathBuf::from("/tmp/book.epub");
        app.import_worker.as_ref().unwrap().close();
        let error = app
            .begin_import(path.clone(), ImportKind::Open)
            .unwrap_err();
        app.settle_import_failure(error, "ebook.importer_unavailable");
        assert!(
            app.load_error
                .as_deref()
                .is_some_and(|s| s.contains("importer"))
        );

        let mut exhausted =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
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

    // --- phase: the_same_book_from_two_paths_shares_one_identity ---
    // The same file opened from two different paths resolves to one content-hash
    // identity, so its highlights and progress are shared — the whole point of
    // keying resources to the book, not the path or the device.
    {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let pa = a.path().join("from-downloads.epub");
        let pb = b.path().join("from-usb-stick.epub");
        std::fs::write(&pa, crate::content::SAMPLE_EPUB).unwrap();
        std::fs::write(&pb, crate::content::SAMPLE_EPUB).unwrap();

        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 720.0);

        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &pa);
        let id = app.library[0].id.clone();
        // A highlight made while reading the first copy.
        TestLibrary::upsert_annotation(&mut doc, &Annotation::new(&id, AnnoKind::Highlight, 0, 1));
        assert_eq!(TestLibrary::annotations_for(&doc, &id).len(), 1);

        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &pb);
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
            TestLibrary::annotations_for(&doc, &app.library[0].id).len(),
            1,
            "the highlight follows the book to the new path"
        );
    }

    // --- phase: removing_a_book_takes_it_off_the_shelf_and_out_of_the_registry ---
    {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("book.epub");
        std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &path);
        assert_eq!(app.library.len(), 1);
        assert_eq!(TestLibrary::read_library_entries(&doc).len(), 1);

        let id = app.library[0].id.clone();
        app.dispatch(
            Action::RemoveFromLibrary(id.clone()),
            &mut Edits::lend(&mut doc),
        );
        assert!(app.library.is_empty(), "removed from the shelf");
        assert_eq!(
            TestLibrary::read_library_entries(&doc).len(),
            1,
            "the library asks the person before it removes the book"
        );
        let mut library =
            ui::tilo_resource::Replica::<crate::resource::Ebook>::from_doc(doc.clone());
        library
            .apply_confirmed(
                crate::resource::Ebook::REMOVE_FROM_LIBRARY
                    .with(crate::resource::books::RemoveFromLibrary { book: id }),
            )
            .unwrap();
        doc = library.doc().clone();
        assert!(
            TestLibrary::read_library_entries(&doc).is_empty(),
            "and from the registry"
        );
    }

    // --- phase: removing_one_watched_book_preserves_other_source_deduplication ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let removed_id = app.library[0].id.clone();
        let retained_id = app.library[1].id.clone();
        let removed_source = PathBuf::from("/watched/removed.epub");
        let retained_source = PathBuf::from("/watched/retained.epub");
        app.watched_sources
            .insert(removed_source.clone(), removed_id);
        app.watched_sources
            .insert(retained_source.clone(), retained_id);

        let id = app.library[0].id.clone();
        app.dispatch(Action::RemoveFromLibrary(id), &mut Edits::lend(&mut doc));

        assert!(
            !app.watched_sources.contains_key(&removed_source),
            "only the removed book becomes eligible for watch retry"
        );
        assert!(
            app.watched_sources.contains_key(&retained_source),
            "unrelated watched books stay deduplicated"
        );
    }
}

/// The import worker never blocks, rejects stale results and wakes its idle host on completion.
#[test]
#[serial]
fn import_worker_is_non_blocking_and_wakes_its_host() {
    // --- phase: import_worker_never_blocks_and_rejects_a_stale_result ---
    {
        // Keep this assertion specifically about latest-request wins and local
        // fallback. A file root cannot host `Books`, so workspace
        // materialisation is unavailable regardless of runner privileges.
        let workspace = tempfile::NamedTempFile::new().unwrap();
        let _workspace = workspace_env(workspace.path());
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let pa = a.path().join("slow-a.epub");
        let pb = b.path().join("new-b.epub");
        std::fs::write(&pa, crate::content::SAMPLE_EPUB).unwrap();
        std::fs::write(&pb, crate::content::SAMPLE_EPUB).unwrap();
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();

        // Queueing returns while the worker is held inside A's parse, so the UI
        // thread never waits for a parse to finish.
        let (entered, release_a) = app.begin_import_held(&pa, ImportKind::Open).unwrap();
        entered
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the worker took A");
        assert!(
            app.import_loading.is_some(),
            "A is pending, not parsed inline"
        );
        // B supersedes A while A is still in the worker. A finishes afterwards,
        // and its generation must be ignored.
        app.begin_import(pb.clone(), ImportKind::Open).unwrap();
        drop(release_a);
        assert_eq!(app.import_loading.as_ref().map(|l| &l.path), Some(&pb));

        wait_for_import(&mut app, &mut doc);
        let book = app.open_book().expect("new request opened");
        assert_eq!(app.library_paths.get(&book.id), Some(&pb));
        assert!(
            !TestLibrary::read_library_entries(&doc)[0].shared,
            "a local import never publishes its source location"
        );
    }

    // --- phase: completed_import_wakes_its_idle_host ---
    {
        let tmp = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(tmp.path());
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
                fetch: None,
                hold: None,
            })
            .unwrap();
        let event = receiver
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("completion wakes the host without an input event or polling frames");
        assert!(scope.accepts(&event));
        assert!(matches!(worker.try_recv(), Ok(result) if result.result.is_ok()));
    }
}

/// Workspace books surface as shared; sharing materialises before success, remote opens verify content ids, and share jobs report their lifecycle.
#[test]
#[serial]
fn workspace_sharing_materialises_and_verifies_books() {
    // --- phase: a_workspace_book_is_surfaced_as_shared ---
    // A book living on the collaborative workspace is surfaced as shared; a book
    // anywhere else is local.
    {
        let ws = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(ws.path());
        let books = crate::content::workspace_books_dir();
        std::fs::create_dir_all(&books).unwrap();
        let shared_path = books.join("shared.epub");
        std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();

        let local = tempfile::tempdir().unwrap();
        let local_path = local.path().join("local.epub");
        std::fs::write(&local_path, crate::content::SAMPLE_EPUB).unwrap();

        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 720.0);

        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &shared_path);
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

    // --- phase: sharing_a_local_book_moves_it_onto_the_workspace ---
    {
        let ws = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(ws.path());
        let home = tempfile::tempdir().unwrap();
        let src = home.path().join("mine.epub");
        std::fs::write(&src, crate::content::SAMPLE_EPUB).unwrap();

        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &src);
        assert!(!app.book_is_shared(0), "starts local");

        let id = app.library[0].id.clone();
        app.dispatch(Action::ShareToWorkspace(id), &mut Edits::lend(&mut doc));
        assert!(
            app.book_is_shared(0),
            "sharing puts the book on the workspace"
        );
    }

    // --- phase: canonical_share_operation_materialises_before_reporting_success ---
    {
        let workspace = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(workspace.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("local.epub");
        std::fs::write(&source, crate::content::SAMPLE_EPUB).unwrap();
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &source);
        let id = app.library[0].id.clone();
        let payload =
            crate::actions::effect_payload(&crate::resource::BooksEffect::Share { book: id });
        let invocation = tilo_actions::InvocationId::new();

        assert!(matches!(
            app.start_effect(invocation, &payload, &doc).unwrap(),
            ui::framework::Effected::Pending
        ));
        assert!(
            !TestLibrary::read_library_entries(&doc)[0].shared,
            "the shared row is not advertised before materialisation commits"
        );
        app.read_record(&doc);
        app.settle(&mut Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(app.book_is_shared(0));
        assert!(TestLibrary::read_library_entries(&doc)[0].shared);
        assert!(matches!(
            app.take_effect_result(invocation),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    // --- phase: remote_open_operation_hydrates_verified_workspace_bytes_before_revealing ---
    {
        let workspace = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(workspace.path());
        let seed = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(seed.path(), crate::content::SAMPLE_EPUB).unwrap();
        let remote = crate::content::parse_path_detailed(seed.path()).unwrap();
        let id = remote.id.clone();
        let shared_path = crate::content::workspace_book_path(&id);
        std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
        std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();

        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        let (_, entry) = crate::content::prepare_imported_book(&shared_path, remote);
        TestLibrary::upsert_library_entry(&mut doc, &entry);
        let invocation = tilo_actions::InvocationId::new();
        let payload = crate::actions::effect_payload(&crate::resource::BooksEffect::Download {
            book: id.clone(),
        });

        assert!(matches!(
            app.start_effect(invocation, &payload, &doc).unwrap(),
            ui::framework::Effected::Pending
        ));
        wait_for_import(&mut app, &mut doc);
        assert_eq!(app.open_book().map(|book| &book.id), Some(&id));
        assert_eq!(app.library_paths.get(&id), Some(&shared_path));
        assert!(matches!(
            app.take_effect_result(invocation),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    // --- phase: remote_open_refuses_workspace_bytes_with_the_wrong_content_id ---
    {
        let workspace = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(workspace.path());
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        let id = "book:claimed-but-wrong".to_string();
        let shared_path = crate::content::workspace_book_path(&id);
        std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
        std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();
        TestLibrary::upsert_library_entry(
            &mut doc,
            &crate::model::LibraryEntry {
                id: id.clone(),
                shared: true,
                title: "Wrong bytes".into(),
                author: String::new(),
                subject: String::new(),
                added: 1,
                content: ui::tilo_resource::Bytes::Empty,
                index: ui::tilo_resource::Bytes::Empty,
            },
        );
        let payload =
            crate::actions::effect_payload(&crate::resource::BooksEffect::Download { book: id });
        let invocation = tilo_actions::InvocationId::new();
        app.start_effect(invocation, &payload, &doc).unwrap();
        wait_for_import(&mut app, &mut doc);
        assert!(app.library.is_empty());
        assert!(matches!(
            app.take_effect_result(invocation),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
    }

    // --- phase: cancellable_share_operation_reports_its_own_lifecycle_status ---
    {
        let workspace = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(workspace.path());
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("cancel.epub");
        std::fs::write(&source, crate::content::SAMPLE_EPUB).unwrap();
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        app.import_and_open_for_test(&mut Edits::lend(&mut doc), &source);
        let payload = crate::actions::effect_payload(&crate::resource::BooksEffect::Share {
            book: app.library[0].id.clone(),
        });
        let invocation = tilo_actions::InvocationId::new();
        app.start_effect(invocation, &payload, &doc).unwrap();

        assert!(
            app.cancel_effect(invocation),
            "the share stops as its caller asked"
        );
        assert!(app.take_effect_result(invocation).is_none());
        app.read_record(&doc);
        app.settle(&mut Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(
            !TestLibrary::read_library_entries(&doc)[0].shared,
            "cancelling before the UI commit never advertises a shared copy"
        );
    }
}

#[test]
fn annotation_cards_delete_and_jump_to_their_location() {
    // --- phase: annotation_delete_is_reachable_and_removes_it ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        // Wide window so the annotations rail is docked-open.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let __p = page_point(&app);
        dbl_click(&mut app, &mut doc, __p);
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        click(&mut app, &mut doc, swatch);
        assert_eq!(
            TestLibrary::list_annotations(&doc).len(),
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
            TestLibrary::list_annotations(&doc).is_empty(),
            "annotation removed"
        );
    }

    // --- phase: clicking_an_annotation_card_jumps_to_its_location ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        // Docked notes rail at a wide width so the card (with a JumpTo hit) shows.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let book_id = app.book_id();
        let nblocks = app.library[app.book_idx].chapters[0].blocks.len().max(1);
        let target = (nblocks / 2).min(nblocks - 1);
        let a = Annotation::new(&book_id, AnnoKind::Highlight, 0, target)
            .with_color(HiColor::Green)
            .with_excerpt("anchored passage");
        TestLibrary::upsert_annotation(&mut doc, &a);
        frame(&mut app, &mut doc, 1280.0, 800.0);

        app.chapter = app.num_chapters().saturating_sub(1);
        let jump = find_hit(&app, |ac| matches!(ac, Action::JumpTo(_))).expect("annotation card");
        assert!(click(&mut app, &mut doc, jump), "jump handled");
        assert_eq!(app.chapter, 0, "jumped to the annotation's chapter");
        assert_eq!(
            app.pending_jump_block,
            Some(target),
            "jump targets the annotation's block"
        );
        frame(&mut app, &mut doc, 1280.0, 800.0);
        assert!(
            app.pending_jump_block.is_none(),
            "pending jump was consumed by layout"
        );
    }
}

/// Chapter destinations start at the beginning and never reuse another
/// chapter's selection for annotation actions.
#[test]
fn chapter_navigation_discards_previous_passage_and_seek() {
    for mode in [ReadMode::Paged, ReadMode::Continuous] {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let chapter = app.library[0].chapters[0].clone();
        Arc::make_mut(&mut Arc::make_mut(&mut app.library)[0])
            .chapters
            .push(chapter);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        dispatch(&mut app, &mut doc, Action::SetMode(mode));
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let point = page_point(&app);
        dbl_click(&mut app, &mut doc, point);
        assert!(app.sel.is_some_and(|selection| !selection.is_empty()));

        // A queued seek must not win over the later chapter destination.
        app.pending_seek_fraction = Some(0.9);
        app.scroll_y = 200.0;
        dispatch(&mut app, &mut doc, Action::GotoChapter(1));
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.chapter, 1);
        assert_eq!(app.page, 0);
        assert_eq!(app.scroll_y, 0.0);
        assert!(app.sel.is_none());
        dispatch(&mut app, &mut doc, Action::SetColor(HiColor::Green));
        dispatch(&mut app, &mut doc, Action::AddNote);
        assert!(TestLibrary::list_annotations(&doc).is_empty());
    }
}

// ---- single source of truth: affordance == palette ActionSpec -----------

/// Deliver a library action with the typed loan native callbacks receive.
fn dispatch(app: &mut BooksApp, doc: &mut AutoCommit, action: Action) {
    app.dispatch(action, &mut Edits::lend(doc));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn selection_toolbar_dispatches_palette_highlight_actions() {
    // --- phase: selection_swatch_dispatches_the_same_action_as_the_palette ---
    // The selection-toolbar swatch, toggling an existing highlight off, dispatches
    // the byte-identical `EncodedAction` the book resource's "Highlight selection"
    // `ActionSpec` exports — the swatch and the palette are one source of truth.
    // Drives the real `SetColor` dispatch arm a swatch click invokes.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        let hl = TestLibrary::list_annotations(&doc)
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

        let mut expected =
            ui::tilo_resource::Replica::<crate::resource::Ebook>::from_doc(doc.fork());
        expected
            .apply(
                crate::resource::Ebook::HIGHLIGHT.with(
                    app.resolve_highlight_plan(app.sel.expect("selection"), app.hi_color)
                        .expect("canonical selection plan"),
                ),
            )
            .unwrap();
        dispatch(&mut app, &mut doc, Action::SetColor(HiColor::Yellow));
        let places = |marks: Vec<Annotation>| {
            let mut places: Vec<_> = marks
                .into_iter()
                .map(|a| (a.kind.as_str(), a.block, a.start, a.end, a.color.as_str()))
                .collect();
            places.sort_unstable();
            places
        };
        assert_eq!(
            places(TestLibrary::list_annotations(&doc)),
            places(TestLibrary::list_annotations(expected.doc())),
            "the swatch uses the canonical selection action"
        );
        assert!(
            TestLibrary::list_annotations(&doc)
                .iter()
                .all(|a| a.kind != AnnoKind::Highlight),
            "the highlight was removed via the dispatched action",
        );
    }

    // --- phase: selection_toolbar_trash_deletes_the_covered_highlight ---
    // The selection toolbar grows a trash button when the selection covers an
    // existing highlight, and it deletes that highlight (item: explicit delete).
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let p = page_point(&app);
        dbl_click(&mut app, &mut doc, p);
        dispatch(&mut app, &mut doc, Action::SetColor(HiColor::Blue));
        app.read_record(&doc);
        assert_eq!(
            TestLibrary::list_annotations(&doc)
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
            TestLibrary::list_annotations(&doc)
                .iter()
                .all(|a| a.kind != AnnoKind::Highlight),
            "the highlight under the selection was deleted",
        );
    }

    // --- phase: shared_selection_hud_shows_in_continuous_mode ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        dispatch(&mut app, &mut doc, Action::SetMode(ReadMode::Continuous));
        frame(&mut app, &mut doc, 1000.0, 700.0);
        app.sel = Some(TextSel {
            anchor: DocPos { block: 1, ch: 0 },
            head: DocPos { block: 1, ch: 24 },
        });
        app.selected_block = Some(1);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            find_hit(&app, |a| matches!(a, Action::CopySelection)).is_some(),
            "the shared selection HUD floats over the continuous flow"
        );
        assert!(
            find_hit(&app, |a| matches!(a, Action::SetColor(_))).is_some(),
            "the HUD offers highlight swatches"
        );
    }
}

// ---- bookmark toggle (icon + 'b') ---------------------------------------

/// Bookmarks toggle via icon and key, shortcuts rebind locally and reserved commands are rejected accessibly.
#[test]
fn bookmarks_and_their_shortcuts() {
    // --- phase: bookmark_toggles_via_icon_and_key ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let bookmarks = |d: &AutoCommit| {
            TestLibrary::list_annotations(d)
                .into_iter()
                .filter(|a| a.kind == AnnoKind::Bookmark)
                .count()
        };
        assert_eq!(bookmarks(&doc), 0, "no bookmark yet");
        let bm = find_hit(&app, |a| matches!(a, Action::ToggleBookmark)).expect("bookmark icon");
        assert!(click(&mut app, &mut doc, bm), "bookmark icon handled");
        assert_eq!(bookmarks(&doc), 1, "icon added a bookmark");
        // Remove via the 'b' shortcut (same page position -> same anchor).
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(key_char(&mut app, &mut doc, "b"), "'b' consumed");
        assert_eq!(bookmarks(&doc), 0, "'b' removed the bookmark");
    }

    // --- phase: device_local_bookmark_shortcut_rebinds_reader_routing ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.reader_keys.set_shortcut("bookmark", "z"));

        assert!(key_char(&mut app, &mut doc, "z"), "custom key is consumed");
        assert_eq!(
            TestLibrary::list_annotations(&doc)
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

    // --- phase: shortcut_capture_rejects_reserved_reader_commands_accessibly ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Shortcuts;
        app.dispatch(
            Action::BeginShortcutCapture(ReaderShortcut::Bookmark),
            &mut Edits::lend(&mut doc),
        );

        assert!(
            key_char(&mut app, &mut doc, "t"),
            "capture consumes the key"
        );
        assert_eq!(app.shortcut_capture, Some(ReaderShortcut::Bookmark));
        assert_eq!(
            app.reader_keys.shortcut(
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
}

/// Large-text, reduced-motion and max-scale reader shots render within the page box with the expected chrome.
#[test]
fn reader_shots_render_within_the_page_box() {
    // --- phase: large_text_shot_is_a_normal_two_hundred_percent_narrow_reader ---
    {
        use ui::offscreen::OffscreenRenderer;

        let (mut app, mut doc) = staged("large-text");
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

    // --- phase: reduced_motion_shot_keeps_the_desktop_reader_chrome ---
    {
        let (mut app, mut doc) = staged("reduced-motion");
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

    // --- phase: narrow_max_text_scale_keeps_rendered_runs_inside_the_page_box ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let mut settings = TestLibrary::read_settings(&doc);
        settings.font_scale = Settings::READING_SCALE_MAX;
        TestLibrary::write_settings(&mut doc, &settings);

        // Every phone width, so the proof does not hinge on where one font's
        // metrics happen to put the line ends.
        for width in (360..=420).step_by(2) {
            app.layout = None;
            frame(&mut app, &mut doc, f64::from(width), 844.0);
            let geom = app.reader_page_geometry(app.rect, app.settings.mode);
            let right = geom.full_content_x + f64::from(geom.full_content_w);
            assert!(right <= app.rect.x1 + 0.01, "content box fits the viewport");
            assert!(
                app.sel_runs
                    .iter()
                    .all(|run| { run.x0 >= geom.full_content_x - 0.5 && run.x1 <= right + 0.5 }),
                "{width}px: every measured painted text run reflows inside the narrow page box"
            );
        }
    }
}

/// Reader keyboard drives navigation and chrome, escape peels back layers and rails respond to width.
#[test]
fn reader_keyboard_escape_and_responsive_rails() {
    // --- phase: reader_keyboard_drives_navigation_and_chrome ---
    {
        let mut app = app_with_sample();
        app.book_idx = 1; // Field Notes: three chapters, so nav is deterministic.
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.chapter, 0);

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
            TestLibrary::read_settings(&doc).theme,
            ReadTheme::Light,
            "'1' set Light"
        );
        assert!(key_char(&mut app, &mut doc, "3"));
        assert_eq!(
            TestLibrary::read_settings(&doc).theme,
            ReadTheme::Dark,
            "'3' set Dark"
        );

        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert_eq!(app.view, View::Library, "escape left the book");
    }

    // --- phase: escape_peels_back_popover_then_overlay_then_book ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);

        // Layer 1: an open type popover — first Escape closes only that.
        app.reading_settings.open();
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert!(!app.reading_settings.is_open(), "escape closed the popover");
        assert_eq!(app.view, View::Reader, "still in the book");

        // Layer 2: a narrow window with the contents drawer floating open.
        frame(&mut app, &mut doc, 560.0, 720.0);
        assert!(app.toc_drawer.is_overlay());
        app.toc_drawer.snap(true);
        assert!(app.toc_open(), "overlay flap open");
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert!(!app.toc_open(), "escape dismissed the overlay flap");
        assert_eq!(app.view, View::Reader, "still in the book");

        // Layer 3: nothing floating -> escape leaves the book.
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert_eq!(app.view, View::Library, "escape left the book");
    }

    // --- phase: rails_collapse_on_narrow_and_dock_on_wide ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        // Wide: both rails docked-open by default.
        frame(&mut app, &mut doc, 1280.0, 720.0);
        assert!(app.toc_open(), "contents docked+open at 1280");
        assert!(app.notes_open(), "annotations docked+open at 1280");
        // Narrow: both become closed overlays, so the page owns the width.
        frame(&mut app, &mut doc, 560.0, 720.0);
        assert!(app.toc_drawer.is_overlay());
        assert!(app.notes_drawer.is_overlay());
        assert!(!app.toc_open(), "contents closed at 560");
        assert!(!app.notes_open(), "annotations closed at 560");
    }
}

// ---- reading-progress persistence + restore -----------------------------

#[test]
fn reading_position_persists_across_reopen() {
    // --- phase: reading_progress_persists_and_restores_on_reopen ---
    {
        let mut first = app_with_sample();
        first.book_idx = 1; // multi-chapter
        let mut doc = library_doc();
        frame(&mut first, &mut doc, 1000.0, 700.0);
        let book_id = first.book_id();

        // Advance two chapters; each turn persists progress to the CRDT.
        key_named(&mut first, &mut doc, NamedKey::ArrowDown);
        key_named(&mut first, &mut doc, NamedKey::ArrowDown);
        let reached = first.chapter;
        assert!(reached >= 1, "advanced into the book");
        assert_eq!(
            TestLibrary::read_progress(&doc, &book_id).chapter,
            reached,
            "progress written to the document"
        );

        let mut reopened = app_with_sample();
        reopened.book_idx = 1;
        frame(&mut reopened, &mut doc, 1000.0, 700.0);
        assert_eq!(
            reopened.chapter, reached,
            "reopen restored the saved chapter"
        );
    }

    // --- phase: the_position_is_a_passage_not_a_page_number ---
    // Reopened in a narrower window, the reader lands on the page that holds
    // the passage it left, not on the same page number.
    {
        let mut first = app_with_sample();
        first.book_idx = 1;
        let mut doc = library_doc();
        frame(&mut first, &mut doc, 1000.0, 700.0);
        for _ in 0..2 {
            first.dispatch(Action::NextPage, &mut ui::widget::Edits::lend(&mut doc));
            frame(&mut first, &mut doc, 1000.0, 700.0);
        }
        let saved = TestLibrary::read_progress(&doc, &first.book_id());
        assert_eq!(saved.block, first.reading_block());

        let mut reopened = app_with_sample();
        reopened.book_idx = 1;
        frame(&mut reopened, &mut doc, 600.0, 900.0);
        let layout = reopened.layout.as_ref().expect("laid out");
        let top = layout
            .blocks
            .iter()
            .find(|b| b.index == saved.block)
            .expect("saved block")
            .top;
        let start = layout.pages.get(reopened.page).copied().unwrap_or(0.0);
        let end = layout
            .pages
            .get(reopened.page + 1)
            .copied()
            .unwrap_or(f32::MAX);
        assert_eq!(reopened.chapter, saved.chapter);
        assert!(
            (start - 0.5..end).contains(&top),
            "page {} ({start}..{end}) holds the saved block's top {top}",
            reopened.page
        );
    }

    // --- phase: continuation_round_trip_reopens_the_same_book_and_reading_position ---
    {
        let mut sender = app_with_sample();
        sender.book_idx = 1;
        sender.chapter = 1;
        sender.pending_jump_block = Some(3);
        let source = library_doc();
        let metadata = sender
            .continuation_metadata(&source, &automerge::ROOT)
            .expect("an open library book has a portable continuation");

        let mut receiver = app_with_sample();
        let mut target = library_doc();
        assert!(receiver.restore_continuation_metadata(
            &metadata,
            &mut Edits::lend(&mut target),
            &automerge::ROOT,
        ));
        assert_eq!(
            receiver.library[receiver.book_idx].id,
            sender.library[sender.book_idx].id
        );
        // The position is a passage, so it survives a different window size.
        assert_eq!(
            (receiver.chapter, receiver.pending_jump_block),
            (1, Some(3))
        );
    }
}

/// A continued book must actually open from the live app's library screen.
#[test]
fn continuation_opens_the_reader_from_the_library() {
    let mut sender = app_with_sample();
    sender.book_idx = 1;
    sender.chapter = 1;
    sender.pending_jump_block = Some(3);
    let metadata = sender
        .continuation_metadata(&AutoCommit::new(), &automerge::ROOT)
        .expect("the reading target is portable");
    let mut receiver = app_with_sample();
    receiver.view = View::Library;
    let mut doc = library_doc();
    let before = doc.save();
    assert!(receiver.restore_continuation_metadata(
        &metadata,
        &mut Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(
        doc.save(),
        before,
        "restoring presentation grants no document mutation"
    );
    frame(&mut receiver, &mut doc, 600.0, 900.0);
    assert_eq!(
        receiver.view,
        View::Reader,
        "the first frame must show the continued book"
    );
    assert_eq!(receiver.library[receiver.book_idx].id, sender.book_id());
    assert!(
        find_semantic_target(
            &Widget::accessibility(&receiver).expect("reader semantics"),
            "books.reader.back"
        )
        .is_some()
    );
}

/// The first sync must not replace an accepted passage with stale shared progress.
#[test]
fn continuation_keeps_the_received_passage_through_first_frame() {
    let mut sender = app_with_sample();
    sender.book_idx = 1;
    sender.chapter = 1;
    sender.pending_jump_block = Some(3);
    let metadata = sender
        .continuation_metadata(&AutoCommit::new(), &automerge::ROOT)
        .expect("the reading target is portable");
    let mut receiver = app_with_sample();
    let mut doc = library_doc();
    assert!(receiver.restore_continuation_metadata(
        &metadata,
        &mut Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    frame(&mut receiver, &mut doc, 600.0, 900.0);
    assert_eq!(
        receiver.chapter, 1,
        "received chapter survives ordinary sync"
    );
    let layout = receiver
        .layout
        .as_ref()
        .expect("the receiver rendered its own layout");
    let top = layout
        .blocks
        .iter()
        .find(|block| block.index == 3)
        .expect("received passage")
        .top;
    let start = layout
        .pages
        .get(receiver.page)
        .copied()
        .expect("current page");
    let end = layout
        .pages
        .get(receiver.page + 1)
        .copied()
        .unwrap_or(f32::MAX);
    assert!(
        (start - 0.5..end).contains(&top),
        "the receiver's page holds the received passage"
    );
}

/// Clicking a highlight opens the comment overlay, which adds, edits and deletes comments through the palette action.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn comments_are_added_edited_and_deleted_through_the_overlay() {
    // --- phase: clicking_a_highlight_opens_the_comment_overlay ---
    // A plain click on highlighted text opens that highlight's floating comment
    // overlay (hit-testing the real char range, not "the paragraph").
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
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
        app.dispatch(Action::SetColor(HiColor::Green), &mut Edits::lend(&mut doc));
        app.read_record(&doc);
        let hl = TestLibrary::list_annotations(&doc)
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

    // --- phase: comment_overlay_adds_edits_and_deletes_multiple_comments ---
    // The comment overlay supports MANY comments per range: add two, edit one,
    // delete one, then trash the whole highlight — all through the real hits.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let p = page_point(&app);
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(
            Action::SetColor(HiColor::Yellow),
            &mut Edits::lend(&mut doc),
        );
        app.read_record(&doc);
        let group = TestLibrary::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("a highlight")
            .group;
        app.comment_overlay.open(group);
        frame(&mut app, &mut doc, 1000.0, 700.0);

        let notes_in_group = |d: &AutoCommit| {
            TestLibrary::list_annotations(d)
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
        let edit =
            find_hit(&app, |a| matches!(a, Action::EditComment(_))).expect("an edit affordance");
        assert!(click(&mut app, &mut doc, edit), "edit handled");
        assert!(
            app.comment_overlay.editing_id().is_some(),
            "a comment reopened for editing"
        );
        key_named(&mut app, &mut doc, NamedKey::Enter);

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
            TestLibrary::list_annotations(&doc)
                .into_iter()
                .all(|a| a.group != group),
            "deleting the highlight removed every row in its group"
        );
        assert!(!app.comment_overlay.is_open(), "the overlay closed");
    }

    // --- phase: comment_delete_button_dispatches_the_same_action_as_the_palette ---
    // The comment-overlay Delete (×) button dispatches the byte-identical
    // `EncodedAction` the palette's "Delete comment" `ActionSpec` exports — proof
    // the two are one code path, not two. Drives the real click through
    // `handle_event` and compares against the live resource tree.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let book_id = app.book_id();
        let group = Uuid::from_u128(0x2222_3333_4444_5555_6666_7777_8888_9999);
        let cid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_00AB);
        let hl = Annotation::new(&book_id, AnnoKind::Highlight, 0, 1)
            .with_excerpt("a marked passage")
            .with_group(group);
        TestLibrary::upsert_annotation(&mut doc, &hl);
        let mut cm = Annotation::new(&book_id, AnnoKind::Note, 0, 1)
            .with_excerpt("a marked passage")
            .with_note("a comment")
            .with_group(group);
        cm.id = cid;
        TestLibrary::upsert_annotation(&mut doc, &cm);
        // Open the highlight's comment overlay (presentation).
        app.comment_overlay.open(group);
        frame(&mut app, &mut doc, 1280.0, 800.0);

        // Root discovery stays bounded; the palette resolves this canonical
        // exact resource through its immutable search provider.
        let spec_encoded = {
            let provider = crate::search_provider::BookSearchProvider::new_shared(
                Arc::clone(&app.library),
                Arc::new(TestLibrary::list_annotations(&doc)),
                Arc::new(TestLibrary::read_library_entries(&doc)),
                1,
            );
            let comment = provider
                .resolve(&crate::resource::mark_place_resource(&cm, true))
                .expect("exact comment resource");
            let spec = comment
                .actions
                .iter()
                .find(|a| a.id == "delete-annotation")
                .expect("delete-annotation spec");
            spec.build_json(&serde_json::Map::new())
                .expect("a bound delete takes no arguments")
        };

        let palette_action: <crate::resource::Ebook as ui::tilo_resource::Resource>::Action =
            serde_json::from_slice(&spec_encoded.payload).unwrap();
        let mut expected =
            ui::tilo_resource::Replica::<crate::resource::Ebook>::from_doc(doc.fork());
        expected.apply_action(&palette_action).unwrap();
        let expected = expected.doc().clone();

        // Click the overlay's per-comment × button (the real affordance).
        let del = find_hit(&app, |a| matches!(a, Action::DeleteComment(_)))
            .expect("a comment delete button");
        assert!(
            click(&mut app, &mut doc, del),
            "delete-comment click handled"
        );

        assert_eq!(
            TestLibrary::list_annotations(&doc),
            TestLibrary::list_annotations(&expected),
            "the button and the palette make the identical edit",
        );
        assert!(
            TestLibrary::list_annotations(&doc)
                .iter()
                .all(|a| a.id != cid),
            "the comment was deleted via the dispatched action",
        );
    }
}

/// The font rename editor uses real text input, commits on blur, cancels on escape and fits before the controls.
#[test]
#[serial]
fn font_rename_editor_commits_cancels_and_fits() {
    // --- phase: font_rename_uses_real_text_input_and_commits_on_blur ---
    // The font-rename field is the toolkit's real `TextInput`: Ctrl+A selects
    // the whole value (so the next keystroke replaces it), and a click
    // outside the field blur-commits the rename to the CRDT.
    {
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1000.0, 800.0);

        // Begin an inline rename on the built-in serif face via the real action,
        // then lay the editor out with another frame.
        app.dispatch(
            Action::BeginRenameFont("serif".into()),
            &mut Edits::lend(&mut doc),
        );
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

        click(&mut app, &mut doc, Point::new(950.0, 24.0));
        assert!(app.renaming.is_none(), "clicking outside ended the rename");
        let fonts = TestLibrary::read_fonts(&doc);
        let serif = fonts
            .iter()
            .find(|f| f.id == "serif")
            .expect("serif entry present");
        assert_eq!(
            serif.name, "Z",
            "the blur committed the new name to the CRDT"
        );
    }

    // --- phase: font_rename_escape_cancels_without_writing ---
    // Esc cancels an inline font rename: the field closes and the CRDT keeps
    // the original name — proven on the real `handle_event` path.
    {
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1000.0, 800.0);

        app.dispatch(
            Action::BeginRenameFont("serif".into()),
            &mut Edits::lend(&mut doc),
        );
        frame(&mut app, &mut doc, 1000.0, 800.0);
        key_char(&mut app, &mut doc, "Q"); // would-be new value
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Escape),
            "Esc is consumed by the editor",
        );
        assert!(app.renaming.is_none(), "Esc closed the rename");
        let fonts = TestLibrary::read_fonts(&doc);
        let serif = fonts
            .iter()
            .find(|f| f.id == "serif")
            .expect("serif entry present");
        assert_eq!(
            serif.name,
            ui::ReadingFace::Default.name(),
            "cancel kept the original name"
        );
    }

    // --- phase: font_rename_editor_stops_before_the_controls_and_starts_on_the_name ---
    // The inline font-rename editor ends an `s3` before the row's control
    // cluster (a fixed 210px reservation used to run it under the toggle),
    // and its left edge backs off by the shared field's text inset so the
    // edited name starts exactly where the static name is painted.
    {
        let mut app =
            BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1000.0, 800.0);
        let static_name_x = hit_rect(
            &app,
            |a| matches!(a, Action::BeginRenameFont(id) if id == "mono"),
        )
        .expect("the static name is a rename target")
        .x0;

        app.dispatch(
            Action::BeginRenameFont("mono".into()),
            &mut Edits::lend(&mut doc),
        );
        frame(&mut app, &mut doc, 1000.0, 800.0);
        let field = app
            .renaming
            .as_ref()
            .expect("rename in progress")
            .field
            .get_rect();
        let toggle = hit_rect(
            &app,
            |a| matches!(a, Action::ToggleFontShown(id) if id == "mono"),
        )
        .expect("the row's toggle is a hit target");
        let theme = Theme::dark();
        assert!(
            field.x1 + f64::from(theme.tokens.spacing.s3) <= toggle.x0 + 0.01,
            "editor ends at {} but the toggle starts at {}",
            field.x1,
            toggle.x0
        );
        assert!(
            (field.x0 - static_name_x).abs() < 0.01,
            "the editor's edge {} is not on the name's edge {static_name_x}",
            field.x0
        );
    }
}

/// TOC rows grow with wrapped titles and the running header fits above the rule at 2x.
#[test]
fn reader_text_rows_fit_at_2x() {
    // --- phase: toc_rows_grow_with_the_text_at_2x_and_never_overlap ---
    // Contents rows are navigation rows: each is as tall as its line at the
    // text scale, so at 200 % rows grow instead of their titles running into
    // the next chapter, and they stay ordered and non-overlapping.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        let rows_at = |app: &mut BooksApp, doc: &mut AutoCommit, scale: f32| -> Vec<Rect> {
            // The contents sidebar may float at a large text scale: open it.
            frame_at_text_scale(app, doc, 1280.0, 800.0, scale);
            app.toc_drawer.snap(true);
            frame_at_text_scale(app, doc, 1280.0, 800.0, scale);
            let theme = Theme::dark().with_text_scale(scale);
            let floor = ui::chrome::nav_row_height(&theme);
            for h in app
                .hits
                .iter()
                .filter(|h| matches!(h.action, Action::GotoChapter(_)))
            {
                assert!(
                    h.rect.height() >= floor - 1e-6,
                    "row {:?} is below the pill floor {floor} at {scale}×",
                    h.rect
                );
            }
            let mut rows: Vec<(usize, Rect)> = app
                .hits
                .iter()
                .filter_map(|h| match h.action {
                    Action::GotoChapter(c) => Some((c, h.rect)),
                    _ => None,
                })
                .collect();
            rows.sort_by_key(|(c, _)| *c);
            rows.into_iter().map(|(_, r)| r).collect()
        };
        let one_x = rows_at(&mut app, &mut doc, 1.0);
        let two_x = rows_at(&mut app, &mut doc, 2.0);
        assert!(
            one_x.len() >= 2 && one_x.len() == two_x.len(),
            "same chapters at both scales"
        );
        for rows in [&one_x, &two_x] {
            for pair in rows.windows(2) {
                assert!(
                    pair[0].y1 <= pair[1].y0 + 1e-6,
                    "contents rows overlap: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
        let tallest_1x = one_x.iter().map(Rect::height).fold(0.0, f64::max);
        let tallest_2x = two_x.iter().map(Rect::height).fold(0.0, f64::max);
        assert!(
            tallest_2x > tallest_1x,
            "a 2× row is taller than a 1× row ({tallest_2x} vs {tallest_1x})"
        );
    }

    // --- phase: running_header_fits_its_slot_above_the_rule_at_2x ---
    // The framed page's running header lives in a slot whose height follows
    // the chrome text scale; the measured 2× header must fit above the rule
    // and the content box must start below it.
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        // A real 2× frame measures the header box the geometry reserves.
        frame_at_text_scale(&mut app, &mut doc, 1280.0, 800.0, 2.0);
        let geom = app.reader_page_geometry(app.rect, app.settings.mode);
        assert!(geom.framed, "1280 wide is the framed page");
        let one_x = {
            let fresh = app_with_sample();
            fresh.reader_page_geometry(Rect::new(0.0, 0.0, 1280.0, 800.0), ReadMode::Paged)
        };
        assert!(
            geom.rule_y > one_x.rule_y,
            "the 2× slot ({}) is taller than the 1× slot ({})",
            geom.rule_y - geom.header_y,
            one_x.rule_y - one_x.header_y
        );
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let title = app
            .library
            .get(app.book_idx)
            .and_then(|b| b.chapters.get(app.chapter))
            .map(|ch| ch.title.clone())
            .expect("the sample book has a current chapter");
        let style = Theme::dark().with_text_scale(2.0).text(TextRole::Caption);
        let (_, head_h) = tr.measure_style(&title, None, &style);
        assert!(
            geom.header_y + f64::from(head_h) <= geom.rule_y,
            "header box ends at {} but the rule is at {}",
            geom.header_y + f64::from(head_h),
            geom.rule_y
        );
        assert!(
            geom.content_y >= geom.rule_y + 1.0,
            "content ({}) starts under the rule ({})",
            geom.content_y,
            geom.rule_y
        );
    }
}

/// Like `frame`, at an accessibility text scale: the theme and the
/// renderer both carry `scale`, exactly as the live host applies it.
fn frame_at_text_scale(app: &mut BooksApp, doc: &mut AutoCommit, w: f64, h: f64, scale: f32) {
    let root = automerge::ROOT;
    app.read_record(doc);
    app.settle(&mut Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &root);
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.resolve_drawers();
    let theme = Theme::dark().with_text_scale(scale);
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(scale);
    let mut scene = Scene::new();
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

fn hit_rect(app: &BooksApp, pred: impl Fn(&Action) -> bool) -> Option<Rect> {
    app.hits
        .iter()
        .rev()
        .find(|h| pred(&h.action))
        .map(|h| h.rect)
}

/// The reading-appearance button holds its 2x label inside the header row,
/// and a font's size stepper is as wide as every value it can show.
#[test]
fn reader_controls_fit_their_labels() {
    // --- phase: reader_type_button_contains_its_label_at_2x_text ---
    {
        for scale in [1.0_f32, 2.0] {
            let mut app = app_with_sample();
            let mut doc = library_doc();
            frame_at_text_scale(&mut app, &mut doc, 800.0, 900.0, scale);
            let button = app.type_button;
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let (_, label_h) = tr.measure_style("Aa", None, &theme.text(TextRole::Label));
            assert!(
                button.height() >= f64::from(label_h) - 0.01,
                "at {scale}× the button ({}) is shorter than its label ({label_h})",
                button.height()
            );
            assert!(
                button.y0 >= app.rect.y0 && button.y1 <= app.rect.y0 + app.header_h + 0.01,
                "at {scale}× the button {button:?} leaves the {}px header row",
                app.header_h
            );
        }
    }

    // --- phase: size_stepper_holds_the_widest_value ---
    {
        let mut app = app_with_sample();
        let mut doc = library_doc();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1200.0, 820.0);
        let field = NumberField::FontMult("mono".into());
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let wanted = app
            .number_controls
            .preferred_width(&field, 100.0, &mut tr, &theme);
        let rect = app
            .number_controls
            .rect(&field)
            .expect("the mono stepper is drawn");
        assert!(
            rect.width() >= wanted - 0.01,
            "the stepper ({}) is narrower than its widest value needs ({wanted})",
            rect.width()
        );
    }
}

#[test]
fn a_rail_shorter_than_the_minimum_scrollbar_thumb_still_draws() {
    let theme = ui::Theme::dark();
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    for (y0, y1) in [(0.0, 1.0), (10.0, 30.0), (0.0, 400.0)] {
        paint_scroll_thumb(
            &mut ctx,
            Rect::new(0.0, y0, 200.0, y1),
            ScrollMetrics::new(300.0, 100.0, 40.0),
        );
    }
}

/// Selecting text docks its controls in the slot above the prose, at the
/// window's UI scale: the text does not move or repaginate, and no control
/// covers a line.
#[test]
fn selection_controls_dock_above_the_prose_without_moving_it() {
    fn draw_at(app: &mut BooksApp, theme: &Theme, width: f64) {
        app.rect = Rect::new(0.0, 0.0, width, 810.0);
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        app.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
    }
    for (scale, width) in [(1.0, 520.0), (1.5, 1440.0), (2.0, 2000.0)] {
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_ui_scale(scale);
            let mut app = app_with_sample();
            draw_at(&mut app, &theme, width);
            let before = app.reader_page_geometry(app.rect, ReadMode::Paged);
            if before.framed {
                assert!(
                    (before.paper_y - before.body_y0 - f64::from(theme.tokens.spacing.s6)).abs()
                        < 0.01,
                    "the page's margin follows the UI scale"
                );
            }
            app.sel = Some(TextSel {
                anchor: DocPos { block: 1, ch: 0 },
                head: DocPos { block: 1, ch: 50 },
            });
            draw_at(&mut app, &theme, width);
            let after = app.reader_page_geometry(app.rect, ReadMode::Paged);
            assert_eq!(
                before.content_y, after.content_y,
                "selecting does not move the text"
            );
            assert_eq!(
                before.content_h, after.content_h,
                "selecting does not repaginate"
            );
            let copy = app
                .hits
                .iter()
                .find(|hit| matches!(hit.action, Action::CopySelection))
                .expect("the selection controls are painted")
                .rect;
            let dock = app
                .hits
                .iter()
                .find(|hit| {
                    matches!(hit.action, Action::Swallow) && hit.rect.contains(copy.center())
                })
                .expect("the selection controls' bar")
                .rect;
            assert!(
                dock.x0 >= after.center_x0 - 0.01
                    && dock.x1 <= after.center_x1 + 0.01
                    && dock.y0 >= after.paper_y - 0.01
                    && dock.y1 + f64::from(theme.tokens.spacing.s2) <= after.content_y + 0.01,
                "{scale}x {width}: the bar {dock:?} stays above the prose at {}",
                after.content_y
            );
            assert!((dock.height() - ui::SelectionHud::height(&theme)).abs() < 0.01);
        }
    }
}

/// A file dropped on the window is taken with no hover before it (Wayland
/// may report only the drop); one that is not a book says so by name.
#[test]
fn a_dropped_file_opens_without_a_hover_and_a_non_book_names_itself() {
    let mut app = BooksApp::new(crate::resource::library_dispatcher_for_test(), false);
    let mut doc = AutoCommit::new();
    assert!(feed(
        &mut app,
        &mut doc,
        &WindowEvent::HoveredFile("/tmp/notes.txt".into())
    ));
    assert!(app.file_open.is_hovering(), "the add tile lights");
    assert!(feed(
        &mut app,
        &mut doc,
        &WindowEvent::DroppedFile("/tmp/notes.txt".into())
    ));
    assert!(!app.file_open.is_hovering());
    assert_eq!(
        app.load_error.as_deref(),
        Some("Couldn't open notes.txt: only EPUB books can be added")
    );
}

/// "Couldn't open x.epub" offers nothing to do, so it says so in the
/// window's bar and leaves on its own after a notice's lifetime: the app
/// wakes for it, and once it is due the bar no longer says it.
#[test]
fn a_failed_open_leaves_on_its_own() {
    use ui::time::testing::with_monotonic_time;
    use ui::widget::toast::NOTICE_LIFETIME;
    let mut app = BooksApp::new(crate::resource::library_dispatcher_for_test(), false);
    let start = ui::time::now();
    let due = start + NOTICE_LIFETIME;
    with_monotonic_time(start, || {
        app.settle_import_failure(
            "Couldn’t open x.epub: not a book".into(),
            "ebook.open_failed",
        );
    });
    assert_eq!(app.next_wake_deadline(), Some(due), "it wakes the app");
    let said = |app: &mut BooksApp| {
        app.expire_load_toast();
        ui::window_toolbar::notice().map(|notice| (notice.text, notice.kind))
    };
    with_monotonic_time(start, || {
        assert_eq!(
            said(&mut app),
            Some((
                "Couldn’t open x.epub: not a book".to_owned(),
                ui::widget::toast::ToastKind::Error
            )),
            "the failure shows in the window's bar"
        );
    });
    with_monotonic_time(due + Duration::from_millis(1), || {
        assert_eq!(said(&mut app), None, "a failure past its time is gone");
    });
    assert_eq!(app.load_error.as_deref(), None);
    assert_eq!(app.next_wake_deadline(), None, "nothing left to wake for");
}

/// "Opening x.epub" follows the import: it stays in the window's bar for as
/// long as the import runs, however long that is, and goes with it.
#[test]
fn opening_a_book_shows_until_the_import_ends() {
    use std::path::PathBuf;
    use ui::time::testing::with_monotonic_time;
    use ui::widget::toast::{NOTICE_LIFETIME, Transient};
    let mut app = BooksApp::new(crate::resource::library_dispatcher_for_test(), false);
    let start = ui::time::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    with_monotonic_time(start, || {
        app.import_loading = Some(LoadingImport {
            generation: 1,
            path: PathBuf::from("/home/reader/Books/x.epub"),
            kind: ImportKind::Open,
            expected_book_id: None,
            card: Transient::showing_progress(()),
        });
    });
    let said = || ui::window_toolbar::notice().map(|notice| notice.text);
    for second in [3, 6, 9] {
        with_monotonic_time(at(second), || {
            app.expire_load_toast();
            assert_eq!(
                said().as_deref(),
                Some("Opening x.epub"),
                "still opening at {second}s"
            );
        });
    }
    assert_eq!(
        app.next_wake_deadline(),
        Some(at(9) + NOTICE_LIFETIME),
        "the app wakes to look at the import again"
    );
    app.import_loading = None;
    with_monotonic_time(at(10), || {
        app.expire_load_toast();
        assert_eq!(said(), None, "the import ended");
    });
    assert_eq!(app.next_wake_deadline(), None);
}

/// The empty library says why the last open failed for as long as that
/// stands, past the failure toast's time, until another import starts.
#[test]
fn the_empty_library_keeps_saying_why_an_open_failed() {
    use std::path::PathBuf;
    use ui::time::testing::with_monotonic_time;
    use ui::widget::toast::NOTICE_LIFETIME;
    let mut app = BooksApp::new(crate::resource::library_dispatcher_for_test(), false);
    let start = ui::time::now();
    let failure = "Couldn’t open x.epub: not a book";
    with_monotonic_time(start, || {
        app.settle_import_failure(failure.into(), "ebook.open_failed");
    });
    with_monotonic_time(start + NOTICE_LIFETIME * 3, || {
        app.expire_load_toast();
        assert_eq!(app.load_error.as_deref(), None, "the toast has gone");
        assert_eq!(
            app.open_failure.as_deref(),
            Some(failure),
            "the state stays"
        );
    });
    app.load_error.dismiss();
    assert_eq!(
        app.open_failure.as_deref(),
        Some(failure),
        "closing the toast leaves the cause standing"
    );
    assert!(
        app.begin_import(PathBuf::from("/home/reader/Books/y.epub"), ImportKind::Open)
            .is_ok()
    );
    assert_eq!(app.open_failure, None, "another import clears it");
}
