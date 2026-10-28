//! A keyboard drawn on the screen for entering a passcode by touch.
//!
//! Where no input method may run — the greeter before any session, the
//! session's lock screen — a touch-only device still needs a way to type a
//! passcode. This is that keyboard as a model, a layout and a paint, for the
//! trusted surface that owns the passcode to draw itself: nothing outside it
//! sees a key. It types letters, digits and symbols, with a one-shot shift
//! and a symbols layer, and it never predicts, corrects or remembers.
//!
//! For a PIN it opens on a phone's number pad instead
//! ([`PasscodeKeyboard::for_pin`]); the pad's letters key opens the letters,
//! for a PIN chosen with letters in it, and their `?123` key comes back.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

use crate::icons::Icon;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// What a key shows: its character, or the icon of what it does.
enum Face {
    Text(String),
    Icon(Icon),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasscodeKey {
    Char(char),
    Shift,
    /// The letters' `?123` key: the symbols layer, or the number pad when
    /// the keyboard is for a PIN.
    Symbols,
    /// The number pad's `ABC` key, which opens the letters.
    Letters,
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

/// How much wider than tall a key may grow: a number pad is wide rows of
/// three, as on a phone, while the letters stay a comfortable key apart.
const PAD_KEY_ASPECT: f64 = 3.0;
const LETTER_KEY_ASPECT: f64 = 1.4;

/// The keyboard's state: which layer shows and whether the next letter is a
/// capital. Small and `Copy`, so a surface can keep it beside its own state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent switches of one small keyboard; the layer follows from `pad` and `symbols`"
)]
pub struct PasscodeKeyboard {
    shift: bool,
    symbols: bool,
    /// The keyboard is for a PIN: it opens on the number pad, and the
    /// letters' `?123` key leads back to it.
    pin: bool,
    /// The number pad shows.
    pad: bool,
}

impl PasscodeKeyboard {
    /// Rows of keys, top to bottom, on every layer: the number pad is as
    /// tall as the letters, so switching between them moves nothing above.
    pub const ROWS: u16 = 5;

    /// The keyboard for a PIN, open on the number pad.
    #[must_use]
    pub fn for_pin() -> Self {
        Self {
            pin: true,
            pad: true,
            ..Self::default()
        }
    }

    /// The keyboard for a secret that is a PIN when `pin`, a password
    /// otherwise.
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

    /// Whether the number pad shows rather than the letters or symbols.
    #[must_use]
    pub fn shows_pad(self) -> bool {
        self.pad
    }

    /// The character a [`PasscodeKey::Char`] key types now, shift applied;
    /// `None` for every other key.
    #[must_use]
    pub fn character(self, key: PasscodeKey) -> Option<String> {
        match key {
            PasscodeKey::Char(c) if self.shift => Some(c.to_uppercase().collect()),
            PasscodeKey::Char(c) => Some(c.to_string()),
            _ => None,
        }
    }

    pub fn height(key_height: f64, gap: f64) -> f64 {
        let rows = f64::from(Self::ROWS);
        key_height * rows + gap * (rows - 1.0)
    }

