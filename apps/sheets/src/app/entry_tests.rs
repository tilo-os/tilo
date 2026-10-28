//! Entering data the way spreadsheet users expect: typing over a selected
//! cell replaces it whichever way the text arrives, the arrow keys finish a
//! typed entry and move on, `AutoSum` adds up only the numbers above, and an
//! error cell says why it is one.

use super::tests::{char_event, doc_cell_raw, feed, named_event, seeded_app};
use super::*;
use crate::grid::GridCmd;
use automerge::AutoCommit;
use ui::widget::{ControlSize, SegmentedFace};
use winit::event::Ime;
use winit::keyboard::NamedKey;

fn shift_mods() -> Modifiers {
    let mut s = winit::keyboard::ModifiersState::empty();
    s.set(winit::keyboard::ModifiersState::SHIFT, true);
    Modifiers::from(s)
}

fn press(app: &mut SheetsApp, doc: &mut AutoCommit, key: NamedKey) {
    assert!(feed(app, doc, &named_event(key), Modifiers::default()));
}

fn type_chars(app: &mut SheetsApp, doc: &mut AutoCommit, text: &str) {
    for c in text.chars() {
        let s = c.to_string();
        assert!(feed(app, doc, &char_event(&s), Modifiers::default()));
    }
}

fn set_cells(app: &mut SheetsApp, doc: &mut AutoCommit, cells: &[(Addr, &str)]) {
    let mut cmds: Vec<GridCmd> = cells
        .iter()
        .map(|(addr, raw)| GridCmd::SetCell {
            addr: *addr,
            raw: (*raw).to_owned(),
        })
        .collect();
    cmds.push(GridCmd::Commit);
    app.apply_cmds(&mut Edits::lend(doc), cmds);
}

/// Text from the page's text field (an input-method commit) on a selected
/// cell starts an entry that replaces the cell, as a typed key does; it is
/// not dropped because no edit was open yet.
#[test]
fn text_committed_on_a_selected_cell_replaces_it() {
    let (mut app, mut doc) = seeded_app();
    let a21 = (0, 20);
    set_cells(&mut app, &mut doc, &[(a21, "old")]);
    app.grid.select_only(a21);
    assert!(!app.grid.is_editing());

    let commit = WindowEvent::Ime(Ime::Commit("Item".into()));
    assert!(feed(&mut app, &mut doc, &commit, Modifiers::default()));
    assert!(app.grid.is_editing(), "the commit opened an entry");
    assert_eq!(
        app.grid.active_display_raw(),
        "Item",
        "and replaced the cell"
    );
    press(&mut app, &mut doc, NamedKey::Enter);
    assert_eq!(doc_cell_raw(&doc, 0, a21), "Item");
    assert_eq!(app.grid.active(), (0, 21), "Enter moved down");

    // The next cell takes the next commit the same way, so a column of typed
    // values all lands.
    let commit = WindowEvent::Ime(Ime::Commit("Cost".into()));
    assert!(feed(&mut app, &mut doc, &commit, Modifiers::default()));
    press(&mut app, &mut doc, NamedKey::Enter);
    assert_eq!(doc_cell_raw(&doc, 0, (0, 21)), "Cost");

    // A composition on a selected cell starts the entry too.
    let preedit = WindowEvent::Ime(Ime::Preedit("にほ".into(), Some((3, 3))));
    assert!(feed(&mut app, &mut doc, &preedit, Modifiers::default()));
    assert!(app.grid.is_editing());
    assert_eq!(
        app.grid.active_display_raw(),
        "",
        "the composition is not typed yet"
    );
}

