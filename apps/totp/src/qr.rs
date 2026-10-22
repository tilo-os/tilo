//! QR-code ingestion for the Add-account flow.
//!
//! An authenticator QR reaches us **from a still image** — a screenshot of
//! the enrolment page, or a photo of a printed code, saved as a file. It is
//! decoded fully offline with the pure-Rust [`rqrr`] detector (built on the
//! `quirc` algorithm) over an [`image`]-decoded greyscale buffer. No camera,
//! no native libraries.
//!
//! [`decode_image_bytes`] decodes the chosen image's bytes and hands its
//! greyscale buffer to the shared [`decode_luma`] core. The output is a single decoded payload string,
//! which the panel hands to [`crate::model::parse_otpauth_uri`]. This module
//! never touches the UI or the vault.

/// Decode the first QR code in an encoded image's bytes (PNG, JPEG, GIF, ...),
/// as a chooser delivers them on every host, a browser page included.
///
/// Returns the decoded payload string — for an authenticator export this is
/// the `otpauth://` URI.
///
/// # Errors
/// Returns `Err` with a short, user-facing reason when the bytes can't be
/// decoded as an image, when no QR grid is found in it, or when a grid is
/// found but its content can't be decoded.
pub fn decode_image_bytes(bytes: &[u8]) -> Result<String, String> {
    let img =
        image::load_from_memory(bytes).map_err(|e| format!("Couldn\u{2019}t open image: {e}"))?;
    decode_luma(img.to_luma8())
}

/// Decode the first QR code in an already-greyscale buffer.
///
/// The shared core of the ingestion path: the file importer feeds it a
/// decoded screenshot. Returns the payload string, or a short reason when no
/// readable QR grid is present.
pub fn decode_luma(luma: image::GrayImage) -> Result<String, String> {
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("No QR code found in that image".to_string());
    }
    for grid in &grids {
        if let Ok((_meta, content)) = grid.decode()
            && !content.trim().is_empty()
        {
            return Ok(content);
        }
    }
    Err("Found a QR code but couldn\u{2019}t read it \u{2014} try a sharper image".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};

    /// The QR decoder reads a rendered otpauth URI back and reports images without a code.
    #[test]
    fn qr_decoder_round_trips_otpauth_and_reports_empty_images() {
        // Case `round_trips_an_otpauth_qr_through_the_decoder`.
        // Render a known otpauth URI into a clean, high-resolution QR bitmap
        // with the `qrcode` crate, then prove our decoder reads it back
        // verbatim. This exercises the real `rqrr` detect+decode path end to
        // end on the exact buffer type the file importer produces.
        {
            let uri = "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme&algorithm=SHA256&digits=8&period=45";
            let luma = render_qr_luma(uri);
            let decoded = decode_luma(luma).expect("decoder reads the rendered QR");
            assert_eq!(decoded, uri);
        }

        {
            // A blank white field has no finder patterns to detect.
            let blank = GrayImage::from_pixel(128, 128, Luma([255]));
            let err = decode_luma(blank).unwrap_err();
            assert!(err.contains("No QR code"), "got: {err}");
        }
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
