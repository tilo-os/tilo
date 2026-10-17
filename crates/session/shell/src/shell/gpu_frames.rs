//! Client frames that stay on the GPU.
//!
//! A Wayland client's dma-buf is imported on the device the compositor
//! presents with — once per buffer: [`import_dmabuf`] keeps the import for
//! the buffer's later commits — and drawn straight from that import. The
//! presenting renderer draws it into its image atlas through a render pass
//! (`vello::Renderer::draw_image_from`), which is the frame's one and only
//! copy: the format conversion (a client's `BGRA` buffer, alpha forced for
//! `X` formats) happens in it, inside the presenter's own command buffer.
//!
//! Drawing from the client's buffer means the compositor holds it: a
//! [`GpuFrame`] carries the [`BufferLease`] that gives the buffer back, and
//! the lease runs only once nothing can read the buffer again — the store
//! replaced the frame, every queued frame that draws it was presented (they
//! pin it, [`FramePins`]), every readback finished — and a GPU marker
//! submitted after that completed. A buffer is never handed back while the
//! GPU may still read it, and never later than the frames already in flight.
//!
//! Pixels are read back only when something asks for them: a screenshot, a
//! window preview, a stream, or a renderer on another device. Those go
//! through [`GpuFrame::rgba`] (once per frame) or the vello external image
//! source this module installs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use vello::peniko::ImageData;
use vello::wgpu;

/// The longest a GPU marker or readback may take before it is abandoned.
const GPU_WAIT: Duration = Duration::from_secs(1);
/// Client buffers kept imported per surface: a swapchain of up to four, and
/// the buffers of the size it is being resized away from.
#[cfg(any(test, target_os = "linux"))]
const IMPORTS_PER_SURFACE: usize = 8;
/// Buffer releases summarised per `CLIENT-BUFFER-RELEASE` line: about ten
/// seconds of one 60 Hz client.
const RELEASE_SUMMARY_EVERY: usize = 600;

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
    #[cfg(target_os = "linux")]
    probe_sampleable(device);
    *PRESENT_DEVICE.lock_recover() = Some(PresentDevice {
        device: device.clone(),
        queue: queue.clone(),
        generation,
    });
    generation
}

/// The client buffer layouts the presenting device was confirmed to sample,
/// and a version that changes whenever the confirmation does.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct Sampleable {
    version: u64,
    formats: Vec<smithay::backend::allocator::Format>,
}

#[cfg(target_os = "linux")]
static SAMPLEABLE: Mutex<Sampleable> = Mutex::new(Sampleable {
    version: 0,
    formats: Vec::new(),
});

/// The 8-bit RGB formats a client frame is imported in on the GPU
/// ([`import_dmabuf_on`]).
#[cfg(target_os = "linux")]
const GPU_IMPORT_FOURCCS: [smithay::backend::allocator::Fourcc; 4] = {
    use smithay::backend::allocator::Fourcc;
    [
        Fourcc::Argb8888,
        Fourcc::Xrgb8888,
        Fourcc::Abgr8888,
        Fourcc::Xbgr8888,
    ]
};

/// Ask `device` which modifiers it can sample for each GPU-imported format,
/// and keep the answer for [`sampleable`] and the dmabuf feedback.
#[cfg(target_os = "linux")]
fn probe_sampleable(device: &wgpu::Device) {
    use smithay::backend::allocator::{Format, Modifier};
    let formats: Vec<Format> = GPU_IMPORT_FOURCCS
        .into_iter()
        .flat_map(|code| {
            ui::dmabuf_import::sampleable_modifiers(device, code as u32)
                .into_iter()
                .map(move |modifier| Format {
                    code,
                    modifier: Modifier::from(modifier),
                })
        })
        .collect();
    let mut known = SAMPLEABLE.lock_recover();
    if known.formats != formats {
        known.version = known.version.wrapping_add(1);
        eprintln!(
            "tilo compositor: the GPU samples {} client dma-buf layouts: {}",
            formats.len(),
            formats
                .iter()
                .map(|format| format!("{:?}:{:#x}", format.code, u64::from(format.modifier)))
                .collect::<Vec<_>>()
                .join(" ")
        );
        known.formats = formats;
    }
}

/// The layouts clients are told the presenting device samples, with a version
/// that changes when they do (`0` until a device is published).
///
/// Every layout the probe confirmed, the Qualcomm compressed (UBWC) ones
/// included. Sampling a client's compressed buffer used to read what the
/// buffer held a frame or two earlier, because the Vulkan import did not
/// wait for the client's GPU writes; each commit's import now makes the GPU
/// wait for them ([`ui::dmabuf_import::wait_for_writes`]), which
/// `a_gpu_client_writing_a_compressed_buffer_is_sampled_exactly_for_fifty_frames`
/// checks pixel for pixel.
#[cfg(target_os = "linux")]
pub fn sampleable_formats() -> (u64, Vec<smithay::backend::allocator::Format>) {
    let known = SAMPLEABLE.lock_recover();
    (known.version, known.formats.clone())
}

/// Whether the presenting device was confirmed to sample a client buffer of
/// `code` laid out with `modifier`.
#[cfg(target_os = "linux")]
pub fn sampleable(
    code: smithay::backend::allocator::Fourcc,
    modifier: smithay::backend::allocator::Modifier,
) -> bool {
    SAMPLEABLE
        .lock_recover()
        .formats
        .iter()
        .any(|format| format.code == code && format.modifier == modifier)
}

#[cfg(target_os = "linux")]
fn present_device() -> Option<PresentDevice> {
    PRESENT_DEVICE.lock_recover().clone()
}

/// GPU-to-CPU readbacks of client frames since the process started. A frame
/// that is drawn from its import never adds to it; only a screenshot, a
/// preview, a stream or a renderer on another device asks for pixels.
static READBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    /// Readbacks made by this thread: the global count is shared with every
    /// test running beside it.
    static THREAD_READBACKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Client-buffer readbacks to the CPU since the process started, on every
/// route: reading a [`GpuFrame`] back, and the EGL importer's fallback.
pub fn readback_count() -> u64 {
    let gl = {
        #[cfg(target_os = "linux")]
        {
            crate::compositor::gpu_import::readback_count()
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    };
    READBACKS
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_add(gl)
}

/// Client dma-bufs imported as Vulkan images since the process started: one
/// per buffer, not one per commit.
fn import_total() -> u64 {
    #[cfg(target_os = "linux")]
    {
        IMPORTS.lock_recover().imports
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// A region of a frame: `x, y, width, height` in its pixels.
type Crop = (u32, u32, u32, u32);

/// Gives a client its buffer back (`wl_buffer.release`). Run once nothing
/// reads the buffer any more; see the module doc.
pub struct BufferLease(Box<dyn FnOnce() + Send>);

impl BufferLease {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self(Box::new(release))
    }
}

impl std::fmt::Debug for BufferLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BufferLease")
    }
}

