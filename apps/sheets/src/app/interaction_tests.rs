//! The painted chrome answers the pointer and the keyboard: the command bar
//! opens menus whose rows act on the sheet, Ctrl+B and the Bold command share
//! one path, the settings sheet applies a choice, and a tab's colour is picked
//! from its popover. Each case drives the production `handle_event` over the
//! rects the last paint left behind.

use super::tests::{char_event, ctrl_mods, feed, measured_app_with_doc, paint_frame};
use super::*;
use crate::model::FormatCategory;
use automerge::AutoCommit;

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

/// Move the pointer to `p` and press and release the left button there.
fn click(app: &mut SheetsApp, doc: &mut AutoCommit, p: Point) {
    let moved = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    };
    feed(app, doc, &moved, Modifiers::default());
    feed(
        app,
        doc,
        &mouse(ElementState::Pressed),
        Modifiers::default(),
    );
    feed(
        app,
        doc,
        &mouse(ElementState::Released),
        Modifiers::default(),
    );
}

/// A seeded app, laid out and painted once at 1280 × 800.
fn painted() -> (SheetsApp, AutoCommit, TextRenderer, Theme) {
    let (mut app, doc, mut tr, theme) = measured_app_with_doc(1280.0, 800.0, 1.0);
    app.sync(&doc, &automerge::ROOT);
    paint_frame(&mut app, &mut tr, &theme);
    (app, doc, tr, theme)
}

/// Click the bar's command at `index` (see [`chrome::bar`]).
fn click_command(app: &mut SheetsApp, doc: &mut AutoCommit, index: usize) {
    let centre = app
        .toolbar
        .item_rect(index)
        .map(|rect| rect.center())
        .expect("the command is on the bar");
    click(app, doc, centre);
}

/// The command whose own menu is open, if any.
fn shown_menu(app: &SheetsApp) -> Option<usize> {
    app.toolbar.shown_menu().and_then(|(index, _)| index)
}

