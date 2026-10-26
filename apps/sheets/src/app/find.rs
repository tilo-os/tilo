//! Find and replace over the workbook, for the app's find band and the
//! window bar's search.
//!
//! The matcher is built on the platform's text folding
//! ([`ui::search::fold_char`]): case-insensitive search also folds
//! compatibility forms and alphabetic diacritics, so "creme" finds "Crème"
//! and "strasse" finds "Straße". `ui::search` itself is an index/ranker
//! (`locate` finds the first match in a field); it has no document-wide
//! find-all matcher, so this module walks the cells and keeps every match.
//!
//! A match is a byte range of a cell's raw input, which is the text replace
//! can edit. Find is also navigation: revealing a match switches to its
//! sheet, selects and scrolls to its cell, and says "match 4 of 12" in the
//! status line. Replacing is one grouped edit, so undo takes it back whole.

use ui::tilo_resource::Invoke;
use ui::widget::{FindMatch, FindOptions, Findable};

use super::{FoundCell, SheetsApp};
use crate::model::Addr;
use crate::workbook::{Cell, Workbook};

/// One compiled query: the needle as it is compared, and the rules it runs
/// under.
struct Query {
    needle: String,
    case_sensitive: bool,
    whole_word: bool,
}

impl Query {
    fn compile(query: &str, options: FindOptions) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        Some(Self {
            needle: if options.match_case {
                query.to_owned()
            } else {
                fold(query)
            },
            case_sensitive: options.match_case,
            whole_word: options.whole_word,
        })
    }
}

/// Fold `text`, keeping the raw byte start and end of every source scalar
/// for each folded byte, so a match in the folded text maps back to the
/// exact range in the cell.
fn folded_with_origins(text: &str) -> (String, Vec<usize>, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut starts = Vec::with_capacity(text.len());
    let mut ends = Vec::with_capacity(text.len());
    for (at, scalar) in text.char_indices() {
        let end = at.saturating_add(scalar.len_utf8());
        ui::search::fold_char(scalar, &mut |part| {
            folded.push(part);
            for _ in 0..part.len_utf8() {
                starts.push(at);
                ends.push(end);
            }
        });
    }
    (folded, starts, ends)
}

fn fold(text: &str) -> String {
    let (folded, _, _) = folded_with_origins(text);
    folded
}

fn is_word(scalar: char) -> bool {
    scalar.is_alphanumeric() || scalar == '_'
}

/// Every non-overlapping occurrence of `query` in `text`, as raw byte ranges.
fn matches_in(text: &str, query: &Query) -> Vec<(usize, usize)> {
    let (haystack, starts, ends) = if query.case_sensitive {
        (
            text.to_owned(),
            (0..text.len()).collect::<Vec<_>>(),
            (0..text.len()).map(|i| i.saturating_add(1)).collect(),
        )
    } else {
        folded_with_origins(text)
    };
    let needle = query.needle.as_str();
    let opens = needle.chars().next().is_some_and(|c| !is_word(c));
    let closes = needle.chars().next_back().is_some_and(|c| !is_word(c));
    let mut found = Vec::new();
    let mut from = 0usize;
    while from <= haystack.len() {
        let Some(relative) = haystack.get(from..).and_then(|rest| rest.find(needle)) else {
            break;
        };
        let at = from.saturating_add(relative);
        let end = at.saturating_add(needle.len());
        let before_ok = opens
            || at == 0
            || haystack
                .get(..at)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|before| !is_word(before));
        let after_ok = closes
            || end >= haystack.len()
            || haystack
                .get(end..)
                .and_then(|after| after.chars().next())
                .is_none_or(|after| !is_word(after));
        if (!query.whole_word || (before_ok && after_ok))
            && let (Some(start), Some(stop)) = (starts.get(at), ends.get(end.saturating_sub(1)))
        {
            found.push((*start, *stop));
        }
        from = if needle.is_empty() {
            at.saturating_add(1)
        } else {
            end
        };
    }
    found
}

