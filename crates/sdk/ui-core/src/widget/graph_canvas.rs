//! Retained graph composition: real controls on Manual, decorative themed edges.
use super::composition_parts::{Parts, retained_parts};
use super::{Element, Manual, MeasureConstraint, Widget};
use crate::{PresenceCtx, Theme, text_render::TextRenderer};
use automerge::ObjId;
use std::{cell::Cell, collections::HashMap, rc::Rc};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, Stroke},
};

/// One domain-owned node's placement and ordinary retained content.
pub struct GraphCanvasNode {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub content: Element,
}

/// Semantic widget IDs of the two retained port controls.
#[derive(Clone, Debug)]
pub struct GraphCanvasConnection {
    pub from: String,
    pub to: String,
}

/// Domain-free canvas. Supplied controls retain their framework action bindings.
/// The canvas paints only connections; applications own typed graph resources.
pub struct GraphCanvas {
    parts: Parts,
    connections: Vec<GraphCanvasConnection>,
    grid_gap: Option<Rc<Cell<f32>>>,
}

impl GraphCanvas {
    #[must_use]
    pub fn new(nodes: Vec<GraphCanvasNode>, connections: Vec<GraphCanvasConnection>) -> Self {
        let placements: Vec<_> = nodes
            .iter()
            .map(|node| (node.x, node.y, node.width))
            .collect();
        let mut manual = Manual::new(move |cx| {
            let mut width = cx.available().width;
            let mut height = 0.0_f32;
            for (index, &(x, y, w)) in placements.iter().enumerate() {
                let size = cx
                    .measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(w),
                            height: MeasureConstraint::Intrinsic,
                        },
                    )?
                    .size;
                cx.place(
                    index,
                    Rect::from_origin_size(
                        (f64::from(x), f64::from(y)),
                        (f64::from(w), f64::from(size.height)),
                    ),
                )?;
                width = width.max(x + w);
                height = height.max(y + size.height);
            }
            Ok(Size { width, height })
        });
        for node in nodes {
            manual = manual.child(node.content);
        }
        Self {
            parts: Parts::new(vec![Box::new(manual)]),
            connections,
            grid_gap: None,
        }
    }

    /// Arrange real controls in measured rows, preventing overlap as text scales.
    /// Authored graph coordinates remain available through [`Self::new`].
    #[must_use]
    pub fn grid(
        nodes: Vec<Element>,
        connections: Vec<GraphCanvasConnection>,
        column_width: f32,
        columns: u16,
    ) -> Self {
        let columns = columns.max(1);
        let gap = Rc::new(Cell::new(0.0_f32));
        let spacing = gap.clone();
        let mut manual = Manual::new(move |cx| {
            let mut width = 0.0_f32;
            let mut y = 0.0_f32;
            for start in (0..cx.len()).step_by(usize::from(columns)) {
                let mut row = Vec::new();
                let mut height = 0.0_f32;
                for index in start..start.saturating_add(usize::from(columns)).min(cx.len()) {
                    let size = cx
                        .measure_child(
                            index,
                            Size {
                                width: MeasureConstraint::Exact(column_width),
                                height: MeasureConstraint::Intrinsic,
                            },
                        )?
                        .size;
                    height = height.max(size.height);
                    row.push((index, size.height));
                }
                for (index, height) in row {
                    let column = u16::try_from(index % usize::from(columns)).unwrap_or(0);
                    let x = f32::from(column) * (column_width + spacing.get());
                    cx.place(
                        index,
                        Rect::from_origin_size(
                            (f64::from(x), f64::from(y)),
                            (f64::from(column_width), f64::from(height)),
                        ),
                    )?;
                    width = width.max(x + column_width);
                }
                y += height + spacing.get();
            }
            Ok(Size { width, height: y })
        });
        for node in nodes {
            manual = manual.child(node);
        }
        Self {
            parts: Parts::new(vec![Box::new(manual)]),
            connections,
            grid_gap: Some(gap),
        }
    }

    fn connection_rects(&self) -> Vec<(Rect, Rect)> {
        self.connections
            .iter()
            .filter_map(|edge| {
                Some((
                    find(self.parts.children[0].as_ref(), &edge.from)?,
                    find(self.parts.children[0].as_ref(), &edge.to)?,
                ))
            })
            .collect()
    }
}

fn find(widget: &dyn Widget, id: &str) -> Option<Rect> {
    super::find_test_id(widget, id).map(Widget::get_rect)
}

impl Widget for GraphCanvas {
    retained_parts!("GraphCanvas");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if let Some(gap) = &self.grid_gap {
            gap.set(theme.gap_standard);
        }
        let node = self.parts.build(
            Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::auto(),
                },
                ..Style::default()
            },
            tree,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for (from, to) in self.connection_rects() {
            let start = from.center();
            let end = to.center();
            let bend = (end.x - start.x).abs() / 2.0;
            let mut path = BezPath::new();
            path.move_to(start);
            path.curve_to(
                Point::new(start.x + bend, start.y),
                Point::new(end.x - bend, end.y),
                end,
            );
            scene.stroke(
                &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
                transform,
                theme.text_dim,
                None,
                &path,
            );
        }
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

#[cfg(test)]
#[path = "graph_canvas/tests.rs"]
mod tests;
