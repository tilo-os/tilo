use super::*;
use crate::synthetic_input::{cursor_moved, mouse_press};
use crate::testing::dispatch;
use crate::time::Duration;
use crate::widget::history::HistoryAuthor;
use automerge::AutoCommit;
use collab_types::ArtifactKind;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn a_bots_change_is_marked_as_the_bots_and_an_agents_is_not() {
    let acting = |kind| {
        HistoryAuthor::new("Standup Bot", 120.0, "ab12")
            .with_relation(crate::widget::history::AuthorRelation::Software)
            .with_attribution(crate::widget::history::HistoryAttribution {
                actor: "bot:standup".into(),
                kind,
                on_behalf_of: Some("ada".into()),
                on_behalf_of_name: Some("Ada".into()),
            })
    };
    assert!(is_bot_author(&acting(tilo_actions::ActorKind::Bot)));
    assert!(!is_bot_author(&acting(tilo_actions::ActorKind::Agent)));
    assert!(!is_bot_author(&HistoryAuthor::new("Ada", 10.0, "cd34")));
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "probe coordinates are non-negative pixel positions inside the test surface"
)]
fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
    let (r, g, b, _) =
        crate::offscreen::OffscreenRenderer::sample(rgba, w, x.floor() as u32, y.floor() as u32);
    crate::a11y::relative_luminance(Color::from_rgb8(r, g, b))
}

/// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
    let dir =
        std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?).join(theme.name.to_lowercase());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// Eight changes by two authors — enough for a band and loose rows —
/// with two rows selected so the selection bar and its chips paint.
fn ux1x_fixture() -> ActivityTimeline {
    let entries: Vec<HistoryEntry> = (0..8u8)
        .map(|i| {
            let who = if i < 5 { "Alice" } else { "Bob" };
            entry(EntryKind::Text, who, 1_000 + u64::from(i) * 30, i)
        })
        .collect();
    let mut t = ActivityTimeline::with_entries(entries);
    t.set_now_for_test(
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2_000))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    );
    t.set_all_expanded(true);
    t.set_selection(Some((5, 6)));
    t
}

