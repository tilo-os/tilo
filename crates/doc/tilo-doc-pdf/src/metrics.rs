//! Glyph-width metrics for the two base-14 fonts we use (Helvetica and
//! Helvetica-Bold). Widths are in 1000-unit em space, indexed by `WinAnsi` byte.
//!
//! These are the standard Adobe AFM advance widths. They let us word-wrap text
//! accurately without embedding a font program — the viewer ships its own
//! Helvetica, but we still need the widths to know where to break lines.

/// Helvetica advance widths (AFM `WX`) for `WinAnsi` byte values 0..=255.
/// Bytes with no glyph use the Helvetica default of 0; unknown printable
/// positions fall back to the space width via [`char_width`].
pub static HELVETICA: [u16; 256] = build_helvetica();

/// Helvetica-Bold advance widths for `WinAnsi` byte values 0..=255.
pub static HELVETICA_BOLD: [u16; 256] = build_helvetica_bold();

/// Advance width (in 1000-em units) of one `WinAnsi` byte in the given table.
#[inline]
pub fn byte_width(table: &[u16; 256], b: u8) -> u16 {
    // `b: u8` is always `0..=255`, exactly `table`'s index range, so this
    // is never `None`; `.get()` just spells that out instead of `[]`.
    let w = table.get(b as usize).copied().unwrap_or(0);
    if w == 0 && b != b' ' {
        // Fall back to space width for any unmapped printable byte so wrapping
        // never under-counts to zero.
        table.get(b' ' as usize).copied().unwrap_or(0)
    } else {
        w
    }
}

/// Width of a `WinAnsi` byte slice at `font_size` points, in points.
pub fn text_width(table: &[u16; 256], bytes: &[u8], font_size: f32) -> f32 {
    let units: u32 = bytes.iter().map(|&b| u32::from(byte_width(table, b))).sum();
    // A cumulative glyph-width sum feeding a word-wrap decision; wrapping
    // is already an approximation, so precision loss here is invisible in
    // practice long before `units` could reach f32's precision limit.
    #[allow(
        clippy::cast_precision_loss,
        reason = "feeds an approximate word-wrap width; no realistic text run reaches f32's precision limit"
    )]
    let units_f32 = units as f32;
    units_f32 * font_size / 1000.0
}

// The tables below are the canonical Adobe Helvetica / Helvetica-Bold AFM
// widths. Generated as const fns so they live in static memory with no runtime
// init cost. Positions not assigned by the AFM are left 0 and resolved by
// `byte_width`'s space fallback.

// ASCII printable range (0x20..0x7E), hoisted out of `build_helvetica` so
// that function stays under the line-count limit; the data itself is
// unchanged, just no longer a local of the function that consumes it.
const HELVETICA_ASCII: [(u8, u16); 95] = [
    (32, 278),
    (33, 278),
    (34, 355),
    (35, 556),
    (36, 556),
    (37, 889),
    (38, 667),
    (39, 191),
    (40, 333),
    (41, 333),
    (42, 389),
    (43, 584),
    (44, 278),
    (45, 333),
    (46, 278),
    (47, 278),
    (48, 556),
    (49, 556),
    (50, 556),
    (51, 556),
    (52, 556),
    (53, 556),
    (54, 556),
    (55, 556),
    (56, 556),
    (57, 556),
    (58, 278),
    (59, 278),
    (60, 584),
    (61, 584),
    (62, 584),
    (63, 556),
    (64, 1015),
    (65, 667),
    (66, 667),
    (67, 722),
    (68, 722),
    (69, 667),
    (70, 611),
    (71, 778),
    (72, 722),
    (73, 278),
    (74, 500),
    (75, 667),
    (76, 556),
    (77, 833),
    (78, 722),
    (79, 778),
    (80, 667),
    (81, 778),
    (82, 722),
    (83, 667),
    (84, 611),
    (85, 722),
    (86, 667),
    (87, 944),
    (88, 667),
    (89, 667),
    (90, 611),
    (91, 278),
    (92, 278),
    (93, 278),
    (94, 469),
    (95, 556),
    (96, 333),
    (97, 556),
    (98, 556),
    (99, 500),
    (100, 556),
    (101, 556),
    (102, 278),
    (103, 556),
    (104, 556),
    (105, 222),
    (106, 222),
    (107, 500),
    (108, 222),
    (109, 833),
    (110, 556),
    (111, 556),
    (112, 556),
    (113, 556),
    (114, 333),
    (115, 500),
    (116, 278),
    (117, 556),
    (118, 500),
    (119, 722),
    (120, 500),
    (121, 500),
    (122, 500),
    (123, 334),
    (124, 260),
    (125, 334),
    (126, 584),
];

