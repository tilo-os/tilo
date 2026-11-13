use super::*;
use crate::testing::{Dispatch, dispatch};
use crate::time::Duration;
use crate::widget::history::HistoryAuthor;
use crate::widget::history_group::NamedRange;
use collab_types::ArtifactKind;
use std::cell::RefCell;
use std::rc::Rc;
use winit::event::Modifiers;

const NOW: u64 = 2_000_000;

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
        summary: format!("+{} -0 chars", usize::from(id).saturating_add(1)),
        text_delta: Some((usize::from(id).saturating_add(1), 0)),
        diff_payload: None,
        redaction: None,
        context: None,
    }
}

fn timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
    let mut t = ActivityTimeline::with_entries(entries);
    t.set_rect(rect);
    t.set_now_for_test(
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(NOW))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    );
    t
}

// Capability-dependent layout fixtures must declare the operations they offer.
fn editable_timeline_at(rect: Rect, entries: Vec<HistoryEntry>) -> ActivityTimeline {
    timeline_at(rect, entries)
        .on_revert(|_| {})
        .on_name_group(|_| {})
}

#[test]
fn band_mutation_affordances_need_callbacks_and_restorable_changes() {
    fn readonly_timeline_hides_mutations_but_keeps_selectable_semantics() {
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 620.0, 620.0), typing_run(20));
        let (name, restore) = timeline.group_chip_rects(0).expect("group row");
        assert_eq!(name, Rect::ZERO);
        assert!(restore.is_none());
        assert!(!timeline.begin_naming_group_at_row(0));
        timeline.set_all_expanded(true);
        let row = (0..timeline.row_count())
            .find(|row| timeline.row_entry(*row).is_some())
            .expect("entry row");
        assert!(timeline.revert_chip_rect(row).is_none());
        assert!(timeline.activate_row(row));
        assert!(!timeline.begin_naming_selection());
        paint(&mut timeline);
        assert_eq!(timeline.name_selection_rect(), Rect::ZERO);
        assert!(
            timeline.clear_selection_rect().area() > 0.0,
            "selection can still be cleared"
        );
        let semantics = timeline.accessibility().expect("history semantics");
        assert!(
            semantics.children.iter().any(
                |node| node.role == crate::a11y::Role::ListItem && node.action_target.is_some()
            )
        );
        assert!(semantics.children.iter().all(|node| matches!(
            node.role,
            crate::a11y::Role::ListItem | crate::a11y::Role::TreeItem
        )));
    }

    fn history_mutation_affordances_require_their_own_callback() {
        let rect = Rect::new(0.0, 0.0, 620.0, 620.0);
        let mut naming = timeline_at(rect, typing_run(20)).on_name_group(|_| {});
        let (name, restore) = naming.group_chip_rects(0).expect("group row");
        assert!(name.area() > 0.0);
        assert!(restore.is_none());
        assert!(naming.begin_naming_group_at_row(0));
        let mut restoring = timeline_at(rect, typing_run(20)).on_revert(|_| {});
        let (name, restore) = restoring.group_chip_rects(0).expect("group row");
        assert_eq!(name, Rect::ZERO);
        assert!(restore.is_some());
        assert!(!restoring.begin_naming_group_at_row(0));
        restoring.set_all_expanded(true);
        let row = (0..restoring.row_count())
            .find(|row| restoring.row_entry(*row).is_some())
            .expect("entry row");
        assert!(restoring.revert_chip_rect(row).is_some());
    }

    fn a_band_of_nothing_restorable_offers_no_restore_chip() {
        // Governance is its own audit trail; an owner counters it rather than
        // rewinding it. A band made only of those must not offer the chip.
        let entries: Vec<HistoryEntry> = (0..6)
            .map(|i| {
                entry(
                    EntryKind::Access,
                    "Alice",
                    1_000_000u64.saturating_add(u64::from(i)),
                    i,
                )
            })
            .collect();
        let t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), entries);
        let (_, restore) = t.group_chip_rects(0).expect("still a band");
        assert!(restore.is_none(), "no restore across governance");
    }

    fn restoring_from_a_band_passes_the_bands_last_change_not_its_first() {
        // This is the whole point of grouping for restore: a boundary that means
        // something, rather than keystroke 4,182.
        let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
        let c = captured.clone();
        let mut t = ActivityTimeline::with_entries(typing_run(20))
            .on_revert(move |id| *c.borrow_mut() = Some(*id));
        t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));

        let (_, restore) = t.group_chip_rects(0).expect("a band header");
        let restore = restore.expect("a band of splices can be restored to");
        click(&mut t, restore.center());
        let mut want = [0u8; 16];
        want[0] = 19;
        assert_eq!(*captured.borrow(), Some(HistoryEntryId(want)));
    }

    readonly_timeline_hides_mutations_but_keeps_selectable_semantics();
    history_mutation_affordances_require_their_own_callback();
    a_band_of_nothing_restorable_offers_no_restore_chip();
    restoring_from_a_band_passes_the_bands_last_change_not_its_first();
}

