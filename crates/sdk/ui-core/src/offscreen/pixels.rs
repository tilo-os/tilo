//! The pixels of a scene, for an app that reads them: the colour under an
//! eyedropper, a picture being exported.
//!
//! Where the thread can wait for the GPU, a [`request`] is rendered and read
//! back at once. A browser page cannot wait: there the host's frame loop
//! owns a [`PixelsHost`], which renders each request, has the GPU copy it
//! out and hands the pixels over once the copy has arrived, a few frames
//! later. Callers keep the [`PixelsRequest`], ask for frames while it
//! [`is_pending`](PixelsRequest::is_pending) and [`take`](PixelsRequest::take)
//! the answer when it is there, which works the same on every host.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use vello::peniko::Color;
use vello::{AaConfig, BumpBudgetPoll, RenderParams, Renderer, Scene};

use super::{OffscreenError, OffscreenRenderer, ResilientAttempts, resilient_submission_accepted};

/// A scene's pixels: straight-alpha RGBA rows, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Why a scene's pixels did not come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PixelsError {
    /// This host renders no pixels for apps: it has no GPU, or its thread
    /// cannot wait and no frame loop renders them.
    Unavailable,
    /// The GPU cannot hold a picture this size; `max` is its longest side.
    TooLarge { width: u32, height: u32, max: u32 },
    /// Rendering them, or reading them back, failed.
    Failed(String),
}

impl std::fmt::Display for PixelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("this host renders no pixels"),
            Self::TooLarge { width, height, max } => write!(
                f,
                "a {width}x{height} picture is larger than the GPU's {max} pixels a side"
            ),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for PixelsError {}

/// What a request is answered with.
pub type PixelsAnswer = Result<Pixels, PixelsError>;

enum Slot {
    Pending,
    Ready(PixelsAnswer),
    Taken,
}

/// The pixels of one scene, on their way. Dropping it gives the request up.
pub struct PixelsRequest {
    slot: Arc<Mutex<Slot>>,
}

impl std::fmt::Debug for PixelsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelsRequest")
            .field("pending", &self.is_pending())
            .finish()
    }
}

impl PixelsRequest {
    fn answered(answer: PixelsAnswer) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Slot::Ready(answer))),
        }
    }

    /// Whether the answer has yet to come; the caller asks for frames
    /// meanwhile, as the host renders the request between them.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(*self.slot.lock_recover(), Slot::Pending)
    }

    /// The answer, once it has come; `None` before, and after it was taken.
    #[must_use]
    pub fn take(&self) -> Option<PixelsAnswer> {
        let mut slot = self.slot.lock_recover();
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Ready(answer) => Some(answer),
            other => {
                *slot = other;
                None
            }
        }
    }
}

/// A request waiting for this thread's [`PixelsHost`]: no GPU resources,
/// so it may live in a thread-local queue (dropping wgpu resources from a
/// thread-local's destructor aborts the process; see the module docs of
/// [`super`]).
struct Wanted {
    scene: Scene,
    width: u32,
    height: u32,
    background: Color,
    slot: Arc<Mutex<Slot>>,
}

impl Wanted {
    /// Whether the caller still holds the request.
    fn wanted(&self) -> bool {
        Arc::strong_count(&self.slot) > 1
    }

    fn answer(&self, answer: PixelsAnswer) {
        *self.slot.lock_recover() = Slot::Ready(answer);
    }
}

thread_local! {
    /// How many [`PixelsHost`]s render this thread's requests (one, in a
    /// browser page).
    static HOSTS: Cell<usize> = const { Cell::new(0) };
    /// Requests no host has picked up yet.
    static WANTED: RefCell<VecDeque<Wanted>> = const { RefCell::new(VecDeque::new()) };
}

/// Whether a frame loop on this thread renders the requests made here (it
/// holds a [`PixelsHost`]): a caller that would render on a thread of its
/// own asks it instead, as the frame loop holds the page's GPU.
#[must_use]
pub fn frame_loop_renders() -> bool {
    HOSTS.get() > 0
}

