//! The words and symbols printed on the keys of the active keyboard layout.
//!
//! A shortcut is shown the way the keyboard under the person's hands spells
//! it, not in the language of the interface: a German using a US layout reads
//! `Ctrl`, the same German on a German layout reads `Strg`. Stored chords and
//! matching stay canonical and English (`Super`, `Ctrl`, `Alt`, `Shift`,
//! `pageup`); only what is painted changes.
//!
//! The host publishes the layout the keyboard is typing with through
//! [`set_active`] whenever it switches, and every label drawn afterwards reads
//! it through [`active`], so an open shortcut guide follows a layout switch
//! on its next frame.
//!
//! The Super key is always called `Super` on PC keyboards: the key carries a
//! vendor logo rather than a word, and `Super` is the name the desktop's
//! documentation and onboarding use for it. Apple keyboards and XKB's Mac
//! layout variants use the symbols printed on Apple keycaps (⌘ ⌥ ⌃ ⇧).

use std::sync::atomic::{AtomicU8, Ordering};

use crate::input::KeyboardModifier;
use crate::interaction::KeyboardFamily;

/// The legend language of a keyboard layout: which words are printed on its
/// modifier and navigation keys. Layouts whose keyboards carry English
/// legends (Dutch, Portuguese, Nordic, Swiss, Japanese) differ from US
/// English only in naming the right Alt key `Alt Gr`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeycapLanguage {
    #[default]
    English,
    German,
    French,
    Spanish,
    Italian,
    Portuguese,
    Dutch,
    Nordic,
    Swiss,
    Japanese,
    Korean,
}

impl KeycapLanguage {
    const ALL: [Self; 11] = [
        Self::English,
        Self::German,
        Self::French,
        Self::Spanish,
        Self::Italian,
        Self::Portuguese,
        Self::Dutch,
        Self::Nordic,
        Self::Swiss,
        Self::Japanese,
        Self::Korean,
    ];

    /// The legend language of XKB layout `layout` (`de`, `fr`, `ch`, …).
    /// Layouts without a table of their own keep US English legends.
    pub fn from_xkb_layout(layout: &str) -> Self {
        match layout.trim().to_ascii_lowercase().as_str() {
            "de" | "at" => Self::German,
            "fr" | "be" => Self::French,
            "es" | "latam" => Self::Spanish,
            "it" => Self::Italian,
            "pt" | "br" => Self::Portuguese,
            "nl" => Self::Dutch,
            "se" | "no" | "dk" | "fi" | "is" => Self::Nordic,
            "ch" | "lu" => Self::Swiss,
            "jp" => Self::Japanese,
            "kr" => Self::Korean,
            _ => Self::English,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::English => 0,
            Self::German => 1,
            Self::French => 2,
            Self::Spanish => 3,
            Self::Italian => 4,
            Self::Portuguese => 5,
            Self::Dutch => 6,
            Self::Nordic => 7,
            Self::Swiss => 8,
            Self::Japanese => 9,
            Self::Korean => 10,
        }
    }

    fn from_code(code: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|language| language.code() == code)
            .unwrap_or_default()
    }
}

/// The legends of one keyboard: its layout's language and whether it is an
/// Apple keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keycaps {
    pub language: KeycapLanguage,
    pub family: KeyboardFamily,
}

impl Default for Keycaps {
    fn default() -> Self {
        Self::US
    }
}

impl Keycaps {
    /// US English legends on a PC keyboard: the default.
    pub const US: Self = Self {
        language: KeycapLanguage::English,
        family: KeyboardFamily::Pc,
    };

    /// The legends of XKB layout `layout` with variant `variant` (empty for
    /// the base layout). Mac variants (`mac`, `de_mac`, `mac_nodeadkeys`, …)
    /// describe Apple keyboards.
    pub fn from_xkb(layout: &str, variant: &str) -> Self {
        let family = if variant.to_ascii_lowercase().contains("mac") {
            KeyboardFamily::Apple
        } else {
            KeyboardFamily::Pc
        };
        Self {
            language: KeycapLanguage::from_xkb_layout(layout),
            family,
        }
    }

    /// The same layout on a keyboard of `family`. An Apple keyboard stays
    /// Apple: its keycaps carry symbols whatever the layout says.
    #[must_use]
    pub fn on_family(self, family: KeyboardFamily) -> Self {
        if family == KeyboardFamily::Apple {
            Self { family, ..self }
        } else {
            self
        }
    }

