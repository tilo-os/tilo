//! A vello [`Encoding`] as bytes, with fonts and images by id.
//!
//! The streams go over as they are — path tags, path data, draw tags, draw
//! data, transforms and styles are already plain words — and so do the late
//! bound resources, except that a glyph run names its font and an image patch
//! its image by the [`FontId`]/[`ImageId`] the app registered them under.
//! Decoding reads every byte once into a fresh [`Encoding`]: a record in
//! shared memory that the app keeps writing to is copied, not revisited, so
//! what [`crate::validate`] checks is what the GPU later sees.
//!
//! Layout, all little-endian, every section padded to four bytes:
//!
//! ```text
//! counts      16 × u32  path tags, path data, draw tags, draw data,
//!                       transforms, styles, patches, color stops, glyphs,
//!                       glyph runs, coords, n_paths, n_path_segments,
//!                       n_clips, n_open_clips, flags
//! path tags   u8 each
//! path data   u32 each
//! draw tags   u32 each
//! draw data   u32 each
//! transforms  6 × f32 each
//! styles      2 × u32 each
//! patches     5 × u32 each: kind, then its fields
//! stops       f32 offset, u8 colour space, u8 missing, u16 0, 4 × f32
//! glyphs      u32 id, f32 x, f32 y
//! glyph runs  see `write_glyph_run`
//! coords      i16 each
//! ```

use peniko::color::{ColorSpaceTag, DynamicColor, Flags, Missing};
use peniko::kurbo::{Cap, Diagonal2, Join, Stroke};
use peniko::{ColorStop, Extend, Fill, FontData, ImageData, Style as BrushStyle};
use vello_encoding::{
    DrawTag, Encoding, FontEmbolden, Glyph, GlyphRun, Patch, PathTag, StreamOffsets, Style,
    Transform,
};

use crate::bytes::{Reader, Writer};
use crate::{FontId, ImageId, WireError};

/// What an app's encoder asks of its resource registry: the id a font or an
/// image goes by on this connection, registering it on first sight.
pub trait ResourceIds {
    /// The id `font` is registered under.
    ///
    /// # Errors
    ///
    /// When the font cannot be registered (a limit, a failed upload).
    fn font(&mut self, font: &FontData) -> Result<FontId, WireError>;

    /// The id `image` is registered under.
    ///
    /// # Errors
    ///
    /// When the image cannot be registered.
    fn image(&mut self, image: &ImageData) -> Result<ImageId, WireError>;
}

/// What the service's decoder needs: the font and image an id stands for on
/// this connection. `None` for an id the connection never registered.
pub trait Resources {
    fn font(&self, id: FontId) -> Option<FontData>;
    fn image(&self, id: ImageId) -> Option<ImageData>;
}

const PATCH_RAMP: u32 = 0;
const PATCH_GLYPH_RUN: u32 = 1;
const PATCH_IMAGE: u32 = 2;

const RUN_GLYPH_TRANSFORM: u32 = 1;
const RUN_BRUSH_TRANSFORM: u32 = 2;
const RUN_HINT: u32 = 4;

const STYLE_FILL_NONZERO: u32 = 0;
const STYLE_FILL_EVEN_ODD: u32 = 1;
const STYLE_STROKE: u32 = 2;

/// Dashes one stroked glyph run may carry.
pub const MAX_DASHES: usize = 64;

const PATCH_BYTES: usize = 20;
const STOP_BYTES: usize = 24;
const GLYPH_BYTES: usize = 12;
/// The fixed part of a glyph run; a stroke's dashes follow it.
const RUN_FIXED_BYTES: usize =
    4 + 24 + 4 + 24 + 24 + 4 + 8 + 8 + 4 + 8 + 8 + 8 + 8 + 24 + 4 + 8 + 4 + 8 + 8 + 4;

