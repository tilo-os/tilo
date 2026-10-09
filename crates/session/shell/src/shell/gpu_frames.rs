//! Client frames that stay on the GPU.
//!
//! A Wayland client's dma-buf is imported on the device the compositor
//! presents with — once per buffer: [`import_dmabuf`] keeps the import for
//! the buffer's later commits — and copied, GPU to GPU, into a texture the
//! compositor owns. Nothing here waits: the copy is submitted and its
//! completion handed back as a [`CopyFence`], which the capture path waits on
//! away from the thread that imports every surface's frames; the client gets
//! its buffer back only after that copy completed. The frame is drawn from
//! that texture: the presenting renderer copies it into its image atlas
//! (`vello::Renderer::override_image`), so no pixel crosses the CPU on the
//! way to the panel.
//!
//! Pixels are read back only when something asks for them: a screenshot, a
//! window preview, a stream, or a renderer on another device. Those go
//! through [`GpuFrame::rgba`] (once per frame) or the vello external image
//! source this module installs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use vello::wgpu;

/// How long an image of a superseded frame stays drawable. A scene built
/// before the newer frame arrived can still be waiting for the presenter.
const SUPERSEDED_GRACE: Duration = Duration::from_secs(1);
/// The longest a GPU copy or readback may take before it is abandoned.
const GPU_WAIT: Duration = Duration::from_secs(1);
/// Client buffers kept imported per surface: a swapchain of up to four, and
/// the buffers of the size it is being resized away from.
#[cfg(any(test, target_os = "linux"))]
const IMPORTS_PER_SURFACE: usize = 8;

/// The device and queue the compositor presents with.
///
/// Devices are told apart by `generation`, not by `wgpu::Device` equality:
/// two devices opened through different wgpu instances can compare equal.
#[derive(Clone)]
struct PresentDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    generation: u64,
}

static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

static PRESENT_DEVICE: Mutex<Option<PresentDevice>> = Mutex::new(None);

/// Make `device` the one client frames are imported on. Called by the
/// presenting renderer whenever it opens a device; it passes the returned
/// generation to [`apply`].
pub fn publish_present_device(device: &wgpu::Device, queue: &wgpu::Queue) -> u64 {
    vello::set_external_image_source(external_pixels);
    let generation = NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    *PRESENT_DEVICE
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(PresentDevice {
        device: device.clone(),
        queue: queue.clone(),
        generation,
    });
    generation
}

fn present_device() -> Option<PresentDevice> {
    PRESENT_DEVICE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// A region of a frame: `x, y, width, height` in its pixels.
type Crop = (u32, u32, u32, u32);

/// One client frame in a compositor-owned `Rgba8Unorm` texture, with
/// premultiplied alpha like the client's buffer.
pub struct GpuFrame {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The presenting device it was made on.
    generation: u64,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    /// The surface it belongs to: its older frames retire when it registers an image.
    surface: u64,
    /// The pixels, once something read them back.
    rgba: OnceLock<Option<Arc<Vec<u8>>>>,
    /// The image drawn for a crop of this frame, reused while the crop holds.
    image: Mutex<Option<(Crop, ImageData)>>,
    /// The GPU copy that filled `texture`.
    copy: CopyFence,
}

/// The submitted GPU copy that fills a [`GpuFrame`]'s texture.
///
/// Work on the presenting queue runs in order, so whatever samples the
/// texture later (a render, a readback) already sees the copied pixels. The
/// fence says when the copy's *source*, the client's buffer, is no longer
/// read.
#[derive(Clone, Debug)]
pub struct CopyFence {
    device: wgpu::Device,
    submission: wgpu::SubmissionIndex,
}

impl CopyFence {
    /// Whether the copy has finished, asked without waiting.
    pub fn is_done(&self) -> bool {
        self.wait_for(Duration::ZERO)
    }

    /// Wait for the copy, at most [`GPU_WAIT`]. `false` when it did not
    /// finish in that time; on a lost device it never does.
    pub fn wait(&self) -> bool {
        self.wait_for(GPU_WAIT)
    }

    fn wait_for(&self, timeout: Duration) -> bool {
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(self.submission.clone()),
                timeout: Some(timeout),
            })
            .is_ok()
    }
}

