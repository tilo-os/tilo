//! A retained widget placed by hand inside a widget that draws itself.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, WindowEvent};

use super::Widget;
use crate::widget::DrawChild;
use crate::widget::bound_nodes::BoundNodeMap;
use crate::{Theme, text_render::TextRenderer};

/// A widget laid out and driven by the widget that owns it.
pub struct Embedded<W: Widget> {
    widget: Box<W>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: Option<NodeId>,
    rect: Rect,
    transform: Affine,
    text_color: Option<Color>,
    laid: Option<(Rect, f64)>,
    focus_request: bool,
    fill: bool,
    /// Build-time theme inputs must match the paint-time theme even when the
    /// rectangle and document have not changed (for example, larger text).
    layout_theme: Option<Theme>,
    owner_viewport: Option<Rect>,
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
            owner_viewport: None,
        }
    }

    /// The child takes exactly the rectangle it is laid out in, rather than its content's
    /// height: a viewport of its own (a terminal, a viewer) that scrolls inside the
    /// rectangle the owner gives it.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Place the child's own space in the owner's: layout rectangles are in the child's
    /// space, and it is drawn, hit and its caret reported through `transform` (a slide's
    /// text box scaled onto the canvas).
    pub fn set_transform(&mut self, transform: Affine) {
        self.transform = transform;
    }

    /// Paint the child's text in `color` rather than the theme's (a text box coloured by
    /// its slide).
    pub fn set_text_color(&mut self, color: Option<Color>) {
        self.text_color = color;
    }

    fn recoloured(&self, theme: &Theme) -> Option<Theme> {
        self.text_color.map(|color| {
            let mut theme = theme.clone();
            theme.tokens.colors.text_primary = color;
            theme.text_primary = color;
            theme
        })
    }

    /// Where the child's caret is, in the owner's space, for the input method's candidate
    /// window.
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

    /// Where the widget was last laid out, at the height it wanted, in the owner's space.
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
            && !(self.widget.needs_redraw() || self.widget.needs_animation_frame())
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
    pub fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let recoloured = self.recoloured(theme);
        let theme = recoloured.as_ref().unwrap_or(theme);
        let owner = self.child_viewport(env.viewport);
        let mut child_env = env.clone();
        if self.transform.determinant().abs() > f64::EPSILON {
            child_env.viewport =
                owner.map(|viewport| self.transform.inverse().transform_rect_bbox(viewport));
        }
        self.widget
            .draw_child(scene, transform * self.transform, text, theme, &child_env);
        self.widget.mark_drawn();
    }

    fn child_viewport(&mut self, viewport: Option<Rect>) -> Option<Rect> {
        if viewport.is_some() {
            self.owner_viewport = viewport;
        }
        viewport
    }

    /// Paint what the widget floats over everything (its menus, a link's popover).
    pub fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        // What floats over everything (a menu, a popover) stands on its own
        // panel in the owner's inks: the words' own colour
        // ([`Self::set_text_color`]) is for the words, and dark words on a
        // light slide must never become a dark menu's labels.
        let owner = self.child_viewport(env.viewport);
        let mut child_env = env.clone();
        if self.transform.determinant().abs() > f64::EPSILON {
            child_env.viewport =
                owner.map(|viewport| self.transform.inverse().transform_rect_bbox(viewport));
        }
        let placed = self.overlay_transform().unwrap_or(self.transform);
        self.widget.draw_overlay(&mut crate::draw::DrawCtx::new(
            scene,
            transform * placed,
            text,
            theme,
            child_env,
        ));
    }

    fn overlay_transform(&self) -> Option<Affine> {
        let panel = self.widget.modal_capture()?;
        let corner = panel.origin();
        let offset = (self.transform * corner) - corner;
        let placed = panel + offset;
        let inside = |low: f64, high: f64, min: f64, max: f64| {
            if high - low > max - min || low < min {
                min - low
            } else if high > max {
                max - high
            } else {
                0.0
            }
        };
        let nudge = self
            .owner_viewport
            .map_or(vello::kurbo::Vec2::ZERO, |viewport| {
                vello::kurbo::Vec2::new(
                    inside(placed.x0, placed.x1, viewport.x0, viewport.x1),
                    inside(placed.y0, placed.y1, viewport.y0, viewport.y1),
                )
            });
        Some(Affine::translate(offset + nudge))
    }

    /// The panel the child has open over everything, in the owner's space
    /// ([`Widget::modal_capture`]): while there is one, the owner routes every pointer and
    /// key event to the child, which takes the panel's.
    pub fn modal_capture(&self) -> Option<Rect> {
        let panel = self.widget.modal_capture()?;
        Some(self.overlay_transform()?.transform_rect_bbox(panel))
    }

    fn input_transform(&self) -> Affine {
        self.overlay_transform().unwrap_or(self.transform)
    }

    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
        handled: &mut bool,
    ) {
        if self.root.is_none() {
            self.rebuild(&Theme::dark());
        }
        let Some(root) = self.root else {
            return;
        };
        let mapped;
        let event = match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } if self.input_transform() != Affine::IDENTITY => {
                mapped = WindowEvent::CursorMoved {
                    device_id: *device_id,
                    position: self.to_child(*position, scale_factor),
                };
                &mapped
            }
            WindowEvent::Touch(touch) if self.input_transform() != Affine::IDENTITY => {
                mapped = WindowEvent::Touch(winit::event::Touch {
                    location: self.to_child(touch.location, scale_factor),
                    ..*touch
                });
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
        if focus.is_some() {
            self.focus_request = true;
        }
    }

    fn to_child(
        &self,
        position: PhysicalPosition<f64>,
        scale_factor: f64,
    ) -> PhysicalPosition<f64> {
        let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
        let local = self.input_transform().inverse() * logical;
        PhysicalPosition::new(local.x * scale_factor, local.y * scale_factor)
    }

    /// Write what the child took up in the sync pass or left to do after
    /// its last event ([`Widget::settle`]): the formatting a keystroke
    /// closed, a toolbar's change made outside an input event. Call from
    /// the owner's own `settle`, which the host runs every frame; a child
    /// that is never settled writes those only with its next event.
    pub fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, path: &ObjId) {
        self.widget.settle(edits, path);
    }

    /// When the child next needs a sync with no input ([`Widget::next_wake_deadline`]): a
    /// spelling squiggle settling, the suggestion for the word under the caret.
    pub fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.widget.next_wake_deadline()
    }

    /// Whether the child is animating ([`Widget::needs_animation_frame`]).
    pub fn needs_animation_frame(&self) -> bool {
        self.widget.needs_animation_frame()
    }

    /// The pointer shape the child shows at `point`, in the owner's space (the text beam
    /// over a text box drawn scaled onto a canvas).
    pub fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.widget.cursor_shape(self.transform.inverse() * point)
    }

    /// What the child holds for the input method ([`Widget::ime_content`]).
    pub fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.widget.ime_content()
    }

    /// Whether the child takes line feeds as text ([`Widget::ime_multiline`]).
    pub fn ime_multiline(&self) -> bool {
        self.widget.ime_multiline()
    }

    /// Whether the child asked for keyboard focus (a press on a field)
    /// since the last call. The owner takes the host's focus in response
    /// (`*ctx.focus_target = Some(ctx.node_id)`) and keeps routing keys and
    /// the input method to the child.
    pub fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.focus_request)
    }

    /// The child's accessibility tree in the owner's space, for a canvas
    /// that draws the child to include among its own nodes: assistive
    /// technology then reads and edits the child as it would anywhere else.
    /// The child's own node is given its rectangle, since it has no widget
    /// of the owner's tree to take one from.
    pub fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        fn map(node: &mut crate::a11y::AccessNode, transform: Affine) {
            if let Some(bounds) = node.bounds {
                node.bounds = Some(transform.transform_rect_bbox(bounds.to_rect()).into());
            }
            for child in &mut node.children {
                map(child, transform);
            }
        }
        let mut node = self.widget.accessibility()?;
        node.bounds
            .get_or_insert_with(|| self.widget.get_rect().into());
        map(&mut node, self.transform);
        Some(node)
    }

    /// An assistive technology operates a node of [`Self::accessibility`]: `key_path` is
    /// relative to the child's own node ([`Widget::perform_action`]).
    pub fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        edits: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.widget.perform_action(action, key_path, edits, path)
    }

    /// An assistive technology operates a node of [`Self::accessibility`] that names its
    /// target ([`Widget::perform_semantic_action`]).
    pub fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        edits: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.widget
            .perform_semantic_action(target, action, edits, path)
    }

    /// Where the person is in the child (a caret, a selection), for the owner to publish to
    /// members as its own ([`crate::presence::capability::PublishesPresence`]).
    #[cfg(feature = "model")]
    pub fn presence_payload(
        &self,
    ) -> Option<(
        crate::presence::PresenceKey,
        ::collaboration::PresencePayload,
    )> {
        crate::presence::capability::presence_payload(&*self.widget)
    }

    /// What the person has selected in the child, for asking an agent about it
    /// ([`crate::presence::capability::PublishesPresence::shared_selection`]).
    #[cfg(feature = "model")]
    pub fn shared_selection(&self) -> Option<crate::shared_subject::SharedSelection> {
        crate::shared_selection::selection_of(&*self.widget)
    }

    /// Bring the widget up to date with the document.
    pub fn sync(&mut self, state: &AutoCommit, path: &ObjId) {
        self.widget.sync(state, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::TaffyTreeExt;
    use vello::Scene;

    struct PanelChild {
        panel: Rect,
        fill: Color,
        viewport: std::rc::Rc<std::cell::Cell<Option<Rect>>>,
    }

    impl Widget for PanelChild {
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _theme: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.leaf_node(taffy::Style {
                size: Size {
                    width: taffy::Dimension::length(200.0),
                    height: taffy::Dimension::length(100.0),
                },
                ..Default::default()
            });
            nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn name(&self) -> &str {
            "panel-child"
        }
        fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
            self.viewport.set(ctx.env.viewport);
        }
        fn modal_capture(&self) -> Option<Rect> {
            Some(self.panel)
        }
        fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
            self.viewport.set(ctx.env.viewport);
            let (scene, transform, _, _, _) = ctx.parts();
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                self.fill,
                None,
                &self.panel,
            );
        }
    }

    fn panel_child(fill: Color) -> Embedded<PanelChild> {
        Embedded::new(PanelChild {
            panel: Rect::new(30.0, 20.0, 110.0, 60.0),
            fill,
            viewport: std::rc::Rc::new(std::cell::Cell::new(None)),
        })
    }

    /// The box `fill` covers in `rgba`, far edges exclusive.
    #[allow(
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        reason = "pixel tallies inside the rendered frame"
    )]
    fn filled_bounds(rgba: &[u8], width: u32, height: u32, fill: Color) -> Rect {
        let ink = fill.to_rgba8();
        let (fr, fg, fb) = (ink.r, ink.g, ink.b);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0_u32, 0_u32);
        for y in 0..height {
            for x in 0..width {
                let at = ((y * width + x) * 4) as usize;
                let (r, g, b, a) = (rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]);
                if a == 0 || r.abs_diff(fr) > 4 || g.abs_diff(fg) > 4 || b.abs_diff(fb) > 4 {
                    continue;
                }
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        assert!(x0 <= x1, "the panel paints");
        Rect::new(
            f64::from(x0),
            f64::from(y0),
            f64::from(x1.saturating_add(1)),
            f64::from(y1.saturating_add(1)),
        )
    }

    #[test]
    fn an_embedded_widgets_panel_stands_where_its_owner_places_it() {
        let fill = Color::from_rgb8(0xff, 0x40, 0x00);
        let mut host = panel_child(fill);
        let mut text = crate::font_catalog::fixture_renderer();
        let theme = Theme::dark();
        host.layout(Rect::new(0.0, 0.0, 200.0, 100.0), &theme, &mut text);
        host.set_transform(Affine::translate((40.0, 25.0)));
        let mut scene = Scene::new();
        host.draw_overlay(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        let (width, height) = (240_u32, 140_u32);
        let mut renderer = crate::offscreen::OffscreenRenderer::new(width, height);
        let rgba = renderer.render(&scene, vello::peniko::Color::TRANSPARENT);
        let bounds = filled_bounds(&rgba, width, height, fill);
        let panel = host.widget().panel;
        let expected = panel + vello::kurbo::Vec2::new(40.0, 25.0);
        assert!(
            (bounds.x0 - expected.x0).abs() < 1.5
                && (bounds.y0 - expected.y0).abs() < 1.5
                && (bounds.x1 - expected.x1).abs() < 1.5
                && (bounds.y1 - expected.y1).abs() < 1.5,
            "the panel stands where its panel is placed, {expected:?}, at {bounds:?}"
        );
    }

    #[test]
    fn an_embedded_widget_scaled_to_nothing_keeps_its_owners_viewport() {
        let mut host = panel_child(Color::BLACK);
        let mut text = crate::font_catalog::fixture_renderer();
        let theme = Theme::dark();
        host.layout(Rect::new(0.0, 0.0, 200.0, 100.0), &theme, &mut text);
        host.set_transform(Affine::scale(0.0));
        let owner = Rect::new(0.0, 0.0, 640.0, 400.0);
        let presence = crate::presence::PresenceCtx::empty();
        let mut env = presence.draw_env();
        env.viewport = Some(owner);
        let seen = host.widget().viewport.clone();
        let mut scene = Scene::new();
        host.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            env.clone(),
        ));
        assert_eq!(
            seen.get(),
            Some(owner),
            "a widget scaled to nothing draws with its owner's viewport, not an inverse of a singular one"
        );
        let mut scene = Scene::new();
        host.draw_overlay(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            env,
        ));
        assert_eq!(seen.get(), Some(owner), "and so does its overlay");
    }
}
