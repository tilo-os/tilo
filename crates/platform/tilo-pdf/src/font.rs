//! Text for appearance streams: a pinned set of fonts, shaping, and
//! embedding exactly the glyphs used.
//!
//! Appearance text (a typed signature, a stamp's lines, free text) can be in
//! any script, so it is never set in a standard font. An export must also be
//! the same bytes for the same edits on every machine, so the fonts are not
//! whatever the host happens to have: they are Tilo's pinned font catalog
//! ([`tilo_font_catalog`]), which the OS image installs once and the helper
//! reads through a read-only Landlock grant on that one directory
//! ([`use_catalog`]). Each file is checked against its pinned SHA-256 as it
//! is mapped, so the bytes are the same everywhere, or no appearance text is
//! set at all. Each run of the text is given the first face of [`PINNED`],
//! in that fixed order, that covers it (staying with the previous run's face
//! while it covers the text), shaped with `harfrust` in its bidi direction
//! (so Arabic joins and combining marks sit where they belong) at the
//! regular weight, and the glyphs used are embedded as a CID-keyed subset
//! font with a `ToUnicode` map, so the text stays selectable and searchable
//! in every reader. A character none of the faces covers becomes the first
//! face's missing glyph — also the same everywhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Chunk, Filter, Name, Ref, Str};
use skrifa::attribute::Style;
use skrifa::instance::{Location, Size};
use skrifa::raw::TableProvider;
use skrifa::string::StringId;
use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};
use tilo_font_catalog::{Error as CatalogError, Scope};

/// The catalog faces appearance text is set in, in preference order: the
/// first face covering a character sets it. `(file, face index, family)`.
const PINNED: [(&str, u32, &str); 10] = [
    ("NotoSans[wght].ttf", 0, "Noto Sans"),
    ("NotoSansArabic[wght].ttf", 0, "Noto Sans Arabic"),
    ("NotoSansHebrew[wght].ttf", 0, "Noto Sans Hebrew"),
    ("NotoSansDevanagari[wght].ttf", 0, "Noto Sans Devanagari"),
    ("NotoSansBengali[wght].ttf", 0, "Noto Sans Bengali"),
    ("NotoSansThai[wght].ttf", 0, "Noto Sans Thai"),
    // Face 2 of the collection is Noto Sans CJK SC: Han in its
    // simplified-Chinese forms, with kana and hangul.
    ("NotoSansCJK-VF.ttc", 2, "Noto Sans CJK SC"),
    ("NotoSansSymbols[wght].ttf", 0, "Noto Sans Symbols"),
    ("NotoSansSymbols2-Regular.ttf", 0, "Noto Sans Symbols 2"),
    ("NotoSansMath-Regular.ttf", 0, "Noto Sans Math"),
];

/// One face of a pinned catalog file, mapped read-only.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BundledFace {
    /// The verified font file (a collection for `index > 0`).
    data: &'static [u8],
    index: u32,
    /// Its family name, recorded in the embedded font.
    family: &'static str,
}

/// The catalog directory appearance text is read from. Set once per process
/// before the first export ([`use_catalog`]); without it appearance text is
/// left out.
static CATALOG: OnceLock<PathBuf> = OnceLock::new();

/// The [`PINNED`] faces, mapped and verified on first use (not at start-up,
/// which every helper pays). Empty when any of them is unavailable.
static USABLE: OnceLock<Vec<BundledFace>> = OnceLock::new();

/// Read appearance fonts from the pinned catalog at `root`. The first call
/// wins: the set must not change within a process, or two exports of the
/// same edits could differ.
pub fn use_catalog(root: PathBuf) {
    let _ = CATALOG.set(root);
}

fn library() -> &'static [BundledFace] {
    USABLE.get_or_init(|| {
        let Some(root) = CATALOG.get() else {
            return Vec::new();
        };
        load(root).unwrap_or_else(|error| {
            // All or nothing: a partial set would pick different faces for
            // the same text on different machines.
            eprintln!("tilo-pdf: appearance text is left out: {error}");
            Vec::new()
        })
    })
}

