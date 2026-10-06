//! A minimal, hand-rolled PDF 1.7 writer.
//!
//! Just enough of the format to emit text (base-14 Helvetica / Helvetica-Bold
//! via `BT`/`ET` + `Tf`/`Td`/`Tj`), vector graphics (lines/rectangles for table
//! grids and image placeholders), and embedded raster images as `/DeviceRGB`
//! `Image` `XObjects` with `FlateDecode`-compressed pixel data.
//!
//! Objects are allocated lazily and serialized with a classic cross-reference
//! table, so the result is a self-contained file that opens in any conformant
//! viewer. There is no `printpdf` dependency — this keeps `tilo-doc-pdf` lockfile-
//! stable and gives full control over pagination and image embedding.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write as _;

/// One indirect object's serialized body (everything between `N 0 obj` and
/// `endobj`). Object numbers are 1-based and assigned in `add` order.
pub struct PdfWriter {
    objects: Vec<Vec<u8>>,
}

/// A reference to an allocated object (its 1-based number).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ObjId(pub usize);

impl PdfWriter {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    /// Reserve an object slot, returning its id. Body is filled later via
    /// [`PdfWriter::set`]. Lets us reference an object before its body exists
    /// (needed for the pages tree / page → content cross references).
    pub fn reserve(&mut self) -> ObjId {
        self.objects.push(Vec::new());
        ObjId(self.objects.len())
    }

    /// Allocate an object with a body in one step.
    pub fn add(&mut self, body: Vec<u8>) -> ObjId {
        let id = self.reserve();
        self.set(id, body);
        id
    }

    pub fn set(&mut self, id: ObjId, body: Vec<u8>) {
        // `id.0` is 1-based and every id this crate creates comes from
        // `reserve` (which always grows `objects` to at least that
        // length) — but `ObjId`'s field is public, so this degrades to a
        // no-op on an out-of-range id instead of assuming the invariant.
        if let Some(idx) = id.0.checked_sub(1)
            && let Some(slot) = self.objects.get_mut(idx)
        {
            *slot = body;
        }
    }

    /// Add a stream object: a dictionary `dict_extra` merged with `/Length`,
    /// followed by the raw stream `data`. `dict_extra` must NOT include the
    /// enclosing `<<`/`>>` — just the inner entries (e.g. `/Filter /FlateDecode`).
    pub fn add_stream(&mut self, dict_extra: &str, data: &[u8]) -> ObjId {
        let mut body = Vec::new();
        let _ = write!(body, "<< /Length {}{} >>\nstream\n", data.len(), dict_extra);
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.add(body)
    }

    /// Serialize the whole document. `root` is the object id of the `/Catalog`.
    pub fn finish(self, root: ObjId) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n");
        // Binary marker comment so tools treat the file as binary.
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        let mut offsets = Vec::with_capacity(self.objects.len());
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            // Object numbers are 1-based; bounded by how many objects one
            // document has, nowhere near `usize::MAX`.
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

/// Flate-compress a byte slice (zlib stream, what PDF `/FlateDecode` expects).
pub fn flate(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = enc.write_all(data);
    enc.finish().unwrap_or_default()
}

/// Escape a string for inclusion inside a PDF literal string `( ... )`.
///
/// `WinAnsiEncoding` is the font encoding we declare; characters outside it are
/// replaced with `?` so the content stream stays well-formed (see
/// [`to_winansi`]). Backslash and parentheses are escaped per the spec.
pub fn escape_pdf_text(s: &str) -> Vec<u8> {
    // A capacity hint only; bounded by the document text's byte length,
    // nowhere near `usize::MAX`.
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

/// Map a `&str` to `WinAnsiEncoding` (CP-1252) bytes. ASCII passes through; a
/// handful of common typographic characters are mapped to their CP-1252 code
/// points; everything else outside the byte range becomes `?`.
///
/// This is the font-coverage limit of the base-14 export: glyphs absent from
/// `WinAnsi` (CJK, most non-Latin scripts, many symbols) render as `?`.
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
                    // Both arms above bound `cp` to `0xFF` or less, so this always fits `u8`.
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