#[test]
fn provenance_and_author_pill_presentation() {
    /// The pane, its quiet chips, the band backdrops and the selected rows
    /// keep a **visible derived edge** at 1× on both presets (HIG §9.5).
    /// Before, the pane wore `border_standard` (three shades from the dark
    /// raised surface), chips and bands literal white veneers, hover
    /// `neutral[3]`, and the selection bar the focus ring.
    ///
    /// Writes `$UX1X_F4_OUT/<theme>/activity_timeline.png` plus the harness
    /// inspection when that variable is set; asserts regardless.
    fn ux1x_pane_chips_and_bands_keep_a_derived_edge_on_both_presets() {
        use crate::offscreen::OffscreenRenderer;
        const EDGE_MIN: f64 = 0.008;
        let margin = 16.0;
        let (pw, ph) = (520.0, 520.0);
        // Evidence first, for both presets, so a failing assertion on one
        // preset still leaves the other's PNG and inspection on disk.
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            if let Some(png) = ux1x_out(&theme, "activity_timeline.png") {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test viewport"
                )]
                let harness = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(ux1x_fixture()),
                    (pw as u32, ph as u32),
                    theme.clone(),
                );
                harness.render_png(&png);
                if let Ok(json) = harness.inspection_json() {
                    std::fs::write(png.with_extension("inspection.json"), json)
                        .expect("write inspection");
                }
            }
        }
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            let mut t = ux1x_fixture();
            let pane = Rect::new(margin, margin, margin + pw, margin + ph);
            t.set_rect(pane);
            let (w, h) = (pane.x1 + margin, pane.y1 + margin);
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            t.place_controls(&theme);
            t.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small test surface"
            )]
            let rgba = OffscreenRenderer::new(w as u32, h as u32)
                .render(&scene, theme.tokens.colors.surfaces.content);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small test surface"
            )]
            let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

            // Pane edge against its own fill, on the pane's centre line (clear
            // of the corner arcs; rows start one `PADDING` in, so the columns
            // beside the edge are bare fill).
            let y = pane.center().y;
            let edge = lum(pane.x0, y);
            let fill = lum(pane.x0 + 3.0, y);
            assert!(
                (edge - fill).abs() >= EDGE_MIN,
                "{}: pane edge {edge:.4} vs fill {fill:.4} did not register",
                theme.name
            );

            // Quiet chip (the second filter chip, not the active first one):
            // its top edge — the straight run of the capsule, where the 1 px
            // stroke covers the whole pixel row — against its own fill just
            // under it, above the label's line box. The accessor answers the
            // pointer target, so recover the painted capsule from the strip.
            let hit = t.chip_rect(ChipKind::Edits).expect("a second filter chip");
            let strip = t.filter_strip_rect();
            let chip_h = (strip.height() - 8.0).max(20.0);
            let chip_top = strip.center().y - chip_h / 2.0;
            let cx = hit.center().x;
            let cedge = lum(cx, chip_top);
            let cfill = lum(cx, chip_top + 3.0);
            assert!(
                (cedge - cfill).abs() >= EDGE_MIN,
                "{}: chip edge {cedge:.4} vs fill {cfill:.4} did not register",
                theme.name
            );

            let band_row = (0..t.row_count())
                .find(|i| t.group_at_row(*i).is_some())
                .expect("two authors' runs produce a band");
            let band = t.row_rect(band_row).expect("the band row is laid out");
            let inner = Rect::new(band.x0, band.y0 + 2.0, band.x1, band.y1 - 2.0);
            let bfill = lum(inner.x1 - 3.0, inner.center().y);
            let pane_fill = lum(pane.x1 - PADDING / 2.0, inner.center().y);
            assert!(
                (bfill - pane_fill).abs() >= EDGE_MIN,
                "{}: band {bfill:.4} vs pane {pane_fill:.4} did not register",
                theme.name
            );
        }
    }

    fn provenance_breadcrumbs_and_collapsed_mixed_scope_survive_hidden_rows() {
        use tilo_actions::ActionContext;
        let mut files = entry(EntryKind::Text, "Alice", 10, 1);
        files.context = Some(
            ActionContext::for_resource("files:brief", "files.entry", "brief.md")
                .with_app("org.tilo.Files", "Files")
                .with_group("workspace:a", "Alpha", "workspace"),
        );
        let mut notes = entry(EntryKind::Text, "Alice", 11, 2);
        notes.context = Some(
            ActionContext::for_resource("notes:plan", "notes.document", "plan")
                .with_app("org.tilo.Notes", "Notes")
                .with_group("workspace:a", "Beta", "workspace"),
        );
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), vec![files, notes]);
        assert_eq!(
            ActivityTimeline::entry_breadcrumb(&timeline.entries()[0]).as_deref(),
            Some("Files / brief.md")
        );
        timeline.set_all_expanded(false);
        let group = (0..timeline.row_count())
            .find(|row| timeline.group_at_row(*row).is_some())
            .expect("collapsed group");
        let semantics = Widget::accessibility(&timeline).expect("timeline semantics");
        let description = semantics.children[group]
            .description
            .as_deref()
            .unwrap_or_default();
        assert!(description.contains("Alpha") && description.contains("Files"));
        assert!(description.contains("Beta") && description.contains("Notes"));
    }

    fn host_can_suppress_duplicate_author_pill_without_hiding_op_filters() {
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), vec![]);
        assert!(timeline.shows_author_pill());
        timeline.set_show_author_pill(false);
        assert!(!timeline.shows_author_pill());
        assert_eq!(timeline.author_pill_rect(), Rect::ZERO);
    }

    fn a_task_band_says_it_is_a_task_and_a_named_run_does_not() {
        use tilo_actions::ActionContext;
        let mut entries = vec![
            entry(EntryKind::Text, "Alice", 10, 1),
            entry(EntryKind::Text, "Bob", 11, 2),
        ];
        for entry in &mut entries {
            entry.context =
                Some(ActionContext::default().with_group("task:7", "Prepare budget", "task"));
        }
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), entries);
        timeline.set_all_expanded(false);
        let spoken = |timeline: &ActivityTimeline| {
            let group = (0..timeline.row_count())
                .find(|row| timeline.group_at_row(*row).is_some())
                .expect("one band");
            Widget::accessibility(timeline)
                .expect("timeline semantics")
                .children[group]
                .description
                .clone()
                .unwrap_or_default()
        };
        let task = crate::i18n::I18nManager::lookup_plain("history-work-task");
        assert!(
            spoken(&timeline).starts_with(&task),
            "{}",
            spoken(&timeline)
        );

        for entry in &mut timeline.entries {
            entry.context = None;
        }
        timeline.set_named_ranges(vec![NamedRange {
            id: "tilo:group:01".into(),
            label: "Budget".into(),
            start: timeline.entries[0].id,
            end: Some(timeline.entries[1].id),
            named_by: None,
            created: SystemTime::UNIX_EPOCH,
        }]);
        assert!(!spoken(&timeline).starts_with(&task));
    }

    fn a_described_change_reads_as_its_description_and_falls_back_to_its_summary() {
        let entry = entry(EntryKind::Text, "Alice", 10, 1);
        let id = entry.id;
        let mut timeline = timeline_at(Rect::new(0.0, 0.0, 300.0, 400.0), vec![entry]);
        let spoken = |timeline: &ActivityTimeline| {
            Widget::accessibility(timeline)
                .expect("timeline semantics")
                .children
                .iter()
                .filter_map(|node| node.name.clone())
                .collect::<Vec<_>>()
                .join("|")
        };
        let raw = timeline.entries()[0].summary.clone();
        assert!(spoken(&timeline).contains(&raw));
        timeline.set_descriptions(std::collections::HashMap::from([(
            id,
            "Noted that costs rose".to_owned(),
        )]));
        assert!(spoken(&timeline).contains("Noted that costs rose"));
        assert_eq!(
            timeline.summary_of(&timeline.entries()[0]),
            "Noted that costs rose"
        );
        timeline.set_descriptions(std::collections::HashMap::new());
        assert!(spoken(&timeline).contains(&raw));
    }

    ux1x_pane_chips_and_bands_keep_a_derived_edge_on_both_presets();
    provenance_breadcrumbs_and_collapsed_mixed_scope_survive_hidden_rows();
    a_described_change_reads_as_its_description_and_falls_back_to_its_summary();
    a_task_band_says_it_is_a_task_and_a_named_run_does_not();
    host_can_suppress_duplicate_author_pill_without_hiding_op_filters();
}

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
    crate::testing::lay_out_in(t, 620.0, 560.0, theme).expect("the pane lays out");
}

