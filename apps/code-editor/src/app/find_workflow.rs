//! Find and replace in the active buffer.
//!
//! The bar is the toolkit's [`FindBar`]: the query field with its counter,
//! the Match case and Whole word chips, previous and next, and the replace
//! row. The editor answers it through [`Findable`] ([`FindTarget`]): it runs
//! the query over the buffer, keeps the matches to paint, selects the current
//! one, and replaces through the same action path every other edit takes, so
//! a replacement is one undoable step.

use ui::widget::{FindBar, FindMatch, FindOptions, Findable};

use super::{CodeApp, EditorDoc, flat_index, replace_chars};
use crate::find::{byte_ranges, matches_in};
use crate::search::Query;

/// The active buffer as the find bar's target for one `drive`.
pub(super) struct FindTarget<'a, 'doc> {
    pub(super) app: &'a mut CodeApp,
    pub(super) doc: &'a mut ui::widget::Edits<'doc>,
}

impl FindTarget<'_, '_> {
    /// The active buffer's text, if a file is open.
    fn text(&self) -> Option<String> {
        let node = self.app.session.active_buffer().map(|b| b.node)?;
        EditorDoc::read_buffer_key(self.doc, &self.app.buffer_key(node))
    }

    /// Replace `matches` (in document order) with `with` in one edit.
    fn splice(&mut self, matches: &[crate::find::Match], with: &str) -> bool {
        let Some(node) = self.app.session.active_buffer().map(|b| b.node) else {
            return false;
        };
        let file_id = self.app.buffer_key(node);
        let text = EditorDoc::read_buffer_key(self.doc, &file_id).unwrap_or_default();
        // Replacements are applied last-to-first so each splice's index is
        // still valid: replacing forwards would shift every later match by the
        // difference in length.
        let mut next = text.clone();
        for m in matches.iter().rev() {
            if let Some(at) = flat_index(&text, m.line, m.col) {
                replace_chars(&mut next, at, m.len, with);
            }
        }
        if let Err(error) = self
            .app
            .dispatch_text_action(self.doc, file_id, &text, &next)
        {
            self.app.editor.reconcile_from_doc(self.doc);
            self.app
                .note(format!("Text edit was refused: {error}"), true);
            return false;
        }
        true
    }
}

impl Findable for FindTarget<'_, '_> {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let text = self.text().unwrap_or_default();
        let query = Query::new(query)
            .case_sensitive(options.match_case)
            .whole_word(options.whole_word);
        let matches = matches_in(&text, &query);
        let ranges = byte_ranges(&text, &matches);
        // A find again after an edit keeps the emphasised match; a new
        // query reveals its first one.
        self.app.find_current = self.app.find_current.filter(|i| *i < matches.len());
        self.app.find_matches = matches;
        self.app
            .editor
            .set_find_matches(&self.app.find_matches, self.app.find_current);
        ranges
    }

    fn content_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.app
            .session
            .active_buffer()
            .map(|b| b.node)
            .hash(&mut hash);
        self.text().hash(&mut hash);
        hash.finish()
    }

    fn reveal(&mut self, index: usize) {
        self.app.focus_match(Some(index));
    }

    fn clear_find(&mut self) {
        self.app.clear_find_matches();
    }

    fn supports_replace(&self) -> bool {
        true
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(m) = self.app.find_matches.get(index).copied() else {
            return false;
        };
        if !self.splice(&[m], with) {
            return false;
        }
        self.app
            .editor
            .move_caret_to(m.line, m.col + with.chars().count());
        true
    }

    fn replace_all(&mut self, with: &str) -> usize {
        let matches = self.app.find_matches.clone();
        let count = matches.len();
        if count == 0 || !self.splice(&matches, with) {
            return 0;
        }
        self.app.note(
            match count {
                1 => "Replaced 1 match".to_string(),
                n => format!("Replaced {n} matches"),
            },
            false,
        );
        count
    }
}

impl CodeApp {
    /// Open the find bar, seeded with the editor's selection, and focus its
    /// query. The bar always offers its replace row, so find and replace are
    /// one command.
    pub(super) fn open_find(&mut self) {
        self.find_open = true;
        let seed = self
            .editor
            .selected_text_public()
            .filter(|text| !text.is_empty() && !text.contains('\n'));
        if let Some(seed) = seed {
            self.find.set_query(&seed);
        }
        self.find.focus_query();
    }

    /// Close the bar and stop painting matches. The bar keeps its query and
    /// options for the next time it opens.
    pub(super) fn close_find(&mut self) {
        self.find_open = false;
        self.clear_find_matches();
    }

    /// Open the bar with its replacement field focused (Ctrl+H).
    pub(super) fn open_replace(&mut self) {
        self.open_find();
        self.find.focus_replace();
    }

    pub(super) fn clear_find_matches(&mut self) {
        if !self.find_matches.is_empty() || self.find_current.is_some() {
            self.find_matches.clear();
            self.find_current = None;
            self.editor.set_find_matches(&[], None);
        }
    }

    /// Make match `index` current: select it, so the next keystroke replaces
    /// it the way it would in any editor, and keep it on screen.
    pub(super) fn focus_match(&mut self, index: Option<usize>) {
        self.find_current = index;
        if let Some(m) = index.and_then(|i| self.find_matches.get(i)).copied() {
            self.editor
                .set_selection_lc((m.line, m.col), (m.line, m.col + m.len));
        }
        self.editor
            .set_find_matches(&self.find_matches, self.find_current);
    }

    /// Carry what the user did to the bar this frame (a new query, a step, a
    /// replacement, a close) over to the buffer.
    pub(super) fn drive_find(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if !self.find_open {
            return;
        }
        let mut bar = std::mem::replace(&mut self.find, FindBar::new());
        bar.drive(&mut FindTarget { app: self, doc });
        self.find = bar;
        if self.find.take_close_requested() {
            self.close_find();
        }
    }

    /// A closed bar paints no matches. While it is open, the bar itself
    /// finds again whenever the buffer's text changes
    /// ([`Findable::content_revision`]), so its counter and the highlights
    /// follow every edit in the same frame.
    pub(super) fn refresh_find(&mut self) {
        if !self.find_open {
            self.clear_find_matches();
        }
    }
}
