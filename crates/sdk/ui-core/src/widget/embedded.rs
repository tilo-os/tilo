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
    /// The owner's viewport as its last draw saw it, in the owner's space:
    /// what a panel the child opens is kept inside.
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
            // Nothing changed size, but what is about to be drawn is placed
            // in `apply_layout` (a hovered handle, an open menu): place it
            // again from the solved tree, as a live host does every frame.
            if self.widget.needs_redraw()
                && let Some(root) = self.root
            {
                self.widget
                    .apply_layout(&self.tree, root, Point::new(rect.x0, rect.y0));
            }
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
        let owner = self.child_viewport(presence);
        self.widget
            .draw(scene, transform * self.transform, text, theme, presence);
        presence.viewport.set(owner);
        self.widget.mark_drawn();
    }

    /// Note the owner's viewport, and show it to the child in the child's
    /// own space for the child's draw: a panel the child places (a menu, a
    /// picker) is kept inside what the owner shows. Returns the owner's, to
    /// put back after.
    fn child_viewport(&mut self, presence: &PresenceCtx) -> Option<Rect> {
        let owner = presence.viewport.get();
        if owner.is_some() {
            self.owner_viewport = owner;
        }
        if self.transform.determinant().abs() > f64::EPSILON {
            presence
                .viewport
                .set(owner.map(|viewport| self.transform.inverse().transform_rect_bbox(viewport)));
        }
        owner
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
        // What floats over everything (a menu, a popover) stands on its own
        // panel in the owner's inks: the words' own colour
        // ([`Self::set_text_color`]) is for the words, and dark words on a
        // light slide must never become a dark menu's labels.
        let owner = self.child_viewport(presence);
        let placed = self.overlay_transform().unwrap_or(self.transform);
        self.widget
            .draw_overlay(scene, transform * placed, text, theme, presence);
        presence.viewport.set(owner);
    }

    /// While the child has a panel open over everything (a menu,
    /// [`Widget::modal_capture`]), what places the child's space for it: the
    /// panel stands where the child drawn under [`Self::set_transform`] puts
    /// its corner, at the owner's own scale, so a menu opened on words drawn
    /// small on a canvas reads and is hit as a menu anywhere else does; and
    /// moved as little as keeps it inside the owner's viewport, as a menu
    /// near a window's edge is.
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
    /// ([`Widget::modal_capture`]): while there is one, the owner routes
    /// every pointer and key event to the child, which takes the panel's.
    pub fn modal_capture(&self) -> Option<Rect> {
        let panel = self.widget.modal_capture()?;
        Some(self.overlay_transform()?.transform_rect_bbox(panel))
    }

    /// From the child's space to the owner's for pointer input and what is
    /// drawn over everything: the panel's placement while one is open,
    /// else the child's transform.
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
        // The owner holds the window's focus for its embedded child: a press
        // that focused something inside is the owner's cue to take it.
        if focus.is_some() {
            self.focus_request = true;
        }
    }

    /// A physical position in the owner's space, in the child's.
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

    /// When the child next needs a sync with no input
    /// ([`Widget::next_wake_deadline`]): a spelling squiggle settling, the
    /// suggestion for the word under the caret. The owner reports it as its
    /// own.
    pub fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.widget.next_wake_deadline()
    }

    /// Whether the child is animating ([`Widget::needs_animation_frame`]).
    pub fn needs_animation_frame(&self) -> bool {
        self.widget.needs_animation_frame()
    }

    /// The pointer shape the child shows at `point`, in the owner's space
    /// (the text beam over a text box drawn scaled onto a canvas).
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

    /// An assistive technology operates a node of [`Self::accessibility`]:
    /// `key_path` is relative to the child's own node
    /// ([`Widget::perform_action`]).
    pub fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        edits: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.widget.perform_action(action, key_path, edits, path)
    }

    /// An assistive technology operates a node of [`Self::accessibility`]
    /// that names its target ([`Widget::perform_semantic_action`]).
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

    /// Where the person is in the child (a caret, a selection), for the
    /// owner to publish to collaborators as its own
    /// ([`crate::presence::capability::PublishesPresence`]).
    pub fn presence_payload(
        &self,
    ) -> Option<(
        crate::presence::PresenceKey,
        crate::collaboration::PresencePayload,
    )> {
        crate::presence::capability::presence_payload(&*self.widget)
    }

    /// What the person has selected in the child, for asking an agent about
    /// it ([`crate::presence::capability::PublishesPresence::shared_selection`]).
    pub fn shared_selection(&self) -> Option<crate::shared_selection::SharedSelection> {
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
    fn a_menu_opened_on_small_words_near_an_edge_stays_inside_the_owner() {
        let theme = Theme::dark();
        let mut text = crate::font_catalog::fixture_renderer();
        let mut host = Embedded::new(BlockEditor::new().with_wrap_lines(true));
        host.widget_mut()
            .present_blocks(&[BlockContent::paragraph("a", "words at the edge")]);
        // Laid out 400 wide and drawn at half size against the right and
        // bottom edges of an 800 x 600 owner.
        host.set_transform(Affine::translate((600.0, 570.0)) * Affine::scale(0.5));
        host.layout(Rect::new(0.0, 0.0, 400.0, 0.0), &theme, &mut text);
        let owner = Rect::new(0.0, 0.0, 800.0, 600.0);
        let presence = PresenceCtx::empty();
        presence.viewport.set(Some(owner));
        host.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &presence,
        );
        assert_eq!(
            presence.viewport.get(),
            Some(owner),
            "the owner's viewport is put back"
        );
        assert!(host.widget_mut().open_text_menu(Point::new(380.0, 10.0)));
        host.draw_overlay(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &presence,
        );
        let menu = host.modal_capture().expect("the menu is open");
        assert!(
            menu.x0 >= owner.x0 - 1e-6
                && menu.x1 <= owner.x1 + 1e-6
                && menu.y0 >= owner.y0 - 1e-6
                && menu.y1 <= owner.y1 + 1e-6,
            "{menu:?} inside {owner:?}"
        );
        assert!(
            menu.width() > 100.0,
            "drawn at the owner's scale, not the words': {menu:?}"
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
