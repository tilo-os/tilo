//! Find and replace in the active buffer.
//!
//! The bar is the toolkit's [`FindBar`]: the query field with its counter,
//! the Match case and Whole word chips, previous and next, and the replace
//! row. The editor answers it through [`Findable`] ([`FindTarget`]): it runs
//! the query over the buffer, keeps the matches to paint, selects the current
//! one, and replaces through the same action path every other edit takes, so
//! a replacement is one undoable step.

use ui::widget::{FindBar, FindMatch, FindOptions, Findable};

use super::{AutoCommit, CodeApp, EditorDoc, flat_index, replace_chars};
use crate::find::{byte_ranges, matches_in};
use crate::search::Query;

/// The active buffer as the find bar's target for one `drive`.
pub(super) struct FindTarget<'a> {
    pub(super) app: &'a mut CodeApp,
    pub(super) doc: &'a mut AutoCommit,
}

impl FindTarget<'_> {
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
        let name = self
            .app
            .session
            .active_buffer()
            .map_or_else(|| "file".to_string(), |buffer| buffer.name.clone());
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
            .dispatch_text_action(self.doc, file_id, &name, &text, &next)
        {
            self.app.editor.reconcile_from_doc(self.doc);
            self.app
                .note(format!("Text edit was refused: {error}"), true);
            return false;
        }
        true
    }
}

impl Findable for FindTarget<'_> {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let text = self.text().unwrap_or_default();
        let query = Query::new(query)
            .case_sensitive(options.match_case)
            .whole_word(options.whole_word);
        let matches = matches_in(&text, &query);
        let ranges = byte_ranges(&text, &matches);
        self.app.find_matches = matches;
        self.app.find_current = None;
        self.app
            .editor
            .set_find_matches(&self.app.find_matches, None);
        ranges
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

    /// Close the bar and stop painting matches. A closed bar is dropped, so
    /// the next one opens fresh.
    pub(super) fn close_find(&mut self) {
        self.find_open = false;
        self.find = FindBar::new().with_replace();
        self.clear_find_matches();
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
    pub(super) fn drive_find(&mut self, doc: &mut AutoCommit) {
        if !self.find_open {
            return;
        }
        let mut bar = std::mem::replace(&mut self.find, FindBar::new());
        bar.drive(&mut FindTarget { app: self, doc });
        self.find = bar;
        if self.find.close_requested() {
            self.close_find();
        }
    }

    /// Keep the highlights true to the buffer while it changes under an open
    /// bar: an edit that adds or removes a match is a new search.
    pub(super) fn refresh_find(&mut self, doc: &AutoCommit) {
        if !self.find_open {
            self.clear_find_matches();
            return;
        }
        let Some(node) = self.session.active_buffer().map(|b| b.node) else {
            return;
        };
        let text = EditorDoc::read_buffer_key(doc, &self.buffer_key(node)).unwrap_or_default();
        let options = self.find.options();
        let query = Query::new(self.find.query())
            .case_sensitive(options.match_case)
            .whole_word(options.whole_word);
        let fresh = matches_in(&text, &query);
        if fresh == self.find_matches {
            return;
        }
        self.find_current = self.find_current.filter(|i| *i < fresh.len());
        self.find_matches = fresh;
        self.editor
            .set_find_matches(&self.find_matches, self.find_current);
    }
}
