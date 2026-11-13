//! What a message carries besides its words: the thing it was sent from.
//!
//! A person points at something in any app — a row, a selection of rows, a
//! sentence, a file — and says something about it. [`Attached`] is that
//! thing, in the terms its own app uses: the [`ResourceAt`] that keeps it,
//! the [`Anchor`] into it when the person meant part of it, and the words
//! they saw when they sent it. Nothing here is chat's: an agent's task, a
//! shared window and a message all attach the same values.
//!
//! A text anchor is held by Automerge cursors rather than by offsets, so an
//! edit above the quoted run moves the anchor with its words instead of
//! reporting the wrong ones after somebody types a sentence above it.

use std::ops::Range;

use automerge::{Cursor, ObjId, ObjType, Prop, ROOT, ReadDoc as _, TextEncoding, Value};
use serde::{Deserialize, Serialize};

use tilo_actions::Doc;

use crate::ResourceAt;

/// The part of a record an attachment names, when it is not the whole of it.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    tilo_resource::Model,
    tilo_resource::JsonSchema,
)]
pub enum Anchor {
    /// One item the record publishes: a row, a card, a block.
    Item {
        /// The item's id in its app, the one the app publishes it under.
        id: String,
    },
    /// Several items, in the order the person picked them.
    Items {
        /// The items' own ids.
        ids: Vec<String>,
    },
    /// A run of text in the record's document, held by Automerge cursors so
    /// an edit above it moves it with its words.
    Text {
        /// Where the text object is in the document: the keys from the root,
        /// `/`-separated, a number naming a list index. `""` is the root
        /// itself, for a document that is one text.
        path: String,
        /// The run's first character, as a cursor's text form.
        start: String,
        /// The character after its last, likewise.
        end: String,
    },
}

impl Anchor {
    /// The whole of `id`.
    #[must_use]
    pub fn item(id: impl Into<String>) -> Self {
        Self::Item { id: id.into() }
    }

    /// Every item of `ids`, in order.
    #[must_use]
    pub fn items(ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Items {
            ids: ids.into_iter().map(Into::into).collect(),
        }
    }

    /// Characters `start..end` of the text at `path` in `doc`, held by
    /// cursors; `None` when the document has no text there.
    #[must_use]
    pub fn text(doc: &Doc, path: impl Into<String>, start: usize, end: usize) -> Option<Self> {
        let path = path.into();
        let obj = text_object(doc, &path)?;
        let text = doc.text(&obj).ok()?;
        let encoding = doc.text_encoding();
        let start_units = units_at(&text, start, encoding)?;
        let end_units = units_at(&text, end, encoding)?;
        let start = cursor_at(doc, &obj, start_units.min(end_units))?;
        let end = cursor_at(doc, &obj, start_units.max(end_units))?;
        Some(Self::Text {
            path,
            start: start.to_string(),
            end: end.to_string(),
        })
    }

    /// The run a [`Anchor::Text`] names in `doc` now, as its words and its
    /// current character range: moved by every edit made since it was
    /// taken. `None` for the other anchors, which are already resolved, and
    /// for a text the document no longer holds.
    #[must_use]
    pub fn resolve_text(&self, doc: &Doc) -> Option<(String, Range<usize>)> {
        let Self::Text { path, start, end } = self else {
            return None;
        };
        let obj = text_object(doc, path)?;
        let text = doc.text(&obj).ok()?;
        let encoding = doc.text_encoding();
        let position = |cursor: &str| -> Option<usize> {
            let cursor = Cursor::try_from(cursor).ok()?;
            doc.get_cursor_position(&obj, &cursor, None).ok()
        };
        let from = chars_before(&text, position(start)?, encoding)?;
        let to = chars_before(&text, position(end)?, encoding)?;
        let (from, to) = (from.min(to), from.max(to));
        let words = text
            .chars()
            .skip(from)
            .take(to.saturating_sub(from))
            .collect();
        Some((words, from..to))
    }
}

/// A cursor at `units` in the text object `obj`: the end cursor where it is
/// the text's end, which the document wants named rather than indexed.
fn cursor_at(doc: &Doc, obj: &ObjId, units: usize) -> Option<Cursor> {
    if units >= doc.length(obj) {
        return Some(Cursor::End);
    }
    doc.get_cursor(obj, units, None).ok()
}

