//! Find and replace in the document for the window bar's search.
//!
//! The editor is the [`ui::widget::Findable`] the bar's search drives
//! (`widget/find.rs`); this keeps what a find needs between the bar's calls:
//! the whole document's paragraphs as of its last change (the editor's own
//! paragraphs are only those around the viewport), the matches in the
//! document's own terms, `(block, start, end)` in characters, the current
//! one, and a replacement the bar asked for until the editor applies it
//! through its own actions.

use tilo_doc::snapshot::BlockView;
use ui::widget::FindOptions;

pub(crate) type FindMatch = (String, usize, usize);

/// A replacement the bar asked for, applied with the document in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingReplace {
    pub(crate) matches: Vec<FindMatch>,
    pub(crate) with: String,
}

#[derive(Debug, Default)]
pub(crate) struct DocumentFind {
    /// A search is open on the document.
    active: bool,
    query: String,
    options: FindOptions,
    /// Every paragraph as of the document's last change, while a search is
    /// open; `None` until the editor reads them.
    blocks: Option<Vec<BlockView>>,
    /// Moves whenever `blocks` does, so the bar finds again.
    revision: u64,
    matches: Vec<FindMatch>,
    current: Option<usize>,
    /// A query ran before the paragraphs were read: the first match is
    /// shown once they are, as the bar shows it for a query that found.
    reveal_on_read: bool,
    /// Replacements the bar asked for, in order, each against the
    /// paragraphs as the ones before it left them.
    pending: Vec<PendingReplace>,
}

