//! [`ActionButton`] — the installer's primary/secondary call-to-action.
//!
//! `ui::Button` is fixed at 120×40 px and always paints the same neutral
//! "raised surface" fill, so it can't express a wizard's visual hierarchy
//! (a prominent accent "Next"/"Begin" vs. a quieter "Back") or fill a nav
//! row. `ActionButton` is a leaf widget that:
//!
//!   * grows to share its row's width (so a lone button fills the row and
//!     a Back/Next pair splits it evenly),
//!   * styles itself **primary** (accent fill, white label) or
//!     **secondary** (raised fill, neutral label),
//!   * supports a **disabled** state (dimmed, non-interactive, dropped
//!     from the focus order),
//!   * fires a CRDT boolean flag on click / Space — the rising-edge
//!     pattern the [`crate::installer_app::InstallerApp`] consumes, exactly
//!     like `ui::Button`'s `on_click_path`.
//!
//! Every colour is read from the [`Theme`]; nothing is hard-coded.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

const HEIGHT: f32 = 44.0;
const LABEL_SIZE: f32 = 15.0;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent interaction/lifecycle flag (primary, disabled, hover, press, focus), not a combinatorial state machine"
)]
pub struct ActionButton {
    label: String,
    /// CRDT path of the boolean flag raised when the button fires.
    click_path: String,
    /// Stable E2E `test_id`. Defaults to the label, but the label can change
    /// (e.g. a checkbox-style toggle), so it can be pinned via
    /// [`ActionButton::with_test_id`] to stay addressable across rebuilds.
    test_id: Option<String>,
    /// `Some` turns the action into a checkbox with the supplied checked
    /// state. Legacy callers may include a compact `[x]` prefix; drawing and
    /// assistive technology both strip it in favour of a real checkbox mark.
    checked: Option<bool>,
    primary: bool,
    disabled: bool,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
}

impl ActionButton {
    /// A secondary (neutral) button firing `click_path` on activation.
    pub fn new(label: &str, click_path: &str) -> Self {
        Self {
            label: label.to_string(),
            click_path: click_path.to_string(),
            test_id: None,
            checked: None,
            primary: false,
            disabled: false,
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
        }
    }

    /// Promote to the accent-filled primary call-to-action.
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Pin a stable E2E `test_id` (defaults to the label otherwise).
    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self
    }

    /// Expose this action as a native checkbox with a derived checked state.
    #[must_use]
    pub fn checkbox(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Mark disabled: dimmed, non-interactive, and removed from the focus
    /// order so keyboard nav skips it.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn fire(&self, state: &mut AutoCommit, root: &ObjId) {
        if self.disabled {
            return;
        }
        let _ = state.put(root, self.click_path.as_str(), true);
    }

    fn accessible_label(&self) -> &str {
        self.label
            .strip_prefix("[x] ")
            .or_else(|| self.label.strip_prefix("[ ] "))
            .unwrap_or(self.label.as_str())
    }
}

#[cfg(test)]
#[allow(
    clippy::items_after_test_module,
    reason = "the tests exercise both the inherent constructor above and Widget implementation below"
)]
mod tests {
    use automerge::{ROOT, ReadDoc};
    use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

    use super::*;

