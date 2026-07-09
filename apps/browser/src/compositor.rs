//! Zero-copy WebView compositor (feature `wpe`).
//!
//! vello cannot sample an external GPU texture, so the browser draws its
//! WebView content *outside* vello: this [`FramePostProcess`] runs on the
//! render thread after vello encodes the chrome, imports the latest
//! WebKit dmabuf frame into a wgpu texture with no copy (see
//! [`ui::webview::wpe::import`]), and blits it over the content rect the
//! chrome leaves for it.

use std::sync::{Arc, Mutex};

use ui::render_thread::FramePostProcess;
use ui::webview::SharedEngine;
use ui::webview::wpe::import::import_dmabuf;
use vello::wgpu;

/// The content rect (WebView surface) as fractions `[x0, y0, x1, y1]` of
/// the whole window, written by the chrome each layout and read by the
/// compositor. Fractions are resolution/scale independent.
pub type ContentRect = Arc<Mutex<Option<[f32; 4]>>>;

// A compute pass, not a render pass: vello's intermediate texture is a
// STORAGE_BINDING target (no RENDER_ATTACHMENT), so we `textureStore` the
// sampled WebView into the content-rect region.
const SHADER: &str = r#"
struct P { ox: u32, oy: u32, w: u32, h: u32 };
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var dst: texture_storage_2d<rgba8unorm, write>;
@compute @workgroup_size(8, 8)
fn cs(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.w || gid.y >= p.h) { return; }
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(p.w), f32(p.h));
    let c = textureSampleLevel(src, samp, uv, 0.0);
    textureStore(dst, vec2<i32>(i32(p.ox + gid.x), i32(p.oy + gid.y)), c);
}
"#;

struct Gpu {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
}

pub struct WebViewCompositor {
    engine: SharedEngine,
    rect: ContentRect,
    gpu: Option<Gpu>,
    /// Current imported frame (kept alive) + its view.
    current: Option<(wgpu::Texture, wgpu::TextureView)>,
}

impl WebViewCompositor {
    pub fn new(engine: SharedEngine, rect: ContentRect) -> Self {
        Self {
            engine,
            rect,
            gpu: None,
            current: None,
        }
    }

    fn ensure_gpu(&mut self, device: &wgpu::Device) -> &Gpu {
        self.gpu.get_or_insert_with(|| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("webview-composite"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("webview-composite"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("webview-composite"),
                bind_group_layouts: &[Some(&bgl)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("webview-composite"),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some("cs"),
                compilation_options: Default::default(),
                cache: None,
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("webview-composite"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("webview-composite-rect"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Gpu {
                pipeline,
                bgl,
                sampler,
                uniform,
            }
        })
    }
}

impl FramePostProcess for WebViewCompositor {
    fn post_render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        self.ensure_gpu(device);

        // Import a freshly exported frame (if any); keep the last one when
        // no new frame arrived this paint.
        if let Some(frame) = self.engine.take_dmabuf() {
            if let Some(tex) = import_dmabuf(device, &frame) {
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                self.current = Some((tex, view));
            }
        }

        let (Some((_, src_view)), Some(rect)) = (&self.current, *self.rect.lock().unwrap()) else {
            return;
        };
        let gpu = self.gpu.as_ref().unwrap();

        // Fractions → pixel region on the target, clamped in bounds.
        let ox = (rect[0] * width as f32).round().clamp(0.0, width as f32) as u32;
        let oy = (rect[1] * height as f32).round().clamp(0.0, height as f32) as u32;
        let x1 = (rect[2] * width as f32).round().clamp(0.0, width as f32) as u32;
        let y1 = (rect[3] * height as f32).round().clamp(0.0, height as f32) as u32;
        let (w, h) = (x1.saturating_sub(ox), y1.saturating_sub(oy));
        if w == 0 || h == 0 {
            return;
        }
        let dims = [ox, oy, w, h];
        let mut bytes = [0u8; 16];
        for (i, v) in dims.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
        }
        queue.write_buffer(&gpu.uniform, 0, &bytes);

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("webview-composite"),
            layout: &gpu.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: gpu.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(target),
                },
            ],
        });

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("webview-composite"),
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("webview-composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        queue.submit([enc.finish()]);
    }
}
