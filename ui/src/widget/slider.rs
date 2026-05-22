use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Circle, Rect, Point};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

/// Horizontal padding between `rect` and the track on each side.
/// Both `draw` and `handle_event` use this to translate between
/// cursor x and a 0..1 value, so it has to live in one place.
const TRACK_PADDING: f64 = 5.0;

/// Knob radius at rest. Hover and press scale this — see the
/// `_SCALE` constants below. Pulled out so tests can pin the
/// idle-vs-hover-vs-press contract without hard-coding pixels.
const KNOB_RADIUS: f64 = 10.0;
const KNOB_RADIUS_HOVER_SCALE: f64 = 1.08;
const KNOB_RADIUS_PRESS_SCALE: f64 = 1.15;

pub struct Slider {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_dragging: bool,
    is_focused: bool,
    value: f64, // 0.0 to 1.0
    on_change_path: String,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Last observed cursor position in logical-pixel space. Updated on
    /// every `CursorMoved`, used by `MouseInput::Pressed` so a press can
    /// (a) decide whether it's inside the slider's rect without relying
    /// on a prior hover edge transition and (b) jump the value to the
    /// click point immediately — the standard "click on track to jump"
    /// affordance.
    last_cursor: Point,
    /// Optional override for the `Widget::test_id()` value. When `None`
    /// the slider returns the role-style default (`"slider"`); when set
    /// callers can disambiguate multiple sliders in the same surface
    /// (e.g. `"volume"`, `"brightness"`).
    id_override: Option<String>,
}