/// One client frame: the client's buffer as imported on the presenting
/// device, premultiplied like the buffer. Any 8-bit RGB format; `X` formats
/// are drawn and read back opaque.
pub struct GpuFrame {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The presenting device it was made on.
    generation: u64,
    /// The imported buffer, read in place.
    texture: wgpu::Texture,
    /// The buffer's alpha channel is padding.
    opaque: bool,
    width: u32,
    height: u32,
    /// The surface it belongs to.
    surface: u64,
    /// The pixels, once something read them back.
    rgba: OnceLock<Option<Arc<Vec<u8>>>>,
    /// The image drawn for a crop of this frame, reused while the crop holds.
    image: Mutex<Option<(Crop, ImageData)>>,
    /// What gives the client's buffer back once this frame is gone.
    leases: Mutex<Vec<BufferLease>>,
    /// When a newer frame of the surface replaced it.
    superseded: OnceLock<Instant>,
    /// The client's buffer itself, for a display that can scan it out.
    #[cfg(target_os = "linux")]
    dmabuf: Option<smithay::backend::allocator::dmabuf::Dmabuf>,
    /// The client's writes this commit's frame waits for on the GPU
    /// ([`ui::dmabuf_import::wait_for_writes`]); kept until a submission made
    /// after the frame went completed.
    #[cfg(target_os = "linux")]
    write_fence: Option<ui::dmabuf_import::WriteFence>,
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

impl Drop for GpuFrame {
    fn drop(&mut self) {
        let leases = std::mem::take(
            self.leases
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        #[cfg(target_os = "linux")]
        let write_fence = self.write_fence.take();
        #[cfg(not(target_os = "linux"))]
        let write_fence: Option<()> = None;
        if leases.is_empty() && write_fence.is_none() {
            return;
        }
        // The queue runs in order: once this marker completes, so did every
        // render and readback submitted while the frame could still be read,
        // and every wait for the client's writes.
        let marker = self.queue.submit(std::iter::empty::<wgpu::CommandBuffer>());
        retire(Retiring {
            device: self.device.clone(),
            marker,
            leases,
            superseded: self.superseded.get().copied(),
            #[cfg(target_os = "linux")]
            write_fence,
        });
    }
}

impl GpuFrame {
    #[cfg(any(test, target_os = "linux"))]
    fn borrowing(
        present: &PresentDevice,
        texture: wgpu::Texture,
        opaque: bool,
        surface: u64,
    ) -> Self {
        Self {
            device: present.device.clone(),
            queue: present.queue.clone(),
            generation: present.generation,
            width: texture.width(),
            height: texture.height(),
            texture,
            opaque,
            surface,
            rgba: OnceLock::new(),
            image: Mutex::new(None),
            leases: Mutex::new(Vec::new()),
            superseded: OnceLock::new(),
            #[cfg(target_os = "linux")]
            dmabuf: None,
            #[cfg(target_os = "linux")]
            write_fence: None,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The buffer's alpha channel is padding: drawn, it covers what is below.
    pub fn opaque(&self) -> bool {
        self.opaque
    }

    /// The client's buffer, when the frame was imported from one. A display
    /// scanning it out holds this frame — and so the buffer — until the page
    /// flip that replaces it retires.
    #[cfg(target_os = "linux")]
    pub fn dmabuf(&self) -> Option<&smithay::backend::allocator::dmabuf::Dmabuf> {
        self.dmabuf.as_ref()
    }

    /// Give the client's buffer back through `lease` once this frame is gone
    /// and the GPU finished reading it.
    pub fn hold(&self, lease: BufferLease) {
        self.leases.lock_recover().push(lease);
    }

    /// A newer frame of the surface replaced this one; the buffer's release
    /// latency is measured from here.
    pub fn note_superseded(&self) {
        let _ = self.superseded.set(Instant::now());
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
        let mut cached = self.image.lock_recover();
        if let Some((at, image)) = cached.as_ref()
            && *at == crop
        {
            return image.clone();
        }
        // Wayland `ARGB8888` is premultiplied; `X` formats are drawn opaque.
        let image = ui::raster::rgba8_premultiplied(Vec::<u8>::new(), crop.2, crop.3);
        registry().register(&image, self, crop);
        *cached = Some((crop, image.clone()));
        image
    }
}

/// Bytes of `wl_shm` pixels written into client-frame textures since the
/// process started ([`UploadRing::upload`]); the key-latency harness reports
/// what each frame uploaded from it.
static SHM_UPLOAD_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bytes of `wl_shm` pixels uploaded to client-frame textures since the
/// process started.
pub fn shm_upload_bytes() -> u64 {
    SHM_UPLOAD_BYTES.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Bytes of `wl_shm` pixels this thread wrote into client-frame textures
    /// ([`UploadRing::upload`]).
    static THREAD_SHM_UPLOAD_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn thread_shm_upload_bytes() -> u64 {
    THREAD_SHM_UPLOAD_BYTES.with(std::cell::Cell::get)
}

/// A rectangle of a client frame, in its pixels: `(x, y, width, height)`.
#[cfg(target_os = "linux")]
pub type FrameRegion = (u32, u32, u32, u32);

/// Regions a texture of an [`UploadRing`] may lack before it is written
/// whole instead: past this many, one whole upload is cheaper to track.
#[cfg(target_os = "linux")]
const MAX_STALE_REGIONS: usize = 128;

/// One texture of an [`UploadRing`].
#[cfg(target_os = "linux")]
struct UploadSlot {
    texture: wgpu::Texture,
    /// The frame drawn from it last; the texture is written again only once
    /// nothing holds that frame (the store replaced it and every queued frame
    /// that draws it was presented).
    frame: Weak<GpuFrame>,
    /// What the texture lacks against the newest frame: the regions written
    /// into the other textures since it was last written; `None`, all of it.
    stale: Option<Vec<FrameRegion>>,
}

/// The textures a `wl_shm` surface's frames are drawn from on the presenting
/// device, so a commit uploads only what changed.
///
/// The SHM worker keeps a surface's pixels on the CPU and lays each commit's
/// damage over them; this ring then writes into a texture no queued frame
/// draws any more only the rows that texture lacks: the damage of this commit
/// and of the commits since it was last written. A texture still drawn by a
/// frame on its way to the screen is never written, so every frame shows the
/// pixels of its own commit. A new size or presenting device starts over.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub struct UploadRing {
    generation: u64,
    size: (u32, u32),
    slots: Vec<UploadSlot>,
}

#[cfg(target_os = "linux")]
impl UploadRing {
    /// Upload `pixels`, a tightly packed premultiplied RGBA frame of `size`
    /// that differs from the previous one uploaded through this ring only in
    /// `changed` (`None`: anywhere), and return the frame drawn from it.
    /// `None` when there is no presenting device; the frame is then drawn
    /// from its CPU pixels.
    pub fn upload(
        &mut self,
        surface: u64,
        pixels: &Arc<Vec<u8>>,
        size: (u32, u32),
        changed: Option<&[FrameRegion]>,
    ) -> Option<Arc<GpuFrame>> {
        let present = present_device()?;
        let expected = (size.0 as usize)
            .saturating_mul(size.1 as usize)
            .saturating_mul(4);
        if size.0 == 0 || size.1 == 0 || pixels.len() != expected {
            return None;
        }
        if self.generation != present.generation || self.size != size {
            *self = Self {
                generation: present.generation,
                size,
                slots: Vec::new(),
            };
        }
        let changed: Option<Vec<FrameRegion>> = changed.map(<[_]>::to_vec);
        let index = self.free_slot(&present.device);
        // Every texture but the one written lacks what changed.
        for (other, slot) in self.slots.iter_mut().enumerate() {
            if other == index {
                continue;
            }
            slot.stale = match (slot.stale.take(), changed.as_ref()) {
                (Some(mut stale), Some(changed)) => {
                    stale.extend_from_slice(changed);
                    (stale.len() <= MAX_STALE_REGIONS).then_some(stale)
                }
                _ => None,
            };
        }
        let slot = self.slots.get_mut(index)?;
        let write = match (slot.stale.take(), changed) {
            (Some(mut stale), Some(changed)) => {
                stale.extend(changed);
                Some(stale)
            }
            _ => None,
        };
        // Written below: up to date with this frame.
        slot.stale = Some(Vec::new());
        let whole = [(0, 0, size.0, size.1)];
        write_regions(
            &present.queue,
            &slot.texture,
            pixels,
            size,
            write.as_deref().unwrap_or(&whole),
        );
        let frame = GpuFrame::borrowing(&present, slot.texture.clone(), false, surface);
        // The CPU pixels are this frame's: a screenshot or another device
        // reads them instead of the texture.
        let _ = frame.rgba.set(Some(Arc::clone(pixels)));
        let frame = Arc::new(frame);
        slot.frame = Arc::downgrade(&frame);
        Some(frame)
    }

    /// A texture no frame draws any more, made when every one is still drawn.
    fn free_slot(&mut self, device: &wgpu::Device) -> usize {
        if let Some(free) = self
            .slots
            .iter()
            .position(|slot| slot.frame.strong_count() == 0)
        {
            return free;
        }
        let (width, height) = self.size;
        self.slots.push(UploadSlot {
            texture: device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shm-client-frame"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }),
            frame: Weak::new(),
            stale: None,
        });
        self.slots.len().saturating_sub(1)
    }
}

/// Write `regions` of `pixels`, a tightly packed RGBA frame of `size`, into
/// the same place of `texture`.
#[cfg(target_os = "linux")]
fn write_regions(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    pixels: &[u8],
    size: (u32, u32),
    regions: &[FrameRegion],
) {
    for &(x, y, width, height) in regions {
        let right = x.saturating_add(width).min(size.0);
        let bottom = y.saturating_add(height).min(size.1);
        if x >= right || y >= bottom {
            continue;
        }
        let (width, height) = (right.saturating_sub(x), bottom.saturating_sub(y));
        let offset = u64::from(y)
            .saturating_mul(u64::from(size.0))
            .saturating_add(u64::from(x))
            .saturating_mul(4);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset,
                bytes_per_row: Some(size.0.saturating_mul(4)),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let bytes = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4);
        SHM_UPLOAD_BYTES.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        THREAD_SHM_UPLOAD_BYTES.with(|count| count.set(count.get().saturating_add(bytes)));
    }
}

/// A frame's leases, waiting for the GPU marker submitted when it went.
struct Retiring {
    device: wgpu::Device,
    marker: wgpu::SubmissionIndex,
    leases: Vec<BufferLease>,
    superseded: Option<Instant>,
    /// Destroyed once the marker completed: no submission still waits on it.
    #[cfg(target_os = "linux")]
    write_fence: Option<ui::dmabuf_import::WriteFence>,
}

impl Retiring {
    fn run(self) {
        let done = self
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(self.marker),
                timeout: Some(GPU_WAIT),
            })
            .is_ok();
        if !done {
            // A lost or hung device never finishes; nothing reads the buffer
            // through it any more either way.
            tracing::warn!(
                target: "shell::compositor::foreign_capture",
                "the GPU did not finish with a client buffer in time; giving it back"
            );
        }
        if let Some(superseded) = self.superseded {
            note_release_latency(superseded.elapsed());
        }
        for lease in self.leases {
            (lease.0)();
        }
        #[cfg(target_os = "linux")]
        drop(self.write_fence);
    }
}

/// Hand `retiring` to the thread that gives buffers back, in the order their
/// frames went. Without that thread, wait here: a buffer is never given back
/// while the GPU may still read it.
fn retire(retiring: Retiring) {
    type Retirer = Option<Mutex<std::sync::mpsc::Sender<Retiring>>>;
    static RETIRER: OnceLock<Retirer> = OnceLock::new();
    let sender = RETIRER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<Retiring>();
        std::thread::Builder::new()
            .name("tilo-buffer-retire".into())
            .spawn(move || {
                while let Ok(retiring) = receiver.recv() {
                    retiring.run();
                }
            })
            .map_err(|error| {
                tracing::warn!(%error, "could not start the client buffer release thread");
            })
            .ok()
            .map(|_| Mutex::new(sender))
    });
    let unsent = match sender {
        Some(sender) => sender
            .lock_recover()
            .send(retiring)
            .err()
            .map(|std::sync::mpsc::SendError(unsent)| unsent),
        None => Some(retiring),
    };
    if let Some(retiring) = unsent {
        retiring.run();
    }
}

