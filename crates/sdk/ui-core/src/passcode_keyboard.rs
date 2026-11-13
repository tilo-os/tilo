//! A keyboard drawn on the screen for entering a passcode by touch. Where no input method may run — the
//! greeter before any session, the session's lock screen — a touch-only device still needs a way to
//! type a passcode. It types letters, digits and symbols, with a one-shot shift and a symbols layer,
//! and it never predicts, corrects or remembers.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

use crate::icons::Icon;
use crate::surface::{control_face, edge_outline};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{ButtonRole, Interaction, REST_BORDER_WIDTH, face_colors, paint_disabled};

/// What a key shows: its character, or the icon of what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyFace {
    Text(String),
    Icon(Icon),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasscodeKey {
    Char(char),
    Shift,
    /// The letters' `?123` key: the symbols layer.
    Symbols,
    Space,
    Backspace,
    Enter,
}

/// What a key press asks of the passcode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasscodeInput {
    Text(String),
    Backspace,
    Enter,
}

const LETTERS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SYMBOLS: [&str; 4] = ["1234567890", "!@#$%^&*()", "-_=+[]{};:", "'\",.<>/?\\"];

const PAD: [&str; 3] = ["123", "456", "789"];

/// How much wider than tall a key may grow: a number pad is wide rows of three, as on a phone, while
/// the letters stay a comfortable key apart.
const PAD_KEY_ASPECT: f64 = 3.0;
const LETTER_KEY_ASPECT: f64 = 1.4;

/// The keyboard's state: which layer shows and whether the next letter is a capital.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PasscodeKeyboard {
    shift: bool,
    symbols: bool,
    pin: bool,
}

impl PasscodeKeyboard {
    /// The keyboard for a PIN: its digits on a phone's number pad, four rows of three.
    #[must_use]
    pub fn for_pin() -> Self {
        Self {
            pin: true,
            ..Self::default()
        }
    }

    /// The keyboard for a secret that is a PIN when `pin`, a password otherwise.
    #[must_use]
    pub fn for_secret(pin: bool) -> Self {
        if pin {
            Self::for_pin()
        } else {
            Self::default()
        }
    }

    /// Whether this keyboard is for a PIN ([`Self::for_pin`]).
    #[must_use]
    pub fn is_for_pin(self) -> bool {
        self.pin
    }

    /// The character a [`PasscodeKey::Char`] key types now, shift applied; `None` for every other key.
    #[must_use]
    pub fn character(self, key: PasscodeKey) -> Option<String> {
        match key {
            PasscodeKey::Char(c) if self.shift => Some(c.to_uppercase().collect()),
            PasscodeKey::Char(c) => Some(c.to_string()),
            _ => None,
        }
    }

    /// How tall the keyboard is: four rows for a PIN's number pad, five for the letters.
    pub fn height(self, key_height: f64, gap: f64) -> f64 {
        let rows = f64::from(u32::try_from(self.rows().len()).unwrap_or(1));
        key_height * rows + gap * (rows - 1.0)
    }

    fn rows(self) -> Vec<Vec<(PasscodeKey, f64)>> {
        if self.pin {
            let mut rows: Vec<Vec<(PasscodeKey, f64)>> = PAD
                .iter()
                .map(|row| row.chars().map(|c| (PasscodeKey::Char(c), 1.0)).collect())
                .collect();
            rows.push(vec![
                (PasscodeKey::Backspace, 1.0),
                (PasscodeKey::Char('0'), 1.0),
                (PasscodeKey::Enter, 1.0),
            ]);
            return rows;
        }
        let source = if self.symbols { SYMBOLS } else { LETTERS };
        let mut rows: Vec<Vec<(PasscodeKey, f64)>> = source
            .iter()
            .map(|row| row.chars().map(|c| (PasscodeKey::Char(c), 1.0)).collect())
            .collect();
        if let Some(last) = rows.last_mut() {
            last.insert(0, (PasscodeKey::Shift, 1.5));
            last.push((PasscodeKey::Backspace, 1.5));
        }
        rows.push(vec![
            (PasscodeKey::Symbols, 1.5),
            (PasscodeKey::Space, 6.0),
            (PasscodeKey::Enter, 2.0),
        ]);
        rows
    }

    /// Every key and where it sits inside `rect`, row by row, left to right.
    pub fn layout(self, rect: Rect, key_height: f64, gap: f64) -> Vec<(PasscodeKey, Rect)> {
        let pad_unit = ((rect.width() - 2.0 * gap) / 3.0).min(key_height * PAD_KEY_ASPECT);
        let mut out = Vec::new();
        let mut y = rect.y0;
        for row in self.rows() {
            let units: f64 = row.iter().map(|(_, width)| width).sum();
            let gaps = gap * f64::from(u32::try_from(row.len()).unwrap_or(1).saturating_sub(1));
            let unit = if self.pin {
                pad_unit
            } else {
                ((rect.width() - gaps) / units).min(key_height * LETTER_KEY_ASPECT)
            };
            let row_width = row.iter().map(|(_, width)| unit * width).sum::<f64>() + gaps;
            let mut x = rect.x0 + (rect.width() - row_width) / 2.0;
            for (key, width) in row {
                let w = unit * width;
                out.push((key, Rect::new(x, y, x + w, y + key_height)));
                x += w + gap;
            }
            y += key_height + gap;
        }
        out
    }