fn click(t: &mut ActivityTimeline, p: Point) {
    dispatch(t, &cursor_moved(p.x, p.y));
    dispatch(t, &mouse_press());
}

/// A control's place is the layout's: a frame that changed its selection
/// or began a name and has laid out but not yet painted still finds the
/// strip's author pill and the bar's actions inside the pane, where it
/// would draw them — not nowhere, at the window's origin, where a control
/// nothing placed stands.
#[test]
fn a_selection_and_a_name_place_the_controls_in_the_layout() {
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

#[test]
fn a_filter_with_nothing_left_places_the_way_back_out_in_the_layout() {
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

/// An app's presenter decides how its changes read: a face's words name
/// the row, its body sets the row's height, choosing a change it opens
/// hands it to the app instead of selecting it, and Copy copies what the
/// face copies. Changes without a face keep their sentence.
#[test]
fn a_presented_change_reads_opens_and_copies_as_the_app_says() {
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
    let _host = crate::host::enter(&crate::host::HostServices::null());
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
        crate::text_selection::clipboard_paste().as_deref(),
        Some("3")
    );

    assert!(timeline.activate_row(row));
    assert_eq!(*opened.borrow(), vec![presented_id], "the app takes it up");
    assert!(timeline.selection.is_none(), "opening is not selecting");
}

#[test]
fn a_presenter_says_what_a_band_amounts_to() {
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

#[test]
fn a_genesis_row_names_no_author_and_no_time() {
    use crate::a11y::Role;
    let mut genesis = entry(EntryKind::Details, "Unknown participant", 0, 1);
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

#[test]
fn timeline_rows_are_accessible_keyboard_and_hover_targets() {
    fn accessibility_reports_list_role_name_and_count() {
        use crate::a11y::Role;
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
        use crate::a11y::{Action, Role};
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
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
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
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
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

    fn hover_row_tracks_cursor_y() {
        let entries = vec![
            entry(EntryKind::Text, "Alice", 1, 1),
            entry(EntryKind::Text, "Alice", 2, 2),
            entry(EntryKind::Text, "Alice", 3, 3),
        ];
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 400.0), entries);
        let row1 = t.row_rect(1).unwrap();
        dispatch(&mut t, &cursor_moved(row1.center().x, row1.center().y));
        assert_eq!(t.hover_row(), Some(1));
        let row2 = t.row_rect(2).unwrap();
        dispatch(&mut t, &cursor_moved(row2.center().x, row2.center().y));
        assert_eq!(t.hover_row(), Some(2));
        // Cache the y bound first so the borrow checker doesn't see a live
        // `&t` while we hand `&mut t` to `dispatch`.
        let far_y = t.rect.y1 + 1000.0;
        dispatch(&mut t, &cursor_moved(0.0, far_y));
        assert_eq!(t.hover_row(), None);
    }

    accessibility_reports_list_role_name_and_count();
    individual_entries_have_stable_accessible_targets_and_activate();
    keyboard_navigation_moves_focus_and_selects_individual_rows();
    hover_row_tracks_cursor_y();
}

/// Only the pub fields the widget reads are overwritten.
fn key_press(logical_key: winit::keyboard::Key) -> WindowEvent {
    crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
        logical_key,
        None,
        ElementState::Pressed,
    ))
}

