use super::tests::{char_event, ctrl_mods, doc_workbook, feed, measured_app_with_doc, paint_frame};
use super::*;
use crate::model::FormatCategory;
use automerge::AutoCommit;
use ui::widget::Tone;

fn mouse(state: ElementState) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, ui::winit::event::MouseButton::Left)
}

fn click(app: &mut SheetsApp, doc: &mut AutoCommit, p: Point) {
    let moved = ui::synthetic_input::cursor_moved(p.x, p.y);
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

fn painted() -> (SheetsApp, AutoCommit, TextRenderer, Theme) {
    let (mut app, doc, mut tr, theme) = measured_app_with_doc(1280.0, 800.0, 1.0);
    app.sync(&doc, &automerge::ROOT);
    paint_frame(&mut app, &mut tr, &theme);
    (app, doc, tr, theme)
}

fn press_bar(app: &mut SheetsApp, doc: &mut AutoCommit, name: &str) {
    Widget::settle(app, &mut Edits::lend(doc), &automerge::ROOT);
    let id = app
        .window_bar()
        .actions
        .into_iter()
        .flat_map(|action| std::iter::once(action.clone()).chain(action.items))
        .find(|action| action.name == name && action.items.is_empty())
        .map_or_else(|| panic!("the bar offers {name}"), |action| action.id);
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action { id });
    Widget::settle(app, &mut Edits::lend(doc), &automerge::ROOT);
}

#[test]
fn the_window_bar_names_the_workbook_and_the_sheet_in_view() {
    let (mut app, _doc, _tr, _theme) = painted();
    let sheet = app
        .active_sheet_id()
        .and_then(|id| app.workbook.sheet(&id))
        .map(|sheet| sheet.name.0.clone())
        .expect("a sheet is in view");
    let levels = |app: &SheetsApp| {
        let location = app.window_bar().location.expect("the bar has a location");
        let segments: Vec<String> = location
            .segments
            .into_iter()
            .map(|segment| segment.label)
            .collect();
        (location.label, segments)
    };
    assert_eq!(
        levels(&app),
        (sheet.clone(), vec![sheet.clone()]),
        "a workbook never named or saved is called by the sheet in view"
    );
    app.show_file_of(std::path::PathBuf::from("/home/ana/Budget 2026.xlsx"));
    assert_eq!(
        levels(&app),
        (
            "Budget 2026".to_owned(),
            vec!["Budget 2026".to_owned(), sheet.clone()]
        ),
        "the workbook is named by its file, without the extension"
    );
    std::rc::Rc::make_mut(&mut app.workbook).title = Some("Household budget".to_owned());
    assert_eq!(
        levels(&app),
        (
            "Household budget".to_owned(),
            vec!["Household budget".to_owned(), sheet]
        ),
        "a workbook's title names it before its file"
    );
}

#[test]
fn a_comment_wears_the_colour_the_member_list_announces_for_its_author() {
    let (mut app, mut doc, mut tr, theme) = painted();
    let sheet = app.sheet_ids[0].clone();
    let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
    book.apply(Workbook::COMMENT.with(crate::resource::verbs::Comment {
        sheet,
        range: "C3".into(),
        text: "Amira asked to swap Tuesday afternoon with Wen.".into(),
    }))
    .unwrap();
    doc = book.doc().clone();
    app.sync(&doc, &automerge::ROOT);
    let c3 = app.grid.cell_screen_rect(2, 2).center();
    click(&mut app, &mut doc, c3);
    paint_frame(&mut app, &mut tr, &theme);
    let author = app.open_thread().expect("the open thread").author.clone();

    let mut presence = ui::presence::PresenceCtx::default();
    presence.set_presence_hue(
        ui::permissions::UserId(author.clone()),
        ui::tokens::PresenceHue::Violet,
    );
    let markers = app.comment_markers(&presence);
    assert_eq!(
        markers[0].1,
        Tone::Presence(ui::tokens::PresenceHue::Violet),
        "the marker wears the colour the member list gives its author"
    );
    assert_eq!(
        app.comment_markers(&ui::presence::PresenceCtx::default())[0].1,
        Tone::of_identity(&author),
        "someone who announced no colour keeps the one their identity derives"
    );
}

