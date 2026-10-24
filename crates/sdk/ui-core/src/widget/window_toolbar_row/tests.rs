use super::*;
use crate::a11y::{AccessNode, Role};
use crate::window_toolbar::{ToolbarAction, ToolbarLocation, ToolbarNavigation};

fn browser() -> WindowToolbar {
    WindowToolbar {
        sidebar: None,
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: false,
        }),
        location: Some(ToolbarLocation {
            label: "duckduckgo.com".into(),
            value: "https://duckduckgo.com/".into(),
            editable: true,
            name: "Address and search".into(),
            ..ToolbarLocation::default()
        }),
        actions: vec![ToolbarAction {
            id: "reload".into(),
            icon: "refresh".into(),
            name: "Reload".into(),
            enabled: true,
            on: false,
            menu: false,
            items: Vec::new(),
            navigates: false,
            toggles: false,
            destructive: false,
            labelled: false,
        }],
        ..WindowToolbar::default()
    }
}

fn row() -> ToolbarRow {
    let mut row = ToolbarRow::new();
    row.show(
        7,
        Some(&browser()),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
    row
}

/// A notice takes the location's place for as long as it is up, at any
/// width, without moving a slot; its button stands right after its text,
/// naming itself where the row has room, and runs as a bar action; a
/// screen reader hears it without the focus moving.
#[test]
fn a_notice_stands_in_the_location_and_is_read_out_politely() {
    use crate::widget::toast::ToastKind;
    use crate::window_toolbar::ToolbarNotice;
    for width in [640.0, 420.0 - 40.0] {
        let bounds = Rect::new(40.0, 0.0, 40.0 + width, 40.0);
        let mut quiet = ToolbarRow::new();
        quiet.show(
            7,
            Some(&browser()),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        quiet.lay_out(bounds);
        let noticed = WindowToolbar {
            notice: Some(ToolbarNotice::new("Saved report.pdf", ToastKind::Success)),
            ..browser()
        };
        let mut row = ToolbarRow::new();
        row.show(
            7,
            Some(&noticed),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(bounds);
        assert_eq!(
            row.slots().location,
            quiet.slots().location,
            "the notice reserves no room of its own at {width}"
        );
        assert!(row.slots().location.width() > 0.0);
        let nodes = row.accessibility();
        let said = nodes
            .iter()
            .find(|node| node.name.as_deref() == Some("Saved report.pdf"))
            .expect("the notice is the location's text");
        assert_eq!(said.live, Some(crate::a11y::Politeness::Polite));
        assert!(
            quiet.accessibility().iter().all(|node| node.live.is_none()),
            "the location is not a live region without a notice"
        );
    }

    let undo = WindowToolbar {
        notice: Some(
            ToolbarNotice::new("Deleted Groceries", ToastKind::Info).action(
                crate::window_toolbar::ToolbarAction::new("notice-undo", "undo-2", "Undo"),
            ),
        ),
        ..browser()
    };
    let mut row = ToolbarRow::new();
    row.show(
        7,
        Some(&undo),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
    assert!(row.measure_views(&Theme::dark()), "the notice is measured");
    let slots = row.slots();
    assert!(
        slots.notice_labelled == [true] && slots.notice[0].width() > CONTROL_PX,
        "Undo names itself in a wide row: {slots:?}"
    );
    assert!(
        (slots.notice[0].x0 - slots.location.x1 - CONTROL_GAP).abs() < 1e-9,
        "right after the notice's text"
    );
    assert_eq!(row.slots().actions.len(), 1, "and is not among the actions");
    let button = row.slots().notice[0].center();
    assert!(row.press(button));
    assert!(row.release(button));
    assert_eq!(
        row.take_events(),
        vec![(
            7,
            ToolbarEvent::Action {
                id: "notice-undo".into()
            }
        )],
        "the notice's button runs as a bar action"
    );
}

#[test]
fn navigation_leads_actions_trail_and_the_location_fills_between() {
    let row = row();
    let slots = row.slots();
    assert!(slots.back.x1 < slots.forward.x0);
    assert!(slots.forward.x1 < slots.location.x0);
    assert!(slots.location.x1 < slots.actions[0].x0);
    assert!((slots.actions[0].x1 - 640.0).abs() < f64::EPSILON);
    assert!(slots.location.width() > 400.0);
}

#[test]
fn back_runs_only_where_it_can_and_forward_not_at_the_end() {
    let mut row = row();
    let back = row.slots().back.center();
    assert!(row.press(back));
    assert!(row.release(back));
    let forward = row.slots().forward.center();
    assert!(row.press(forward));
    assert!(row.release(forward));
    let reload = row.slots().actions[0].center();
    assert!(row.press(reload));
    assert!(row.release(reload));
    assert_eq!(
        row.take_events(),
        vec![
            (7, ToolbarEvent::Back),
            (
                7,
                ToolbarEvent::Action {
                    id: "reload".into()
                }
            )
        ],
        "forward cannot go"
    );
}

#[test]
fn a_click_on_the_location_edits_the_whole_address_and_esc_puts_it_back() {
    let mut row = row();
    let location = row.slots().location.center();
    assert!(row.press(location));
    assert!(row.is_editing());
    assert_eq!(row.field_text(), Some("https://duckduckgo.com/"));
    assert!(row.cancel_edit());
    assert!(!row.is_editing());
    assert!(row.take_events().is_empty(), "nothing was committed");
    assert_eq!(
        row.take_drafts(),
        vec![(
            7,
            ToolbarEvent::Edit {
                text: "https://duckduckgo.com/".into()
            }
        )],
        "the app hears its location put back"
    );
}

/// A terminal's slots: where its shell runs, one of a few places.
fn terminal() -> WindowToolbar {
    use crate::window_toolbar::LocationChoice;
    let choice = |label: &str, value: &str, current: bool, separated: bool| LocationChoice {
        label: label.into(),
        value: value.into(),
        current,
        separated,
    };
    WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Your shell on this device".into(),
            choices: vec![
                choice("Your shell on this device", "target:host", true, false),
                choice("work", "target:environment:work", false, false),
                choice("Manage environments…", "manage", false, true),
            ],
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    }
}

#[test]
fn a_location_with_choices_opens_its_picker_and_commits_the_one_chosen() {
    let mut row = ToolbarRow::new();
    row.show(
        9,
        Some(&terminal()),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.set_popup_area(Rect::new(0.0, 0.0, 800.0, 600.0));
    row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
    let location = row.slots().location.center();
    assert!(row.press(location));
    assert!(!row.is_editing(), "a picker never turns into a field");
    assert!(!row.choices_open(), "it opens as the press ends");
    assert!(row.release(location));
    assert!(row.choices_open());
    assert_eq!(
        row.menu_labels(),
        vec![
            "Your shell on this device".to_owned(),
            "work".to_owned(),
            "Manage environments…".to_owned()
        ]
    );
    let picker = row
        .accessibility()
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some(LOCATION_TARGET))
        .expect("the picker reaches assistive technology");
    assert_eq!(picker.state.expanded, Some(true));
    assert_eq!(picker.children.len(), 3);
    assert_eq!(picker.children[0].state.checked, Some(true));
    assert!(row.perform(&format!("{CHOICE_TARGET_PREFIX}1")));
    assert!(!row.choices_open(), "choosing closes the picker");
    assert_eq!(
        row.take_events(),
        vec![(
            9,
            ToolbarEvent::Commit {
                text: "target:environment:work".into()
            }
        )]
    );
}

#[test]
fn an_edited_document_says_so_after_its_label_and_to_assistive_technology() {
    let document = |edited: bool| ToolbarLocation {
        label: "Harbour cut".into(),
        edited,
        ..Default::default()
    };
    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    let saved = LocationLabel::measure(&mut tr, &theme, "Harbour cut", false, 400.0);
    let edited = LocationLabel::measure(&mut tr, &theme, "Harbour cut", true, 400.0);
    assert!(saved.edited.is_none());
    let (word, after) = edited.edited.clone().expect("an edited document says so");
    assert_eq!(word, text("window-toolbar-edited"));
    assert!(after > saved.width, "the word follows the whole label");
    assert!(edited.width > saved.width);
    assert_eq!(edited.line, saved.line, "the label itself is unchanged");
    // Too narrow for both: the label keeps the room.
    let narrow = LocationLabel::measure(&mut tr, &theme, "Harbour cut", true, 30.0);
    assert!(narrow.edited.is_none());

    let node = |edited: bool| {
        let mut row = ToolbarRow::new();
        let toolbar = WindowToolbar {
            location: Some(document(edited)),
            ..Default::default()
        };
        row.show(
            1,
            Some(&toolbar),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 640.0, 40.0));
        row.accessibility()
            .into_iter()
            .find(|node| node.name.as_deref() == Some("Harbour cut"))
            .expect("the location reaches assistive technology")
    };
    assert_eq!(node(true).description, Some(text("window-toolbar-edited")));
    assert_eq!(node(false).description, None);
}

fn text(key: &str) -> String {
    crate::i18n::I18nManager::lookup_plain(key)
}

/// The node with `role` and `name` among `nodes` and their children.
fn find_node<'a>(nodes: &'a [AccessNode], role: Role, name: &str) -> Option<&'a AccessNode> {
    nodes.iter().find_map(|node| {
        if node.role == role && node.name.as_deref() == Some(name) {
            Some(node)
        } else {
            find_node(&node.children, role, name)
        }
    })
}

