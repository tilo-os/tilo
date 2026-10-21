//! One frame of one surface, as it sits in the ring.
//!
//! ```text
//! header   u32 magic "TRF1", u16 version, u16 flags, u32 record length,
//!          u32 surface, u64 sequence, u32 width, u32 height,
//!          4 × f32 base colour (premultiplied-free sRGB, alpha last)
//! sections u16 kind, u16 section flags, u32 length, payload, zero padding to 4
//! ```
//!
//! A section whose kind the reader does not know is skipped when its sender
//! set [`SECTION_OPTIONAL`], and refuses the frame otherwise: a later step can
//! add scene annotations as optional sections that older services ignore,
//! and service-run motion as required ones that only a service which
//! announced them in its hello receives.
//!
//! Sections of version 1:
//!
//! - [`SECTION_DAMAGE`]: rectangles of the surface that changed since the
//!   previous frame, in pixels. With the header's [`FRAME_FULL`] the whole
//!   surface did.
//! - [`SECTION_SCROLL_COPY`]: copies within the retained surface, in order,
//!   before the damage is repainted (what a scroll keeps).
//! - [`SECTION_LAYER`]: a layer's new content, transform, opacity and clip;
//!   or that it keeps its content; or that it is gone. Layers not named keep
//!   everything.
//! - [`SECTION_ORDER`]: the order layers draw in, bottom first. Without it
//!   the previous order holds; new layers go on top.
//! - [`SECTION_BACKDROP`]: a floating surface's blurred backdrop: the
//!   placeholder image the scene draws, where, how blurred, and the scene
//!   composed below it.

use crate::bytes::{Reader, Writer};
use crate::scene::{self, ResourceIds, Resources};
use crate::validate::{SceneCost, validate};
use crate::{ImageId, LayerId, Limits, SurfaceId, WireError};
use vello_encoding::{Encoding, Transform};

const MAGIC: u32 = u32::from_le_bytes(*b"TRF1");
const HEADER_BYTES: usize = 48;

/// The whole surface changed: repaint all of it.
pub const FRAME_FULL: u16 = 1;

/// A reader that does not know this section's kind skips it.
pub const SECTION_OPTIONAL: u16 = 1;

pub const SECTION_DAMAGE: u16 = 1;
pub const SECTION_SCROLL_COPY: u16 = 2;
pub const SECTION_LAYER: u16 = 3;
pub const SECTION_ORDER: u16 = 4;
pub const SECTION_BACKDROP: u16 = 5;

/// A rectangle in surface pixels, `x0 <= x1`, `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    fn valid(&self) -> bool {
        [self.x0, self.y0, self.x1, self.y1]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= crate::validate::MAX_COORDINATE)
            && self.x0 <= self.x1
            && self.y0 <= self.y1
    }
}

/// A copy within the retained surface: the source rectangle and how far it
/// moves, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollCopy {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub dx: i32,
    pub dy: i32,
}

/// What a frame does to one layer.
#[derive(Clone)]
pub enum LayerChange {
    /// New content.
    Content(Box<Encoding>),
    /// The content stays; the properties change.
    Keep,
    /// The layer is gone.
    Remove,
}

/// A layer as a frame names it.
#[derive(Clone)]
pub struct LayerUpdate {
    pub id: LayerId,
    pub change: LayerChange,
    /// Placed by this transform.
    pub transform: Transform,
    /// Drawn at this opacity, `0..=1`.
    pub opacity: f32,
    /// Clipped to this rectangle, in surface pixels.
    pub clip: Option<Rect>,
}

impl LayerUpdate {
    /// Layer `id` with new `content`, untransformed, opaque, unclipped.
    #[must_use]
    pub fn content(id: LayerId, content: Encoding) -> Self {
        Self {
            id,
            change: LayerChange::Content(Box::new(content)),
            transform: Transform::IDENTITY,
            opacity: 1.0,
            clip: None,
        }
    }
}

