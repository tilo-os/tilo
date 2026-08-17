//! `SwipeRow` — a list row that can be dragged horizontally to reveal a
//! mapped action on each side, iOS-Mail style.
//!
//! The row wraps a single content [`Widget`] (an article row, a mail row, a
//! conversation…). The caller supplies up to one [`SwipeAction`] per side —
//! each an icon, a label and a background tint. Dragging the row **right**
//! reveals the `left` action (it lives against the left edge); dragging
//! **left** reveals the `right` action. Releasing past the trigger threshold
//! fires that side's action and the row springs back; releasing before the
//! threshold just springs back with nothing fired.
//!
//! It is deliberately generic: feeds maps left→favourite and right→mark-read,
//! but mail (archive / delete) or any other list reuses the same primitive
//! with its own actions, colours and icons. Which side fired is drained by the
//! host via [`SwipeRow::take_triggered`] — the same `&mut self` / CRDT-friendly
//! channel [`crate::widget::ContextMenu`] uses, so the mutation runs against the
//! host's Automerge document rather than through an `Arc<dyn Fn()>`.
//!
//! Input: both pointer (press-drag-release) and touch (a single finger) drive
//! the same offset. A drag is only *captured* once the horizontal travel
//! exceeds [`CAPTURE_PX`], so a plain tap still reaches the content widget and
//! a vertical scroll is never hijacked. While captured the row reports
//! [`Widget::wants_pointer_capture`] so an enclosing `Scrollable` yields the
//! gesture.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

use crate::animation::Spring;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::SpringParams;
use crate::widget::Widget;

/// Which edge of the row an action lives on / which side fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwipeSide {
    /// Revealed by dragging the row to the **right**; lives on the left edge.
    Left,
    /// Revealed by dragging the row to the **left**; lives on the right edge.
    Right,
}

/// One swipe-revealed action: an icon, a short label and the colours of the
/// panel that slides out behind the row. Colours are caller-supplied Theme
/// tokens so light/dark and OS accent stay consistent.
#[derive(Clone, Debug)]
pub struct SwipeAction {
    pub icon: Icon,
    pub label: String,
    /// Fill of the revealed panel.
    pub bg: Color,
    /// Icon + label colour drawn on the panel.
    pub fg: Color,
}

impl SwipeAction {
    pub fn new(icon: Icon, label: impl Into<String>, bg: Color, fg: Color) -> Self {
        Self { icon, label: label.into(), bg, fg }
    }
}

/// Minimum horizontal travel (logical px) before a press-drag is treated as a
/// swipe rather than a tap. Below this the press flows to the content widget.
pub const CAPTURE_PX: f64 = 8.0;

/// Icon box edge painted in the revealed panel.
const ICON_PX: f64 = 22.0;
/// Label size in the revealed panel.
const LABEL_SIZE: f32 = 12.0;

/// A list row with per-side swipe actions wrapping one content widget.
pub struct SwipeRow<C: Widget> {
    content: C,
    left: Option<SwipeAction>,
    right: Option<SwipeAction>,
    rect: Rect,

    /// Current horizontal displacement of the content (px). `> 0` reveals the
    /// left action, `< 0` the right action.
    offset: f64,
    /// Spring driving the release snap-back toward `offset == 0`.
    spring: Spring,
    /// `true` between a settled state and the next spring step, so the host
    /// keeps pumping frames until the row rests.
    animating: bool,

    /// Pointer/touch gesture bookkeeping.
    press_start: Option<Point>,
    press_start_offset: f64,
    captured: bool,
    active_touch: Option<u64>,
    last_cursor: Point,

    /// Set when a release crosses the trigger threshold on a side that has an
    /// action. Drained by [`Self::take_triggered`].
    triggered: Option<SwipeSide>,
    dirty: bool,
}

