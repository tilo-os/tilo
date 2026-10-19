//! `ResizableSplit` — a two-pane container with a draggable divider — and
//! the divider itself, [`SplitHandle`], for hosts that paint their panes.
//!
//! The divider is a hairline at rest with a wider target around it; it
//! thickens under the pointer and while dragged. Dragging it, or the arrow
//! keys while it holds keyboard focus, moves the [`SplitRatio`], clamped so
//! neither pane collapses. Assistive technology meets it as a separator
//! whose value is the first pane's share.
//!
//! State lives on the widget rather than the CRDT: the ratio is a
//! per-instance visual tuning, not replicated content.

use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// The least share either pane keeps, so neither is lost off-screen.
const SPLIT_MIN: f32 = 0.1;
/// See [`SPLIT_MIN`].
const SPLIT_MAX: f32 = 0.9;
const SPLIT_DEFAULT: f32 = 0.5;
/// How far one arrow key moves the divider.
const KEY_STEP: f32 = 0.05;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitOrientation {
    /// Panes sit side-by-side; divider is a vertical strip.
    Horizontal,
    /// Panes stack vertically; divider is a horizontal strip.
    Vertical,
}

/// The share of a two-pane split the first pane takes, clamped to bounds
/// so neither pane collapses. One model for [`ResizableSplit`] and for
/// painted hosts ([`SplitHandle`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitRatio {
    position: f32,
    min: f32,
    max: f32,
}

impl Default for SplitRatio {
    fn default() -> Self {
        Self::new(SPLIT_DEFAULT)
    }
}

impl SplitRatio {
    /// A split with the first pane at `position` of the extent (`0..1`),
    /// within the default 10–90 % bounds.
    #[must_use]
    pub fn new(position: f32) -> Self {
        Self {
            position: position.clamp(SPLIT_MIN, SPLIT_MAX),
            min: SPLIT_MIN,
            max: SPLIT_MAX,
        }
    }

    /// Narrow the range the divider may move in.
    #[must_use]
    pub fn with_bounds(mut self, min: f32, max: f32) -> Self {
        self.min = min.clamp(0.0, 1.0);
        self.max = max.clamp(self.min, 1.0);
        self.position = self.position.clamp(self.min, self.max);
        self
    }

    /// The first pane's share.
    #[must_use]
    pub fn position(&self) -> f32 {
        self.position
    }

    /// Move to `position` (clamped); returns whether it moved.
    pub fn set(&mut self, position: f32) -> bool {
        let next = position.clamp(self.min, self.max);
        let moved = (next - self.position).abs() > f32::EPSILON;
        self.position = next;
        moved
    }

    /// Move by `delta` (clamped); returns whether it moved.
    pub fn step(&mut self, delta: f32) -> bool {
        self.set(self.position + delta)
    }

    /// Follow a pointer at `along` on an extent from `start` that is
    /// `extent` long; returns whether it moved.
    pub fn follow(&mut self, along: f64, start: f64, extent: f64) -> bool {
        self.set(crate::num::f32_of_f64((along - start) / extent.max(1.0)))
    }

    /// The first pane's length on an extent `total` long.
    #[must_use]
    pub fn first_extent(&self, total: f64) -> f64 {
        total * f64::from(self.position)
    }

    /// The share in whole percent, for assistive technology.
    #[must_use]
    pub fn percent(&self) -> u32 {
        // A clamped 0..1 share: the product is 0..=100.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a share in 0..=1 times 100 rounds into 0..=100"
        )]
        let percent = (self.position * 100.0).round() as u32;
        percent
    }
}

/// The divider between two panes: hit-testing, hover, dragging, keyboard
/// steps, painting and its accessible separator node. [`ResizableSplit`]
/// keeps one; a host that paints its panes itself (a query editor over its
/// results, a source beside its preview) keeps one too, sets its rect each
/// layout and lays its panes out in [`SplitHandle::pane_rects`].
#[derive(Clone, Debug)]
pub struct SplitHandle {
    orientation: SplitOrientation,
    ratio: SplitRatio,
    rect: Rect,
    divider: Rect,
    hit_half: f64,
    hover: bool,
    /// `Some(offset)` while dragging: cursor distance from the divider's
    /// centre at press time, so moves translate without snapping.
    drag: Option<f64>,
    focused: bool,
    cursor: Point,
    label: String,
    test_id: Option<String>,
    layout_dirty: bool,
}

impl SplitHandle {
    #[must_use]
    pub fn new(orientation: SplitOrientation, ratio: SplitRatio) -> Self {
        Self {
            orientation,
            ratio,
            rect: Rect::ZERO,
            divider: Rect::ZERO,
            hit_half: 4.0,
            hover: false,
            drag: None,
            focused: false,
            cursor: Point::ZERO,
            label: "Resize panes".to_string(),
            test_id: None,
            layout_dirty: false,
        }
    }

    /// What assistive technology calls the divider ("Resize sidebar").
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn ratio(&self) -> SplitRatio {
        self.ratio
    }

    pub fn ratio_mut(&mut self) -> &mut SplitRatio {
        &mut self.ratio
    }

    /// The whole split, divider included, for this layout; the hit area
    /// follows `rhythm.tight` on each side of the hairline.
    pub fn set_rect(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        self.hit_half = f64::from(theme.tokens.spacing.rhythm.tight);
        self.place();
    }