/// An address book's slots: the sidebar, Back, its place as the
/// location, and New contact, Import and Export, or Save and Cancel
/// while a person is edited.
fn address_book(editing: bool) -> WindowToolbar {
    let actions = if editing {
        vec![
            ToolbarAction::new("save", "check", "Save"),
            ToolbarAction::new("cancel", "x", "Cancel"),
        ]
    } else {
        vec![
            ToolbarAction::new("new-contact", "plus", "New contact"),
            ToolbarAction::new("import", "download", "Import contacts").in_menu(),
            ToolbarAction::new("export", "upload", "Export contacts").in_menu(),
        ]
    };
    WindowToolbar {
        sidebar: Some(false),
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: false,
        }),
        location: Some(ToolbarLocation {
            label: "All contacts".into(),
            editable: true,
            name: "Search contacts".into(),
            ..ToolbarLocation::default()
        }),
        actions,
        ..WindowToolbar::default()
    }
    .bounded()
}

fn address_book_row(editing: bool, width: f64) -> ToolbarRow {
    let mut row = ToolbarRow::new();
    row.show(
        4,
        Some(&address_book(editing)),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.set_popup_area(Rect::new(0.0, 0.0, 1200.0, 800.0));
    row.lay_out(Rect::new(0.0, 0.0, width, 40.0));
    row
}

#[test]
fn every_slot_and_overflow_item_is_found_by_role_and_name_and_runs() {
    let wide = address_book_row(false, 900.0).accessibility();
    assert!(
        find_node(
            &wide,
            Role::ToggleButton,
            &text("window-toolbar-show-sidebar")
        )
        .is_some()
    );
    assert!(find_node(&wide, Role::Button, &text("window-toolbar-back")).is_some());
    assert!(
        find_node(&wide, Role::Button, "Search contacts").is_some(),
        "the editable location is a control"
    );
    let new = find_node(&wide, Role::Button, "New contact").expect("in the row, a button");
    let more =
        find_node(&wide, Role::Button, &text("window-toolbar-more")).expect("the overflow control");
    assert_eq!(more.state.expanded, Some(false));
    let import = find_node(&more.children, Role::MenuItem, "Import contacts")
        .expect("what the row has no room for is the overflow menu's");
    assert!(
        import.state.hidden,
        "a closed menu's item is hidden from a reader"
    );
    assert!(
        find_node(&wide, Role::Button, "Import contacts").is_none(),
        "an item in the menu is not also a button in the row"
    );
    assert_ne!(new.bounds, more.bounds);

    // Narrow, New contact goes in the menu too; open, each item stands
    // where its row is drawn.
    let mut narrow = address_book_row(false, 240.0);
    assert!(narrow.slots().actions.is_empty(), "no room for any action");
    assert!(narrow.perform(OVERFLOW_TARGET), "More opens its menu");
    assert!(narrow.popup_open());
    let nodes = narrow.accessibility();
    let more = find_node(&nodes, Role::Button, &text("window-toolbar-more")).unwrap();
    assert_eq!(more.state.expanded, Some(true));
    let new = find_node(&nodes, Role::MenuItem, "New contact").expect("in the open menu");
    let import = find_node(&nodes, Role::MenuItem, "Import contacts").unwrap();
    assert!(!new.state.hidden && new.state.focusable);
    let (new_at, import_at) = (new.bounds.unwrap(), import.bounds.unwrap());
    assert!(
        new_at != more.bounds.unwrap() && new_at.y1 <= import_at.y0,
        "each item is its own row of the menu: {new_at:?}, {import_at:?}"
    );
    let target = new.action_target.clone().unwrap();
    assert!(narrow.perform(&target), "pressing the item runs it");
    assert!(!narrow.popup_open(), "and closes its menu");
    assert_eq!(
        narrow.take_events(),
        vec![(
            4,
            ToolbarEvent::Action {
                id: "new-contact".into()
            }
        )]
    );

    // Editing a person: Save and Cancel, closed menu or not.
    let mut editing = address_book_row(true, 240.0);
    let nodes = editing.accessibility();
    let save = find_node(&nodes, Role::MenuItem, "Save").expect("Save is reachable");
    assert!(find_node(&nodes, Role::MenuItem, "Cancel").is_some());
    let target = save.action_target.clone().unwrap();
    assert!(editing.perform(&target), "a closed menu's item still runs");
    assert_eq!(
        editing.take_events(),
        vec![(4, ToolbarEvent::Action { id: "save".into() })]
    );

    let mut row = address_book_row(false, 900.0);
    assert!(row.begin_edit());
    assert!(
        row.accessibility()
            .iter()
            .any(|node| node.role == Role::TextInput),
        "the edited location is a field"
    );
}

#[test]
fn slots_shown_where_none_stood_are_at_rest_at_once() {
    let mut row = ToolbarRow::new();
    let now = crate::time::now();
    row.show(7, Some(&browser()), Theme::dark().motion(), now);
    assert!(
        !row.needs_animation_frame(now),
        "nothing faded out, so nothing fades in"
    );
    row.show(8, Some(&browser()), Theme::dark().motion(), now);
    assert!(
        row.needs_animation_frame(now),
        "another tab's slots cross-fade"
    );
}

#[test]
fn another_tab_cross_fades_and_drops_what_was_typed_in_this_one() {
    let mut row = row();
    assert!(row.begin_edit());
    let now = crate::time::now();
    row.show(8, None, Theme::dark().motion(), now);
    assert!(!row.is_editing());
    assert!(row.toolbar().is_none());
    assert!(row.needs_animation_frame(now), "the old slots fade out");
}

/// A calendar's slots: the sidebar, previous, next and today, the
/// period as the location, the view switcher, New event and an import
/// for the menu.
fn calendar() -> WindowToolbar {
    use crate::window_toolbar::{ToolbarView, ToolbarViews};
    let view = |id: &str, label: &str| ToolbarView {
        id: id.into(),
        label: label.into(),
    };
    WindowToolbar {
        sidebar: Some(true),
        leading: vec![
            ToolbarAction::new("previous", "chevron-left", "Previous week").navigating(),
            ToolbarAction::new("next", "chevron-right", "Next week").navigating(),
            ToolbarAction::new("today", "calendar", "Today").labelled(),
        ],
        location: Some(ToolbarLocation::period(a_week())),
        views: Some(ToolbarViews {
            name: "View".into(),
            choices: vec![
                view("month", "Month"),
                view("week", "Week"),
                view("day", "Day"),
            ],
            chosen: "week".into(),
        }),
        actions: vec![
            ToolbarAction::new("import", "download", "Import").in_menu(),
            ToolbarAction::new("new-event", "plus", "New event"),
        ],
        ..WindowToolbar::default()
    }
    .bounded()
}

/// Mon, Oct 19 to Sun, Oct 25, 2026.
fn a_week() -> crate::format::Period {
    let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 10, d).unwrap_or_default();
    crate::format::Period::Days(day(19), day(25))
}

