//! The CPU pass every scene passes before the GPU sees it.
//!
//! The render service draws every app of a room on one device. A scene that
//! makes vello's CPU stages panic takes the room's rendering down; one that
//! makes its shaders read the wrong data draws another app's pixels or hangs
//! the GPU for everyone. So a decoded [`Encoding`] is checked against exactly
//! the invariants vello's own encoder establishes and its resolver and
//! shaders rely on:
//!
//! - **Streams agree with each other.** Every path tag is a tag vello writes;
//!   the path data holds exactly the points the segment tags consume; there
//!   is one transform per transform tag, one style per style tag, one draw
//!   object per path (glyph runs bring their paths at resolve), and the draw
//!   data holds exactly what the draw tags say.
//! - **Every late-bound resource is where its draw is.** A gradient's ramp
//!   and an image's atlas slot are written by the resolver at the offset its
//!   patch names, so every gradient and image draw has exactly one patch at
//!   its own offset, in stream order, and an image draw's size is the
//!   registered image's: a scene cannot point the shader at another app's
//!   pixels in the shared atlas. Glyph runs are referenced once each, in
//!   stream order, with their glyph and coordinate ranges in bounds.
//! - **Numbers are finite and bounded.** Coordinates, transforms, stroke
//!   widths, gradient geometry, layer alpha and glyph sizes.
//! - **Clips nest.** Begin and end balance, never deeper than the limit, and
//!   a layer leaves none open.
//! - **Work is bounded** by the app's [`Limits`]: paths, segments, draw
//!   objects, glyphs.
//!
//! The pass is O(encoded size) and allocates nothing proportional to it
//! beyond a per-draw marker.

use vello_encoding::{DrawTag, Encoding, Patch, PathTag, Style, Transform};

use crate::{Limits, WireError};

/// Largest coordinate magnitude a scene may encode. Far beyond any surface;
/// small enough that tile arithmetic on the GPU stays exact.
pub const MAX_COORDINATE: f32 = 1.0e7;

/// Largest glyph size, in pixels per em.
pub const MAX_FONT_SIZE: f32 = 16_384.0;

/// Variation coordinates of one glyph run (a font has at most a few dozen axes).
pub const MAX_RUN_COORDS: usize = 64;

/// What a valid scene costs, for the service's per-frame budget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneCost {
    pub paths: u32,
    pub segments: u64,
    pub draw_objects: u32,
    pub glyphs: u32,
    pub clip_depth: u32,
}

impl SceneCost {
    /// Both costs together (a frame's layers and backdrops).
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            paths: self.paths.saturating_add(other.paths),
            segments: self.segments.saturating_add(other.segments),
            draw_objects: self.draw_objects.saturating_add(other.draw_objects),
            glyphs: self.glyphs.saturating_add(other.glyphs),
            clip_depth: self.clip_depth.max(other.clip_depth),
        }
    }

    /// The first limit this cost breaks.
    ///
    /// # Errors
    ///
    /// Names the limit that was exceeded.
    pub fn check(&self, limits: &Limits) -> Result<(), WireError> {
        if self.paths > limits.paths {
            return Err(WireError::OverLimit("paths"));
        }
        if self.segments > u64::from(limits.segments) {
            return Err(WireError::OverLimit("segments"));
        }
        if self.draw_objects > limits.draw_objects {
            return Err(WireError::OverLimit("draw objects"));
        }
        if self.glyphs > limits.glyphs {
            return Err(WireError::OverLimit("glyphs"));
        }
        if self.clip_depth > limits.clip_depth {
            return Err(WireError::OverLimit("clip depth"));
        }
        Ok(())
    }
}

/// Segments a glyph's outline is reckoned at before it is expanded, for the
/// segment budget. Text glyphs are tens; a pathological outline is bounded by
/// the glyph cache and vello's scratch budget, not by this estimate.
const SEGMENTS_PER_GLYPH: u64 = 64;

