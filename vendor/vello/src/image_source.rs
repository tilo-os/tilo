// Copyright 2026 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Images whose pixels are drawn into the atlas from a texture through a
//! render pass (Tilo patch; see `vendor/PATCHES.md`).
//!
//! [`crate::Renderer::override_image`] copies a texture into the atlas with
//! `copy_texture_to_texture`, which needs the atlas format exactly. A
//! [`ImageSource`] may be any sampleable 8-bit colour texture — a Wayland
//! client's `BGRA` buffer, imported as it is — and may be drawn opaque, so the
//! one copy into the atlas is also the format conversion.

use std::collections::HashMap;

use wgpu::util::DeviceExt as _;
use wgpu::{Device, TextureFormat};

/// A texture an image's pixels are drawn from, through a render pass.
#[derive(Clone, Debug)]
pub struct ImageSource {
    /// Any colour format the device samples as float; read with
    /// `textureLoad`, so `Bgra8Unorm` lands in the atlas as RGBA.
    pub texture: wgpu::Texture,
    /// Where the image starts in `texture`, in its pixels.
    pub origin: [u32; 2],
    /// Draw alpha as one, for a format whose alpha channel is padding.
    pub opaque: bool,
}

const SHADER: &str = r"
override opaque: bool = false;
struct Place {
    // Where the image starts in the atlas, and in the source.
    dst: vec2<u32>,
    src: vec2<u32>,
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> place: Place;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(
        source,
        vec2<i32>(at.xy) - vec2<i32>(place.dst) + vec2<i32>(place.src),
        0,
    );
    return select(texel, vec4<f32>(texel.rgb, 1.0), opaque);
}
";

/// The pipelines that draw [`ImageSource`]s into one atlas format.
pub(crate) struct SourceBlit {
    layout: wgpu::BindGroupLayout,
    keep_alpha: wgpu::RenderPipeline,
    opaque: wgpu::RenderPipeline,
}

impl SourceBlit {
    fn new(device: &Device, format: TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vello.image_source"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vello.image_source"),
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
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vello.image_source"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |opaque: bool| {
            let constants = [("opaque", if opaque { 1.0 } else { 0.0 })];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vello.image_source"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &constants,
                        ..Default::default()
                    },
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            layout,
            keep_alpha: pipeline(false),
            opaque: pipeline(true),
        }
    }
}

/// Every [`ImageSource`] a renderer draws, and what drawing them cost.
#[derive(Default)]
pub(crate) struct ImageSources {
    pub(crate) sources: HashMap<u64, ImageSource>,
    blits: HashMap<TextureFormat, SourceBlit>,
    /// Images drawn into the atlas from a source, and their bytes written.
    pub(crate) writes: u64,
    pub(crate) bytes: u64,
}

/// Where an image is drawn in the atlas: its corner and size.
pub(crate) struct AtlasSlot<'a> {
    pub(crate) atlas: &'a wgpu::Texture,
    pub(crate) at: [u32; 2],
    pub(crate) size: [u32; 2],
}

impl ImageSources {
    /// Draw `id`'s source into `slot`, if it has one. Returns whether it did.
    pub(crate) fn write(
        &mut self,
        device: &Device,
        encoder: &mut wgpu::CommandEncoder,
        id: u64,
        slot: &AtlasSlot<'_>,
    ) -> bool {
        let Some(source) = self.sources.get(&id) else {
            return false;
        };
        let [x, y] = slot.at;
        let [width, height] = slot.size;
        if width == 0 || height == 0 {
            return true;
        }
        let blit = self
            .blits
            .entry(slot.atlas.format())
            .or_insert_with(|| SourceBlit::new(device, slot.atlas.format()));
        let place: [u32; 4] = [x, y, source.origin[0], source.origin[1]];
        let mut bytes = [0_u8; 16];
        for (chunk, value) in bytes.chunks_exact_mut(4).zip(place) {
            chunk.copy_from_slice(&value.to_ne_bytes());
        }
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vello.image_source"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let source_view = source
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let target_view = slot
            .atlas
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vello.image_source"),
            layout: &blit.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vello.image_source"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Every other image in the atlas stays.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(x as f32, y as f32, width as f32, height as f32, 0.0, 1.0);
            pass.set_scissor_rect(x, y, width, height);
            pass.set_pipeline(if source.opaque {
                &blit.opaque
            } else {
                &blit.keep_alpha
            });
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.writes = self.writes.saturating_add(1);
        self.bytes = self
            .bytes
            .saturating_add(u64::from(width) * u64::from(height) * 4);
        true
    }
}
