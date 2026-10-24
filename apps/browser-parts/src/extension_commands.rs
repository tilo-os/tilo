//! The keyboard shortcuts extensions suggest (`commands` in the manifest),
//! as Chrome assigns and fires them.
//!
//! Chrome turns a suggested key into a browser accelerator; Tilo draws every
//! page offscreen, where Chromium has no window to take accelerators from, so
//! the Browser matches the keys itself. It does so after its own shortcuts
//! and before the page: a Browser shortcut stays the Browser's, and an
//! extension's shortcut works whatever the page does with keys. A shortcut
//! two extensions suggest goes to the first of them in the profile's order,
//! and one the Browser keeps for itself goes to neither.

use serde_json::Value;
use ui::winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// The command that does what a click on the extension's toolbar button
/// does: opens its popup, or tells it of the click. MV2's names for it work
/// as well.
pub const EXECUTE_ACTION: &str = "_execute_action";
const EXECUTE_ACTION_ALIASES: [&str; 3] = [
    EXECUTE_ACTION,
    "_execute_browser_action",
    "_execute_page_action",
];
/// Chrome assigns at most four suggested shortcuts of one extension.
const MAX_SUGGESTED: usize = 4;
const MAX_COMMANDS: usize = 64;
const MAX_NAME_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 512;

/// The key of a shortcut: the keys Chrome lets an extension suggest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutKey {
    /// `A`-`Z` and `0`-`9`, in lower case.
    Character(char),
    Comma,
    Period,
    Space,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Up,
    Down,
    Left,
    Right,
    MediaNextTrack,
    MediaPrevTrack,
    MediaPlayPause,
    MediaStop,
}

impl ShortcutKey {
    fn parse(token: &str) -> Option<Self> {
        let mut characters = token.chars();
        if let (Some(character), None) = (characters.next(), characters.next())
            && character.is_ascii_alphanumeric()
        {
            return Some(Self::Character(character.to_ascii_lowercase()));
        }
        Some(match token {
            "Comma" => Self::Comma,
            "Period" => Self::Period,
            "Space" => Self::Space,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "Insert" => Self::Insert,
            "Delete" => Self::Delete,
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "MediaNextTrack" => Self::MediaNextTrack,
            "MediaPrevTrack" => Self::MediaPrevTrack,
            "MediaPlayPause" => Self::MediaPlayPause,
            "MediaStop" => Self::MediaStop,
            _ => return None,
        })
    }

    const fn media(self) -> bool {
        matches!(
            self,
            Self::MediaNextTrack | Self::MediaPrevTrack | Self::MediaPlayPause | Self::MediaStop
        )
    }

    fn label(self) -> String {
        match self {
            Self::Character(character) => character.to_ascii_uppercase().to_string(),
            Self::Comma => ",".into(),
            Self::Period => ".".into(),
            other => format!("{other:?}"),
        }
    }

    fn named(self) -> Option<NamedKey> {
        Some(match self {
            Self::Space => NamedKey::Space,
            Self::Home => NamedKey::Home,
            Self::End => NamedKey::End,
            Self::PageUp => NamedKey::PageUp,
            Self::PageDown => NamedKey::PageDown,
            Self::Insert => NamedKey::Insert,
            Self::Delete => NamedKey::Delete,
            Self::Up => NamedKey::ArrowUp,
            Self::Down => NamedKey::ArrowDown,
            Self::Left => NamedKey::ArrowLeft,
            Self::Right => NamedKey::ArrowRight,
            Self::MediaNextTrack => NamedKey::MediaTrackNext,
            Self::MediaPrevTrack => NamedKey::MediaTrackPrevious,
            Self::MediaPlayPause => NamedKey::MediaPlayPause,
            Self::MediaStop => NamedKey::MediaStop,
            Self::Character(_) | Self::Comma | Self::Period => return None,
        })
    }

