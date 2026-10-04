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
        self.search_needle = search::needle(query, kind);
        self.search_rules = rules;
        self.matches_truncated = false;
        self.matches = self
            .search_needle
            .as_ref()
            .map(|needle| {
                let (hits, truncated) = search::find_matches(
                    self.model.bytes(),
                    needle,
                    rules,
                    search::MAX_RETAINED_MATCHES,
                );
                self.matches_truncated = truncated;
                hits.into_iter()
                    .map(|start| FindMatch::new(start, start.saturating_add(needle.len())))
                    .collect()
            })
            .unwrap_or_default();
        self.match_count = self.matches.len();
        let cursor = self.model.cursor();
        let mut nearest = self
            .matches
            .iter()
            .enumerate()
            .find(|(_, found)| found.start >= cursor)
            .map(|(index, found)| (index, found.start));
        if self.matches_truncated {
            // Count the full set without retaining it. Ordinary next/previous
            // steps below continue from their last offset, not byte zero.
            let tail = self
                .matches
                .last()
                .map_or(0, |found| found.start.saturating_add(1));
            for start in tail..self.model.len() {
                if self.match_starts_at(start) {
                    if nearest.is_none() && start >= cursor {
                        nearest = Some((self.match_count, start));
                    }
                    self.match_count += 1;
                }
            }
        }
        let nearest = nearest.or_else(|| self.matches.first().map(|found| (0, found.start)));
        self.match_idx = nearest.map(|(index, _)| index);
        self.current_match_start = nearest.map(|(_, start)| start);
        self.dirty = true;
        self.matches.clone()
    }

    fn match_count(&self) -> Option<usize> {
        Some(self.match_count)
    }

    fn current_match(&self) -> Option<usize> {
        self.match_idx
    }

    fn reveal(&mut self, index: usize) {
        let Some(start) = self.match_start(index) else {
            return;
        };
        self.match_idx = Some(index);
        self.current_match_start = Some(start);
        self.model.move_to(start, false);
        let (row, _) = self.model.cursor_cell();
        self.model.scroll_into_view(row, self.visible_rows);
        self.dirty = true;
    }

    fn clear_find(&mut self) {
        self.matches.clear();
        self.search_needle = None;
        self.matches_truncated = false;
        self.match_count = 0;
        self.current_match_start = None;
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
            self.clear_find();
            return;
        }
        let mut bar = std::mem::take(&mut self.find_bar);
        bar.refresh(self);
        self.find_bar = bar;
    }
}

/// The find bar of a byte view: text and hex modes.
pub(super) fn new_find_bar() -> ui::widget::FindBar {
    ui::widget::FindBar::new().with_hex_mode()
}

impl HexView {
    fn match_start(&self, index: usize) -> Option<usize> {
        if index >= self.match_count {
            return None;
        }
        if let Some(found) = self.matches.get(index) {
            return Some(found.start);
        }
        // Wrapping backwards finds the final hit from the end of the file.
        if index + 1 == self.match_count {
            return (0..self.model.len())
                .rev()
                .find(|start| self.match_starts_at(*start));
        }
        if let (Some(current), Some(start)) = (self.match_idx, self.current_match_start) {
            if index == current {
                return Some(start);
            }
            if index > current {
                return (start.saturating_add(1)..self.model.len())
                    .filter(|start| self.match_starts_at(*start))
                    .nth(index - current - 1);
            }
            return (0..start)
                .rev()
                .filter(|start| self.match_starts_at(*start))
                .nth(current - index - 1);
        }
        (0..self.model.len())
            .filter(|start| self.match_starts_at(*start))
            .nth(index)
    }

    pub(super) fn is_match_offset(&self, offset: usize) -> bool {
        let Some(needle) = &self.search_needle else {
            return false;
        };
        let first = offset.saturating_add(1).saturating_sub(needle.len());
        (first..=offset).any(|start| self.match_starts_at(start))
    }

    pub(super) fn match_starts_at(&self, start: usize) -> bool {
        let Some(needle) = &self.search_needle else {
            return false;
        };
        let bytes = self.model.bytes();
        let Some(window) = bytes.get(start..start.saturating_add(needle.len())) else {
            return false;
        };
        let same = if self.search_rules.match_case {
            window == needle
        } else {
            window.eq_ignore_ascii_case(needle)
        };
        let word = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'_';
        same && (!self.search_rules.whole_word
            || (!start
                .checked_sub(1)
                .and_then(|i| bytes.get(i))
                .is_some_and(word)
                && !bytes.get(start + needle.len()).is_some_and(word)))
    }
}
