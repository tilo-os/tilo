//! Data-driven keyboard layouts and pure geometry: a layout is rows of keys, and
//! adding one is data.

/// Bundled letter layouts, in their legacy switching order.
pub const LAYOUT_NAMES: [&str; 2] = ["qwerty", "azerty"];

use ui::icons::Icon;
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
    /// A vector glyph drawn instead of [`label`]: the conventional Shift, Backspace
    /// and Return symbols have no glyph in the shipped font, and the label stays as
    /// the accessible name.
    pub icon: Option<Icon>,
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
    pub fn with_icon(mut self, icon: Icon) -> Self {
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
            Self::Shift => ui::format::capitalize_first(word),
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

    pub fn azerty() -> Self {
        Self::alpha("azerty", &["azertyuiop", "qsdfghjklm", "wxcvbn"])
    }

    fn alpha(name: &str, letters: &[&str; 3]) -> Self {
        let mut rows: Vec<Vec<Key>> = Vec::new();
        rows.push(letters[0].chars().map(Key::letter).collect());
        rows.push(letters[1].chars().map(Key::letter).collect());
        let mut r3 = vec![Key::action("Shift", KeyAction::Shift, 1.5).with_icon(Icon::ArrowBigUp)];
        r3.extend(letters[2].chars().map(Key::letter));
        r3.push(Key::action("Backspace", KeyAction::Backspace, 1.5).with_icon(Icon::Delete));
        rows.push(r3);
        rows.push(vec![
            Key::action("?123", KeyAction::SymbolsToggle, 1.5),
            Key::ch(","),
            Key::action("Emoji", KeyAction::Emoji, 1.5).with_icon(Icon::Smile),
            Key::action("space", KeyAction::Space, 5.0),
            Key::ch("."),
            Key::action(
                if name == "qwerty" { "FR" } else { "EN" },
                KeyAction::Layout(if name == "qwerty" { "azerty" } else { "qwerty" }.into()),
                1.5,
            ),
            Key::action("Return", KeyAction::Enter, 1.5).with_icon(Icon::CornerDownLeft),
        ]);
        Self {
            name: name.to_string(),
            rows,
        }
    }

    /// A calculator-style number pad on four columns, every row on the same grid so
    /// the key edges line up: digits on the first three, Backspace, minus, the
    /// decimal point and Enter down the fourth, `0` across the bottom three.
    pub fn numeric() -> Self {
        Self::digit_pad("numeric", Key::ch("-"), Key::ch("."))
    }

    /// Like [`numeric`](Self::numeric) without a minus sign, so a field that wants
    /// only a non-negative decimal can pick it.
    pub fn decimal() -> Self {
        Self::digit_pad("decimal", Key::ch("."), Key::ch(","))
    }

    /// A phone dialpad: `1-9` plus `* 0 #`, with `+` and Backspace on the side. No
    /// Enter, since dialing is committed by the host.
    pub fn phone() -> Self {
        let rows = vec![
            "123".chars().map(Key::ch_char).collect::<Vec<_>>(),
            "456".chars().map(Key::ch_char).collect::<Vec<_>>(),
            "789".chars().map(Key::ch_char).collect::<Vec<_>>(),
            vec![Key::ch("*"), Key::ch("0"), Key::ch("#")],
            vec![
                Key::ch("+"),
                Key::action("Backspace", KeyAction::Backspace, 2.0).with_icon(Icon::Delete),
            ],
        ];
        Self {
            name: "phone".to_string(),
            rows,
        }
    }

    /// A PIN pad: digits, Backspace and Enter only, since a secret numeric code is
    /// just digits.
    pub fn pin() -> Self {
        let rows = vec![
            "123".chars().map(Key::ch_char).collect::<Vec<_>>(),
            "456".chars().map(Key::ch_char).collect::<Vec<_>>(),
            "789".chars().map(Key::ch_char).collect::<Vec<_>>(),
            vec![
                Key::action("Backspace", KeyAction::Backspace, 1.0).with_icon(Icon::Delete),
                Key::ch("0"),
                Key::action("Return", KeyAction::Enter, 1.0).with_icon(Icon::CornerDownLeft),
            ],
        ];
        Self {
            name: "pin".to_string(),
            rows,
        }
    }

    fn digit_pad(name: &str, second: Key, third: Key) -> Self {
        let digits = |chars: &str, side: Key| {
            let mut row: Vec<Key> = chars.chars().map(Key::ch_char).collect();
            row.push(side);
            row
        };
        let rows = vec![
            digits(
                "123",
                Key::action("Backspace", KeyAction::Backspace, 1.0).with_icon(Icon::Delete),
            ),
            digits("456", second),
            digits("789", third),
            vec![
                Key::ch("0").with_width(3.0),
                Key::action("Return", KeyAction::Enter, 1.0).with_icon(Icon::CornerDownLeft),
            ],
        ];
        Self {
            name: name.to_string(),
            rows,
        }
    }

    /// The QWERTY letters with `@` and `.` on the bottom row, so an address needs no
    /// detour through the symbols layer.
    pub fn email() -> Self {
        Self::alpha_with_bottom(
            "email",
            &["qwertyuiop", "asdfghjkl", "zxcvbnm"],
            vec![
                Key::action("?123", KeyAction::SymbolsToggle, 1.5),
                Key::ch("@"),
                Key::ch("."),
                Key::action("space", KeyAction::Space, 3.5),
                Key::ch(".com").with_width(1.5),
                Key::action("Return", KeyAction::Enter, 1.5).with_icon(Icon::CornerDownLeft),
            ],
        )
    }

    /// The QWERTY letters with `/` and `.` on the bottom row, where the wide key
    /// stays a working Space labelled `"/"`.
    pub fn url() -> Self {
        Self::alpha_with_bottom(
            "url",
            &["qwertyuiop", "asdfghjkl", "zxcvbnm"],
            vec![
                Key::action("?123", KeyAction::SymbolsToggle, 1.5),
                Key::ch("."),
                Key::ch("/"),
                // A URL has no spaces: the wide bar stays a Space key, labelled "no space".
                Key::action("no space", KeyAction::Space, 3.5),
                Key::ch(".com").with_width(1.5),
                Key::action("Return", KeyAction::Enter, 1.5).with_icon(Icon::CornerDownLeft),
            ],
        )
    }

    fn alpha_with_bottom(name: &str, letters: &[&str; 3], bottom: Vec<Key>) -> Self {
        let mut rows: Vec<Vec<Key>> = Vec::new();
        rows.push(letters[0].chars().map(Key::letter).collect());
        rows.push(letters[1].chars().map(Key::letter).collect());
        let mut r3 = vec![Key::action("Shift", KeyAction::Shift, 1.5).with_icon(Icon::ArrowBigUp)];
        r3.extend(letters[2].chars().map(Key::letter));
        r3.push(Key::action("Backspace", KeyAction::Backspace, 1.5).with_icon(Icon::Delete));
        rows.push(r3);
        rows.push(bottom);
        Self {
            name: name.to_string(),
            rows,
        }
    }

    pub fn symbols() -> Self {
        let rows = vec![
            "1234567890".chars().map(Key::letter).collect::<Vec<_>>(),
            "@#$_&-+()/".chars().map(Key::letter).collect::<Vec<_>>(),
            {
                // Eight marks and a wider Backspace fill the ten columns the rows above use.
                let mut r: Vec<Key> = "%*\"':;!?".chars().map(Key::letter).collect();
                r.push(Key::action("Backspace", KeyAction::Backspace, 2.0).with_icon(Icon::Delete));
                r
            },
            vec![
                Key::action("ABC", KeyAction::SymbolsToggle, 1.5),
                Key::ch(","),
                Key::action("space", KeyAction::Space, 5.0),
                Key::ch("."),
                Key::action("Return", KeyAction::Enter, 1.5).with_icon(Icon::CornerDownLeft),
            ],
        ];
        Self {
            name: "symbols".to_string(),
            rows,
        }
    }
}