/// Run a real frame so the rects the click handler reads (the strip's and
/// the selection bar's controls) are placed and painted exactly as they are
/// live: the layout places them, the paint draws them.
fn paint(t: &mut ActivityTimeline) {
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    tr.apply_typography(&theme.tokens.typography);
    t.place_controls(&theme);
    let presence = PresenceCtx::empty();
    t.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

use crate::synthetic_input::cursor_moved;

use crate::synthetic_input::mouse_press;

fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
    crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
        logical_key,
        None,
        ElementState::Pressed,
    ))
}

/// Shift held down, built the same way winit reports it.
fn shift() -> Modifiers {
    let state = winit::keyboard::ModifiersState::SHIFT;
    Modifiers::from(state)
}

/// Click whatever is at `p`: move there first so `hover_row` latches, the
/// way a real pointer does.
fn click(t: &mut ActivityTimeline, p: Point) {
    dispatch(t, &cursor_moved(p.x, p.y));
    dispatch(t, &mouse_press());
}

fn type_str(t: &mut ActivityTimeline, s: &str) {
    for ch in s.chars() {
        if ch == ' ' {
            dispatch(t, &key_press(Key::Named(NamedKey::Space)));
        } else {
            dispatch(t, &key_press(Key::Character(ch.to_string().into())));
        }
    }
}

/// Twenty changes a second apart by one person: a debounced typing run.
fn typing_run(n: u8) -> Vec<HistoryEntry> {
    (0..n)
        .map(|i| {
            entry(
                EntryKind::Text,
                "Alice",
                1_000_000u64.saturating_add(u64::from(i)),
                i,
            )
        })
        .collect()
}

