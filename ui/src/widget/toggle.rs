use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Circle, Rect, Point, Stroke};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

pub struct Toggle {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    is_focused: bool,
    on_toggle_path: String, // Path in CRDT to mutate
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When `false` the toggle is disabled: skipped by focus, rejects
    /// input (no toggle, no hover feedback) and renders dimmed. Default `true`.
    enabled: bool,
    /// Optional override for `Widget::test_id()`. Defaults to `"toggle"`.
    id_override: Option<String>,
    /// Spoken name for a standalone toggle that has no adjacent text label
    /// (e.g. an icon-only switch in a toolbar). When unset the toggle
    /// relies on a sibling label resolved by the tree builder.
    accessible_name: Option<String>,
}

impl Toggle {
    pub fn new(id: &str, on_toggle_path: &str) -> Self {
        Self {
            _id: id.to_string(),
            rect: Rect::ZERO,
            is_hovered: false,
            is_checked: false,
            is_focused: false,
            on_toggle_path: on_toggle_path.to_string(),
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            id_override: None,
            accessible_name: None,
        }
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Override the value returned from `Widget::test_id()`. Without a
    /// custom id the toggle reports `"toggle"`; callers with multiple
    /// toggles in the same surface can disambiguate them with e.g.
    /// `with_id("airplane_mode")`.
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    /// Set the enabled state at build time. A disabled toggle is skipped
    /// by focus, rejects input and renders dimmed. Default `true`.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Give a label-less toggle a spoken name, so a screen reader
    /// announces what it switches ("Airplane mode", "Dark theme") rather
    /// than just "switch, on".
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    /// Imperatively flip the enabled state.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// Interaction state for an immediate-mode toggle face. The retained
/// [`Toggle`] tracks these itself; an immediate-mode caller (a canvas app
/// that owns its own hit-testing) passes what it knows — usually just
/// `hovered`, derived from the last cursor position.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ToggleInteract {
    pub hovered: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// Resolve the `(track, knob, border, border_width)` colours for a switch in
/// a given on/off + interaction state. The single source of truth for both
/// the retained [`Toggle`]'s draw and the immediate-mode [`Toggle::paint`] —
/// so every switch across the OS shares one palette per state.
pub fn toggle_face_colors(
    theme: &Theme,
    on: bool,
    interact: ToggleInteract,
) -> (vello::peniko::Color, vello::peniko::Color, vello::peniko::Color, f64) {
    let track = if on {
        // On-state = active fill (indigo-500); indigo-600 is reserved for hover.
        theme.accent_primary
    } else {
        theme.tokens.colors.neutral[3]
    };
    // The knob is the whole of what a switch says. Both tones are chosen for
    // the neutral off-track, and on the accent on-track they collapse into it
    // — 1.16:1 on the dark palette, which is a switch with no knob at all. So
    // each tone is resolved against the track it will actually sit on.
    let knob_tone = if interact.hovered { theme.text_primary } else { theme.text_dim };
    let knob = theme.tokens.colors.ink_on_any(
        &[track],
        knob_tone,
        crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
    );
    let (border, border_w) = if interact.focused {
        (theme.border_focus, 2.0)
    } else {
        (theme.tokens.colors.border_standard, 1.0)
    };
    (track, knob, border, border_w)
}

/// Configuration for [`Toggle::paint`], the immediate-mode switch face.
/// Built fluently: `ToggleFace::new(rect, on).hovered(hit)`. The knob scales
/// to the rect height so callers can paint switches of any size while keeping
/// the retained widget's proportions.
pub struct ToggleFace {
    rect: Rect,
    on: bool,
    interact: ToggleInteract,
}

impl ToggleFace {
    pub fn new(rect: Rect, on: bool) -> Self {
        Self { rect, on, interact: ToggleInteract::default() }
    }
    pub fn hovered(mut self, v: bool) -> Self { self.interact.hovered = v; self }
    pub fn focused(mut self, v: bool) -> Self { self.interact.focused = v; self }
    pub fn disabled(mut self, v: bool) -> Self { self.interact.disabled = v; self }
    /// The resolved `(track, knob, border, border_width)` palette for the
    /// current state, without painting. Exposed for tests and callers.
    pub fn colors(&self, theme: &Theme) -> (vello::peniko::Color, vello::peniko::Color, vello::peniko::Color, f64) {
        toggle_face_colors(theme, self.on, self.interact)
    }
}

impl Toggle {
    /// Immediate-mode paint of a themed switch inside `face.rect`, matching
    /// the retained [`Toggle`]'s look (track fill on/off, focus ring, knob
    /// slid to the on/off edge, disabled dim). For canvas/immediate-mode apps
    /// that own their own rect hit-testing: call this to paint, keep your
    /// existing hit-test to fire the toggle. Extends the widget rather than
    /// forking a second painter — the retained draw and every converted app
    /// now share [`toggle_face_colors`].
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        face: &ToggleFace,
    ) {
        let (track, knob, border, border_w) = toggle_face_colors(theme, face.on, face.interact);
        let radius = face.rect.height() / 2.0;
        let track_shape = RoundedRect::from_rect(face.rect, radius);
        let dim = face.interact.disabled;
        if dim {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &track_shape,
            );
        }
        scene.fill(vello::peniko::Fill::NonZero, transform, track, None, &track_shape);
        scene.stroke(&Stroke::new(border_w), transform, border, None, &track_shape);
        // Knob: radius scaled to the rect with a 2-px edge gap, matching the
        // retained widget's proportions at its 44×24 default (knob r = 10).
        let kr = (radius - 2.0).max(1.0);
        let cy = face.rect.y0 + face.rect.height() / 2.0;
        let cx = if face.on {
            face.rect.x1 - kr - 2.0
        } else {
            face.rect.x0 + kr + 2.0
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            knob,
            None,
            &Circle::new(Point::new(cx, cy), kr),
        );
        if dim {
            scene.pop_layer();
        }
    }
}