/// The kind of value a focused field expects, which picks a layout. These mirror
/// the Wayland `text-input-unstable-v3` `content_purpose` values, with no Wayland
/// dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputPurpose {
    #[default]
    Text,
    Terminal,
    Number,
    Decimal,
    Telephone,
    Url,
    Email,
    Pin,
    /// A password: the full letter keyboard, so every character is reachable; the
    /// host masks the displayed value.
    Password,
    Date,
    Time,
}

impl KeyboardLayout {
    /// The name of the built-in layout for a purpose, always one the widget can
    /// register on demand.
    pub fn for_purpose(purpose: InputPurpose) -> &'static str {
        match purpose {
            // Number entry, and date/time (no dedicated layout yet), are all
            // entered as digits.
            InputPurpose::Number | InputPurpose::Date | InputPurpose::Time => "numeric",
            InputPurpose::Decimal => "decimal",
            InputPurpose::Telephone => "phone",
            InputPurpose::Pin => "pin",
            InputPurpose::Email => "email",
            InputPurpose::Url => "url",
            // Passwords need every character, so they use the full keyboard.
            InputPurpose::Password | InputPurpose::Text | InputPurpose::Terminal => "qwerty",
        }
    }

    /// Construct a built-in layout by its name, or `None` for an unknown one, so a
    /// caller falls back rather than panics.
    pub fn by_name(name: &str) -> Option<Self> {
        Some(match name {
            "qwerty" => Self::qwerty(),
            "azerty" => Self::azerty(),
            "symbols" => Self::symbols(),
            "numeric" => Self::numeric(),
            "decimal" => Self::decimal(),
            "phone" => Self::phone(),
            "pin" => Self::pin(),
            "email" => Self::email(),
            "url" => Self::url(),
            _ => return None,
        })
    }
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
    fn letter_layouts_are_data_with_weighted_rows_and_icon_keys() {
        eprintln!("phase qwerty_has_four_rows_and_a_wide_space");
        {
            let l = KeyboardLayout::qwerty();
            assert_eq!(l.row_count(), 4);
            // Top row is the 10 QWERTY letters.
            assert_eq!(l.rows[0].len(), 10);
            let space = l.rows[3]
                .iter()
                .find(|k| k.action == KeyAction::Space)
                .expect("space key present");
            assert!(space.width >= 5.0, "spacebar should be wide");
        }

        eprintln!("phase azerty_is_just_data");
        {
            let l = KeyboardLayout::azerty();
            assert_eq!(l.name, "azerty");
            assert_eq!(l.rows[0][0].label, "a");
            assert_eq!(l.rows[1][0].label, "q");
        }

        // The Shift, Backspace and Return keys carry a vector icon on every layout that
        // exposes them: their Unicode symbols have no glyph in the shipped font. This
        // checks the production layout data, not a helper.
        eprintln!("phase modifier_and_edit_keys_carry_icons");
        {
            fn icon_for_action(l: &KeyboardLayout, action: &KeyAction) -> Option<Icon> {
                l.rows
                    .iter()
                    .flatten()
                    .find(|k| &k.action == action)
                    .and_then(|k| k.icon)
            }
            // Every layout that has Shift/Backspace/Enter must icon it.
            for l in [
                KeyboardLayout::qwerty(),
                KeyboardLayout::azerty(),
                KeyboardLayout::email(),
                KeyboardLayout::url(),
                KeyboardLayout::symbols(),
                KeyboardLayout::numeric(),
                KeyboardLayout::decimal(),
                KeyboardLayout::phone(),
                KeyboardLayout::pin(),
            ] {
                if has_action(&l, &KeyAction::Shift) {
                    assert_eq!(
                        icon_for_action(&l, &KeyAction::Shift),
                        Some(Icon::ArrowBigUp),
                        "{} shift key needs an icon",
                        l.name
                    );
                }
                if has_action(&l, &KeyAction::Backspace) {
                    assert_eq!(
                        icon_for_action(&l, &KeyAction::Backspace),
                        Some(Icon::Delete),
                        "{} backspace key needs an icon",
                        l.name
                    );
                }
                if has_action(&l, &KeyAction::Enter) {
                    assert_eq!(
                        icon_for_action(&l, &KeyAction::Enter),
                        Some(Icon::CornerDownLeft),
                        "{} enter key needs an icon",
                        l.name
                    );
                }
            }
        }
    }

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

    /// All characters a layout can emit, as single-char strings.
    fn emitted_chars(l: &KeyboardLayout) -> Vec<String> {
        l.rows
            .iter()
            .flatten()
            .filter_map(|k| match &k.action {
                KeyAction::Char(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn has_action(l: &KeyboardLayout, action: &KeyAction) -> bool {
        l.rows.iter().flatten().any(|k| &k.action == action)
    }

    fn emits(l: &KeyboardLayout, s: &str) -> bool {
        emitted_chars(l).iter().any(|c| c == s)
    }

    fn has_any_letter(l: &KeyboardLayout) -> bool {
        l.rows.iter().flatten().any(Key::is_letter)
    }

    #[test]
    fn purpose_layouts_carry_exactly_their_keys() {
        eprintln!("phase numeric_has_all_digits_backspace_no_letters");
        {
            let l = KeyboardLayout::numeric();
            for d in "0123456789".chars() {
                assert!(emits(&l, &d.to_string()), "numeric missing digit {d}");
            }
            assert!(has_action(&l, &KeyAction::Backspace));
            assert!(!has_any_letter(&l), "numeric must have no letters");
            assert!(emits(&l, "-"));
            assert!(emits(&l, "."));
        }

        eprintln!("phase decimal_has_point_and_no_letters");
        {
            let l = KeyboardLayout::decimal();
            assert!(emits(&l, "."), "decimal must offer the decimal point");
            for d in "0123456789".chars() {
                assert!(emits(&l, &d.to_string()));
            }
            assert!(has_action(&l, &KeyAction::Backspace));
            assert!(!has_any_letter(&l));
        }

        eprintln!("phase phone_is_a_dialpad");
        {
            let l = KeyboardLayout::phone();
            for d in "0123456789".chars() {
                assert!(emits(&l, &d.to_string()), "phone missing digit {d}");
            }
            assert!(emits(&l, "*"));
            assert!(emits(&l, "#"));
            assert!(emits(&l, "+"));
            assert!(has_action(&l, &KeyAction::Backspace));
            assert!(!has_any_letter(&l));
        }

        eprintln!("phase pin_is_digits_only");
        {
            let l = KeyboardLayout::pin();
            for d in "0123456789".chars() {
                assert!(emits(&l, &d.to_string()), "pin missing digit {d}");
            }
            assert!(has_action(&l, &KeyAction::Backspace));
            assert!(has_action(&l, &KeyAction::Enter));
            assert!(!has_any_letter(&l));
            // No decimal point, sign, or dialpad symbols in a secret code.
            for forbidden in [".", "-", "*", "#", "+"] {
                assert!(!emits(&l, forbidden), "pin must not offer {forbidden}");
            }
        }

        eprintln!("phase email_has_at_and_dot_and_letters");
        {
            let l = KeyboardLayout::email();
            assert!(emits(&l, "@"), "email needs an @ key");
            assert!(emits(&l, "."), "email needs a . key");
            assert!(has_any_letter(&l), "email is a letter layout");
            // The convenience key is nice-to-have but expected here.
            assert!(emits(&l, ".com"));
            // Still has a space (email local parts don't, but the bar stays usable).
            assert!(has_action(&l, &KeyAction::Space));
        }

        eprintln!("phase url_has_slash_and_dot_and_letters");
        {
            let l = KeyboardLayout::url();
            assert!(emits(&l, "/"), "url needs a / key");
            assert!(emits(&l, "."), "url needs a . key");
            assert!(emits(&l, ".com"));
            assert!(has_any_letter(&l), "url is a letter layout");
        }
    }

    #[test]
    fn layout_lookup_maps_every_purpose_and_rejects_unknown_names() {
        eprintln!("phase for_purpose_maps_every_variant");
        {
            use InputPurpose::*;
            let cases = [
                (Text, "qwerty"),
                (Number, "numeric"),
                (Decimal, "decimal"),
                (Telephone, "phone"),
                (Url, "url"),
                (Email, "email"),
                (Pin, "pin"),
                (Password, "qwerty"),
                (Date, "numeric"),
                (Time, "numeric"),
            ];
            for (purpose, expect) in cases {
                assert_eq!(
                    KeyboardLayout::for_purpose(purpose),
                    expect,
                    "{purpose:?} should map to {expect}"
                );
                // Every name `for_purpose` returns must be constructible.
                assert!(
                    KeyboardLayout::by_name(expect).is_some(),
                    "{expect} must be a known layout"
                );
            }
        }

        eprintln!("phase by_name_rejects_unknown");
        {
            assert!(KeyboardLayout::by_name("dvorak").is_none());
            assert_eq!(KeyboardLayout::by_name("phone").unwrap().name, "phone");
        }
    }

    /// Every row of a layout runs on the same column grid: the cumulative
    /// key edges of any row all fall on edges of the first row, so a key edge
    /// never sits between two keys of the rows above and below.
    #[test]
    fn number_and_symbol_rows_share_one_column_grid() {
        let edges = |row: &[Key]| -> Vec<f32> {
            let total: f32 = row.iter().map(|k| k.width).sum();
            row.iter()
                .scan(0.0, |x, k| {
                    *x += k.width;
                    Some(*x / total)
                })
                .collect()
        };
        for l in [
            KeyboardLayout::numeric(),
            KeyboardLayout::decimal(),
            KeyboardLayout::pin(),
            KeyboardLayout::phone(),
        ] {
            let grid = edges(&l.rows[0]);
            for row in &l.rows {
                for e in edges(row) {
                    assert!(
                        grid.iter().any(|g| (g - e).abs() < 1e-4),
                        "{}: an edge at {e} is off the grid {grid:?}",
                        l.name
                    );
                }
            }
        }
        let symbols = KeyboardLayout::symbols();
        let widths: Vec<f32> = symbols.rows[..3]
            .iter()
            .map(|r| r.iter().map(|k| k.width).sum())
            .collect();
        assert!(widths.iter().all(|w| (w - 10.0).abs() < 1e-6), "{widths:?}");
    }
}
