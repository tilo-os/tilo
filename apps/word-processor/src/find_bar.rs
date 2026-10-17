//! Find & Replace for the word processor: the toolkit's [`ui::widget::FindBar`]
//! over the document's paragraphs.
//!
//! The bar is the framework's (field, counter, Match case / Whole word,
//! previous / next, the replace row); this module keeps the matches in the
//! document's own terms — `(block, start, end)` in characters — and hands
//! replacements back to the editor, which remains the sole owner of
//! undoable document mutations.

use tilo_doc::snapshot::BlockView;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{EventCtx, FindOptions, Findable, Widget};

pub(crate) type FindMatch = (String, usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindEffect {
    None,
    SelectActive,
}

/// What the bar needs from the editor: the paragraphs to search, and a way
/// to replace matches through the editor's own actions.
pub(crate) trait FindHost {
    /// Every paragraph a find runs over, in reading order.
    fn search_blocks(&mut self) -> Vec<BlockView>;
    /// Replace each of `matches` with `with`; returns how many changed.
    fn replace_matches(&mut self, matches: &[FindMatch], with: &str) -> usize;
}

/// A read-only host: paragraphs to search, nothing to replace.
pub(crate) struct Blocks<'a>(pub(crate) &'a [BlockView]);

impl FindHost for Blocks<'_> {
    fn search_blocks(&mut self) -> Vec<BlockView> {
        self.0.to_vec()
    }

    fn replace_matches(&mut self, _matches: &[FindMatch], _with: &str) -> usize {
        0
    }
}

pub(crate) struct FindBar {
    open: bool,
    bar: ui::widget::FindBar,
    matches: Vec<FindMatch>,
    index: usize,
}

impl Default for FindBar {
    fn default() -> Self {
        Self {
            open: false,
            bar: ui::widget::FindBar::new().with_replace().with_regex(),
            matches: Vec::new(),
            index: 0,
        }
    }
}

/// The document as the toolkit's [`Findable`]: a match is reported by its
/// index into the editor's list, and replacements go to the host.
struct Finder<'a> {
    host: &'a mut dyn FindHost,
    matches: &'a mut Vec<FindMatch>,
    revealed: Option<usize>,
}

impl Findable for Finder<'_> {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<ui::widget::FindMatch> {
        let blocks = self.host.search_blocks();
        *self.matches = find_in(&blocks, query, options);
        (0..self.matches.len())
            .map(|index| ui::widget::FindMatch::new(index, index.saturating_add(1)))
            .collect()
    }

    fn reveal(&mut self, index: usize) {
        self.revealed = Some(index);
    }

    fn clear_find(&mut self) {
        self.matches.clear();
    }

    fn supports_replace(&self) -> bool {
        true
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(found) = self.matches.get(index).cloned() else {
            return false;
        };
        self.host.replace_matches(&[found], with) > 0
    }

    fn replace_all(&mut self, with: &str) -> usize {
        let all = self.matches.clone();
        self.host.replace_matches(&all, with)
    }
}

impl FindBar {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The band the bar reserves at the top of the content.
    pub(crate) fn height(&self, theme: &Theme) -> f64 {
        if self.open {
            self.bar.height(theme)
        } else {
            0.0
        }
    }

    pub(crate) fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    pub(crate) fn active_match(&self) -> Option<&FindMatch> {
        self.matches.get(self.index)
    }

    /// The Find query as typed; with [`Self::matches`] it is the window's
    /// Find state in its participant view.
    pub(crate) fn query(&self) -> &str {
        self.bar.query()
    }

    pub(crate) fn open(&mut self, seed: Option<&str>, host: &mut dyn FindHost) -> FindEffect {
        self.open = true;
        if let Some(seed) = seed.filter(|s| !s.is_empty() && !s.contains('\n')) {
            self.bar.set_query(seed);
        }
        self.bar.focus_query();
        Widget::set_focus(&mut self.bar, true);
        self.drive(host)
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        Widget::set_focus(&mut self.bar, false);
        self.bar = ui::widget::FindBar::new().with_replace();
    }

    /// Re-run the current query after the document changed.
    pub(crate) fn refresh(&mut self, host: &mut dyn FindHost) -> FindEffect {
        if !self.open {
            return FindEffect::None;
        }
        self.bar.refresh();
        self.drive(host)
    }

    /// Carry what the person did in the bar to the document: search again,
    /// step, replace, or close. Returns whether the current match moved.
    pub(crate) fn drive(&mut self, host: &mut dyn FindHost) -> FindEffect {
        let mut finder = Finder {
            host,
            matches: &mut self.matches,
            revealed: None,
        };
        self.bar.drive(&mut finder);
        let revealed = finder.revealed;
        if self.bar.close_requested() {
            self.close();
            return FindEffect::None;
        }
        self.index = self.index.min(self.matches.len().saturating_sub(1));
        match revealed {
            Some(index) if index < self.matches.len() => {
                self.index = index;
                FindEffect::SelectActive
            }
            _ => FindEffect::None,
        }
    }

    /// Route an event to the bar; returns whether it took it.
    pub(crate) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.open {
            return false;
        }
        self.bar.route(event, ctx)
    }

    pub(crate) fn set_host_focused(&mut self, focused: bool) {
        Widget::set_focus(&mut self.bar, focused && self.open);
    }

    pub(crate) fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.bar)
    }

    pub(crate) fn ime_content(&self) -> Option<ui::ime::InputContent> {
        Widget::ime_content(&self.bar)
    }

    pub(crate) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.open
            .then(|| Widget::accessibility(&self.bar))
            .flatten()
    }

    /// Lay out and paint the bar in its band.
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        band: Rect,
    ) {
        if !self.open {
            return;
        }
        self.bar.layout(band, theme, text);
        self.bar.paint(scene, transform, text, theme, presence);
    }

    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.open {
            Widget::draw_overlay(&mut self.bar, scene, transform, text, theme, presence);
        }
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
            let mut host = Blocks(&blocks);
            let mut matches = Vec::new();
            let mut finder = Finder {
                host: &mut host,
                matches: &mut matches,
                revealed: None,
            };
            let hits = finder.find(query, options);
            assert_eq!(hits.len(), expected.len(), "{text}");
            assert_eq!(
                matches.iter().map(|(_, s, e)| (*s, *e)).collect::<Vec<_>>(),
                expected,
                "{text}"
            );
        }
    }

    /// Opening with a seed finds and selects the first match; closing drops
    /// every match.
    #[test]
    fn opening_finds_and_closing_forgets() {
        let blocks = [block("p1", "report and report")];
        let mut bar = FindBar::default();
        assert_eq!(
            bar.open(Some("report"), &mut Blocks(&blocks)),
            FindEffect::SelectActive
        );
        assert_eq!(bar.matches().len(), 2);
        assert_eq!(bar.active_match(), Some(&("p1".to_string(), 0, 6)));
        bar.close();
        assert!(!bar.is_open());
        assert!(bar.matches().is_empty());
    }
}
