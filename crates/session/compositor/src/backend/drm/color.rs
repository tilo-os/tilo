//! An output's colour: ICC calibration and HDR signalling.
//!
//! An output with an ICC profile, or driven in HDR, gets a final GLES pass:
//! the scene renders into an offscreen texture, and one element draws that
//! texture onto the output through a shader that applies the profile (a 3D
//! lookup table sampled from the qcms transform sRGB to the panel) and, for
//! HDR, encodes it as a PQ BT.2020 signal with SDR white at the person's
//! chosen brightness. HDR also tells the display: the connector's
//! `HDR_OUTPUT_METADATA`, `Colorspace` and `max bpc`. Outputs without either
//! keep the plain path, planes and direct scanout included.

use std::path::{Path, PathBuf};

use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::DrmDevice;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{Element, Id, RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, ffi,
};
use smithay::backend::renderer::multigpu::Error as MultiError;
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Bind as _, ImportMem as _, Offscreen as _, RendererSuper};
use smithay::output::Output;
use smithay::reexports::drm::control::{Device as _, connector, property};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use super::Renderer;
use crate::config::Hdr;
use crate::render::OutputRenderElement;

/// Grid points a side of the calibration lookup table.
pub(crate) const GRID: u32 = 33;

/// The calibration lookup table for `transform` (RGBA8 in, RGBA8 out): the
/// grid's colours run through it, laid out as `GRID` blue slices side by
/// side, red across each slice, green down. RGBA8 rows, `GRID * GRID`
/// pixels wide, `GRID` high.
pub(crate) fn lut_atlas(transform: impl Fn(&mut [u8])) -> Vec<u8> {
    let step = |index: u32| -> u8 {
        u8::try_from(
            index
                .saturating_mul(255)
                .checked_div(GRID.saturating_sub(1))
                .unwrap_or(0),
        )
        .unwrap_or(u8::MAX)
    };
    let mut atlas = Vec::with_capacity(usize::try_from(GRID.pow(3).saturating_mul(4)).unwrap_or(0));
    for green in 0..GRID {
        for blue in 0..GRID {
            for red in 0..GRID {
                atlas.extend_from_slice(&[step(red), step(green), step(blue), 255]);
            }
        }
    }
    transform(&mut atlas);
    atlas
}

/// The lookup table of the ICC profile at `path` (perceptual, from sRGB);
/// `None` when qcms cannot read it.
pub(crate) fn icc_atlas(path: &Path) -> Option<Vec<u8>> {
    let input = qcms::Profile::new_sRGB();
    let output = qcms::Profile::new_from_path(path.to_str()?)?;
    let transform = qcms::Transform::new(
        &input,
        &output,
        qcms::DataType::RGBA8,
        qcms::Intent::Perceptual,
    )?;
    Some(lut_atlas(|pixels| transform.apply(pixels)))
}

/// Length of `struct hdr_output_metadata` in `drm_mode.h`: a `u32` type and
/// the 26-byte `hdr_metadata_infoframe`, padded to the struct's 4-byte
/// alignment. The kernel refuses a blob of any other size.
pub(crate) const HDR_OUTPUT_METADATA_BYTES: usize = 32;

/// CTA-861-G EOTF code for SMPTE ST 2084 (PQ).
const EOTF_SMPTE_ST2084: u8 = 2;

