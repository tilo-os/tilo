//! Retained graph composition: real controls on Manual, decorative themed edges.
use super::composition_parts::{Parts, retained_parts};
use super::{Element, Manual, MeasureConstraint, Widget};
use crate::{Theme, text_render::TextRenderer};
use std::{cell::Cell, collections::HashMap, rc::Rc};
use taffy::{NodeId, Size, Style, TaffyTree};
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
    /// What each node is called, in order, for a connection that names a node rather than a
    /// control in one.
    node_ids: Vec<String>,
    /// What the graph is called: shown over it, as a chart's title, and its name for assistive
    /// technology.
    title: Option<String>,
    /// Where its described nodes stand, as its props said ([`Described`](crate::component::Described)).
    layout: String,
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
            node_ids: Vec::new(),
            title: None,
            layout: String::new(),
        }
    }

    /// Arrange real controls in measured rows, preventing overlap as text scales.
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
            node_ids: Vec::new(),
            title: None,
            layout: String::new(),
        }
    }

    /// Show `title` over the graph, in a chart's title style, a related gap over its nodes.
    #[must_use]
    pub fn with_title(mut self, title: Option<String>) -> Self {
        if let Some(title) = &title {
            let heading = super::Text::new(title).with_role(crate::tokens::TextRole::Heading);
            self.parts.children.insert(0, Box::new(heading));
        }
        self.title = title;
        self
    }

    fn connection_rects(&self) -> Vec<(Rect, Rect)> {
        let Some(nodes) = self.parts.children.last() else {
            return Vec::new();
        };
        let nodes = nodes.as_ref();
        let end = |id: &str| {
            self.node_ids
                .iter()
                .position(|node| node == id)
                .and_then(|index| nodes.children().get(index).map(|node| node.get_rect()))
                .or_else(|| find(nodes, id))
        };
        self.connections
            .iter()
            .filter_map(|edge| Some((end(&edge.from)?, end(&edge.to)?)))
            .collect()
    }
}

fn find(widget: &dyn Widget, id: &str) -> Option<Rect> {
    super::find_test_id(widget, id).map(Widget::get_rect)
}

impl Widget for GraphCanvas {
    crate::described!();

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
        // The title, if any, over the nodes, a related gap apart.
        let node = self.parts.build(
            Style {
                size: Size::auto(),
                flex_direction: taffy::FlexDirection::Column,
                gap: Size {
                    width: taffy::LengthPercentage::length(0.0),
                    height: taffy::LengthPercentage::length(f32::from(
                        theme.tokens.spacing.rhythm.related,
                    )),
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
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        // The lines are the canvas's own paint; its nodes draw as widgets of their own.
        crate::scene_parts::picture();
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
        self.parts.draw(&mut crate::draw::DrawCtx::new(scene, transform, text, theme, env.clone()));
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name(self.title.clone()),
        )
    }
}

#[path = "graph_canvas/graph_canvas_component.rs"]
mod graph_canvas_component;
pub use graph_canvas_component::{
    Connection, GraphCanvasProps, GraphNodeProps, connection_description,
};

#[cfg(test)]
#[path = "graph_canvas/tests.rs"]
mod tests;