/// The calendar's bar with an address-like location, which cannot be the
/// views' picker.
fn calendar_with_a_field() -> WindowToolbar {
    let mut toolbar = calendar();
    if let Some(location) = toolbar.location.as_mut() {
        location.editable = true;
    }
    toolbar
}

fn calendar_row(width: f64) -> ToolbarRow {
    calendar_row_of(&calendar(), width)
}

fn calendar_row_of(toolbar: &WindowToolbar, width: f64) -> ToolbarRow {
    let mut row = ToolbarRow::new();
    row.show(
        3,
        Some(toolbar),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(0.0, 0.0, width, 40.0));
    assert!(
        row.measure_views(&Theme::dark()),
        "the switcher is measured"
    );
    row
}

fn file_toolbar(actions: usize) -> WindowToolbar {
    WindowToolbar {
        sidebar: Some(false),
        location: Some(ToolbarLocation {
            label: "explorer.rs".into(),
            ..ToolbarLocation::default()
        }),
        actions: (0..actions)
            .map(|index| ToolbarAction::new(format!("a{index}"), "panel-bottom", "Act"))
            .collect(),
        ..WindowToolbar::default()
    }
}

#[test]
fn a_compact_row_never_folds_one_action_alone_into_the_menu() {
    // As wide as the row a compact (420 px) window leaves the toolbar.
    let compact = Rect::new(0.0, 0.0, 220.0, 40.0);
    let one = Slots::lay_out(
        &file_toolbar(1),
        compact,
        None,
        (&LocationForms::default(), &[]),
    );
    assert_eq!(one.actions.len(), 1, "the action stands in the row");
    assert_eq!(one.overflow, Rect::ZERO, "no menu for one action");

    for declared in [2, 3] {
        let slots = Slots::lay_out(
            &file_toolbar(declared),
            compact,
            None,
            (&LocationForms::default(), &[]),
        );
        let folded = declared - slots.actions.len();
        assert!(
            folded == 0 || folded >= 2,
            "{declared} actions: the menu holds {folded}"
        );
        assert_eq!(slots.overflow.area() > 0.0, folded > 0);
    }

    let wide = Slots::lay_out(
        &file_toolbar(1),
        Rect::new(0.0, 0.0, 900.0, 40.0),
        None,
        (&LocationForms::default(), &[]),
    );
    assert_eq!(wide.actions.len(), 1);
    assert_eq!(wide.overflow, Rect::ZERO);
}

