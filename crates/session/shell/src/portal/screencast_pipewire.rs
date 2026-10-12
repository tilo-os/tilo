//! Pipewire stream provisioning for the `ScreenCast` portal backend.
//!
//! [`ScreenCastStreamProvisioner`] is the surface the portal calls into to
//! allocate a Pipewire node and return its id + properties to the
//! frontend. Native production uses [`RealPipewireProvisioner`]; hosted and
//! headless paths use [`UnavailableScreenCastProvisioner`] and advertise no
//! source types rather than returning a synthetic node.
//! [`MockScreenCastProvisioner`] is test-only. `RealPipewireProvisioner` opens
//! a connection to the system daemon, creates a video stream of the
//! workspace's composed scene at the negotiated dimensions, and returns
//! the node id, which the compositor's frame loop pushes buffers into.
//!
//! Pipewire is a C-library system dependency. Tilo's native image includes it;
//! tests inject the mock and hosted mode remains explicitly unavailable.
//!
//! Frames come from the compositor's final composed scene, so local apps,
//! peer surfaces, shell chrome, and overlays have the same spatial result as
//! the display. Native dispatch suppresses fresh frames while the seat is
//! locked so the password surface and lock-screen metadata are never shared.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::sync::Once;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::Duration;

use zbus::zvariant::{OwnedValue, Value};

use super::PortalCaller;

/// What the provisioner needs to spin up a stream.
#[derive(Clone, Debug)]
pub struct PipewireStreamRequest {
    /// Identity of the requesting app — used for `pw.client.app.name` on the stream's property dict.
    pub caller: PortalCaller,
    /// Width in pixels of the stream's video frames, negotiated with the frontend during `SelectSources`.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Target framerate; v1 hard-codes to 30 unless the frontend asked for a specific rate.
    pub framerate: u32,
    /// The source type this stream captures: `1` monitor, `2` window, `4`
    /// virtual (see `ScreenCast.AvailableSourceTypes`), chosen from the
    /// user's `SelectSources` selection and echoed back on [`PipewireStreamInfo`].
    pub source_type: u32,
    /// Cursor embedding mode: `1` hidden, `2` embedded, `4` metadata (see
    /// `ScreenCast.AvailableCursorModes`) — the real provisioner uses this to decide whether to composite the cursor or ship it as metadata.
    pub cursor_mode: u32,
}

/// Production-shape description of a provisioned Pipewire stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipewireStreamInfo {
    /// Pipewire node id (uint32) — the frontend uses this with `pipewire-stream-1.0` to attach to the producer side.
    pub node_id: u32,
    /// Pipewire `source_type`: 1 = monitor, 2 = window, 4 = virtual. v1
    /// always emits 1 — sharing the workspace's rendered output as a single "monitor" surface.
    pub source_type: u32,
    /// `id` field — opaque, disambiguates multiple streams from the same node for the frontend. v1 always 0.
    pub id: u32,
    /// `(width, height)` of the stream's video frames. Producers use unsigned
    /// dimensions; the portal wire contract uses signed `(ii)`.
    pub size: (u32, u32),
    /// `(x, y)` position in the compositor coordinate space; per the
    /// `ScreenCast` spec **only** meaningful for monitor sources — [`Self::into_stream_tuple`] omits it otherwise.
    pub position: (i32, i32),
}

impl PipewireStreamInfo {
    /// Serialise into the `(node_id, properties_dict)` pair
    /// `ScreenCast.Start`'s `streams` field expects: `id`, `size`,
    /// `source_type`, and — for monitor sources — `position`.
    pub fn into_stream_tuple(self) -> (u32, HashMap<String, OwnedValue>) {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(Value::U32(self.id)) {
            props.insert("id".to_string(), v);
        }
        if let Ok(v) = OwnedValue::try_from(Value::U32(self.source_type)) {
            props.insert("source_type".to_string(), v);
        }
        // `ScreenCast.Start` requires a signed `(ii)` tuple. Saturate an
        // impossible oversized producer dimension rather than wrapping it
        // negative at the D-Bus boundary.
        let size = (
            i32::try_from(self.size.0).unwrap_or(i32::MAX),
            i32::try_from(self.size.1).unwrap_or(i32::MAX),
        );
        if let Ok(v) = OwnedValue::try_from(Value::new(size)) {
            props.insert("size".to_string(), v);
        }
        // `position` is only defined for monitor sources (source_type 1).
        if self.source_type == 1 {
            let pos = (self.position.0, self.position.1);
            if let Ok(v) = OwnedValue::try_from(Value::new(pos)) {
                props.insert("position".to_string(), v);
            }
        }
        (self.node_id, props)
    }
}

