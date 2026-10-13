//! HDR content composited at full precision.
//!
//! The desktop is drawn by vello into an 8-bit sRGB image. A client surface
//! or peer stream carrying an HDR signal (PQ, HLG or scRGB-style linear
//! light in 10-bit, half-float or P010 buffers) keeps two renditions:
//!
//! - an 8-bit, tone-mapped SDR image, drawn into the scene exactly like any
//!   other surface, and
//! - an [`HdrPlane`]: the signal at the buffer's own precision together with
//!   its image description.
//!
//! Every draw of such an image registers an [`HdrLayer`] while a frame is
//! being recorded ([`HdrLayerRecording`]). A presenter that drives an HDR
//! output renders the scene twice with the layer images substituted: once
//! with opaque black and once with a primary colour unique to the layer (red,
//! green or blue). vello's compositing is linear in a source colour, so the
//! difference between the two renders in the layer's channel is exactly how
//! much of the layer reaches each pixel: clipping, rounded corners, opacity,
//! antialiasing and everything drawn above it (popups, OSD, cursor, scrims)
//! included. The first render is the SDR desktop with the layers left out.
//!
//! The output pass then composites in linear light: the SDR desktop at the
//! user's SDR white, plus each layer's weight times its signal decoded from
//! its own description into the output's primaries, and encodes once. An SDR
//! output of a mixed setup instead gets each layer tone-mapped with
//! [`crate::shell::hdr_tonemap`]. Anything that renders the scene without
//! the substitution (screenshots, screencasts, SDR outputs, a pass that
//! cannot composite) sees the tone-mapped SDR rendition, so no path can show
//! a hole.
//!
//! [`composite_hdr_pixel`] is the CPU reference and the readback presenter's
//! implementation; `hdr_composite.wgsl` is its GPU mirror.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::ImageData;

use crate::collab::colorimetry::{ColorPrimaries, Colorimetry, TransferCharacteristics};
use crate::shell::color_pipeline::{
    BT709_PRIMARIES, BT2020_PRIMARIES, DISPLAY_P3_PRIMARIES, Mat3, SDR_REFERENCE_WHITE_NITS,
    bt709_to_bt2020_matrix, gamut_matrix, hlg_inverse_oetf, pack_xbgr2101010, pq_eotf,
    pq10_from_nits, srgb_eotf, srgb_oetf,
};
use crate::shell::hdr_tonemap::{HdrTonemapConfig, tonemap_rgb_signal_to_sdr_linear};

/// The GPU composite for PQ outputs; mirrors [`composite_hdr_pixel`].
pub const COMPOSITE_SHADER_SOURCE: &str = concat!(
    include_str!("output_orientation.wgsl"),
    "\n",
    include_str!("hdr_composite.wgsl")
);

/// Layers one frame can composite: one per weight channel.
pub const MAX_HDR_LAYERS: usize = 3;

/// The BT.2100 HLG nominal display peak the OOTF is defined against.
const HLG_NOMINAL_PEAK_NITS: f32 = 1000.0; // a luminance in cd/m², not a length
/// The HLG system gamma at [`HLG_NOMINAL_PEAK_NITS`].
const HLG_SYSTEM_GAMMA: f32 = 1.2;
/// Where BT.2408 puts HLG reference white on a 1000 cd/m² display: the same
/// diffuse white as SDR content in PQ.
const HLG_REFERENCE_WHITE_NITS: f32 = SDR_REFERENCE_WHITE_NITS;
/// BT.2020 luminance weights, for the HLG OOTF.
const BT2020_LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];

/// How an HDR signal turns into light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalTransfer {
    /// SMPTE ST 2084: absolute luminance.
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG: scene light, shown with the BT.2100 OOTF
    /// and HLG reference white placed at the output's SDR white.
    Hlg,
    /// Extended linear light (scRGB): `1.0` at the output's SDR white.
    Linear,
}

impl SignalTransfer {
    /// The value the composite shader switches on.
    pub fn shader_code(self) -> u32 {
        match self {
            Self::Pq => 0,
            Self::Hlg => 1,
            Self::Linear => 2,
        }
    }
}