/// Check `encoding` and return what it costs. `limits` bounds the cost.
///
/// # Errors
///
/// The first invariant the encoding breaks.
pub fn validate(encoding: &Encoding, limits: &Limits) -> Result<SceneCost, WireError> {
    let paths = check_path_streams(encoding)?;
    let draws = check_draw_stream(encoding, limits)?;
    check_patches(encoding, &draws)?;
    check_glyph_runs(encoding)?;
    check_transforms(&encoding.transforms)?;
    check_styles(&encoding.styles)?;
    check_stops(encoding)?;
    if encoding.flags & !(Encoding::FORCE_NEXT_TRANSFORM | Encoding::FORCE_NEXT_STYLE) != 0 {
        return Err(WireError::Invalid("unknown encoding flags"));
    }
    let glyphs = encoding.resources.glyphs.len();
    let cost = SceneCost {
        paths: paths
            .paths
            .saturating_add(u32::try_from(encoding.resources.glyph_runs.len()).unwrap_or(u32::MAX)),
        segments: u64::from(paths.segments)
            .saturating_add((glyphs as u64).saturating_mul(SEGMENTS_PER_GLYPH)),
        draw_objects: u32::try_from(encoding.draw_tags.len()).unwrap_or(u32::MAX),
        glyphs: u32::try_from(glyphs).unwrap_or(u32::MAX),
        clip_depth: draws.max_depth,
    };
    cost.check(limits)?;
    Ok(cost)
}

struct PathCounts {
    paths: u32,
    segments: u32,
}

fn finite_coordinate(value: f32) -> bool {
    value.is_finite() && value.abs() <= MAX_COORDINATE
}

fn finite_transform(transform: &Transform) -> bool {
    transform
        .matrix
        .iter()
        .chain(transform.translation.iter())
        .all(|value| finite_coordinate(*value))
}

fn check_transforms(transforms: &[Transform]) -> Result<(), WireError> {
    if transforms.iter().all(finite_transform) {
        Ok(())
    } else {
        Err(WireError::Invalid("a transform is not finite"))
    }
}

/// The path tags, path data, transform and style counts, and the number of
/// paths and segments.
fn check_path_streams(encoding: &Encoding) -> Result<PathCounts, WireError> {
    let data = &encoding.path_data;
    let mut offset = 0_usize;
    let mut paths = 0_u32;
    let mut segments = 0_u32;
    let mut transforms = 0_usize;
    let mut styles = 0_usize;
    for tag in &encoding.path_tags {
        let bits = tag.0;
        if bits == PathTag::PATH.0 {
            paths = paths.checked_add(1).ok_or(WireError::Overflow)?;
            continue;
        }
        if bits == PathTag::TRANSFORM.0 {
            transforms = transforms.checked_add(1).ok_or(WireError::Overflow)?;
            continue;
        }
        if bits == PathTag::STYLE.0 {
            styles = styles.checked_add(1).ok_or(WireError::Overflow)?;
            continue;
        }
        // A segment: two type bits, the subpath-end and f32 bits, nothing else.
        if bits & !0x0f != 0 || !tag.is_path_segment() {
            return Err(WireError::Invalid("unknown path tag"));
        }
        segments = segments.checked_add(1).ok_or(WireError::Overflow)?;
        let points = usize::from(tag.path_segment_type().0);
        let words_per_point = if tag.is_f32() { 2 } else { 1 };
        // The segment reads its start point at `offset` and its own points
        // after it; the last of them is the next segment's start.
        let read = points
            .checked_add(1)
            .and_then(|read| read.checked_mul(words_per_point))
            .ok_or(WireError::Overflow)?;
        let end = offset.checked_add(read).ok_or(WireError::Overflow)?;
        let words = data
            .get(offset..end)
            .ok_or(WireError::Invalid("path data shorter than its segments"))?;
        if tag.is_f32()
            && !words
                .iter()
                .all(|word| finite_coordinate(f32::from_bits(*word)))
        {
            return Err(WireError::Invalid("a path coordinate is not finite"));
        }
        let advance = points
            .checked_add(usize::from(tag.is_subpath_end()))
            .and_then(|advance| advance.checked_mul(words_per_point))
            .ok_or(WireError::Overflow)?;
        offset = offset.checked_add(advance).ok_or(WireError::Overflow)?;
    }
    if offset != data.len() {
        return Err(WireError::Invalid("path data does not match its segments"));
    }
    if transforms != encoding.transforms.len() {
        return Err(WireError::Invalid(
            "transform count does not match its tags",
        ));
    }
    if styles != encoding.styles.len() {
        return Err(WireError::Invalid("style count does not match its tags"));
    }
    if paths != encoding.n_paths {
        return Err(WireError::Invalid("path count does not match its tags"));
    }
    if segments != encoding.n_path_segments {
        return Err(WireError::Invalid("segment count does not match its tags"));
    }
    Ok(PathCounts { paths, segments })
}

