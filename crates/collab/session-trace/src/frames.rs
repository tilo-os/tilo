//! The pixel layer of a session: compositor frames of the recorded surfaces.
//!
//! Structure replays exactly; pixels are the check on that claim
//! (`docs/architecture/feedback.md`, "Two layers"). While a recording keeps
//! pixels, the compositor renders each recorded app surface — that surface
//! alone, no shell chrome and no other app — at a low base rate and on
//! damage, and hands it over as a [`SurfaceFrame`] stamped on the trace's
//! clock. The frame is masked before it is kept ([`crate::mask`]), stored
//! as a lossless PNG keyframe ([`StoredFrame`]) and bounded in a
//! [`FrameStrip`], which is the blob a `session` record references with
//! relation [`FRAMES_BLOB_RELATION`](crate::FRAMES_BLOB_RELATION).
//!
//! Frames are lossless on purpose: the divergence check
//! ([`crate::divergence`]) compares captured pixels with replayed ones, and
//! a lossy codec would make it compare codec noise.
//!
//! Replay produces the other side through [`FrameProvider`].

use std::hash::{Hash as _, Hasher as _};

use serde::{Deserialize, Serialize};
use tilo_actions::Sensitivity;
use tilo_reference::inspect::{InspectedNode, Pixels};

use crate::{Event, InspectKeyframe, Offset, Route, SessionTrace};

/// Media type of an encoded [`FrameStrip`].
pub const FRAMES_MIME: &str = "application/vnd.tilo.session-frames";
/// Layout version of an encoded [`FrameStrip`]. Readers refuse any other.
pub const FRAMES_FORMAT: u32 = 1;
/// Largest frame edge accepted, in pixels.
pub const MAX_FRAME_EDGE: u32 = 8_192;

/// Why frames could not be produced, stored or read.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("no replayed surface for {0:?}")]
    NoSurface(Route),
    #[error("replay cannot reach offset {0:?}")]
    Unreachable(Offset),
    #[error("{0}")]
    Render(String),
    #[error("frame is {width}×{height} but carries {len} bytes")]
    Malformed { width: u32, height: u32, len: usize },
    #[error("frame encoding failed: {0}")]
    Encode(String),
    #[error("frame strip format {0} is not supported")]
    UnsupportedFormat(u32),
}

/// One surface as it looked at one moment: the app's tile content alone,
/// with the surface origin at pixel `(0, 0)`.
///
/// `rgba` is tightly packed RGBA8, stride `width * 4`, exactly what the
/// toolkit's offscreen renderer reads back (sRGB-encoded values, vello's
/// premultiplied alpha; an opaque app surface is alpha 255 everywhere).
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceFrame {
    /// When, on the trace's clock.
    pub at: Offset,
    /// Which surface. Capture fills `app_id` and `room`; `record_id` is
    /// empty when the shell cannot name the record a tile shows. Compare
    /// with [`SurfaceFrame::same_surface`], not `==`.
    pub route: Route,
    pub width: u32,
    pub height: u32,
    /// Pixels per surface coordinate unit: Inspect bounds times `scale`
    /// index this frame.
    pub scale: f64,
    pub rgba: Vec<u8>,
}

impl SurfaceFrame {
    /// A frame whose buffer is checked against its size.
    ///
    /// # Errors
    ///
    /// [`FrameError::Malformed`] when the size is zero, above
    /// [`MAX_FRAME_EDGE`], or does not match `rgba`.
    pub fn new(
        at: Offset,
        route: Route,
        width: u32,
        height: u32,
        scale: f64,
        rgba: Vec<u8>,
    ) -> Result<Self, FrameError> {
        let frame = Self {
            at,
            route,
            width,
            height,
            scale,
            rgba,
        };
        frame.validate()?;
        Ok(frame)
    }

    /// # Errors
    ///
    /// [`FrameError::Malformed`] when the buffer does not match the size.
    pub fn validate(&self) -> Result<(), FrameError> {
        let expected = rgba_len(self.width, self.height);
        if self.width == 0
            || self.height == 0
            || self.width > MAX_FRAME_EDGE
            || self.height > MAX_FRAME_EDGE
            || expected != Some(self.rgba.len())
            || !self.scale.is_finite()
            || self.scale <= 0.0
        {
            return Err(FrameError::Malformed {
                width: self.width,
                height: self.height,
                len: self.rgba.len(),
            });
        }
        Ok(())
    }

