//! The remote renderer: a window's frames drawn by the room's render service
//! instead of a GPU in this process.
//!
//! The render worker's contract is unchanged — the main thread still lays
//! out, records each frame as a vello [`Scene`] and sends it with its damage
//! through [`RenderThreadHandle`] — so app code does not change. What the
//! worker does with a frame does: it writes the scene into the shared ring
//! (`tilo_render_protocol::frame`), names fonts and images by id (a font from
//! the shared read-only view by its path, anything else uploaded once into a
//! sealed memfd this process owns, so its memory is the app's), and tells the
//! service. The service answers with the buffer the frame is in; the worker
//! attaches it to the window's own surface and commits, so the compositor
//! takes it like any client buffer. No GPU device, no surface, no pipelines in
//! the app.
//!
//! Each window renders on a channel of its own, which the worker asks the
//! compositor's render broker for (`tilo_render_protocol::broker`). When the
//! service goes away (it crashed, or the room moved to another one), the
//! window keeps showing its last frame, the worker asks for a new channel and
//! the next frame is a whole one: the toolkit always has the full scene.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use tilo_render_protocol::channel::{Channel, ChannelError, Received};
use tilo_render_protocol::control::{
    AlphaType, AppMessage, BufferKind, DmabufFormat, ImageFormat, Refusal, ServiceMessage,
};
use tilo_render_protocol::frame::{Backdrop, FrameWriter, Rect as WireRect, ScrollCopy};
use tilo_render_protocol::scene::ResourceIds;
use tilo_render_protocol::shm::{RingWriter, sealed_blob};
use tilo_render_protocol::{FontId, ImageId, LayerId, PROTOCOL_VERSION, SurfaceId, WireError};
use vello::Scene;
use vello::kurbo::Rect;
use vello::peniko::{FontData, ImageData, WeakBlob};
use winit::platform::wayland::{
    ClientBuffer, ClientBufferSource, WaylandPresentation, WindowExtWayland as _,
};
use winit::window::Window;

use super::{
    BufferPool, FrameOutcome, PresentHook, RenderRequest, RenderThreadHandle, notify_buffer_waiter,
    notify_render_completion,
};

/// The ring a connection starts with; a frame larger than it grows it.
const INITIAL_RING: usize = 4 << 20;

/// The one surface a window's channel draws.
const SURFACE: SurfaceId = SurfaceId(1);

/// DRM's linear modifier: the service hands out linear buffers only.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Frames between sweeps for images the app dropped.
const SWEEP_EVERY: u64 = 64;

/// Whether this process draws through a render service: the launcher handed
/// it a broker connection, or (outside a sandbox) it was pointed at the
/// broker's socket. `TILO_RENDER=local` keeps the in-process renderer.
pub fn requested() -> bool {
    static REQUESTED: OnceLock<bool> = OnceLock::new();
    *REQUESTED.get_or_init(|| {
        let local = std::env::var("TILO_RENDER").is_ok_and(|mode| mode == "local");
        let wanted = !local
            && (std::env::var_os(tilo_render_protocol::broker::FD_ENV).is_some()
                || std::env::var_os(tilo_render_protocol::broker::SOCKET_ENV).is_some());
        if wanted {
            ACTIVE.store(true, Ordering::Release);
        }
        wanted
    })
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether frames go to a render service, cheaply, for code that records
/// what the service will need (font origins).
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Fonts the text renderer loaded from a file, by blob: the service maps the
/// same file from the shared read-only view rather than receiving its bytes.
fn font_files() -> &'static Mutex<HashMap<u64, PathBuf>> {
    static FILES: OnceLock<Mutex<HashMap<u64, PathBuf>>> = OnceLock::new();
    FILES.get_or_init(Mutex::default)
}

/// `font` was loaded from `path`.
pub fn note_font_file(font: &FontData, path: &Path) {
    font_files()
        .lock_recover()
        .entry(font.data.id())
        .or_insert_with(|| path.to_path_buf());
}