#[test]
fn leading_actions_follow_the_navigation_and_views_stand_before_the_actions() {
    let row = calendar_row(1000.0);
    let slots = row.slots();
    assert_eq!(slots.leading.len(), 3);
    assert!(slots.sidebar.x1 < slots.leading[0].x0);
    assert!(slots.leading[2].x1 < slots.location.x0);
    assert_eq!(slots.views.len(), 3, "the switcher has room in the row");
    assert!(slots.location.x1 < slots.views_strip().x0);
    assert!(slots.views_strip().x1 < slots.actions[0].x0);
    assert_eq!(slots.actions.len(), 1, "Import is the menu's only");
    assert!(slots.overflow.area() > 0.0, "the menu holds Import");
}

#[test]
fn each_view_tab_is_as_wide_as_its_label() {
    let theme = Theme::dark();
    let row = calendar_row(1280.0);
    let widths = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
        SegmentedFace::tab_widths(&["Month", "Week", "Day"], VIEWS_SIZE, text, &theme)
    });
    let tabs = &row.slots().views;
    assert_eq!(tabs.len(), widths.len());
    for (tab, width) in tabs.iter().zip(&widths) {
        assert!(
            (tab.width() - width).abs() < 0.5,
            "a tab {} wide for a label needing {width}",
            tab.width()
        );
    }
    for pair in tabs.windows(2) {
        assert!((pair[0].x1 - pair[1].x0).abs() < 1e-9, "tabs are packed");
    }
}

#[test]
fn a_view_is_chosen_in_the_row_or_from_the_menu_when_the_row_has_no_room() {
    let mut row = calendar_row(1000.0);
    let month = row.slots().views[0].center();
    assert!(row.press(month));
    assert!(row.release(month));
    let today = row.slots().leading[2].center();
    assert!(row.press(today));
    assert!(row.release(today));
    assert_eq!(
        row.take_events(),
        vec![
            (3, ToolbarEvent::Action { id: "month".into() }),
            (3, ToolbarEvent::Action { id: "today".into() }),
        ]
    );

    // Narrower than the sidebar's toggle and the views menu together.
    let mut narrow = calendar_row(120.0);
    assert!(
        narrow.slots().views.is_empty() && narrow.slots().views_menu == Rect::ZERO,
        "no room even for the views menu: the views go in the overflow menu"
    );
    assert!(narrow.slots().overflow.area() > 0.0);
    assert!(narrow.open_overflow());
    assert_eq!(
        narrow.menu_entries.first().copied().flatten(),
        Some(MenuEntry::View(0)),
        "the menu starts with the views"
    );
    assert!(narrow.perform(&format!("{VIEW_TARGET_PREFIX}day")));
    assert!(narrow.perform(&format!("{LEADING_TARGET_PREFIX}previous")));
    assert_eq!(
        narrow.take_events(),
        vec![
            (3, ToolbarEvent::Action { id: "day".into() }),
            (
                3,
                ToolbarEvent::Action {
                    id: "previous".into()
                }
            ),
        ]
    );
    let nodes = narrow.accessibility();
    let week = find_node(&nodes, Role::MenuItem, "Week")
        .expect("a view in the overflow menu reaches assistive technology");
    assert_eq!(week.state.checked, Some(true));
}

/// A row with no room for the switcher folds the views into a menu that
/// names the chosen view: a press (a click or a tap) lists every view, the
/// chosen one checked, and choosing one hands the app its id. The menu is
/// as wide as its widest view, so choosing another never moves the row.
/// As wide as the row a compact (420 px) window leaves the calendar's
/// slots beside its bar's own search, History and window controls.
const COMPACT_CALENDAR_ROW: f64 = 233.0;

/// The views a row's open menu lists, as assistive technology reaches
/// each: its name, what it runs, and its check.
fn listed_views(row: &ToolbarRow) -> Vec<(String, String, bool, Option<bool>)> {
    row.menu_entries
        .iter()
        .flatten()
        .filter(|entry| matches!(entry, MenuEntry::View(_)))
        .filter_map(|entry| row.entry_semantics(*entry))
        .collect()
}

