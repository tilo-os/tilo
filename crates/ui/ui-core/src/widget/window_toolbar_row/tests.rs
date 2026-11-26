use super::*;
use crate::window_toolbar::{ToolbarAction, ToolbarLocation, ToolbarNavigation};
use tilo_ui_base::{a11y::AccessNode, a11y::Role};

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
            primary: false,
            ..ToolbarAction::default()
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
        tilo_ui_base::motion::frame_now(),
    );
    row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
    row
}

tilo_test_env::scenario! {
    fn a_notice_stands_in_the_row_and_its_buttons_run;

    phase a_notice_stands_in_the_location_and_is_read_out_politely;
    {
        use crate::widget::toast::ToastKind;
        use crate::window_toolbar::ToolbarNotice;
        for width in [640.0, 420.0 - 40.0] {
            let bounds = Rect::new(40.0, 0.0, 40.0 + width, 40.0);
            let mut quiet = ToolbarRow::new();
            quiet.show(
                7,
                Some(&browser()),
                Theme::dark().motion(),
                tilo_ui_base::motion::frame_now(),
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
                tilo_ui_base::motion::frame_now(),
            );
            row.lay_out(bounds);
            assert_eq!(
                row.parts().location,
                quiet.parts().location,
                "the notice reserves no area of its own at {width}"
            );
            assert!(row.parts().location.width() > 0.0);
            let nodes = row.accessibility();
            let said = nodes
                .iter()
                .find(|node| node.name.as_deref() == Some("Saved report.pdf"))
                .expect("the notice is the location's text");
            assert_eq!(said.live, Some(tilo_ui_base::a11y::Politeness::Polite));
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
            tilo_ui_base::motion::frame_now(),
        );
        row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
        assert!(row.measure_views(&Theme::dark()), "the notice is measured");
        let parts = row.parts();
        assert!(
            parts.notice_labelled == [true] && parts.notice[0].width() > CONTROL_PX,
            "Undo names itself in a wide row: {parts:?}"
        );
        assert!(
            (parts.notice[0].x0 - parts.location.x1 - CONTROL_GAP).abs() < 1e-9,
            "right after the notice's text"
        );
        assert_eq!(row.parts().actions.len(), 1, "and is not among the actions");
        let button = row.parts().notice[0].center();
        assert!(row.press(button, false));
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


    phase a_compact_windows_notice_has_a_strip_whose_buttons_run_by_pointer_and_semantics;
    {
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
                tilo_ui_base::motion::frame_now(),
            );
            quiet.lay_out(Rect::new(0.0, 0.0, 220.0, 40.0));
            let mut row = ToolbarRow::new();
            row.set_compact(true);
            row.show(
                3,
                Some(&noticed),
                theme.motion(),
                tilo_ui_base::motion::frame_now(),
            );
            row.lay_out(Rect::new(0.0, 0.0, 220.0, 40.0));
            assert_eq!(
                row.parts(),
                quiet.parts(),
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
            assert_eq!(said.live, Some(tilo_ui_base::a11y::Politeness::Polite));
            for (button, (name, id)) in buttons.iter().zip(&actions) {
                let target = format!("{NOTICE_TARGET_PREFIX}{id}");
                assert!(
                    nodes.iter().any(|node| node.name.as_deref() == Some(*name)
                        && node.action_target.as_deref() == Some(target.as_str())),
                    "{name} is the notice's button"
                );
                let pressed = vec![(3, ToolbarEvent::Action { id: (*id).into() })];
                assert!(row.press(button.center(), false));
                assert!(row.release(button.center()));
                assert_eq!(row.take_events(), pressed, "{name} runs by pointer");
                assert!(row.perform(&target));
                assert_eq!(row.take_events(), pressed, "{name} runs by its action");
            }
        }
    }
}