    /// Move the split to `rect`, keeping the hit area from the last
    /// [`Self::set_rect`] (`rhythm.tight` each side by default), for a host
    /// that lays out without a theme in hand.
    pub fn set_bounds(&mut self, rect: Rect) {
        self.rect = rect;
        self.place();
    }

    fn place(&mut self) {
        let r = self.rect;
        self.divider = match self.orientation {
            SplitOrientation::Horizontal => {
                let x = (r.x0 + self.ratio.first_extent(r.width())).round();
                Rect::new(x - self.hit_half, r.y0, x + self.hit_half, r.y1)
            }
            SplitOrientation::Vertical => {
                let y = (r.y0 + self.ratio.first_extent(r.height())).round();
                Rect::new(r.x0, y - self.hit_half, r.x1, y + self.hit_half)
            }
        };
    }

    /// Where the divider takes the pointer.
    #[must_use]
    pub fn hit_rect(&self) -> Rect {
        self.divider
    }

    /// The two panes, either side of the hairline.
    #[must_use]
    pub fn pane_rects(&self) -> (Rect, Rect) {
        let r = self.rect;
        match self.orientation {
            SplitOrientation::Horizontal => {
                let x = self.divider.center().x;
                (
                    Rect::new(r.x0, r.y0, x, r.y1),
                    Rect::new(x, r.y0, r.x1, r.y1),
                )
            }
            SplitOrientation::Vertical => {
                let y = self.divider.center().y;
                (
                    Rect::new(r.x0, r.y0, r.x1, y),
                    Rect::new(r.x0, y, r.x1, r.y1),
                )
            }
        }
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    #[must_use]
    pub fn is_hovered(&self) -> bool {
        self.hover
    }

    /// Follow the host's keyboard focus on the divider.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The resize cursor over the divider and throughout a drag.
    #[must_use]
    pub fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.drag.is_some() || self.divider.contains(point)).then_some(match self.orientation {
            SplitOrientation::Horizontal => winit::window::CursorIcon::EwResize,
            SplitOrientation::Vertical => winit::window::CursorIcon::NsResize,
        })
    }

    /// Hover, drag and keyboard steps. Marks the event handled when the
    /// divider took it and asks for keyboard focus on a press. Returns
    /// whether the ratio moved.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
                self.hover = self.divider.contains(self.cursor);
                if let Some(offset) = self.drag {
                    *ctx.handled = true;
                    let r = self.rect;
                    let moved = match self.orientation {
                        SplitOrientation::Horizontal => {
                            self.ratio.follow(self.cursor.x - offset, r.x0, r.width())
                        }
                        SplitOrientation::Vertical => {
                            self.ratio.follow(self.cursor.y - offset, r.y0, r.height())
                        }
                    };
                    self.place();
                    return moved;
                }
                false
            }
            WindowEvent::CursorLeft { .. } => {
                self.hover = false;
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.divider.contains(self.cursor) => {
                let centre = self.divider.center();
                self.drag = Some(match self.orientation {
                    SplitOrientation::Horizontal => self.cursor.x - centre.x,
                    SplitOrientation::Vertical => self.cursor.y - centre.y,
                });
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.drag.is_some() => {
                self.drag = None;
                *ctx.handled = true;
                false
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let (back, forward) = match self.orientation {
                    SplitOrientation::Horizontal => (NamedKey::ArrowLeft, NamedKey::ArrowRight),
                    SplitOrientation::Vertical => (NamedKey::ArrowUp, NamedKey::ArrowDown),
                };
                let moved = match &event.logical_key {
                    Key::Named(key) if *key == back => self.ratio.step(-KEY_STEP),
                    Key::Named(key) if *key == forward => self.ratio.step(KEY_STEP),
                    Key::Named(NamedKey::Home) => self.ratio.set(0.0),
                    Key::Named(NamedKey::End) => self.ratio.set(1.0),
                    _ => return false,
                };
                *ctx.handled = true;
                self.place();
                moved
            }
            _ => false,
        }
    }

    /// Apply an assistive-technology step; returns whether it moved.
    pub fn perform_action(&mut self, action: &crate::a11y::Action) -> bool {
        let moved = match action {
            crate::a11y::Action::Increment => self.ratio.step(KEY_STEP),
            crate::a11y::Action::Decrement => self.ratio.step(-KEY_STEP),
            crate::a11y::Action::SetValue(value) => value
                .trim_end_matches('%')
                .trim()
                .parse::<f32>()
                .is_ok_and(|percent| percent.is_finite() && self.ratio.set(percent / 100.0)),
            _ => return false,
        };
        self.place();
        moved
    }

    /// Paint the divider: a hairline derived from `ground` at rest, the
    /// inverted fill (thicker) under the pointer, while dragged and while
    /// focused (with the focus ring).
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        ground: vello::peniko::Color,
        theme: &Theme,
    ) {
        let active = self.hover || self.drag.is_some() || self.focused;
        let centre = self.divider.center();
        let half = if active { 1.5 } else { 0.5 };
        let line = match self.orientation {
            SplitOrientation::Horizontal => {
                let x = centre.x.round();
                Rect::new(x - half, self.divider.y0, x + half, self.divider.y1)
            }
            SplitOrientation::Vertical => {
                let y = centre.y.round();
                Rect::new(self.divider.x0, y - half, self.divider.x1, y + half)
            }
        };
        let colour = if active {
            theme.tokens.colors.intent.neutral_fill
        } else {
            crate::surface::edge_on(ground)
        };
        scene.fill(vello::peniko::Fill::NonZero, transform, colour, None, &line);
        if self.focused {
            crate::widget::paint_focus_ring(scene, transform, self.divider, 2.0, theme);
        }
    }

    /// The divider as a separator node with the first pane's share as its
    /// value.
    pub fn accessibility(&self) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        AccessNode::new(Role::Separator)
            .with_name(self.label.clone())
            .with_value(format!("{}%", self.ratio.percent()))
            .with_state(AccessState::new().focusable(true).focused(self.focused))
    }
}

