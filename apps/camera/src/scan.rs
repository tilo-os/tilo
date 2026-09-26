//! QR encode + decode glue.
//!
//! * [`encode`] turns a payload string into a square module matrix — used to
//!   synthesize the sample scanner poster so the offline scanner has a real,
//!   decodable code to work against.
//! * [`decode_greyscale`] runs the real [`rqrr`] detector over a greyscale
//!   buffer and returns every code it finds with its decoded text and the
//!   four corner points. The scanner UI draws its bounding box from these
//!   corners — they come from an actual decode, not a hand-placed overlay.

use crate::model::{ScannedCode, classify};

/// A square QR module matrix. `modules[y * size + x]` is `true` for a dark
/// module.
#[derive(Clone, Debug)]
pub struct QrMatrix {
    pub size: usize,
    pub modules: Vec<bool>,
}

impl QrMatrix {
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.size
            && y < self.size
            && self
                .modules
                .get(y * self.size + x)
                .copied()
                .unwrap_or(false)
    }
}

/// Encode `payload` into a QR module matrix. Returns `None` if the payload
/// is too large for any QR version.
pub fn encode(payload: &str) -> Option<QrMatrix> {
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;
    let size = code.width();
    let colors = code.to_colors();
    let modules = colors
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Some(QrMatrix { size, modules })
}

/// One decoded code with the geometry the detector reported.
#[derive(Clone, Debug)]
pub struct Decoded {
    pub text: String,
    /// Four corners, clockwise from top-left, in buffer pixel coordinates.
    pub corners: [(f32, f32); 4],
}

/// Run the real QR detector over a greyscale buffer (`w * h` bytes,
/// row-major, 0 = black). Returns every decodable code found.
pub fn decode_greyscale(luma: &[u8], w: usize, h: usize) -> Vec<Decoded> {
    if luma.len() < w * h || w == 0 || h == 0 {
        return Vec::new();
    }
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
        luma.get(y * w + x).copied().unwrap_or(0)
    });
    let mut out = Vec::new();
    for grid in img.detect_grids() {
        if let Ok((_meta, text)) = grid.decode() {
            let c = grid.bounds;
            out.push(Decoded {
                text,
                corners: [
                    (c[0].x as f32, c[0].y as f32),
                    (c[1].x as f32, c[1].y as f32),
                    (c[2].x as f32, c[2].y as f32),
                    (c[3].x as f32, c[3].y as f32),
                ],
            });
        }
    }
    out
}

/// Decode and classify in one step, mapping buffer-space corners through
/// `map` into the caller's coordinate space (e.g. viewfinder pixels).
pub fn decode_and_classify(
    luma: &[u8],
    w: usize,
    h: usize,
    mut map: impl FnMut((f32, f32)) -> (f32, f32),
) -> Option<ScannedCode> {
    let decoded = decode_greyscale(luma, w, h).into_iter().next()?;
    let kind = classify(&decoded.text);
    let mut corners = [(0.0, 0.0); 4];
    for (dst, p) in corners.iter_mut().zip(decoded.corners.iter()) {
        *dst = map(*p);
    }
    Some(ScannedCode {
        raw: decoded.text,
        kind,
        symbology: "QR Code".to_string(),
        corners,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CodeKind;

    /// Rasterize a [`QrMatrix`] into a greyscale buffer with a quiet zone
    /// so the detector has the margin it expects.
    fn rasterize(m: &QrMatrix, scale: usize, quiet: usize) -> (Vec<u8>, usize) {
        let dim = (m.size + quiet * 2) * scale;
        let mut buf = vec![255u8; dim * dim];
        for my in 0..m.size {
            for mx in 0..m.size {
                if !m.is_dark(mx, my) {
                    continue;
                }
                let px0 = (mx + quiet) * scale;
                let py0 = (my + quiet) * scale;
                for dy in 0..scale {
                    for dx in 0..scale {
                        buf[(py0 + dy) * dim + (px0 + dx)] = 0;
                    }
                }
            }
        }
        (buf, dim)
    }

    #[test]
    fn qr_decoding_round_trips_classifies_and_ignores_blank_buffers() {
        // Phase `encode_then_decode_round_trips`.
        {
            let payload = "https://example.test/welcome";
            let m = encode(payload).expect("encode");
            let (buf, dim) = rasterize(&m, 6, 4);
            let decoded = decode_greyscale(&buf, dim, dim);
            assert_eq!(decoded.len(), 1, "exactly one code detected");
            assert_eq!(decoded[0].text, payload);
            // Corners are inside the rasterized image.
            for (x, y) in decoded[0].corners {
                assert!(x >= 0.0 && x <= dim as f32);
                assert!(y >= 0.0 && y <= dim as f32);
            }
        }
        // Phase `decode_wifi_classifies`.
        {
            let payload = "WIFI:T:WPA2;S:Tilo Guest;P:hunter2;H:false;;";
            let m = encode(payload).unwrap();
            let (buf, dim) = rasterize(&m, 6, 4);
            let scanned = decode_and_classify(&buf, dim, dim, |p| p).expect("decode");
            assert!(matches!(scanned.kind, CodeKind::Wifi { .. }));
            assert_eq!(scanned.symbology, "QR Code");
        }
        // Phase `empty_buffer_decodes_nothing`.
        {
            let buf = vec![255u8; 64 * 64];
            assert!(decode_greyscale(&buf, 64, 64).is_empty());
        }
    }
}
