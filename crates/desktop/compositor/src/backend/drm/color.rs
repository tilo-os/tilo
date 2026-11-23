//! An output's colour: ICC calibration and HDR signalling.
//! An output with an ICC profile, or driven in HDR, gets a final GLES pass: the scene renders into
//! an offscreen texture, and one element draws that texture onto the output through a shader that
//! applies the profile (a 3D lookup table sampled from the qcms transform sRGB to the panel) and,
//! for HDR, composites the per-surface layers the scene recorded at their own precision and encodes
//! the result as a PQ BT.2020 signal with SDR white at the person's chosen brightness
//! ([`hdr_layers`]). HDR also tells the display: the connector's `HDR_OUTPUT_METADATA` ,
//! `Colorspace` and `max bpc` , and the output's swapchain moves to 10 bits a channel where the GPU
//! draws into such a buffer and the primary plane shows it ([`depth_formats`]), so the PQ signal
//! does not band. Outputs without either keep the plain path, planes and direct scanout included.

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
use super::hdr_layers::{self, Light, Lights};
use crate::color_filter::ColorFilter;
use crate::config::Hdr;
use crate::render::OutputRenderElement;
use settings_store::hardware::HdrUnavailable;
use tilo_color_pipeline::hdr_surface::HdrLayer;

/// Grid points a side of the calibration lookup table: the fewest at which profiles that keep sRGB
/// inside the panel's gamut stay within one code value of qcms; at 33 they reach 2.
pub(crate) const GRID_POINTS: u32 = 65;

/// The grid is even in `linear^(1 / GRID_GAMMA)`, which puts more of its points near black, where
/// a panel's curves bend hardest.
const GRID_GAMMA: f32 = 2.2;

/// Blue slices a row of the atlas holds.
const SLICES_ACROSS: u32 = 8;

/// The calibration lookup table, as the final pass reads it from one RGBA8 texture.
///
/// qcms takes 8-bit input only, so the grid points are 8-bit codes, spaced evenly in a gamma-2.2
/// domain rather than in code values. Each blue grid point is a slice of `points²` texels, red
/// across and green down; the slices go [`SLICES_ACROSS`] to a row. The last row maps each input
/// code 0..=255 to its position on the grid: the cell in red, and how far across that cell it lies
/// in green (high byte) and blue (low byte), in 65535ths. The shader reads every texel at its
/// centre without filtering and interpolates tetrahedrally itself, so the result does not depend
/// on the GPU's filter precision.
pub(crate) struct LutAtlas {
    /// Grid points a side.
    pub points: u32,
    pub width: u32,
    pub height: u32,
    /// RGBA8 rows of `width` texels.
    pub pixels: Vec<u8>,
}

/// The calibration lookup table for `transform` (RGBA8 in, RGBA8 out): the grid's colours run
/// through it ([`LutAtlas`]).
pub(crate) fn lut_atlas(transform: impl Fn(&mut [u8])) -> LutAtlas {
    let codes = grid_codes();
    let points = u32::try_from(codes.len()).unwrap_or(1);
    let mut samples = Vec::with_capacity(codes.len().saturating_pow(3).saturating_mul(4));
    for &blue in &codes {
        for &green in &codes {
            for &red in &codes {
                samples.extend_from_slice(&[red, green, blue, u8::MAX]);
            }
        }
    }
    transform(&mut samples);
    let width = SLICES_ACROSS.saturating_mul(points);
    let height = points
        .div_ceil(SLICES_ACROSS)
        .saturating_mul(points)
        .saturating_add(1);
    let texel_at = |x: u32, y: u32| -> usize {
        usize::try_from(y.saturating_mul(width).saturating_add(x).saturating_mul(4))
            .unwrap_or(usize::MAX)
    };
    let mut pixels = vec![0_u8; texel_at(0, height)];
    let mut index = 0_u32;
    for texel in samples.chunks_exact(4) {
        let rem = |value: u32, by: u32| value.checked_rem(by).unwrap_or(0);
        let div = |value: u32, by: u32| value.checked_div(by).unwrap_or(0);
        let (red, green, blue) = (
            rem(index, points),
            rem(div(index, points), points),
            div(index, points.saturating_mul(points)),
        );
        let x = rem(blue, SLICES_ACROSS)
            .saturating_mul(points)
            .saturating_add(red);
        let y = div(blue, SLICES_ACROSS)
            .saturating_mul(points)
            .saturating_add(green);
        let at = texel_at(x, y);
        if let Some(slot) = pixels.get_mut(at..at.saturating_add(4)) {
            slot.copy_from_slice(texel);
        }
        index = index.saturating_add(1);
    }
    let last_cell = f32::from(u16::try_from(points.saturating_sub(2)).unwrap_or(0));
    let row = texel_at(0, height.saturating_sub(1));
    for (code, position) in grid_coordinates(&codes).into_iter().enumerate() {
        let cell = position.floor().clamp(0.0, last_cell);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both are clamped into the u8 and u16 ranges first"
        )]
        let (cell_code, fraction) = (
            cell as u8,
            ((position - cell).clamp(0.0, 1.0) * 65535.0).round() as u16,
        );
        let [high, low] = fraction.to_be_bytes();
        let at = row.saturating_add(code.saturating_mul(4));
        if let Some(slot) = pixels.get_mut(at..at.saturating_add(4)) {
            slot.copy_from_slice(&[cell_code, high, low, u8::MAX]);
        }
    }
    LutAtlas {
        points,
        width,
        height,
        pixels,
    }
}