impl SplitHandle {
    fn layout_style(&self) -> Style {
        let mut style = Style {
            position: taffy::Position::Absolute,
            ..Style::default()
        };
        match self.orientation {
            SplitOrientation::Horizontal => {
                style.size.width = length((self.hit_half * 2.0) as f32);
                style.size.height = percent(1.0_f32);
                style.inset.left = percent(self.ratio.position());
                style.inset.top = length(0.0_f32);
                style.margin.left = length(-self.hit_half as f32);
            }
            SplitOrientation::Vertical => {
                style.size.width = percent(1.0_f32);
                style.size.height = length((self.hit_half * 2.0) as f32);
                style.inset.left = length(0.0_f32);
                style.inset.top = percent(self.ratio.position());
                style.margin.top = length(-self.hit_half as f32);
            }
        }
        style
    }
}

impl Widget for SplitHandle {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(self.layout_style(), node_map, self)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let style = self.layout_style();
        if taffy.style(node).is_ok_and(|current| *current != style) {
            return taffy.set_style(node, style).is_ok();
        }
        false
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(
            scene,
            transform,
            theme.tokens.colors.surfaces.content,
            theme,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.layout_dirty |= SplitHandle::handle_event(self, event, ctx);
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let moved = SplitHandle::perform_action(self, action);
        self.layout_dirty |= moved;
        moved
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.set_focused(focused);
    }
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging()
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        SplitHandle::cursor_shape(self, point)
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = SplitHandle::accessibility(self);
        if let Some(id) = &self.test_id {
            node = node.with_action_target(id);
        }
        Some(node)
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        if self.test_id.as_deref() != Some(target) {
            return false;
        }
        let moved = SplitHandle::perform_action(self, action);
        self.layout_dirty |= moved;
        moved
    }

    fn get_rect(&self) -> Rect {
        self.divider
    }
    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }
    fn name(&self) -> &'static str {
        "SplitHandle"
    }
}

/// Two panes and the divider between them.
pub struct ResizableSplit<L: Widget, R: Widget> {
    left: L,
    right: R,
    orientation: SplitOrientation,
    handle: SplitHandle,
    /// The first pane takes its content's size until the divider is moved.
    content_sized_first: bool,
    /// The first pane's declared axis size, before ratio layout overrides it.
    first_content_size: taffy::Dimension,
    /// The ratio moved since the panes were last laid out.
    layout_dirty: bool,
    rect: Rect,
}

impl<L: Widget, R: Widget> ResizableSplit<L, R> {
    pub fn new(left: L, right: R, orientation: SplitOrientation) -> Self {
        Self {
            left,
            right,
            orientation,
            handle: SplitHandle::new(orientation, SplitRatio::default()),
            content_sized_first: false,
            first_content_size: taffy::Dimension::auto(),
            layout_dirty: false,
            rect: Rect::ZERO,
        }
    }

    /// Clamped to the ratio's bounds so a caller can't construct a split
    /// where one pane is invisible from the start.
    #[must_use]
    pub fn with_split_position(mut self, position: f32) -> Self {
        self.handle.ratio_mut().set(position);
        self
    }

    /// Bound how far the divider moves (default 10–90 %).
    #[must_use]
    pub fn with_bounds(mut self, min: f32, max: f32) -> Self {
        let ratio = self.handle.ratio().with_bounds(min, max);
        *self.handle.ratio_mut() = ratio;
        self
    }

    /// The first pane takes its content's size (a toolbar-like editor over
    /// a results table) until the person moves the divider.
    #[must_use]
    pub fn with_content_sized_first(mut self) -> Self {
        self.content_sized_first = true;
        self
    }

    /// What assistive technology calls the divider.
    #[must_use]
    pub fn with_divider_label(mut self, label: impl Into<String>) -> Self {
        self.handle = self.handle.with_label(label);
        self
    }

    /// Give the actual divider a stable identity for accessibility and input tests.
    #[must_use]
    pub fn with_divider_id(mut self, id: impl Into<String>) -> Self {
        self.handle.test_id = Some(id.into());
        self
    }

    pub fn split_position(&self) -> f32 {
        self.handle.ratio().position()
    }

    /// Move the divider (clamped).
    pub fn set_split_position(&mut self, position: f32) {
        if self.handle.ratio_mut().set(position) {
            self.content_sized_first = false;
            self.layout_dirty = true;
        }
    }

    /// Restore the first pane's natural content size after manual resizing.
    pub fn reset_to_content_size(&mut self) {
        self.content_sized_first = true;
        self.layout_dirty = true;
        self.handle.layout_dirty = false;
    }