impl<C: Widget> SwipeRow<C> {
    /// Wrap `content` with no actions yet (add via [`Self::left_action`] /
    /// [`Self::right_action`]).
    pub fn new(content: C) -> Self {
        Self {
            content,
            left: None,
            right: None,
            rect: Rect::ZERO,
            offset: 0.0,
            // Snap-back uses the "snappy" preset (stiff, well-damped) so the
            // row returns crisply without wobble.
            spring: Spring::new(0.0, SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 }),
            animating: false,
            press_start: None,
            press_start_offset: 0.0,
            captured: false,
            active_touch: None,
            last_cursor: Point::ZERO,
            triggered: None,
            dirty: true,
        }
    }

    /// Set the action revealed by a rightward drag (left edge).
    pub fn left_action(mut self, action: SwipeAction) -> Self {
        self.left = Some(action);
        self
    }

    /// Set the action revealed by a leftward drag (right edge).
    pub fn right_action(mut self, action: SwipeAction) -> Self {
        self.right = Some(action);
        self
    }

    /// Borrow the wrapped content widget.
    pub fn content(&self) -> &C {
        &self.content
    }

    /// Mutably borrow the wrapped content widget.
    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    /// Current content displacement in logical px (`>0` right, `<0` left).
    pub fn offset(&self) -> f64 {
        self.offset
    }

    /// `true` while the release snap-back spring has not settled.
    pub fn is_animating(&self) -> bool {
        self.animating
    }

    /// Drain the side whose action a release just triggered — `Some` exactly
    /// once per trigger, then `None`. Poll after dispatching an event and run
    /// the mapped mutation against the host's document.
    pub fn take_triggered(&mut self) -> Option<SwipeSide> {
        self.triggered.take()
    }

    /// Trigger threshold in px for the current row width. A release past this
    /// (on a side with an action) fires that action.
    fn trigger_px(&self) -> f64 {
        (self.rect.width() * 0.30).clamp(72.0, 200.0)
    }

    /// Clamp a raw offset to the sides that actually have an action, so a row
    /// with no left action can't be dragged right.
    fn clamp_offset(&self, raw: f64) -> f64 {
        let w = self.rect.width();
        let max_pos = if self.left.is_some() { w } else { 0.0 };
        let max_neg = if self.right.is_some() { w } else { 0.0 };
        raw.clamp(-max_neg, max_pos)
    }

    /// Begin a gesture at `p` (pointer press or first touch point).
    fn gesture_begin(&mut self, p: Point) {
        self.press_start = Some(p);
        self.press_start_offset = self.offset;
        self.captured = false;
    }

    /// Advance a gesture to `p`. Returns `true` once the drag is captured (so
    /// the caller consumes the event and withholds it from the content).
    fn gesture_move(&mut self, p: Point) -> bool {
        let Some(start) = self.press_start else { return false };
        let dx = p.x - start.x;
        if !self.captured {
            if dx.abs() < CAPTURE_PX {
                return false;
            }
            self.captured = true;
            // Cancel any in-flight snap-back — the finger owns the offset now.
            self.animating = false;
        }
        let new_offset = self.clamp_offset(self.press_start_offset + dx);
        if new_offset != self.offset {
            self.offset = new_offset;
            self.dirty = true;
        }
        true
    }

    /// End a gesture. Decides whether the release crossed the trigger
    /// threshold, records the fired side, and starts the snap-back.
    fn gesture_end(&mut self) {
        if self.captured {
            let trigger = self.trigger_px();
            if self.offset >= trigger && self.left.is_some() {
                self.triggered = Some(SwipeSide::Left);
            } else if self.offset <= -trigger && self.right.is_some() {
                self.triggered = Some(SwipeSide::Right);
            }
            // Always spring back to rest — the action, if any, has fired.
            self.spring.snap(self.offset as f32);
            self.spring.set_target(0.0);
            self.animating = true;
            self.dirty = true;
        }
        self.press_start = None;
        self.captured = false;
        self.active_touch = None;
    }

    /// Step the snap-back spring by `dt` seconds. Exposed for deterministic
    /// tests; the `draw` path drives it from a real clock.
    pub fn step_animation(&mut self, dt: f32) {
        if !self.animating {
            return;
        }
        self.spring.step(dt);
        self.offset = self.spring.value() as f64;
        self.dirty = true;
        if self.spring.is_settled() {
            self.offset = 0.0;
            self.animating = false;
        }
    }

    /// Draw the revealed action panel behind the content on the active side.
    fn draw_reveal(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.offset.abs() < 0.5 {
            return;
        }
        let (action, panel) = if self.offset > 0.0 {
            let Some(a) = &self.left else { return };
            (a, Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + self.offset, self.rect.y1))
        } else {
            let Some(a) = &self.right else { return };
            (a, Rect::new(self.rect.x1 + self.offset, self.rect.y0, self.rect.x1, self.rect.y1))
        };
        if panel.width() <= 0.0 {
            return;
        }
        let shape = RoundedRect::from_rect(panel, theme.radius_standard);
        scene.fill(vello::peniko::Fill::NonZero, transform, action.bg, None, &shape);

        // Icon + label centred in the revealed strip; past the trigger point
        // the affordance shifts toward the content edge (iOS "will fire" feel).
        let revealed = panel.width();
        let cx = if self.offset > 0.0 {
            panel.x0 + (revealed.min(80.0)) / 2.0
        } else {
            panel.x1 - (revealed.min(80.0)) / 2.0
        };
        let cy = (panel.y0 + panel.y1) / 2.0;
        if revealed >= ICON_PX + 4.0 {
            let icon_rect =
                Rect::new(cx - ICON_PX / 2.0, cy - ICON_PX / 2.0 - 8.0, cx + ICON_PX / 2.0, cy + ICON_PX / 2.0 - 8.0);
            action.icon.draw_with_stroke(
                scene,
                transform,
                icon_rect,
                action.fg,
                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        if revealed >= 56.0 {
            let (tw, _th) = text_renderer.measure(&action.label, LABEL_SIZE, None);
            let tx = cx - tw as f64 / 2.0;
            text_renderer.draw_text(
                scene,
                &action.label,
                LABEL_SIZE,
                action.fg,
                transform * Affine::translate((tx, cy + 6.0)),
                None,
            );
        }
    }
}

