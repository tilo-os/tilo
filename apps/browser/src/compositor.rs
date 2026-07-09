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

const SHADER: &str = r#"
struct R { x0: f32, y0: f32, x1: f32, y1: f32 };
@group(0) @binding(0) var<uniform> r: R;
@group(0) @binding(1) var t: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    let c = vec2<f32>(f32(i & 1u), f32((i >> 1u) & 1u));
    let x = mix(r.x0, r.x1, c.x);
    let y = mix(r.y0, r.y1, c.y);
    var o: VO;
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}
@fragment fn fs(in: VO) -> @location(0) vec4<f32> {
    return textureSample(t, s, in.uv);
}
"#;

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
}

pub struct WebViewCompositor {
    engine: SharedEngine,
    rect: ContentRect,
    gpu: Option<Gpu>,
    /// Current imported frame (kept alive) + its bind group.
    current: Option<(wgpu::Texture, wgpu::BindGroup)>,
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
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("webview-composite"),
                bind_group_layouts: &[Some(&bgl)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("webview-composite"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        // vello's intermediate texture (render_thread).
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None, // opaque: the page owns its content rect
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
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
        _width: u32,
        _height: u32,
    ) {
        self.ensure_gpu(device);

        // Import a freshly exported frame (if any) and (re)build its bind
        // group. Keeps the last texture when no new frame arrived.
        if let Some(frame) = self.engine.take_dmabuf() {
            if let Some(tex) = import_dmabuf(device, &frame) {
                let gpu = self.gpu.as_ref().unwrap();
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
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
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                        },
                    ],
                });
                self.current = Some((tex, bg));
            }
        }

        let (Some((_, bind_group)), Some(rect)) =
            (&self.current, *self.rect.lock().unwrap())
        else {
            return;
        };
        let gpu = self.gpu.as_ref().unwrap();
        let mut rect_bytes = [0u8; 16];
        for (i, f) in rect.iter().enumerate() {
            rect_bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_ne_bytes());
        }
        queue.write_buffer(&gpu.uniform, 0, &rect_bytes);

        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webview-composite"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("webview-composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Keep the chrome vello already drew; draw over the
                        // content rect only.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
        queue.submit([enc.finish()]);
    }
}
