//! How a search hit reads in the results list. A provider says what a hit *is* — a
//! message, a video, a match in a file — and fills that row's slots; the platform draws
//! it in the palette's row slot on the list row geometry, in every palette, the same way
//! for every app. Rows are data, never drawing code, so they reach the shell's palette
//! from any app unchanged.

use std::ops::Range;

use vello::peniko::Color;

use crate::icons::AppImage;

/// The shape of one search hit's row.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchRow {
    Message {
        sender: String,
        subject: String,
        snippet: Option<String>,
        date: Option<String>,
        unread: bool,
    },
    Media {
        thumbnail: Option<AppImage>,
        title: String,
        byline: Option<String>,
        duration: Option<String>,
    },
    /// A match inside a file: the file's path, the line, and the matched line itself with
    /// the matched span marked (byte range into `snippet`).
    FileHit {
        path: String,
        line: Option<u32>,
        snippet: String,
        matched: Option<Range<usize>>,
    },
    Command(Box<CommandRow>),
    /// An item of a published resource: the app that keeps it, the item's kind and its
    /// id. A hit is a reference to what it opens, never a copy of it: choosing one runs
    /// the app's `open` at the item, and the row it is drawn with is the app's own.
    Item {
        app: String,
        kind: String,
        id: String,
    },
}

/// The slots of a [`SearchRow::Command`] row beyond its title, subtitle and icon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandRow {
    pub description_is_path: bool,
    pub facts: Vec<String>,
    pub image: Option<AppImage>,
    pub avatar: Option<(String, String)>,
    pub badge: Option<String>,
    pub keycaps: Option<Vec<String>>,
    pub faces: Vec<RowFace>,
    pub outcome: Option<String>,
    pub alternates: Vec<Alternate>,
    pub echoes_query: bool,
}

impl CommandRow {
    /// The alternative the selected row shows: the one whose modifier is `held`, else the
    /// first it offers.
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
    pub identity: String,
    pub colour: Color,
    pub is_agent: bool,
    pub dimmed: bool,
}

/// A modifier held while a row is activated, choosing an alternative activation the row
/// offers ([`CommandRow::alternates`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActivationModifier {
    Shift,
    Ctrl,
    Alt,
}

impl ActivationModifier {
    /// The one modifier `state` holds, or `None` for none or a mix: a chord of two is not
    /// one of the alternatives.
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

/// What a row does when it is activated with `modifier` held, in the words the selected
/// row shows while that modifier is down ("Opens as a tab here").
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

    /// The item `id` of the kind `kind` in `app`, as a reference.
    pub fn item(app: impl Into<String>, kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self::Item {
            app: app.into(),
            kind: kind.into(),
            id: id.into(),
        }
    }

    /// A command row whose title is what the person typed, said back to them: the row
    /// that hands a request to an agent or takes a query to the web
    /// ([`CommandRow::echoes_query`]).
    pub fn echoing_query() -> Self {
        Self::Command(Box::new(CommandRow {
            echoes_query: true,
            ..CommandRow::default()
        }))
    }

    /// The item row for `resource` (`notes.notebook/pages/<id>`) of `app`: what a hit on
    /// a resource an app publishes as items is drawn with.
    pub fn item_of(app: impl Into<String>, resource: &str) -> Option<Self> {
        let kind = tilo_resource::items::item_kind(resource)?;
        let id = resource.rsplit('/').next()?;
        Some(Self::item(app, kind, id))
    }

    /// The resource id of the item this row is, when it is one:
    /// `<resource>/<collection>/<id>` (`notes.notebook/pages/<id>`), the address its
    /// app's `open` shows.
    #[must_use]
    pub fn item_resource(&self) -> Option<String> {
        let Self::Item { kind, id, .. } = self else {
            return None;
        };
        let (resource, collection) = kind.rsplit_once('.')?;
        (!resource.is_empty() && !collection.is_empty() && !id.is_empty())
            .then(|| format!("{resource}/{collection}/{id}"))
    }

    /// The app keeping the item this row is, when it is one.
    #[must_use]
    pub fn item_app(&self) -> Option<&str> {
        match self {
            Self::Item { app, .. } => Some(app),
            _ => None,
        }
    }

    /// The message's body snippet, or a file hit's matched line.
    #[must_use]
    pub fn snippet(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        match &mut self {
            Self::Message { snippet, .. } => *snippet = Some(text),
            Self::FileHit { snippet, .. } => *snippet = text,
            Self::Media { .. } | Self::Command(_) | Self::Item { .. } => {}
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

    /// The matched span of a file hit's snippet, as a byte range.
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
            Self::Item { .. } => String::new(),
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

    /// A row that restates the query says so, so every palette draws it without setting
    /// off spans of the person's own words; an ordinary command row matches as it always
    /// did.
    #[test]
    fn a_row_that_restates_the_query_declares_itself() {
        let echo = SearchRow::echoing_query();
        let SearchRow::Command(row) = &echo else {
            panic!("an echo is a command row");
        };
        assert!(row.echoes_query);
        assert!(!CommandRow::default().echoes_query);
    }

    #[test]
    fn an_item_row_addresses_the_item_it_is() {
        let row = SearchRow::item("so.tilo.notes", "notes.notebook.pages", "page-7");
        assert_eq!(row.item_app(), Some("so.tilo.notes"));
        assert_eq!(
            row.item_resource().as_deref(),
            Some("notes.notebook/pages/page-7")
        );
        assert_eq!(row.spoken(), "", "the result's title speaks for it");
        assert_eq!(row.location(), None);
        assert_eq!(
            SearchRow::item("so.tilo.notes", "no-dot", "x").item_resource(),
            None,
            "a kind with no collection names no item"
        );
        assert_eq!(SearchRow::message("Ada", "Hi").item_resource(), None);
    }

    #[test]
    fn an_item_row_is_read_from_the_resource_it_names() {
        assert_eq!(
            SearchRow::item_of("so.tilo.notes", "notes.notebook/pages/page-7"),
            Some(SearchRow::item(
                "so.tilo.notes",
                "notes.notebook.pages",
                "page-7"
            ))
        );
        assert_eq!(
            SearchRow::item_of("so.tilo.notes", "notes.notebook"),
            None,
            "a whole notebook is no item"
        );
        assert_eq!(SearchRow::item_of("so.tilo.notes", "page-7"), None);
    }
}