#[test]
fn typing_runs_collapse_into_expandable_bands() {
    fn a_typing_run_collapses_to_one_band_and_the_header_opens_it() {
        let mut t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
        assert_eq!(t.visible_len(), 20, "every change still passes the filter");
        assert_eq!(t.row_count(), 1, "twenty changes read as one band");
        let (_, expanded) = t.group_at_row(0).expect("line 0 is a band");
        assert!(!expanded, "a twenty-change band is past the expand budget");
        assert_eq!(t.group_title_at_row(0), Some("20 edits"));

        // Click the header (not the chips) — the production toggle path.
        let row = t.row_rect(0).expect("the header lays out");
        click(&mut t, Point::new(row.x0 + 60.0, row.center().y));
        assert!(t.group_at_row(0).unwrap().1, "the click opened it");
        assert_eq!(t.row_count(), 21, "header plus twenty changes");
        assert!(t.row_entry(1).is_some(), "line 1 is a change");
        assert_eq!(
            t.row_depth_at(1),
            Some(1),
            "and it is indented under the band"
        );

        click(&mut t, Point::new(row.x0 + 60.0, row.center().y));
        assert_eq!(t.row_count(), 1, "clicking again closes it");
    }

    fn a_short_history_reads_as_a_flat_list_with_no_bands_at_all() {
        // The flat list is the degenerate case, not a second mode: four changes
        // by four people are four groups of one, and a group of one is a row.
        let entries = vec![
            entry(EntryKind::Text, "Alice", 1_000_000, 0),
            entry(EntryKind::Text, "Bob", 1_000_001, 1),
            entry(EntryKind::Text, "Carol", 1_000_002, 2),
            entry(EntryKind::Text, "Dave", 1_000_003, 3),
        ];
        let t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), entries);
        assert_eq!(t.row_count(), 4);
        for i in 0..4 {
            assert!(t.row_entry(i).is_some(), "line {i} is a plain change row");
            assert_eq!(t.row_depth_at(i), Some(0), "and it is not indented");
        }
    }

    fn an_open_band_says_it_is_still_open_while_it_is_being_appended_to() {
        let entries = typing_run(20);
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone());
        t.set_named_ranges(vec![NamedRange {
            id: "tilo:group:rec".into(),
            label: "Working".into(),
            start: entries[0].id,
            end: None,
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        }]);
        let sub = t.group_subtitle_at_row(0).unwrap();
        assert!(sub.starts_with("still open"), "got {sub:?}");
        assert!(sub.contains("20 changes"), "got {sub:?}");

        // A new change lands while the reader is looking at it. The band must
        // absorb it and the count must move.
        let mut grown = entries;
        grown.push(entry(EntryKind::Text, "Alice", 1_000_400, 99));
        t.set_entries(grown);
        let sub = t.group_subtitle_at_row(0).unwrap();
        assert!(sub.contains("21 changes"), "got {sub:?}");
        assert!(
            sub.starts_with("still open"),
            "and it is still open: {sub:?}"
        );
    }

    fn expansion_survives_a_refresh_that_appends_a_change() {
        // The band key is derived from its first change, which does not move as
        // the band grows — so an open band stays open when the document does.
        let mut t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
        let row = t.row_rect(0).unwrap();
        click(&mut t, Point::new(row.x0 + 60.0, row.center().y));
        assert!(t.group_at_row(0).unwrap().1);
        let key = t.group_at_row(0).unwrap().0.to_string();

        let mut grown = typing_run(20);
        grown.push(entry(EntryKind::Text, "Alice", 1_000_020, 99));
        t.set_entries(grown);
        assert_eq!(t.group_at_row(0).unwrap().0, key, "same band, same key");
        assert!(
            t.group_at_row(0).unwrap().1,
            "it did not slam shut under the reader"
        );
    }

    fn nesting_renders_the_child_inside_the_parent_and_no_deeper() {
        let entries = typing_run(20);
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 900.0), entries.clone());
        t.set_named_ranges(vec![
            NamedRange {
                id: "tilo:group:outer".into(),
                label: "Q3 release".into(),
                start: entries[0].id,
                end: Some(entries[19].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
            NamedRange {
                id: "tilo:group:inner".into(),
                label: "Intro rewrite".into(),
                start: entries[4].id,
                end: Some(entries[8].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
        ]);
        t.set_all_expanded(true);
        let depths: Vec<usize> = (0..t.row_count())
            .map(|i| t.row_depth_at(i).unwrap())
            .collect();
        assert_eq!(
            *depths.iter().max().unwrap(),
            2,
            "rows inside a nested band"
        );
        assert_eq!(t.group_title_at_row(0), Some("Q3 release"));
        let inner = (0..t.row_count())
            .find(|i| t.group_title_at_row(*i) == Some("Intro rewrite"))
            .expect("the child band renders");
        assert_eq!(t.row_depth_at(inner), Some(1));
        // Every line between the child header and the next depth<=1 line is a
        // change of the child's.
        let after = inner + 1;
        assert_eq!(t.row_depth_at(after), Some(2));
        assert!(t.row_entry(after).is_some());
    }

    a_typing_run_collapses_to_one_band_and_the_header_opens_it();
    a_short_history_reads_as_a_flat_list_with_no_bands_at_all();
    an_open_band_says_it_is_still_open_while_it_is_being_appended_to();
    expansion_survives_a_refresh_that_appends_a_change();
    nesting_renders_the_child_inside_the_parent_and_no_deeper();
}

#[test]
fn naming_a_band_writes_its_whole_range_or_nothing() {
    fn naming_a_band_asks_the_host_to_write_the_bands_whole_range() {
        let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let c = captured.clone();
        let mut t = ActivityTimeline::with_entries(typing_run(20))
            .on_name_group(move |r| c.borrow_mut().push(r.clone()));
        t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));
        t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW));

        let (name_chip, _) = t
            .group_chip_rects(0)
            .expect("a band header has a name chip");
        click(&mut t, name_chip.center());
        assert_eq!(t.naming_buffer(), Some(""), "the field is up and empty");
        assert!(t.wants_keyboard(), "the field owns the keyboard");

        type_str(&mut t, "Intro rewrite");
        assert_eq!(t.naming_buffer(), Some("Intro rewrite"));
        dispatch(&mut t, &key_press(Key::Named(NamedKey::Enter)));

        let reqs = captured.borrow();
        assert_eq!(reqs.len(), 1, "one write, on commit");
        let r = &reqs[0];
        assert_eq!(r.label, "Intro rewrite");
        assert_eq!(r.record_id, None, "nothing was named here before");
        let mut first = [0u8; 16];
        let mut last = [0u8; 16];
        last[0] = 19;
        assert_eq!(
            r.start,
            HistoryEntryId(first),
            "the range starts at change 0"
        );
        assert_eq!(r.end, HistoryEntryId(last), "and ends at change 19");
        let _ = &mut first;
        assert!(!t.wants_keyboard(), "the field is gone");
    }

    fn escape_abandons_a_name_without_writing_anything() {
        let captured: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let c = captured.clone();
        let mut t = ActivityTimeline::with_entries(typing_run(20))
            .on_name_group(move |_| *c.borrow_mut() += 1);
        t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));

        let (name_chip, _) = t.group_chip_rects(0).unwrap();
        click(&mut t, name_chip.center());
        type_str(&mut t, "oops");
        dispatch(&mut t, &key_press(Key::Named(NamedKey::Escape)));
        assert!(!t.wants_keyboard());
        assert_eq!(*captured.borrow(), 0, "nothing was written");
    }

    fn typing_a_name_does_not_leak_into_the_author_filter() {
        // The pane's other keyboard consumer is the author typeahead. A version
        // called "Bob's edits" must not silently filter the pane to Bob.
        let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
        let (name_chip, _) = t.group_chip_rects(0).unwrap();
        click(&mut t, name_chip.center());
        type_str(&mut t, "Bob");
        assert_eq!(t.author_query(), "", "the typeahead was not touched");
        assert_eq!(t.naming_buffer(), Some("Bob"));
    }

    fn a_named_band_renames_through_the_record_it_already_has() {
        let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let c = captured.clone();
        let entries = typing_run(20);
        let mut t = ActivityTimeline::with_entries(entries.clone())
            .on_name_group(move |r| c.borrow_mut().push(r.clone()));
        t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));
        t.set_named_ranges(vec![NamedRange {
            id: "tilo:group:rec".into(),
            label: "Intro rewrite".into(),
            start: entries[0].id,
            end: Some(entries[19].id),
            named_by: Some(HistoryAuthor::new("Alice", 120.0, "deadbeef")),
            created: SystemTime::UNIX_EPOCH,
        }]);
        assert_eq!(t.group_title_at_row(0), Some("Intro rewrite"));

        let (name_chip, _) = t.group_chip_rects(0).unwrap();
        click(&mut t, name_chip.center());
        assert_eq!(
            t.naming_buffer(),
            Some("Intro rewrite"),
            "renaming starts from the current name, not from blank"
        );
        // Clear it out entirely and commit: that is how a name comes back off.
        for _ in 0.."Intro rewrite".len() {
            dispatch(&mut t, &key_press(Key::Named(NamedKey::Backspace)));
        }
        dispatch(&mut t, &key_press(Key::Named(NamedKey::Enter)));
        let reqs = captured.borrow();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].label, "", "an empty label is 'remove the name'");
        assert_eq!(
            reqs[0].record_id.as_deref(),
            Some("tilo:group:rec"),
            "and it addresses the record that already exists"
        );
    }

    fn a_band_someone_else_named_says_who_named_it() {
        let entries = typing_run(20);
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone());
        t.set_named_ranges(vec![NamedRange {
            id: "tilo:group:rec".into(),
            label: "Ada's pass".into(),
            start: entries[0].id,
            end: Some(entries[19].id),
            named_by: Some(HistoryAuthor::new("Ada Lovelace", 300.0, "cafebabe")),
            created: SystemTime::UNIX_EPOCH,
        }]);
        let sub = t
            .group_subtitle_at_row(0)
            .expect("a band has a second line");
        assert!(
            sub.contains("named by Ada Lovelace"),
            "a name written by someone else must say so: {sub:?}"
        );
        assert!(
            sub.contains("Alice"),
            "and still attribute the changes to whoever made them: {sub:?}"
        );
    }

    naming_a_band_asks_the_host_to_write_the_bands_whole_range();
    escape_abandons_a_name_without_writing_anything();
    typing_a_name_does_not_leak_into_the_author_filter();
    a_named_band_renames_through_the_record_it_already_has();
    a_band_someone_else_named_says_who_named_it();
}

