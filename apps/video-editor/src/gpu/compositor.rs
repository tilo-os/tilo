//! The wgpu compute pipeline that composites a timeline frame.
//!
//! One [`VideoCompositor`] owns the pipelines and a pair of accumulation
//! textures. [`VideoCompositor::render`] walks the [`crate::preview::plan_layers`]
//! stack bottom-to-top, dispatching one compute pass per layer that reads
//! the accumulation, blends the layer over it, and writes the other
//! accumulation — then presents the result into the caller's target.
//!
//! Both the preview canvas and the export call the *same* method, so what
//! is encoded is what was on screen.

use std::sync::Arc;

use vello::wgpu;

use crate::model::Project;
use crate::preview::{self, Layer};

use super::{FrameSource, SourceTexture};

/// Effects beyond this per clip are ignored by the shader (its uniform
/// carries a fixed-size array). Kept in sync with `MAX_EFFECTS` in
/// `composite.wgsl`.
pub const MAX_SHADER_EFFECTS: usize = 8;

/// Bytes of the uniform struct in `composite.wgsl`: thirteen `vec4<f32>`
/// headers plus the effect array.
const PARAMS_BYTES: usize = (13 + MAX_SHADER_EFFECTS) * 16;

/// Where on the target the composited frame lands, in target pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    /// The whole of a `width × height` target.
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

struct Gpu {
    layer_pipeline: wgpu::ComputePipeline,
    present_pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// A 1×1 texture bound wherever a pass has no real input, so one bind
    /// group layout serves every pass.
    dummy: wgpu::TextureView,
    uniform_stride: u32,
}

struct Accum {
    width: u32,
    height: u32,
    textures: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
}

/// One of the two accumulation targets. Passes ping-pong between them, so an
/// index names a target by its parity alone.
fn ping_pong<T>(pair: &[T; 2], i: usize) -> &T {
    let [even, odd] = pair;
    if i.is_multiple_of(2) { even } else { odd }
}

/// The baked tone curves for one composite: `LUT_SIZE` entries across, one
/// row per layer.
///
/// It is rebuilt only when some layer actually carries a curve — a timeline
/// nobody has graded allocates and uploads nothing, and its layers bind the
/// 1×1 dummy at the LUT slot instead.
struct Lut {
    rows: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The bytes currently on the GPU, so a held playhead re-uploads
    /// nothing. A curve only changes when the user shapes it or an animated
    /// mix moves, both of which change these bytes.
    uploaded: Vec<u8>,
}

/// The single compositor used by the preview and the export.
#[derive(Default)]
pub struct VideoCompositor {
    gpu: Option<Gpu>,
    accum: Option<Accum>,
    uniform: Option<(wgpu::Buffer, u32)>,
    lut: Option<Lut>,
    /// Draws title layers. A title's picture comes from the document, not
    /// from a decoder, so the compositor makes it itself rather than asking
    /// the frame source — which is what lets a title render with no media
    /// pipeline at all.
    titles: super::title::TitleRaster,
    /// Uploaded title pictures, keyed by the raster they came from, so a
    /// held title is uploaded once instead of every frame.
    title_textures: std::collections::HashMap<usize, SourceTexture>,
    /// The buffer the finished frame is copied into to be read back, kept
    /// across frames with the size it was made for.
    ///
    /// An export reads back every frame at full resolution — 8 MB at 1080p —
    /// and this used to be allocated and freed for each one. Allocating a
    /// buffer that size thirty times a second is real work for the driver's
    /// allocator, and it is pure waste: the size only changes when the output
    /// size does. The accumulation textures beside it were already kept this
    /// way; the readback buffer was the one thing in the frame still being
    /// rebuilt from scratch.
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

    /// Bake every layer's tone curve into one `LUT_SIZE × layers` texture,
    /// returning `true` when at least one layer has a curve to look up.
    ///
    /// A layer with no curve still gets a row — the identity ramp — so the
    /// row index is simply the layer index and there is no mapping to keep
    /// in step. Its uniform says `lut.x = 0` regardless, so that row is
    /// never read.
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
        // `layers` is never empty on this branch (an empty stack has no
        // curve), but the texture must still have the row it was sized for.
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
        // Re-upload only when the baked bytes actually differ, so holding a
        // playhead over a graded clip costs one comparison.
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

