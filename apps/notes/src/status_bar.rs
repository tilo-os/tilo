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

    /// How much the notebook and the open note hold, as a sentence fragment.
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
                format!("{pages}, {words}")
            }
            None => pages,
        }
    }

    /// The bubble's label: the save state and, once the note is saved, how
    /// long it is. A save that is running or has failed says only that.
    pub fn bubble_text(&self) -> String {
        match (self.state.save_status, self.state.word_count) {
            (SaveStatus::Saved, Some(1)) => format!("{}, 1 word", self.save_text()),
            (SaveStatus::Saved, Some(words)) => format!("{}, {words} words", self.save_text()),
            _ => self.save_text().to_string(),
        }
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
    fn status_text_describes_save_state_counts_and_words() {
        let mut bar = StatusModel::new();
        for (status, text) in [
            (SaveStatus::Saved, "Saved"),
            (SaveStatus::Saving, "Saving…"),
            (SaveStatus::Failed, "Save failed"),
        ] {
            bar.set_state(StatusBarState {
                save_status: status,
                ..Default::default()
            });
            assert_eq!(bar.save_text(), text, "save status {status:?}");
        }

        // (page path, pages, words, right-hand text): counts singularize and a
        // missing word count is dropped.
        for (page_path, page_count, word_count, text) in [
            ("Welcome", 1, Some(1), "1 note, 1 word"),
            ("Welcome", 4, Some(142), "4 notes, 142 words"),
            ("", 3, None, "3 notes"),
        ] {
            bar.set_state(StatusBarState {
                save_status: SaveStatus::Saved,
                page_path: page_path.into(),
                page_count,
                word_count,
            });
            assert_eq!(
                bar.right_text(),
                text,
                "{page_count} pages, {word_count:?} words"
            );
        }

        // "**bold** italic and `code`" is 4 word-tokens by whitespace
        // splitting. The status bar uses this so its count matches a human's
        // mental model regardless of inline markdown markers (the markers
        // count as part of the surrounding word).
        for (text, words) in [
            ("**bold** italic and `code`", 4),
            ("", 0),
            ("   ", 0),
            ("one\ntwo\tthree", 3),
        ] {
            assert_eq!(count_words(text), words, "words in {text:?}");
        }
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
