//! The wgpu compute pipeline that composites one timeline frame: one compute pass
//! per layer, bottom to top, ping-ponging between two accumulation textures.
//!
//! The preview canvas and the export call the same method, so what is encoded is
//! what was on screen.

use std::sync::Arc;

use vello::wgpu;

use crate::model::Project;
use crate::preview::{self, Layer};

use super::{FrameSource, SourceTexture};

/// Kept in sync with `MAX_EFFECTS` in `composite.wgsl`, whose uniform carries a
/// fixed-size array.
pub const MAX_SHADER_EFFECTS: usize = 8;

const PARAMS_BYTES: usize = (13 + MAX_SHADER_EFFECTS) * 16;

struct Gpu {
    layer_pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    uniform_stride: u32,
}

struct Accum {
    width: u32,
    height: u32,
    textures: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
}

fn ping_pong<T>(pair: &[T; 2], i: usize) -> &T {
    let [even, odd] = pair;
    if i.is_multiple_of(2) { even } else { odd }
}

/// Rebuilt only when some layer carries a curve: a timeline nobody has graded
/// allocates nothing and binds the 1×1 dummy at the LUT slot.
struct Lut {
    rows: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The bytes on the GPU, so a held playhead re-uploads nothing.
    uploaded: Vec<u8>,
}

#[derive(Default)]
pub struct VideoCompositor {
    gpu: Option<Gpu>,
    accum: Option<Accum>,
    uniform: Option<(wgpu::Buffer, u32)>,
    lut: Option<Lut>,
    /// A title's picture comes from the document, not a decoder, which is what lets
    /// a title render with no media pipeline at all.
    titles: super::title::TitleRaster,
    title_textures: std::collections::HashMap<usize, SourceTexture>,
    /// An export reads back every frame at full resolution, 8 MB at 1080p, and the
    /// size only changes when the output size does.
    readback: Option<(wgpu::Buffer, u32, u32)>,
}

impl VideoCompositor {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_gpu(&mut self, device: &wgpu::Device) -> &Gpu {
        self.gpu.get_or_insert_with(|| build_gpu(device))
    }

