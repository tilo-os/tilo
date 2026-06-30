//! Data-driven keyboard layouts + pure geometry.
//!
//! A [`KeyboardLayout`] is just rows of [`Key`]s. Adding AZERTY, QWERTZ, a
//! Dvorak layer, an emoji layer, … is *data* — build a `KeyboardLayout` and
//! register it on the widget (see [`crate::widget::OskWidget::with_layout`]).
//! Nothing about rendering, sizing, or input is layout-specific.
//!
//! The *same* normalized key positions double as the FUTO
//! swipe `load_layout_json()` geometry — one source of truth for rendering and
//! (later) swipe decoding. [`key_rects`] is that single geometry pass;
//! a swipe decoder can normalise its output rects into 0..1 space.

use vello::kurbo::Rect;

/// What a key emits / does when activated. The text-producing variants
/// ([`Char`](KeyAction::Char), [`Space`](KeyAction::Space),
/// [`Enter`](KeyAction::Enter)) append to the widget's output; the rest are
/// state transitions (`Shift`, `SymbolsToggle`, `Layout`) or edits
/// (`Backspace`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Emit a literal string (usually one grapheme — a letter, digit, or
    /// symbol). Held as a `String` so future multi-codepoint keys (emoji,
    /// accented composites) need no enum change.
    Char(String),
    /// Delete the grapheme before the caret.
    Backspace,
    /// Commit a newline / submit.
    Enter,
    /// Emit a single space.
    Space,
    /// Cycle the shift state (Off → Shift → CapsLock → Off).
    Shift,
    /// Toggle between the active letter layout and the symbols layer.
    SymbolsToggle,
    /// Switch the active letter layout to the named registered layout
    /// (e.g. `"azerty"`). Data-driven layout switching.
    Layout(String),
}

/// One key in a layout row.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    /// Base label shown on the keycap (lowercase for letters). The
    /// *displayed* label is derived via [`Key::display_label`] so shift /
    /// caps can upper-case letters without mutating the data.
    pub label: String,
    /// What the key does.
    pub action: KeyAction,
    /// Width *weight* relative to a standard 1.0-wide key. The spacebar is
    /// wide (e.g. 5.0); Shift / Backspace are often 1.5. Geometry divides
    /// the row's available width by the sum of weights — see [`key_rects`].
    pub width: f32,
}

impl Key {
    /// A standard 1.0-wide letter key. `c` is the lowercase form; shift
    /// upper-cases both label and emitted value at activation time.
    pub fn letter(c: char) -> Self {
        Self {
            label: c.to_string(),
            action: KeyAction::Char(c.to_string()),
            width: 1.0,
        }
    }

    /// A 1.0-wide key emitting an arbitrary string (digits, symbols).
    pub fn ch(s: &str) -> Self {
        Self {
            label: s.to_string(),
            action: KeyAction::Char(s.to_string()),
            width: 1.0,
        }
    }

    /// An action / modifier key with an explicit label and weight.
    pub fn action(label: &str, action: KeyAction, width: f32) -> Self {
        Self {
            label: label.to_string(),
            action,
            width,
        }
    }

    /// Override this key's width weight (builder).
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Whether this key produces an upper-/lower-casable letter.
    pub fn is_letter(&self) -> bool {
        matches!(&self.action, KeyAction::Char(s)
            if s.chars().count() == 1 && s.chars().all(|c| c.is_alphabetic()))
    }

    /// The label to render given the current shift state. Letters
    /// upper-case under [`ShiftState::Shift`] / [`ShiftState::CapsLock`];
    /// everything else is unaffected.
    pub fn display_label(&self, shift: ShiftState) -> String {
        if self.is_letter() && shift.is_upper() {
            self.label.to_uppercase()
        } else {
            self.label.clone()
        }
    }

    /// The action to *emit* given the current shift state. A `Char` letter
    /// is upper-cased under shift/caps; all other actions pass through
    /// unchanged.
    pub fn emit_action(&self, shift: ShiftState) -> KeyAction {
        match &self.action {
            KeyAction::Char(s) if self.is_letter() && shift.is_upper() => {
                KeyAction::Char(s.to_uppercase())
            }
            other => other.clone(),
        }
    }
}

/// Shift / caps-lock state. The transition cycle on tapping Shift is
/// Off → Shift → CapsLock → Off (deterministic and unit-tested). A single
/// `Shift` is one-shot: it auto-resets to `Off` after the next character is
/// emitted; `CapsLock` is sticky.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShiftState {
    #[default]
    Off,
    Shift,
    CapsLock,
}

impl ShiftState {
    /// Whether letters should render / emit upper-case in this state.
    pub fn is_upper(self) -> bool {
        matches!(self, ShiftState::Shift | ShiftState::CapsLock)
    }

    /// Next state when the Shift key is tapped.
    pub fn cycle(self) -> Self {
        match self {
            ShiftState::Off => ShiftState::Shift,
            ShiftState::Shift => ShiftState::CapsLock,
            ShiftState::CapsLock => ShiftState::Off,
        }
    }
}

