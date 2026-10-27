//! The reader's controls live in the window's bar: the pages toggle, the
//! document's name, the tools, the marker colour, the review panel, zoom,
//! Open and Save, History, and find in the pages. Each slot runs the
//! reader's published action.

use serial_test::serial;
use ui::framework::testing::TestApp;
use ui::window_toolbar::{SearchOpening, ToolbarAction, ToolbarEvent, WindowToolbar};

use super::super::selection_tests::text_pdf;
use super::super::tests::launched;
use super::{
    FIT_PAGE, FIT_WIDTH, MARKER_COLOUR, NEXT_PAGE, OPEN, PREVIOUS_PAGE, REVIEW_PANEL, SAVE, TOOLS,
    ZOOM, ZOOM_IN, ZOOM_OUT, colour_id, tool_id,
};
use crate::actions::PdfEffect;
use crate::markup::PdfMarkup;
use crate::model::{AnnoKind, Annotation, MarkupStyle, PtRect, Swatch, Tool};
use crate::{PdfApp, PdfViewerShell};

/// The reader on a two-page text PDF, launched as a person opens it.
fn reader() -> (tempfile::TempDir, TestApp<PdfViewerShell>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = text_pdf(directory.path(), 2);
    let mut app = launched(&path);
    app.harness().present_in_window_bar();
    app.harness().relayout();
    (directory, app)
}

fn action(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

/// Press `event` in the window's bar, as the desktop hands it to the app,
/// and let the reader run it.
fn press(app: &mut TestApp<PdfViewerShell>, event: ToolbarEvent) {
    app.harness().press_window_slot(event);
    app.settle();
}

fn reader_state<T>(app: &mut TestApp<PdfViewerShell>, read: impl FnOnce(&mut PdfApp) -> T) -> T {
    app.with_surface::<PdfApp, _>(read).expect("the reader")
}

fn bar(app: &mut TestApp<PdfViewerShell>) -> WindowToolbar {
    reader_state(app, |pdf| pdf.window_toolbar())
}

fn slot<'a>(toolbar: &'a WindowToolbar, id: &str) -> &'a ToolbarAction {
    toolbar
        .actions
        .iter()
        .find(|action| action.id == id)
        .unwrap_or_else(|| panic!("the bar has {id}: {toolbar:?}"))
}