    fn ensure_accum(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let stale = self
            .accum
            .as_ref()
            .is_none_or(|a| a.width != width || a.height != height);
        if !stale {
            return;
        }
        let make = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
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
            })
        };
        let a = make("video-composite-accum-a");
        let b = make("video-composite-accum-b");
        let va = a.create_view(&wgpu::TextureViewDescriptor::default());
        let vb = b.create_view(&wgpu::TextureViewDescriptor::default());
        self.accum = Some(Accum {
            width,
            height,
            textures: [a, b],
            views: [va, vb],
        });
    }

    fn ensure_uniform(&mut self, device: &wgpu::Device, stride: u32, count: usize) {
        let needed = u64::from(stride) * count.max(1) as u64;
        let big_enough = self
            .uniform
            .as_ref()
            .is_some_and(|(b, _)| b.size() >= needed);
        if big_enough {
            return;
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-composite-params"),
            size: needed.max(u64::from(stride)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.uniform = Some((buffer, stride));
    }

    /// A layer with no curve still gets the identity row, so the row index is the
    /// layer index; its uniform says `lut.x = 0`, so that row is never read.
    fn ensure_lut(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, layers: &[Layer]) -> bool {
        use crate::curve::LUT_SIZE;
        if layers.iter().all(|l| l.curve.is_identity()) {
            return false;
        }
        let rows = layers.len().max(1) as u32;
        let mut bytes = Vec::with_capacity(LUT_SIZE * 4 * rows as usize);
        for l in layers {
            bytes.extend_from_slice(&l.curve.bake());
        }
        bytes.resize(LUT_SIZE * 4 * rows as usize, 255);

        let lut = match &mut self.lut {
            Some(lut) if lut.rows == rows => lut,
            slot => {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("video-composite-lut"),
                    size: wgpu::Extent3d {
                        width: LUT_SIZE as u32,
                        height: rows,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                slot.insert(Lut {
                    rows,
                    texture,
                    view,
                    uploaded: Vec::new(),
                })
            }
        };
        if lut.uploaded != bytes {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &lut.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(LUT_SIZE as u32 * 4),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: LUT_SIZE as u32,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            lut.uploaded = bytes;
        }
        true
    }

    /// This is the export's frame source, running the identical layer stack, grade,
    /// effect chain and transition math the preview shows, so the encoded file is the
    /// preview.
    ///
    /// # Panics
    ///
    /// When the GPU fails to map the readback buffer.
    #[expect(
        clippy::expect_used,
        reason = "the accumulation and readback buffer are built just above; a failed map is fatal"
    )]
    pub fn render_rgba(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        project: &Project,
        playhead: f64,
        width: u32,
        height: u32,
        sources: &mut dyn FrameSource,
    ) -> Vec<u8> {
        let (w, h) = (width.max(1), height.max(1));
        let layers = preview::plan_layers(project, playhead);
        let last = self.accumulate(device, queue, w, h, &layers, sources);
        if !matches!(self.readback, Some((_, rw, rh)) if rw == w && rh == h) {
            self.readback = Some((readback_buffer(device, w, h), w, h));
        }
        let staging = &self
            .readback
            .as_ref()
            .expect("readback buffer just ensured")
            .0;
        self.copy_out(device, queue, last, staging);

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("readback channel").expect("readback map");
        unpad(staging, w, h)
    }

    /// The frame is composited and its read-back started;
    /// [`PendingFrame::try_take`] has the pixels once the GPU hands them over. A
    /// browser page cannot wait for its GPU, so its preview and export composite
    /// this way.
    pub fn render_rgba_later(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        project: &Project,
        playhead: f64,
        width: u32,
        height: u32,
        sources: &mut dyn FrameSource,
    ) -> PendingFrame {
        let (w, h) = (width.max(1), height.max(1));
        let layers = preview::plan_layers(project, playhead);
        let last = self.accumulate(device, queue, w, h, &layers, sources);
        // A buffer of its own: the last one may still be mapped by whoever has not
        // taken its pixels yet.
        let buffer = readback_buffer(device, w, h);
        self.copy_out(device, queue, last, &buffer);
        let mapped = Arc::new(std::sync::Mutex::new(None));
        let done = mapped.clone();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            if let Ok(mut slot) = done.lock() {
                *slot = Some(r.is_ok());
            }
        });
        // Native wgpu calls the callback only from a poll; a page calls it from its
        // event loop.
        let _ = device.poll(wgpu::PollType::Poll);
        PendingFrame {
            buffer,
            width: w,
            height: h,
            mapped,
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "the accumulation is built by accumulate just before"
    )]
    fn copy_out(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        last: usize,
        staging: &wgpu::Buffer,
    ) {
        let accum = self.accum.as_ref().expect("accum built by accumulate");
        let (w, h) = (accum.width, accum.height);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("video-composite-readback"),
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: ping_pong(&accum.textures, last),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row(w)),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
    }

    fn title_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text: &crate::model::TextSource,
        w: u32,
        h: u32,
    ) -> Option<SourceTexture> {
        let rgba = self.titles.raster(text, w, h)?;
        // The `Arc`'s address identifies the raster: the cache hands back the same
        // allocation for the same text, style and size.
        let key = std::sync::Arc::as_ptr(&rgba) as usize;
        if let Some(hit) = self.title_textures.get(&key) {
            return Some(hit.clone());
        }
        // A handful of live titles is the realistic working set.
        if self.title_textures.len() > 8 {
            self.title_textures.clear();
        }
        let tex = upload_rgba(device, queue, w, h, &rgba)?;
        self.title_textures.insert(key, tex.clone());
        Some(tex)
    }

    /// The accumulation index holding the finished frame.
    #[expect(
        clippy::expect_used,
        reason = "the pipelines, accumulation and uniform buffer are ensured at the top"
    )]
    fn accumulate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
        layers: &[Layer],
        sources: &mut dyn FrameSource,
    ) -> usize {
        let stride = {
            let gpu = self.ensure_gpu(device);
            gpu.uniform_stride
        };
        self.ensure_accum(device, w, h);
        // One slot per layer.
        self.ensure_uniform(device, stride, layers.len());
        let has_lut = self.ensure_lut(device, queue, layers);

        // Resolve every layer's picture up front: `sources` may want the device, and
        // holding that borrow across the pass loop would clash with the `&mut self`
        // the pipelines live behind.
        sources.begin_frame();
        let resolved: Vec<Option<SourceTexture>> = layers
            .iter()
            .map(|l| match &l.title {
                Some(t) => self.title_texture(device, queue, t, w, h),
                None => sources.resolve(device, queue, l.media, l.source_time),
            })
            .collect();

        let gpu = self.gpu.as_ref().expect("gpu built above");
        let accum = self.accum.as_ref().expect("accum built above");
        let (buffer, _) = self.uniform.as_ref().expect("uniform built above");
        // With no curve anywhere the dummy fills the slot, and no uniform names a
        // row.
        let lut_view: &wgpu::TextureView = match (has_lut, self.lut.as_ref()) {
            (true, Some(l)) => &l.view,
            _ => &gpu.dummy,
        };

        // With no layers the frame is letterbox black, but one clearing pass still
        // has to run so the accumulation is not stale.
        let passes = layers.len().max(1);
        let mut bind_groups = Vec::with_capacity(passes);
        let mut offsets = Vec::with_capacity(passes);

        for i in 0..passes {
            let tex = resolved.get(i).and_then(|t| t.as_ref());
            let params = layer_params(w, h, i, layers.get(i), tex, has_lut);

            let offset = u64::from(stride) * i as u64;
            queue.write_buffer(buffer, offset, &params.to_bytes());
            offsets.push(offset as u32);

            let src_view: &wgpu::TextureView = tex.map_or(&gpu.dummy, |t| t.view.as_ref());
            bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video-composite-layer"),
                layout: &gpu.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer,
                            offset: 0,
                            size: std::num::NonZeroU64::new(PARAMS_BYTES as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(ping_pong(
                            &accum.views,
                            i + 1,
                        )),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(ping_pong(&accum.views, i)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(lut_view),
                    },
                ],
            }));
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("video-composite-layers"),
        });
        for (bg, offset) in bind_groups.iter().zip(&offsets) {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("video-composite-layer"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.layer_pipeline);
            pass.set_bind_group(0, bg, &[*offset]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        queue.submit([enc.finish()]);

        (passes - 1) % 2
    }
}