impl Slider {
    pub fn new(id: &str, on_change_path: &str) -> Self {
        Self {
            _id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_dragging: false,
            is_focused: false,
            value: 0.5,
            on_change_path: on_change_path.to_string(),
            presence_path: Vec::new(),
            permission_scope: None,
            last_cursor: Point::ZERO,
            id_override: None,
        }
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Override the value returned from `Widget::test_id()`. Without a
    /// custom id the slider reports `"slider"` (the role-style default);
    /// callers with more than one slider in the same frame can disambiguate
    /// them with `with_id("volume")` etc.
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    /// Gate this widget behind a permission scope.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Map a cursor x in widget-local logical pixels to the slider's
    /// 0..1 value, clamped at both ends.
    fn value_from_cursor_x(&self, cursor_x: f64) -> f64 {
        let track_width = self.rect.width() - TRACK_PADDING * 2.0;
        ((cursor_x - (self.rect.x0 + TRACK_PADDING)) / track_width).clamp(0.0, 1.0)
    }

    /// Knob radius for the current visual state. Pressed > hover > idle.
    /// Public so the size contract is testable without poking the scene.
    pub fn knob_radius_for_state(&self) -> f64 {
        if self.is_dragging {
            KNOB_RADIUS * KNOB_RADIUS_PRESS_SCALE
        } else if self.is_hovered {
            KNOB_RADIUS * KNOB_RADIUS_HOVER_SCALE
        } else {
            KNOB_RADIUS
        }
    }

    #[cfg(test)]
    pub(crate) fn set_hovered_for_test(&mut self, hovered: bool) {
        self.is_hovered = hovered;
    }

    #[cfg(test)]
    pub(crate) fn set_dragging_for_test(&mut self, dragging: bool) {
        self.is_dragging = dragging;
    }

    /// Commit a new value to the CRDT iff it differs from the cached
    /// `value` by more than a frame-of-noise epsilon. Returns `true` if
    /// the commit happened so callers can flag the event as handled.
    fn commit_value(&mut self, new_value: f64, state: &mut AutoCommit, root: &ObjId) -> bool {
        if (new_value - self.value).abs() <= 0.001 {
            return false;
        }
        self.value = new_value;
        let _ = state.put(
            root,
            self.on_change_path.as_str(),
            automerge::ScalarValue::F64(self.value),
        );
        true
    }
}

impl Widget for Slider {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, self.on_change_path.as_str()) {
            if let Value::Scalar(s) = val {
                match s.as_ref() {
                    automerge::ScalarValue::F64(f) => self.value = *f,
                    automerge::ScalarValue::Int(i) => self.value = *i as f64,
                    automerge::ScalarValue::Uint(u) => self.value = *u as f64,
                    _ => {}
                }
            }
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(30.0),
            },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x as f64 + layout.location.x as f64,
            origin.y as f64 + layout.location.y as f64,
            origin.x as f64 + layout.location.x as f64 + layout.size.width as f64,
            origin.y as f64 + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, _text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        // Focus indicator (deferred until after the knob is positioned so
        // we can draw a tight ring around the knob, not the whole widget
        // rect — the full-rect ring looked like a stray border around
        // siblings in a Row layout).
        // Track
        let track_height = 6.0;
        let track_rect = Rect::new(
            self.rect.x0 + TRACK_PADDING,
            self.rect.y0 + (self.rect.height() - track_height) / 2.0,
            self.rect.x1 - TRACK_PADDING,
            self.rect.y0 + (self.rect.height() + track_height) / 2.0,
        );
        let track_shape = RoundedRect::from_rect(track_rect, 3.0);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.neutral[3], None, &track_shape);
        scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, theme.tokens.colors.border_standard, None, &track_shape);

        // Active Track (Blue part on the left)
        let active_track_rect = Rect::new(
            track_rect.x0,
            track_rect.y0,
            track_rect.x0 + self.value * track_rect.width(),
            track_rect.y1,
        );
        let active_track_shape = RoundedRect::from_rect(active_track_rect, 3.0);
        // Active track = filled portion of the slider — concept uses
        // indigo-500 here, indigo-600 only for hover affordances.
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.accent_primary, None, &active_track_shape);

        // Knob — radius scales with state so hover/press are obvious
        // even before you read the colors. Idle uses the translucent
        // surface_white_10 token over a border_white_20 stroke, hover
        // bumps the fill to surface_white_20, press fills with the
        // accent and adds a soft halo.
        let knob_radius = self.knob_radius_for_state();
        let knob_x = track_rect.x0 + self.value * track_rect.width();
        let knob_y = self.rect.y0 + self.rect.height() / 2.0;
        let knob_center = Point::new(knob_x, knob_y);
        let knob_shape = Circle::new(knob_center, knob_radius);

        let colors = &theme.tokens.colors;
        let (fill_color, border_color, border_width) = if self.is_dragging {
            (theme.accent_primary, colors.border_white_20, 1.5)
        } else if self.is_hovered {
            (colors.surface_white_20, theme.text_dim, 1.5)
        } else {
            (colors.surface_white_10, colors.border_white_20, 1.5)
        };

        // Pressed halo: two nested strokes at decreasing alpha *outside*
        // the knob so you get a visible glow on engage. Drawn before the
        // fill so the knob sits cleanly on top.
        if self.is_dragging {
            let halo_outer = Circle::new(knob_center, knob_radius + 4.0);
            let halo_inner = Circle::new(knob_center, knob_radius + 2.0);
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, colors.border_white_10, None, &halo_outer);
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, colors.border_white_20, None, &halo_inner);
        }

        scene.fill(vello::peniko::Fill::NonZero, transform, fill_color, None, &knob_shape);
        scene.stroke(&vello::kurbo::Stroke::new(border_width), transform, border_color, None, &knob_shape);

        // Tight focus ring around the knob (2 px outset) — replaces the
        // earlier full-rect border that looked like a misdrawn outer
        // chrome when the slider lived inside a Row.
        if self.is_focused {
            let focus_ring = Circle::new(knob_center, knob_radius + 2.0);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                theme.border_focus,
                None,
                &focus_ring,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let hover = self.rect.contains(p);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }

                if self.is_dragging {
                    let new_value = self.value_from_cursor_x(p.x);
                    if self.commit_value(new_value, state, root) {
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                // Press uses `rect.contains(last_cursor)` rather than
                // `is_hovered` so the press latches without depending on
                // an earlier hover-edge transition (e.g. focus delivered
                // a press without a preceding `CursorMoved`). On entry
                // we also jump `value` to the click x — the standard
                // "press on track to jump" affordance.
                if *element_state == ElementState::Pressed
                    && self.rect.contains(self.last_cursor)
                {
                    self.is_dragging = true;
                    *focus_target = Some(node_id);
                    *handled = true;
                    let new_value = self.value_from_cursor_x(self.last_cursor.x);
                    self.commit_value(new_value, state, root);
                } else if *element_state == ElementState::Released {
                    if self.is_dragging {
                        self.is_dragging = false;
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// While dragging, the slider must keep receiving pointer events even
    /// when the cursor leaves its `rect` — otherwise releasing outside
    /// would silently strand the drag in the engaged state.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn name(&self) -> &str { "Slider" }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("slider"))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.presence_path.clone() })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);
                scene.fill(vello::peniko::Fill::NonZero, transform, overlay, None, &self.rect);
            }
        }

        if let Some(key) = self.presence_key() {
            if let Some(peers) = presence.here.get(&key) {
                if let Some(user) = peers.first() {
                    presence.draw_peer_focus(scene, theme, user, self.rect);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a slider with a known rect so we don't need a full taffy pass.
    /// Rect spans [0..200, 0..30] — track-padding 5 px, so the track itself
    /// is [5..195] (width 190).
    fn slider_at(x0: f64, y0: f64, x1: f64, y1: f64) -> Slider {
        let mut s = Slider::new("test", "value");
        s.rect = Rect::new(x0, y0, x1, y1);
        s
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn mouse_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    /// Drive an event through the slider with default scaffolding.
    fn dispatch(s: &mut Slider, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    #[test]
    fn knob_radius_grows_on_hover_and_drag() {
        // Idle < hover < pressed. Visual contract for state contrast —
        // if this test fails the slider has stopped scaling the knob
        // and the polish has regressed.
        let mut s = Slider::new("test", "v");
        let r0 = s.knob_radius_for_state();

        s.set_hovered_for_test(true);
        let r1 = s.knob_radius_for_state();

        s.set_dragging_for_test(true);
        let r2 = s.knob_radius_for_state();

        assert!(r0 < r1, "hover must grow knob: idle={}, hover={}", r0, r1);
        assert!(r1 < r2, "press must grow knob further: hover={}, press={}", r1, r2);
    }

    #[test]
    fn drag_continues_after_cursor_leaves_rect() {
        // Rect [0..200, 0..30]. Track usable width 190 (padding 5 each side).
        let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

        // Move cursor inside, near the left edge of the track.
        dispatch(&mut s, &cursor_moved(20.0, 15.0));
        // Press inside the rect — drag begins.
        dispatch(&mut s, &mouse_press());
        assert!(s.is_dragging, "press inside rect must start drag");

        // Cursor leaves the rect vertically (still within the track's
        // horizontal range). The drag must continue updating `value`.
        dispatch(&mut s, &cursor_moved(150.0, 200.0));
        let expected = ((150.0 - 5.0) / 190.0_f64).clamp(0.0, 1.0);
        assert!(
            (s.value - expected).abs() < 1e-6,
            "drag outside rect must keep tracking cursor x — got {}, expected {}",
            s.value,
            expected
        );

        // And another move further outside — value still tracks.
        dispatch(&mut s, &cursor_moved(95.0, -50.0));
        let expected2 = ((95.0 - 5.0) / 190.0_f64).clamp(0.0, 1.0);
        assert!(
            (s.value - expected2).abs() < 1e-6,
            "drag continues to track even far outside rect — got {}, expected {}",
            s.value,
            expected2
        );

        // Release ends the drag.
        dispatch(&mut s, &mouse_release());
        assert!(!s.is_dragging, "release must end drag");
    }

    #[test]
    fn press_on_track_jumps_value() {
        // Rect [0..200, 0..30], track [5..195] (width 190).
        // x at 0.8 of width → 5 + 0.8*190 = 157.0
        let mut s = slider_at(0.0, 0.0, 200.0, 30.0);

        // Move cursor to the press location *and* press without any
        // prior hover state being toggled by the same code path —
        // this is the "click on track to jump" affordance.
        let press_x = 5.0 + 0.8 * 190.0;
        dispatch(&mut s, &cursor_moved(press_x, 15.0));
        dispatch(&mut s, &mouse_press());

        assert!(s.is_dragging, "press inside rect must start drag");
        assert!(
            (s.value - 0.8).abs() < 1e-3,
            "press on track at x=0.8 of width must jump value to ≈0.8 — got {}",
            s.value
        );
    }

    #[test]
    fn slider_test_id_default_is_slider() {
        let s = Slider::new("any", "value");
        assert_eq!(Widget::test_id(&s), Some("slider"));
    }

    #[test]
    fn slider_test_id_can_be_overridden() {
        let s = Slider::new("any", "value").with_id("volume");
        assert_eq!(Widget::test_id(&s), Some("volume"));
    }
}