#[test]
fn the_location_picks_the_view_from_the_width_its_narrower_form_fits_whole() {
    let at = (233..=900)
        .map(f64::from)
        .find(|width| calendar_row(*width).slots().views_in_location())
        .expect("a row wide enough for the period's narrower form makes it the views' picker");

    // Just narrower, the views are the overflow menu's.
    let mut below = calendar_row(at - 1.0);
    assert!(below.slots().views.is_empty() && below.slots().views_menu == Rect::ZERO);
    assert!(
        below.slots().location.area() > 0.0,
        "the period still stands"
    );
    assert!(below.open_overflow());
    let in_overflow = listed_views(&below);

    // At it, the location is the picker, and lists the same views.
    let mut picker = calendar_row(at);
    let slots = picker.slots().clone();
    assert_eq!(
        slots.views_menu, slots.location,
        "the location picks the view"
    );
    assert!(slots.views.is_empty(), "no switcher beside it");
    assert!(picker.press(slots.location.center()));
    assert!(picker.release(slots.location.center()));
    assert_eq!(picker.menu_labels(), ["Month", "Week", "Day"]);
    assert_eq!(
        listed_views(&picker),
        in_overflow,
        "one menu of views, wherever it opens"
    );
    assert_eq!(in_overflow.len(), 3);
    picker.menu.close();
    assert!(picker.open_overflow());
    assert!(
        listed_views(&picker).is_empty(),
        "the overflow menu does not list them again"
    );

    let nodes = picker.accessibility();
    let label = a_week().forms().first().cloned().unwrap_or_default();
    let node = find_node(&nodes, Role::Button, &label).expect("named by the period it shows");
    assert_eq!(
        node.value.as_deref(),
        Some("Week"),
        "valued by the chosen view"
    );
    assert_eq!(node.description.as_deref(), Some("View"));
    assert!(picker.perform(&format!("{VIEW_TARGET_PREFIX}day")));
    assert_eq!(
        picker.take_events(),
        vec![(3, ToolbarEvent::Action { id: "day".into() })]
    );
}

#[test]
fn today_says_its_name_where_the_row_has_room_and_folds_to_its_icon_then_the_menu() {
    const TODAY: usize = 2;
    let shown = |width: f64| {
        let row = calendar_row(width);
        let slots = row.slots();
        let rect = slots.leading[TODAY];
        if rect.area() <= 0.0 {
            "in the menu"
        } else if slots.labelled {
            assert!(
                rect.width() > CONTROL_PX,
                "a text button is wider than an icon"
            );
            "named"
        } else {
            assert_eq!(rect.width(), CONTROL_PX);
            "an icon"
        }
    };
    assert_eq!(shown(1280.0), "named", "a wide row names Today");
    assert_eq!(shown(233.0), "in the menu", "a compact row folds it");
    let order = ["in the menu", "an icon", "named"];
    let mut was = 0;
    let mut seen = Vec::new();
    for width in (233..=1280).map(f64::from) {
        let now = shown(width);
        let rank = order.iter().position(|form| *form == now).unwrap_or(0);
        assert!(
            rank >= was,
            "{width} px shows Today {now} after {}",
            order[was]
        );
        if rank > was || seen.is_empty() {
            seen.push(now);
        }
        was = rank;
    }
    assert_eq!(
        seen, order,
        "it narrows from its name to its icon to the menu"
    );
}

#[test]
fn a_wider_row_never_names_the_period_in_a_narrower_form() {
    let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 10, d).unwrap_or_default();
    for period in [
        a_week(),
        crate::format::Period::Day(day(21)),
        crate::format::Period::Month(day(21)),
    ] {
        let mut toolbar = calendar();
        toolbar.location = Some(ToolbarLocation::period(period));
        let forms = period.forms();
        let mut narrower: Option<(f64, usize)> = None;
        for width in (120..=1400).map(f64::from) {
            let row = calendar_row_of(&toolbar, width);
            let slots = row.slots();
            if slots.location.area() <= 0.0 {
                continue;
            }
            if width >= 233.0 {
                assert!(
                    slots.leading[..2].iter().all(|rect| rect.area() > 0.0),
                    "{width} px keeps the navigation for {:?}",
                    forms[slots.form.min(forms.len() - 1)],
                );
            }
            if let Some((at, form)) = narrower {
                assert!(
                    slots.form <= form,
                    "{width} px names {:?} as {:?}, narrower than {:?} at {at} px",
                    forms[0],
                    forms[slots.form.min(forms.len() - 1)],
                    forms[form.min(forms.len() - 1)],
                );
            }
            narrower = Some((width, slots.form));
        }
        assert_eq!(
            narrower.map(|(_, form)| form),
            Some(0),
            "a wide row names {:?} whole",
            forms[0]
        );
    }
}

#[test]
fn a_compact_row_folds_today_then_the_views_and_keeps_its_navigation_and_location() {
    // A compact window's row: the views go into the overflow menu before
    // previous and next do, and the location keeps at least its least width.
    let mut compact = calendar_row(COMPACT_CALENDAR_ROW);
    let slots = compact.slots().clone();
    assert!(slots.views.is_empty() && slots.views_menu == Rect::ZERO);
    assert!(slots.leading[0].area() > 0.0 && slots.leading[1].area() > 0.0);
    assert_eq!(slots.leading[2], Rect::ZERO, "Today folds");
    assert!(
        slots.location.width() >= MIN_LOCATION_PX,
        "{:?}",
        slots.location
    );
    assert!(slots.location.x0 >= slots.leading[1].x1);
    assert!(slots.overflow.x0 >= slots.location.x1);
    assert!(compact.open_overflow());
    let listed: Vec<MenuEntry> = compact.menu_entries.iter().flatten().copied().collect();
    assert_eq!(
        listed.get(..4),
        Some(
            &[
                MenuEntry::View(0),
                MenuEntry::View(1),
                MenuEntry::View(2),
                MenuEntry::Leading(2)
            ][..]
        ),
        "the views, then Today, run from the overflow menu"
    );
}