/// A floating surface's blurred backdrop.
#[derive(Clone)]
pub struct Backdrop {
    /// The placeholder image the frame draws, which the service replaces
    /// with the blur.
    pub image: ImageId,
    /// Where the placeholder lands, in surface pixels.
    pub rect: Rect,
    /// Blur standard deviation, in surface pixels.
    pub blur_px: f32,
    /// What the backdrop shows where nothing below covers it.
    pub ground: [f32; 4],
    /// Everything composed below the surface.
    pub below: Encoding,
}

/// A frame record, decoded.
#[derive(Clone)]
pub struct Frame {
    pub surface: SurfaceId,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub base_color: [f32; 4],
    pub full: bool,
    pub damage: Vec<Rect>,
    pub scroll_copies: Vec<ScrollCopy>,
    pub layers: Vec<LayerUpdate>,
    pub order: Option<Vec<LayerId>>,
    pub backdrops: Vec<Backdrop>,
}

/// Writes a frame record; [`FrameWriter::finish`] fills in its length.
pub struct FrameWriter<'a> {
    out: &'a mut Vec<u8>,
    start: usize,
}

impl<'a> FrameWriter<'a> {
    /// Start a record for `surface` at the end of `out`.
    pub fn new(
        out: &'a mut Vec<u8>,
        (surface, sequence): (SurfaceId, u64),
        (width, height): (u32, u32),
        base_color: [f32; 4],
        full: bool,
    ) -> Self {
        let start = out.len();
        let mut w = Writer::new(out);
        w.u32(MAGIC);
        w.u16(crate::PROTOCOL_VERSION);
        w.u16(if full { FRAME_FULL } else { 0 });
        w.u32(0);
        w.u32(surface.0);
        w.u64(sequence);
        w.u32(width);
        w.u32(height);
        for value in base_color {
            w.f32(value);
        }
        Self { out, start }
    }

    fn section(
        &mut self,
        kind: u16,
        body: impl FnOnce(&mut Writer<'_>) -> Result<(), WireError>,
    ) -> Result<(), WireError> {
        let mut w = Writer::new(self.out);
        w.u16(kind);
        w.u16(0);
        let length_at = w.len();
        w.u32(0);
        let body_start = w.len();
        body(&mut w)?;
        let length = w.len().checked_sub(body_start).ok_or(WireError::Overflow)?;
        w.patch_u32(
            length_at,
            u32::try_from(length).map_err(|_| WireError::Overflow)?,
        )?;
        w.align4(length);
        Ok(())
    }

    /// The rectangles that changed.
    ///
    /// # Errors
    ///
    /// When the list does not fit the wire.
    pub fn damage(&mut self, rects: &[Rect]) -> Result<(), WireError> {
        if rects.is_empty() {
            return Ok(());
        }
        self.section(SECTION_DAMAGE, |w| {
            w.count(rects.len())?;
            for rect in rects {
                for value in [rect.x0, rect.y0, rect.x1, rect.y1] {
                    w.f32(value);
                }
            }
            Ok(())
        })
    }

    /// The copies a scroll keeps, in order.
    ///
    /// # Errors
    ///
    /// When the list does not fit the wire.
    pub fn scroll_copies(&mut self, copies: &[ScrollCopy]) -> Result<(), WireError> {
        if copies.is_empty() {
            return Ok(());
        }
        self.section(SECTION_SCROLL_COPY, |w| {
            w.count(copies.len())?;
            for copy in copies {
                for value in [copy.x, copy.y, copy.width, copy.height] {
                    w.u32(value);
                }
                w.i32(copy.dx);
                w.i32(copy.dy);
            }
            Ok(())
        })
    }

    /// One layer's change; its content's resources are registered through
    /// `ids`.
    ///
    /// # Errors
    ///
    /// When a resource cannot be registered or a count does not fit.
    pub fn layer(
        &mut self,
        layer: &LayerUpdate,
        ids: &mut dyn ResourceIds,
    ) -> Result<(), WireError> {
        self.section(SECTION_LAYER, |w| {
            w.u32(layer.id.0);
            let (op, content) = match &layer.change {
                LayerChange::Content(content) => (0_u32, Some(content)),
                LayerChange::Keep => (1, None),
                LayerChange::Remove => (2, None),
            };
            w.u32(op | if layer.clip.is_some() { 1 << 8 } else { 0 });
            for value in layer
                .transform
                .matrix
                .iter()
                .chain(&layer.transform.translation)
            {
                w.f32(*value);
            }
            w.f32(layer.opacity);
            let clip = layer.clip.unwrap_or(Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 0.0,
                y1: 0.0,
            });
            for value in [clip.x0, clip.y0, clip.x1, clip.y1] {
                w.f32(value);
            }
            if let Some(content) = content {
                scene::encode(content, ids, w.vec())?;
            }
            Ok(())
        })
    }