    /// What is printed on `modifier`.
    pub fn modifier(self, modifier: KeyboardModifier) -> &'static str {
        use KeyboardModifier as M;
        use KeycapLanguage as L;
        if self.family == KeyboardFamily::Apple {
            return match modifier {
                M::Command | M::System => "⌘",
                M::Control => "⌃",
                M::Alt | M::AltGr => "⌥",
                M::Shift => "⇧",
                M::Fn => "fn",
            };
        }
        match (modifier, self.language) {
            (M::System, _) => "Super",
            (M::Command | M::Control, L::German) => "Strg",
            (M::Command | M::Control, _) => "Ctrl",
            (M::Alt, _) => "Alt",
            (M::AltGr, L::English) => "AltGr",
            (M::AltGr, _) => "Alt Gr",
            (M::Shift, L::German) => "Umschalt",
            (M::Shift, L::French) => "Maj",
            (M::Shift, L::Spanish) => "Mayús",
            (M::Shift, L::Italian) => "Maiusc",
            (M::Shift, _) => "Shift",
            (M::Fn, _) => "Fn",
        }
    }

    /// Format native keysym metadata without composing text or applying Ctrl
    /// transformations. Shared by static previews and the focused-seat map.
    pub fn native_symbol(self, name: &str, text: &str) -> String {
        if !text.is_empty() && !text.chars().any(char::is_control) {
            return text.to_owned();
        }
        if let Some(dead) = dead_legend(name) {
            return dead.into();
        }
        match name {
            "Shift_L" | "Shift_R" => self.modifier(KeyboardModifier::Shift).into(),
            "Control_L" | "Control_R" => self.modifier(KeyboardModifier::Control).into(),
            "Alt_L" | "Alt_R" => self.modifier(KeyboardModifier::Alt).into(),
            "ISO_Level3_Shift" => self.modifier(KeyboardModifier::AltGr).into(),
            "Super_L" | "Super_R" => self.modifier(KeyboardModifier::System).into(),
            "Prior" | "KP_Prior" => self.key("pageup"),
            "Next" | "KP_Next" => self.key("pagedown"),
            "Caps_Lock" => self.key("capslock"),
            "ISO_Left_Tab" => self.key("tab"),
            "Print" => self.key("printscreen"),
            _ => self.key(name),
        }
    }

    /// What is printed on the key with canonical name `name` (`pageup`,
    /// `escape`, `f4`, `k`): a single character upper-cased (unless its
    /// capital is more than one letter, like `ß`), a function key
    /// as `F4`, and a name this table does not know as given.
    pub fn key(self, name: &str) -> String {
        let lower = name.trim().to_ascii_lowercase();
        let canonical = match lower.as_str() {
            "esc" => "escape",
            "muhenkan" => "nonconvert",
            "henkan" | "henkan_mode" => "convert",
            "hiragana_katakana" => "kanamode",
            "hangul_hanja" => "hanja",
            "return" | "kp_enter" => "enter",
            "del" | "kp_delete" => "delete",
            "ins" | "kp_insert" => "insert",
            "arrowleft" | "kp_left" => "left",
            "arrowright" | "kp_right" => "right",
            "arrowup" | "kp_up" => "up",
            "arrowdown" | "kp_down" => "down",
            "page_up" | "kp_prior" | "kp_page_up" | "prior" => "pageup",
            "page_down" | "kp_next" | "kp_page_down" | "next" => "pagedown",
            "print" => "printscreen",
            "kp_home" => "home",
            "kp_end" => "end",
            "kp_begin" | "kp_beg" => "clear",
            "kp_add" => "+",
            "kp_subtract" => "−",
            "kp_multiply" => "×",
            "kp_divide" => "÷",
            "num_lock" => "numlock",
            "scroll_lock" => "scrolllock",
            "sys_req" => "sysreq",
            other => other,
        };
        if let Some(label) = self.named(canonical) {
            return label.to_owned();
        }
        let mut chars = canonical.chars();
        match (chars.next(), chars.next()) {
            (Some(character), None) => {
                let mut upper = character.to_uppercase();
                match (upper.next(), upper.next()) {
                    (Some(single), None) => single.to_string(),
                    _ => character.to_string(),
                }
            }
            (Some('f' | 'F'), Some(_)) if name.get(1..).is_some_and(is_number) => {
                format!("F{}", name.get(1..).unwrap_or_default())
            }
            _ => name.to_owned(),
        }
    }

    /// A canonical chord (`["Super", "Shift"]`, `"s"`) as the keyboard spells
    /// it. PC legends are joined with `+`; Apple symbols are written
    /// together, as Apple does (`⌘⇧S`). Modifiers are always listed in the
    /// same order, Super first, whatever order they were given in.
    pub fn chord<'a>(self, modifiers: impl IntoIterator<Item = &'a str>, key: &str) -> String {
        let separator = if self.family == KeyboardFamily::Apple {
            ""
        } else {
            "+"
        };
        self.chord_keys(modifiers, key).join(separator)
    }

    /// The keys of a chord as this keyboard spells them, modifiers first in
    /// the order the platform names them: what drawn keycaps show.
    pub fn chord_keys<'a>(
        self,
        modifiers: impl IntoIterator<Item = &'a str>,
        key: &str,
    ) -> Vec<String> {
        let mut held: Vec<KeyboardModifier> = modifiers
            .into_iter()
            .filter_map(canonical_modifier)
            .collect();
        held.sort_by_key(|modifier| display_rank(*modifier));
        held.dedup();
        held.into_iter()
            .map(|modifier| self.modifier(modifier).to_owned())
            .chain(std::iter::once(self.key(key)))
            .collect()
    }

    /// One token of a free-text chord (`Super`, `Ctrl`, `pageup`, `K`) as the
    /// keyboard spells it: a canonical modifier gets its legend, anything
    /// else is a key.
    pub fn token(self, token: &str) -> String {
        canonical_modifier(token).map_or_else(
            || self.key(token),
            |modifier| self.modifier(modifier).to_owned(),
        )
    }

    fn named(self, name: &str) -> Option<&'static str> {
        use KeycapLanguage as L;
        if self.family == KeyboardFamily::Apple
            && let Some(label) = apple_key(name)
        {
            return Some(label);
        }
        let localized = match self.language {
            L::German => german_key(name),
            L::French => french_key(name),
            L::Spanish => spanish_key(name),
            L::Italian => italian_key(name),
            L::Japanese => match name {
                "nonconvert" => Some("無変換"),
                "convert" => Some("変換"),
                "kanamode" => Some("かな"),
                _ => None,
            },
            L::Korean => match name {
                "hangul" => Some("한/영"),
                "hanja" => Some("한자"),
                _ => None,
            },
            L::English | L::Portuguese | L::Dutch | L::Nordic | L::Swiss => None,
        };
        localized.or_else(|| english_key(name))
    }

    fn code(self) -> u8 {
        let family = match self.family {
            KeyboardFamily::Pc => 0,
            KeyboardFamily::Apple => 0x80,
        };
        self.language.code() | family
    }

    fn from_code(code: u8) -> Self {
        Self {
            language: KeycapLanguage::from_code(code & 0x7f),
            family: if code & 0x80 == 0 {
                KeyboardFamily::Pc
            } else {
                KeyboardFamily::Apple
            },
        }
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

/// The canonical modifier a chord token names, in any of the spellings the
/// chord parsers accept.
fn canonical_modifier(token: &str) -> Option<KeyboardModifier> {
    Some(match token.trim().to_ascii_lowercase().as_str() {
        "super" | "meta" | "cmd" | "command" | "logo" => KeyboardModifier::System,
        "ctrl" | "control" => KeyboardModifier::Control,
        "alt" | "option" => KeyboardModifier::Alt,
        "altgr" => KeyboardModifier::AltGr,
        "shift" => KeyboardModifier::Shift,
        "fn" => KeyboardModifier::Fn,
        _ => return None,
    })
}

/// The order modifiers are listed in: the desktop's own modifier first.
pub(crate) fn display_rank(modifier: KeyboardModifier) -> u8 {
    match modifier {
        KeyboardModifier::Command | KeyboardModifier::System => 0,
        KeyboardModifier::Control => 1,
        KeyboardModifier::Alt => 2,
        KeyboardModifier::AltGr => 3,
        KeyboardModifier::Shift => 4,
        KeyboardModifier::Fn => 5,
    }
}

fn english_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "space" => "Space",
        "tab" => "Tab",
        "enter" => "Enter",
        "escape" => "Esc",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page Up",
        "pagedown" => "Page Down",
        "printscreen" => "Print Screen",
        "numlock" => "Num Lock",
        "scrolllock" => "Scroll Lock",
        "pause" => "Pause",
        "sysreq" => "SysRq",
        "clear" => "Clear",
        "capslock" => "Caps Lock",
        "nonconvert" => "Non-convert",
        "convert" => "Convert",
        "kanamode" => "Kana",
        "hangul" => "Hangul",
        "hanja" => "Hanja",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        _ => return None,
    })
}

