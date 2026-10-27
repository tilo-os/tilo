//! Production geometry regressions for retained editor chrome at large text.
//! The shared controls own their measurements; these checks inspect the
//! rectangles actually painted by the editor rather than legacy pixel floors.

use super::tests::{mount, mount_themed};
use super::*;
use ui::headless::Framed;
use ui::widget::{ControlSize, Edits};

fn overlaps(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.01 && i.height() > 0.01
}

fn label_height(theme: &Theme) -> f64 {
    f64::from(ControlSize::Small.label_style(theme).line_height_px())
}

fn assert_panel_controls_fit(wp: &WordProcessorWidget, theme: &Theme) {
    let hits = &wp.panel_hits;
    assert!(hits.len() > 10, "the inspector publishes its controls");
    for (index, (rect, hit)) in hits.iter().enumerate() {
        assert!(
            rect.x0 >= wp.panel_rect.x0 - 0.01
                && rect.x1 <= wp.panel_rect.x1 + 0.01
                && rect.y0 >= wp.panel_rect.y0 - 0.01
                && rect.y1 <= wp.panel_rect.y1 + 0.01,
            "control #{index} {rect:?} leaves the visible panel {:?}",
            wp.panel_rect
        );
        assert!(
            rect.height() >= label_height(theme) - 0.01 || matches!(hit, PanelHit::SetColor(_)),
            "control #{index} clips its label"
        );
    }
    for (i, (a, _)) in hits.iter().enumerate() {
        for (j, (b, _)) in hits.iter().enumerate().skip(i + 1) {
            assert!(
                !overlaps(*a, *b),
                "inspector controls #{i} and #{j} overlap"
            );
        }
    }
}

#[test]
fn paragraph_controls_follow_their_measured_labels() {
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut state = crate::initialized_state();
        let mut wp = WordProcessorWidget::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut Edits::lend(&mut state.doc), "Panel rows");
        wp.set_styles_panel_open(true);
        let mut framed = mount_themed(state, wp, 1400, 1200, &theme);
        let (wp, _) = Framed::split(&mut framed);
        assert_panel_controls_fit(wp, &theme);
        let swatch_x0 = wp
            .panel_hits
            .iter()
            .filter(|(_, hit)| matches!(hit, PanelHit::SetColor(_)))
            .map(|(rect, _)| rect.x0)
            .fold(f64::INFINITY, f64::min);
        let chip = wp
            .panel_hits
            .iter()
            .find(|(rect, hit)| {
                matches!(hit, PanelHit::OpenColorPicker)
                    && (rect.width() - rect.height()).abs() < 0.01
            })
            .expect("the custom colour chip is visible")
            .0;
        assert!(
            chip.x0 > swatch_x0,
            "the custom colour chip follows the palette"
        );
        for (rect, hit) in &wp.panel_hits {
            if matches!(hit, PanelHit::SetColor(_)) {
                assert!((rect.center().y - chip.center().y).abs() < 0.01);
                assert!(rect.x1 <= chip.x0, "palette and custom chip never overlap");
            }
        }
        assert!(chip.height() >= label_height(&theme));
    }
}

#[test]
fn layout_controls_center_on_the_theme_sized_bar() {
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut state = crate::initialized_state();
        let mut wp = WordProcessorWidget::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut Edits::lend(&mut state.doc), "chips");
        let mut framed = mount_themed(state, wp, 1000, 700, &theme);
        let (wp, _) = Framed::split(&mut framed);
        let bar_top = wp.rect.y1 - wp.layout_bar_height;
        let bar_mid = f64::midpoint(bar_top, wp.rect.y1);
        assert_eq!(wp.layout_chips.len(), 2, "both layout choices are placed");
        assert!(wp.layout_bar_height >= ControlSize::Small.height(&theme));
        for (_, rect) in &wp.layout_chips {
            assert!(
                rect.height() >= label_height(&theme),
                "the layout label fits"
            );
            assert!(rect.y0 >= bar_top - 0.01 && rect.y1 <= wp.rect.y1 + 0.01);
            assert!(
                (rect.center().y - bar_mid).abs() < 0.01,
                "layout controls share the centre line"
            );
        }
        assert!(!overlaps(wp.layout_chips[0].1, wp.layout_chips[1].1));
    }
}