/// The `HDR_OUTPUT_METADATA` blob for a PQ signal: the display's primaries
/// and white point as the mastering display, its peak and frame-average
/// luminance as MaxCLL/MaxFALL. CTA-861.3 defines zero as "unknown".
///
/// Chromaticities are in units of 0.00002; the primaries go red, green,
/// blue, the order the kernel copies into the infoframe.
pub(crate) fn hdr_output_metadata(hdr: &Hdr) -> [u8; HDR_OUTPUT_METADATA_BYTES] {
    let chroma = |micro: u32| -> u16 {
        u16::try_from(micro.checked_div(20).unwrap_or(0).min(50_000)).unwrap_or(50_000)
    };
    let mut fields: Vec<u8> = Vec::with_capacity(HDR_OUTPUT_METADATA_BYTES);
    // `metadata_type`: HDMI_STATIC_METADATA_TYPE1.
    fields.extend_from_slice(&0_u32.to_le_bytes());
    fields.push(EOTF_SMPTE_ST2084);
    // Infoframe `metadata_type`: static metadata type 1.
    fields.push(0);
    for (x, y) in hdr.primaries.iter().chain([&hdr.white_point]) {
        fields.extend_from_slice(&chroma(*x).to_le_bytes());
        fields.extend_from_slice(&chroma(*y).to_le_bytes());
    }
    // Mastering luminance: max in cd/m², min in 0.0001 cd/m².
    fields.extend_from_slice(&hdr.max_nits.to_le_bytes());
    let min = u16::try_from(hdr.min_luminance_micro_nits).unwrap_or(u16::MAX);
    fields.extend_from_slice(&min.to_le_bytes());
    // MaxCLL and MaxFALL.
    fields.extend_from_slice(&hdr.max_nits.to_le_bytes());
    fields.extend_from_slice(&hdr.max_frame_average_nits.to_le_bytes());
    let mut blob = [0_u8; HDR_OUTPUT_METADATA_BYTES];
    for (slot, byte) in blob.iter_mut().zip(fields) {
        *slot = byte;
    }
    blob
}

/// The `Colorspace` enum values for SDR (`Default`) and HDR (`BT2020_RGB`).
pub(crate) fn colorspace_values<'a>(
    values: impl IntoIterator<Item = (u64, &'a [u8])>,
) -> Option<(u64, u64)> {
    let mut default = None;
    let mut bt2020 = None;
    for (value, name) in values {
        match name {
            b"Default" => default = Some(value),
            b"BT2020_RGB" => bt2020 = Some(value),
            _ => {}
        }
    }
    Some((default?, bt2020?))
}

/// The link depth HDR needs: ten bits a channel, or more when the connector
/// already runs deeper.
pub(crate) fn hdr_max_bpc(current: u64, range_max: u64) -> Option<u64> {
    (range_max >= 10).then(|| current.max(10).min(range_max))
}

/// The final pass's fragment shader, in smithay's texture-shader form.
const SHADER: &str = r"
//_DEFINES
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform sampler2D lut;
uniform float use_lut;
uniform float pq;
uniform float sdr_white;
const float N = 33.0;

vec3 calibrate(vec3 c) {
    float b = clamp(c.b, 0.0, 1.0) * (N - 1.0);
    float b0 = floor(b);
    float b1 = min(b0 + 1.0, N - 1.0);
    vec2 rg = (clamp(c.rg, 0.0, 1.0) * (N - 1.0) + 0.5) / vec2(N * N, N);
    vec3 lo = texture2D(lut, rg + vec2(b0 / N, 0.0)).rgb;
    vec3 hi = texture2D(lut, rg + vec2(b1 / N, 0.0)).rgb;
    return mix(lo, hi, b - b0);
}

float srgb_eotf(float v) {
    float c = clamp(v, 0.0, 1.0);
    return c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4);
}

float pq_inverse_eotf(float nits) {
    float y = pow(clamp(nits / 10000.0, 0.0, 1.0), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * y) / (1.0 + 18.6875 * y), 78.84375);
}

vec3 encode_pq(vec3 c) {
    vec3 linear = vec3(srgb_eotf(c.r), srgb_eotf(c.g), srgb_eotf(c.b));
    vec3 bt2020 = vec3(
        dot(vec3(0.627404, 0.329283, 0.043313), linear),
        dot(vec3(0.069097, 0.919540, 0.011362), linear),
        dot(vec3(0.016391, 0.088013, 0.895595), linear));
    vec3 nits = bt2020 * sdr_white;
    return vec3(pq_inverse_eotf(nits.r), pq_inverse_eotf(nits.g), pq_inverse_eotf(nits.b));
}

