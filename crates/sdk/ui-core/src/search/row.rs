//! How a search hit reads in the results list.
//!
//! A provider says what a hit *is* — a message, a video, a match in a file —
//! and fills that row's slots; the platform draws it in the palette's row
//! slot on the list row geometry, in every palette, the same way for every
//! app. A hit without a [`SearchRow`] is drawn as the generic row: icon,
//! title and subtitle. Rows are data, never drawing code, so they reach the
//! shell's palette from any app unchanged.

use std::ops::Range;

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
            Self::Media { .. } => {}
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