#[test]
fn image_inspector_controls_never_overlap_at_large_text() {
    let theme = Theme::dark().with_text_scale(2.0);
    let mut state = crate::initialized_state();
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_image_bytes(&mut Edits::lend(&mut state.doc), super::tests::TEST_PNG);
    wp.refresh_blocks(&state.doc);
    let image = wp
        .blocks
        .iter()
        .find(|block| block.kind == "image")
        .expect("canonical image insertion")
        .id
        .clone();
    wp.model_mut().set_caret(crate::word_processor::Caret {
        block: image,
        offset: 0,
    });

    wp.set_styles_panel_open(true);
    let mut framed = mount_themed(state, wp, 1400, 1200, &theme);
    let (wp, _) = Framed::split(&mut framed);
    assert!(wp.selected_image_id().is_some(), "the image is selected");
    assert!(
        wp.panel_hits
            .iter()
            .any(|(_, hit)| matches!(hit, PanelHit::ImageFlipH)),
        "the image inspector is showing"
    );
    assert_panel_controls_fit(wp, &theme);
}

#[test]
fn the_commands_are_the_window_bars_and_the_editor_draws_no_toolbar_row() {
    for scale in [1.0, 2.0] {
        for width in [520, 800, 1800] {
            let theme = Theme::dark().with_text_scale(scale);
            let state = crate::initialized_state();
            let mut wp = WordProcessorWidget::detached();
            wp.sync(&state.doc, &state.root);
            wp.set_styles_panel_open(true);
            let mut framed = mount_themed(state, wp, width, 900, &theme);
            let (wp, _) = Framed::split(&mut framed);
            let bar = wp.window_bar();
            let ids: Vec<&str> = bar.actions.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(
                ids,
                [
                    "format",
                    "insert",
                    "toggle-styles",
                    "file-menu",
                    "toggle-ruler"
                ],
                "{width}px: the commands are the window bar's slots"
            );
            assert_eq!(bar.sidebar, Some(false), "the outline is the bar's sidebar");
            // The page's column and the inspector start at the window's top:
            // nothing the editor paints sits above them.
            assert!(
                // Docked it starts at the top; floating, a drawer's inset from it.
                wp.inspector_drawer.sidebar_rect().y0 - wp.rect.y0 < 16.0
                    || wp.styles_panel_full_window(),
                "{width}px@{scale}: the inspector starts at the top, {:?}",
                wp.inspector_drawer.sidebar_rect()
            );
            assert!(
                (wp.zoom_viewport.y0 - wp.rect.y0 - wp.ruler.rect().height()).abs() < 0.01,
                "{width}px@{scale}: the page starts under the ruler alone"
            );
        }
    }
}

#[test]
fn frame_reserves_large_text_chrome_without_covering_the_page() {
    let theme = Theme::dark().with_text_scale(2.0);
    let mut state = crate::initialized_state();
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut Edits::lend(&mut state.doc), "Large text");
    wp.set_styles_panel_open(true);
    let mut framed = mount_themed(state, wp, 800, 900, &theme);
    let (wp, _) = Framed::split(&mut framed);
    assert!(wp.panel_rect.y0 >= wp.rect.y0 - 0.01);
    assert!(wp.panel_rect.y1 <= wp.rect.y1 - wp.layout_bar_height + 0.01);
    let state = crate::initialized_state();
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    let mut framed = mount(state, wp, 800, 900);
    let (wp, _) = Framed::split(&mut framed);
    assert!(
        (wp.layout_bar_height - WordProcessorWidget::status_band_height(&Theme::dark())).abs()
            < 0.01
    );
}
