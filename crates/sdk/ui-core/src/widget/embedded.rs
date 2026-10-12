//! A retained widget placed by hand inside a widget that draws itself.
//!
//! Canvas-style widgets (a word processor's review panel, a PDF viewer's
//! comment overlay) paint their own chrome and hand out rectangles. A child
//! that lays itself out through Taffy (a block editor, a card) still needs a
//! layout tree, node bindings and the event plumbing a host gives it.
//! [`Embedded`] is that host, owned by the parent: lay the child out in a
//! rectangle, then draw it and route events to it as usual.
//!
//! A child can live in its own coordinate space under a transform: a text
//! box on a slide is laid out at the slide's size and drawn scaled to the
//! canvas, with pointer events mapped back into it and its caret mapped out.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, WindowEvent};

use super::Widget;
use crate::bound_dispatch::BoundNodeMap;
use crate::{PresenceCtx, Theme, text_render::TextRenderer};

/// A widget laid out and driven by the widget that owns it.
pub struct Embedded<W: Widget> {
    // Boxed so the node map's pointers stay valid when the host moves.
    widget: Box<W>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: Option<NodeId>,
    rect: Rect,
    /// From the child's own space to the owner's.
    transform: Affine,
    /// The text colour the child paints with, in place of the theme's.
    text_color: Option<Color>,
    /// The rectangle last laid out and the height it came to, reused while
    /// nothing about the child changed (a thumbnail redrawn every frame).
    laid: Option<(Rect, f64)>,
    /// A press inside the child asked for keyboard focus since the owner
    /// last looked ([`Embedded::take_focus_request`]).
    focus_request: bool,
    /// The child takes exactly the rectangle it is given.
    fill: bool,
    /// Build-time theme inputs must match the paint-time theme even when the
    /// rectangle and document have not changed (for example, larger text).
    layout_theme: Option<Theme>,
}

