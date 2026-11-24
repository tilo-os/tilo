//! A picture's QR code, read offline: the still image a person chose as a screenshot or photo,
//! decoded to its `otpauth://` link by the pure-Rust `rqrr` detector.

/// The payload of the first QR code in `encoded`, or a short reason none read.
///
/// # Errors
///
/// The bytes are no image, or the image holds no QR code the detector can read.
pub fn decode_image_bytes(encoded: &[u8]) -> Result<String, String> {
    let image = tilo_image_codec::DecodeImage {
        encoded,
        max_dimension: 4096,
    }
    .decode_native()
    .map_err(|error| format!("Couldn’t open the picture: {error:?}"))?;
    let luma = luma_of(image.width, image.height, &image.rgba);
    decode_luma(image.width, image.height, &luma)
}

/// The greyscale of an RGBA image, as the detector reads it.
fn luma_of(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0)),
    );
    for pixel in rgba.chunks_exact(4) {
        let (r, g, b) = (
            u32::from(pixel.first().copied().unwrap_or(0)),
            u32::from(pixel.get(1).copied().unwrap_or(0)),
            u32::from(pixel.get(2).copied().unwrap_or(0)),
        );
        let weighted = r
            .saturating_mul(299)
            .saturating_add(g.saturating_mul(587))
            .saturating_add(b.saturating_mul(114));
        let grey = u8::try_from(weighted / 1000).unwrap_or(u8::MAX);
        out.push(grey);
    }
    out
}

/// The payload of the first QR code in a greyscale image.
///
/// # Errors
///
/// The image holds no QR code, or its payload reads as nothing.
pub fn decode_luma(width: u32, height: u32, luma: &[u8]) -> Result<String, String> {
    let width = usize::try_from(width).unwrap_or(0);
    let height = usize::try_from(height).unwrap_or(0);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
        y.checked_mul(width)
            .and_then(|row| row.checked_add(x))
            .and_then(|at| luma.get(at))
            .copied()
            .unwrap_or(u8::MAX)
    });
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("No QR code found in that picture".to_string());
    }
    for grid in &grids {
        if let Ok((_meta, content)) = grid.decode()
            && !content.trim().is_empty()
        {
            return Ok(content);
        }
    }
    Err("Found a QR code but couldn’t read it — try a sharper picture".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QR code of `data`, rasterised to a greyscale image with clean finder patterns.
    fn render(data: &str) -> (u32, u32, Vec<u8>) {
        use qrcode::{Color, EcLevel, QrCode};
        const SCALE: usize = 6;
        const QUIET: usize = 4;
        let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).unwrap();
        let modules = code.to_colors();
        let margin = vec![Color::Light; QUIET];
        let rows: Vec<Vec<Color>> = modules
            .chunks(code.width())
            .map(|row| margin.iter().chain(row).chain(&margin).copied().collect())
            .collect();
        let across = rows.first().unwrap().len();
        let quiet_row = vec![Color::Light; across];
        let out: Vec<u8> = std::iter::repeat_n(&quiet_row, QUIET)
            .chain(&rows)
            .chain(std::iter::repeat_n(&quiet_row, QUIET))
            .flat_map(|row| {
                let line: Vec<u8> = row
                    .iter()
                    .flat_map(|module| {
                        std::iter::repeat_n(if *module == Color::Dark { 0 } else { 255 }, SCALE)
                    })
                    .collect();
                std::iter::repeat_n(line, SCALE).flatten()
            })
            .collect();
        let side = u32::try_from(across.checked_mul(SCALE).unwrap()).unwrap();
        (side, side, out)
    }

    #[test]
    fn a_rendered_setup_link_reads_back_and_a_blank_picture_says_so() {
        let uri = "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let (width, height, luma) = render(uri);
        assert_eq!(decode_luma(width, height, &luma).unwrap(), uri);
        let blank = vec![255u8; 128 * 128];
        let error = decode_luma(128, 128, &blank).unwrap_err();
        assert!(error.contains("No QR code"), "{error}");
    }
}