fn load(root: &Path) -> Result<Vec<BundledFace>, CatalogError> {
    tilo_font_catalog::check_manifest(root)?;
    let manifest = tilo_font_catalog::manifest()?;
    let mut mapped: BTreeMap<&str, &'static [u8]> = BTreeMap::new();
    let mut faces = Vec::with_capacity(PINNED.len());
    for (file, index, family) in PINNED {
        let data = if let Some(data) = mapped.get(file) {
            *data
        } else {
            let asset = manifest
                .font
                .iter()
                .find(|asset| asset.file == file && asset.scope == Scope::Product)
                .ok_or_else(|| CatalogError::at(root, format!("{file} is not pinned")))?;
            let path = asset.path_in(root)?;
            let data = map(&path)?;
            asset.verify_bytes(&path, data)?;
            mapped.insert(file, data);
            data
        };
        let usable = FontRef::from_index(data, index).is_ok_and(|f| {
            has_table(&f, Tag::new(b"glyf"))
                || has_table(&f, Tag::new(b"CFF "))
                || has_table(&f, Tag::new(b"CFF2"))
        });
        if !usable {
            return Err(CatalogError::at(
                root.join(file),
                format!("face {index} has no outlines"),
            ));
        }
        faces.push(BundledFace {
            data,
            index,
            family,
        });
    }
    Ok(faces)
}

/// Map a catalog file read-only for the rest of the process.
fn map(path: &Path) -> Result<&'static [u8], CatalogError> {
    let file = std::fs::File::open(path).map_err(|e| CatalogError::at(path, e))?;
    // SAFETY: a read-only shared mapping of a font file. The installed
    // catalog lives on the read-only system image, its bytes are checked
    // against their pinned hash right after mapping, and nothing in this
    // process writes to it.
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| CatalogError::at(path, e))?;
    Ok(&**Box::leak(Box::new(map)))
}

/// The regular weight, for faces with a weight axis.
const WEIGHT: (Tag, f32) = (Tag::new(b"wght"), 400.0);

impl BundledFace {
    fn face(&self) -> Option<FontRef<'static>> {
        FontRef::from_index(self.data, self.index).ok()
    }

    fn covers(&self, c: char) -> bool {
        self.face().is_some_and(|f| f.charmap().map(c).is_some())
    }

    /// Variation coordinates the subset is instanced at.
    fn coordinates(&self) -> Vec<(subsetter::Tag, f32)> {
        let variable = self
            .face()
            .is_some_and(|f| f.axes().get_by_tag(WEIGHT.0).is_some());
        if variable {
            vec![(subsetter::Tag::new(b"wght"), WEIGHT.1)]
        } else {
            Vec::new()
        }
    }
}

/// Where `face` is read: at the regular weight when it is variable.
fn regular(face: &FontRef<'_>) -> Location {
    face.axes().location([WEIGHT])
}

fn has_table(face: &FontRef<'_>, tag: Tag) -> bool {
    face.table_data(tag).is_some()
}