impl DocumentFind {
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    pub(crate) fn active_match(&self) -> Option<&FindMatch> {
        self.current.and_then(|index| self.matches.get(index))
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the editor should read every paragraph again: a search is
    /// open and has none yet, or the document `moved`.
    pub(crate) fn wants_blocks(&self, moved: bool) -> bool {
        self.active && (moved || self.blocks.is_none())
    }

    /// The document's paragraphs as they are now; the query runs on them
    /// again, keeping the current match where it still is.
    pub(crate) fn set_blocks(&mut self, blocks: Vec<BlockView>) {
        self.blocks = Some(blocks);
        self.revision = self.revision.wrapping_add(1);
        self.rerun();
    }

    /// Run `query` under `options` on the paragraphs; an empty query closes
    /// the search. Returns how many matches there are.
    pub(crate) fn find(&mut self, query: &str, options: FindOptions) -> usize {
        if query.is_empty() {
            self.clear();
            return 0;
        }
        if !self.active || self.query != query || self.options != options {
            self.current = None;
        }
        self.active = true;
        self.reveal_on_read = self.blocks.is_none();
        query.clone_into(&mut self.query);
        self.options = options;
        self.rerun();
        self.matches.len()
    }

    fn rerun(&mut self) {
        self.matches = self.blocks.as_deref().map_or_else(Vec::new, |blocks| {
            find_in(blocks, &self.query, self.options)
        });
        self.current = self.current.filter(|index| *index < self.matches.len());
    }

    /// Make match `index` the current one; whether there is such a match.
    pub(crate) fn reveal(&mut self, index: usize) -> bool {
        let found = index < self.matches.len();
        self.current = found.then_some(index);
        self.reveal_on_read = false;
        found
    }

    /// Whether to show the first match now the paragraphs are read: once,
    /// for a query that ran before them.
    pub(crate) fn take_first_reveal(&mut self) -> bool {
        self.blocks.is_some() && std::mem::take(&mut self.reveal_on_read) && self.reveal(0)
    }

    /// The search closed: forget its matches and the paragraphs read for it.
    pub(crate) fn clear(&mut self) {
        self.active = false;
        self.query.clear();
        self.blocks = None;
        self.matches.clear();
        self.current = None;
        self.reveal_on_read = false;
    }

    /// Ask for `matches` to be replaced with `with`, and show the paragraphs
    /// as they will read, so the bar's count follows at once. Returns how
    /// many matches are to be replaced.
    pub(crate) fn queue_replace(&mut self, matches: Vec<FindMatch>, with: &str) -> usize {
        if matches.is_empty() {
            return 0;
        }
        if let Some(blocks) = self.blocks.as_mut() {
            splice_blocks(blocks, &matches, with);
        }
        let count = matches.len();
        self.pending.push(PendingReplace {
            matches,
            with: with.to_owned(),
        });
        self.rerun();
        count
    }

    /// The replacements to apply now, in the order the bar asked for them.
    pub(crate) fn take_pending(&mut self) -> Vec<PendingReplace> {
        std::mem::take(&mut self.pending)
    }
}

/// Replace `matches` in `blocks` with `with`, from the last offset of a
/// paragraph back so the earlier offsets hold.
pub(crate) fn splice_blocks(blocks: &mut [BlockView], matches: &[FindMatch], with: &str) {
    let mut ordered = matches.to_vec();
    ordered.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    for (block, start, end) in ordered.iter().rev() {
        let Some(view) = blocks.iter_mut().find(|view| &view.id == block) else {
            continue;
        };
        let chars: Vec<char> = view.text.chars().collect();
        let (Some(head), Some(tail)) = (chars.get(..*start), chars.get(*end..)) else {
            continue;
        };
        if start > end {
            continue;
        }
        let mut text: String = head.iter().collect();
        text.push_str(with);
        text.extend(tail);
        view.text = text;
    }
}

/// Every match of `query` in `blocks` under `options`, as character spans.
fn find_in(blocks: &[BlockView], query: &str, options: FindOptions) -> Vec<FindMatch> {
    if options.mode == ui::widget::FindMode::Regex {
        return find_pattern(blocks, query, options);
    }
    // Match against the original text: lowercasing can expand a character
    // (for example İ), so offsets in a folded copy are not document offsets.
    find_pattern(blocks, &regex::escape(query), options)
}

/// Every match of the regular expression `query` in the paragraphs, in
/// characters. A pattern that does not compile, or matches only empty
/// spans, finds nothing.
fn find_pattern(blocks: &[BlockView], query: &str, options: FindOptions) -> Vec<FindMatch> {
    let Ok(pattern) = regex::RegexBuilder::new(query)
        .case_insensitive(!options.match_case)
        .build()
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for block in blocks {
        let chars: Vec<char> = block.text.chars().collect();
        let char_at = |byte: usize| {
            block
                .text
                .get(..byte)
                .map_or(0, |head| head.chars().count())
        };
        let mut cursor = 0;
        while let Some(hit) = pattern.find_at(&block.text, cursor) {
            let (start, end) = (char_at(hit.start()), char_at(hit.end()));
            let accepted =
                !hit.is_empty() && (!options.whole_word || is_word_match(&chars, start, end));
            if accepted {
                found.push((block.id.clone(), start, end));
            }
            if hit.is_empty() || (!accepted && options.mode != ui::widget::FindMode::Regex) {
                // A rejected literal must not consume a later overlapping
                // candidate. Advance one original Unicode scalar, not one byte.
                let Some(next) = block.text.get(hit.start()..).and_then(|s| s.chars().next())
                else {
                    break;
                };
                cursor = hit.start() + next.len_utf8();
            } else {
                cursor = hit.end();
            }
        }
    }
    found
}

fn is_word_match(chars: &[char], start: usize, end: usize) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let left_ok = start == 0 || chars.get(start.wrapping_sub(1)).is_none_or(|c| !word(*c));
    let right_ok = chars.get(end).is_none_or(|c| !word(*c));
    left_ok && right_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: &str, text: &str) -> BlockView {
        BlockView {
            id: id.to_string(),
            kind: "paragraph".to_string(),
            text: text.to_string(),
            marks: Vec::new(),
            attrs: std::collections::BTreeMap::new(),
            children: Vec::new(),
        }
    }

    /// Case and whole-word options narrow literal matches, and a query runs
    /// across every paragraph in reading order.
    #[test]
    fn matching_honours_the_options_across_paragraphs() {
        let blocks = [
            block("p1", "Hello hello HELLO category"),
            block("p2", "a cat sat"),
        ];
        let any = FindOptions::default();
        assert_eq!(find_in(&blocks, "hello", any).len(), 3);
        let case = FindOptions {
            match_case: true,
            ..FindOptions::default()
        };
        assert_eq!(find_in(&blocks, "hello", case), vec![("p1".into(), 6, 11)]);
        let word = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        assert_eq!(find_in(&blocks, "cat", word), vec![("p2".into(), 2, 5)]);
        assert_eq!(find_in(&blocks, "cat", any).len(), 2);
        let pattern = FindOptions {
            mode: ui::widget::FindMode::Regex,
            ..FindOptions::default()
        };
        assert_eq!(
            find_in(&blocks, r"c\w+", pattern),
            vec![("p1".into(), 18, 26), ("p2".into(), 2, 5)],
            "a pattern matches in characters"
        );
        assert!(
            find_in(&blocks, "(", pattern).is_empty(),
            "a pattern that does not compile finds nothing"
        );
    }

    #[test]
    fn literal_whole_word_reconsiders_overlapping_candidates_after_boundary_rejection() {
        let options = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        for (text, query, expected) in [
            ("xfoo-foo-foo", "foo-foo", vec![(5, 12)]),
            ("İfoo-foo-foo", "foo-foo", vec![(5, 12)]),
            ("xéé-éé-éé", "éé-éé", vec![(4, 9)]),
            ("🙂 xfoo-foo-foo", "foo-foo", vec![(7, 14)]),
            ("foo-foo-foo", "foo-foo", vec![(0, 7)]),
            ("xfoo-foo", "foo-foo", vec![]),
        ] {
            let blocks = [block("p1", text)];
            assert_eq!(
                find_in(&blocks, query, options)
                    .iter()
                    .map(|(_, s, e)| (*s, *e))
                    .collect::<Vec<_>>(),
                expected,
                "{text}"
            );
        }
    }

    /// A search finds once it has the paragraphs, keeps the current match,
    /// and closing it forgets every match.
    #[test]
    fn a_search_finds_reveals_and_closing_forgets() {
        let mut find = DocumentFind::default();
        assert_eq!(find.find("report", FindOptions::default()), 0);
        assert!(
            find.wants_blocks(false),
            "an open search reads the paragraphs"
        );
        let revision = find.revision();
        find.set_blocks(vec![block("p1", "report and report")]);
        assert_ne!(find.revision(), revision, "the bar finds again");
        assert_eq!(find.matches().len(), 2);
        assert!(find.take_first_reveal(), "the first match shows once read");
        assert_eq!(find.active_match(), Some(&("p1".to_string(), 0, 6)));
        assert!(!find.take_first_reveal(), "and only once");
        assert!(find.reveal(1));
        assert_eq!(find.active_match(), Some(&("p1".to_string(), 11, 17)));
        find.clear();
        assert!(!find.is_active());
        assert!(find.matches().is_empty());
        assert!(!find.wants_blocks(true), "a closed search reads nothing");
    }

    /// A replacement shows in the paragraphs at once, so the count follows,
    /// and waits for the editor in the order it was asked for.
    #[test]
    fn replacements_wait_in_order_and_show_at_once() {
        let mut find = DocumentFind::default();
        find.set_blocks(vec![block("p1", "cat and cat")]);
        find.find("cat", FindOptions::default());
        let first = find.matches()[0].clone();
        assert_eq!(find.queue_replace(vec![first.clone()], "dog"), 1);
        assert_eq!(find.matches(), [("p1".to_string(), 8, 11)]);
        let rest = find.matches().to_vec();
        assert_eq!(find.queue_replace(rest.clone(), "bird"), 1);
        assert!(find.matches().is_empty());
        assert_eq!(
            find.take_pending(),
            vec![
                PendingReplace {
                    matches: vec![first],
                    with: "dog".into()
                },
                PendingReplace {
                    matches: rest,
                    with: "bird".into()
                },
            ]
        );
        assert!(find.take_pending().is_empty());
    }
}