    /// Composite the timeline at `playhead` into `target`, inside
    /// `viewport`. `sources` resolves each layer's picture; a layer whose
    /// source yields nothing falls back to its deterministic
    /// representative fill, so this works with no decoder at all.
    ///
    /// `target` must be an `Rgba8Unorm` texture view with
    /// `STORAGE_BINDING` usage — vello's intermediate render target and
    /// the export's offscreen texture both qualify.
    ///
    /// # Panics
    ///
    /// Never in practice: the pipelines, accumulation and uniform buffer it
    /// reads are all built by the accumulation pass it runs first.
    #[expect(
        clippy::expect_used,
        reason = "`accumulate` builds every resource read here before returning"
    )]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        viewport: Viewport,
        project: &Project,
        playhead: f64,
        sources: &mut dyn FrameSource,
    ) {
        let (w, h) = (viewport.width.max(1), viewport.height.max(1));
        let layers = preview::plan_layers(project, playhead);
        let last = self.accumulate(device, queue, w, h, &layers, sources);

        let gpu = self.gpu.as_ref().expect("gpu built by accumulate");
        let (buffer, stride) = self.uniform.as_ref().expect("uniform built by accumulate");
        let accum = self.accum.as_ref().expect("accum built by accumulate");

        // The present pass reads the finished accumulation and stores it
        // at the viewport's origin on the caller's target.
        let params = Params {
            frame: [w as f32, h as f32, 1.0, 0.0],
            wipe: [0.0, 0.0, viewport.x as f32, viewport.y as f32],
            ..Params::default()
        };
        let offset = u64::from(*stride) * layers.len() as u64;
        queue.write_buffer(buffer, offset, &params.to_bytes());

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-composite-present"),
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
                    resource: wgpu::BindingResource::TextureView(ping_pong(&accum.views, last)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&gpu.dummy),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(target),
                },
                // The present pass copies a finished frame; it never grades,
                // so the LUT slot takes the dummy.
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&gpu.dummy),
                },
            ],
        });

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("video-composite-present"),
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("video-composite-present"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gpu.present_pipeline);
            pass.set_bind_group(0, &bg, &[offset as u32]);
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        queue.submit([enc.finish()]);
    }

    /// Composite the timeline at `playhead` and read the frame back as a
    /// tightly-packed `width * height * 4` RGBA buffer.
    ///
    /// This is the export's frame source. It runs the identical layer
    /// stack, grade, effect chain and transition math the preview shows,
    /// so the encoded file *is* the preview.
    ///
    /// # Panics
    ///
    /// When the GPU fails to map the readback buffer — the frame cannot be
    /// read at all then.
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
        let accum = self.accum.as_ref().expect("accum built by accumulate");

        // `copy_texture_to_buffer` requires each row to start on a
        // COPY_BYTES_PER_ROW_ALIGNMENT boundary, so the staging buffer is
        // padded and the rows are re-packed after the map.
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let unpadded = w * 4;
        let padded = unpadded.div_ceil(align) * align;
        // Re-use the buffer whenever the output size is unchanged, which is
        // every frame of an export and every frame of a preview that is not
        // being resized.
        if !matches!(self.readback, Some((_, rw, rh)) if rw == w && rh == h) {
            self.readback = Some((
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("video-composite-readback"),
                    size: u64::from(padded) * u64::from(h),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                w,
                h,
            ));
        }
        let staging = &self
            .readback
            .as_ref()
            .expect("readback buffer just ensured")
            .0;

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
                    bytes_per_row: Some(padded),
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

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("readback channel").expect("readback map");

        let mapped = slice.get_mapped_range();
        let row = unpadded as usize;
        let mut out = Vec::with_capacity(row * h as usize);
        // The buffer holds exactly `h` padded rows, each at least `row` long.
        for padded_row in mapped.chunks(padded as usize).take(h as usize) {
            if let Some(pixels) = padded_row.get(..row) {
                out.extend_from_slice(pixels);
            }
        }
        drop(mapped);
        staging.unmap();
        out
    }

    /// Raster a title at the frame size and upload it, re-using the upload
    /// when the same raster comes back.
    fn title_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text: &crate::model::TextSource,
        w: u32,
        h: u32,
    ) -> Option<SourceTexture> {
        let rgba = self.titles.raster(text, w, h)?;
        // The `Arc`'s address identifies the raster: the cache hands back
        // the same allocation for the same text, style and size, so a held
        // title uploads once.
        let key = std::sync::Arc::as_ptr(&rgba) as usize;
        if let Some(hit) = self.title_textures.get(&key) {
            return Some(hit.clone());
        }
        // Uploads are cheap to keep but not free; a handful of live titles
        // is the realistic working set.
        if self.title_textures.len() > 8 {
            self.title_textures.clear();
        }
        let tex = upload_rgba(device, queue, w, h, &rgba)?;
        self.title_textures.insert(key, tex.clone());
        Some(tex)
    }

    /// Run every layer pass. Returns which accumulation index holds the
    /// finished frame.
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
        // One slot per layer plus one for the present pass.
        self.ensure_uniform(device, stride, layers.len() + 1);
        let has_lut = self.ensure_lut(device, queue, layers);

        // Resolve every layer's picture up front: `sources` may want the
        // device, and holding the borrow across the pass loop would clash
        // with the &mut self the pipelines live behind.
        sources.begin_frame();
        let resolved: Vec<Option<SourceTexture>> = layers
            .iter()
            .map(|l| match &l.title {
                // A title is drawn, not decoded.
                Some(t) => self.title_texture(device, queue, t, w, h),
                None => sources.resolve(device, queue, l.media, l.source_time),
            })
            .collect();

        let gpu = self.gpu.as_ref().expect("gpu built above");
        let accum = self.accum.as_ref().expect("accum built above");
        let (buffer, _) = self.uniform.as_ref().expect("uniform built above");
        // Every pass binds something at the LUT slot so one bind group
        // layout serves them all; with no curve anywhere that is the 1×1
        // dummy, and no uniform names a row.
        let lut_view: &wgpu::TextureView = match (has_lut, self.lut.as_ref()) {
            (true, Some(l)) => &l.view,
            _ => &gpu.dummy,
        };

        // With no layers at all the frame is letterbox black; a single
        // clearing pass still has to run so the accumulation is not stale.
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
                        // Read the accumulation the previous pass wrote.
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

