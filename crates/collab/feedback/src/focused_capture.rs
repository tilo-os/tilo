//! One opt-in readback of the current app frame. No desktop or timer capture.
use crate::capture::{MAX_CAPTURE_REGIONS, check_masking_budget};
use crate::{CapturePermit, Error, MaskedScreenshot, PixelRect, ReviewText, SurfaceCapture};

/// Classification supplied by the renderer for every visible text region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintClass {
    Public,
    Sensitive,
    /// Text without an explicit classification is always masked.
    Unclassified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintRegion {
    pub bounds: PixelRect,
    pub class: PaintClass,
}

/// The renderer's inspection of the exact presented focused app frame.
/// `None` means incomplete geometry, including a cached subtree that did not
/// retain classification. It must never be replaced by an empty region list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureInspection {
    pub surface: String,
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    /// Includes the host's app policy and secret-paint protection.
    pub protected: bool,
    pub regions: Option<Vec<PaintRegion>>,
}

impl CaptureInspection {
    fn sensitive_regions(&self) -> Result<Vec<PixelRect>, Error> {
        let pixels = u64::from(self.width).saturating_mul(u64::from(self.height));
        if self.protected || pixels == 0 || pixels > 4_000_000 {
            return Err(Error::CaptureProtected);
        }
        let regions = self.regions.as_ref().ok_or(Error::CaptureProtected)?;
        if regions.len() > MAX_CAPTURE_REGIONS {
            return Err(Error::CaptureProtected);
        }
        let mut sensitive = Vec::new();
        for region in regions {
            let rect = region.bounds;
            if rect.width == 0
                || rect.height == 0
                || rect
                    .x
                    .checked_add(rect.width)
                    .is_none_or(|right| right > self.width)
                || rect
                    .y
                    .checked_add(rect.height)
                    .is_none_or(|bottom| bottom > self.height)
            {
                return Err(Error::CaptureProtected);
            }
            if region.class != PaintClass::Public {
                sensitive.push(rect);
            }
        }
        check_masking_budget(&sensitive)?;
        Ok(sensitive)
    }
}

/// Supplied by the existing host renderer, never an app-selected screenshot
/// URL or arbitrary desktop readback. Inspection and pixels name the same
/// presented frame, including cached classification and physical coordinates.
pub trait FocusedSurface {
    /// # Errors
    /// Refuses absent focus or an unavailable presented-frame inspection.
    fn inspect(&mut self) -> Result<CaptureInspection, Error>;
    /// Read only the inspected app surface. Do not substitute a newer frame.
    /// # Errors
    /// Refuses changed focus, frame, bounds or unavailable app readback.
    fn read_rgba(&mut self, frame: &CaptureInspection) -> Result<Vec<u8>, Error>;
    /// # Errors
    /// Refuses context that has not been classified for opt-in review.
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        Err(Error::CaptureProtected)
    }
}

/// Called only by the existing explicit screenshot action. Validate privacy
/// before acquiring pixels, recheck the entire inspection after readback, then
/// consume one permit and mask before any preview, encoding or persistence.
/// # Errors
/// Refuses secret/protected apps, incomplete or invalid geometry, focus/frame
/// changes, failed readback and malformed pixel buffers.
pub fn capture_focused(source: &mut dyn FocusedSurface) -> Result<MaskedScreenshot, Error> {
    let frame = source.inspect()?;
    let regions = frame.sensitive_regions()?;
    let permit = CapturePermit::requested(frame.surface.clone(), frame.revision)?;
    let rgba = source.read_rgba(&frame)?;
    if source.inspect()? != frame {
        return Err(Error::CaptureChanged);
    }
    permit.mask(SurfaceCapture {
        surface: frame.surface,
        revision: frame.revision,
        width: frame.width,
        height: frame.height,
        rgba,
        capture_protected: frame.protected,
        sensitive_regions: Some(regions),
    })
}

#[cfg(test)]
mod tests;
