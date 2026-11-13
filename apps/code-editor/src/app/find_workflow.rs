//! Find and replace in the active buffer, for the window bar's search.
//!

use ui::widget::toast::ToastKind;
use ui::widget::{FindMatch, FindOptions, Findable};
use ui::window_toolbar::SearchOpening;

use super::CodeApp;
use crate::find::{byte_ranges, matches_in};
use crate::resource::CATALOG;
use crate::search::Query;

impl Findable for CodeApp {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let query = Query::new(query)
            .case_sensitive(options.match_case)
            .whole_word(options.whole_word);
        let (matches, ranges) = if self.editor.is_bound() {
            let text = self.editor.text();
            let matches = matches_in(text, &query);
            let ranges = byte_ranges(text, &matches);
            (matches, ranges)
        } else {
            (Vec::new(), Vec::new())
        };
        self.find_current = self.find_current.filter(|i| *i < matches.len());
        self.find_matches = matches;
        self.editor
            .set_find_matches(&self.find_matches, self.find_current);
        ranges
    }

    fn content_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.session.active_buffer().map(|b| b.node).hash(&mut hash);
        self.editor.text().hash(&mut hash);
        hash.finish()
    }

    fn reveal(&mut self, index: usize) {
        self.focus_match(Some(index));
    }

    fn clear_find(&mut self) {
        self.clear_find_matches();
    }

    fn supports_replace(&self) -> bool {
        self.editor.is_bound()
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(m) = self.find_matches.get(index).copied() else {
            return false;
        };
        self.editor.replace_matches(&[m], with)
    }

    fn replace_all(&mut self, with: &str) -> usize {
        let matches = std::mem::take(&mut self.find_matches);
        let count = matches.len();
        if count == 0 || !self.editor.replace_matches(&matches, with) {
            self.find_matches = matches;
            return 0;
        }
        Self::note(
            match count {
                1 => CATALOG.t("code-editor-replaced-one"),
                n => CATALOG.t_args(
                    "code-editor-replaced",
                    &[("count", ui::i18n::ArgValue::Text(n.to_string()))],
                ),
            },
            ToastKind::Success,
        );
        count
    }
}

impl CodeApp {
    pub(super) fn open_find(opening: SearchOpening) {
        ui::window_toolbar::open_search(opening);
    }

    pub(super) fn clear_find_matches(&mut self) {
        if !self.find_matches.is_empty() || self.find_current.is_some() {
            self.find_matches.clear();
            self.find_current = None;
            self.editor.set_find_matches(&[], None);
        }
    }

    pub(super) fn focus_match(&mut self, index: Option<usize>) {
        self.find_current = index;
        if let Some(m) = index.and_then(|i| self.find_matches.get(i)).copied() {
            self.editor
                .set_selection_lc((m.line, m.col), (m.line, m.col + m.len));
        }
        self.editor
            .set_find_matches(&self.find_matches, self.find_current);
    }
}
