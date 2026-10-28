//! A scroll that only moves the view places only the rows near it: rows
//! far away keep their boxes until they come near, and nothing that reads
//! boxes (the copy's walk, the pointer) meets one that stands stale. A row
//! that lays out again with a wider paint is found near the view from then.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "test fixtures: failures are the assertion; boxes are placed exactly"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::theme::Theme;
use crate::widget::{Column, Scrollable, TaffyTreeExt as _, Widget};

const ROW: f32 = 20.0;
const VIEW: f32 = 400.0;

/// A row that paints `reach` past its box below it, asks for a layout of
/// its own when told to, and counts the pointer moves it is given.
struct Glow {
    rect: Rect,
    reach: Rc<Cell<f64>>,
    relayout: Rc<Cell<bool>>,
    pointed: Rc<Cell<u32>>,
}

impl Widget for Glow {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(200.0, ROW),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.relayout.set(false);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn paint_bounds(&self) -> Rect {
        let mut bounds = self.rect;
        bounds.y1 += self.reach.get();
        bounds
    }

    fn is_layout_dirty(&self) -> bool {
        self.relayout.get()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { .. } = event {
            self.pointed.set(self.pointed.get() + 1);
        }
        let _ = ctx;
    }

    fn name(&self) -> &'static str {
        "Glow"
    }
}

struct Rows {
    scroll: Scrollable,
    taffy: TaffyTree<()>,
    node: NodeId,
    reach: Vec<Rc<Cell<f64>>>,
    relayout: Vec<Rc<Cell<bool>>>,
    pointed: Vec<Rc<Cell<u32>>>,
}

/// A scroll view `VIEW` tall over `count` rows, laid out and placed once
/// in full at the top.
fn rows(count: usize) -> Rows {
    let reach: Vec<_> = (0..count).map(|_| Rc::new(Cell::new(0.0))).collect();
    let relayout: Vec<_> = (0..count).map(|_| Rc::new(Cell::new(false))).collect();
    let pointed: Vec<_> = (0..count).map(|_| Rc::new(Cell::new(0))).collect();
    let column = (0..count).fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(0.0),
        |column, row| {
            column.push(Box::new(Glow {
                rect: Rect::ZERO,
                reach: reach[row].clone(),
                relayout: relayout[row].clone(),
                pointed: pointed[row].clone(),
            }))
        },
    );
    let mut scroll = Scrollable::new_vertical()
        .with_scrollbar(false)
        .with_child(Box::new(column));
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(VIEW),
            },
        )
        .unwrap();
    scroll.apply_layout(&taffy, node, Point::ZERO);
    Rows {
        scroll,
        taffy,
        node,
        reach,
        relayout,
        pointed,
    }
}

impl Rows {
    /// Scroll to `offset` and place the rows again from the unchanged
    /// layout, as a host's frame of a scroll does.
    fn scroll_to(&mut self, offset: f32) {
        self.scroll.set_offset_y(offset);
        let tree = crate::widget::layout::TreeId::of(&self.taffy);
        crate::widget::layout::placing_again_in(tree, || {
            self.scroll
                .apply_layout(&self.taffy, self.node, Point::ZERO);
        });
    }

    /// The rows the copy's walk of the tree visits, with their boxes.
    fn walked(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        self.scroll.for_each_child_in_clip(&mut |column| {
            column.for_each_child_in_clip(&mut |row| out.push(row.get_rect()));
        });
        out
    }

    fn row(&self, index: usize) -> Rect {
        let mut found = None;
        self.scroll.for_each_child(&mut |column| {
            let mut at = 0;
            column.for_each_child(&mut |row| {
                if at == index {
                    found = Some(row.get_rect());
                }
                at += 1;
            });
        });
        found.unwrap()
    }
}

#[test]
fn a_scroll_places_only_the_rows_near_the_view_and_walks_only_those() {
    let mut rows = rows(2_000);
    rows.scroll_to(2_000.0);
    // Rows 100 to 120 meet the view.
    let walked = rows.walked();
    assert!(!walked.is_empty() && walked.len() < 30, "{walked:?}");
    for rect in &walked {
        assert!(
            rect.y1 > -1.0 && rect.y0 < f64::from(VIEW) + 1.0,
            "a walked row stands where the view shows it: {rect:?}"
        );
    }
    assert_eq!(rows.row(100).y0, 0.0, "row 100 tops the view");
    // The rows the view showed before were placed once more as they left.
    assert_eq!(rows.row(0).y0, -2_000.0);
    // Row 500 was never near the view: it keeps the box of the first
    // placement, which no walk and no pointer meets.
    assert_eq!(rows.row(500).y0, 10_000.0);
    crate::testing::dispatch(
        &mut rows.scroll,
        &crate::synthetic_input::cursor_moved(10.0, 10.0),
    );
    assert_eq!(
        rows.pointed[500].get(),
        0,
        "a row far from the view is not given the pointer"
    );
    assert_eq!(rows.pointed[100].get(), 1, "a row in view is");

    // Rows leaving the view are placed once more, where they now stand.
    rows.scroll_to(2_400.0);
    assert_eq!(rows.row(100).y0, -400.0, "row 100 left the view upwards");
    assert_eq!(rows.row(120).y0, 0.0);
}

#[test]
fn a_row_whose_paint_grows_is_found_near_the_view_once_it_lays_out_again() {
    let mut rows = rows(400);
    rows.scroll_to(2_000.0);
    let row_80 = |rows: &Rows| rows.walked().contains(&rows.row(80));
    assert!(!row_80(&rows), "row 80 stands 400 px above the view");
    // Row 80 now paints 500 px below its box: into the view.
    rows.reach[80].set(500.0);
    rows.relayout[80].set(true);
    rows.scroll_to(2_000.0);
    assert_eq!(
        rows.row(80).y0,
        -400.0,
        "the row that asked for a layout was placed where it stands"
    );
    assert!(
        row_80(&rows),
        "what it paints reaches the view: the walk visits it"
    );
    rows.scroll_to(2_040.0);
    assert_eq!(rows.row(80).y0, -440.0);
    assert!(row_80(&rows), "and keeps visiting it as the view moves on");
}