    /// Whether `route` names this frame's surface: same app and room, and
    /// the same record when both name one.
    pub fn same_surface(&self, route: &Route) -> bool {
        same_surface(&self.route, route)
    }

    /// The whole frame as a rectangle.
    pub const fn bounds(&self) -> PixelRect {
        PixelRect::new(0, 0, self.width, self.height)
    }

    /// RGBA of pixel `(x, y)`, or `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = pixel_index(self.width, x, y)?;
        let bytes = self.rgba.get(index..index.checked_add(4)?)?;
        bytes.try_into().ok()
    }
}

/// Whether two routes name the same surface: same app and room, and the
/// same record when both name one.
pub fn same_surface(a: &Route, b: &Route) -> bool {
    a.app_id == b.app_id
        && a.room == b.room
        && (a.record_id.is_empty() || b.record_id.is_empty() || a.record_id == b.record_id)
}

pub(crate) fn rgba_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

pub(crate) fn pixel_index(width: u32, x: u32, y: u32) -> Option<usize> {
    usize::try_from(y)
        .ok()?
        .checked_mul(usize::try_from(width).ok()?)?
        .checked_add(usize::try_from(x).ok()?)?
        .checked_mul(4)
}

/// Produces the frames a replay rendered, for the divergence check.
///
/// [`crate::divergence::check`] calls [`FrameProvider::frames_at`] once per
/// step, in increasing offset order, so an implementation may fast-forward
/// and never has to rewind. Each returned frame must be rendered like a
/// captured one: the app surface alone at its recorded geometry. A surface
/// missing from the answer is reported as diverging, not as an error.
pub trait FrameProvider {
    /// Every replayed surface as it looks at `at`.
    ///
    /// # Errors
    ///
    /// When the replay cannot reach `at` or cannot render.
    fn frames_at(&mut self, at: Offset) -> Result<Vec<SurfaceFrame>, FrameError>;
}

/// An axis-aligned pixel rectangle, `x`/`y` inclusive, extent exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[must_use]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn right(&self) -> u32 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(&self) -> u32 {
        self.y.saturating_add(self.height)
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn area(&self) -> u64 {
        (self.width as u64).saturating_mul(self.height as u64)
    }

    /// Every pixel a surface-space rectangle touches, even partly, clipped
    /// to `within`. Rounding outward is what makes a mask exact: a pixel
    /// that shows any part of a sensitive node is masked.
    pub fn covering(rect: &SurfaceRect, scale: f64, within: Self) -> Self {
        let edge = |value: f64, round: fn(f64) -> f64| {
            let scaled = round(value * scale);
            if scaled.is_nan() || scaled <= 0.0 {
                0
            } else if scaled >= f64::from(u32::MAX) {
                u32::MAX
            } else {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to 0..u32::MAX just above"
                )]
                let pixel = scaled as u32;
                pixel
            }
        };
        let (left, right) = (rect.x, rect.x + rect.width);
        let (top, bottom) = (rect.y, rect.y + rect.height);
        let x0 = edge(left.min(right), f64::floor).max(within.x);
        let y0 = edge(top.min(bottom), f64::floor).max(within.y);
        let x1 = edge(left.max(right), f64::ceil).min(within.right());
        let y1 = edge(top.max(bottom), f64::ceil).min(within.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Self::default();
        }
        Self::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Self::default();
        }
        Self::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    pub fn contains(&self, other: &Self) -> bool {
        other.is_empty()
            || (other.x >= self.x
                && other.y >= self.y
                && other.right() <= self.right()
                && other.bottom() <= self.bottom())
    }

    pub fn contains_point(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    /// `self` minus `cut`, as at most four disjoint rectangles.
    pub fn subtract(&self, cut: &Self) -> Vec<Self> {
        let overlap = self.intersect(cut);
        if overlap.is_empty() {
            return vec![*self];
        }
        let mut out = Vec::with_capacity(4);
        let above = Self::new(self.x, self.y, self.width, overlap.y.saturating_sub(self.y));
        let below = Self::new(
            self.x,
            overlap.bottom(),
            self.width,
            self.bottom().saturating_sub(overlap.bottom()),
        );
        let left = Self::new(
            self.x,
            overlap.y,
            overlap.x.saturating_sub(self.x),
            overlap.height,
        );
        let right = Self::new(
            overlap.right(),
            overlap.y,
            self.right().saturating_sub(overlap.right()),
            overlap.height,
        );
        for part in [above, below, left, right] {
            if !part.is_empty() {
                out.push(part);
            }
        }
        out
    }

    /// Grow by `by` pixels on every side, clipped to `within`.
    pub fn inflate(&self, by: u32, within: Self) -> Self {
        let x0 = self.x.saturating_sub(by).max(within.x);
        let y0 = self.y.saturating_sub(by).max(within.y);
        let x1 = self.right().saturating_add(by).min(within.right());
        let y1 = self.bottom().saturating_add(by).min(within.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Self::default();
        }
        Self::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }
}

