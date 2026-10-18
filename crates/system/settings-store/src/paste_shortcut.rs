//! Which key chord pastes in an app, for when the shell pastes on the
//! person's behalf (a clipboard history item chosen with Super+V that has no
//! text: an image, a file list). Most apps paste with Ctrl+V; terminals with
//! Ctrl+Shift+V, since Ctrl+V types a literal ^V there.
//!
//! The default comes from the app id ([`TERMINAL_PATTERNS`]); Settings →
//! Mouse & Keyboard stores an override per app id, so an app the patterns
//! miss is fixed without a code change:
//!
//! ```json
//! "input": { "paste_shortcuts": { "org.example.MyTerm": "ctrl_shift_v" } }
//! ```

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// Settings section and key that hold the per-app overrides.
pub const SECTION: &str = "input";
pub const KEY: &str = "paste_shortcuts";

/// Longest app id an override may name.
pub const MAX_APP_ID_LEN: usize = 255;

/// App-id fragments that mark a terminal, matched case-insensitively.
pub const TERMINAL_PATTERNS: [&str; 11] = [
    "terminal",
    "kitty",
    "alacritty",
    "foot",
    "wezterm",
    "konsole",
    "xterm",
    "ghostty",
    "tilix",
    "terminator",
    "ptyxis",
];

/// The chord that pastes in an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteShortcut {
    CtrlV,
    CtrlShiftV,
}

impl PasteShortcut {
    /// How it is stored.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::CtrlV => "ctrl_v",
            Self::CtrlShiftV => "ctrl_shift_v",
        }
    }

    /// The stored form back; `None` for anything else.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "ctrl_v" => Some(Self::CtrlV),
            "ctrl_shift_v" => Some(Self::CtrlShiftV),
            _ => None,
        }
    }
}

/// Whether `app_id` can carry an override: non-empty, bounded, printable.
#[must_use]
pub fn valid_app_id(app_id: &str) -> bool {
    !app_id.trim().is_empty()
        && app_id.len() <= MAX_APP_ID_LEN
        && !app_id.chars().any(char::is_control)
}

/// The chord the app id alone suggests: Ctrl+Shift+V for a terminal.
#[must_use]
pub fn default_for(app_id: &str) -> PasteShortcut {
    let id = app_id.to_ascii_lowercase();
    if TERMINAL_PATTERNS.iter().any(|name| id.contains(name)) {
        PasteShortcut::CtrlShiftV
    } else {
        PasteShortcut::CtrlV
    }
}

/// The chord that pastes in `app_id`: the person's override for it (app ids
/// compared case-insensitively), else [`default_for`].
#[must_use]
pub fn resolve(app_id: &str, overrides: &BTreeMap<String, String>) -> PasteShortcut {
    overrides
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(app_id))
        .and_then(|(_, shortcut)| PasteShortcut::parse(shortcut))
        .unwrap_or_else(|| default_for(app_id))
}

/// The valid overrides in an `input` section; malformed entries are skipped.
#[must_use]
pub fn overrides_from_section(section: &Map<String, Value>) -> BTreeMap<String, String> {
    section
        .get(KEY)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter(|(id, _)| valid_app_id(id))
                .filter_map(|(id, value)| {
                    let shortcut = PasteShortcut::parse(value.as_str()?)?;
                    Some((id.clone(), shortcut.id().to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Serde `deserialize_with` for a stored override map: keeps the valid
/// entries and drops the rest, so one bad entry never loses the section.
///
/// # Errors
///
/// Only when the input is not JSON at all.
pub fn lenient_overrides<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Value as serde::Deserialize>::deserialize(deserializer)?;
    let mut section = Map::new();
    section.insert(KEY.into(), value);
    Ok(overrides_from_section(&section))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_paste_with_ctrl_shift_v_unless_the_person_says_otherwise() {
        for id in [
            "so.tilo.terminal",
            "kitty",
            "org.gnome.Ptyxis",
            "foot",
            "Alacritty",
        ] {
            assert_eq!(default_for(id), PasteShortcut::CtrlShiftV, "{id}");
        }
        for id in ["so.tilo.notes", "org.mozilla.firefox"] {
            assert_eq!(default_for(id), PasteShortcut::CtrlV, "{id}");
        }
        let section: Map<String, Value> = serde_json::from_str(
            r#"{ "paste_shortcuts": {
                "org.example.Shell": "ctrl_shift_v",
                "so.tilo.terminal": "ctrl_v",
                "bad\u0007id": "ctrl_v",
                "org.example.Typo": "ctrl+v"
            } }"#,
        )
        .unwrap();
        let overrides = overrides_from_section(&section);
        assert_eq!(
            overrides.len(),
            2,
            "malformed entries are skipped: {overrides:?}"
        );
        assert_eq!(
            resolve("org.example.shell", &overrides),
            PasteShortcut::CtrlShiftV,
            "an unusual terminal is fixed by its override"
        );
        assert_eq!(
            resolve("so.tilo.terminal", &overrides),
            PasteShortcut::CtrlV
        );
        assert_eq!(
            resolve("org.example.Typo", &overrides),
            PasteShortcut::CtrlV
        );
        assert_eq!(resolve("kitty", &overrides), PasteShortcut::CtrlShiftV);
    }
}
