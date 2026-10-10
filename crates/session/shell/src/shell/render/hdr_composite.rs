//! GPU side of HDR layer compositing (see [`crate::shell::hdr_surface`]).
//!
//! [`HdrCompositor::prepare`] uploads each layer's plane, provides the black
//! and channel-colour textures vello substitutes for the layers' SDR images,
//! and the texture the second render lands in. [`HdrCompositor::encode`]
//! then runs [`COMPOSITE_SHADER_SOURCE`] into an HDR scanout target in place
//! of the plain SDR → PQ encode.

use settings_store::hardware::OutputTransform;
use vello::peniko::ImageData;
use vello::wgpu;

use super::orientation::shader_code;
use crate::shell::color_pipeline::sanitize_sdr_white_nits;
use crate::shell::hdr_surface::{
    COMPOSITE_SHADER_SOURCE, HdrLayer, HdrTexelFormat, MAX_HDR_LAYERS,
};

/// Layer entries the shader reads; one layer on two displays is two entries.
const GPU_LAYER_ENTRIES: usize = 4;
/// Bytes of one shader `Layer`.
const LAYER_BYTES: usize = 112;
/// Bytes of the shader's `Params`.
const PARAMS_BYTES: usize = 16 + GPU_LAYER_ENTRIES * LAYER_BYTES;
/// Byte offset of `Params.orientation`, after `sdr_white_nits` and `layer_count`.
const ORIENTATION_OFFSET: u64 = 8;

/// One layer image vello draws: black in the first render, its channel
/// colour in the second.
#[derive(Clone)]
pub struct HdrSubstitution {
    pub image: ImageData,
    pub black: wgpu::TexelCopyTextureInfoBase<wgpu::Texture>,
    pub lit: wgpu::TexelCopyTextureInfoBase<wgpu::Texture>,
}

/// What the renderer needs for one composited frame.
#[derive(Clone)]
pub struct HdrPass {
    pub substitutions: Vec<HdrSubstitution>,
    /// Where the second render goes.
    pub second_view: wgpu::TextureView,
}

struct PlaneSlot {
    id: u64,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct SizedTexture {
    texture: wgpu::Texture,
    size: (u32, u32),
}

pub struct HdrCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    module: wgpu::ShaderModule,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    params: wgpu::Buffer,
    sampler: wgpu::Sampler,
    empty_plane: wgpu::TextureView,
    second: Option<(wgpu::Texture, wgpu::TextureView)>,
    planes: [Option<PlaneSlot>; MAX_HDR_LAYERS],
    black: Option<SizedTexture>,
    lit: [Option<SizedTexture>; MAX_HDR_LAYERS],
}