/// The commands, the menus and the sheet tabs act on pointer and keyboard.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn painted_chrome_answers_pointer_and_keyboard() {
    // Case `format_menu_applies_the_clicked_format`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        click_command(&mut app, &mut doc, chrome::bar::FORMAT);
        assert_eq!(shown_menu(&app), Some(chrome::bar::FORMAT));
        paint_frame(&mut app, &mut tr, &theme);
        let (_, menu) = app.toolbar.shown_menu().expect("the Format menu");
        let percent = menu
            .items()
            .iter()
            .position(|item| item.label() == Some("Percent"))
            .expect("a Percent row");
        let row = menu.item_rect(percent).expect("the row is laid out");
        click(&mut app, &mut doc, row.center());
        assert_eq!(shown_menu(&app), None, "choosing a row closes the menu");
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(
            app.current_active_format.category(),
            FormatCategory::Percent,
            "the active cell took the format"
        );
    }

    // Case `an_opener_opens_and_closes_its_menu`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        click_command(&mut app, &mut doc, chrome::bar::FILE);
        assert_eq!(shown_menu(&app), Some(chrome::bar::FILE));
        paint_frame(&mut app, &mut tr, &theme);
        click_command(&mut app, &mut doc, chrome::bar::FILE);
        assert_eq!(shown_menu(&app), None, "the same opener closes its menu");
        paint_frame(&mut app, &mut tr, &theme);
        click_command(&mut app, &mut doc, chrome::bar::FORMAT);
        assert_eq!(shown_menu(&app), Some(chrome::bar::FORMAT));
    }

    // Case `bold_command_and_ctrl_b_share_one_path`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        app.grid.select_only((1, 1));
        app.sync(&doc, &automerge::ROOT);
        assert!(!app.active_cell_bold);
        click_command(&mut app, &mut doc, chrome::bar::BOLD);
        app.sync(&doc, &automerge::ROOT);
        assert!(app.active_cell_bold, "the Bold command bolds the cell");
        paint_frame(&mut app, &mut tr, &theme);
        let bold = Widget::accessibility(&app.toolbar)
            .and_then(|bar| {
                bar.children
                    .into_iter()
                    .find(|node| node.name.as_deref() == Some("Bold"))
            })
            .expect("the Bold toggle");
        assert_eq!(bold.state.checked, Some(true), "the toggle reads as on");
        assert!(feed(&mut app, &mut doc, &char_event("b"), ctrl_mods()));
        app.sync(&doc, &automerge::ROOT);
        assert!(!app.active_cell_bold, "Ctrl+B takes it off again");
    }

    // Case `settings_sheet_applies_a_choice_and_closes_on_done`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        click_command(&mut app, &mut doc, chrome::bar::SETTINGS);
        assert_eq!(app.settings_panel, SettingsPanel::Document);
        paint_frame(&mut app, &mut tr, &theme);
        let eur = app
            .settings_sheet
            .as_ref()
            .and_then(|sheet| {
                sheet
                    .options
                    .iter()
                    .find(|option| option.kind == SettingKind::Currency(Some(Currency::EUR)))
            })
            .map(|option| option.rect.center())
            .expect("the EUR option is on the sheet");
        click(&mut app, &mut doc, eur);
        assert_eq!(app.doc_settings.currency, Some(Currency::EUR));
        assert_eq!(
            app.settings_panel,
            SettingsPanel::Document,
            "choosing keeps the sheet open"
        );
        paint_frame(&mut app, &mut tr, &theme);
        let done = app
            .settings_sheet
            .as_ref()
            .and_then(|sheet| sheet.modal.actions.first().copied())
            .expect("the Done action");
        click(&mut app, &mut doc, done.center());
        assert_eq!(app.settings_panel, SettingsPanel::None, "Done closes it");
    }

    // Case `tab_colour_is_picked_from_its_popover`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        app.open_tab_menu(0);
        paint_frame(&mut app, &mut tr, &theme);
        let pick = app
            .tab_menu_rows
            .iter()
            .position(|row| matches!(row, Some(overlays::TabMenuRow::PickColour)))
            .expect("a colour row");
        let row = app.tab_menu.item_rect(pick).expect("the row is laid out");
        click(&mut app, &mut doc, row.center());
        assert!(app.tab_colour.is_open(), "the row opens the colour popover");
        paint_frame(&mut app, &mut tr, &theme);
        // The first swatch: the popover's padding, the picker's own and half a
        // pointer target in.
        let panel = app.tab_colour.rect();
        let sp = &theme.tokens.spacing;
        let first = Point::new(
            panel.x0
                + f64::from(sp.s4)
                + f64::from(sp.s1)
                + f64::from(theme.tokens.metrics.min_pointer_target) / 2.0,
            panel.y0
                + f64::from(sp.s4)
                + f64::from(sp.s1)
                + f64::from(theme.tokens.metrics.min_pointer_target) / 2.0,
        );
        click(&mut app, &mut doc, first);
        assert!(!app.tab_colour.is_open(), "choosing closes the popover");
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(
            app.sheet_colors.first().copied().flatten(),
            Some(0),
            "the sheet took the first colour"
        );
    }
}