/// After typing over a cell, the arrow keys commit and move like Enter and
/// Tab do; after F2 they move the caret within the text. Shift+Tab and
/// Shift+Enter commit and move back.
#[test]
fn arrows_finish_a_typed_entry_but_move_the_caret_in_an_edit() {
    let (mut app, mut doc) = seeded_app();
    app.grid.select_only((0, 20)); // A21
    type_chars(&mut app, &mut doc, "Item");
    press(&mut app, &mut doc, NamedKey::ArrowRight);
    assert!(!app.grid.is_editing(), "the arrow finished the entry");
    assert_eq!(doc_cell_raw(&doc, 0, (0, 20)), "Item");
    assert_eq!(app.grid.active(), (1, 20), "and moved right");

    for (key, to) in [
        (NamedKey::ArrowDown, (1, 21)),
        (NamedKey::ArrowLeft, (0, 21)),
        (NamedKey::ArrowUp, (0, 20)),
    ] {
        type_chars(&mut app, &mut doc, "7");
        press(&mut app, &mut doc, key);
        assert!(!app.grid.is_editing());
        assert_eq!(app.grid.active(), to, "{key:?} committed and moved");
    }
    assert_eq!(doc_cell_raw(&doc, 0, (0, 21)), "7");

    // F2 opens the cell ("Item", caret at its end) for caret editing:
    // arrows stay inside the text.
    assert_eq!(app.grid.active_display_raw(), "Item");
    press(&mut app, &mut doc, NamedKey::F2);
    press(&mut app, &mut doc, NamedKey::ArrowLeft);
    assert!(
        app.grid.is_editing(),
        "F2 editing keeps the arrows in the text"
    );
    assert_eq!(app.grid.edit_caret(), Some(3));
    press(&mut app, &mut doc, NamedKey::Escape);

    // A typed formula keeps the caret moving: it is composed piecewise.
    type_chars(&mut app, &mut doc, "=1");
    press(&mut app, &mut doc, NamedKey::ArrowLeft);
    assert!(app.grid.is_editing());
    press(&mut app, &mut doc, NamedKey::Escape);

    app.grid.select_only((2, 22)); // C23
    type_chars(&mut app, &mut doc, "x");
    assert!(feed(
        &mut app,
        &mut doc,
        &named_event(NamedKey::Tab),
        shift_mods()
    ));
    assert_eq!(doc_cell_raw(&doc, 0, (2, 22)), "x");
    assert_eq!(app.grid.active(), (1, 22));
    type_chars(&mut app, &mut doc, "y");
    assert!(feed(
        &mut app,
        &mut doc,
        &named_event(NamedKey::Enter),
        shift_mods()
    ));
    assert_eq!(doc_cell_raw(&doc, 0, (1, 22)), "y");
    assert_eq!(app.grid.active(), (1, 21));
}

/// `AutoSum` under a column with a total and error cells sums only the values
/// above the total: errors directly above are stepped over and the existing
/// total ends the run instead of being counted again.
#[test]
fn auto_sum_skips_errors_and_stops_at_an_existing_total() {
    let (mut app, mut doc) = seeded_app();
    let col = 7; // H, empty in the sample
    set_cells(
        &mut app,
        &mut doc,
        &[
            ((col, 20), "3.5"),
            ((col, 21), "4.25"),
            ((col, 22), "=SUM(H21:H22)"),
            ((col, 23), "=1/0"),
            ((col, 24), "=FOO(1)"),
        ],
    );
    // Under the errors: they are stepped over, the total stops the run, and
    // nothing numeric is left to add.
    app.grid.select_only((col, 25));
    app.auto_sum(&mut Edits::lend(&mut doc));
    assert_eq!(doc_cell_raw(&doc, 0, (col, 25)), "");
    assert_eq!(app.status, "No numeric run above");

    // Numbers between the total and the errors are summed without either.
    set_cells(
        &mut app,
        &mut doc,
        &[((col, 23), "10"), ((col, 24), "=2/0")],
    );
    app.grid.select_only((col, 25));
    app.auto_sum(&mut Edits::lend(&mut doc));
    assert_eq!(doc_cell_raw(&doc, 0, (col, 25)), "=SUM(H24:H24)");

    // A plain run with no total above sums it all.
    set_cells(
        &mut app,
        &mut doc,
        &[((col + 1, 20), "1"), ((col + 1, 21), "2")],
    );
    app.grid.select_only((col + 1, 22));
    app.auto_sum(&mut Edits::lend(&mut doc));
    assert_eq!(doc_cell_raw(&doc, 0, (col + 1, 22)), "=SUM(I21:I22)");
}