/// The item of menu `menu` that is on.
fn chosen(toolbar: &WindowToolbar, menu: &str) -> Vec<String> {
    slot(toolbar, menu)
        .items
        .iter()
        .filter(|item| item.on)
        .map(|item| item.id.clone())
        .collect()
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

#[test]
#[serial(pdfium)]
fn the_reader_draws_no_toolbar_and_its_controls_are_in_the_window_bar() {
    let (_directory, mut app) = reader();
    let toolbar = bar(&mut app);
    assert_eq!(
        toolbar.sidebar,
        Some(true),
        "the pages are the bar's sidebar"
    );
    assert_eq!(
        toolbar
            .location
            .as_ref()
            .map(|location| location.label.as_str()),
        Some("text.pdf"),
        "the bar names the document"
    );
    let ids: Vec<&str> = toolbar
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            TOOLS,
            MARKER_COLOUR,
            REVIEW_PANEL,
            ZOOM,
            PREVIOUS_PAGE,
            NEXT_PAGE,
            OPEN,
            SAVE
        ]
    );
    assert_eq!(
        toolbar.row_actions(),
        3,
        "zoom, page stepping, Open and Save are in the menu"
    );
    let tools: Vec<String> = slot(&toolbar, TOOLS)
        .items
        .iter()
        .map(|item| item.id.clone())
        .collect();
    assert_eq!(tools, Tool::ALL.map(tool_id));
    assert_eq!(chosen(&toolbar, TOOLS), [tool_id(Tool::Select)]);
    let colours: Vec<String> = slot(&toolbar, MARKER_COLOUR)
        .items
        .iter()
        .map(|item| item.id.clone())
        .collect();
    assert_eq!(colours, Swatch::ALL.map(colour_id));
    let zoom: Vec<&str> = slot(&toolbar, ZOOM)
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(zoom, [ZOOM_IN, ZOOM_OUT, FIT_PAGE, FIT_WIDTH]);
    assert!(
        toolbar == toolbar.clone().bounded(),
        "the bar takes the declaration as it is"
    );

    let (rect, canvas, tree) = reader_state(&mut app, |pdf| {
        (
            pdf.rect,
            pdf.canvas_rect,
            ui::Widget::accessibility(pdf).expect("the reader's semantics"),
        )
    });
    assert!(
        (canvas.y0 - rect.y0).abs() < 1e-9,
        "the pages start at the top: the reader draws no row above them"
    );
    let mut drawn = Vec::new();
    labels(&tree, &mut drawn);
    for own in [
        "Open PDF",
        "Save annotated PDF",
        "Find in document",
        "Zoom in",
        "Zoom out",
        "Fit page or width",
        "Page thumbnails",
        "Marker colour",
        "pdf.open",
        "pdf.save",
        "pdf.search.open",
        "pdf.toggle-rail",
        "pdf.toggle-sidebar",
        "pdf.zoom-in",
        ui::widget::HISTORY_CONTROL_ID,
    ] {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the reader draws no {own} of its own: {drawn:?}"
        );
    }
    assert!(
        !drawn.iter().any(|label| label.starts_with("pdf.tool:")),
        "the reader draws no tool buttons of its own: {drawn:?}"
    );

    let empty = PdfApp::empty().window_toolbar();
    assert_eq!(
        empty
            .actions
            .iter()
            .map(|action| (action.id.as_str(), action.menu))
            .collect::<Vec<_>>(),
        [(OPEN, true)],
        "with no document open the bar's menu offers Open, where it always is"
    );
    assert_eq!(empty.sidebar, None, "and nothing to show beside a document");
}