#[test]
fn band_undo_chip_targets_the_whole_band_only_when_supported() {
    fn a_band_offers_no_undo_chip_until_a_host_can_perform_one() {
        // Opt-in: a control that silently does nothing is worse than none.
        let entries = typing_run(20);
        let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone());
        assert!(
            t.group_undo_chip_rect(0).is_none(),
            "no band-undo callback, no chip"
        );

        let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries).on_undo_band(|_| {});
        assert!(
            t.group_undo_chip_rect(0).is_some(),
            "with one installed, the band offers it"
        );
        assert!(
            t.group_undo_chip_rect(1).is_none(),
            "and only band headers get it, never a plain change row"
        );
    }

    fn the_undo_chip_asks_for_the_whole_band_not_one_change() {
        // Must carry a range, not a single id — that's what distinguishes
        // undo from restore.
        let entries = typing_run(20);
        let captured: Rc<RefCell<Vec<BandUndoRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let c = captured.clone();
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries.clone())
            .on_undo_band(move |r| c.borrow_mut().push(r.clone()));
        t.set_named_ranges(vec![NamedRange {
            id: "tilo:group:rec".into(),
            label: "The agent's pass".into(),
            start: entries[0].id,
            end: Some(entries[19].id),
            named_by: Some(HistoryAuthor::new("Alice", 120.0, "deadbeef")),
            created: SystemTime::UNIX_EPOCH,
        }]);

        let chip = t.group_undo_chip_rect(0).expect("a named band offers undo");
        click(&mut t, chip.center());

        let reqs = captured.borrow();
        assert_eq!(reqs.len(), 1, "the chip fired once");
        assert_eq!(reqs[0].start, entries[0].id, "the band's first change");
        assert_eq!(reqs[0].end, entries[19].id, "and its last");
        assert_ne!(
            reqs[0].start, reqs[0].end,
            "a range, not the single id a restore would carry"
        );
        assert_eq!(reqs[0].label.as_deref(), Some("The agent's pass"));
        assert_eq!(
            reqs[0]
                .authors
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alice"],
            "and who authored it, so the host can scope the revert"
        );
    }

    fn clicking_undo_never_falls_through_to_restore() {
        // Undo takes the band out; restore discards everything after it. A
        // click on one must not reach the other.
        let entries = typing_run(20);
        let undone: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let restored: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let (u, r) = (undone.clone(), restored.clone());
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries)
            .on_undo_band(move |_| *u.borrow_mut() += 1)
            .on_revert(move |_| *r.borrow_mut() += 1);

        let undo_chip = t.group_undo_chip_rect(0).expect("undo chip");
        let (_, restore) = t.group_chip_rects(0).expect("a band header");
        let restore = restore.expect("restore chip");
        assert!(
            undo_chip.x1 <= restore.x0,
            "undo sits left of restore and they do not overlap: {undo_chip:?} {restore:?}"
        );

        click(&mut t, undo_chip.center());
        assert_eq!((*undone.borrow(), *restored.borrow()), (1, 0));

        click(&mut t, restore.center());
        assert_eq!((*undone.borrow(), *restored.borrow()), (1, 1));
    }

    a_band_offers_no_undo_chip_until_a_host_can_perform_one();
    the_undo_chip_asks_for_the_whole_band_not_one_change();
    clicking_undo_never_falls_through_to_restore();
}