/// Uploads shared by every window's connection: one sealed memfd per image
/// or font, sent to each connection that draws it, closed once the app drops
/// the image.
fn uploads() -> &'static Mutex<HashMap<u64, (Arc<OwnedFd>, WeakBlob<u8>)>> {
    static UPLOADS: OnceLock<Mutex<HashMap<u64, (Arc<OwnedFd>, WeakBlob<u8>)>>> = OnceLock::new();
    UPLOADS.get_or_init(Mutex::default)
}

fn upload(data: &vello::peniko::Blob<u8>, bytes: &[u8]) -> io::Result<Arc<OwnedFd>> {
    let mut uploads = uploads().lock_recover();
    if let Some((fd, _)) = uploads.get(&data.id()) {
        return Ok(fd.clone());
    }
    let fd = Arc::new(sealed_blob("tilo-render-upload", bytes)?);
    uploads.insert(data.id(), (fd.clone(), data.downgrade()));
    Ok(fd)
}

/// The broker connection: the launcher's, or one this process made.
fn broker() -> Option<&'static Mutex<OwnedFd>> {
    static BROKER: OnceLock<Option<Mutex<OwnedFd>>> = OnceLock::new();
    BROKER
        .get_or_init(|| {
            if let Some(fd) = std::env::var(tilo_render_protocol::broker::FD_ENV)
                .ok()
                .and_then(|fd| fd.parse::<i32>().ok())
                .filter(|fd| *fd > 2)
            {
                // SAFETY: the launcher placed the broker connection at this
                // descriptor for this process, and nothing else takes it.
                return Some(Mutex::new(unsafe { OwnedFd::from_raw_fd(fd) }));
            }
            let path = std::env::var_os(tilo_render_protocol::broker::SOCKET_ENV)?;
            let mut stream = std::os::unix::net::UnixStream::connect(path).ok()?;
            let app_id = std::env::var("TILO_PACKAGE_ID").unwrap_or_else(|_| {
                std::env::args()
                    .next()
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            });
            tilo_render_protocol::broker::write_handshake(&mut stream, &app_id, None).ok()?;
            Some(Mutex::new(OwnedFd::from(stream)))
        })
        .as_ref()
}

/// A connection to the service: its channel, its ring and what was
/// registered on it.
struct Session {
    channel: Arc<Channel>,
    ring: RingWriter,
    max_ring: usize,
    font_roots: Vec<PathBuf>,
    fonts: HashMap<u64, FontId>,
    images: HashMap<u64, (ImageId, WeakBlob<u8>)>,
    next_font: u32,
    next_image: u32,
}

impl Session {
    fn connect() -> Result<Self, String> {
        tilo_startup_metrics::mark("remote-render-connect-start");
        let broker = broker().ok_or("no render broker")?;
        let channel = {
            let broker = broker.lock_recover();
            tilo_render_protocol::broker::request_channel(broker.as_fd())
                .map_err(|error| format!("render broker: {error}"))?
        };
        let ring = RingWriter::create(INITIAL_RING).map_err(|error| format!("ring: {error}"))?;
        channel
            .send(
                &AppMessage::Hello {
                    version: PROTOCOL_VERSION,
                },
                &[ring.fd()],
            )
            .map_err(|error| format!("render service: {error}"))?;
        let welcome = channel
            .recv::<ServiceMessage>()
            .map_err(|error| format!("render service: {error}"))?;
        let ServiceMessage::Welcome {
            limits, font_roots, ..
        } = welcome.message
        else {
            return Err("the render service did not welcome this app".to_owned());
        };
        tilo_startup_metrics::mark("remote-render-connect-done");
        Ok(Self {
            channel: Arc::new(channel),
            ring,
            max_ring: limits.ring_bytes as usize,
            font_roots: font_roots.into_iter().map(PathBuf::from).collect(),
            fonts: HashMap::new(),
            images: HashMap::new(),
            next_font: 0,
            next_image: 0,
        })
    }

