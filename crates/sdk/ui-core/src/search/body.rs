//! Full-text bodies for [`SearchCatalog`](super::SearchCatalog).
//!
//! A body is prose the catalogue must find but must not keep: an EPUB
//! chapter, a long note, a message. Its owner keeps the text and exposes it
//! through a [`SearchBodySource`]; the catalogue reads it once while
//! indexing. A source that names locations reports the segment a match is
//! in as its own hit.

use std::fmt;
use std::sync::Arc;

pub use tilo_search::BodyPiece;

use super::provider::SearchResult;

/// Prose a catalogue record can be found by, owned by the app.
///
/// A body is a sequence of *segments* (the unit a hit points at: a paragraph,
/// a block, a line) and each segment a sequence of [`BodyPiece`]s (runs of
/// styled text, say). The source must be immutable: the catalogue indexes it
/// as it is when the record is added.
pub trait SearchBodySource: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>>;

    /// The hit to report for a match in `segment`. `None` reports the
    /// record's own result instead, once.
    fn location(&self, _segment: usize) -> Option<SearchResult> {
        None
    }
}

/// A single-segment body of plain text.
impl SearchBodySource for String {
    fn segment_count(&self) -> usize {
        1
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment == 0)
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        (segment == 0 && piece == 0).then_some(BodyPiece {
            text: self,
            space_before: false,
        })
    }
}

/// One segment per string, separated by word breaks.
impl SearchBodySource for Vec<String> {
    fn segment_count(&self) -> usize {
        self.len()
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment < self.len())
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        let text = self.get(segment)?;
        (piece == 0).then_some(BodyPiece {
            text,
            space_before: segment > 0,
        })
    }
}

/// A body attached to a [`SearchRecord`](super::SearchRecord).
///
/// Cloning one is O(1). A record whose body is the same source as before
/// is unchanged, so apps that rebuild records over mostly unchanged content
/// keep their `SearchBody` values and hand the same ones back.
#[derive(Clone)]
pub struct SearchBody {
    source: Arc<dyn SearchBodySource>,
}

impl fmt::Debug for SearchBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchBody")
            .field("segments", &self.source.segment_count())
            .finish()
    }
}

impl PartialEq for SearchBody {
    fn eq(&self, other: &Self) -> bool {
        self.shares_source_with(other)
    }
}

impl SearchBody {
    #[must_use]
    pub fn new(source: Arc<dyn SearchBodySource>) -> Self {
        Self { source }
    }

    /// Whether both bodies are the same source.
    #[must_use]
    pub fn shares_source_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
    }

    #[must_use]
    pub fn source(&self) -> &Arc<dyn SearchBodySource> {
        &self.source
    }
}

/// An app's body as the index reads it.
pub(crate) struct BodyIndex(pub Arc<dyn SearchBodySource>);

impl tilo_search::BodySource for BodyIndex {
    fn segment_count(&self) -> usize {
        self.0.segment_count()
    }

    fn piece_count(&self, segment: usize) -> usize {
        self.0.piece_count(segment)
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        self.0.piece(segment, piece)
    }

    fn locates(&self, segment: usize) -> bool {
        self.0.location(segment).is_some()
    }
}
