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
            ToolbarAction::new("today", "calendar", "Today"),
        ],
        location: Some(ToolbarLocation {
            label: "Oct 19 – 25, 2026".into(),
            ..ToolbarLocation::default()
        }),
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

fn calendar_row(width: f64) -> ToolbarRow {
    let mut row = ToolbarRow::new();
    row.show(
        3,
        Some(&calendar()),
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
    let one = Slots::lay_out(&file_toolbar(1), compact, None);
    assert_eq!(one.actions.len(), 1, "the action stands in the row");
    assert_eq!(one.overflow, Rect::ZERO, "no menu for one action");

    for declared in [2, 3] {
        let slots = Slots::lay_out(&file_toolbar(declared), compact, None);
        let folded = declared - slots.actions.len();
        assert!(
            folded == 0 || folded >= 2,
            "{declared} actions: the menu holds {folded}"
        );
        assert_eq!(slots.overflow.area() > 0.0, folded > 0);
    }

    let wide = Slots::lay_out(&file_toolbar(1), Rect::new(0.0, 0.0, 900.0, 40.0), None);
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
    let widths: Vec<f64> = slots.views.iter().map(Rect::width).collect();
    assert!(
        widths.iter().all(|w| (w - widths[0]).abs() < 0.5),
        "{widths:?}"
    );
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

#[test]
fn a_narrow_row_folds_today_then_the_views_and_keeps_its_navigation_and_location() {
    // Room for previous and next, the location at its least and the menu:
    // Today, which jumps rather than steps, folds first.
    let mut row = calendar_row(293.0);
    let slots = row.slots().clone();
    assert!(
        slots.views_menu.area() > 0.0,
        "the views menu stands in the row"
    );
    assert!(slots.leading[0].area() > 0.0 && slots.leading[1].area() > 0.0);
    assert_eq!(slots.leading[2], Rect::ZERO, "Today folds");
    assert!(
        slots.location.width() >= MIN_LOCATION_PX,
        "{:?}",
        slots.location
    );
    assert!(slots.location.x0 >= slots.leading[1].x1);
    assert!(slots.views_menu.x0 >= slots.location.x1);
    assert!(slots.overflow.x0 >= slots.views_menu.x1);
    assert!(row.open_overflow());
    assert_eq!(
        row.menu_entries.first().copied().flatten(),
        Some(MenuEntry::Leading(2)),
        "the overflow menu starts with Today"
    );
    assert!(row.perform(&format!("{LEADING_TARGET_PREFIX}today")));
    assert_eq!(
        row.take_events(),
        vec![(3, ToolbarEvent::Action { id: "today".into() })]
    );

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
    let mut row = calendar_row(420.0);
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

    let mut toolbar = calendar();
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