#[test]
fn a_narrow_row_folds_the_views_into_a_menu_of_every_view() {
    let mut row = calendar_row_of(&calendar_with_a_field(), 420.0);
    let slots = row.slots().clone();
    assert!(slots.views.is_empty(), "no segmented switcher");
    let menu = slots.views_menu;
    assert!(menu.area() > 0.0, "the views menu stands in the row");
    assert!(menu.x0 >= slots.leading[2].x1, "after the leading buttons");
    assert!(slots.overflow.area() > 0.0, "Import is the overflow menu's");
    assert!(row.open_overflow());
    assert!(
        row.menu_entries
            .iter()
            .all(|entry| !matches!(entry, Some(MenuEntry::View(_)))),
        "the overflow menu does not list the views again"
    );
    row.menu.close();

    assert!(row.press(menu.center()));
    assert!(row.release(menu.center()));
    assert_eq!(row.menu_labels(), ["Month", "Week", "Day"]);
    let nodes = row.accessibility();
    let views = find_node(&nodes, Role::Button, "View").expect("the views menu is a control");
    assert_eq!(views.value.as_deref(), Some("Week"));
    assert_eq!(views.state.expanded, Some(true));
    for label in ["Month", "Week", "Day"] {
        let item = find_node(&views.children, Role::MenuItem, label).expect("every view is listed");
        assert_eq!(item.state.checked, Some(label == "Week"));
    }
    assert!(row.perform(&format!("{VIEW_TARGET_PREFIX}day")));
    assert!(!row.menu.is_open(), "choosing closes the menu");
    assert_eq!(
        row.take_events(),
        vec![(3, ToolbarEvent::Action { id: "day".into() })]
    );
    assert!(
        row.perform(VIEWS_MENU_TARGET),
        "assistive technology opens it"
    );
    assert!(row.menu.is_open());

    let mut toolbar = calendar_with_a_field();
    if let Some(views) = toolbar.views.as_mut() {
        views.chosen = "day".into();
    }
    row.show(
        3,
        Some(&toolbar),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(0.0, 0.0, 420.0, 40.0));
    assert_eq!(row.slots().views_menu, menu, "another view, the same place");
}

#[test]
fn a_menu_button_lists_its_items_and_the_overflow_menu_carries_them() {
    let toolbar = WindowToolbar {
        actions: vec![
            ToolbarAction::new("format", "type", "Format").with_items(vec![
                ToolbarAction::new("bold", "bold", "Bold").on(true),
                ToolbarAction::new("italic", "italic", "Italic"),
            ]),
            ToolbarAction::new("export", "upload", "Export").in_menu(),
        ],
        ..WindowToolbar::default()
    }
    .bounded();
    let mut row = ToolbarRow::new();
    row.show(
        5,
        Some(&toolbar),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(0.0, 0.0, 800.0, 40.0));
    let format = row.slots().actions[0].center();
    assert!(row.press(format) && row.release(format));
    assert!(
        row.take_events().is_empty(),
        "a menu button sends nothing itself"
    );
    assert!(row.popup_open(), "its menu lists the items");
    assert_eq!(row.menu_entries.len(), 2);
    assert!(row.run_menu_entry(1));
    assert_eq!(
        row.take_events(),
        vec![(
            5,
            ToolbarEvent::Action {
                id: "italic".into()
            }
        )]
    );
    assert!(
        row.perform(&format!("{ACTION_TARGET_PREFIX}bold")),
        "AT runs an item"
    );
    assert_eq!(
        row.take_events(),
        vec![(5, ToolbarEvent::Action { id: "bold".into() })]
    );

    let mut narrow = ToolbarRow::new();
    let crowded = WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Cover letter".into(),
            ..ToolbarLocation::default()
        }),
        ..toolbar.clone()
    };
    narrow.show(
        5,
        Some(&crowded),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    narrow.lay_out(Rect::new(0.0, 0.0, 240.0, 40.0));
    assert!(
        narrow.slots().actions.is_empty(),
        "no room: Format goes in the menu"
    );
    assert!(narrow.open_overflow());
    assert!(
        narrow
            .menu_entries
            .iter()
            .flatten()
            .any(|entry| *entry == MenuEntry::Item(0, 1)),
        "the overflow menu carries Format's items"
    );
}

#[test]
fn a_destructive_action_folded_into_the_menu_keeps_its_danger_row() {
    let toolbar = WindowToolbar {
        actions: vec![
            ToolbarAction::new("mute", "mic", "Mute").in_menu(),
            ToolbarAction::new("leave", "phone-off", "Leave call")
                .destructive()
                .in_menu(),
        ],
        ..WindowToolbar::default()
    }
    .bounded();
    let mut row = ToolbarRow::new();
    row.show(
        5,
        Some(&toolbar),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(0.0, 0.0, 800.0, 40.0));
    assert!(row.open_overflow());
    let danger: Vec<bool> = row
        .menu
        .items()
        .iter()
        .filter_map(|item| match item {
            MenuItem::Action { destructive, .. } => Some(*destructive),
            _ => None,
        })
        .collect();
    assert_eq!(danger, vec![false, true], "only Leave is a danger row");
}