/// `code` in the domain the grid is even in.
fn grid_domain(code: u8) -> f32 {
    let encoded = f32::from(code) / 255.0;
    let linear = if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    };
    linear.powf(GRID_GAMMA.recip())
}

/// The 8-bit codes nearest to [`GRID_POINTS`] even steps of [`grid_domain`], from 0 to 255.
fn grid_codes() -> Vec<u8> {
    let last = f32::from(u16::try_from(GRID_POINTS.saturating_sub(1)).unwrap_or(u16::MAX));
    let mut codes: Vec<u8> = (0..GRID_POINTS)
        .map(|point| {
            let target = f32::from(u16::try_from(point).unwrap_or(u16::MAX)) / last;
            (0..=u8::MAX)
                .min_by(|a, b| {
                    (grid_domain(*a) - target)
                        .abs()
                        .total_cmp(&(grid_domain(*b) - target).abs())
                })
                .unwrap_or(0)
        })
        .collect();
    codes.dedup();
    codes
}

/// The grid position of every 8-bit code: the index of the cell it falls in plus how far across
/// that cell it lies, in [`grid_domain`].
fn grid_coordinates(codes: &[u8]) -> Vec<f32> {
    let mut coordinates = vec![0.0_f32; 256];
    for (cell, pair) in (0_u8..=u8::MAX).zip(codes.windows(2)) {
        let &[low, high] = pair else {
            continue;
        };
        let (start, end) = (grid_domain(low), grid_domain(high));
        for code in low..=high {
            if let Some(slot) = coordinates.get_mut(usize::from(code)) {
                *slot = f32::from(cell) + (grid_domain(code) - start) / (end - start);
            }
        }
    }
    coordinates
}

/// The lookup table of the ICC profile at `path` (perceptual, from sRGB);
/// `None` when qcms cannot read it.
pub(crate) fn icc_atlas(path: &Path) -> Option<LutAtlas> {
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

/// Length of `struct hdr_output_metadata` in `drm_mode.h` : a `u32` type and the 26-byte
/// `hdr_metadata_infoframe` , padded to the struct's 4-byte alignment. The kernel refuses a blob of
/// any other size.
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
    // `metadata_type` : HDMI_STATIC_METADATA_TYPE1.
    fields.extend_from_slice(&0_u32.to_le_bytes());
    fields.push(EOTF_SMPTE_ST2084);
    // Infoframe `metadata_type` : static metadata type 1.
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

/// Whether HDR can be signalled on `connector` at all: the driver offers the metadata property, a
/// BT.2020 colourspace and ten bits a channel. What [`signal_hdr`] needs, asked without switching
/// anything.
pub(crate) fn hdr_supported(
    device: &DrmDevice,
    connector: connector::Handle,
) -> Result<(), HdrUnavailable> {
    let properties = device
        .get_properties(connector)
        .map_err(|_| HdrUnavailable::DriverUnsupported)?;
    let find = |name: &[u8]| -> Option<(property::Handle, property::Info, u64)> {
        properties.iter().find_map(|(handle, value)| {
            let info = device.get_property(*handle).ok()?;
            (info.name().to_bytes() == name).then_some((*handle, info, *value))
        })
    };
    if find(b"HDR_OUTPUT_METADATA").is_none() {
        return Err(HdrUnavailable::DriverUnsupported);
    }
    let bt2020 = find(b"Colorspace").is_some_and(|(_, info, _)| {
        let property::ValueType::Enum(values) = info.value_type() else {
            return false;
        };
        let (_, entries) = values.values();
        colorspace_values(
            entries
                .iter()
                .map(|entry| (entry.value(), entry.name().to_bytes())),
        )
        .is_some()
    });
    if !bt2020 {
        return Err(HdrUnavailable::DriverUnsupported);
    }
    let deep_enough = find(b"max bpc").is_none_or(|(_, info, current)| {
        let property::ValueType::UnsignedRange(_, max) = info.value_type() else {
            return true;
        };
        hdr_max_bpc(current, max).is_some()
    });
    if deep_enough {
        Ok(())
    } else {
        Err(HdrUnavailable::DriverUnsupported)
    }
}

/// The ICC profiles a person may choose, path → the name they read: every profile in the system's
/// and the person's ICC directories that qcms can build a transform from, so the list never offers
/// a profile that cannot be applied. Read once; the directories do not change under a session.
pub(crate) fn color_profiles() -> std::collections::BTreeMap<String, String> {
    static PROFILES: std::sync::OnceLock<std::collections::BTreeMap<String, String>> =
        std::sync::OnceLock::new();
    PROFILES
        .get_or_init(|| {
            let mut roots = vec![
                PathBuf::from("/usr/share/color/icc"),
                PathBuf::from("/usr/local/share/color/icc"),
                PathBuf::from("/var/lib/colord/icc"),
            ];
            if let Some(data) = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
                })
            {
                roots.push(data.join("icc"));
                roots.push(data.join("color/icc"));
            }
            let mut profiles = std::collections::BTreeMap::new();
            'roots: for root in roots {
                let Ok(root) = root.canonicalize() else {
                    continue;
                };
                let Ok(entries) = std::fs::read_dir(&root) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if profiles.len() >= MAX_PROFILES {
                        break 'roots;
                    }
                    let path = entry.path();
                    let extension = path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .unwrap_or_default();
                    if !extension.eq_ignore_ascii_case("icc")
                        && !extension.eq_ignore_ascii_case("icm")
                    {
                        continue;
                    }
                    let Ok(path) = path.canonicalize() else {
                        continue;
                    };
                    if !path.starts_with(&root)
                        || !path.is_file()
                        || path
                            .metadata()
                            .is_ok_and(|metadata| metadata.len() > MAX_PROFILE_BYTES)
                    {
                        continue;
                    }
                    if icc_atlas(&path).is_none() {
                        continue;
                    }
                    let Some(key) = path.to_str() else {
                        continue;
                    };
                    let label = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or(key)
                        .to_owned();
                    profiles.insert(key.to_owned(), label);
                }
            }
            profiles
        })
        .clone()
}

