//! [`ActionButton`] — the installer's primary/secondary call-to-action.
//!
//! A wizard button fills its nav row rather than sitting at a fixed width,
//! runs a published [`Control`] on activation, and can stand as a checkbox.
//! Its face is the toolkit's own ([`ButtonFace`]): a **primary** action is
//! the accent fill with its theme-resolved label, a **secondary** one the
//! raised control with its derived edge, and a **quiet** one the plain role's
//! text look. Every wizard button is the large control size: a wizard's
//! actions are the screen's one thing to do.
//!
//! Every colour, size, radius and edge is read from the [`Theme`]; nothing is
//! hard-coded.

use std::collections::HashMap;
use ui::widget::ModelPath;
use ui::widget::TaffyTreeExt;

use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize, Widget};

use crate::setup::destination::control::Control;

/// The size of a wizard button: the wizard's actions are what the screen is
/// for, so they take the large control.
const SIZE: ControlSize = ControlSize::Large;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent interaction/lifecycle flag (primary, quiet, disabled, hover, press, focus), not a combinatorial state machine"
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
    /// The checkbox's mark on its way in or out. The view builds the
    /// button anew on each change, so the motion goes on from the button
    /// it replaces (`motion::keep`, by its test id or label).
    check: ui::widget::CheckMotion,
    /// It has looked for the motion its predecessor left.
    check_taken: bool,
    primary: bool,
    /// Text-link look for a low-key alternative: no fill, no hairline.
    quiet: bool,
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
            check: ui::widget::CheckMotion::default(),
            check_taken: false,
            primary: false,
            quiet: false,
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

    /// The checkbox is still on its way.
    fn check_moving(&self) -> bool {
        self.checked.is_some() && self.check.is_moving(ui::motion::frame_now())
    }

    /// Its identity across rebuilds: its test id, else what it says.
    fn carry_key(&self) -> String {
        self.test_id
            .clone()
            .unwrap_or_else(|| self.accessible_label().to_owned())
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

    /// What this button is: a step's main action, a low-key text action, or
    /// an ordinary one. The role is the whole look; nothing else sets it.
    fn role(&self) -> ButtonRole {
        if self.primary {
            ButtonRole::Primary
        } else if self.quiet {
            ButtonRole::Plain
        } else {
            ButtonRole::Secondary
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
    use crate::setup::destination::effects::DestinationEffect;

    use ui::synthetic_input::named_key;

    fn assert_key_fires(key: NamedKey) {
        let (control, presses) = Control::detached(DestinationEffect::Back);
        let mut button = ActionButton::new("Back", control);
        button.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = button.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let handled = ui::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node)
            .send(&mut button, &named_key(key))
            .handled;
        assert!(handled);
        let fired = presses.take().expect("the button fires its action");
        assert!(fired.effect == DestinationEffect::Back);
        assert!(presses.take().is_none(), "one activation is one press");
        assert_eq!(doc.length(ROOT), 0, "a button never writes the document");
        assert!(button.is_focused(), "activation must not move focus");
    }

    #[test]
    fn action_buttons_are_named_activatable_native_controls() {
        {
            assert_key_fires(NamedKey::Enter);
            assert_key_fires(NamedKey::Space);
        }
        {
            let (control, presses) = Control::detached(DestinationEffect::Reboot);
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
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().map(|press| press.effect) == Some(DestinationEffect::Reboot));
            assert_eq!(doc.length(ROOT), 0);
            assert!(button.is_focused(), "AT activation must not steal focus");
            // A disabled button runs nothing.
            let mut disabled = button.disabled(true);
            assert!(!disabled.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().is_none());
        }
        {
            let (control, _) = Control::detached(DestinationEffect::SetRecoveryPromise {
                acknowledged: false,
            });
            let button = ActionButton::new("[x] I will save my recovery key", control)
                .with_test_id("recovery_ack")
                .checkbox(true);
            let node = button.accessibility().expect("checkbox semantics");
            assert_eq!(node.role, ui::a11y::Role::Checkbox);
            assert_eq!(node.name.as_deref(), Some("I will save my recovery key"));
            assert_eq!(node.state.checked, Some(true));
        }
    }

    /// The view builds the box anew on each change; the rebuilt button goes
    /// on from the motion its predecessor left, and a key lands.
    #[test]
    fn a_rebuilt_checkbox_fades_its_mark_on_from_the_box_it_replaces() {
        use ui::time::testing::with_monotonic_time;
        use ui::tokens::MotionSource;
        let theme = Theme::dark();
        // What the view does: build the button for the value and lay it out.
        let built = |checked: bool| {
            let (control, _) = Control::detached(DestinationEffect::SetRecoveryPromise {
                acknowledged: !checked,
            });
            let mut button = ActionButton::new("I will save my recovery key", control)
                .with_test_id("recovery_ack")
                .checkbox(checked);
            button.build_layout(&mut TaffyTree::new(), &theme, &mut HashMap::new());
            button.rect = Rect::new(0.0, 0.0, 320.0, 40.0);
            button
        };
        let paint = |button: &mut ActionButton| {
            button.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                &PresenceCtx::empty(),
            );
        };
        let t0 = ui::time::now();
        let at = |ms: u64| t0 + ui::time::Duration::from_millis(ms);
        let mut button = built(false);
        with_monotonic_time(t0, || paint(&mut button));
        assert!(!button.needs_animation_frame(), "first seen, it rests");
        // A click: the view drops the button and builds a checked one.
        drop(button);
        let mut button = built(true);
        with_monotonic_time(at(10), || {
            ui::motion::note_source(MotionSource::Pointer);
            paint(&mut button);
        });
        with_monotonic_time(at(20), || {
            assert!(button.needs_animation_frame(), "the mark fades in");
            assert!(button.needs_redraw());
        });
        // Rebuilt again mid-fade, it goes on fading rather than landing.
        drop(button);
        let mut button = built(true);
        with_monotonic_time(at(30), || {
            paint(&mut button);
            assert!(button.needs_animation_frame(), "still on its way");
        });
        with_monotonic_time(at(2_000), || {
            assert!(!button.needs_animation_frame(), "then rests");
        });
        // A key lands at once.
        drop(button);
        let mut button = built(false);
        with_monotonic_time(at(3_000), || {
            ui::motion::note_source(MotionSource::Keyboard);
            paint(&mut button);
            assert!(!button.needs_animation_frame());
        });
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
        let height = ui::num::f32_of_f64(ButtonFace::height(SIZE, theme));
        let style = match self.width {
            Some(width) => Style {
                size: Size::from_lengths(width, height),
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
        if self.checked.is_some() && !self.check_taken {
            // Built again by its view: go on from the box it replaces.
            self.check_taken = true;
            if let Some(kept) = ui::motion::take_kept(&self.carry_key()) {
                self.check = kept;
            }
        }
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // The checkbox goes where its value is, as the input that changed it
        // asks. It follows the input before the label is read, so the two
        // borrows of the button never overlap.
        if let Some(checked) = self.checked {
            self.check.follow_input(
                checked,
                ui::widget::CheckGlyph::Check,
                theme.motion(),
                ui::motion::frame_now(),
            );
        }
        let label = self.accessible_label();
        // A checkbox button paints its own label beside its box, centred on
        // the box as one group, so the face paints without one.
        let face = ButtonFace::new(self.rect, if self.checked.is_some() { "" } else { label })
            .role(self.role())
            .control_size(SIZE)
            .hovered(self.is_hovered)
            .pressed(self.is_pressed)
            .focused(self.is_focused)
            .disabled(self.disabled);
        let (_, _, ink) = face.colors(theme);
        Button::paint(scene, transform, text_renderer, theme, &face);

        let Some(checked) = self.checked else {
            return;
        };
        let metrics = &theme.tokens.metrics;
        let side = f64::from(metrics.icon_control);
        let gap = f64::from(metrics.icon_label_gap);
        let style = SIZE.label_style(theme);
        let (tw, th) = text_renderer.measure_style(label, None, &style);
        let group_half_width = (f64::from(tw) + side + gap) / 2.0;
        let center = self.rect.center();
        let tx = center.x - group_half_width;
        let box_rect = Rect::new(tx, center.y - side / 2.0, tx + side, center.y + side / 2.0);
        // The shared checkbox face, posed by its motion.
        let checkbox = ui::widget::CheckboxFace::new(box_rect, checked)
            .pose(self.check.pose(ui::motion::frame_now()));
        ui::widget::Checkbox::paint(scene, transform, theme, &checkbox);
        text_renderer.draw_style(
            scene,
            label,
            ink,
            transform.pre_translate(Vec2::new(tx + side + gap, center.y - f64::from(th) / 2.0)),
            None,
            &style,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.disabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = ctx.logical(position);
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

    fn needs_redraw(&self) -> bool {
        self.check_moving()
    }

    fn needs_animation_frame(&self) -> bool {
        self.check_moving()
    }

    fn name(&self) -> &'static str {
        "ActionButton"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.test_id.as_deref().unwrap_or(self.label.as_str()))
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
        _key_path: &[u64],
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

/// A checkbox a rebuild drops leaves its motion for the one put in its place.
impl Drop for ActionButton {
    fn drop(&mut self) {
        if self.checked.is_none() || !self.check_taken {
            return;
        }
        ui::motion::keep(
            self.carry_key(),
            Some(self.rect),
            std::mem::take(&mut self.check),
        );
    }
}
