use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

/// Interaction state for an immediate-mode checkbox face. Canvas apps keep
/// their own hit-testing while sharing the retained checkbox's visual states.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CheckboxInteract {
    pub hovered: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// Stateless checkbox presentation for hand-laid-out canvases.
///
/// [`Checkbox`] remains the right choice when the value is bound directly to
/// an Automerge path. `CheckboxFace` avoids every immediate-mode app copying
/// the box, focus border, disabled treatment, and proportional check mark.
pub struct CheckboxFace {
    rect: Rect,
    checked: bool,
    interact: CheckboxInteract,
    fill_unchecked: bool,
}

impl CheckboxFace {
    pub fn new(rect: Rect, checked: bool) -> Self {
        Self {
            rect,
            checked,
            interact: CheckboxInteract::default(),
            fill_unchecked: true,
        }
    }

    pub fn hovered(mut self, hovered: bool) -> Self {
        self.interact.hovered = hovered;
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.interact.focused = focused;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.interact.disabled = disabled;
        self
    }

    /// Let the surrounding canvas provide the unchecked background. Useful
    /// for checkboxes embedded in rows and cards rather than standalone forms.
    pub fn transparent_unchecked(mut self) -> Self {
        self.fill_unchecked = false;
        self
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag (is_hovered, is_checked, is_focused, enabled) is an independent piece of \
              widget state — pointer hover, CRDT-synced checked value, keyboard focus, and \
              build-time disabled config — not a combinatorial options API"
)]
pub struct Checkbox {
    _id: String,
    rect: Rect,
    is_hovered: bool,
    is_checked: bool,
    is_focused: bool,
    on_toggle_path: String,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When `false` the checkbox is disabled: skipped by focus, rejects
    /// input (no toggle, no hover feedback) and renders dimmed. Default `true`.
    enabled: bool,
    /// Optional override for `Widget::test_id()`. Defaults to `"checkbox"`.
    id_override: Option<String>,
    /// Spoken name for a checkbox with no adjacent text label. When unset
    /// the checkbox relies on a sibling label resolved by the tree builder.
    accessible_name: Option<String>,
}

impl Checkbox {
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
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Override the value returned from `Widget::test_id()`. Without a
    /// custom id the checkbox reports `"checkbox"`; callers with multiple
    /// checkboxes in the same surface can disambiguate them, e.g.
    /// `with_id("remember_me")`.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id_override = Some(id.to_string());
        self
    }

    /// Set the enabled state at build time. A disabled checkbox is skipped
    /// by focus, rejects input and renders dimmed. Default `true`.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Give a label-less checkbox a spoken name so a screen reader
    /// announces what it controls rather than just "checkbox, checked".
    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    /// Imperatively flip the enabled state.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Paint a checkbox without taking ownership of its state or events.
    pub fn paint(scene: &mut Scene, transform: Affine, theme: &Theme, face: &CheckboxFace) {
        let shape = RoundedRect::from_rect(face.rect, f64::from(theme.tokens.radii.md));
        if face.interact.disabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }

        let box_color = if face.checked {
            theme.accent_primary
        } else if face.interact.hovered {
            theme.bg_hover
        } else {
            theme.bg_active
        };
        if face.checked || face.fill_unchecked || face.interact.hovered {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                box_color,
                None,
                &shape,
            );
        }

        let stroke_color = if face.interact.focused {
            theme.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if face.interact.focused { 2.0 } else { 1.0 }),
            transform,
            stroke_color,
            None,
            &shape,
        );

        if face.checked {
            let mut path = BezPath::new();
            path.move_to(Point::new(
                face.rect.x0 + face.rect.width() * 0.25,
                face.rect.y0 + face.rect.height() * 0.50,
            ));
            path.line_to(Point::new(
                face.rect.x0 + face.rect.width() * 0.42,
                face.rect.y0 + face.rect.height() * 0.67,
            ));
            path.line_to(Point::new(
                face.rect.x0 + face.rect.width() * 0.75,
                face.rect.y0 + face.rect.height() * 0.25,
            ));
            scene.stroke(
                &Stroke::new((face.rect.width().min(face.rect.height()) * 0.105).max(1.5))
                    .with_caps(vello::kurbo::Cap::Round)
                    .with_join(vello::kurbo::Join::Round),
                transform,
                theme.tokens.colors.ink_on_any(
                    &[box_color],
                    Color::WHITE,
                    crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                ),
                None,
                &path,
            );
        }

        if face.interact.disabled {
            scene.pop_layer();
        }
    }
}

