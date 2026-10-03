//! `StatusModel` — the editor's caret position, language, diagnostics and
//! buffer status, surfaced as a transient floating [`ui::StatusBubble`] rather
//! than a permanent full-width bottom strip (which stole a row of vertical
//! space from the editor at every width).
//!
//! `CodeApp` refreshes a [`StatusState`] snapshot each frame and, when the
//! composed label actually changes (a caret move, a save flip, an indexing
//! tick), pokes the bubble so it surfaces briefly then quietly fades. The
//! bubble also appears while the pointer hovers the bottom-right corner.
//!
//! This type owns no render state and does no drawing — it holds the snapshot
//! and formats the label, which keeps it trivially unit-testable.

/// Snapshot of everything the status readout can show. The app refreshes this
/// each frame; the model composes the actionable subset into the floating
/// bubble's label.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusState {
    pub language: String,
    pub line: usize,
    pub col: usize,
    pub selection: usize,
    pub caret_count: usize,
    pub indent: String,
    pub problems: usize,
    pub project: String,
    /// What the language server is doing — "rust-analyzer: indexing 42%".
    /// Empty when there is no server.
    pub lsp: String,
    /// True while the server is starting or busy — the label surfaces the LSP
    /// readout only then, so a resting "ready" doesn't clutter it.
    pub lsp_working: bool,
    /// The last file-operation message ("Saved main.rs", "Could not save …").
    pub notice: String,
    pub notice_is_error: bool,
    /// True when the active buffer has unsaved edits.
    pub dirty: bool,
}

/// Holds the latest [`StatusState`] and composes it into the floating status
/// bubble's label.
#[derive(Default)]
pub struct StatusModel {
    state: StatusState,
    /// The last composed label, so `set_state` reports whether the string the
    /// bubble shows actually changed (and only then pokes it).
    label: String,
}

impl StatusModel {
    pub fn new() -> Self {
        let mut m = StatusModel::default();
        m.label = m.compose();
        m
    }

    /// Replace the snapshot. Returns whether the composed bubble label changed,
    /// so the caller pokes the bubble only on a real change — `refresh_status`
    /// runs every frame, and poking unconditionally would pin the bubble open
    /// forever (and re-poke it on state changes the label never surfaces, like
    /// the project name or indent width).
    pub fn set_state(&mut self, new: StatusState) -> bool {
        if new == self.state {
            return false;
        }
        self.state = new;
        let label = self.compose();
        let changed = label != self.label;
        self.label = label;
        changed
    }

    /// The full bubble label: the caret + language, then any actionable
    /// contextual bits, combined with " · " separators.
    pub fn bubble_text(&self) -> String {
        self.label.clone()
    }

    /// Compose the label from the current snapshot. The primary segment is the
    /// caret position and the language; the remaining segments are appended only
    /// when they carry a genuinely-actionable signal (a dirty buffer, a live
    /// selection, real problems, an indexing server, a fresh file-op notice).
    fn compose(&self) -> String {
        let s = &self.state;
        let mut parts: Vec<String> = Vec::new();

        // Primary: caret position, then the language when a buffer is bound.
        if s.language.is_empty() {
            parts.push(format!("Ln {}, Col {}", s.line, s.col));
        } else {
            parts.push(format!("Ln {}, Col {} · {}", s.line, s.col, s.language));
        }

        // Unsaved marker for the active buffer.
        if s.dirty {
            parts.push("\u{25CF} Unsaved".to_string());
        }

        // Multi-selection: characters selected and any extra carets.
        if s.selection > 0 {
            parts.push(format!("{} selected", s.selection));
        }
        if s.caret_count > 1 {
            parts.push(format!("{} cursors", s.caret_count));
        }

        // Problems, only once there is at least one — a clean bill of health is
        // the resting state and needs no chip.
        if s.problems > 0 {
            let word = if s.problems == 1 {
                "problem"
            } else {
                "problems"
            };
            parts.push(format!("\u{26A0} {} {}", s.problems, word));
        }

        // Language-server progress, only while it is actually working (an
        // "indexing 42%" readout), not its idle "ready".
        if s.lsp_working && !s.lsp.is_empty() {
            parts.push(s.lsp.clone());
        }

        // The last file-operation notice, so a save is visibly a save and a
        // failed one is visibly a failure.
        if !s.notice.is_empty() {
            parts.push(s.notice.clone());
        }

        parts.join(" \u{b7} ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(line: usize, col: usize, language: &str) -> StatusState {
        StatusState {
            language: language.to_string(),
            line,
            col,
            ..Default::default()
        }
    }

    #[test]
    fn bubble_label_shows_caret_language_and_only_actionable_signals() {
        let cases: Vec<(&str, StatusState, &str)> = vec![
            (
                "leads with caret and language",
                caret(12, 5, "Rust"),
                "Ln 12, Col 5 · Rust",
            ),
            (
                "drops language when no buffer is bound",
                caret(1, 1, ""),
                "Ln 1, Col 1",
            ),
            // No problems, no selection, an idle (not working) server, a clean
            // buffer and no notice ⇒ nothing beyond the caret + language.
            (
                "omits default and clean signals",
                StatusState {
                    language: "Rust".into(),
                    line: 3,
                    col: 9,
                    problems: 0,
                    selection: 0,
                    caret_count: 1,
                    lsp: "rust-analyzer: ready".into(),
                    lsp_working: false,
                    dirty: false,
                    ..Default::default()
                },
                "Ln 3, Col 9 · Rust",
            ),
            (
                "appends actionable contextual bits",
                StatusState {
                    language: "Rust".into(),
                    line: 8,
                    col: 2,
                    dirty: true,
                    selection: 20,
                    caret_count: 3,
                    problems: 4,
                    lsp: "rust-analyzer: indexing 42%".into(),
                    lsp_working: true,
                    notice: "Saved main.rs".into(),
                    ..Default::default()
                },
                "Ln 8, Col 2 · Rust · \u{25CF} Unsaved · 20 selected · 3 cursors · \u{26A0} 4 problems · rust-analyzer: indexing 42% · Saved main.rs",
            ),
            (
                "problems count singularizes",
                StatusState {
                    problems: 1,
                    ..caret(1, 1, "Rust")
                },
                "Ln 1, Col 1 · Rust · \u{26A0} 1 problem",
            ),
        ];
        for (name, state, want) in cases {
            let mut m = StatusModel::new();
            m.set_state(state);
            assert_eq!(m.bubble_text(), want, "case {name}");
        }
    }

    #[test]
    fn set_state_reports_change_only_when_the_label_changes() {
        let mut m = StatusModel::new();
        assert!(
            m.set_state(caret(1, 1, "Rust")),
            "first real snapshot changes the label"
        );
        assert!(
            !m.set_state(caret(1, 1, "Rust")),
            "an identical snapshot reports no change (so the bubble is not re-poked)"
        );
        // A field the label never surfaces (the project name) must NOT re-poke.
        assert!(
            !m.set_state(StatusState {
                project: "some-project".into(),
                ..caret(1, 1, "Rust")
            }),
            "a change the label doesn't surface reports no change"
        );
        // A caret move the label does surface must poke.
        assert!(
            m.set_state(StatusState {
                project: "some-project".into(),
                ..caret(2, 1, "Rust")
            }),
            "a caret move changes the label"
        );
    }
}
