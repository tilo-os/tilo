//! Glyph outlines, built once and kept across frames.
//!
//! Outlines are drawn with skrifa exactly as `vello_encoding`'s glyph cache
//! draws them (same size, variation coordinates, hinting options and
//! synthetic emboldening), in pixel units with y up; a glyph's placement
//! transform flips them into the scene.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, HintingInstance, HintingOptions, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use smallvec::SmallVec;
use vello_cpu::kurbo::{BezPath, Diagonal2, Join, Point, Rect};
use vello_cpu::peniko::{FontData, Style};
use vello_encoding::FontEmbolden;

/// A glyph outline in pixels, y up.
#[derive(Debug)]
pub(crate) struct Outline {
    pub path: BezPath,
    pub bbox: Rect,
}

/// Everything besides the glyph id that shapes an outline.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct GlyphKey {
    font_id: u64,
    font_index: u32,
    coords: SmallVec<[i16; 8]>,
    size_bits: u32,
    embolden: [u32; 4],
    embolden_join: u8,
    hint: bool,
    style: [u32; 2],
}

impl GlyphKey {
    pub fn run(
        font: &FontData,
        coords: &[i16],
        size: f32,
        embolden: &FontEmbolden,
        hint: bool,
        style: &Style,
    ) -> (Self, FontData, FontEmbolden) {
        let style_bits = match style {
            Style::Fill(fill) => vello_encoding::Style::from_fill(*fill),
            Style::Stroke(stroke) => vello_encoding::Style::from_stroke(stroke).unwrap_or_default(),
        };
        let key = Self {
            font_id: font.data.id(),
            font_index: font.index,
            coords: coords.iter().copied().collect(),
            size_bits: size.to_bits(),
            embolden: [
                (embolden.amount.xx as f32).to_bits(),
                (embolden.amount.yy as f32).to_bits(),
                (embolden.miter_limit as f32).to_bits(),
                (embolden.tolerance as f32).to_bits(),
            ],
            embolden_join: match embolden.join {
                Join::Bevel => 0,
                Join::Miter => 1,
                Join::Round => 2,
            },
            hint,
            style: [
                style_bits.flags_and_miter_limit,
                style_bits.line_width.to_bits(),
            ],
        };
        (key, font.clone(), *embolden)
    }
}

/// The hinting instance of a run, built on its first outline.
enum Hinting {
    Untried,
    Failed,
    Ready(HintingInstance),
}

struct RunEntry {
    font: FontData,
    embolden: FontEmbolden,
    coords: SmallVec<[NormalizedCoord; 8]>,
    size: Size,
    hint: bool,
    glyphs: HashMap<u32, Option<Arc<Outline>>>,
    hinting: Hinting,
    last_used: u64,
}

/// Glyph outlines keyed by font instance and glyph id.
#[derive(Default)]
pub(crate) struct GlyphCache {
    runs: HashMap<GlyphKey, RunEntry>,
    frame: u64,
}

const MAX_IDLE_FRAMES: u64 = 120;

impl GlyphCache {
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn end_frame(&mut self) {
        if self.frame.is_multiple_of(32) {
            let frame = self.frame;
            self.runs
                .retain(|_, entry| frame - entry.last_used <= MAX_IDLE_FRAMES);
        }
    }

    pub fn session(
        &mut self,
        (key, font, embolden): (GlyphKey, FontData, FontEmbolden),
    ) -> Option<Session<'_>> {
        // A font that does not parse yields no session, like vello's resolver.
        FontRef::from_index(font.data.as_ref(), font.index).ok()?;
        let frame = self.frame;
        let entry = self.runs.entry(key).or_insert_with_key(|key| RunEntry {
            font,
            embolden,
            coords: key
                .coords
                .iter()
                .map(|&c| NormalizedCoord::from_bits(c))
                .collect(),
            size: Size::new(f32::from_bits(key.size_bits)),
            hint: key.hint,
            glyphs: HashMap::new(),
            hinting: Hinting::Untried,
            last_used: frame,
        });
        entry.last_used = frame;
        Some(Session { entry })
    }
}

pub(crate) struct Session<'a> {
    entry: &'a mut RunEntry,
}

impl Session<'_> {
    pub fn get(&mut self, glyph: u32) -> (Option<Arc<Outline>>, bool) {
        if let Some(outline) = self.entry.glyphs.get(&glyph) {
            return (outline.clone(), false);
        }
        let outline = build(self.entry, glyph).map(Arc::new);
        self.entry.glyphs.insert(glyph, outline.clone());
        (outline, true)
    }
}

/// `vello_encoding`'s hinting options, so hinted outlines match.
const HINTING: HintingOptions = HintingOptions {
    engine: skrifa::outline::Engine::AutoFallback,
    target: skrifa::outline::Target::Smooth {
        mode: skrifa::outline::SmoothMode::Lcd,
        symmetric_rendering: false,
        preserve_linear_metrics: true,
    },
};

struct Pen<'a>(&'a mut BezPath);

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(cx1), f64::from(cy1)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.0.close_path();
    }
}

fn build(entry: &mut RunEntry, glyph: u32) -> Option<Outline> {
    let font = FontRef::from_index(entry.font.data.as_ref(), entry.font.index).ok()?;
    let outlines = font.outline_glyphs();
    let outline = outlines.get(GlyphId::new(glyph))?;
    let location = LocationRef::new(&entry.coords);
    if entry.hint && matches!(entry.hinting, Hinting::Untried) {
        entry.hinting = HintingInstance::new(&outlines, entry.size, location, HINTING)
            .map_or(Hinting::Failed, Hinting::Ready);
    }
    let settings = match &entry.hinting {
        Hinting::Ready(instance) if entry.hint => DrawSettings::hinted(instance, false),
        _ => DrawSettings::unhinted(entry.size, location),
    };
    let mut path = BezPath::new();
    outline.draw(settings, &mut Pen(&mut path)).ok()?;
    let embolden = entry.embolden;
    if embolden.amount != Diagonal2::new(0.0, 0.0) {
        path = vello_cpu::kurbo::expand_path(
            &path,
            embolden.amount,
            embolden.join,
            embolden.miter_limit,
            embolden.tolerance,
        );
    }
    let bbox = if path.elements().is_empty() {
        Rect::from_points(Point::ZERO, Point::ZERO)
    } else {
        path.control_box()
    };
    Some(Outline { path, bbox })
}