/// A formula that can't be read says what is wrong when it is entered and
/// whenever its cell is selected, instead of a bare `#ERROR!`.
#[test]
fn an_error_cell_explains_itself() {
    let (mut app, mut doc) = seeded_app();
    app.grid.select_only((7, 20)); // H21
    type_chars(&mut app, &mut doc, "=B2*");
    press(&mut app, &mut doc, NamedKey::Enter);
    assert!(
        app.status.contains("H21") && app.status.contains("ends with an operator"),
        "entering it explains the error: {}",
        app.status
    );

    app.status = "Ready".into();
    app.grid.select_only((7, 20));
    let bubble = app.status_label().unwrap_or_default();
    assert!(bubble.contains("ends with an operator"), "bubble: {bubble}");

    for (raw, why) in [
        ("=SUM(1", "not closed"),
        ("=1/0", "Division by zero"),
        ("=NOPE(1)", "Unknown function"),
    ] {
        set_cells(&mut app, &mut doc, &[((7, 21), raw)]);
        app.grid.select_only((7, 21));
        let bubble = app.status_label().unwrap_or_default();
        assert!(bubble.contains(why), "{raw}: {bubble}");
    }

    // A plain value says nothing of errors.
    set_cells(&mut app, &mut doc, &[((7, 22), "5")]);
    assert!(app.error_explanation((7, 22)).is_none());
}

/// A selected formula cell traces its references with a wash only; the
/// outline (which reads as a second selected cell) is kept for editing.
#[test]
fn reference_outlines_only_while_editing() {
    let (mut app, mut doc) = seeded_app();
    set_cells(&mut app, &mut doc, &[((7, 20), "=B2*2")]);
    app.grid.select_only((7, 20));
    assert!(app.grid.highlight_formula_body().is_some());
    assert!(!app.grid.reference_outlines_visible());
    press(&mut app, &mut doc, NamedKey::F2);
    assert!(app.grid.reference_outlines_visible());
}

/// The settings sheet's date strip gives "Language" the room it needs rather
/// than cutting it to an equal share of a narrow sheet.
#[test]
fn settings_strip_labels_are_not_cut() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let natural =
        SegmentedFace::intrinsic_width(&["Language"], ControlSize::Small, &mut tr, &theme);
    let rects = SheetsApp::settings_strip_rects(&mut tr, &theme, 480.0);
    let (lang, us) = (rects[1], rects[3]);
    assert!(
        lang.width() + 0.5 >= natural,
        "Language gets {} of the {natural} it needs",
        lang.width()
    );
    assert!(lang.width() > us.width());
}

/// In the running app the sheet declares its slots, so its History is the
/// window bar's too: no History band stands above the sheet.
#[test]
fn the_running_app_has_no_history_band_above_the_sheet() {
    let mut app = ui::framework::testing::TestApp::<crate::resource::SheetsShell>::new().unwrap();
    app.harness().resize((1200, 800)).unwrap();
    app.settle();
    assert!(
        ui::window_toolbar::declared().is_some(),
        "the sheet declares its slots"
    );
    let top = app
        .with_surface::<SheetsApp, _>(|sheets| sheets.rect.y0)
        .unwrap();
    assert!(
        top.abs() < 0.5,
        "the sheet starts at the window's top, not under a band: {top}"
    );
}

