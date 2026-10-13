//! The person's diff preferences: which layout they read diffs in, whether
//! they are syntax-highlighted, whether whitespace shows and long lines wrap.
//! They follow the person to every device as a personal settings section,
//! never the shared document, so a collaborator's choice never changes how
//! someone else reads a diff.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

/// How the diff pane lays out old vs. new content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLayout {
    /// Single column with +/- gutter markers.
    #[serde(rename = "unified")]
    Unified,
    /// Two columns: removed on the left, added on the right.
    #[serde(rename = "split")]
    SideBySide,
}

/// The person's diff preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub diff_layout: DiffLayout,
    pub show_whitespace: bool,
    pub syntax_highlight: bool,
    pub wrap_lines: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            diff_layout: DiffLayout::Unified,
            show_whitespace: false,
            syntax_highlight: true,
            // Code wraps inside its pane by default, so no line is cut at
            // the pane edge; turning it off scrolls long lines sideways.
            wrap_lines: true,
        }
    }
}

impl SettingsSchema for Settings {
    const SCOPE: Scope = Scope::Personal;
}

impl Settings {
    pub fn toggle_layout(&mut self) {
        self.diff_layout = match self.diff_layout {
            DiffLayout::Unified => DiffLayout::SideBySide,
            DiffLayout::SideBySide => DiffLayout::Unified,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset preferences read as the defaults, and the layout keeps its
    /// short stored form.
    #[test]
    fn unset_preferences_are_the_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.diff_layout, DiffLayout::Unified);
        assert!(s.syntax_highlight);
        let split: Settings = serde_json::from_str(r#"{"diff_layout":"split"}"#).unwrap();
        assert_eq!(split.diff_layout, DiffLayout::SideBySide);
    }
}