/// Recoverable errors from the provisioner. The portal layer turns these into `response=2` (other error) per the freedesktop.org portal contract.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProvisioningError {
    /// The Pipewire daemon couldn't be reached. Production logs and falls
    /// through to deny — the consent dialog succeeds but streaming fails with a clear error.
    #[error("pipewire daemon unreachable: {0}")]
    DaemonUnreachable(String),
    /// Stream creation failed mid-handshake — usually a format rejection by the daemon.
    #[error("pipewire stream setup failed: {0}")]
    StreamSetup(String),
    /// The compositor has no real `PipeWire` producer wired in. Production uses
    /// this error instead of returning a synthetic node that no client can
    /// consume; the mock provisioner is reserved for tests.
    #[error("screen capture is unavailable: {0}")]
    NotImplemented(String),
}

/// Host-side contract: provision a Pipewire stream the frontend can
/// attach to. Synchronous because the Pipewire C API is; production runs this from a tokio-blocking task or the compositor thread.
pub trait ScreenCastStreamProvisioner: Send + Sync {
    /// # Errors
    ///
    /// Returns `Err` when the Pipewire daemon can't be reached, stream
    /// setup fails mid-handshake, or the `pipewire` feature isn't
    /// compiled in ([`ProvisioningError::NotImplemented`]).
    fn provision_stream(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError>;

    /// Stop producing a previously provisioned node. The compositor-backed
    /// implementation uses this to detach its frame source when a portal
    /// session closes. Implementations predating lifecycle integration may
    /// leave the default no-op in place.
    fn release_stream(&self, _node_id: u32) {}

    /// Feed the most recently composed compositor frame to active nodes.
    /// Implementations intentionally retain only one frame, which bounds both
    /// latency and memory when a consumer is slower than the compositor.
    fn publish_frame(&self, _width: u32, _height: u32, _rgba: Arc<[u8]>) {}

    /// Whether this backend can actually create a source node.  The portal
    /// uses this to avoid advertising capture in hosted/headless sessions.
    fn is_available(&self) -> bool {
        false
    }

    /// Source types the concrete producer can truthfully emit.  This is kept
    /// on the producer (rather than a backend-wide constant) because a mock
    /// can exercise the complete protocol while the native renderer currently
    /// has one composed desktop output.
    fn available_source_types(&self) -> u32 {
        0
    }

    /// Cursor representations the concrete producer can truthfully emit.
    fn available_cursor_modes(&self) -> u32 {
        0
    }

    /// Whether any portal session currently owns a producer node.
    fn has_streams(&self) -> bool {
        false
    }
}

/// Cheap-clone wrapper around `Arc<dyn ScreenCastStreamProvisioner>`.
#[derive(Clone)]
pub struct ScreenCastStreamProvisionerHandle {
    inner: Arc<dyn ScreenCastStreamProvisioner>,
}

/// Honest production fallback for builds without a compositor-to-PipeWire
/// producer. It never advertises an inert node id or `/dev/null` as a remote.
/// Keeping this separate from [`MockScreenCastProvisioner`] makes it difficult
/// to accidentally ship the test double again.
pub struct UnavailableScreenCastProvisioner {
    reason: String,
}

impl UnavailableScreenCastProvisioner {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl ScreenCastStreamProvisioner for UnavailableScreenCastProvisioner {
    fn provision_stream(
        &self,
        _req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        Err(ProvisioningError::NotImplemented(self.reason.clone()))
    }
}

impl ScreenCastStreamProvisionerHandle {
    pub fn new(inner: Arc<dyn ScreenCastStreamProvisioner>) -> Self {
        Self { inner }
    }