pub use tilo_reference::inspect::SurfaceRect;

/// Where one semantic node was on its surface, and what that means for
/// its pixels: the part of an Inspect answer node the pixel layer needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElementRegion {
    /// The node's stable semantic id.
    pub id: String,
    /// The node's on-screen part, after every ancestor clip, in the
    /// surface's logical pixels.
    pub bounds: SurfaceRect,
    /// The node is `Personal` or `Secret`: its pixels never leave unmasked.
    pub masked: bool,
    /// The node's own content is described by the semantic tree. A web
    /// view or an embedded foreign surface is a node whose content is not:
    /// its pixels are unexplained.
    pub described: bool,
    /// The node paints feedback of the recording device's own clock and
    /// state, which no replay reproduces: a frame comparison leaves these
    /// pixels out. See [`Pixels::Transient`].
    pub transient: bool,
}

impl ElementRegion {
    /// The region of an inspected node, or `None` when it is off screen.
    /// Every sensitivity but `Public` is masked: a `Secret` node keeps its
    /// bounds in an answer precisely so its pixels can be covered.
    pub fn of(node: &InspectedNode) -> Option<Self> {
        Some(Self {
            id: node.id.clone(),
            bounds: node.visible?,
            masked: node.sensitivity != Sensitivity::Public,
            described: node.pixels != Pixels::Undescribed,
            transient: node.pixels == Pixels::Transient,
        })
    }
}

/// The element regions an Inspect keyframe describes.
///
/// A refused inspection describes nothing, so its surface's pixels count
/// as unexplained rather than as explained by a guess.
pub fn keyframe_regions(keyframe: &InspectKeyframe) -> Vec<ElementRegion> {
    if keyframe.reply.refused.is_some() {
        return Vec::new();
    }
    keyframe
        .reply
        .nodes
        .iter()
        .filter_map(ElementRegion::of)
        .collect()
}

/// The semantic layout of every surface over a trace: its Inspect
/// keyframes, per route, in order.
#[derive(Clone, Debug, Default)]
pub struct SemanticLayout {
    keyframes: Vec<(Route, Offset, Vec<ElementRegion>)>,
}

impl SemanticLayout {
    pub fn from_trace(trace: &SessionTrace) -> Self {
        let keyframes = trace
            .timeline
            .iter()
            .filter_map(|entry| match &entry.event {
                Event::Inspect(keyframe) => {
                    Some((keyframe.route.clone(), entry.at, keyframe_regions(keyframe)))
                }
                _ => None,
            })
            .collect();
        Self { keyframes }
    }

    /// A layout of explicit keyframes, in offset order.
    pub fn from_keyframes(keyframes: Vec<(Route, Offset, Vec<ElementRegion>)>) -> Self {
        Self { keyframes }
    }

    /// The layout in effect for `surface` at `at`: its last keyframe at or
    /// before `at`, else none.
    pub fn at(&self, surface: &Route, at: Offset) -> Option<&[ElementRegion]> {
        self.keyframes
            .iter()
            .rev()
            .find(|(route, when, _)| *when <= at && same_surface(route, surface))
            .map(|(_, _, regions)| regions.as_slice())
    }