/// A full keyboard layout: a name plus rows of keys.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyboardLayout {
    pub name: String,
    pub rows: Vec<Vec<Key>>,
}

/// Stable identity for a key within a layout: its (row, column) index. The
/// pair is stable for a given layout snapshot, which is all the widget and
/// the geometry pass need to correlate a hit-test with a [`Key`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyId {
    pub row: usize,
    pub col: usize,
}

impl KeyId {
    pub fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

impl KeyboardLayout {
    /// Look up a key by id, if the id is in range.
    pub fn key(&self, id: KeyId) -> Option<&Key> {
        self.rows.get(id.row).and_then(|r| r.get(id.col))
    }

    /// Total number of rows — drives the intrinsic keyboard height.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The standard English QWERTY letter layout with a bottom function
    /// row (symbols-toggle, comma, space, period, Enter) and a Shift /
    /// Backspace flanked third row.
    pub fn qwerty() -> Self {
        Self::alpha("qwerty", &["qwertyuiop", "asdfghjkl", "zxcvbnm"])
    }

    /// French AZERTY — proof that another locale is *just data*. Same
    /// chrome (Shift / Backspace / function row) as QWERTY.
    pub fn azerty() -> Self {
        Self::alpha("azerty", &["azertyuiop", "qsdfghjklm", "wxcvbn"])
    }

    /// Build a three-row alpha layout from row strings, adding the shared
    /// Shift/Backspace row chrome and the bottom function row. The three
    /// `letters` rows are the letter clusters top-to-bottom.
    fn alpha(name: &str, letters: &[&str; 3]) -> Self {
        let mut rows: Vec<Vec<Key>> = Vec::new();
        // Rows 0 and 1: bare letters.
        rows.push(letters[0].chars().map(Key::letter).collect());
        rows.push(letters[1].chars().map(Key::letter).collect());
        // Row 2: Shift, letters, Backspace.
        let mut r3 = vec![Key::action("⇧", KeyAction::Shift, 1.5)];
        r3.extend(letters[2].chars().map(Key::letter));
        r3.push(Key::action("⌫", KeyAction::Backspace, 1.5));
        rows.push(r3);
        // Row 3: function row.
        rows.push(vec![
            Key::action("?123", KeyAction::SymbolsToggle, 1.5),
            Key::ch(","),
            Key::action("space", KeyAction::Space, 5.0),
            Key::ch("."),
            Key::action("⏎", KeyAction::Enter, 1.5),
        ]);
        Self {
            name: name.to_string(),
            rows,
        }
    }

    /// The symbols / numbers layer. The leading key returns to letters
    /// (`SymbolsToggle`); ships a digit row and two symbol rows.
    pub fn symbols() -> Self {
        let rows = vec![
            "1234567890".chars().map(Key::letter).collect::<Vec<_>>(),
            "@#$_&-+()/".chars().map(Key::letter).collect::<Vec<_>>(),
            {
                let mut r = vec![Key::action("=\\<", KeyAction::SymbolsToggle, 1.5)];
                r.extend("*\"':;!?".chars().map(Key::letter));
                r.push(Key::action("⌫", KeyAction::Backspace, 1.5));
                r
            },
            vec![
                Key::action("ABC", KeyAction::SymbolsToggle, 1.5),
                Key::ch(","),
                Key::action("space", KeyAction::Space, 5.0),
                Key::ch("."),
                Key::action("⏎", KeyAction::Enter, 1.5),
            ],
        ];
        Self {
            name: "symbols".to_string(),
            rows,
        }
    }
}

/// **Pure geometry.** Lay `layout` out inside `area`, returning a rect for
/// every key keyed by its [`KeyId`].
///
/// Sizing model (responsive, phone → desktop):
/// * Rows split `area`'s height evenly (`area.height / row_count`).
/// * Within a row, each key's width is `(area.width / Σ weights) × weight`,
///   so the keyboard always *fills the available width* and wide keys
///   (spacebar = 5.0) scale proportionally. Doubling `area.width` doubles
///   every key's width; the target key *height* is chosen by the caller
///   when it sizes `area` (see [`crate::widget::OskWidget`]).
///
/// `shift` is accepted for forward-compatibility (a future layout could
/// vary geometry by shift — e.g. revealing long-press accent rows). The
/// current built-in layouts are shift-invariant in geometry, so it does
/// not affect the returned rects today.
pub fn key_rects(layout: &KeyboardLayout, area: Rect, shift: ShiftState) -> Vec<(KeyId, Rect)> {
    // Reserved for shift-dependent geometry (accent rows etc.); the shipped
    // layouts size identically regardless of shift.
    let _ = shift;

    let mut out = Vec::new();
    let row_count = layout.rows.len();
    if row_count == 0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return out;
    }
    let row_h = area.height() / row_count as f64;