impl std::fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("surface", &self.surface)
            .finish_non_exhaustive()
    }
}

/// Textures of dropped frames, reused for the next frames of the same size so
/// a client presenting steadily costs no allocation per frame.
static POOL: Mutex<Vec<(u64, wgpu::Texture)>> = Mutex::new(Vec::new());
/// Spare textures kept: a window's next frame or two, for a few windows.
const POOL_LIMIT: usize = 6;

fn take_pooled(generation: u64, width: u32, height: u32) -> Option<wgpu::Texture> {
    let mut pool = POOL.lock().unwrap_or_else(PoisonError::into_inner);
    let at = pool.iter().position(|(owner, texture)| {
        *owner == generation && texture.width() == width && texture.height() == height
    })?;
    Some(pool.swap_remove(at).1)
}

impl Drop for GpuFrame {
    fn drop(&mut self) {
        let mut pool = POOL.lock().unwrap_or_else(PoisonError::into_inner);
        // The pool holds one device's textures: a new device starts it over.
        pool.retain(|(owner, _)| *owner == self.generation);
        if pool.len() == POOL_LIMIT {
            pool.remove(0);
        }
        pool.push((self.generation, self.texture.clone()));
    }
}

impl GpuFrame {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The copy that fills this frame. The client's buffer is read until it
    /// is done.
    pub fn copy_fence(&self) -> CopyFence {
        self.copy.clone()
    }

    /// The frame's pixels, tightly packed RGBA, read back on first use.
    pub fn rgba(&self) -> Option<Arc<Vec<u8>>> {
        self.rgba
            .get_or_init(|| match read_back(self) {
                Ok(pixels) => Some(Arc::new(pixels)),
                Err(error) => {
                    tracing::warn!(%error, "could not read a client frame back from the GPU");
                    None
                }
            })
            .clone()
    }

    /// The image that draws `crop` (`x, y, width, height`) of this frame.
    /// Its blob is empty: the presenting renderer draws it from the texture,
    /// any other renderer through the external image source.
    pub fn image(self: &Arc<Self>, crop: (u32, u32, u32, u32)) -> ImageData {
        let mut cached = self.image.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, image)) = cached.as_ref()
            && *at == crop
        {
            return image.clone();
        }
        let image = ImageData {
            data: Blob::new(Arc::new(Vec::<u8>::new())),
            format: ImageFormat::Rgba8,
            // Wayland `ARGB8888` is premultiplied; `X` formats are made opaque on import.
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: crop.2,
            height: crop.3,
        };
        registry().register(&image, Arc::clone(self), crop);
        *cached = Some((crop, image.clone()));
        image
    }
}

struct Registered {
    image: ImageData,
    frame: Arc<GpuFrame>,
    crop: (u32, u32, u32, u32),
    /// When a newer frame of the same surface registered an image.
    superseded: Option<Instant>,
}

#[derive(Default)]
struct Registry {
    images: Mutex<HashMap<u64, Registered>>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

impl Registry {
    fn register(&self, image: &ImageData, frame: Arc<GpuFrame>, crop: (u32, u32, u32, u32)) {
        let now = Instant::now();
        let mut images = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        for entry in images.values_mut() {
            if entry.frame.surface == frame.surface
                && !Arc::ptr_eq(&entry.frame, &frame)
                && entry.superseded.is_none()
            {
                entry.superseded = Some(now);
            }
        }
        images.retain(|_, entry| {
            entry
                .superseded
                .is_none_or(|at| now.saturating_duration_since(at) < SUPERSEDED_GRACE)
        });
        images.insert(
            image.data.id(),
            Registered {
                image: image.clone(),
                frame,
                crop,
                superseded: None,
            },
        );
    }

    /// Forget every image of `surface`: it was destroyed.
    fn forget_surface(&self, surface: u64) {
        self.images
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, entry| entry.frame.surface != surface);
    }
}

