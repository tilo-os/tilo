//! The alignment the editor's chrome promises, read back from the production
//! layout functions rather than from a copy of their arithmetic: a control
//! cluster shares one centre line, the two pane headings share one band, a
//! sibling row keeps the clearance its neighbours have, and the topbar is
//! inset the same at both edges.
//!
//! A child module of `app` so it can reach the private geometry — `regions`,
//! `transport_buttons`, `inspector_layout` — which is exactly what the draw
//! pass and the hit-test consume.

use super::*;

/// A seeded project laid out at `w`×`h`, the way `--shots` composes one.
fn app_sized(w: f64, h: f64) -> VideoEditorApp {
    let mut doc = AutoCommit::new();
    VideoDoc::seed_sample(&mut doc);
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.sync(&mut doc, &automerge::ROOT);
    app
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{what}: {actual} != {expected}"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn transport_and_topbar_share_centre_lines_insets_and_steps() {
    // Case `the_transport_cluster_shares_one_centre_line`.
    {
        let app = app_sized(1280.0, 800.0);
        let r = app.regions();
        let cy = r.transport.y0 + TRANSPORT_CONTROL_CY;
        for (i, (_, rect)) in app.transport_buttons(&r).into_iter().enumerate() {
            assert_close(rect.center().y, cy, &format!("transport button {i} centre"));
        }
        let history = app.history_buttons(&r);
        assert!(
            !history.is_empty(),
            "a 1280px strip carries the history buttons"
        );
        for (verb, rect) in history {
            assert_close(rect.center().y, cy, &format!("{verb:?} button centre"));
        }

        // The readout, at the sizes the strip draws it: the playhead time centred
        // on the same line, every smaller part standing on its foot.
        let mut tr = TextRenderer::new();
        let heights = [
            f64::from(
                tr.measure_weighted("00:10:00", 18.0, None, TextWeight::Bold)
                    .1,
            ),
            f64::from(tr.measure("/  00:28:00", 13.0, None).1),
            f64::from(tr.measure("SHUTTLE ▶▶ 8×", 10.0, None).1),
            f64::from(tr.measure("1920×1080 · 30 fps", 11.5, None).1),
        ];
        let tops = readout_tops(r.transport, cy, heights);
        assert_close(tops[0] + heights[0] / 2.0, cy, "playhead time centre");
        let foot = tops[0] + heights[0];
        for (top, height) in tops.iter().zip(heights).skip(1) {
            assert_close(top + height, foot, "readout part foot");
            assert!(
                *top >= tops[0],
                "a smaller part never starts above the playhead time"
            );
        }
    }
    // Case `a_tall_readout_is_held_inside_the_strip_rather_than_pushed_above_it`.
    {
        // Line boxes at a 2× text scale: taller than the strip's upper half.
        let transport = Rect::new(0.0, 100.0, 700.0, 100.0 + TRANSPORT_H);
        let heights = [45.0, 32.5, 25.0, 28.75];
        let tops = readout_tops(transport, transport.y0 + TRANSPORT_CONTROL_CY, heights);
        assert!(
            tops[0] >= transport.y0 + 2.0,
            "the playhead time keeps its top inside the strip: {}",
            tops[0]
        );
        let foot = tops[0] + heights[0];
        for (top, height) in tops.iter().zip(heights).skip(1) {
            assert_close(top + height, foot, "readout part foot at large text");
        }
    }
    // Case `the_topbar_commands_and_side_toggles_keep_one_sibling_step_at_every_width`.
    {
        for w in [390.0, 480.0, 768.0, 1280.0] {
            let app = app_sized(w, 800.0);
            let r = app.regions();
            let mut row: Vec<(&str, Rect)> = [
                ("menu", r.menu_btn),
                ("project", r.project_btn),
                ("inspector", r.inspector_btn),
                ("import", r.import_btn),
                ("split", r.split_btn),
                ("export", r.export_btn),
            ]
            .into_iter()
            .filter(|(_, rect)| rect.width() > 0.0)
            .collect();
            row.sort_by(|a, b| a.1.x0.total_cmp(&b.1.x0));
            // The trailing cluster (inspector toggle, Import, Split, Export) is
            // packed at exactly the sibling step; the leading controls keep at
            // least that much from whatever follows them — the project button is
            // width-bounded and may leave the middle of the bar empty, but never
            // creeps closer than a step to the trailing cluster.
            let trailing = ["inspector", "import", "split", "export"];
            for pair in row.windows(2) {
                let gap = x_gap(pair[0].1, pair[1].1);
                assert!(
                    gap >= TOPBAR_GAP - 1e-6,
                    "{w}px: {} ↔ {} gap {gap}, want at least {TOPBAR_GAP}",
                    pair[0].0,
                    pair[1].0
                );
                if trailing.contains(&pair[0].0) && trailing.contains(&pair[1].0) {
                    assert_close(
                        gap,
                        TOPBAR_GAP,
                        &format!("{w}px: {} ↔ {}", pair[0].0, pair[1].0),
                    );
                }
            }
            const { assert!(TOPBAR_GAP >= S1, "the topbar step is on the scale") };
            let (first, last) = (row.first().unwrap().1, row.last().unwrap().1);
            assert_close(
                first.x0 - r.topbar.x0,
                TOPBAR_EDGE,
                &format!("{w}px leading inset"),
            );
            assert_close(
                r.topbar.x1 - last.x1,
                TOPBAR_EDGE,
                &format!("{w}px trailing inset"),
            );
        }
    }
    // Case `the_topbar_is_inset_equally_at_both_edges_and_its_commands_share_one_width`.
    {
        let app = app_sized(1280.0, 800.0);
        let r = app.regions();
        assert!(!r.bin_overlay, "1280px docks the bin");
        assert_close(
            r.project_btn.x0 - r.topbar.x0,
            r.topbar.x1 - r.export_btn.x1,
            "leading inset vs trailing inset",
        );
        for (name, rect) in [
            ("import", r.import_btn),
            ("split", r.split_btn),
            ("export", r.export_btn),
        ] {
            assert_close(rect.width(), TOPBAR_BTN_W, &format!("{name} width"));
            assert_close(
                rect.center().y,
                r.topbar.y0 + TOPBAR_H / 2.0,
                &format!("{name} centre on the topbar's midline"),
            );
        }
    }
    // Case `the_go_to_field_stacks_its_three_lines_above_the_transport`:
    // The go-to field is three measured lines — caption, timecode, hint — one
    // step apart, anchored above the transport whose playhead it moves; at 2×
    // the timecode no longer draws through the hint.
    {
        for scale in [1.0, 2.0] {
            let (mut tr, theme) = scaled(scale);
            let app = app_sized(1280.0, 800.0);
            let r = app.regions();
            let l = app.goto_layout(&r, &mut tr, &theme);
            let ty = l.type_sizes;
            let (line_xs, timecode_line) = (line(&mut tr, ty.xs), line(&mut tr, ty.xl));
            assert!(
                l.label_top >= l.card.y0,
                "@{scale}x: the caption starts in the card"
            );
            assert!(
                l.label_top + line_xs <= l.entry_top,
                "@{scale}x: the caption runs into the timecode"
            );
            assert!(
                l.entry_top + timecode_line <= l.hint_top,
                "@{scale}x: the timecode runs into the hint"
            );
            assert!(
                l.hint_top + line_xs <= l.card.y1,
                "@{scale}x: the hint runs off the card"
            );
            assert!(
                l.card.y1 <= r.transport.y0,
                "@{scale}x: the field sits above the transport"
            );
            assert!(
                l.card.x0 >= app.rect.x0 && l.card.x1 <= app.rect.x1,
                "@{scale}x: the field fits the window"
            );
        }
    }
}