/// The most profiles offered, so a machine with a profile library cannot make the list endless.
const MAX_PROFILES: usize = 128;
/// The largest file read as an ICC profile.
const MAX_PROFILE_BYTES: u64 = 16 * 1024 * 1024;

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

/// The link depth HDR needs: ten bits a channel, or more when the connector already runs deeper.
pub(crate) fn hdr_max_bpc(current: u64, range_max: u64) -> Option<u64> {
    (range_max >= 10).then(|| current.max(10).min(range_max))
}

/// The 8-bit formats an output's swapchain uses, best first.
pub(crate) const SDR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

/// The 10-bit formats an HDR output's swapchain may use, best first: opaque ones (the commonest on
/// primary planes) before those with alpha.
pub(crate) const HDR_FORMATS: [Fourcc; 4] = [
    Fourcc::Xrgb2101010,
    Fourcc::Xbgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr2101010,
];

/// The swapchain formats to try, best first, for an output whose swapchain
/// is in `current` and that does (`hdr`) or does not signal HDR; empty when
/// `current` already fits. `renderable` says whether the GPU draws into a
/// format. Whether the primary plane shows it is found by trying; when no
/// 10-bit format works the swapchain stays at 8 bits.
pub(crate) fn depth_formats(
    hdr: bool,
    current: Fourcc,
    renderable: impl Fn(Fourcc) -> bool,
) -> Vec<Fourcc> {
    let wanted: &[Fourcc] = if hdr { &HDR_FORMATS } else { &SDR_FORMATS };
    if wanted.contains(&current) {
        return Vec::new();
    }
    wanted
        .iter()
        .copied()
        .filter(|&format| renderable(format))
        .collect()
}

/// The final pass's fragment shader, in smithay's texture-shader form.
const SHADER: &str = include_str!("color.glsl");

/// What an output's colour is asked to be.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Wanted {
    pub profile: Option<PathBuf>,
    pub hdr: Option<Hdr>,
    /// The whole-desktop colour filter.
    pub filter: ColorFilter,
}