    /// # Errors
    ///
    /// Returns `Err` under the same conditions as the wrapped
    /// [`ScreenCastStreamProvisioner::provision_stream`] implementation.
    pub fn provision(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        self.inner.provision_stream(req)
    }

    /// [`Self::provision`] from async code. Setting up a stream waits for the
    /// `PipeWire` producer thread to report its node (up to seconds), so it
    /// waits on the blocking pool instead of holding a portal worker.
    ///
    /// # Errors
    ///
    /// As [`Self::provision`]; a panicking provisioner reports a stream
    /// setup failure.
    pub async fn provision_blocking(
        &self,
        req: PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        let provisioner = self.clone();
        tokio::task::spawn_blocking(move || provisioner.provision(&req))
            .await
            .unwrap_or_else(|_| {
                Err(ProvisioningError::StreamSetup(
                    "stream provisioning panicked".into(),
                ))
            })
    }

    /// Release a stream as part of request/session teardown.
    pub fn release(&self, node_id: u32) {
        self.inner.release_stream(node_id);
    }

    pub fn publish_frame(&self, width: u32, height: u32, rgba: Arc<[u8]>) {
        self.inner.publish_frame(width, height, rgba);
    }

    pub fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    pub fn available_source_types(&self) -> u32 {
        self.inner.available_source_types()
    }

    pub fn available_cursor_modes(&self) -> u32 {
        self.inner.available_cursor_modes()
    }

    pub fn has_streams(&self) -> bool {
        self.inner.has_streams()
    }
}

/// One compositor frame retained by the producer.  It is immutable after
/// construction so PipeWire's realtime callback only copies from shared data;
/// it never touches compositor or widget state.
#[derive(Clone)]
#[cfg(any(test, target_os = "linux"))]
struct ComposedFrame {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

#[cfg(target_os = "linux")]
struct RealStream {
    stop: Arc<AtomicBool>,
}

/// Native compositor PipeWire producer.
///
/// Every portal source gets a separate PipeWire node and worker thread.  The
/// worker owns all PipeWire objects (they are deliberately !Send); the native
/// frame loop merely replaces a single shared latest-frame slot.  This gives
/// capture a strict one-frame queue, releases nodes when the portal Session is
/// closed, and prevents a slow consumer from accumulating desktop pixels.
#[cfg(target_os = "linux")]
pub struct RealPipewireProvisioner {
    latest: Arc<Mutex<Option<ComposedFrame>>>,
    streams: Arc<Mutex<HashMap<u32, RealStream>>>,
    /// Includes nodes still performing their bounded PipeWire setup, so a
    /// burst of concurrent portal starts cannot temporarily exceed the cap.
    slots: Arc<AtomicUsize>,
}

#[cfg(target_os = "linux")]
impl RealPipewireProvisioner {
    pub fn new() -> Result<Self, ProvisioningError> {
        ensure_pipewire_initialised();
        // Do a narrow availability probe now.  This keeps the ScreenCast
        // properties honest rather than accepting consent and failing later.
        let _ = pipewire_socket()?;
        Ok(Self {
            latest: Arc::new(Mutex::new(None)),
            streams: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(AtomicUsize::new(0)),
        })
    }
}

#[cfg(target_os = "linux")]
impl ScreenCastStreamProvisioner for RealPipewireProvisioner {
    fn provision_stream(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        if self
            .slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 64).then_some(count + 1)
            })
            .is_err()
        {
            return Err(ProvisioningError::StreamSetup(
                "too many concurrent screen capture streams".into(),
            ));
        }
        let latest = self.latest.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let request = req.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("tilo-pipewire-screen".into())
            .spawn(move || {
                run_pipewire_stream(request, latest, worker_stop, ready_tx);
            })
            .map_err(|error| {
                self.slots.fetch_sub(1, Ordering::AcqRel);
                ProvisioningError::StreamSetup(format!("spawn producer: {error}"))
            })?;
        let info = match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(info)) => info,
            Ok(Err(error)) => {
                self.slots.fetch_sub(1, Ordering::AcqRel);
                return Err(error);
            }
            Err(_) => {
                stop.store(true, Ordering::Release);
                self.slots.fetch_sub(1, Ordering::AcqRel);
                return Err(ProvisioningError::StreamSetup(
                    "producer did not become ready".into(),
                ));
            }
        };
        let mut streams = self.streams.lock().map_err(|_| {
            stop.store(true, Ordering::Release);
            self.slots.fetch_sub(1, Ordering::AcqRel);
            ProvisioningError::StreamSetup("producer registry poisoned".into())
        })?;
        if streams.contains_key(&info.node_id) {
            stop.store(true, Ordering::Release);
            self.slots.fetch_sub(1, Ordering::AcqRel);
            return Err(ProvisioningError::StreamSetup(
                "PipeWire reused a live screen capture node id".into(),
            ));
        }
        streams.insert(info.node_id, RealStream { stop });
        Ok(info)
    }

    fn release_stream(&self, node_id: u32) {
        if let Ok(mut streams) = self.streams.lock()
            && let Some(stream) = streams.remove(&node_id)
        {
            stream.stop.store(true, Ordering::Release);
            self.slots.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn publish_frame(&self, width: u32, height: u32, rgba: Arc<[u8]>) {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if width == 0 || height == 0 || expected != Some(rgba.len()) {
            return;
        }
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(ComposedFrame {
                width,
                height,
                rgba,
            });
        }
    }

    fn is_available(&self) -> bool {
        pipewire_socket().is_ok()
    }

    fn available_source_types(&self) -> u32 {
        // Tilo produces exactly one node from the compositor's composed
        // desktop scene.  Do not label that node as a window or a virtual
        // monitor: xdg-desktop-portal uses this bit to describe what the
        // user actually approved.
        1 // ScreenCast source_type::MONITOR
    }

    fn available_cursor_modes(&self) -> u32 {
        // The frame bridge intentionally sends the clean scene and does not
        // yet maintain a separate PipeWire cursor-metadata stream.  Hidden is
        // the only representation it can promise without leaking stale seat
        // coordinates or lying about embedded pixels.
        1 // ScreenCast cursor_mode::HIDDEN
    }

    fn has_streams(&self) -> bool {
        self.streams.lock().is_ok_and(|streams| !streams.is_empty())
    }
}

