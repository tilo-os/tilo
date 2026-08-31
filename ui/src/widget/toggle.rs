use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

/// `is_hovered`, `is_checked`, `is_focused` and `enabled` are four
/// independent state axes (pointer, CRDT-synced value, keyboard focus,
/// build-time enablement), each set/read by name — not positional bool
/// arguments — so bundling them into a bitflags type would touch every
/// read/write site here for no behavioural change.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent state axes, all set/read by name, not positional bool args"
)]
pub struct Toggle {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    is_focused: bool,
    on_toggle_path: String,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    enabled: bool,
    id_override: Option<String>,
    /// Spoken name for a label-less toggle (e.g. an icon-only switch);
    /// unset falls back to a sibling label from the tree builder.
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

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Default `test_id()` is `"toggle"`; override to disambiguate multiple
    /// toggles in one surface.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    /// Set the enabled state at build time. A disabled toggle is skipped
    /// by focus, rejects input and renders dimmed. Default `true`.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// Interaction state for the immediate-mode toggle face; the retained
/// [`Toggle`] tracks these itself. A canvas app doing its own hit-testing
/// usually only knows `hovered`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ToggleInteract {
    pub hovered: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// Hover is visible without a colour swap: the knob is pushed this multiple
/// of the contrast it already reached, rather than resolved to a second tone.
const HOVER_KNOB_GAIN: f64 = 1.4;

/// Single source of truth for both the retained [`Toggle`]'s draw and
/// [`Toggle::paint`], so every switch shares one palette per state.
pub fn toggle_face_colors(
    theme: &Theme,
    on: bool,
    interact: ToggleInteract,
) -> (
    vello::peniko::Color,
    vello::peniko::Color,
    vello::peniko::Color,
    f64,
) {
    let track = if on {
        theme.accent_primary
    } else {
        theme.tokens.colors.neutral[3]
    };
    // Both knob tones are chosen for the off-track; on-track they collapse
    // into it (1.16:1 on dark) — no visible knob, the state that matters.
    // Resolving each tone separately breaks on light accents (the dim tone
    // would jump near-white to near-black under the pointer), so the
    // resting knob is resolved once and hover pushes it further rather
    // than swapping to a second tone.
    let knob = theme.tokens.colors.ink_on_any(
        &[track],
        theme.text_dim,
        crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
    );
    let knob = if interact.hovered {
        let reached = crate::a11y::contrast_ratio(knob, track);
        theme
            .tokens
            .colors
            .ink_on_any(&[track], knob, reached * HOVER_KNOB_GAIN)
    } else {
        knob
    };
    let (border, border_w) = if interact.focused {
        (theme.border_focus, 2.0)
    } else {
        (theme.tokens.colors.border_standard, 1.0)
    };
    (track, knob, border, border_w)
}

pub struct ToggleFace {
    rect: Rect,
    on: bool,
    interact: ToggleInteract,
    on_color: Option<vello::peniko::Color>,
}

impl ToggleFace {
    pub fn new(rect: Rect, on: bool) -> Self {
        Self {
            rect,
            on,
            interact: ToggleInteract::default(),
            on_color: None,
        }
    }
    #[must_use]
    pub fn hovered(mut self, v: bool) -> Self {
        self.interact.hovered = v;
        self
    }
    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.interact.focused = v;
        self
    }
    #[must_use]
    pub fn disabled(mut self, v: bool) -> Self {
        self.interact.disabled = v;
        self
    }
    /// Override the active track for a semantic switch (for example a
    /// connection-status switch). Off, focus, hover, and contrast behavior
    /// remain shared.
    #[must_use]
    pub fn on_color(mut self, color: vello::peniko::Color) -> Self {
        self.on_color = Some(color);
        self
    }
    pub fn colors(
        &self,
        theme: &Theme,
    ) -> (
        vello::peniko::Color,
        vello::peniko::Color,
        vello::peniko::Color,
        f64,
    ) {
        let (mut track, mut knob, border, border_w) =
            toggle_face_colors(theme, self.on, self.interact);
        if self.on
            && let Some(color) = self.on_color
        {
            track = color;
            knob = theme.tokens.colors.ink_on_any(
                &[track],
                theme.text_dim,
                crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
            );
            if self.interact.hovered {
                let reached = crate::a11y::contrast_ratio(knob, track);
                knob = theme
                    .tokens
                    .colors
                    .ink_on_any(&[track], knob, reached * HOVER_KNOB_GAIN);
            }
        }
        (track, knob, border, border_w)
    }
}

/// Build the accessibility node shared by retained toggles and
/// context-specific toggle controls that own a different mutation channel.
pub fn toggle_accessibility(
    on: bool,
    focused: bool,
    enabled: bool,
    accessible_name: Option<&str>,
) -> crate::a11y::AccessNode {
    use crate::a11y::{AccessNode, AccessState, Role};
    AccessNode::new(Role::Switch)
        .with_optional_name(
            accessible_name
                .map(|name| crate::i18n!(name))
                .filter(|name| !name.is_empty()),
        )
        .with_state(AccessState {
            focusable: enabled,
            focused,
            disabled: !enabled,
            checked: Some(on),
            ..Default::default()
        })
}