    fn send(
        &self,
        message: &AppMessage,
        fds: &[std::os::fd::BorrowedFd<'_>],
    ) -> Result<(), WireError> {
        self.channel
            .send(message, fds)
            .map_err(|_| WireError::Malformed("the render service is gone"))
    }

    /// Tell the service about images the app no longer holds.
    fn sweep(&mut self) {
        let dead: Vec<(u64, ImageId)> = self
            .images
            .iter()
            .filter(|(_, (_, weak))| weak.upgrade().is_none())
            .map(|(blob, (id, _))| (*blob, *id))
            .collect();
        for (blob, id) in dead {
            self.images.remove(&blob);
            let _ = self.send(&AppMessage::DropImage { image: id }, &[]);
        }
        uploads()
            .lock_recover()
            .retain(|_, (_, weak)| weak.upgrade().is_some());
    }

    /// Make the ring hold `len` bytes, replacing it when a frame outgrew it.
    fn fit_ring(&mut self, len: usize) -> Result<(), Lost> {
        if len <= self.ring.len() {
            return Ok(());
        }
        let wanted = len
            .checked_next_power_of_two()
            .filter(|wanted| *wanted <= self.max_ring)
            .ok_or_else(|| {
                Lost::Failed(format!(
                    "a {len}-byte frame is over the service's ring limit"
                ))
            })?
            .max(INITIAL_RING);
        let ring =
            RingWriter::create(wanted).map_err(|error| Lost::Failed(format!("ring: {error}")))?;
        self.send(&AppMessage::Ring, &[ring.fd()])
            .map_err(|error| Lost::Disconnected(error.to_string()))?;
        self.ring = ring;
        Ok(())
    }
}

impl ResourceIds for Session {
    fn font(&mut self, font: &FontData) -> Result<FontId, WireError> {
        if let Some(id) = self.fonts.get(&font.data.id()) {
            return Ok(*id);
        }
        let id = FontId(self.next_font);
        self.next_font = self.next_font.wrapping_add(1);
        let file = font_files().lock_recover().get(&font.data.id()).cloned();
        if let Some(path) =
            file.filter(|path| self.font_roots.iter().any(|root| path.starts_with(root)))
        {
            self.send(
                &AppMessage::RegisterFontFile {
                    font: id,
                    path: path.display().to_string(),
                    index: font.index,
                },
                &[],
            )?;
        } else {
            let fd = upload(&font.data, font.data.data())
                .map_err(|_| WireError::Malformed("font upload"))?;
            self.send(
                &AppMessage::RegisterFontData {
                    font: id,
                    index: font.index,
                },
                &[fd.as_fd()],
            )?;
        }
        self.fonts.insert(font.data.id(), id);
        Ok(id)
    }

    fn image(&mut self, image: &ImageData) -> Result<ImageId, WireError> {
        if let Some((id, _)) = self.images.get(&image.data.id()) {
            return Ok(*id);
        }
        let id = ImageId(self.next_image);
        self.next_image = self.next_image.wrapping_add(1);
        let length = image
            .format
            .size_in_bytes(image.width, image.height)
            .ok_or(WireError::Overflow)?;
        let pixels = image.data.data();
        // An image without CPU pixels (drawn from a texture elsewhere) is
        // transparent here, as vello draws it without a source.
        let zeros;
        let bytes = if let Some(bytes) = pixels.get(..length) {
            bytes
        } else {
            zeros = vec![0_u8; length];
            &zeros
        };
        let fd = upload(&image.data, bytes).map_err(|_| WireError::Malformed("image upload"))?;
        self.send(
            &AppMessage::RegisterImage {
                image: id,
                width: image.width,
                height: image.height,
                format: if matches!(image.format, vello::peniko::ImageFormat::Bgra8) {
                    ImageFormat::Bgra8
                } else {
                    ImageFormat::Rgba8
                },
                alpha: match image.alpha_type {
                    vello::peniko::ImageAlphaType::AlphaPremultiplied => AlphaType::Premultiplied,
                    vello::peniko::ImageAlphaType::Alpha => AlphaType::Straight,
                },
            },
            &[fd.as_fd()],
        )?;
        self.images
            .insert(image.data.id(), (id, image.data.downgrade()));
        Ok(id)
    }
}

/// A connection made while the app constructs, for its first window.
fn prepared() -> &'static Mutex<Option<Receiver<Result<Session, String>>>> {
    static PREPARED: OnceLock<Mutex<Option<Receiver<Result<Session, String>>>>> = OnceLock::new();
    PREPARED.get_or_init(Mutex::default)
}