#[cfg(target_os = "linux")]
fn ensure_pipewire_initialised() {
    static INIT: Once = Once::new();
    INIT.call_once(pipewire::init);
}

#[cfg(target_os = "linux")]
fn pipewire_socket() -> Result<UnixStream, ProvisioningError> {
    let runtime = tilo_paths::xdg::runtime_dir().ok_or_else(|| {
        ProvisioningError::DaemonUnreachable("XDG_RUNTIME_DIR is unset or not absolute".into())
    })?;
    let remote = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| "pipewire-0".into());
    let path = if remote.starts_with('/') {
        std::path::PathBuf::from(remote)
    } else {
        runtime.join(remote)
    };
    UnixStream::connect(&path).map_err(|error| {
        ProvisioningError::DaemonUnreachable(format!("connect {}: {error}", path.display()))
    })
}

/// Copy the most recent compositor scene into a negotiated PipeWire buffer.
/// PipeWire stream dimensions are negotiated per consumer, whereas Tilo paints
/// one physical desktop-sized frame.  Nearest-neighbour is deliberately
/// allocation-free in the realtime callback; the producer's shared input slot
/// stays bounded to one full-resolution frame.
#[cfg(any(test, target_os = "linux"))]
fn copy_rgba_to_negotiated_buffer(
    destination: &mut [u8],
    frame: &ComposedFrame,
    target_width: u32,
    target_height: u32,
) -> usize {
    let target_row = match usize::try_from(target_width)
        .ok()
        .and_then(|width| width.checked_mul(4))
    {
        Some(row) if row > 0 => row,
        _ => return 0,
    };
    let source_row = match usize::try_from(frame.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
    {
        Some(row) if row > 0 => row,
        _ => return 0,
    };
    let source_needed = usize::try_from(frame.height)
        .ok()
        .and_then(|height| source_row.checked_mul(height));
    if frame.height == 0 || !source_needed.is_some_and(|needed| frame.rgba.len() >= needed) {
        return 0;
    }
    let rows = (destination.len() / target_row).min(target_height as usize);
    if frame.width == target_width && frame.height == target_height {
        let bytes = rows.saturating_mul(target_row);
        destination[..bytes].copy_from_slice(&frame.rgba[..bytes]);
        return bytes;
    }
    for y in 0..rows {
        let source_y = y.saturating_mul(frame.height as usize) / target_height as usize;
        let target_start = y.saturating_mul(target_row);
        let source_start = source_y.saturating_mul(source_row);
        for x in 0..target_width as usize {
            let source_x = x.saturating_mul(frame.width as usize) / target_width as usize;
            let source = source_start + source_x * 4;
            let target = target_start + x * 4;
            destination[target..target + 4].copy_from_slice(&frame.rgba[source..source + 4]);
        }
    }
    rows.saturating_mul(target_row)
}

#[cfg(target_os = "linux")]
fn run_pipewire_stream(
    request: PipewireStreamRequest,
    latest: Arc<Mutex<Option<ComposedFrame>>>,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::SyncSender<Result<PipewireStreamInfo, ProvisioningError>>,
) {
    use pipewire as pw;
    use pw::properties::properties;
    use pw::spa;
    use pw::spa::pod::Pod;

    ensure_pipewire_initialised();
    let initial = latest.lock().ok().and_then(|frame| frame.clone());
    let width = initial
        .as_ref()
        .map_or(request.width, |frame| frame.width)
        .clamp(1, 8192);
    let height = initial
        .as_ref()
        .map_or(request.height, |frame| frame.height)
        .clamp(1, 8192);
    let framerate = request.framerate.clamp(1, 120);
    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(loop_) => loop_,
        Err(error) => {
            let _ = ready.send(Err(ProvisioningError::DaemonUnreachable(error.to_string())));
            return;
        }
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(context) => context,
        Err(error) => {
            let _ = ready.send(Err(ProvisioningError::DaemonUnreachable(error.to_string())));
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(core) => core,
        Err(error) => {
            let _ = ready.send(Err(ProvisioningError::DaemonUnreachable(error.to_string())));
            return;
        }
    };
    let stream = match pw::stream::StreamBox::new(
        &core,
        "Tilo ScreenCast",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_DESCRIPTION => "Tilo screen share",
            *pw::keys::APP_NAME => request.caller.app_id.as_str(),
        },
    ) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(ProvisioningError::StreamSetup(error.to_string())));
            return;
        }
    };
    let callback_frame = latest.clone();
    let _listener = match stream
        .add_local_listener::<()>()
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let Some(destination) = data.data() else {
                return;
            };
            let frame = callback_frame.lock().ok().and_then(|frame| frame.clone());
            let Some(frame) = frame else {
                destination.fill(0);
                return;
            };
            let bytes = copy_rgba_to_negotiated_buffer(destination, &frame, width, height);
            if bytes < destination.len() {
                destination[bytes..].fill(0);
            }
            // `libspa` currently exposes a read-only raw view for Chunk.  The
            // PipeWire callback owns this buffer, so updating the negotiated
            // byte count here is the documented C API operation.
            unsafe {
                let chunk = data.chunk_mut().as_raw() as *const _ as *mut pw::spa::sys::spa_chunk;
                (*chunk).offset = 0;
                (*chunk).size = bytes as u32;
                (*chunk).stride = (width.saturating_mul(4)) as i32;
            }
        })
        .register()
    {
        Ok(listener) => listener,
        Err(error) => {
            let _ = ready.send(Err(ProvisioningError::StreamSetup(error.to_string())));
            return;
        }
    };
    let object = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Id,
            pw::spa::param::video::VideoFormat::RGBA
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            pw::spa::utils::Rectangle { width, height }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Fraction,
            pw::spa::utils::Fraction {
                num: framerate,
                denom: 1
            }
        ),
    );
    let values = match pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(object),
    ) {
        Ok(values) => values.0.into_inner(),
        Err(error) => {
            let _ = ready.send(Err(ProvisioningError::StreamSetup(error.to_string())));
            return;
        }
    };
    let parameter = match Pod::from_bytes(&values) {
        Some(parameter) => parameter,
        None => {
            let _ = ready.send(Err(ProvisioningError::StreamSetup(
                "PipeWire rejected the stream format parameter".into(),
            )));
            return;
        }
    };
    let mut params = [parameter];
    if let Err(error) = stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    ) {
        let _ = ready.send(Err(ProvisioningError::StreamSetup(error.to_string())));
        return;
    }
    // Node creation is asynchronous.  Give the daemon a bounded two seconds
    // to assign the id before the portal reports failure.
    for _ in 0..40 {
        let _ = mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
        let node_id = stream.node_id();
        if node_id != 0 && node_id != u32::MAX {
            let info = PipewireStreamInfo {
                node_id,
                source_type: request.source_type,
                id: node_id,
                size: (width, height),
                position: (0, 0),
            };
            if ready.send(Ok(info)).is_err() {
                let _ = stream.disconnect();
                return;
            }
            while !stop.load(Ordering::Acquire) {
                let _ = mainloop
                    .loop_()
                    .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(16)));
            }
            let _ = stream.disconnect();
            return;
        }
    }
    let _ = stream.disconnect();
    let _ = ready.send(Err(ProvisioningError::StreamSetup(
        "PipeWire did not assign a screen-share node".into(),
    )));
}

