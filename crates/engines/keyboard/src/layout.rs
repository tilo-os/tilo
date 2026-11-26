//! Data-driven keyboard layouts and pure geometry: a layout is rows of keys, and
//! adding one is data.

use vello::kurbo::Rect;

/// What a key emits or does when activated: the text variants append to the
/// widget's output, the rest are state transitions or edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Emit a literal string (usually one grapheme — a letter, digit, or
    /// symbol). Held as a `String` so future multi-codepoint keys (emoji,
    /// accented composites) need no enum change.
    Char(String),
    Backspace,
    Enter,
    Space,
    /// Cycle the shift state (Off → Shift → `CapsLock` → Off).
    Shift,
    SymbolsToggle,
    Layout(String),
    Emoji,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    /// The label on the keycap, lowercased for letters; [`Key::display_label`]
    /// applies shift, so the data is never mutated.
    pub label: String,
    pub action: KeyAction,
    /// Width relative to a standard 1.0-wide key (spacebar 5.0, Shift 1.5):
    /// geometry divides the row's width by the sum of the weights.
    pub width: f32,
    /// A vector glyph drawn instead of [`label`], by its platform icon name (the
    /// conventional Shift, Backspace and Return symbols have no glyph in the shipped
    /// font, and the label stays as the accessible name).
    pub icon: Option<&'static str>,
}

impl Key {
    pub fn letter(c: char) -> Self {
        Self {
            label: c.to_string(),
            action: KeyAction::Char(c.to_string()),
            width: 1.0,
            icon: None,
        }
    }

    pub fn ch(s: &str) -> Self {
        Self {
            label: s.to_string(),
            action: KeyAction::Char(s.to_string()),
            width: 1.0,
            icon: None,
        }
    }

    pub fn ch_char(c: char) -> Self {
        Self::ch(&c.to_string())
    }

    pub fn action(label: &str, action: KeyAction, width: f32) -> Self {
        Self {
            label: label.to_string(),
            action,
            width,
            icon: None,
        }
    }

    #[must_use]
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn is_letter(&self) -> bool {
        matches!(&self.action, KeyAction::Char(s)
            if s.chars().count() == 1 && s.chars().all(char::is_alphabetic))
    }

    pub fn display_label(&self, shift: ShiftState) -> String {
        if self.is_letter() && shift.is_upper() {
            ShiftState::CapsLock.case_word(&self.label)
        } else {
            self.label.clone()
        }
    }

    pub fn emit_action(&self, shift: ShiftState) -> KeyAction {
        match &self.action {
            KeyAction::Char(s) if self.is_letter() && shift.is_upper() => {
                KeyAction::Char(ShiftState::CapsLock.case_word(s))
            }
            other => other.clone(),
        }
    }

    /// What a long press offers: the letter itself first, so a press released without
    /// sliding types it, then its accented forms cased for `shift`, the forms the
    /// typing language uses first.
    pub fn alternates(&self, shift: ShiftState, usage: impl Fn(char) -> f64) -> Vec<String> {
        let KeyAction::Char(s) = &self.action else {
            return Vec::new();
        };
        let Some(&(_, forms)) = ALTERNATES.iter().find(|(base, _)| base == s) else {
            return Vec::new();
        };
        let mut ranked: Vec<(usize, f64, char)> = forms
            .iter()
            .enumerate()
            .map(|(i, &c)| (i, usage(c).max(0.0), c))
            .collect();
        // Stable on the inventory order for equal (including zero) usage.
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let case = |c: char| {
            let form = c.to_string();
            let upper = ShiftState::CapsLock.case_word(&form);
            // Casing must not change the length (ß → SS): keep such forms.
            if shift.is_upper() && upper.chars().count() == 1 {
                upper
            } else {
                form
            }
        };
        let base = s.chars().next().map(case);
        base.into_iter()
            .chain(ranked.into_iter().map(|(_, _, c)| case(c)))
            .collect()
    }
}

/// Accented forms offered on a long press, for the Latin letters the shipped
/// layouts type; the typing language's usage reorders them, and this order
/// breaks ties.
const ALTERNATES: &[(&str, &[char])] = &[
    ("a", &['à', 'â', 'á', 'ä', 'æ', 'ã', 'å', 'ā']),
    ("c", &['ç', 'ć', 'č']),
    ("e", &['é', 'è', 'ê', 'ë', 'ē', 'ę', 'ė']),
    ("i", &['î', 'ï', 'í', 'ì', 'ī']),
    ("l", &['ł']),
    ("n", &['ñ', 'ń']),
    ("o", &['ô', 'œ', 'ö', 'ó', 'ò', 'ø', 'õ', 'ō']),
    ("s", &['ß', 'ś', 'š']),
    ("u", &['ù', 'û', 'ü', 'ú', 'ū']),
    ("y", &['ÿ', 'ý']),
    ("z", &['ž', 'ź', 'ż']),
];