    pub fn key_at(
        self,
        rect: Rect,
        key_height: f64,
        gap: f64,
        point: Point,
    ) -> Option<PasscodeKey> {
        self.layout(rect, key_height, gap)
            .into_iter()
            .find(|(_, key_rect)| key_rect.contains(point))
            .map(|(key, _)| key)
    }

    /// Press `key`: shift and the symbols layer change the keyboard; every other key asks something of
    /// the passcode.
    pub fn press(&mut self, key: PasscodeKey) -> Option<PasscodeInput> {
        match key {
            PasscodeKey::Char(c) => {
                let c = if self.shift {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c
                };
                self.shift = false;
                Some(PasscodeInput::Text(c.to_string()))
            }
            PasscodeKey::Space => Some(PasscodeInput::Text(" ".into())),
            PasscodeKey::Backspace => Some(PasscodeInput::Backspace),
            PasscodeKey::Enter => Some(PasscodeInput::Enter),
            PasscodeKey::Shift => {
                self.shift = !self.shift;
                None
            }
            PasscodeKey::Symbols => {
                self.symbols = !self.symbols;
                self.shift = false;
                None
            }
        }
    }

    /// What `key` shows on this keyboard: the character it types (shift applied), its layer's word, or
    /// the icon of what it does.
    #[must_use]
    pub fn face(self, key: PasscodeKey) -> KeyFace {
        if let Some(character) = self.character(key) {
            return KeyFace::Text(character);
        }
        match key {
            PasscodeKey::Char(_) | PasscodeKey::Space => KeyFace::Text(String::new()),
            PasscodeKey::Shift => KeyFace::Icon(Icon::ArrowBigUp),
            PasscodeKey::Symbols if self.symbols => KeyFace::Text("abc".into()),
            PasscodeKey::Symbols => KeyFace::Text("?123".into()),
            PasscodeKey::Backspace => KeyFace::Icon(Icon::Delete),
            PasscodeKey::Enter => KeyFace::Icon(Icon::CornerDownLeft),
        }
    }

