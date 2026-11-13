//! Inline completion in a text field: `:shortcode` offers the catalogue's emoji and `@name` offers
//! the workspace's people and agents, in the one list beside the caret.
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::keyboard::Key;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
#[cfg(test)]
use crate::widget::autocomplete::Suggestion;
use crate::widget::autocomplete::SuggestionList;
use crate::widget::completion::{
    self, Candidate, CompletionSource, Entry, MAX_ROWS, Pick, PreparedSource, Tier,
};
use crate::widget::completion_card;
use crate::widget::mention_picker::{self, MentionCandidate};
const MAX_SHORTCODE_LEN: usize = 32;

/// Which parts of a text field's inline completion it offers.
///
/// [`TextCompletion::ALL`] is the default for prose. [`TextCompletion::PLAIN`]
/// is a field that is UTF-8 only: no `:emoji:` completion, no `@mention`
/// completion and no list, because what is typed there is syntax (a search
/// query, a URL, code, a secret) rather than prose. Say at the call site why
/// a field is plain; a field switched off for no stated reason is
/// indistinguishable from one where it was forgotten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextCompletion {
    emoji: bool,
    mentions: bool,
}

impl TextCompletion {
    pub const ALL: Self = Self {
        emoji: true,
        mentions: true,
    };
    pub const PLAIN: Self = Self {
        emoji: false,
        mentions: false,
    };
    /// A field where `:` is a shortcode but `@` is not a mention (a handle
    /// that names no one, a system that writes `@` itself).
    pub const EMOJI: Self = Self {
        emoji: true,
        mentions: false,
    };
    pub const MENTIONS: Self = Self {
        emoji: false,
        mentions: true,
    };

    #[must_use]
    pub const fn emoji(self) -> bool {
        self.emoji
    }

    #[must_use]
    pub const fn mentions(self) -> bool {
        self.mentions
    }

    #[must_use]
    pub const fn is_plain(self) -> bool {
        !self.emoji && !self.mentions
    }

    #[must_use]
    pub const fn with_emoji(mut self, on: bool) -> Self {
        self.emoji = on;
        self
    }

    #[must_use]
    pub const fn with_mentions(mut self, on: bool) -> Self {
        self.mentions = on;
        self
    }
}

impl Default for TextCompletion {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiQuery {
    /// Byte offset of the opening `:`.
    pub start: usize,
    /// Where the token ends: one past its closing `:` where it already has
    /// one, otherwise the caret. What accepting replaces.
    pub end: usize,
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletionToken {
    pub(crate) trigger: char,
    /// Byte offset of the trigger.
    pub(crate) start: usize,
    /// One past the last byte a pick replaces.
    pub(crate) end: usize,
    pub(crate) query: String,
}

/// How long a token may grow before it stops being one: names have spaces,
/// so a space cannot end one; this stops a stray trigger holding the drawer
/// open over a whole sentence.
const MAX_QUERY_LEN: usize = 40;

fn opens_token(previous: Option<char>) -> bool {
    previous.is_none_or(|character| {
        character.is_whitespace()
            || (character.is_ascii_punctuation()
                && !matches!(character, ':' | '@' | '_' | '.' | '%' | '+' | '-'))
    })
}

fn is_shortcode_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '+')
}

/// The `:shortcode` ending at `caret`, if the caret sits in one: a `:` that
/// opens a token, followed by at most `MAX_SHORTCODE_LEN` shortcode
/// characters. `None` inside a word (`10:30`, `a:smile`) and for a token
/// that already has its closing `:` after the typed part.
#[must_use]
pub fn emoji_query_at_caret(text: &str, caret: usize) -> Option<EmojiQuery> {
    let caret = text.floor_char_boundary(caret.min(text.len()));
    let head = text.get(..caret)?;
    let colon = head.rfind(':')?;
    if !opens_token(head.get(..colon)?.chars().next_back()) {
        return None;
    }
    let query = head.get(colon + ':'.len_utf8()..)?;
    if query.is_empty()
        || query.chars().count() > MAX_SHORTCODE_LEN
        || !query.chars().all(is_shortcode_char)
    {
        return None;
    }
    let mut end = caret;
    for (offset, character) in text.get(caret..)?.char_indices() {
        if character == ':' {
            end = caret.saturating_add(offset).saturating_add(1);
            break;
        }
        if !is_shortcode_char(character) {
            return Some(EmojiQuery {
                start: colon,
                end: caret,
                query: query.to_owned(),
            });
        }
    }
    Some(EmojiQuery {
        start: colon,
        end,
        query: query.to_owned(),
    })
}