    /// The key where the layout puts it, for a layout whose character on it
    /// is no Latin letter or digit (Cyrillic, Greek; a shifted digit).
    fn code(self) -> Option<KeyCode> {
        const LETTERS: [KeyCode; 26] = [
            KeyCode::KeyA,
            KeyCode::KeyB,
            KeyCode::KeyC,
            KeyCode::KeyD,
            KeyCode::KeyE,
            KeyCode::KeyF,
            KeyCode::KeyG,
            KeyCode::KeyH,
            KeyCode::KeyI,
            KeyCode::KeyJ,
            KeyCode::KeyK,
            KeyCode::KeyL,
            KeyCode::KeyM,
            KeyCode::KeyN,
            KeyCode::KeyO,
            KeyCode::KeyP,
            KeyCode::KeyQ,
            KeyCode::KeyR,
            KeyCode::KeyS,
            KeyCode::KeyT,
            KeyCode::KeyU,
            KeyCode::KeyV,
            KeyCode::KeyW,
            KeyCode::KeyX,
            KeyCode::KeyY,
            KeyCode::KeyZ,
        ];
        const DIGITS: [KeyCode; 10] = [
            KeyCode::Digit0,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        match self {
            Self::Character(character @ 'a'..='z') => {
                LETTERS.get(usize::from(character as u8 - b'a')).copied()
            }
            Self::Character(character @ '0'..='9') => {
                DIGITS.get(usize::from(character as u8 - b'0')).copied()
            }
            Self::Comma => Some(KeyCode::Comma),
            Self::Period => Some(KeyCode::Period),
            _ => None,
        }
    }
}

/// A shortcut as Chrome writes it (`Ctrl+Shift+Y`). `primary` is `Ctrl`, which
/// on macOS is Command (`Command` there too); `control` is macOS's Control
/// key (`MacCtrl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four modifiers Chrome's shortcuts are written with"
)]
pub struct Shortcut {
    pub primary: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: ShortcutKey,
}

impl Shortcut {
    /// Read a suggested key under Chrome's rules: `Ctrl` or `Alt` (on macOS
    /// also `Command` or `MacCtrl`), an optional `Shift`, and one key; never
    /// `Ctrl` with `Alt`, which typing a character with Alt Gr presses. A
    /// media key needs no modifier.
    pub fn parse(text: &str, mac: bool) -> Option<Self> {
        let mut tokens: Vec<&str> = text.split('+').map(str::trim).collect();
        let key = ShortcutKey::parse(tokens.pop()?)?;
        let mut shortcut = Self {
            primary: false,
            control: false,
            alt: false,
            shift: false,
            key,
        };
        for token in tokens {
            let held = match token {
                "Ctrl" => &mut shortcut.primary,
                "Command" if mac => &mut shortcut.primary,
                "MacCtrl" if mac => &mut shortcut.control,
                "Alt" => &mut shortcut.alt,
                "Shift" => &mut shortcut.shift,
                _ => return None,
            };
            if std::mem::replace(held, true) {
                return None;
            }
        }
        let modifier = shortcut.primary || shortcut.control || shortcut.alt;
        let valid = if key.media() {
            !modifier && !shortcut.shift
        } else {
            modifier && !(shortcut.primary && shortcut.alt && !mac)
        };
        valid.then_some(shortcut)
    }

