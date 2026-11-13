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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::dmabuf::{Colorimetry, DRM_FORMAT_ARGB8888, DRM_FORMAT_XRGB8888, DmabufImage};
    use crate::frame::{FramePayload, PlanarFormat, PlanarImage};
    use std::sync::Arc;

    /// ARM AFBC with a 16x16 superblock, as VOP2's cluster windows list it.
    const AFBC: u64 = 0x0800_0000_0000_0001;

    fn nv12_frame(width: u32, height: u32, modifier: u64) -> Frame {
        Frame {
            width,
            height,
            pts_ms: 0,
            payload: FramePayload::Dmabuf(Arc::new(DmabufImage {
                planes: vec![],
                fourcc: DRM_FORMAT_NV12,
                modifier,
                width,
                height,
                colorimetry: Colorimetry::BT709_LIMITED,
            })),
        }
    }

    /// The planes VOP2 exposes on an RK3566's first video port, as probed on
    /// an Orange Pi 3B under mainline 7.2: an Esmart window that takes linear
    /// NV12 and a Cluster window that takes AFBC only. Neither has a
    /// `COLOR_ENCODING` property.
    fn vop2() -> (PrimaryPlane, Vec<PlaneCaps>) {
        let rgb = |fourcc| PlaneFormat {
            fourcc,
            modifiers: vec![DRM_FORMAT_MOD_LINEAR],
        };
        (
            PrimaryPlane {
                zpos: 0,
                alpha: true,
            },
            vec![
                PlaneCaps {
                    id: 41,
                    formats: vec![
                        rgb(DRM_FORMAT_XRGB8888),
                        rgb(DRM_FORMAT_ARGB8888),
                        rgb(DRM_FORMAT_NV12),
                        rgb(DRM_FORMAT_NV15),
                    ],
                    max_downscale: 8,
                    max_upscale: 8,
                    zpos: Some(Zpos {
                        value: 3,
                        range: None,
                    }),
                    color_encodings: vec![],
                    color_ranges: vec![],
                },
                PlaneCaps {
                    id: 47,
                    formats: vec![PlaneFormat {
                        fourcc: DRM_FORMAT_XRGB8888,
                        modifiers: vec![AFBC],
                    }],
                    max_downscale: 8,
                    max_upscale: 8,
                    zpos: Some(Zpos {
                        value: 4,
                        range: None,
                    }),
                    color_encodings: vec![],
                    color_ranges: vec![],
                },
            ],
        )
    }

    #[test]
    fn decoded_nv12_goes_on_the_plane_that_takes_it() {
        let (primary, planes) = vop2();
        let frame = nv12_frame(1920, 1080, DRM_FORMAT_MOD_LINEAR);
        let req = VideoSurfaceRequest {
            frame: &frame,
            dst: Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            ui_overlaps: false,
        };
        let Placement::Overlay(p) = place(&req, (1920, 1080), primary, &planes, &[]) else {
            panic!("fullscreen linear NV12 belongs on the Esmart plane");
        };
        assert_eq!(p.plane_id, 41);
        assert_eq!(p.src_16_16, [0, 0, 1920 << 16, 1080 << 16]);
        assert_eq!(p.stacking, Stacking::Above);
        // VOP2 has no COLOR_ENCODING property: BT.709 shows through BT.601.
        assert!(!p.colour_exact);
        assert_eq!(p.color_encoding, None);

        let frame = nv12_frame(1920, 1080, DRM_FORMAT_MOD_INVALID);
        let req = VideoSurfaceRequest {
            frame: &frame,
            ..req
        };
        assert!(matches!(
            place(&req, (1920, 1080), primary, &planes, &[]),
            Placement::Overlay(_)
        ));

        // Once the Esmart plane is taken, the AFBC-only cluster cannot help.
        assert_eq!(
            place(&req, (1920, 1080), primary, &planes, &[41]),
            Placement::Composite(CompositeReason::Format {
                fourcc: DRM_FORMAT_NV12,
                modifier: DRM_FORMAT_MOD_INVALID
            })
        );
    }

    #[test]
    fn placement_respects_scaling_stacking_and_memory() {
        let (primary, planes) = vop2();
        let frame = nv12_frame(1920, 1080, DRM_FORMAT_MOD_LINEAR);

        let tiny = VideoSurfaceRequest {
            frame: &frame,
            dst: Rect {
                x: 10,
                y: 10,
                width: 120,
                height: 67,
            },
            ui_overlaps: false,
        };
        assert_eq!(
            place(&tiny, (1920, 1080), primary, &planes, &[]),
            Placement::Composite(CompositeReason::Scale)
        );

        // Controls over the video: the plane must go below a hole. The Esmart
        // plane's zpos (3) is immutable and above the primary (0), so it
        // cannot, and the frame is composited.
        let windowed = VideoSurfaceRequest {
            frame: &frame,
            dst: Rect {
                x: 100,
                y: 100,
                width: 960,
                height: 540,
            },
            ui_overlaps: true,
        };
        assert_eq!(
            place(&windowed, (1920, 1080), primary, &planes, &[]),
            Placement::Composite(CompositeReason::NeedsUnderlay)
        );
        let mut movable = planes.clone();
        movable[0].zpos = Some(Zpos {
            value: 3,
            range: Some((0, 5)),
        });
        let primary_above = PrimaryPlane {
            zpos: 2,
            alpha: true,
        };
        let Placement::Overlay(p) = place(&windowed, (1920, 1080), primary_above, &movable, &[])
        else {
            panic!("a movable plane goes under the hole");
        };
        assert_eq!(p.stacking, Stacking::BelowHole);
        // But not when the desktop has no alpha to cut the hole with.
        assert_eq!(
            place(
                &windowed,
                (1920, 1080),
                PrimaryPlane {
                    zpos: 2,
                    alpha: false
                },
                &movable,
                &[]
            ),
            Placement::Composite(CompositeReason::NeedsUnderlay)
        );

        // A picture in system memory is never a plane candidate.
        let planar = Frame {
            width: 2,
            height: 2,
            pts_ms: 0,
            payload: FramePayload::Planar(Arc::new(PlanarImage {
                format: PlanarFormat::Nv12,
                planes: vec![],
                colorimetry: Colorimetry::default(),
            })),
        };
        let req = VideoSurfaceRequest {
            frame: &planar,
            ..tiny
        };
        assert_eq!(
            place(&req, (1920, 1080), primary, &planes, &[]),
            Placement::Composite(CompositeReason::NotDmabuf)
        );
    }

    #[test]
    fn a_window_hanging_off_the_output_crops_its_source_in_proportion() {
        let (src, dst) = clip(
            (1920, 1080),
            Rect {
                x: 1920 - 1920,
                y: -100,
                width: 3840,
                height: 2160,
            },
            (1920, 1080),
            true,
        )
        .unwrap();
        assert_eq!(
            dst,
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(src, [0, 50, 960, 540]);

        let (src, _) = clip(
            (1920, 1080),
            Rect {
                x: -3,
                y: 0,
                width: 1920,
                height: 1080,
            },
            (1920, 1080),
            true,
        )
        .unwrap();
        assert_eq!(src[0] % 2, 0, "even x for subsampled chroma");
        assert_eq!(src[0], 2);

        assert!(
            clip(
                (1920, 1080),
                Rect {
                    x: 2000,
                    y: 0,
                    width: 100,
                    height: 100
                },
                (1920, 1080),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn in_formats_blobs_parse_into_per_format_modifiers() {
        // Two formats, NV12 linear only and XR24 linear + AFBC.
        let mut blob = Vec::new();
        let formats_offset = 24u32;
        let modifiers_offset = formats_offset + 8;
        for v in [1u32, 0, 2, formats_offset, 2, modifiers_offset] {
            blob.extend_from_slice(&v.to_ne_bytes());
        }
        blob.extend_from_slice(&DRM_FORMAT_NV12.to_ne_bytes());
        blob.extend_from_slice(&DRM_FORMAT_XRGB8888.to_ne_bytes());
        blob.extend_from_slice(&0b11u64.to_ne_bytes());
        blob.extend_from_slice(&0u32.to_ne_bytes());
        blob.extend_from_slice(&0u32.to_ne_bytes());
        blob.extend_from_slice(&DRM_FORMAT_MOD_LINEAR.to_ne_bytes());
        // AFBC: XR24 only (bit 1).
        blob.extend_from_slice(&0b10u64.to_ne_bytes());
        blob.extend_from_slice(&0u32.to_ne_bytes());
        blob.extend_from_slice(&0u32.to_ne_bytes());
        blob.extend_from_slice(&AFBC.to_ne_bytes());

        let parsed = parse_in_formats(&blob);
        assert_eq!(
            parsed,
            vec![
                PlaneFormat {
                    fourcc: DRM_FORMAT_NV12,
                    modifiers: vec![DRM_FORMAT_MOD_LINEAR]
                },
                PlaneFormat {
                    fourcc: DRM_FORMAT_XRGB8888,
                    modifiers: vec![DRM_FORMAT_MOD_LINEAR, AFBC]
                },
            ]
        );
        assert!(parse_in_formats(&blob[..30]).is_empty());
        assert!(parse_in_formats(&[]).is_empty());
    }
}