/// The text object at `path` in `doc`.
fn text_object(doc: &Doc, path: &str) -> Option<ObjId> {
    let mut obj = ROOT;
    let mut named = false;
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        let prop = match segment.parse::<usize>() {
            Ok(index) => Prop::Seq(index),
            Err(_) => Prop::Map(segment.to_owned()),
        };
        let (value, id) = doc.get(&obj, prop).ok().flatten()?;
        if !matches!(value, Value::Object(_)) {
            return None;
        }
        obj = id;
        named = true;
    }
    named
        .then_some(obj)
        .filter(|obj| doc.object_type(obj).is_ok_and(|kind| kind == ObjType::Text))
}

/// How many units of the document's indexing one character counts as, or
/// `None` for an indexing of whole grapheme clusters, which no writer of
/// Tilo documents sets and this crate cannot count without its own table.
fn units_per_char(ch: char, encoding: TextEncoding) -> Option<usize> {
    Some(match encoding {
        TextEncoding::UnicodeCodePoint => 1,
        TextEncoding::Utf8CodeUnit => ch.len_utf8(),
        TextEncoding::Utf16CodeUnit => ch.len_utf16(),
        TextEncoding::GraphemeCluster => return None,
    })
}

/// How many characters come before `units` of `text`.
fn chars_before(text: &str, units: usize, encoding: TextEncoding) -> Option<usize> {
    let (mut used, mut chars): (usize, usize) = (0, 0);
    for ch in text.chars() {
        let width = units_per_char(ch, encoding)?;
        if used.saturating_add(width) > units {
            break;
        }
        used = used.saturating_add(width);
        chars = chars.saturating_add(1);
    }
    Some(chars)
}

/// How many units of `text` its first `chars` characters take.
fn units_at(text: &str, chars: usize, encoding: TextEncoding) -> Option<usize> {
    text.chars().take(chars).try_fold(0usize, |used, ch| {
        used.checked_add(units_per_char(ch, encoding)?)
    })
}

/// A file kept with a record: its bytes are in the content store under
/// `blob`, and these are what a reader shows before loading them.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    tilo_resource::Model,
    tilo_resource::JsonSchema,
)]
pub enum StoredFile {
    /// A picture, with the pixel size read when it was imported.
    Image {
        /// The name it was posted under.
        name: String,
        /// Its content hash in the store.
        blob: String,
        /// Its width in pixels.
        w: u32,
        /// Its height in pixels.
        h: u32,
    },
    /// A video, shown as a poster card; the bytes are read on demand.
    Video {
        /// The name it was posted under.
        name: String,
        /// Its content hash in the store.
        blob: String,
        /// Its width in pixels, 0 when the container was not read.
        w: u32,
        /// Its height in pixels, 0 likewise.
        h: u32,
        /// Its length in bytes.
        size: u64,
    },
    /// Any other file, shown as a chip with its name and size.
    File {
        /// The name it was posted under.
        name: String,
        /// Its content hash in the store.
        blob: String,
        /// Its length in bytes.
        size: u64,
    },
}

impl StoredFile {
    /// The portable content hash backing it: safe to replicate, and never a
    /// path meaningful on one machine.
    #[must_use]
    pub fn blob(&self) -> &str {
        match self {
            Self::Image { blob, .. } | Self::Video { blob, .. } | Self::File { blob, .. } => blob,
        }
    }

    /// The name it was posted under.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Image { name, .. } | Self::Video { name, .. } | Self::File { name, .. } => name,
        }
    }

    /// Its length in bytes, when the import measured one: an image's is the
    /// store's, and an older one carries none rather than a guess.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        match self {
            Self::Image { .. } => None,
            Self::Video { size, .. } | Self::File { size, .. } => Some(*size),
        }
    }
}