/// Stop drawing `surface`'s GPU frames once it is gone, and drop the imports
/// of its buffers.
pub fn forget_surface(surface: u64) {
    registry().forget_surface(surface);
    #[cfg(target_os = "linux")]
    IMPORTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .forget_surface(surface);
}

/// The client frames a renderer draws from their textures, by image id.
#[derive(Default)]
pub struct AppliedFrames {
    applied: HashMap<u64, ImageData>,
}

/// Point `renderer`, on the presenting device of `generation`, at the textures
/// of every live frame made there, and drop the frames that retired since the
/// last call. A renderer on any other device (`None`) draws them through the
/// external image source.
pub fn apply(renderer: &mut vello::Renderer, generation: Option<u64>, applied: &mut AppliedFrames) {
    let images = registry()
        .images
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    applied.applied.retain(|id, image| {
        let live = images.contains_key(id);
        if !live {
            renderer.override_image(image, None);
        }
        live
    });
    for (id, entry) in images.iter() {
        if Some(entry.frame.generation) != generation || applied.applied.contains_key(id) {
            continue;
        }
        renderer.override_image(
            &entry.image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: entry.frame.texture.clone(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: entry.crop.0,
                    y: entry.crop.1,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            }),
        );
        applied.applied.insert(*id, entry.image.clone());
    }
}

/// The vello external image source: a registered image's pixels, read back
/// for a renderer that cannot copy its texture.
fn external_pixels(id: u64) -> Option<Vec<u8>> {
    let (frame, crop) = registry()
        .images
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&id)
        .map(|entry| (Arc::clone(&entry.frame), entry.crop))?;
    let pixels = frame.rgba()?;
    Some(crop_rgba(&pixels, frame.width, crop))
}

/// The `(x, y, width, height)` region of a tightly packed RGBA frame.
pub fn crop_rgba(rgba: &[u8], width: u32, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
    let stride = (width as usize).saturating_mul(4);
    let row = (w as usize).saturating_mul(4);
    let left = (x as usize).saturating_mul(4);
    let mut out = Vec::with_capacity(row.saturating_mul(h as usize));
    for line in y..y.saturating_add(h) {
        let start = (line as usize).saturating_mul(stride).saturating_add(left);
        if let Some(pixels) = rgba.get(start..start.saturating_add(row)) {
            out.extend_from_slice(pixels);
        }
    }
    out
}

fn read_back(frame: &GpuFrame) -> Result<Vec<u8>, String> {
    let row = frame.width.saturating_mul(4);
    let padded = row
        .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .saturating_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = frame.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("client-frame-readback"),
        size: u64::from(padded).saturating_mul(u64::from(frame.height)),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = frame
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        frame.texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(frame.height),
            },
        },
        wgpu::Extent3d {
            width: frame.width,
            height: frame.height,
            depth_or_array_layers: 1,
        },
    );
    let submission = frame.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    frame
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_WAIT),
        })
        .map_err(|error| format!("readback wait: {error:?}"))?;
    rx.recv_timeout(GPU_WAIT)
        .map_err(|error| format!("readback map: {error}"))?
        .map_err(|error| format!("readback map: {error:?}"))?;
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((row as usize).saturating_mul(frame.height as usize));
    for line in mapped.chunks(padded as usize).take(frame.height as usize) {
        pixels.extend_from_slice(line.get(..row as usize).unwrap_or(line));
    }
    drop(mapped);
    buffer.unmap();
    Ok(pixels)
}

/// The copy pipelines for one device: alpha kept, and alpha forced opaque.
struct CopyPipelines {
    generation: u64,
    layout: wgpu::BindGroupLayout,
    keep_alpha: wgpu::RenderPipeline,
    opaque: wgpu::RenderPipeline,
}

const COPY_SHADER: &str = r"
override opaque: bool = false;
@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(source, vec2<i32>(at.xy), 0);
    return select(texel, vec4<f32>(texel.rgb, 1.0), opaque);
}
";