/// A name-table entry, preferring English; `None` when absent or empty.
fn name(face: &FontRef<'_>, id: StringId) -> Option<String> {
    face.localized_strings(id)
        .english_or_first()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// The OS/2 weight class, regular when the table is missing.
fn weight_class(face: &FontRef<'_>) -> u16 {
    face.os2().map_or(400, |os2| os2.us_weight_class())
}

fn is_italic(face: &FontRef<'_>) -> bool {
    matches!(face.attributes().style, Style::Italic)
}

/// A font-unit value as the whole number PDF widths are written from.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped to the u16 range font units use"
)]
fn font_units(value: f32) -> u16 {
    value.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

#[derive(Clone, Debug)]
pub(crate) struct PlacedGlyph {
    /// Index into [`TextSetter`]'s fonts.
    pub font: usize,
    /// Glyph id in the embedded subset (the CID written in the content).
    pub cid: u16,
    /// Pen position of the glyph origin, relative to the line start and
    /// baseline, in points (y up).
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ShapedLine {
    pub glyphs: Vec<PlacedGlyph>,
    /// Advance width in points.
    pub width: f32,
}

/// A font chosen for some text in one export, and the glyphs used from it.
struct UsedFont {
    source: usize,
    remapper: subsetter::GlyphRemapper,
    /// Subset glyph id → the text it stands for (the first cluster seen).
    text: BTreeMap<u16, String>,
    /// Subset glyph id → advance in the font's units.
    advances: BTreeMap<u16, u16>,
}

/// Lays out appearance text for one export and embeds the fonts it used.
#[derive(Default)]
pub(crate) struct TextSetter {
    fonts: Vec<UsedFont>,
}

/// A font written into the output: its [`TextSetter`] index (see
/// [`TextSetter::resource_name`]) and its `Type0` font object.
pub(crate) struct EmbeddedFont {
    pub index: usize,
    pub font: Ref,
}

impl TextSetter {
    pub fn resource_name(index: usize) -> String {
        format!("TF{index}")
    }

    fn used_font(&mut self, source: usize) -> usize {
        if let Some(i) = self.fonts.iter().position(|f| f.source == source) {
            return i;
        }
        self.fonts.push(UsedFont {
            source,
            remapper: subsetter::GlyphRemapper::new(),
            text: BTreeMap::new(),
            advances: BTreeMap::new(),
        });
        self.fonts.len().saturating_sub(1)
    }

    /// Shape one line of `text` at `size` points. Characters no system font
    /// covers are left out.
    pub fn shape_line(&mut self, text: &str, size: f32) -> ShapedLine {
        let mut line = ShapedLine::default();
        let faces = library();
        if text.is_empty() || faces.is_empty() {
            return line;
        }
        let bidi = unicode_bidi::BidiInfo::new(text, None);
        let Some(para) = bidi.paragraphs.first() else {
            return line;
        };
        let (levels, runs) = bidi.visual_runs(para, para.range.clone());
        let mut pen = 0.0f32;
        for run in runs {
            let rtl = levels
                .get(run.start)
                .is_some_and(unicode_bidi::Level::is_rtl);
            let Some(run_text) = text.get(run.clone()) else {
                continue;
            };
            let mut segments = font_segments(faces, run_text);
            if rtl {
                segments.reverse();
            }
            for (source, range) in segments {
                let Some(segment) = run_text.get(range) else {
                    continue;
                };
                pen = self.shape_segment(source, segment, rtl, size, pen, &mut line.glyphs);
            }
        }
        line.width = pen;
        line
    }

    fn shape_segment(
        &mut self,
        source: usize,
        segment: &str,
        rtl: bool,
        size: f32,
        mut pen: f32,
        out: &mut Vec<PlacedGlyph>,
    ) -> f32 {
        let Some(face_source) = library().get(source) else {
            return pen;
        };
        let Some(face) = face_source.face() else {
            return pen;
        };
        let location = regular(&face);
        let shaper_data = harfrust::ShaperData::new(&face);
        let instance = harfrust::ShaperInstance::from_variations(
            &face,
            [harfrust::Variation {
                tag: WEIGHT.0,
                value: WEIGHT.1,
            }],
        );
        let shaper = shaper_data.shaper(&face).instance(Some(&instance)).build();
        let metrics = face.metrics(Size::unscaled(), &location);
        let advances = face.glyph_metrics(Size::unscaled(), &location);
        let scale = size / f32::from(metrics.units_per_em.max(1));
        let mut buffer = harfrust::UnicodeBuffer::new();
        buffer.push_str(segment);
        buffer.set_direction(if rtl {
            harfrust::Direction::RightToLeft
        } else {
            harfrust::Direction::LeftToRight
        });
        buffer.guess_segment_properties();
        let glyphs = shaper.shape(buffer, harfrust::ShapeOptions::new());
        let infos = glyphs.glyph_infos();
        let positions = glyphs.glyph_positions();
        // Cluster start offsets, to give each glyph the text it came from.
        let mut starts: Vec<usize> = infos.iter().map(|g| g.cluster as usize).collect();
        starts.sort_unstable();
        starts.dedup();
        let cluster_text = |start: usize| -> String {
            let end = starts
                .iter()
                .copied()
                .find(|&s| s > start)
                .unwrap_or(segment.len());
            segment.get(start..end).unwrap_or_default().to_string()
        };
        let font = self.used_font(source);
        let mut seen_clusters = std::collections::HashSet::new();
        for (info, pos) in infos.iter().zip(positions) {
            let Ok(old) = u16::try_from(info.glyph_id) else {
                continue;
            };
            let Some(used) = self.fonts.get_mut(font) else {
                break;
            };
            let cid = used.remapper.remap(old);
            let text = if seen_clusters.insert(info.cluster) {
                cluster_text(info.cluster as usize)
            } else {
                String::new()
            };
            used.text.entry(cid).or_insert(text);
            let advance = advances
                .advance_width(GlyphId::new(u32::from(old)))
                .map_or(0, font_units);
            used.advances.entry(cid).or_insert(advance);
            #[allow(
                clippy::cast_precision_loss,
                reason = "glyph positions are small font-unit integers"
            )]
            out.push(PlacedGlyph {
                font,
                cid,
                x: pen + pos.x_offset as f32 * scale,
                y: pos.y_offset as f32 * scale,
            });
            #[allow(
                clippy::cast_precision_loss,
                reason = "glyph advances are small font-unit integers"
            )]
            {
                pen += pos.x_advance as f32 * scale;
            }
        }
        pen
    }

    /// Break `text` into lines no wider than `width` points at `size`:
    /// at spaces, between ideographs, and at explicit line breaks. A word
    /// wider than the line is kept whole on its own line.
    pub fn wrap(&mut self, text: &str, size: f32, width: f32) -> Vec<ShapedLine> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let paragraph = paragraph.trim_end_matches('\r');
            let pieces = break_opportunities(paragraph);
            let mut current = String::new();
            for piece in pieces {
                let candidate = format!("{current}{piece}");
                if current.is_empty() {
                    current = candidate;
                    continue;
                }
                // Measure without recording glyphs.
                let fits = measure(candidate.trim_end(), size) <= width;
                if fits {
                    current = candidate;
                } else {
                    lines.push(self.shape_line(current.trim_end(), size));
                    current = piece.trim_start().to_string();
                }
            }
            lines.push(self.shape_line(current.trim_end(), size));
        }
        lines
    }

    /// Write every font used as a subset CID font, one object per chunk
    /// pushed to `objects`, numbering objects from `next`.
    pub fn embed(self, objects: &mut Vec<Chunk>, next: &mut i32) -> Vec<EmbeddedFont> {
        let mut out = Vec::new();
        let faces = library();
        for (index, used) in self.fonts.into_iter().enumerate() {
            let Some(source) = faces.get(used.source) else {
                continue;
            };
            let Some(font) = embed_font(objects, next, source, &used) else {
                continue;
            };
            out.push(EmbeddedFont { index, font });
        }
        out
    }
}

