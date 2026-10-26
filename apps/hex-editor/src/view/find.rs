//! Find and replace in the buffer, for the window bar's search: the view is
//! the [`Findable`] the bar drives in place.
//!
//! A hex query is a byte pattern (`de ad be ef`); a text query is matched as
//! UTF-8, honouring the search's Match case and Whole word. Matches are byte
//! ranges; the grid paints each with `paint_find_match` over the matched
//! cells only. A replacement is bytes in the same mode as the query, and
//! goes through the view's own edit path as one undoable action. Find
//! (Ctrl+F) and Replace (Ctrl+H) open the bar's search; the view draws no
//! find field of its own.

use ui::widget::{FindMatch, FindMode, FindOptions, Findable};
use ui::window_toolbar::SearchOpening;

use super::{HexView, byte_edit};
use crate::model::ChangesLength;
use crate::search::{self, MatchRules, SearchKind};

/// The query the search last ran: its bytes and how they match.
#[derive(Clone, Debug, Default)]
pub(super) struct FindQuery {
    needle: Option<Vec<u8>>,
    rules: Option<MatchRules>,
    mode: FindMode,
}

impl Findable for HexView {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let (kind, rules) = match options.mode {
            FindMode::Hex => (SearchKind::Hex, MatchRules::EXACT),
            // The byte view offers no pattern matching: a text query is text.
            FindMode::Text | FindMode::Regex => (
                SearchKind::Text,
                MatchRules {
                    match_case: options.match_case,
                    whole_word: options.whole_word,
                },
            ),
        };
        let needle = search::needle(query, kind);
        self.matches = needle
            .as_ref()
            .map(|needle| {
                let (hits, _) = search::find_matches(
                    self.model.bytes(),
                    needle,
                    rules,
                    search::MAX_RETAINED_MATCHES,
                );
                hits.into_iter()
                    .map(|start| FindMatch::new(start, start.saturating_add(needle.len())))
                    .collect()
            })
            .unwrap_or_default();
        self.query = FindQuery {
            needle,
            rules: Some(rules),
            mode: options.mode,
        };
        self.match_idx = None;
        self.dirty = true;
        self.matches.clone()
    }

    fn content_revision(&self) -> u64 {
        self.revision
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
        self.query = FindQuery::default();
        self.dirty = true;
    }

    fn supports_replace(&self) -> bool {
        self.has_file
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(found) = self.matches.get(index) else {
            return false;
        };
        let span = (found.start, found.end);
        self.replace_spans(&[span], with) > 0
    }

    fn replace_all(&mut self, with: &str) -> usize {
        // Every match in the buffer, not only the ones kept to paint.
        let (Some(needle), Some(rules)) = (&self.query.needle, self.query.rules) else {
            return 0;
        };
        let (hits, _) = search::find_matches(self.model.bytes(), needle, rules, usize::MAX);
        let spans: Vec<(usize, usize)> = hits
            .into_iter()
            .map(|start| (start, start + needle.len()))
            .collect();
        let count = self.replace_spans(&spans, with);
        match count {
            0 => {}
            1 => self.notice("Replaced 1 match"),
            n => self.notice(&format!("Replaced {n} matches")),
        }
        count
    }

    fn supports_hex(&self) -> bool {
        true
    }
}

impl HexView {
    /// Open the window bar's search on the buffer: on its query for Find,
    /// on its replacement for Replace.
    pub(super) fn open_find(opening: SearchOpening) {
        ui::window_toolbar::open_search(opening);
    }

    /// The bytes `with` stands for in the mode of the last query: hex bytes
    /// in hex mode, its UTF-8 bytes in text mode.
    fn replacement(&self, with: &str) -> Option<Vec<u8>> {
        match self.query.mode {
            FindMode::Hex => crate::actions::parse_bytes(with).ok(),
            FindMode::Text | FindMode::Regex => Some(with.as_bytes().to_vec()),
        }
    }

    /// Replace `spans` with `with` in the view's copy and submit what that
    /// did as one action of the record, one undo step. Returns how many
    /// spans changed.
    fn replace_spans(&mut self, spans: &[(usize, usize)], with: &str) -> usize {
        let Some(bytes) = self.replacement(with) else {
            self.problem(
                "Couldn't replace",
                "Write the replacement as hexadecimal bytes, like de ad be ef.",
            );
            return 0;
        };
        match self.model.replace_spans(spans, &bytes) {
            Ok(0) => 0,
            Ok(count) => {
                self.submit_replacement();
                count
            }
            Err(ChangesLength) => {
                self.problem(
                    "Couldn't replace in Overwrite mode",
                    "The replacement has another length than the match. Switch to Insert to change the file's length.",
                );
                0
            }
        }
    }

    /// Submit the edits a replacement made as one action: a step of its own,
    /// never joined to typing before or after it.
    pub(super) fn submit_replacement(&mut self) {
        let edits = self.model.take_edits();
        self.content_changed();
        let (row, _) = self.model.cursor_cell();
        self.model.scroll_into_view(row, self.visible_rows);
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        dispatch.settle();
        dispatch.group(|| {
            for edit in edits.iter().filter_map(byte_edit) {
                dispatch.submit(edit);
            }
        });
        dispatch.settle();
    }

    /// The bytes changed: an open search finds again, keeping its place.
    pub(super) fn content_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.dirty = true;
    }
}