    /// How `chrome.commands.getAll` writes it: `Ctrl+Shift+Y`, and on macOS
    /// with the symbols of its menus, in their order.
    pub fn label(self, mac: bool) -> String {
        let key = self.key.label();
        if mac {
            let mut text = String::new();
            for (held, symbol) in [
                (self.control, '\u{2303}'),
                (self.alt, '\u{2325}'),
                (self.shift, '\u{21e7}'),
                (self.primary, '\u{2318}'),
            ] {
                if held {
                    text.push(symbol);
                }
            }
            return text + &key;
        }
        let mut parts: Vec<&str> = Vec::new();
        for (held, name) in [
            (self.primary, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if held {
                parts.push(name);
            }
        }
        parts.push(&key);
        parts.join("+")
    }

    /// Whether a key press is this shortcut. Letters and digits are matched
    /// by the character the layout types, and by where the key sits when
    /// the layout types no Latin letter or digit there.
    pub fn matches(
        self,
        logical: &Key,
        physical: PhysicalKey,
        modifiers: ModifiersState,
        mac: bool,
    ) -> bool {
        let (primary, control) = if mac {
            (modifiers.super_key(), modifiers.control_key())
        } else {
            (modifiers.control_key(), modifiers.super_key())
        };
        if primary != self.primary
            || control != self.control
            || modifiers.alt_key() != self.alt
            || modifiers.shift_key() != self.shift
        {
            return false;
        }
        if let Some(named) = self.key.named() {
            return *logical == Key::Named(named)
                || (self.key == ShortcutKey::Space && *logical == Key::Character(" ".into()));
        }
        let wanted = match self.key {
            ShortcutKey::Character(character) => character,
            ShortcutKey::Comma => ',',
            ShortcutKey::Period => '.',
            _ => return false,
        };
        let typed = match logical {
            Key::Character(text) => {
                let mut characters = text.chars();
                match (characters.next(), characters.next()) {
                    (Some(character), None) => Some(character.to_ascii_lowercase()),
                    _ => None,
                }
            }
            _ => None,
        };
        if typed == Some(wanted) {
            return true;
        }
        let latin = typed.is_some_and(|character| character.is_ascii_alphanumeric());
        !latin
            && self
                .key
                .code()
                .is_some_and(|code| physical == PhysicalKey::Code(code))
    }
}

/// The shortcuts the Browser answers itself, ahead of any extension
/// (`BrowserWidget::handle_browser_shortcut`, the address field's, and the
/// editing keys every text field and page has).
pub fn reserved(shortcut: Shortcut, mac: bool) -> bool {
    let character = match shortcut.key {
        ShortcutKey::Character(character) => Some(character),
        _ => None,
    };
    if shortcut.primary && !shortcut.control && !shortcut.alt {
        // Find (Shift+G its previous match), bookmark, reload, the address,
        // zoom back; select all, copy, paste (Shift: as plain text), cut,
        // undo (Shift: redo).
        let keys = if shortcut.shift {
            "fgdrl0vz"
        } else {
            "fgdrl0acvxz"
        };
        if character.is_some_and(|c| keys.contains(c)) {
            return true;
        }
    }
    // Reader view (Command+Option+R) on macOS.
    if mac && shortcut.primary && shortcut.alt && character == Some('r') {
        return true;
    }
    // Back and forward, except on macOS.
    !mac && shortcut.alt
        && !shortcut.primary
        && !shortcut.shift
        && matches!(shortcut.key, ShortcutKey::Left | ShortcutKey::Right)
}

/// One of an extension's commands, as its manifest declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// Its suggested key on this platform, when it has a valid one.
    pub suggested: Option<Shortcut>,
}

impl Command {
    pub fn executes_action(&self) -> bool {
        EXECUTE_ACTION_ALIASES.contains(&self.name.as_str())
    }
}

/// The manifest's `commands`, each with its suggested key for this platform:
/// the platform's own entry (`mac`, `linux`) or else `default`.
pub fn commands(manifest: &Value, mac: bool, localize: impl Fn(&str) -> String) -> Vec<Command> {
    let Some(entries) = manifest.get("commands").and_then(Value::as_object) else {
        return Vec::new();
    };
    let platform = if mac { "mac" } else { "linux" };
    entries
        .iter()
        .filter(|(name, _)| !name.is_empty() && name.len() <= MAX_NAME_BYTES)
        .take(MAX_COMMANDS)
        .map(|(name, entry)| {
            let suggested = match entry.get("suggested_key") {
                Some(Value::String(text)) => Some(text.as_str()),
                Some(Value::Object(keys)) => keys
                    .get(platform)
                    .or_else(|| keys.get("default"))
                    .and_then(Value::as_str),
                _ => None,
            };
            let mut description = entry
                .get("description")
                .and_then(Value::as_str)
                .map(&localize)
                .unwrap_or_default();
            if description.len() > MAX_DESCRIPTION_BYTES {
                description = description
                    .char_indices()
                    .take_while(|(at, character)| {
                        at + character.len_utf8() <= MAX_DESCRIPTION_BYTES
                    })
                    .map(|(_, character)| character)
                    .collect();
            }
            Command {
                name: name.clone(),
                description,
                suggested: suggested.and_then(|text| Shortcut::parse(text, mac)),
            }
        })
        .collect()
}