/// How long client buffers were held after a newer frame replaced them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReleaseLatency {
    /// Buffers given back since the process started.
    pub releases: u64,
    /// Over the latest window of up to [`RELEASE_SUMMARY_EVERY`] releases.
    pub p50: Duration,
    pub p99: Duration,
    pub max: Duration,
}

struct ReleaseStats {
    window: Vec<Duration>,
    latest: ReleaseLatency,
}

static RELEASES: Mutex<ReleaseStats> = Mutex::new(ReleaseStats {
    window: Vec::new(),
    latest: ReleaseLatency {
        releases: 0,
        p50: Duration::ZERO,
        p99: Duration::ZERO,
        max: Duration::ZERO,
    },
});

/// The latency summary of `window` (sorted here).
fn summarise(window: &mut [Duration], releases: u64) -> ReleaseLatency {
    window.sort_unstable();
    let at = |permille: usize| {
        let index = window
            .len()
            .saturating_sub(1)
            .saturating_mul(permille)
            .checked_div(1000)
            .unwrap_or(0);
        window.get(index).copied().unwrap_or_default()
    };
    ReleaseLatency {
        releases,
        p50: at(500),
        p99: at(990),
        max: window.last().copied().unwrap_or_default(),
    }
}

fn note_release_latency(held: Duration) {
    let mut stats = RELEASES.lock_recover();
    stats.window.push(held);
    let releases = stats.latest.releases.saturating_add(1);
    let mut window = std::mem::take(&mut stats.window);
    stats.latest = summarise(&mut window, releases);
    if window.len() < RELEASE_SUMMARY_EVERY {
        stats.window = window;
        return;
    }
    let latest = stats.latest;
    drop(stats);
    eprintln!(
        "CLIENT-BUFFER-RELEASE releases={} window={} p50_us={} p99_us={} max_us={} \
         client_readbacks={} client_imports={}",
        latest.releases,
        window.len(),
        latest.p50.as_micros(),
        latest.p99.as_micros(),
        latest.max.as_micros(),
        readback_count(),
        import_total(),
    );
}

/// Client buffer release latency — from a newer frame replacing the buffer's
/// frame to the buffer going back — over the latest window.
pub fn release_latency() -> ReleaseLatency {
    RELEASES.lock_recover().latest
}

struct Registered {
    image: ImageData,
    /// Weak: an image is drawable exactly as long as something holds its
    /// frame — the store while it is current, a queued frame that draws it,
    /// a readback.
    frame: Weak<GpuFrame>,
    surface: u64,
    crop: Crop,
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
    fn register(&self, image: &ImageData, frame: &Arc<GpuFrame>, crop: Crop) {
        let mut images = self.images.lock_recover();
        images.retain(|_, entry| entry.frame.strong_count() > 0);
        images.insert(
            image.data.id(),
            Registered {
                image: image.clone(),
                frame: Arc::downgrade(frame),
                surface: frame.surface,
                crop,
            },
        );
    }

    /// Forget every image of `surface`: it was destroyed.
    fn forget_surface(&self, surface: u64) {
        self.images
            .lock_recover()
            .retain(|_, entry| entry.surface != surface);
    }

    /// The live frame image `id` draws, and its crop.
    fn frame(&self, id: u64) -> Option<(Arc<GpuFrame>, Crop)> {
        self.images
            .lock_recover()
            .get(&id)
            .and_then(|entry| Some((entry.frame.upgrade()?, entry.crop)))
    }
}

/// The client frame `image` draws, when it draws one whole: the image a
/// [`crate::shell::scanout_candidate::ScanoutCandidate`] names. Alive as long
/// as the store or a queued frame holds it.
pub fn client_frame(image: &ImageData) -> Option<Arc<GpuFrame>> {
    // Client frames carry no CPU pixels; every other image does.
    if !image.data.is_empty() {
        return None;
    }
    let (frame, crop) = registry().frame(image.data.id())?;
    (crop == (0, 0, frame.width, frame.height)).then_some(frame)
}

/// Stop drawing `surface`'s GPU frames once it is gone, and drop the imports
/// of its buffers. A frame something still holds (a queued frame drawing it)
/// gives its buffer back when it goes.
pub fn forget_surface(surface: u64) {
    registry().forget_surface(surface);
    #[cfg(target_os = "linux")]
    IMPORTS.lock_recover().forget_surface(surface);
}

/// The client frames a queued frame draws, held until it was presented: the
/// buffers behind them are not given back while it can still be rendered.
#[derive(Default)]
pub struct FramePins(Vec<Arc<GpuFrame>>);