/// Keys whose legend is an arrow on every keyboard that translates its
/// legends: there is no word to translate.
fn arrow_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "left" => "←",
        "right" => "→",
        "up" => "↑",
        "down" => "↓",
        _ => return None,
    })
}

fn german_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "space" => "Leertaste",
        "enter" => "Eingabe",
        "backspace" => "Rück",
        "delete" => "Entf",
        "insert" => "Einfg",
        "home" => "Pos1",
        "end" => "Ende",
        "pageup" => "Bild↑",
        "pagedown" => "Bild↓",
        "printscreen" => "Druck",
        "numlock" => "Num",
        "scrolllock" => "Rollen",
        "pause" => "Pause",
        "sysreq" => "S-Abf",
        "clear" => "Löschen",
        "capslock" => "Feststell",
        other => return arrow_key(other),
    })
}

fn french_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "space" => "Espace",
        "enter" => "Entrée",
        "escape" => "Échap",
        "backspace" => "Retour arrière",
        "delete" => "Suppr",
        "insert" => "Inser",
        "home" => "Début",
        "end" => "Fin",
        "pageup" => "Pg préc",
        "pagedown" => "Pg suiv",
        "printscreen" => "Impr écran",
        "numlock" => "Verr num",
        "scrolllock" => "Arrêt défil",
        "pause" => "Pause",
        "sysreq" => "Syst",
        "clear" => "Effacer",
        "capslock" => "Verr maj",
        other => return arrow_key(other),
    })
}