/// Ask for the pixels of `scene`, `width` x `height` (at least one each),
/// over `background`.
///
/// Answered at once where the thread can wait for the GPU and no frame loop
/// renders requests; otherwise the thread's [`PixelsHost`] answers it a few
/// frames later, and with no host either it is answered
/// [`PixelsError::Unavailable`].
#[must_use]
pub fn request(scene: Scene, width: u32, height: u32, background: Color) -> PixelsRequest {
    let (width, height) = (width.max(1), height.max(1));
    if HOSTS.get() > 0 {
        let slot = Arc::new(Mutex::new(Slot::Pending));
        WANTED.with_borrow_mut(|wanted| {
            wanted.push_back(Wanted {
                scene,
                width,
                height,
                background,
                slot: Arc::clone(&slot),
            });
        });
        return PixelsRequest { slot };
    }
    if !tilo_rt::available(tilo_rt::Feature::BlockOn) {
        return PixelsRequest::answered(Err(PixelsError::Unavailable));
    }
    PixelsRequest::answered(render_now(&scene, width, height, background))
}

/// The offscreen renderers of the last sizes asked for, the newest last.
/// Each has learned how much memory a scene this busy needs, so the next
/// request fits at once, and an eyedropper's tiny picture does not cost a
/// larger one its renderer.
static KEPT: Mutex<Vec<((u32, u32), OffscreenRenderer)>> = Mutex::new(Vec::new());

/// How many sizes keep a renderer.
const KEPT_RENDERERS: usize = 2;

fn render_now(scene: &Scene, width: u32, height: u32, background: Color) -> PixelsAnswer {
    let mut kept = KEPT.lock_recover();
    if let Some(at) = kept.iter().position(|(size, _)| *size == (width, height)) {
        let entry = kept.remove(at);
        kept.push(entry);
    } else {
        let renderer = match OffscreenRenderer::try_new(width, height) {
            Ok(renderer) => renderer,
            // Without an adapter (a confined app worker, a machine with no
            // GPU) the picture is drawn on the CPU instead; a browser page,
            // which has no CPU rasterizer, draws none.
            Err(OffscreenError::NoAdapter) => {
                drop(kept);
                return match super::cpu::render(scene, width, height, background) {
                    Some(drawn) => drawn
                        .map(|rgba| Pixels {
                            rgba,
                            width,
                            height,
                        })
                        .map_err(PixelsError::Failed),
                    None => Err(PixelsError::Unavailable),
                };
            }
            Err(OffscreenError::CannotWait) => return Err(PixelsError::Unavailable),
            Err(error) => return Err(PixelsError::Failed(error.to_string())),
        };
        if kept.len() >= KEPT_RENDERERS {
            kept.remove(0);
        }
        kept.push(((width, height), renderer));
    }
    let Some((_, renderer)) = kept.last_mut() else {
        return Err(PixelsError::Unavailable);
    };
    let rgba = renderer
        .try_render_resilient(scene, background)
        .map_err(PixelsError::Failed)?;
    Ok(Pixels {
        rgba,
        width,
        height,
    })
}

/// Renders the requests made on its thread, for a frame loop whose thread
/// cannot wait for the GPU: [`drive`](Self::drive) it every frame while it
/// is [`waiting`](Self::waiting). Requests made while one exists go to it.
///
/// It draws with a renderer of its own rather than the frame's: one-shot
/// pixels must know whether their render dropped geometry (a scene busier
/// than the reserved memory), and the frame's renderer reports that for
/// whichever of its frames it last measured.
pub struct PixelsHost {
    gpu: Option<HostGpu>,
    current: Option<Job>,
}

struct HostGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

impl Default for PixelsHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PixelsHost {
    fn drop(&mut self) {
        HOSTS.set(HOSTS.get().saturating_sub(1));
    }
}

impl PixelsHost {
    /// A host for this thread's requests, with no GPU yet: requests wait
    /// for [`set_gpu`](Self::set_gpu).
    #[must_use]
    pub fn new() -> Self {
        HOSTS.set(HOSTS.get().saturating_add(1));
        Self {
            gpu: None,
            current: None,
        }
    }