void main() {
    vec3 rgb = texture2D(tex, v_coords).rgb;
    if (use_lut > 0.5) {
        rgb = calibrate(rgb);
    }
    if (pq > 0.5) {
        rgb = encode_pq(rgb);
    }
    gl_FragColor = vec4(rgb, 1.0) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        gl_FragColor = vec4(0.0, 0.2, 0.0, 0.2) + gl_FragColor * 0.8;
#endif
}
";

/// What an output's colour is asked to be.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Wanted {
    pub profile: Option<PathBuf>,
    pub hdr: Option<Hdr>,
}

impl Wanted {
    pub(crate) fn plain(&self) -> bool {
        self.profile.is_none() && self.hdr.is_none()
    }
}

/// An output's final pass: the offscreen scene and how it is drawn.
pub(crate) struct ColorPass {
    wanted: Wanted,
    texture: GlesTexture,
    size: Size<i32, Physical>,
    damage: OutputDamageTracker,
    age: usize,
    program: GlesTexProgram,
    lut: Option<GlesTexture>,
    id: Id,
    commit: CommitCounter,
}

impl ColorPass {
    pub(crate) fn wanted(&self) -> &Wanted {
        &self.wanted
    }

    /// The pass for `output` as `wanted`, on the renderer drawing it.
    pub(crate) fn new(
        renderer: &mut Renderer<'_>,
        output: &Output,
        wanted: Wanted,
    ) -> Result<Self, String> {
        let mode = output.current_mode().ok_or("no mode")?;
        let size = output.current_transform().transform_size(mode.size);
        let gles: &mut GlesRenderer = renderer.as_mut();
        let program = gles
            .compile_custom_texture_shader(
                SHADER,
                &[
                    UniformName::new("lut", UniformType::_1i),
                    UniformName::new("use_lut", UniformType::_1f),
                    UniformName::new("pq", UniformType::_1f),
                    UniformName::new("sdr_white", UniformType::_1f),
                ],
            )
            .map_err(|error| format!("colour shader: {error:?}"))?;
        let lut = match &wanted.profile {
            Some(path) => {
                let atlas = icc_atlas(path)
                    .ok_or_else(|| format!("{}: not a readable ICC profile", path.display()))?;
                let edge = i32::try_from(GRID).unwrap_or(1);
                Some(
                    gles.import_memory(
                        &atlas,
                        Fourcc::Abgr8888,
                        (edge.saturating_mul(edge), edge).into(),
                        false,
                    )
                    .map_err(|error| format!("lookup table: {error:?}"))?,
                )
            }
            None => None,
        };
        let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
        let texture = renderer
            .create_buffer(Fourcc::Abgr8888, buffer_size)
            .map_err(|error| format!("offscreen scene: {error:?}"))?;
        let scale = output.current_scale().fractional_scale();
        Ok(Self {
            wanted,
            texture,
            size,
            damage: OutputDamageTracker::new(size, scale, Transform::Normal),
            age: 0,
            program,
            lut,
            id: Id::new(),
            commit: CommitCounter::default(),
        })
    }

    /// Render the scene offscreen; the pass's element then draws it.
    pub(crate) fn render<'r>(
        &mut self,
        renderer: &mut Renderer<'r>,
        elements: &[OutputRenderElement<Renderer<'r>>],
    ) -> Result<RenderElementStates, String> {
        let mut framebuffer = renderer
            .bind(&mut self.texture)
            .map_err(|error| format!("bind offscreen scene: {error:?}"))?;
        let result = self
            .damage
            .render_output(
                renderer,
                &mut framebuffer,
                self.age,
                elements,
                crate::render::BACKGROUND,
            )
            .map_err(|error| format!("offscreen scene: {error:?}"))?;
        drop(framebuffer);
        self.age = 1;
        if result.damage.is_some_and(|damage| !damage.is_empty()) {
            self.commit.increment();
        }
        Ok(result.states)
    }

