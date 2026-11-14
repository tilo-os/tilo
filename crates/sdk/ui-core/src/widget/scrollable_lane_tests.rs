//! The bar's lane across the passes of one layout.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, TaffyTree};

use super::Scrollable;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Column, Text, Widget};

/// Lay `scroll` out `height` tall and let it follow its overflow, as a
/// host's pass does before the layout is applied: whether it changed.
fn pass(
    scroll: &mut Scrollable,
    taffy: &mut TaffyTree<()>,
    node: NodeId,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    height: f32,
) -> bool {
    let mut text = TextRenderer::new();
    let mut style = taffy.style(node).unwrap().clone();
    style.size = Size {
        width: Dimension::length(400.0),
        height: Dimension::length(height),
    };
    taffy.set_style(node, style).unwrap();
    taffy
        .compute_layout_with_measure(
            node,
            Size::max_content(),
            |known, available, node_id, _, _| match nodes.get(&node_id) {
                // SAFETY: the map holds the live widgets of this tree.
                Some(&widget) => unsafe { (*widget).measure(known, available, &mut text) },
                None => Size::ZERO,
            },
        )
        .unwrap();
    scroll.adjust_layout_style(taffy, node, &mut text)
}

/// A parent sizing itself to its content lays the view out in a box other
/// than the one it gets. Content that overflowed that box, but fits the
/// real one, keeps no lane, so the layout settles at the full width.
#[test]
fn a_lane_needed_only_in_a_box_measured_on_the_way_is_given_back() {
    let mut lines = Column::new().with_intrinsic_height();
    for i in 0..4 {
        lines = lines.push(Box::new(Text::new(&format!("line {i}"))));
    }
    let mut scroll = Scrollable::new_vertical().with_child(Box::new(lines));
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);

    assert!(pass(&mut scroll, &mut taffy, node, &nodes, 20.0));
    let lane = |taffy: &TaffyTree<()>| {
        taffy.style(node).unwrap().padding.right != taffy::LengthPercentage::length(0.0)
    };
    assert!(lane(&taffy), "the short box overflows: a lane");

    let settled = (0..4).any(|_| !pass(&mut scroll, &mut taffy, node, &nodes, 300.0));
    assert!(settled, "the real box settles");
    assert!(
        !lane(&taffy),
        "the content fits the box it gets, so its bar's lane is given back"
    );
}

/// A page in a navigation shell, as Clocks shows its timers: the
/// shell's first solve gives the scroll view no width, where the
/// wrapped page is far taller than the window, and the next gives it
/// its place, where the page fits. That early overflow does not keep
/// the bar's lane at the real size, or each layout would end with the
/// lane the one before it dropped, and the window would never settle
/// on any width.
#[test]
fn content_that_overflowed_before_the_view_had_its_place_keeps_no_lane() {
    let page = || -> crate::widget::Element {
        let mut column = Column::new().with_intrinsic_height();
        for _ in 0..6 {
            column = column.push(Box::new(Text::new(
                "A paragraph long enough to wrap onto many lines when the view it \
                 is laid out in has no width yet, and onto one or two once it has.",
            )));
        }
        Box::new(column)
    };
    let shell = crate::widget::NavShell::new()
        .content_kind(crate::widget::ContentKind::Page)
        .mode(crate::widget::NavMode::Push)
        .sidebar(Box::new(crate::widget::NavList::new(vec![
            crate::widget::NavSection::untitled(
                "sections",
                ["One", "Two", "Three"]
                    .into_iter()
                    .map(|label| crate::widget::NavItem::new(label.to_owned(), label))
                    .collect(),
            ),
        ])))
        .content(Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(page()),
        ));
    let mut harness = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(shell),
        (900, 760),
        Theme::dark(),
    );
    assert!(harness.take_skip_report().is_none());
    for width in [880, 700, 520] {
        harness
            .resize((width, 760))
            .unwrap_or_else(|error| panic!("{width} px: {error:?}"));
        let lane = harness
            .with_widget_mut(|scroll: &mut Scrollable| scroll.reserve_vertical)
            .unwrap();
        assert!(!lane, "{width} px: the page fits, so no lane is kept");
    }
}

/// A place on a canvas that hugs what it holds, up to a cap: the region
/// measures the scroll view at its natural height (a probe solve, in a copy of
/// the tree) and places it no taller than the cap. Words longer than the cap
/// overflow the real box, so its bar keeps its lane there; in the probe the
/// view is as tall as its words and needs none. Both solves run the same
/// widget, and the lane one tree decided must not be taken for the other's,
/// or the window settles on no frame at all.
#[test]
fn a_view_measured_at_its_natural_height_and_placed_at_a_cap_settles_with_its_lane() {
    use crate::widget::{Manual, MeasureConstraint};
    const CAP: f64 = 120.0;
    let mut words = Column::new().with_intrinsic_height();
    for _ in 0..8 {
        words = words.push(Box::new(Text::new(
            "A bulleted answer long enough to wrap over a few lines in a turn's card, \
             so the card is taller than the room the canvas gives it.",
        )));
    }
    let region = Manual::new(|cx| {
        let width = 520.0_f32;
        let natural = cx
            .measure_child(
                0,
                Size {
                    width: crate::widget::MeasureConstraint::Exact(width),
                    height: MeasureConstraint::Intrinsic,
                },
            )?
            .size
            .height;
        let height = f64::from(natural).min(CAP);
        cx.place(
            0,
            vello::kurbo::Rect::from_origin_size((10.0, 10.0), (f64::from(width), height)),
        )?;
        Ok(cx.available())
    })
    .filling()
    .child(Box::new(
        Scrollable::new_vertical().with_child(Box::new(words)),
    ));
    let mut harness = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(region),
        (1280, 800),
        Theme::dark(),
    );
    harness.relayout();
    assert_eq!(harness.take_skip_report(), None, "every frame lays out");
    harness.recompose().expect("a frame after settles too");
    let (lane, height) = harness
        .with_widget_mut(|scroll: &mut Scrollable| (scroll.reserve_vertical, scroll.rect.height()))
        .unwrap();
    assert!(
        (height - CAP).abs() < 1.0,
        "the view stands at the cap: {height}"
    );
    assert!(
        lane,
        "the words overflow the cap, so the bar keeps its lane"
    );
}