    fn press(key: NamedKey) -> WindowEvent {
        // SAFETY: ActionButton reads only the initialized KeyEvent fields.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(key);
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            // SAFETY: ActionButton does not inspect the device id.
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    fn assert_key_fires(key: NamedKey) {
        let mut button = ActionButton::new("Back", "installer.back");
        button.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = button.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let mut handled = false;
        let mut focus_target = None;
        button.handle_event(
            &press(key),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(
            doc.get(&ROOT, "installer.back")
                .unwrap()
                .and_then(|(value, _)| value.as_bool()),
            Some(true)
        );
        assert!(button.is_focused(), "activation must not move focus");
    }

    #[test]
    fn action_buttons_are_named_activatable_native_controls() {
        // Phase `enter_and_space_fire_the_focused_action`.
        {
            assert_key_fires(NamedKey::Enter);
            assert_key_fires(NamedKey::Space);
        }
        // Phase `button_semantics_are_named_focusable_and_activatable`.
        {
            let mut button =
                ActionButton::new("Reboot now", "installer.reboot").with_test_id("reboot");
            button.set_focus(true);
            let node = button.accessibility().expect("button semantics");
            assert_eq!(node.role, ui::a11y::Role::Button);
            assert_eq!(node.name.as_deref(), Some("Reboot now"));
            assert!(node.state.focusable);
            assert!(node.state.focused);

            let mut doc = AutoCommit::new();
            assert!(button.perform_action(&ui::a11y::Action::Activate, &mut doc, &ROOT));
            assert_eq!(
                doc.get(&ROOT, "installer.reboot")
                    .unwrap()
                    .and_then(|(value, _)| value.as_bool()),
                Some(true)
            );
            assert!(button.is_focused(), "AT activation must not steal focus");
        }
        // Phase `recovery_ack_is_a_cleanly_named_native_checkbox`.
        {
            let button =
                ActionButton::new("[x] I will save my recovery key", "installer.recovery_ack")
                    .with_test_id("recovery_ack")
                    .checkbox(true);
            let node = button.accessibility().expect("checkbox semantics");
            assert_eq!(node.role, ui::a11y::Role::Checkbox);
            assert_eq!(node.name.as_deref(), Some("I will save my recovery key"));
            assert_eq!(node.state.checked, Some(true));
        }
    }
}

impl Widget for ActionButton {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only errors for a corrupted tree; `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so surfacing a
        // theoretical error here would mean threading a `Result` through the
        // whole toolkit for a branch that can't be taken.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh style cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(HEIGHT),
                },
                // Grow to share the nav row's width; basis 0 so two
                // buttons split evenly and a lone button fills the row.
                flex_grow: 1.0,
                flex_basis: length(0.0_f32),
                flex_shrink: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);

        // Fill + label colour by variant and interaction state.
        let (bg, label_color) = if self.disabled {
            (theme.bg_primary, theme.text_dim)
        } else if self.primary {
            let bg = if self.is_pressed {
                theme.tokens.colors.accent[3]
            } else if self.is_hovered {
                theme.accent_hover
            } else {
                theme.accent_primary
            };
            (bg, Color::WHITE)
        } else {
            let bg = if self.is_pressed {
                theme.bg_active
            } else if self.is_hovered {
                theme.bg_hover
            } else {
                theme.bg_primary
            };
            (bg, theme.text_primary)
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);

        // Border: focus ring wins; secondary buttons keep a hairline.
        if self.is_focused && !self.disabled {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &shape,
            );
        } else if !self.primary {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &shape,
            );
        }

        let label = self.accessible_label();
        let (tw, th) = text_renderer.measure(label, LABEL_SIZE, None);
        let checkbox_width = self.checked.map_or(0.0, |_| 24.0);
        let group_half_width = f64::midpoint(f64::from(tw) + checkbox_width, 0.0);
        let tx = self.rect.center().x - group_half_width;
        let ty = self.rect.y0 + (self.rect.height() - f64::from(th)) / 2.0;
        if let Some(checked) = self.checked {
            let side = 16.0;
            let box_rect = Rect::new(
                tx,
                self.rect.center().y - side / 2.0,
                tx + side,
                self.rect.center().y + side / 2.0,
            );
            let box_shape = RoundedRect::from_rect(box_rect, 4.0);
            scene.fill(
                Fill::NonZero,
                transform,
                if checked {
                    theme.accent_primary
                } else {
                    theme.bg_primary
                },
                None,
                &box_shape,
            );
            scene.stroke(
                &Stroke::new(if checked { 1.5 } else { 1.0 }),
                transform,
                if checked {
                    Color::WHITE
                } else {
                    theme.border_standard
                },
                None,
                &box_shape,
            );
            if checked {
                let (cw, ch) = text_renderer.measure("✓", 12.0, None);
                text_renderer.draw_text(
                    scene,
                    "✓",
                    12.0,
                    Color::WHITE,
                    transform.pre_translate(Vec2::new(
                        box_rect.center().x - f64::from(cw) / 2.0,
                        box_rect.center().y - f64::from(ch) / 2.0,
                    )),
                    None,
                );
            }
        }
        text_renderer.draw_text(
            scene,
            label,
            LABEL_SIZE,
            label_color,
            transform.pre_translate(Vec2::new(tx + checkbox_width, ty)),
            None,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.disabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *ctx.handled = true;
                        if pressed {
                            *ctx.focus_target = Some(ctx.node_id);
                        } else {
                            // Fire on release (matches ui::Button).
                            self.fire(ctx.state, ctx.path);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter | NamedKey::Space),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                self.fire(ctx.state, ctx.path);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ActionButton"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.test_id.as_deref().unwrap_or(self.label.as_str()))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        !self.disabled
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn is_enabled(&self) -> bool {
        !self.disabled
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        root: &ModelPath,
    ) -> bool {
        let (state, root) = ui::doc::document(state, root);
        let accepted = matches!(action, ui::a11y::Action::Activate)
            || (self.checked.is_some() && matches!(action, ui::a11y::Action::ToggleCheck));
        if self.disabled || !accepted {
            return false;
        }
        self.fire(state, root);
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let control_id = self.test_id().unwrap_or(self.click_path.as_str());
        let clean_label = self.accessible_label();
        let role = if self.checked.is_some() {
            Role::Checkbox
        } else {
            Role::Button
        };
        Some(
            AccessNode::new(role)
                .with_semantic_key(control_id)
                .with_name(clean_label)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused,
                    disabled: self.disabled,
                    checked: self.checked,
                    ..Default::default()
                })
                .with_action_target(control_id),
        )
    }
}

impl ui::widget::controls::ActionControls for ActionButton {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        vec![ui::widget::ControlBindingMetadata {
            control_id: self
                .test_id()
                .unwrap_or(self.click_path.as_str())
                .to_string(),
            kind: ui::widget::ControlBindingKind::CollaborativeDocument(self.click_path.clone()),
        }]
    }
}