/// Builds [`HELVETICA`] from [`HELVETICA_ASCII`] plus a handful of
/// high-byte overrides. `HELVETICA_ASCII`'s indices/loop counter are
/// bounded by its own fixed length, checked by the const evaluator at
/// compile time — there is no runtime input and no failure path.
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "const fn over a fixed-length literal table; the compile-time evaluator itself checks every index and increment"
)]
const fn build_helvetica() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < HELVETICA_ASCII.len() {
        t[HELVETICA_ASCII[i].0 as usize] = HELVETICA_ASCII[i].1;
        i += 1;
    }
    // A few high-byte WinAnsi positions used by `to_winansi` (quotes, dashes,
    // bullet, ellipsis, nbsp). Widths from the Helvetica AFM.
    t[0x85] = 1000; // ellipsis
    t[0x91] = 222; // quoteleft
    t[0x92] = 222; // quoteright
    t[0x93] = 333; // quotedblleft
    t[0x94] = 333; // quotedblright
    t[0x95] = 350; // bullet
    t[0x96] = 556; // endash
    t[0x97] = 1000; // emdash
    t[0xA0] = 278; // nbsp
    t
}

// Hoisted out of `build_helvetica_bold` for the same reason as
// `HELVETICA_ASCII` above; the data is unchanged.
const HELVETICA_BOLD_ASCII: [(u8, u16); 95] = [
    (32, 278),
    (33, 333),
    (34, 474),
    (35, 556),
    (36, 556),
    (37, 889),
    (38, 722),
    (39, 238),
    (40, 333),
    (41, 333),
    (42, 389),
    (43, 584),
    (44, 278),
    (45, 333),
    (46, 278),
    (47, 278),
    (48, 556),
    (49, 556),
    (50, 556),
    (51, 556),
    (52, 556),
    (53, 556),
    (54, 556),
    (55, 556),
    (56, 556),
    (57, 556),
    (58, 333),
    (59, 333),
    (60, 584),
    (61, 584),
    (62, 584),
    (63, 611),
    (64, 975),
    (65, 722),
    (66, 722),
    (67, 722),
    (68, 722),
    (69, 667),
    (70, 611),
    (71, 778),
    (72, 722),
    (73, 278),
    (74, 556),
    (75, 722),
    (76, 611),
    (77, 833),
    (78, 722),
    (79, 778),
    (80, 667),
    (81, 778),
    (82, 722),
    (83, 667),
    (84, 611),
    (85, 722),
    (86, 667),
    (87, 944),
    (88, 667),
    (89, 667),
    (90, 611),
    (91, 333),
    (92, 278),
    (93, 333),
    (94, 584),
    (95, 556),
    (96, 333),
    (97, 556),
    (98, 611),
    (99, 556),
    (100, 611),
    (101, 556),
    (102, 333),
    (103, 611),
    (104, 611),
    (105, 278),
    (106, 278),
    (107, 556),
    (108, 278),
    (109, 889),
    (110, 611),
    (111, 611),
    (112, 611),
    (113, 611),
    (114, 389),
    (115, 556),
    (116, 333),
    (117, 611),
    (118, 556),
    (119, 778),
    (120, 556),
    (121, 556),
    (122, 500),
    (123, 389),
    (124, 280),
    (125, 389),
    (126, 584),
];

/// Builds [`HELVETICA_BOLD`]; see [`build_helvetica`] for why the
/// indexing/arithmetic here is allowed.
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "const fn over a fixed-length literal table; the compile-time evaluator itself checks every index and increment"
)]
const fn build_helvetica_bold() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < HELVETICA_BOLD_ASCII.len() {
        t[HELVETICA_BOLD_ASCII[i].0 as usize] = HELVETICA_BOLD_ASCII[i].1;
        i += 1;
    }
    t[0x85] = 1000; // ellipsis
    t[0x91] = 278; // quoteleft
    t[0x92] = 278; // quoteright
    t[0x93] = 500; // quotedblleft
    t[0x94] = 500; // quotedblright
    t[0x95] = 350; // bullet
    t[0x96] = 556; // endash
    t[0x97] = 1000; // emdash
    t[0xA0] = 278; // nbsp
    t
}
