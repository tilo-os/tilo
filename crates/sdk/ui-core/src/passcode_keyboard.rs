//! A keyboard drawn on the screen for entering a passcode by touch.
//!
//! Where no input method may run — the greeter before any session, the
//! session's lock screen — a touch-only device still needs a way to type a
//! passcode. This is that keyboard as a model, a layout and a paint, for the
//! trusted surface that owns the passcode to draw itself: nothing outside it
//! sees a key. It types letters, digits and symbols, with a one-shot shift
//! and a symbols layer, and it never predicts, corrects or remembers.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

use crate::text_render::TextRenderer;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasscodeKey {
    Char(char),
    Shift,
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

/// The keyboard's state: which layer shows and whether the next letter is a
/// capital. Small and `Copy`, so a surface can keep it beside its own state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PasscodeKeyboard {
    shift: bool,
    symbols: bool,
}

impl PasscodeKeyboard {
    /// Rows of keys, top to bottom.
    pub const ROWS: u16 = 5;

    pub fn height(key_height: f64, gap: f64) -> f64 {
        let rows = f64::from(Self::ROWS);
        key_height * rows + gap * (rows - 1.0)
    }

    fn rows(self) -> Vec<Vec<(PasscodeKey, f64)>> {
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

    /// Every key and where it sits inside `rect`. Keys are no wider than a
    /// comfortable target and each row is centred.
    pub fn layout(self, rect: Rect, key_height: f64, gap: f64) -> Vec<(PasscodeKey, Rect)> {
        let mut out = Vec::new();
        let mut y = rect.y0;
        for row in self.rows() {
            let units: f64 = row.iter().map(|(_, width)| width).sum();
            let gaps = gap * f64::from(u32::try_from(row.len()).unwrap_or(1).saturating_sub(1));
            let unit = ((rect.width() - gaps) / units).min(key_height * 1.4);
            let row_width = unit * units + gaps;
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
            PasscodeKey::Symbols => {
                self.symbols = !self.symbols;
                self.shift = false;
                None
            }
        }
    }

    fn label(self, key: PasscodeKey) -> String {
        match key {
            PasscodeKey::Char(c) if self.shift => c.to_uppercase().collect(),
            PasscodeKey::Char(c) => c.to_string(),
            PasscodeKey::Shift => "⇧".into(),
            PasscodeKey::Symbols if self.symbols => "abc".into(),
            PasscodeKey::Symbols => "?123".into(),
            PasscodeKey::Space => String::new(),
            PasscodeKey::Backspace => "⌫".into(),
            PasscodeKey::Enter => "↵".into(),
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
            let label = self.label(key);
            if label.is_empty() {
                continue;
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a key label size is a few dozen pixels"
            )]
            let size = (key_height * 0.45).clamp(12.0, 28.0) as f32;
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
                shift: false,
            },
        ] {
            let keys = keyboard.layout(RECT, 40.0, 8.0);
            assert!(keys.len() > 30);
            for (i, (_, a)) in keys.iter().enumerate() {
                assert!(RECT.contains(a.center()), "{a:?}");
                assert!(a.width() >= 30.0 && a.height() >= 30.0, "{a:?}");
                for (_, b) in keys.iter().skip(i + 1) {
                    assert!(a.intersect(*b).area() <= 0.0, "{a:?} overlaps {b:?}");
                }
            }
        }
    }
}