/// `raw` with `start..end` replaced by `with`.
fn replace_range(raw: &str, start: usize, end: usize, with: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len().saturating_sub(end - start) + with.len());
    out.push_str(raw.get(..start)?);
    out.push_str(with);
    out.push_str(raw.get(end..)?);
    Some(out)
}

impl SheetsApp {
    /// Every match of `query` over the shown sheets, in sheet, row, column
    /// order.
    fn collect_find_matches(&self, query: &str, options: FindOptions) -> Vec<FoundCell> {
        let Some(query) = Query::compile(query, options) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (sheet, id) in self.sheet_ids.iter().enumerate() {
            let Some(state) = self.workbook.sheet(id) else {
                continue;
            };
            let snapshot = state.snapshot();
            let mut cells: Vec<(Addr, String)> = snapshot
                .raw
                .into_iter()
                .filter(|(_, raw)| !raw.is_empty())
                .collect();
            cells.sort_by_key(|(addr, _)| (addr.1, addr.0));
            for (addr, raw) in cells {
                for (start, end) in matches_in(&raw, &query) {
                    found.push(FoundCell {
                        sheet,
                        addr,
                        start,
                        end,
                    });
                }
            }
        }
        found
    }

    /// The raw input of a matched cell, if it still exists.
    fn find_cell_raw(&self, found: FoundCell) -> Option<String> {
        let id = self.sheet_ids.get(found.sheet)?;
        let sheet = self.workbook.sheet(id)?;
        let layout = sheet.layout();
        sheet
            .cell_at(&layout, found.addr)
            .map(|cell| cell.raw.0.clone())
    }

    /// One cell with `with` in place of the matched range.
    fn replaced_cell(&self, found: FoundCell, with: &str) -> Option<(usize, Addr, String)> {
        let raw = self.find_cell_raw(found)?;
        let replaced = replace_range(&raw, found.start, found.end, with)?;
        Some((found.sheet, found.addr, replaced))
    }

    /// Apply the replacements the bar asked for as one grouped edit: one
    /// history entry, one undo step, across every sheet they touch.
    pub(super) fn drain_find_replace(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if self.pending_find_replace.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_find_replace);
        let mut by_sheet: std::collections::BTreeMap<usize, Vec<(Addr, Cell)>> =
            std::collections::BTreeMap::new();
        for (sheet, addr, raw) in pending {
            let Some(id) = self.sheet_ids.get(sheet).cloned() else {
                continue;
            };
            let stored = self
                .workbook
                .sheet(&id)
                .and_then(|state| {
                    let layout = state.layout();
                    state.cell_at(&layout, addr).cloned()
                })
                .unwrap_or_default();
            let mut cell = stored;
            cell.raw = ui::tilo_resource::Atom(raw);
            by_sheet.entry(sheet).or_default().push((addr, cell));
        }
        let mut members: Vec<Invoke<Workbook>> = Vec::new();
        for (sheet, cells) in by_sheet {
            let Some(id) = self.sheet_ids.get(sheet) else {
                continue;
            };
            members.extend(
                crate::workbook::cell_edits(&self.workbook, id, &cells)
                    .iter()
                    .filter_map(|edit| Invoke::edit(edit).ok()),
            );
        }
        if members.is_empty() {
            return;
        }
        self.submit(doc, Invoke::group(members));
        self.reload();
        self.find.refresh();
        self.pending.redraw = true;
    }

    /// The status line's "match 4 of 12 — Budget!B3" navigation sentence.
    fn find_status(&self, index: usize) -> String {
        let total = self.find_matches.len();
        let Some(found) = self.find_matches.get(index) else {
            return String::new();
        };
        let sheet = self
            .sheet_names
            .get(found.sheet)
            .cloned()
            .unwrap_or_default();
        format!(
            "Match {} of {}: {}!{}",
            index.saturating_add(1),
            total,
            sheet,
            crate::model::addr_to_a1(found.addr.0, found.addr.1)
        )
    }
}