    /// The first (leading or top) pane.
    pub fn left(&self) -> &L {
        &self.left
    }

    /// The second (trailing or bottom) pane.
    pub fn right(&self) -> &R {
        &self.right
    }

    fn moved(&mut self) {
        self.content_sized_first = false;
        self.layout_dirty = true;
    }

    /// Grow factors for the two panes: the ratio, or the first pane at its
    /// content size.
    fn grows(&self) -> (f32, f32, bool) {
        if self.content_sized_first {
            (0.0, 1.0, true)
        } else {
            let s = self.handle.ratio().position();
            (s, 1.0 - s, false)
        }
    }

    /// Not consumed by the divider — forward to whichever pane the cursor is
    /// over (positional events), or to both in turn until one claims
    /// `handled` (non-positional events; same convention `Row`/`Column` use).
    fn forward_to_children(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let left_node = children.first().copied();
        let right_node = children.get(1).copied();

        let pointer_event = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        // The capturing child owns motion and release even outside its pane.
        // The divider has already had its own capture handled first.
        if pointer_event {
            if self.left.wants_pointer_capture()
                && let Some(node) = left_node
            {
                self.left
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
                return;
            }
            if self.right.wants_pointer_capture()
                && let Some(node) = right_node
            {
                self.right
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
                return;
            }
        }
        let cursor_pt = match event {
            WindowEvent::CursorMoved { position, .. } => Some(ctx.logical(position)),
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => {
                Some(self.handle.cursor)
            }
            WindowEvent::Touch(touch) => Some(ctx.logical(touch.location)),
            _ => None,
        };

        let left_rect = self.left.get_rect();
        let right_rect = self.right.get_rect();
        if let Some(pt) = cursor_pt {
            if let WindowEvent::CursorMoved { device_id, .. } = event {
                self.clear_inactive_hover(pt, *device_id, ctx);
            }
            if left_rect.contains(pt) {
                if let Some(n) = left_node {
                    self.left
                        .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                }
            } else if right_rect.contains(pt)
                && let Some(n) = right_node
            {
                self.right
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return;
        }

        if let Some(n) = left_node {
            self.left
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return;
            }
        }
        if let Some(n) = right_node {
            self.right
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
    }

    fn clear_inactive_hover(
        &mut self,
        point: Point,
        device_id: winit::event::DeviceId,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        // Boundary notifications clear stale sibling hover without claiming
        // the original move. Captured motion never enters this path.
        let leave = WindowEvent::CursorLeft { device_id };
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let handled = *ctx.handled;
        if !self.left.get_rect().contains(point)
            && let Some(&node) = children.first()
        {
            self.left
                .handle_event(&leave, &mut ctx.reborrow(ctx.path, node));
        }
        if !self.right.get_rect().contains(point)
            && let Some(&node) = children.get(1)
        {
            self.right
                .handle_event(&leave, &mut ctx.reborrow(ctx.path, node));
        }
        *ctx.handled = handled;
    }

    fn style_panes(
        &self,
        taffy: &mut TaffyTree<()>,
        left_node: NodeId,
        right_node: NodeId,
    ) -> bool {
        let (left_grow, right_grow, content_sized) = self.grows();
        let cross = match self.orientation {
            SplitOrientation::Horizontal => taffy::Size {
                width: taffy::Dimension::auto(),
                height: percent(1.0_f32),
            },
            SplitOrientation::Vertical => taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
        };
        let mut changed = false;
        for (node, grow, sized_by_content) in [
            (left_node, left_grow, content_sized),
            (right_node, right_grow, false),
        ] {
            if let Ok(current) = taffy.style(node) {
                let mut style = current.clone();
                // A zero basis divides the available axis by the two grow
                // factors, avoiding percentage sizing's circular
                // intrinsic-width calculation. A content-sized pane keeps
                // its own size along the split.
                if sized_by_content {
                    match self.orientation {
                        SplitOrientation::Horizontal => {
                            style.size.width = self.first_content_size;
                            style.size.height = cross.height;
                        }
                        SplitOrientation::Vertical => {
                            style.size.width = cross.width;
                            style.size.height = self.first_content_size;
                        }
                    }
                    style.flex_basis = taffy::Dimension::auto();
                } else {
                    style.size = cross;
                    style.flex_basis = length(0.0_f32);
                }
                style.flex_grow = grow;
                style.flex_shrink = 1.0;
                if *current != style {
                    changed |= taffy.set_style(node, style).is_ok();
                }
            }
        }
        changed
    }
}

impl<L: Widget + 'static, R: Widget + 'static> Widget for ResizableSplit<L, R> {
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    /// Forward the per-frame update pass to both panes, so a pane that
    /// polls a model (a data table, a pending image) keeps updating.
    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let left = self.left.update(state, path);
        let right = self.right.update(state, path);
        left || right
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.left.sync(state, path);
        self.right.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let left_node = self.left.build_layout(taffy, theme, node_map);
        let right_node = self.right.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(left_node) {
            self.first_content_size = match self.orientation {
                SplitOrientation::Horizontal => style.size.width,
                SplitOrientation::Vertical => style.size.height,
            };
        }
        self.handle.set_rect(self.rect, theme);
        let mut children = vec![left_node, right_node];
        if self.handle.test_id.is_some() {
            children.push(self.handle.build_layout(taffy, theme, node_map));
        }
        self.style_panes(taffy, left_node, right_node);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: match self.orientation {
                SplitOrientation::Horizontal => taffy::FlexDirection::Row,
                SplitOrientation::Vertical => taffy::FlexDirection::Column,
            },
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    /// The panes follow a moved divider without rebuilding the tree.
    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let Ok(children) = taffy.children(node) else {
            return false;
        };
        let (Some(&left), Some(&right)) = (children.first(), children.get(1)) else {
            return false;
        };
        if std::mem::take(&mut self.handle.layout_dirty) {
            self.moved();
        }
        let mut changed = self.style_panes(taffy, left, right);
        if let Some(&divider) = children.get(2) {
            changed |= self.handle.adjust_layout_style(taffy, divider, text);
        }
        changed
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self.handle.layout_dirty
            || self.left.is_layout_dirty()
            || self.right.is_layout_dirty()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let children = taffy.children(node).unwrap_or_default();
        // The Taffy children deliberately match `Widget::children`.
        if let Some(&n) = children.first() {
            self.left.apply_layout(taffy, n, new_origin);
        }
        if let Some(&n) = children.get(1) {
            self.right.apply_layout(taffy, n, new_origin);
        }
        let divider_followed_content = self.content_sized_first && {
            // The divider sits where the content-sized pane ends.
            let first = self.left.get_rect();
            let (along, extent) = match self.orientation {
                SplitOrientation::Horizontal => (first.x1, self.rect.width()),
                SplitOrientation::Vertical => (first.y1, self.rect.height()),
            };
            let start = match self.orientation {
                SplitOrientation::Horizontal => self.rect.x0,
                SplitOrientation::Vertical => self.rect.y0,
            };
            self.handle.ratio_mut().follow(along, start, extent)
        };
        self.layout_dirty = divider_followed_content && self.handle.test_id.is_some();
        self.handle.set_bounds(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Children first — the divider paints on top.
        use crate::widget::draw_child;
        draw_child(
            &mut self.left,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        draw_child(
            &mut self.right,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        self.handle.paint(
            scene,
            transform,
            theme.tokens.colors.surfaces.content,
            theme,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let was_dragging = self.handle.is_dragging();
        let divider = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|children| children.get(2).copied());
        let moved = if let Some(node) = divider {
            self.handle
                .handle_event(event, &mut ctx.reborrow(ctx.path, node))
        } else {
            self.handle.handle_event(event, ctx)
        };
        if moved {
            self.moved();
        }

        if *ctx.handled || was_dragging {
            return;
        }
        self.forward_to_children(event, ctx);
    }

    fn is_focusable(&self) -> bool {
        self.handle.test_id.is_none()
    }

    fn set_focus(&mut self, focused: bool) {
        self.handle.set_focused(focused);
    }

    fn is_focused(&self) -> bool {
        self.handle.focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        if let Some(id) = &self.handle.test_id {
            Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_semantic_key(format!("resizable-split:{id}")),
            )
        } else {
            Some(self.handle.accessibility())
        }
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let moved = self.handle.perform_action(action);
        if moved {
            self.moved();
        }
        moved
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.left, &self.right];
        if self.handle.test_id.is_some() {
            children.push(&self.handle);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.left);
        visitor(&self.right);
        if self.handle.test_id.is_some() {
            visitor(&self.handle);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let named = self.handle.test_id.is_some();
        let mut children: Vec<&mut dyn Widget> = vec![&mut self.left, &mut self.right];
        if named {
            children.push(&mut self.handle);
        }
        children
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// East-west over a horizontal divider, north-south over a vertical
    /// one; kept while dragging regardless of where the cursor strays.
    /// Off the divider, `None` lets the pane under the point claim its
    /// own cursor via the host's `node_map` sweep.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.handle.cursor_shape(point)
    }

    fn name(&self) -> &'static str {
        "ResizableSplit"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let l = self.left.take_structure_dirty();
        let r = self.right.take_structure_dirty();
        l || r
    }