    #[must_use]
    pub fn waiting(&self) -> bool {
        self.current.is_some() || WANTED.with_borrow(|wanted| !wanted.is_empty())
    }

    /// Whether requests wait for a renderer: the host creates one (on the
    /// web, asynchronously) and hands it over with [`set_gpu`](Self::set_gpu).
    #[must_use]
    pub fn needs_gpu(&self) -> bool {
        self.gpu.is_none() && self.waiting()
    }

    /// Whether it renders on `device`; a host whose device was lost and
    /// replaced hands it a renderer on the new one.
    #[must_use]
    pub fn draws_on(&self, device: &wgpu::Device) -> bool {
        self.gpu.as_ref().is_some_and(|gpu| gpu.device == *device)
    }

    /// Render from now on with `renderer` on `device`.
    pub fn set_gpu(&mut self, device: wgpu::Device, queue: wgpu::Queue, renderer: Renderer) {
        // A request half way on the old device starts again on the new one.
        if let Some(job) = self.current.take() {
            WANTED.with_borrow_mut(|wanted| wanted.push_front(job.wanted));
        }
        self.gpu = Some(HostGpu {
            device,
            queue,
            renderer,
        });
    }

    /// Answer every waiting request with `why`: the host could not create
    /// its renderer.
    pub fn refuse(&mut self, why: &str) {
        let mut refused: Vec<Wanted> = WANTED.with_borrow_mut(|wanted| wanted.drain(..).collect());
        refused.extend(self.current.take().map(|job| job.wanted));
        for wanted in refused {
            wanted.answer(Err(PixelsError::Failed(why.to_owned())));
        }
    }

    /// Move the requests on without waiting: start the next one, or look
    /// whether the GPU has copied the current one out and answer it. One
    /// request at a time, so the renderer's memory report is that request's.
    /// Whether it answered one: what asked for it reads it on the next
    /// frame, so the caller draws one.
    pub fn drive(&mut self) -> bool {
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        let mut answered = false;
        loop {
            let mut job = match self.current.take() {
                Some(job) => job,
                None => match WANTED.with_borrow_mut(VecDeque::pop_front) {
                    Some(wanted) => Job::new(wanted),
                    None => return answered,
                },
            };
            if !job.wanted.wanted() {
                continue;
            }
            match job.step(gpu) {
                Step::Waiting => {
                    self.current = Some(job);
                    return answered;
                }
                Step::Done(answer) => {
                    job.wanted.answer(answer);
                    answered = true;
                }
            }
        }
    }
}

/// A request the host is rendering.
struct Job {
    wanted: Wanted,
    stage: Stage,
    /// A clean render has been read back; the next clean one is accepted
    /// (the first may still have been filling the renderer's atlas).
    confirmed: bool,
    attempts: ResilientAttempts,
    /// The renderer's memory reservation when the current render began.
    reserved: u64,
}

enum Stage {
    /// Nothing submitted.
    Idle,
    /// Rendered; the GPU is copying the picture out.
    Copying(Readback),
    /// Read back; the renderer has yet to report whether the render fit.
    Settling(Vec<u8>),
}

struct Readback {
    /// Kept until the copy has finished.
    _texture: wgpu::Texture,
    buffer: wgpu::Buffer,
    padded_bytes_per_row: u32,
    /// 0 while mapping, 1 once mapped, 2 when mapping failed.
    mapped: Arc<AtomicU8>,
}

enum Step {
    Waiting,
    Done(PixelsAnswer),
}

impl Job {
    fn new(wanted: Wanted) -> Self {
        Self {
            wanted,
            stage: Stage::Idle,
            confirmed: false,
            attempts: ResilientAttempts::default(),
            reserved: 0,
        }
    }