/// An HDR surface's image description, as far as compositing needs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceSignal {
    pub transfer: SignalTransfer,
    pub primaries: ColorPrimaries,
    /// The description it came from, for the SDR tone-map.
    pub colorimetry: Colorimetry,
    /// How an SDR output compresses it.
    pub tonemap: HdrTonemapConfig,
}

impl SurfaceSignal {
    /// The signal a buffer described by `colorimetry` carries, or `None`
    /// when that is an SDR signal the 8-bit desktop already shows exactly.
    pub fn for_colorimetry(colorimetry: Colorimetry, tonemap: HdrTonemapConfig) -> Option<Self> {
        let transfer = match colorimetry.transfer {
            TransferCharacteristics::SmpteSt2084 => SignalTransfer::Pq,
            TransferCharacteristics::AribStdB67 => SignalTransfer::Hlg,
            TransferCharacteristics::Linear => SignalTransfer::Linear,
            TransferCharacteristics::Bt709
            | TransferCharacteristics::Srgb
            | TransferCharacteristics::Unspecified => return None,
        };
        Some(Self {
            transfer,
            primaries: colorimetry.primaries,
            colorimetry,
            tonemap,
        })
    }

    /// Linear signal primaries → linear BT.2020. Unspecified primaries are
    /// the signal's usual ones: BT.2020 for PQ and HLG, sRGB for scRGB.
    pub fn to_bt2020(&self) -> Mat3 {
        match (self.primaries, self.transfer) {
            (ColorPrimaries::Bt2020, _)
            | (ColorPrimaries::Unspecified, SignalTransfer::Pq | SignalTransfer::Hlg) => {
                Mat3::IDENTITY
            }
            (ColorPrimaries::DisplayP3, _) => {
                gamut_matrix(&DISPLAY_P3_PRIMARIES, &BT2020_PRIMARIES)
            }
            (ColorPrimaries::Bt709 | ColorPrimaries::Unspecified, _) => {
                gamut_matrix(&BT709_PRIMARIES, &BT2020_PRIMARIES)
            }
        }
    }
}

/// How an [`HdrPlane`] stores its texels. Both are the byte layouts of the
/// matching `wgpu` formats, so a plane uploads without conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTexelFormat {
    /// One little-endian word per pixel, red in the low ten bits (DRM
    /// `ABGR2101010`, wgpu `Rgb10a2Unorm`).
    Rgb10a2,
    /// Four little-endian IEEE half floats per pixel, red first (DRM
    /// `ABGR16161616F`, wgpu `Rgba16Float`).
    Rgba16Float,
}

impl HdrTexelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb10a2 => 4,
            Self::Rgba16Float => 8,
        }
    }
}

static NEXT_PLANE_ID: AtomicU64 = AtomicU64::new(1);

/// One frame of an HDR signal at its native precision, row-major, no
/// padding.
#[derive(Debug, Clone)]
pub struct HdrPlane {
    id: u64,
    width: u32,
    height: u32,
    format: HdrTexelFormat,
    bytes: Vec<u8>,
    signal: SurfaceSignal,
}

/// Planes are immutable once built and every build gets a fresh id, so the id
/// alone decides equality.
impl PartialEq for HdrPlane {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for HdrPlane {}

impl HdrPlane {
    /// Wrap `bytes` laid out as `format`, or `None` when they do not hold
    /// exactly `width × height` pixels.
    pub fn new(
        width: u32,
        height: u32,
        format: HdrTexelFormat,
        bytes: Vec<u8>,
        signal: SurfaceSignal,
    ) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(format.bytes_per_pixel())?;
        (width > 0 && height > 0 && bytes.len() == expected).then(|| Self {
            id: NEXT_PLANE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            format,
            bytes,
            signal,
        })
    }

