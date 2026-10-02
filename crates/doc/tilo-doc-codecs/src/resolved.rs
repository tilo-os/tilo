//! A snapshot together with the binary content its images reference, fetched once up front so an
//! exporter's recursive writers can embed it without threading a content source through each call.

use std::collections::BTreeMap;
use std::ops::Deref;

use tilo_content::ContentSource;
use tilo_doc::DocSnapshot;

/// A [`DocSnapshot`] plus the bytes of every image it references that `content` could provide.
/// Dereferences to the snapshot.
pub(crate) struct Resolved<'a> {
    snap: &'a DocSnapshot,
    images: BTreeMap<String, Vec<u8>>,
}

impl<'a> Resolved<'a> {
    pub(crate) fn new(snap: &'a DocSnapshot, content: &dyn ContentSource) -> Self {
        let images = snap
            .image_content()
            .into_iter()
            .filter_map(|hash| {
                let bytes = content.content(&hash)?.into_owned();
                Some((hash, bytes))
            })
            .collect();
        Self { snap, images }
    }

    /// The bytes of the image at `hash`, if they were available.
    pub(crate) fn image(&self, hash: &str) -> Option<&[u8]> {
        self.images.get(hash).map(Vec::as_slice)
    }

    /// A different snapshot sharing this one's resolved images.
    #[cfg_attr(
        not(any(feature = "epub", feature = "odt")),
        allow(dead_code, reason = "used by the epub and odt exporters")
    )]
    pub(crate) fn with_snapshot<'b>(&self, snap: &'b DocSnapshot) -> Resolved<'b> {
        Resolved {
            snap,
            images: self.images.clone(),
        }
    }
}

impl Deref for Resolved<'_> {
    type Target = DocSnapshot;

    fn deref(&self) -> &DocSnapshot {
        self.snap
    }
}