/// The uniform for pass `i` of a `w × h` composite: `layer` over the
/// accumulation, or — with no layer at all — the clearing pass.
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
        // The empty-timeline clear: fully transparent coverage
        // leaves the frame at black.
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
    // Geometry. `Layer::transform` is already sampled at this
    // instant, so an animated move arrives here as a number.
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
    // A key or a mask that is switched off is passed as
    // inactive rather than as neutral numbers, so the shader
    // takes neither branch on the ordinary clip.
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
    // A curve is a baked table this layer looks up; an identity
    // one is left inactive so the shader takes no lookup at all.
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

/// Host-side mirror of the shader's `Params` uniform.
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
            // An identity grade at full coverage.
            grade: [1.0, 1.0, 1.0, 1.0],
            palette: [0.0; 4],
            src: [0.0; 4],
            wipe: [0.0; 4],
            // The identity placement: unit scale, no offset, no rotation,
            // centre anchor, opaque source.
            xform_a: [1.0, 1.0, 0.0, 0.0],
            xform_b: [0.0, 0.5, 0.5, 0.0],
            // The whole source is visible.
            crop: [0.0, 0.0, 1.0, 1.0],
            // No key and no mask: every pixel of the layer contributes.
            key_a: [0.0; 4],
            key_b: [0.0; 4],
            mask_a: [0.0; 4],
            mask_b: [0.0; 4],
            // No baked tone curve: the shader takes no lookup at all.
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
            // The baked tone-curve table: read with `textureLoad`, never
            // sampled, so it is unfilterable as far as the layout cares.
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
    let present_pipeline = make("cs_present", "video-composite-present");

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
        present_pipeline,
        bgl,
        sampler,
        dummy,
        uniform_stride,
    }
}

/// Upload a tightly-packed RGBA8 picture into a fresh texture.
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
