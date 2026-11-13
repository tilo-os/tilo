//! A minimal, hand-rolled PDF 1.7 writer: enough of the format to emit text
//! (base-14 Helvetica via `BT`/`ET` + `Tf`/`Td`/`Tj`), vector graphics for table
//! grids and placeholders, and embedded `/DeviceRGB` images with `FlateDecode`.
//!
//! Objects are allocated lazily and serialized with a classic cross-reference
//! table, so the file opens in any conformant viewer.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write as _;

/// One indirect object's serialized body, between `N 0 obj` and `endobj`.
/// Object numbers are 1-based and assigned in `add` order.
pub struct PdfWriter {
    objects: Vec<Vec<u8>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ObjId(pub usize);

impl PdfWriter {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    pub fn reserve(&mut self) -> ObjId {
        self.objects.push(Vec::new());
        ObjId(self.objects.len())
    }

    pub fn add(&mut self, body: Vec<u8>) -> ObjId {
        let id = self.reserve();
        self.set(id, body);
        id
    }

    pub fn set(&mut self, id: ObjId, body: Vec<u8>) {
        /// `id.0` is 1-based and every id this crate makes comes from `reserve`, but
        /// `ObjId`'s field is public, so an out-of-range id is a no-op rather than being
        /// assumed valid.
        if let Some(idx) = id.0.checked_sub(1)
            && let Some(slot) = self.objects.get_mut(idx)
        {
            *slot = body;
        }
    }

    /// Add a stream object: `dict_extra` merged with `/Length`, followed by `data`.
    /// `dict_extra` must hold only the inner entries (e.g. `/Filter /FlateDecode`),
    /// never the enclosing `<<`/`>>`.
    pub fn add_stream(&mut self, dict_extra: &str, data: &[u8]) -> ObjId {
        let mut body = Vec::new();
        let _ = write!(body, "<< /Length {}{} >>\nstream\n", data.len(), dict_extra);
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.add(body)
    }

    pub fn finish(self, root: ObjId) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n");
        // Binary marker comment so tools treat the file as binary.
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        let mut offsets = Vec::with_capacity(self.objects.len());
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            let _ = writeln!(out, "{} 0 obj", i.saturating_add(1));
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }

        let xref_pos = out.len();
        let count = self.objects.len().saturating_add(1); // +1 for the free object 0
        let _ = write!(out, "xref\n0 {count}\n");
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            let _ = writeln!(out, "{off:010} 00000 n ");
        }
        let _ = write!(
            out,
            "trailer\n<< /Size {count} /Root {} 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            root.0
        );
        out
    }
}

pub fn flate(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = enc.write_all(data);
    enc.finish().unwrap_or_default()
}

/// Escape a string for a PDF literal string `( ... )`: backslash and parentheses
/// per the spec, and characters outside `WinAnsiEncoding` as `?` so the content
/// stream stays well-formed (see [`to_winansi`]).
pub fn escape_pdf_text(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len().saturating_add(8));
    for b in to_winansi(s) {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'(' => out.extend_from_slice(b"\\("),
            b')' => out.extend_from_slice(b"\\)"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            _ => out.push(b),
        }
    }
    out
}

/// Map a `&str` to `WinAnsiEncoding` (CP-1252) bytes: ASCII passes, common
/// typographic characters map to their CP-1252 code points, and the rest becomes
/// `?` — the coverage limit of the base-14 export.
pub fn to_winansi(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let b = match ch {
            '\u{2022}' => 0x95, // • bullet
            '\u{2018}' => 0x91, // ‘
            '\u{2019}' => 0x92, // ’
            '\u{201C}' => 0x93, // “
            '\u{201D}' => 0x94, // ”
            '\u{2013}' => 0x96, // – en dash
            '\u{2014}' => 0x97, // — em dash
            '\u{2026}' => 0x85, // … ellipsis
            '\u{00A0}' => 0x20, // nbsp → space
            c if (c as u32) <= 0xFF => {
                let cp = c as u32;
                // 0x80..=0x9F in CP-1252 are mostly undefined / handled above;
                // pass through Latin-1 range only.
                if (0x20..=0x7E).contains(&cp) || (0xA0..=0xFF).contains(&cp) {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "cp is bounded to 0x20..=0x7E or 0xA0..=0xFF just above, so it always fits u8"
                    )]
                    let byte = cp as u8;
                    byte
                } else if cp == 0x09 {
                    b' ' // tab → space
                } else {
                    b'?'
                }
            }
            _ => b'?',
        };
        out.push(b);
    }
    out
}