impl FramePins {
    /// Pin every client frame `scene` draws.
    pub fn add_scene(&mut self, scene: &vello::Scene) {
        let registry = registry();
        for patch in &scene.encoding().resources.patches {
            let vello_encoding::Patch::Image { image, .. } = patch else {
                continue;
            };
            // Client frames carry no CPU pixels; every other image does.
            if !image.data.is_empty() {
                continue;
            }
            let Some((frame, _)) = registry.frame(image.data.id()) else {
                continue;
            };
            if !self.0.iter().any(|pinned| Arc::ptr_eq(pinned, &frame)) {
                self.0.push(frame);
            }
        }
    }

    /// Client frames pinned.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for FramePins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FramePins").field(&self.0.len()).finish()
    }
}

/// The client frames a renderer draws from their textures, by image id.
#[derive(Default)]
pub struct AppliedFrames {
    applied: HashMap<u64, ImageData>,
}

/// Point `renderer`, on the presenting device of `generation`, at the textures
/// of every live frame made there, and stop drawing the frames that went
/// since the last call. A renderer on any other device (`None`) draws them
/// through the external image source.
pub fn apply(renderer: &mut vello::Renderer, generation: Option<u64>, applied: &mut AppliedFrames) {
    let images = registry().images.lock_recover();
    applied.applied.retain(|id, image| {
        let live = images
            .get(id)
            .is_some_and(|entry| entry.frame.strong_count() > 0);
        if !live {
            renderer.draw_image_from(image, None);
        }
        live
    });
    for (id, entry) in images.iter() {
        if applied.applied.contains_key(id) {
            continue;
        }
        let Some(frame) = entry.frame.upgrade() else {
            continue;
        };
        if Some(frame.generation) != generation {
            continue;
        }
        renderer.draw_image_from(
            &entry.image,
            Some(vello::ImageSource {
                texture: frame.texture.clone(),
                origin: [entry.crop.0, entry.crop.1],
                opaque: frame.opaque,
            }),
        );
        applied.applied.insert(*id, entry.image.clone());
    }
}