    fn wants_pointer_capture(&self) -> bool {
        // Without this, straying outside the strip mid-drag routes the
        // release to a sibling and the drag silently ends early.
        self.handle.is_dragging()
            || self.left.wants_pointer_capture()
            || self.right.wants_pointer_capture()
    }
}

impl<L: Widget + 'static, R: Widget + 'static> crate::presence::capability::PublishesPresence
    for ResizableSplit<L, R>
{
}

impl<L: Widget + 'static, R: Widget + 'static> crate::permissions::capability::PermissionGated
    for ResizableSplit<L, R>
{
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use automerge::AutoCommit;

    /// Bare-minimum `Widget` for use as a `ResizableSplit` child in tests.
    struct TestLeaf {
        rect: Rect,
        updates: std::rc::Rc<std::cell::Cell<u32>>,
        changes: bool,
        width: Option<f32>,
    }

    impl TestLeaf {
        fn new() -> Self {
            Self {
                rect: Rect::ZERO,
                updates: std::rc::Rc::default(),
                changes: true,
                width: None,
            }
        }
    }

    impl Widget for TestLeaf {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: taffy::Size {
                    width: self.width.map_or(taffy::Dimension::auto(), length),
                    height: taffy::Dimension::auto(),
                },
                ..Style::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        fn update(
            &mut self,
            _state: &crate::widget::AppModel,
            _path: &crate::widget::ModelPath,
        ) -> bool {
            self.updates.set(self.updates.get() + 1);
            self.changes
        }

        crate::rect_geometry!();

        fn name(&self) -> &'static str {
            "TestLeaf"
        }
    }