    /// Paint the keys at `place` (the keyboard's rectangle, key height and gap) as the theme's
    /// controls: a key's filled face and its label, the active modifiers lit.
    pub fn paint(
        self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        place: (Rect, f64, f64),
        enter_enabled: bool,
    ) {
        let (rect, key_height, gap) = place;
        let radius = f64::from(theme.tokens.radii.control);
        let (ground, _, key_ink) = face_colors(theme, ButtonRole::Key, Interaction::default());
        let (disabled_fill, disabled_edge, disabled_ink) = face_colors(
            theme,
            ButtonRole::Primary,
            Interaction {
                disabled: true,
                ..Interaction::default()
            },
        );
        let style = theme.text(TextRole::Title);
        let icon_side = f64::from(style.size).round();
        for (key, key_rect) in self.layout(rect, key_height, gap) {
            let lit = matches!(key, PasscodeKey::Shift) && self.shift
                || matches!(key, PasscodeKey::Symbols) && self.symbols;
            let disabled = matches!(key, PasscodeKey::Enter) && !enter_enabled;
            let (fill, ink) = if disabled {
                (disabled_fill, disabled_ink)
            } else if lit {
                (theme.border_focus, key_ink)
            } else {
                (ground, key_ink)
            };
            let shape = RoundedRect::from_rect(key_rect, radius);
            if disabled {
                paint_disabled(scene, transform, &shape);
                control_face(scene, transform, &shape, fill, theme);
            } else {
                scene.fill(Fill::NonZero, transform, fill, None, &shape);
            }
            let center = key_rect.center();
            match self.face(key) {
                KeyFace::Text(label) if label.is_empty() => {}
                KeyFace::Text(label) => {
                    let (w, h) = text.measure_style(&label, None, &style);
                    text.draw_style(
                        scene,
                        &label,
                        ink,
                        transform.pre_translate(Vec2::new(
                            center.x - f64::from(w) / 2.0,
                            center.y - f64::from(h) / 2.0,
                        )),
                        None,
                        &style,
                    );
                }
                KeyFace::Icon(icon) => {
                    icon.draw(
                        scene,
                        transform,
                        Rect::from_center_size(center, (icon_side, icon_side)),
                        ink,
                    );
                }
            }
            if disabled {
                scene.pop_layer();
                if let Some(edge) = disabled_edge {
                    scene.stroke(
                        &Stroke::new(REST_BORDER_WIDTH),
                        transform,
                        edge,
                        None,
                        &edge_outline(key_rect, radius),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: Rect = Rect::new(0.0, 400.0, 720.0, 632.0);

    fn center(keyboard: PasscodeKeyboard, key: PasscodeKey) -> Point {
        keyboard
            .layout(RECT, 40.0, 8.0)
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, rect)| rect.center())
            .expect("the key is on this layer")
    }

    #[test]
    fn keys_type_text_shifted_once_and_symbols_on_their_own_layer() {
        let mut keyboard = PasscodeKeyboard::default();
        let mut typed = Vec::new();
        for key in [
            PasscodeKey::Char('p'),
            PasscodeKey::Shift,
            PasscodeKey::Char('a'),
            PasscodeKey::Char('s'),
            PasscodeKey::Symbols,
            PasscodeKey::Char('#'),
            PasscodeKey::Char('7'),
            PasscodeKey::Backspace,
            PasscodeKey::Enter,
        ] {
            let at = center(keyboard, key);
            assert_eq!(keyboard.key_at(RECT, 40.0, 8.0, at), Some(key));
            typed.extend(keyboard.press(key));
        }
        assert_eq!(
            typed,
            [
                PasscodeInput::Text("p".into()),
                PasscodeInput::Text("A".into()),
                PasscodeInput::Text("s".into()),
                PasscodeInput::Text("#".into()),
                PasscodeInput::Text("7".into()),
                PasscodeInput::Backspace,
                PasscodeInput::Enter,
            ]
        );
    }

    #[test]
    fn every_key_is_a_touch_target_inside_the_keyboard_and_none_overlap() {
        assert!(
            (PasscodeKeyboard::default().height(40.0, 8.0) - RECT.height()).abs() < f64::EPSILON
        );
        for keyboard in [
            PasscodeKeyboard::default(),
            PasscodeKeyboard {
                symbols: true,
                ..PasscodeKeyboard::default()
            },
            PasscodeKeyboard::for_pin(),
        ] {
            let keyboard_rect = Rect::new(
                RECT.x0,
                RECT.y0,
                RECT.x1,
                RECT.y0 + keyboard.height(40.0, 8.0),
            );
            let keys = keyboard.layout(keyboard_rect, 40.0, 8.0);
            assert!(keys.len() >= 12);
            for (i, (_, a)) in keys.iter().enumerate() {
                assert!(keyboard_rect.contains(a.center()), "{a:?}");
                assert!(a.width() >= 30.0 && a.height() >= 30.0, "{a:?}");
                for (_, b) in keys.iter().skip(i + 1) {
                    assert!(a.intersect(*b).area() <= 0.0, "{a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn a_pin_is_a_phone_pad_of_digits_only_with_delete_left_and_enter_right() {
        let mut keyboard = PasscodeKeyboard::for_secret(true);
        assert!(keyboard.is_for_pin());
        assert!(!PasscodeKeyboard::for_secret(false).is_for_pin());
        assert!(
            keyboard.height(40.0, 8.0) < PasscodeKeyboard::default().height(40.0, 8.0),
            "the pad has four rows, the letters five"
        );

        let keys = keyboard.layout(RECT, 40.0, 8.0);
        let order: Vec<PasscodeKey> = keys.iter().map(|(key, _)| *key).collect();
        let mut expected: Vec<PasscodeKey> = "123456789".chars().map(PasscodeKey::Char).collect();
        expected.extend([
            PasscodeKey::Backspace,
            PasscodeKey::Char('0'),
            PasscodeKey::Enter,
        ]);
        assert_eq!(order, expected, "row by row, left to right");
        // Three columns: each row lines up with the one above.
        let at = |i: usize| keys[i].1;
        for i in 3..12 {
            assert!((at(i).x0 - at(i - 3).x0).abs() < 1e-9 && at(i).y0 > at(i - 3).y0);
        }
        assert!(
            at(0).width() > 40.0 * LETTER_KEY_ASPECT,
            "pad keys are wide targets"
        );

        let mut typed = Vec::new();
        for key in [
            PasscodeKey::Char('4'),
            PasscodeKey::Char('2'),
            PasscodeKey::Backspace,
            PasscodeKey::Enter,
        ] {
            let at = center(keyboard, key);
            assert_eq!(keyboard.key_at(RECT, 40.0, 8.0, at), Some(key));
            typed.extend(keyboard.press(key));
        }
        assert_eq!(
            typed,
            [
                PasscodeInput::Text("4".into()),
                PasscodeInput::Text("2".into()),
                PasscodeInput::Backspace,
                PasscodeInput::Enter,
            ]
        );
        assert_eq!(
            keyboard,
            PasscodeKeyboard::for_pin(),
            "pressing moves nothing"
        );

        // A password's ?123 opens the symbols.
        let mut password = PasscodeKeyboard::for_secret(false);
        password.press(PasscodeKey::Symbols);
        assert_eq!(
            password.key_at(RECT, 40.0, 8.0, center(password, PasscodeKey::Char('#'))),
            Some(PasscodeKey::Char('#'))
        );
    }
}