impl Toggle {
    /// Paints a themed switch inside `face.rect`, matching the retained
    /// [`Toggle`]'s look. Callers keep their own hit-testing; only the
    /// palette ([`toggle_face_colors`]) is shared.
    pub fn paint(scene: &mut Scene, transform: Affine, theme: &Theme, face: &ToggleFace) {
        let (track, knob, border, border_w) = face.colors(theme);
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            track,
            None,
            &track_shape,
        );
        scene.stroke(
            &Stroke::new(border_w),
            transform,
            border,
            None,
            &track_shape,
        );
        // 2-px edge gap; matches the retained widget's 44×24 default (r = 10).
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
        if let Ok(Some((val, _))) = state.get(root, self.on_toggle_path.as_str())
            && let Value::Scalar(s) = val
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
        {
            self.is_checked = *b;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(44.0),
                height: Dimension::length(24.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        let disabled = !self.enabled;
        if disabled {
            let clip = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.full));
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &clip,
            );
        }
        let interact = ToggleInteract {
            hovered: self.is_hovered,
            focused: self.is_focused,
            disabled,
        };
        let (track_color, knob_color, stroke_color, stroke_w) =
            toggle_face_colors(theme, self.is_checked, interact);

        let track_shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.full));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            track_color,
            None,
            &track_shape,
        );

        scene.stroke(
            &Stroke::new(stroke_w),
            transform,
            stroke_color,
            None,
            &track_shape,
        );

        let knob_radius = 10.0;
        let knob_x = if self.is_checked {
            self.rect.x1 - knob_radius - 2.0
        } else {
            self.rect.x0 + knob_radius + 2.0
        };
        let knob_y = self.rect.y0 + self.rect.height() / 2.0;

        let knob_shape = Circle::new(Point::new(knob_x, knob_y), knob_radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            knob_color,
            None,
            &knob_shape,
        );

        if disabled {
            scene.pop_layer();
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.enabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } if self.is_hovered && *element_state == ElementState::Pressed => {
                *handled = true;
                *focus_target = Some(node_id);
                let new_val = !self.is_checked;
                let _ = state.put(
                    root,
                    self.on_toggle_path.as_str(),
                    automerge::ScalarValue::Boolean(new_val),
                );
                // Set now, not on next sync: focus narration triggered by
                // this same click must not speak the pre-click state.
                self.is_checked = new_val;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Toggle"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("toggle"))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// AT/keyboard activation (Enter/Space route to `Activate`/`ToggleCheck`
    /// upstream) — same mutation as the mouse path.
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
                self.is_checked = new_val;
                true
            }
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(toggle_accessibility(
            self.is_checked,
            self.is_focused(),
            self.is_enabled(),
            self.accessible_name.as_deref(),
        ))
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
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
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope()
            && !permissions.check(&scope)
        {
            let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                overlay,
                None,
                &self.rect,
            );
        }

        if let Some(key) = self.presence_key()
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
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
        assert!(
            Widget::is_enabled(&Toggle::new("any", "path")),
            "default is enabled"
        );
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
        assert_eq!(
            off.0, theme.tokens.colors.neutral[3],
            "off-track is neutral"
        );
        assert_ne!(on.0, off.0, "on and off tracks must differ");
    }

    #[test]
    fn toggle_face_colors_focus_swaps_border_and_weight() {
        let theme = Theme::high_contrast();
        let rest = toggle_face_colors(&theme, true, ToggleInteract::default());
        let focused = toggle_face_colors(
            &theme,
            true,
            ToggleInteract {
                focused: true,
                ..Default::default()
            },
        );
        assert_eq!(
            focused.2, theme.border_focus,
            "focused border is the focus ring colour"
        );
        assert_eq!(focused.3, 2.0, "focused border is 2px");
        assert_eq!(rest.2, theme.tokens.colors.border_standard);
        assert_eq!(rest.3, 1.0);
    }

    #[test]
    fn toggle_face_matches_shared_colors() {
        let theme = Theme::dark();
        let face = ToggleFace::new(Rect::new(0.0, 0.0, 44.0, 24.0), true).hovered(true);
        assert_eq!(
            face.colors(&theme),
            toggle_face_colors(
                &theme,
                true,
                ToggleInteract {
                    hovered: true,
                    ..Default::default()
                }
            ),
        );
    }

    #[test]
    fn toggle_face_accepts_semantic_active_track() {
        let theme = Theme::dark();
        let semantic = theme.tokens.colors.semantic.success;
        let face = ToggleFace::new(Rect::new(0.0, 0.0, 44.0, 24.0), true).on_color(semantic);
        let (track, knob, _, _) = face.colors(&theme);
        assert_eq!(track, semantic);
        assert!(
            crate::a11y::contrast_ratio(knob, track)
                >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST
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
            &WindowEvent::CursorMoved {
                device_id: device,
                position: (10.0, 10.0).into(),
            },
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
        t.handle_event(
            &WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );

        assert!(focus.is_none(), "disabled toggle must not request focus");
        assert!(!handled, "disabled toggle must not swallow input");
        assert!(
            doc.get(&root, "on").unwrap().is_none(),
            "disabled toggle must not write the CRDT path"
        );
    }
}