impl HdrCompositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tilo-hdr-composite"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER_SOURCE.into()),
        });
        let texture_entry = |binding: u32, filterable: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-hdr-composite"),
            entries: &[
                texture_entry(0, false),
                texture_entry(1, false),
                texture_entry(2, true),
                texture_entry(3, true),
                texture_entry(4, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_BYTES as u64),
                    },
                    count: None,
                },
            ],
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-hdr-composite-params"),
            size: PARAMS_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tilo-hdr-composite-plane"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let empty_plane = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("tilo-hdr-composite-empty-plane"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            device: device.clone(),
            queue: queue.clone(),
            layout,
            module,
            pipelines: Vec::new(),
            params,
            sampler,
            empty_plane,
            second: None,
            planes: std::array::from_fn(|_| None),
            black: None,
            lit: std::array::from_fn(|_| None),
        }
    }

    fn ensure_pipeline(&mut self, format: wgpu::TextureFormat) {
        if self.pipelines.iter().any(|(f, _)| *f == format) {
            return;
        }
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("tilo-hdr-composite"),
                bind_group_layouts: &[Some(&self.layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("tilo-hdr-composite"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
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
            });
        self.pipelines.push((format, pipeline));
    }

    /// Get everything ready to composite `layers` into a `size` frame: upload
    /// planes that changed, size the substitution and second-render textures,
    /// and write the shader parameters. `target` names the scanout format and
    /// SDR white when the GPU composites; the readback presenter composites
    /// on the CPU and passes `None`.
    pub fn prepare(
        &mut self,
        size: (u32, u32),
        layers: &[HdrLayer],
        target: Option<(wgpu::TextureFormat, f32)>,
    ) -> HdrPass {
        if let Some((format, _)) = target {
            self.ensure_pipeline(format);
        }
        let second_view = self.ensure_second(size);
        let mut substitutions: Vec<HdrSubstitution> = Vec::new();
        for layer in layers {
            let channel = usize::from(layer.channel);
            self.upload_plane(channel, layer);
            if substitutions
                .iter()
                .any(|known| known.image.data.id() == layer.image.data.id())
            {
                continue;
            }
            let extent = (layer.image.width, layer.image.height);
            let black = self.substitution_texture(None, extent);
            let lit = self.substitution_texture(Some(channel), extent);
            let (Some(black), Some(lit)) = (black, lit) else {
                continue;
            };
            substitutions.push(HdrSubstitution {
                image: layer.image.clone(),
                black: copy_source(black),
                lit: copy_source(lit),
            });
        }
        let sdr_white = target.map_or(0.0, |(_, white)| sanitize_sdr_white_nits(white));
        self.queue
            .write_buffer(&self.params, 0, &params_bytes(sdr_white, layers));
        HdrPass {
            substitutions,
            second_view,
        }
    }

    fn ensure_second(&mut self, (width, height): (u32, u32)) -> wgpu::TextureView {
        if let Some((texture, view)) = &self.second
            && (texture.width(), texture.height()) == (width, height)
        {
            return view.clone();
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-hdr-composite-second"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.second = Some((texture, view.clone()));
        view
    }

    fn upload_plane(&mut self, channel: usize, layer: &HdrLayer) {
        let plane = &layer.plane;
        let Some(slot) = self.planes.get_mut(channel) else {
            return;
        };
        if slot.as_ref().is_some_and(|slot| slot.id == plane.id()) {
            return;
        }
        let format = match plane.format() {
            HdrTexelFormat::Rgb10a2 => wgpu::TextureFormat::Rgb10a2Unorm,
            HdrTexelFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        };
        let extent = wgpu::Extent3d {
            width: plane.width(),
            height: plane.height(),
            depth_or_array_layers: 1,
        };
        // A playing video brings a new plane every frame at the same size;
        // its texture is rewritten rather than reallocated.
        let reusable = slot.take().filter(|slot| {
            slot.texture.format() == format
                && (slot.texture.width(), slot.texture.height()) == (extent.width, extent.height)
        });
        let (texture, view) = if let Some(slot) = reusable {
            (slot.texture, slot.view)
        } else {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tilo-hdr-composite-plane"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        };
        let bytes_per_row = u32::try_from(plane.format().bytes_per_pixel())
            .unwrap_or(8)
            .saturating_mul(plane.width());
        self.queue.write_texture(
            texture.as_image_copy(),
            plane.bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(plane.height()),
            },
            extent,
        );
        *slot = Some(PlaneSlot {
            id: plane.id(),
            texture,
            view,
        });
    }

    /// The opaque black texture (`channel` `None`) or the texture in
    /// `channel`'s primary colour, at least `extent` large.
    fn substitution_texture(
        &mut self,
        channel: Option<usize>,
        (width, height): (u32, u32),
    ) -> Option<wgpu::Texture> {
        let colour = match channel {
            None => [0.0, 0.0, 0.0],
            Some(0) => [1.0, 0.0, 0.0],
            Some(1) => [0.0, 1.0, 0.0],
            Some(_) => [0.0, 0.0, 1.0],
        };
        let slot = match channel {
            None => &mut self.black,
            Some(channel) => self.lit.get_mut(channel)?,
        };
        if let Some(existing) = slot.as_ref()
            && existing.size.0 >= width
            && existing.size.1 >= height
        {
            return Some(existing.texture.clone());
        }
        let size = slot.as_ref().map_or((width, height), |existing| {
            (existing.size.0.max(width), existing.size.1.max(height))
        });
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-hdr-composite-substitute"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tilo-hdr-composite-substitute"),
            });
        {
            let [r, g, b] = colour;
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tilo-hdr-composite-substitute"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit([encoder.finish()]);
        *slot = Some(SizedTexture {
            texture: texture.clone(),
            size,
        });
        Some(texture)
    }

    /// How the next [`Self::encode`] turns the output against its surface: the shader's
    /// `Params.orientation`, after the parameters [`Self::prepare`] wrote. Takes effect with
    /// the next submission on the queue.
    pub fn set_orientation(&self, orientation: OutputTransform) {
        self.queue.write_buffer(
            &self.params,
            ORIENTATION_OFFSET,
            &shader_code(orientation).to_le_bytes(),
        );
    }

    /// Composite `first` (vello's render with the layers black) and the
    /// prepared second render into `target`, encoded as PQ BT.2020 and turned by
    /// [`Self::set_orientation`].
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        first: &wgpu::TextureView,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let (Some((_, pipeline)), Some((_, second))) = (
            self.pipelines.iter().find(|(f, _)| *f == format),
            self.second.as_ref(),
        ) else {
            return;
        };
        let plane = |channel: usize| {
            self.planes
                .get(channel)
                .and_then(Option::as_ref)
                .map_or(&self.empty_plane, |slot| &slot.view)
        };
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-hdr-composite"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(first),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(second),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(plane(0)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(plane(1)),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(plane(2)),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tilo-hdr-composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Read the completed second render back as tight RGBA, for the CPU
    /// composite of the readback presenter.
    ///
    /// # Errors
    /// Fails when the copy cannot be mapped before `timeout`.
    pub fn read_second(&self, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
        let Some((texture, _)) = self.second.as_ref() else {
            return Err("no second render".to_owned());
        };
        let (width, height) = (texture.width(), texture.height());
        let tight = width.saturating_mul(4);
        let padded = tight
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .saturating_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-hdr-composite-second-readback"),
            size: u64::from(padded).saturating_mul(u64::from(height)),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tilo-hdr-composite-second-readback"),
            });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(timeout),
            })
            .map_err(|error| format!("second render readback: {error:?}"))?;
        rx.recv_timeout(timeout)
            .map_err(|error| format!("second render map: {error}"))?
            .map_err(|error| format!("second render map: {error:?}"))?;
        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((tight as usize).saturating_mul(height as usize));
        for row in mapped.chunks(padded as usize).take(height as usize) {
            out.extend_from_slice(row.get(..tight as usize).unwrap_or(row));
        }
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }
}