/// Append `encoding` to `out`.
///
/// # Errors
///
/// When a resource cannot be registered or a count does not fit the wire.
pub fn encode(
    encoding: &Encoding,
    ids: &mut dyn ResourceIds,
    out: &mut Vec<u8>,
) -> Result<(), WireError> {
    let res = &encoding.resources;
    let mut w = Writer::new(out);
    for count in [
        encoding.path_tags.len(),
        encoding.path_data.len(),
        encoding.draw_tags.len(),
        encoding.draw_data.len(),
        encoding.transforms.len(),
        encoding.styles.len(),
        res.patches.len(),
        res.color_stops.len(),
        res.glyphs.len(),
        res.glyph_runs.len(),
        res.normalized_coords.len(),
    ] {
        w.count(count)?;
    }
    for value in [
        encoding.n_paths,
        encoding.n_path_segments,
        encoding.n_clips,
        encoding.n_open_clips,
        encoding.flags,
    ] {
        w.u32(value);
    }
    for tag in &encoding.path_tags {
        w.u8(tag.0);
    }
    w.align4(encoding.path_tags.len());
    for word in &encoding.path_data {
        w.u32(*word);
    }
    for tag in &encoding.draw_tags {
        w.u32(tag.0);
    }
    for word in &encoding.draw_data {
        w.u32(*word);
    }
    for transform in &encoding.transforms {
        write_transform(&mut w, transform);
    }
    for style in &encoding.styles {
        w.u32(style.flags_and_miter_limit);
        w.f32(style.line_width);
    }
    for patch in &res.patches {
        match patch {
            Patch::Ramp {
                draw_data_offset,
                stops,
                extend,
            } => {
                w.u32(PATCH_RAMP);
                w.count(*draw_data_offset)?;
                w.count(stops.start)?;
                w.count(stops.end)?;
                w.u32(*extend as u32);
            }
            Patch::GlyphRun { index } => {
                w.u32(PATCH_GLYPH_RUN);
                w.count(*index)?;
                w.u32(0);
                w.u32(0);
                w.u32(0);
            }
            Patch::Image {
                draw_data_offset,
                image,
            } => {
                let id = ids.image(image)?;
                w.u32(PATCH_IMAGE);
                w.count(*draw_data_offset)?;
                w.u32(id.0);
                w.u32(0);
                w.u32(0);
            }
        }
    }
    for stop in &res.color_stops {
        write_stop(&mut w, stop);
    }
    for glyph in &res.glyphs {
        w.u32(glyph.id);
        w.f32(glyph.x);
        w.f32(glyph.y);
    }
    for run in &res.glyph_runs {
        let font = ids.font(&run.font)?;
        write_glyph_run(&mut w, run, font)?;
    }
    for coord in &res.normalized_coords {
        w.i16(*coord);
    }
    w.align4(res.normalized_coords.len().saturating_mul(2));
    Ok(())
}

fn write_transform(w: &mut Writer<'_>, transform: &Transform) {
    for value in transform.matrix {
        w.f32(value);
    }
    for value in transform.translation {
        w.f32(value);
    }
}

fn write_stop(w: &mut Writer<'_>, stop: &ColorStop) {
    w.f32(stop.offset);
    w.u8(stop.color.cs as u8);
    let missing = stop.color.flags.missing();
    let mut bits = 0_u8;
    for ix in 0..4_u8 {
        if missing.contains(usize::from(ix)) {
            bits |= 1 << ix;
        }
    }
    w.u8(bits);
    w.u16(0);
    for value in stop.color.components {
        w.f32(value);
    }
}

fn write_glyph_run(w: &mut Writer<'_>, run: &GlyphRun, font: FontId) -> Result<(), WireError> {
    w.u32(font.0);
    write_transform(w, &run.transform);
    let mut flags = 0;
    if run.glyph_transform.is_some() {
        flags |= RUN_GLYPH_TRANSFORM;
    }
    if run.brush_transform.is_some() {
        flags |= RUN_BRUSH_TRANSFORM;
    }
    if run.hint {
        flags |= RUN_HINT;
    }
    w.u32(flags);
    write_transform(w, &run.glyph_transform.unwrap_or(Transform::IDENTITY));
    write_transform(w, &run.brush_transform.unwrap_or(Transform::IDENTITY));
    w.f32(run.font_size);
    w.f64(run.font_embolden.amount.xx);
    w.f64(run.font_embolden.amount.yy);
    w.u32(join_bits(run.font_embolden.join));
    w.f64(run.font_embolden.miter_limit);
    w.f64(run.font_embolden.tolerance);
    w.count(run.normalized_coords.start)?;
    w.count(run.normalized_coords.end)?;
    w.count(run.glyphs.start)?;
    w.count(run.glyphs.end)?;
    let offsets = &run.stream_offsets;
    for offset in [
        offsets.path_tags,
        offsets.path_data,
        offsets.draw_tags,
        offsets.draw_data,
        offsets.transforms,
        offsets.styles,
    ] {
        w.count(offset)?;
    }
    match &run.style {
        BrushStyle::Fill(Fill::NonZero) => write_stroke(w, STYLE_FILL_NONZERO, &Stroke::new(0.0)),
        BrushStyle::Fill(Fill::EvenOdd) => write_stroke(w, STYLE_FILL_EVEN_ODD, &Stroke::new(0.0)),
        BrushStyle::Stroke(stroke) => write_stroke(w, STYLE_STROKE, stroke),
    }
}

