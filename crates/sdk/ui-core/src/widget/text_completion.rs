//! Inline completion in a text field: `:shortcode` offers the catalogue's
//! emoji and `@name` offers the room's people and agents, in the one list
//! beside the caret.
//!
//! A field offers both by default; what it offers is a property of the field
//! ([`TextCompletion`]), not of the app. A search field, a secret, a code
//! editor or a URL is plain because `:` and `@` are syntax there, not
//! because completion is unavailable.
//!
//! Both triggers share one keyboard-driven list: Up and Down move the armed
//! row, Enter or Tab accepts it, Escape dismisses this token. Accepting
//! replaces the token with the emoji character (a workspace emoji stays
//! written `:name:`) or with the mention's name as it will resolve. A field
//! with an input method composition open offers nothing, so completion never
//! competes with the input method for the same keys.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::keyboard::Key;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::autocomplete::{Suggestion, SuggestionList};
use crate::widget::mention_picker::{self, MentionCandidate};

/// The most rows offered for one token.
const MAX_ROWS: usize = 8;
/// How much of a shortcode may be typed before it is no longer one.
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
    /// Emoji and mentions: the default for prose.
    pub const ALL: Self = Self {
        emoji: true,
        mentions: true,
    };
    /// UTF-8 only: neither, and no completion list.
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
    /// A field where `@` names someone but `:` is punctuation (a code
    /// comment, a log line).
    pub const MENTIONS: Self = Self {
        emoji: false,
        mentions: true,
    };

    /// Whether `:shortcode` completion is offered.
    #[must_use]
    pub const fn emoji(self) -> bool {
        self.emoji
    }

    /// Whether `@mention` completion is offered.
    #[must_use]
    pub const fn mentions(self) -> bool {
        self.mentions
    }

    /// Whether the field offers neither, and therefore no list at all.
    #[must_use]
    pub const fn is_plain(self) -> bool {
        !self.emoji && !self.mentions
    }

    /// This setting with emoji completion on or off.
    #[must_use]
    pub const fn with_emoji(mut self, on: bool) -> Self {
        self.emoji = on;
        self
    }

    /// This setting with mention completion on or off.
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

/// The `:shortcode` the caret sits in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiQuery {
    /// Byte offset of the opening `:`.
    pub start: usize,
    /// Where the token ends: one past its closing `:` where it already has
    /// one, otherwise the caret. What accepting replaces.
    pub end: usize,
    /// What was typed between the `:` and the caret.
    pub query: String,
}

/// Which trigger a token answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionKind {
    Emoji,
    Mention,
}

/// The token the caret sits in, and what accepting it replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletionToken {
    pub(crate) kind: CompletionKind,
    /// Byte offset of the `:` or `@`.
    pub(crate) start: usize,
    /// One past the last byte a pick replaces.
    pub(crate) end: usize,
    /// What was typed after the trigger, up to the caret.
    pub(crate) query: String,
}

/// Whether `previous` lets a `:` or `@` open a token: the start of the text,
/// whitespace, or punctuation that does not make the character part of a
/// word (an email's local part, a shortcode's own colons, a URL).
fn opens_token(previous: Option<char>) -> bool {
    previous.is_none_or(|character| {
        character.is_whitespace()
            || (character.is_ascii_punctuation()
                && !matches!(character, ':' | '@' | '_' | '.' | '%' | '+' | '-'))
    })
}

/// Whether `character` may be typed in a `:shortcode`.
fn is_shortcode_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '+')
}

/// The `:shortcode` ending at `caret`, if the caret sits in one: a `:` that
/// opens a token, followed by at most [`MAX_SHORTCODE_LEN`] shortcode
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
    // A token that already has its closing `:` is complete; accepting it
    // would leave stray text behind. Mid-token (the caret before a closing
    // `:`) still offers, and replaces the whole token.
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

/// The rows the catalogue offers for what was typed after the `:`.
fn emoji_rows(query: &str) -> Vec<Suggestion> {
    crate::emoji_catalogue::shortcode_matches(query, MAX_ROWS)
        .into_iter()
        .map(|shortcode| {
            let label = if shortcode.emoji.starts_with(':') {
                shortcode.name.clone()
            } else {
                shortcode.emoji.clone()
            };
            Suggestion::labelled(shortcode.emoji, label)
                .with_detail(format!(":{}:", shortcode.name))
        })
        .collect()
}

/// The rows the roster offers for what was typed after the `@`.
fn mention_rows(roster: &[MentionCandidate], query: &str) -> Vec<Suggestion> {
    mention_picker::rank(roster, query)
        .into_iter()
        .take(MAX_ROWS)
        .map(mention_picker::mention_suggestion)
        .collect()
}

/// What a key did to the open list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompletionKey {
    /// Not the list's: the field handles it.
    Pass,
    /// The list moved, dismissed itself or accepted nothing.
    Handled,
    /// Accept this row, replacing the token.
    Accept(Suggestion),
}