/// Mock provisioner — returns a fixed node id for portal protocol tests.
pub struct MockScreenCastProvisioner {
    next_node: Mutex<u32>,
    /// Tracks the requests seen so tests can assert on the caller + dimensions the backend forwarded.
    seen: Mutex<Vec<PipewireStreamRequest>>,
    name_hint: String,
}

impl MockScreenCastProvisioner {
    /// Build a mock whose first response carries `start_node_id`; subsequent responses increment by 1.
    pub fn new_with_node(start_node_id: u32, name_hint: &str) -> Self {
        Self {
            next_node: Mutex::new(start_node_id),
            seen: Mutex::new(Vec::new()),
            name_hint: name_hint.to_string(),
        }
    }

    /// Read-only access to the recorded request log.
    pub fn requests(&self) -> Vec<PipewireStreamRequest> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Read-only access to the descriptive label set at construction time —
    /// used by tests asserting the mock's wiring without duplicating the name.
    pub fn name_hint(&self) -> &str {
        &self.name_hint
    }
}

impl ScreenCastStreamProvisioner for MockScreenCastProvisioner {
    fn provision_stream(
        &self,
        req: &PipewireStreamRequest,
    ) -> Result<PipewireStreamInfo, ProvisioningError> {
        if let Ok(mut s) = self.seen.lock() {
            s.push(req.clone());
        }
        let node_id = {
            let mut n = self
                .next_node
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = *n;
            *n = n.wrapping_add(1);
            id
        };
        Ok(PipewireStreamInfo {
            node_id,
            // Echo the requested source type so a window/virtual selection
            // round-trips through the stream props instead of being silently downgraded to monitor.
            source_type: req.source_type,
            id: 0,
            size: (req.width, req.height),
            position: (0, 0),
        })
    }