    /// Lay the split out at `width` × `height` and return the tree, so a
    /// test can re-solve it after an event as the host would.
    fn solve<L: Widget + 'static, R: Widget + 'static>(
        split: &mut ResizableSplit<L, R>,
        width: f32,
        height: f32,
    ) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = HashMap::new();
        let node = split.build_layout(&mut tree, &Theme::dark(), &mut node_map);
        relayout(split, &mut tree, node, width, height);
        (tree, node)
    }

    fn relayout<L: Widget + 'static, R: Widget + 'static>(
        split: &mut ResizableSplit<L, R>,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        width: f32,
        height: f32,
    ) {
        split.adjust_layout_style(tree, node, &mut crate::text_render::TextRenderer::new());
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
        )
        .expect("valid split layout");
        split.apply_layout(tree, node, Point::ZERO);
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    use crate::synthetic_input::mouse_release;

    fn dispatch<L: Widget + 'static, R: Widget + 'static>(
        s: &mut ResizableSplit<L, R>,
        tree: &TaffyTree<()>,
        node: NodeId,
        event: &WindowEvent,
    ) {
        Dispatch::new().layout(tree, node).send(s, event);
    }

    fn split(orientation: SplitOrientation) -> ResizableSplit<TestLeaf, TestLeaf> {
        ResizableSplit::new(TestLeaf::new(), TestLeaf::new(), orientation)
    }

    fn hosted_segment_bounds(
        harness: &crate::headless::FrameHarness,
        left: bool,
        index: usize,
    ) -> Rect {
        let split = harness.root::<ResizableSplit<crate::widget::Column, crate::widget::Column>>();
        let pane = if left { &split.left } else { &split.right };
        pane.children()[0].accessibility().unwrap().children[index]
            .bounds
            .expect("the actual retained segment publishes bounds")
            .to_rect()
    }

    #[test]
    fn hosted_pointer_moves_between_panes_without_activating_stale_hover() {
        use crate::widget::{Column, Segment, ToggleGroup};
        let left = std::rc::Rc::new(std::cell::Cell::new(0));
        let right = std::rc::Rc::new(std::cell::Cell::new(0));
        let left_changed = left.clone();
        let right_changed = right.clone();
        let split = ResizableSplit::new(
            Column::new().with_no_padding().push(Box::new(
                ToggleGroup::new(
                    vec![Segment::new("Left first"), Segment::new("Left second")],
                    0,
                )
                .on_view("left", move |index| left_changed.set(index)),
            )),
            Column::new().with_no_padding().push(Box::new(
                ToggleGroup::new(
                    vec![Segment::new("Right first"), Segment::new("Right second")],
                    0,
                )
                .on_view("right", move |index| right_changed.set(index)),
            )),
            SplitOrientation::Vertical,
        )
        .with_divider_id("pointer-divider");
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            split,
            (400, 300),
            Theme::dark(),
        );
        for left in [true, false] {
            let rect = hosted_segment_bounds(&harness, left, 1);
            assert!(rect.width() > 0.0 && rect.height() > 0.0);
            harness.click_at(rect.center().x, rect.center().y);
        }
        assert_eq!(left.get(), 1);
        assert_eq!(right.get(), 1, "the right pane receives its physical press");
    }

    #[test]
    fn hosted_child_drag_keeps_capture_and_releases_outside_its_pane() {
        use crate::widget::{Column, Segment, TextInput, ToggleGroup};
        struct InspectInput(TextInput);
        impl crate::widget::Decorator for InspectInput {
            type Inner = TextInput;
            fn inner(&self) -> &TextInput {
                &self.0
            }
            fn inner_mut(&mut self) -> &mut TextInput {
                &mut self.0
            }
            fn as_any(&self) -> Option<&dyn std::any::Any> {
                Some(self)
            }
        }
        type Split = ResizableSplit<Column, Column>;
        let split = ResizableSplit::new(
            Column::new().with_no_padding().push(Box::new(InspectInput(
                TextInput::local().with_id("drag-text"),
            ))),
            Column::new().with_no_padding().push(Box::new(
                ToggleGroup::new(
                    vec![Segment::new("Other first"), Segment::new("Other second")],
                    0,
                )
                .on_view("other", |_| {}),
            )),
            SplitOrientation::Horizontal,
        )
        .with_divider_id("capture-divider");
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            split,
            (600, 200),
            Theme::dark(),
        );
        harness
            .perform_named(
                "drag-text",
                &crate::a11y::Action::SetValue("select this text".into()),
            )
            .unwrap();
        let input = &harness.root::<Split>().left.children()[0]
            .as_any()
            .unwrap()
            .downcast_ref::<InspectInput>()
            .unwrap()
            .0;
        assert_eq!(
            input.value(),
            "select this text",
            "the hosted subject contains real text before dragging"
        );
        let text = harness.targets()["drag-text"];
        assert!(text.width() > 0.0 && text.height() > 0.0);
        harness.drag_at((text.x0 + 20.0, text.center().y), (590.0, text.center().y));
        let input = &harness.root::<Split>().left.children()[0]
            .as_any()
            .unwrap()
            .downcast_ref::<InspectInput>()
            .unwrap()
            .0;
        let (start, end) = input
            .selection_range_public()
            .expect("actual drag selects text");
        assert!(start < end);
        assert_eq!(end, "select this text".len());
        assert!(
            !input.wants_pointer_capture(),
            "release reaches the capturing text field"
        );
        let bounds = hosted_segment_bounds(&harness, false, 1);
        harness.click_at(bounds.center().x, bounds.center().y);
        assert_eq!(
            harness.root::<Split>().right.children()[0]
                .accessibility()
                .unwrap()
                .value
                .as_deref(),
            Some("Other second")
        );
    }

    #[test]
    fn named_divider_targets_the_separator_and_resizes_through_host_input() {
        for orientation in [SplitOrientation::Horizontal, SplitOrientation::Vertical] {
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                split(orientation).with_divider_id("request-divider"),
                (400, 300),
                Theme::dark(),
            );
            let divider = harness.targets()["request-divider"];
            let along = match orientation {
                SplitOrientation::Horizontal => {
                    assert!(divider.width() < 400.0 / 10.0);
                    assert_eq!(divider.height(), 300.0);
                    divider.center().x
                }
                SplitOrientation::Vertical => {
                    assert!(divider.height() < 300.0 / 10.0);
                    assert_eq!(divider.width(), 400.0);
                    divider.center().y
                }
            };
            assert_eq!(
                along,
                if orientation == SplitOrientation::Horizontal {
                    200.0
                } else {
                    150.0
                }
            );
            let root = harness.root::<ResizableSplit<TestLeaf, TestLeaf>>();
            let separator = root.children()[2];
            assert_eq!(separator.test_id(), Some("request-divider"));
            assert_eq!(
                separator.accessibility().unwrap().role,
                crate::a11y::Role::Separator
            );
            assert!(!root.is_focusable());
            let resources = harness.resources();
            let separator = resources
                .iter()
                .find(|resource| {
                    resource
                        .actions
                        .iter()
                        .any(|action| action.id == "set-value")
                })
                .expect("named shared divider publishes its canonical resize door");
            for verb in ["increment", "decrement", "set-value"] {
                assert!(separator.actions.iter().any(|action| action.id == verb));
            }
            harness
                .perform_named(
                    "request-divider",
                    &crate::a11y::Action::SetValue("40%".into()),
                )
                .expect("canonical named divider dispatch");
            assert!(
                (harness
                    .root::<ResizableSplit<TestLeaf, TestLeaf>>()
                    .split_position()
                    - 0.4)
                    .abs()
                    < f32::EPSILON
            );
            harness
                .perform_named(
                    "request-divider",
                    &crate::a11y::Action::SetValue("50%".into()),
                )
                .expect("restore through the same public control");
            harness.click_named("request-divider").unwrap();
            harness.key(if orientation == SplitOrientation::Horizontal {
                "ArrowRight"
            } else {
                "ArrowDown"
            });
            assert!(
                (harness
                    .root::<ResizableSplit<TestLeaf, TestLeaf>>()
                    .split_position()
                    - 0.55)
                    .abs()
                    < f32::EPSILON
            );
            let moved = harness.targets()["request-divider"].center();
            assert_eq!(
                if orientation == SplitOrientation::Horizontal {
                    moved.x
                } else {
                    moved.y
                },
                if orientation == SplitOrientation::Horizontal {
                    220.0
                } else {
                    165.0
                }
            );
        }
    }

    #[test]
    fn reset_restores_content_sized_first_pane_after_resizing() {
        let mut left = TestLeaf::new();
        left.width = Some(120.0);
        let mut split = ResizableSplit::new(left, TestLeaf::new(), SplitOrientation::Horizontal)
            .with_content_sized_first()
            .with_divider_id("request-divider");
        let (mut tree, node) = solve(&mut split, 400.0, 300.0);
        assert_eq!(split.left().get_rect().width(), 120.0);
        split.set_split_position(0.7);
        relayout(&mut split, &mut tree, node, 400.0, 300.0);
        assert_eq!(split.left().get_rect().width(), 280.0);
        split.reset_to_content_size();
        relayout(&mut split, &mut tree, node, 400.0, 300.0);
        assert_eq!(split.left().get_rect().width(), 120.0);
        assert_eq!(split.handle.hit_rect().center().x, 120.0);
    }

    #[test]
    fn both_panes_receive_the_update_pass() {
        let mut s = split(SplitOrientation::Horizontal);
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.left.changes = false;
        s.right.changes = false;
        assert!(!s.update(&doc, &root), "nothing changed");
        s.right.changes = true;
        assert!(s.update(&doc, &root), "a pane's change is the split's");
        assert_eq!((s.left.updates.get(), s.right.updates.get()), (2, 2));
    }

    #[test]
    fn split_layout_starts_even_and_fills_the_axis() {
        assert_eq!(split(SplitOrientation::Horizontal).split_position(), 0.5);
        let mut s = split(SplitOrientation::Horizontal).with_split_position(0.32);
        let (tree, node) = solve(&mut s, 1000.0, 640.0);
        assert_eq!(tree.children(node).expect("split children").len(), 2);
        assert!((s.left().rect.width() - 320.0).abs() < 0.1);
        assert!((s.right().rect.width() - 680.0).abs() < 0.1);
    }

    #[test]
    fn dragging_the_divider_relays_out_the_panes_within_bounds() {
        let mut s = split(SplitOrientation::Horizontal);
        let (mut tree, node) = solve(&mut s, 400.0, 300.0);
        dispatch(&mut s, &tree, node, &cursor_moved(200.0, 150.0));
        assert!(s.handle.is_hovered(), "the divider is under the pointer");
        assert_eq!(
            Widget::cursor_shape(&s, Point::new(200.0, 150.0)),
            Some(winit::window::CursorIcon::EwResize)
        );
        assert_eq!(Widget::cursor_shape(&s, Point::new(50.0, 150.0)), None);
        dispatch(&mut s, &tree, node, &mouse_press());
        assert!(s.handle.is_dragging());
        dispatch(&mut s, &tree, node, &cursor_moved(100.0, 150.0));
        dispatch(&mut s, &tree, node, &mouse_release());
        assert!(!s.handle.is_dragging());
        assert!(
            (s.split_position() - 0.25).abs() < 1e-3,
            "{}",
            s.split_position()
        );
        assert!(s.is_layout_dirty(), "the panes follow the divider");
        relayout(&mut s, &mut tree, node, 400.0, 300.0);
        assert!(
            (s.left().rect.width() - 100.0).abs() < 0.5,
            "{:?}",
            s.left().rect
        );
        assert!(!s.layout_dirty, "laid out at the new ratio");

        // Dragging past either end clamps.
        dispatch(&mut s, &tree, node, &cursor_moved(100.0, 150.0));
        dispatch(&mut s, &tree, node, &mouse_press());
        dispatch(&mut s, &tree, node, &cursor_moved(2.0, 150.0));
        dispatch(&mut s, &tree, node, &mouse_release());
        assert_eq!(s.split_position(), SPLIT_MIN);

        let v = split(SplitOrientation::Vertical);
        let mut v = v;
        solve(&mut v, 400.0, 300.0);
        assert_eq!(
            Widget::cursor_shape(&v, Point::new(200.0, 150.0)),
            Some(winit::window::CursorIcon::NsResize)
        );
    }

    #[test]
    fn the_divider_steps_with_the_keyboard_and_reads_as_a_separator() {
        let mut s = split(SplitOrientation::Horizontal).with_divider_label("Resize preview");
        let (tree, node) = solve(&mut s, 400.0, 300.0);
        assert!(s.is_focusable());
        s.set_focus(true);
        let right = crate::synthetic_input::key_input(
            &Key::Named(NamedKey::ArrowRight),
            ElementState::Pressed,
        );
        dispatch(&mut s, &tree, node, &right);
        assert!((s.split_position() - 0.55).abs() < 1e-4);
        let node_access = s.accessibility().expect("a separator");
        assert_eq!(node_access.role, crate::a11y::Role::Separator);
        assert_eq!(node_access.name.as_deref(), Some("Resize preview"));
        assert_eq!(node_access.value.as_deref(), Some("55%"));
        let mut doc = AutoCommit::new();
        assert!(s.perform_action(
            &crate::a11y::Action::Decrement,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!((s.split_position() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn the_per_frame_update_reaches_both_panes() {
        let mut s = split(SplitOrientation::Horizontal);
        let (left, right) = (s.left().updates.clone(), s.right().updates.clone());
        let doc = AutoCommit::new();
        assert!(Widget::update(&mut s, &doc, &automerge::ROOT));
        assert_eq!((left.get(), right.get()), (1, 1));
    }

    #[test]
    fn a_content_sized_first_pane_keeps_its_size_until_the_divider_moves() {
        let mut first = TestLeaf::new();
        first.width = Some(120.0);
        let mut s = ResizableSplit::new(first, TestLeaf::new(), SplitOrientation::Horizontal)
            .with_content_sized_first();
        let (mut tree, node) = solve(&mut s, 600.0, 300.0);
        assert!(
            (s.left().rect.width() - 120.0).abs() < 0.5,
            "{:?}",
            s.left().rect
        );
        assert!(
            (s.split_position() - 0.2).abs() < 1e-3,
            "the divider sits where it ends"
        );
        s.set_split_position(0.5);
        relayout(&mut s, &mut tree, node, 600.0, 300.0);
        assert!((s.left().rect.width() - 300.0).abs() < 0.5);
    }

    #[test]
    fn a_painted_host_shares_the_ratio_model() {
        let theme = Theme::dark();
        let mut handle = SplitHandle::new(SplitOrientation::Vertical, SplitRatio::new(0.3));
        handle.set_rect(Rect::new(0.0, 0.0, 400.0, 500.0), &theme);
        let (top, bottom) = handle.pane_rects();
        assert!((top.height() - 150.0).abs() < 0.5);
        assert!(
            (top.y1 - bottom.y0).abs() < 1e-9,
            "the panes meet at the hairline"
        );
        let mut ratio = SplitRatio::new(0.5).with_bounds(0.2, 0.6);
        assert!(ratio.set(0.9));
        assert!((ratio.position() - 0.6).abs() < 1e-6);
        assert!(!ratio.step(0.1), "already at the bound");
    }
}