#[test]
fn a_sidebar_or_panel_toggle_shows_whether_its_panel_is_open() {
    for open in [true, false] {
        let toolbar = WindowToolbar {
            sidebar: Some(open),
            actions: vec![ToolbarAction::new("inspector", "panel-right", "Inspector").on(open)],
            ..WindowToolbar::default()
        }
        .bounded();
        let mut row = ToolbarRow::new();
        row.show(
            5,
            Some(&toolbar),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 600.0, 40.0));
        assert_eq!(
            shows_on(&toolbar, &Slot::Sidebar),
            open,
            "drawn on while open"
        );
        assert_eq!(
            shows_on(&toolbar, &Slot::Action(0)),
            open,
            "drawn on while open"
        );
        let nodes = row.accessibility();
        let sidebar_name = text(if open {
            "window-toolbar-hide-sidebar"
        } else {
            "window-toolbar-show-sidebar"
        });
        for name in [sidebar_name.as_str(), "Inspector"] {
            let toggle = find_node(&nodes, Role::ToggleButton, name)
                .unwrap_or_else(|| panic!("{name} is a toggle, open or closed"));
            assert_eq!(
                toggle.state.checked,
                Some(open),
                "{name} is pressed while open"
            );
        }
    }
}

/// A view with no sidebar keeps the toggle's place, disabled, so the
/// location stands where it stands in the views that have one.
#[test]
fn an_unavailable_sidebar_holds_its_place_and_cannot_act() {
    let row_for = |unavailable: bool| {
        let mut toolbar = address_book(false);
        toolbar.sidebar_unavailable = unavailable;
        let mut row = ToolbarRow::new();
        row.show(
            4,
            Some(&toolbar),
            Theme::dark().motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 420.0, 40.0));
        row
    };
    let available = row_for(false);
    let mut unavailable = row_for(true);
    assert_eq!(
        unavailable.slots().sidebar,
        available.slots().sidebar,
        "the toggle keeps its place"
    );
    assert_eq!(unavailable.slots().location, available.slots().location);

    let toggle = find_node(
        &unavailable.accessibility(),
        Role::ToggleButton,
        &text("window-toolbar-show-sidebar"),
    )
    .cloned()
    .expect("the toggle is still found");
    assert!(toggle.state.disabled, "and says it cannot act");
    assert!(!unavailable.perform(SIDEBAR_TARGET));
    let center = unavailable.slots().sidebar.center();
    unavailable.press(center);
    unavailable.release(center);
    assert!(
        unavailable.take_events().is_empty(),
        "neither a press nor assistive technology toggles it"
    );
}

/// The bars of a sync that failed (Sync again, Open Settings), a held save
/// failure (Try again) and an undoable deletion (Undo), each with its
/// notice up, and the names of the notice's buttons.
fn noticed() -> [(WindowToolbar, Vec<&'static str>); 3] {
    use crate::widget::toast::ToastKind;
    use crate::window_toolbar::ToolbarNotice;
    let sync = WindowToolbar {
        notice: Some(
            ToolbarNotice::new(
                "Sync failed: the account needs signing in",
                ToastKind::Error,
            )
            .action(ToolbarAction::new("sync-again", "refresh", "Sync again"))
            .action(ToolbarAction::new(
                "open-settings",
                "settings",
                "Open Settings",
            )),
        ),
        ..calendar()
    };
    let failed = WindowToolbar {
        notice: Some(
            ToolbarNotice::new("Save failed: the disk is full", ToastKind::Error).action(
                ToolbarAction::new("save-status-retry", "rotate-cw", "Try again"),
            ),
        ),
        ..calendar()
    };
    let deleted = WindowToolbar {
        notice: Some(
            ToolbarNotice::new("Moved 3 items to the trash", ToastKind::Info)
                .action(ToolbarAction::new("notice-undo", "undo-2", "Undo")),
        ),
        leading: vec![ToolbarAction::new(
            "new-folder",
            "folder-plus",
            "New folder",
        )],
        ..file_toolbar(3)
    };
    [
        (sync, vec!["Sync again", "Open Settings"]),
        (failed, vec!["Try again"]),
        (deleted, vec!["Undo"]),
    ]
}

/// Every node named `name` in `nodes` and under them.
fn named<'a>(nodes: &'a [AccessNode], name: &str, out: &mut Vec<&'a AccessNode>) {
    for node in nodes {
        if node.name.as_deref() == Some(name) {
            out.push(node);
        }
        named(&node.children, name, out);
    }
}

