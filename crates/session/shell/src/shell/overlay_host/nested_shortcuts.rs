//! Windowed-session defaults without changing the native desktop or saved preferences.

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};

use super::ShellOverlayHost;

impl ShellOverlayHost {
    /// Windowed compositors cannot rely on Super reaching them through the
    /// host desktop. Use ordinary Control chords for their entry points.
    pub(crate) fn set_nested_shortcuts(&mut self, enabled: bool) {
        self.nested_shortcuts = enabled.then(|| nested_config(&self.shortcut_config));
    }

    pub(super) fn refresh_nested_shortcuts(&mut self) {
        if self.nested_shortcuts.is_some() {
            self.set_nested_shortcuts(true);
        }
    }
}

fn nested_config(saved: &ShortcutConfig) -> ShortcutConfig {
    let defaults = ShortcutConfig::default();
    let mut effective = saved.clone();
    let bindings: [(ShortcutAction, &[&str]); 2] = [
        (ShortcutAction::Search, &["Ctrl"]),
        (ShortcutAction::WorkspaceOverview, &["Ctrl", "Shift"]),
    ];
    for (action, modifiers) in bindings {
        // Explicit rebindings win, as do desktop or published actions that
        // already own the proposed chord. Never silently take their input.
        if saved.bindings.get(&action) != defaults.bindings.get(&action) {
            continue;
        }
        let chord = KeyChord {
            modifiers: modifiers
                .iter()
                .map(|modifier| String::from(*modifier))
                .collect(),
            key: "space".into(),
        };
        if let Err(error) = effective.set(action, chord) {
            tracing::debug!(
                ?action,
                ?error,
                "nested shortcut preserves an existing binding"
            );
        }
    }
    effective
}

#[cfg(test)]
mod tests;