fn spanish_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "space" => "Espacio",
        "enter" => "Intro",
        "backspace" => "Retroceso",
        "delete" => "Supr",
        "insert" => "Insert",
        "home" => "Inicio",
        "end" => "Fin",
        "pageup" => "Re Pág",
        "pagedown" => "Av Pág",
        "printscreen" => "Impr Pant",
        "numlock" => "Bloq Num",
        "scrolllock" => "Bloq Despl",
        "pause" => "Pausa",
        "sysreq" => "Pet Sis",
        "clear" => "Borrar",
        "capslock" => "Bloq Mayús",
        other => return arrow_key(other),
    })
}

fn italian_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "space" => "Spazio",
        "enter" => "Invio",
        "delete" => "Canc",
        "insert" => "Ins",
        "home" => "Inizio",
        "end" => "Fine",
        "pageup" => "Pag↑",
        "pagedown" => "Pag↓",
        "printscreen" => "Stamp",
        "numlock" => "Bloc Num",
        "scrolllock" => "Bloc Scorr",
        "pause" => "Pausa",
        "sysreq" => "R Sist",
        "clear" => "Cancella",
        "capslock" => "Bloc Maiusc",
        other => return arrow_key(other),
    })
}

fn apple_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "tab" => "⇥",
        "enter" => "↩",
        "escape" => "esc",
        "backspace" => "⌫",
        "delete" => "⌦",
        "home" => "↖",
        "end" => "↘",
        "pageup" => "⇞",
        "pagedown" => "⇟",
        "capslock" => "⇪",
        other => return arrow_key(other),
    })
}

static ACTIVE: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// The layout [`scoped`] put in place on this thread, read ahead of the
    /// process-wide one.
    static SCOPED: std::cell::Cell<Option<Keycaps>> = const { std::cell::Cell::new(None) };
}

/// The legends of the layout the keyboard is typing with now.
pub fn active() -> Keycaps {
    SCOPED
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| Keycaps::from_code(ACTIVE.load(Ordering::Relaxed)))
}

/// Publish the legends of the layout the keyboard just switched to.
pub fn set_active(keycaps: Keycaps) {
    ACTIVE.store(keycaps.code(), Ordering::Relaxed);
}

/// Read `keycaps` as the active layout on this thread only, until the
/// returned guard drops; the previous scope comes back then, even when the
/// holder panics. For code that renders labels under a given layout (a test
/// asking for German keycaps) without changing what every other thread
/// reads: [`set_active`] is process-wide, so a test using it would leak its
/// layout into every test running beside it.
#[must_use = "the layout applies only while the guard lives"]
pub fn scoped(keycaps: Keycaps) -> ScopedKeycaps {
    ScopedKeycaps {
        previous: SCOPED.with(|scoped| scoped.replace(Some(keycaps))),
    }
}