fn layer_params(
    w: u32,
    h: u32,
    i: usize,
    layer: Option<&Layer>,
    tex: Option<&SourceTexture>,
    has_lut: bool,
) -> Params {
    let mut params = Params {
        frame: [
            w as f32,
            h as f32,
            if i == 0 { 0.0 } else { 1.0 },
            if tex.is_some() { 1.0 } else { 0.0 },
        ],
        ..Params::default()
    };
    let Some(layer) = layer else {
        // Transparent coverage leaves the frame at black.
        params.grade = [1.0, 1.0, 1.0, 0.0];
        return params;
    };
    let filters = &layer.filters;
    params.grade = [
        crate::model::Filters::gain(filters.brightness) as f32,
        crate::model::Filters::gain(filters.contrast) as f32,
        crate::model::Filters::gain(filters.saturation) as f32,
        layer.alpha as f32,
    ];
    let rgb = preview::palette_rgb(layer.palette);
    let count = layer.effects.len().min(MAX_SHADER_EFFECTS);
    params.palette = [rgb.r as f32, rgb.g as f32, rgb.b as f32, count as f32];
    for (slot, effect) in params
        .effects
        .iter_mut()
        .zip(layer.effects.iter())
        .take(count)
    {
        slot[0] = effect_index(effect.kind) as f32;
        slot[1] = effect.amount as f32;
    }
    if let Some(tex) = tex {
        params.src[0] = tex.width as f32;
        params.src[1] = tex.height as f32;
    }
    if let Some(wp) = layer.wipe {
        params.src[2] = 1.0;
        params.src[3] = wp.dir.index() as f32;
        params.wipe[0] = wp.progress as f32;
        params.wipe[1] = wp.softness as f32;
    }
    // `Layer::transform` is already sampled at this instant.
    let xf = &layer.transform;
    params.xform_a = [
        xf.scale_x as f32,
        xf.scale_y as f32,
        xf.pos_x as f32,
        xf.pos_y as f32,
    ];
    let (cl, ct, cr, cb) = xf.crop_rect();
    params.xform_b = [
        xf.rotation.to_radians() as f32,
        xf.anchor_x as f32,
        xf.anchor_y as f32,
        if layer.source_alpha { 1.0 } else { 0.0 },
    ];
    params.crop = [cl as f32, ct as f32, cr as f32, cb as f32];
    // A key or mask that is switched off is passed as inactive rather than as
    // neutral numbers, so the shader takes neither branch on an ordinary clip.
    if layer.key.is_active() {
        let [kr, kg, kb] = layer.key.color;
        params.key_a = [
            1.0,
            layer.key.tolerance as f32,
            layer.key.softness as f32,
            layer.key.spill as f32,
        ];
        params.key_b = [kr as f32, kg as f32, kb as f32, 0.0];
    }
    // An identity curve is left inactive, so the shader takes no lookup at all.
    if has_lut && !layer.curve.is_identity() {
        params.lut = [1.0, i as f32, 0.0, 0.0];
    }
    if layer.mask.is_active() {
        let mask = &layer.mask;
        params.mask_a = [
            mask.shape.index() as f32,
            mask.x as f32,
            mask.y as f32,
            mask.rotation.to_radians() as f32,
        ];
        params.mask_b = [
            mask.w as f32,
            mask.h as f32,
            mask.feather as f32,
            if mask.invert { 1.0 } else { 0.0 },
        ];
    }
    params
}

fn effect_index(kind: crate::model::EffectKind) -> u32 {
    use crate::model::EffectKind as E;
    match kind {
        E::Grayscale => 0,
        E::Sepia => 1,
        E::Invert => 2,
        E::Warm => 3,
        E::Cool => 4,
        E::Vignette => 5,
    }
}