/// Width of `text` at `size` in points, shaped like [`TextSetter::shape_line`].
fn measure(text: &str, size: f32) -> f32 {
    TextSetter::default().shape_line(text, size).width
}

/// Split `text` after each break opportunity: after spaces, and around
/// ideographs (which break between any two).
fn break_opportunities(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        let next = chars.peek().copied();
        let boundary = match next {
            None => true,
            Some(n) => {
                (c.is_whitespace() && !n.is_whitespace()) || is_ideograph(c) || is_ideograph(n)
            }
        };
        if boundary {
            out.push(std::mem::take(&mut current));
        }
    }
    out
}

fn is_ideograph(c: char) -> bool {
    matches!(u32::from(c),
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// Whether `c` follows its base character into whatever font that uses.
fn attaches(c: char) -> bool {
    matches!(u32::from(c),
        0x0300..=0x036F // combining diacritics
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED // Arabic marks
        | 0x200C | 0x200D // joiners
        | 0xFE00..=0xFE0F) // variation selectors
}

/// Split `text` into runs, each set in one face: the previous run's face
/// while it covers the text, otherwise the best face that covers the next
/// character. Characters nothing covers go with the current face (they
/// become its missing glyph).
fn font_segments(faces: &[BundledFace], text: &str) -> Vec<(usize, std::ops::Range<usize>)> {
    let mut out: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
    let mut current: Option<usize> = None;
    for (at, c) in text.char_indices() {
        let end = at.saturating_add(c.len_utf8());
        let keep = current.is_some_and(|f| {
            attaches(c) || c.is_whitespace() || faces.get(f).is_some_and(|s| s.covers(c))
        });
        let face = if keep {
            current
        } else {
            faces
                .iter()
                .position(|s| s.covers(c))
                .or(current)
                .or(Some(0))
        };
        match (out.last_mut(), face) {
            (Some((last, range)), Some(f)) if *last == f => range.end = end,
            (_, Some(f)) => out.push((f, at..end)),
            _ => {}
        }
        current = face;
    }
    out
}

/// A subset tag (six capitals) derived from the glyph set, so the same
/// glyphs always name the font the same way.
fn subset_tag(source: &BundledFace, used: &UsedFont) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(source.family.as_bytes());
    for (cid, text) in &used.text {
        hasher.update(&cid.to_le_bytes());
        hasher.update(text.as_bytes());
    }
    let digest = hasher.finalize();
    digest
        .as_bytes()
        .iter()
        .take(6)
        .map(|b| char::from(b'A'.saturating_add(b % 26)))
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "font metrics are small integers; one pass writing the font's five objects in order"
)]
fn embed_font(
    objects: &mut Vec<Chunk>,
    next: &mut i32,
    source: &BundledFace,
    used: &UsedFont,
) -> Option<Ref> {
    let face = source.face()?;
    let location = regular(&face);
    // Instanced at the regular weight; variable CFF2 outlines come back as
    // TrueType.
    let subset = subsetter::subset_with_variations(
        source.data,
        source.index,
        &source.coordinates(),
        &used.remapper,
    )
    .ok()?;
    let is_cff = !has_table(&FontRef::new(&subset).ok()?, Tag::new(b"glyf"));
    let font_bytes = if is_cff {
        let raw = FontRef::new(&subset).ok()?;
        raw.table_data(Tag::new(b"CFF "))?.as_bytes().to_vec()
    } else {
        subset
    };
    let metrics = face.metrics(Size::unscaled(), &location);
    let upem = f32::from(metrics.units_per_em.max(1));
    let to_pdf = |v: f32| v * 1000.0 / upem;
    // A variable face's own name is its default instance's; name the
    // instance embedded.
    let postscript = if face.axes().is_empty() {
        name(&face, StringId::POSTSCRIPT_NAME).unwrap_or_else(|| source.family.replace(' ', ""))
    } else {
        format!("{}-Regular", source.family.replace(' ', ""))
    };
    let postscript: String = postscript
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(100)
        .collect();
    let base_font = format!("{}+{postscript}", subset_tag(source, used));

    let mut alloc = || {
        let r = Ref::new(*next);
        *next = next.saturating_add(1);
        r
    };
    let type0 = alloc();
    let cid_ref = alloc();
    let descriptor = alloc();
    let file = alloc();
    let cmap_ref = alloc();

    let type0_name = if is_cff {
        format!("{base_font}-Identity-H")
    } else {
        base_font.clone()
    };
    let mut chunk = Chunk::new();
    chunk
        .type0_font(type0)
        .base_font(Name(type0_name.as_bytes()))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(cid_ref)
        .to_unicode(cmap_ref);
    objects.push(chunk);

    let system_info = SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    };
    let mut chunk = Chunk::new();
    {
        let mut cid = chunk.cid_font(cid_ref);
        cid.subtype(if is_cff {
            CidFontType::Type0
        } else {
            CidFontType::Type2
        });
        cid.base_font(Name(base_font.as_bytes()));
        cid.system_info(system_info);
        cid.font_descriptor(descriptor);
        cid.default_width(0.0);
        if !is_cff {
            cid.cid_to_gid_map_predefined(Name(b"Identity"));
        }
        let mut widths = cid.widths();
        for (cid_id, advance) in &used.advances {
            widths.consecutive(*cid_id, [to_pdf(f32::from(*advance))]);
        }
    }
    objects.push(chunk);

    let bbox = metrics.bounds.unwrap_or_default();
    let mut flags = FontFlags::SYMBOLIC;
    if metrics.is_monospace {
        flags |= FontFlags::FIXED_PITCH;
    }
    if is_italic(&face) {
        flags |= FontFlags::ITALIC;
    }
    let ascent = to_pdf(metrics.ascent);
    let cap_height = metrics.cap_height.map_or(ascent, to_pdf);
    let mut chunk = Chunk::new();
    {
        let mut fd = chunk.font_descriptor(descriptor);
        fd.name(Name(base_font.as_bytes()))
            .flags(flags)
            .bbox(pdf_writer::Rect::new(
                to_pdf(bbox.x_min),
                to_pdf(bbox.y_min),
                to_pdf(bbox.x_max),
                to_pdf(bbox.y_max),
            ))
            .italic_angle(metrics.italic_angle)
            .ascent(ascent)
            .descent(to_pdf(metrics.descent))
            .cap_height(cap_height)
            .stem_v(10.0 + 0.244 * (f32::from(weight_class(&face)) - 50.0));
        if is_cff {
            fd.font_file3(file);
        } else {
            fd.font_file2(file);
        }
    }
    objects.push(chunk);

    let compressed = deflate(&font_bytes);
    let mut chunk = Chunk::new();
    {
        let mut stream = chunk.stream(file, &compressed);
        stream.filter(Filter::FlateDecode);
        if is_cff {
            stream.pair(Name(b"Subtype"), Name(b"CIDFontType0C"));
        } else {
            stream.pair(
                Name(b"Length1"),
                i32::try_from(font_bytes.len()).unwrap_or(i32::MAX),
            );
        }
    }
    objects.push(chunk);

    let mut cmap = UnicodeCmap::new(Name(b"Custom"), system_info);
    for (cid_id, text) in &used.text {
        if !text.is_empty() {
            cmap.pair_with_multiple(*cid_id, text.chars());
        }
    }
    let cmap_bytes = cmap.finish();
    let mut chunk = Chunk::new();
    chunk.cmap(cmap_ref, &cmap_bytes).system_info(system_info);
    objects.push(chunk);
    Some(type0)
}

/// zlib-compress `data` (deterministic for the same input).
pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    if encoder.write_all(data).is_err() {
        return Vec::new();
    }
    encoder.finish().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaking_segments_keep_marks_and_break_between_words_and_ideographs() {
        // Phase `segments_keep_marks_with_their_base`.
        fn segments_keep_marks_with_their_base() {
            let faces = library();
            if faces.is_empty() {
                return;
            }
            let segments = font_segments(faces, "e\u{301}x");
            assert_eq!(segments.len(), 1, "{segments:?}");
        }
        {
            assert_eq!(break_opportunities("ab cd  ef"), vec!["ab ", "cd  ", "ef"]);
            assert_eq!(break_opportunities("你好a"), vec!["你", "好", "a"]);
        }
        // Phase `segments_keep_marks_with_their_base` (defined above).
        segments_keep_marks_with_their_base();
    }
}
