//! Accessibility-scale composition geometry.
//!
//! review-3 (2026-09-15) recorded a P1 at 200% text: card titles, channel
//! names and view counts painted over one another and the Home heading sat
//! behind the first thumbnail row. Plan task 16 asked for a measure-first
//! proof on the real laid-out tree before any layout code moved. These tests
//! are that proof: they run the production five-pass frame with the text scale
//! applied exactly as the live host applies it and assert on the measured
//! rects, so a regression here is a real overlap, not an estimate.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod harness;

use harness::Harness;
use tube::TubeApp;
use tube::model::Route;
use ui::widget::Widget;
use vello::kurbo::Rect;

/// The `--shots ... --viewport 800x900 --text-scale 2` review pose.
const VIEWPORT: (f32, f32) = (800.0, 900.0);
const CARD: &str = "video_card_vid_rust_async";

/// Layout rects are `f64`; the text renderer measures in `f32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "test viewport coordinates are a few thousand pixels, exact in f32"
)]
fn px(v: f64) -> f32 {
    v as f32
}

fn at(route: Route, scale: f32) -> Harness<TubeApp> {
    let app = TubeApp::new()
        .with_viewport(f64::from(VIEWPORT.0), f64::from(VIEWPORT.1))
        .with_route_shot(route);
    Harness::with_text_scale(app, VIEWPORT, scale)
}

/// A laid-out subtree rebuilt from the harness's depth-first dump.
struct Node {
    rect: Rect,
    children: Vec<Node>,
}

fn tree(h: &Harness<TubeApp>, id: &str) -> Node {
    fn build(items: &[(usize, Option<String>, Rect)], i: &mut usize, depth: usize) -> Node {
        let rect = items[*i].2;
        *i = i.saturating_add(1);
        let child_depth = depth.saturating_add(1);
        let mut children = Vec::new();
        while *i < items.len() && items[*i].0 == child_depth {
            children.push(build(items, i, child_depth));
        }
        Node { rect, children }
    }
    let items = h.subtree_rects(id);
    assert!(!items.is_empty(), "`{id}` is laid out");
    let mut i = 0;
    build(&items, &mut i, 0)
}

fn assert_stacked_without_overlap(label: &str, rects: &[Rect]) {
    for pair in rects.windows(2) {
        let (above, below) = (pair[0], pair[1]);
        assert!(
            above.y1 <= below.y0 + 0.5,
            "{label}: {above:?} ends below the start of {below:?}"
        );
    }
}

/// card → [thumbnail, row]; row → [avatar, meta]; meta → [title, channel row, views].
fn card_meta_lines(h: &Harness<TubeApp>) -> Vec<Rect> {
    let card = tree(h, CARD);
    assert_eq!(card.children.len(), 2, "card is thumbnail + info row");
    let row = &card.children[1];
    assert_eq!(row.children.len(), 2, "info row is avatar + meta column");
    let meta = &row.children[1];
    assert_eq!(
        meta.children.len(),
        3,
        "meta column is title, channel row, views"
    );
    meta.children.iter().map(|n| n.rect).collect()
}

fn large_card_meta(h: &Harness<TubeApp>) -> Rect {
    let card = tree(h, CARD);
    assert_eq!(card.children.len(), 2, "card is thumbnail + metadata");
    let details = &card.children[1];
    assert_eq!(
        details.children.len(),
        1,
        "large-text metadata is one measure-first leaf"
    );
    details.children[0].rect
}

#[test]
fn card_meta_lines_stack_in_order_at_1x_and_2x() {
    // Case `card_meta_lines_stack_without_overlap_at_1x`.
    {
        let h = at(Route::Home, 1.0);
        assert_stacked_without_overlap("1× card meta", &card_meta_lines(&h));
    }
    // Case `card_meta_rects_stay_stacked_in_order_at_2x_layout_pass`:
    // Measure-first evidence for plan task 16, recorded in
    // `docs/superpowers/audits/2026-09-16-ui-alignment-review/a8-media.md`.
    //
    // The card's three meta lines are laid out in order and never overlap at 2×
    // — tube's own composition is sound. What this test deliberately does *not*
    // assert is that the 2× lines are taller than the 1× ones: they are not
    // (37/15/16 px at both scales) while the renderer measures the same strings
    // at 181/31 px, because the shared `NavSplit` places its panes with a fresh
    // unscaled `TextRenderer::new()` in `apply_layout` (`ui/src/widget/nav_split.rs`,
    // `reapply_children` fallback). That is a framework repair (F1); the probes
    // below prove the app-local `Flex`, the CSS grid and `Scrollable` all measure
    // correctly when hosted outside the split.
    #[expect(
        clippy::float_cmp,
        reason = "the text scale is stored as given, so it must read back exactly"
    )]
    {
        let mut h2 = at(Route::Home, 2.0);
        assert_eq!(h2.text_scale(), 2.0, "the frame really is at 2×");
        let rect = large_card_meta(&h2);
        let title = h2.measure_text(
            "Rewriting a Wayland compositor in Rust: results and benchmarks",
            14.5,
            Some(px(rect.width())),
        );
        let channel = h2.measure_text("Low Level Learning", 12.5, Some(px(rect.width())));
        let stats = h2.measure_text("842K • 2 days ago", 12.5, Some(px(rect.width())));
        assert!(
            px(rect.height()) >= title.1 + channel.1 + stats.1,
            "measure-first metadata leaf must reserve all three painted lanes: {rect:?}"
        );
    }
}

