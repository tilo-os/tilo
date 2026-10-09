//! Output orientation on the GPU.
//!
//! A rotated output's surface holds its part of the scene upright; its scanout target is the
//! physical KMS mode. The final pass that writes the target (a copy, the PQ encode or the HDR
//! composite) reads each target pixel from the surface pixel it shows, through
//! `oriented_source_pixel` (`shell/output_orientation.wgsl`, concatenated in front of each
//! final-pass shader). That is the mapping the readback presenter applies on the
//! CPU (`write_subrect_transformed` in `paint.rs`), so a rotated output can render direct
//! with the same pixels and no readback.
//!
//! [`OrientedCopy`] is the SDR final pass for a rotated output; an unrotated one keeps
//! `wgpu::util::TextureBlitter`.

use settings_store::hardware::OutputTransform;
use vello::wgpu;

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
    r"
struct CopyParams {
    orientation: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: CopyParams;

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
    return textureLoad(source, px, 0);
}
"
);

/// Bytes of the copy shader's `CopyParams` uniform.
const PARAMS_BYTES: u64 = 16;

/// The SDR final pass of a rotated output: every pixel of a `Bgra8Unorm` target is the
/// surface pixel it shows, unfiltered.
pub struct OrientedCopy {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tilo-oriented-copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tilo-oriented-copy"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-oriented-copy-params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            params,
        }
    }

    /// The orientation the next copy writes. Takes effect with the next submission on
    /// `queue`.
    pub fn set_orientation(&self, queue: &wgpu::Queue, transform: OutputTransform) {
        let mut bytes = [0_u8; 16];
        if let Some(slot) = bytes.get_mut(..4) {
            slot.copy_from_slice(&shader_code(transform).to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &bytes);
    }

    /// Write all of `target` from `source` (the output's surface), turned.
    pub fn copy(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
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
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tilo-oriented-copy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The triangle writes every pixel, so nothing needs clearing first.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
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
