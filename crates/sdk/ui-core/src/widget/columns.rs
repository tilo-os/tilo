//! A main column with an aside beside it, stacked on narrow windows:
//! [`Columns`].
//!
//! A detail with its related facts, a document with its outline, a form with
//! a preview: the aside keeps `metrics.inspector_width` beside the main
//! column while the main column still has `metrics.content_min_width`; below
//! that the aside goes under the main column at full width. The breakpoint
//! comes from the theme, so it follows `ui_scale`; nothing is decided by the
//! app.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{AppModel, Element, ModelPath, TaffyTreeExt, Widget};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;

/// See the [module docs](self).
pub struct Columns {
    main: Element,
    aside: Option<Element>,
    nodes: Vec<NodeId>,
    rect: Rect,
    aside_width: f32,
    gap: f32,
    breakpoint: f32,
    stacked: bool,
}

impl Columns {
    /// `main` alone, until an aside is added.
    #[must_use]
    pub fn new(main: Element) -> Self {
        Self {
            main,
            aside: None,
            nodes: Vec::new(),
            rect: Rect::ZERO,
            aside_width: 0.0,
            gap: 0.0,
            breakpoint: 0.0,
            stacked: false,
        }
    }

    /// The aside at the trailing edge (under the main column once stacked).
    #[must_use]
    pub fn aside(mut self, aside: Element) -> Self {
        self.aside = Some(aside);
        self
    }

    /// Whether the aside stands under the main column now.
    #[must_use]
    pub fn is_stacked(&self) -> bool {
        self.stacked
    }

    fn styles(&self) -> (Style, Style, Style) {
        let row = Style {
            display: taffy::Display::Flex,
            flex_direction: if self.stacked {
                taffy::FlexDirection::Column
            } else {
                taffy::FlexDirection::Row
            },
            align_items: Some(taffy::AlignItems::FlexStart),
            gap: taffy::Size {
                width: length(self.gap),
                height: length(self.gap),
            },
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Style::default()
        };
        let main = Style {
            flex_grow: if self.stacked { 0.0 } else { 1.0 },
            flex_basis: if self.stacked {
                taffy::Dimension::auto()
            } else {
                length(0.0_f32)
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            size: taffy::Size {
                width: if self.stacked {
                    percent(1.0_f32)
                } else {
                    taffy::Dimension::auto()
                },
                height: taffy::Dimension::auto(),
            },
            ..Style::default()
        };
        let aside = Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: if self.stacked {
                    percent(1.0_f32)
                } else {
                    length(self.aside_width)
                },
                height: taffy::Dimension::auto(),
            },
            ..Style::default()
        };
        (row, main, aside)
    }
}

fn wrap(tree: &mut TaffyTree<()>, child: NodeId, style: Style) -> NodeId {
    tree.parent_node(style, &[child])
}

impl Widget for Columns {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = &theme.tokens.metrics;
        self.aside_width = metrics.inspector_width;
        self.gap = theme.tokens.spacing.rhythm.get(Rhythm::Group);
        self.breakpoint = metrics.content_min_width + metrics.inspector_width + self.gap;
        let (row, main_style, aside_style) = self.styles();
        let main = self.main.build_layout(tree, theme, map);
        let mut nodes = vec![wrap(tree, main, main_style)];
        if let Some(aside) = &mut self.aside {
            let aside = aside.build_layout(tree, theme, map);
            nodes.push(wrap(tree, aside, aside_style));
        }
        self.nodes = nodes;
        let node = tree.parent_node(row, &self.nodes);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// Stack below the breakpoint, side by side above it.
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let Ok(width) = tree.layout(node).map(|layout| layout.size.width) else {
            return false;
        };
        let stacked = self.aside.is_some() && width < self.breakpoint;
        if stacked == self.stacked {
            return false;
        }
        self.stacked = stacked;
        let (row, main, aside) = self.styles();
        let mut changed = tree.set_style(node, row).is_ok();
        if let Some(&wrapper) = self.nodes.first() {
            changed |= tree.set_style(wrapper, main).is_ok();
        }
        if let Some(&wrapper) = self.nodes.get(1) {
            changed |= tree.set_style(wrapper, aside).is_ok();
        }
        changed
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(tree, node, origin);
        let start = Point::new(self.rect.x0, self.rect.y0);
        let parts = std::iter::once(&mut self.main).chain(self.aside.as_mut());
        for (child, &wrapper) in parts.zip(&self.nodes) {
            let at = tree.layout(wrapper).copied().unwrap_or_default();
            let inner = tree
                .children(wrapper)
                .ok()
                .and_then(|children| children.first().copied());
            if let Some(inner) = inner {
                child.apply_layout(
                    tree,
                    inner,
                    Point::new(
                        start.x + f64::from(at.location.x),
                        start.y + f64::from(at.location.y),
                    ),
                );
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.main.draw(scene, transform, text, theme, presence);
        if let Some(aside) = &mut self.aside {
            aside.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let parts = std::iter::once(&mut self.main).chain(self.aside.as_mut());
        for (child, &wrapper) in parts.zip(&self.nodes) {
            let inner = ctx
                .taffy
                .children(wrapper)
                .ok()
                .and_then(|children| children.first().copied());
            if let Some(inner) = inner {
                let path = ctx.path;
                child.handle_event(event, &mut ctx.reborrow(path, inner));
                if *ctx.handled {
                    break;
                }
            }
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.main.sync(state, path);
        if let Some(aside) = &mut self.aside {
            aside.sync(state, path);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        std::iter::once(self.main.as_ref())
            .chain(self.aside.as_deref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.main.as_ref());
        if let Some(aside) = &self.aside {
            visitor(aside.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![self.main.as_mut()];
        if let Some(aside) = &mut self.aside {
            out.push(aside.as_mut());
        }
        out
    }

    fn take_structure_dirty(&mut self) -> bool {
        let main = self.main.take_structure_dirty();
        let aside = self
            .aside
            .as_mut()
            .is_some_and(|aside| aside.take_structure_dirty());
        main || aside
    }

    fn name(&self) -> &'static str {
        "Columns"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Spacer;

    fn solve(columns: &mut Columns, width: f32) {
        let theme = Theme::dark();
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = columns.build_layout(&mut tree, &theme, &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        let mut text = crate::font_catalog::fixture_renderer();
        // SAFETY: exact live, exclusively owned widget tree and matching maps.
        unsafe {
            bindings.rebuild(columns, &tree, node, &nodes);
            crate::widget::compute_widget_layout(
                &mut tree,
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &nodes,
                &bindings,
                &mut text,
            )
            .expect("settles");
        }
        columns.apply_layout(&tree, node, Point::ZERO);
    }

    #[test]
    fn the_aside_stands_beside_until_the_main_column_would_be_too_narrow() {
        let theme = Theme::dark();
        let metrics = theme.tokens.metrics;
        let mut columns =
            Columns::new(Box::new(Spacer::fixed(10.0))).aside(Box::new(Spacer::fixed(10.0)));
        solve(&mut columns, 1200.0);
        assert!(!columns.is_stacked());
        let aside = columns.children()[1].get_rect();
        assert!((aside.x0 - (1200.0 - f64::from(metrics.inspector_width))).abs() < 0.5);
        solve(&mut columns, 700.0);
        assert!(columns.is_stacked());
        let (main, aside) = (
            columns.children()[0].get_rect(),
            columns.children()[1].get_rect(),
        );
        assert!(aside.y0 > main.y1, "the aside goes under the main column");
    }
}