/// The vello external image source: a registered image's pixels, read back
/// for a renderer that cannot draw its texture.
fn external_pixels(id: u64) -> Option<Vec<u8>> {
    let (frame, crop) = registry().frame(id)?;
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

/// Read `frame` back as tightly packed RGBA: a `BGRA` buffer is swizzled and
/// an `X` format made opaque, as the presenting renderer draws them.
fn read_back(frame: &GpuFrame) -> Result<Vec<u8>, String> {
    READBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    THREAD_READBACKS.with(|count| count.set(count.get().saturating_add(1)));
    copy_back(frame)
}

/// [`read_back`]'s copy, not counted: for a texture that is not a client's.
fn copy_back(frame: &GpuFrame) -> Result<Vec<u8>, String> {
    let bgra = match frame.texture.format() {
        wgpu::TextureFormat::Rgba8Unorm => false,
        wgpu::TextureFormat::Bgra8Unorm => true,
        other => return Err(format!("{other:?} is not read back")),
    };
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
    for texel in pixels.chunks_exact_mut(4) {
        if bgra {
            texel.swap(0, 2);
        }
        if frame.opaque
            && let Some(alpha) = texel.get_mut(3)
        {
            *alpha = u8::MAX;
        }
    }
    Ok(pixels)
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
    let present = PresentDevice {
        device: device.clone(),
        queue: queue.clone(),
        generation,
    };
    let texture = test_buffer(
        &present,
        wgpu::TextureFormat::Rgba8Unorm,
        (width, height),
        pixels,
    );
    Arc::new(GpuFrame::borrowing(&present, texture, false, surface))
}

/// A texture as the import of a client buffer is: sampled and copied from,
/// never written by the compositor.
#[cfg(test)]
fn test_buffer(
    present: &PresentDevice,
    format: wgpu::TextureFormat,
    (width, height): (u32, u32),
    pixels: &[u8],
) -> wgpu::Texture {
    let texture = present.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("client-frame-test-buffer"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    present.queue.write_texture(
        texture.as_image_copy(),
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
    texture
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
    fn get_or_import<E>(
        &mut self,
        (generation, surface): (u64, u64),
        buffer: K,
        import: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
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
static IMPORTS: Mutex<
    ImportCache<smithay::backend::allocator::dmabuf::WeakDmabuf, ui::dmabuf_import::DmabufImage>,
> = Mutex::new(ImportCache::new());

/// Why a client's dma-buf was not kept on the GPU, so it is read back on the
/// CPU instead. Typed so the compositor can log each reason once and a test
/// can name the one it expects.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportFallback {
    /// The presenting renderer has not opened its device yet.
    NoPresentingGpu,
    /// Not an 8-bit RGB format (10-bit, half float and YUV take the readback).
    NotEightBitRgb { fourcc: String },
    /// A format with more or fewer than one plane.
    PlaneCount { planes: usize },
    /// Vulkan could not import the buffer: no `VK_EXT_external_memory_dma_buf`,
    /// no `VK_EXT_image_drm_format_modifier` for a modifier or pitch the driver
    /// would not choose itself, or a driver refusal.
    Vulkan(ui::dmabuf_import::ImportError),
}

#[cfg(target_os = "linux")]
impl std::fmt::Display for ImportFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPresentingGpu => f.write_str("no presenting GPU yet"),
            Self::NotEightBitRgb { fourcc } => write!(f, "{fourcc} is not an 8-bit RGB format"),
            Self::PlaneCount { planes } => write!(f, "expected one plane, got {planes}"),
            Self::Vulkan(error) => write!(f, "{error}"),
        }
    }
}

/// Import a client's single-plane 8-bit RGB dma-buf on the presenting device,
/// or reuse its import from an earlier commit, as a frame drawn straight from
/// it. Nothing is copied or submitted here.
///
/// The caller makes sure the client's rendering into the buffer finished
/// first (its implicit fence signalled): Vulkan does not wait for it. Once the
/// frame can be drawn, the caller holds the buffer for it ([`GpuFrame::hold`]):
/// the client must not get it back while the frame lives.
///
/// `declared_opaque` is the commit's opaque region covering the whole
/// surface: an `ARGB`/`ABGR` buffer's alpha is then padding, and the frame is
/// [`GpuFrame::opaque`] like an `X` format's — drawn ignoring its alpha, and
/// eligible for scanout as its `X` twin.
///
/// # Errors
///
/// No presenting device yet, a format or layout the import cannot describe,
/// or a GPU failure. The caller falls back to reading the buffer on the CPU.
#[cfg(target_os = "linux")]
pub fn import_dmabuf(
    dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
    surface: u64,
    declared_opaque: bool,
) -> Result<Arc<GpuFrame>, ImportFallback> {
    let present = present_device().ok_or(ImportFallback::NoPresentingGpu)?;
    import_dmabuf_on(&present, dmabuf, surface, declared_opaque)
}

#[cfg(target_os = "linux")]
fn import_dmabuf_on(
    present: &PresentDevice,
    dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
    surface: u64,
    declared_opaque: bool,
) -> Result<Arc<GpuFrame>, ImportFallback> {
    use smithay::backend::allocator::{Buffer as _, Fourcc};
    use std::os::fd::AsFd as _;

    let fourcc = dmabuf.format().code;
    let opaque = match fourcc {
        Fourcc::Argb8888 | Fourcc::Abgr8888 => declared_opaque,
        Fourcc::Xrgb8888 | Fourcc::Xbgr8888 => true,
        other => {
            return Err(ImportFallback::NotEightBitRgb {
                fourcc: format!("{other:?}"),
            });
        }
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
        return Err(ImportFallback::PlaneCount {
            planes: handles.len(),
        });
    };
    let source = IMPORTS.lock_recover().get_or_import(
        (present.generation, surface),
        dmabuf.weak(),
        || {
            let planes = [ui::dmabuf_import::PlaneRef {
                fd: fd.as_fd(),
                offset,
                stride,
            }];
            ui::dmabuf_import::import_image(
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
            .map_err(ImportFallback::Vulkan)
        },
    )?;
    // A texture of the buffer for this commit: the producer wrote it again
    // since the last, so its first use acquires the image from the foreign
    // queue family.
    let source = source
        .texture(&present.device)
        .map_err(ImportFallback::Vulkan)?;
    let mut frame = GpuFrame::borrowing(present, source, opaque, surface);
    // The client may hand the buffer over before its GPU finished writing it
    // (implicit sync): every submission from here on waits for those writes
    // on the GPU, so no frame samples what the buffer held before.
    frame.write_fence =
        ui::dmabuf_import::wait_for_writes(&present.device, &present.queue, fd.as_fd())
            .map_err(ImportFallback::Vulkan)?;
    frame.dmabuf = Some(dmabuf.clone());
    Ok(Arc::new(frame))
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

    /// Clients are offered every layout the device was confirmed to sample,
    /// compressed ones included: each commit's import waits on the GPU for
    /// the client's writes, so sampling them reads what the client wrote.
    #[cfg(target_os = "linux")]
    #[test]
    fn every_sampleable_layout_is_advertised() {
        use smithay::backend::allocator::{Format, Fourcc, Modifier};
        let format = |code, modifier: u64| Format {
            code,
            modifier: Modifier::from(modifier),
        };
        let sampled = vec![
            format(Fourcc::Xrgb8888, 0),
            format(Fourcc::Xrgb8888, 0x0500_0000_0000_0001),
            format(Fourcc::Argb8888, 0x0500_0000_0000_0004),
        ];
        let version = {
            let mut known = SAMPLEABLE.lock_recover();
            known.version = known.version.wrapping_add(1);
            known.formats.clone_from(&sampled);
            known.version
        };
        assert_eq!(sampleable_formats(), (version, sampled));
    }

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

    /// A 3×2 client buffer of `format` holding `texel` everywhere, wrapped as
    /// the frame its import would be.
    fn client_frame(
        present: &PresentDevice,
        format: wgpu::TextureFormat,
        texel: [u8; 4],
        (opaque, surface): (bool, u64),
    ) -> Arc<GpuFrame> {
        let texture = test_buffer(present, format, (3, 2), &texel.repeat(6));
        Arc::new(GpuFrame::borrowing(present, texture, opaque, surface))
    }

    /// A `wl_shm` commit that damages one text line uploads that line, not
    /// the frame; a texture a queued frame still draws is never written, and
    /// one written again catches up with every commit it missed.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_shm_upload_writes_only_what_the_texture_lacks() {
        let Some(present) = present() else {
            eprintln!("no wgpu adapter; skipping");
            return;
        };
        *PRESENT_DEVICE.lock_recover() = Some(present);
        let (width, height) = (64u32, 48u32);
        let row = u64::from(width) * 4;
        let whole = row * u64::from(height);
        let grid = |lines: &[u32]| {
            let mut pixels = [20, 20, 20, 255].repeat((width * height) as usize);
            for &line in lines {
                for y in line * 4..line * 4 + 4 {
                    let start = (y * width * 4) as usize;
                    pixels[start..start + (width * 4) as usize]
                        .copy_from_slice(&[200, 200, 200, 255].repeat(width as usize));
                }
            }
            Arc::new(pixels)
        };
        let line = |n: u32| (0, n * 4, width, 4);
        let uploaded = |upload: &mut dyn FnMut() -> Arc<GpuFrame>| {
            let before = thread_shm_upload_bytes();
            let frame = upload();
            (frame, thread_shm_upload_bytes() - before)
        };
        let mut ring = UploadRing::default();
        let surface = 77;

        let first = grid(&[]);
        let (a, bytes) = uploaded(&mut || {
            ring.upload(surface, &first, (width, height), None)
                .expect("frame")
        });
        assert_eq!(bytes, whole, "the first frame is uploaded whole");

        // A queued frame still draws `a`: the next commit goes to another
        // texture, which has never been written.
        let second = grid(&[1]);
        let (b, bytes) = uploaded(&mut || {
            ring.upload(surface, &second, (width, height), Some(&[line(1)]))
                .expect("frame")
        });
        assert_eq!(bytes, whole, "a new texture is written whole");
        drop(a);

        // `a`'s texture is free again; it lacks line 1 and gets line 2.
        let third = grid(&[1, 2]);
        let (c, bytes) = uploaded(&mut || {
            ring.upload(surface, &third, (width, height), Some(&[line(2)]))
                .expect("frame")
        });
        assert_eq!(bytes, 2 * 4 * row, "the missed line and the typed one");
        assert_eq!(
            copy_back(&c).expect("readback"),
            *third,
            "the rewritten texture shows exactly the newest frame"
        );
        drop((b, c));

        // Steady typing: a free texture already up to date gets only the
        // typed line.
        let fourth = grid(&[1, 2, 3]);
        let (d, bytes) = uploaded(&mut || {
            ring.upload(surface, &fourth, (width, height), Some(&[line(3)]))
                .expect("frame")
        });
        assert_eq!(bytes, 4 * row, "one typed line");
        assert_eq!(copy_back(&d).expect("readback"), *fourth);
        assert_eq!(
            d.rgba().as_deref(),
            Some(&*fourth),
            "the CPU pixels go with the frame: nothing is read back"
        );
        drop(d);

        // A new size starts over.
        let resized = Arc::new([20, 20, 20, 255].repeat((width * (height + 4)) as usize));
        let (_, bytes) = uploaded(&mut || {
            ring.upload(surface, &resized, (width, height + 4), Some(&[line(0)]))
                .expect("frame")
        });
        assert_eq!(
            bytes,
            row * u64::from(height + 4),
            "a resized frame is uploaded whole"
        );
    }

    #[test]
    fn client_frames_are_read_in_place_and_read_back_only_on_demand() {
        let Some(present) = present() else {
            eprintln!("no wgpu adapter; skipping");
            return;
        };
        // Phase `a_bgra_buffer_reads_back_as_rgba_with_its_alpha`.
        {
            let frame = client_frame(
                &present,
                wgpu::TextureFormat::Bgra8Unorm,
                [10, 20, 30, 128],
                (false, 1),
            );
            assert!(
                frame.rgba.get().is_none(),
                "nothing is read back until asked"
            );
            let pixels = frame.rgba().expect("readback");
            assert_eq!(pixels.len(), 3 * 2 * 4);
            assert_eq!(&pixels[..4], &[30, 20, 10, 128]);
        }
        // Phase `an_x_format_reads_back_opaque`.
        {
            let frame = client_frame(
                &present,
                wgpu::TextureFormat::Bgra8Unorm,
                [3, 2, 1, 0],
                (true, 2),
            );
            assert_eq!(&frame.rgba().expect("readback")[..4], &[1, 2, 3, 255]);
        }
        // Phase `a_renderer_without_the_texture_gets_the_cropped_pixels`.
        {
            let frame = client_frame(
                &present,
                wgpu::TextureFormat::Rgba8Unorm,
                [9, 8, 7, 255],
                (false, 3),
            );
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
        // Phase `an_image_is_drawable_exactly_while_its_frame_is_held`.
        {
            let frame = client_frame(
                &present,
                wgpu::TextureFormat::Rgba8Unorm,
                [1, 1, 1, 255],
                (false, 4),
            );
            let image = frame.image((0, 0, 3, 2));
            assert!(external_pixels(image.data.id()).is_some());
            drop(frame);
            assert_eq!(
                external_pixels(image.data.id()),
                None,
                "no grace period: a frame nobody holds is not drawn"
            );
        }
    }

    /// The renderer target and its pixels, read back as RGBA.
    fn render_and_read(
        present: &PresentDevice,
        renderer: &mut vello::Renderer,
        scene: &vello::Scene,
        (width, height): (u32, u32),
    ) -> Vec<u8> {
        let target = present.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client-frame-test-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        renderer
            .render_to_texture(
                &present.device,
                &present.queue,
                scene,
                &view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::BLACK,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("render");
        let frame = GpuFrame::borrowing(present, target, false, u64::MAX);
        // Not a client's buffer, so not counted as a client readback.
        copy_back(&frame).expect("target readback")
    }

    fn assert_near(got: &[u8], want: &[u8]) {
        assert!(
            got.iter().zip(want).all(|(g, w)| g.abs_diff(*w) <= 1),
            "{got:?} is not {want:?}"
        );
    }

    /// One client buffer reaches the panel with one GPU copy: the presenting
    /// renderer draws it into its atlas from the import itself, converting
    /// `BGRA` and `X` formats on the way, and draws it from the atlas after.
    #[test]
    fn a_client_frame_is_drawn_with_one_copy() {
        let Some(present) = present() else {
            eprintln!("no wgpu adapter; skipping");
            return;
        };
        let mut renderer = vello::Renderer::new(
            &present.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .expect("renderer");
        let mut applied = AppliedFrames::default();
        let draw = |frame: &Arc<GpuFrame>| {
            let mut scene = vello::Scene::new();
            scene.fill(
                vello::peniko::Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                &vello::peniko::ImageBrush::new(frame.image((0, 0, 3, 2))),
                None,
                &vello::kurbo::Rect::new(0.0, 0.0, 3.0, 2.0),
            );
            scene
        };
        // Phase `a_bgra_x_buffer_is_converted_by_the_one_copy`.
        let frame = client_frame(
            &present,
            wgpu::TextureFormat::Bgra8Unorm,
            [0x30, 0x20, 0x10, 0x00],
            (true, 10),
        );
        let scene = draw(&frame);
        apply(&mut renderer, Some(present.generation), &mut applied);
        let before = renderer.image_source_writes();
        let pixels = render_and_read(&present, &mut renderer, &scene, (3, 2));
        assert_near(&pixels[..4], &[0x10, 0x20, 0x30, 0xFF]);
        let after = renderer.image_source_writes();
        assert_eq!(
            after.0.saturating_sub(before.0),
            1,
            "one copy, from the client's buffer"
        );
        assert_eq!(
            after.1.saturating_sub(before.1),
            3 * 2 * 4,
            "of the frame's bytes"
        );
        // Phase `the_next_render_draws_it_from_the_atlas`.
        apply(&mut renderer, Some(present.generation), &mut applied);
        let again = render_and_read(&present, &mut renderer, &scene, (3, 2));
        assert_eq!(again, pixels);
        assert_eq!(
            renderer.image_source_writes(),
            after,
            "a frame already in the atlas is not copied again"
        );
        // Phase `a_premultiplied_buffer_keeps_its_alpha`.
        let translucent = client_frame(
            &present,
            wgpu::TextureFormat::Rgba8Unorm,
            [0x40, 0x00, 0x00, 0x80],
            (false, 11),
        );
        let scene = draw(&translucent);
        apply(&mut renderer, Some(present.generation), &mut applied);
        let pixels = render_and_read(&present, &mut renderer, &scene, (3, 2));
        // Half-covering red over the black base.
        assert_near(&pixels[..4], &[0x40, 0x00, 0x00, 0xFF]);
        // Phase `a_frame_that_went_is_no_longer_drawn_from_its_buffer`.
        drop(frame);
        apply(&mut renderer, Some(present.generation), &mut applied);
        assert_eq!(
            applied.applied.len(),
            1,
            "only the live frame stays applied"
        );
    }

    /// A client's dma-buf, imported and drawn, is never read back to the CPU,
    /// and a buffer already imported is not imported again.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one GPU fixture: allocate, import, draw and count readbacks in order"
    )]
    fn a_client_dmabuf_is_drawn_from_its_import_without_a_cpu_readback() {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::allocator::dmabuf::AsDmabuf as _;
        use smithay::backend::allocator::gbm::{GbmBuffer, GbmBufferFlags, GbmDevice};

        const WIDTH: u32 = 1000;
        const HEIGHT: u32 = 64;
        /// A byte that varies along an axis, so a sheared picture is caught.
        fn column(at: usize) -> u8 {
            u8::try_from(at % 251).unwrap_or(0)
        }
        // A check that cannot observe its subject fails: it must not pass on a
        // machine without the GPU, render node or extensions it exercises.
        // A client buffer lives on a render node; without one (a CI runner
        // with a software adapter only) there is nothing to import.
        let Ok(node) = crate::compositor::render_node::probe() else {
            eprintln!("skipping: no advertisable render node on this machine");
            return;
        };
        let present = present().expect("this test needs a wgpu adapter");
        let has_extension = |wanted: &[u8]| {
            unsafe { present.device.as_hal::<wgpu::hal::api::Vulkan>() }.is_some_and(|hal| {
                hal.enabled_device_extensions()
                    .iter()
                    .any(|name| name.to_bytes() == wanted)
            })
        };
        assert!(
            has_extension(b"VK_EXT_external_memory_dma_buf"),
            "the device must import dma-bufs"
        );
        assert!(
            has_extension(b"VK_EXT_image_drm_format_modifier"),
            "the device must import a dma-buf with its explicit layout"
        );
        // A buffer whose row pitch is the allocator's own, whatever it is: the
        // explicit layout is what makes it importable.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node.path)
            .expect("open the render node");
        let device = GbmDevice::new(file).expect("gbm device");
        let (mut bo, implicit) = match device.create_buffer_object_with_modifiers2::<()>(
            WIDTH,
            HEIGHT,
            Fourcc::Xrgb8888,
            std::iter::once(smithay::backend::allocator::Modifier::Linear),
            GbmBufferFlags::RENDERING,
        ) {
            Ok(bo) => (bo, false),
            Err(_) => (
                device
                    .create_buffer_object::<()>(
                        WIDTH,
                        HEIGHT,
                        Fourcc::Xrgb8888,
                        GbmBufferFlags::RENDERING | GbmBufferFlags::LINEAR,
                    )
                    .expect("a linear XRGB8888 buffer"),
                true,
            ),
        };
        bo.map_mut(0, 0, WIDTH, HEIGHT, |map| {
            let stride = map.stride() as usize;
            let bytes = map.buffer_mut();
            for y in 0..HEIGHT as usize {
                for x in 0..WIDTH as usize {
                    // B, G, R, X in memory.
                    let at = y.saturating_mul(stride).saturating_add(x.saturating_mul(4));
                    bytes[at..at.saturating_add(4)].copy_from_slice(&[
                        column(x),
                        column(y),
                        0x80,
                        0,
                    ]);
                }
            }
        })
        .expect("map the buffer");
        let dmabuf = GbmBuffer::from_bo(bo, implicit)
            .export()
            .expect("export as a dmabuf");

        let mut renderer = vello::Renderer::new(
            &present.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .expect("renderer");
        let mut applied = AppliedFrames::default();
        let surface = 9_000;
        let readbacks = THREAD_READBACKS.with(std::cell::Cell::get);
        let readbacks_now = || THREAD_READBACKS.with(std::cell::Cell::get);
        let imports_of_surface = || {
            IMPORTS
                .lock_recover()
                .entries
                .iter()
                .filter(|entry| entry.surface == surface)
                .count()
        };

        // Phase `a_commit_imports_the_buffer_and_draws_it_with_no_readback`.
        let frame = import_dmabuf_on(&present, &dmabuf, surface, false).unwrap_or_else(|why| {
            panic!("a linear client buffer must import, not fall back: {why}")
        });
        let mut scene = vello::Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            vello::kurbo::Affine::IDENTITY,
            &vello::peniko::ImageBrush::new(frame.image((0, 0, WIDTH, HEIGHT))),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
        );
        apply(&mut renderer, Some(present.generation), &mut applied);
        let before = renderer.image_source_writes();
        let pixels = render_and_read(&present, &mut renderer, &scene, (WIDTH, HEIGHT));
        for (x, y) in [(0_usize, 0_usize), (1, 0), (7, 3), (999, 63), (500, 40)] {
            let at = y
                .saturating_mul(WIDTH as usize)
                .saturating_add(x)
                .saturating_mul(4);
            assert_near(
                &pixels[at..at.saturating_add(4)],
                &[0x80, column(y), column(x), 0xFF],
            );
        }
        assert_eq!(
            readbacks_now(),
            readbacks,
            "importing and drawing a client dma-buf reads nothing back to the CPU"
        );
        assert_eq!(imports_of_surface(), 1, "one Vulkan import for the buffer");
        let after = renderer.image_source_writes();
        assert_eq!(
            after.0.saturating_sub(before.0),
            1,
            "one GPU copy into the atlas"
        );

        // Phase `a_recommit_of_the_same_buffer_imports_and_uploads_nothing`.
        let again = import_dmabuf_on(&present, &dmabuf, surface, false).expect("cached import");
        assert_eq!(
            imports_of_surface(),
            1,
            "the buffer's later commits reuse its import"
        );
        assert_ne!(
            again.image((0, 0, WIDTH, HEIGHT)).data.id(),
            frame.image((0, 0, WIDTH, HEIGHT)).data.id(),
            "a new commit is a new frame; it only costs a GPU copy"
        );
        apply(&mut renderer, Some(present.generation), &mut applied);
        let _ = render_and_read(&present, &mut renderer, &scene, (WIDTH, HEIGHT));
        assert_eq!(
            renderer.image_source_writes(),
            after,
            "a frame already in the atlas is not copied again"
        );
        assert_eq!(readbacks_now(), readbacks, "still no CPU readback");

        // Phase `reading_pixels_back_is_counted`.
        let read = frame.rgba().expect("on-demand readback");
        assert_eq!(
            readbacks_now(),
            readbacks.saturating_add(1),
            "the counter sees a readback"
        );
        assert_eq!(&read[..4], &[0x80, 0, 0, 0xFF]);
        forget_surface(surface);
    }

    /// Every layout the device is probed to sample is one a buffer allocated
    /// with it imports as, and the probe lists layouts at all. Nothing is
    /// sampled: a tiled buffer is never written here, so only the import (an
    /// image and its memory) reaches the driver.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_buffer_allocated_with_a_probed_layout_imports_on_the_device() {
        use smithay::backend::allocator::dmabuf::AsDmabuf as _;
        use smithay::backend::allocator::gbm::{GbmBuffer, GbmBufferFlags, GbmDevice};
        use smithay::backend::allocator::{Fourcc, Modifier};

        const WIDTH: u32 = 256;
        const HEIGHT: u32 = 64;
        // A client buffer lives on a render node; without one (a CI runner
        // with a software adapter only) there is nothing to import.
        let Ok(node) = crate::compositor::render_node::probe() else {
            eprintln!("skipping: no advertisable render node on this machine");
            return;
        };
        let present = present().expect("this test needs a wgpu adapter");
        let probed =
            ui::dmabuf_import::sampleable_modifiers(&present.device, Fourcc::Xrgb8888 as u32);
        assert!(
            !probed.is_empty(),
            "the device lists no sampleable layout for XRGB8888; nothing can be advertised"
        );
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node.path)
            .expect("open the render node");
        let device = GbmDevice::new(file).expect("gbm device");
        let mut imported = Vec::new();
        for modifier in probed {
            // A layout the allocator cannot make is not one a client can send.
            let Ok(bo) = device.create_buffer_object_with_modifiers2::<()>(
                WIDTH,
                HEIGHT,
                Fourcc::Xrgb8888,
                std::iter::once(Modifier::from(modifier)),
                GbmBufferFlags::RENDERING,
            ) else {
                continue;
            };
            let dmabuf = GbmBuffer::from_bo(bo, false)
                .export()
                .expect("export as a dmabuf");
            let surface = 9_100 + imported.len() as u64;
            let frame = import_dmabuf_on(&present, &dmabuf, surface, false).unwrap_or_else(|why| {
                panic!("a buffer allocated with the probed layout {modifier:#x} must import: {why}")
            });
            drop(frame);
            forget_surface(surface);
            imported.push(modifier);
        }
        eprintln!("imported probed layouts: {imported:#x?}");
        assert!(
            !imported.is_empty(),
            "the allocator made none of the probed layouts, so none could be imported"
        );
    }

    /// A GPU client (EGL/GLES through GBM) writes a known pattern into a
    /// buffer allocated with the Qualcomm compressed layout and hands it over
    /// with no CPU wait, as a real client does: the kernel's implicit fence on
    /// the buffer is all that orders its writes before the compositor's
    /// reads. The compositor's Vulkan import (which acquires the image from
    /// the foreign queue family on first use) samples it into a frame, and the
    /// pixels read back must be the pattern. Fifty frames over two buffers,
    /// the pattern different each time, so a stale or half-written read shows.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one GPU fixture: a client writes, the compositor samples, fifty frames compared"
    )]
    fn a_gpu_client_writing_a_compressed_buffer_is_sampled_exactly_for_fifty_frames() {
        use smithay::backend::allocator::dmabuf::AsDmabuf as _;
        use smithay::backend::allocator::gbm::{GbmBuffer, GbmBufferFlags, GbmDevice};
        use smithay::backend::allocator::{Fourcc, Modifier};
        use smithay::backend::renderer::{Bind as _, Color32F, Frame as _, Renderer as _};
        use smithay::utils::{Rectangle, Transform};

        const COMPRESSED: u64 = 0x0500_0000_0000_0001;
        const WIDTH: u32 = 256;
        const HEIGHT: u32 = 128;
        const CELL: u32 = 16;
        const FRAMES: usize = 50;
        /// The colour of cell `(cx, cy)` in frame `frame`.
        fn cell(frame: usize, cx: u32, cy: u32) -> [u8; 3] {
            let byte = |value: usize| u8::try_from(value % 251).unwrap_or(0);
            [
                byte((cx as usize) * 15 + frame * 3),
                byte((cy as usize) * 31 + frame * 7),
                byte(frame * 5 + (cx * cy) as usize),
            ]
        }
        // A client buffer lives on a render node; without one (a CI runner
        // with a software adapter only) there is nothing to import.
        let Ok(node) = crate::compositor::render_node::probe() else {
            eprintln!("skipping: no advertisable render node on this machine");
            return;
        };
        let present = present().expect("this test needs a wgpu adapter");
        // The acquire this test covers needs the extension; without it the
        // barrier is not emitted and a pass would prove nothing about it.
        assert!(
            unsafe { present.device.as_hal::<wgpu::hal::api::Vulkan>() }.is_some_and(|hal| {
                hal.enabled_device_extensions()
                    .iter()
                    .any(|name| name.to_bytes() == b"VK_EXT_queue_family_foreign")
            }),
            "the device did not enable VK_EXT_queue_family_foreign"
        );
        assert!(
            ui::dmabuf_import::sampleable_modifiers(&present.device, Fourcc::Argb8888 as u32)
                .contains(&COMPRESSED),
            "the device does not list the compressed layout as sampleable"
        );
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node.path)
            .expect("open the render node");
        let device = GbmDevice::new(file).expect("gbm device");
        let mut buffers: Vec<_> = (0..2)
            .map(|_| {
                let bo = device
                    .create_buffer_object_with_modifiers2::<()>(
                        WIDTH,
                        HEIGHT,
                        Fourcc::Argb8888,
                        std::iter::once(Modifier::from(COMPRESSED)),
                        GbmBufferFlags::RENDERING,
                    )
                    .expect("a compressed ARGB8888 buffer");
                GbmBuffer::from_bo(bo, false)
                    .export()
                    .expect("export as a dmabuf")
            })
            .collect();
        let mut producer = crate::compositor::gpu_import::GpuImporter::new(&node.path)
            .expect("a GL context for the client on the render node");

        let mut renderer = vello::Renderer::new(
            &present.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .expect("renderer");
        let mut applied = AppliedFrames::default();
        let surface = 9_200;
        // The memory order of the rows the producer wrote: GL may store the
        // image upside down; decided on the first frame, then it must hold.
        let mut flipped: Option<bool> = None;
        let mut most_alive = 0;
        let (_, wraps_before, nanos_before) = ui::dmabuf_import::image_stats();
        let waits_before = ui::dmabuf_import::write_fence_waits();
        for frame_number in 0..FRAMES {
            let slot = frame_number % 2;
            let buffer = &mut buffers[slot];
            {
                let gl = producer.renderer_mut();
                let mut target = gl
                    .bind(buffer)
                    .expect("the client's GL cannot render into the compressed layout");
                let mut frame = gl
                    .render(
                        &mut target,
                        (WIDTH.cast_signed(), HEIGHT.cast_signed()).into(),
                        Transform::Normal,
                    )
                    .expect("begin the client's frame");
                for cy in 0..HEIGHT / CELL {
                    for cx in 0..WIDTH / CELL {
                        let [r, g, b] = cell(frame_number, cx, cy);
                        frame
                            .clear(
                                Color32F::new(
                                    f32::from(r) / 255.0,
                                    f32::from(g) / 255.0,
                                    f32::from(b) / 255.0,
                                    1.0,
                                ),
                                &[Rectangle::new(
                                    ((cx * CELL).cast_signed(), (cy * CELL).cast_signed()).into(),
                                    (CELL.cast_signed(), CELL.cast_signed()).into(),
                                )],
                            )
                            .expect("clear a cell");
                    }
                }
                // Deliberately not waited on: the client hands the buffer over
                // as soon as its commands are submitted.
                let _fence = frame.finish().expect("submit the client's frame");
            }
            let imported = import_dmabuf_on(&present, buffer, surface + slot as u64, false)
                .unwrap_or_else(|why| panic!("the compressed buffer must import: {why}"));
            let mut scene = vello::Scene::new();
            scene.fill(
                vello::peniko::Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                &vello::peniko::ImageBrush::new(imported.image((0, 0, WIDTH, HEIGHT))),
                None,
                &vello::kurbo::Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
            );
            apply(&mut renderer, Some(present.generation), &mut applied);
            let pixels = render_and_read(&present, &mut renderer, &scene, (WIDTH, HEIGHT));
            most_alive = most_alive.max(ui::dmabuf_import::image_stats().0);
            let expect = |x: u32, y: u32, upside_down: bool| {
                let row = if upside_down { HEIGHT - 1 - y } else { y };
                let [r, g, b] = cell(frame_number, x / CELL, row / CELL);
                [r, g, b, 0xFF]
            };
            let matches = |upside_down: bool| {
                (0..HEIGHT).all(|y| {
                    (0..WIDTH).all(|x| {
                        let at = (y as usize * WIDTH as usize + x as usize) * 4;
                        let got = &pixels[at..at + 4];
                        let want = expect(x, y, upside_down);
                        got.iter()
                            .zip(want)
                            .all(|(got, want)| got.abs_diff(want) <= 1)
                    })
                })
            };
            let upside_down = *flipped.get_or_insert_with(|| !matches(false) && matches(true));
            if !matches(upside_down) {
                let bad = (0..HEIGHT)
                    .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
                    .find(|&(x, y)| {
                        let at = (y as usize * WIDTH as usize + x as usize) * 4;
                        pixels[at..at + 4]
                            .iter()
                            .zip(expect(x, y, upside_down))
                            .any(|(got, want)| got.abs_diff(want) > 1)
                    })
                    .unwrap_or((0, 0));
                let at = (bad.1 as usize * WIDTH as usize + bad.0 as usize) * 4;
                panic!(
                    "frame {frame_number}: pixel {bad:?} is {:?}, the client wrote {:?}",
                    &pixels[at..at + 4],
                    expect(bad.0, bad.1, upside_down)
                );
            }
        }
        // Each commit's image lives until the GPU work that sampled it
        // retired (wgpu runs the destroy then), so the live count is the GPU's
        // backlog: it must not grow with the frames, and once everything has
        // retired and the frames are dropped, nothing is left.
        let (_, wraps, nanos) = ui::dmabuf_import::image_stats();
        let wrapped = wraps - wraps_before;
        assert_eq!(wrapped, FRAMES as u64, "one image per commit");
        eprintln!(
            "per-commit image: {} us each over {wrapped} commits; at most {most_alive} alive at once",
            (nanos - nanos_before) / wrapped / 1000
        );
        eprintln!(
            "{} of {FRAMES} commits were imported before the client's GPU finished them; \
             the frame waited for its writes on the GPU",
            ui::dmabuf_import::write_fence_waits() - waits_before
        );
        assert!(
            most_alive <= 4,
            "{most_alive} per-commit images alive at once over {FRAMES} frames: they are leaking"
        );
        drop(applied);
        let _ = present.device.poll(wgpu::PollType::wait_indefinitely());
        let alive = ui::dmabuf_import::image_stats().0;
        assert!(
            alive <= 2,
            "{alive} per-commit images still alive after the GPU work retired"
        );
        // Two buffers were imported once each; every commit after made only a
        // new image over its memory, which acquired from the foreign queue.
        let imports = IMPORTS
            .lock_recover()
            .entries
            .iter()
            .filter(|entry| entry.surface == surface || entry.surface == surface + 1)
            .count();
        assert_eq!(imports, 2, "one import per buffer, not one per commit");
        forget_surface(surface);
        forget_surface(surface + 1);
    }

    #[test]
    fn a_buffer_goes_back_only_after_its_last_holder_and_a_gpu_marker() {
        let Some(present) = present() else {
            eprintln!("no wgpu adapter; skipping");
            return;
        };
        let (on_release, given_back) = std::sync::mpsc::channel::<&'static str>();
        let lease = |name: &'static str| {
            let on_release = on_release.clone();
            BufferLease::new(move || {
                let _ = on_release.send(name);
            })
        };
        let frame = client_frame(
            &present,
            wgpu::TextureFormat::Rgba8Unorm,
            [1, 2, 3, 255],
            (false, 20),
        );
        frame.hold(lease("first"));
        let image = frame.image((0, 0, 3, 2));
        let mut scene = vello::Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            vello::kurbo::Affine::IDENTITY,
            &vello::peniko::ImageBrush::new(image),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 3.0, 2.0),
        );
        // Phase `a_queued_frame_pins_what_it_draws`.
        let mut pins = FramePins::default();
        pins.add_scene(&scene);
        pins.add_scene(&scene);
        assert_eq!(pins.len(), 1, "one frame, pinned once");
        // Phase `a_superseded_frame_a_queued_frame_draws_is_kept`.
        let releases_before = release_latency().releases;
        frame.note_superseded();
        drop(frame);
        assert!(
            given_back.recv_timeout(Duration::from_millis(200)).is_err(),
            "the buffer stays held while a queued frame can still draw it"
        );
        // Phase `presenting_the_frame_gives_the_buffer_back_once`.
        drop(pins);
        assert_eq!(
            given_back.recv_timeout(GPU_WAIT.saturating_mul(2)),
            Ok("first"),
            "the buffer goes back once the frame drawing it is done"
        );
        assert!(given_back.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(release_latency().releases > releases_before);
        // Phase `a_readback_holds_its_frame`.
        let frame = client_frame(
            &present,
            wgpu::TextureFormat::Rgba8Unorm,
            [4, 5, 6, 255],
            (false, 21),
        );
        frame.hold(lease("second"));
        let reader = Arc::clone(&frame);
        drop(frame);
        assert_eq!(&reader.rgba().expect("readback")[..4], &[4, 5, 6, 255]);
        assert!(given_back.recv_timeout(Duration::from_millis(100)).is_err());
        drop(reader);
        assert_eq!(
            given_back.recv_timeout(GPU_WAIT.saturating_mul(2)),
            Ok("second")
        );
    }

    #[test]
    fn release_latency_summarises_its_window() {
        let mut window: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let summary = summarise(&mut window, 7);
        assert_eq!(summary.releases, 7);
        assert_eq!(summary.p50, Duration::from_millis(50));
        assert_eq!(summary.p99, Duration::from_millis(99));
        assert_eq!(summary.max, Duration::from_millis(100));
        assert_eq!(summarise(&mut [], 0), ReleaseLatency::default());
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