    /// A plane of half-float texels from normalised signal values, alpha
    /// opaque.
    pub fn from_signal_rgb(
        width: u32,
        height: u32,
        rgb: impl IntoIterator<Item = [f32; 3]>,
        signal: SurfaceSignal,
    ) -> Option<Self> {
        let mut bytes = Vec::with_capacity(
            (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(8),
        );
        for [r, g, b] in rgb {
            for channel in [r, g, b, 1.0] {
                bytes.extend_from_slice(&f32_to_half(channel).to_le_bytes());
            }
        }
        Self::new(width, height, HdrTexelFormat::Rgba16Float, bytes, signal)
    }

    /// A plane from P010 planes (BT.2020 `Y'CbCr`, 10 bits in the top of each
    /// sample), converted to R'G'B' signal at half-float precision.
    /// `strides` are in samples.
    pub fn p010(
        luma: &[u16],
        chroma: &[u16],
        (y_stride, uv_stride): (u32, u32),
        (width, height): (u32, u32),
        signal: SurfaceSignal,
    ) -> Option<Self> {
        let limited = matches!(
            signal.colorimetry.range,
            crate::collab::colorimetry::ColorRange::Limited
        );
        let planes = crate::shell::hdr_tonemap::P010Planes {
            luma,
            chroma,
            luma_stride: y_stride,
            chroma_stride: uv_stride,
            limited,
        };
        let rgb = (0..height).flat_map(|row| {
            (0..width).map(move |col| planes.signal_at(col, row).unwrap_or([0.0; 3]))
        });
        Self::from_signal_rgb(width, height, rgb, signal)
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> HdrTexelFormat {
        self.format
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn signal(&self) -> &SurfaceSignal {
        &self.signal
    }

    /// The normalised signal of texel `(x, y)`, clamped into the plane.
    pub fn signal_at(&self, x: u32, y: u32) -> [f32; 3] {
        let x = x.min(self.width.saturating_sub(1)) as usize;
        let y = y.min(self.height.saturating_sub(1)) as usize;
        let bpp = self.format.bytes_per_pixel();
        let offset = y
            .saturating_mul(self.width as usize)
            .saturating_add(x)
            .saturating_mul(bpp);
        let Some(texel) = self.bytes.get(offset..offset.saturating_add(bpp)) else {
            return [0.0; 3];
        };
        match self.format {
            HdrTexelFormat::Rgb10a2 => {
                let word = texel.try_into().map(u32::from_le_bytes).unwrap_or_default();
                [0, 10, 20].map(|shift| {
                    let code = u16::try_from(word.wrapping_shr(shift) & 0x3FF).unwrap_or(0);
                    f32::from(code) / 1023.0
                })
            }
            HdrTexelFormat::Rgba16Float => [0_usize, 2, 4].map(|at| {
                let bits = texel
                    .get(at..at.saturating_add(2))
                    .and_then(|pair| pair.try_into().ok())
                    .map(u16::from_le_bytes)
                    .unwrap_or_default();
                let value = half_to_f32(bits);
                if value.is_finite() { value } else { 0.0 }
            }),
        }
    }

    /// The `(x, y, w, h)` part of this plane as a plane of its own, or
    /// `None` when that is empty or outside it.
    pub fn crop(&self, (x, y, w, h): (u32, u32, u32, u32)) -> Option<Self> {
        if (x, y, w, h) == (0, 0, self.width, self.height) {
            return Some(self.clone());
        }
        if x.checked_add(w)? > self.width || y.checked_add(h)? > self.height {
            return None;
        }
        let bpp = self.format.bytes_per_pixel();
        let stride = (self.width as usize).saturating_mul(bpp);
        let row_bytes = (w as usize).saturating_mul(bpp);
        let left = (x as usize).saturating_mul(bpp);
        let mut bytes = Vec::with_capacity(row_bytes.saturating_mul(h as usize));
        for row in y..y.saturating_add(h) {
            let start = (row as usize).saturating_mul(stride).saturating_add(left);
            bytes.extend_from_slice(self.bytes.get(start..start.saturating_add(row_bytes))?);
        }
        Self::new(w, h, self.format, bytes, self.signal)
    }
}

/// IEEE 754 half → `f32`.
pub fn half_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = i32::from((bits >> 10) & 0x1F);
    let mantissa = f32::from(bits & 0x3FF);
    match exponent {
        0 => sign * mantissa * 2.0_f32.powi(-24),
        31 => {
            if mantissa == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + mantissa / 1024.0) * 2.0_f32.powi(exponent.saturating_sub(15)),
    }
}

/// `f32` → IEEE 754 half, rounding to nearest; out-of-range values saturate
/// to the largest finite half and NaN becomes zero.
pub fn f32_to_half(value: f32) -> u16 {
    if value.is_nan() {
        return 0;
    }
    let sign: u16 = if value.is_sign_negative() { 0x8000 } else { 0 };
    let magnitude = value.abs().min(65_504.0);
    if magnitude < 2.0_f32.powi(-24) {
        return sign;
    }
    if magnitude < 2.0_f32.powi(-14) {
        // Subnormal: a multiple of 2^-24.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a subnormal half mantissa is below 1024 after rounding"
        )]
        let mantissa = (magnitude * 2.0_f32.powi(24)).round().min(1023.0) as u16;
        return sign | mantissa;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the exponent of a value in [2^-14, 65504] is in -14..=15"
    )]
    let exponent = magnitude.log2().floor() as i32;
    let scale = 2.0_f32.powi(exponent);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "(magnitude / scale - 1) is in [0, 1), so the rounded mantissa is at most 1024"
    )]
    let mut mantissa = ((magnitude / scale - 1.0) * 1024.0).round() as u16;
    let mut biased = u16::try_from(exponent.saturating_add(15)).unwrap_or(0);
    if mantissa >= 1024 {
        mantissa = 0;
        biased = biased.saturating_add(1).min(30);
    }
    sign | (biased << 10) | mantissa
}

