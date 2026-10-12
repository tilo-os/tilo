//! Output orientation on the GPU, and the SDR final pass that also calibrates.
//!
//! A rotated output's surface holds its part of the scene upright; its scanout target is the
//! physical KMS mode. The final pass that writes the target (a copy, the PQ encode or the HDR
//! composite) reads each target pixel from the surface pixel it shows, through
//! `oriented_source_pixel` (`shell/output_orientation.wgsl`, concatenated in front of each
//! final-pass shader). That is the mapping the readback presenter applies on the
//! CPU (`write_subrect_transformed` in `paint.rs`), so a rotated output can render direct
//! with the same pixels and no readback.
//!
//! [`OrientedCopy`] is the SDR final pass for a rotated or ICC-calibrated output: it turns the
//! surface and, given an [`IccLut`], maps each pixel through the output's profile
//! (`shell/output_color_lut.wgsl`) in the same pass. An unrotated, uncalibrated output keeps
//! `wgpu::util::TextureBlitter`.

use settings_store::hardware::OutputTransform;
use vello::wgpu;

use super::fullscreen_pass;
use super::icc_lut::{IccLut, IccLutTextures};

/// The `orientation` value the shaders take for `transform`.
pub fn shader_code(transform: OutputTransform) -> u32 {
    match transform {
        OutputTransform::Normal => 0,
        OutputTransform::Rotate90 => 1,
        OutputTransform::Rotate180 => 2,
        OutputTransform::Rotate270 => 3,
    }
}

/// `size` turned by `transform`: the scanout extent of a surface of `size`, or the surface
/// extent of a scanout of `size` (a quarter turn swaps the axes either way).
pub fn turned(size: (u32, u32), transform: OutputTransform) -> (u32, u32) {
    match transform {
        OutputTransform::Rotate90 | OutputTransform::Rotate270 => (size.1, size.0),
        OutputTransform::Normal | OutputTransform::Rotate180 => size,
    }
}

const COPY_SHADER_SOURCE: &str = concat!(
    include_str!("../output_orientation.wgsl"),
    include_str!("../output_color_lut.wgsl"),
    r"
struct CopyParams {
    orientation: u32,
    // 1 when `color_lut` holds the output's ICC calibration.
    calibrated: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: CopyParams;
@group(0) @binding(2) var color_lut: texture_3d<f32>;
@group(0) @binding(3) var color_lut_coordinates: texture_1d<f32>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let surface = vec2<i32>(textureDimensions(source));
    let px = oriented_source_pixel(vec2<i32>(in.position.xy), surface, params.orientation);
    let texel = textureLoad(source, px, 0);
    if (params.calibrated == 0u) {
        return texel;
    }
    return vec4<f32>(icc_lut_apply(color_lut, color_lut_coordinates, texel.rgb), texel.a);
}
"
);

/// Uploaded calibrations an [`OrientedCopy`] keeps. More calibrated outputs than this upload
/// their table again when they come round.
const MAX_LUTS: usize = 8;

/// Bytes of the copy shader's `CopyParams` uniform.
const PARAMS_BYTES: u64 = 16;

/// The SDR final pass of a rotated or calibrated output: every pixel of a `Bgra8Unorm` target
/// is the surface pixel it shows, unfiltered, through the output's ICC calibration if it has
/// one.
pub struct OrientedCopy {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    /// Bound in place of a calibration when the output has none; never read.
    no_lut: IccLutTextures,
    /// Calibrations on the GPU, by [`IccLut::id`], oldest first.
    luts: Vec<(u64, IccLutTextures)>,
}

impl OrientedCopy {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tilo-oriented-copy"),
            source: wgpu::ShaderSource::Wgsl(COPY_SHADER_SOURCE.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-oriented-copy"),
            entries: &[
                fullscreen_pass::texture_entry(0, wgpu::TextureViewDimension::D2, false),
                fullscreen_pass::uniform_entry(1, PARAMS_BYTES),
                fullscreen_pass::texture_entry(2, wgpu::TextureViewDimension::D3, false),
                fullscreen_pass::texture_entry(3, wgpu::TextureViewDimension::D1, false),
            ],
        });
        let pipeline = fullscreen_pass::pipeline(
            device,
            "tilo-oriented-copy",
            &module,
            &layout,
            wgpu::TextureFormat::Bgra8Unorm,
        );
        let params =
            fullscreen_pass::uniform_buffer(device, "tilo-oriented-copy-params", PARAMS_BYTES);
        Self {
            pipeline,
            layout,
            params,
            no_lut: IccLutTextures::new(device, 1),
            luts: Vec::new(),
        }
    }

    /// The orientation and calibration the next copy writes, with `lut` uploaded if it is
    /// new. Takes effect with the next submission on `queue`.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        transform: OutputTransform,
        lut: Option<&IccLut>,
    ) {
        if let Some(lut) = lut
            && !self.luts.iter().any(|(id, _)| *id == lut.id())
        {
            if self.luts.len() >= MAX_LUTS {
                self.luts.remove(0);
            }
            self.luts.push((lut.id(), lut.upload(device, queue)));
        }
        let mut bytes = [0_u8; 16];
        if let Some(slot) = bytes.get_mut(..4) {
            slot.copy_from_slice(&shader_code(transform).to_le_bytes());
        }
        if let Some(slot) = bytes.get_mut(4..8) {
            slot.copy_from_slice(&u32::from(lut.is_some()).to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &bytes);
    }

    /// Write all of `target` from `source` (the output's surface), turned and, with `lut`,
    /// calibrated, as [`Self::prepare`] set up.
    pub fn copy(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        lut: Option<&IccLut>,
    ) {
        let lut = lut
            .and_then(|lut| self.luts.iter().find(|(id, _)| *id == lut.id()))
            .map_or(&self.no_lut, |(_, textures)| textures);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-oriented-copy"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(lut.lut_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(lut.coordinates_view()),
                },
            ],
        });
        fullscreen_pass::draw(
            encoder,
            "tilo-oriented-copy",
            target,
            wgpu::LoadOp::Load,
            &self.pipeline,
            &bind_group,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turned_extent_is_the_scanout_mode_and_back() {
        for transform in [
            OutputTransform::Normal,
            OutputTransform::Rotate90,
            OutputTransform::Rotate180,
            OutputTransform::Rotate270,
        ] {
            let surface = (1080, 1920);
            let scanout = turned(surface, transform);
            assert_eq!(turned(scanout, transform), surface, "{transform:?}");
            let quarter = matches!(
                transform,
                OutputTransform::Rotate90 | OutputTransform::Rotate270
            );
            assert_eq!(scanout == (1920, 1080), quarter, "{transform:?}");
        }
    }
}
