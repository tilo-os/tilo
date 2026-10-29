//! How a search hit reads in the results list.
//!
//! A provider says what a hit *is* — a message, a video, a match in a file —
//! and fills that row's slots; the platform draws it in the palette's row
//! slot on the list row geometry, in every palette, the same way for every
//! app. A hit without a [`SearchRow`] is drawn as the generic row: icon,
//! title and subtitle. Rows are data, never drawing code, so they reach the
//! shell's palette from any app unchanged.

use std::ops::Range;

use vello::peniko::Color;

use crate::icons::AppImage;

/// The shape of one search hit's row.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchRow {
    /// A message: who sent it, its subject, a snippet of its body, when it
    /// arrived, and whether it is unread (a dot before the sender).
    Message {
        sender: String,
        subject: String,
        snippet: Option<String>,
        date: Option<String>,
        unread: bool,
    },
    /// A video, track or other media item: its thumbnail, title, who made it
    /// (the channel or artist), and its duration.
    Media {
        thumbnail: Option<AppImage>,
        title: String,
        byline: Option<String>,
        duration: Option<String>,
    },
    /// A match inside a file: the file's path, the line, and the matched line
    /// itself with the matched span marked (byte range into `snippet`).
    FileHit {
        path: String,
        line: Option<u32>,
        snippet: String,
        matched: Option<Range<usize>>,
    },
    /// A command, place or person: the title over an optional description
    /// (the result's subtitle), with what leads and trails it.
    Command(Box<CommandRow>),
}

/// The slots of a [`SearchRow::Command`] row beyond its title, subtitle and
/// icon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandRow {
    /// The description is a path, so it loses its middle rather than its
    /// end when it doesn't fit. Prose loses its end.
    pub description_is_path: bool,
    /// The description (the result's subtitle) as separate facts, drawn as
    /// runs of their own with space between them. Empty: one run.
    pub facts: Vec<String>,
    /// The application's own artwork, painted in place of the icon when it
    /// decodes.
    pub image: Option<AppImage>,
    /// A person as `(name, key)`: their avatar in place of the icon.
    pub avatar: Option<(String, String)>,
    /// A short status, as a trailing pill.
    pub badge: Option<String>,
    /// A shortcut's keycap legends, as a trailing chip.
    pub keycaps: Option<Vec<String>>,
    /// The people the row is about, as a stack of faces at its end.
    pub faces: Vec<RowFace>,
    /// What choosing the row does, as a button at its end ("Join").
    pub outcome: Option<String>,
    /// Other activations behind a held modifier; the selected row shows the
    /// one whose modifier is down, else the first as a hint.
    pub alternates: Vec<Alternate>,
}

impl CommandRow {
    /// The alternative the selected row shows: the one whose modifier is
    /// `held`, else the first it offers.
    #[must_use]
    pub fn shown_alternate(&self, held: Option<ActivationModifier>) -> Option<&Alternate> {
        self.alternates
            .iter()
            .find(|alternate| Some(alternate.modifier) == held)
            .or_else(|| self.alternates.first())
    }
}

/// One person in a row's face stack ([`CommandRow::faces`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RowFace {
    pub name: String,
    /// Their stable id: picks their picture.
    pub identity: String,
    /// Their presence colour, as every other surface draws them.
    pub colour: Color,
    pub is_agent: bool,
    /// Idle or away: the face dims.
    pub dimmed: bool,
}

/// A modifier held while a row is activated, choosing an alternative
/// activation the row offers ([`CommandRow::alternates`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActivationModifier {
    Shift,
    Ctrl,
    Alt,
}

impl ActivationModifier {
    /// The one modifier `state` holds, or `None` for none or a mix: a chord
    /// of two is not one of the alternatives.
    pub fn of(state: winit::keyboard::ModifiersState) -> Option<Self> {
        match (
            state.shift_key(),
            state.control_key() || state.super_key(),
            state.alt_key(),
        ) {
            (true, false, false) => Some(Self::Shift),
            (false, true, false) => Some(Self::Ctrl),
            (false, false, true) => Some(Self::Alt),
            _ => None,
        }
    }

    /// The key's name, as the shared keycap legends spell it.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Shift => "Shift",
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
        }
    }
}

/// What a row does when it is activated with `modifier` held, in the words
/// the selected row shows while that modifier is down ("Opens as a tab
/// here").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alternate {
    pub modifier: ActivationModifier,
    pub label: String,
}

impl SearchRow {
    pub fn message(sender: impl Into<String>, subject: impl Into<String>) -> Self {
        Self::Message {
            sender: sender.into(),
            subject: subject.into(),
            snippet: None,
            date: None,
            unread: false,
        }
    }

    pub fn media(title: impl Into<String>) -> Self {
        Self::Media {
            thumbnail: None,
            title: title.into(),
            byline: None,
            duration: None,
        }
    }