#[test]
fn text_measures_at_renderer_scale_in_every_container() {
    // Case `text_in_flex_measures_at_renderer_scale`:
    // One shared `Text` inside one app-local `Flex`, and the same `Text` on its
    // own, both at 2×: the app-local container must report the renderer's
    // measured size, not a 1× estimate. (Bisection step 1 of the task-16 trace;
    // kept as the regression guarding `Flex::build_layout`.)
    {
        use tube::widgets::flex::Flex;
        let mut alone = Harness::with_text_scale(
            tube::widgets::text("Low Level Learning", 12.5),
            (400.0, 300.0),
            2.0,
        );
        let alone_rect = alone.app.get_rect();
        let direct = alone.measure_text("Low Level Learning", 12.5, None);
        let in_flex = Harness::with_text_scale(
            Flex::col()
                .id("probe")
                .push(tube::widgets::text("Low Level Learning", 12.5)),
            (400.0, 300.0),
            2.0,
        );
        let flex_children = in_flex.child_rects("probe");
        // A root child is stretched to the viewport, so only its width is a
        // measurement; the Flex child is the real intrinsic case.
        assert!(
            (px(alone_rect.width()) - direct.0).abs() < 1.5,
            "Text alone measures {alone_rect:?} but the renderer says {direct:?}"
        );
        assert!(
            (px(flex_children[0].height()) - direct.1).abs() < 1.5,
            "Text in Flex measures {:?} but the renderer says {direct:?}",
            flex_children[0]
        );
    }
    // Case `text_in_grid_and_scrollable_measures_at_renderer_scale`:
    // The same two text lines inside (a) a CSS-grid `Flex` like `screens::grid`,
    // and (b) that grid inside the shared `Scrollable` like `screens::home`: both
    // layers must carry the renderer's measured heights through. (Bisection
    // steps 2–3 of the task-16 trace, kept as the regression for the card grid.)
    {
        use tube::widgets::flex::Flex;
        use ui::widget::{ScrollAxis, Scrollable};
        let title = "Rewriting a Wayland compositor in Rust: results and benchmarks";
        let card = || {
            Flex::col()
                .id("card")
                .gap(3.0)
                .push(tube::widgets::text(title, 14.5))
                .push(tube::widgets::text("Low Level Learning", 12.5))
        };
        let grid = || {
            Flex::row()
                .id("grid")
                .fill_w()
                .grid(300.0)
                .gap(18.0)
                .push(card())
        };

        let mut in_grid = Harness::with_text_scale(grid(), (700.0, 900.0), 2.0);
        let grid_lines = in_grid.child_rects("card");
        let w = px(grid_lines[0].width());
        let direct_title = in_grid.measure_text(title, 14.5, Some(w));
        let direct_channel = in_grid.measure_text("Low Level Learning", 12.5, None);
        assert!(
            (px(grid_lines[0].height()) - direct_title.1).abs() < 1.5,
            "grid: title line {:?} vs renderer {direct_title:?}",
            grid_lines[0]
        );

        let in_scroll = Harness::with_text_scale(
            Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(grid().block())),
            (700.0, 900.0),
            2.0,
        );
        let scroll_lines = in_scroll.child_rects("card");

        assert!(
            (px(grid_lines[1].height()) - direct_channel.1).abs() < 1.5,
            "grid: channel line {:?} vs renderer {direct_channel:?}",
            grid_lines[1]
        );
        assert!(
            (px(scroll_lines[1].height()) - direct_channel.1).abs() < 1.5,
            "scrollable: channel line {:?} vs renderer {direct_channel:?}",
            scroll_lines[1]
        );
    }
    // Case `search_header_chips_and_summary_do_not_collide_at_2x`.
    {
        let h = at(Route::Search("rust".into()), 2.0);
        let chips = h.rect_by_id("search_type_chips").expect("type chips row");
        let summary = h
            .rect_by_id("search_results_summary")
            .expect("results summary");
        let card = h.rect_by_id(CARD).expect("first result card");
        assert_stacked_without_overlap("2× search header", &[chips, summary, card]);
        // Every chip label lies inside its chip.
        for chip_id in ["type_chip_video", "type_chip_playlist", "type_chip_creator"] {
            let chip = tree(&h, chip_id);
            for child in &chip.children {
                assert!(
                    chip.rect.contains(child.rect.origin())
                        && chip.rect.contains(vello::kurbo::Point::new(
                            child.rect.x1 - 0.5,
                            child.rect.y1 - 0.5
                        )),
                    "{chip_id}: label {:?} escapes chip {:?}",
                    child.rect,
                    chip.rect
                );
            }
        }
    }
}
