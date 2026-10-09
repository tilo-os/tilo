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
//!   * on click / Enter / Space / assistive activation fires its
//!     [`Control`]: the published action the
//!     [`crate::installer_app::InstallerApp`] runs through its action door.
//!
//! Every colour is read from the [`Theme`]; nothing is hard-coded.

use std::collections::HashMap;
use ui::widget::ModelPath;
use ui::widget::TaffyTreeExt;

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

use crate::control::Control;

const HEIGHT: f32 = 44.0;
const LABEL_SIZE: f32 = 15.0;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent interaction/lifecycle flag (primary, quiet, large, disabled, hover, press, focus), not a combinatorial state machine"
)]
pub struct ActionButton {
    label: String,
    /// The published action the button runs when it fires.
    control: Control,
    /// Stable E2E `test_id`. Defaults to the label, but the label can change
    /// (e.g. a checkbox-style toggle), so it can be pinned via
    /// [`ActionButton::with_test_id`] to stay addressable across rebuilds.
    test_id: Option<String>,
    /// `Some` turns the action into a checkbox with the supplied checked
    /// state. Legacy callers may include a compact `[x]` prefix; drawing and
    /// assistive technology both strip it in favour of a real checkbox mark.
    checked: Option<bool>,
    primary: bool,
    /// Text-link look for a low-key alternative: no fill, no hairline.
    quiet: bool,
    /// A taller button with a larger label.
    large: bool,
    /// A fixed width instead of sharing the row.
    width: Option<f32>,
    disabled: bool,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
}

impl ActionButton {
    /// A secondary (neutral) button firing `control` on activation.
    pub fn new(label: &str, control: Control) -> Self {
        Self {
            label: label.to_string(),
            control,
            test_id: None,
            checked: None,
            primary: false,
            quiet: false,
            large: false,
            width: None,
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

    /// A low-key alternative drawn like a text link: dim label, no fill or
    /// hairline until hovered. Still a full-size, focusable target.
    #[must_use]
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// A step's one deliberate action: taller, with a larger label.
    #[must_use]
    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }

    /// Keep this width instead of sharing the row, so the button can stand
    /// centred on its own.
    #[must_use]
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = Some(width);
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

    fn fire(&self) {
        if self.disabled {
            return;
        }
        self.control.fire();
    }

    /// Fill and label colour by variant and interaction state.
    fn face(&self, theme: &Theme) -> (Color, Color) {
        let raised = theme.tokens.colors.surfaces.raised;
        if self.disabled {
            (raised, theme.text_dim)
        } else if self.quiet {
            let bg = if self.is_pressed {
                theme.tokens.colors.lift_on(raised, 1.0)
            } else if self.is_hovered {
                theme.tokens.colors.lift_on(raised, 0.5)
            } else {
                Color::TRANSPARENT
            };
            (
                bg,
                if self.is_hovered || self.is_focused {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
            )
        } else if self.primary {
            let bg = if self.is_pressed || self.is_hovered {
                theme.tokens.colors.intent.inverted_hover
            } else {
                theme.tokens.colors.intent.inverted
            };
            (bg, theme.tokens.colors.intent.on_inverted)
        } else {
            let bg = if self.is_pressed {
                theme.tokens.colors.lift_on(raised, 1.0)
            } else if self.is_hovered {
                theme.tokens.colors.lift_on(raised, 0.5)
            } else {
                raised
            };
            (bg, theme.text_primary)
        }
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

    use automerge::{AutoCommit, ROOT, ReadDoc};

    use super::*;

    use ui::synthetic_input::named_key;

    fn assert_key_fires(key: NamedKey) {
        let (control, presses) = Control::detached("installer", "back", serde_json::Value::Null);
        let mut button = ActionButton::new("Back", control);
        button.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = button.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let mut handled = false;
        let mut focus_target = None;
        button.handle_event(
            &named_key(key),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(&mut doc),
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        let fired = presses.take().expect("the button fires its action");
        assert_eq!(
            fired.address.encode(),
            tilo_actions::ActionAddress::new("installer", "back").encode()
        );
        assert!(presses.take().is_none(), "one activation is one press");
        assert_eq!(doc.length(ROOT), 0, "a button never writes the document");
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
            let (control, presses) =
                Control::detached("installer.power", "reboot", serde_json::Value::Null);
            let mut button = ActionButton::new("Reboot now", control).with_test_id("reboot");
            button.set_focus(true);
            let node = button.accessibility().expect("button semantics");
            assert_eq!(node.role, ui::a11y::Role::Button);
            assert_eq!(node.name.as_deref(), Some("Reboot now"));
            assert!(node.state.focusable);
            assert!(node.state.focused);

            let mut doc = AutoCommit::new();
            assert!(button.perform_action(
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert_eq!(
                presses.take().map(|press| press.address.action),
                Some("reboot".to_string())
            );
            assert_eq!(doc.length(ROOT), 0);
            assert!(button.is_focused(), "AT activation must not steal focus");
            // A disabled button runs nothing.
            let mut disabled = button.disabled(true);
            assert!(!disabled.perform_action(
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().is_none());
        }
        // Phase `recovery_ack_is_a_cleanly_named_native_checkbox`.
        {
            let (control, _) = Control::detached(
                "installer.review",
                "set-recovery-promise",
                serde_json::json!({ "acknowledged": false }),
            );
            let button = ActionButton::new("[x] I will save my recovery key", control)
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
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only errors for a corrupted tree; `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so surfacing a
        // theoretical error here would mean threading a `Result` through the
        // whole toolkit for a branch that can't be taken.
        let height = if self.large {
            (theme.tokens.spacing.s12 + theme.tokens.spacing.s2)
                .max(theme.text(ui::tokens::TextRole::Heading).size + theme.tokens.spacing.s6)
        } else {
            HEIGHT
        };
        let style = match self.width {
            Some(width) => Style {
                size: Size {
                    width: Dimension::length(width),
                    height: Dimension::length(height),
                },
                flex_grow: 0.0,
                flex_shrink: 1.0,
                ..Default::default()
            },
            None => Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(height),
                },
                // Grow to share the nav row's width; basis 0 so two
                // buttons split evenly and a lone button fills the row.
                flex_grow: 1.0,
                flex_basis: length(0.0_f32),
                flex_shrink: 1.0,
                ..Default::default()
            },
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(l, origin);
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

        let (bg, label_color) = self.face(theme);
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
        } else if !self.primary && !self.quiet {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &shape,
            );
        }

        let label = self.accessible_label();
        let label_size = if self.large {
            theme.tokens.typography.size(ui::tokens::TextRole::Heading)
        } else {
            LABEL_SIZE
        };
        let (tw, th) = text_renderer.measure(label, label_size, None);
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
                    theme.tokens.colors.intent.inverted
                } else {
                    theme.tokens.colors.surfaces.raised
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
            label_size,
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
                            self.fire();
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
                self.fire();
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
        _state: &mut ui::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        let accepted = matches!(action, ui::a11y::Action::Activate)
            || (self.checked.is_some() && matches!(action, ui::a11y::Action::ToggleCheck));
        if self.disabled || !accepted {
            return false;
        }
        self.fire();
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let control_id = self.test_id().unwrap_or(self.label.as_str());
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
        vec![
            self.control
                .metadata(self.test_id().unwrap_or(self.label.as_str())),
        ]
    }
}