/// The inline candidate list of one field.
#[derive(Default)]
pub(crate) struct CompletionList {
    list: SuggestionList,
    token: Option<CompletionToken>,
    /// The token the person dismissed with Escape: it stays dismissed while
    /// they type on after it.
    dismissed: Option<(usize, CompletionKind)>,
}

impl CompletionList {
    pub(crate) fn is_open(&self) -> bool {
        self.token.is_some() && !self.list.suggestions().is_empty()
    }

    /// The token the open list answers.
    pub(crate) fn token(&self) -> Option<&CompletionToken> {
        self.token.as_ref().filter(|_| self.is_open())
    }

    /// The rows in view, for tests.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Suggestion] {
        self.list.suggestions()
    }

    pub(crate) fn close(&mut self) {
        self.token = None;
        self.list.clear();
    }

    /// Close the list and keep this token closed while the person types on
    /// after it: what accepting a row leaves behind is no longer a token.
    pub(crate) fn dismiss(&mut self) {
        self.dismissed = self.token.as_ref().map(|token| (token.start, token.kind));
        self.close();
    }

    /// Follow the caret: offer what `text` holds at `caret` under `setting`,
    /// from `roster` for mentions. Returns whether anything shown changed.
    pub(crate) fn follow(
        &mut self,
        text: &str,
        caret: usize,
        roster: &[MentionCandidate],
        setting: TextCompletion,
    ) -> bool {
        let was = (self.token.clone(), self.list.suggestions().to_vec());
        let token = self
            .token_at(text, caret, setting)
            .filter(|token| self.dismissed != Some((token.start, token.kind)));
        let Some(token) = token else {
            self.close();
            return was != (self.token.clone(), self.list.suggestions().to_vec());
        };
        self.dismissed = None;
        let rows = match token.kind {
            CompletionKind::Emoji => emoji_rows(&token.query),
            CompletionKind::Mention => mention_rows(roster, &token.query),
        };
        if rows.is_empty() {
            self.close();
            return was != (self.token.clone(), self.list.suggestions().to_vec());
        }
        let keep = self
            .list
            .highlighted()
            .map(|row| row.value.clone())
            .and_then(|value| rows.iter().position(|row| row.value == value));
        self.list.set_suggestions(rows);
        self.list.set_highlight(Some(keep.unwrap_or(0)));
        self.token = Some(token);
        was != (self.token.clone(), self.list.suggestions().to_vec())
    }

    fn token_at(
        &self,
        text: &str,
        caret: usize,
        setting: TextCompletion,
    ) -> Option<CompletionToken> {
        if setting.mentions()
            && let Some(query) = mention_picker::query_at_caret(text, caret)
        {
            return Some(CompletionToken {
                kind: CompletionKind::Mention,
                start: query.start,
                end: text.floor_char_boundary(caret.min(text.len())),
                query: query.query,
            });
        }
        if setting.emoji()
            && let Some(query) = emoji_query_at_caret(text, caret)
        {
            return Some(CompletionToken {
                kind: CompletionKind::Emoji,
                start: query.start,
                end: query.end,
                query: query.query,
            });
        }
        None
    }

    /// A key while the list is open. The field passes every key here first;
    /// [`CompletionKey::Pass`] means the list does not want it.
    pub(crate) fn handle_key(&mut self, key: &Key) -> CompletionKey {
        if !self.is_open() {
            return CompletionKey::Pass;
        }
        match key {
            Key::Named(winit::keyboard::NamedKey::ArrowDown) => {
                self.list.move_highlight(true);
                CompletionKey::Handled
            }
            Key::Named(winit::keyboard::NamedKey::ArrowUp) => {
                self.list.move_highlight(false);
                CompletionKey::Handled
            }
            Key::Named(winit::keyboard::NamedKey::Enter | winit::keyboard::NamedKey::Tab) => {
                match self.list.highlighted() {
                    Some(row) => CompletionKey::Accept(row.clone()),
                    None => CompletionKey::Handled,
                }
            }
            Key::Named(winit::keyboard::NamedKey::Escape) => {
                self.dismissed = self.token.as_ref().map(|token| (token.start, token.kind));
                self.close();
                CompletionKey::Handled
            }
            _ => CompletionKey::Pass,
        }
    }

    /// The row under `point`, for a click.
    pub(crate) fn click_at(&self, point: Point) -> Option<Suggestion> {
        if !self.is_open() {
            return None;
        }
        self.list
            .row_at(point)
            .and_then(|index| self.list.suggestions().get(index).cloned())
    }

    /// Whether `point` is over the open list.
    pub(crate) fn contains(&self, point: Point) -> bool {
        self.is_open() && self.list.rect().is_some_and(|rect| rect.contains(point))
    }

    /// Paint the list under the caret, inside `viewport` when the window's
    /// is known.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        caret: Rect,
        viewport: Option<Rect>,
    ) {
        if !self.is_open() {
            return;
        }
        self.list.set_anchor(caret);
        if let Some(viewport) = viewport {
            self.list.set_viewport(viewport);
        }
        self.list.paint(scene, transform, text, theme);
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
        vec![MentionCandidate::new("u_ada", "Ada Lovelace", false)]
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
