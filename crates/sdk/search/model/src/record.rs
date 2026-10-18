//! A search index's records and what a search returns: [`Document`],
//! [`BodySource`], [`Hit`], [`Hits`], [`Page`] and [`Signals`].

use std::fmt;
use std::sync::Arc;

use crate::weight::FieldWeight;

/// The segment value of a record's own document.
pub const RECORD_SEGMENT: u64 = u64::MAX;

/// One contiguous run of body text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPiece<'a> {
    pub text: &'a str,
    /// Whether a word break separates this piece from the text before it.
    pub space_before: bool,
}

/// Long text a record is found by, owned by the caller: the index reads it
/// while indexing and never copies it into the retained record.
///
/// A body is a sequence of *segments* (a paragraph, a block, a line), each
/// a sequence of [`BodyPiece`]s. The source must not change while indexed.
pub trait BodySource: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>>;

    /// Whether a match in `segment` is its own hit ([`Hit::segment`]).
    /// Other segments are found as the record itself.
    fn locates(&self, _segment: usize) -> bool {
        false
    }
}

/// One record: a caller-chosen key, weighted fields, and optionally a body.
#[derive(Clone)]
pub struct Document {
    key: u64,
    fields: Vec<(String, FieldWeight)>,
    body: Option<Arc<dyn BodySource>>,
    signals: (i32, u32),
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("key", &self.key)
            .field("fields", &self.fields)
            .field("body", &self.body.is_some())
            .field("signals", &self.signals)
            .finish()
    }
}

impl Document {
    /// A record with no fields yet. Keys order hits of equal relevance.
    #[must_use]
    pub const fn new(key: u64) -> Self {
        Self {
            key,
            fields: Vec::new(),
            body: None,
            signals: (0, 0),
        }
    }

    /// Per-record numbers a custom score reads ([`Signals`]): a boost, and
    /// a length such as the name's.
    #[must_use]
    pub const fn signals(mut self, boost: i32, length: u32) -> Self {
        self.signals = (boost, length);
        self
    }

    /// Add a short field, found and ranked by its weight. Values never
    /// match across each other.
    #[must_use]
    pub fn field(mut self, text: impl Into<String>, weight: FieldWeight) -> Self {
        self.fields.push((text.into(), weight));
        self
    }

    /// Attach a body. A body match follows every field match.
    #[must_use]
    pub fn body(mut self, body: Arc<dyn BodySource>) -> Self {
        self.body = Some(body);
        self
    }

    #[must_use]
    pub const fn key(&self) -> u64 {
        self.key
    }

    #[must_use]
    pub fn fields(&self) -> &[(String, FieldWeight)] {
        &self.fields
    }

    /// The record's [`Self::signals`]: its boost and length.
    #[must_use]
    pub const fn signals_pair(&self) -> (i32, u32) {
        self.signals
    }

    /// The record's body, if it has one ([`Self::body`]).
    #[must_use]
    pub fn body_source(&self) -> Option<&Arc<dyn BodySource>> {
        self.body.as_ref()
    }
}

/// One match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub key: u64,
    /// The body segment a located match is in; `None` for the record itself.
    pub segment: Option<usize>,
    /// The field rank (`0..=`[`MAX_FIELD_RANK`](crate::weight::MAX_FIELD_RANK)) of a
    /// field match; `None` for a match in body text alone.
    pub rank: Option<i32>,
    /// The ordering score: the rank, or what a custom score made of it.
    pub score: i32,
}

/// What a custom score (`SearchIndex::search_scored` in `tilo-search`) sees of one hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signals {
    /// As [`Hit::rank`].
    pub rank: Option<i32>,
    /// The record's [`Document::signals`].
    pub boost: i32,
    pub length: u32,
}

/// One page of matches, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    pub hits: Vec<Hit>,
    /// Whether more matches follow this page.
    pub more: bool,
}

/// Which page of matches to return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    /// Matches to skip.
    pub offset: usize,
    pub limit: usize,
}

impl Page {
    #[must_use]
    pub const fn first(limit: usize) -> Self {
        Self { offset: 0, limit }
    }
}