    /// What a frame of `surface` taken at `at` is compared in: the layout
    /// in effect, plus every transient region the surface declared anywhere
    /// in the session. Transient feedback comes and goes on the recording
    /// device's clock, so the replay can show it where the recording showed
    /// none at that moment; wherever it appeared is left out at every step.
    pub fn for_comparison(&self, surface: &Route, at: Offset) -> Vec<ElementRegion> {
        let mut regions: Vec<ElementRegion> =
            self.at(surface, at).map(<[_]>::to_vec).unwrap_or_default();
        for (_, _, keyframe) in self
            .keyframes
            .iter()
            .filter(|(route, _, _)| same_surface(route, surface))
        {
            for region in keyframe.iter().filter(|region| region.transient) {
                if !regions
                    .iter()
                    .any(|kept| kept.transient && kept.bounds == region.bounds)
                {
                    regions.push(region.clone());
                }
            }
        }
        regions
    }

    /// The first keyframe for `surface` after `at`.
    fn next(&self, surface: &Route, at: Offset) -> Option<&[ElementRegion]> {
        self.keyframes
            .iter()
            .find(|(route, when, _)| *when > at && same_surface(route, surface))
            .map(|(_, _, regions)| regions.as_slice())
    }

    /// What must be considered for masking a frame of `surface` taken at
    /// `at`: the layout in effect plus the next one. A frame can show a
    /// tree change a moment before its keyframe reaches the trace; masking
    /// both is what keeps a node that just moved or appeared covered.
    pub fn around(&self, surface: &Route, at: Offset) -> Vec<ElementRegion> {
        let mut regions: Vec<ElementRegion> =
            self.at(surface, at).map(<[_]>::to_vec).unwrap_or_default();
        if let Some(next) = self.next(surface, at) {
            regions.extend(next.iter().cloned());
        }
        regions
    }

    pub fn is_empty(&self) -> bool {
        self.keyframes.is_empty()
    }
}

/// Why a strip holds frames at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelPurpose {
    /// Kept only so the device can check replay against them; offered for
    /// attachment region by region, where they diverge.
    Check,
    /// The user asked to keep pixels with this recording.
    Keep,
}

impl PixelPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Keep => "keep",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "check" => Some(Self::Check),
            "keep" => Some(Self::Keep),
            _ => None,
        }
    }
}

/// One kept frame: a masked, lossless PNG of a [`SurfaceFrame`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredFrame {
    pub at: Offset,
    pub route: Route,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub png: Vec<u8>,
    /// Rectangles filled with the mask because a `Personal` or `Secret`
    /// node was there.
    pub masked: Vec<PixelRect>,
    /// Rectangles no semantic node describes, flagged for review.
    pub unexplained: Vec<PixelRect>,
}

impl StoredFrame {
    /// The frame's pixels.
    ///
    /// # Errors
    ///
    /// When the PNG does not decode to the recorded size.
    pub fn decode(&self) -> Result<SurfaceFrame, FrameError> {
        let rgba = decode_png(&self.png, self.width, self.height)?;
        SurfaceFrame::new(
            self.at,
            self.route.clone(),
            self.width,
            self.height,
            self.scale,
            rgba,
        )
    }
}

/// Bounds on one strip. Hitting one marks the strip truncated and drops
/// later frames; nothing kept is discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLimits {
    pub max_frames: usize,
    /// Encoded PNG bytes across the strip. Below the device content
    /// store's largest blob, so the strip is always storable.
    pub max_bytes: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            max_frames: 4_096,
            max_bytes: 48 * 1024 * 1024,
        }
    }
}

/// The frame blob of a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameStrip {
    pub purpose: PixelPurpose,
    /// In capture order; offsets never decrease per surface.
    pub frames: Vec<StoredFrame>,
    pub truncated: bool,
}

#[derive(Serialize, Deserialize)]
struct Encoded<T> {
    format: u32,
    strip: T,
}