/// A shortcut an extension was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assigned {
    pub extension: String,
    pub command: String,
    pub shortcut: Shortcut,
}

/// Give each extension, in the order given, the shortcuts it suggests that
/// the Browser and the extensions before it have not taken: at most four
/// each, as in Chrome.
pub fn assign<'a>(
    extensions: impl IntoIterator<Item = (&'a str, &'a [Command])>,
    mac: bool,
) -> Vec<Assigned> {
    let mut assigned: Vec<Assigned> = Vec::new();
    for (extension, commands) in extensions {
        for command in commands
            .iter()
            .filter(|command| command.suggested.is_some())
            .take(MAX_SUGGESTED)
        {
            let Some(shortcut) = command.suggested else {
                continue;
            };
            if reserved(shortcut, mac) || assigned.iter().any(|taken| taken.shortcut == shortcut) {
                continue;
            }
            assigned.push(Assigned {
                extension: extension.to_owned(),
                command: command.name.clone(),
                shortcut,
            });
        }
    }
    assigned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(character: &str, code: KeyCode) -> (Key, PhysicalKey) {
        (Key::Character(character.into()), PhysicalKey::Code(code))
    }

    #[test]
    fn a_suggested_key_is_read_as_chrome_reads_it() {
        let shortcut = Shortcut::parse("Ctrl+Shift+Y", false).unwrap();
        assert_eq!(
            shortcut,
            Shortcut {
                primary: true,
                control: false,
                alt: false,
                shift: true,
                key: ShortcutKey::Character('y'),
            }
        );
        assert_eq!(shortcut.label(false), "Ctrl+Shift+Y");
        assert_eq!(shortcut.label(true), "\u{21e7}\u{2318}Y");
        assert_eq!(
            Shortcut::parse("Alt+Comma", false).unwrap().label(false),
            "Alt+,"
        );
        assert_eq!(
            Shortcut::parse("MacCtrl+Command+Up", true)
                .unwrap()
                .label(true),
            "\u{2303}\u{2318}Up"
        );
        assert!(Shortcut::parse("MediaPlayPause", false).is_some());
        for refused in [
            "Shift+Y",
            "Y",
            "Ctrl+Alt+Y",
            "Command+Y",
            "Ctrl+Ctrl+Y",
            "Ctrl+F5",
            "Ctrl+",
            "Ctrl+MediaStop",
            "Ctrl+Shift+Yy",
        ] {
            assert_eq!(Shortcut::parse(refused, false), None, "{refused}");
        }
    }

    #[test]
    fn a_command_takes_its_platforms_key_or_the_default() {
        let manifest = serde_json::json!({"commands": {
            "_execute_action": {"suggested_key": {"default": "Ctrl+Shift+E", "mac": "MacCtrl+Shift+E"}},
            "toggle": {"suggested_key": "Alt+Shift+T", "description": "__MSG_toggle__"},
            "unbound": {"description": "Nothing pressed"},
            "broken": {"suggested_key": "Ctrl+Alt+B"},
        }});
        let localize = |text: &str| text.replace("__MSG_toggle__", "Switch it");
        let linux = commands(&manifest, false, localize);
        let find =
            |list: &[Command], name: &str| list.iter().find(|c| c.name == name).cloned().unwrap();
        assert_eq!(
            find(&linux, "_execute_action")
                .suggested
                .unwrap()
                .label(false),
            "Ctrl+Shift+E"
        );
        assert!(find(&linux, "_execute_action").executes_action());
        assert_eq!(find(&linux, "toggle").description, "Switch it");
        assert_eq!(find(&linux, "unbound").suggested, None);
        assert_eq!(
            find(&linux, "broken").suggested,
            None,
            "Ctrl with Alt is AltGr"
        );
        let mac = commands(&manifest, true, str::to_owned);
        assert_eq!(
            find(&mac, "_execute_action").suggested.unwrap().label(true),
            "\u{2303}\u{21e7}E"
        );
        assert!(commands(&serde_json::json!({}), false, str::to_owned).is_empty());
    }

    #[test]
    fn a_key_press_matches_by_character_and_where_the_layout_has_none() {
        let shortcut = Shortcut::parse("Ctrl+Shift+Y", false).unwrap();
        let held = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let (key, code) = letter("Y", KeyCode::KeyY);
        assert!(shortcut.matches(&key, code, held, false));
        assert!(!shortcut.matches(&key, code, ModifiersState::CONTROL, false));
        assert!(!shortcut.matches(&key, code, held | ModifiersState::ALT, false));
        // A German layout types Z where a US one types Y: Z is not Y.
        let (key, code) = letter("Z", KeyCode::KeyY);
        assert!(!shortcut.matches(&key, code, held, false));
        // A Cyrillic layout types no Latin letter on the key.
        let (key, code) = letter("Н", KeyCode::KeyY);
        assert!(shortcut.matches(&key, code, held, false));
        // On macOS Ctrl is Command.
        let (key, code) = letter("y", KeyCode::KeyY);
        assert!(shortcut.matches(
            &key,
            code,
            ModifiersState::SUPER | ModifiersState::SHIFT,
            true
        ));
        assert!(!shortcut.matches(&key, code, held, true));
        // A shifted digit types its symbol.
        let digit = Shortcut::parse("Alt+Shift+1", false).unwrap();
        let (key, code) = letter("!", KeyCode::Digit1);
        assert!(digit.matches(
            &key,
            code,
            ModifiersState::ALT | ModifiersState::SHIFT,
            false
        ));
        let arrow = Shortcut::parse("Alt+Shift+Up", false).unwrap();
        assert!(arrow.matches(
            &Key::Named(NamedKey::ArrowUp),
            PhysicalKey::Code(KeyCode::ArrowUp),
            ModifiersState::ALT | ModifiersState::SHIFT,
            false
        ));
    }

    #[test]
    fn a_shortcut_goes_to_the_first_extension_and_never_one_the_browser_keeps() {
        let suggest = |name: &str, key: &str| Command {
            name: name.into(),
            description: String::new(),
            suggested: Shortcut::parse(key, false),
        };
        let first = [
            suggest("one", "Ctrl+Shift+1"),
            suggest("find", "Ctrl+F"),
            suggest("back", "Alt+Left"),
        ];
        let second = [
            suggest("same", "Ctrl+Shift+1"),
            suggest("a", "Alt+Shift+A"),
            suggest("b", "Alt+Shift+B"),
            suggest("c", "Alt+Shift+C"),
            suggest("d", "Alt+Shift+D"),
            suggest("e", "Alt+Shift+E"),
        ];
        let assigned = assign([("first", &first[..]), ("second", &second[..])], false);
        let names: Vec<(&str, &str)> = assigned
            .iter()
            .map(|a| (a.extension.as_str(), a.command.as_str()))
            .collect();
        // Four suggestions of the second count, the one taken among them.
        assert_eq!(
            names,
            [
                ("first", "one"),
                ("second", "a"),
                ("second", "b"),
                ("second", "c")
            ]
        );
    }
}