/// Keeps a [`scoped`] layout in place on its thread.
#[derive(Debug)]
pub struct ScopedKeycaps {
    previous: Option<Keycaps>,
}

impl Drop for ScopedKeycaps {
    fn drop(&mut self) {
        SCOPED.with(|scoped| scoped.set(self.previous));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_layout_stays_on_its_thread_and_unwinds() {
        let german = Keycaps::from_xkb("de", "");
        let outer = scoped(german);
        assert_eq!(active(), german);
        let other = std::thread::spawn(active).join().ok();
        assert_eq!(
            other,
            Some(Keycaps::from_code(ACTIVE.load(Ordering::Relaxed))),
            "another thread keeps reading the process-wide layout"
        );
        {
            let _mac = scoped(Keycaps::from_xkb("us", "mac"));
            assert_eq!(active(), Keycaps::from_xkb("us", "mac"));
        }
        assert_eq!(active(), german, "the inner scope restores the outer");
        drop(outer);
        assert_eq!(active(), Keycaps::from_code(ACTIVE.load(Ordering::Relaxed)));
    }

    fn chord(keycaps: Keycaps, modifiers: &[&str], key: &str) -> String {
        keycaps.chord(modifiers.iter().copied(), key)
    }

    #[test]
    fn us_is_the_default_and_keeps_the_desktop_spelling() {
        let us = Keycaps::from_xkb("us", "");
        assert_eq!(us, Keycaps::default());
        assert_eq!(chord(us, &["Shift", "Super"], "s"), "Super+Shift+S");
        assert_eq!(chord(us, &["Ctrl", "Alt"], "escape"), "Ctrl+Alt+Esc");
        assert_eq!(chord(us, &["Super"], "pageup"), "Super+Page Up");
        assert_eq!(chord(us, &[], "printscreen"), "Print Screen");
        assert_eq!(chord(us, &["Alt"], "f4"), "Alt+F4");
        assert_eq!(chord(us, &["Super"], "/"), "Super+/");
        assert_eq!(us.modifier(KeyboardModifier::AltGr), "AltGr");
        // Layouts without their own table read as US English.
        assert_eq!(Keycaps::from_xkb("ru", ""), us);
        assert_eq!(Keycaps::from_xkb("gb", "extd"), us);
    }

    #[test]
    fn german_layouts_use_german_legends() {
        for (layout, variant) in [("de", ""), ("de", "nodeadkeys"), ("at", "")] {
            let de = Keycaps::from_xkb(layout, variant);
            assert_eq!(de.language, KeycapLanguage::German, "{layout}:{variant}");
            assert_eq!(de.family, KeyboardFamily::Pc);
        }
        let de = Keycaps::from_xkb("de", "");
        assert_eq!(chord(de, &["Ctrl", "Shift"], "t"), "Strg+Umschalt+T");
        assert_eq!(de.modifier(KeyboardModifier::AltGr), "Alt Gr");
        assert_eq!(de.modifier(KeyboardModifier::System), "Super");
        assert_eq!(de.key("delete"), "Entf");
        assert_eq!(de.key("insert"), "Einfg");
        assert_eq!(de.key("home"), "Pos1");
        assert_eq!(de.key("end"), "Ende");
        assert_eq!(chord(de, &["Super"], "pageup"), "Super+Bild↑");
        assert_eq!(chord(de, &["Super"], "pagedown"), "Super+Bild↓");
        assert_eq!(chord(de, &["Super", "Shift"], "left"), "Super+Umschalt+←");
        assert_eq!(chord(de, &["Ctrl", "Alt"], "escape"), "Strg+Alt+Esc");
    }

    #[test]
    fn french_layouts_use_french_legends() {
        for layout in ["fr", "be"] {
            assert_eq!(
                Keycaps::from_xkb(layout, "").language,
                KeycapLanguage::French
            );
        }
        let fr = Keycaps::from_xkb("fr", "azerty");
        assert_eq!(chord(fr, &["Ctrl", "Shift"], "a"), "Ctrl+Maj+A");
        assert_eq!(chord(fr, &["Ctrl", "Alt"], "escape"), "Ctrl+Alt+Échap");
        assert_eq!(fr.key("delete"), "Suppr");
        assert_eq!(fr.key("pageup"), "Pg préc");
        assert_eq!(fr.modifier(KeyboardModifier::AltGr), "Alt Gr");
    }

    #[test]
    fn spanish_and_italian_layouts_use_their_legends() {
        let es = Keycaps::from_xkb("es", "");
        assert_eq!(
            Keycaps::from_xkb("latam", "").language,
            KeycapLanguage::Spanish
        );
        assert_eq!(chord(es, &["Ctrl", "Shift"], "enter"), "Ctrl+Mayús+Intro");
        assert_eq!(es.key("delete"), "Supr");
        assert_eq!(es.key("pagedown"), "Av Pág");
        let it = Keycaps::from_xkb("it", "");
        assert_eq!(chord(it, &["Ctrl", "Shift"], "delete"), "Ctrl+Maiusc+Canc");
        assert_eq!(it.key("enter"), "Invio");
        assert_eq!(it.key("end"), "Fine");
    }

    #[test]
    fn english_legend_layouts_only_rename_alt_gr() {
        for (layout, language) in [
            ("nl", KeycapLanguage::Dutch),
            ("pt", KeycapLanguage::Portuguese),
            ("br", KeycapLanguage::Portuguese),
            ("se", KeycapLanguage::Nordic),
            ("no", KeycapLanguage::Nordic),
            ("dk", KeycapLanguage::Nordic),
            ("fi", KeycapLanguage::Nordic),
            ("ch", KeycapLanguage::Swiss),
            ("jp", KeycapLanguage::Japanese),
        ] {
            let keycaps = Keycaps::from_xkb(layout, "");
            assert_eq!(keycaps.language, language, "{layout}");
            assert_eq!(
                chord(keycaps, &["Super", "Ctrl", "Shift"], "pageup"),
                "Super+Ctrl+Shift+Page Up",
                "{layout}"
            );
            assert_eq!(keycaps.key("delete"), "Delete", "{layout}");
            assert_eq!(keycaps.modifier(KeyboardModifier::AltGr), "Alt Gr");
        }
        // Swiss French and Swiss German keyboards carry the same legends.
        assert_eq!(Keycaps::from_xkb("ch", "fr"), Keycaps::from_xkb("ch", ""));
    }

    #[test]
    fn mac_variants_use_apple_symbols_written_together() {
        for (layout, variant) in [
            ("us", "mac"),
            ("de", "mac"),
            ("ch", "de_mac"),
            ("fr", "mac"),
        ] {
            let mac = Keycaps::from_xkb(layout, variant);
            assert_eq!(mac.family, KeyboardFamily::Apple, "{layout}:{variant}");
            assert_eq!(chord(mac, &["Shift", "Super"], "s"), "⌘⇧S");
            assert_eq!(chord(mac, &["Ctrl", "Alt"], "escape"), "⌃⌥esc");
        }
        let mac = Keycaps::from_xkb("de", "mac_nodeadkeys");
        assert_eq!(mac.language, KeycapLanguage::German);
        assert_eq!(mac.key("pageup"), "⇞");
        assert_eq!(mac.key("left"), "←");
        // A German layout on an Apple keyboard spells what Apple leaves as a word.
        assert_eq!(mac.key("insert"), "Einfg");
        assert_eq!(
            Keycaps::from_xkb("de", "").on_family(KeyboardFamily::Apple),
            Keycaps {
                language: KeycapLanguage::German,
                family: KeyboardFamily::Apple
            }
        );
    }

    #[test]
    fn native_keypad_and_control_names_use_human_legends() {
        let de = Keycaps::from_xkb("de", "");
        for (name, label) in [
            ("KP_Insert", "Einfg"),
            ("KP_Delete", "Entf"),
            ("KP_Home", "Pos1"),
            ("KP_Beg", "Löschen"),
            ("KP_Prior", "Bild↑"),
            ("Pause", "Pause"),
            ("Sys_Req", "S-Abf"),
            ("Scroll_Lock", "Rollen"),
            ("KP_Divide", "÷"),
        ] {
            assert_eq!(de.key(name), label);
        }
        assert_eq!(Keycaps::US.key("Pause"), "Pause");
        assert_eq!(Keycaps::US.key("KP_Delete"), "Delete");
    }

    #[test]
    fn native_language_keys_and_dead_symbols_use_shared_legends() {
        let japanese = Keycaps::from_xkb("jp", "");
        assert_eq!(japanese.native_symbol("Muhenkan", ""), "無変換");
        assert_eq!(japanese.native_symbol("Henkan_Mode", ""), "変換");
        assert_eq!(japanese.native_symbol("Hiragana_Katakana", ""), "かな");
        let korean = Keycaps::from_xkb("kr", "");
        assert_eq!(korean.native_symbol("Hangul", ""), "한/영");
        assert_eq!(korean.native_symbol("Hangul_Hanja", ""), "한자");
        assert_eq!(Keycaps::US.native_symbol("dead_acute", ""), "◌́");
        assert_eq!(Keycaps::US.native_symbol("Greek_alpha", "α"), "α");
    }

    #[test]
    fn tokens_and_unknown_keys_pass_through() {
        let de = Keycaps::from_xkb("de", "");
        assert_eq!(de.token("Ctrl"), "Strg");
        assert_eq!(de.token("PageUp"), "Bild↑");
        assert_eq!(de.token("K"), "K");
        assert_eq!(de.key("f12"), "F12");
        assert_eq!(de.key("volumeup"), "volumeup");
        assert_eq!(de.key("ß"), "ß");
        assert_eq!(de.key("ö"), "Ö");
    }

    #[test]
    fn toolkit_legends_and_palette_hints_follow_the_active_layout() {
        use crate::input::{KeyLegend, KeyboardBinding};
        use crate::platform::{Platform, PlatformConventions, Shortcut};
        let linux = PlatformConventions::new(Platform::Linux);
        let binding = KeyboardBinding::new(
            [KeyboardModifier::Control, KeyboardModifier::Shift],
            KeyLegend::Delete,
        );
        let entry =
            crate::widget::command_palette::CommandEntry::new("x", "X").keybinding("Super+PageUp");
        set_active(Keycaps::from_xkb("de", ""));
        let german = (
            binding.keycap_labels(linux),
            Shortcut::command("S").shift().label(linux),
            entry.keybinding_label(linux),
        );
        set_active(Keycaps::US);
        assert_eq!(german.0, ["Strg", "Umschalt", "Entf"]);
        assert_eq!(german.1, "Strg+Umschalt+S");
        assert_eq!(german.2.as_deref(), Some("Super+Bild↑"));
        assert_eq!(binding.keycap_labels(linux), ["Ctrl", "Shift", "Delete"]);
    }

    #[test]
    fn the_active_layout_round_trips_every_combination() {
        let before = active();
        for language in KeycapLanguage::ALL {
            for family in [KeyboardFamily::Pc, KeyboardFamily::Apple] {
                let keycaps = Keycaps { language, family };
                set_active(keycaps);
                assert_eq!(active(), keycaps);
            }
        }
        set_active(before);
    }
}

fn dead_legend(name: &str) -> Option<&'static str> {
    Some(match name {
        "dead_grave" => "◌̀",
        "dead_acute" => "◌́",
        "dead_circumflex" => "◌̂",
        "dead_tilde" => "◌̃",
        "dead_macron" => "◌̄",
        "dead_breve" => "◌̆",
        "dead_abovedot" => "◌̇",
        "dead_diaeresis" => "◌̈",
        "dead_abovering" => "◌̊",
        "dead_doubleacute" => "◌̋",
        "dead_caron" => "◌̌",
        "dead_cedilla" => "◌̧",
        "dead_ogonek" => "◌̨",
        "dead_iota" => "◌ͅ",
        "dead_voiced_sound" => "◌゙",
        "dead_semivoiced_sound" => "◌゚",
        "dead_belowdot" => "◌̣",
        "dead_hook" => "◌̉",
        "dead_horn" => "◌̛",
        "dead_stroke" => "◌̵",
        "dead_abovecomma" => "◌̓",
        "dead_abovereversedcomma" => "◌̔",
        "dead_doublegrave" => "◌̏",
        "dead_belowring" => "◌̥",
        "dead_belowmacron" => "◌̱",
        "dead_belowcircumflex" => "◌̭",
        "dead_belowtilde" => "◌̰",
        "dead_belowbreve" => "◌̮",
        "dead_belowdiaeresis" => "◌̤",
        "dead_invertedbreve" => "◌̑",
        "dead_belowcomma" => "◌̦",
        _ => return None,
    })
}
