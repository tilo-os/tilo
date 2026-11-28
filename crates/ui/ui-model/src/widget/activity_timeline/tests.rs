use super::*;
use crate::widget::history::HistoryAuthor;
use automerge::AutoCommit;
use collab_types::ArtifactKind;
use std::cell::RefCell;
use std::rc::Rc;
use tilo_ui::testing::dispatch;
use tilo_ui::time::Duration;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{synthetic_input::cursor_moved, synthetic_input::mouse_press};
use winit::event::ElementState;

fn entry(op: EntryKind, author: &str, ts: u64, id: u8) -> HistoryEntry {
    let mut id_bytes = [0u8; 16];
    id_bytes[0] = id;
    HistoryEntry {
        id: HistoryEntryId(id_bytes),
        author: HistoryAuthor::new(author, 120.0, "deadbeef"),
        entry_kind: op,
        artifact_kind: ArtifactKind::Note,
        timestamp: SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(ts))
            .unwrap_or(SystemTime::UNIX_EPOCH),
        summary: format!("op {id} by {author}"),
        text_delta: Some((1, 0)),
        diff_payload: None,
        redaction: None,
        context: None,
        target: None,
    }
}

fn timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
    let mut t = ActivityTimeline::with_entries(entries);
    t.rect = rect;
    t.set_now_for_test(
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2_000_000))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    );
    t.set_newest_first(false); // ascending order for clarity
    t
}

// Geometry and naming-input tests explicitly install the capabilities they
// exercise; the ordinary fixture remains a read-only history viewer.
fn editable_timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
    timeline_at(rect, entries)
        .on_revert(|_| {})
        .on_name_group(|_| {})
}

/// A few changes by one person, laid out by `lay_out_in`: the fixtures
/// below read what the layout placed, never what a paint drew.
fn placed_timeline(theme: &Theme) -> ActivityTimeline {
    let entries: Vec<HistoryEntry> = (0..8u8)
        .map(|i| entry(EntryKind::Text, "Alice", 1_000 + u64::from(i) * 30, i))
        .collect();
    let mut t = ActivityTimeline::with_entries(entries)
        .on_revert(|_| {})
        .on_name_group(|_| {});
    t.set_now_for_test(
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2_000))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    );
    t.set_newest_first(false);
    lay_out(&mut t, theme);
    t
}

fn lay_out(t: &mut ActivityTimeline, theme: &Theme) {
    tilo_ui::testing::lay_out_in(t, 620.0, 560.0, theme).expect("the pane lays out");
}

fn click(t: &mut ActivityTimeline, p: Point) {
    dispatch(t, &cursor_moved(p.x, p.y));
    dispatch(t, &mouse_press());
}

/// Only the pub fields the widget reads are overwritten.
fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
    tilo_ui_base::synthetic_input::keyboard_input(tilo_ui_base::synthetic_input::key_event(
        logical_key,
        None,
        ElementState::Pressed,
    ))
}