impl<C: Widget + 'static> Widget for SwipeRow<C> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.content.build_layout(taffy, theme, node_map);
        let node = taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::percent(1.0), height: Dimension::auto() },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let child_node = taffy.children(node).unwrap()[0];
        self.content.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Advance the snap-back from a real clock so the row settles smoothly
        // between frames. `step_animation` is a no-op when not animating.
        if self.animating {
            self.step_animation(1.0 / 60.0);
        }
        self.draw_reveal(scene, transform, text_renderer, theme);
        // The content rides on top, translated by the current offset.
        self.content
            .draw(scene, transform * Affine::translate((self.offset, 0.0)), text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Forward to the content first for the non-gesture cases; the gesture
        // arms below decide whether to *also* consume.
        let mut forward_to_child = true;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                if self.press_start.is_some() && self.gesture_move(p) {
                    *handled = true;
                    forward_to_child = false;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.rect.contains(self.last_cursor) {
                    self.gesture_begin(self.last_cursor);
                    // Let the press reach the child too (tap selection); we
                    // only take over once the drag is captured.
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.captured {
                    self.gesture_end();
                    *handled = true;
                    forward_to_child = false;
                } else {
                    self.gesture_end();
                }
            }
            WindowEvent::Touch(t) => {
                let p = Point::new(t.location.x / scale_factor, t.location.y / scale_factor);
                match t.phase {
                    TouchPhase::Started => {
                        if self.rect.contains(p) && self.active_touch.is_none() {
                            self.active_touch = Some(t.id);
                            self.last_cursor = p;
                            self.gesture_begin(p);
                        }
                    }
                    TouchPhase::Moved => {
                        if self.active_touch == Some(t.id) {
                            self.last_cursor = p;
                            if self.gesture_move(p) {
                                *handled = true;
                                forward_to_child = false;
                            }
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if self.active_touch == Some(t.id) {
                            let was_captured = self.captured;
                            self.gesture_end();
                            if was_captured {
                                *handled = true;
                                forward_to_child = false;
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        if forward_to_child && !*handled {
            let child_node = taffy.children(node_id).unwrap()[0];
            self.content
                .handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, child_node, taffy);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.content as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.content as &mut (dyn Widget + '_)]
    }

    fn name(&self) -> &str {
        "SwipeRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// While a drag is captured the row owns the pointer so an enclosing
    /// `Scrollable` doesn't steal the gesture as a scroll.
    fn wants_pointer_capture(&self) -> bool {
        self.captured || self.content.wants_pointer_capture()
    }

    fn needs_animation_frame(&self) -> bool {
        self.animating || self.content.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.animating || self.content.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.content.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.content.is_layout_dirty()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.content.take_structure_dirty()
    }

    fn needs_commit(&self) -> bool {
        self.content.needs_commit()
    }

    fn on_commit(&mut self) {
        self.content.on_commit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::DeviceId;

    /// A fixed-size leaf standing in for real row content. A definite height
    /// (64px) means `compute_layout` resolves the row's box without needing the
    /// text-measure pass, and it records whether it received a forwarded press.
    struct FixedContent {
        rect: Rect,
        got_press: bool,
    }
    impl FixedContent {
        fn new() -> Self {
            Self { rect: Rect::ZERO, got_press: false }
        }
    }
    impl Widget for FixedContent {
        fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size { width: Dimension::percent(1.0), height: Dimension::length(64.0) },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            self.rect = Rect::new(
                origin.x + l.location.x as f64,
                origin.y + l.location.y as f64,
                origin.x + l.location.x as f64 + l.size.width as f64,
                origin.y + l.location.y as f64 + l.size.height as f64,
            );
        }
        fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {}
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut AutoCommit,
            _: &ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
            if matches!(
                event,
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
            ) {
                self.got_press = true;
            }
        }
        fn name(&self) -> &str {
            "FixedContent"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn theme_action_left() -> SwipeAction {
        SwipeAction::new(Icon::Star, "Favourite", Color::from_rgb8(200, 160, 0), Color::WHITE)
    }
    fn theme_action_right() -> SwipeAction {
        SwipeAction::new(Icon::Check, "Read", Color::from_rgb8(0, 120, 200), Color::WHITE)
    }

    fn row() -> SwipeRow<FixedContent> {
        SwipeRow::new(FixedContent::new())
            .left_action(theme_action_left())
            .right_action(theme_action_right())
    }

    /// Lay the row out in an 800×64 slot so `rect.width()` is real (the
    /// trigger threshold and clamp depend on it).
    fn lay_out(r: &mut SwipeRow<FixedContent>) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = r.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(64.0),
                },
            )
            .unwrap();
        r.apply_layout(&taffy, node, Point::ZERO);
    }

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    // The wrapped `Text` gives the row an auto height of roughly one line, so
    // press points must sit near the top of the row to land inside its rect.
    const ROW_Y: f64 = 8.0;
    fn cursor(x: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(x, ROW_Y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }
    fn touch(id: u64, phase: TouchPhase, x: f64) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: synth_device(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, ROW_Y),
            force: None,
            id,
        })
    }

    fn dispatch(r: &mut SwipeRow<FixedContent>, event: &WindowEvent) -> bool {
        // A trivial taffy tree with one child so `taffy.children(node)[0]`
        // resolves during event forwarding.
        let mut taffy = TaffyTree::<()>::new();
        let child = taffy.new_leaf(Style::default()).unwrap();
        let node = taffy.new_with_children(Style::default(), &[child]).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        r.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    /// Drag the row a signed `dx` from centre and release. Returns the fired
    /// side (drained).
    fn drag_and_release(r: &mut SwipeRow<FixedContent>, dx: f64) -> Option<SwipeSide> {
        let start = 400.0;
        dispatch(r, &cursor(start));
        dispatch(r, &press());
        dispatch(r, &cursor(start + dx));
        dispatch(r, &release());
        r.take_triggered()
    }

    #[test]
    fn swipe_left_past_threshold_triggers_right_action() {
        let mut r = row();
        lay_out(&mut r);
        // A leftward drag (negative dx) well past the trigger reveals + fires
        // the RIGHT action.
        let fired = drag_and_release(&mut r, -300.0);
        assert_eq!(fired, Some(SwipeSide::Right), "a full left swipe fires the right action");
        assert_eq!(r.take_triggered(), None, "the trigger drains exactly once");
        // And the row is springing back toward rest.
        assert!(r.is_animating(), "a released swipe snaps back");
        assert_eq!(r.spring.target(), 0.0, "snap-back targets rest");
    }

    #[test]
    fn swipe_right_past_threshold_triggers_left_action() {
        let mut r = row();
        lay_out(&mut r);
        let fired = drag_and_release(&mut r, 300.0);
        assert_eq!(fired, Some(SwipeSide::Left), "a full right swipe fires the left action");
    }

    #[test]
    fn short_swipe_below_threshold_does_not_trigger_but_snaps_back() {
        let mut r = row();
        lay_out(&mut r);
        // 40px is past CAPTURE (so it's a swipe, not a tap) but below the
        // ~240px trigger for an 800px row.
        let fired = drag_and_release(&mut r, -40.0);
        assert_eq!(fired, None, "a short swipe fires nothing");
        assert!(r.is_animating(), "but it still springs back to rest");
        assert_eq!(r.spring.target(), 0.0);
    }

    #[test]
    fn tap_below_capture_does_not_capture_and_forwards() {
        let mut r = row();
        lay_out(&mut r);
        dispatch(&mut r, &cursor(400.0));
        dispatch(&mut r, &press());
        // Move only 3px — below CAPTURE_PX. Must not capture, so the move is
        // not consumed as a swipe.
        let handled = dispatch(&mut r, &cursor(403.0));
        assert!(!handled, "a sub-capture move is not swallowed as a swipe");
        assert_eq!(r.offset(), 0.0, "no offset accrues below the capture threshold");
        // Release without capture fires nothing.
        dispatch(&mut r, &release());
        assert_eq!(r.take_triggered(), None);
    }

    #[test]
    fn missing_action_side_clamps_and_never_fires() {
        // A row with only a right action can't be dragged right.
        let mut r = SwipeRow::new(FixedContent::new()).right_action(theme_action_right());
        lay_out(&mut r);
        let fired = drag_and_release(&mut r, 300.0);
        assert_eq!(fired, None, "no left action → a rightward swipe fires nothing");
        assert_eq!(r.offset(), 0.0, "a side with no action clamps the offset to zero");
    }

    #[test]
    fn touch_drag_triggers_like_pointer() {
        let mut r = row();
        lay_out(&mut r);
        dispatch(&mut r, &touch(7, TouchPhase::Started, 400.0));
        let handled = dispatch(&mut r, &touch(7, TouchPhase::Moved, 100.0));
        assert!(handled, "a captured touch move is consumed");
        dispatch(&mut r, &touch(7, TouchPhase::Ended, 100.0));
        assert_eq!(r.take_triggered(), Some(SwipeSide::Right), "a left touch-swipe fires the right action");
    }

    #[test]
    fn snap_back_settles_offset_to_zero() {
        let mut r = row();
        lay_out(&mut r);
        drag_and_release(&mut r, -300.0);
        // Pump the spring; it must converge to exactly zero and stop animating.
        for _ in 0..600 {
            if !r.is_animating() {
                break;
            }
            r.step_animation(1.0 / 60.0);
        }
        assert!(!r.is_animating(), "the snap-back settles");
        assert_eq!(r.offset(), 0.0, "the row rests at zero offset");
    }

    #[test]
    fn reveal_panel_paints_when_swiped() {
        use vello_encoding::DrawTag;
        let mut r = row();
        lay_out(&mut r);
        // Capture a leftward drag so the right panel is revealed.
        dispatch(&mut r, &cursor(400.0));
        dispatch(&mut r, &press());
        dispatch(&mut r, &cursor(200.0));
        assert!(r.offset() < 0.0, "the row is displaced left");

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        r.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(fills >= 1, "a swiped row paints its coloured reveal panel");
    }
}