/// What kind of late-bound data a draw object's first data word waits for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Ramp,
    Image { width_height: u32 },
}

struct Draws {
    /// Draw data offset (in words) of every draw whose first word a patch
    /// fills, in stream order.
    slots: Vec<(usize, Slot)>,
    max_depth: u32,
}

/// Words of draw data a draw tag carries, or `None` for a tag vello never
/// writes.
fn draw_words(tag: DrawTag) -> Option<usize> {
    let known = [
        DrawTag::COLOR,
        DrawTag::LINEAR_GRADIENT,
        DrawTag::RADIAL_GRADIENT,
        DrawTag::SWEEP_GRADIENT,
        DrawTag::IMAGE,
        DrawTag::BLUR_RECT,
        DrawTag::BEGIN_CLIP,
        DrawTag::END_CLIP,
    ];
    known
        .contains(&tag)
        .then_some(((tag.0 >> 2) & 0x7) as usize)
}

fn floats_finite(words: &[u32]) -> bool {
    words
        .iter()
        .all(|word| finite_coordinate(f32::from_bits(*word)))
}

fn valid_blend(mode: u32) -> bool {
    use vello_encoding::DrawBeginClip;
    if mode == DrawBeginClip::LUMINANCE_MASK_BLEND_MODE || mode == DrawBeginClip::CLIP_BLEND_MODE {
        return true;
    }
    let mix = mode >> 8;
    let compose = mode & 0xff;
    mix <= 15 && compose <= 13
}

fn check_draw_stream(encoding: &Encoding, limits: &Limits) -> Result<Draws, WireError> {
    let data = &encoding.draw_data;
    let mut offset = 0_usize;
    let mut depth = 0_u32;
    let mut max_depth = 0_u32;
    let mut clips = 0_u32;
    let mut slots = Vec::new();
    for tag in &encoding.draw_tags {
        let words = draw_words(*tag).ok_or(WireError::Invalid("unknown draw tag"))?;
        let end = offset.checked_add(words).ok_or(WireError::Overflow)?;
        let body = data
            .get(offset..end)
            .ok_or(WireError::Invalid("draw data shorter than its tags"))?;
        match *tag {
            DrawTag::LINEAR_GRADIENT | DrawTag::RADIAL_GRADIENT | DrawTag::SWEEP_GRADIENT => {
                if !floats_finite(body.get(1..).unwrap_or_default()) {
                    return Err(WireError::Invalid("gradient geometry is not finite"));
                }
                slots.push((offset, Slot::Ramp));
            }
            DrawTag::IMAGE => {
                let width_height = *body.get(1).ok_or(WireError::Invalid("image draw data"))?;
                slots.push((offset, Slot::Image { width_height }));
            }
            DrawTag::BLUR_RECT => {
                if !floats_finite(body.get(1..).unwrap_or_default()) {
                    return Err(WireError::Invalid("blurred rectangle is not finite"));
                }
            }
            DrawTag::BEGIN_CLIP => {
                let (Some(mode), Some(alpha)) = (body.first(), body.get(1)) else {
                    return Err(WireError::Invalid("clip draw data"));
                };
                let alpha = f32::from_bits(*alpha);
                if !valid_blend(*mode) || !(0.0..=1.0).contains(&alpha) {
                    return Err(WireError::Invalid("layer blend or alpha"));
                }
                depth = depth.checked_add(1).ok_or(WireError::Overflow)?;
                max_depth = max_depth.max(depth);
                if depth > limits.clip_depth {
                    return Err(WireError::OverLimit("clip depth"));
                }
                clips = clips.checked_add(1).ok_or(WireError::Overflow)?;
            }
            DrawTag::END_CLIP => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(WireError::Invalid("a clip ends that never began"))?;
                clips = clips.checked_add(1).ok_or(WireError::Overflow)?;
            }
            _ => {}
        }
        offset = end;
    }
    if offset != data.len() {
        return Err(WireError::Invalid("draw data does not match its tags"));
    }
    if depth != 0 || encoding.n_open_clips != 0 {
        return Err(WireError::Invalid("a clip is left open"));
    }
    if clips != encoding.n_clips {
        return Err(WireError::Invalid("clip count does not match its tags"));
    }
    // One draw object per path; a glyph run's paths arrive at resolve.
    let objects = usize::try_from(encoding.n_paths)
        .ok()
        .and_then(|paths| paths.checked_add(encoding.resources.glyph_runs.len()))
        .ok_or(WireError::Overflow)?;
    if objects != encoding.draw_tags.len() {
        return Err(WireError::Invalid("draw objects do not match paths"));
    }
    Ok(Draws { slots, max_depth })
}

