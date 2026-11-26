//! The activity timeline's selected rows, bands and quiet chips keep a visible
//! derived edge at 1x on both presets, and its faces, breadcrumbs and bands read
//! as the change they describe.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::NamedRange;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::activity_timeline::ChipKind;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "probe coordinates are non-negative pixel positions inside the test surface"
)]
fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
    let (r, g, b, _) = OffscreenRenderer::sample(rgba, w, x.floor() as u32, y.floor() as u32);
    tilo_ui_base::a11y::relative_luminance(Color::from_rgb8(r, g, b))
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
    /// Writes `$UX1X_F4_OUT/<theme>/activity_timeline.png` plus the driver's
    /// inspection when that variable is set; asserts regardless.
    fn ux1x_pane_chips_and_bands_keep_a_derived_edge_on_both_presets() {
        use tilo_ui_base::offscreen::OffscreenRenderer;
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
                let harness = tilo_ui::app::driver::Driver::new_with_theme(
                    tilo_ui::State::new(),
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
            t.place_controls_for_tests(&theme);
            t.draw(&mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                PresenceCtx::empty().draw_env(),
            ));
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
            let pane_fill = lum(
                pane.x1 - tilo_ui::widget::ActivityTimeline::pane_padding_for_tests() / 2.0,
                inner.center().y,
            );
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
            ActivityTimeline::entry_breadcrumb_for_tests(&timeline.entries()[0]).as_deref(),
            Some("Files / brief.md")
        );
        // An app's own collection is the app, by the name a person knows it: never the
        // collection's kind ("Issues", "Folder"), nor the app's id.
        for (app, label, kind, collection, said) in [
            (
                "so.tilo.tasks",
                "so.tilo.tasks",
                "tasks.issues",
                "Issues",
                "Tasks",
            ),
            (
                "so.tilo.file-explorer",
                "so.tilo.file-explorer",
                "files.folder",
                "Folder",
                "Files",
            ),
            ("so.tilo.chat", "Chat", "chat.messages", "Messages", "Chat"),
        ] {
            let mut own = entry(EntryKind::Text, "Alice", 12, 3);
            own.context =
                Some(ActionContext::for_resource(kind, kind, collection).with_app(app, label));
            assert_eq!(
                ActivityTimeline::entry_breadcrumb_for_tests(&own).as_deref(),
                Some(said),
                "{app}'s {collection}"
            );
        }
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
        let task = tilo_ui_base::i18n::I18nManager::lookup_plain("history-work-task");
        assert!(
            spoken(&timeline).starts_with(&task),
            "{}",
            spoken(&timeline)
        );

        for entry in timeline.entries_mut_for_tests() {
            entry.context = None;
        }
        timeline.set_named_ranges(vec![NamedRange {
            id: "tilo:group:01".into(),
            label: "Budget".into(),
            start: timeline.entries()[0].id,
            end: Some(timeline.entries()[1].id),
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
}

fn entry(op: EntryKind, author: &str, ts: u64, id: u8) -> HistoryEntry {
    let mut id_bytes = [0u8; 16];
    id_bytes[0] = id;
    HistoryEntry {
        id: HistoryEntryId(id_bytes),
        author: HistoryAuthor::new(author, 120.0, "deadbeef"),
        entry_kind: op,
        artifact_kind: collab_types::ArtifactKind::Note,
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
    t.set_rect(rect);
    t.set_now_for_test(
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2_000_000))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    );
    t.set_newest_first(false); // ascending order for clarity
    t
}