tilo_test_env::scenario! {
    /// Filters, selection and the empty states of the activity timeline.
    /// Each phase was a test of its own and builds its own fixture.
    fn timeline_filters_and_selection_narrow_and_explain_the_list;

    phase a_selection_and_a_name_place_the_controls_in_the_layout;
    // A control's place is the layout's: a frame that changed its selection
    // or began a name and has laid out but not yet painted still finds the
    // strip's author pill and the bar's actions inside the pane, where it
    // would draw them — not nowhere, at the window's origin, where a control
    // nothing placed stands.
    {
        let theme = Theme::dark();
        let mut t = placed_timeline(&theme);

        let pill = t.author_pill_rect();
        let strip = t.filter_strip_rect();
        assert!(
            !pill.is_zero_area(),
            "the layout places the author pill: {pill:?}"
        );
        assert!(
            strip.contains(pill.center()),
            "inside the strip: {pill:?} in {strip:?}"
        );
        assert!(!t.is_layout_dirty(), "the layout that placed it answers");

        t.set_selection(Some((2, 4)));
        assert!(
            t.is_layout_dirty(),
            "a selection moves the bar's controls, so the layout runs again"
        );
        lay_out(&mut t, &theme);
        let bar = t.selection_bar_rect();
        let clear = t.clear_selection_rect();
        let name = t.name_selection_rect();
        assert!(!bar.is_zero_area(), "the bar is up: {bar:?}");
        assert!(
            !clear.is_zero_area(),
            "the layout places its way out: {clear:?}"
        );
        assert!(
            bar.contains(clear.center()),
            "{clear:?} is in the bar {bar:?}"
        );
        assert!(!name.is_zero_area(), "and its name action: {name:?}");
        assert!(
            bar.contains(name.center()),
            "{name:?} is in the bar {bar:?}"
        );
        assert!(
            clear.intersect(name).is_zero_area(),
            "the two actions never claim the same pixels"
        );
        assert!(!t.is_layout_dirty(), "the layout placed them");

        click(&mut t, clear.center());
        assert_eq!(t.selection(), None, "the press cleared the placed bar");

        t.set_selection(Some((2, 4)));
        lay_out(&mut t, &theme);
        assert!(t.begin_naming_selection(), "a selection can be named");
        assert!(
            t.is_layout_dirty(),
            "a name being typed changes the bar's controls"
        );
        lay_out(&mut t, &theme);
        assert_eq!(
            t.name_selection_rect(),
            Rect::ZERO,
            "the field stands where the action did"
        );
        assert!(
            !t.clear_selection_rect().is_zero_area(),
            "the way out stays: {:?}",
            t.clear_selection_rect()
        );
        assert!(!t.is_layout_dirty(), "and the layout answers again");
    }

    phase a_filter_with_nothing_left_places_the_way_back_out_in_the_layout;
    {
        let theme = Theme::dark();
        let mut t = placed_timeline(&theme);
        assert_eq!(
            t.clear_filters_rect(),
            Rect::ZERO,
            "a list with rows offers no way back out"
        );

        t.set_author_query("nobody");
        assert!(
            t.is_layout_dirty(),
            "a filter that empties the list changes what the pane shows"
        );
        lay_out(&mut t, &theme);
        let chip = t.clear_filters_rect();
        let pane = Widget::get_rect(&t);
        assert!(
            !chip.is_zero_area(),
            "the layout places the way back out: {chip:?}"
        );
        assert!(
            pane.contains(chip.center()),
            "{chip:?} in the pane {pane:?}"
        );
        assert!(
            chip.y0 >= t.filter_strip_rect().y1,
            "under the filter strip: {chip:?}"
        );

        click(&mut t, chip.center());
        assert_eq!(t.author_query(), "", "the press cleared the filters");
    }

    phase empty_and_filtered_timelines_explain_themselves;
    {
        fn an_empty_history_and_a_filtered_one_explain_themselves_differently() {
            let empty = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), vec![]);
            let (title, detail) = empty.empty_state_lines();
            assert_eq!(title, "No activity yet");
            assert!(detail.is_none(), "nothing happened; nothing to explain");
            assert!(!empty.is_filtered());

            let mut t = timeline_at(
                Rect::new(0.0, 0.0, 400.0, 300.0),
                vec![
                    entry(EntryKind::Text, "Alice", 1, 1),
                    entry(EntryKind::Text, "Bob", 2, 2),
                ],
            );
            t.set_active_chip(ChipKind::Comments);
            assert_eq!(t.visible_len(), 0);
            assert!(t.is_filtered());
            let (title, detail) = t.empty_state_lines();
            assert_eq!(title, "No activity matches these filters");
            let detail = detail.expect("a filtered-empty list must say why");
            assert!(
                detail.contains('2') && detail.contains("hidden"),
                "counts what it is hiding: {detail}"
            );
            assert!(detail.contains("Comments"), "names the filter: {detail}");
        }

        fn a_host_time_range_is_named_in_the_explanation() {
            // The widget cannot put a `SystemTime` pair into the user's words,
            // so the host labels it.
            let mut t = timeline_at(
                Rect::new(0.0, 0.0, 400.0, 300.0),
                vec![entry(EntryKind::Text, "Alice", 1, 1)],
            );
            t.set_filter(HistoryFilter {
                time_range: Some((
                    SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(500))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(600))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                )),
                ..Default::default()
            });
            assert_eq!(t.visible_len(), 0);
            assert!(t.active_filter_summary().contains("a date range"));
            t.set_time_range_label(Some("Last 7 days".to_string()));
            assert!(
                t.active_filter_summary().contains("Last 7 days"),
                "got {:?}",
                t.active_filter_summary()
            );
        }

        an_empty_history_and_a_filtered_one_explain_themselves_differently();
        a_host_time_range_is_named_in_the_explanation();
    }

    phase filters_narrow_by_entry_kind_and_author_and_clear;
    {
        fn filter_by_entry_kind_hides_other_rows() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 1, 1),
                entry(EntryKind::Comment, "Bob", 2, 2),
                entry(EntryKind::Move, "Carol", 3, 3),
                entry(EntryKind::Comment, "Dave", 4, 4),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            assert_eq!(t.visible_len(), 4);
            t.set_active_chip(ChipKind::Comments);
            assert_eq!(t.visible_len(), 2);
            for i in 0..t.visible_len() {
                assert_eq!(t.visible_entry(i).unwrap().entry_kind, EntryKind::Comment);
            }
        }

        fn every_strip_chip_takes_a_press_before_anything_paints() {
            // Nothing here draws: the layout places each chip, and a press on one
            // filters the pane. A chip whose place only a paint worked out is
            // nowhere to press.
            let entries = vec![
                entry(EntryKind::Text, "Alice", 1, 1),
                entry(EntryKind::Comment, "Bob", 2, 2),
                entry(EntryKind::Access, "Carol", 3, 3),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            lay_out(&mut t, &Theme::dark());

            for chip in ChipKind::ALL {
                let hit = t
                    .chip_rect(chip)
                    .unwrap_or_else(|| panic!("the layout placed the {chip:?} chip"));
                click(&mut t, hit.center());
                assert_eq!(
                    t.active_chip(),
                    chip,
                    "a press on the {chip:?} chip activates it",
                );
                let kinds = chip.entry_kinds();
                for row in 0..t.visible_len() {
                    let shown = t.visible_entry(row).expect("a visible row").entry_kind;
                    assert!(
                        kinds.is_empty() || kinds.contains(&shown),
                        "the {chip:?} chip shows only the changes of its kind",
                    );
                }
                // The host lays out again whenever the pane says what it places
                // may have moved; the chosen chip and the rows changed.
                lay_out(&mut t, &Theme::dark());
            }

            let (a, b) = (ChipKind::ALL[0], ChipKind::ALL[1]);
            let (a, b) = (
                t.chip_rect(a).expect("chip a"),
                t.chip_rect(b).expect("chip b"),
            );
            if b.x0 - a.x1 > 1.0 {
                let was = t.active_chip();
                click(&mut t, Point::new(f64::midpoint(a.x1, b.x0), a.center().y));
                assert_eq!(t.active_chip(), was, "a press between chips does nothing");
            }
        }

        fn people_chips_take_a_press_before_anything_paints() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 1, 1),
                entry(EntryKind::Comment, "Bob", 2, 2),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            t.set_filters_people(true);
            lay_out(&mut t, &Theme::dark());

            for who in [Some("Bob"), Some("Alice"), None] {
                let hit = t
                    .people_chip_rect(who)
                    .unwrap_or_else(|| panic!("the layout placed the {who:?} chip"));
                click(&mut t, hit.center());
                assert_eq!(
                    t.filter().authors.first().map(String::as_str),
                    who,
                    "a press on the {who:?} chip filters the pane to them",
                );
                lay_out(&mut t, &Theme::dark());
            }
        }

        fn clear_filters_resets_the_widget_and_tells_the_host() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 1, 1),
                entry(EntryKind::Comment, "Bob", 2, 2),
            ];
            let cleared = Rc::new(RefCell::new(0usize));
            let c = cleared.clone();
            let mut t = ActivityTimeline::with_entries(entries).on_clear_filters(move || {
                *c.borrow_mut() += 1;
            });
            t.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            t.set_active_chip(ChipKind::Governance);
            t.set_author_query("zzz");
            assert_eq!(t.visible_len(), 0);

            t.clear_filters();
            assert_eq!(t.active_chip(), ChipKind::All);
            assert_eq!(t.author_query(), "");
            assert_eq!(t.visible_len(), 2, "every row is back");
            assert_eq!(
                *cleared.borrow(),
                1,
                "the host is told so it can drop the filters it owns"
            );
        }

        fn filter_by_author_typeahead_narrows_list() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 1, 1),
                entry(EntryKind::Text, "Alicia", 2, 2),
                entry(EntryKind::Text, "Bob", 3, 3),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            t.set_author_query("ali");
            assert_eq!(t.visible_len(), 2);
            t.set_author_query("");
            assert_eq!(t.visible_len(), 3);
        }

        fn typing_narrows_by_author_and_escape_or_backspace_widens_again() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 1, 1),
                entry(EntryKind::Text, "Bob", 2, 2),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), entries);
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Character("b".into())),
            );
            assert_eq!(t.author_query(), "b");
            assert_eq!(t.visible_len(), 1);
            assert_eq!(t.visible_entry(0).unwrap().author.name, "Bob");
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Named(NamedKey::Escape)),
            );
            assert_eq!(t.author_query(), "");
            assert_eq!(t.visible_len(), 2);

            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Character("b".into())),
            );
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Named(NamedKey::Backspace)),
            );
            assert_eq!(t.author_query(), "");
            assert_eq!(t.visible_len(), 2);

            dispatch(
                &mut t,
                &WindowEvent::Ime(winit::event::Ime::Commit("bo\u{301}".into())),
            );
            assert_eq!(t.author_query(), "bo\u{301}");
            dispatch(
                &mut t,
                &key_press(winit::keyboard::Key::Named(NamedKey::Backspace)),
            );
            assert_eq!(t.author_query(), "b");
        }

        filter_by_entry_kind_hides_other_rows();
        every_strip_chip_takes_a_press_before_anything_paints();
        people_chips_take_a_press_before_anything_paints();
        clear_filters_resets_the_widget_and_tells_the_host();
        filter_by_author_typeahead_narrows_list();
        typing_narrows_by_author_and_escape_or_backspace_widens_again();
    }
}