#[derive(Clone, Copy)]
struct Params {
    frame: [f32; 4],
    grade: [f32; 4],
    palette: [f32; 4],
    src: [f32; 4],
    wipe: [f32; 4],
    xform_a: [f32; 4],
    xform_b: [f32; 4],
    crop: [f32; 4],
    key_a: [f32; 4],
    key_b: [f32; 4],
    mask_a: [f32; 4],
    mask_b: [f32; 4],
    lut: [f32; 4],
    effects: [[f32; 4]; MAX_SHADER_EFFECTS],
}

impl Default for Params {
    fn default() -> Self {
        Self {
            frame: [0.0; 4],
            grade: [1.0, 1.0, 1.0, 1.0],
            palette: [0.0; 4],
            src: [0.0; 4],
            wipe: [0.0; 4],
            xform_a: [1.0, 1.0, 0.0, 0.0],
            xform_b: [0.0, 0.5, 0.5, 0.0],
            crop: [0.0, 0.0, 1.0, 1.0],
            key_a: [0.0; 4],
            key_b: [0.0; 4],
            mask_a: [0.0; 4],
            mask_b: [0.0; 4],
            lut: [0.0; 4],
            effects: [[0.0; 4]; MAX_SHADER_EFFECTS],
        }
    }
}

impl Params {
    fn to_bytes(self) -> [u8; PARAMS_BYTES] {
        let vectors = [
            self.frame,
            self.grade,
            self.palette,
            self.src,
            self.wipe,
            self.xform_a,
            self.xform_b,
            self.crop,
            self.key_a,
            self.key_b,
            self.mask_a,
            self.mask_b,
            self.lut,
        ]
        .into_iter()
        .chain(self.effects);
        debug_assert_eq!(vectors.clone().count() * 16, PARAMS_BYTES);
        let mut out = [0u8; PARAMS_BYTES];
        let (words, _) = out.as_chunks_mut::<4>();
        for (word, c) in words.iter_mut().zip(vectors.flatten()) {
            *word = c.to_ne_bytes();
        }
        out
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "declarative wgpu descriptors for the one bind group layout and its pipelines"
)]
fn build_gpu(device: &wgpu::Device) -> Gpu {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("video-composite"),
        source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("video-composite"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: std::num::NonZeroU64::new(PARAMS_BYTES as u64),
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
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("video-composite"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let make = |entry: &str, label: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        })
    };
    let layer_pipeline = make("cs_layer", "video-composite-layer");

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("video-composite"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("video-composite-dummy"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let dummy = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let align = device.limits().min_uniform_buffer_offset_alignment.max(1);
    let uniform_stride = (PARAMS_BYTES as u32).div_ceil(align) * align;

    Gpu {
        layer_pipeline,
        bgl,
        sampler,
        dummy,
        uniform_stride,
    }
}

/// `copy_texture_to_buffer` needs each row to start on a
/// `COPY_BYTES_PER_ROW_ALIGNMENT` boundary, so the staging buffer is padded and the
/// rows are re-packed after the map.
fn padded_row(width: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    (width * 4).div_ceil(align) * align
}

fn readback_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("video-composite-readback"),
        size: u64::from(padded_row(width)) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

fn unpad(staging: &wgpu::Buffer, width: u32, height: u32) -> Vec<u8> {
    let slice = staging.slice(..);
    let mapped = slice.get_mapped_range();
    let row = (width * 4) as usize;
    let mut out = Vec::with_capacity(row * height as usize);
    for padded in mapped
        .chunks(padded_row(width) as usize)
        .take(height as usize)
    {
        if let Some(pixels) = padded.get(..row) {
            out.extend_from_slice(pixels);
        }
    }
    drop(mapped);
    staging.unmap();
    out
}

pub struct PendingFrame {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    mapped: Arc<std::sync::Mutex<Option<bool>>>,
}

impl PendingFrame {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn try_take(&self) -> Option<Result<Vec<u8>, String>> {
        let mapped = self.mapped.lock().ok().and_then(|slot| *slot)?;
        if !mapped {
            return Some(Err("the GPU could not hand the frame back".into()));
        }
        Some(Ok(unpad(&self.buffer, self.width, self.height)))
    }
}

impl Drop for PendingFrame {
    fn drop(&mut self) {
        // A page keeps a dropped buffer until its garbage collector runs; one per
        // composite would pile up.
        self.buffer.destroy();
    }
}

pub fn upload_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Option<SourceTexture> {
    let (w, h) = (width, height);
    if w == 0 || h == 0 {
        return None;
    }
    let pixels = rgba.get(..(w as usize * h as usize * 4))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("video-composite-source"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));
    Some(SourceTexture {
        texture: Arc::new(texture),
        view,
        width: w,
        height: h,
    })
}
