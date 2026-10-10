//! The pass after vello that turns its 8-bit sRGB frame into an HDR signal.
//!
//! One render pipeline per HDR scanout format (`Rgb10a2Unorm` for
//! `XBGR2101010`, `Rgba16Float` for `ABGR16161616F`) running
//! [`crate::shell::color_pipeline::OUTPUT_ENCODE_SHADER_SOURCE`]. Both formats
//! carry the PQ-encoded BT.2020 signal the connector's HDR metadata announces.

use vello::wgpu;

use settings_store::hardware::OutputTransform;

use super::fullscreen_pass;
use super::orientation::shader_code;
use crate::shell::color_pipeline::{OUTPUT_ENCODE_SHADER_SOURCE, sanitize_sdr_white_nits};

/// Bytes of the shader's `EncodeParams` uniform: SDR white (`f32`) and the output's
/// orientation (`u32`), padded to 16.
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
                fullscreen_pass::texture_entry(0, wgpu::TextureViewDimension::D2, false),
                fullscreen_pass::uniform_entry(1, PARAMS_BYTES),
            ],
        });
        let pipeline =
            fullscreen_pass::pipeline(device, "tilo-hdr-output-encode", &module, &layout, format);
        let params =
            fullscreen_pass::uniform_buffer(device, "tilo-hdr-output-encode-params", PARAMS_BYTES);
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

    /// Where SDR white lands, in cd/m², and how the output is turned against its surface.
    /// Takes effect with the next submission on `queue`.
    pub fn set_params(&self, queue: &wgpu::Queue, nits: f32, orientation: OutputTransform) {
        let mut bytes = [0_u8; 16];
        let white = sanitize_sdr_white_nits(nits).to_le_bytes();
        if let Some(slot) = bytes.get_mut(..4) {
            slot.copy_from_slice(&white);
        }
        if let Some(slot) = bytes.get_mut(4..8) {
            slot.copy_from_slice(&shader_code(orientation).to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &bytes);
    }

    /// Encode all of `source` (the vello target, in scene orientation) into `target`, turned
    /// by the orientation of [`Self::set_params`].
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
        fullscreen_pass::draw(
            encoder,
            "tilo-hdr-output-encode",
            target,
            // PQ black; the triangle then covers every pixel.
            wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            &self.pipeline,
            &bind_group,
        );
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
        pipeline.set_params(queue, sdr_white, OutputTransform::Normal);
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