/// A notice's text stands whole before any action that yields to the
/// location stands beside it, and is cut short only once they are all in
/// the overflow menu. Its buttons stand in order right after the text at
/// every width: each names itself where the row has room for that beside
/// the whole text, from there on at every wider width, the last giving its
/// label up first, and is its icon where not; none is ever in the
/// overflow menu.
#[test]
fn a_notice_stands_whole_before_the_actions_and_its_buttons_stay_in_the_row() {
    for (toolbar, names) in noticed() {
        let mut labelled_from = vec![None; names.len()];
        // From the row of the narrowest window that says its notice in
        // the row (one at the compact breakpoint: its bar's row is 200 px
        // narrower, as a 420 px window's is 220 px) to a wide one's.
        // Narrower windows say it in a strip of their own.
        let narrowest = f64::from(crate::tokens::Metrics::BASE.compact_breakpoint) - 200.0;
        for width in (0..)
            .map(|step: u32| narrowest + 4.0 * f64::from(step))
            .take_while(|width| *width <= 1100.0)
        {
            let row = calendar_row_of(&toolbar, width);
            let slots = row.slots();
            assert_eq!(
                slots.notice.len(),
                names.len(),
                "{names:?} stand in a {width} px row: {slots:?}"
            );
            assert!(
                slots.location.area() > 0.0
                    && slots.notice[0].x0 >= slots.location.x1 + CONTROL_GAP - 1e-9
                    && slots
                        .notice
                        .windows(2)
                        .all(|pair| pair[1].x0 >= pair[0].x1 + CONTROL_GAP - 1e-9),
                "{names:?} stand in order after the notice's text at {width} px: {slots:?}"
            );
            let nodes = row.accessibility();
            for name in &names {
                let mut found = Vec::new();
                named(&nodes, name, &mut found);
                assert!(
                    !found.is_empty()
                        && found.iter().all(|node| node
                            .action_target
                            .as_deref()
                            .is_some_and(|target| target.starts_with(NOTICE_TARGET_PREFIX))),
                    "{name} is the notice's button alone, never an overflow item, at {width} px"
                );
            }
            if row.notice_whole() != Some(true) {
                assert!(
                    slots.actions.is_empty()
                        && slots
                            .leading
                            .iter()
                            .zip(&toolbar.leading)
                            .all(|(rect, action)| action.navigates || rect.area() <= 0.0),
                    "the notice is cut short at {width} px only with every plain action \
                     in the menu: {slots:?}"
                );
            }
            assert!(
                slots
                    .notice_labelled
                    .windows(2)
                    .all(|pair| pair[0] || !pair[1]),
                "the last gives its label up first at {width} px: {slots:?}"
            );
            for (i, name) in names.iter().enumerate() {
                let labelled = slots.notice_labelled[i];
                match (labelled, labelled_from[i]) {
                    (true, None) => labelled_from[i] = Some(width),
                    (false, Some(from)) => {
                        panic!("{name} named itself at {from} px and not at {width} px")
                    }
                    _ => {}
                }
                if !labelled {
                    assert!(
                        (slots.notice[i].width() - CONTROL_PX).abs() < 1e-9,
                        "folded, {name} is its icon"
                    );
                }
            }
        }
        assert!(
            labelled_from
                .iter()
                .all(|from| from.is_some_and(|from| from < 1100.0)),
            "{names:?} name themselves where the row has room"
        );
    }
}

/// In a compact window a notice says itself in a strip of its own under
/// the row: its whole text, read out as it arrives, and its actions as
/// buttons that name themselves, after the text where the whole text
/// stands beside them and on a row of their own under it where not, each
/// run by pointer and by its semantic action alike. The row stands as it
/// does without a notice.
#[test]
fn a_compact_windows_notice_has_a_strip_whose_buttons_run_by_pointer_and_semantics() {
    use crate::widget::toast::ToastKind;
    use crate::window_toolbar::ToolbarNotice;
    let theme = Theme::dark();
    let long = "Save failed: your disk is full, so the note was not written";
    let short = "Sync failed";
    let cases: [(&str, Vec<(&str, &str)>); 4] = [
        (long, vec![("Try again", "save-status-retry")]),
        (long, vec![("Undo", "notice-undo")]),
        (
            long,
            vec![
                ("Sync again", "sync-again"),
                ("Open Settings", "open-settings"),
            ],
        ),
        (
            short,
            vec![
                ("Sync again", "sync-again"),
                ("Open Settings", "open-settings"),
            ],
        ),
    ];
    for (text, actions) in cases {
        let notice = actions.iter().fold(
            ToolbarNotice::new(text, ToastKind::Error),
            |notice, (name, id)| notice.action(ToolbarAction::new(*id, "rotate-cw", *name)),
        );
        let noticed = WindowToolbar {
            notice: Some(notice),
            ..file_toolbar(3)
        };
        let mut quiet = ToolbarRow::new();
        quiet.show(
            3,
            Some(&file_toolbar(3)),
            theme.motion(),
            crate::motion::frame_now(),
        );
        quiet.lay_out(Rect::new(0.0, 0.0, 220.0, 40.0));
        let mut row = ToolbarRow::new();
        row.set_compact(true);
        row.show(
            3,
            Some(&noticed),
            theme.motion(),
            crate::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 220.0, 40.0));
        assert_eq!(
            row.slots(),
            quiet.slots(),
            "the row stands as it does without a notice"
        );
        assert!(row.measure_strip(&theme, 404.0), "the strip is measured");
        let height = row.strip_height();
        assert!(height >= CONTROL_PX, "{height}");
        let strip = Rect::new(8.0, 44.0, 412.0, 44.0 + height);
        row.place_strip(strip);
        let (shown, text_at, whole, buttons) = row.strip_notice().expect("the strip says it");
        assert_eq!(shown.text, text);
        assert!(whole, "the whole text stands, on up to two lines: {text}");
        assert_eq!(buttons.len(), actions.len());
        for (button, (name, _)) in buttons.iter().zip(&actions) {
            assert!(
                button.width() > CONTROL_PX
                    && strip.contains(button.center())
                    && (text_at.x1 <= button.x0 || text_at.y1 <= button.y0),
                "{name} names itself after the text or under it: {text_at:?} {button:?}"
            );
        }
        let nodes = row.accessibility();
        let said = nodes
            .iter()
            .find(|node| node.name.as_deref() == Some(text))
            .expect("the strip's text is a node");
        assert_eq!(said.live, Some(crate::a11y::Politeness::Polite));
        for (button, (name, id)) in buttons.iter().zip(&actions) {
            let target = format!("{NOTICE_TARGET_PREFIX}{id}");
            assert!(
                nodes.iter().any(|node| node.name.as_deref() == Some(*name)
                    && node.action_target.as_deref() == Some(target.as_str())),
                "{name} is the notice's button"
            );
            let pressed = vec![(3, ToolbarEvent::Action { id: (*id).into() })];
            assert!(row.press(button.center()));
            assert!(row.release(button.center()));
            assert_eq!(row.take_events(), pressed, "{name} runs by pointer");
            assert!(row.perform(&target));
            assert_eq!(row.take_events(), pressed, "{name} runs by its action");
        }
    }
}