    for (r, row) in layout.rows.iter().enumerate() {
        let total_weight: f32 = row.iter().map(|k| k.width).sum();
        if total_weight <= 0.0 {
            continue;
        }
        let unit_w = area.width() / total_weight as f64;
        let y0 = area.y0 + r as f64 * row_h;
        let y1 = y0 + row_h;
        // Accumulate width to avoid rounding gaps between adjacent keys.
        let mut x = area.x0;
        for (c, key) in row.iter().enumerate() {
            let w = unit_w * key.width as f64;
            let rect = Rect::new(x, y0, x + w, y1);
            out.push((KeyId::new(r, c), rect));
            x += w;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwerty_has_four_rows_and_a_wide_space() {
        let l = KeyboardLayout::qwerty();
        assert_eq!(l.row_count(), 4);
        // Top row is the 10 QWERTY letters.
        assert_eq!(l.rows[0].len(), 10);
        // The space key is the widest in the function row.
        let space = l.rows[3]
            .iter()
            .find(|k| k.action == KeyAction::Space)
            .expect("space key present");
        assert!(space.width >= 5.0, "spacebar should be wide");
    }

    #[test]
    fn azerty_is_just_data() {
        let l = KeyboardLayout::azerty();
        assert_eq!(l.name, "azerty");
        assert_eq!(l.rows[0][0].label, "a");
        assert_eq!(l.rows[1][0].label, "q");
    }

    #[test]
    fn key_rects_fills_width_and_respects_weights() {
        let l = KeyboardLayout::qwerty();
        let area = Rect::new(0.0, 0.0, 1000.0, 320.0);
        let rects = key_rects(&l, area, ShiftState::Off);
        assert_eq!(rects.len(), l.rows.iter().map(|r| r.len()).sum::<usize>());

        // Row 0 (10 equal keys) spans the full width with no gaps.
        let row0: Vec<_> = rects.iter().filter(|(id, _)| id.row == 0).collect();
        assert_eq!(row0.len(), 10);
        let leftmost = row0.iter().map(|(_, r)| r.x0).fold(f64::MAX, f64::min);
        let rightmost = row0.iter().map(|(_, r)| r.x1).fold(f64::MIN, f64::max);
        assert!((leftmost - 0.0).abs() < 1e-6);
        assert!((rightmost - 1000.0).abs() < 1e-6, "row fills full width");

        // The spacebar (weight 5) is ~5× a weight-1 key in the same row.
        let func: Vec<_> = rects.iter().filter(|(id, _)| id.row == 3).collect();
        let space_id = KeyId::new(3, 2);
        let comma_id = KeyId::new(3, 1);
        let space_w = func.iter().find(|(id, _)| *id == space_id).unwrap().1.width();
        let comma_w = func.iter().find(|(id, _)| *id == comma_id).unwrap().1.width();
        assert!((space_w / comma_w - 5.0).abs() < 1e-3, "space ≈ 5× a unit key");
    }

    #[test]
    fn key_rects_scales_with_area_width() {
        let l = KeyboardLayout::qwerty();
        let narrow = key_rects(&l, Rect::new(0.0, 0.0, 360.0, 300.0), ShiftState::Off);
        let wide = key_rects(&l, Rect::new(0.0, 0.0, 720.0, 300.0), ShiftState::Off);
        let nk = narrow[0].1.width();
        let wk = wide[0].1.width();
        assert!((wk / nk - 2.0).abs() < 1e-6, "2× width → 2× key width (phone→desktop)");
    }

    #[test]
    fn shift_transforms_letter_label_and_value() {
        let k = Key::letter('a');
        assert_eq!(k.display_label(ShiftState::Off), "a");
        assert_eq!(k.display_label(ShiftState::Shift), "A");
        assert_eq!(k.display_label(ShiftState::CapsLock), "A");
        assert_eq!(k.emit_action(ShiftState::Off), KeyAction::Char("a".into()));
        assert_eq!(k.emit_action(ShiftState::Shift), KeyAction::Char("A".into()));
    }

    #[test]
    fn shift_does_not_transform_non_letters() {
        let k = Key::ch(",");
        assert_eq!(k.display_label(ShiftState::Shift), ",");
        assert_eq!(k.emit_action(ShiftState::Shift), KeyAction::Char(",".into()));
        let space = Key::action("space", KeyAction::Space, 5.0);
        assert_eq!(space.emit_action(ShiftState::Shift), KeyAction::Space);
    }

    #[test]
    fn shift_state_cycles() {
        assert_eq!(ShiftState::Off.cycle(), ShiftState::Shift);
        assert_eq!(ShiftState::Shift.cycle(), ShiftState::CapsLock);
        assert_eq!(ShiftState::CapsLock.cycle(), ShiftState::Off);
    }

    #[test]
    fn empty_layout_yields_no_rects() {
        let l = KeyboardLayout {
            name: "empty".into(),
            rows: vec![],
        };
        assert!(key_rects(&l, Rect::new(0.0, 0.0, 100.0, 100.0), ShiftState::Off).is_empty());
    }
}