fn write_stroke(w: &mut Writer<'_>, kind: u32, stroke: &Stroke) -> Result<(), WireError> {
    w.u32(kind);
    w.f64(stroke.width);
    w.u32(
        join_bits(stroke.join)
            | (cap_bits(stroke.start_cap) << 8)
            | (cap_bits(stroke.end_cap) << 16),
    );
    w.f64(stroke.miter_limit);
    w.f64(stroke.dash_offset);
    if stroke.dash_pattern.len() > MAX_DASHES {
        return Err(WireError::OverLimit("dashes of a stroked glyph run"));
    }
    w.count(stroke.dash_pattern.len())?;
    for dash in &stroke.dash_pattern {
        w.f64(*dash);
    }
    Ok(())
}

fn join_bits(join: Join) -> u32 {
    match join {
        Join::Bevel => 0,
        Join::Miter => 1,
        Join::Round => 2,
    }
}

fn cap_bits(cap: Cap) -> u32 {
    match cap {
        Cap::Butt => 0,
        Cap::Square => 1,
        Cap::Round => 2,
    }
}

fn join_from(bits: u32) -> Result<Join, WireError> {
    match bits {
        0 => Ok(Join::Bevel),
        1 => Ok(Join::Miter),
        2 => Ok(Join::Round),
        _ => Err(WireError::Malformed("join")),
    }
}

fn cap_from(bits: u32) -> Result<Cap, WireError> {
    match bits {
        0 => Ok(Cap::Butt),
        1 => Ok(Cap::Square),
        2 => Ok(Cap::Round),
        _ => Err(WireError::Malformed("cap")),
    }
}

fn extend_from(bits: u32) -> Result<Extend, WireError> {
    match bits {
        0 => Ok(Extend::Pad),
        1 => Ok(Extend::Repeat),
        2 => Ok(Extend::Reflect),
        _ => Err(WireError::Malformed("extend")),
    }
}

fn color_space_from(bits: u8) -> Result<ColorSpaceTag, WireError> {
    Ok(match bits {
        0 => ColorSpaceTag::Srgb,
        1 => ColorSpaceTag::LinearSrgb,
        2 => ColorSpaceTag::Lab,
        3 => ColorSpaceTag::Lch,
        4 => ColorSpaceTag::Hsl,
        5 => ColorSpaceTag::Hwb,
        6 => ColorSpaceTag::Oklab,
        7 => ColorSpaceTag::Oklch,
        8 => ColorSpaceTag::DisplayP3,
        9 => ColorSpaceTag::A98Rgb,
        10 => ColorSpaceTag::ProphotoRgb,
        11 => ColorSpaceTag::Rec2020,
        12 => ColorSpaceTag::AcesCg,
        13 => ColorSpaceTag::XyzD50,
        14 => ColorSpaceTag::XyzD65,
        15 => ColorSpaceTag::Aces2065_1,
        _ => return Err(WireError::Malformed("colour space")),
    })
}

/// The counts at the head of an encoding, as read.
struct Counts {
    path_tags: usize,
    path_data: usize,
    draw_tags: usize,
    draw_data: usize,
    transforms: usize,
    styles: usize,
    patches: usize,
    stops: usize,
    glyphs: usize,
    runs: usize,
    coords: usize,
}

/// Read one encoding written by [`encode`], resolving its ids through
/// `resources`. Only the record's shape is checked here (every count backed
/// by bytes, every enumeration known, every id registered); whether it is
/// safe to draw is [`crate::validate::validate`]'s question.
pub fn decode(bytes: &[u8], resources: &dyn Resources) -> Result<Encoding, WireError> {
    let mut r = Reader::new(bytes);
    let mut header = [0_usize; 11];
    for slot in &mut header {
        *slot = usize::try_from(r.u32()?).map_err(|_| WireError::Overflow)?;
    }
    let [
        path_tags,
        path_data,
        draw_tags,
        draw_data,
        transforms,
        styles,
        patches,
        stops,
        glyphs,
        runs,
        coords,
    ] = header;
    let counts = Counts {
        path_tags,
        path_data,
        draw_tags,
        draw_data,
        transforms,
        styles,
        patches,
        stops,
        glyphs,
        runs,
        coords,
    };
    let mut encoding = Encoding::new();
    encoding.n_paths = r.u32()?;
    encoding.n_path_segments = r.u32()?;
    encoding.n_clips = r.u32()?;
    encoding.n_open_clips = r.u32()?;
    encoding.flags = r.u32()?;
    check_backed(&counts, r.remaining())?;
    read_streams(&mut r, &counts, &mut encoding)?;
    read_resources(&mut r, &counts, resources, &mut encoding)?;
    if !r.is_empty() {
        return Err(WireError::Malformed("bytes after the encoding"));
    }
    Ok(encoding)
}