    fn draw_gles(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let lut = self.lut.as_ref().map(GlesTexture::tex_id);
        if let Some(lut) = lut {
            frame.with_context(|gl| {
                // SAFETY: the renderer's context is current inside a frame;
                // unit 1 is restored after the draw.
                unsafe {
                    gl.ActiveTexture(ffi::TEXTURE1);
                    gl.BindTexture(ffi::TEXTURE_2D, lut);
                    for (parameter, value) in [
                        (ffi::TEXTURE_MIN_FILTER, ffi::LINEAR),
                        (ffi::TEXTURE_MAG_FILTER, ffi::LINEAR),
                        (ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE),
                        (ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE),
                    ] {
                        gl.TexParameteri(
                            ffi::TEXTURE_2D,
                            parameter,
                            i32::try_from(value).unwrap_or(0),
                        );
                    }
                    gl.ActiveTexture(ffi::TEXTURE0);
                }
            })?;
        }
        let flag = |on: bool| if on { 1.0_f32 } else { 0.0 };
        let drawn = frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &[
                Uniform::new("lut", 1_i32),
                Uniform::new("use_lut", flag(lut.is_some())),
                Uniform::new("pq", flag(self.wanted.hdr.is_some())),
                Uniform::new(
                    "sdr_white",
                    self.wanted
                        .hdr
                        .as_ref()
                        .map_or(0.0, |hdr| hdr.sdr_white_nits),
                ),
            ],
        );
        if lut.is_some() {
            frame.with_context(|gl| {
                // SAFETY: as above.
                unsafe {
                    gl.ActiveTexture(ffi::TEXTURE1);
                    gl.BindTexture(ffi::TEXTURE_2D, 0);
                    gl.ActiveTexture(ffi::TEXTURE0);
                }
            })?;
        }
        drawn
    }
}

impl Element for ColorPass {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(
            self.size
                .to_logical(1)
                .to_buffer(1, Transform::Normal)
                .to_f64(),
        )
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::new(Point::default(), self.size)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[self.geometry(scale)])
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::from_slice(&[self.geometry(scale)])
    }
}