impl Widget for Checkbox {
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
                width: Dimension::length(24.0),
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
        Self::paint(
            scene,
            transform,
            theme,
            &CheckboxFace::new(self.rect, self.is_checked)
                .hovered(self.is_hovered)
                .focused(self.is_focused)
                .disabled(!self.enabled),
        );
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
        // Disabled: reject all input — no toggle, no hover feedback.
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
                // Update the local cache now so accessibility() reflects the
                // new state this same frame — otherwise the focus narration
                // triggered by this click would speak the pre-click state.
                self.is_checked = new_val;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Checkbox"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id_override.as_deref().unwrap_or("checkbox"))
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

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The checkbox's own label is usually a sibling `Text`, resolved by
        // the tree builder; an explicit accessible name covers the label-less
        // case so the control isn't anonymous. The checked state is the
        // load-bearing semantic.
        let name = self
            .accessible_name
            .as_ref()
            .map(|n| crate::i18n!(n))
            .filter(|n| !n.is_empty());
        Some(
            AccessNode::new(Role::Checkbox)
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

    /// Accessibility: an AT toggles the checkbox with
    /// [`Action::ToggleCheck`](crate::a11y::Action::ToggleCheck) or the generic
    /// [`Action::Activate`](crate::a11y::Action::Activate). Both flip the
    /// checked state through the exact CRDT write a real click performs. A
    /// disabled checkbox rejects the action.
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
                // Mirror the optimistic local update the event path does on
                // the next `sync`; setting it here keeps `accessibility()`
                // consistent within the same frame.
                self.is_checked = new_val;
                true
            }
            _ => false,
        }
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
    fn checkbox_test_id_default_is_checkbox() {
        let c = Checkbox::new("any", "path");
        assert_eq!(Widget::test_id(&c), Some("checkbox"));
    }

    #[test]
    fn checkbox_test_id_can_be_overridden() {
        let c = Checkbox::new("any", "path").with_id("remember_me");
        assert_eq!(Widget::test_id(&c), Some("remember_me"));
    }

    #[test]
    fn immediate_face_preserves_canvas_owned_state() {
        let rect = Rect::new(2.0, 3.0, 18.0, 19.0);
        let face = CheckboxFace::new(rect, true)
            .hovered(true)
            .focused(true)
            .disabled(true)
            .transparent_unchecked();
        assert_eq!(face.rect, rect);
        assert!(face.checked);
        assert!(face.interact.hovered);
        assert!(face.interact.focused);
        assert!(face.interact.disabled);
        assert!(!face.fill_unchecked);
    }

    #[test]
    fn with_enabled_false_is_reported_by_is_enabled() {
        let c = Checkbox::new("any", "path").with_enabled(false);
        assert!(!Widget::is_enabled(&c));
        assert!(
            Widget::is_enabled(&Checkbox::new("any", "path")),
            "default is enabled"
        );
    }

    #[test]
    fn accessibility_unchecked_default() {
        use crate::a11y::Role;
        let c = Checkbox::new("any", "path");
        let node = Widget::accessibility(&c).unwrap();
        assert_eq!(node.role, Role::Checkbox);
        assert_eq!(node.state.checked, Some(false));
        assert!(!node.state.disabled);
        assert!(node.state.focusable);
    }

    #[test]
    fn accessibility_uses_accessible_name_when_set() {
        // No name by default (the label is usually a sibling); the explicit
        // accessible name names a checkbox that stands alone.
        let plain = Checkbox::new("any", "path");
        assert_eq!(Widget::accessibility(&plain).unwrap().name, None);

        let named = Checkbox::new("any", "path").with_accessible_name("Remember me");
        assert_eq!(
            Widget::accessibility(&named).unwrap().name.as_deref(),
            Some("Remember me"),
        );
    }

    #[test]
    fn accessibility_disabled_checked_reports_checked_and_disabled() {
        use crate::a11y::Role;
        let mut c = Checkbox::new("any", "path").with_enabled(false);
        c.is_checked = true; // same module → can set the synced state directly
        let node = Widget::accessibility(&c).unwrap();
        assert_eq!(node.role, Role::Checkbox);
        assert_eq!(node.state.checked, Some(true), "checked state surfaces");
        assert!(node.state.disabled, "disabled surfaces");
    }

    #[test]
    fn perform_action_toggle_check_flips_state() {
        use crate::a11y::Action;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut c = Checkbox::new("any", "checked");
        assert!(!c.is_checked);
        assert!(Widget::perform_action(
            &mut c,
            &Action::ToggleCheck,
            &mut doc,
            &root
        ));
        assert!(c.is_checked, "ToggleCheck flips to checked");
        // Activate also toggles.
        assert!(Widget::perform_action(
            &mut c,
            &Action::Activate,
            &mut doc,
            &root
        ));
        assert!(!c.is_checked, "Activate flips back");
    }

    #[test]
    fn disabled_checkbox_rejects_perform_action() {
        use crate::a11y::Action;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut c = Checkbox::new("any", "checked").with_enabled(false);
        assert!(!Widget::perform_action(
            &mut c,
            &Action::ToggleCheck,
            &mut doc,
            &root
        ));
        assert!(!c.is_checked, "a disabled checkbox must not toggle");
    }

    #[test]
    fn disabled_checkbox_does_not_toggle() {
        use winit::event::DeviceId;

        let mut c = Checkbox::new("id", "checked").with_enabled(false);

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = c.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(50.0),
                height: taffy::AvailableSpace::Definite(50.0),
            },
        )
        .unwrap();
        c.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut focus = None;
        let mut handled = false;

        c.handle_event(
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
        c.handle_event(
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

        assert!(focus.is_none(), "disabled checkbox must not request focus");
        assert!(!handled, "disabled checkbox must not swallow input");
        assert!(
            doc.get(&root, "checked").unwrap().is_none(),
            "disabled checkbox must not write the CRDT path"
        );
    }
}