#[test]
#[serial(pdfium)]
fn the_bar_runs_the_same_actions_as_the_published_ones() {
    let (_directory, mut app) = reader();

    // The pages: the bar's sidebar toggle, and Toggle page thumbnails.
    press(&mut app, ToolbarEvent::ToggleSidebar);
    assert!(!reader_state(&mut app, |pdf| pdf.show_rail));
    assert_eq!(bar(&mut app).sidebar, Some(false));
    app.effect(&PdfEffect::TogglePages)
        .expect("the pages toggle");
    assert!(reader_state(&mut app, |pdf| pdf.show_rail));

    // The review panel: the bar's action, and Toggle sidebar.
    let shown = reader_state(&mut app, |pdf| pdf.show_sidebar);
    press(&mut app, action(REVIEW_PANEL));
    assert_eq!(reader_state(&mut app, |pdf| pdf.show_sidebar), !shown);
    assert_eq!(slot(&bar(&mut app), REVIEW_PANEL).on, !shown);
    app.effect(&PdfEffect::ToggleSidebar)
        .expect("the review panel toggles");
    assert_eq!(reader_state(&mut app, |pdf| pdf.show_sidebar), shown);

    // The tools: an item of the bar's Tools menu, and Use tool.
    press(&mut app, action(&tool_id(Tool::Ink)));
    assert_eq!(reader_state(&mut app, |pdf| pdf.tool), Tool::Ink);
    let toolbar = bar(&mut app);
    assert_eq!(chosen(&toolbar, TOOLS), [tool_id(Tool::Ink)]);
    assert_eq!(slot(&toolbar, TOOLS).icon, ui::icons::Icon::Pencil.name());
    app.effect(&PdfEffect::UseTool { tool: Tool::Note })
        .expect("the note tool arms");
    assert_eq!(reader_state(&mut app, |pdf| pdf.tool), Tool::Note);

    // Zoom: the bar's Zoom in and Zoom out, and the published ones.
    let zoom = reader_state(&mut app, |pdf| pdf.zoom());
    press(&mut app, action(ZOOM_IN));
    let pressed = reader_state(&mut app, |pdf| pdf.zoom());
    assert!(pressed > zoom, "Zoom in zooms in: {zoom} to {pressed}");
    app.effect(&PdfEffect::ZoomIn).expect("zoom in");
    let invoked = reader_state(&mut app, |pdf| pdf.zoom());
    assert!(
        (invoked / pressed - pressed / zoom).abs() < 1e-9,
        "the bar's Zoom in is the published one"
    );
    press(&mut app, action(ZOOM_OUT));
    assert!((reader_state(&mut app, |pdf| pdf.zoom()) - pressed).abs() < 1e-9);

    // Fit: the bar's items, checked as the fit is, and the published ones.
    press(&mut app, action(FIT_PAGE));
    assert!(reader_state(&mut app, |pdf| pdf.fit == super::FitMode::Page));
    assert_eq!(chosen(&bar(&mut app), ZOOM), [FIT_PAGE]);
    app.effect(&PdfEffect::FitWidth).expect("fit width");
    assert_eq!(chosen(&bar(&mut app), ZOOM), [FIT_WIDTH]);
    press(&mut app, action(FIT_PAGE));
    let page_zoom = reader_state(&mut app, |pdf| pdf.zoom());
    app.effect(&PdfEffect::FitWidth).expect("fit width");
    app.effect(&PdfEffect::FitPage).expect("fit page");
    assert!((reader_state(&mut app, |pdf| pdf.zoom()) - page_zoom).abs() < 1e-9);

    // The marker colour: an item of the bar's menu, and Marker colour.
    press(&mut app, action(&colour_id(Swatch::Blue)));
    assert_eq!(reader_state(&mut app, |pdf| pdf.swatch), Swatch::Blue);
    assert_eq!(
        chosen(&bar(&mut app), MARKER_COLOUR),
        [colour_id(Swatch::Blue)]
    );
    app.effect(&PdfEffect::SetMarkerColour {
        colour: Swatch::Pink,
    })
    .expect("the marker colour is set");
    assert_eq!(reader_state(&mut app, |pdf| pdf.swatch), Swatch::Pink);

    // Save: offered while a PDF is open.
    assert!(slot(&bar(&mut app), SAVE).enabled);
}

#[test]
#[serial(pdfium)]
fn the_marker_colour_recolours_the_selected_mark_through_the_record() {
    let (_directory, mut app) = reader();
    let mark = Annotation::new(
        1,
        AnnoKind::Markup {
            style: MarkupStyle::Highlight,
            quads: vec![PtRect::new(56.0, 40.0, 200.0, 14.0)],
        },
        Swatch::Yellow,
        "me",
    );
    app.invoke(crate::actions::add_annotation_on(&mark, "source-1"))
        .expect("the mark is added");
    app.settle();
    app.surface_mut::<PdfApp>().selected_anno = Some(mark.id.clone());
    let colour_of = |app: &mut TestApp<PdfViewerShell>| {
        app.read_document(|doc| {
            PdfMarkup::of(doc)
                .list_annotations()
                .into_iter()
                .find(|stored| stored.id == mark.id)
                .map(|stored| stored.color)
        })
    };

    press(&mut app, action(&colour_id(Swatch::Green)));
    app.settle();
    assert_eq!(
        colour_of(&mut app),
        Some(Swatch::Green),
        "the bar recolours it"
    );

    app.effect(&PdfEffect::SetMarkerColour {
        colour: Swatch::Purple,
    })
    .expect("the marker colour is set");
    app.settle();
    assert_eq!(
        colour_of(&mut app),
        Some(Swatch::Purple),
        "so does the published action"
    );
}

