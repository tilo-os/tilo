//! QR-code ingestion: a still image (a screenshot or a photo) decoded offline
//! into its `otpauth://` payload by the pure-Rust `rqrr` detector.

/// The payload of the first QR code in an image, or a short reason none decoded.
pub async fn decode_image_bytes(bytes: Vec<u8>) -> Result<String, String> {
    let image = ui::image_codec::decode(&bytes, None)
        .await
        .map_err(|e| format!("Couldn\u{2019}t open image: {e}"))?;
    decode_luma(image.width, image.height, &image.luma())
}

pub fn decode_luma(width: u32, height: u32, luma: &[u8]) -> Result<String, String> {
    let width = width as usize;
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, height as usize, |x, y| {
            y.checked_mul(width)
                .and_then(|row| row.checked_add(x))
                .and_then(|at| luma.get(at))
                .copied()
                .unwrap_or(u8::MAX)
        });
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

    #[test]
    fn qr_decoder_round_trips_otpauth_and_reports_empty_images() {
        {
            let uri = "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme&algorithm=SHA256&digits=8&period=45";
            let luma = render_qr_luma(uri);
            let decoded = decode_luma(luma.width(), luma.height(), luma.as_raw())
                .expect("decoder reads the rendered QR");
            assert_eq!(decoded, uri);
        }

        {
            // A blank white field has no finder patterns to detect.
            let blank = GrayImage::from_pixel(128, 128, Luma([255]));
            let err = decode_luma(blank.width(), blank.height(), blank.as_raw()).unwrap_err();
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
