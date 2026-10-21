//! Where the person is in each document an app shows.
//!
//! A reading position is the person's own: it follows them to their other
//! devices and never reaches a collaborator, whatever room the document is
//! in. Every reader keeps its places in this one record of the person's own
//! room ([`crate::framework::PersonRecord`]), so the system owns reading
//! positions the same way for Feeds, the Browser's reader mode and Mail:
//! a reader reports a [`ReadingPlace`](ui_core::reading::ReadingPlace) and
//! gets it back when the document opens again, on any device.

use serde::{Deserialize, Serialize};
use tilo_resource::{ActionCx, Map, Model, Refusal, resource};
use ui_core::reading::ReadingPlace;

/// Where the person is in one document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Model)]
pub struct Place {
    /// The block at the top of the screen, counted from 0 in the document's
    /// body: a block, not an offset, so the person lands on the same passage
    /// at any window or text size.
    pub block: usize,
    /// How far into that block, from 0 to 1.
    pub fraction: f32,
    /// How far through the whole document, from 0 to 1.
    pub progress: f32,
    /// When the person was last here, in milliseconds since the epoch.
    pub read_at: u64,
}

impl Place {
    /// The place as a reader shows it.
    #[must_use]
    pub fn reading_place(&self) -> ReadingPlace {
        ReadingPlace {
            block: u32::try_from(self.block).unwrap_or(u32::MAX),
            fraction: self.fraction,
            progress: self.progress,
        }
    }
}

/// The person's reading positions, by document.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct ReadingPositions {
    /// Where the person is, under each document's key (an article's id, a
    /// page's address).
    pub places: Map<Place>,
}

#[resource(kind = "tilo.reading-positions", label = "Reading positions")]
impl ReadingPositions {
    /// Keep the place
    ///
    /// Remembers where the person is in a document.
    #[action(housekeeping)]
    pub fn keep_place(
        &mut self,
        cx: &mut ActionCx,
        #[param("The document's key")] document: String,
        #[param("The block at the top of the screen, counted from 0")] block: usize,
        #[param("How far into that block, from 0 to 1")] fraction: f32,
        #[param("How far through the whole document, from 0 to 1")] progress: f32,
    ) -> Result<(), Refusal> {
        if document.trim().is_empty() {
            return Err(Refusal::new("a reading position names its document"));
        }
        if !fraction.is_finite() || !progress.is_finite() {
            return Err(Refusal::new("a reading position is a fraction from 0 to 1"));
        }
        self.places.insert(
            document,
            Place {
                block,
                fraction: fraction.clamp(0.0, 1.0),
                progress: progress.clamp(0.0, 1.0),
                read_at: u64::try_from(cx.now_ms()).unwrap_or_default(),
            },
        );
        Ok(())
    }

    /// Forget the place
    ///
    /// Forgets where the person was in a document.
    #[action(housekeeping)]
    pub fn forget_place(&mut self, #[param("The document's key")] document: String) {
        self.places.remove(&document);
    }
}

impl ReadingPositions {
    /// Where the person is in `document`, when they have been there.
    #[must_use]
    pub fn place(&self, document: &str) -> Option<ReadingPlace> {
        self.places.get(document).map(Place::reading_place)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Replica;

    /// The place kept for a document is the one given back for it, on another replica too, and
    /// other documents keep their own.
    #[test]
    fn a_kept_place_comes_back_for_its_document_on_another_device() {
        let mut phone = Replica::<ReadingPositions>::new();
        phone
            .apply(ReadingPositions::KEEP_PLACE.with(("article-1".to_owned(), 12, 0.25, 0.4)))
            .expect("kept");
        phone
            .apply(ReadingPositions::KEEP_PLACE.with(("article-2".to_owned(), 3, 0.0, 0.1)))
            .expect("kept");
        let mut laptop = Replica::<ReadingPositions>::new();
        laptop.sync_with(&mut phone).expect("synced");
        let state = laptop.state().expect("state");
        let place = state.place("article-1").expect("a place");
        assert_eq!(place.block, 12);
        assert!((place.fraction - 0.25).abs() < 1e-6);
        assert_eq!(state.place("article-2").map(|p| p.block), Some(3));
        assert!(state.place("article-3").is_none());
        assert!(
            phone
                .apply(ReadingPositions::KEEP_PLACE.with(("".to_owned(), 0, 0.0, 0.0)))
                .is_err()
        );
    }
}