fn check_patches(encoding: &Encoding, draws: &Draws) -> Result<(), WireError> {
    let res = &encoding.resources;
    let mut slots = draws.slots.iter();
    let mut next_run = 0_usize;
    let mut run_offsets = [0_usize; 6];
    for patch in &res.patches {
        match patch {
            Patch::Ramp {
                draw_data_offset,
                stops,
                ..
            } => {
                let Some((offset, Slot::Ramp)) = slots.next() else {
                    return Err(WireError::Invalid("a ramp patch without its gradient"));
                };
                if offset != draw_data_offset {
                    return Err(WireError::Invalid("a ramp patch away from its gradient"));
                }
                if stops.start >= stops.end || res.color_stops.get(stops.clone()).is_none() {
                    return Err(WireError::Invalid("gradient stops out of range"));
                }
            }
            Patch::Image {
                draw_data_offset,
                image,
            } => {
                let Some((offset, Slot::Image { width_height })) = slots.next() else {
                    return Err(WireError::Invalid("an image patch without its draw"));
                };
                if offset != draw_data_offset {
                    return Err(WireError::Invalid("an image patch away from its draw"));
                }
                let expected = (image.width < 1 << 16 && image.height < 1 << 16)
                    .then_some((image.width << 16) | image.height);
                if expected != Some(*width_height) {
                    return Err(WireError::Invalid(
                        "an image draw's size is not its image's",
                    ));
                }
                let pixels = image
                    .format
                    .size_in_bytes(image.width, image.height)
                    .ok_or(WireError::Overflow)?;
                if image.data.data().len() < pixels {
                    return Err(WireError::Invalid("image data shorter than its size"));
                }
            }
            Patch::GlyphRun { index } => {
                if *index != next_run {
                    return Err(WireError::Invalid("glyph runs out of order"));
                }
                next_run = next_run.checked_add(1).ok_or(WireError::Overflow)?;
                let run = res
                    .glyph_runs
                    .get(*index)
                    .ok_or(WireError::Invalid("a glyph run patch without its run"))?;
                let offsets = &run.stream_offsets;
                let now = [
                    offsets.path_tags,
                    offsets.path_data,
                    offsets.draw_tags,
                    offsets.draw_data,
                    offsets.transforms,
                    offsets.styles,
                ];
                let lengths = [
                    encoding.path_tags.len(),
                    encoding.path_data.len(),
                    encoding.draw_tags.len(),
                    encoding.draw_data.len(),
                    encoding.transforms.len(),
                    encoding.styles.len(),
                ];
                for ((now, before), length) in now.iter().zip(run_offsets).zip(lengths) {
                    if *now < before || *now > length {
                        return Err(WireError::Invalid("glyph run stream offsets"));
                    }
                }
                run_offsets = now;
            }
        }
    }
    if slots.next().is_some() {
        return Err(WireError::Invalid(
            "a gradient or image draw without its patch",
        ));
    }
    if next_run != res.glyph_runs.len() {
        return Err(WireError::Invalid("a glyph run without its patch"));
    }
    Ok(())
}

