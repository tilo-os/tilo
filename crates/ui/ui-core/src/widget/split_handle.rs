//! [`SplitHandle`], the divider between two panes, for hosts that paint their panes
//! and share its ratio: the share the first pane takes and its bounds.

use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;

const SPLIT_MIN: f32 = 0.1;
const SPLIT_MAX: f32 = 0.9;
const SPLIT_DEFAULT: f32 = 0.5;
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

    #[must_use]
    pub fn with_bounds(mut self, min: f32, max: f32) -> Self {
        self.min = min.clamp(0.0, 1.0);
        self.max = max.clamp(self.min, 1.0);
        self.position = self.position.clamp(self.min, self.max);
        self
    }

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
        self.set(tilo_ui_base::num::f32_of_f64((along - start) / extent.max(1.0)))
    }

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

/// The divider between two panes: hit-testing, hover, dragging, keyboard steps, painting and
/// its accessible separator node.
#[derive(Clone, Debug)]
#[tilo_ui_base::component::not_a_component(part, "the handle of the language's `split`")]
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
    /// The most the first pane can use, when the host knows it: past it
    /// the divider stands at it and the rest goes to the second pane,
    /// while the ratio the person chose is kept for when it fits again.
    first_max: Option<f64>,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
}

impl SplitHandle {
    #[must_use]
    pub fn new(orientation: SplitOrientation, ratio: SplitRatio) -> Self {
        Self {
            dirty: true,
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
            first_max: None,
        }
    }

    /// What assistive technology calls the divider ("Resize sidebar").
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Give the divider a stable identity: assistive technology and agents
    /// address it by it ([`Widget::perform_semantic_action`]).
    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
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

    /// The most the first pane can use (a picture that already fills its
    /// width); `None`: all the ratio gives it. The ratio itself is kept.
    pub fn set_first_max(&mut self, max: Option<f64>) {
        self.dirty = true;
        self.first_max = max.map(|max| max.max(0.0));
        self.place();
    }

    /// The first pane's length on the split's extent `total`: the ratio's,
    /// up to what the pane can use.
    fn first_extent(&self, total: f64) -> f64 {
        let extent = self.ratio.first_extent(total);
        self.first_max.map_or(extent, |max| extent.min(max))
    }

    /// The share the first pane shows, which is the ratio's unless the
    /// pane can use less.
    fn shown_position(&self) -> f32 {
        let total = match self.orientation {
            SplitOrientation::Horizontal => self.rect.width(),
            SplitOrientation::Vertical => self.rect.height(),
        };
        if total <= 0.0 {
            return self.ratio.position();
        }
        tilo_ui_base::num::f32_of_f64(self.first_extent(total) / total)
    }

    /// Step the first pane smaller from where the divider stands, not from
    /// a ratio past what the pane can use, so the step shows.
    fn step_back(&mut self) -> bool {
        let shown = self.shown_position();
        if shown < self.ratio.position() {
            self.ratio.set(shown);
        }
        self.ratio.step(-KEY_STEP)
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
                let x = (r.x0 + self.first_extent(r.width())).round();
                Rect::new(x - self.hit_half, r.y0, x + self.hit_half, r.y1)
            }
            SplitOrientation::Vertical => {
                let y = (r.y0 + self.first_extent(r.height())).round();
                Rect::new(r.x0, y - self.hit_half, r.x1, y + self.hit_half)
            }
        };
    }

    #[must_use]
    pub fn hit_rect(&self) -> Rect {
        self.divider
    }

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
        self.dirty = true;
        self.focused = focused;
    }

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
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        self.dirty = true;
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
                    Key::Named(key) if *key == back => self.step_back(),
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
    pub fn perform_action(&mut self, action: &tilo_ui_base::a11y::Action) -> bool {
        let moved = match action {
            tilo_ui_base::a11y::Action::Increment => self.ratio.step(KEY_STEP),
            tilo_ui_base::a11y::Action::Decrement => self.step_back(),
            tilo_ui_base::a11y::Action::SetValue(value) => value
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
            theme.tokens.colors.intent.value_fill
        } else {
            tilo_ui_base::surface::edge_on(ground)
        };
        scene.fill(vello::peniko::Fill::NonZero, transform, colour, None, &line);
        if self.focused {
            crate::widget::paint_focus_ring(scene, transform, self.divider, 2.0, theme);
        }
    }

    pub fn accessibility(&self) -> tilo_ui_base::a11y::AccessNode {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        AccessNode::new(Role::Separator)
            .with_name(self.label.clone())
            .with_value(format!(
                "{}%",
                SplitRatio::new(self.shown_position()).percent()
            ))
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
        _text: &mut tilo_ui_base::text_render::TextRenderer,
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

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text_renderer, theme, mut env) = ctx.parts();
        self.paint(
            scene,
            transform,
            theme.tokens.colors.surfaces.content,
            theme,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
        self.layout_dirty |= SplitHandle::handle_event(self, event, ctx);
    }

    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        _key_path: &[u64],
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &tilo_ui_base::widget::ModelPath,
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
        self.dirty = true;
        self.set_focused(focused);
    }
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging()
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        SplitHandle::cursor_shape(self, point)
    }
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let mut node = SplitHandle::accessibility(self);
        if let Some(id) = &self.test_id {
            node = node.with_action_target(id);
        }
        Some(node)
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &tilo_ui_base::widget::ModelPath,
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

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
    }
    fn name(&self) -> &'static str {
        "SplitHandle"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_handle_shares_the_ratio_and_its_bounds;

        phase a_capped_first_pane_keeps_the_chosen_ratio;
        {
            let theme = Theme::dark();
            let mut handle = SplitHandle::new(SplitOrientation::Vertical, SplitRatio::new(0.6));
            handle.set_rect(Rect::new(0.0, 0.0, 400.0, 500.0), &theme);
            handle.set_first_max(Some(200.0));
            let (top, bottom) = handle.pane_rects();
            assert!(
                (top.height() - 200.0).abs() < 0.5,
                "the divider stands at the cap"
            );
            assert!((bottom.height() - 300.0).abs() < 0.5, "the rest goes below");
            assert!(
                (handle.ratio().position() - 0.6).abs() < 1e-6,
                "the ratio is kept"
            );
            handle.set_first_max(Some(1000.0));
            let (top, _) = handle.pane_rects();
            assert!(
                (top.height() - 300.0).abs() < 0.5,
                "with area, the ratio is back"
            );
            handle.set_first_max(Some(200.0));
            assert!(handle.perform_action(&tilo_ui_base::a11y::Action::Decrement));
            let (top, _) = handle.pane_rects();
            assert!(
                top.height() < 199.5,
                "a step back moves from where the divider stands"
            );
        }
    }
}