/// Menus and popovers stand clear of the window's edges and of what opened
/// them, and the tab editor never covers a neighbour.
#[test]
fn floating_surfaces_keep_clear_of_edges_and_openers() {
    // Case `the_more_menu_keeps_off_the_window_edge`.
    // A window narrower than the bar's own width moves the commands that do
    // not fit into the More menu, which opens under its button and stays
    // inside the window with room to spare.
    {
        let (mut probe, _doc, mut tr, theme) = measured_app_with_doc(1280.0, 600.0, 1.0);
        let whole = probe.toolbar.intrinsic_size(&mut tr, &theme).width;
        // Two thirds of the bar's width leaves File and Format in place with
        // the rest in More, whatever the commands measure at.
        let width = whole * 2.0 / 3.0 + 2.0 * probe.metrics.s3;
        let (mut app, doc, mut tr, theme) = measured_app_with_doc(width, 600.0, 1.0);
        app.sync(&doc, &automerge::ROOT);
        paint_frame(&mut app, &mut tr, &theme);
        assert!(
            app.toolbar.has_overflow(),
            "{width:.0} px is too narrow for a {whole:.0} px bar"
        );
        app.toolbar.open_overflow();
        paint_frame(&mut app, &mut tr, &theme);
        let (index, menu) = app.toolbar.shown_menu().expect("the More menu opens");
        assert_eq!(index, None);
        assert!(!menu.items().is_empty());
        let menu = menu.get_rect();
        assert!(
            menu.x1 <= app.rect.x1 - app.metrics.s2 + 0.01,
            "the menu {menu:?} touches the window's right edge"
        );
        assert!(
            menu.y0
                > app
                    .toolbar
                    .item_rect(chrome::bar::FORMAT)
                    .expect("Format stays")
                    .y1
        );
    }

    // Case `a_phone_width_moves_several_commands_into_the_menu`.
    // The bar fits the 800 x 600 shot size, so the overflow is reached at the
    // framework's phone width, where several commands wait in the menu.
    {
        let (fits, _doc, _tr, _theme) = measured_app_with_doc(800.0, 600.0, 1.0);
        assert!(!fits.toolbar.has_overflow(), "the bar fits 800 px at 1x");
        let (mut app, doc, mut tr, theme) = measured_app_with_doc(390.0, 844.0, 1.0);
        app.sync(&doc, &automerge::ROOT);
        paint_frame(&mut app, &mut tr, &theme);
        app.toolbar.open_overflow();
        paint_frame(&mut app, &mut tr, &theme);
        let (_, menu) = app.toolbar.shown_menu().expect("the More menu opens");
        assert!(
            menu.items().len() >= 2,
            "a phone-width bar moves several commands into the menu"
        );
        assert!(menu.get_rect().x1 <= app.rect.x1 - app.metrics.s2 + 0.01);
    }

    // Case `the_tab_menu_and_colour_popover_stand_clear_of_the_tab`.
    {
        let (mut app, _doc, mut tr, theme) = painted();
        app.open_tab_menu(0);
        paint_frame(&mut app, &mut tr, &theme);
        let tab = app.tab_rects.first().map(|(rect, _)| *rect).expect("a tab");
        let menu = app.tab_menu.get_rect();
        assert!(
            tab.y0 - menu.y1 >= app.metrics.s2,
            "the tab menu {menu:?} nearly touches the tab {tab:?}"
        );
        app.tab_menu.close();
        app.tab_colour.set_open(true);
        paint_frame(&mut app, &mut tr, &theme);
        let panel = app.tab_colour.rect();
        assert!(
            tab.y0 - panel.y1 >= app.metrics.s2,
            "the colour popover {panel:?} nearly touches the tab {tab:?}"
        );
    }

    // Case `the_tab_editor_makes_room_instead_of_covering_neighbours`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        let before = app.tab_rects.clone();
        app.begin_tab_rename(0);
        for ch in [
            "Q", "u", "a", "r", "t", "e", "r", "l", "y", " ", "p", "l", "a", "n",
        ] {
            feed(&mut app, &mut doc, &char_event(ch), Modifiers::default());
        }
        paint_frame(&mut app, &mut tr, &theme);
        let editor = app
            .tab_rename
            .as_ref()
            .map(|rename| rename.rect)
            .expect("rename");
        let second = app
            .tab_rects
            .get(1)
            .map(|(rect, _)| *rect)
            .expect("a second tab");
        assert!(
            editor.x1 <= second.x0 + 0.01,
            "the editor {editor:?} covers the next tab {second:?}"
        );
        assert!(
            editor.x1 <= app.add_tab_rect.x0 + 0.01,
            "the editor covers the add button"
        );
        assert!(
            editor.width() > before[0].0.width(),
            "the slot grew with the name being typed"
        );
        assert!(second.x0 > before[1].0.x0, "the later tabs moved aside");
    }
}

/// The command bar's row is the window's header row: the History control the
/// host offers stands in its trailing corner, clear of every command, and a
/// press on it goes through the History door.
#[test]
fn the_header_row_holds_the_window_history_control() {
    use ui::widget::WindowHeader as _;
    use ui::widget::controls::ActionControls as _;

    let (mut app, mut doc, mut tr, theme) = painted();
    assert!(app.history_rect.is_zero_area(), "no History until offered");
    app.set_history_control(Some(ui::widget::HistoryControl::Closed));
    paint_frame(&mut app, &mut tr, &theme);
    let history = app.history_rect;
    assert!(!history.is_zero_area());
    assert!(app.toolbar_rect.contains(history.center()));
    assert!(history.x1 <= app.rect.x1 && history.x1 > app.rect.x1 - 64.0);
    for index in chrome::bar::COMMANDS {
        if let Some(rect) = app.toolbar.item_rect(index) {
            assert!(
                rect.intersect(history).is_zero_area(),
                "command {index} {rect:?} under History {history:?}"
            );
        }
    }
    click(&mut app, &mut doc, history.center());
    assert!(
        app.take_control_invocation().is_some(),
        "the press opens History"
    );
}