#[test]
fn bin_track_heads_and_captions_keep_token_clearances() {
    // Case `the_bin_heading_chip_sits_on_the_heading_band_and_clear_of_the_list`.
    {
        let app = app_sized(1280.0, 800.0);
        let r = app.regions();
        let add = app.add_title_rect(&r);
        assert_close(
            add.center().y,
            r.bin.y0 + PANE_HEADING_Y0 + PANE_HEADING_H / 2.0,
            "\"+ Title\" centre against the heading band",
        );
        assert_close(add.height(), PANE_HEADING_H, "\"+ Title\" height");
        assert!(
            add.y1 <= r.bin.y0 + VideoEditorApp::BIN_LIST_TOP,
            "the heading row ends before the first bin row starts"
        );
        // The export chip lives in the strip under the topbar, beside the
        // inspector's heading: same band, same centre line.
        assert_close(
            r.export_chip.center().y,
            r.bin.y0 + PANE_HEADING_Y0 + PANE_HEADING_H / 2.0,
            "export chip centre against the heading band",
        );
    }
    // Case `the_transition_caption_keeps_the_clearance_every_other_row_has`.
    {
        let app = app_sized(1280.0, 800.0);
        let r = app.regions();
        let l = app.inspector_layout(&r);
        let opacity = l.opacity.expect("a video clip is selected");
        let brightness = l.brightness.expect("a video clip has a grade");
        let fade = l.fade_in.expect("every clip has a fade row");
        // A slider label is drawn at `track.y0 - 20`, the transition caption at
        // `transition_y - 20`: equal advances from the row above mean equal
        // clearance over that row's knobs.
        assert_close(
            l.transition_y - fade.y1,
            brightness.y0 - opacity.y1,
            "transition caption clearance vs an ordinary slider row",
        );
    }
    // Case `every_track_head_clears_its_lane_and_its_neighbours_by_a_token_step`.
    {
        let app = app_sized(1280.0, 800.0);
        let r = app.regions();
        let lanes = app.lane_rects(&r);
        assert!(lanes.len() >= 4, "the sample project has four lanes");
        for pair in lanes.windows(2) {
            assert_close(pair[1].2.y0 - pair[0].2.y1, S1, "lane ↔ lane");
        }
        for (_, kind, lane) in &lanes {
            let head = VideoEditorApp::track_head_rect(&r, lane);
            assert_close(lane.x0 - head.x1, S1, "head ↔ lane stripe");
            let h = chrome_layout::track_head(head);
            assert!(
                h.meter.y0 - h.kind.y1 >= S1,
                "{kind:?}: kind line ↔ meter gap {}",
                h.meter.y0 - h.kind.y1
            );
            assert!(
                head.contains(h.solo.center()) && head.contains(h.meter.center()),
                "{kind:?}: every band inside the head"
            );
        }
        // The add-track footer sits an `S2` under the last lane, and its two
        // buttons an `S2` apart.
        let last = lanes.last().unwrap().2;
        let buttons = app.add_track_buttons(&r);
        assert_eq!(buttons.len(), 2, "both add-track buttons fit at 800px");
        assert_close(buttons[0].1.y0 - last.y1, S2, "last lane ↔ add-track row");
        assert_close(x_gap(buttons[0].1, buttons[1].1), S2, "+ Video ↔ + Audio");
    }
    // Case `the_bin_rows_and_the_row_controls_keep_token_gaps`.
    {
        let app = app_sized(1280.0, 800.0);
        let r = app.regions();
        let rows = app.bin_item_rects(&r);
        assert!(rows.len() >= 2);
        for pair in rows.windows(2) {
            assert_close(pair[1].1.y0 - pair[0].1.y1, S2, "bin row ↔ bin row");
        }
        let row = Rect::new(0.0, 0.0, 276.0, 26.0);
        let (name, note, rename, del) = marker_row_parts(row);
        assert!(x_gap(name, note) >= S1, "marker name ↔ note button");
        assert_close(x_gap(note, rename), S1, "marker note ↔ rename");
        assert_close(x_gap(rename, del), S1, "marker rename ↔ delete");
        // Effects use a 24px base row (markers use 26px); the inspector scales
        // that base for large text, including its token gaps.
        let effect_row = Rect::new(row.x0, row.y0, row.x1, row.y0 + 24.0);
        let (key, up, down, slider, remove) = VideoEditorApp::effect_row_parts(effect_row);
        assert_close(x_gap(key, up), S1, "effect key ↔ up");
        assert_close(x_gap(up, down), S1, "effect up ↔ down");
        assert!(x_gap(down, slider) >= S1, "effect down ↔ slider");
        assert_close(x_gap(slider, remove), S2, "effect slider ↔ remove");
    }
}

