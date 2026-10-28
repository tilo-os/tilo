//! The keyboard the sign-in screen draws along its bottom edge when no
//! keyboard is at hand, or a finger is in use (docs/security/sign-in.md
//! §3.4). It is the lock screen's passcode keyboard
//! ([`ui::passcode_keyboard`]): no input method runs before anyone signs
//! in, and what is typed never leaves the screen.
//!
//! A key is the same size and in the same place for a finger as for the
//! pointer: the keyboard's layout depends on the screen and the theme
//! alone, and a tap reaches it as the pointer press the toolkit makes of it.
//!
//! A PIN opens the number pad, a password the letters; either switches to
//! the other. Each key is a button to the screen reader, named as it types
//! or by what it does, row by row.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::passcode_keyboard::{PasscodeInput, PasscodeKey, PasscodeKeyboard};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FieldRole, SecretPrompt, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use zeroize::Zeroizing;

use super::AuthPhase;
use crate::i18n::t;

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

    /// Open the keyboard the chosen person's secret wants: the number pad
    /// for a PIN, the letters for a password. A keyboard already for that
    /// kind of secret stays on the layer it is on.
    pub(super) fn fit_touch_keyboard_to_secret(&self) {
        let pin = self.secret_is_pin();
        if self.touch_keyboard.keyboard.get().is_for_pin() != pin {
            self.touch_keyboard
                .keyboard
                .set(PasscodeKeyboard::for_secret(pin));
        }
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
        let keyboard = self.shared.keyboard.get();
        let Some(key) = keyboard.key_at(self.rect, self.metrics.0, self.metrics.1, point) else {
            return false;
        };
        self.press(key);
        true
    }

    fn press(&self, key: PasscodeKey) {
        let mut keyboard = self.shared.keyboard.get();
        let typed = keyboard.press(key);
        if keyboard != self.shared.keyboard.get() {
            self.shared.keyboard.set(keyboard);
            self.dirty.set(true);
        }
        if let Some(typed) = typed {
            self.shared.typed.borrow_mut().push(typed);
        }
    }
}

/// What the screen reader calls `key` on `keyboard`: the character it
/// types, or what it does.
fn key_name(keyboard: PasscodeKeyboard, key: PasscodeKey) -> String {
    if let Some(character) = keyboard.character(key) {
        return character;
    }
    t(match key {
        PasscodeKey::Shift => "greeter-key-shift",
        PasscodeKey::Symbols if keyboard.is_for_pin() => "greeter-key-numbers",
        PasscodeKey::Symbols => "greeter-key-symbols",
        PasscodeKey::Letters => "greeter-key-letters",
        PasscodeKey::Space => "greeter-key-space",
        PasscodeKey::Backspace => "greeter-key-delete",
        PasscodeKey::Enter | PasscodeKey::Char(_) => "greeter-key-enter",
    })
}

/// The identity a key keeps for the screen reader across layers and
/// relayouts: what it is, not where it sits.
fn key_id(key: PasscodeKey) -> String {
    match key {
        PasscodeKey::Char(c) => format!("char:{c}"),
        other => format!("{other:?}"),
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

    /// The keys as buttons, row by row and left to right, which is the
    /// order the screen reader walks them in.
    fn accessibility(&self) -> Option<AccessNode> {
        let keyboard = self.shared.keyboard.get();
        let keys = keyboard
            .layout(self.rect, self.metrics.0, self.metrics.1)
            .into_iter()
            .map(|(key, rect)| {
                AccessNode::new(Role::Button)
                    .with_name(key_name(keyboard, key))
                    .with_interface_name(true)
                    .with_hashed_key(key_id(key))
                    .with_bounds(rect)
                    .with_state(AccessState::new().focusable(true))
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("greeter-keyboard"))
                .with_interface_name(true)
                .with_children(keys)
                .with_keyed_action_routes("greeter.touch-keyboard"),
        )
    }

    /// The screen reader presses a key as a tap would.
    fn perform_action(
        &mut self,
        action: &Action,
        key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        let ([key], Action::Activate) = (key_path, action) else {
            return false;
        };
        let keyboard = self.shared.keyboard.get();
        let Some((pressed, _)) = keyboard
            .layout(self.rect, self.metrics.0, self.metrics.1)
            .into_iter()
            .find(|(candidate, _)| ui::a11y::hash_key(&key_id(*candidate)) == *key)
        else {
            return false;
        };
        self.press(pressed);
        true
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
        laid_out_for(theme, PasscodeKeyboard::default())
    }

    fn laid_out_for(theme: &Theme, start: PasscodeKeyboard) -> TouchKeyboard {
        crate::i18n::ensure();
        let shared = Shared::default();
        shared.keyboard.set(start);
        let mut keyboard = TouchKeyboard::new(shared);
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

    /// What the screen reader says for each key, in the order the keyboard
    /// lists them to it.
    fn spoken_keys(keyboard: &TouchKeyboard) -> Vec<String> {
        keyboard
            .accessibility()
            .expect("the keyboard is accessible")
            .children
            .iter()
            .inspect(|node| assert_eq!(node.role, Role::Button))
            .map(|node| node.name.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn the_pad_taps_digits_and_its_letters_key_opens_the_letters_and_back() {
        let theme = Theme::dark();
        let keyboard = laid_out_for(&theme, PasscodeKeyboard::for_pin());
        for key in [
            PasscodeKey::Char('2'),
            PasscodeKey::Letters,
            PasscodeKey::Char('k'),
            PasscodeKey::Symbols,
            PasscodeKey::Char('0'),
            PasscodeKey::Enter,
        ] {
            assert!(keyboard.press_at(centre_of(&keyboard, key)), "{key:?}");
        }
        assert!(keyboard.shared.keyboard.get().shows_pad());
        assert_eq!(
            *keyboard.shared.typed.borrow(),
            [
                PasscodeInput::Text("2".into()),
                PasscodeInput::Text("k".into()),
                PasscodeInput::Text("0".into()),
                PasscodeInput::Enter,
            ]
        );
    }

    #[test]
    fn the_screen_reader_names_every_key_row_by_row_and_presses_it() {
        let theme = Theme::dark();
        let mut keyboard = laid_out_for(&theme, PasscodeKeyboard::for_pin());
        assert_eq!(
            spoken_keys(&keyboard),
            [
                "1", "2", "3", "4", "5", "6", "7", "8", "9", "Letters", "0", "Delete", "Enter"
            ]
        );
        let node = keyboard
            .accessibility()
            .expect("the keyboard is accessible");
        assert!(
            node.children.iter().all(|key| key.bounds.is_some()
                && key.state.focusable
                && key.action_target.is_some())
        );
        assert_eq!(
            ui::screen_reader::describe(&node.children[11]),
            "Delete, button"
        );

        // Activating "Letters" opens the letters, named as they type.
        let letters = node
            .children
            .iter()
            .find(|key| key.name.as_deref() == Some("Letters"))
            .and_then(|key| key.action_target.clone())
            .unwrap();
        let mut doc = automerge::AutoCommit::new();
        let mut edits = ui::widget::Edits::lend(&mut doc);
        assert!(keyboard.perform_semantic_action(
            &letters,
            &Action::Activate,
            &mut edits,
            &automerge::ROOT
        ));
        let spoken = spoken_keys(&keyboard);
        assert_eq!(
            &spoken[..10],
            ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]
        );
        assert_eq!(spoken[10], "q");
        for name in ["Shift", "Delete", "Numbers", "Space", "Enter"] {
            assert!(
                spoken.iter().any(|said| said == name),
                "{name} in {spoken:?}"
            );
        }
    }
}
