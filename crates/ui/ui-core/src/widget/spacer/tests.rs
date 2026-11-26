use tilo_ui_base::widget::spacer::*;
use tilo_ui_base::text_render::TextRenderer;
use crate::widget::{Column, Row, Text};
use taffy::style_helpers::length;
use taffy::{AvailableSpace, Size};

/// Same five-pass layout the `Column` tests use, with the real text
/// renderer measuring the `Text` siblings.
fn lay_out(root: &mut dyn Widget, width: f32, height: f32) -> (TaffyTree<()>, NodeId) {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nodes: std::collections::HashMap<NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let mut tr = TextRenderer::new();
    let node = root.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = nodes.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .unwrap();
    root.apply_layout(&taffy, node, Point::ZERO);
    (taffy, node)
}

fn child_rects(root: &dyn Widget) -> Vec<Rect> {
    root.children().iter().map(|c| c.get_rect()).collect()
}

#[test]
fn spacers_push_or_separate_by_their_resolved_extent() {
    {
        let mut row = Row::new()
            .no_wrap()
            .push(Box::new(Text::new("lead")))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new("trail")));
        lay_out(&mut row, 600.0, 100.0);
        let rects = child_rects(&row);
        assert!(
            rects[1].width() > 300.0,
            "the spacer took the slack: {rects:?}"
        );
        assert!(
            (rects[2].x1 - 600.0).abs() < 0.5,
            "trailing text lands on the far edge: {rects:?}"
        );
    }

    {
        let mut row = Row::new()
            .no_wrap()
            .push(Box::new(Text::new("a")))
            .push(Box::new(Spacer::fixed(16.0)))
            .push(Box::new(Text::new("b")));
        lay_out(&mut row, 600.0, 100.0);
        let rects = child_rects(&row);
        assert_eq!(rects[1].width(), 16.0, "{rects:?}");
        assert_eq!(rects[1].height(), 16.0, "{rects:?}");
        assert!(
            rects[2].x1 < 300.0,
            "siblings stay packed at the start; the fixed spacer took no slack: {rects:?}"
        );
    }

    {
        let theme = Theme::dark();
        let mut column = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .push(Box::new(Text::new("a")))
            .push(Box::new(Spacer::fixed(16.0)))
            .push(Box::new(Text::new("b")));
        lay_out(&mut column, 400.0, 400.0);
        let rects = child_rects(&column);
        assert_eq!(rects[1].height(), 16.0, "{rects:?}");
        let expected = rects[0].y1 + f64::from(theme.gap_standard) * 2.0 + 16.0;
        assert!(
            (rects[2].y0 - expected).abs() < 0.5,
            "b starts after a, two gaps and the spacer: {rects:?}"
        );
    }

    {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let mut spacer = Spacer::token(SpacingStep::S4);
        let node = spacer.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        let size = taffy.style(node).unwrap().size;
        assert_eq!(size.width, length(16.0_f32));
        assert_eq!(size.height, length(16.0_f32));

        let scaled = Theme::dark().with_ui_scale(1.5);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut spacer = Spacer::token(SpacingStep::S4);
        let node = spacer.build_layout(&mut taffy, &scaled, &mut nodes);
        assert_eq!(taffy.style(node).unwrap().size.height, length(24.0_f32));
    }

    // Case `zero_extent_spacer_at_the_origin_is_not_permanently_layout_dirty`.
    {
        let mut column = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(Spacer::token(SpacingStep::S0)))
            .push(Box::new(Text::new("body")));
        assert!(
            column.children()[0].is_layout_dirty(),
            "dirty before layout"
        );
        lay_out(&mut column, 400.0, 400.0);
        let spacer = column.children()[0];
        assert_eq!(spacer.get_rect(), Rect::ZERO, "zero size at the origin");
        assert!(!spacer.is_layout_dirty(), "laid out once is laid out");
    }

    {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let mut spacer = Spacer::fixed(-8.0);
        let node = spacer.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        assert_eq!(
            taffy.style(node).unwrap().display,
            taffy::Display::None,
            "clamped to nothing, and out of the flow"
        );
    }
}