impl CopyPipelines {
    fn new(device: &wgpu::Device, generation: u64) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("client-frame-copy"),
            source: wgpu::ShaderSource::Wgsl(COPY_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("client-frame-copy"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("client-frame-copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |opaque: bool| {
            let constants = [("opaque", if opaque { 1.0 } else { 0.0 })];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("client-frame-copy"),
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
                        format: wgpu::TextureFormat::Rgba8Unorm,
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
            generation,
            layout,
            keep_alpha: pipeline(false),
            opaque: pipeline(true),
        }
    }
}

/// Copy `source` (any colour format the device samples) into a new owned
/// `Rgba8Unorm` texture. The copy is submitted, not waited for: the buffer
/// behind `source` may be released once the frame's [`CopyFence`] is done.
fn copy_to_owned(
    present: &PresentDevice,
    source: &wgpu::Texture,
    opaque: bool,
    surface: u64,
) -> GpuFrame {
    static PIPELINES: Mutex<Option<Arc<CopyPipelines>>> = Mutex::new(None);
    let pipelines = {
        let mut slot = PIPELINES.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some(pipelines) if pipelines.generation == present.generation => Arc::clone(pipelines),
            _ => {
                let pipelines = Arc::new(CopyPipelines::new(&present.device, present.generation));
                *slot = Some(Arc::clone(&pipelines));
                pipelines
            }
        }
    };
    let (width, height) = (source.width(), source.height());
    let texture = take_pooled(present.generation, width, height).unwrap_or_else(|| {
        present.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    });
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
    let target_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = present
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("client-frame-copy"),
            layout: &pipelines.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            }],
        });
    let mut encoder = present
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("client-frame-copy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(if opaque {
            &pipelines.opaque
        } else {
            &pipelines.keep_alpha
        });
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    let submission = present.queue.submit([encoder.finish()]);
    GpuFrame {
        device: present.device.clone(),
        queue: present.queue.clone(),
        generation: present.generation,
        texture,
        width,
        height,
        surface,
        rgba: OnceLock::new(),
        image: Mutex::new(None),
        copy: CopyFence {
            device: present.device.clone(),
            submission,
        },
    }
}

/// A frame holding `texel` everywhere, on `device`, as an imported client
/// frame would be.
#[cfg(test)]
pub(crate) fn frame_for_test(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    generation: u64,
    (width, height): (u32, u32),
    texel: [u8; 4],
    surface: u64,
) -> Arc<GpuFrame> {
    frame_pixels_for_test(
        device,
        queue,
        generation,
        (width, height),
        &texel.repeat(width.saturating_mul(height) as usize),
        surface,
    )
}

#[cfg(test)]
pub(crate) fn frame_pixels_for_test(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    generation: u64,
    (width, height): (u32, u32),
    pixels: &[u8],
    surface: u64,
) -> Arc<GpuFrame> {
    let source = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("client-frame-test-source"),
        size: wgpu::Extent3d {
            width,
            height,
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
        source.as_image_copy(),
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width.saturating_mul(4)),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let present = PresentDevice {
        device: device.clone(),
        queue: queue.clone(),
        generation,
    };
    Arc::new(copy_to_owned(&present, &source, false, surface))
}

/// What names a client buffer in the import cache, and says when it is gone.
#[cfg(any(test, target_os = "linux"))]
trait BufferIdentity: PartialEq {
    /// The buffer was destroyed: its import can never be used again.
    fn is_gone(&self) -> bool;
}

#[cfg(target_os = "linux")]
impl BufferIdentity for smithay::backend::allocator::dmabuf::WeakDmabuf {
    fn is_gone(&self) -> bool {
        smithay::backend::allocator::dmabuf::WeakDmabuf::is_gone(self)
    }
}

#[cfg(any(test, target_os = "linux"))]
struct CachedImport<K, V> {
    /// The presenting device it was imported on.
    generation: u64,
    surface: u64,
    buffer: K,
    value: V,
    /// When it was last used, on the cache's own clock.
    used: u64,
}

/// Client buffers imported on the presenting device, kept for their next
/// commits: a client cycles through the same two to four buffers, so a steady
/// one costs no Vulkan import after each buffer's first commit. An entry goes
/// when its buffer is destroyed, its surface is forgotten, the presenting
/// device changes, or its surface imports more than [`IMPORTS_PER_SURFACE`]
/// buffers (the least recently used goes first).
#[cfg(any(test, target_os = "linux"))]
struct ImportCache<K, V> {
    entries: Vec<CachedImport<K, V>>,
    clock: u64,
    /// Imports made since the process started.
    imports: u64,
}