impl Findable for SheetsApp {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.find_matches = self.collect_find_matches(query, options);
        self.find_current = self
            .find_current
            .filter(|index| *index < self.find_matches.len());
        self.pending.redraw = true;
        (0..self.find_matches.len())
            .map(|index| FindMatch::new(index, index.saturating_add(1)))
            .collect()
    }

    fn reveal(&mut self, index: usize) {
        let Some(found) = self.find_matches.get(index).copied() else {
            return;
        };
        self.find_current = Some(index);
        if found.sheet != self.active_sheet {
            self.active_sheet = found.sheet.min(self.sheet_ids.len().saturating_sub(1));
            self.reload();
        }
        self.grid.select_only(found.addr);
        self.status = self.find_status(index);
        self.pending.redraw = true;
    }

    fn mark_current(&mut self, index: usize) {
        self.find_current = Some(index);
        self.status = self.find_status(index);
        self.pending.redraw = true;
    }

    fn clear_find(&mut self) {
        self.find_matches.clear();
        self.find_current = None;
        self.pending.redraw = true;
    }

    fn supports_replace(&self) -> bool {
        // The window bar's search keeps replacement usable at any width.
        // This app's own band drops the row when its two localised buttons
        // would leave no field (a phone with large text); Ctrl+H then opens
        // the window bar's search instead.
        self.dispatch.is_some() && !(self.find_open && !self.find_replace_roomy)
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(found) = self.find_matches.get(index).copied() else {
            return false;
        };
        let Some((sheet, addr, replaced)) = self.replaced_cell(found, with) else {
            return false;
        };
        self.pending_find_replace.push((sheet, addr, replaced));
        self.find_current = Some(index);
        self.pending.redraw = true;
        true
    }

    fn replace_all(&mut self, with: &str) -> usize {
        let mut per_cell: std::collections::BTreeMap<(usize, Addr), Vec<(usize, usize)>> =
            std::collections::BTreeMap::new();
        for found in &self.find_matches {
            per_cell
                .entry((found.sheet, found.addr))
                .or_default()
                .push((found.start, found.end));
        }
        let count = self.find_matches.len();
        for ((sheet, addr), mut ranges) in per_cell {
            let found = FoundCell {
                sheet,
                addr,
                start: 0,
                end: 0,
            };
            let Some(raw) = self.find_cell_raw(found) else {
                continue;
            };
            ranges.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            let mut replaced = raw;
            for (start, end) in ranges {
                if let Some(next) = replace_range(&replaced, start, end, with) {
                    replaced = next;
                }
            }
            self.pending_find_replace.push((sheet, addr, replaced));
        }
        self.pending.redraw = true;
        count
    }

    fn replaces_later(&self) -> bool {
        true
    }

    fn content_revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(text: &str, needle: &str, case: bool, whole: bool) -> Vec<(usize, usize)> {
        let query = Query::compile(
            needle,
            FindOptions {
                match_case: case,
                whole_word: whole,
                ..FindOptions::default()
            },
        )
        .expect("a query");
        matches_in(text, &query)
    }

    #[test]
    fn folding_finds_case_and_diacritics_and_maps_back_to_raw_bytes() {
        let text = "Le Café Crème, déjà vu";
        let found = matches(text, "creme", false, false);
        assert_eq!(found.len(), 1);
        assert_eq!(text.get(found[0].0..found[0].1), Some("Crème"));
        assert!(matches(text, "CREME", true, false).is_empty());
        assert_eq!(matches("Straße", "strasse", false, false).len(), 1);
    }

    #[test]
    fn whole_word_and_non_overlapping_matches_hold() {
        let text = "aa a aa";
        assert_eq!(matches(text, "aa", true, true).len(), 2);
        assert_eq!(matches(text, "a", true, true).len(), 1);
        // `aa` in `aaaa` is two matches, not three.
        assert_eq!(matches("aaaa", "aa", true, false).len(), 2);
    }

    #[test]
    fn replacements_are_byte_exact_and_can_expand() {
        assert_eq!(
            replace_range("crème", 0, 6, "creme").as_deref(),
            Some("creme")
        );
        assert_eq!(replace_range("abc", 1, 2, "XYZ").as_deref(), Some("aXYZc"));
    }
}