/// Every count backed by bytes before anything is reserved, so a hostile
/// header cannot make the service allocate what the record does not carry.
fn check_backed(counts: &Counts, remaining: usize) -> Result<(), WireError> {
    let fixed = [
        (counts.path_tags, 1),
        (counts.path_data, 4),
        (counts.draw_tags, 4),
        (counts.draw_data, 4),
        (counts.transforms, 24),
        (counts.styles, 8),
        (counts.patches, PATCH_BYTES),
        (counts.stops, STOP_BYTES),
        (counts.glyphs, GLYPH_BYTES),
        (counts.runs, RUN_FIXED_BYTES),
        (counts.coords, 2),
    ];
    let mut total = 0_usize;
    for (count, size) in fixed {
        let bytes = count.checked_mul(size).ok_or(WireError::Overflow)?;
        total = total.checked_add(bytes).ok_or(WireError::Overflow)?;
    }
    if total > remaining {
        return Err(WireError::Truncated);
    }
    Ok(())
}

fn read_transform(r: &mut Reader<'_>) -> Result<Transform, WireError> {
    Ok(Transform {
        matrix: [r.f32()?, r.f32()?, r.f32()?, r.f32()?],
        translation: [r.f32()?, r.f32()?],
    })
}

fn read_streams(
    r: &mut Reader<'_>,
    counts: &Counts,
    encoding: &mut Encoding,
) -> Result<(), WireError> {
    encoding.path_tags = r
        .take(counts.path_tags)?
        .iter()
        .map(|tag| PathTag(*tag))
        .collect();
    r.align4(counts.path_tags)?;
    encoding.path_data = (0..counts.path_data)
        .map(|_| r.u32())
        .collect::<Result<_, _>>()?;
    encoding.draw_tags = (0..counts.draw_tags)
        .map(|_| r.u32().map(DrawTag))
        .collect::<Result<_, _>>()?;
    encoding.draw_data = (0..counts.draw_data)
        .map(|_| r.u32())
        .collect::<Result<_, _>>()?;
    encoding.transforms = (0..counts.transforms)
        .map(|_| read_transform(r))
        .collect::<Result<_, _>>()?;
    encoding.styles = (0..counts.styles)
        .map(|_| {
            Ok(Style {
                flags_and_miter_limit: r.u32()?,
                line_width: r.f32()?,
            })
        })
        .collect::<Result<_, WireError>>()?;
    Ok(())
}

fn index(value: u32) -> Result<usize, WireError> {
    usize::try_from(value).map_err(|_| WireError::Overflow)
}

fn read_resources(
    r: &mut Reader<'_>,
    counts: &Counts,
    resources: &dyn Resources,
    encoding: &mut Encoding,
) -> Result<(), WireError> {
    let res = &mut encoding.resources;
    res.patches = (0..counts.patches)
        .map(|_| read_patch(r, resources))
        .collect::<Result<_, _>>()?;
    res.color_stops = (0..counts.stops)
        .map(|_| read_stop(r))
        .collect::<Result<_, _>>()?;
    res.glyphs = (0..counts.glyphs)
        .map(|_| {
            Ok(Glyph {
                id: r.u32()?,
                x: r.f32()?,
                y: r.f32()?,
            })
        })
        .collect::<Result<_, WireError>>()?;
    res.glyph_runs = (0..counts.runs)
        .map(|_| read_glyph_run(r, resources))
        .collect::<Result<_, _>>()?;
    res.normalized_coords = (0..counts.coords)
        .map(|_| r.i16())
        .collect::<Result<_, _>>()?;
    r.align4(counts.coords.saturating_mul(2))?;
    Ok(())
}

fn read_patch(r: &mut Reader<'_>, resources: &dyn Resources) -> Result<Patch, WireError> {
    let kind = r.u32()?;
    let [w0, w1, w2, w3] = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
    match kind {
        PATCH_RAMP => Ok(Patch::Ramp {
            draw_data_offset: index(w0)?,
            stops: index(w1)?..index(w2)?,
            extend: extend_from(w3)?,
        }),
        PATCH_GLYPH_RUN if (w1, w2, w3) == (0, 0, 0) => Ok(Patch::GlyphRun { index: index(w0)? }),
        PATCH_IMAGE if (w2, w3) == (0, 0) => Ok(Patch::Image {
            draw_data_offset: index(w0)?,
            image: resources
                .image(ImageId(w1))
                .ok_or(WireError::UnknownResource("an image"))?,
        }),
        _ => Err(WireError::Malformed("patch")),
    }
}

