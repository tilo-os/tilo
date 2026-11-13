//! `StatusModel`: the editor's caret position, language, diagnostics and buffer status, composed into
//! the toolkit's status bar readouts.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusState {
    pub language: String,
    pub line: usize,
    pub col: usize,
    pub selection: usize,
    pub caret_count: usize,
    pub problems: usize,
    pub lsp: String,
    /// True while the server is starting or busy — the readout surfaces the
    /// server's state only then, so a resting "ready" doesn't clutter it.
    pub lsp_working: bool,
    pub dirty: bool,
    pub code_help: String,
}

#[derive(Default)]
pub struct StatusModel {
    state: StatusState,
}

impl StatusModel {
    pub fn new() -> Self {
        StatusModel::default()
    }

    pub fn set_state(&mut self, new: StatusState) {
        self.state = new;
    }

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

    pub fn trouble(&self) -> Option<String> {
        let s = &self.state;
        (!s.code_help.is_empty()).then(|| s.code_help.clone())
    }

    /// The readout at the leading edge: the language, then only the signals
    /// worth acting on: unsaved edits, problems, and a language server that is
    /// still working. What a file operation came to is said in the window's
    /// bar, which is where a message about an operation belongs.
    pub fn left(&self) -> String {
        let s = &self.state;
        let mut parts: Vec<String> = Vec::new();
        if !s.language.is_empty() && self.trouble().is_none() {
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
            (
                "omits default and clean signals",
                StatusState {
                    language: "Rust".into(),
                    line: 3,
                    col: 9,
                    caret_count: 1,
                    lsp: String::new(),
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
                    lsp: "Loading project 42%".into(),
                    lsp_working: true,
                    ..Default::default()
                },
                "Rust, Unsaved changes, 4 problems, Loading project 42%",
                "Ln 8, Col 2, 20 selected, 3 cursors",
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
            (
                "missing code help names the language in its own readout",
                StatusState {
                    code_help: "Rust: code help unavailable".into(),
                    dirty: true,
                    ..caret(4, 1, "Rust")
                },
                "Unsaved changes",
                "Ln 4, Col 1",
            ),
        ];
        for (name, state, left, right) in cases {
            let mut m = StatusModel::new();
            m.set_state(state);
            assert_eq!(m.left(), left, "left of {name}");
            assert_eq!(m.right(), right, "right of {name}");
        }
        let mut m = StatusModel::new();
        m.set_state(StatusState {
            code_help: "Rust: code help unavailable".into(),
            ..caret(4, 1, "Rust")
        });
        assert_eq!(m.trouble().as_deref(), Some("Rust: code help unavailable"));
    }
}
