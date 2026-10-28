//! The keyboard the sign-in screen draws along its bottom edge when no
//! keyboard is at hand, or a finger is in use (docs/security/sign-in.md
//! §3.4). It is the lock screen's passcode keyboard
//! ([`ui::passcode_keyboard`]): no input method runs before anyone signs
//! in, and what is typed never leaves the screen.
//!
//! A key is the same size and in the same place for a finger as for the
//! pointer: the keyboard's layout depends on the screen and the theme
//! alone, and a tap reaches it as the pointer press the toolkit makes of it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use ui::passcode_keyboard::{PasscodeInput, PasscodeKeyboard};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FieldRole, SecretPrompt, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use zeroize::Zeroizing;

use super::AuthPhase;

impl super::GreeterApp {
    /// Decide whether a keyboard is attached by asking the kernel
    /// ([`crate::keyboards`]) rather than the host, as under the kiosk.
    #[must_use]
    pub fn with_keyboard_probe(mut self) -> Self {
        self.keyboards = Some(crate::keyboards::present(std::path::Path::new(
            crate::keyboards::INPUT_CLASS,
        )));
        self
    }

    /// Whether the on-screen keyboard shows: no keyboard is attached, or a
    /// finger is in use, as the session's automatic on-screen keyboard
    /// decides.
    pub(super) fn shows_touch_keyboard(&self) -> bool {
        let keyboard = self.keyboards.unwrap_or_else(|| {
            self.environment
                .available
                .contains(ui::InputDevice::Keyboard)
        });
        !keyboard || self.environment.modality == ui::InputModality::Touch
    }

    /// Type what was pressed on the on-screen keyboard into the passcode, as
    /// keys typed there would.
    pub(super) fn take_touch_keys(&mut self) {
        let typed = std::mem::take(&mut *self.touch_keyboard.typed.borrow_mut());
        if typed.is_empty() || self.phase != AuthPhase::Idle {
            return;
        }
        let field = self
            .composed
            .element_mut()
            .and_then(|composed| widget_with_id(composed, "greeter.password"))
            .and_then(|widget| widget.as_any_mut())
            .and_then(|any| any.downcast_mut::<SecretPrompt>())
            .and_then(|prompt| prompt.field_mut(FieldRole::Secret));
        let Some(field) = field else {
            return;
        };
        let mut submit = false;
        for input in typed {
            match input {
                ui::passcode_keyboard::PasscodeInput::Text(text) => field.insert_at_cursor(&text),
                ui::passcode_keyboard::PasscodeInput::Backspace => {
                    let mut value = Zeroizing::new(field.value().to_owned());
                    value.pop();
                    field.set_text(&value);
                }
                ui::passcode_keyboard::PasscodeInput::Enter => submit = true,
            }
        }
        if submit {
            self.requests.sign_in.set(true);
        }
    }
}

/// The widget under `widget` whose automation id is `id`.
fn widget_with_id<'a>(widget: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
    if widget.test_id() == Some(id) {
        return Some(widget);
    }
    widget
        .children_mut()
        .into_iter()
        .find_map(|child| widget_with_id(child, id))
}

/// What the keyboard shares with the screen across rebuilds: its layer and
/// shift, and the keys pressed that the screen has not taken yet.
#[derive(Clone, Default)]
pub(super) struct Shared {
    pub keyboard: Rc<Cell<PasscodeKeyboard>>,
    pub typed: Rc<RefCell<Vec<PasscodeInput>>>,
}

/// The size of a key and the gap between keys, as the lock screen's
/// keyboard has them.
fn metrics(theme: &Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.metrics.panel_row_height),
        f64::from(theme.tokens.spacing.s2),
    )
}

/// The height the keyboard takes, its gap above included.
pub(super) fn height(theme: &Theme) -> f64 {
    let (key, gap) = metrics(theme);
    PasscodeKeyboard::height(key, gap) + f64::from(theme.tokens.spacing.s2)
}