/// What a message or a task carries besides its words: a record or a part
/// of one, words quoted from one, or a file.
///
/// One type for every way of pointing at something, so whoever sends a task
/// to a colleague, or hands one to an agent, attaches the same thing whether
/// it is a whole record, the rows they selected in it, a sentence, or a file
/// — and the reader opens it where it lives without knowing which app that
/// was.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    tilo_resource::Model,
    tilo_resource::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attached {
    /// Something a record keeps, and the part of it meant.
    Resource {
        /// The record it is in.
        at: ResourceAt,
        /// The part meant, when not the whole.
        anchor: Option<Anchor>,
        /// What the person saw it called when they sent it.
        label: String,
    },
    /// Words quoted from a record, as they read when the person sent them.
    Text {
        /// The words.
        text: String,
        /// The record they were quoted from, when known.
        from: Option<ResourceAt>,
    },
    /// An image; `w`/`h` are the real pixel dimensions read at import time.
    Image {
        name: String,
        blob: String,
        #[serde(alias = "width")]
        w: u32,
        #[serde(alias = "height")]
        h: u32,
    },
    /// A video held by its content hash, `w`/`h` its resolution when known.
    Video {
        name: String,
        blob: String,
        #[serde(alias = "width")]
        w: u32,
        #[serde(alias = "height")]
        h: u32,
        size: u64,
    },
    /// A file held by its content hash.
    File {
        name: String,
        blob: String,
        size: u64,
    },
}

impl Attached {
    /// Something `at` keeps, marked `label`.
    #[must_use]
    pub fn resource(at: ResourceAt, anchor: Option<Anchor>, label: impl Into<String>) -> Self {
        Self::Resource {
            at,
            anchor,
            label: label.into(),
        }
    }

    /// What a chip for it says: the label it was sent with, the words it
    /// quotes, or a file's name.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Resource { label, .. } => label,
            Self::Text { text, .. } => text,
            Self::Image { name, .. } | Self::Video { name, .. } | Self::File { name, .. } => name,
        }
    }

    /// Whether it shows as a chip rather than a card: a record or a run of
    /// words, which hold no bytes of their own.
    #[must_use]
    pub fn is_chip(&self) -> bool {
        matches!(self, Self::Resource { .. } | Self::Text { .. })
    }

    /// The record it is in, when it is in one.
    #[must_use]
    pub fn at(&self) -> Option<&ResourceAt> {
        match self {
            Self::Resource { at, .. } => Some(at),
            Self::Text { from, .. } => from.as_ref(),
            Self::Image { .. } | Self::Video { .. } | Self::File { .. } => None,
        }
    }

    /// The part of its record it names, when it names one.
    #[must_use]
    pub fn anchor(&self) -> Option<&Anchor> {
        match self {
            Self::Resource { anchor, .. } => anchor.as_ref(),
            Self::Text { .. } | Self::Image { .. } | Self::Video { .. } | Self::File { .. } => None,
        }
    }

    /// The content hash of the file it carries, when it carries one. Safe to
    /// replicate: it cannot expose a peer-local filesystem layout.
    #[must_use]
    pub fn blob(&self) -> Option<&str> {
        match self {
            Self::Image { blob, .. } | Self::Video { blob, .. } | Self::File { blob, .. } => {
                Some(blob)
            }
            Self::Resource { .. } | Self::Text { .. } => None,
        }
    }

    /// Its length in bytes, when the import measured one: an image's is the
    /// store's, and an older one carries none rather than a guess.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        match self {
            Self::Video { size, .. } | Self::File { size, .. } => Some(*size),
            Self::Image { .. } | Self::Resource { .. } | Self::Text { .. } => None,
        }
    }

    /// Its pixel dimensions, when it is a picture or a clip whose header was
    /// read: `(0, 0)` for anything else.
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Image { w, h, .. } | Self::Video { w, h, .. } => (*w, *h),
            Self::File { .. } | Self::Resource { .. } | Self::Text { .. } => (0, 0),
        }
    }

    /// The `record:<app>:<record>` provider of the record it is in, its
    /// record empty for the app's primary one.
    #[must_use]
    pub fn provider(&self) -> Option<String> {
        let at = self.at()?;
        let record = crate::link::RecordSegment::new(at.record.as_deref().unwrap_or_default())
            .map(|segment| segment.provider_part().to_owned())
            .unwrap_or_default();
        Some(format!("record:{}:{record}", at.app))
    }

    /// What it points at, as an agent asked about it reads it: the record by
    /// its app, kind and id, the items or the run of characters meant, and
    /// what the person saw it as. `None` for words quoted from nowhere and for
    /// a file, which name no record to act on.
    #[must_use]
    pub fn subject(&self) -> Option<tilo_actions::SelectionSubject> {
        use tilo_actions::{SelectionAnchor, SelectionPlace};
        let at = self.at()?;
        let span = at
            .reference
            .span
            .filter(|span| span.unit == crate::SpanUnit::Chars && !span.is_empty());
        let anchor = match self.anchor() {
            Some(Anchor::Item { id }) => SelectionAnchor::default().at(match span {
                Some(span) => SelectionPlace::range(id.clone(), span.start, span.end),
                None => SelectionPlace::item(id.clone()),
            }),
            Some(Anchor::Items { ids }) => SelectionAnchor {
                items: ids
                    .iter()
                    .take(tilo_actions::MAX_SELECTED_ITEMS)
                    .map(|id| SelectionPlace::item(id.clone()))
                    .collect(),
                ..SelectionAnchor::default()
            },
            Some(Anchor::Text { .. }) | None => SelectionAnchor::default(),
        }
        .with_excerpt(self.label());
        let label = if at.reference.label.trim().is_empty() {
            self.label()
        } else {
            &at.reference.label
        };
        Some(tilo_actions::SelectionSubject {
            app_id: at.app.clone(),
            app_label: at.app.clone(),
            resource: tilo_actions::ActionResource {
                id: at.reference.id.clone(),
                kind: at.reference.kind.clone(),
                label: tilo_actions::excerpt_of(label).unwrap_or_default(),
            },
            anchor,
        })
        .filter(|subject| subject.validate().is_ok())
    }

    /// The one action that opens it in the workspace, on the app and record
    /// that keep it: none for a file, which a reader opens from its card.
    #[must_use]
    pub fn open_target(&self, workspace: &str) -> Option<tilo_actions::remote::RemoteActionTarget> {
        let at = self.at()?;
        let primary = at
            .record
            .as_deref()
            .map(crate::link::RecordSegment::new)
            .and_then(Result::ok);
        let record = match primary {
            Some(crate::link::RecordSegment::Id(record)) => Some(record),
            Some(crate::link::RecordSegment::Primary) | None => None,
        };
        Some(
            at.reference
                .open_target(workspace, &at.app, record.as_deref()),
        )
    }
}