    /// Layer `id`'s new content, untransformed, opaque, unclipped: what a
    /// toolkit frame sends for its root layer, without copying the encoding.
    ///
    /// # Errors
    ///
    /// When a resource cannot be registered or a count does not fit.
    pub fn content(
        &mut self,
        id: LayerId,
        content: &Encoding,
        ids: &mut dyn ResourceIds,
    ) -> Result<(), WireError> {
        self.section(SECTION_LAYER, |w| {
            w.u32(id.0);
            w.u32(0);
            for value in [1.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0] {
                w.f32(value);
            }
            scene::encode(content, ids, w.vec())?;
            Ok(())
        })
    }

    /// The order layers draw in, bottom first.
    ///
    /// # Errors
    ///
    /// When the list does not fit the wire.
    pub fn order(&mut self, order: &[LayerId]) -> Result<(), WireError> {
        self.section(SECTION_ORDER, |w| {
            w.count(order.len())?;
            for id in order {
                w.u32(id.0);
            }
            Ok(())
        })
    }

    /// A floating surface's backdrop.
    ///
    /// # Errors
    ///
    /// When a resource cannot be registered or a count does not fit.
    pub fn backdrop(
        &mut self,
        backdrop: &Backdrop,
        ids: &mut dyn ResourceIds,
    ) -> Result<(), WireError> {
        self.section(SECTION_BACKDROP, |w| {
            w.u32(backdrop.image.0);
            let rect = backdrop.rect;
            for value in [rect.x0, rect.y0, rect.x1, rect.y1, backdrop.blur_px] {
                w.f32(value);
            }
            for value in backdrop.ground {
                w.f32(value);
            }
            scene::encode(&backdrop.below, ids, w.vec())?;
            Ok(())
        })
    }

    /// A section of a kind this crate does not write (a later step's), as
    /// raw bytes.
    ///
    /// # Errors
    ///
    /// When the payload does not fit the wire.
    pub fn raw_section(
        &mut self,
        kind: u16,
        optional: bool,
        payload: &[u8],
    ) -> Result<(), WireError> {
        let mut w = Writer::new(self.out);
        w.u16(kind);
        w.u16(if optional { SECTION_OPTIONAL } else { 0 });
        w.count(payload.len())?;
        w.bytes(payload);
        w.align4(payload.len());
        Ok(())
    }

    /// Close the record and return its length.
    ///
    /// # Errors
    ///
    /// When the record is longer than the wire can say.
    pub fn finish(self) -> Result<usize, WireError> {
        let length = self
            .out
            .len()
            .checked_sub(self.start)
            .ok_or(WireError::Overflow)?;
        let at = self.start.checked_add(8).ok_or(WireError::Overflow)?;
        Writer::new(self.out)
            .patch_u32(at, u32::try_from(length).map_err(|_| WireError::Overflow)?)?;
        Ok(length)
    }
}

/// A frame, read and validated, with what it costs to draw.
pub struct Checked {
    pub frame: Frame,
    pub cost: SceneCost,
}

