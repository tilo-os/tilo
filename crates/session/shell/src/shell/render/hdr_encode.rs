//! The pass after vello that turns its 8-bit sRGB frame into an HDR signal.
//!
//! One render pipeline per HDR scanout format (`Rgb10a2Unorm` for
//! `XBGR2101010`, `Rgba16Float` for `ABGR16161616F`) running
//! [`crate::shell::color_pipeline::OUTPUT_ENCODE_SHADER_SOURCE`]. Both formats
//! carry the PQ-encoded BT.2020 signal the connector's HDR metadata announces.

use vello::wgpu;

use crate::shell::color_pipeline::{OUTPUT_ENCODE_SHADER_SOURCE, sanitize_sdr_white_nits};

/// Bytes of the shader's `EncodeParams` uniform: one `f32` padded to 16.
const PARAMS_BYTES: u64 = 16;

pub struct HdrEncodePipeline {
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
}

impl HdrEncodePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tilo-hdr-output-encode"),
            source: wgpu::ShaderSource::Wgsl(OUTPUT_ENCODE_SHADER_SOURCE.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-hdr-output-encode"),
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
            label: Some("tilo-hdr-output-encode"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tilo-hdr-output-encode"),
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
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-hdr-output-encode-params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            format,
            pipeline,
            layout,
            params,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Where SDR white lands, in cd/m². Takes effect with the next
    /// submission on `queue`.
    pub fn set_sdr_white(&self, queue: &wgpu::Queue, nits: f32) {
        let mut bytes = [0_u8; 16];
        let white = sanitize_sdr_white_nits(nits).to_le_bytes();
        if let Some(slot) = bytes.get_mut(..4) {
            slot.copy_from_slice(&white);
        }
        queue.write_buffer(&self.params, 0, &bytes);
    }

    /// Encode all of `source` (the vello target) into `target`.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-hdr-output-encode"),
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
            label: Some("tilo-hdr-output-encode"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // PQ black; the triangle then covers every pixel.
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::color_pipeline::map_sdr_pixel_to_pq_bt2020_at;
    use pollster::FutureExt;

    const PIXELS: [[u8; 4]; 8] = [
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [128, 128, 128, 255],
        [10, 200, 90, 255],
        [3, 3, 3, 255],
    ];

    /// Encode `PIXELS` on a real adapter and read back the 10-bit words.
    fn encode_on_gpu(sdr_white: f32) -> Vec<u32> {
        let mut context = ui::gpu::RenderContext::new();
        let id = context.device(None).block_on().expect("real GPU adapter");
        let gpu = &context.devices[id];
        let (device, queue) = (&gpu.device, &gpu.queue);
        let width = u32::try_from(PIXELS.len()).expect("small");
        let extent = wgpu::Extent3d {
            width,
            height: 1,
            depth_or_array_layers: 1,
        };
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr-encode-test-source"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = PIXELS.iter().flatten().copied().collect();
        queue.write_texture(
            source.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width.saturating_mul(4)),
                rows_per_image: Some(1),
            },
            extent,
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr-encode-test-target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgb10a2Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hdr-encode-test-readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let pipeline = HdrEncodePipeline::new(device, wgpu::TextureFormat::Rgb10a2Unorm);
        assert_eq!(pipeline.format(), wgpu::TextureFormat::Rgb10a2Unorm);
        pipeline.set_sdr_white(queue, sdr_white);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        pipeline.encode(
            device,
            &mut encoder,
            &source.create_view(&wgpu::TextureViewDescriptor::default()),
            &target.create_view(&wgpu::TextureViewDescriptor::default()),
        );
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            extent,
        );
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(tilo_test_env::hang_bound()),
            })
            .expect("GPU finishes the encode");
        rx.recv().expect("map callback").expect("map succeeds");
        let mapped = slice.get_mapped_range();
        mapped
            .chunks_exact(4)
            .take(PIXELS.len())
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect()
    }

    #[test]
    fn gpu_encode_matches_the_cpu_reference() {
        for white in [203.0, 400.0] {
            let words = encode_on_gpu(white);
            for (pixel, word) in PIXELS.iter().zip(words) {
                let reference =
                    map_sdr_pixel_to_pq_bt2020_at([pixel[0], pixel[1], pixel[2]], white);
                // Rgb10a2Unorm: red in the low ten bits, as XBGR2101010.
                let got = [word & 0x3FF, (word >> 10) & 0x3FF, (word >> 20) & 0x3FF];
                for (channel, (g, r)) in got.iter().zip(reference).enumerate() {
                    let delta = i64::from(*g) - i64::from(r);
                    assert!(
                        delta.abs() <= 2,
                        "{white} nits, pixel {pixel:?} channel {channel}: GPU {g}, CPU {r}"
                    );
                }
            }
        }
    }
}