fn emoji_source(query: &str) -> CompletionSource {
    let candidates = crate::emoji_catalogue::shortcode_matches(query, MAX_ROWS)
        .into_iter()
        .map(|shortcode| {
            let name = if shortcode.emoji.starts_with(':') {
                shortcode.name.clone()
            } else {
                format!("{} {}", shortcode.emoji, shortcode.name)
            };
            Candidate::text(name, shortcode.emoji).with_handle(format!(":{}:", shortcode.name))
        })
        .collect();
    CompletionSource::new(':', vec![Tier::new(candidates)])
}

fn roster_source(roster: &[MentionCandidate]) -> CompletionSource {
    CompletionSource::new(
        '@',
        vec![Tier::new(
            roster.iter().map(mention_picker::candidate).collect(),
        )],
    )
}

fn token_for(text: &str, caret: usize, trigger: char) -> Option<CompletionToken> {
    let caret = text.floor_char_boundary(caret.min(text.len()));
    if trigger == ':' {
        let query = emoji_query_at_caret(text, caret)?;
        return Some(CompletionToken {
            trigger,
            start: query.start,
            end: query.end,
            query: query.query,
        });
    }
    let head = text.get(..caret)?;
    let at = head.rfind(trigger)?;
    if !opens_token(head.get(..at)?.chars().next_back()) {
        return None;
    }
    if trigger == '/' && !head.get(..at)?.trim().is_empty() {
        return None;
    }
    let query = head.get(at.saturating_add(trigger.len_utf8())..)?;
    if query.len() > MAX_QUERY_LEN || query.contains('\n') || query.contains(trigger) {
        return None;
    }
    if query.starts_with(char::is_whitespace) {
        return None;
    }
    Some(CompletionToken {
        trigger,
        start: at,
        end: caret,
        query: query.to_owned(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompletionKey {
    Pass,
    Handled,
    Accept(Pick),
}

#[derive(Default)]
pub(crate) struct CompletionList {
    list: SuggestionList,
    entries: Vec<Entry>,
    token: Option<CompletionToken>,
    dismissed: Option<(usize, char)>,
    sources: Vec<PreparedSource>,
    /// The person moved the selection off the first row: a row they chose keeps its place
    /// while they type on; untouched, the selection stays on the best match.
    chosen: bool,
}

impl CompletionList {
    pub(crate) fn is_open(&self) -> bool {
        self.token.is_some() && !self.entries.is_empty()
    }

    pub(crate) fn token(&self) -> Option<&CompletionToken> {
        self.token.as_ref().filter(|_| self.is_open())
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Suggestion] {
        self.list.rows()
    }

    pub(crate) fn selected(&self) -> Option<&Entry> {
        self.list.highlight().and_then(|at| self.entries.get(at))
    }

    pub(crate) fn set_sources(&mut self, sources: Vec<CompletionSource>) -> bool {
        let same = sources.len() == self.sources.len()
            && sources
                .iter()
                .zip(&self.sources)
                .all(|(next, held)| *next == held.source);
        if same {
            return false;
        }
        self.sources = sources.into_iter().map(PreparedSource::new).collect();
        true
    }

    pub(crate) fn close(&mut self) {
        self.token = None;
        self.chosen = false;
        self.entries.clear();
        self.list.clear();
    }

    pub(crate) fn dismiss(&mut self) {
        self.dismissed = self
            .token
            .as_ref()
            .map(|token| (token.start, token.trigger));
        self.close();
    }

    fn triggers(&self, setting: TextCompletion) -> Vec<char> {
        let mut triggers: Vec<char> = self.sources.iter().map(PreparedSource::trigger).collect();
        if setting.emoji() && !triggers.contains(&':') {
            triggers.push(':');
        }
        if setting.mentions() && !triggers.contains(&'@') {
            triggers.push('@');
        }
        triggers
    }

    pub(crate) fn follow(
        &mut self,
        text: &str,
        caret: usize,
        roster: &[MentionCandidate],
        setting: TextCompletion,
    ) -> bool {
        let was = (self.token.clone(), self.entries.clone());
        let token = self
            .triggers(setting)
            .into_iter()
            .filter_map(|trigger| token_for(text, caret, trigger))
            .max_by_key(|token| token.start)
            .filter(|token| self.dismissed != Some((token.start, token.trigger)));
        let Some(token) = token else {
            self.close();
            return was != (self.token.clone(), self.entries.clone());
        };
        self.dismissed = None;
        let fallback;
        let source =
            if let Some(source) = self.sources.iter().find(|s| s.trigger() == token.trigger) {
                source
            } else {
                fallback = PreparedSource::new(if token.trigger == ':' {
                    emoji_source(&token.query)
                } else {
                    roster_source(roster)
                });
                &fallback
            };
        let rows = completion::rows(source, &token.query);
        let matched = rows.iter().any(|entry| matches!(entry, Entry::Item { .. }));
        if !matched && token.query.ends_with(char::is_whitespace) {
            self.token = Some(token);
            self.dismiss();
            return was != (self.token.clone(), self.entries.clone());
        }
        let same_token = self
            .token
            .as_ref()
            .is_some_and(|held| held.start == token.start && held.trigger == token.trigger);
        if !same_token {
            self.chosen = false;
        }
        let kept = self
            .list
            .highlight()
            .filter(|_| self.chosen)
            .and_then(|at| {
                let key = self.entries.get(at)?.key()?.to_owned();
                Some((key, at))
            });
        let (rows, selected) =
            completion::keep_place(rows, kept.as_ref().map(|(key, at)| (key.as_str(), *at)));
        self.list
            .set_rows(rows.iter().map(Entry::suggestion).collect());
        self.list.set_highlight(selected);
        self.entries = rows;
        self.token = Some(token);
        was != (self.token.clone(), self.entries.clone())
    }

    pub(crate) fn handle_key(&mut self, key: &Key) -> CompletionKey {
        use winit::keyboard::NamedKey;
        if !self.is_open() {
            return CompletionKey::Pass;
        }
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                self.chosen = true;
                self.list.move_highlight(true);
                CompletionKey::Handled
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.chosen = true;
                self.list.move_highlight(false);
                CompletionKey::Handled
            }
            Key::Named(NamedKey::PageDown) => {
                self.chosen = true;
                self.list.page(true);
                CompletionKey::Handled
            }
            Key::Named(NamedKey::PageUp) => {
                self.chosen = true;
                self.list.page(false);
                CompletionKey::Handled
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => match self.pick_selected() {
                Some(pick) => CompletionKey::Accept(pick),
                None => CompletionKey::Pass,
            },
            Key::Named(NamedKey::Escape) => {
                self.dismiss();
                CompletionKey::Handled
            }
            _ => CompletionKey::Pass,
        }
    }

    fn pick_selected(&self) -> Option<Pick> {
        self.list
            .highlight()
            .and_then(|at| self.entries.get(at))
            .and_then(|entry| self.pick(entry))
    }

    fn pick(&self, entry: &Entry) -> Option<Pick> {
        match entry {
            Entry::Item { candidate, .. } if candidate.command.is_some() => {
                Some(Pick::Command(candidate.clone()))
            }
            Entry::Item { candidate, .. } => Some(Pick::Insert(candidate.clone())),
            Entry::Create { query, .. } => Some(Pick::Create {
                trigger: self.token.as_ref()?.trigger,
                query: query.clone(),
            }),
            Entry::Header { .. } | Entry::NoMatch(_) => None,
        }
    }

    pub(crate) fn click_at(&self, point: Point) -> Option<Pick> {
        if !self.is_open() {
            return None;
        }
        self.list
            .row_at(point)
            .and_then(|index| self.entries.get(index))
            .and_then(|entry| self.pick(entry))
    }

    pub(crate) fn hover_at(&mut self, point: Point) -> bool {
        self.is_open() && self.list.hover_at(point)
    }

    pub(crate) fn contains(&self, point: Point) -> bool {
        self.is_open() && self.list.rect().is_some_and(|rect| rect.contains(point))
    }

    pub(crate) fn rect(&self) -> Option<Rect> {
        self.is_open().then(|| self.list.rect()).flatten()
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.is_open() && self.list.is_animating(crate::motion::frame_now())
    }

    /// Paint the drawer at the caret: above it when the field sits in the
    /// lower half of the window (a composer), below otherwise; on a compact
    /// window docked across it above the field. On a wide window the selected
    /// row's card stands beside the drawer.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        caret: Rect,
        field: Rect,
        viewport: Option<Rect>,
    ) {
        if !self.is_open() {
            return;
        }
        let breakpoint = viewport
            .map(|viewport| crate::tokens::Breakpoint::of(viewport.width(), &theme.tokens.metrics));
        let docked = breakpoint.is_some_and(crate::tokens::Breakpoint::is_compact);
        self.list.set_docked(docked);
        self.list.set_anchor(if docked { field } else { caret });
        let lower = viewport.is_some_and(|viewport| caret.center().y > viewport.center().y);
        self.list.set_side(if lower {
            crate::widget::PopoverSide::Above
        } else {
            crate::widget::PopoverSide::Below
        });
        self.list
            .set_min_width(f64::from(theme.tokens.metrics.panel_width_list));
        if let Some(viewport) = viewport {
            self.list.set_viewport(viewport);
        }
        self.list.paint(scene, transform, text, theme);
        if breakpoint.is_some_and(crate::tokens::Breakpoint::is_wide)
            && let (Some(panel), Some(viewport), Some(Entry::Item { candidate, .. })) =
                (self.list.rect(), viewport, self.selected())
            && (candidate.handle.is_some()
                || candidate.aside.is_some()
                || candidate.label.is_some()
                || candidate.command.is_some())
        {
            completion_card::paint(scene, transform, text, theme, candidate, panel, viewport);
        }
    }

    pub(crate) fn access(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Politeness, Role};
        let panel = self.rect()?;
        let count = self
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Item { .. }))
            .count();
        let mut args = fluent::FluentArgs::new();
        args.set("count", count);
        let name = crate::i18n::I18nManager::lookup("completion-results", Some(&args));
        let mut node = AccessNode::new(Role::ListBox)
            .with_name(&name)
            .with_bounds(panel)
            .with_live(Politeness::Polite);
        if let Some(Entry::Item { candidate, .. }) = self.selected() {
            node = node.with_description(&candidate.name);
        }
        let rows = self
            .list
            .access_rows(|row| {
                use std::hash::{Hash as _, Hasher as _};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (&row.value, &row.label).hash(&mut hasher);
                hasher.finish()
            })
            .into_iter()
            .map(|mut row| {
                if row.role == Role::Button {
                    row.role = Role::ListBoxOption;
                }
                row
            })
            .collect();
        Some(node.with_children(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emoji_token_is_detected_at_the_start_in_the_middle_and_after_punctuation() {
        assert_eq!(
            emoji_query_at_caret(":smi", 4),
            Some(EmojiQuery {
                start: 0,
                end: 4,
                query: "smi".into()
            })
        );
        assert_eq!(
            emoji_query_at_caret("hi :smi there", 7),
            Some(EmojiQuery {
                start: 3,
                end: 7,
                query: "smi".into()
            })
        );
        assert_eq!(
            emoji_query_at_caret("(:smi", 5),
            Some(EmojiQuery {
                start: 1,
                end: 5,
                query: "smi".into()
            })
        );
        assert_eq!(
            emoji_query_at_caret(":thumbs_up:", 10),
            Some(EmojiQuery {
                start: 0,
                end: 11,
                query: "thumbs_up".into()
            }),
            "a caret before the closing colon replaces the whole token"
        );
    }

    #[test]
    fn a_colon_inside_a_word_url_or_time_is_not_an_emoji_token() {
        assert_eq!(emoji_query_at_caret("a:smi", 5), None);
        assert_eq!(emoji_query_at_caret("10:30", 5), None);
        assert_eq!(emoji_query_at_caret("https://example", 15), None);
        assert_eq!(emoji_query_at_caret(":smile:", 7), None, "already closed");
        assert_eq!(emoji_query_at_caret(":smi le", 7), None, "a space ends it");
        assert_eq!(emoji_query_at_caret(":", 1), None);
    }

    fn ada() -> Vec<MentionCandidate> {
        vec![MentionCandidate::new(
            "u_ada",
            "Ada Lovelace",
            false,
            crate::tokens::PresenceHue::of_identity("u_ada"),
        )]
    }

    #[test]
    fn the_one_list_offers_the_catalogues_emoji_and_the_rosters_people() {
        let mut list = CompletionList::default();
        let roster = ada();
        assert!(list.follow(":piz", 4, &roster, TextCompletion::ALL));
        assert!(list.is_open());
        assert!(
            list.rows().iter().any(|row| row.value == "🍕"),
            "the catalogue names pizza"
        );
        assert!(list.follow("hi @ad", 6, &roster, TextCompletion::ALL));
        assert!(list.is_open());
        assert_eq!(
            list.rows().first().map(|row| row.value.as_str()),
            Some("@Ada Lovelace"),
            "accepting puts the name the mention resolves to"
        );
    }

    #[test]
    fn escape_dismisses_this_token_and_a_new_one_opens_again() {
        let mut list = CompletionList::default();
        let roster = ada();
        assert!(list.follow("hi @ad", 6, &roster, TextCompletion::ALL));
        assert_eq!(
            list.handle_key(&Key::Named(winit::keyboard::NamedKey::Escape)),
            CompletionKey::Handled
        );
        assert!(!list.is_open());
        list.follow("hi @ada", 7, &roster, TextCompletion::ALL);
        assert!(!list.is_open(), "the dismissed @ stays closed");
        list.follow("hi @ad @a", 9, &roster, TextCompletion::ALL);
        assert!(list.is_open(), "a new @ opens it again");
    }

    #[test]
    fn accepting_the_armed_row_replaces_the_token() {
        let mut list = CompletionList::default();
        let roster = ada();
        list.follow("hi @ad", 6, &roster, TextCompletion::ALL);
        list.handle_key(&Key::Named(winit::keyboard::NamedKey::ArrowDown));
        assert!(matches!(
            list.handle_key(&Key::Named(winit::keyboard::NamedKey::Enter)),
            CompletionKey::Accept(_)
        ));
    }

    #[test]
    fn a_mention_token_grows_across_a_name_and_stops_at_an_email() {
        assert_eq!(
            mention_picker::query_at_caret("@ad", 3),
            Some(mention_picker::MentionQuery {
                start: 0,
                query: "ad".into()
            })
        );
        assert_eq!(
            mention_picker::query_at_caret("hi, @ada lovelace", 17),
            Some(mention_picker::MentionQuery {
                start: 4,
                query: "ada lovelace".into()
            })
        );
        assert_eq!(
            mention_picker::query_at_caret("(@ad", 4),
            Some(mention_picker::MentionQuery {
                start: 1,
                query: "ad".into()
            }),
            "after punctuation opens a mention"
        );
        assert_eq!(mention_picker::query_at_caret("a@b", 3), None, "an email");
        assert_eq!(mention_picker::query_at_caret("me@example", 10), None);
    }
}