#[test]
fn band_rows_keep_chips_names_and_text_inside_the_row() {
    fn three_band_chips_still_fit_inside_the_row() {
        for w in [360.0_f64, 620.0, 980.0] {
            let entries = typing_run(20);
            let t =
                editable_timeline_at(Rect::new(0.0, 0.0, w, 500.0), entries).on_undo_band(|_| {});
            let row = t.row_rect(0).expect("a band header");
            let (name, restore) = t.group_chip_rects(0).unwrap();
            let undo = t.group_undo_chip_rect(0).unwrap();
            let restore = restore.unwrap();
            assert!(restore.x1 <= row.x1, "restore inside the row at {w}");
            assert!(undo.x1 <= restore.x0, "undo left of restore at {w}");
            assert!(name.x1 <= undo.x0, "name left of undo at {w}");
            assert!(
                name.x0 >= row.x0,
                "and the strip does not run off the left at {w}"
            );
        }
    }

    fn a_very_long_name_is_truncated_rather_than_pushed_over_the_actions() {
        let entries = typing_run(20);
        let long = "Rewrote the introduction, then the second section, then \
                reconsidered the whole framing and put most of it back"
            .to_string();
        let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), entries.clone());
        t.set_named_ranges(vec![NamedRange {
            id: "tilo:group:rec".into(),
            label: long.clone(),
            start: entries[0].id,
            end: Some(entries[19].id),
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        }]);
        assert_eq!(t.group_title_at_row(0), Some(long.as_str()));

        let row = t.row_rect(0).unwrap();
        let (name, restore) = t.group_chip_rects(0).unwrap();
        let restore = restore.unwrap();
        assert!(name.x0 > row.x0, "the name chip stays inside the row");
        assert!(restore.x1 <= row.x1, "so does the restore chip");
        assert!(name.x1 <= restore.x0, "and they do not overlap");
        // Painting must not panic and must fit the title into what is left.
        paint(&mut t);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let budget = name.x0 - (row.x0 + 26.0) - 8.0;
        let shown = fit_text(&mut tr, &long, &theme.text(TextRole::Label), budget);
        assert!(
            shown.chars().count() < long.chars().count(),
            "the header's own budget must actually truncate this name"
        );
        assert!(
            shown.ends_with('\u{2026}'),
            "and say that it did: {shown:?}"
        );
    }

    fn a_narrow_pane_keeps_the_band_actions_inside_the_row() {
        let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 300.0, 500.0), typing_run(20));
        let row = t.row_rect(0).unwrap();
        let (name, restore) = t.group_chip_rects(0).unwrap();
        let restore = restore.unwrap();
        assert!(name.x0 >= row.x0, "the name chip starts inside the row");
        assert!(restore.x1 <= row.x1);
        assert!(name.x1 <= restore.x0);
        paint(&mut t);

        // Narrow, but tall enough to scroll the child band into view — an
        // off-screen line correctly does not lay out at all.
        let mut t = editable_timeline_at(Rect::new(0.0, 0.0, 300.0, 1200.0), typing_run(20));
        let entries = typing_run(20);
        t.set_named_ranges(vec![
            NamedRange {
                id: "tilo:group:outer".into(),
                label: "Outer".into(),
                start: entries[0].id,
                end: Some(entries[19].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
            NamedRange {
                id: "tilo:group:inner".into(),
                label: "Inner".into(),
                start: entries[5].id,
                end: Some(entries[9].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            },
        ]);
        t.set_all_expanded(true);
        let nested = (0..t.row_count())
            .find(|i| t.group_title_at_row(*i) == Some("Inner"))
            .expect("the nested band renders");
        assert_eq!(t.row_depth_at(nested), Some(1));
        let (n2, _) = t.group_chip_rects(nested).unwrap();
        let row2 = t.row_rect(nested).unwrap();
        assert!(
            n2.x0 > row2.x0 + INDENT,
            "indented, and the chip still fits"
        );
        paint(&mut t);
    }

    /// Row text is centred on the renderer's measured line box, which keeps it on the avatar's
    /// centre line when type is scaled. Every chip the timeline offers the pointer — the filter
    /// strip's class chips, the author pill, a band header's Name/Restore — is at least the
    /// minimum pointer target tall and no two claim the same pixels, while the strip's chips
    /// stay inside their strip.
    fn chip_targets_meet_the_minimum_pointer_target_and_never_overlap() {
        let mut t = ActivityTimeline::with_entries(typing_run(20))
            .on_revert(|_| {})
            .on_name_group(|_| {});
        t.set_rect(Rect::new(0.0, 0.0, 520.0, 500.0));
        t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW));
        let theme = Theme::dark();
        let min = f64::from(theme.tokens.metrics.min_pointer_target);
        t.place_controls(&theme);

        let strip = t.filter_strip_rect();
        let mut targets: Vec<(String, Rect)> = t
            .strip_chips
            .iter()
            .map(|chip| (format!("{:?}", chip.target), chip.hit))
            .collect();
        assert!(targets.len() >= 2, "the layout placed no strip chips");
        for (k, r) in &targets {
            assert!(
                r.y0 >= strip.y0 - 1e-6 && r.y1 <= strip.y1 + 1e-6,
                "{k} target {r:?} leaves the strip {strip:?}"
            );
        }
        targets.push(("author".to_string(), t.author_pill_rect()));
        let (name, restore) = t.group_chip_rects(0).expect("a band header has chips");
        targets.push(("name".to_string(), name));
        if let Some(r) = restore {
            targets.push(("restore".to_string(), r));
        }
        for (k, r) in &targets {
            assert!(
                r.height() >= min - 1e-6,
                "{k} target {r:?} is under the {min}px minimum pointer target"
            );
        }
        for (i, (a_name, a)) in targets.iter().enumerate() {
            for (b_name, b) in targets.iter().skip(i + 1) {
                assert!(
                    a.intersect(*b).is_zero_area(),
                    "{a_name} {a:?} and {b_name} {b:?} claim the same pixels"
                );
            }
        }
    }

    fn row_text_centring_tracks_the_measured_line_box_at_every_text_scale() {
        let theme = Theme::dark();
        let style = theme.text(TextRole::Caption);
        for scale in [1.0_f32, 2.0] {
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let cy = 40.0;
            let top = text_top(&mut tr, &style, cy);
            let (_, h) = tr.measure_style("Alice", None, &style);
            assert!(
                (top + f64::from(h) / 2.0 - cy).abs() < 1e-6,
                "at {scale}x: top {top} + half of {h} misses the row centre {cy}"
            );
            assert!(
                f64::from(h) > f64::from(style.size),
                "the line box {h} is taller than the bare size {}; centring on \
             size/2 would sit the text low",
                style.size
            );
        }
    }

    /// The gutters are constants because hit-testing has no theme; they must
    /// nevertheless be the tokens' values, so a token change surfaces here.
    fn gutter_constants_equal_the_spacing_tokens() {
        let spacing = crate::tokens::Spacing::default();
        assert!(
            (PADDING - f64::from(spacing.s2)).abs() < 1e-9,
            "PADDING is s2"
        );
        assert!(
            (TRAILING_INSET - f64::from(spacing.s3)).abs() < 1e-9,
            "TRAILING_INSET is s3"
        );
    }

    three_band_chips_still_fit_inside_the_row();
    a_very_long_name_is_truncated_rather_than_pushed_over_the_actions();
    a_narrow_pane_keeps_the_band_actions_inside_the_row();
    chip_targets_meet_the_minimum_pointer_target_and_never_overlap();
    row_text_centring_tracks_the_measured_line_box_at_every_text_scale();
    gutter_constants_equal_the_spacing_tokens();
}

