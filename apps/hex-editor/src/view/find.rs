//! Find in the buffer: the view is [`Findable`], the framework's
//! [`FindBar`](ui::widget::FindBar) is its bar.
//!
//! A hex query is a byte pattern (`de ad be ef`); a text query is matched as
//! UTF-8, honouring the bar's Match case and Whole word. Matches are byte
//! ranges; the grid paints each with `paint_find_match` over the matched
//! cells only.

use ui::widget::{FindMatch, FindMode, FindOptions, Findable};

use super::HexView;
use crate::search::{self, MatchRules, SearchKind};

impl Findable for HexView {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let (kind, rules) = match options.mode {
            FindMode::Hex => (SearchKind::Hex, MatchRules::EXACT),
            FindMode::Text => (
                SearchKind::Text,
                MatchRules {
                    match_case: options.match_case,
                    whole_word: options.whole_word,
                },
            ),
        };
        self.matches = search::needle(query, kind)
            .map(|needle| {
                let (hits, _) = search::find_matches(
                    self.model.bytes(),
                    &needle,
                    rules,
                    search::MAX_RETAINED_MATCHES,
                );
                hits.into_iter()
                    .map(|start| FindMatch::new(start, start.saturating_add(needle.len())))
                    .collect()
            })
            .unwrap_or_default();
        self.match_idx = None;
        self.dirty = true;
        self.matches.clone()
    }

    fn reveal(&mut self, index: usize) {
        let Some(found) = self.matches.get(index) else {
            return;
        };
        let start = found.start;
        self.match_idx = Some(index);
        self.model.move_to(start, false);
        let (row, _) = self.model.cursor_cell();
        self.model.scroll_into_view(row, self.visible_rows);
        self.dirty = true;
    }

    fn clear_find(&mut self) {
        self.matches.clear();
        self.match_idx = None;
        self.dirty = true;
    }

    fn supports_hex(&self) -> bool {
        true
    }
}

impl HexView {
    /// Open the find band (Ctrl+F) with its query focused.
    pub(super) fn open_find(&mut self) {
        self.find_open = true;
        self.find_bar.focus_query();
        self.relayout_now();
        self.drive_find();
        self.dirty = true;
    }

    /// Close the band and clear the matches. The query is kept, so Ctrl+F
    /// again finds the same thing.
    pub(super) fn close_find(&mut self) {
        let query = self.find_bar.query().to_owned();
        self.clear_find();
        self.find_bar = new_find_bar();
        if !query.is_empty() {
            self.find_bar.set_query(&query);
        }
        self.find_open = false;
        self.relayout_now();
        self.dirty = true;
    }

    /// Carry what the bar asked for into the view: re-run the query, step
    /// the current match, or close.
    pub(super) fn drive_find(&mut self) {
        let mut bar = std::mem::take(&mut self.find_bar);
        bar.drive(self);
        let closed = bar.close_requested();
        self.find_bar = bar;
        if closed {
            self.close_find();
        }
    }

    /// The matches changed under an edit: find again.
    pub(super) fn refind(&mut self) {
        if !self.find_open {
            self.matches.clear();
            self.match_idx = None;
            return;
        }
        let query = self.find_bar.query().to_owned();
        self.find_bar.set_query(&query);
        self.drive_find();
    }
}

/// The find bar of a byte view: text and hex modes.
pub(super) fn new_find_bar() -> ui::widget::FindBar {
    ui::widget::FindBar::new().with_hex_mode()
}
