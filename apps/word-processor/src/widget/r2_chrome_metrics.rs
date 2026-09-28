//! Round-two chrome-metric regressions for the word processor.
//!
//! The styles panel, the layout bar and the find bar used to lay their rows
//! out at literal offsets (30 px rows, 24 px chips, 26 px fields, 7 px per
//! glyph) while the text inside followed the accessibility scale, so at 2×
//! captions painted under the control below them, chips clipped their
//! labels and the find bar's controls ran into each other. Their geometry
//! now comes from measured line boxes plus theme tokens with the classic 1×
//! values as floors, and the toolbar packer states the toolkit's row model
//! once ([`ToolbarModel`]).

use super::tests::{mount, mount_themed};
use super::*;
use ui::headless::Framed;

fn overlaps(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.01 && i.height() > 0.01
}

fn renderer(scale: f32) -> TextRenderer {
    let mut tr = TextRenderer::new();
    tr.set_text_scale(scale);
    tr
}

/// Styles panel and find bar controls follow measured text at every scale, and layout bar chips centre
/// on measured labels.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn panel_and_bar_controls_follow_their_measured_labels() {
    // Phase: styles_panel_controls_follow_measured_text_at_every_scale.
    // Every styles-panel control fits its measured label, no two controls
    // overlap, all of them stay inside the panel, and the custom-colour chip
    // starts on the swatch column (review item W6) — at 1× and 2×.
    {
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut state = ui::State::new();
            let mut wp = WordProcessorWidget::new();
            wp.sync(&mut state.doc, &state.root);
            wp.model_mut().insert_str(&mut state.doc, "Panel rows");
            wp.set_styles_panel_open(true);
            let mut framed = mount_themed(state, wp, 1400, 900, &theme);
            let (wp, _) = Framed::split(&mut framed);

            let pm = wp.panel_metrics;
            assert!(
                (pm.scale - f64::from(scale)).abs() < 0.01,
                "metrics measured at {scale}x"
            );
            assert!(
                pm.control_h() >= pm.lh_label + pm.tk.s2 - 0.01,
                "control fits its label box"
            );
            let panel = wp.panel_rect;
            assert!(
                panel.width() >= STYLES_PANEL_W * f64::from(scale) - 0.01,
                "the panel column grows with the text: {:.0} at {scale}x",
                panel.width()
            );
            let hits = &wp.panel_hits;
            assert!(
                hits.len() > 10,
                "the paragraph editor registered its controls"
            );
            for (index, (r, _)) in hits.iter().enumerate() {
                assert!(
                    r.x0 >= panel.x0 - 0.01 && r.x1 <= panel.x1 + 0.01 && r.y0 >= panel.y0 - 0.01,
                    "control #{index} {r:?} leaves the panel {panel:?} at {scale}x"
                );
                assert!(
                    r.height() >= pm.lh_label - 0.01
                        || matches!(hits[index].1, PanelHit::SetColor(_)),
                    "control #{index} {r:?} is shorter than its {:.1} px label at {scale}x",
                    pm.lh_label
                );
            }
            for (i, (a, _)) in hits.iter().enumerate() {
                for (j, (b, _)) in hits.iter().enumerate().skip(i + 1) {
                    assert!(
                        !overlaps(*a, *b),
                        "controls #{i} {a:?} and #{j} {b:?} overlap at {scale}x"
                    );
                }
            }
            // W6: the square custom-colour chip sits where the swatch strip starts.
            let swatch_x0 = hits
                .iter()
                .filter(|(_, h)| matches!(h, PanelHit::SetColor(_)))
                .map(|(r, _)| r.x0)
                .fold(f64::INFINITY, f64::min);
            let chip = hits
                .iter()
                .filter(|(r, h)| {
                    matches!(h, PanelHit::OpenColorPicker) && (r.width() - r.height()).abs() < 0.01
                })
                .map(|(r, _)| *r)
                .next()
                .expect("custom colour chip");
            assert!(
                (chip.x0 - swatch_x0).abs() < 0.01,
                "chip {chip:?} starts at the swatch column {swatch_x0:.1} at {scale}x"
            );
            if (scale - 1.0).abs() < f32::EPSILON {
                assert!(
                    (pm.control_h() - PANEL_CONTROL_H).abs() < 0.01,
                    "1x control {}",
                    pm.control_h()
                );
                // A 24 px control plus the `s2` sibling seam (the 1× polish pass
                // moved the seam from an off-scale 6 px onto the token grid).
                assert!(
                    (pm.row_adv() - (PANEL_CONTROL_H + PANEL_ROW_SEAM)).abs() < 0.01,
                    "1x row advance {}",
                    pm.row_adv()
                );
                assert!(
                    (pm.row_adv() - 32.0).abs() < 0.01,
                    "1x row advance is 24 + s2"
                );
                assert!(
                    (panel.width() - STYLES_PANEL_W).abs() < 0.01,
                    "1x panel width"
                );
            }
        }
    }

    // Phase: find_bar_controls_fit_their_measured_labels_at_every_scale.
    // The find bar's fields, toggles, readout and actions are laid out from
    // measured labels: none overlap, all sit inside the bar, each is at least
    // its text tall, and the row ends inside the window — at 1× and 2×.
    {
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut state = ui::State::new();
            let mut wp = WordProcessorWidget::new();
            wp.sync(&mut state.doc, &state.root);
            wp.model_mut()
                .insert_str(&mut state.doc, "A report about the report");
            wp.stage_find_for_shot(&state.doc, "report");
            let mut framed = mount_themed(state, wp, 1200, 800, &theme);
            let (wp, _) = Framed::split(&mut framed);
            let rects = wp.find.button_rects();
            assert_eq!(rects.len(), 10, "find, 3 toggles, replace, 5 actions");
            let bar_h = wp.find.height();
            let bar_top = wp.rect.y0 + wp.toolbar_height(wp.rect.width()) + wp.recovery.height();
            let mut tr = renderer(scale);
            let lh_action = f64::from(tr.measure("Ag", 13.0, None).1);
            assert!(
                bar_h >= lh_action + 3.0 * wp.tokens.s2 - 0.01,
                "bar {bar_h} holds a field plus s2 each side"
            );
            // At 2× one row of controls is wider than a 1200 px window, so the
            // bar wraps: two field-height rows, and the actions sit on the second.
            let field_h = rects[0].height();
            let rows = if bar_h >= 2.0 * field_h + 3.0 * wp.tokens.s2 - 0.01 {
                2
            } else {
                1
            };
            let expected_rows = if scale > 1.5 { 2 } else { 1 };
            assert_eq!(rows, expected_rows, "bar {bar_h} at {scale}x");
            if rows == 2 {
                assert!(
                    rects[9].y0 > rects[0].y1 + 0.01,
                    "the close button {:?} sits below the find field {:?}",
                    rects[9],
                    rects[0]
                );
                assert!(
                    rects[4].x0 <= rects[0].x0 + 0.01,
                    "the replace field starts the second row"
                );
            }
            for r in &rects {
                assert!(
                    r.y0 >= bar_top - 0.01 && r.y1 <= bar_top + bar_h + 0.01,
                    "{r:?} leaves the find band {bar_top}..{} at {scale}x",
                    bar_top + bar_h
                );
                assert!(
                    r.height() >= lh_action - 0.01,
                    "{r:?} shorter than its text at {scale}x"
                );
                assert!(
                    r.x1 <= wp.rect.x1 + 0.01,
                    "{r:?} runs off the window at {scale}x"
                );
            }
            for (i, a) in rects.iter().enumerate() {
                for (j, b) in rects.iter().enumerate().skip(i + 1) {
                    assert!(
                        !overlaps(*a, *b),
                        "find controls #{i} {a:?} and #{j} {b:?} overlap at {scale}x"
                    );
                }
            }
            // Labels fit their buttons: the widest action ("Ersetzen"/"Replace")
            // is at least its measured text plus s2 each side.
            let replace = rects[7];
            let (w, _) = tr.measure(&crate::i18n::t("find-replace"), 13.0, None);
            assert!(replace.width() >= f64::from(w) + 2.0 * wp.tokens.s2 - 0.01);
            if (scale - 1.0).abs() < f32::EPSILON {
                assert!(
                    (bar_h - crate::find_bar::HEIGHT).abs() < 0.01,
                    "1x bar is 42"
                );
                assert!((rects[0].height() - 26.0).abs() < 0.01, "1x field is 26");
            }
        }
    }

    // Phase: layout_bar_chips_center_on_measured_labels.
    // Layout-bar chips are their measured label plus `s1`, centred on the bar,
    // and the bar is the chip plus `s1` above and below (classic 32 / 22 at 1×).
    {
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut state = ui::State::new();
            let mut wp = WordProcessorWidget::new();
            wp.sync(&mut state.doc, &state.root);
            wp.model_mut().insert_str(&mut state.doc, "chips");
            let mut framed = mount_themed(state, wp, 1000, 700, &theme);
            let (wp, _) = Framed::split(&mut framed);
            let mut tr = renderer(scale);
            let lh = f64::from(tr.measure("Mg", LAYOUT_BAR_FONT, None).1);
            let tk = wp.tokens;
            let chip_h = layout_chip_h(lh, tk);
            assert!(
                (wp.layout_bar_height - LAYOUT_BAR_H.max(chip_h + 2.0 * tk.s1)).abs() < 0.01,
                "bar height {} follows its chip {chip_h} at {scale}x",
                wp.layout_bar_height
            );
            let bar_top = wp.rect.y1 - wp.layout_bar_height;
            let bar_mid = f64::midpoint(bar_top, wp.rect.y1);
            assert_eq!(wp.layout_chips.len(), 2, "both layout chips are placed");
            for (_, r) in &wp.layout_chips {
                assert!(
                    r.height() >= lh + tk.s1 - 0.01,
                    "chip {r:?} fits its label at {scale}x"
                );
                assert!(
                    r.y0 >= bar_top - 0.01 && r.y1 <= wp.rect.y1 + 0.01,
                    "chip inside the bar"
                );
                assert!(
                    (r.center().y - bar_mid).abs() < 0.01,
                    "chip on the bar's centre line"
                );
            }
            if (scale - 1.0).abs() < f32::EPSILON {
                assert!(
                    (wp.layout_bar_height - LAYOUT_BAR_H).abs() < 0.01,
                    "1x bar is 32"
                );
                assert!((chip_h - LAYOUT_CHIP_H).abs() < 0.01, "1x chip is 22");
            }
        }
    }
}