#[cfg(any(test, target_os = "linux"))]
impl<K: BufferIdentity, V: Clone> ImportCache<K, V> {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
            clock: 0,
            imports: 0,
        }
    }

    /// The import of `buffer` on the device of `generation`, made by `import`
    /// only when it is not cached.
    fn get_or_import(
        &mut self,
        (generation, surface): (u64, u64),
        buffer: K,
        import: impl FnOnce() -> Result<V, String>,
    ) -> Result<V, String> {
        self.clock = self.clock.wrapping_add(1);
        self.entries
            .retain(|entry| entry.generation == generation && !entry.buffer.is_gone());
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.surface == surface && entry.buffer == buffer)
        {
            entry.used = self.clock;
            return Ok(entry.value.clone());
        }
        let value = import()?;
        self.imports = self.imports.saturating_add(1);
        let oldest = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.surface == surface)
            .min_by_key(|(_, entry)| entry.used)
            .map(|(at, _)| at);
        let held = self
            .entries
            .iter()
            .filter(|entry| entry.surface == surface)
            .count();
        if held >= IMPORTS_PER_SURFACE
            && let Some(oldest) = oldest
        {
            self.entries.swap_remove(oldest);
        }
        self.entries.push(CachedImport {
            generation,
            surface,
            buffer,
            value: value.clone(),
            used: self.clock,
        });
        Ok(value)
    }

    fn forget_surface(&mut self, surface: u64) {
        self.entries.retain(|entry| entry.surface != surface);
    }
}

/// The imports of every client buffer, as the texture of their one plane.
#[cfg(target_os = "linux")]
static IMPORTS: Mutex<ImportCache<smithay::backend::allocator::dmabuf::WeakDmabuf, wgpu::Texture>> =
    Mutex::new(ImportCache::new());

/// Vulkan imports of client buffers since the process started. A steady
/// client adds one per buffer it owns, not one per commit.
#[cfg(target_os = "linux")]
pub fn buffer_imports() -> u64 {
    IMPORTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .imports
}

/// Import a client's single-plane 8-bit RGB dma-buf on the presenting device,
/// or reuse its import from an earlier commit, and submit its copy into a
/// compositor-owned frame. The client's buffer is read until the frame's
/// [`GpuFrame::copy_fence`] is done.
///
/// The caller makes sure the client's rendering into the buffer finished
/// first (its implicit fence signalled): Vulkan does not wait for it.
///
/// # Errors
///
/// No presenting device yet, a format or layout the import cannot describe,
/// or a GPU failure. The caller falls back to reading the buffer on the CPU.
#[cfg(target_os = "linux")]
pub fn import_dmabuf(
    dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
    surface: u64,
) -> Result<Arc<GpuFrame>, String> {
    let present = present_device().ok_or_else(|| "no presenting GPU yet".to_owned())?;
    import_dmabuf_on(&present, dmabuf, surface)
}