impl<W: Widget + 'static> Embedded<W> {
    #[must_use]
    pub fn new(widget: W) -> Self {
        Self {
            widget: Box::new(widget),
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            bindings: BoundNodeMap::new(),
            root: None,
            rect: Rect::ZERO,
            transform: Affine::IDENTITY,
            text_color: None,
            laid: None,
            focus_request: false,
            fill: false,
            layout_theme: None,
        }
    }

    /// The child takes exactly the rectangle it is laid out in, rather than
    /// its content's height: a viewport of its own (a terminal, a viewer)
    /// that scrolls inside the rectangle the owner gives it.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Place the child's own space in the owner's: layout rectangles are in
    /// the child's space, and it is drawn, hit and its caret reported
    /// through `transform` (a slide's text box scaled onto the canvas).
    pub fn set_transform(&mut self, transform: Affine) {
        self.transform = transform;
    }

    /// Paint the child's text in `color` rather than the theme's (a text
    /// box coloured by its slide).
    pub fn set_text_color(&mut self, color: Option<Color>) {
        self.text_color = color;
    }

    /// The theme the child is laid out and painted with, when it is not
    /// the owner's.
    fn recoloured(&self, theme: &Theme) -> Option<Theme> {
        self.text_color.map(|color| {
            let mut theme = theme.clone();
            theme.tokens.colors.text_primary = color;
            theme.text_primary = color;
            theme
        })
    }

    /// Where the child's caret is, in the owner's space, for the input
    /// method's candidate window.
    pub fn ime_cursor_rect(&self) -> Option<Rect> {
        self.widget
            .ime_cursor_rect()
            .map(|rect| self.transform.transform_rect_bbox(rect))
    }

    /// The hosted widget.
    pub fn widget(&self) -> &W {
        &self.widget
    }

    /// The hosted widget, mutably.
    pub fn widget_mut(&mut self) -> &mut W {
        &mut self.widget
    }

    /// Where the widget was last laid out, at the height it wanted, in the
    /// owner's space.
    pub fn rect(&self) -> Rect {
        self.transform.transform_rect_bbox(self.rect)
    }

    fn rebuild(&mut self, theme: &Theme) {
        self.bindings.clear();
        self.nodes.clear();
        self.tree = TaffyTree::new();
        let root = self
            .widget
            .build_layout(&mut self.tree, theme, &mut self.nodes);
        // SAFETY: the boxed widget owns every node in `nodes` and stays in
        // place until the next rebuild, which clears the maps first.
        unsafe {
            self.bindings
                .rebuild(self.widget.as_ref(), &self.tree, root, &self.nodes);
        }
        self.root = Some(root);
        self.layout_theme = Some(theme.clone());
        self.laid = None;
    }

    /// Lay the widget out at `rect`'s width and origin, as tall as its
    /// content wants (at least `rect`'s height); returns that height.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical pixel sizes narrow to Taffy's f32"
    )]
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) -> f64 {
        let recoloured = self.recoloured(theme);
        let theme = recoloured.as_ref().unwrap_or(theme);
        let structure_dirty = self.widget.take_structure_dirty();
        let rebuilt = self.root.is_none()
            || structure_dirty
            || self.layout_theme.as_ref().is_none_or(|laid| laid != theme);
        if !rebuilt
            && let Some((laid, height)) = self.laid
            && laid == rect
            && !self.widget.layout_needs_measure()
        {
            return height;
        }
        if rebuilt {
            self.rebuild(theme);
        }
        let Some(root) = self.root else {
            return 0.0;
        };
        let available = Size {
            width: AvailableSpace::Definite(rect.width() as f32),
            height: if self.fill {
                AvailableSpace::Definite(rect.height() as f32)
            } else {
                AvailableSpace::MaxContent
            },
        };
        if self.fill
            && let Ok(mut style) = self.tree.style(root).cloned()
        {
            style.size = Size {
                width: taffy::Dimension::length(rect.width() as f32),
                height: taffy::Dimension::length(rect.height() as f32),
            };
            let _ = self.tree.set_style(root, style);
        }
        // SAFETY: the tree, node map and bindings were built from this exact
        // boxed widget, which nothing else borrows while the layout runs.
        let solved = unsafe {
            super::compute_widget_layout(
                &mut self.tree,
                root,
                available,
                &self.nodes,
                &self.bindings,
                text,
            )
        };
        if let Err(error) = solved {
            tracing::debug!(%error, "embedded widget layout did not settle");
        }
        self.widget
            .apply_layout(&self.tree, root, Point::new(rect.x0, rect.y0));
        let height = self
            .tree
            .layout(root)
            .map_or(0.0, |layout| f64::from(layout.size.height))
            .max(rect.height());
        self.rect = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + height);
        self.laid = Some((rect, height));
        height
    }

    /// Paint the widget where it was last laid out.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let recoloured = self.recoloured(theme);
        let theme = recoloured.as_ref().unwrap_or(theme);
        self.widget
            .draw(scene, transform * self.transform, text, theme, presence);
        self.widget.mark_drawn();
    }

    /// Paint what the widget floats over everything (its menus, a link's
    /// popover). Call from the owner's own `draw_overlay`, so it lands in the
    /// window's top layer, above the rest of the owner's paint.
    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let recoloured = self.recoloured(theme);
        let theme = recoloured.as_ref().unwrap_or(theme);
        self.widget
            .draw_overlay(scene, transform * self.transform, text, theme, presence);
    }

    /// Route one window event to the widget.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
        handled: &mut bool,
    ) {
        // Keys can reach a field before it is first laid out (typing right
        // after it opens); give the widget its tree so they are not lost.
        if self.root.is_none() {
            self.rebuild(&Theme::dark());
        }
        let Some(root) = self.root else {
            return;
        };
        // Pointer positions arrive in the owner's space; the child hits in
        // its own.
        let mapped;
        let event = match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } if self.transform != Affine::IDENTITY => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                let local = self.transform.inverse() * logical;
                mapped = WindowEvent::CursorMoved {
                    device_id: *device_id,
                    position: PhysicalPosition::new(local.x * scale_factor, local.y * scale_factor),
                };
                &mapped
            }
            _ => event,
        };
        let mut focus = None;
        self.widget.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers,
                state: state.reborrow(),
                path,
                handled,
                focus_target: &mut focus,
                node_id: root,
                taffy: &self.tree,
            },
        );
        // The owner holds the window's focus for its embedded child: a press
        // that focused something inside is the owner's cue to take it.
        if focus.is_some() {
            self.focus_request = true;
        }
    }

    /// Whether the child asked for keyboard focus (a press on a field)
    /// since the last call. The owner takes the host's focus in response
    /// (`*ctx.focus_target = Some(ctx.node_id)`) and keeps routing keys and
    /// the input method to the child.
    pub fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.focus_request)
    }

    /// Bring the widget up to date with the document.
    pub fn sync(&mut self, state: &AutoCommit, path: &ObjId) {
        self.widget.sync(state, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{BlockContent, BlockEditor, TaffyTreeExt};
    use winit::event::{ElementState, MouseButton};

    fn click(host: &mut Embedded<BlockEditor>, doc: &mut crate::widget::Edits<'_>, at: Point) {
        let device_id = winit::event::DeviceId::dummy();
        for event in [
            WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(at.x, at.y),
            },
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            let mut handled = false;
            host.handle_event(
                &event,
                1.0,
                &Modifiers::default(),
                doc,
                &automerge::ROOT,
                &mut handled,
            );
        }
    }

    #[test]
    fn a_child_under_a_transform_is_hit_and_reported_where_it_is_drawn() {
        let theme = Theme::dark();
        let mut text = crate::font_catalog::fixture_renderer();
        let mut doc = AutoCommit::new();
        let mut host = Embedded::new(BlockEditor::new().with_wrap_lines(true));
        host.widget_mut()
            .present_blocks(&[BlockContent::paragraph("a", "a line of text")]);
        // The box is laid out at (100, 100)..(500, ..) in its own space and
        // drawn at half size, 10 px in.
        host.set_transform(Affine::translate((10.0, 10.0)) * Affine::scale(0.5));
        let height = host.layout(Rect::new(100.0, 100.0, 500.0, 100.0), &theme, &mut text);
        assert!(height > 0.0);
        let drawn = host.rect();
        assert!((drawn.x0 - 60.0).abs() < 1e-9 && (drawn.width() - 200.0).abs() < 1e-9);
        // A click where the box is drawn lands in it.
        click(
            &mut host,
            &mut crate::widget::Edits::lend(&mut doc),
            Point::new(drawn.x0 + 20.0, drawn.y0 + 4.0),
        );
        assert!(host.widget().is_focused(), "the click reached the box");
        assert!(
            host.take_focus_request(),
            "the owner is asked to take focus"
        );
        assert!(!host.take_focus_request(), "once");
        let caret = host.ime_cursor_rect().expect("a focused line has a caret");
        assert!(
            drawn.inflate(1.0, 1.0).contains(caret.center()),
            "{caret:?} within {drawn:?}"
        );
    }

    #[test]
    fn retained_unfocused_editor_reflows_when_the_host_text_scale_changes() {
        let normal = Theme::dark();
        let large = Theme::high_contrast().with_text_scale(2.0);
        let fixture = [BlockContent::paragraph("private", "Unsent Studio context")];
        for width in [220.0, 180.0] {
            let mut text = crate::font_catalog::fixture_renderer();
            let mut host = Embedded::new(BlockEditor::new().with_wrap_lines(true));
            host.widget_mut().present_blocks(&fixture);
            host.widget_mut()
                .focus_block(0, super::super::CursorPlacement::End);
            host.widget_mut().select_before_caret(8);
            host.widget_mut().set_focus(false);
            let before = host
                .widget()
                .blocks()
                .first()
                .unwrap()
                .line
                .selection_range();
            // Let the initial structure-dirty pass settle before changing
            // settings; a pending rebuild would hide a stale retained tree.
            for _ in 0..3 {
                host.layout(Rect::new(0.0, 0.0, 220.0, 0.0), &normal, &mut text);
                host.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut text,
                    &normal,
                    &PresenceCtx::empty(),
                );
            }

            text.set_text_scale(large.text_scale);
            let rect = Rect::new(0.0, 0.0, width, 0.0);
            let retained_height = host.layout(rect, &large, &mut text);
            let mut fresh = Embedded::new(BlockEditor::new().with_wrap_lines(true));
            fresh.widget_mut().present_blocks(&fixture);
            let fresh_height = fresh.layout(rect, &large, &mut text);
            assert!(fresh_height > 40.0, "the large-text fixture must wrap");
            assert!(
                (retained_height - fresh_height).abs() < 0.01,
                "retained editor measured {retained_height}, fresh editor {fresh_height} at width {width}"
            );
            let block = host.widget().blocks().first().unwrap();
            assert_eq!(block.line.value(), "Unsent Studio context");
            assert_eq!(block.line.selection_range(), before);
            assert!(!host.widget().is_focused());
        }
    }

    #[test]
    fn equal_themes_reuse_layout_but_same_named_custom_tokens_rebuild_it() {
        struct ThemedBox {
            builds: usize,
        }
        impl Widget for ThemedBox {
            fn build_layout(
                &mut self,
                tree: &mut TaffyTree<()>,
                theme: &Theme,
                nodes: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                self.builds = self.builds.saturating_add(1);
                let node = tree.leaf_node(taffy::Style {
                    size: Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length(theme.padding_standard),
                    },
                    ..Default::default()
                });
                nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
                node
            }
            fn name(&self) -> &str {
                "themed-box"
            }
        }

        let mut theme = Theme::dark();
        let original_name = theme.name.clone();
        let mut text = crate::font_catalog::fixture_renderer();
        let mut host = Embedded::new(ThemedBox { builds: 0 });
        let rect = Rect::new(0.0, 0.0, 200.0, 0.0);
        let original_height = host.layout(rect, &theme, &mut text);
        host.layout(rect, &theme.clone(), &mut text);
        assert_eq!(
            host.widget().builds,
            1,
            "equal theme content keeps the tree"
        );
        theme.padding_standard += 8.0;
        assert_eq!(theme.name, original_name);
        assert_eq!(host.layout(rect, &theme, &mut text), original_height + 8.0);
        assert_eq!(
            host.widget().builds,
            2,
            "changed tokens rebuild the same widget"
        );
        host.layout(rect, &theme.clone(), &mut text);
        assert_eq!(host.widget().builds, 2);
    }
}