#[test]
fn band_selection_tracks_ids_and_filters() {
    fn shift_clicking_two_rows_selects_the_run_between_them_and_names_it() {
        let captured: Rc<RefCell<Vec<GroupNameRequest>>> = Rc::new(RefCell::new(Vec::new()));
        let c = captured.clone();
        let mut t = ActivityTimeline::with_entries(typing_run(20))
            .on_name_group(move |r| c.borrow_mut().push(r.clone()));
        t.set_rect(Rect::new(0.0, 0.0, 620.0, 620.0));
        t.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(NOW));
        t.set_all_expanded(true);

        // Newest-first, so line 1 is change 19 and line 4 is change 16.
        let a = t.row_entry_index(1).expect("line 1 is a change");
        let b = t.row_entry_index(4).expect("line 4 is a change");
        assert_eq!((a, b), (19, 16));

        let r1 = t.row_rect(1).unwrap();
        click(&mut t, Point::new(r1.x0 + 40.0, r1.center().y));
        assert_eq!(t.selection(), Some((19, 19)), "a plain click anchors");

        let r4 = t.row_rect(4).unwrap();
        dispatch(&mut t, &cursor_moved(r4.x0 + 40.0, r4.center().y));
        Dispatch::new()
            .modifiers(shift())
            .send(&mut t, &mouse_press());
        assert_eq!(t.selection(), Some((16, 19)), "shift-click extends the run");

        // Paint so the bar's action rect is where the click handler will look.
        paint(&mut t);
        let action = t.name_selection_rect();
        assert!(
            action.area() > 0.0,
            "the selection bar offers a name action"
        );
        click(&mut t, action.center());
        type_str(&mut t, "Second pass");
        dispatch(&mut t, &key_press(Key::Named(NamedKey::Enter)));

        let reqs = captured.borrow();
        assert_eq!(reqs.len(), 1);
        let mut start = [0u8; 16];
        start[0] = 16;
        let mut end = [0u8; 16];
        end[0] = 19;
        assert_eq!(reqs[0].start, HistoryEntryId(start));
        assert_eq!(reqs[0].end, HistoryEntryId(end));
        assert_eq!(reqs[0].label, "Second pass");
        assert_eq!(t.selection(), None, "committing clears the selection");
    }

    fn selection_tracks_stable_ids_across_feed_insertion() {
        let mut timeline = timeline_at(
            Rect::new(0.0, 0.0, 620.0, 600.0),
            vec![
                entry(EntryKind::Text, "Ada", 1, 1),
                entry(EntryKind::Text, "Ada", 3, 3),
            ],
        );
        let selected_id = timeline.entries()[1].id;
        timeline.set_selection(Some((1, 1)));
        timeline.set_entries(vec![
            entry(EntryKind::Text, "Ada", 1, 1),
            entry(EntryKind::Text, "Ada", 2, 2),
            entry(EntryKind::Text, "Ada", 3, 3),
        ]);
        assert_eq!(timeline.selection(), Some((2, 2)));
        assert_eq!(timeline.entries()[2].id, selected_id);
    }

    fn filtering_out_a_selection_clears_destructive_targeting() {
        let mut timeline = timeline_at(
            Rect::new(0.0, 0.0, 620.0, 600.0),
            vec![
                entry(EntryKind::Text, "Ada", 1, 1),
                entry(EntryKind::Text, "Maren", 2, 2),
            ],
        );
        timeline.set_selection(Some((0, 0)));
        timeline.set_filter(HistoryFilter::default().with_author("Maren"));
        assert_eq!(timeline.selection(), None);
    }

    fn a_filter_that_hides_part_of_a_band_says_how_much_it_is_showing() {
        let mut entries = typing_run(10);
        entries[3].entry_kind = EntryKind::Comment;
        entries[4].entry_kind = EntryKind::Comment;
        // Comments split the band automatically, so name the whole run to force
        // one band that a filter can then cut into.
        let ids = (entries[0].id, entries[9].id);
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 500.0), entries);
        t.set_named_ranges(vec![NamedRange {
            id: "tilo:group:rec".into(),
            label: "Morning".into(),
            start: ids.0,
            end: Some(ids.1),
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        }]);
        assert!(
            !t.group_subtitle_at_row(0).unwrap().contains(" of "),
            "unfiltered, there is nothing to qualify: {:?}",
            t.group_subtitle_at_row(0)
        );

        t.set_active_chip(ChipKind::Comments);
        let sub = t.group_subtitle_at_row(0).expect("the band survives");
        assert!(
            sub.starts_with("2 of 10 shown"),
            "a band cut by a filter must say so: {sub:?}"
        );
        assert_eq!(t.visible_len(), 2);
    }

    shift_clicking_two_rows_selects_the_run_between_them_and_names_it();
    selection_tracks_stable_ids_across_feed_insertion();
    filtering_out_a_selection_clears_destructive_targeting();
    a_filter_that_hides_part_of_a_band_says_how_much_it_is_showing();
}

