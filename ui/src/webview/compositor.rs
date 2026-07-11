//! Multi-layer zero-copy WebView compositor (feature `webview-wpe`).
//!
//! vello cannot sample an external GPU texture, so a WebView's page content
//! is composited *outside* vello: after vello encodes the scene into its
//! intermediate texture, this compositor imports each WebView's latest
//! WebKit dmabuf frame into a wgpu texture with no copy (see
//! [`crate::webview::wpe::import`]) and writes it over that WebView's rect.
//!
//! It is **multi-layer**: any number of WebViews can be composited in one
//! frame, each keyed by its engine. The render thread hands it the set of
//! [`LayerSnapshot`]s collected while drawing the widget tree — so a
//! WebView that wasn't laid out this frame simply isn't composited (no
//! stale content), and layers are pruned when their engine goes away.
//!
//! The composite is a **compute** pass, not a render pass: vello's
//! intermediate texture is a `STORAGE_BINDING` target (no
//! `RENDER_ATTACHMENT`), so it cannot be a color attachment — we
//! `textureStore` the sampled page into the target region instead.

use std::collections::HashMap;

use vello::wgpu;

use crate::render_thread::LayerSnapshot;
use crate::webview::wpe::import::import_dmabuf;

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
}

/// The last dmabuf frame imported for one engine, kept alive so its view
/// stays valid until the next import replaces it.
struct Imported {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Persistent compositor state living on the render thread: one shared
/// pipeline plus a per-engine imported-texture cache and a small pool of
/// per-layer uniform buffers.
pub struct MultiLayerCompositor {
    gpu: Option<Gpu>,
    imported: HashMap<usize, Imported>,
    uniforms: Vec<wgpu::Buffer>,
}

impl MultiLayerCompositor {
    pub fn new() -> Self {
        Self {
            gpu: None,
            imported: HashMap::new(),
            uniforms: Vec::new(),
        }
    }

    fn ensure_gpu(&mut self, device: &wgpu::Device) {
        if self.gpu.is_some() {
            return;
        }
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
        self.gpu = Some(Gpu {
            pipeline,
            bgl,
            sampler,
        });
    }

    fn uniform(&mut self, device: &wgpu::Device, i: usize) -> &wgpu::Buffer {
        while self.uniforms.len() <= i {
            self.uniforms.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("webview-composite-rect"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        &self.uniforms[i]
    }

    /// Composite every layer's latest page frame into `target`. Called on
    /// the render thread after vello encodes the scene, before the blit to
    /// the surface. `layers` are the WebViews drawn this frame, with their
    /// rects in **device pixels**.
    pub fn composite(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
        layers: &[LayerSnapshot],
    ) {
        if layers.is_empty() {
            // Nothing on screen this frame — free the imported textures so
            // a hidden WebView doesn't pin GPU memory.
            self.imported.clear();
            return;
        }
        self.ensure_gpu(device);

        // Import a freshly exported frame per engine; keep the previous one
        // when no new frame arrived this paint. Then drop engines that
        // aren't present this frame.
        let mut live: Vec<usize> = Vec::with_capacity(layers.len());
        for layer in layers {
            let id = layer.engine_id();
            live.push(id);
            if let Some(frame) = layer.engine.take_dmabuf() {
                if let Some(tex) = import_dmabuf(device, &frame) {
                    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                    self.imported.insert(id, Imported { _tex: tex, view });
                }
            }
        }
        self.imported.retain(|k, _| live.contains(k));

        // Grow the uniform-buffer pool up front so the composite loop only
        // needs shared borrows of self.
        for i in 0..layers.len() {
            let _ = self.uniform(device, i);
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("webview-composite"),
        });
        // Build all bind groups first, then run one pass.
        let mut jobs: Vec<(wgpu::BindGroup, u32, u32)> = Vec::new();
        for (i, layer) in layers.iter().enumerate() {
            let Some(img) = self.imported.get(&layer.engine_id()) else {
                continue;
            };
            let r = layer.dev_rect;
            let ox = (r[0]).round().clamp(0.0, width as f32) as u32;
            let oy = (r[1]).round().clamp(0.0, height as f32) as u32;
            let x1 = (r[2]).round().clamp(0.0, width as f32) as u32;
            let y1 = (r[3]).round().clamp(0.0, height as f32) as u32;
            let (w, h) = (x1.saturating_sub(ox), y1.saturating_sub(oy));
            if w == 0 || h == 0 {
                continue;
            }
            let mut bytes = [0u8; 16];
            for (j, v) in [ox, oy, w, h].iter().enumerate() {
                bytes[j * 4..j * 4 + 4].copy_from_slice(&v.to_ne_bytes());
            }
            queue.write_buffer(&self.uniforms[i], 0, &bytes);

            let gpu = self.gpu.as_ref().unwrap();
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("webview-composite"),
                layout: &gpu.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms[i].as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&img.view),
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
            jobs.push((bg, w.div_ceil(8), h.div_ceil(8)));
        }

        if jobs.is_empty() {
            return;
        }
        {
            let gpu = self.gpu.as_ref().unwrap();
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("webview-composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            for (bg, gx, gy) in &jobs {
                pass.set_bind_group(0, bg, &[]);
                pass.dispatch_workgroups(*gx, *gy, 1);
            }
        }
        queue.submit([enc.finish()]);
    }
}

impl Default for MultiLayerCompositor {
    fn default() -> Self {
        Self::new()
    }
}
