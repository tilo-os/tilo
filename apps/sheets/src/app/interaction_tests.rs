//! The painted chrome answers the pointer and the keyboard: the command bar
//! opens menus whose rows act on the sheet, Ctrl+B and the Bold command share
//! one path, the settings sheet applies a choice, and a tab's colour is picked
//! from its popover. Each case drives the production `handle_event` over the
//! rects the last paint left behind.

use super::tests::{char_event, ctrl_mods, feed, measured_app_with_doc, paint_frame};
use super::*;
use crate::model::FormatCategory;

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button: MouseButton::Left,
    }
}

/// Move the pointer to `p` and press and release the left button there.
fn click(app: &mut SheetsApp, doc: &mut AutoCommit, p: Point) {
    let moved = WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
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
    let (mut app, mut doc, mut tr, theme) = measured_app_with_doc(1280.0, 800.0, 1.0);
    app.sync(&mut doc, &automerge::ROOT);
    paint_frame(&mut app, &mut tr, &theme);
    (app, doc, tr, theme)
}

/// Click the command on the bar that opens `opens` and is called `label`.
fn click_command(app: &mut SheetsApp, doc: &mut AutoCommit, opens: Option<OpenMenu>, label: &str) {
    let centre = app
        .menu_buttons
        .iter()
        .find(|button| button.opens == opens && button.label == label)
        .map(|button| button.rect.center())
        .expect("the command is on the bar");
    click(app, doc, centre);
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
        click_command(&mut app, &mut doc, Some(OpenMenu::Format), "Format");
        assert!(app.menu.is_open() && app.open_menu == OpenMenu::Format);
        paint_frame(&mut app, &mut tr, &theme);
        let percent = app
            .menu_items
            .iter()
            .position(|item| matches!(item, Some(MenuItem::SetFormat(f)) if f.category() == FormatCategory::Percent))
            .expect("a Percent row");
        let row = app.menu.item_rect(percent).expect("the row is laid out");
        click(&mut app, &mut doc, row.center());
        assert!(!app.menu.is_open(), "choosing a row closes the menu");
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.current_active_format.category(),
            FormatCategory::Percent,
            "the active cell took the format"
        );
    }

    // Case `one_click_moves_from_one_menu_to_the_next`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        click_command(&mut app, &mut doc, Some(OpenMenu::File), "File");
        assert_eq!(app.open_menu, OpenMenu::File);
        paint_frame(&mut app, &mut tr, &theme);
        click_command(&mut app, &mut doc, Some(OpenMenu::Format), "Format");
        assert_eq!(app.open_menu, OpenMenu::Format, "Format opens in one click");
        paint_frame(&mut app, &mut tr, &theme);
        click_command(&mut app, &mut doc, Some(OpenMenu::Format), "Format");
        assert!(
            !app.menu.is_open() && app.open_menu == OpenMenu::None,
            "the same opener closes its menu"
        );
    }

    // Case `bold_command_and_ctrl_b_share_one_path`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        app.grid.select_only((1, 1));
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.active_cell_bold);
        click_command(&mut app, &mut doc, None, "Bold");
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.active_cell_bold, "the Bold command bolds the cell");
        paint_frame(&mut app, &mut tr, &theme);
        assert!(app.command_is_toggled(Some(&MenuItem::ToggleBold)));
        assert!(feed(&mut app, &mut doc, &char_event("b"), ctrl_mods()));
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.active_cell_bold, "Ctrl+B takes it off again");
    }

    // Case `settings_sheet_applies_a_choice_and_closes_on_done`.
    {
        let (mut app, mut doc, mut tr, theme) = painted();
        click_command(&mut app, &mut doc, None, "Document settings");
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
        app.sync(&mut doc, &automerge::ROOT);
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
    // A window too narrow for the bar moves the commands that do not fit into
    // the More menu, which opens under its button and stays inside the window
    // with room to spare.
    {
        let (mut app, mut doc, mut tr, theme) = measured_app_with_doc(520.0, 600.0, 1.0);
        app.sync(&mut doc, &automerge::ROOT);
        paint_frame(&mut app, &mut tr, &theme);
        assert!(
            app.menu_buttons
                .iter()
                .any(|button| button.opens == Some(OpenMenu::More)),
            "520 px is too narrow for the whole bar"
        );
        app.open_command_menu(OpenMenu::More);
        paint_frame(&mut app, &mut tr, &theme);
        let menu = app.menu.get_rect();
        assert!(app.menu.is_open() && !app.menu_items.is_empty());
        assert!(
            menu.x1 <= app.rect.x1 - app.metrics.s2 + 0.01,
            "the menu {menu:?} touches the window's right edge"
        );
        let anchor = app
            .menu_buttons
            .iter()
            .find(|button| button.opens == Some(OpenMenu::More))
            .map(|button| button.rect)
            .expect("the More button");
        assert!(menu.y0 > anchor.y1, "the menu opens below its button");
    }

    // Case `a_phone_width_moves_several_commands_into_the_menu`.
    // The bar fits the 800 x 600 shot size, so the overflow is reached at the
    // framework's phone width, where several commands wait in the menu.
    {
        let (fits, _doc, _tr, _theme) = measured_app_with_doc(800.0, 600.0, 1.0);
        assert!(
            fits.menu_buttons
                .iter()
                .all(|button| button.opens != Some(OpenMenu::More)),
            "the bar fits 800 px at 1x"
        );
        let (mut app, mut doc, mut tr, theme) = measured_app_with_doc(390.0, 844.0, 1.0);
        app.sync(&mut doc, &automerge::ROOT);
        paint_frame(&mut app, &mut tr, &theme);
        app.open_command_menu(OpenMenu::More);
        assert!(app.menu.is_open());
        assert!(
            app.menu_items.len() >= 2,
            "a phone-width bar moves several commands into the menu"
        );
        assert!(app.menu.get_rect().x1 <= app.rect.x1 - app.metrics.s2 + 0.01);
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