/// `word` with its first character upper-cased: one-shot Shift's case for a word.
fn capitalize_first(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Shift and caps-lock state. Tapping Shift cycles Off → Shift → `CapsLock` → Off;
/// one `Shift` resets after the next character, `CapsLock` is sticky.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShiftState {
    #[default]
    Off,
    Shift,
    CapsLock,
}

impl ShiftState {
    /// Apply typing case to a decoded word: one-shot Shift changes its first
    /// character, Caps Lock the whole word.
    #[must_use]
    pub fn case_word(self, word: &str) -> String {
        match self {
            Self::Off => word.to_owned(),
            Self::Shift => capitalize_first(word),
            Self::CapsLock => word.to_uppercase(),
        }
    }

    pub fn is_upper(self) -> bool {
        matches!(self, ShiftState::Shift | ShiftState::CapsLock)
    }

    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            ShiftState::Off => ShiftState::Shift,
            ShiftState::Shift => ShiftState::CapsLock,
            ShiftState::CapsLock => ShiftState::Off,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyboardLayout {
    pub name: String,
    pub rows: Vec<Vec<Key>>,
}

/// Stable identity for a key within a layout: its (row, column) index.
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
    pub fn key(&self, id: KeyId) -> Option<&Key> {
        self.rows.get(id.row).and_then(|r| r.get(id.col))
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn qwerty() -> Self {
        Self::alpha("qwerty", &["qwertyuiop", "asdfghjkl", "zxcvbnm"])
    }

    fn alpha(name: &str, letters: &[&str; 3]) -> Self {
        let mut rows: Vec<Vec<Key>> = Vec::new();
        rows.push(letters[0].chars().map(Key::letter).collect());
        rows.push(letters[1].chars().map(Key::letter).collect());
        let mut r3 = vec![Key::action("Shift", KeyAction::Shift, 1.5).with_icon("arrow-big-up")];
        r3.extend(letters[2].chars().map(Key::letter));
        r3.push(Key::action("Backspace", KeyAction::Backspace, 1.5).with_icon("delete"));
        rows.push(r3);
        rows.push(vec![
            Key::action("?123", KeyAction::SymbolsToggle, 1.5),
            Key::ch(","),
            Key::action("Emoji", KeyAction::Emoji, 1.5).with_icon("smile"),
            Key::action("space", KeyAction::Space, 5.0),
            Key::ch("."),
            Key::action(
                if name == "qwerty" { "FR" } else { "EN" },
                KeyAction::Layout(if name == "qwerty" { "azerty" } else { "qwerty" }.into()),
                1.5,
            ),
            Key::action("Return", KeyAction::Enter, 1.5).with_icon("corner-down-left"),
        ]);
        Self {
            name: name.to_string(),
            rows,
        }
    }
}

/// A layout as the app that draws it carries it: each row a comma-separated list of
/// `label:width`, the keys left to right. The app owns the layouts as data (its keys are its
/// words and its purposes), and an ask about a finger's path or a tap's neighbour hands them in
/// this way, so this crate answers about the keys the person saw. `None` for no row, or a row
/// that names no key.
#[must_use]
pub fn from_rows(rows: &[String]) -> Option<KeyboardLayout> {
    let mut out: Vec<Vec<Key>> = Vec::new();
    for row in rows {
        let mut keys = Vec::new();
        for key in row.split(',') {
            let key = key.trim();
            let (label, width) = key.split_once(':').unwrap_or((key, "1"));
            let (label, width) = (label.trim(), width.trim().parse::<f32>().unwrap_or(1.0));
            if label.is_empty() || width <= 0.0 {
                continue;
            }
            keys.push(Key {
                label: label.to_owned(),
                action: KeyAction::Char(label.to_owned()),
                width,
                icon: None,
            });
        }
        if keys.is_empty() {
            return None;
        }
        out.push(keys);
    }
    (!out.is_empty()).then(|| KeyboardLayout {
        name: "carried".to_owned(),
        rows: out,
    })
}

/// Lay `layout` out inside `area`, a rect for every key. Rows split the height
/// evenly, and a key's width is `(area.width / Σ weights) × weight`, so a layout
/// fills the width and wide keys scale with it; the caller picks the height.
#[allow(
    clippy::cast_precision_loss,
    reason = "row counts and indices become floating display coordinates; subpixel rounding is inherent in the geometry"
)]
pub fn key_rects(layout: &KeyboardLayout, area: Rect, shift: ShiftState) -> Vec<(KeyId, Rect)> {
    // Reserved for shift-dependent geometry; the shipped layouts size identically.
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
        let unit_w = area.width() / f64::from(total_weight);
        let y0 = area.y0 + r as f64 * row_h;
        let y1 = y0 + row_h;
        // Accumulate width to avoid rounding gaps between adjacent keys.
        let mut x = area.x0;
        for (c, key) in row.iter().enumerate() {
            let w = unit_w * f64::from(key.width);
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
    fn key_rects_fill_width_respect_weights_and_scale() {
        eprintln!("phase key_rects_fills_width_and_respects_weights");
        {
            let l = KeyboardLayout::qwerty();
            let area = Rect::new(0.0, 0.0, 1000.0, 320.0);
            let rects = key_rects(&l, area, ShiftState::Off);
            assert_eq!(
                rects.len(),
                l.rows.iter().map(std::vec::Vec::len).sum::<usize>()
            );

            // Row 0 (10 equal keys) spans the full width with no gaps.
            let row0: Vec<_> = rects.iter().filter(|(id, _)| id.row == 0).collect();
            assert_eq!(row0.len(), 10);
            let leftmost = row0.iter().map(|(_, r)| r.x0).fold(f64::MAX, f64::min);
            let rightmost = row0.iter().map(|(_, r)| r.x1).fold(f64::MIN, f64::max);
            assert!((leftmost - 0.0).abs() < 1e-6);
            assert!((rightmost - 1000.0).abs() < 1e-6, "row fills full width");

            // The spacebar (weight 5) is ~5× a weight-1 key in the same row.
            let func: Vec<_> = rects.iter().filter(|(id, _)| id.row == 3).collect();
            let space_id = KeyId::new(3, 3);
            let comma_id = KeyId::new(3, 1);
            let space_w = func
                .iter()
                .find(|(id, _)| *id == space_id)
                .unwrap()
                .1
                .width();
            let comma_w = func
                .iter()
                .find(|(id, _)| *id == comma_id)
                .unwrap()
                .1
                .width();
            assert!(
                (space_w / comma_w - 5.0).abs() < 1e-3,
                "space ≈ 5× a unit key"
            );
        }

        eprintln!("phase key_rects_scales_with_area_width");
        {
            let l = KeyboardLayout::qwerty();
            let narrow = key_rects(&l, Rect::new(0.0, 0.0, 360.0, 300.0), ShiftState::Off);
            let wide = key_rects(&l, Rect::new(0.0, 0.0, 720.0, 300.0), ShiftState::Off);
            let nk = narrow[0].1.width();
            let wk = wide[0].1.width();
            assert!(
                (wk / nk - 2.0).abs() < 1e-6,
                "2× width → 2× key width (phone→desktop)"
            );
        }

        eprintln!("phase empty_layout_yields_no_rects");
        {
            let l = KeyboardLayout {
                name: "empty".into(),
                rows: vec![],
            };
            assert!(key_rects(&l, Rect::new(0.0, 0.0, 100.0, 100.0), ShiftState::Off).is_empty());
        }
    }

    #[test]
    fn shift_transforms_only_letters_and_cycles_states() {
        eprintln!("phase shift_transforms_letter_label_and_value");
        {
            let k = Key::letter('a');
            assert_eq!(k.display_label(ShiftState::Off), "a");
            assert_eq!(k.display_label(ShiftState::Shift), "A");
            assert_eq!(k.display_label(ShiftState::CapsLock), "A");
            assert_eq!(k.emit_action(ShiftState::Off), KeyAction::Char("a".into()));
            assert_eq!(
                k.emit_action(ShiftState::Shift),
                KeyAction::Char("A".into())
            );
        }

        eprintln!("phase shift_does_not_transform_non_letters");
        {
            let k = Key::ch(",");
            assert_eq!(k.display_label(ShiftState::Shift), ",");
            assert_eq!(
                k.emit_action(ShiftState::Shift),
                KeyAction::Char(",".into())
            );
            let space = Key::action("space", KeyAction::Space, 5.0);
            assert_eq!(space.emit_action(ShiftState::Shift), KeyAction::Space);
        }

        eprintln!("phase unicode_typing_case_preserves_word_and_expands_letters");
        assert_eq!(ShiftState::Off.case_word("élève"), "élève");
        assert_eq!(ShiftState::Shift.case_word("élève"), "Élève");
        assert_eq!(ShiftState::CapsLock.case_word("élève"), "ÉLÈVE");
        assert_eq!(ShiftState::Shift.case_word("ßeta"), "SSeta");
        assert_eq!(ShiftState::CapsLock.case_word("straße"), "STRASSE");
        assert_eq!(ShiftState::Shift.case_word(""), "");

        eprintln!("phase shift_state_cycles");
        {
            assert_eq!(ShiftState::Off.cycle(), ShiftState::Shift);
            assert_eq!(ShiftState::Shift.cycle(), ShiftState::CapsLock);
            assert_eq!(ShiftState::CapsLock.cycle(), ShiftState::Off);
        }
    }
}
