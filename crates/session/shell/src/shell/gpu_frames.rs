//! Client frames that stay on the GPU.
//!
//! A Wayland client's dma-buf is imported on the device the compositor
//! presents with and copied, GPU to GPU, into a texture the compositor owns;
//! the client gets its buffer back as soon as that copy completes. The frame
//! is drawn from that texture: the presenting renderer copies it into its
//! image atlas (`vello::Renderer::override_image`), so no pixel crosses the
//! CPU on the way to the panel.
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

/// Stop drawing `surface`'s GPU frames once it is gone.
pub fn forget_surface(surface: u64) {
    registry().forget_surface(surface);
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
/// `Rgba8Unorm` texture and wait for the copy, so the caller may release the
/// buffer behind `source`.
fn copy_to_owned(
    present: &PresentDevice,
    source: &wgpu::Texture,
    opaque: bool,
    surface: u64,
) -> Result<GpuFrame, String> {
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
    present
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_WAIT),
        })
        .map_err(|error| format!("client frame copy wait: {error:?}"))?;
    Ok(GpuFrame {
        device: present.device.clone(),
        queue: present.queue.clone(),
        generation: present.generation,
        texture,
        width,
        height,
        surface,
        rgba: OnceLock::new(),
        image: Mutex::new(None),
    })
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
        &texel.repeat(width.saturating_mul(height) as usize),
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
    Arc::new(copy_to_owned(&present, &source, false, surface).expect("copy"))
}

/// Import a client's single-plane 8-bit RGB dma-buf on the presenting device
/// and copy it into a compositor-owned frame. On return the client's buffer
/// is no longer read.
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
    use smithay::backend::allocator::{Buffer as _, Fourcc};
    use std::os::fd::AsFd as _;

    let present = present_device().ok_or_else(|| "no presenting GPU yet".to_owned())?;
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
    // Vulkan does not wait for the client's rendering into the buffer: the
    // dma-buf's implicit fence does. Readable means its writes are done.
    wait_readable(fd.as_fd());
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
    let source = imported
        .planes
        .first()
        .ok_or_else(|| "the import returned no plane".to_owned())?;
    let frame = copy_to_owned(&present, source, opaque, surface)?;
    // The imported aliases go only after the copy completed.
    drop(imported);
    Ok(Arc::new(frame))
}

/// Block until a dma-buf's pending writes (its implicit fence) are done, at
/// most [`GPU_WAIT`].
#[cfg(target_os = "linux")]
fn wait_readable(fd: std::os::fd::BorrowedFd<'_>) {
    use std::os::fd::AsRawFd as _;
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout = i32::try_from(GPU_WAIT.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one valid pollfd, borrowed for the duration of the call.
    let _ = unsafe { libc::poll(&raw mut poll, 1, timeout) };
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
            let frame = Arc::new(copy_to_owned(&present, &texture, false, 1).expect("copy"));
            assert!(
                frame.rgba.get().is_none(),
                "nothing is read back until asked"
            );
            let pixels = frame.rgba().expect("readback");
            assert_eq!(pixels.len(), 3 * 2 * 4);
            assert_eq!(&pixels[..4], &[30, 20, 10, 128]);
        }
        // Phase `an_x_format_is_made_opaque`.
        {
            let texture = source(&present, wgpu::TextureFormat::Rgba8Unorm, [1, 2, 3, 0]);
            let frame = Arc::new(copy_to_owned(&present, &texture, true, 2).expect("copy"));
            assert_eq!(&frame.rgba().expect("readback")[..4], &[1, 2, 3, 255]);
        }
        // Phase `a_renderer_without_the_texture_gets_the_cropped_pixels`.
        {
            let texture = source(&present, wgpu::TextureFormat::Rgba8Unorm, [9, 8, 7, 255]);
            let frame = Arc::new(copy_to_owned(&present, &texture, false, 3).expect("copy"));
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
}
