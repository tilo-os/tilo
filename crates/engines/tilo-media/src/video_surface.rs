//! Putting a decoded picture on a display plane instead of into the scene: a
//! hardware decoder already leaves its picture where the display engine can read
//! it, so handing the dmabuf to an overlay plane and leaving a transparent hole in
//! the desktop shows video without touching its pixels. On a device whose desktop
//! renders on the CPU that is the difference between 1080p60 and a slideshow.
//!
//! [`place`] decides per frame whether the picture can go on a plane
//! ([`Placement::Overlay`], with the clipped source and destination rectangles KMS
//! takes) or must be composited ([`Placement::Composite`], with the reason).
//! Everything here is pure, so the decision is the same in a test as on the
//! device.

use crate::dmabuf::{
    ColorRange, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, YuvMatrix, fourcc, fourcc_str,
};
#[cfg(unix)]
use crate::dmabuf::{DRM_FORMAT_NV12, DRM_FORMAT_P010};
use crate::frame::Frame;

pub const DRM_FORMAT_NV15: u32 = fourcc(b'N', b'V', b'1', b'5');

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaneFormat {
    pub fourcc: u32,
    pub modifiers: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zpos {
    pub value: u64,
    pub range: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaneCaps {
    pub id: u32,
    pub formats: Vec<PlaneFormat>,
    /// How far the plane shrinks a picture: 8 means down to one eighth.
    /// 1 means no downscaling.
    pub max_downscale: u32,
    /// How far it enlarges one. 1 means no upscaling.
    pub max_upscale: u32,
    pub zpos: Option<Zpos>,
    pub color_encodings: Vec<YuvMatrix>,
    pub color_ranges: Vec<ColorRange>,
}

impl PlaneCaps {
    /// Whether the plane scans out `fourcc` with `modifier`. A producer that
    /// did not state a modifier is read as linear, the only layout such
    /// producers make.
    pub fn takes(&self, fourcc: u32, modifier: u64) -> bool {
        let modifier = if modifier == DRM_FORMAT_MOD_INVALID {
            DRM_FORMAT_MOD_LINEAR
        } else {
            modifier
        };
        self.formats
            .iter()
            .any(|f| f.fourcc == fourcc && f.modifiers.contains(&modifier))
    }

    #[cfg(unix)]
    fn zpos_value(&self) -> u64 {
        self.zpos.map_or(0, |z| z.value)
    }

    #[cfg(unix)]
    fn can_go_below(&self, primary: u64) -> bool {
        self.zpos_value() < primary
            || self
                .zpos
                .and_then(|z| z.range)
                .is_some_and(|(min, _)| min < primary)
    }

    #[cfg(unix)]
    fn can_go_above(&self, primary: u64) -> bool {
        self.zpos_value() > primary
            || self
                .zpos
                .and_then(|z| z.range)
                .is_some_and(|(_, max)| max > primary)
    }

    #[cfg(unix)]
    fn colour_exact(&self, matrix: YuvMatrix, range: ColorRange) -> bool {
        let matrix_ok = if self.color_encodings.is_empty() {
            matrix == YuvMatrix::Bt601
        } else {
            self.color_encodings.contains(&matrix)
        };
        let range_ok = if self.color_ranges.is_empty() {
            range == ColorRange::Limited
        } else {
            self.color_ranges.contains(&range)
        };
        matrix_ok && range_ok
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimaryPlane {
    pub zpos: u64,
    pub alpha: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct VideoSurfaceRequest<'a> {
    pub frame: &'a Frame,
    pub dst: Rect,
    /// Whether any desktop content is drawn over the picture — player
    /// controls, a notification, a menu. Such content has to stay visible,
    /// so the plane must go below the desktop and show through a hole.
    pub ui_overlaps: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stacking {
    /// Above the primary plane. Only valid while nothing is drawn over the
    /// video.
    Above,
    /// Below the primary plane; the compositor clears the destination
    /// rectangle of the desktop to transparent so the video shows through.
    BelowHole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayPlacement {
    pub plane_id: u32,
    pub src_16_16: [u32; 4],
    pub dst: Rect,
    pub stacking: Stacking,
    pub color_encoding: Option<YuvMatrix>,
    pub color_range: Option<ColorRange>,
    /// Whether the plane converts with the picture's own matrix and range.
    /// `false` on a plane fixed to BT.601 showing BT.709 video: usable, with
    /// a slight hue shift, and worth knowing when judging colour.
    pub colour_exact: bool,
}

/// Why a picture is composited instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompositeReason {
    /// The picture is in system memory, not a dmabuf.
    NotDmabuf,
    Format {
        fourcc: u32,
        modifier: u64,
    },
    Scale,
    NeedsUnderlay,
}

impl std::fmt::Display for CompositeReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompositeReason::NotDmabuf => f.write_str("picture is not a dmabuf"),
            CompositeReason::Format { fourcc, modifier } => write!(
                f,
                "no free plane scans out {} with modifier {modifier:#x}",
                fourcc_str(*fourcc)
            ),
            CompositeReason::Scale => f.write_str("scale factor beyond every plane's limits"),
            CompositeReason::NeedsUnderlay => {
                f.write_str("desktop content overlaps and no plane can go below a hole")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    Hidden,
    Overlay(OverlayPlacement),
    Composite(CompositeReason),
}

/// Decide where `request`'s picture goes on an output of `output` pixels.
///
/// `planes` are the overlay planes on the CRTC; `taken` the ids already used
/// this frame. The first plane that can show the picture wins, so callers list
/// them in preference order.
#[cfg_attr(
    not(unix),
    expect(
        clippy::needless_return,
        reason = "the Unix placement follows the early return"
    )
)]
pub fn place(
    request: &VideoSurfaceRequest<'_>,
    output: (u32, u32),
    primary: PrimaryPlane,
    planes: &[PlaneCaps],
    taken: &[u32],
) -> Placement {
    #[cfg(unix)]
    let Some(image) = request.frame.dmabuf() else {
        return Placement::Composite(CompositeReason::NotDmabuf);
    };
    #[cfg(not(unix))]
    {
        let _ = (request, output, primary, planes, taken);
        return Placement::Composite(CompositeReason::NotDmabuf);
    }
    #[cfg(unix)]
    {
        let Some((src, dst)) = clip(
            (image.width, image.height),
            request.dst,
            output,
            subsampled(image.fourcc),
        ) else {
            return Placement::Hidden;
        };
        let stacking = if request.ui_overlaps {
            Stacking::BelowHole
        } else {
            Stacking::Above
        };
        let matrix = image.colorimetry.matrix;
        let range = image.colorimetry.range;

        let free: Vec<&PlaneCaps> = planes.iter().filter(|p| !taken.contains(&p.id)).collect();
        let by_format: Vec<&PlaneCaps> = free
            .iter()
            .copied()
            .filter(|p| p.takes(image.fourcc, image.modifier))
            .collect();
        if by_format.is_empty() {
            return Placement::Composite(CompositeReason::Format {
                fourcc: image.fourcc,
                modifier: image.modifier,
            });
        }
        let by_scale: Vec<&PlaneCaps> = by_format
            .into_iter()
            .filter(|p| scale_fits(p, src, dst))
            .collect();
        if by_scale.is_empty() {
            return Placement::Composite(CompositeReason::Scale);
        }
        let stackable = by_scale.into_iter().find(|p| match stacking {
            Stacking::Above => p.can_go_above(primary.zpos),
            Stacking::BelowHole => primary.alpha && p.can_go_below(primary.zpos),
        });
        let Some(plane) = stackable else {
            return Placement::Composite(CompositeReason::NeedsUnderlay);
        };
        let [sx, sy, sw, sh] = src;
        Placement::Overlay(OverlayPlacement {
            plane_id: plane.id,
            src_16_16: [sx, sy, sw, sh].map(|v| v.saturating_mul(1 << 16)),
            dst,
            stacking,
            color_encoding: plane.color_encodings.contains(&matrix).then_some(matrix),
            color_range: plane.color_ranges.contains(&range).then_some(range),
            colour_exact: plane.colour_exact(matrix, range),
        })
    }
}

#[cfg(unix)]
fn subsampled(fourcc: u32) -> bool {
    matches!(fourcc, DRM_FORMAT_NV12 | DRM_FORMAT_P010 | DRM_FORMAT_NV15)
}

#[cfg(unix)]
fn scale_fits(plane: &PlaneCaps, src: [u32; 4], dst: Rect) -> bool {
    let [_, _, sw, sh] = src;
    let axis = |s: u32, d: u32| -> bool {
        let s = u64::from(s);
        let d = u64::from(d);
        s <= d.saturating_mul(u64::from(plane.max_downscale.max(1)))
            && d <= s.saturating_mul(u64::from(plane.max_upscale.max(1)))
    };
    axis(sw, dst.width) && axis(sh, dst.height)
}

pub fn clip(
    picture: (u32, u32),
    dst: Rect,
    output: (u32, u32),
    even: bool,
) -> Option<([u32; 4], Rect)> {
    let (pw, ph) = picture;
    if pw == 0 || ph == 0 || dst.width == 0 || dst.height == 0 {
        return None;
    }
    let x0 = i64::from(dst.x);
    let y0 = i64::from(dst.y);
    let x1 = x0.saturating_add(i64::from(dst.width));
    let y1 = y0.saturating_add(i64::from(dst.height));
    let cx0 = x0.max(0);
    let cy0 = y0.max(0);
    let cx1 = x1.min(i64::from(output.0));
    let cy1 = y1.min(i64::from(output.1));
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    let map = |edge: i64, origin: i64, extent: u32, size: u32| -> i64 {
        let offset = edge.saturating_sub(origin);
        offset
            .saturating_mul(i64::from(size))
            .checked_div(i64::from(extent))
            .unwrap_or(0)
    };
    let align = |v: i64| if even { v & !1 } else { v };
    let align_up = |v: i64| if even { v.saturating_add(1) & !1 } else { v };
    let sx0 = align(map(cx0, x0, dst.width, pw));
    let sy0 = align(map(cy0, y0, dst.height, ph));
    let sx1 = align_up(map(cx1, x0, dst.width, pw)).min(i64::from(pw));
    let sy1 = align_up(map(cy1, y0, dst.height, ph)).min(i64::from(ph));
    let to_u32 = |v: i64| u32::try_from(v).ok();
    let width = to_u32(sx1.saturating_sub(sx0))?;
    let height = to_u32(sy1.saturating_sub(sy0))?;
    if width == 0 || height == 0 {
        return None;
    }
    let src = [to_u32(sx0)?, to_u32(sy0)?, width, height];
    let clipped = Rect {
        x: i32::try_from(cx0).ok()?,
        y: i32::try_from(cy0).ok()?,
        width: to_u32(cx1.saturating_sub(cx0))?,
        height: to_u32(cy1.saturating_sub(cy0))?,
    };
    Some((src, clipped))
}

pub fn parse_in_formats(blob: &[u8]) -> Vec<PlaneFormat> {
    let u32_at = |off: usize| -> Option<u32> {
        let bytes = blob.get(off..off.checked_add(4)?)?;
        Some(u32::from_ne_bytes(bytes.try_into().ok()?))
    };
    let u64_at = |off: usize| -> Option<u64> {
        let bytes = blob.get(off..off.checked_add(8)?)?;
        Some(u64::from_ne_bytes(bytes.try_into().ok()?))
    };
    let parse = || -> Option<Vec<PlaneFormat>> {
        let count_formats = u32_at(8)? as usize;
        let formats_offset = u32_at(12)? as usize;
        let count_modifiers = u32_at(16)? as usize;
        let modifiers_offset = u32_at(20)? as usize;
        let mut formats: Vec<PlaneFormat> = (0..count_formats)
            .map(|i| {
                Some(PlaneFormat {
                    fourcc: u32_at(formats_offset.checked_add(i.checked_mul(4)?)?)?,
                    modifiers: Vec::new(),
                })
            })
            .collect::<Option<_>>()?;
        // struct drm_format_modifier { u64 formats; u32 offset; u32 pad; u64 modifier; }
        for i in 0..count_modifiers {
            let base = modifiers_offset.checked_add(i.checked_mul(24)?)?;
            let mask = u64_at(base)?;
            let offset = u32_at(base.checked_add(8)?)? as usize;
            let modifier = u64_at(base.checked_add(16)?)?;
            for bit in 0..64usize {
                if mask >> bit & 1 == 1
                    && let Some(f) = formats.get_mut(offset.checked_add(bit)?)
                    && !f.modifiers.contains(&modifier)
                {
                    f.modifiers.push(modifier);
                }
            }
        }
        Some(formats)
    };
    parse().unwrap_or_default()
}
