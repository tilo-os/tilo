//! Presentations: a presentation file read as the deck it holds, and a deck written as a file.
//! The engine is `tilo-deck`, which draws nothing.
//!
//! A deck crosses this boundary as JSON, the shape its resources have: its title, its palette and
//! look, and each slide's layout, background, speaker notes and elements in stacking order. A
//! text box's words are paragraphs with their inline marks, as the workspace stores them. Pictures
//! cross as content: an import keeps each one and writes its address in the deck, and an export
//! reads each address back into the file.

use crate::blob::{BlobHost, FileValue};
use crate::core::Core;

use tilo_deck::Pictures;

/// The most a presentation file may hold: an import reads it whole.
const MAX_BYTES: i64 = 64 * 1024 * 1024;

/// The prefix of the address a picture's content reads at.
const BLOB: &str = "tilo-blob://";

impl Core {
    /// The deck a presentation file holds, as text the app keeps.
    pub(crate) fn presentation_import(&self, file: &FileValue) -> Result<String, String> {
        let borrowed = self.blobs.borrow();
        let Some(blobs) = borrowed.as_ref() else {
            return Err("this host holds no content store".to_owned());
        };
        let bytes = read(blobs.as_ref(), file)?;
        let pictures = HostPictures {
            blobs: blobs.as_ref(),
        };
        let deck = tilo_deck::import(&bytes, &file.name, &pictures)?;
        serde_json::to_string(&deck).map_err(|why| why.to_string())
    }

    /// The bytes of a deck's presentation file, of the format named.
    pub(crate) fn presentation_export(&self, deck: &str, format: &str) -> Result<Vec<u8>, String> {
        let deck: tilo_deck::Deck = serde_json::from_str(deck)
            .map_err(|why| format!("the deck is not JSON: {why}"))?;
        let format = match format.trim().to_ascii_lowercase().as_str() {
            "odp" => tilo_deck::Format::Odp,
            "pptx" => tilo_deck::Format::Pptx,
            other => return Err(format!("`{other}` is not a presentation format: odp or pptx")),
        };
        let borrowed = self.blobs.borrow();
        let pictures = borrowed.as_ref().map(|blobs| HostPictures {
            blobs: blobs.as_ref(),
        });
        let pictures: &dyn Pictures = match &pictures {
            Some(pictures) => pictures,
            None => &NoPictures,
        };
        tilo_deck::export(&deck, pictures, format)
    }

}

/// Bridges the workspace's content store to the engine's pictures.
struct HostPictures<'a> {
    blobs: &'a dyn BlobHost,
}

impl Pictures for HostPictures<'_> {
    fn read(&self, src: &str) -> Option<Vec<u8>> {
        let hash = src.strip_prefix(BLOB).unwrap_or(src);
        self.blobs.read(hash)
    }

    fn keep(&self, bytes: &[u8]) -> Option<String> {
        let media = tilo_content_types::image_format(bytes)?.mime;
        self.blobs.keep("picture", media, bytes).ok().map(|file| file.url())
    }
}

/// A host with no content store: a deck's pictures cannot be read or kept, and the deck still
/// crosses whole.
struct NoPictures;

impl Pictures for NoPictures {
    fn read(&self, _src: &str) -> Option<Vec<u8>> {
        None
    }

    fn keep(&self, _bytes: &[u8]) -> Option<String> {
        None
    }
}

/// The bytes a file holds, read whole.
fn read(blobs: &dyn BlobHost, file: &FileValue) -> Result<Vec<u8>, String> {
    if file.size > MAX_BYTES {
        return Err(format!("`{}` is too large to open here", file.name));
    }
    blobs
        .read(&file.hash)
        .ok_or_else(|| format!("the content of `{}` is not on this device", file.name))
}
