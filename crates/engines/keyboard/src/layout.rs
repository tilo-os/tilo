//! Data-driven keyboard layouts and pure geometry: a layout is rows of keys, and
//! adding one is data.

/// Bundled letter layouts, in their legacy switching order.
pub const LAYOUT_NAMES: [&str; 2] = ["qwerty", "azerty"];

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

    pub fn azerty() -> Self {
        Self::alpha("azerty", &["azertyuiop", "qsdfghjklm", "wxcvbn"])
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
                Key::action("Backspace", KeyAction::Backspace, 2.0).with_icon("delete"),
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
                Key::action("Backspace", KeyAction::Backspace, 1.0).with_icon("delete"),
                Key::ch("0"),
                Key::action("Return", KeyAction::Enter, 1.0).with_icon("corner-down-left"),
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
                Key::action("Backspace", KeyAction::Backspace, 1.0).with_icon("delete"),
            ),
            digits("456", second),
            digits("789", third),
            vec![
                Key::ch("0").with_width(3.0),
                Key::action("Return", KeyAction::Enter, 1.0).with_icon("corner-down-left"),
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
                Key::action("Return", KeyAction::Enter, 1.5).with_icon("corner-down-left"),
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
                Key::action("Return", KeyAction::Enter, 1.5).with_icon("corner-down-left"),
            ],
        )
    }

    fn alpha_with_bottom(name: &str, letters: &[&str; 3], bottom: Vec<Key>) -> Self {
        let mut rows: Vec<Vec<Key>> = Vec::new();
        rows.push(letters[0].chars().map(Key::letter).collect());
        rows.push(letters[1].chars().map(Key::letter).collect());
        let mut r3 = vec![Key::action("Shift", KeyAction::Shift, 1.5).with_icon("arrow-big-up")];
        r3.extend(letters[2].chars().map(Key::letter));
        r3.push(Key::action("Backspace", KeyAction::Backspace, 1.5).with_icon("delete"));
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
                r.push(Key::action("Backspace", KeyAction::Backspace, 2.0).with_icon("delete"));
                r
            },
            vec![
                Key::action("ABC", KeyAction::SymbolsToggle, 1.5),
                Key::ch(","),
                Key::action("space", KeyAction::Space, 5.0),
                Key::ch("."),
                Key::action("Return", KeyAction::Enter, 1.5).with_icon("corner-down-left"),
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