/// Read one frame record from `bytes` (exactly one record) and validate
/// every scene in it against `limits`. Every byte is read once.
///
/// # Errors
///
/// The first thing wrong with the record.
pub fn read(
    bytes: &[u8],
    resources: &dyn Resources,
    limits: &Limits,
) -> Result<Checked, WireError> {
    if bytes.len() > limits.frame_bytes as usize {
        return Err(WireError::OverLimit("frame bytes"));
    }
    let mut r = Reader::new(bytes);
    if r.u32()? != MAGIC {
        return Err(WireError::Malformed("not a frame record"));
    }
    if r.u16()? != crate::PROTOCOL_VERSION {
        return Err(WireError::Malformed("frame version"));
    }
    let flags = r.u16()?;
    if flags & !FRAME_FULL != 0 {
        return Err(WireError::Malformed("frame flags"));
    }
    if r.u32()? as usize != bytes.len() {
        return Err(WireError::Malformed("frame length"));
    }
    let mut frame = Frame {
        surface: SurfaceId(r.u32()?),
        sequence: r.u64()?,
        width: r.u32()?,
        height: r.u32()?,
        base_color: [r.f32()?, r.f32()?, r.f32()?, r.f32()?],
        full: flags & FRAME_FULL != 0,
        damage: Vec::new(),
        scroll_copies: Vec::new(),
        layers: Vec::new(),
        order: None,
        backdrops: Vec::new(),
    };
    debug_assert_eq!(bytes.len().saturating_sub(r.remaining()), HEADER_BYTES);
    if frame.width == 0
        || frame.height == 0
        || frame.width > limits.surface_edge
        || frame.height > limits.surface_edge
    {
        return Err(WireError::OverLimit("surface size"));
    }
    if !frame.base_color.iter().all(|value| value.is_finite()) {
        return Err(WireError::Invalid("base colour"));
    }
    let mut cost = SceneCost::default();
    while !r.is_empty() {
        let kind = r.u16()?;
        let section_flags = r.u16()?;
        let length = r.u32()? as usize;
        let payload = r.take(length)?;
        r.align4(length)?;
        if section_flags & !SECTION_OPTIONAL != 0 {
            return Err(WireError::Malformed("section flags"));
        }
        let mut body = Reader::new(payload);
        match kind {
            SECTION_DAMAGE => read_damage(&mut body, &mut frame, limits)?,
            SECTION_SCROLL_COPY => read_copies(&mut body, &mut frame, limits)?,
            SECTION_LAYER => {
                if frame.layers.len() >= limits.layers as usize {
                    return Err(WireError::OverLimit("layers"));
                }
                let (layer, layer_cost) = read_layer(&mut body, resources, limits)?;
                cost = cost.plus(layer_cost);
                frame.layers.push(layer);
            }
            SECTION_ORDER => {
                let count = body.count(4)?;
                if count > limits.layers as usize || frame.order.is_some() {
                    return Err(WireError::OverLimit("layer order"));
                }
                let order = (0..count)
                    .map(|_| body.u32().map(LayerId))
                    .collect::<Result<Vec<_>, _>>()?;
                frame.order = Some(order);
            }
            SECTION_BACKDROP => {
                if frame.backdrops.len() >= limits.backdrops as usize {
                    return Err(WireError::OverLimit("backdrops"));
                }
                let (backdrop, below_cost) = read_backdrop(&mut body, resources, limits)?;
                cost = cost.plus(below_cost);
                frame.backdrops.push(backdrop);
            }
            _ if section_flags & SECTION_OPTIONAL != 0 => continue,
            _ => return Err(WireError::UnknownSection(kind)),
        }
        if !body.is_empty() {
            return Err(WireError::Malformed("bytes after a section"));
        }
    }
    cost.check(limits)?;
    Ok(Checked { frame, cost })
}

fn read_damage(r: &mut Reader<'_>, frame: &mut Frame, limits: &Limits) -> Result<(), WireError> {
    let count = r.count(16)?;
    if count.saturating_add(frame.damage.len()) > limits.rects as usize {
        return Err(WireError::OverLimit("damage rectangles"));
    }
    for _ in 0..count {
        let rect = Rect {
            x0: r.f32()?,
            y0: r.f32()?,
            x1: r.f32()?,
            y1: r.f32()?,
        };
        if !rect.valid() {
            return Err(WireError::Invalid("damage rectangle"));
        }
        frame.damage.push(rect);
    }
    Ok(())
}