impl FrameStrip {
    /// The blob bytes.
    ///
    /// # Errors
    ///
    /// When encoding fails.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        postcard::to_allocvec(&Encoded {
            format: FRAMES_FORMAT,
            strip: self,
        })
        .map_err(|error| FrameError::Encode(error.to_string()))
    }

    /// Read a blob written by [`FrameStrip::encode`].
    ///
    /// # Errors
    ///
    /// When the bytes are not a strip of [`FRAMES_FORMAT`].
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        #[derive(Deserialize)]
        struct Header {
            format: u32,
        }
        let header: Header =
            postcard::from_bytes(bytes).map_err(|error| FrameError::Encode(error.to_string()))?;
        if header.format != FRAMES_FORMAT {
            return Err(FrameError::UnsupportedFormat(header.format));
        }
        let encoded: Encoded<Self> =
            postcard::from_bytes(bytes).map_err(|error| FrameError::Encode(error.to_string()))?;
        Ok(encoded.strip)
    }

    /// Frames of `surface`, in order.
    pub fn frames_of<'a>(&'a self, surface: &'a Route) -> impl Iterator<Item = &'a StoredFrame> {
        self.frames
            .iter()
            .filter(move |frame| same_surface(&frame.route, surface))
    }

    /// Distinct capture offsets, ascending: the steps a divergence check
    /// walks.
    pub fn offsets(&self) -> Vec<Offset> {
        let mut offsets: Vec<Offset> = self.frames.iter().map(|frame| frame.at).collect();
        offsets.sort_unstable();
        offsets.dedup();
        offsets
    }
}

/// A frame masked and encoded, ready for [`FrameStripBuilder::push_prepared`].
pub struct PreparedFrame {
    stored: StoredFrame,
    digest: u64,
}

impl PreparedFrame {
    /// Mask `frame` with `layout` and encode it. The unmasked pixels are
    /// dropped here: nothing after this point can reach them.
    ///
    /// # Errors
    ///
    /// When the frame is malformed or cannot be encoded.
    pub fn prepare(mut frame: SurfaceFrame, layout: &[ElementRegion]) -> Result<Self, FrameError> {
        frame.validate()?;
        let report = crate::mask::mask_frame(&mut frame, layout);
        let digest = digest(&frame);
        let png = encode_png(&frame.rgba, frame.width, frame.height)?;
        Ok(Self {
            stored: StoredFrame {
                at: frame.at,
                route: frame.route,
                width: frame.width,
                height: frame.height,
                scale: frame.scale,
                png,
                masked: report.masked,
                unexplained: report.unexplained,
            },
            digest,
        })
    }
}

/// What [`FrameStripBuilder::push`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePush {
    Kept,
    /// Identical to the surface's previous kept frame; nothing to keep.
    Unchanged,
    /// A bound was hit; the frame was dropped and the strip is truncated.
    Full,
}

/// Collects a recording's frames as the compositor captures them.
pub struct FrameStripBuilder {
    purpose: PixelPurpose,
    limits: FrameLimits,
    bytes: usize,
    frames: Vec<StoredFrame>,
    last: Vec<(Route, u64)>,
    truncated: bool,
}

impl FrameStripBuilder {
    pub fn new(purpose: PixelPurpose, limits: FrameLimits) -> Self {
        Self {
            purpose,
            limits,
            bytes: 0,
            frames: Vec::new(),
            last: Vec::new(),
            truncated: false,
        }
    }