tilo_test_env::scenario! {
    /// What one row of the activity timeline shows, says and offers.
    /// Each phase was a test of its own and builds its own fixture.
    fn timeline_rows_present_who_changed_what;

    phase a_bots_change_is_marked_as_the_bots_and_an_agents_is_not;
    {
        let acting = |acting: tilo_actions::Acting| {
            HistoryAuthor::new("Standup Bot", 120.0, "ab12")
                .with_relation(crate::widget::history::AuthorRelation::Software)
                .with_attribution(crate::widget::history::HistoryAttribution {
                    acting: acting.acting_for(tilo_actions::Acting::person("ada")),
                    on_behalf_of_name: Some("Ada".into()),
                writer: None,
                writer_label: None,
                device: None,
                folder: None,
            })
        };
        assert!(is_bot_author(&acting(tilo_actions::Acting::bot("standup"))));
        assert!(!is_bot_author(&acting(tilo_actions::Acting::agent("standup", "ada"))));
        assert!(!is_bot_author(&HistoryAuthor::new("Ada", 10.0, "cd34")));
    }

    phase a_presented_change_reads_opens_and_copies_as_the_app_says;
    // An app's presenter decides how its changes read: a face's words name
    // the row, its body sets the row's height, choosing a change it opens
    // hands it to the app instead of selecting it, and Copy copies what the
    // face copies. Changes without a face keep their sentence.
    {
        struct Tall;
        impl crate::widget::EntryBody for Tall {
            fn height(&self, _: &Theme) -> f64 {
                ROW_HEIGHT * 2.0
            }
            fn paint(&self, _: &mut PaintCtx<'_>, _: Rect) {}
        }
        struct Calculations;
        impl HistoryEntryPresenter for Calculations {
            fn face(&self, entry: &HistoryEntry) -> Option<EntryFace> {
                (entry.id.0[0] == 1).then(|| {
                    EntryFace::new("1+2 = 3")
                        .with_body(Rc::new(Tall))
                        .copying("3")
                        .opening()
                })
            }
        }
        let _host = tilo_ui::process::enter(&tilo_ui::process::ProcessServices::null());
        let opened = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&opened);
        let presented = entry(EntryKind::Details, "Alice", 10, 1);
        let plain = entry(EntryKind::Details, "Alice", 20, 2);
        let (presented_id, plain_summary) = (presented.id, plain.summary.clone());
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), vec![presented, plain])
            .on_open_entry(move |id| seen.borrow_mut().push(*id));
        timeline.set_entry_presenter(Some(Rc::new(Calculations)));
        let row = (0..timeline.row_count())
            .find(|row| {
                timeline
                    .row_entry(*row)
                    .is_some_and(|entry| entry.id == presented_id)
            })
            .expect("the change is listed");
        let names: Vec<String> = Widget::accessibility(&timeline)
            .expect("timeline semantics")
            .children
            .iter()
            .filter_map(|node| node.name.clone())
            .collect();
        assert!(
            names.iter().any(|name| name.ends_with("1+2 = 3")),
            "{names:?}"
        );
        assert!(
            names.iter().any(|name| name.ends_with(&plain_summary)),
            "{names:?}"
        );
        assert!((timeline.row_height(row) - ROW_HEIGHT * 2.0).abs() < 1e-9);

        assert!(timeline.copy_row(row), "the face copies its result");
        assert_eq!(
            tilo_ui_base::text_selection::clipboard_paste().as_deref(),
            Some("3")
        );

        assert!(timeline.activate_row(row));
        assert_eq!(*opened.borrow(), vec![presented_id], "the app takes it up");
        assert!(timeline.selection.is_none(), "opening is not selecting");
    }

    phase a_presenter_says_what_a_band_amounts_to;
    {
        struct Counting;
        impl HistoryEntryPresenter for Counting {
            fn face(&self, _: &HistoryEntry) -> Option<EntryFace> {
                None
            }
            fn group_summary(&self, entries: &[HistoryEntry]) -> Option<String> {
                Some(format!("{} calculations", entries.len()))
            }
        }
        let entries: Vec<HistoryEntry> = (1..=3)
            .map(|id| entry(EntryKind::Text, "Alice", u64::from(id), id))
            .collect();
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        let band = |timeline: &ActivityTimeline| {
            (0..timeline.row_count())
                .find_map(|row| timeline.group_subtitle_at_row(row))
                .expect("the changes form a band")
        };
        assert!(band(&timeline).contains("chars"), "{}", band(&timeline));
        timeline.set_entry_presenter(Some(Rc::new(Counting)));
        let said = band(&timeline);
        assert!(said.contains("3 calculations"), "{said}");
        assert!(!said.contains("chars"), "{said}");
    }

    phase a_genesis_row_names_no_author_and_no_time;
    {
        use tilo_ui_base::a11y::Role;
        let mut genesis = entry(EntryKind::Details, "Unknown member", 0, 1);
        genesis.summary = "Created".into();
        genesis.context = Some(tilo_actions::ActionContext {
            facets: vec![crate::widget::history::genesis_facet()],
            ..tilo_actions::ActionContext::default()
        });
        let entries = vec![genesis, entry(EntryKind::Text, "Alice", 10, 2)];
        let t = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
        let semantics = Widget::accessibility(&t).expect("timeline semantics");
        let rows: Vec<_> = semantics
            .children
            .iter()
            .filter(|node| node.role == Role::ListItem)
            .map(|node| (node.name.clone(), node.description.clone()))
            .collect();
        assert_eq!(
            rows[0],
            (
                Some("Created".to_owned()),
                Some(EntryKind::Details.glyph().to_owned())
            ),
            "the version the document was created as, by no one at no time"
        );
        assert_eq!(rows[1].0.as_deref(), Some("Alice: op 2 by Alice"));
        assert!(
            rows[1]
                .1
                .as_deref()
                .is_some_and(|text| text.contains(" · "))
        );
    }

    phase timeline_rows_are_accessible_keyboard_and_hover_targets;
    {
        fn accessibility_reports_list_role_name_and_count() {
            use tilo_ui_base::a11y::Role;
            let entries = vec![
                entry(EntryKind::Text, "Alice", 10, 1),
                entry(EntryKind::Text, "Bob", 20, 2),
            ];
            let t = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
            let node = Widget::accessibility(&t).expect("timeline is not presentational");
            assert_eq!(node.role, Role::List);
            assert_eq!(node.name.as_deref(), Some("History entries"));
            assert_eq!(node.description.as_deref(), Some("2 entries"));
        }

        fn individual_entries_have_stable_accessible_targets_and_activate() {
            use tilo_ui_base::{a11y::Action, a11y::Role};
            let activated = Rc::new(RefCell::new(Vec::new()));
            let received = activated.clone();
            let entries = vec![entry(EntryKind::Text, "Alice", 10, 1)];
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries)
                .on_activate_entry(move |id| received.borrow_mut().push(*id));
            let semantics = Widget::accessibility(&timeline).expect("timeline semantics");
            let row = semantics
                .children
                .iter()
                .find(|node| node.role == Role::ListItem)
                .expect("individual entry semantic row");
            assert!(row.bounds.is_some(), "custom-painted row has AT bounds");
            assert!(row.state.focusable);
            let target = row.action_target.clone().expect("stable entry target");
            assert!(Widget::perform_semantic_action(
                &mut timeline,
                &target,
                &Action::Focus,
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(
                timeline.focused_entry().map(|entry| entry.id),
                Some(HistoryEntryId([
                    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
                ]))
            );
            assert!(Widget::perform_semantic_action(
                &mut timeline,
                &target,
                &Action::Activate,
                &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(timeline.selection(), Some((0, 0)));
            assert_eq!(
                activated.borrow().as_slice(),
                &[HistoryEntryId([
                    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
                ])]
            );
        }

        fn keyboard_navigation_moves_focus_and_selects_individual_rows() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 10, 1),
                entry(EntryKind::Text, "Alice", 11, 2),
            ];
            let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
            timeline.set_all_expanded(true);
            assert!(timeline.focus_row(0));
            dispatch(&mut timeline, &key_press(Key::Named(NamedKey::ArrowDown)));
            let focused = timeline.focused_row().expect("moved focus");
            assert!(focused > 0);
            if let Some(entry) = timeline.row_entry_index(focused) {
                assert_eq!(timeline.selection(), Some((entry, entry)));
            }
        }

        accessibility_reports_list_role_name_and_count();
        individual_entries_have_stable_accessible_targets_and_activate();
        keyboard_navigation_moves_focus_and_selects_individual_rows();
    }

    phase revert_chip_fires_only_for_revertible_ops;
    {
        fn revert_callback_fires_on_chip_click() {
            let entries = vec![entry(EntryKind::Text, "Alice", 1, 42)];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 200.0), entries);
            let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            t.on_revert = Some(Box::new(move |id| {
                *captured_clone.borrow_mut() = Some(*id);
            }));

            let row = t.row_rect(0).expect("seeded row 0 must lay out");
            dispatch(&mut t, &cursor_moved(row.center().x, row.center().y));
            assert_eq!(t.hover_row(), Some(0));
            let chip = t.revert_chip_rect(0).expect("splice row must have a chip");
            dispatch(&mut t, &cursor_moved(chip.center().x, chip.center().y));
            dispatch(&mut t, &mouse_press());

            let mut id_bytes = [0u8; 16];
            id_bytes[0] = 42;
            assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
        }

        fn non_revertible_entry_kinds_hide_chip() {
            let entries = vec![
                entry(EntryKind::Comment, "Alice", 1, 1),
                entry(EntryKind::Access, "Bob", 2, 2),
                entry(EntryKind::Text, "Carol", 3, 3),
            ];
            let t = editable_timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            assert!(t.revert_chip_rect(0).is_none(), "Comment must hide chip");
            assert!(t.revert_chip_rect(1).is_none(), "Govern must hide chip");
            assert!(
                t.revert_chip_rect(2).is_some(),
                "Splice must expose chip for hit-test"
            );
        }

        revert_callback_fires_on_chip_click();
        non_revertible_entry_kinds_hide_chip();
    }

    phase rows_order_newest_first_and_virtualize_within_narrow_panes;
    {
        fn entries_sharing_a_timestamp_still_order_newest_first() {
            // Automerge stamps in whole seconds, so a burst ties; newest-first
            // must fall back to causal order, not input order.
            let entries: Vec<HistoryEntry> = (0u8..5)
                .map(|i| entry(EntryKind::Text, "Alice", 100, i))
                .collect();
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            t.set_newest_first(true);
            let ids: Vec<u8> = (0..t.visible_len())
                .map(|i| t.visible_entry(i).unwrap().id.0[0])
                .collect();
            assert_eq!(
                ids,
                vec![4, 3, 2, 1, 0],
                "the last change the producer emitted is the newest"
            );

            t.set_newest_first(false);
            let ids: Vec<u8> = (0..t.visible_len())
                .map(|i| t.visible_entry(i).unwrap().id.0[0])
                .collect();
            assert_eq!(ids, vec![0, 1, 2, 3, 4]);
        }

        fn newest_first_reverses_chronological_order() {
            let entries = vec![
                entry(EntryKind::Text, "Alice", 100, 1),
                entry(EntryKind::Text, "Alice", 200, 2),
                entry(EntryKind::Text, "Alice", 300, 3),
            ];
            let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
            t.set_newest_first(true);
            assert_eq!(
                t.visible_entry(0)
                    .unwrap()
                    .timestamp
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                300
            );
            assert_eq!(
                t.visible_entry(2)
                    .unwrap()
                    .timestamp
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                100
            );
        }

        fn narrow_panes_shrink_the_trailing_slots_rather_than_overlapping_them() {
            // On a narrow pane the revert chip must still sit inside the row and
            // leave the summary somewhere to go.
            let entries = vec![entry(EntryKind::Text, "Alice", 1, 1)];
            let wide = editable_timeline_at(Rect::new(0.0, 0.0, 700.0, 300.0), entries.clone());
            let narrow = editable_timeline_at(Rect::new(0.0, 0.0, 300.0, 300.0), entries);

            let wide_chip = wide.revert_chip_rect(0).unwrap();
            let narrow_chip = narrow.revert_chip_rect(0).unwrap();
            assert!(
                narrow_chip.width() < wide_chip.width(),
                "the chip gives width back on a narrow pane"
            );
            let narrow_row = narrow.row_rect(0).unwrap();
            assert!(
                narrow_chip.x0 > narrow_row.x0 + AUTHOR_SLOT_W,
                "the chip must not overlap the author cluster"
            );
            assert!(
                narrow_chip.x1 <= narrow_row.x1,
                "and must stay inside the row"
            );
        }

        entries_sharing_a_timestamp_still_order_newest_first();
        newest_first_reverses_chronological_order();
        narrow_panes_shrink_the_trailing_slots_rather_than_overlapping_them();
    }
}