tilo_test_env::scenario! {
    fn navigation_and_the_location_stand_in_the_row_and_say_where_the_window_is;


    phase back_runs_only_where_it_can_and_forward_not_at_the_end;
    {
        let mut row = row();
        let back = row.parts().back.center();
        assert!(row.press(back, false));
        assert!(row.release(back));
        let forward = row.parts().forward.center();
        assert!(row.press(forward, false));
        assert!(row.release(forward));
        let reload = row.parts().actions[0].center();
        assert!(row.press(reload, false));
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

    phase a_click_on_the_location_edits_the_whole_address_and_esc_puts_it_back;
    {
        let mut row = row();
        let location = row.parts().location.center();
        assert!(row.press(location, false));
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

    phase a_location_with_choices_opens_its_picker_and_commits_the_one_chosen;
    {
        let mut row = ToolbarRow::new();
        row.show(
            9,
            Some(&terminal()),
            Theme::dark().motion(),
            tilo_ui_base::motion::frame_now(),
        );
        row.set_popup_area(Rect::new(0.0, 0.0, 800.0, 600.0));
        row.lay_out(Rect::new(40.0, 0.0, 640.0, 40.0));
        let location = row.parts().location.center();
        assert!(row.press(location, false));
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

    phase an_edited_document_says_so_after_its_label_and_to_assistive_technology;
    {
        let document = |edited: bool| ToolbarLocation {
            label: "Harbour cut".into(),
            edited,
            ..Default::default()
        };
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let saved = LocationLabel::measure(&mut tr, &theme, "Harbour cut", None, 400.0);
        let edited = LocationLabel::measure(&mut tr, &theme, "Harbour cut", Some(edited_word()), 400.0);
        assert!(saved.edited.is_none());
        let (word, after) = edited.edited.clone().expect("an edited document says so");
        assert_eq!(word, text("window-toolbar-edited"));
        assert!(after > saved.width, "the word follows the whole label");
        assert!(edited.width > saved.width);
        assert_eq!(edited.line, saved.line, "the label itself is unchanged");
        // Too narrow for both: the label keeps the width.
        let narrow = LocationLabel::measure(&mut tr, &theme, "Harbour cut", Some(edited_word()), 30.0);
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
                tilo_ui_base::motion::frame_now(),
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


    phase another_tab_cross_fades_and_drops_what_was_typed_in_this_one;
    {
        let mut row = row();
        assert!(row.begin_edit());
        let now = crate::time::now();
        row.show(8, None, Theme::dark().motion(), now);
        assert!(!row.is_editing());
        assert!(row.toolbar().is_none());
        assert!(row.needs_animation_frame(), "the old parts fade out");
    }
}

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

fn text(key: &str) -> String {
    tilo_ui_base::i18n::I18nManager::lookup_plain(key)
}

fn find_node<'a>(nodes: &'a [AccessNode], role: Role, name: &str) -> Option<&'a AccessNode> {
    nodes.iter().find_map(|node| {
        if node.role == role && node.name.as_deref() == Some(name) {
            Some(node)
        } else {
            find_node(&node.children, role, name)
        }
    })
}

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
        tilo_ui_base::motion::frame_now(),
    );
    row.set_popup_area(Rect::new(0.0, 0.0, 1200.0, 800.0));
    row.lay_out(Rect::new(0.0, 0.0, width, 40.0));
    row
}