    fn step(&mut self, gpu: &mut HostGpu) -> Step {
        loop {
            match std::mem::replace(&mut self.stage, Stage::Idle) {
                Stage::Idle => match self.submit(gpu) {
                    Ok(readback) => self.stage = Stage::Copying(readback),
                    Err(error) => return Step::Done(Err(error)),
                },
                Stage::Copying(readback) => {
                    // Gives finished callbacks their turn natively; the web
                    // runs them from its event loop.
                    let _ = gpu.device.poll(wgpu::PollType::Poll);
                    match readback.mapped.load(Ordering::Acquire) {
                        0 => {
                            self.stage = Stage::Copying(readback);
                            return Step::Waiting;
                        }
                        1 => self.stage = Stage::Settling(self.read(&readback)),
                        _ => {
                            return Step::Done(Err(PixelsError::Failed(
                                "the GPU could not copy the picture back".to_owned(),
                            )));
                        }
                    }
                }
                Stage::Settling(rgba) => match gpu.renderer.poll_bump_budget(&gpu.device) {
                    Err(error) => return Step::Done(Err(PixelsError::Failed(error.to_string()))),
                    Ok(BumpBudgetPoll::Pending) => {
                        self.stage = Stage::Settling(rgba);
                        return Step::Waiting;
                    }
                    Ok(BumpBudgetPoll::Complete { overflowed }) => {
                        if let Some(answer) = self.judge(gpu, rgba, overflowed) {
                            return Step::Done(answer);
                        }
                    }
                },
            }
        }
    }

    /// Accept a read-back render, or decide to render again (`None`), as
    /// [`OffscreenRenderer::try_render_resilient`] does.
    fn judge(&mut self, gpu: &HostGpu, rgba: Vec<u8>, overflowed: bool) -> Option<PixelsAnswer> {
        let scene = &self.wanted.scene;
        let grew = gpu.renderer.memory_stats().bump_budget_bytes > self.reserved;
        let unexpectedly_transparent = self.wanted.background == Color::TRANSPARENT
            && !scene.encoding().is_empty()
            && rgba
                .chunks_exact(4)
                .all(|pixel| pixel.get(3).copied().unwrap_or(0) == 0);
        if overflowed || unexpectedly_transparent {
            self.confirmed = false;
        } else if resilient_submission_accepted(scene.encoding().is_empty(), self.confirmed) {
            return Some(Ok(Pixels {
                rgba,
                width: self.wanted.width,
                height: self.wanted.height,
            }));
        } else {
            self.confirmed = true;
        }
        if self.attempts.another(grew) {
            None
        } else {
            Some(Err(PixelsError::Failed(format!(
                "the picture did not fit the GPU's memory after {} renders",
                self.attempts.submissions()
            ))))
        }
    }

    /// Render the scene and have the GPU copy it into a buffer to map.
    fn submit(&mut self, gpu: &mut HostGpu) -> Result<Readback, PixelsError> {
        let Wanted {
            width,
            height,
            background,
            ..
        } = self.wanted;
        let max = gpu.device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return Err(PixelsError::TooLarge { width, height, max });
        }
        self.reserved = gpu.renderer.memory_stats().bump_budget_bytes;
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pixels-request-target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.renderer
            .render_to_texture(
                &gpu.device,
                &gpu.queue,
                &self.wanted.scene,
                &view,
                &RenderParams {
                    base_color: background,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|error| PixelsError::Failed(error.to_string()))?;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = width
            .saturating_mul(4)
            .div_ceil(align)
            .saturating_mul(align);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pixels-request-readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        gpu.queue.submit([encoder.finish()]);
        let mapped = Arc::new(AtomicU8::new(0));
        let done = Arc::clone(&mapped);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                done.store(if result.is_ok() { 1 } else { 2 }, Ordering::Release);
            });
        Ok(Readback {
            _texture: texture,
            buffer,
            padded_bytes_per_row,
            mapped,
        })
    }

    /// The mapped picture without the rows' padding.
    fn read(&self, readback: &Readback) -> Vec<u8> {
        let row = (self.wanted.width as usize).saturating_mul(4);
        let padded = readback.padded_bytes_per_row as usize;
        let mut rgba = Vec::with_capacity(row.saturating_mul(self.wanted.height as usize));
        {
            let mapped = readback.buffer.slice(..).get_mapped_range();
            for line in mapped.chunks(padded).take(self.wanted.height as usize) {
                rgba.extend_from_slice(line.get(..row).unwrap_or(line));
            }
        }
        readback.buffer.unmap();
        rgba
    }
}