    fn rows(self) -> Vec<Vec<(PasscodeKey, f64)>> {
        if self.pad {
            let mut rows: Vec<Vec<(PasscodeKey, f64)>> = PAD
                .iter()
                .map(|row| row.chars().map(|c| (PasscodeKey::Char(c), 1.0)).collect())
                .collect();
            rows.push(vec![
                (PasscodeKey::Letters, 1.0),
                (PasscodeKey::Char('0'), 1.0),
                (PasscodeKey::Backspace, 1.0),
            ]);
            // Enter spans the three columns, the gaps between them included.
            rows.push(vec![(PasscodeKey::Enter, 3.0)]);
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

    /// Every key and where it sits inside `rect`, row by row, left to
    /// right. Keys are no wider than a comfortable target and each row is
    /// centred; the number pad's rows share one grid of three columns.
    pub fn layout(self, rect: Rect, key_height: f64, gap: f64) -> Vec<(PasscodeKey, Rect)> {
        let pad_unit = ((rect.width() - 2.0 * gap) / 3.0).min(key_height * PAD_KEY_ASPECT);
        // A pad key spanning columns covers the gaps between them.
        let span_gap = if self.pad { gap } else { 0.0 };
        let mut out = Vec::new();
        let mut y = rect.y0;
        for row in self.rows() {
            let units: f64 = row.iter().map(|(_, width)| width).sum();
            let gaps = gap * f64::from(u32::try_from(row.len()).unwrap_or(1).saturating_sub(1));
            let unit = if self.pad {
                pad_unit
            } else {
                ((rect.width() - gaps) / units).min(key_height * LETTER_KEY_ASPECT)
            };
            let width_of = |width: f64| unit * width + span_gap * (width - 1.0);
            let row_width = row.iter().map(|(_, width)| width_of(*width)).sum::<f64>() + gaps;
            let mut x = rect.x0 + (rect.width() - row_width) / 2.0;
            for (key, width) in row {
                let w = width_of(width);
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

    /// Press `key`: shift and the symbols layer change the keyboard; every
    /// other key asks something of the passcode.
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
            PasscodeKey::Symbols if self.pin => {
                self.pad = true;
                self.symbols = false;
                self.shift = false;
                None
            }
            PasscodeKey::Symbols => {
                self.symbols = !self.symbols;
                self.shift = false;
                None
            }
            PasscodeKey::Letters => {
                self.pad = false;
                self.symbols = false;
                self.shift = false;
                None
            }
        }
    }

    fn face(self, key: PasscodeKey) -> Face {
        if let Some(character) = self.character(key) {
            return Face::Text(character);
        }
        match key {
            PasscodeKey::Char(_) | PasscodeKey::Space => Face::Text(String::new()),
            PasscodeKey::Letters => Face::Text("ABC".into()),
            PasscodeKey::Shift => Face::Icon(Icon::ArrowBigUp),
            PasscodeKey::Symbols if self.symbols => Face::Text("abc".into()),
            PasscodeKey::Symbols => Face::Text("?123".into()),
            PasscodeKey::Backspace => Face::Icon(Icon::Delete),
            PasscodeKey::Enter => Face::Icon(Icon::CornerDownLeft),
        }
    }

    /// Paint the keys at `place` (the keyboard's rectangle, key height and
    /// gap): the theme's panel ground with a hairline, the active modifiers
    /// lit.
    pub fn paint(
        self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        place: (Rect, f64, f64),
    ) {
        let (rect, key_height, gap) = place;
        let colors = &theme.tokens.colors;
        let ground = crate::chrome::panel_ground(theme);
        let hairline = crate::tokens::colors::Colors::hairline_on(ground, 40);
        let radius = f64::from(theme.tokens.radii.control);
        for (key, key_rect) in self.layout(rect, key_height, gap) {
            let lit = matches!(key, PasscodeKey::Shift) && self.shift
                || matches!(key, PasscodeKey::Symbols) && self.symbols;

            let shape = RoundedRect::from_rect(key_rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                if lit { colors.border_focus } else { ground },
                None,
                &shape,
            );
            scene.stroke(&Stroke::new(1.0), transform, hairline, None, &shape);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a key label size is a few dozen pixels"
            )]
            let size = (key_height * 0.45).clamp(12.0, 28.0) as f32;
            let label = match self.face(key) {
                Face::Text(label) if label.is_empty() => continue,
                Face::Text(label) => label,
                Face::Icon(icon) => {
                    let side = f64::from(size);
                    let center = key_rect.center();
                    icon.draw(
                        scene,
                        transform,
                        Rect::from_center_size(center, (side, side)),
                        colors.text_primary,
                    );
                    continue;
                }
            };
            let (w, h) = text.measure(&label, size, None);
            text.draw_text(
                scene,
                &label,
                size,
                colors.text_primary,
                transform.pre_translate(Vec2::new(
                    key_rect.center().x - f64::from(w) / 2.0,
                    key_rect.center().y - f64::from(h) / 2.0,
                )),
                None,
            );
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
        assert!((PasscodeKeyboard::height(40.0, 8.0) - RECT.height()).abs() < f64::EPSILON);
        for keyboard in [
            PasscodeKeyboard::default(),
            PasscodeKeyboard {
                symbols: true,
                ..PasscodeKeyboard::default()
            },
            PasscodeKeyboard::for_pin(),
        ] {
            let keys = keyboard.layout(RECT, 40.0, 8.0);
            assert!(keys.len() >= 13);
            for (i, (_, a)) in keys.iter().enumerate() {
                assert!(RECT.contains(a.center()), "{a:?}");
                assert!(a.width() >= 30.0 && a.height() >= 30.0, "{a:?}");
                for (_, b) in keys.iter().skip(i + 1) {
                    assert!(a.intersect(*b).area() <= 0.0, "{a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn a_pin_opens_a_phone_pad_whose_letters_key_opens_the_letters_and_back() {
        let mut keyboard = PasscodeKeyboard::for_secret(true);
        assert!(keyboard.shows_pad() && keyboard.is_for_pin());
        assert!(!PasscodeKeyboard::for_secret(false).shows_pad());

        let keys = keyboard.layout(RECT, 40.0, 8.0);
        let order: Vec<PasscodeKey> = keys.iter().map(|(key, _)| *key).collect();
        let mut expected: Vec<PasscodeKey> = "123456789".chars().map(PasscodeKey::Char).collect();
        expected.extend([
            PasscodeKey::Letters,
            PasscodeKey::Char('0'),
            PasscodeKey::Backspace,
            PasscodeKey::Enter,
        ]);
        assert_eq!(order, expected, "row by row, left to right");
        // Three columns: each row lines up with the one above, and enter
        // spans all three.
        let at = |i: usize| keys[i].1;
        for i in 3..12 {
            assert!((at(i).x0 - at(i - 3).x0).abs() < 1e-9 && at(i).y0 > at(i - 3).y0);
        }
        assert!((at(12).x0 - at(9).x0).abs() < 1e-9 && (at(12).x1 - at(11).x1).abs() < 1e-9);
        assert!(
            at(0).width() > 40.0 * LETTER_KEY_ASPECT,
            "pad keys are wide targets"
        );

        let mut typed = Vec::new();
        for key in [
            PasscodeKey::Char('4'),
            PasscodeKey::Letters,
            PasscodeKey::Char('x'),
            PasscodeKey::Symbols,
            PasscodeKey::Char('2'),
        ] {
            let at = center(keyboard, key);
            assert_eq!(
                keyboard.key_at(RECT, 40.0, 8.0, at),
                Some(key),
                "{keyboard:?}"
            );
            typed.extend(keyboard.press(key));
        }
        assert!(
            keyboard.shows_pad(),
            "?123 on a PIN's letters comes back to the pad"
        );
        assert_eq!(
            typed,
            ["4", "x", "2"].map(|text| PasscodeInput::Text(text.into()))
        );

        // A password's ?123 still opens the symbols.
        let mut password = PasscodeKeyboard::for_secret(false);
        password.press(PasscodeKey::Symbols);
        assert!(!password.shows_pad());
        assert_eq!(
            password.key_at(RECT, 40.0, 8.0, center(password, PasscodeKey::Char('#'))),
            Some(PasscodeKey::Char('#'))
        );
    }
}