/// Connect to the service on a thread of its own from process start, so the
/// first window's first frame finds the channel open.
#[allow(
    clippy::disallowed_methods,
    reason = "native host runner: the render connection is made beside app construction"
)]
pub(crate) fn prepare() {
    let (sender, receiver) = mpsc::channel();
    if std::thread::Builder::new()
        .name("remote-render-connect".into())
        .spawn(move || {
            let _ = sender.send(Session::connect());
        })
        .is_ok()
    {
        *prepared().lock_recover() = Some(receiver);
    }
}

fn take_session() -> Result<Session, String> {
    let preparation = prepared().lock_recover().take();
    match preparation.map(|receiver| receiver.recv()) {
        Some(Ok(Ok(session))) => Ok(session),
        _ => Session::connect(),
    }
}

/// Where buffer releases and presentation feedback go: the current channel,
/// replaced when the worker reconnects. Each channel is an epoch: a buffer
/// of an earlier service, released late, must not free a buffer of the same
/// number on the new one while the app still shows it.
#[derive(Default)]
struct Sink {
    channel: Mutex<(u64, Option<Arc<Channel>>)>,
}

impl Sink {
    fn epoch(&self) -> u64 {
        self.channel.lock_recover().0
    }

    fn replace(&self, channel: Arc<Channel>) {
        let mut current = self.channel.lock_recover();
        current.0 = current.0.wrapping_add(1);
        current.1 = Some(channel);
    }

    /// Send `message` on the channel of `epoch`, if it is still the current one.
    fn send(&self, epoch: u64, message: &AppMessage) {
        let channel = {
            let current = self.channel.lock_recover();
            (current.0 == epoch).then(|| current.1.clone()).flatten()
        };
        if let Some(channel) = channel {
            let _ = channel.send(message, &[]);
        }
    }
}

/// The surface's buffers, as `wl_buffer`s.
struct Buffers {
    generation: u32,
    list: Vec<ClientBuffer>,
}