/// The gap between two rects that neighbour on the x axis, negative when they
/// overlap.
fn x_gap(left: Rect, right: Rect) -> f64 {
    right.x0 - left.x1
}

/// A renderer and theme at `scale`, the way the host pairs them: the theme
/// carries the text scale in its tokens and the renderer applies the same
/// factor to every base size it is handed.
fn scaled(scale: f32) -> (TextRenderer, Theme) {
    let mut tr = TextRenderer::new();
    tr.set_text_scale(scale);
    (tr, Theme::dark().with_text_scale(scale))
}

fn line(tr: &mut TextRenderer, size: f32) -> f64 {
    f64::from(tr.measure("Hg", size, None).1)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn dialogs_and_sheets_stay_on_their_card_at_every_text_scale() {
    // Case `the_export_dialog_stacks_its_body_without_overlap_at_every_text_scale`:
    // The export dialog's body is a stack of measured lines: at 1× and at 2×
    // alike, every part ends before the next begins, the option rows stay
    // pointer-sized, and the whole stack ends before the action row. This is
    // the dialog whose failure reason used to share its slot with the flow
    // notice and end a pixel above the buttons.
    {
        for (w, h, scale) in [
            (1280.0, 800.0, 1.0),
            (800.0, 900.0, 2.0),
            (480.0, 700.0, 1.0),
        ] {
            let (mut tr, theme) = scaled(scale);
            let mut app = app_sized(w, h);
            app.presentation.export_open = true;
            app.export_status = ExportStatus::Failed {
                reason: "The selected encoder rejected the profile because the source contains \
                     an unsupported colour space; choose a compatible profile or install an \
                     encoder that supports this footage."
                    .into(),
            };
            app.export_notice = Some("Choosing where to save…".into());
            let l = app.export_layout(&mut tr, &theme);
            let what = format!("{w}x{h} @{scale}x");
            let ty = l.type_sizes;
            let (line_sm, line_xs) = (line(&mut tr, ty.sm), line(&mut tr, ty.xs));
            let body = l.modal.body;
            assert!(!l.modal.body_overflows, "{what}: the card holds its body");

            // Header.
            let mut y = l.filename_top;
            assert!(y >= body.y0, "{what}: the filename starts in the body");
            y += line_sm;
            if let Some(mix) = l.mix_top {
                assert!(mix >= y, "{what}: the mix line starts under the filename");
                y = mix + line_xs;
            }
            // Three captioned groups, each caption clear of its segments and each
            // group clear of the next.
            let groups = [
                l.resolution.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
                l.container.iter().map(|(_, r)| *r).collect(),
                l.fps.iter().map(|(_, r)| *r).collect(),
            ];
            let min_target = f64::from(theme.tokens.metrics.min_pointer_target);
            for (caption, rects) in l.captions.iter().zip(&groups) {
                assert!(*caption >= y, "{what}: caption {caption} starts under {y}");
                let first = rects.first().expect("a group offers options");
                assert!(
                    caption + line_xs <= first.y0,
                    "{what}: caption runs into its segments"
                );
                for rect in rects {
                    assert!(
                        rect.height() >= min_target - 1e-6,
                        "{what}: segment {rect:?} is under the pointer minimum"
                    );
                    assert!(
                        rect.height() >= line_sm,
                        "{what}: segment {rect:?} is shorter than its label"
                    );
                    assert!(
                        rect.x0 >= body.x0 - 1e-6 && rect.x1 <= body.x1 + 1e-6,
                        "{what}: segment {rect:?} leaves the body"
                    );
                }
                y = first.y1;
            }
            // Footer: size line, status slot, notice, then the actions.
            assert!(
                l.info_top >= y,
                "{what}: the size line starts under the last group"
            );
            assert!(
                l.info_top + line_sm <= l.status.y0,
                "{what}: the size line runs into the status slot"
            );
            assert!(
                l.status.height() >= 2.0 * line_xs - 1e-6,
                "{what}: two failure lines fit the status slot"
            );
            let notice = l.notice_top;
            assert!(
                notice >= l.status.y1,
                "{what}: the notice sits under the status, not in it"
            );
            assert!(
                notice + line_xs <= body.y1 + 1e-6,
                "{what}: the notice ends inside the body"
            );
            assert!(
                body.y1 <= l.cancel.y0 && body.y1 <= l.confirm.y0,
                "{what}: the body ends before the actions"
            );
            assert!(
                l.cancel.y1 <= l.modal.card.y1 && l.confirm.y1 <= l.modal.card.y1,
                "{what}: the actions sit inside the card"
            );
        }
    }
    // Case `the_discard_question_keeps_its_verbs_inside_the_card_on_a_phone_and_at_large_text`:
    // The unsaved-work question's three verbs stay inside its card — beside
    // one another when they fit, stacked when a phone-width card or large text
    // makes the row wider than the card — and never over its body lines.
    {
        for (w, h, scale) in [
            (1280.0, 800.0, 1.0),
            (390.0, 844.0, 1.0),
            (800.0, 900.0, 2.0),
            (480.0, 900.0, 2.0),
        ] {
            let (mut tr, theme) = scaled(scale);
            let mut app = app_sized(w, h);
            app.pending_open = Some(PendingOpen {
                path: PathBuf::from("/home/you/Videos/Reel_2026.qvp"),
                losing: "Coastline_Doc_v3".into(),
            });
            let l = app
                .discard_layout(&mut tr, &theme)
                .expect("a question is pending");
            let what = format!("{w}x{h} @{scale}x");
            let card = l.modal.card;
            let body = l.modal.body;
            let line_sm = line(&mut tr, l.type_sizes.sm);
            assert!(
                card.x0 >= app.rect.x0 && card.x1 <= app.rect.x1,
                "{what}: the card fits the window"
            );
            assert!(
                l.lines[1] >= l.lines[0] + line_sm,
                "{what}: the two body lines do not overlap"
            );
            assert!(
                l.lines[1] + line_sm <= body.y1 + 1e-6,
                "{what}: the body lines end inside the body"
            );
            let verbs = [
                ("Cancel", l.cancel),
                ("Open anyway", l.discard),
                ("Save…", l.save),
            ];
            for (name, rect) in verbs {
                assert!(rect.area() > 0.0, "{what}: {name} has a rect");
                assert!(
                    rect.x0 >= card.x0 - 1e-6 && rect.x1 <= card.x1 + 1e-6,
                    "{what}: {name} {rect:?} leaves the card {card:?}"
                );
                assert!(
                    rect.y0 >= body.y1 - 1e-6,
                    "{what}: {name} sits over the body lines"
                );
                assert!(
                    rect.y1 <= card.y1 + 1e-6,
                    "{what}: {name} leaves the card's foot"
                );
            }
            for (a, b) in [(0, 1), (1, 2), (0, 2)] {
                let (na, ra) = verbs[a];
                let (nb, rb) = verbs[b];
                assert!(
                    ra.intersect(rb).is_zero_area(),
                    "{what}: {na} and {nb} overlap"
                );
            }
        }
    }
    // Case `the_shortcut_sheet_rows_grow_with_the_text`:
    // The shortcut sheet's rows are measured from their lines, so they grow
    // with the text instead of drawing through each other at 2×.
    {
        let (mut tr1, theme1) = scaled(1.0);
        let (mut tr2, theme2) = scaled(2.0);
        let ty1 = dialog_type(&tr1, &theme1);
        let ty2 = dialog_type(&tr2, &theme2);
        let m1 = HelpMetrics::measure(&mut tr1, &theme1, ty1);
        let m2 = HelpMetrics::measure(&mut tr2, &theme2, ty2);
        assert!(
            m2.row_h > m1.row_h * 1.5,
            "rows at 2x ({}) barely grew from 1x ({})",
            m2.row_h,
            m1.row_h
        );
        assert!(
            m2.header_h > m1.header_h * 1.5,
            "headings at 2x ({}) barely grew from 1x ({})",
            m2.header_h,
            m1.header_h
        );
        assert_close(
            m1.row_h,
            line(&mut tr1, ty1.sm) + f64::from(theme1.tokens.spacing.s1),
            "a 1x row is its line plus one step",
        );

        // And the sheet lays every row out with them, at either scale.
        for scale in [1.0, 2.0] {
            let (mut tr, theme) = scaled(scale);
            let app = app_sized(1280.0, 800.0);
            let l = app.help_layout(&mut tr, &theme);
            let m = HelpMetrics::measure(&mut tr, &theme, l.type_sizes);
            for g in &l.groups {
                assert_close(g.heading.height(), m.header_h, "heading band");
                for row in &g.rows {
                    assert_close(row.height(), m.row_h, "row band");
                }
            }
        }
    }
    // Case `the_export_footer_stays_on_the_card_when_the_window_cannot_hold_the_dialog`:
    // A window too short for the export dialog's natural stack gets a card the
    // frame clamps to the window; the footer — size line, status slot, notice —
    // stays on the card, anchored to the body's foot, and the option groups
    // give up their gaps and height above it. The first version of this dialog
    // clipped the footer away instead, so a failure reason disappeared exactly
    // when the window was small.
    {
        for (w, h, scale) in [(1280.0, 720.0, 2.0), (1280.0, 440.0, 1.0)] {
            let (mut tr, theme) = scaled(scale);
            let mut app = app_sized(w, h);
            app.presentation.export_open = true;
            app.export_status = ExportStatus::Failed {
                reason: "The selected encoder rejected the profile because the source contains \
                     an unsupported colour space; choose a compatible profile or install an \
                     encoder that supports this footage."
                    .into(),
            };
            app.export_notice = Some("Choosing where to save…".into());
            let l = app.export_layout(&mut tr, &theme);
            let what = format!("{w}x{h} @{scale}x");
            let ty = l.type_sizes;
            let (line_sm, line_xs) = (line(&mut tr, ty.sm), line(&mut tr, ty.xs));
            let body = l.modal.body;
            let card = l.modal.card;
            assert!(
                l.modal.body_overflows,
                "{what}: the fixture is a window that cannot hold the natural stack"
            );
            // The footer is whole and on the card.
            assert!(
                l.info_top + line_sm <= l.status.y0 + 1e-6,
                "{what}: the size line runs into the status slot"
            );
            assert!(
                l.status.height() >= 2.0 * line_xs - 1e-6,
                "{what}: two failure lines fit the status slot"
            );
            let notice = l.notice_top;
            assert!(
                notice >= l.status.y1,
                "{what}: the notice sits under the status"
            );
            assert!(
                notice + line_xs <= body.y1 + 1e-6,
                "{what}: the notice ends inside the body ({} > {})",
                notice + line_xs,
                body.y1
            );
            assert!(
                body.y1 <= l.cancel.y0 && l.cancel.y1 <= card.y1 && l.confirm.y1 <= card.y1,
                "{what}: the actions sit on the card below the body"
            );
            // Nothing above it runs into it, and nothing overlaps.
            let last = l.fps.last().expect("frame rates are offered").1;
            assert!(
                last.y1 <= l.info_top + 1e-6,
                "{what}: the last option row ({}) runs into the size line ({})",
                last.y1,
                l.info_top
            );
            let mut y = l.filename_top + line_sm;
            if let Some(mix) = l.mix_top {
                assert!(mix >= y, "{what}: the mix line starts under the filename");
                y = mix + line_xs;
            }
            for (caption, rects) in l.captions.iter().zip([
                l.resolution.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
                l.container.iter().map(|(_, r)| *r).collect(),
                l.fps.iter().map(|(_, r)| *r).collect(),
            ]) {
                assert!(
                    *caption >= y - 1e-6,
                    "{what}: caption {caption} starts under {y}"
                );
                let first = rects.first().expect("a group offers options");
                assert!(
                    caption + line_xs <= first.y0 + 1e-6,
                    "{what}: caption runs into its segments"
                );
                assert!(
                    first.height() >= line_sm + f64::from(theme.tokens.spacing.s1) - 1e-6,
                    "{what}: a segment {first:?} is shorter than its label"
                );
                y = first.y1;
            }
        }
    }
    // Case `the_shortcut_sheet_falls_back_to_one_column_and_scrolls_at_large_text`:
    // On a window whose text-scaled width has no room for two columns the
    // sheet lays one out — so every description keeps the width it needs to
    // be read — and, when the table is taller than the body, the body scrolls
    // rather than dropping rows.
    {
        let (mut tr, theme) = scaled(2.0);
        let mut app = app_sized(800.0, 900.0);
        app.navigation.help_open = true;
        let l = app.help_layout(&mut tr, &theme);
        let ty = l.type_sizes;
        let columns: std::collections::BTreeSet<u64> =
            l.groups.iter().map(|g| g.heading.x0.to_bits()).collect();
        assert_eq!(
            columns.len(),
            1,
            "an 800px window at 2x lays out one column"
        );
        let widest_description = SHORTCUTS
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .map(|(_, what)| f64::from(tr.measure(what, ty.sm, None).0))
            .fold(0.0, f64::max);
        let row = l.groups[0].rows[0];
        assert!(
            row.width() - l.what_dx >= widest_description,
            "the description column ({}) is narrower than the widest description ({widest_description})",
            row.width() - l.what_dx
        );
        assert!(
            l.modal.body_overflows,
            "the table is taller than the window"
        );
        assert!(l.max_scroll > 0.0, "so the body scrolls");
        let hint = l.footer_hint.expect("a scrolling body says so at its foot");
        assert!(hint < l.modal.body.y1, "the hint sits inside the body");
        // Scrolled to the end, the last row is inside the body, above the hint.
        app.help_scroll = l.max_scroll;
        let scrolled = app.help_layout(&mut tr, &theme);
        let last = scrolled
            .groups
            .last()
            .and_then(|g| g.rows.last())
            .copied()
            .expect("the table has rows");
        assert!(
            last.y1 <= hint + 1e-6,
            "the last row ({}) is reachable above the hint ({hint})",
            last.y1
        );
        assert!(
            scrolled.hint_top < scrolled.modal.body.y0,
            "the sheet's own hint line scrolled away with the rows"
        );
    }
}