#[test]
fn bands_order_virtualize_and_describe_themselves() {
    fn a_thousand_bands_only_lay_out_the_ones_on_screen() {
        // A thousand distinct authors is a thousand bands. The paint window and
        // the hit-test must both stay proportional to the viewport.
        let entries: Vec<HistoryEntry> = (0u32..2000)
            .map(|i| {
                entry(
                    EntryKind::Text,
                    &format!("Person {}", i / 2),
                    1_000_000u64.saturating_add(u64::from(i)),
                    u8::try_from(i % 251).unwrap_or(u8::MAX),
                )
            })
            .collect();
        let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 620.0), entries);
        assert_eq!(t.visible_len(), 2000);
        assert!(
            t.row_count() >= 1000,
            "two changes each by a thousand people is a thousand bands, got {}",
            t.row_count()
        );

        let range = t.visible_row_range();
        let max_lines_bounded = (620.0_f64 / ROW_HEIGHT).ceil().clamp(0.0, 1_000_000.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=1_000_000 above, so this cast cannot truncate or flip sign"
        )]
        let max_lines = (max_lines_bounded as usize).saturating_add(2);
        assert!(
            range.len() <= max_lines,
            "laid out {} lines for a viewport that holds at most {}",
            range.len(),
            max_lines
        );
        assert_eq!(range.start, 0, "unscrolled, the window starts at the top");
        for i in range.clone() {
            assert!(t.row_rect(i).is_some(), "line {i} is on screen");
        }
        assert!(
            t.row_rect(range.end + 8).is_none(),
            "a line well past the window must not lay out"
        );
    }

    fn the_selection_bar_takes_its_space_out_of_the_scrollable_list() {
        let mut t = timeline_at(Rect::new(0.0, 0.0, 620.0, 620.0), typing_run(20));
        t.set_all_expanded(true);
        let before = t.row_rect(1).unwrap();
        let reachable_before = (0..t.row_count())
            .filter(|i| t.row_rect(*i).is_some())
            .count();

        t.set_selection(Some((16, 19)));
        let bar = t.selection_bar_rect();
        assert!(bar.area() > 0.0, "a selection puts the bar on screen");

        // The rows above it do not move — the bar is not a layout reflow.
        assert_eq!(t.row_rect(1).unwrap(), before);
        // It takes its space out of the viewport, so strictly fewer lines fit.
        let reachable_after = (0..t.row_count())
            .filter(|i| t.row_rect(*i).is_some())
            .count();
        assert!(
            reachable_after < reachable_before,
            "{reachable_after} lines still fit where {reachable_before} did"
        );
        // Nothing inside the bar is a hit target for the list, so a click on
        // [Name these] can never also select a row underneath it.
        for x in [bar.x0 + 4.0, bar.center().x, bar.x1 - 4.0] {
            for y in [bar.y0 + 2.0, bar.center().y, bar.y1 - 2.0] {
                assert_eq!(
                    t.row_at(Point::new(x, y)),
                    None,
                    "({x}, {y}) is inside the bar and must not hit a row"
                );
            }
        }
        // And everything stays reachable: the scroll extent grew by exactly the
        // height the bar took.
        let with_bar = t.max_scroll();
        t.set_selection(None);
        let without_bar = t.max_scroll();
        assert!(
            (with_bar - without_bar - SELECTION_BAR_H).abs() < 0.01,
            "scroll extent moved by {} for a {SELECTION_BAR_H} px bar",
            with_bar - without_bar
        );
    }

    fn bands_are_ordered_by_their_clock_but_never_broken_apart() {
        // A peer's session merges in last while carrying a clock from days
        // ago; ordering rows by clock alone would interleave it with local
        // work, so bands sort by their newest change and stay contiguous.
        let day = 86_400u64;
        let mut entries: Vec<HistoryEntry> = Vec::new();
        for i in 0..4 {
            entries.push(entry(EntryKind::Text, "Alice", NOW - 60 + u64::from(i), i));
        }
        // Peer session, merged after but stamped days earlier.
        for i in 0..3 {
            entries.push(entry(
                EntryKind::Text,
                "Ada",
                NOW - 5 * day + u64::from(i),
                10 + i,
            ));
        }
        let t = timeline_at(Rect::new(0.0, 0.0, 620.0, 900.0), entries);
        let mut tl = t;
        tl.set_all_expanded(true);

        let authors: Vec<&str> = (0..tl.row_count())
            .filter_map(|i| tl.row_entry(i).map(|e| e.author.name.as_str()))
            .collect();
        // Newest-first: Alice's whole band, then Ada's whole band. No interleaving.
        assert_eq!(
            authors,
            vec!["Alice", "Alice", "Alice", "Alice", "Ada", "Ada", "Ada"],
            "the recent band comes first and neither band is broken apart"
        );
        // And the band headers agree with that order.
        let titles: Vec<&str> = (0..tl.row_count())
            .filter_map(|i| tl.group_title_at_row(i))
            .collect();
        assert_eq!(titles.len(), 2, "two bands: {titles:?}");

        // `visible` walks the same order, so a count and a position agree.
        let visible_authors: Vec<&str> = (0..tl.visible_len())
            .map(|i| tl.visible_entry(i).unwrap().author.name.as_str())
            .collect();
        assert_eq!(visible_authors, authors);

        tl.set_newest_first(false);
        let authors: Vec<&str> = (0..tl.row_count())
            .filter_map(|i| tl.row_entry(i).map(|e| e.author.name.as_str()))
            .collect();
        assert_eq!(
            authors,
            vec!["Ada", "Ada", "Ada", "Alice", "Alice", "Alice", "Alice"],
            "oldest-first flips the bands, and still does not break them"
        );
    }

    fn the_accessible_description_counts_bands_as_well_as_changes() {
        let t = timeline_at(Rect::new(0.0, 0.0, 520.0, 500.0), typing_run(20));
        let node = Widget::accessibility(&t).unwrap();
        assert_eq!(
            node.description.as_deref(),
            Some("20 entries in 1 groups"),
            "a screen reader must not be told '20 items' over one collapsed band"
        );
    }

    a_thousand_bands_only_lay_out_the_ones_on_screen();
    the_selection_bar_takes_its_space_out_of_the_scrollable_list();
    bands_are_ordered_by_their_clock_but_never_broken_apart();
    the_accessible_description_counts_bands_as_well_as_changes();
}

