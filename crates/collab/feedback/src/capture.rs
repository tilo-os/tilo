use image::ImageEncoder as _;

use crate::{Error, ScreenshotRef};

pub(crate) const MAX_CAPTURE_REGIONS: usize = 8192;
const MAX_MASKING_WORK: u64 = 8_000_000;

pub(crate) fn check_masking_budget(regions: &[PixelRect]) -> Result<(), Error> {
    if regions.len() > MAX_CAPTURE_REGIONS {
        return Err(Error::CaptureProtected);
    }
    regions.iter().try_fold(0_u64, |work, rect| {
        work.checked_add(u64::from(rect.width).saturating_mul(u64::from(rect.height)))
            .filter(|total| *total <= MAX_MASKING_WORK)
            .ok_or(Error::CaptureProtected)
    })?;
    Ok(())
}

/// One explicit user request, bound to the focused app and its inspected frame.
/// A permit is consumed once; no ambient buffer or periodic capture is enabled.
pub struct CapturePermit {
    surface: String,
    revision: u64,
}

/// Sensitive bounds in the captured surface's physical pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Produced only by the host's app-surface capture adapter, never a whole-desktop
/// snapshot. The adapter marks all unclassified text sensitive and refuses
/// incomplete geometry; protected apps are excluded even with user consent.
pub struct SurfaceCapture {
    pub surface: String,
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub capture_protected: bool,
    pub sensitive_regions: Option<Vec<PixelRect>>,
}

/// Only masked PNG bytes are offered to the preview and content store.
#[derive(Clone)]
pub struct MaskedScreenshot {
    reference: ScreenshotRef,
    png: std::sync::Arc<[u8]>,
}

impl CapturePermit {
    /// Called by the explicit screenshot action, never opening the composer.
    ///
    /// # Errors
    /// Refuses empty/oversized surface identity.
    pub fn requested(surface: String, revision: u64) -> Result<Self, Error> {
        if surface.is_empty() || surface.len() > 256 || surface.chars().any(char::is_control) {
            return Err(Error::Invalid("capture surface"));
        }
        Ok(Self { surface, revision })
    }

    /// Consume this request and mask before encoding or persisting any pixels.
    ///
    /// # Errors
    /// Refuses changed/protected surfaces, unknown or invalid sensitive bounds,
    /// oversized/malformed pixels, or PNG encoding failure. Never falls back to
    /// an unmasked frame when inspection fails.
    pub fn mask(self, mut capture: SurfaceCapture) -> Result<MaskedScreenshot, Error> {
        if capture.surface != self.surface || capture.revision != self.revision {
            return Err(Error::CaptureChanged);
        }
        if capture.capture_protected {
            return Err(Error::CaptureProtected);
        }
        let regions = capture
            .sensitive_regions
            .take()
            .ok_or(Error::CaptureProtected)?;
        let pixels = u64::from(capture.width).saturating_mul(u64::from(capture.height));
        if pixels == 0
            || pixels > 4_000_000
            || usize::try_from(pixels.saturating_mul(4)).ok() != Some(capture.rgba.len())
        {
            return Err(Error::Invalid("capture pixels"));
        }
        check_masking_budget(&regions)?;
        for rect in regions {
            let right = rect
                .x
                .checked_add(rect.width)
                .ok_or(Error::CaptureProtected)?;
            let bottom = rect
                .y
                .checked_add(rect.height)
                .ok_or(Error::CaptureProtected)?;
            if rect.width == 0
                || rect.height == 0
                || right > capture.width
                || bottom > capture.height
            {
                return Err(Error::CaptureProtected);
            }
            for y in rect.y..bottom {
                for x in rect.x..right {
                    let offset = u64::from(y)
                        .saturating_mul(u64::from(capture.width))
                        .saturating_add(u64::from(x))
                        .saturating_mul(4);
                    let start = usize::try_from(offset).map_err(|_| Error::CaptureProtected)?;
                    let pixel = capture
                        .rgba
                        .get_mut(start..start.saturating_add(4))
                        .ok_or(Error::CaptureProtected)?;
                    pixel.copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png).write_image(
            &capture.rgba,
            capture.width,
            capture.height,
            image::ExtendedColorType::Rgba8,
        )?;
        Ok(MaskedScreenshot {
            reference: ScreenshotRef {
                hash: blake3::hash(&png).to_hex().to_string(),
                width: capture.width,
                height: capture.height,
            },
            png: png.into(),
        })
    }
}

impl MaskedScreenshot {
    pub fn reference(&self) -> &ScreenshotRef {
        &self.reference
    }
    pub fn png(&self) -> &[u8] {
        &self.png
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> SurfaceCapture {
        SurfaceCapture {
            surface: "app:book".into(),
            revision: 42,
            width: 2,
            height: 1,
            rgba: vec![200, 100, 50, 255, 10, 20, 30, 255],
            capture_protected: false,
            sensitive_regions: Some(vec![PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }]),
        }
    }
    fn permit() -> CapturePermit {
        CapturePermit::requested("app:book".into(), 42).unwrap()
    }

    #[test]
    fn sensitive_pixels_are_masked_in_the_actual_encoded_attachment() {
        let masked = permit().mask(capture()).unwrap();
        let pixels = image::load_from_memory_with_format(masked.png(), image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(pixels.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(1, 0).0, [10, 20, 30, 255]);
        assert_eq!(
            masked.reference().hash,
            blake3::hash(masked.png()).to_hex().to_string()
        );
    }

    #[test]
    fn protected_or_stale_capture_and_missing_redaction_geometry_fail_closed() {
        let mut protected = capture();
        protected.capture_protected = true;
        assert!(matches!(
            permit().mask(protected),
            Err(Error::CaptureProtected)
        ));
        let mut unknown = capture();
        unknown.sensitive_regions = None;
        assert!(matches!(
            permit().mask(unknown),
            Err(Error::CaptureProtected)
        ));
        let mut stale = capture();
        stale.revision = 43;
        assert!(matches!(permit().mask(stale), Err(Error::CaptureChanged)));
    }
}