/// Where one HDR surface appears in a frame.
#[derive(Debug, Clone)]
pub struct HdrLayer {
    pub plane: Arc<HdrPlane>,
    /// The tone-mapped SDR rendition drawn in the scene; the presenter
    /// substitutes it to find the layer's weight.
    pub image: ImageData,
    /// Plane texel → scene pixel.
    pub image_to_scene: Affine,
    /// Scene pixel → plane texel.
    pub scene_to_image: Affine,
    /// The scene pixels this entry may cover. Entries of one layer on
    /// several displays share a channel but never overlap.
    pub bounds: Rect,
    /// Which weight channel (0 red, 1 green, 2 blue) carries this layer.
    pub channel: u8,
}

impl HdrLayer {
    /// The same layer seen through `transform`, restricted to `clip` (both in
    /// the transformed space). `None` when nothing of it is left.
    #[must_use]
    pub fn transformed(&self, transform: Affine, clip: Rect) -> Option<Self> {
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine multiplication is floating-point matrix composition; it cannot panic"
        )]
        let image_to_scene = transform * self.image_to_scene;
        let bounds = transform.transform_rect_bbox(self.bounds).intersect(clip);
        (bounds.width() > 0.0 && bounds.height() > 0.0).then(|| Self {
            plane: Arc::clone(&self.plane),
            image: self.image.clone(),
            image_to_scene,
            scene_to_image: image_to_scene.inverse(),
            bounds,
            channel: self.channel,
        })
    }

    /// The plane texel under scene pixel `(x, y)`'s centre, when there is one.
    pub fn texel_at(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine × Point is floating-point arithmetic; it cannot panic"
        )]
        let texel = self.scene_to_image * Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
        let inside = texel.x >= 0.0
            && texel.y >= 0.0
            && texel.x < f64::from(self.plane.width())
            && texel.y < f64::from(self.plane.height());
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked non-negative and below the plane's u32 extent just above"
        )]
        inside.then_some((texel.x as u32, texel.y as u32))
    }

    /// Whether scene pixel `(x, y)`'s centre lies within [`Self::bounds`].
    pub fn covers(&self, x: u32, y: u32) -> bool {
        let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
        px >= self.bounds.x0 && py >= self.bounds.y0 && px < self.bounds.x1 && py < self.bounds.y1
    }
}

thread_local! {
    static RECORDING: RefCell<Option<Vec<HdrLayer>>> = const { RefCell::new(None) };
}

/// Collects the HDR layers drawn on this thread while it lives.
#[must_use = "layers are collected only while the recording is alive"]
pub struct HdrLayerRecording {
    /// What the thread was recording before, restored when this ends.
    previous: Option<Vec<HdrLayer>>,
    finished: bool,
}