/// A render worker that draws through the room's render service, with the
/// same handle the in-process one has.
///
/// # Errors
///
/// When the worker thread cannot start.
#[allow(
    clippy::disallowed_methods,
    reason = "native host runner: the remote render worker is a dedicated thread"
)]
pub(crate) fn spawn(
    window: Arc<Window>,
    transparent: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<RenderThreadHandle, String> {
    let (req_tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (pool_tx, pool_rx) = mpsc::sync_channel(2);
    let (outcome_tx, frame_complete) = mpsc::sync_channel::<FrameOutcome>(8);
    let waiting = Arc::new(AtomicBool::new(false));
    let full_repaint = Arc::new(AtomicBool::new(false));
    let buffers = BufferPool::new(pool_rx, waiting.clone());
    let _ = pool_tx.send((Scene::new(), Vec::with_capacity(8)));
    let _ = pool_tx.send((Scene::new(), Vec::with_capacity(8)));
    let mut worker = Worker {
        window,
        opaque: !transparent,
        wake,
        waiting,
        full_repaint: full_repaint.clone(),
        pool_tx,
        outcome_tx,
        sink: Arc::new(Sink::default()),
        buffers: None,
        retired: None,
        sequence: 0,
        first_presented: false,
    };
    // Opened here, so a session without a render service falls back to the
    // in-process renderer (`create_window_render`); the launch connected
    // while the app constructed, so this rarely waits.
    let session = worker.open()?;
    let join = std::thread::Builder::new()
        .name("ui-render-remote".into())
        .spawn(move || {
            tilo_startup_metrics::mark("render-worker-start");
            tilo_rt::mark_ui_thread();
            worker.run(session, &rx);
            None
        })
        .map_err(|error| format!("could not spawn the remote render thread: {error}"))?;
    Ok(RenderThreadHandle {
        render_tx: req_tx,
        buffers,
        frame_complete,
        retry: super::retry::RenderRetryState::default(),
        full_repaint,
        join: Some(join),
    })
}

/// A frame, as the main thread sent it.
struct FrameJob<'a> {
    scene: &'a Scene,
    width: u32,
    height: u32,
    base_color: [f32; 4],
    damage: &'a [Rect],
    copies: &'a [crate::scroll_copy::DeviceCopy],
    full: bool,
    backdrop: &'a crate::backdrop::BackdropFrame,
}

/// Why a frame got no outcome from the service.
enum Lost {
    /// The channel broke: reconnect, then repaint everything.
    Disconnected(String),
    /// This frame cannot be drawn (an encoding or ring failure).
    Failed(String),
}

struct Worker {
    window: Arc<Window>,
    opaque: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
    waiting: Arc<AtomicBool>,
    full_repaint: Arc<AtomicBool>,
    pool_tx: SyncSender<(Scene, Vec<Rect>)>,
    outcome_tx: SyncSender<FrameOutcome>,
    sink: Arc<Sink>,
    buffers: Option<Buffers>,
    /// The previous buffers, kept until a buffer of the new ones is on
    /// screen: destroying an attached buffer would blank the window.
    retired: Option<Buffers>,
    sequence: u64,
    first_presented: bool,
}

impl Worker {
    fn run(mut self, session: Session, rx: &Receiver<RenderRequest>) {
        let mut session = Some(session);
        let mut hooks: Vec<PresentHook> = Vec::new();
        while let Ok(request) = rx.recv() {
            let RenderRequest::Frame {
                mut scene,
                width,
                height,
                base_color,
                mut damage_regions,
                scroll_copies,
                full_redraw,
                backdrop,
                on_present,
                ..
            } = request
            else {
                break;
            };
            hooks.extend(on_present);
            let job = FrameJob {
                scene: &scene,
                width,
                height,
                base_color: base_color.components,
                damage: &damage_regions,
                copies: &scroll_copies,
                full: full_redraw,
                backdrop: &backdrop,
            };
            let outcome = if let Some(live) = session.as_mut() {
                match self.frame(live, &job) {
                    Ok(outcome) => outcome,
                    Err(Lost::Failed(reason)) => {
                        tracing::error!(%reason, "frame not sent to the render service");
                        FrameOutcome::Skipped
                    }
                    Err(Lost::Disconnected(reason)) => {
                        tracing::warn!(%reason, "render service gone; reconnecting");
                        session = self.open().ok();
                        self.repaint();
                        FrameOutcome::Skipped
                    }
                }
            } else {
                session = self.open().ok();
                self.repaint();
                FrameOutcome::Skipped
            };
            if outcome == FrameOutcome::Presented && !hooks.is_empty() {
                let presented = crate::time::wall();
                for hook in std::mem::take(&mut hooks) {
                    hook(presented);
                }
            }
            scene.reset();
            damage_regions.clear();
            let _ = self.pool_tx.send((scene, damage_regions));
            let _ = self.outcome_tx.send(outcome);
            notify_render_completion(&self.waiting, self.wake.as_ref(), outcome);
        }
        drop(self.pool_tx);
        notify_buffer_waiter(&self.waiting, self.wake.as_ref());
    }

    /// A session with this window's surface on it.
    fn open(&self) -> Result<Session, String> {
        let session = take_session()?;
        let formats = self
            .window
            .client_buffer_formats()
            .into_iter()
            .filter(|(_, modifier)| *modifier == DRM_FORMAT_MOD_LINEAR)
            .map(|(fourcc, modifier)| DmabufFormat { fourcc, modifier })
            .collect();
        session
            .send(
                &AppMessage::CreateSurface {
                    surface: SURFACE,
                    opaque: self.opaque,
                    formats,
                },
                &[],
            )
            .map_err(|error| error.to_string())?;
        self.sink.replace(session.channel.clone());
        Ok(session)
    }

    /// The next frame must be whole: what the service had is gone.
    fn repaint(&self) {
        self.full_repaint.store(true, Ordering::SeqCst);
        (self.wake)();
    }

    fn frame(&mut self, session: &mut Session, job: &FrameJob<'_>) -> Result<FrameOutcome, Lost> {
        self.sequence = self.sequence.wrapping_add(1);
        let sequence = self.sequence;
        if sequence.is_multiple_of(SWEEP_EVERY) {
            session.sweep();
        }
        let record =
            encode(session, job, sequence).map_err(|error| Lost::Failed(error.to_string()))?;
        session.fit_ring(record.len())?;
        let offset = session
            .ring
            .write(sequence, &record)
            .ok_or_else(|| Lost::Failed("the ring has no room".to_owned()))?;
        if sequence == 1 {
            tilo_startup_metrics::mark("remote-render-first-frame-sent");
        }
        session
            .channel
            .send(
                &AppMessage::Frame {
                    sequence,
                    offset: offset as u64,
                    len: u32::try_from(record.len())
                        .map_err(|_| Lost::Failed("frame size".to_owned()))?,
                },
                &[],
            )
            .map_err(|error| Lost::Disconnected(error.to_string()))?;
        loop {
            let Received { message, fds } = match session.channel.recv::<ServiceMessage>() {
                Ok(received) => received,
                Err(ChannelError::Closed) => {
                    return Err(Lost::Disconnected("the channel closed".to_owned()));
                }
                Err(error) => return Err(Lost::Disconnected(error.to_string())),
            };
            match message {
                ServiceMessage::Buffers {
                    generation,
                    width,
                    height,
                    kind,
                    planes,
                    ..
                } => self.take_buffers(generation, (width, height), kind, &planes, &fds),
                ServiceMessage::FrameReady {
                    sequence: ready,
                    generation,
                    buffer,
                    damage,
                    ..
                } if ready == sequence => {
                    session.ring.release(sequence);
                    return Ok(self.attach(generation, buffer, &damage, sequence));
                }
                ServiceMessage::FrameDropped {
                    sequence: dropped,
                    why,
                } if dropped == sequence => {
                    session.ring.release(sequence);
                    return Ok(self.dropped(&why));
                }
                ServiceMessage::Refused { why } => {
                    tracing::warn!(?why, "the render service refused a registration");
                }
                other => tracing::debug!(?other, "unexpected render service message"),
            }
        }
    }

    fn take_buffers(
        &mut self,
        generation: u32,
        (width, height): (u32, u32),
        kind: BufferKind,
        planes: &[tilo_render_protocol::control::Plane],
        fds: &[OwnedFd],
    ) {
        let mut list = Vec::with_capacity(fds.len());
        for (index, (fd, plane)) in fds.iter().zip(planes).enumerate() {
            let (Ok(width), Ok(height), Ok(index)) = (
                i32::try_from(width),
                i32::try_from(height),
                u32::try_from(index),
            ) else {
                return;
            };
            let source = match kind {
                BufferKind::Dmabuf(format) => ClientBufferSource::Dmabuf {
                    fd: fd.as_fd(),
                    width,
                    height,
                    fourcc: format.fourcc,
                    modifier: format.modifier,
                    offset: plane.offset,
                    stride: plane.stride,
                },
                BufferKind::Shm { format } => ClientBufferSource::Shm {
                    fd: fd.as_fd(),
                    width,
                    height,
                    format,
                    offset: i32::try_from(plane.offset).unwrap_or(0),
                    stride: i32::try_from(plane.stride).unwrap_or(0),
                },
            };
            let sink = Arc::downgrade(&self.sink);
            let epoch = self.sink.epoch();
            let released = Box::new(move || {
                if let Some(sink) = sink.upgrade() {
                    sink.send(
                        epoch,
                        &AppMessage::Released {
                            surface: SURFACE,
                            generation,
                            buffer: index,
                        },
                    );
                }
            });
            let Some(buffer) = self.window.create_client_buffer(source, released) else {
                tracing::error!(
                    ?kind,
                    "the compositor takes none of the render service's buffers"
                );
                return;
            };
            list.push(buffer);
        }
        self.retired = self.buffers.replace(Buffers { generation, list });
    }

    fn attach(
        &mut self,
        generation: u32,
        buffer: u32,
        damage: &[[i32; 4]],
        sequence: u64,
    ) -> FrameOutcome {
        let Some(buffers) = self
            .buffers
            .as_ref()
            .filter(|set| set.generation == generation)
        else {
            return FrameOutcome::Skipped;
        };
        let Some(buffer) = buffers.list.get(buffer as usize) else {
            return FrameOutcome::Skipped;
        };
        let damage: Vec<(i32, i32, i32, i32)> = damage
            .iter()
            .map(|[x, y, width, height]| (*x, *y, *width, *height))
            .collect();
        let sink = Arc::downgrade(&self.sink);
        let epoch = self.sink.epoch();
        let observer: winit::platform::wayland::WaylandPresentationObserver =
            Box::new(move |presentation: WaylandPresentation| {
                let Some(sink) = sink.upgrade() else {
                    return;
                };
                let message = match presentation {
                    WaylandPresentation::Presented {
                        timestamp, refresh, ..
                    } => AppMessage::Presented {
                        surface: SURFACE,
                        sequence,
                        presented_ns: u64::try_from(timestamp.as_nanos()).unwrap_or(u64::MAX),
                        refresh_ns: u64::try_from(refresh.as_nanos()).unwrap_or(u64::MAX),
                        discarded: false,
                    },
                    WaylandPresentation::Discarded => AppMessage::Presented {
                        surface: SURFACE,
                        sequence,
                        presented_ns: 0,
                        refresh_ns: 0,
                        discarded: true,
                    },
                };
                sink.send(epoch, &message);
            });
        self.window
            .attach_client_buffer(buffer, &damage, Some(observer));
        // A buffer of the new generation is on screen: the old ones can go.
        drop(self.retired.take());
        if !self.first_presented {
            self.first_presented = true;
            tilo_startup_metrics::mark("first-frame-presented");
            tilo_startup_metrics::dump();
            if let Some(line) = tilo_startup_metrics::summary_line(&super::startup_app_name()) {
                crate::diag!("{line}");
            }
            tilo_startup_metrics::close_startup();
        }
        FrameOutcome::Presented
    }

    fn dropped(&self, why: &Refusal) -> FrameOutcome {
        match why {
            Refusal::Throttled => FrameOutcome::Skipped,
            Refusal::RenderFailed(reason) => {
                tracing::warn!(%reason, "the render service could not draw a frame");
                self.repaint();
                FrameOutcome::Skipped
            }
            Refusal::Invalid(reason) | Refusal::OverLimit(reason) => {
                tracing::error!(%reason, "the render service refused a frame");
                FrameOutcome::Skipped
            }
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "pixel rectangles of a surface; the service checks the range"
)]
fn wire_rect(rect: Rect) -> WireRect {
    WireRect {
        x0: rect.x0 as f32,
        y0: rect.y0 as f32,
        x1: rect.x1 as f32,
        y1: rect.y1 as f32,
    }
}

/// A frame as its record in the ring.
fn encode(session: &mut Session, job: &FrameJob<'_>, sequence: u64) -> Result<Vec<u8>, WireError> {
    let mut record = Vec::new();
    let mut writer = FrameWriter::new(
        &mut record,
        (SURFACE, sequence),
        (job.width, job.height),
        job.base_color,
        job.full,
    );
    if !job.full {
        let damage: Vec<WireRect> = job
            .damage
            .iter()
            .copied()
            .filter(|rect| rect.x1 > rect.x0 && rect.y1 > rect.y0)
            .map(wire_rect)
            .collect();
        writer.damage(&damage)?;
        let copies: Vec<ScrollCopy> = job
            .copies
            .iter()
            .map(|copy| ScrollCopy {
                x: copy.x,
                y: copy.y,
                width: copy.width,
                height: copy.height,
                dx: copy.dx,
                dy: copy.dy,
            })
            .collect();
        writer.scroll_copies(&copies)?;
    }
    writer.content(LayerId::ROOT, job.scene.encoding(), session)?;
    for region in &job.backdrop.regions {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a blur radius in pixels; the service checks the range"
        )]
        let backdrop = Backdrop {
            image: session.image(&region.image)?,
            rect: wire_rect(region.rect),
            blur_px: region.blur_px as f32,
            ground: region.ground.components,
            below: region.below.encoding().clone(),
        };
        writer.backdrop(&backdrop, session)?;
    }
    writer.finish()?;
    Ok(record)
}