fn copy_source(texture: wgpu::Texture) -> wgpu::TexelCopyTextureInfoBase<wgpu::Texture> {
    wgpu::TexelCopyTextureInfoBase {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

/// The shader's `Params` for `layers`, little-endian.
fn params_bytes(sdr_white_nits: f32, layers: &[HdrLayer]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(PARAMS_BYTES);
    let count = layers.len().min(GPU_LAYER_ENTRIES);
    if layers.len() > GPU_LAYER_ENTRIES {
        tracing::debug!(
            entries = layers.len(),
            "more HDR layer entries than the composite shader reads; the rest keep their SDR rendition"
        );
    }
    bytes.extend_from_slice(&sdr_white_nits.to_le_bytes());
    bytes.extend_from_slice(&u32::try_from(count).unwrap_or(0).to_le_bytes());
    // `orientation` (normal until `set_orientation`) and padding.
    bytes.extend_from_slice(&[0; 8]);
    let mut push = |values: [f32; 4]| {
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    };
    for index in 0..GPU_LAYER_ENTRIES {
        let Some(layer) = layers.get(index).filter(|_| index < count) else {
            for _ in 0..LAYER_BYTES / 16 {
                push([0.0; 4]);
            }
            continue;
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scene coordinates and affine coefficients are small; f32 is the shader's precision"
        )]
        let [xx, yx, xy, yy, x0, y0] = layer.scene_to_image.as_coeffs().map(|v| v as f32);
        push([xx, xy, x0, 0.0]);
        push([yx, yy, y0, 0.0]);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scene pixel bounds fit f32 exactly enough for a coverage test"
        )]
        push([
            layer.bounds.x0 as f32,
            layer.bounds.y0 as f32,
            layer.bounds.x1 as f32,
            layer.bounds.y1 as f32,
        ]);
        let matrix = layer.plane.signal().to_bt2020();
        for [r0, r1, r2] in matrix.m {
            push([r0, r1, r2, 0.0]);
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "plane extents are texture sizes, far below f32's exact-integer range"
        )]
        let (width, height) = (layer.plane.width() as f32, layer.plane.height() as f32);
        push([
            width,
            height,
            f32::from_bits(u32::from(layer.channel)),
            f32::from_bits(layer.plane.signal().transfer.shader_code()),
        ]);
    }
    bytes
}