#[test]
fn empty_and_filtered_timelines_explain_themselves() {
    fn empty_timeline_paints_placeholder() {
        let t = timeline_at(Rect::new(0.0, 0.0, 400.0, 300.0), vec![]);
        assert_eq!(t.visible_len(), 0);
        let strip = t.filter_strip_rect();
        assert!(strip.y0 >= t.rect.y0);
        assert!(strip.y1 < t.rect.y1);
        assert!(t.row_rect(0).is_none());
    }

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

    empty_timeline_paints_placeholder();
    an_empty_history_and_a_filtered_one_explain_themselves_differently();
    a_host_time_range_is_named_in_the_explanation();
}

#[test]
fn filters_narrow_by_entry_kind_and_author_and_clear() {
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

    fn chip_strip_active_state_round_trips() {
        let mut t = timeline_at(Rect::new(0.0, 0.0, 400.0, 200.0), vec![]);
        assert_eq!(t.active_chip(), ChipKind::All);
        t.set_active_chip(ChipKind::Governance);
        assert_eq!(t.active_chip(), ChipKind::Governance);
        let entries = vec![
            entry(EntryKind::Comment, "Alice", 1, 1),
            entry(EntryKind::Access, "Bob", 2, 2),
        ];
        t.set_entries(entries);
        assert_eq!(t.visible_len(), 1, "Governance chip narrows to govern row");
        t.set_active_chip(ChipKind::All);
        assert_eq!(t.visible_len(), 2);
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
    chip_strip_active_state_round_trips();
    every_strip_chip_takes_a_press_before_anything_paints();
    people_chips_take_a_press_before_anything_paints();
    clear_filters_resets_the_widget_and_tells_the_host();
    filter_by_author_typeahead_narrows_list();
    typing_narrows_by_author_and_escape_or_backspace_widens_again();
}

#[test]
fn revert_chip_fires_only_for_revertible_ops() {
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

#[test]
fn rows_order_newest_first_and_virtualize_within_narrow_panes() {
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

    fn only_the_visible_window_of_a_huge_list_is_touched_per_frame() {
        // Paint and hit-test must look at the ~15 rows on screen, not all
        // ten thousand, or a workspace-scale history drops frames.
        let entries: Vec<HistoryEntry> = (0u32..10_000)
            .map(|i| {
                entry(
                    EntryKind::Text,
                    "Alice",
                    u64::from(i),
                    u8::try_from(i % 251).unwrap_or(u8::MAX),
                )
            })
            .collect();
        let t = timeline_at(Rect::new(0.0, 0.0, 600.0, 600.0), entries);
        assert_eq!(t.visible_len(), 10_000, "every row passes the filter");

        let range = t.visible_row_range();
        let viewport_rows_bounded = (t.viewport_height() / ROW_HEIGHT)
            .ceil()
            .clamp(0.0, 1_000_000.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=1_000_000 above, so this cast cannot truncate or flip sign"
        )]
        let viewport_rows = viewport_rows_bounded as usize;
        assert!(
            range.len() <= viewport_rows.saturating_add(2),
            "painted {} rows for a viewport that holds {}",
            range.len(),
            viewport_rows
        );
        assert_eq!(range.start, 0, "unscrolled, the window starts at the top");

        for i in range.clone() {
            assert!(t.row_rect(i).is_some(), "row {i} is on screen");
        }
        assert!(
            t.row_rect(range.end.saturating_add(4)).is_none(),
            "a row well past the window must not lay out"
        );
    }

    fn row_at_agrees_with_row_rect_at_the_boundaries() {
        // `row_at` binary-searches the cumulative row tops; must still agree
        // with `row_rect` one pixel either side of a seam between two rows
        // of *different* heights.
        let entries: Vec<HistoryEntry> = (0u8..40)
            .map(|i| entry(EntryKind::Text, "Alice", u64::from(i), i))
            .collect();
        let mut t = timeline_at(Rect::new(0.0, 0.0, 500.0, 500.0), entries);
        // Forty changes by one author in forty seconds is one band, and it
        // collapses. Open it so the list has both kinds of line in it.
        t.set_all_expanded(true);
        assert!(
            matches!(t.group_at_row(0), Some((_, true))),
            "line 0 is the opened band header"
        );
        assert!(t.row_entry(1).is_some(), "line 1 is a change inside it");
        assert!(
            (t.row_rect(0).unwrap().height() - t.row_rect(1).unwrap().height()).abs() > 1.0,
            "the two line kinds must be different heights for this test to mean anything"
        );
        for i in 0..8 {
            let r = t.row_rect(i).unwrap();
            assert_eq!(t.row_at(r.center()), Some(i));
            assert_eq!(t.row_at(Point::new(r.center().x, r.y0 + 0.5)), Some(i));
            assert_eq!(
                t.row_at(Point::new(r.center().x, r.y1 - 0.5)),
                Some(i),
                "the bottom edge of row {i} still belongs to it"
            );
        }
        let r0 = t.row_rect(0).unwrap();
        assert_eq!(t.row_at(Point::new(r0.x0 - 4.0, r0.center().y)), None);
        assert_eq!(t.row_at(Point::new(250.0, 499.9)), None);
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
            narrow_chip.x0 > narrow_row.x0 + narrow.author_slot_w(),
            "the chip must not overlap the author cluster"
        );
        assert!(
            narrow_chip.x1 <= narrow_row.x1,
            "and must stay inside the row"
        );
    }

    entries_sharing_a_timestamp_still_order_newest_first();
    newest_first_reverses_chronological_order();
    only_the_visible_window_of_a_huge_list_is_touched_per_frame();
    row_at_agrees_with_row_rect_at_the_boundaries();
    narrow_panes_shrink_the_trailing_slots_rather_than_overlapping_them();
}