#[test]
fn day_headings_bucket_changes_into_today_yesterday_this_week_and_earlier() {
    // 1970-01-01 was Thursday. Day 8 is Friday 1970-01-09; that week's
    // Monday is day 4. Today / yesterday / mid-week / previous week.
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(8 * 86_400 + 12 * 3_600))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let secs = |day: u64, hour: u64| day * 86_400 + hour * 3_600;
    let entries = vec![
        entry(EntryKind::Text, "Ada", secs(8, 10), 1),
        entry(EntryKind::Text, "Maren", secs(7, 16), 2),
        entry(EntryKind::Text, "Ada", secs(5, 9), 3),
        entry(EntryKind::Text, "Bob", secs(2, 11), 4),
    ];
    let mut t = ActivityTimeline::with_entries(entries);
    t.set_now_for_test(now);
    t.set_newest_first(true);
    t.set_all_expanded(true);
    t.set_show_day_headings(true);
    let headings: Vec<String> = (0..t.row_count())
        .filter_map(|i| t.group_title_at_row(i).map(str::to_string))
        .filter(|title| {
            matches!(
                title.as_str(),
                "Today" | "Yesterday" | "This week" | "Earlier"
            )
        })
        .collect();
    assert_eq!(
        headings,
        vec!["Today", "Yesterday", "This week", "Earlier"],
        "newest-first calendar headings, not work-band titles"
    );
    assert!(
        (0..t.row_count()).any(|i| t.row_entry(i).is_some_and(|e| e.author.name == "Ada")),
        "Today still lists today's change"
    );
}