/// A writer's name as long as a session title never pushes a row's own mark,
/// sentence or writer chip under the trailing time: at a narrow pane the
/// author cluster keeps a bounded share, its chip elides there, and the
/// change's mark and sentence take the rest and truncate.
#[test]
fn a_long_writer_name_never_reaches_a_rows_time() {
    let long = "Workspace folder pass, sixth round, agent run, second pass, renamed";
    let theme = Theme::dark();
    // A pane narrow enough that the chip keeps its mark alone, and one wide
    // enough for its name: neither may put anything under the time.
    let panes = [
        Rect::new(8.0, 8.0, 328.0, 408.0),
        Rect::new(8.0, 8.0, 528.0, 408.0),
    ]
    .into_iter()
    .map(|pane| {
        let mut entry = entry(EntryKind::Text, long, 1_000, 1);
        entry.author = HistoryAuthor::new("Ada", 120.0, "deadbeef").with_attribution(
            crate::widget::history::HistoryAttribution::new(tilo_actions::Acting::person("ada"))
                .written_by(
                    collab_types::history::authors::AttributedWriter::Program("editor".into()),
                    long,
                ),
        );
        let mut t = timeline_at(pane, vec![entry]);
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        t.place_controls(&theme);
        t.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            PresenceCtx::empty().draw_env(),
        ));
        (pane, t)
    })
    .collect::<Vec<_>>();
    for (pane, t) in panes {
        let parts = t.row_parts(0).expect("the row painted");
        let chip = parts.chip.expect("the writer's chip");
        let mark = parts.mark.expect("the change's own mark");
        assert!(
            parts.body.x1 <= parts.time.x0,
            "in {pane:?} the change's column ends before the time: {parts:?}"
        );
        assert!(
            chip.x1 <= parts.body.x0,
            "in {pane:?} the writer's chip ends before the change's mark: {parts:?}"
        );
        assert!(
            mark.x1 <= parts.time.x0,
            "in {pane:?} the mark never reaches the time: {parts:?}"
        );
        assert!(parts.time.x1 <= parts.row.x1, "the time stays in the row");
        for (name, rect) in [
            ("chip", chip),
            ("mark", mark),
            ("body", parts.body),
            ("time", parts.time),
        ] {
            assert!(
                parts.row.contains_rect(rect),
                "the {name} {rect:?} stays inside the row {:?}",
                parts.row
            );
        }
    }
}

/// Software that is a row's author wears its own face and no chip repeating its name, so the
/// row's width goes to what it did.
#[test]
fn an_agent_author_has_its_face_and_no_chip() {
    let theme = Theme::dark();
    let mut entry = entry(
        EntryKind::Text,
        "Renamed Draft the plan to Final plan",
        1_000,
        1,
    );
    entry.author = HistoryAuthor::new("Planner", 120.0, "deadbeef")
        .with_relation(crate::widget::history::AuthorRelation::Software);
    let mut t = timeline_at(Rect::new(8.0, 8.0, 360.0, 408.0), vec![entry]);
    t.place_controls(&theme);
    t.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        PresenceCtx::empty().draw_env(),
    ));
    assert_eq!(t.row_parts(0).expect("the row painted").chip, None);
}