/// Never wider than a wide sheet, so keys stay a thumb's reach apart.
fn max_width(theme: &Theme) -> f32 {
    theme.tokens.metrics.panel_width_sheet * 1.25
}

pub(super) struct TouchKeyboard {
    shared: Shared,
    rect: Rect,
    metrics: (f64, f64),
    pointer: Option<Point>,
    /// The keyboard changed since it was last drawn.
    dirty: Cell<bool>,
}

impl TouchKeyboard {
    pub fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            metrics: (0.0, 0.0),
            pointer: None,
            dirty: Cell::new(true),
        }
    }

    fn press_at(&self, point: Point) -> bool {
        let mut keyboard = self.shared.keyboard.get();
        let Some(key) = keyboard.key_at(self.rect, self.metrics.0, self.metrics.1, point) else {
            return false;
        };
        let typed = keyboard.press(key);
        if keyboard != self.shared.keyboard.get() {
            self.shared.keyboard.set(keyboard);
            self.dirty.set(true);
        }
        if let Some(typed) = typed {
            self.shared.typed.borrow_mut().push(typed);
        }
        true
    }
}

impl Widget for TouchKeyboard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = metrics(theme);
        let height = PasscodeKeyboard::height(self.metrics.0, self.metrics.1);
        let style = Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(ui::num::f32_of_f64(height)),
            },
            max_size: taffy::Size {
                width: taffy::Dimension::length(max_width(theme)),
                height: taffy::Dimension::auto(),
            },
            align_self: Some(taffy::AlignSelf::Center),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty.set(true);
            // Where each key is, for a test that types with a finger.
            tracing::debug!(
                keys = ?self.shared.keyboard.get().layout(rect, self.metrics.0, self.metrics.1),
                "touch keyboard laid out"
            );
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.shared.keyboard.get().paint(
            scene,
            transform,
            text_renderer,
            theme,
            (self.rect, self.metrics.0, self.metrics.1),
        );
        self.dirty.set(false);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(position.x, position.y));
            }
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = self.pointer.filter(|point| self.rect.contains(*point))
                    && self.press_at(point)
                {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty.get()
    }

    fn name(&self) -> &'static str {
        "TouchKeyboard"
    }

    fn test_id(&self) -> Option<&str> {
        Some("greeter.touch-keyboard")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::passcode_keyboard::PasscodeKey;

    fn laid_out(theme: &Theme) -> TouchKeyboard {
        let mut keyboard = TouchKeyboard::new(Shared::default());
        keyboard.metrics = metrics(theme);
        keyboard.rect = Rect::new(
            0.0,
            400.0,
            720.0,
            400.0 + PasscodeKeyboard::height(keyboard.metrics.0, keyboard.metrics.1),
        );
        keyboard
    }

    fn centre_of(keyboard: &TouchKeyboard, key: PasscodeKey) -> Point {
        keyboard
            .shared
            .keyboard
            .get()
            .layout(keyboard.rect, keyboard.metrics.0, keyboard.metrics.1)
            .into_iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, rect)| rect.center())
            .unwrap()
    }

    #[test]
    fn keys_pressed_are_kept_for_the_screen_and_shift_changes_the_keyboard() {
        let theme = Theme::dark();
        let keyboard = laid_out(&theme);
        for key in [
            PasscodeKey::Char('1'),
            PasscodeKey::Shift,
            PasscodeKey::Char('a'),
            PasscodeKey::Backspace,
            PasscodeKey::Enter,
        ] {
            let at = centre_of(&keyboard, key);
            assert!(keyboard.press_at(at), "{key:?}");
        }
        assert!(!keyboard.press_at(Point::new(-5.0, -5.0)));
        assert_eq!(
            *keyboard.shared.typed.borrow(),
            [
                PasscodeInput::Text("1".into()),
                PasscodeInput::Text("A".into()),
                PasscodeInput::Backspace,
                PasscodeInput::Enter,
            ]
        );
    }
}