impl From<StoredFile> for Attached {
    fn from(file: StoredFile) -> Self {
        match file {
            StoredFile::Image { name, blob, w, h } => Self::Image { name, blob, w, h },
            StoredFile::Video {
                name,
                blob,
                w,
                h,
                size,
            } => Self::Video {
                name,
                blob,
                w,
                h,
                size,
            },
            StoredFile::File { name, blob, size } => Self::File { name, blob, size },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use tilo_resource::Model as _;

    use crate::{ResourceRef, Span};

    fn at(app: &str, record: &str, kind: &str, id: &str, label: &str) -> ResourceAt {
        ResourceAt {
            app: app.to_owned(),
            record: (!record.is_empty()).then(|| record.to_owned()),
            reference: ResourceRef::whole(kind, id, label),
        }
    }

    #[test]
    fn an_attachment_is_what_its_own_app_keeps_and_opens_where_it_lives() {
        let note = at(
            "so.tilo.notes",
            "uuid:7",
            "notes.page",
            "notes.page:1",
            "Plan",
        );
        let attached = Attached::resource(note.clone(), Some(Anchor::item("block-2")), "Plan");
        assert_eq!(attached.label(), "Plan");
        assert_eq!(attached.at(), Some(&note));
        assert_eq!(attached.anchor(), Some(&Anchor::item("block-2")));
        assert_eq!(
            attached.provider().as_deref(),
            Some("record:so.tilo.notes:uuid:7")
        );
        let target = attached.open_target("ws-1").expect("opens");
        assert_eq!(
            tilo_actions::remote::application_provider_record(&target.provider),
            Some("uuid:7")
        );
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new("notes.page:1", "open")
        );

        // The app's primary record is the record it opens on its own.
        let primary = Attached::resource(
            at("so.tilo.chat", "", "chat.message", "m1", "Hi"),
            None,
            "Hi",
        );
        assert_eq!(primary.provider().as_deref(), Some("record:so.tilo.chat:"));
        let target = primary.open_target("ws-1").expect("opens");
        assert_eq!(
            tilo_actions::remote::application_provider_record(&target.provider),
            None
        );

        // A file opens from its own card, not through an app's record.
        let file = Attached::from(StoredFile::File {
            name: "report.pdf".into(),
            blob: "blake3:00".into(),
            size: 12,
        });
        assert_eq!(file.label(), "report.pdf");
        assert_eq!(file.open_target("ws-1"), None);
        assert_eq!(file.provider(), None);
    }

    #[test]
    fn a_selected_sentence_is_quoted_with_the_record_it_came_from() {
        let notes = at("so.tilo.notes", "", "notes.page", "notes.page:1", "Plan");
        let quoted = Attached::Text {
            text: "ship on Friday".into(),
            from: Some(notes.clone()),
        };
        assert_eq!(quoted.label(), "ship on Friday");
        assert_eq!(quoted.at(), Some(&notes));
        assert!(quoted.open_target("ws-1").is_some());
        // Words alone: worth reading, opening nothing.
        let bare = Attached::Text {
            text: "ship on Friday".into(),
            from: None,
        };
        assert_eq!(bare.label(), "ship on Friday");
        assert!(bare.open_target("ws-1").is_none());
    }

    /// What a message carries reaches an agent as the record it names, never
    /// a picture of it: the app, kind and id to act on, and the item meant.
    #[test]
    fn an_attachment_is_the_subject_an_agent_acts_on() {
        let issue = at(
            "so.tilo.tasks",
            "",
            "tasks.issue",
            "tasks.tracker/issues/42",
            "Launch checklist",
        );
        let attached = Attached::resource(issue, Some(Anchor::item("42")), "Launch checklist");
        let subject = attached.subject().expect("a record to act on");
        assert_eq!(subject.app_id, "so.tilo.tasks");
        assert_eq!(subject.resource.id, "tasks.tracker/issues/42");
        assert_eq!(subject.resource.kind, "tasks.issue");
        assert_eq!(
            subject.anchor.place,
            Some(tilo_actions::SelectionPlace::item("42"))
        );
        assert_eq!(subject.anchor.excerpt.as_deref(), Some("Launch checklist"));

        // A whole record is still a subject, said by what the person saw.
        let whole = Attached::resource(
            at("so.tilo.notes", "uuid:7", "notes.page", "notes.page:1", ""),
            None,
            "Plan",
        );
        let subject = whole.subject().expect("the whole record");
        assert_eq!(subject.resource.label, "Plan");
        assert!(subject.anchor.place.is_none() && subject.anchor.items.is_empty());

        // Quoted words name the record they came from; bare words and files
        // name none.
        let quoted = Attached::Text {
            text: "ship on Friday".into(),
            from: Some(at(
                "so.tilo.notes",
                "",
                "notes.page",
                "notes.page:1",
                "Plan",
            )),
        };
        assert_eq!(
            quoted
                .subject()
                .expect("from a record")
                .anchor
                .excerpt
                .as_deref(),
            Some("ship on Friday")
        );
        let bare = Attached::Text {
            text: "ship on Friday".into(),
            from: None,
        };
        assert_eq!(bare.subject(), None);
    }

    /// A text anchor names the run, not its offsets: an edit above it, made
    /// on another device and merged here, moves it with its words.
    #[test]
    fn a_text_anchor_survives_an_edit_above_it() {
        let mut doc = Doc::new();
        let body = doc.put_object(ROOT, "body", ObjType::Text).expect("a text");
        doc.splice_text(&body, 0, 0, "hello world").expect("writes");
        let anchor = Anchor::text(&doc, "body", 6, 11).expect("a run");
        assert_eq!(anchor.resolve_text(&doc), Some(("world".to_owned(), 6..11)));

        // Somebody types above it, in another copy, and the copies meet.
        let mut other = doc.fork();
        let other_body = other.get(ROOT, "body").expect("reads").expect("a body").1;
        other
            .splice_text(&other_body, 0, 0, "oh, ")
            .expect("writes above");
        let edits = other.save();
        tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &edits).expect("merges");
        let body = doc.get(ROOT, "body").expect("reads").expect("a body").1;
        assert_eq!(doc.text(&body).expect("reads"), "oh, hello world");

        // The anchor still names "world", at its new offsets — where the
        // offsets it was taken at would now read somebody else's words.
        assert_eq!(
            anchor.resolve_text(&doc),
            Some(("world".to_owned(), 10..15))
        );
        assert_eq!(Span::chars(6, 11).slice("oh, hello world"), Some("llo w"));
    }

    #[test]
    fn an_anchor_reads_the_same_after_a_round_trip_through_its_record() {
        let anchor = Anchor::items(["row-1", "row-2"]);
        let json = anchor.to_json();
        assert_eq!(Anchor::from_json(&json).expect("reads"), anchor);
        let item = Anchor::item("row-1");
        assert_eq!(Anchor::from_json(&item.to_json()).expect("reads"), item);
    }
}