    pub const fn purpose(&self) -> PixelPurpose {
        self.purpose
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Mask `frame` with `layout` (the regions in effect as it was taken)
    /// and keep it, unless it shows exactly what the surface showed last.
    ///
    /// # Errors
    ///
    /// When the frame is malformed or cannot be encoded.
    pub fn push(
        &mut self,
        frame: SurfaceFrame,
        layout: &[ElementRegion],
    ) -> Result<FramePush, FrameError> {
        Ok(self.push_prepared(PreparedFrame::prepare(frame, layout)?))
    }

    /// Keep a frame masked and encoded by [`PreparedFrame::prepare`], which
    /// a caller runs outside whatever lock guards this builder.
    pub fn push_prepared(&mut self, prepared: PreparedFrame) -> FramePush {
        if self.truncated || self.frames.len() >= self.limits.max_frames {
            self.truncated = true;
            return FramePush::Full;
        }
        let PreparedFrame { stored, digest } = prepared;
        match self
            .last
            .iter_mut()
            .find(|(route, _)| same_surface(route, &stored.route))
        {
            Some((_, last)) if *last == digest => return FramePush::Unchanged,
            Some((_, last)) => *last = digest,
            None => self.last.push((stored.route.clone(), digest)),
        }
        let bytes = self.bytes.saturating_add(stored.png.len());
        if bytes > self.limits.max_bytes {
            self.truncated = true;
            return FramePush::Full;
        }
        self.bytes = bytes;
        self.frames.push(stored);
        FramePush::Kept
    }

    /// Seal the strip against the finished trace: every frame is masked
    /// again with the layout around it ([`SemanticLayout::around`]), so a
    /// sensitive node whose keyframe arrived just after the frame is still
    /// covered, and its unexplained regions are taken from that layout.
    ///
    /// # Errors
    ///
    /// When a frame cannot be decoded or re-encoded.
    pub fn finish(self, trace: &SessionTrace) -> Result<FrameStrip, FrameError> {
        let layout = SemanticLayout::from_trace(trace);
        let mut frames = Vec::with_capacity(self.frames.len());
        for frame in self.frames {
            let regions = layout.around(&frame.route, frame.at);
            frames.push(remask(frame, &regions)?);
        }
        Ok(FrameStrip {
            purpose: self.purpose,
            frames,
            truncated: self.truncated,
        })
    }
}

/// Apply whatever `regions` masks that `frame` does not already cover.
fn remask(mut frame: StoredFrame, regions: &[ElementRegion]) -> Result<StoredFrame, FrameError> {
    let bounds = PixelRect::new(0, 0, frame.width, frame.height);
    let missing = crate::mask::masked_rects(regions, frame.scale, bounds)
        .into_iter()
        .any(|rect| !frame.masked.iter().any(|done| done.contains(&rect)));
    if missing {
        let mut pixels = frame.decode()?;
        let already = std::mem::take(&mut frame.masked);
        let report = crate::mask::mask_frame(&mut pixels, regions);
        frame.png = encode_png(&pixels.rgba, pixels.width, pixels.height)?;
        frame.masked = already;
        for rect in report.masked {
            if !frame.masked.contains(&rect) {
                frame.masked.push(rect);
            }
        }
    }
    frame.unexplained = crate::mask::unexplained_rects(regions, frame.scale, bounds);
    Ok(frame)
}

fn digest(frame: &SurfaceFrame) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    frame.width.hash(&mut hasher);
    frame.height.hash(&mut hasher);
    frame.rgba.hash(&mut hasher);
    hasher.finish()
}

/// Lossless PNG of tightly packed RGBA8. Fast compression: capture runs on
/// the compositor's frame thread.
///
/// # Errors
///
/// When the buffer does not match the size or encoding fails.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, FrameError> {
    use image::ImageEncoder as _;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    if rgba_len(width, height) != Some(rgba.len()) {
        return Err(FrameError::Malformed {
            width,
            height,
            len: rgba.len(),
        });
    }
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Sub)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| FrameError::Encode(error.to_string()))?;
    Ok(out)
}

/// Decode a PNG written by [`encode_png`], refusing any other size.
///
/// # Errors
///
/// When the bytes are not a PNG of `width`×`height`.
pub fn decode_png(png: &[u8], width: u32, height: u32) -> Result<Vec<u8>, FrameError> {
    if width > MAX_FRAME_EDGE || height > MAX_FRAME_EDGE {
        return Err(FrameError::Malformed {
            width,
            height,
            len: png.len(),
        });
    }
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|error| FrameError::Encode(error.to_string()))?
        .into_rgba8();
    if image.width() != width || image.height() != height {
        return Err(FrameError::Malformed {
            width: image.width(),
            height: image.height(),
            len: png.len(),
        });
    }
    Ok(image.into_raw())
}

/// An inspection reply describing `regions`, as an app would answer.
#[cfg(test)]
pub(crate) fn answer(regions: &[ElementRegion]) -> tilo_reference::inspect::InspectReply {
    let mut reply = tilo_reference::inspect::InspectReply::empty();
    reply.nodes = regions
        .iter()
        .map(|region| InspectedNode {
            id: region.id.clone(),
            identity: tilo_reference::inspect::Identity::Durable,
            role: if region.described { "group" } else { "canvas" }.into(),
            label: None,
            value: None,
            sensitivity: if region.masked {
                Sensitivity::Personal
            } else {
                Sensitivity::Public
            },
            content: tilo_reference::inspect::Content::Label,
            bounds: Some(region.bounds),
            visible: Some(region.bounds),
            pixels: if region.transient {
                Pixels::Transient
            } else if region.described {
                Pixels::Described
            } else {
                Pixels::Undescribed
            },
            state: tilo_reference::inspect::NodeState::default(),
            children: Vec::new(),
            reference: None,
        })
        .collect();
    reply
}