impl<'r> RenderElement<Renderer<'r>> for ColorPass {
    fn draw(
        &self,
        frame: &mut <Renderer<'r> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <Renderer<'r> as RendererSuper>::Error> {
        self.draw_gles(frame.as_mut(), src, dst, damage, opaque_regions)
            .map_err(MultiError::Render)
    }
}

/// Switch HDR signalling on `connector` on (with `hdr`) or off. Properties
/// the driver lacks are skipped; a display that cannot take HDR shows the
/// PQ picture washed out, which the session avoids by asking only for
/// displays whose EDID offers PQ.
pub(crate) fn signal_hdr(
    device: &DrmDevice,
    connector: connector::Handle,
    hdr: Option<&Hdr>,
) -> Result<(), String> {
    let properties = device
        .get_properties(connector)
        .map_err(|error| format!("connector properties: {error}"))?;
    let find = |name: &[u8]| -> Option<(property::Handle, property::Info, u64)> {
        properties.iter().find_map(|(handle, value)| {
            let info = device.get_property(*handle).ok()?;
            (info.name().to_bytes() == name).then_some((*handle, info, *value))
        })
    };
    let Some((metadata, _, _)) = find(b"HDR_OUTPUT_METADATA") else {
        return Err("the driver offers no HDR metadata on this connector".into());
    };
    let blob = match hdr {
        Some(hdr) => {
            let bytes = hdr_output_metadata(hdr);
            let blob = device
                .create_property_blob(&bytes)
                .map_err(|error| format!("HDR metadata blob: {error}"))?;
            u64::from(blob)
        }
        None => 0,
    };
    device
        .set_property(connector, metadata, blob)
        .map_err(|error| format!("HDR metadata: {error}"))?;
    if let Some((handle, info, _)) = find(b"Colorspace")
        && let property::ValueType::Enum(values) = info.value_type()
    {
        let (_, entries) = values.values();
        let named = entries
            .iter()
            .map(|entry| (entry.value(), entry.name().to_bytes()));
        if let Some((default, bt2020)) = colorspace_values(named) {
            let value = if hdr.is_some() { bt2020 } else { default };
            device
                .set_property(connector, handle, value)
                .map_err(|error| format!("colorspace: {error}"))?;
        }
    }
    if hdr.is_some()
        && let Some((handle, info, current)) = find(b"max bpc")
        && let property::ValueType::UnsignedRange(_, max) = info.value_type()
        && let Some(bpc) = hdr_max_bpc(current, max)
    {
        device
            .set_property(connector, handle, bpc)
            .map_err(|error| format!("max bpc: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(blob: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(*blob[at..].first_chunk::<2>().unwrap())
    }

    fn sink() -> Hdr {
        Hdr {
            sdr_white_nits: 203.0,
            primaries: [(708_000, 292_000), (170_000, 797_000), (131_000, 46_000)],
            white_point: (312_700, 329_000),
            max_nits: 1015,
            max_frame_average_nits: 403,
            min_luminance_micro_nits: 500,
        }
    }

    #[test]
    fn the_identity_transform_gives_the_grid_itself() {
        let atlas = lut_atlas(|_| {});
        let side = usize::try_from(GRID).unwrap();
        assert_eq!(atlas.len(), side * side * side * 4);
        let pixel = |red: usize, green: usize, blue: usize| {
            let at = (green * side * side + blue * side + red) * 4;
            [atlas[at], atlas[at + 1], atlas[at + 2], atlas[at + 3]]
        };
        assert_eq!(pixel(0, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(side - 1, side - 1, side - 1), [255, 255, 255, 255]);
        assert_eq!(pixel(16, 0, 32), [127, 0, 255, 255]);
    }

    #[test]
    fn a_missing_or_unreadable_profile_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panel.icc");
        assert!(icc_atlas(&path).is_none(), "missing");
        std::fs::write(&path, b"not an ICC profile").unwrap();
        assert!(icc_atlas(&path).is_none(), "unreadable");
    }

    #[test]
    fn hdr_metadata_blob_matches_the_kernel_struct_layout() {
        let blob = hdr_output_metadata(&sink());
        assert_eq!(blob.len(), 32, "sizeof(struct hdr_output_metadata)");
        assert_eq!(&blob[0..4], &[0, 0, 0, 0], "HDMI_STATIC_METADATA_TYPE1");
        assert_eq!(blob[4], 2, "EOTF SMPTE ST 2084");
        assert_eq!(blob[5], 0, "static metadata descriptor type 1");
        assert_eq!(u16_at(&blob, 6), 35_400, "red x 0.708");
        assert_eq!(u16_at(&blob, 8), 14_600);
        assert_eq!(u16_at(&blob, 10), 8_500, "green x");
        assert_eq!(u16_at(&blob, 14), 6_550, "blue x");
        assert_eq!(u16_at(&blob, 18), 15_635, "white x 0.3127");
        assert_eq!(u16_at(&blob, 20), 16_450, "white y 0.329");
        assert_eq!(u16_at(&blob, 22), 1015, "mastering max, cd/m²");
        assert_eq!(u16_at(&blob, 24), 500, "mastering min, 0.0001 cd/m²");
        assert_eq!(u16_at(&blob, 26), 1015, "MaxCLL");
        assert_eq!(u16_at(&blob, 28), 403, "MaxFALL");
        assert_eq!(&blob[30..32], &[0, 0], "struct padding");
        let wild = Hdr {
            white_point: (2_000_000, 2_000_000),
            ..sink()
        };
        assert_eq!(
            u16_at(&hdr_output_metadata(&wild), 18),
            50_000,
            "saturates at 1.0"
        );
    }

    #[test]
    fn hdr_takes_ten_bits_and_the_bt2020_colorspace() {
        assert_eq!(hdr_max_bpc(8, 16), Some(10));
        assert_eq!(hdr_max_bpc(12, 16), Some(12));
        assert_eq!(hdr_max_bpc(8, 8), None);
        let values = [
            (0, &b"Default"[..]),
            (9, &b"BT2020_RGB"[..]),
            (5, &b"SMPTE_170M_YCC"[..]),
        ];
        assert_eq!(colorspace_values(values), Some((0, 9)));
        assert_eq!(colorspace_values([(0, &b"Default"[..])]), None);
    }
}