fn check_glyph_runs(encoding: &Encoding) -> Result<(), WireError> {
    let res = &encoding.resources;
    for run in &res.glyph_runs {
        if run.glyphs.start > run.glyphs.end || res.glyphs.get(run.glyphs.clone()).is_none() {
            return Err(WireError::Invalid("glyph range out of bounds"));
        }
        let coords = run.normalized_coords.clone();
        if coords.start > coords.end
            || res.normalized_coords.get(coords.clone()).is_none()
            || coords.len() > MAX_RUN_COORDS
        {
            return Err(WireError::Invalid("variation coordinates out of bounds"));
        }
        if !(run.font_size.is_finite() && run.font_size > 0.0 && run.font_size <= MAX_FONT_SIZE) {
            return Err(WireError::Invalid("font size"));
        }
        let transforms_finite = finite_transform(&run.transform)
            && run.glyph_transform.as_ref().is_none_or(finite_transform)
            && run.brush_transform.as_ref().is_none_or(finite_transform);
        if !transforms_finite {
            return Err(WireError::Invalid("a glyph run transform is not finite"));
        }
        let embolden = &run.font_embolden;
        let bounded =
            |value: f64, low: f64, high: f64| value.is_finite() && (low..=high).contains(&value);
        if !(bounded(embolden.amount.xx, -1.0e3, 1.0e3)
            && bounded(embolden.amount.yy, -1.0e3, 1.0e3)
            && bounded(embolden.miter_limit, 0.0, 1.0e3)
            && bounded(embolden.tolerance, 0.0, 1.0e3))
        {
            return Err(WireError::Invalid("glyph embolden"));
        }
        if let peniko::Style::Stroke(stroke) = &run.style {
            let dashes_ok = stroke
                .dash_pattern
                .iter()
                .all(|dash| bounded(*dash, 0.0, 1.0e6))
                && (stroke.dash_pattern.is_empty()
                    || stroke.dash_pattern.iter().sum::<f64>() > 1.0e-3);
            if !(bounded(stroke.width, 0.0, 1.0e6)
                && bounded(stroke.miter_limit, 0.0, 1.0e3)
                && bounded(stroke.dash_offset, -1.0e6, 1.0e6)
                && dashes_ok)
            {
                return Err(WireError::Invalid("glyph stroke"));
            }
        }
        if !res
            .glyphs
            .get(run.glyphs.clone())
            .unwrap_or_default()
            .iter()
            .all(|glyph| finite_coordinate(glyph.x) && finite_coordinate(glyph.y))
        {
            return Err(WireError::Invalid("a glyph position is not finite"));
        }
    }
    Ok(())
}

/// Style flag bits vello defines: fill or stroke, fill rule, join, caps and
/// the half-float miter limit.
const STYLE_BITS: u32 = Style::FLAGS_STYLE_BIT
    | Style::FLAGS_FILL_BIT
    | Style::FLAGS_JOIN_MASK
    | Style::FLAGS_START_CAP_MASK
    | Style::FLAGS_END_CAP_MASK
    | Style::MITER_LIMIT_MASK;

fn check_styles(styles: &[Style]) -> Result<(), WireError> {
    for style in styles {
        let flags = style.flags_and_miter_limit;
        let width = style.line_width;
        let join = flags & Style::FLAGS_JOIN_MASK;
        let caps_known = (flags & Style::FLAGS_START_CAP_MASK) != Style::FLAGS_START_CAP_MASK
            && (flags & Style::FLAGS_END_CAP_MASK) != Style::FLAGS_END_CAP_MASK;
        if flags & !STYLE_BITS != 0
            || join == Style::FLAGS_JOIN_MASK
            || !caps_known
            || !(width.is_finite() && (0.0..=1.0e6).contains(&width))
        {
            return Err(WireError::Invalid("style"));
        }
    }
    Ok(())
}

fn check_stops(encoding: &Encoding) -> Result<(), WireError> {
    for stop in &encoding.resources.color_stops {
        let valid = stop.offset.is_finite()
            && (0.0..=1.0).contains(&stop.offset)
            && stop.color.components.iter().all(|value| value.is_finite());
        if !valid {
            return Err(WireError::Invalid("gradient stop"));
        }
    }
    Ok(())
}
