//! A self-contained file format for one recorded scene: its encoding streams
//! plus the fonts and images it references. Scenes recorded where the shell
//! runs can be replayed by a benchmark on another machine.

use std::io::{self, Read, Write};
use std::sync::Arc;

use vello_cpu::color::{AlphaColor, DynamicColor, Srgb};
use vello_cpu::kurbo::{Cap, Diagonal2, Join, Stroke};
use vello_cpu::peniko::{
    Blob, ColorStop, Extend, Fill, FontData, ImageAlphaType, ImageData, ImageFormat, Style,
};
use vello_encoding::{
    DrawTag, Encoding, FontEmbolden, Glyph, GlyphRun, Patch, PathTag, StreamOffsets,
    Style as EncStyle, Transform,
};

const MAGIC: &[u8; 8] = b"TILOSCN1";

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

struct Out<W: Write>(W);

impl<W: Write> Out<W> {
    fn u8(&mut self, v: u8) -> io::Result<()> {
        self.0.write_all(&[v])
    }
    fn u32(&mut self, v: u32) -> io::Result<()> {
        self.0.write_all(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> io::Result<()> {
        self.0.write_all(&v.to_le_bytes())
    }
    fn f32(&mut self, v: f32) -> io::Result<()> {
        self.u32(v.to_bits())
    }
    fn f64(&mut self, v: f64) -> io::Result<()> {
        self.u64(v.to_bits())
    }
    fn len(&mut self, v: usize) -> io::Result<()> {
        self.u64(v as u64)
    }
    fn words(&mut self, v: &[u32]) -> io::Result<()> {
        self.len(v.len())?;
        v.iter().try_for_each(|w| self.u32(*w))
    }
    fn bytes(&mut self, v: &[u8]) -> io::Result<()> {
        self.len(v.len())?;
        self.0.write_all(v)
    }
    fn transform(&mut self, t: &Transform) -> io::Result<()> {
        t.matrix
            .iter()
            .chain(&t.translation)
            .try_for_each(|v| self.f32(*v))
    }
    fn opt_transform(&mut self, t: Option<&Transform>) -> io::Result<()> {
        match t {
            Some(t) => {
                self.u8(1)?;
                self.transform(t)
            }
            None => self.u8(0),
        }
    }
    fn range(&mut self, r: &std::ops::Range<usize>) -> io::Result<()> {
        self.len(r.start)?;
        self.len(r.end)
    }
}

struct In<R: Read>(R);

impl<R: Read> In<R> {
    fn array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut b = [0; N];
        self.0.read_exact(&mut b)?;
        Ok(b)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn len(&mut self) -> io::Result<usize> {
        usize::try_from(self.u64()?).map_err(|_| invalid("length"))
    }
    fn words(&mut self) -> io::Result<Vec<u32>> {
        let n = self.len()?;
        (0..n).map(|_| self.u32()).collect()
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.len()?;
        let mut v = vec![0; n];
        self.0.read_exact(&mut v)?;
        Ok(v)
    }
    fn transform(&mut self) -> io::Result<Transform> {
        Ok(Transform {
            matrix: [self.f32()?, self.f32()?, self.f32()?, self.f32()?],
            translation: [self.f32()?, self.f32()?],
        })
    }
    fn opt_transform(&mut self) -> io::Result<Option<Transform>> {
        Ok(match self.u8()? {
            0 => None,
            _ => Some(self.transform()?),
        })
    }
    fn range(&mut self) -> io::Result<std::ops::Range<usize>> {
        Ok(self.len()?..self.len()?)
    }
}

fn join_bits(join: Join) -> u8 {
    match join {
        Join::Bevel => 0,
        Join::Miter => 1,
        Join::Round => 2,
    }
}

fn join_from(bits: u8) -> Join {
    match bits {
        0 => Join::Bevel,
        1 => Join::Miter,
        _ => Join::Round,
    }
}

fn cap_bits(cap: Cap) -> u8 {
    match cap {
        Cap::Butt => 0,
        Cap::Square => 1,
        Cap::Round => 2,
    }
}

fn cap_from(bits: u8) -> Cap {
    match bits {
        0 => Cap::Butt,
        1 => Cap::Square,
        _ => Cap::Round,
    }
}

fn extend_bits(extend: Extend) -> u8 {
    match extend {
        Extend::Pad => 0,
        Extend::Repeat => 1,
        Extend::Reflect => 2,
    }
}

fn extend_from(bits: u8) -> Extend {
    match bits {
        1 => Extend::Repeat,
        2 => Extend::Reflect,
        _ => Extend::Pad,
    }
}

/// Write `encoding` and everything it references to `out`.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write(encoding: &Encoding, out: impl Write) -> io::Result<()> {
    let mut o = Out(io::BufWriter::new(out));
    o.0.write_all(MAGIC)?;
    o.bytes(&encoding.path_tags.iter().map(|t| t.0).collect::<Vec<_>>())?;
    o.words(&encoding.path_data)?;
    o.words(&encoding.draw_tags.iter().map(|t| t.0).collect::<Vec<_>>())?;
    o.words(&encoding.draw_data)?;
    o.len(encoding.transforms.len())?;
    encoding
        .transforms
        .iter()
        .try_for_each(|t| o.transform(t))?;
    o.len(encoding.styles.len())?;
    for s in &encoding.styles {
        o.u32(s.flags_and_miter_limit)?;
        o.f32(s.line_width)?;
    }
    for v in [
        encoding.n_paths,
        encoding.n_path_segments,
        encoding.n_clips,
        encoding.n_open_clips,
        encoding.flags,
    ] {
        o.u32(v)?;
    }

    let r = &encoding.resources;
    o.len(r.color_stops.len())?;
    for stop in &r.color_stops {
        o.f32(stop.offset)?;
        let c = stop.color.to_alpha_color::<Srgb>();
        c.components.iter().try_for_each(|v| o.f32(*v))?;
    }
    o.len(r.glyphs.len())?;
    for g in &r.glyphs {
        o.u32(g.id)?;
        o.f32(g.x)?;
        o.f32(g.y)?;
    }
    o.len(r.normalized_coords.len())?;
    r.normalized_coords
        .iter()
        .try_for_each(|c| o.u32(u32::from(*c as u16)))?;

    // Fonts and images, deduplicated by blob.
    let mut fonts: Vec<&FontData> = Vec::new();
    let mut images: Vec<&ImageData> = Vec::new();
    let mut font_of = Vec::with_capacity(r.glyph_runs.len());
    for run in &r.glyph_runs {
        let ix = fonts
            .iter()
            .position(|f| f.data.id() == run.font.data.id() && f.index == run.font.index)
            .unwrap_or_else(|| {
                fonts.push(&run.font);
                fonts.len() - 1
            });
        font_of.push(ix);
    }
    let mut image_of = Vec::new();
    for patch in &r.patches {
        if let Patch::Image { image, .. } = patch {
            let ix = images
                .iter()
                .position(|i| i.data.id() == image.data.id())
                .unwrap_or_else(|| {
                    images.push(image);
                    images.len() - 1
                });
            image_of.push(ix);
        }
    }
    o.len(fonts.len())?;
    for f in &fonts {
        o.u32(f.index)?;
        o.bytes(f.data.data())?;
    }
    o.len(images.len())?;
    for i in &images {
        o.u8(u8::from(i.format == ImageFormat::Bgra8))?;
        o.u8(u8::from(i.alpha_type == ImageAlphaType::AlphaPremultiplied))?;
        o.u32(i.width)?;
        o.u32(i.height)?;
        o.bytes(i.data.data())?;
    }

    o.len(r.glyph_runs.len())?;
    for (run, font) in r.glyph_runs.iter().zip(&font_of) {
        o.len(*font)?;
        o.transform(&run.transform)?;
        o.opt_transform(run.glyph_transform.as_ref())?;
        o.opt_transform(run.brush_transform.as_ref())?;
        o.f32(run.font_size)?;
        o.f64(run.font_embolden.amount.xx)?;
        o.f64(run.font_embolden.amount.yy)?;
        o.u8(join_bits(run.font_embolden.join))?;
        o.f64(run.font_embolden.miter_limit)?;
        o.f64(run.font_embolden.tolerance)?;
        o.u8(u8::from(run.hint))?;
        o.range(&run.normalized_coords)?;
        match &run.style {
            Style::Fill(fill) => {
                o.u8(0)?;
                o.u8(u8::from(*fill == Fill::EvenOdd))?;
            }
            Style::Stroke(s) => {
                o.u8(1)?;
                o.f64(s.width)?;
                o.u8(join_bits(s.join))?;
                o.f64(s.miter_limit)?;
                o.u8(cap_bits(s.start_cap))?;
                o.u8(cap_bits(s.end_cap))?;
                o.len(s.dash_pattern.len())?;
                s.dash_pattern.iter().try_for_each(|d| o.f64(*d))?;
                o.f64(s.dash_offset)?;
            }
        }
        o.range(&run.glyphs)?;
        let so = &run.stream_offsets;
        for v in [
            so.path_tags,
            so.path_data,
            so.draw_tags,
            so.draw_data,
            so.transforms,
            so.styles,
        ] {
            o.len(v)?;
        }
    }

    o.len(r.patches.len())?;
    let mut images_seen = 0;
    for patch in &r.patches {
        match patch {
            Patch::Ramp {
                draw_data_offset,
                stops,
                extend,
            } => {
                o.u8(0)?;
                o.len(*draw_data_offset)?;
                o.range(stops)?;
                o.u8(extend_bits(*extend))?;
            }
            Patch::GlyphRun { index } => {
                o.u8(1)?;
                o.len(*index)?;
            }
            Patch::Image {
                draw_data_offset, ..
            } => {
                o.u8(2)?;
                o.len(*draw_data_offset)?;
                o.len(image_of.get(images_seen).copied().unwrap_or_default())?;
                images_seen += 1;
            }
        }
    }
    o.0.flush()
}

/// Read a scene written by [`write`].
///
/// # Errors
///
/// Returns the reader's error, or `InvalidData` for anything malformed.
pub fn read(input: impl Read) -> io::Result<Encoding> {
    let mut i = In(io::BufReader::new(input));
    if &i.array::<8>()? != MAGIC {
        return Err(invalid("not a scene capture"));
    }
    let mut e = Encoding::new();
    e.path_tags = i.bytes()?.into_iter().map(PathTag).collect();
    e.path_data = i.words()?;
    e.draw_tags = i.words()?.into_iter().map(DrawTag).collect();
    e.draw_data = i.words()?;
    let n = i.len()?;
    e.transforms = (0..n).map(|_| i.transform()).collect::<io::Result<_>>()?;
    let n = i.len()?;
    e.styles = (0..n)
        .map(|_| {
            Ok(EncStyle {
                flags_and_miter_limit: i.u32()?,
                line_width: i.f32()?,
            })
        })
        .collect::<io::Result<_>>()?;
    e.n_paths = i.u32()?;
    e.n_path_segments = i.u32()?;
    e.n_clips = i.u32()?;
    e.n_open_clips = i.u32()?;
    e.flags = i.u32()?;

    let r = &mut e.resources;
    let n = i.len()?;
    r.color_stops = (0..n)
        .map(|_| {
            let offset = i.f32()?;
            let c = [i.f32()?, i.f32()?, i.f32()?, i.f32()?];
            Ok(ColorStop {
                offset,
                color: DynamicColor::from_alpha_color(AlphaColor::<Srgb>::new(c)),
            })
        })
        .collect::<io::Result<_>>()?;
    let n = i.len()?;
    r.glyphs = (0..n)
        .map(|_| {
            Ok(Glyph {
                id: i.u32()?,
                x: i.f32()?,
                y: i.f32()?,
            })
        })
        .collect::<io::Result<_>>()?;
    let n = i.len()?;
    r.normalized_coords = (0..n)
        .map(|_| Ok(i.u32()? as u16 as i16))
        .collect::<io::Result<_>>()?;

    let n = i.len()?;
    let fonts: Vec<FontData> = (0..n)
        .map(|_| {
            let index = i.u32()?;
            let data = i.bytes()?;
            Ok(FontData::new(Blob::new(Arc::new(data)), index))
        })
        .collect::<io::Result<_>>()?;
    let n = i.len()?;
    let images: Vec<ImageData> = (0..n)
        .map(|_| {
            let bgra = i.u8()? != 0;
            let premultiplied = i.u8()? != 0;
            let width = i.u32()?;
            let height = i.u32()?;
            let data = i.bytes()?;
            Ok(ImageData {
                data: Blob::new(Arc::new(data)),
                format: if bgra {
                    ImageFormat::Bgra8
                } else {
                    ImageFormat::Rgba8
                },
                alpha_type: if premultiplied {
                    ImageAlphaType::AlphaPremultiplied
                } else {
                    ImageAlphaType::Alpha
                },
                width,
                height,
            })
        })
        .collect::<io::Result<_>>()?;

    let n = i.len()?;
    for _ in 0..n {
        let font = fonts
            .get(i.len()?)
            .cloned()
            .ok_or_else(|| invalid("font index"))?;
        let transform = i.transform()?;
        let glyph_transform = i.opt_transform()?;
        let brush_transform = i.opt_transform()?;
        let font_size = i.f32()?;
        let font_embolden = FontEmbolden {
            amount: Diagonal2::new(i.f64()?, i.f64()?),
            join: join_from(i.u8()?),
            miter_limit: i.f64()?,
            tolerance: i.f64()?,
        };
        let hint = i.u8()? != 0;
        let normalized_coords = i.range()?;
        let style = if i.u8()? == 0 {
            Style::Fill(if i.u8()? == 0 {
                Fill::NonZero
            } else {
                Fill::EvenOdd
            })
        } else {
            let width = i.f64()?;
            let join = join_from(i.u8()?);
            let miter_limit = i.f64()?;
            let start_cap = cap_from(i.u8()?);
            let end_cap = cap_from(i.u8()?);
            let dashes = i.len()?;
            let pattern = (0..dashes)
                .map(|_| i.f64())
                .collect::<io::Result<Vec<_>>>()?;
            let offset = i.f64()?;
            let mut stroke = Stroke::new(width)
                .with_join(join)
                .with_miter_limit(miter_limit)
                .with_start_cap(start_cap)
                .with_end_cap(end_cap);
            if !pattern.is_empty() {
                stroke = stroke.with_dashes(offset, pattern);
            }
            Style::Stroke(stroke)
        };
        let glyphs = i.range()?;
        let stream_offsets = StreamOffsets {
            path_tags: i.len()?,
            path_data: i.len()?,
            draw_tags: i.len()?,
            draw_data: i.len()?,
            transforms: i.len()?,
            styles: i.len()?,
        };
        r.glyph_runs.push(GlyphRun {
            font,
            transform,
            glyph_transform,
            brush_transform,
            font_size,
            font_embolden,
            hint,
            normalized_coords,
            style,
            glyphs,
            stream_offsets,
        });
    }

    let n = i.len()?;
    for _ in 0..n {
        let patch = match i.u8()? {
            0 => Patch::Ramp {
                draw_data_offset: i.len()?,
                stops: i.range()?,
                extend: extend_from(i.u8()?),
            },
            1 => Patch::GlyphRun { index: i.len()? },
            _ => {
                let draw_data_offset = i.len()?;
                let image = images
                    .get(i.len()?)
                    .cloned()
                    .ok_or_else(|| invalid("image index"))?;
                Patch::Image {
                    draw_data_offset,
                    image,
                }
            }
        };
        r.patches.push(patch);
    }
    Ok(e)
}