    fn is_available(&self) -> bool {
        true
    }

    fn available_source_types(&self) -> u32 {
        1 | 2 | 4
    }

    fn available_cursor_modes(&self) -> u32 {
        1 | 2 | 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;

    #[test]
    fn pipewire_provisioners_are_real_or_recorded_never_synthetic() {
        // Phase `mock_returns_incrementing_node_ids`.
        {
            let m = MockScreenCastProvisioner::new_with_node(7, "test");
            let req = PipewireStreamRequest {
                caller: PortalCaller::new(WorkspaceId(1), "app"),
                width: 1920,
                height: 1080,
                framerate: 30,
                source_type: 1,
                cursor_mode: 2,
            };
            let a = m.provision_stream(&req).expect("first stream");
            let b = m.provision_stream(&req).expect("second stream");
            assert_eq!(a.node_id, 7);
            assert_eq!(b.node_id, 8);
            // size matches the request.
            assert_eq!(a.size, (1920, 1080));
            // source_type 1 == monitor — echoed from the request.
            assert_eq!(a.source_type, 1);
        }
        // Phase `mock_records_requests`.
        {
            let m = Arc::new(MockScreenCastProvisioner::new_with_node(1, "test"));
            let h = ScreenCastStreamProvisionerHandle::new(m.clone());
            let req = PipewireStreamRequest {
                caller: PortalCaller::new(WorkspaceId(5), "org.example"),
                width: 1280,
                height: 720,
                framerate: 30,
                source_type: 1,
                cursor_mode: 2,
            };
            let _ = h.provision(&req).unwrap();
            let seen = m.requests();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].caller.app_id, "org.example");
            assert_eq!(seen[0].width, 1280);
        }
        // Phase `mock_echoes_requested_source_type`.
        {
            let m = MockScreenCastProvisioner::new_with_node(1, "test");
            let mut req = PipewireStreamRequest {
                caller: PortalCaller::new(WorkspaceId(1), "app"),
                width: 100,
                height: 100,
                framerate: 30,
                source_type: 4, // virtual
                cursor_mode: 4,
            };
            let info = m.provision_stream(&req).unwrap();
            assert_eq!(info.source_type, 4, "virtual selection round-trips");
            req.source_type = 2; // window
            let info2 = m.provision_stream(&req).unwrap();
            assert_eq!(info2.source_type, 2);
        }
        // Phase `unavailable_provisioner_never_returns_a_synthetic_stream`.
        {
            let p = UnavailableScreenCastProvisioner::new("capture producer missing");
            let req = PipewireStreamRequest {
                caller: PortalCaller::new(WorkspaceId(8), "org.example"),
                width: 1280,
                height: 720,
                framerate: 30,
                source_type: 1,
                cursor_mode: 2,
            };

            assert!(matches!(
                p.provision_stream(&req),
                Err(ProvisioningError::NotImplemented(reason))
                    if reason == "capture producer missing"
            ));
        }
    }

    #[test]
    fn stream_info_serialises_required_keys_and_bounded_scaling() {
        // Phase `stream_info_serialises_into_dict_with_required_keys`.
        {
            let info = PipewireStreamInfo {
                node_id: 42,
                source_type: 1,
                id: 0,
                size: (640, 480),
                position: (100, 200),
            };
            let (node_id, props) = info.into_stream_tuple();
            assert_eq!(node_id, 42);
            // The freedesktop ScreenCast spec requires `size`; `id`/`source_type`
            // are optional but xdg-desktop-portal's frontends consume them.
            assert!(props.contains_key("size"));
            let size: (i32, i32) = props
                .get("size")
                .unwrap()
                .try_clone()
                .unwrap()
                .try_into()
                .unwrap();
            assert_eq!(size, (640, 480), "wire type must be standard (ii)");
            assert!(props.contains_key("source_type"));
            assert!(props.contains_key("id"));
            // `position` is present for monitor sources.
            assert!(props.contains_key("position"));
        }
        // Phase `stream_info_omits_position_for_window_source`.
        {
            // Window (2) / virtual (4) sources have no compositor-space position; the spec says `position` is monitor-only.
            let info = PipewireStreamInfo {
                node_id: 9,
                source_type: 2,
                id: 0,
                size: (800, 600),
                position: (5, 5),
            };
            let (_node, props) = info.into_stream_tuple();
            assert!(!props.contains_key("position"));
            assert!(props.contains_key("size"));
        }
        // Phase `negotiated_buffer_scaling_is_bounded_and_preserves_corner_pixels`.
        {
            let frame = ComposedFrame {
                width: 2,
                height: 2,
                rgba: Arc::from([
                    1, 2, 3, 255, // top left
                    4, 5, 6, 255, // top right
                    7, 8, 9, 255, // bottom left
                    10, 11, 12, 255, // bottom right
                ]),
            };
            let mut destination = [0_u8; 4 * 4 * 4];
            let bytes = copy_rgba_to_negotiated_buffer(&mut destination, &frame, 4, 4);
            assert_eq!(bytes, destination.len());
            assert_eq!(&destination[..4], &[1, 2, 3, 255]);
            assert_eq!(&destination[12..16], &[4, 5, 6, 255]);
            assert_eq!(&destination[48..52], &[7, 8, 9, 255]);
            assert_eq!(&destination[60..64], &[10, 11, 12, 255]);
        }
    }
}