/// Invoke `effect` on the document by its address, as a headless drive's
/// `invoke` step does: no coordinates, no shortcut.
fn drive_invoke(app: &mut TestApp<PdfViewerShell>, effect: &PdfEffect) {
    use ui::tilo_resource::{Effect as _, Resource as _};
    let address = effect.address_on(PdfMarkup::KIND);
    app.harness()
        .invoke_action(&address, &effect.arguments(), false, None)
        .unwrap_or_else(|error| panic!("{effect:?}: {error}"));
    app.settle();
}

#[test]
#[serial(pdfium)]
fn a_drive_of_a_scenario_zooms_and_fits_by_address() {
    let mut app = TestApp::<PdfViewerShell>::drive("search").expect("the sample opens");
    let zoom = reader_state(&mut app, |pdf| pdf.zoom());
    drive_invoke(&mut app, &PdfEffect::ZoomIn);
    let zoomed = reader_state(&mut app, |pdf| pdf.zoom());
    assert!(zoomed > zoom, "Zoom in zooms in: {zoom} to {zoomed}");
    drive_invoke(&mut app, &PdfEffect::ZoomOut);
    assert!((reader_state(&mut app, |pdf| pdf.zoom()) - zoom).abs() < 1e-9);
    drive_invoke(&mut app, &PdfEffect::FitPage);
    assert!(reader_state(&mut app, |pdf| pdf.fit == super::FitMode::Page));
    drive_invoke(&mut app, &PdfEffect::FitWidth);
    assert!(reader_state(&mut app, |pdf| pdf.fit == super::FitMode::Width));
}

#[test]
#[serial(pdfium)]
fn a_drive_of_every_scenario_reaches_every_published_action() {
    for scenario in crate::scenarios::SCENARIOS {
        let mut app = TestApp::<PdfViewerShell>::drive(scenario)
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        assert_eq!(
            ui::framework::testing::unreachable_actions(&mut app),
            Vec::<String>::new(),
            "{scenario}: published actions a drive cannot invoke"
        );
    }
}

#[test]
fn history_is_the_window_bars_control() {
    let mut app = TestApp::<PdfViewerShell>::capture(None).expect("the sample opens");
    app.harness().present_in_window_bar();
    app.harness().relayout();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Closed),
        "the window's bar offers History"
    );
    app.harness().press_window_history().expect("History opens");
    app.settle();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Open)
    );
}

#[test]
fn ctrl_f_and_find_open_the_window_search() {
    let mut app = crate::scenarios::sample_reader();
    let mut state = crate::scenarios::sample_state();
    app.sync_settled(&mut state.doc, &state.root);
    let _ = ui::window_toolbar::take_search_request();
    let mut ctrl = winit::keyboard::ModifiersState::empty();
    ctrl.set(winit::keyboard::ModifiersState::CONTROL, true);
    ui::testing::Dispatch::new()
        .modifiers(winit::event::Modifiers::from(ctrl))
        .doc(&mut state.doc)
        .path(&state.root)
        .send(
            &mut app,
            &ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
                winit::keyboard::Key::Character("f".into()),
                Some("f".into()),
                winit::event::ElementState::Pressed,
            )),
        );
    assert_eq!(
        ui::window_toolbar::take_search_request(),
        Some(SearchOpening::Find),
        "Ctrl+F opens the window's search"
    );
    assert!(app.show(&PdfEffect::Find));
    assert_eq!(
        ui::window_toolbar::take_search_request(),
        Some(SearchOpening::Find),
        "Find in document opens the window's search"
    );
}