impl Wanted {
    /// Whether the output shows the scene as it is, needing no pass.
    pub(crate) fn plain(&self) -> bool {
        self.profile.is_none() && self.hdr.is_none() && self.filter.is_off()
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
    /// The scene drawn again with each described surface in its weight channel colour: how much of
    /// each layer reaches a pixel.
    lit: Option<GlesTexture>,
    lit_damage: OutputDamageTracker,
    /// The HDR layers the scene drew this frame and the planes they sample.
    layers: Vec<Light>,
    planes: Lights,
    /// The uniforms that stay the same for every frame.
    uniforms: Vec<Uniform<'static>>,
    id: Id,
    commit: CommitCounter,
}

/// The final pass's shader, compiled on `gles`.
fn compile_program(gles: &mut GlesRenderer) -> Result<GlesTexProgram, String> {
    let mut names = vec![
        UniformName::new("lut", UniformType::_1i),
        UniformName::new("use_lut", UniformType::_1f),
        UniformName::new("lut_points", UniformType::_1f),
        UniformName::new("lut_extent", UniformType::_2f),
        UniformName::new("pq", UniformType::_1f),
        UniformName::new("sdr_white", UniformType::_1f),
        UniformName::new("filter_row_r", UniformType::_3f),
        UniformName::new("filter_row_g", UniformType::_3f),
        UniformName::new("filter_row_b", UniformType::_3f),
        UniformName::new("filter_offset", UniformType::_3f),
        UniformName::new("use_filter", UniformType::_1f),
        UniformName::new("scene_size", UniformType::_2f),
        UniformName::new("layer_count", UniformType::_1f),
    ];
    names.extend(hdr_layers::uniform_names());
    gles.compile_custom_texture_shader(SHADER, &names)
        .map_err(|error| format!("colour shader: {error:?}"))
}

/// `atlas` uploaded as the texture the shader reads its calibration from.
fn lut_texture(gles: &mut GlesRenderer, atlas: &LutAtlas) -> Result<GlesTexture, String> {
    let size = (
        i32::try_from(atlas.width).map_err(|error| error.to_string())?,
        i32::try_from(atlas.height).map_err(|error| error.to_string())?,
    );
    gles.import_memory(&atlas.pixels, Fourcc::Abgr8888, size.into(), false)
        .map_err(|error| format!("lookup table: {error:?}"))
}

/// The uniforms that stay the same for every frame of a pass for `wanted` on an output of `size`,
/// calibrated through `atlas` when there is one.
#[allow(
    clippy::cast_precision_loss,
    reason = "an output's size and an atlas's are below 2^24 pixels, exactly representable as f32"
)]
fn pass_uniforms(
    wanted: &Wanted,
    atlas: Option<&LutAtlas>,
    size: Size<i32, Physical>,
) -> Vec<Uniform<'static>> {
    // Unit 0 draws the scene; the calibration table, the weight pass's scene and the three layer
    // planes take the units after it.
    let flag = |on: bool| if on { 1.0_f32 } else { 0.0 };
    let matrix = wanted.filter.matrix();
    let mut uniforms = vec![
        Uniform::new("lut", 1_i32),
        Uniform::new("use_lut", flag(atlas.is_some())),
        Uniform::new("lut_points", atlas.map_or(2.0, |atlas| atlas.points as f32)),
        Uniform::new(
            "lut_extent",
            atlas.map_or([1.0, 1.0], |atlas| {
                [atlas.width as f32, atlas.height as f32]
            }),
        ),
        Uniform::new("pq", flag(wanted.hdr.is_some())),
        Uniform::new(
            "sdr_white",
            wanted.hdr.as_ref().map_or(0.0, |hdr| hdr.sdr_white_nits),
        ),
        Uniform::new("filter_row_r", matrix.rows[0]),
        Uniform::new("filter_row_g", matrix.rows[1]),
        Uniform::new("filter_row_b", matrix.rows[2]),
        Uniform::new("filter_offset", matrix.offset),
        Uniform::new("use_filter", flag(!wanted.filter.is_off())),
        Uniform::new("scene_size", [size.w as f32, size.h as f32]),
        Uniform::new("lit_tex", 2_i32),
    ];
    for slot in 0..hdr_layers::LAYERS {
        uniforms.push(Uniform::new(
            format!("plane{slot}"),
            hdr_layers::PLANE_UNIT.saturating_add(i32::try_from(slot).unwrap_or(0)),
        ));
    }
    uniforms
}

impl ColorPass {
    pub(crate) fn wanted(&self) -> &Wanted {
        &self.wanted
    }