/// In the running app Tab belongs to the grid: typing then Tab commits the
/// cell and moves right, rather than the host moving focus to the next
/// control (the window's title, the shell). An open settings sheet hands
/// Tab back to the focus order.
#[test]
fn tab_commits_and_moves_right_in_the_running_app() {
    let mut app = ui::framework::testing::TestApp::<crate::resource::SheetsShell>::new().unwrap();
    app.harness().resize((1200, 800)).unwrap();
    app.settle();
    let body = app
        .with_surface::<SheetsApp, _>(|sheets| sheets.rect.center())
        .unwrap();
    app.harness().click_at(body.x, body.y);
    app.settle();
    let start = app
        .with_surface::<SheetsApp, _>(|sheets| sheets.grid.active())
        .unwrap();
    app.harness().type_text("Item");
    app.harness().key("Tab");
    app.settle();
    let (active, raw) = app
        .with_surface::<SheetsApp, _>(|sheets| {
            (sheets.grid.active(), sheets.grid.cell_raw_at(start))
        })
        .unwrap();
    assert_eq!(raw, "Item", "Tab committed the typed cell");
    assert_eq!(active, (start.0 + 1, start.1), "and moved right");

    let wants = app
        .with_surface::<SheetsApp, _>(|sheets| {
            let grid = sheets.wants_tab_input();
            sheets.settings_panel = SettingsPanel::Document;
            (grid, sheets.wants_tab_input())
        })
        .unwrap();
    assert_eq!(wants, (true, false));
}

/// Sheets paints its regions as parts, and reports as damage the regions
/// whose drawing changed: a typed character repaints the editor and the
/// formula bar, a committed cell its rows and marks, a resize the window.
#[test]
fn a_typed_character_damages_its_cell_and_formula_bar_and_a_resize_the_window() {
    let (w, h) = (1200.0, 800.0);
    let (mut app, mut doc, mut tr, theme) = super::tests::measured_app_with_doc(w, h, 1.0);
    let presence = PresenceCtx::default();
    // One text renderer across frames, as a host keeps: its fonts are the
    // same fonts from frame to frame.
    let mut frame = |app: &mut SheetsApp, w: f64, h: f64| {
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.relayout();
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let mut damage = Vec::new();
        app.collect_damage(&mut damage);
        app.mark_drawn();
        damage
    };
    let b3 = (1, 2);
    app.grid.select_only(b3);
    type_chars(&mut app, &mut doc, "hello");
    assert!(app.grid.is_editing());
    for _ in 0..3 {
        frame(&mut app, w, h);
    }
    let still = frame(&mut app, w, h);
    assert!(
        still.is_empty(),
        "an unchanged frame damages nothing: {still:?}"
    );

    type_chars(&mut app, &mut doc, "!");
    let damage = frame(&mut app, w, h);
    let row = app.grid.cell_screen_rect(b3.0, b3.1);
    let body = app.grid.body_rect();
    let editor_row = Rect::new(body.x0, row.y0, body.x1, row.y1).inflate(0.0, 4.0);
    let formula_bar = app
        .fx_rect
        .union(app.formula_field_rect)
        .union(app.name_box_rect)
        .inflate(4.0, 4.0);
    assert!(!damage.is_empty(), "the typed character is repainted");
    for rect in &damage {
        assert!(
            editor_row.union(*rect) == editor_row || formula_bar.union(*rect) == formula_bar,
            "{rect:?} is outside the edited row {editor_row:?} and the formula bar {formula_bar:?}"
        );
    }

    press(&mut app, &mut doc, NamedKey::Enter);
    let damage = frame(&mut app, w, h);
    let window = Rect::new(0.0, 0.0, w, h);
    assert!(
        damage.iter().any(|rect| rect.union(row) == *rect),
        "the committed cell is repainted: {damage:?}"
    );
    let area: f64 = damage.iter().map(Rect::area).sum();
    assert!(
        area < window.area() / 4.0,
        "a committed cell repaints its rows and marks, not the window: {damage:?}"
    );

    let wider = Rect::new(0.0, 0.0, w + 100.0, h);
    let damage = frame(&mut app, w + 100.0, h);
    assert!(
        damage.iter().any(|rect| rect.union(wider) == *rect),
        "a resize repaints the window: {damage:?}"
    );
}