#[test]
#[serial(pdfium)]
fn find_through_the_window_bar_finds_steps_and_shows_the_current_match() {
    let (_directory, mut app) = reader();
    // Every line of both pages says "quick brown".
    let lines = 2 * 40;
    let deadline = tilo_test_env::hang_deadline();
    let found = loop {
        let found = app
            .harness()
            .search_in_window(SearchOpening::Find, "quick brown");
        if found.is_some_and(|(_, total)| total == lines) {
            break found;
        }
        assert!(
            ui::time::now() < deadline,
            "the pages' text never arrived: {found:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.settle();
    };
    assert_eq!(found, Some((1, lines)), "the bar counts every match");

    let stepped = app.harness().step_window_search(true);
    assert_eq!(stepped, Some((2, lines)), "the bar steps to the next match");
    let (current, page, bands, query) = reader_state(&mut app, |pdf| {
        (
            pdf.active_match,
            pdf.current_page,
            pdf.found_bands(1),
            pdf.query().to_owned(),
        )
    });
    assert_eq!(current, 1, "the reader's current match is the bar's");
    assert_eq!(page, 1, "the current match is in view");
    assert_eq!(query, "quick brown");
    assert_eq!(bands.len(), 40, "every match on the page is painted");
    let emphasised: Vec<usize> = bands
        .iter()
        .enumerate()
        .filter(|(_, (_, current))| *current)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(emphasised, [1], "only the current match paints emphasised");

    let back = app.harness().step_window_search(false);
    assert_eq!(back, Some((1, lines)));
    assert_eq!(reader_state(&mut app, |pdf| pdf.active_match), 0);

    app.harness()
        .perform_named("window-search-close", &ui::a11y::Action::Activate)
        .expect("the search closes");
    app.settle();
    let (finding, painted) = reader_state(&mut app, |pdf| (pdf.finding(), pdf.found_bands(1)));
    assert!(
        !finding && painted.is_empty(),
        "closing the search clears the matches"
    );
}

/// An action that goes to a search result (an agent's) hands its find to
/// the window's search: the reader asks for it, and the field shows the
/// query with the match it went to current.
#[test]
#[serial(pdfium)]
fn going_to_a_search_result_shows_its_query_in_the_window_search() {
    let (_directory, mut app) = reader();
    let lines = 2 * 40;
    let deadline = tilo_test_env::hang_deadline();
    while reader_state(&mut app, |pdf| {
        use ui::widget::Findable as _;
        let total = pdf
            .find("quick brown", ui::widget::FindOptions::default())
            .len();
        pdf.clear_find();
        total
    }) < lines
    {
        assert!(ui::time::now() < deadline, "the pages' text never arrived");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.settle();
    }
    let _ = ui::window_toolbar::take_search_request();

    // The search result row's action, as an agent runs it.
    let went = reader_state(&mut app, |pdf| {
        pdf.show(&PdfEffect::GoToMatch {
            query: "quick brown".into(),
            whole_word: false,
            case_sensitive: false,
            index: 41,
            page: 2,
        })
    });
    assert!(went, "the reader goes to the result");
    assert_eq!(
        ui::window_toolbar::take_search_request(),
        Some(SearchOpening::Find),
        "the reader asks the window to open its search"
    );
    app.harness()
        .perform_named("window-search-open", &ui::a11y::Action::Activate)
        .expect("the window opens its search");
    assert_eq!(app.harness().window_search_query(), "quick brown");
    assert_eq!(
        app.harness().window_search_position(),
        Some((42, lines)),
        "the match the reader went to is current"
    );
    assert_eq!(reader_state(&mut app, |pdf| pdf.active_match), 41);
    let stepped = app.harness().step_window_search(true);
    assert_eq!(stepped, Some((43, lines)), "the bar steps on from there");
}

/// A match's highlight is the span of exactly its glyphs: it ends at the
/// advance of the match's last glyph, short of the glyph after it ("quic"
/// in "quick" stops before the "k").
#[test]
#[serial(pdfium)]
fn a_match_highlight_ends_at_its_last_glyph_advance() {
    let (_directory, mut app) = reader();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = app.harness().search_in_window(SearchOpening::Find, "quic");
        if found.is_some() && reader_state(&mut app, |pdf| !pdf.found_bands(1).is_empty()) {
            break;
        }
        assert!(ui::time::now() < deadline, "the page's text never arrived");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.settle();
    }
    let (band, last, next) = reader_state(&mut app, |pdf| {
        let found = &pdf.raster_matches[0];
        let layer = pdf.raster_text.get(&found.page).expect("the page's text");
        let end = found.char_start + found.char_len;
        let one = |at: usize| layer.bands_for_range(at, at + 1)[0];
        (pdf.found_bands(found.page)[0].0, one(end - 1), one(end))
    });
    let right = |rect: PtRect| rect.x + rect.w;
    assert!(
        (right(band) - right(last)).abs() < 1e-6,
        "the highlight ends at the last glyph's advance: {band:?} against {last:?}"
    );
    assert!(
        right(band) <= next.x + 1e-6,
        "the highlight stops short of the glyph after the match: {band:?} against {next:?}"
    );
}