/// Image inspector controls never overlap at 2x, the toolbar packer mirrors the toolkit row model, and
/// the 2x frame reserves its measured chrome.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn two_x_chrome_is_reserved_packed_and_never_overlapping() {
    // Phase: image_inspector_controls_never_overlap_at_two_x.
    // The image inspector's rows keep the same discipline once an image is the
    // selected object.
    {
        const PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3,
            8, 2, 0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73,
            69, 78, 68, 174, 66, 96, 130,
        ];
        let theme = Theme::dark().with_text_scale(2.0);
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Picture");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_image_bytes(&mut state.doc, PNG);
        wp.sync(&mut state.doc, &state.root);
        let image = tilo_doc::model::open(&state.doc, &state.root)
            .ok()
            .and_then(|c| {
                tilo_doc::model::materialize(&state.doc, &c)
                    .blocks
                    .into_iter()
                    .find(|b| b.kind == "image")
            })
            .expect("an image block");
        wp.model_mut().set_caret(crate::word_processor::Caret {
            block: image.id,
            offset: 0,
        });
        wp.set_styles_panel_open(true);
        let mut framed = mount_themed(state, wp, 1400, 900, &theme);
        let (wp, _) = Framed::split(&mut framed);
        assert!(
            wp.selected_image_id().is_some(),
            "the image is the selected object"
        );
        let hits = &wp.panel_hits;
        assert!(
            hits.iter().any(|(_, h)| matches!(h, PanelHit::ImageFlipH)),
            "the image inspector is showing"
        );
        let pm = wp.panel_metrics;
        for (index, (r, _)) in hits.iter().enumerate() {
            assert!(
                r.height() >= pm.lh_label - 0.01,
                "image control #{index} {r:?} is shorter than its label"
            );
            assert!(r.x1 <= wp.panel_rect.x1 + 0.01 && r.x0 >= wp.panel_rect.x0 - 0.01);
        }
        for (i, (a, _)) in hits.iter().enumerate() {
            for (j, (b, _)) in hits.iter().enumerate().skip(i + 1) {
                assert!(
                    !overlaps(*a, *b),
                    "image controls #{i} {a:?} and #{j} {b:?} overlap at 2x"
                );
            }
        }
    }

    // Phase: toolbar_packer_mirrors_the_toolkit_row_model.
    // The app-side [`ToolbarModel`] states the toolkit's row model: a row the
    // packer admits is one `ui::chrome::toolbar::draw` lays out inside the same
    // width, starting at the toolkit's left pad, and the row height leaves room
    // for the toolkit's `label + 12` item.
    {
        let widths = [80.0, 60.0, 100.0, 70.0, 90.0, 40.0];
        let starts = [true, false, true, false, true, false];
        let avail = 300.0;
        let rows = pack_toolbar_rows(&widths, &starts, avail, 1.0);
        assert!(rows.len() >= 2, "the fixture wraps: {rows:?}");
        let theme = Theme::dark();
        let mut tr = renderer(1.0);
        let mut scene = Scene::new();
        for (s, e) in rows {
            let items: Vec<ToolbarItem> = (s..e)
                .map(|i| ToolbarItem::new("x", widths[i]).group_start(starts[i]))
                .collect();
            let rects = ui::chrome::toolbar::draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                Rect::new(0.0, 0.0, avail, TOOLBAR_H),
                &items,
            );
            let first = rects.first().expect("a row has an item");
            let lead = TOOLBAR_MODEL.pad + if starts[s] { TOOLBAR_MODEL.sep } else { 0.0 };
            assert!(
                (first.x0 - lead).abs() < 0.01,
                "toolkit lead-in {} vs model {lead}",
                first.x0
            );
            let last = rects.last().expect("a row has an item");
            assert!(
                last.x1 <= avail - TOOLBAR_MODEL.right_pad + TOOLBAR_MODEL.gap + 0.01,
                "toolkit drew {:.1} past the packer's usable width",
                last.x1
            );
            for pair in rects.windows(2) {
                assert!(
                    (pair[1].x0 - pair[0].x1 - TOOLBAR_MODEL.gap).abs() < TOOLBAR_MODEL.sep + 0.01,
                    "gap between items is the model's gap (plus a separator)"
                );
            }
        }
        let (_, lh) = tr.measure_weighted(
            "Ag",
            TOOLBAR_MODEL.label_font,
            None,
            ui::text_render::TextWeight::Regular,
        );
        let lh = f64::from(lh);
        assert!(
            (TOOLBAR_MODEL.row_h(lh, 18.0) - TOOLBAR_H).abs() < 0.01,
            "1x row is the classic 50"
        );
        let mut tr2 = renderer(2.0);
        let (_, lh2) = tr2.measure_weighted(
            "Ag",
            TOOLBAR_MODEL.label_font,
            None,
            ui::text_render::TextWeight::Regular,
        );
        let lh2 = f64::from(lh2);
        assert!(
            TOOLBAR_MODEL.row_h(lh2, 36.0) >= lh2 + TOOLBAR_MODEL.item_pad + 2.0,
            "2x row keeps the toolkit's item (label + 12) inside with room"
        );
    }

    // Phase: two_x_frame_reserves_its_measured_chrome.
    // The whole frame at 2× and 800×900: the toolbar rows, the layout bar and
    // the styles panel share the same measured metrics on the production draw
    // path (`--shots` geometry), and the page never starts under the toolbar.
    {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Large text");
        wp.set_styles_panel_open(true);
        let mut framed = mount_themed(state, wp, 800, 900, &theme);
        let (wp, _) = Framed::split(&mut framed);
        let tb_h = wp.toolbar_height(wp.rect.width());
        for (_, r) in &wp.toolbar {
            assert!(
                r.y1 <= wp.rect.y0 + tb_h + 0.01,
                "toolbar target {r:?} below the reserved band"
            );
        }
        assert!(
            wp.panel_rect.y0 >= wp.rect.y0 + tb_h - 0.01,
            "panel starts under the toolbar"
        );
        assert!(
            wp.panel_rect.y1 <= wp.rect.y1 - wp.layout_bar_height + 0.01,
            "panel ends above the layout bar"
        );
        // And the same document at 1× draws the classic density.
        let mut wp1 = WordProcessorWidget::new();
        let mut state1 = ui::State::new();
        wp1.sync(&mut state1.doc, &state1.root);
        let mut framed_wp1 = mount(state1, wp1, 800, 900);
        let (wp1, _) = Framed::split(&mut framed_wp1);
        assert!((wp1.toolbar_row_height - TOOLBAR_H).abs() < 0.01);
        assert!((wp1.layout_bar_height - LAYOUT_BAR_H).abs() < 0.01);
    }
}