fn read_copies(r: &mut Reader<'_>, frame: &mut Frame, limits: &Limits) -> Result<(), WireError> {
    let count = r.count(24)?;
    if count.saturating_add(frame.scroll_copies.len()) > limits.rects as usize {
        return Err(WireError::OverLimit("scroll copies"));
    }
    for _ in 0..count {
        let copy = ScrollCopy {
            x: r.u32()?,
            y: r.u32()?,
            width: r.u32()?,
            height: r.u32()?,
            dx: r.i32()?,
            dy: r.i32()?,
        };
        let inside = |origin: u32, extent: u32, delta: i32, limit: u32| {
            origin.checked_add(extent).is_some_and(|end| end <= limit)
                && origin
                    .checked_add_signed(delta)
                    .and_then(|moved| moved.checked_add(extent))
                    .is_some_and(|end| end <= limit)
        };
        if copy.width == 0
            || copy.height == 0
            || !inside(copy.x, copy.width, copy.dx, frame.width)
            || !inside(copy.y, copy.height, copy.dy, frame.height)
        {
            return Err(WireError::Invalid("scroll copy outside the surface"));
        }
        frame.scroll_copies.push(copy);
    }
    Ok(())
}

fn read_encoding(
    r: &mut Reader<'_>,
    resources: &dyn Resources,
    limits: &Limits,
) -> Result<(Encoding, SceneCost), WireError> {
    let bytes = r.take(r.remaining())?;
    let encoding = scene::decode(bytes, resources)?;
    let cost = validate(&encoding, limits)?;
    Ok((encoding, cost))
}

fn read_layer(
    r: &mut Reader<'_>,
    resources: &dyn Resources,
    limits: &Limits,
) -> Result<(LayerUpdate, SceneCost), WireError> {
    let id = LayerId(r.u32()?);
    let op = r.u32()?;
    if op & !0x1ff != 0 {
        return Err(WireError::Malformed("layer op"));
    }
    let transform = Transform {
        matrix: [r.f32()?, r.f32()?, r.f32()?, r.f32()?],
        translation: [r.f32()?, r.f32()?],
    };
    let opacity = r.f32()?;
    let clip = Rect {
        x0: r.f32()?,
        y0: r.f32()?,
        x1: r.f32()?,
        y1: r.f32()?,
    };
    let finite = transform
        .matrix
        .iter()
        .chain(&transform.translation)
        .all(|value| value.is_finite() && value.abs() <= crate::validate::MAX_COORDINATE);
    if !finite || !(0.0..=1.0).contains(&opacity) || !clip.valid() {
        return Err(WireError::Invalid("layer properties"));
    }
    let (change, cost) = match op & 0xff {
        0 => {
            let (content, cost) = read_encoding(r, resources, limits)?;
            (LayerChange::Content(Box::new(content)), cost)
        }
        1 => (LayerChange::Keep, SceneCost::default()),
        2 => (LayerChange::Remove, SceneCost::default()),
        _ => return Err(WireError::Malformed("layer op")),
    };
    Ok((
        LayerUpdate {
            id,
            change,
            transform,
            opacity,
            clip: (op & (1 << 8) != 0).then_some(clip),
        },
        cost,
    ))
}

fn read_backdrop(
    r: &mut Reader<'_>,
    resources: &dyn Resources,
    limits: &Limits,
) -> Result<(Backdrop, SceneCost), WireError> {
    let image = ImageId(r.u32()?);
    if resources.image(image).is_none() {
        return Err(WireError::UnknownResource("a backdrop image"));
    }
    let rect = Rect {
        x0: r.f32()?,
        y0: r.f32()?,
        x1: r.f32()?,
        y1: r.f32()?,
    };
    let blur_px = r.f32()?;
    let ground = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    let valid = rect.valid()
        && blur_px.is_finite()
        && (0.0..=4096.0).contains(&blur_px)
        && ground.iter().all(|value| value.is_finite());
    if !valid {
        return Err(WireError::Invalid("backdrop"));
    }
    let (below, cost) = read_encoding(r, resources, limits)?;
    Ok((
        Backdrop {
            image,
            rect,
            blur_px,
            ground,
            below,
        },
        cost,
    ))
}