/// A host whose first Save fails where it keeps the file and whose next
/// one saves there; every other chooser is refused.
struct SaveFailsOnce {
    asked: std::sync::Arc<std::sync::Mutex<usize>>,
}

impl ui::host::FilePicker for SaveFailsOnce {
    fn pick(&self, _: ui::portal::PickRequest) -> ui::portal::PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::PortalOutcome::Failed("no chooser here".into()));
        rx
    }
    fn pick_multiple(&self, _: ui::portal::PickRequest) -> ui::portal::MultiChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::MultiOutcome::Failed("no chooser here".into()));
        rx
    }
    fn open_file(&self, _: ui::portal::PickRequest) -> ui::portal::OpenFileChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::OpenFileOutcome::Failed(
            "no chooser here".into(),
        ));
        rx
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::LaunchOutcome::Failed("no chooser here".into()));
        rx
    }
    fn save_destination(&self, request: ui::portal::PickRequest) -> ui::portal::DestinationChannel {
        let first = {
            let mut asked = self.asked.lock().unwrap();
            *asked = asked.saturating_add(1);
            *asked == 1
        };
        let name = request.suggested_name().unwrap_or("download").to_owned();
        ui::portal::DestinationChannel::answered(ui::portal::DestinationOutcome::Chosen(
            ui::portal::SaveDestination::kept(name, move |_| {
                let (tx, rx) = std::sync::mpsc::channel();
                let _ = tx.send(if first {
                    ui::portal::SaveOutcome::Failed("the disk is full".into())
                } else {
                    ui::portal::SaveOutcome::Saved
                });
                rx
            }),
        ))
    }
}

fn note(app: &mut TestApp<PdfViewerShell>) -> String {
    reader_state(app, |pdf| {
        pdf.status_note.as_deref().unwrap_or_default().to_owned()
    })
}

fn wait_for_note(app: &mut TestApp<PdfViewerShell>, says: &str) {
    tilo_test_env::wait_until(format!("the bar to say {says}"), || {
        app.settle();
        note(app) == says
    });
}

/// The name the reader suggests for its saved copy.
fn suggested_name(app: &mut TestApp<PdfViewerShell>) -> String {
    reader_state(app, |pdf| pdf.suggested_save_name())
}

/// A save that failed carries Try again: pressed by pointer or through its
/// semantic action, wide or compact, it asks where to save again and saves.
#[test]
#[serial(pdfium)]
fn a_failed_saves_try_again_saves_again() {
    for (width, semantic) in [
        (1280.0, false),
        (1280.0, true),
        (420.0, false),
        (420.0, true),
    ] {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(0));
        let services =
            ui::host::HostServices::null().with_files(std::sync::Arc::new(SaveFailsOnce {
                asked: std::sync::Arc::clone(&asked),
            }));
        let _host = ui::host::enter(&services);
        let (_directory, mut app) = reader();
        let name = suggested_name(&mut app);
        press(&mut app, action(SAVE));
        wait_for_note(&mut app, &crate::app::save_failed_note(&name));
        let buttons: Vec<String> = app
            .harness()
            .window_bar_notice(width)
            .expect("the failure is in the bar")
            .buttons
            .into_iter()
            .map(|button| button.name)
            .collect();
        assert_eq!(buttons, ["Try again"], "at {width} px");
        let harness = app.harness();
        let pressed = if semantic {
            harness.invoke_window_notice_action(width, 0)
        } else {
            harness.click_window_notice_action(width, 0)
        };
        pressed.expect("Try again stands in the bar");
        app.settle();
        wait_for_note(&mut app, &crate::app::saved_note(&name));
        assert_eq!(
            *asked.lock().unwrap(),
            2,
            "Try again asked where to save again"
        );
        let saved = app
            .harness()
            .window_bar_notice(width)
            .expect("the save is said");
        assert!(saved.buttons.is_empty(), "a saved PDF only informs");
    }
}

