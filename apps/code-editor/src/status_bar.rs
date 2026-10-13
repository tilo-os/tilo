//! `StatusModel` — the editor's caret position, language, diagnostics and
//! buffer status, composed into the two readouts of the toolkit's status bar:
//! what the editor is doing at the leading edge, where the caret is at the
//! trailing edge.
//!
//! `CodeApp` refreshes a [`StatusState`] snapshot each frame and draws the two
//! strings in the band [`ui::chrome::status_bar::split`] reserves under the
//! editor. This type owns no render state and does no drawing — it holds the
//! snapshot and formats the readouts, which keeps it trivially unit-testable.

/// Snapshot of everything the status readout can show. The app refreshes this
/// each frame; the model composes the actionable subset into the readouts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusState {
    pub language: String,
    pub line: usize,
    pub col: usize,
    pub selection: usize,
    pub caret_count: usize,
    pub problems: usize,
    /// What the language server is doing — "rust-analyzer: indexing 42%".
    /// Empty when there is no server.
    pub lsp: String,
    /// True while the server is starting or busy — the readout surfaces the
    /// server's state only then, so a resting "ready" doesn't clutter it.
    pub lsp_working: bool,
    /// The last file-operation message ("Saved main.rs", "Could not save …").
    pub notice: String,
    /// True when the active buffer has unsaved edits.
    pub dirty: bool,
}

/// Holds the latest [`StatusState`] and composes it into the status bar's
/// readouts.
#[derive(Default)]
pub struct StatusModel {
    state: StatusState,
}

impl StatusModel {
    pub fn new() -> Self {
        StatusModel::default()
    }

    /// Replace the snapshot.
    pub fn set_state(&mut self, new: StatusState) {
        self.state = new;
    }

    /// The caret readout at the trailing edge: line and column, then how much
    /// is selected and how many carets there are when that is more than one.
    /// Blank while no buffer is open (line 0).
    pub fn right(&self) -> String {
        let s = &self.state;
        if s.line == 0 {
            return String::new();
        }
        let mut parts = vec![format!("Ln {}, Col {}", s.line, s.col)];
        if s.selection > 0 {
            parts.push(format!("{} selected", s.selection));
        }
        if s.caret_count > 1 {
            parts.push(format!("{} cursors", s.caret_count));
        }
        parts.join(", ")
    }

    /// The readout at the leading edge. A fresh file-operation notice leads (so
    /// a save is visibly a save and a failed one a failure); otherwise the
    /// language, then only the signals worth acting on: unsaved edits,
    /// problems, and a language server that is still working.
    pub fn left(&self) -> String {
        let s = &self.state;
        if !s.notice.is_empty() {
            return s.notice.clone();
        }
        let mut parts: Vec<String> = Vec::new();
        if !s.language.is_empty() {
            parts.push(s.language.clone());
        }
        if s.dirty {
            parts.push("Unsaved changes".to_string());
        }
        // Problems, only once there is at least one: a clean bill of health is
        // the resting state and needs no mention.
        if s.problems > 0 {
            parts.push(if s.problems == 1 {
                "1 problem".to_string()
            } else {
                format!("{} problems", s.problems)
            });
        }
        // Language-server progress, only while it is actually working (an
        // "indexing 42%" readout), not its idle "ready".
        if s.lsp_working && !s.lsp.is_empty() {
            parts.push(s.lsp.clone());
        }
        parts.join(", ")
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
    fn the_readouts_say_where_the_caret_is_and_only_what_is_worth_acting_on() {
        let cases: Vec<(&str, StatusState, &str, &str)> = vec![
            (
                "caret and language",
                caret(12, 5, "Rust"),
                "Rust",
                "Ln 12, Col 5",
            ),
            ("no buffer open", caret(0, 0, ""), "", ""),
            // No problems, no selection, an idle (not working) server, a clean
            // buffer and no notice => nothing beyond the caret and language.
            (
                "omits default and clean signals",
                StatusState {
                    language: "Rust".into(),
                    line: 3,
                    col: 9,
                    caret_count: 1,
                    lsp: "rust-analyzer: ready".into(),
                    ..Default::default()
                },
                "Rust",
                "Ln 3, Col 9",
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
                    ..Default::default()
                },
                "Rust, Unsaved changes, 4 problems, rust-analyzer: indexing 42%",
                "Ln 8, Col 2, 20 selected, 3 cursors",
            ),
            (
                "a notice leads",
                StatusState {
                    notice: "Saved main.rs".into(),
                    ..caret(2, 1, "Rust")
                },
                "Saved main.rs",
                "Ln 2, Col 1",
            ),
            (
                "problems count singularizes",
                StatusState {
                    problems: 1,
                    ..caret(1, 1, "Rust")
                },
                "Rust, 1 problem",
                "Ln 1, Col 1",
            ),
        ];
        for (name, state, left, right) in cases {
            let mut m = StatusModel::new();
            m.set_state(state);
            assert_eq!(m.left(), left, "left of {name}");
            assert_eq!(m.right(), right, "right of {name}");
        }
    }
}
