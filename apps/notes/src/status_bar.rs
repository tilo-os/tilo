//! `StatusModel` — the note's save + page/word status, surfaced as a
//! transient floating [`ui::StatusBubble`] rather than a permanent bottom
//! strip (which stole a row of vertical space from the editor at every
//! width, and read especially poorly on mobile).
//!
//! `NotesApp` refreshes a [`StatusBarState`] snapshot and, when it changes,
//! pokes the bubble for save or navigation changes, then quietly fades.
//! Word counts update without reopening it; failures remain visible. The
//! bubble also appears while the pointer hovers the bottom-right corner.
//!
//! This type owns no CRDT state and does no rendering — it holds the
//! snapshot and formats the labels, which keeps it trivially unit-testable.

use ui::theme::Theme;
use vello::peniko::Color;

use crate::storage::SaveStatus;

/// Snapshot of the note's status. Cheap to clone (small strings, two
/// numbers, one enum).
#[derive(Clone, Debug, Default)]
pub struct StatusBarState {
    /// Combined save-state — the worse of (workspace saver, page saver)
    /// so the UI surfaces the most actionable signal.
    pub save_status: SaveStatus,
    /// Breadcrumb path of the selected page, e.g. `Notes / Welcome`.
    /// Empty when nothing is selected.
    pub page_path: String,
    /// Total number of pages in the workspace.
    pub page_count: usize,
    /// Word count of the currently-selected page's body. None when
    /// nothing is selected.
    pub word_count: Option<usize>,
}

/// Holds the latest [`StatusBarState`] and formats it for the floating
/// status bubble.
#[derive(Default)]
pub struct StatusModel {
    state: StatusBarState,
}

impl StatusModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the snapshot, returning whether anything observable changed
    /// (so the caller can poke the bubble only on a real change).
    pub fn set_state(&mut self, new: StatusBarState) -> bool {
        let changed = self.state.save_status != new.save_status
            || self.state.page_path != new.page_path
            || self.state.page_count != new.page_count
            || self.state.word_count != new.word_count;
        if changed {
            self.state = new;
        }
        changed
    }

    /// Read accessor for tests.
    pub fn state(&self) -> &StatusBarState {
        &self.state
    }

    pub fn save_text(&self) -> &'static str {
        match self.state.save_status {
            SaveStatus::Saved => "Saved",
            SaveStatus::Saving => "Saving…",
            SaveStatus::Failed => "Save failed",
            SaveStatus::SessionOnly => "Session only",
        }
    }

    pub fn save_color(&self, theme: &Theme) -> Color {
        match self.state.save_status {
            SaveStatus::Saved | SaveStatus::SessionOnly => theme.tokens.colors.text_dim,
            SaveStatus::Saving => theme.accent_primary,
            SaveStatus::Failed => theme.tokens.colors.semantic.danger,
        }
    }

    pub fn right_text(&self) -> String {
        let pages = match self.state.page_count {
            1 => "1 note".to_string(),
            n => format!("{n} notes"),
        };
        match self.state.word_count {
            Some(w) => {
                let words = match w {
                    1 => "1 word".to_string(),
                    _ => format!("{w} words"),
                };
                format!("{pages} · {words}")
            }
            None => pages,
        }
    }

    /// The full bubble label: save state, then the page/word counts.
    pub fn bubble_text(&self) -> String {
        format!("{}  ·  {}", self.save_text(), self.right_text())
    }
}

/// Compute a word count over a markdown body. Used by `NotesApp` to
/// fill the right-hand status-bar slot for the selected page. We
/// intentionally count via [`str::split_whitespace`] so the result
/// matches what a user thinks of as "words" (consecutive non-whitespace
/// runs) rather than what a markdown tokenizer would say (which would
/// double-count things like `**bold**` as separate tokens).
pub fn count_words(body: &str) -> usize {
    body.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_indicator_text_per_status() {
        let mut bar = StatusModel::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            ..Default::default()
        });
        assert_eq!(bar.save_text(), "Saved");
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saving,
            ..Default::default()
        });
        assert_eq!(bar.save_text(), "Saving…");
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Failed,
            ..Default::default()
        });
        assert_eq!(bar.save_text(), "Save failed");
    }

    #[test]
    fn right_text_singularizes_page_and_word() {
        let mut bar = StatusModel::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "Welcome".into(),
            page_count: 1,
            word_count: Some(1),
        });
        assert_eq!(bar.right_text(), "1 note · 1 word");
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "Welcome".into(),
            page_count: 4,
            word_count: Some(142),
        });
        assert_eq!(bar.right_text(), "4 notes · 142 words");
    }

    #[test]
    fn right_text_drops_word_count_when_none() {
        let mut bar = StatusModel::new();
        bar.set_state(StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: String::new(),
            page_count: 3,
            word_count: None,
        });
        assert_eq!(bar.right_text(), "3 notes");
    }

    #[test]
    fn count_words_handles_punctuation_and_marks() {
        // "**bold** italic and `code`" is 4 word-tokens by
        // whitespace splitting. The status bar uses this so its
        // count matches a human's mental model regardless of inline
        // markdown markers (the markers count as part of the
        // surrounding word).
        assert_eq!(count_words("**bold** italic and `code`"), 4);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("   "), 0);
        assert_eq!(count_words("one\ntwo\tthree"), 3);
    }

    #[test]
    fn set_state_reports_change_only_when_observably_different() {
        let mut bar = StatusModel::new();
        let s = StatusBarState {
            save_status: SaveStatus::Saved,
            page_path: "X".into(),
            page_count: 1,
            word_count: Some(0),
        };
        assert!(bar.set_state(s.clone()), "first snapshot is a change");
        assert!(
            !bar.set_state(s),
            "an identical snapshot reports no change (so the bubble is not re-poked)"
        );
        assert!(
            bar.set_state(StatusBarState {
                save_status: SaveStatus::Saving,
                page_path: "X".into(),
                page_count: 1,
                word_count: Some(0),
            }),
            "a save-state flip is a change"
        );
    }
}