/// A host whose Open chooser answers with each of `files` in turn; every
/// other chooser is refused.
struct ChoosesInTurn {
    files: std::sync::Mutex<std::collections::VecDeque<std::path::PathBuf>>,
}

impl ui::host::FilePicker for ChoosesInTurn {
    fn pick(&self, _: ui::portal::PickRequest) -> ui::portal::PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let next = self.files.lock().unwrap().pop_front();
        let _ = tx.send(match next {
            Some(path) => ui::portal::PortalOutcome::Opened(path),
            None => ui::portal::PortalOutcome::Cancelled,
        });
        rx
    }
    fn pick_multiple(&self, _: ui::portal::PickRequest) -> ui::portal::MultiChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::MultiOutcome::Failed(
            "one at a time here".into(),
        ));
        rx
    }
    fn open_file(&self, _: ui::portal::PickRequest) -> ui::portal::OpenFileChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::OpenFileOutcome::Failed(
            "no descriptors here".into(),
        ));
        rx
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::LaunchOutcome::Failed("no launcher here".into()));
        rx
    }
    fn save_destination(&self, _: ui::portal::PickRequest) -> ui::portal::DestinationChannel {
        ui::portal::DestinationChannel::answered(ui::portal::DestinationOutcome::Failed(
            "no saving here".into(),
        ))
    }
}

/// Pages that could not be imported carry Try again: pressed by pointer or
/// through its semantic action, wide or compact, it asks which pages to
/// import again and imports them.
#[test]
#[serial(pdfium)]
fn a_failed_imports_try_again_imports_again() {
    for (width, semantic) in [
        (1280.0, false),
        (1280.0, true),
        (420.0, false),
        (420.0, true),
    ] {
        let (directory, mut app) = reader();
        let broken = directory.path().join("broken.pdf");
        std::fs::write(&broken, b"not a PDF at all").unwrap();
        let swatch = directory.path().join("swatch.png");
        image::RgbaImage::from_pixel(96, 64, image::Rgba([210, 60, 60, 255]))
            .save(&swatch)
            .unwrap();
        let services =
            ui::host::HostServices::null().with_files(std::sync::Arc::new(ChoosesInTurn {
                files: std::sync::Mutex::new([broken, swatch].into()),
            }));
        let _host = ui::host::enter(&services);
        let pages = reader_state(&mut app, |pdf| pdf.doc.page_count());
        reader_state(&mut app, PdfApp::request_import_via_portal);
        wait_for_note(&mut app, &crate::t("pdf-note-import-failed"));
        let buttons: Vec<String> = app
            .harness()
            .window_bar_notice(width)
            .expect("the failure is in the bar")
            .buttons
            .into_iter()
            .map(|button| button.name)
            .collect();
        assert_eq!(buttons, ["Try again"], "at {width} px");
        let harness = app.harness();
        let pressed = if semantic {
            harness.invoke_window_notice_action(width, 0)
        } else {
            harness.click_window_notice_action(width, 0)
        };
        pressed.expect("Try again stands in the bar");
        app.settle();
        wait_for_note(
            &mut app,
            &crate::t_args("pdf-note-imported", &[("count", 1_usize.into())]),
        );
        assert_eq!(
            reader_state(&mut app, |pdf| pdf.doc.page_count()),
            pages.saturating_add(1),
            "Try again imported the chosen page"
        );
    }
}