impl HdrLayerRecording {
    pub fn begin() -> Self {
        Self {
            previous: RECORDING.with(|slot| slot.replace(Some(Vec::new()))),
            finished: false,
        }
    }

    /// The layers drawn since [`Self::begin`], bottom to top.
    pub fn finish(mut self) -> Vec<HdrLayer> {
        self.finished = true;
        let previous = self.previous.take();
        RECORDING
            .with(|slot| slot.replace(previous))
            .unwrap_or_default()
    }
}

impl Drop for HdrLayerRecording {
    fn drop(&mut self) {
        // An unfinished recording discards what it collected.
        if !self.finished {
            let previous = self.previous.take();
            RECORDING.with(|slot| slot.replace(previous));
        }
    }
}

/// Register that `image`, the SDR rendition of `plane`, is being drawn with
/// `image_to_scene` and can cover at most `bounds` (both in the coordinates
/// of the scene being recorded). A no-op outside a recording. Returns whether
/// the layer will be composited: a frame carries at most
/// [`MAX_HDR_LAYERS`] distinct planes, and the rest keep their tone-mapped
/// rendition.
pub fn record(
    plane: &Arc<HdrPlane>,
    image: &ImageData,
    image_to_scene: Affine,
    bounds: Rect,
) -> bool {
    if image.width != plane.width() || image.height != plane.height() {
        return false;
    }
    RECORDING.with(|recording| {
        let mut recording = recording.borrow_mut();
        let Some(layers) = recording.as_mut() else {
            return false;
        };
        let known = layers.iter().find(|layer| layer.plane.id() == plane.id());
        let channel = if let Some(layer) = known {
            layer.channel
        } else {
            let mut used = [false; MAX_HDR_LAYERS];
            for layer in layers.iter() {
                if let Some(slot) = used.get_mut(usize::from(layer.channel)) {
                    *slot = true;
                }
            }
            let Some(free) = used.iter().position(|taken| !taken) else {
                return false;
            };
            u8::try_from(free).unwrap_or(0)
        };
        layers.push(HdrLayer {
            plane: Arc::clone(plane),
            image: image.clone(),
            image_to_scene,
            scene_to_image: image_to_scene.inverse(),
            bounds,
            channel,
        });
        true
    })
}

/// Decode a normalised signal into linear BT.2020 light in cd/m², as an HDR
/// output shows it with SDR white at `sdr_white_nits`.
pub fn signal_to_output_nits(
    rgb: [f32; 3],
    signal: &SurfaceSignal,
    sdr_white_nits: f32,
) -> [f32; 3] {
    let light = match signal.transfer {
        SignalTransfer::Pq => rgb.map(|c| pq_eotf(c.clamp(0.0, 1.0))),
        SignalTransfer::Hlg => {
            let scene = rgb.map(|c| hlg_inverse_oetf(c.clamp(0.0, 1.0)));
            let [wr, wg, wb] = BT2020_LUMA;
            let luma = (wr * scene[0] + wg * scene[1] + wb * scene[2]).max(1e-6);
            let gain = HLG_NOMINAL_PEAK_NITS
                * luma.powf(HLG_SYSTEM_GAMMA - 1.0)
                * (sdr_white_nits / HLG_REFERENCE_WHITE_NITS);
            scene.map(|c| c * gain)
        }
        SignalTransfer::Linear => rgb.map(|c| c * sdr_white_nits),
    };
    signal.to_bt2020().mul_vec(light).map(|c| c.max(0.0))
}

/// What an output does with composited light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompositeTarget {
    /// A PQ BT.2020 HDR signal with SDR white at `sdr_white_nits`.
    Pq { sdr_white_nits: f32 },
    /// An sRGB SDR signal; HDR layers are tone-mapped.
    Sdr,
}

/// The weight each covering layer has at scene pixel `(x, y)`, given the
/// first (black) and second (channel colour) renders there.
fn layer_weights(
    first: [u8; 4],
    second: [u8; 4],
    x: u32,
    y: u32,
    layers: &[HdrLayer],
) -> impl Iterator<Item = (&HdrLayer, f32)> {
    layers.iter().filter_map(move |layer| {
        if !layer.covers(x, y) {
            return None;
        }
        let channel = usize::from(layer.channel);
        let (Some(&black), Some(&lit)) = (first.get(channel), second.get(channel)) else {
            return None;
        };
        let weight = (f32::from(lit) - f32::from(black)) / 255.0;
        (weight > 0.0).then_some((layer, weight.min(1.0)))
    })
}