impl Widget for Toggle {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(Some((val, _))) = state.get(root, self.on_toggle_path.as_str()) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    self.is_checked = *b;
                }
            }
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(44.0),
                height: Dimension::length(24.0),
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
        // Disabled widgets paint their (forced-idle) look under a reduced
        // group opacity so they read as non-interactive.
        let disabled = !self.enabled;
        if disabled {
            let clip = RoundedRect::from_rect(self.rect, theme.tokens.radii.full as f64);
            scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), crate::widget::DISABLED_ALPHA, transform, &clip);
        }
        // Palette resolved from the shared source of truth so the retained
        // switch and every immediate-mode `Toggle::paint` caller stay in lock-step.
        let interact = ToggleInteract {
            hovered: self.is_hovered,
            focused: self.is_focused,
            disabled,
        };
        let (track_color, knob_color, stroke_color, stroke_w) =
            toggle_face_colors(theme, self.is_checked, interact);

        let track_shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.full as f64);
        scene.fill(vello::peniko::Fill::NonZero, transform, track_color, None, &track_shape);

        scene.stroke(&Stroke::new(stroke_w), transform, stroke_color, None, &track_shape);

        // Knob
        let knob_radius = 10.0;
        let knob_x = if self.is_checked {
            self.rect.x1 - knob_radius - 2.0
        } else {
            self.rect.x0 + knob_radius + 2.0
        };
        let knob_y = self.rect.y0 + self.rect.height() / 2.0;

        let knob_shape = Circle::new(Point::new(knob_x, knob_y), knob_radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, knob_color, None, &knob_shape);

        if disabled {
            scene.pop_layer();
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        // Disabled: reject all input — no toggle, no hover feedback.
        if !self.enabled { return; }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                if self.is_hovered && *element_state == ElementState::Pressed {
                    *handled = true;
                    *focus_target = Some(node_id);
                    // CRDT Mutation
                    let new_val = !self.is_checked;
                    let _ = state.put(root, self.on_toggle_path.as_str(), automerge::ScalarValue::Boolean(new_val));
                    // Update the local cache now so accessibility() reflects the
                    // new state this same frame — otherwise the focus narration
                    // triggered by this click would speak the pre-click state.
                    self.is_checked = new_val;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Toggle" }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("toggle"))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
    fn is_enabled(&self) -> bool { self.enabled }

    /// Keyboard / AT activation: Enter, Space, or an AccessKit `Activate` /
    /// `ToggleCheck` flips the switch — the same mutation the mouse path does.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        use crate::a11y::Action;
        if !self.enabled {
            return false;
        }
        match action {
            Action::ToggleCheck | Action::Activate => {
                let new_val = !self.is_checked;
                let _ = state.put(
                    path,
                    self.on_toggle_path.as_str(),
                    automerge::ScalarValue::Boolean(new_val),
                );
                // Keep accessibility() consistent within this frame (mirrors
                // the optimistic update the next sync would apply).
                self.is_checked = new_val;
                true
            }
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A toggle is a Switch: the on/off state lives in `checked`. An
        // explicit accessible name covers label-less switches; otherwise a
        // sibling label is resolved by the tree builder.
        let name = self
            .accessible_name
            .as_ref()
            .map(|n| crate::i18n!(n))
            .filter(|n| !n.is_empty());
        Some(
            AccessNode::new(Role::Switch)
                .with_optional_name(name)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    checked: Some(self.is_checked),
                    ..Default::default()
                }),
        )
    }

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
        if presence.should_cull(self.get_rect()) { return; }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
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
    use crate::widget::Widget;

    #[test]
    fn toggle_test_id_default_is_toggle() {
        let t = Toggle::new("any", "path");
        assert_eq!(Widget::test_id(&t), Some("toggle"));
    }

    #[test]
    fn toggle_test_id_can_be_overridden() {
        let t = Toggle::new("any", "path").with_id("airplane_mode");
        assert_eq!(Widget::test_id(&t), Some("airplane_mode"));
    }

    #[test]
    fn with_enabled_false_is_reported_by_is_enabled() {
        let t = Toggle::new("any", "path").with_enabled(false);
        assert!(!Widget::is_enabled(&t));
        assert!(Widget::is_enabled(&Toggle::new("any", "path")), "default is enabled");
    }

    #[test]
    fn accessibility_reports_switch_role_and_checked_state() {
        use crate::a11y::Role;
        let mut t = Toggle::new("any", "path");
        let off = Widget::accessibility(&t).unwrap();
        assert_eq!(off.role, Role::Switch);
        assert_eq!(off.state.checked, Some(false));

        t.is_checked = true; // same module → set the synced on-state directly
        let on = Widget::accessibility(&t).unwrap();
        assert_eq!(on.state.checked, Some(true));
    }

    #[test]
    fn accessibility_uses_accessible_name_when_set() {
        // A label-less toggle has no name by default, but `with_accessible_name`
        // gives the switch a spoken identity.
        let plain = Toggle::new("any", "path");
        assert_eq!(Widget::accessibility(&plain).unwrap().name, None);

        let named = Toggle::new("any", "path").with_accessible_name("Airplane mode");
        assert_eq!(
            Widget::accessibility(&named).unwrap().name.as_deref(),
            Some("Airplane mode"),
        );
    }

    #[test]
    fn toggle_face_colors_track_tracks_on_state() {
        let theme = Theme::dark();
        let off = toggle_face_colors(&theme, false, ToggleInteract::default());
        let on = toggle_face_colors(&theme, true, ToggleInteract::default());
        assert_eq!(on.0, theme.accent_primary, "on-track is the accent fill");
        assert_eq!(off.0, theme.tokens.colors.neutral[3], "off-track is neutral");
        assert_ne!(on.0, off.0, "on and off tracks must differ");
    }

    #[test]
    fn toggle_face_colors_focus_swaps_border_and_weight() {
        let theme = Theme::high_contrast();
        let rest = toggle_face_colors(&theme, true, ToggleInteract::default());
        let focused = toggle_face_colors(
            &theme,
            true,
            ToggleInteract { focused: true, ..Default::default() },
        );
        assert_eq!(focused.2, theme.border_focus, "focused border is the focus ring colour");
        assert_eq!(focused.3, 2.0, "focused border is 2px");
        assert_eq!(rest.2, theme.tokens.colors.border_standard);
        assert_eq!(rest.3, 1.0);
    }

    #[test]
    fn toggle_face_matches_shared_colors() {
        // The immediate-mode face resolves through the same source of truth
        // as the retained widget, so a face's palette equals the free fn.
        let theme = Theme::dark();
        let face = ToggleFace::new(Rect::new(0.0, 0.0, 44.0, 24.0), true).hovered(true);
        assert_eq!(
            face.colors(&theme),
            toggle_face_colors(&theme, true, ToggleInteract { hovered: true, ..Default::default() }),
        );
    }

    #[test]
    fn disabled_toggle_does_not_toggle() {
        use winit::event::DeviceId;

        let mut t = Toggle::new("id", "on").with_enabled(false);

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = t.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(60.0),
                height: taffy::AvailableSpace::Definite(40.0),
            },
        )
        .unwrap();
        t.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut focus = None;
        let mut handled = false;

        t.handle_event(
            &WindowEvent::CursorMoved { device_id: device, position: (10.0, 10.0).into() },
            1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &tree,
        );
        t.handle_event(
            &WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left },
            1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &tree,
        );

        assert!(focus.is_none(), "disabled toggle must not request focus");
        assert!(!handled, "disabled toggle must not swallow input");
        assert!(
            doc.get(&root, "on").unwrap().is_none(),
            "disabled toggle must not write the CRDT path"
        );
    }
}