    /// The pass for `output` as `wanted` , on the renderer drawing it.
    #[allow(
        clippy::cast_precision_loss,
        reason = "an output's size in pixels is below 2^24, exactly representable as f32"
    )]
    pub(crate) fn new(
        renderer: &mut Renderer<'_>,
        output: &Output,
        wanted: Wanted,
    ) -> Result<Self, String> {
        let mode = output.current_mode().ok_or("no mode")?;
        let size = output.current_transform().transform_size(mode.size);
        let gles: &mut GlesRenderer = renderer.as_mut();
        let program = compile_program(gles)?;
        let atlas = match &wanted.profile {
            Some(path) => Some(
                icc_atlas(path)
                    .ok_or_else(|| format!("{}: not a readable ICC profile", path.display()))?,
            ),
            None => None,
        };
        let lut = atlas
            .as_ref()
            .map(|atlas| lut_texture(gles, atlas))
            .transpose()?;
        let uniforms = pass_uniforms(&wanted, atlas.as_ref(), size);
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
            lit: None,
            lit_damage: OutputDamageTracker::new(size, scale, Transform::Normal),
            layers: Vec::new(),
            planes: Lights::default(),
            uniforms,
            id: Id::new(),
            commit: CommitCounter::default(),
        })
    }

    /// Render the scene offscreen; the pass's element then draws it. `hdr` are the layers the scene
    /// recorded, and `weights` the same scene drawn with each of them in its weight channel colour:
    /// an HDR output composites their own light on top of what they leave.
    pub(crate) fn render<'r>(
        &mut self,
        renderer: &mut Renderer<'r>,
        scene: &[OutputRenderElement<Renderer<'r>>],
        weights: &[OutputRenderElement<Renderer<'r>>],
        hdr: &[HdrLayer],
    ) -> Result<RenderElementStates, String> {
        self.layers.clear();
        if !weights.is_empty() && !hdr.is_empty() {
            match self.planes.prepare(renderer, hdr) {
                Ok(lights) => self.layers = lights,
                Err(error) => {
                    tracing::warn!(%error, "HDR layers shown as their tone-mapped renditions");
                }
            }
        }
        let states = {
            let mut framebuffer = renderer
                .bind(&mut self.texture)
                .map_err(|error| format!("bind offscreen scene: {error:?}"))?;
            let result = self
                .damage
                .render_output(
                    renderer,
                    &mut framebuffer,
                    self.age,
                    scene,
                    crate::render::BACKGROUND,
                )
                .map_err(|error| format!("offscreen scene: {error:?}"))?;
            drop(framebuffer);
            self.age = 1;
            if result.damage.is_some_and(|damage| !damage.is_empty()) {
                self.commit.increment();
            }
            result.states
        };
        if !self.layers.is_empty() {
            self.render_weights(renderer, weights)?;
        }
        Ok(states)
    }

    /// The scene drawn again with each described surface in its layer's weight channel colour, into
    /// the texture the shader reads the weights from.
    fn render_weights<'r>(
        &mut self,
        renderer: &mut Renderer<'r>,
        weights: &[OutputRenderElement<Renderer<'r>>],
    ) -> Result<(), String> {
        if self.lit.is_none() {
            let buffer_size = self.size.to_logical(1).to_buffer(1, Transform::Normal);
            let texture = renderer
                .create_buffer(Fourcc::Abgr8888, buffer_size)
                .map_err(|error| format!("offscreen layer weights: {error:?}"))?;
            self.lit = Some(texture);
        }
        // Every weight can have changed wherever a layer is, so the pass is drawn whole.
        let Some(texture) = self.lit.as_mut() else {
            return Ok(());
        };
        let mut framebuffer = renderer
            .bind(texture)
            .map_err(|error| format!("bind layer weights: {error:?}"))?;
        let result = self
            .lit_damage
            .render_output(
                renderer,
                &mut framebuffer,
                0,
                weights,
                crate::render::BACKGROUND,
            )
            .map_err(|error| format!("offscreen layer weights: {error:?}"))?;
        drop(framebuffer);
        if result.damage.is_some_and(|damage| !damage.is_empty()) {
            self.commit.increment();
        }
        Ok(())
    }

    fn draw_gles(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        // The units the shader reads: the weights texture and the planes,
        // after the calibration table's.
        let mut samplers: Vec<(u32, u32)> = Vec::with_capacity(self.layers.len().saturating_add(2));
        if let Some(lut) = self.lut.as_ref().map(GlesTexture::tex_id) {
            samplers.push((1, lut));
        }
        if !self.layers.is_empty()
            && let Some(lit) = self.lit.as_ref().map(GlesTexture::tex_id)
        {
            samplers.push((2, lit));
            for (slot, light) in self.layers.iter().enumerate() {
                let unit = u32::try_from(slot)
                    .unwrap_or(0)
                    .saturating_add(u32::try_from(hdr_layers::PLANE_UNIT).unwrap_or(0));
                samplers.push((unit, light.texture.tex_id()));
            }
        }
        if !samplers.is_empty() {
            frame.with_context(|gl| {
                for (unit, texture) in &samplers {
                    // The calibration table is read texel by texel, at each centre, and
                    // interpolated in the shader: filtering would blend neighbouring
                    // slices and coordinates.
                    let filter = if *unit == 1 {
                        ffi::NEAREST
                    } else {
                        ffi::LINEAR
                    };
                    // SAFETY: the renderer's context is current inside a frame;
                    // every unit is released again after the draw.
                    unsafe {
                        gl.ActiveTexture(ffi::TEXTURE0.saturating_add(*unit));
                        gl.BindTexture(ffi::TEXTURE_2D, *texture);
                        for (parameter, value) in [
                            (ffi::TEXTURE_MIN_FILTER, filter),
                            (ffi::TEXTURE_MAG_FILTER, filter),
                            (ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE),
                            (ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE),
                        ] {
                            gl.TexParameteri(
                                ffi::TEXTURE_2D,
                                parameter,
                                i32::try_from(value).unwrap_or(0),
                            );
                        }
                    }
                }
                // SAFETY: as above.
                unsafe { gl.ActiveTexture(ffi::TEXTURE0) };
            })?;
        }
        let mut uniforms = self.uniforms.clone();
        uniforms.push(Uniform::new(
            "layer_count",
            u8::try_from(self.layers.len()).map_or(0.0, f32::from),
        ));
        for light in &self.layers {
            uniforms.extend(light.uniforms.iter().cloned());
        }
        let drawn = frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &uniforms,
        );
        if !samplers.is_empty() {
            frame.with_context(|gl| {
                for (unit, _) in &samplers {
                    // SAFETY: as above.
                    unsafe {
                        gl.ActiveTexture(ffi::TEXTURE0.saturating_add(*unit));
                        gl.BindTexture(ffi::TEXTURE_2D, 0);
                    }
                }
                // SAFETY: as above.
                unsafe { gl.ActiveTexture(ffi::TEXTURE0) };
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

/// Switch HDR signalling on `connector` on (with `hdr` ) or off. Properties the driver lacks are
/// skipped; a display that cannot take HDR shows the PQ picture washed out, which the session
/// avoids by asking only for displays whose EDID offers PQ.
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
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "test fixtures over small grids and colour lists: an overflow is the failing test"
)]
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

    tilo_test_env::scenario! {
        /// The colour transform of an output: its grid, its filter and its profile.
        fn an_outputs_colour_pass_is_built_only_from_what_can_be_read;

        phase the_identity_transform_gives_the_grid_itself;
        {
            let atlas = lut_atlas(|_| {});
            let points = atlas.points as usize;
            assert_eq!(atlas.points, GRID_POINTS, "65 distinct codes a side");
            let width = atlas.width as usize;
            let texel = |x: usize, y: usize| {
                let at = (y * width + x) * 4;
                [
                    atlas.pixels[at],
                    atlas.pixels[at + 1],
                    atlas.pixels[at + 2],
                    atlas.pixels[at + 3],
                ]
            };
            // Blue slices eight to a row, red across and green down each.
            let point = |red: usize, green: usize, blue: usize| {
                texel((blue % 8) * points + red, (blue / 8) * points + green)
            };
            assert_eq!(point(0, 0, 0), [0, 0, 0, 255]);
            let last = points - 1;
            assert_eq!(point(last, last, last), [255, 255, 255, 255]);
            let codes = grid_codes();
            assert_eq!(point(16, 0, 32), [codes[16], 0, codes[32], 255]);
            assert!(
                codes[1] - codes[0] < codes[last] - codes[last - 1],
                "the grid is denser near black than near white: {codes:?}"
            );
            // The last row: each code's cell, and how far across it.
            let row = atlas.height as usize - 1;
            assert_eq!(texel(0, row), [0, 0, 0, 255], "black: the first corner");
            assert_eq!(
                texel(255, row),
                [u8::try_from(points - 2).unwrap(), 255, 255, 255],
                "white: the far side of the last cell"
            );
            assert_eq!(
                texel(usize::from(codes[16]), row),
                [16, 0, 0, 255],
                "a grid code sits on its point"
            );
        }

        phase a_colour_filter_asks_for_the_final_pass;
        {
            assert!(
                Wanted::default().plain(),
                "an untouched output needs no pass"
            );
            for filter in [
                ColorFilter::Grayscale,
                ColorFilter::Inverted,
                ColorFilter::GrayscaleInverted,
            ] {
                let wanted = Wanted {
                    filter,
                    ..Wanted::default()
                };
                assert!(!wanted.plain(), "{filter:?} is drawn by the pass");
            }
        }

        phase a_missing_or_unreadable_profile_is_refused;
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("panel.icc");
            assert!(icc_atlas(&path).is_none(), "missing");
            std::fs::write(&path, b"not an ICC profile").unwrap();
            assert!(icc_atlas(&path).is_none(), "unreadable");
        }
    }

    tilo_test_env::scenario! {
        /// What turning HDR on asks of the display.
        fn hdr_takes_ten_bits_bt2020_and_the_kernels_metadata_layout;

        phase hdr_metadata_blob_matches_the_kernel_struct_layout;
        {
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

        phase hdr_moves_the_swapchain_to_ten_bits_and_back;
        {
            let all = |_| true;
            assert_eq!(
                depth_formats(true, Fourcc::Argb8888, all),
                HDR_FORMATS,
                "opaque 10-bit formats first"
            );
            assert_eq!(
                depth_formats(true, Fourcc::Argb8888, |format| format
                    == Fourcc::Abgr2101010
                    || format == Fourcc::Argb8888),
                [Fourcc::Abgr2101010],
                "only formats the GPU draws into"
            );
            assert!(
                depth_formats(true, Fourcc::Argb8888, |format| !HDR_FORMATS
                    .contains(&format))
                .is_empty(),
                "no 10-bit format to draw into: stays at 8 bits"
            );
            assert!(depth_formats(true, Fourcc::Xbgr2101010, all).is_empty());
            assert_eq!(depth_formats(false, Fourcc::Xrgb2101010, all), SDR_FORMATS);
            assert!(depth_formats(false, Fourcc::Xrgb8888, all).is_empty());
        }

        phase hdr_takes_ten_bits_and_the_bt2020_colorspace;
        {
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

    /// The pass drawn on a plain GLES renderer, as the headless backend makes one: the same
    /// shader and draw as on a display, which the test reads back.
    impl RenderElement<GlesRenderer> for ColorPass {
        fn draw(
            &self,
            frame: &mut GlesFrame<'_, '_>,
            src: Rectangle<f64, Buffer>,
            dst: Rectangle<i32, Physical>,
            damage: &[Rectangle<i32, Physical>],
            opaque_regions: &[Rectangle<i32, Physical>],
        ) -> Result<(), GlesError> {
            self.draw_gles(frame, src, dst, damage, opaque_regions)
        }
    }

    /// `colours` (sRGB) through the production final pass calibrated by `atlas`, on the GPU, as
    /// the panel's 8-bit signals.
    fn calibrate_on_gpu(
        gles: &mut GlesRenderer,
        atlas: &LutAtlas,
        colours: &[[u8; 3]],
    ) -> Vec<[u8; 3]> {
        // One row: no question of which way up the readback is.
        let width = i32::try_from(colours.len()).unwrap();
        let size = Size::<i32, Physical>::from((width, 1));
        let pixels: Vec<u8> = colours
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect();
        let scene = gles
            .import_memory(&pixels, Fourcc::Abgr8888, (width, 1).into(), false)
            .unwrap();
        let wanted = Wanted {
            profile: Some(PathBuf::from("panel.icc")),
            ..Wanted::default()
        };
        let pass = ColorPass {
            uniforms: pass_uniforms(&wanted, Some(atlas), size),
            wanted,
            texture: scene,
            size,
            damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            age: 0,
            program: compile_program(gles).unwrap(),
            lut: Some(lut_texture(gles, atlas).unwrap()),
            lit: None,
            lit_damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            layers: Vec::new(),
            planes: Lights::default(),
            id: Id::new(),
            commit: CommitCounter::default(),
        };
        let drawn = crate::capture::render_to_memory::<GlesRenderer, GlesTexture, _>(
            gles,
            size,
            1.0,
            Transform::Normal,
            &[pass],
            [0.0; 4],
        )
        .unwrap();
        drawn
            .chunks_exact(4)
            .take(colours.len())
            .map(|rgba| [rgba[0], rgba[1], rgba[2]])
            .collect()
    }

    /// The calibration the old GPU pass made, on the CPU: qcms's samples on a 65-point grid even
    /// in gamma 2.2 (red fastest, then green, then blue), each code placed on it by its
    /// coordinate, interpolated over the tetrahedron that holds it. Profiles that keep sRGB inside
    /// the panel's gamut land within one code value of qcms through it.
    fn tetrahedral_reference(transform: &qcms::Transform, colours: &[[u8; 3]]) -> Vec<[u8; 3]> {
        let codes = grid_codes();
        let coordinates = grid_coordinates(&codes);
        let points = codes.len();
        let mut samples = Vec::new();
        for &blue in &codes {
            for &green in &codes {
                for &red in &codes {
                    samples.extend_from_slice(&[red, green, blue, 255]);
                }
            }
        }
        transform.apply(&mut samples);
        let at = |[r, g, b]: [usize; 3]| -> [f32; 3] {
            let index = ((b * points + g) * points + r) * 4;
            [0, 1, 2].map(|channel| f32::from(samples[index + channel]) / 255.0)
        };
        colours
            .iter()
            .map(|colour| {
                let position = colour.map(|code| coordinates[usize::from(code)]);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "grid positions are 0..=64"
                )]
                let cell = position.map(|p| (p.floor() as usize).min(points - 2));
                let f = [0, 1, 2].map(|axis| position[axis] - cell[axis] as f32);
                // Corners from the origin to the far corner, largest fraction first.
                let mut order = [0_usize, 1, 2];
                order.sort_by(|a, b| f[*b].total_cmp(&f[*a]));
                let mut corner = cell;
                let mut colour = at(corner).map(|value| value * (1.0 - f[order[0]]));
                for (step, axis) in order.iter().enumerate() {
                    corner[*axis] += 1;
                    let weight = f[*axis] - order.get(step + 1).map_or(0.0, |next| f[*next]);
                    let value = at(corner);
                    for channel in 0..3 {
                        colour[channel] += weight * value[channel];
                    }
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to 0..=255 first"
                )]
                colour.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
            })
            .collect()
    }

    /// Every colour on a lattice of step 15, every grey, and the primaries and secondaries.
    fn known_colours() -> Vec<[u8; 3]> {
        let lattice: Vec<u8> = (0..=u8::MAX).step_by(15).collect();
        let mut colours = Vec::new();
        for &b in &lattice {
            for &g in &lattice {
                for &r in &lattice {
                    colours.push([r, g, b]);
                }
            }
        }
        colours.extend((0..=u8::MAX).map(|grey| [grey; 3]));
        colours
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=1 first"
    )]
    fn to_u16(value: f64) -> u16 {
        (value.clamp(0.0, 1.0) * 65535.0).round() as u16
    }

    fn xy((x, y): (f64, f64)) -> qcms::CIE_xyY {
        qcms::CIE_xyY { x, y, Y: 1.0 }
    }

    /// Calibrated displays a desktop meets, sRGB to each as the pass maps it: a wide-gamut
    /// matrix profile with a gamma curve, and one with a measured-looking table curve.
    fn panels() -> Vec<(&'static str, qcms::Transform)> {
        let d65 = (0.3127, 0.3290);
        let adobe = qcms::CIE_xyYTRIPLE {
            red: xy((0.64, 0.33)),
            green: xy((0.21, 0.71)),
            blue: xy((0.15, 0.06)),
        };
        let p3 = qcms::CIE_xyYTRIPLE {
            red: xy((0.680, 0.320)),
            green: xy((0.265, 0.690)),
            blue: xy((0.150, 0.060)),
        };
        // Gamma 2.2 with a ripple, as a calibration tool measures a panel.
        let ripple: Vec<u16> = (0..1024)
            .map(|i| {
                let signal = f64::from(i) / 1023.0;
                let light = (signal.powf(2.2)
                    * (1.0 + 0.04 * (signal * 12.0).sin() * signal * (1.0 - signal)))
                    .clamp(0.0, 1.0);
                to_u16(light)
            })
            .collect();
        let profiles = [
            ("sRGB", qcms::Profile::new_sRGB()),
            (
                "Adobe RGB, gamma 2.2",
                qcms::Profile::new_rgb_with_gamma_set(xy(d65), adobe, 2.2, 2.2, 2.2).unwrap(),
            ),
            (
                "Display P3, calibration table",
                qcms::Profile::new_rgb_with_table(xy(d65), p3, &ripple).unwrap(),
            ),
        ];
        let srgb = qcms::Profile::new_sRGB();
        profiles
            .into_iter()
            .map(|(name, panel)| {
                let transform = qcms::Transform::new(
                    &srgb,
                    &panel,
                    qcms::DataType::RGBA8,
                    qcms::Intent::Perceptual,
                )
                .unwrap();
                (name, transform)
            })
            .collect()
    }

    /// The largest per-channel difference between two lists of colours, and where.
    fn largest_difference(got: &[[u8; 3]], want: &[[u8; 3]]) -> (u8, usize) {
        got.iter()
            .zip(want)
            .enumerate()
            .map(|(index, (got, want))| {
                let delta = (0..3).map(|c| got[c].abs_diff(want[c])).max().unwrap();
                (delta, index)
            })
            .max()
            .unwrap()
    }

    tilo_test_env::scenario! {
        /// A calibrated output's colours on the GPU: the 65-point tetrahedral table the pass
        /// had before, and within one code value of qcms.
        fn a_calibrated_output_matches_qcms_within_one_code_value;

        phase known_colours_land_on_the_tetrahedral_reference_and_on_qcms;
        {
            let display = crate::backend::headless::display().expect("an EGL display");
            let context = smithay::backend::egl::EGLContext::new(&display).expect("an EGL context");
            // SAFETY: the context is current on this thread, the one that draws.
            let mut gles = unsafe { GlesRenderer::new(context) }.expect("a GLES renderer");
            let colours = known_colours();
            for (name, transform) in panels() {
                let atlas = lut_atlas(|pixels| transform.apply(pixels));
                let got = calibrate_on_gpu(&mut gles, &atlas, &colours);
                let reference = tetrahedral_reference(&transform, &colours);
                let (delta, at) = largest_difference(&got, &reference);
                assert!(
                    delta <= 1,
                    "{name}: {:?} is {:?} on the GPU, {:?} by the tetrahedral reference",
                    colours[at],
                    got[at],
                    reference[at]
                );
                let mut qcms_colours: Vec<u8> = colours
                    .iter()
                    .flat_map(|&[r, g, b]| [r, g, b, 255])
                    .collect();
                transform.apply(&mut qcms_colours);
                let want: Vec<[u8; 3]> = qcms_colours
                    .chunks_exact(4)
                    .map(|rgba| [rgba[0], rgba[1], rgba[2]])
                    .collect();
                let (delta, at) = largest_difference(&got, &want);
                assert!(
                    delta <= 1,
                    "{name}: {:?} is {:?} on the GPU, {:?} by qcms",
                    colours[at],
                    got[at],
                    want[at]
                );
            }
        }
    }
}