fn read_stop(r: &mut Reader<'_>) -> Result<ColorStop, WireError> {
    let offset = r.f32()?;
    let cs = color_space_from(r.u8()?)?;
    let bits = r.u8()?;
    if bits & !0xf != 0 || r.u16()? != 0 {
        return Err(WireError::Malformed("colour stop"));
    }
    let mut missing = Missing::EMPTY;
    for ix in 0..4_u8 {
        if bits & (1 << ix) != 0 {
            missing.insert(usize::from(ix));
        }
    }
    let components = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    Ok(ColorStop {
        offset,
        color: DynamicColor {
            cs,
            flags: Flags::from_missing(missing),
            components,
        },
    })
}

fn read_glyph_run(r: &mut Reader<'_>, resources: &dyn Resources) -> Result<GlyphRun, WireError> {
    let font = resources
        .font(FontId(r.u32()?))
        .ok_or(WireError::UnknownResource("a font"))?;
    let transform = read_transform(r)?;
    let flags = r.u32()?;
    if flags & !(RUN_GLYPH_TRANSFORM | RUN_BRUSH_TRANSFORM | RUN_HINT) != 0 {
        return Err(WireError::Malformed("glyph run flags"));
    }
    let glyph_transform = read_transform(r)?;
    let brush_transform = read_transform(r)?;
    let font_size = r.f32()?;
    let amount = Diagonal2 {
        xx: r.f64()?,
        yy: r.f64()?,
    };
    let embolden = FontEmbolden::new(amount)
        .with_join(join_from(r.u32()?)?)
        .with_miter_limit(r.f64()?)
        .with_tolerance(r.f64()?);
    let normalized_coords = index(r.u32()?)?..index(r.u32()?)?;
    let glyphs = index(r.u32()?)?..index(r.u32()?)?;
    let stream_offsets = StreamOffsets {
        path_tags: index(r.u32()?)?,
        path_data: index(r.u32()?)?,
        draw_tags: index(r.u32()?)?,
        draw_data: index(r.u32()?)?,
        transforms: index(r.u32()?)?,
        styles: index(r.u32()?)?,
    };
    let style = read_style(r)?;
    Ok(GlyphRun {
        font,
        transform,
        glyph_transform: (flags & RUN_GLYPH_TRANSFORM != 0).then_some(glyph_transform),
        brush_transform: (flags & RUN_BRUSH_TRANSFORM != 0).then_some(brush_transform),
        font_size,
        font_embolden: embolden,
        hint: flags & RUN_HINT != 0,
        normalized_coords,
        style,
        glyphs,
        stream_offsets,
    })
}

fn read_style(r: &mut Reader<'_>) -> Result<BrushStyle, WireError> {
    let kind = r.u32()?;
    let width = r.f64()?;
    let bits = r.u32()?;
    let miter_limit = r.f64()?;
    let dash_offset = r.f64()?;
    let dashes = r.count(8)?;
    if dashes > MAX_DASHES {
        return Err(WireError::OverLimit("dashes of a stroked glyph run"));
    }
    let pattern = (0..dashes)
        .map(|_| r.f64())
        .collect::<Result<Vec<_>, _>>()?;
    if bits & !0x00ff_ffff != 0 {
        return Err(WireError::Malformed("stroke"));
    }
    match kind {
        STYLE_FILL_NONZERO | STYLE_FILL_EVEN_ODD => {
            if dashes != 0 {
                return Err(WireError::Malformed("a fill with dashes"));
            }
            Ok(BrushStyle::Fill(if kind == STYLE_FILL_NONZERO {
                Fill::NonZero
            } else {
                Fill::EvenOdd
            }))
        }
        STYLE_STROKE => {
            let mut stroke = Stroke::new(width)
                .with_join(join_from(bits & 0xff)?)
                .with_start_cap(cap_from((bits >> 8) & 0xff)?)
                .with_end_cap(cap_from((bits >> 16) & 0xff)?)
                .with_miter_limit(miter_limit);
            if !pattern.is_empty() {
                stroke = stroke.with_dashes(dash_offset, pattern);
            }
            Ok(BrushStyle::Stroke(stroke))
        }
        _ => Err(WireError::Malformed("glyph run style")),
    }
}