#[test]
fn painted_chrome_answers_pointer_and_keyboard() {
    {
        let (mut app, mut doc, _tr, _theme) = painted();
        press_bar(&mut app, &mut doc, "Percent");
        app.sync(&doc, &automerge::ROOT);
        assert_eq!(
            app.current_active_format.category(),
            FormatCategory::Percent,
            "the active cell took the format"
        );
    }

    {
        let (mut app, mut doc, mut tr, theme) = painted();
        app.grid.select_only((1, 1));
        app.sync(&doc, &automerge::ROOT);
        assert!(!app.active_cell_bold);
        press_bar(&mut app, &mut doc, "Bold");
        app.sync(&doc, &automerge::ROOT);
        assert!(app.active_cell_bold, "the Bold command bolds the cell");
        paint_frame(&mut app, &mut tr, &theme);
        let bold = app
            .window_bar()
            .actions
            .into_iter()
            .find(|action| action.name == "Bold")
            .expect("the Bold toggle");
        assert!(bold.on, "the toggle reads as on");
        assert!(feed(&mut app, &mut doc, &char_event("b"), ctrl_mods()));
        app.sync(&doc, &automerge::ROOT);
        assert!(!app.active_cell_bold, "Ctrl+B takes it off again");
    }

    {
        let (mut app, mut doc, mut tr, theme) = painted();
        press_bar(&mut app, &mut doc, "Document settings");
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

#[test]
fn the_commands_are_the_window_bars_and_the_formula_bar_leads() {
    let (app, _doc, _tr, _theme) = painted();
    let toolbar = app.window_bar();
    let names: Vec<&str> = toolbar.actions.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "File",
            "Format",
            "Bold",
            "AutoSum",
            "Show formulas",
            "Document settings"
        ]
    );
    assert!(toolbar.actions.iter().take(2).all(|a| !a.items.is_empty()));
    assert!(toolbar.actions.iter().skip(3).all(|a| a.menu));
    assert!(
        (app.formula_rect.y0 - app.rect.y0).abs() < 0.5,
        "no command row above the formula bar"
    );
}

#[test]
fn a_comment_is_marked_on_its_cell_opens_from_it_and_names_its_author() {
    let (mut app, mut doc, mut tr, theme) = painted();
    let sheet = app.sheet_ids[0].clone();
    let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
    book.apply(Workbook::COMMENT.with(crate::resource::verbs::Comment {
        sheet,
        range: "C3".into(),
        text: "Amira asked to swap Tuesday afternoon with Wen.".into(),
    }))
    .unwrap();
    doc = book.doc().clone();
    app.sync(&doc, &automerge::ROOT);
    let a1 = app.grid.cell_screen_rect(0, 0).center();
    click(&mut app, &mut doc, a1);
    paint_frame(&mut app, &mut tr, &theme);

    let c3 = app.grid.cell_screen_rect(2, 2);
    let markers = app.comment_markers(&ui::presence::PresenceCtx::default());
    assert_eq!(markers.len(), 1, "one comment, one marker");
    assert_eq!(markers[0].0, c3, "the marker is on the commented cell");
    assert!(app.open_thread().is_none(), "the thread waits on its cell");
    assert!(
        !app.annotation_clicks
            .iter()
            .any(|(_, action)| matches!(action, AnnotationClick::Reply(_))),
        "no thread is shown before its cell is chosen"
    );

    click(&mut app, &mut doc, c3.center());
    paint_frame(&mut app, &mut tr, &theme);
    let thread = app
        .open_thread()
        .cloned()
        .expect("the cell opens its thread");
    let card = app.comment_card_rect(&thread).expect("the card is shown");
    assert!(
        card.intersect(c3).is_zero_area(),
        "the card never hides its cell"
    );
    assert!(
        app.annotation_clicks
            .iter()
            .any(|(rect, action)| matches!(action, AnnotationClick::Reply(_))
                && card.contains(rect.center())),
        "the open thread offers Reply on its card"
    );

    // The author by name: the workspace's name for their id, never the id.
    let author = "4f1c9a7be0d23c58a6e1f0b9d7c4a2e85b3f6d1a9c0e7b2d4f8a1c3e5b7d9f0a";
    let stranger = "9e8d7c6b5a49382716051f2e3d4c5b6a79881726354f4e3d2c1b0a9f8e7d6c5b";
    app.attach_people(ui::framework::People::named(
        Some("me"),
        &[(author, "Amira Haddad")],
    ));
    assert_eq!(app.author_name(author), "Amira Haddad");
    assert_eq!(
        app.author_name(stranger),
        "Someone",
        "someone the workspace has not named yet is never shown by id"
    );
    assert_ne!(app.author_name("me"), "me", "the reader is not shown by id");
    assert_eq!(app.author_name(""), "Someone");
}