#[cfg(target_os = "linux")]
fn import_dmabuf_on(
    present: &PresentDevice,
    dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
    surface: u64,
) -> Result<Arc<GpuFrame>, String> {
    use smithay::backend::allocator::{Buffer as _, Fourcc};
    use std::os::fd::AsFd as _;

    let fourcc = dmabuf.format().code;
    let opaque = match fourcc {
        Fourcc::Argb8888 | Fourcc::Abgr8888 => false,
        Fourcc::Xrgb8888 | Fourcc::Xbgr8888 => true,
        other => return Err(format!("{other:?} is not an 8-bit RGB format")),
    };
    let handles: Vec<_> = dmabuf.handles().collect();
    let offsets: Vec<_> = dmabuf.offsets().collect();
    let strides: Vec<_> = dmabuf.strides().collect();
    let (Some(fd), Some(&offset), Some(&stride), 1) = (
        handles.first(),
        offsets.first(),
        strides.first(),
        handles.len(),
    ) else {
        return Err("expected one plane".to_owned());
    };
    let source = IMPORTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_import((present.generation, surface), dmabuf.weak(), || {
            let planes = [ui::dmabuf_import::PlaneRef {
                fd: fd.as_fd(),
                offset,
                stride,
            }];
            let imported = ui::dmabuf_import::import(
                &present.device,
                &ui::dmabuf_import::DmabufRef {
                    planes: &planes,
                    fourcc: fourcc as u32,
                    modifier: u64::from(dmabuf.format().modifier),
                    width: dmabuf.width(),
                    height: dmabuf.height(),
                    colorimetry: media::dmabuf::Colorimetry::RGB,
                },
            )
            .map_err(|error| error.to_string())?;
            // The texture owns its `VkImage` and imported memory; wgpu frees
            // them only after the last submission that used them retired.
            imported
                .planes
                .into_iter()
                .next()
                .ok_or_else(|| "the import returned no plane".to_owned())
        })?;
    Ok(Arc::new(copy_to_owned(present, &source, opaque, surface)))
}