/// Composite scene pixel `(x, y)` in linear light.
///
/// `first` is the render with every layer image black, `second` the render
/// with each in its channel colour. For [`CompositeTarget::Pq`] the result is
/// linear BT.2020 in cd/m²; for [`CompositeTarget::Sdr`] linear BT.709
/// relative to SDR white.
pub fn composite_hdr_pixel(
    first: [u8; 4],
    second: [u8; 4],
    x: u32,
    y: u32,
    layers: &[HdrLayer],
    target: CompositeTarget,
) -> [f32; 3] {
    let mut covered = 0.0_f32;
    let mut light = [0.0_f32; 3];
    for (layer, weight) in layer_weights(first, second, x, y, layers) {
        let Some((tx, ty)) = layer.texel_at(x, y) else {
            continue;
        };
        let rgb = layer.plane.signal_at(tx, ty);
        let signal = layer.plane.signal();
        let decoded = match target {
            CompositeTarget::Pq { sdr_white_nits } => {
                signal_to_output_nits(rgb, signal, sdr_white_nits)
            }
            CompositeTarget::Sdr => sdr_linear_for_signal(rgb, signal),
        };
        for (sum, value) in light.iter_mut().zip(decoded) {
            *sum += weight * value;
        }
        covered += weight;
    }
    let sdr = sdr_linear(first, covered);
    match target {
        CompositeTarget::Pq { sdr_white_nits } => {
            let sdr = bt709_to_bt2020_matrix().mul_vec(sdr);
            [0, 1, 2].map(|c| {
                sdr.get(c).copied().unwrap_or(0.0) * sdr_white_nits
                    + light.get(c).copied().unwrap_or(0.0)
            })
        }
        CompositeTarget::Sdr => [0, 1, 2]
            .map(|c| sdr.get(c).copied().unwrap_or(0.0) + light.get(c).copied().unwrap_or(0.0)),
    }
}

/// The SDR desktop's light at a pixel where layers cover `covered` of it:
/// what remains is SDR content with coverage `1 - covered`, linearised as
/// that content's own colour.
fn sdr_linear(first: [u8; 4], covered: f32) -> [f32; 3] {
    let coverage = (1.0 - covered).clamp(0.0, 1.0);
    if coverage <= 0.0 {
        return [0.0; 3];
    }
    [0, 1, 2].map(|c| {
        let value = f32::from(first.get(c).copied().unwrap_or(0)) / 255.0;
        coverage * srgb_eotf((value / coverage).min(1.0))
    })
}

/// Linear BT.709 light relative to SDR white for an SDR output.
fn sdr_linear_for_signal(rgb: [f32; 3], signal: &SurfaceSignal) -> [f32; 3] {
    tonemap_rgb_signal_to_sdr_linear(rgb, signal.colorimetry, signal.tonemap)
}

/// One `XBGR2101010` word of PQ signal for composited HDR light.
pub fn encode_pq_xbgr2101010(nits: [f32; 3]) -> u32 {
    pack_xbgr2101010(nits.map(pq10_from_nits))
}