#[cfg(test)]
mod tests {
    use super::{PixelsError, PixelsHost, request};
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};
    use vello::{AaSupport, Renderer, RendererOptions, Scene};

    fn red_square() -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(220, 30, 30),
            None,
            &Rect::new(0.0, 0.0, 6.0, 6.0),
        );
        scene
    }

    /// A host as a browser page's frame loop has one, on this device.
    fn host() -> PixelsHost {
        let gpu = super::super::shared_gpu().unwrap();
        let renderer = Renderer::new(
            &gpu.device,
            RendererOptions {
                antialiasing_support: AaSupport::area_only(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut host = PixelsHost::new();
        host.set_gpu(gpu.device, gpu.queue, renderer);
        host
    }

    /// Drive `host` as frames would, until nothing waits: whether a frame
    /// said it answered a request.
    fn frames(host: &mut PixelsHost) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut answered = false;
        while host.waiting() {
            assert!(
                std::time::Instant::now() < deadline,
                "the request was never answered"
            );
            answered |= host.drive();
            std::thread::yield_now();
        }
        answered
    }

    #[test]
    fn a_thread_that_can_wait_is_answered_at_once() {
        let asked = request(red_square(), 8, 8, Color::WHITE);
        assert!(!asked.is_pending());
        let pixels = asked.take().unwrap().unwrap();
        assert_eq!(
            (pixels.width, pixels.height, pixels.rgba.len()),
            (8, 8, 8 * 8 * 4)
        );
        assert_eq!(pixels.rgba.get(..4), Some(&[220, 30, 30, 255][..]));
        assert_eq!(
            pixels.rgba.get(7 * 4..8 * 4),
            Some(&[255, 255, 255, 255][..])
        );
        assert!(asked.take().is_none(), "an answer is taken once");
    }

    /// The browser's way: the request waits for the frame loop's host, which
    /// renders it without blocking and answers once the copy came back.
    #[test]
    fn a_frame_loop_answers_requests_between_frames() {
        let mut host = PixelsHost::new();
        // A width the GPU's row alignment pads.
        let asked = request(red_square(), 10, 3, Color::WHITE);
        assert!(asked.is_pending());
        assert!(
            host.needs_gpu(),
            "the request waits for the host's renderer"
        );
        assert!(!host.drive(), "nothing is answered without a renderer");
        assert!(asked.is_pending(), "nothing renders without a renderer");
        drop(host);
        let mut host = self::host();
        assert!(
            frames(&mut host),
            "the frame that answered says so, to draw the next"
        );
        let pixels = asked.take().unwrap().unwrap();
        assert_eq!(
            (pixels.width, pixels.height, pixels.rgba.len()),
            (10, 3, 10 * 3 * 4)
        );
        assert_eq!(pixels.rgba.get(..4), Some(&[220, 30, 30, 255][..]));
        assert_eq!(
            pixels.rgba.get(9 * 4..10 * 4),
            Some(&[255, 255, 255, 255][..])
        );
        // The last row is the picture's, not padding.
        assert_eq!(
            pixels.rgba.get(2 * 40..2 * 40 + 4),
            Some(&[220, 30, 30, 255][..])
        );
    }

    #[test]
    fn a_request_given_up_is_not_rendered() {
        let mut host = host();
        drop(request(red_square(), 8, 8, Color::WHITE));
        host.drive();
        assert!(!host.waiting());
    }

    #[test]
    fn a_picture_larger_than_the_gpu_holds_says_so() {
        let mut host = host();
        let asked = request(red_square(), 1 << 20, 1, Color::WHITE);
        frames(&mut host);
        assert!(matches!(
            asked.take(),
            Some(Err(PixelsError::TooLarge {
                width: 1_048_576,
                ..
            }))
        ));
    }

    #[test]
    fn a_host_without_a_renderer_refuses_what_waits() {
        let mut host = PixelsHost::new();
        let asked = request(red_square(), 8, 8, Color::WHITE);
        host.refuse("no WebGPU device");
        assert!(!host.waiting());
        assert_eq!(
            asked.take(),
            Some(Err(PixelsError::Failed("no WebGPU device".to_owned())))
        );
    }
}