/// Block until a dma-buf's pending writes (its implicit fence) are done, at
/// most [`GPU_WAIT`]. Only for a buffer whose fence could not be watched on
/// the compositor loop.
#[cfg(target_os = "linux")]
pub(crate) fn wait_readable(dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) {
    use std::os::fd::AsRawFd as _;
    let timeout = i32::try_from(GPU_WAIT.as_millis()).unwrap_or(i32::MAX);
    for fd in dmabuf.handles() {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, borrowed for the duration of the call.
        let _ = unsafe { libc::poll(&raw mut poll, 1, timeout) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present() -> Option<PresentDevice> {
        let mut context = ui::gpu::RenderContext::new();
        let id = pollster::block_on(context.device(None))?;
        let handle = context.devices.get(id)?;
        Some(PresentDevice {
            device: handle.device.clone(),
            queue: handle.queue.clone(),
            generation: NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        })
    }

    fn source(
        present: &PresentDevice,
        format: wgpu::TextureFormat,
        texel: [u8; 4],
    ) -> wgpu::Texture {
        let texture = present.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client-frame-test-source"),
            size: wgpu::Extent3d {
                width: 3,
                height: 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        present.queue.write_texture(
            texture.as_image_copy(),
            &texel.repeat(6),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(12),
                rows_per_image: Some(2),
            },
            wgpu::Extent3d {
                width: 3,
                height: 2,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    #[test]
    fn client_frames_copy_on_the_gpu_and_read_back_only_on_demand() {
        let Some(present) = present() else {
            eprintln!("no wgpu adapter; skipping");
            return;
        };
        // Phase `a_bgra_buffer_lands_as_rgba_with_its_alpha`.
        {
            let texture = source(&present, wgpu::TextureFormat::Bgra8Unorm, [10, 20, 30, 128]);
            let frame = Arc::new(copy_to_owned(&present, &texture, false, 1));
            assert!(
                frame.rgba.get().is_none(),
                "nothing is read back until asked"
            );
            // Phase `the_copy_is_submitted_and_its_fence_says_when_the_source_is_free`.
            let fence = frame.copy_fence();
            assert!(fence.wait(), "the submitted copy finishes");
            assert!(
                fence.is_done(),
                "a finished copy reads as done without waiting"
            );
            let pixels = frame.rgba().expect("readback");
            assert_eq!(pixels.len(), 3 * 2 * 4);
            assert_eq!(&pixels[..4], &[30, 20, 10, 128]);
        }
        // Phase `an_x_format_is_made_opaque`.
        {
            let texture = source(&present, wgpu::TextureFormat::Rgba8Unorm, [1, 2, 3, 0]);
            let frame = Arc::new(copy_to_owned(&present, &texture, true, 2));
            assert_eq!(&frame.rgba().expect("readback")[..4], &[1, 2, 3, 255]);
        }
        // Phase `a_renderer_without_the_texture_gets_the_cropped_pixels`.
        {
            let texture = source(&present, wgpu::TextureFormat::Rgba8Unorm, [9, 8, 7, 255]);
            let frame = Arc::new(copy_to_owned(&present, &texture, false, 3));
            let image = frame.image((1, 0, 2, 2));
            assert_eq!((image.width, image.height), (2, 2));
            assert!(image.data.is_empty(), "the image carries no CPU pixels");
            assert_eq!(
                frame.image((1, 0, 2, 2)).data.id(),
                image.data.id(),
                "the same crop keeps its atlas identity"
            );
            assert_eq!(
                external_pixels(image.data.id()),
                Some([9, 8, 7, 255].repeat(4))
            );
            forget_surface(3);
            assert_eq!(external_pixels(image.data.id()), None);
        }
    }

    /// A client buffer in the import cache: equal by id, gone once dropped.
    struct TestBuffer(u32, Arc<std::sync::atomic::AtomicBool>);

    impl PartialEq for TestBuffer {
        fn eq(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }

    impl BufferIdentity for TestBuffer {
        fn is_gone(&self) -> bool {
            self.1.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[test]
    fn a_client_buffer_is_imported_once_across_commits() {
        let fresh = |id| TestBuffer(id, Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let mut cache = ImportCache::<TestBuffer, u32>::new();
        let mut made: usize = 0;
        let mut import = |value| {
            made = made.saturating_add(1);
            Ok::<u32, String>(value)
        };
        // Phase `a_steady_swapchain_imports_each_buffer_once`.
        let first = fresh(1);
        let second = fresh(2);
        for _ in 0..3 {
            for buffer in [&first, &second] {
                let got = cache
                    .get_or_import((1, 10), TestBuffer(buffer.0, Arc::clone(&buffer.1)), || {
                        import(buffer.0)
                    })
                    .expect("import");
                assert_eq!(got, buffer.0, "the cached import is the buffer's own");
            }
        }
        assert_eq!(cache.imports, 2, "three commits of two buffers import two");
        // Phase `the_same_buffer_on_another_surface_is_its_own_import`.
        cache
            .get_or_import((1, 11), TestBuffer(1, Arc::clone(&first.1)), || import(1))
            .expect("import");
        assert_eq!(cache.imports, 3);
        // Phase `a_destroyed_buffer_is_dropped_and_a_new_device_imports_again`.
        first.1.store(true, std::sync::atomic::Ordering::Relaxed);
        cache
            .get_or_import((1, 10), TestBuffer(2, Arc::clone(&second.1)), || import(2))
            .expect("import");
        assert!(
            cache.entries.iter().all(|entry| entry.buffer.0 != 1),
            "the destroyed buffer's imports are gone"
        );
        cache
            .get_or_import((2, 10), TestBuffer(2, Arc::clone(&second.1)), || import(2))
            .expect("import");
        assert_eq!(cache.imports, 4, "a new presenting device imports again");
        assert_eq!(cache.entries.len(), 1, "nothing of the old device is kept");
        // Phase `a_failed_import_is_not_cached`.
        let failed = cache.get_or_import((2, 10), fresh(3), || Err("refused".to_owned()));
        assert_eq!(failed, Err("refused".to_owned()));
        assert_eq!(cache.entries.len(), 1);
        // Phase `a_surface_keeps_a_bounded_set_and_forgets_on_destroy`.
        let buffers: Vec<_> = (100..)
            .take(IMPORTS_PER_SURFACE.saturating_add(3))
            .map(fresh)
            .collect();
        for buffer in &buffers {
            cache
                .get_or_import((2, 12), TestBuffer(buffer.0, Arc::clone(&buffer.1)), || {
                    import(buffer.0)
                })
                .expect("import");
        }
        assert_eq!(
            cache
                .entries
                .iter()
                .filter(|entry| entry.surface == 12)
                .count(),
            IMPORTS_PER_SURFACE
        );
        assert!(
            cache.entries.iter().all(|entry| entry.buffer.0 != 100),
            "the least recently used buffer went first"
        );
        cache.forget_surface(12);
        assert!(cache.entries.iter().all(|entry| entry.surface != 12));
        assert_eq!(cache.entries.len(), 1, "another surface's import stays");
        assert_eq!(made, buffers.len().saturating_add(4));
    }
}
