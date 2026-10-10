//! Masked PNG custody through the existing content store, never a document blob.
use std::io::{Cursor, Read};

use image::ImageDecoder as _;
use tilo_content::{ContentDurability, ContentStore};

use crate::{Error, MaskedScreenshot, ScreenshotRef};

const MAX_PNG_BYTES: u64 = 16_100_000;

/// A host supplies its existing store. Memory, browser cache and regenerable
/// backends cannot attest durable custody. This handle creates no transport or
/// permission, and does not make a reporter's store the destination's store.
#[derive(Clone)]
pub struct ReviewedAttachments {
    content: ContentStore,
}

impl ReviewedAttachments {
    pub fn new(content: ContentStore) -> Self {
        Self { content }
    }

    /// Persist only the masked pixels produced by an explicit capture permit.
    /// This local write does not upload or acknowledge a report.
    ///
    /// # Errors
    /// Refuses failed writes, non-durable storage or changed content.
    pub fn persist(&self, image: &MaskedScreenshot) -> Result<(), Error> {
        let hash = self
            .content
            .put(image.png())
            .map_err(|_| Error::NotDurable)?;
        if hash != image.reference().hash {
            return Err(Error::ReceiptMismatch);
        }
        self.verify(image.reference())
    }

    /// Verify the current destination's exact PNG, dimensions and durable bytes.
    /// Report metadata or the reporter's local copy cannot satisfy this check.
    ///
    /// # Errors
    /// Refuses missing, malformed, oversized, mismatched or uncommitted bytes.
    pub fn verify(&self, reference: &ScreenshotRef) -> Result<(), Error> {
        if reference.width == 0
            || reference.height == 0
            || u64::from(reference.width).saturating_mul(u64::from(reference.height)) > 4_000_000
        {
            return Err(Error::ReceiptMismatch);
        }
        let reader = self
            .content
            .open(&reference.hash)
            .ok_or(Error::NotDurable)?;
        let mut png = Vec::new();
        reader
            .take(MAX_PNG_BYTES + 1)
            .read_to_end(&mut png)
            .map_err(|_| Error::NotDurable)?;
        if u64::try_from(png.len()).unwrap_or(u64::MAX) > MAX_PNG_BYTES
            || blake3::hash(&png).to_hex().as_str() != reference.hash
        {
            return Err(Error::ReceiptMismatch);
        }
        let decoder = image::codecs::png::PngDecoder::new(Cursor::new(png))?;
        if decoder.dimensions() != (reference.width, reference.height) {
            return Err(Error::ReceiptMismatch);
        }
        // Decode the bounded image too: a valid header alone is not an image.
        image::DynamicImage::from_decoder(decoder)?;
        if self
            .content
            .durability(&reference.hash)
            .map_err(|_| Error::NotDurable)?
            != ContentDurability::Durable
        {
            return Err(Error::NotDurable);
        }
        Ok(())
    }
}