    /// A file-hit row for `snippet`, the matched line of `path`.
    pub fn file_hit(path: impl Into<String>, snippet: impl Into<String>) -> Self {
        Self::FileHit {
            path: path.into(),
            line: None,
            snippet: snippet.into(),
            matched: None,
        }
    }

    /// The message's body snippet, or a file hit's matched line.
    #[must_use]
    pub fn snippet(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        match &mut self {
            Self::Message { snippet, .. } => *snippet = Some(text),
            Self::FileHit { snippet, .. } => *snippet = text,
            Self::Media { .. } | Self::Command(_) => {}
        }
        self
    }

    /// When a message arrived, already formatted ("Tue", "12 Mar").
    #[must_use]
    pub fn date(mut self, text: impl Into<String>) -> Self {
        if let Self::Message { date, .. } = &mut self {
            *date = Some(text.into());
        }
        self
    }

    #[must_use]
    pub fn unread(mut self, on: bool) -> Self {
        if let Self::Message { unread, .. } = &mut self {
            *unread = on;
        }
        self
    }

    #[must_use]
    pub fn thumbnail(mut self, image: AppImage) -> Self {
        if let Self::Media { thumbnail, .. } = &mut self {
            *thumbnail = Some(image);
        }
        self
    }

    /// The channel, artist or author of a media item.
    #[must_use]
    pub fn byline(mut self, text: impl Into<String>) -> Self {
        if let Self::Media { byline, .. } = &mut self {
            *byline = Some(text.into());
        }
        self
    }

    /// A media item's duration, already formatted ("12:04").
    #[must_use]
    pub fn duration(mut self, text: impl Into<String>) -> Self {
        if let Self::Media { duration, .. } = &mut self {
            *duration = Some(text.into());
        }
        self
    }

    /// The 1-based line of a file hit.
    #[must_use]
    pub fn line(mut self, number: u32) -> Self {
        if let Self::FileHit { line, .. } = &mut self {
            *line = Some(number);
        }
        self
    }

    /// The matched span of a file hit's snippet, as a byte range. A range
    /// that is not on character boundaries of the snippet is ignored.
    #[must_use]
    pub fn matched(mut self, range: Range<usize>) -> Self {
        if let Self::FileHit {
            snippet, matched, ..
        } = &mut self
        {
            *matched = (range.start < range.end
                && snippet.is_char_boundary(range.start)
                && snippet.is_char_boundary(range.end))
            .then_some(range);
        }
        self
    }

    /// The text a screen reader announces for the row, in reading order.
    #[must_use]
    pub fn spoken(&self) -> String {
        match self {
            Self::Message {
                sender,
                subject,
                snippet,
                date,
                unread,
            } => {
                let mut parts = Vec::new();
                if *unread {
                    parts.push("Unread".to_owned());
                }
                parts.push(sender.clone());
                parts.push(subject.clone());
                parts.extend(date.clone());
                parts.extend(snippet.clone());
                parts.join(", ")
            }
            Self::Media {
                title,
                byline,
                duration,
                ..
            } => {
                let mut parts = vec![title.clone()];
                parts.extend(byline.clone());
                parts.extend(duration.clone());
                parts.join(", ")
            }
            Self::FileHit {
                path,
                line,
                snippet,
                ..
            } => match line {
                Some(line) => format!("{path}, line {line}, {}", snippet.trim()),
                None => format!("{path}, {}", snippet.trim()),
            },
            Self::Command(row) => {
                let mut parts = Vec::new();
                if !row.facts.is_empty() {
                    parts.push(row.facts.join(", "));
                }
                if let Some(badge) = &row.badge {
                    parts.push(format!("Status: {badge}"));
                }
                parts.join(". ")
            }
        }
    }

    /// The row's location line: `path:line` for a file hit.
    #[must_use]
    pub fn location(&self) -> Option<String> {
        match self {
            Self::FileHit {
                path,
                line: Some(line),
                ..
            } => Some(format!("{path}:{line}")),
            Self::FileHit { path, .. } => Some(path.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_fill_only_their_own_shape() {
        let message = SearchRow::message("Ada", "Invoice")
            .snippet("Attached is March")
            .date("Tue")
            .unread(true)
            .line(4);
        assert_eq!(
            message.spoken(),
            "Unread, Ada, Invoice, Tue, Attached is March"
        );
        let hit = SearchRow::file_hit("src/main.rs", "fn main() {}")
            .line(3)
            .matched(3..7);
        assert_eq!(hit.location().as_deref(), Some("src/main.rs:3"));
        let SearchRow::FileHit { matched, .. } = &hit else {
            unreachable!()
        };
        assert_eq!(matched.clone(), Some(3..7));
        let bad = SearchRow::file_hit("a", "é").matched(0..1);
        assert!(matches!(bad, SearchRow::FileHit { matched: None, .. }));
        let media = SearchRow::media("Talk").byline("Conf").duration("12:04");
        assert_eq!(media.spoken(), "Talk, Conf, 12:04");
    }
}
