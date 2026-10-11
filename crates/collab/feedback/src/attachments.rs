//! Masked PNG custody through the existing content store, never a document blob.
use std::io::{Cursor, Read};

use image::ImageDecoder as _;
use tilo_content::{ContentDurability, ContentStore};

use crate::{Error, MaskedScreenshot, ScreenshotRef};

const MAX_PNG_BYTES: u64 = 16_100_000;

pub type AttachmentFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + 'a>>;

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

    /// Browser hosts await current per-address storage custody rather than
    /// promoting their synchronous mirror. No transfer or receipt is created.
    /// # Errors
    /// Refuses failed writes, corrupt media and absent/unqualified custody.
    pub fn persist_async<'a>(&'a self, image: &'a MaskedScreenshot) -> AttachmentFuture<'a> {
        Box::pin(async move {
            let stored = self
                .content
                .put_async(image.png())
                .await
                .map_err(|_| Error::NotDurable)?;
            if stored.hash != image.reference().hash {
                return Err(Error::ReceiptMismatch);
            }
            self.verify_async(image.reference()).await
        })
    }

    /// Verify the current destination's exact PNG, dimensions and durable bytes.
    /// Report metadata or the reporter's local copy cannot satisfy this check.
    ///
    /// # Errors
    /// Refuses missing, malformed, oversized, mismatched or uncommitted bytes.
    pub fn verify(&self, reference: &ScreenshotRef) -> Result<(), Error> {
        self.verify_image(reference)?;
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

    /// Fresh asynchronous owner custody for this exact reviewed image.
    /// # Errors
    /// Refuses invalid pixels and failed or unqualified persisted readback.
    pub fn verify_async<'a>(&'a self, reference: &'a ScreenshotRef) -> AttachmentFuture<'a> {
        Box::pin(async move {
            self.verify_image(reference)?;
            if self
                .content
                .durability_async(&reference.hash)
                .await
                .map_err(|_| Error::NotDurable)?
                != ContentDurability::Durable
            {
                return Err(Error::NotDurable);
            }
            Ok(())
        })
    }

    fn verify_image(&self, reference: &ScreenshotRef) -> Result<(), Error> {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapturePermit, SurfaceCapture};

    fn ready(future: AttachmentFuture<'_>) -> std::task::Poll<Result<(), Error>> {
        let mut future = future;
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        future.as_mut().poll(&mut context)
    }

    #[test]
    fn async_attachment_custody_cannot_promote_present_memory() {
        let content = ContentStore::memory();
        let image = CapturePermit::requested("synthetic:app".into(), 1)
            .unwrap()
            .mask(SurfaceCapture {
                surface: "synthetic:app".into(),
                revision: 1,
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
                capture_protected: false,
                sensitive_regions: Some(vec![]),
            })
            .unwrap();
        let attachments = ReviewedAttachments::new(content.clone());
        assert!(matches!(
            ready(attachments.persist_async(&image)),
            std::task::Poll::Ready(Err(Error::NotDurable))
        ));
        assert_eq!(
            content.read(&image.reference().hash).as_deref(),
            Some(image.png())
        );
        assert!(matches!(
            ready(attachments.verify_async(image.reference())),
            std::task::Poll::Ready(Err(Error::NotDurable))
        ));
    }
}