tilo_test_env::scenario! {
    fn the_rows_actions_stand_fold_into_the_menu_and_run;

    phase every_part_and_overflow_item_is_found_by_role_and_name_and_runs;
    {
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
            .expect("what the row cannot fit is the overflow menu's");
        assert!(
            import.state.hidden,
            "a closed menu's item is hidden from a reader"
        );
        assert!(
            find_node(&wide, Role::Button, "Import contacts").is_none(),
            "an item in the menu is not also a button in the row"
        );
        assert_ne!(new.bounds, more.bounds);

        let mut narrow = address_book_row(false, 120.0);
        assert!(narrow.parts().actions.is_empty(), "no area for any action");
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

        let mut editing = address_book_row(true, 240.0);
        let nodes = editing.accessibility();
        let save = find_node(&nodes, Role::MenuItem, "Save")
            .or_else(|| find_node(&nodes, Role::Button, "Save"))
            .expect("Save is reachable");
        assert!(
            find_node(&nodes, Role::MenuItem, "Cancel")
                .or_else(|| find_node(&nodes, Role::Button, "Cancel"))
                .is_some()
        );
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


    phase a_menu_button_lists_its_items_and_the_overflow_menu_carries_them;
    {
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
            tilo_ui_base::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 800.0, 40.0));
        let format = row.parts().actions[0].center();
        assert!(row.press(format, false) && row.release(format));
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
            leading: vec![
                ToolbarAction::new("previous", "chevron-left", "Previous").navigating(),
                ToolbarAction::new("next", "chevron-right", "Next").navigating(),
            ],
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
            tilo_ui_base::motion::frame_now(),
        );
        narrow.lay_out(Rect::new(0.0, 0.0, 100.0, 40.0));
        assert!(
            narrow.parts().actions.is_empty(),
            "no area: Format goes in the menu"
        );
        assert!(narrow.open_overflow());
        assert!(
            narrow
                .menu_entries
                .iter()
                .flatten()
                .any(|entry| *entry == MenuEntry::Item("format".into(), 1)),
            "the overflow menu carries Format's items"
        );
    }

    phase the_shells_items_follow_the_apps_commands_in_one_menu;
    {
        let toolbar = WindowToolbar {
            actions: vec![
                ToolbarAction::new("notes-menu-rename", "pencil", "Rename").in_menu(),
                ToolbarAction::new("tilo.shell.share", "copy", "Share to channel…").in_menu(),
            ],
            ..WindowToolbar::default()
        }
        .bounded();
        let mut row = ToolbarRow::new();
        row.show(
            5,
            Some(&toolbar),
            Theme::dark().motion(),
            tilo_ui_base::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 800.0, 40.0));
        assert!(row.open_overflow(), "the window's More menu holds both");
        assert_eq!(
            row.menu_entries,
            vec![Some(MenuEntry::Action(0)), None, Some(MenuEntry::Action(1))],
            "the shell's item stands in a section after the app's commands"
        );
    }

    phase a_destructive_action_folded_into_the_menu_keeps_its_danger_row;
    {
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
            tilo_ui_base::motion::frame_now(),
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

    phase a_sidebar_or_panel_toggle_shows_whether_its_panel_is_open;
    {
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
                tilo_ui_base::motion::frame_now(),
            );
            row.lay_out(Rect::new(0.0, 0.0, 600.0, 40.0));
            assert_eq!(
                shows_on(&toolbar, &Part::Sidebar),
                open,
                "drawn on while open"
            );
            assert_eq!(
                shows_on(&toolbar, &Part::Action(0)),
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

    phase an_unavailable_sidebar_holds_its_place_and_cannot_act;
    // A view with no sidebar keeps the toggle's place, disabled, so the
    // location stands where it stands in the views that have one.
    {
        let row_for = |unavailable: bool| {
            let mut toolbar = address_book(false);
            toolbar.sidebar_unavailable = unavailable;
            let mut row = ToolbarRow::new();
            row.show(
                4,
                Some(&toolbar),
                Theme::dark().motion(),
                tilo_ui_base::motion::frame_now(),
            );
            row.lay_out(Rect::new(0.0, 0.0, 420.0, 40.0));
            row
        };
        let available = row_for(false);
        let mut unavailable = row_for(true);
        assert_eq!(
            unavailable.parts().sidebar,
            available.parts().sidebar,
            "the toggle keeps its place"
        );
        assert_eq!(unavailable.parts().location, available.parts().location);

        let toggle = find_node(
            &unavailable.accessibility(),
            Role::ToggleButton,
            &text("window-toolbar-show-sidebar"),
        )
        .cloned()
        .expect("the toggle is still found");
        assert!(toggle.state.disabled, "and says it cannot act");
        assert!(!unavailable.perform(SIDEBAR_TARGET));
        let center = unavailable.parts().sidebar.center();
        unavailable.press(center, false);
        unavailable.release(center);
        assert!(
            unavailable.take_events().is_empty(),
            "neither a press nor assistive technology toggles it"
        );
    }

}

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

fn a_week() -> tilo_ui_base::format::Period {
    let day = |d| jiff::civil::Date::new(2026, 10, d).ok().unwrap_or_default();
    tilo_ui_base::format::Period::Days(day(19), day(25))
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
        tilo_ui_base::motion::frame_now(),
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

tilo_test_env::scenario! {
    fn the_views_and_the_period_fold_as_the_row_narrows;



    phase a_view_is_chosen_in_the_row_or_from_the_menu_when_the_row_is_too_small;
    {
        let mut row = calendar_row(1000.0);
        let month = row.parts().views[0].center();
        assert!(row.press(month, false));
        assert!(row.release(month));
        let today = row.parts().leading[2].center();
        assert!(row.press(today, false));
        assert!(row.release(today));
        assert_eq!(
            row.take_events(),
            vec![
                (3, ToolbarEvent::Action { id: "month".into() }),
                (3, ToolbarEvent::Action { id: "today".into() }),
            ]
        );

        let mut narrow = calendar_row(120.0);
        assert!(
            narrow.parts().views.is_empty() && narrow.parts().views_menu == Rect::ZERO,
            "no area even for the views menu: the views go in the overflow menu"
        );
        assert!(narrow.parts().overflow.area() > 0.0);
        assert!(narrow.open_overflow());
        assert_eq!(
            listed_views(&narrow).first().map(|(name, ..)| name.as_str()),
            Some("Month"),
            "the sidebar yields before the periodic views do; the views still run from the menu"
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



    phase a_primary_action_stands_where_a_secondary_one_folds;
    {
        let mut toolbar = calendar();
        toolbar.actions = vec![
            ToolbarAction::new("upload", "upload", "Upload files").primary(),
            ToolbarAction::new("new-folder", "folder-plus", "New folder"),
            ToolbarAction::new("new-file", "file-plus", "New file"),
            ToolbarAction::new("import", "download", "Import").in_menu(),
        ];
        for width in [360.0, 420.0] {
            let parts = calendar_row_of(&toolbar, width);
            let parts = parts.parts();
            assert!(
                parts.actions.first().is_some_and(|rect| rect.area() > 0.0),
                "{width} px keeps the window's main action in the row"
            );
        }
    }

    phase a_narrow_row_folds_the_views_into_a_menu_of_every_view;
    {
        let mut row = calendar_row_of(&calendar_with_a_field(), 420.0);
        let parts = row.parts().clone();
        assert!(parts.views.is_empty(), "no segmented switcher");
        let menu = parts.views_menu;
        assert!(menu.area() > 0.0, "the views menu stands in the row");
        assert!(menu.x0 >= parts.leading[2].x1, "after the leading buttons");
        assert!(parts.overflow.area() > 0.0, "Import is the overflow menu's");
        assert!(row.open_overflow());
        assert!(
            row.menu_entries
                .iter()
                .all(|entry| !matches!(entry, Some(MenuEntry::View(_)))),
            "the overflow menu does not list the views again"
        );
        row.menu.close();

        assert!(row.press(menu.center(), false));
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
            tilo_ui_base::motion::frame_now(),
        );
        row.lay_out(Rect::new(0.0, 0.0, 420.0, 40.0));
        assert_eq!(row.parts().views_menu, menu, "another view, the same place");
    }
}

/// A row with no width for the switcher folds the views into a menu that
/// names the chosen view: a press (a click or a tap) lists every view, the
/// chosen one checked, and choosing one hands the app its id. The menu is
fn listed_views(row: &ToolbarRow) -> Vec<(String, String, bool, Option<bool>)> {
    row.menu_entries
        .iter()
        .flatten()
        .filter(|entry| matches!(entry, MenuEntry::View(_)))
        .filter_map(|entry| row.entry_semantics(entry.clone()))
        .collect()
}

fn changed_to(row: &mut ToolbarRow, toolbar: &WindowToolbar, theme: &Theme) {
    let rect = row.row;
    row.show(
        7,
        Some(toolbar),
        theme.motion(),
        tilo_ui_base::motion::frame_now(),
    );
    row.lay_out(rect);
    row.start_part_change(theme);
}

fn later(t0: crate::time::Instant) -> crate::time::Instant {
    t0.checked_add(crate::time::Duration::from_secs(2))
        .unwrap_or(t0)
}

tilo_test_env::scenario! {
    fn a_changed_part_fades_and_the_parts_it_moves_glide;

    phase a_part_the_app_adds_fades_in_and_the_parts_it_pushes_glide;
    {
        use crate::time::testing::with_monotonic_time;
        let theme = Theme::dark();
        let t0 = crate::time::now();
        let mut row = with_monotonic_time(t0, row);
        let back_before = row.parts().back;
        let mut with_sidebar = browser();
        with_sidebar.sidebar = Some(false);
        with_monotonic_time(t0, || {
            changed_to(&mut row, &with_sidebar, &theme);
            let back = row.parts().back;
            assert!(back.x0 > back_before.x0, "the sidebar pushed back along");
            assert!(row.needs_animation_frame(), "the change moves");
            let poses = row.part_changes.poses(t0);
            let (opacity, _) = poses[&part_change::PartKey::Sidebar];
            assert!(opacity < 0.05, "the sidebar fades in from clear: {opacity}");
            let (_, offset) = poses[&part_change::PartKey::Back];
            assert!(
                (back.x0 + offset.x - back_before.x0).abs() < 0.5,
                "back starts where it stood"
            );
            assert_eq!(
                row.part_at(back_before.center()),
                Some(Part::Back),
                "back is pressed where it is drawn"
            );
        });
        let end = later(t0);
        with_monotonic_time(end, || {
            assert!(!row.needs_animation_frame(), "and settles");
            row.start_part_change(&theme);
            assert!(row.part_changes.poses(end).is_empty());
            assert_eq!(row.part_at(row.parts().back.center()), Some(Part::Back));
            assert_eq!(
                row.part_at(row.parts().sidebar.center()),
                Some(Part::Sidebar)
            );
        });
    }


}

#[test]
fn a_change_a_key_just_asked_for_lands_and_one_of_state_moves_nothing() {
    use crate::time::testing::with_monotonic_time;
    let theme = Theme::dark();
    let t0 = crate::time::now();
    let mut row = with_monotonic_time(t0, row);
    let mut with_sidebar = browser();
    with_sidebar.sidebar = Some(false);
    with_monotonic_time(t0, || {
        tilo_ui_base::motion::note_source(MotionSource::Keyboard);
        changed_to(&mut row, &with_sidebar, &theme);
        assert!(!row.needs_animation_frame(), "a key's change lands");
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
        let mut opened = with_sidebar.clone();
        opened.sidebar = Some(true);
        changed_to(&mut row, &opened, &theme);
        assert!(
            !row.needs_animation_frame(),
            "the sidebar opening moves no part"
        );
    });
}

#[path = "reactions_tests.rs"]
mod reactions;

tilo_test_env::scenario! {
    fn the_row_says_what_a_person_can_do_and_moves_only_what_they_moved;

    // A window the person may only look at says so after its label, where an editable one says
    // it was edited.
    phase a_view_only_window_says_so_after_its_label;
    {
        let toolbar = WindowToolbar {
            location: Some(ToolbarLocation {
                label: "Plan".into(),
                edited: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let shown = crate::window_toolbar::with_window_owned(Some(toolbar), None, None, true, false)
            .and_then(|toolbar| toolbar.location)
            .expect("the location stays");
        assert!(shown.view_only);
        assert_eq!(trailing_word(&shown), Some(text("window-toolbar-view-only")));
        let editable = ToolbarLocation {
            view_only: false,
            ..shown
        };
        assert_eq!(trailing_word(&editable), Some(text("window-toolbar-edited")));
    }

    // A compact row never hides a lone action behind a menu: one action stands in the row, and
    // a menu opens only for two or more.
    phase a_compact_row_never_folds_one_action_alone_into_the_menu;
    {
        for (width, declared) in [(220.0, 1), (220.0, 2), (220.0, 3), (900.0, 1)] {
            let parts = Parts::lay_out(
                &file_toolbar(declared),
                Rect::new(0.0, 0.0, width, 40.0),
                None,
                (&LocationForms::default(), &[], &[]),
            );
            let folded = declared - parts.actions.len();
            assert!(folded == 0 || folded >= 2, "{width}, {declared}: the menu holds {folded}");
            assert_eq!(parts.overflow.area() > 0.0, folded > 0, "{width}, {declared}");
        }
    }

    // A part the app takes away fades out where it stood and takes no press on its way; under
    // reduced motion the parts that move land at once, and what arrives still fades in.
    phase a_part_taken_away_fades_where_it_stood_and_reduced_motion_lands_the_rest;
    {
        use crate::time::testing::with_monotonic_time;
        let theme = Theme::dark();
        let t0 = crate::time::now();
        let mut taken = with_monotonic_time(t0, row);
        let reload = taken.parts().actions[0];
        let mut without_reload = browser();
        without_reload.actions.clear();
        with_monotonic_time(t0, || {
            changed_to(&mut taken, &without_reload, &theme);
            let leaving = taken.part_changes.leaving(t0);
            assert_eq!(leaving.len(), 1, "reload fades out");
            assert_eq!(leaving[0].rect, reload, "where it stood");
            assert!(leaving[0].opacity > 0.95, "from whole");
            assert_ne!(taken.part_at(reload.center()), Some(Part::Action(0)), "and takes no press");
        });
        let end = later(t0);
        with_monotonic_time(end, || {
            taken.start_part_change(&theme);
            assert!(taken.part_changes.leaving(end).is_empty(), "and is gone");
            assert!(!taken.needs_animation_frame());
        });

        let still = Theme::dark().with_reduce_motion(true);
        let mut row = with_monotonic_time(t0, row);
        let mut with_sidebar = browser();
        with_sidebar.sidebar = Some(false);
        with_monotonic_time(t0, || {
            changed_to(&mut row, &with_sidebar, &still);
            let poses = row.part_changes.poses(t0);
            assert!(!poses.contains_key(&part_change::PartKey::Back), "back lands at once");
            assert!(poses[&part_change::PartKey::Sidebar].0 < 0.05, "the sidebar fades in");
        });
    }
}
