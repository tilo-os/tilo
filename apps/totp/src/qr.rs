//! QR-code ingestion for the Add-account flow.
//!
//! Two ways an authenticator QR ever reaches us, mirrored here as the two
//! halves of this module:
//!
//!  * **From a still image** — a screenshot of the enrolment page, or a
//!    photo of a printed code, saved as a file. This is decoded **today**,
//!    fully offline, with the pure-Rust [`rqrr`] detector (built on the
//!    `quirc` algorithm) over an [`image`]-decoded greyscale buffer. No
//!    camera, no native libraries.
//!
//!  * **From the live camera** — the user points a webcam at the code. The
//!    OS does not yet expose a shared camera-capture pipeline (the camera
//!    app owns its own capture and there is no toolkit-level surface to
//!    borrow frames from), so [`scan_with_camera`] is a clean placeholder
//!    that reports *why* it can't run yet rather than silently failing.
//!    When the capture layer lands, the per-frame greyscale buffer flows
//!    straight into [`decode_luma`] — the decode half is already done.
//!
//! Both paths converge on a single output: the decoded payload string,
//! which the panel hands to [`crate::model::parse_otpauth_uri`]. This
//! module never touches the UI or the vault.

use std::path::Path;

/// Why a live-camera scan could not start. Distinct from a decode failure:
/// the capture pipeline itself is unavailable, which is a platform gap, not
/// user error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraUnavailable {
    /// No shared camera-capture surface exists in the toolkit yet.
    NoCapturePipeline,
}

impl CameraUnavailable {
    /// A calm, user-facing one-liner explaining the current limitation.
    pub fn message(&self) -> &'static str {
        match self {
            CameraUnavailable::NoCapturePipeline => {
                "Live camera scanning needs camera access, which isn\u{2019}t \
                 wired up yet. For now, import a screenshot of the QR code."
            }
        }
    }
}

/// Start a live-camera QR scan.
///
/// TODO(camera): wire this to the shared camera-capture pipeline once the
/// toolkit exposes one. The intended shape, so the wiring is unambiguous
/// when the capture surface exists:
///
/// 1. Request a capture session from the compositor over the same
///    Unix-socket capability channel used for file-access / collaboration
///    prompts — the cluster sandbox gates camera access exactly like any
///    other device, so the user sees a permission prompt.
/// 2. Stream frames as `image::GrayImage` (or a raw luma plane we wrap),
///    one per redraw, into [`decode_luma`].
/// 3. The first frame that decodes to a payload ends the session and is
///    returned to the panel, which parses it like any other otpauth URI.
///
/// Until that capture surface lands this returns
/// [`CameraUnavailable::NoCapturePipeline`] so the UI can show *why*.
pub fn scan_with_camera() -> Result<String, CameraUnavailable> {
    Err(CameraUnavailable::NoCapturePipeline)
}

/// Decode the first QR code found in an image file (PNG / JPEG / GIF / …).
///
/// Returns the decoded payload string — for an authenticator export this is
/// the `otpauth://` URI. `Err` carries a short, user-facing reason: the file
/// couldn't be read/decoded as an image, or no QR code was found in it.
pub fn decode_image_file(path: &Path) -> Result<String, String> {
    let img = image::open(path).map_err(|e| format!("Couldn\u{2019}t open image: {e}"))?;
    decode_luma(img.to_luma8())
}

/// Decode the first QR code in an already-greyscale buffer.
///
/// This is the shared core of both ingestion paths: the file importer feeds
/// it a decoded screenshot today, and the future camera path will feed it
/// one webcam frame at a time. Returns the payload string, or a short reason
/// when no readable QR grid is present.
pub fn decode_luma(luma: image::GrayImage) -> Result<String, String> {
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("No QR code found in that image".to_string());
    }
    for grid in &grids {
        if let Ok((_meta, content)) = grid.decode() {
            if !content.trim().is_empty() {
                return Ok(content);
            }
        }
    }
    Err("Found a QR code but couldn\u{2019}t read it \u{2014} try a sharper image".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};

    /// Render a known otpauth URI into a clean, high-resolution QR bitmap
    /// with the `qrcode` crate, then prove our decoder reads it back
    /// verbatim. This exercises the real `rqrr` detect+decode path end to
    /// end on the exact buffer type the file importer produces.
    #[test]
    fn round_trips_an_otpauth_qr_through_the_decoder() {
        let uri = "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme&algorithm=SHA256&digits=8&period=45";
        let luma = render_qr_luma(uri);
        let decoded = decode_luma(luma).expect("decoder reads the rendered QR");
        assert_eq!(decoded, uri);
    }

    #[test]
    fn empty_image_reports_no_qr_found() {
        // A blank white field has no finder patterns to detect.
        let blank = GrayImage::from_pixel(128, 128, Luma([255]));
        let err = decode_luma(blank).unwrap_err();
        assert!(err.contains("No QR code"), "got: {err}");
    }

    #[test]
    fn camera_path_reports_unavailable_with_a_reason() {
        let err = scan_with_camera().unwrap_err();
        assert_eq!(err, CameraUnavailable::NoCapturePipeline);
        assert!(!err.message().is_empty());
    }

    /// Encode `data` as a QR code and rasterise it to a greyscale image,
    /// scaling each module up so `rqrr`'s detector has clean finder
    /// patterns to lock onto.
    fn render_qr_luma(data: &str) -> GrayImage {
        use qrcode::{EcLevel, QrCode};
        let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).unwrap();
        let modules = code.to_colors();
        let dim = (modules.len() as f64).sqrt() as usize;
        let scale = 6usize;
        let quiet = 4usize; // quiet-zone modules on every side
        let side = (dim + quiet * 2) * scale;
        let mut img = GrayImage::from_pixel(side as u32, side as u32, Luma([255]));
        for y in 0..dim {
            for x in 0..dim {
                if modules[y * dim + x] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = ((x + quiet) * scale + dx) as u32;
                            let py = ((y + quiet) * scale + dy) as u32;
                            img.put_pixel(px, py, Luma([0]));
                        }
                    }
                }
            }
        }
        img
    }
}