/// sRGB bytes for composited SDR light.
pub fn encode_srgb8(light: [f32; 3]) -> [u8; 3] {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into 0.0..=255.0 before the cast"
    )]
    light.map(|c| (srgb_oetf(c) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// The weights and layers an output needs to composite on the CPU.
#[derive(Clone, Copy)]
pub struct CpuComposite<'a> {
    /// The second render (layers in their channel colours), tight RGBA at
    /// the combined scene's width.
    pub second: &'a [u8],
    pub scene_width: usize,
    pub layers: &'a [HdrLayer],
}

impl CpuComposite<'_> {
    /// Whether any layer can cover scene pixel `(x, y)`.
    pub fn touches(&self, x: u32, y: u32) -> bool {
        self.layers.iter().any(|layer| layer.covers(x, y))
    }

    /// The second render's pixel at `(x, y)`.
    pub fn second_at(&self, x: u32, y: u32) -> [u8; 4] {
        let offset = (y as usize)
            .saturating_mul(self.scene_width)
            .saturating_add(x as usize)
            .saturating_mul(4);
        self.second
            .get(offset..offset.saturating_add(4))
            .and_then(|pixel| pixel.try_into().ok())
            .unwrap_or([0; 4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::colorimetry::{ColorRange, MatrixCoefficients};
    use crate::shell::color_pipeline::pq_inverse_eotf;

    fn pq_signal() -> SurfaceSignal {
        SurfaceSignal::for_colorimetry(Colorimetry::hdr10(), HdrTonemapConfig::default())
            .expect("PQ is HDR")
    }

    fn image(width: u32, height: u32) -> ImageData {
        ui::raster::rgba8_premultiplied(
            vec![
                0_u8;
                (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4)
            ],
            width,
            height,
        )
    }

    fn close(a: [f32; 3], b: [f32; 3], relative: f32) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (x - y).abs() <= relative * y.abs().max(1.0))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn hdr_surface_decodes_describes_and_composites() {
        // Phase `half_floats_round_trip`.
        for value in [
            0.0_f32, 1.0, 0.5, 0.123_456, 3.75, 1000.0, 65_504.0, 1e-5, -2.0,
        ] {
            let back = half_to_f32(f32_to_half(value));
            assert!(
                (back - value).abs() <= value.abs() * 1e-3 + 1e-7,
                "{value} came back as {back}"
            );
        }
        assert_eq!(f32_to_half(1.0), 0x3C00);

        // Phase `only_hdr_descriptions_make_planes`.
        let tonemap = HdrTonemapConfig::default();
        assert!(SurfaceSignal::for_colorimetry(Colorimetry::sdr_bt709(), tonemap).is_none());
        assert_eq!(pq_signal().transfer, SignalTransfer::Pq);
        let hlg = SurfaceSignal::for_colorimetry(Colorimetry::hdr_hlg(), tonemap).expect("HLG");
        assert_eq!(hlg.transfer, SignalTransfer::Hlg);
        let scrgb = Colorimetry {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristics::Linear,
            matrix: MatrixCoefficients::Identity,
            range: ColorRange::Full,
        };
        let scrgb = SurfaceSignal::for_colorimetry(scrgb, tonemap).expect("scRGB");
        assert_eq!(scrgb.transfer, SignalTransfer::Linear);

        // Phase `decode_places_reference_whites_at_sdr_white`.
        {
            let white = 240.0;
            // scRGB 1.0 is SDR white, its BT.709 primaries converted.
            let [r, g, b] = signal_to_output_nits([1.0, 1.0, 1.0], &scrgb, white);
            for c in [r, g, b] {
                assert!((c - white).abs() < 0.5, "scRGB white gave {c}");
            }
            // BT.2408: HLG 75% is reference white.
            let [r, g, b] = signal_to_output_nits([0.75, 0.75, 0.75], &hlg, white);
            for c in [r, g, b] {
                assert!((c - white).abs() / white < 0.01, "HLG 75% gave {c}");
            }
            // PQ is absolute whatever the SDR white.
            let code = pq_inverse_eotf(600.0);
            let [r, _, _] = signal_to_output_nits([code, code, code], &pq_signal(), white);
            assert!((r - 600.0).abs() < 1.0, "PQ 600 nits gave {r}");
        }

        // Phase `plane_texels_read_back_and_crop`.
        {
            let signal = pq_signal();
            let words: Vec<u8> = [(1023_u32, 0_u32, 512_u32), (0, 1023, 0)]
                .into_iter()
                .flat_map(|(r, g, b)| (r | (g << 10) | (b << 20) | 0xC000_0000).to_le_bytes())
                .collect();
            let plane =
                HdrPlane::new(2, 1, HdrTexelFormat::Rgb10a2, words, signal).expect("valid plane");
            let [r, g, b] = plane.signal_at(0, 0);
            assert!((r - 1.0).abs() < 1e-6 && g == 0.0 && (b - 512.0 / 1023.0).abs() < 1e-6);
            let right = plane.crop((1, 0, 1, 1)).expect("crop");
            assert_eq!(right.signal_at(0, 0), [0.0, 1.0, 0.0]);
            assert_ne!(right.id(), plane.id());
            assert!(plane.crop((1, 0, 2, 1)).is_none());
            assert!(HdrPlane::new(3, 1, HdrTexelFormat::Rgb10a2, vec![0; 8], signal).is_none());
        }

        // Phase `recording_assigns_channels_and_caps_layers`.
        {
            let signal = pq_signal();
            let plane =
                || Arc::new(HdrPlane::from_signal_rgb(2, 2, [[0.5; 3]; 4], signal).expect("plane"));
            let bounds = Rect::new(0.0, 0.0, 2.0, 2.0);
            let outside = plane();
            assert!(!record(&outside, &image(2, 2), Affine::IDENTITY, bounds));
            let recording = HdrLayerRecording::begin();
            let first = plane();
            assert!(record(&first, &image(2, 2), Affine::IDENTITY, bounds));
            assert!(record(
                &first,
                &image(2, 2),
                Affine::translate((4.0, 0.0)),
                bounds
            ));
            assert!(
                !record(&plane(), &image(3, 2), Affine::IDENTITY, bounds),
                "an image of another size is not this plane's rendition"
            );
            for _ in 0..2 {
                assert!(record(&plane(), &image(2, 2), Affine::IDENTITY, bounds));
            }
            assert!(
                !record(&plane(), &image(2, 2), Affine::IDENTITY, bounds),
                "a fourth plane has no channel left"
            );
            let layers = recording.finish();
            let channels: Vec<u8> = layers.iter().map(|layer| layer.channel).collect();
            assert_eq!(channels, [0, 0, 1, 2]);
            assert!(!record(&first, &image(2, 2), Affine::IDENTITY, bounds));
        }

        // Phase `cpu_composite_is_linear_light`.
        {
            let white = 203.0;
            let code = pq_inverse_eotf(1000.0);
            let plane = Arc::new(
                HdrPlane::from_signal_rgb(4, 4, [[code; 3]; 16], pq_signal()).expect("plane"),
            );
            let layers = [HdrLayer {
                plane,
                image: image(4, 4),
                image_to_scene: Affine::IDENTITY,
                scene_to_image: Affine::IDENTITY,
                bounds: Rect::new(0.0, 0.0, 4.0, 4.0),
                channel: 0,
            }];
            let target = CompositeTarget::Pq {
                sdr_white_nits: white,
            };
            // Fully visible: only the layer's light.
            let full = composite_hdr_pixel([0, 0, 0, 255], [255, 0, 0, 255], 1, 1, &layers, target);
            assert!(close(full, [1000.0; 3], 0.01), "{full:?}");
            // Under a half-transparent white scrim: half white, half layer.
            let half = composite_hdr_pixel(
                [128, 128, 128, 255],
                [255, 128, 128, 255],
                1,
                1,
                &layers,
                target,
            );
            // The scrim covers 128/255 of the pixel, the layer the rest.
            let expected = 128.0 / 255.0 * white + 127.0 / 255.0 * 1000.0;
            assert!(close(half, [expected; 3], 0.02), "{half:?} vs {expected}");
            // Outside the layer's bounds only the desktop counts.
            let outside = composite_hdr_pixel(
                [255, 255, 255, 255],
                [255, 255, 255, 255],
                9,
                9,
                &layers,
                target,
            );
            assert!(close(outside, [white; 3], 0.01), "{outside:?}");
            // An SDR output tone-maps the layer instead.
            let sdr = composite_hdr_pixel(
                [0, 0, 0, 255],
                [255, 0, 0, 255],
                1,
                1,
                &layers,
                CompositeTarget::Sdr,
            );
            let reference = tonemap_rgb_signal_to_sdr_linear(
                [code; 3],
                Colorimetry::hdr10(),
                HdrTonemapConfig::default(),
            );
            // The plane holds the signal at half precision.
            assert!(close(sdr, reference, 5e-3), "{sdr:?} vs {reference:?}");
        }
    }
}