#[test]
fn a_cell_right_click_opens_the_edit_menu_and_inserts_the_range_as_one_verb() {
    let (mut app, mut doc, _tr, _theme) = painted();
    let anchor = app.grid.cell_screen_rect(1, 2).center(); // B3
    feed(
        &mut app,
        &mut doc,
        &ui::synthetic_input::cursor_moved(anchor.x, anchor.y),
        Modifiers::default(),
    );
    feed(
        &mut app,
        &mut doc,
        &ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            ui::winit::event::MouseButton::Right,
        ),
        Modifiers::default(),
    );
    assert!(app.cell_menu.is_open(), "a cell has a context menu");
    assert_eq!(
        app.cell_menu_target,
        Some((1, 2)),
        "the pressed cell is the target"
    );
    assert_eq!(app.grid.active(), (1, 2), "the pressed cell becomes active");

    app.grid.select_range_public((1, 1), (1, 2)); // B2:B3, two rows
    let before = doc_workbook(&doc)
        .sheets
        .values()
        .next()
        .map_or(0, |sheet| sheet.cells.rows().len());
    let mut edits = Edits::lend(&mut doc);
    app.run_cell_menu_row(&mut edits, CellMenuRow::InsertRowsAbove);
    let after = doc_workbook(&doc)
        .sheets
        .values()
        .next()
        .map_or(0, |sheet| sheet.cells.rows().len());
    assert_eq!(
        after,
        before + 2,
        "both selected rows are inserted by one verb"
    );
    assert!(
        app.grid.is_animating(),
        "the rows arrive as a band that opens, not by teleporting"
    );
}

#[test]
fn find_navigates_to_a_match_on_another_sheet() {
    use ui::widget::{FindOptions, Findable};
    let (mut app, _doc, _tr, _theme) = painted();
    let found = app.find("Growth", FindOptions::default());
    assert!(!found.is_empty(), "the sales header is findable");
    let index = app
        .find_matches
        .iter()
        .position(|found| found.sheet == 1)
        .expect("a match on the second sheet");
    app.reveal(index);
    assert_eq!(app.active_sheet, 1, "find switched to the match's sheet");
    assert_eq!(
        app.grid.active(),
        app.find_matches[index].addr,
        "the match's cell is selected"
    );
    assert!(
        app.status.starts_with("Match "),
        "the status names the match and where it went: {}",
        app.status
    );
}

#[test]
fn a_revealed_match_stands_clear_of_the_window_searchs_options() {
    use crate::grid::GridCmd;
    use ui::widget::{FindOptions, Findable};
    let (mut app, mut doc, _tr, _theme) = painted();
    let first = (1, 0);
    let needle = (1, 40);
    app.apply_cmds(
        &mut Edits::lend(&mut doc),
        vec![
            GridCmd::SetCell {
                addr: first,
                raw: "first".into(),
            },
            GridCmd::SetCell {
                addr: needle,
                raw: "needle".into(),
            },
            GridCmd::Commit,
        ],
    );
    app.sync(&doc, &automerge::ROOT);
    let found = app.find("needle", FindOptions::default());
    assert_eq!(found.len(), 1, "one cell says it");

    let body = app.grid.body_rect();
    let cover = Rect::new(body.x0, app.grid_rect.y0, body.x1, body.y0 + 120.0);
    app.grid.select_only((1, 120));
    app.set_find_cover(Some(cover));
    app.reveal(0);
    let cell = app.grid.cell_screen_rect(needle.0, needle.1);
    assert!(
        (cell.y0 - cover.y1).abs() < 0.5,
        "the match stands just below what the search covers: {cell:?} under {cover:?}"
    );

    app.grid.select_only(first);
    app.grid.ensure_active_visible();
    let cell = app.grid.cell_screen_rect(first.0, first.1);
    assert!(
        (cell.y0 - cover.y1).abs() < 0.5,
        "the first row scrolls up behind the options: {cell:?} under {cover:?}"
    );

    app.set_find_cover(None);
    app.grid.select_only((1, 120));
    app.reveal(0);
    let cell = app.grid.cell_screen_rect(needle.0, needle.1);
    assert!(
        (cell.y0 - body.y0).abs() < 0.5,
        "with nothing covering it, the match stands at the body's top: {cell:?}"
    );
}

#[test]
fn a_sideways_wheel_the_columns_cannot_follow_is_left_to_the_desk() {
    let (mut app, mut doc, _tr, _theme) = painted();
    let wheel = |dx: f64| ui::winit::event::WindowEvent::MouseWheel {
        device_id: ui::winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(ui::winit::dpi::PhysicalPosition::new(-dx, 0.0)),
        phase: ui::winit::event::TouchPhase::Moved,
    };
    assert!(
        !feed(&mut app, &mut doc, &wheel(-24.0), Modifiers::default()),
        "the sheet shows its first column: a swipe before it is the desk's"
    );
    app.pending.redraw = false;
    assert!(
        feed(&mut app, &mut doc, &wheel(24.0), Modifiers::default()),
        "the later columns are the sheet's"
    );
    assert!(app.pending.redraw, "the sheet repaints where it scrolled");
}
