//! `wl_data_device` + `zwp_primary_selection_device_v1` on the app's own
//! Wayland connection — the native clipboard for a Wayland app; see
//! the crate docs for why this, rather than `arboard`, serves Wayland.

use std::sync::Arc;

use crate::{
    ClipboardBackend, ClipboardImage, ClipboardTarget, MAX_CLIPBOARD_IMAGE_BYTES,
    MAX_CLIPBOARD_IMAGE_INPUT_BYTES,
};

/// The image another app put on the clipboard, read when the selection
/// changed so a paste never waits on the copying app.
type PastedImage = Arc<std::sync::Mutex<Option<ClipboardImage>>>;

/// A completed same-offer rich selection. Constructed on the owning host so
/// completion wakes that host through its existing wake scope.
struct RichClipboard {
    offer: std::sync::Mutex<Option<(u64, crate::RichTextOffer)>>,
    generation: std::sync::atomic::AtomicU64,
    wake: tilo_rt::wake::WakeHandle,
}

impl RichClipboard {
    fn new() -> Self {
        Self {
            offer: std::sync::Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            wake: tilo_rt::wake::WakeHandle::new(),
        }
    }

    fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    fn invalidate(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Ok(mut offer) = self.offer.lock() {
            *offer = None;
        }
        self.wake.wake();
    }

    fn publish(&self, generation: u64, value: crate::RichTextOffer) {
        if let Ok(mut offer) = self.offer.lock() {
            if self.generation() != generation {
                return;
            }
            *offer = Some((generation, value));
        }
        self.wake.wake();
    }

    fn read(&self) -> Option<crate::RichTextOffer> {
        let cached = self.offer.try_lock().ok()?;
        let (generation, offer) = cached.as_ref()?;
        if *generation != self.generation() {
            return None;
        }
        let result = offer.clone();
        let generation = *generation;
        drop(cached);
        (generation == self.generation()).then_some(result)
    }
}

const CLIPBOARD_READ_BUDGET: tilo_clock::Duration = tilo_clock::Duration::from_millis(40);

/// How long an explicit paste waits for the clipboard's owner to send its
/// text.
const EXPLICIT_READ_BUDGET: tilo_clock::Duration = tilo_clock::Duration::from_secs(2);

/// How long a copy may wait for the compositor before it is refused. A copy
/// is two compositor roundtrips (the focus check, then the selection's
/// confirmation), and each waits for an iteration of the compositor's loop,
/// which also paints; the compositor itself only calls an iteration slow
/// beyond 100 ms. The app's UI thread does not wait: the outcome settles a
/// [`crate::ClipboardCopy`].
const PUBLICATION_BUDGET: tilo_clock::Duration = tilo_clock::Duration::from_millis(250);

fn publication_deadline() -> tilo_clock::Instant {
    let now = tilo_clock::now();
    now.checked_add(PUBLICATION_BUDGET).unwrap_or(now)
}

/// What a copy publishes.
enum PublicationPayload {
    Text(Vec<u8>),
    SecretText(Vec<u8>),
    /// Formatted text: its HTML and its plain-text fallback.
    Rich {
        html: Vec<u8>,
        plain: Vec<u8>,
    },
    Image(Vec<u8>),
}

/// Reports a copy's outcome once. Dropped unreported (the worker went away),
/// it reports a failure.
struct Settle(Option<Box<dyn FnOnce(bool) + Send>>);

impl Settle {
    fn new(report: impl FnOnce(bool) + Send + 'static) -> Self {
        Self(Some(Box::new(report)))
    }

    fn settle(mut self, copied: bool) {
        if let Some(report) = self.0.take() {
            report(copied);
        }
    }
}

impl Drop for Settle {
    fn drop(&mut self) {
        if let Some(report) = self.0.take() {
            report(false);
        }
    }
}

/// A [`Settle`] the focus check, the confirmation and the deadline share;
/// the first to report wins.
type SharedSettle = Arc<std::sync::Mutex<Option<Settle>>>;

fn settle_once(settle: &SharedSettle, copied: bool) {
    let taken = settle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(settle) = taken {
        settle.settle(copied);
    }
}

/// A queued copy and the decision on it, which the worker's focus check and
/// its deadline race to make exactly once.
struct Publication {
    payload: PublicationPayload,
    settle: Settle,
    deadline: tilo_clock::Instant,
}

impl Publication {
    fn new(payload: PublicationPayload, settle: Settle, deadline: tilo_clock::Instant) -> Self {
        Self {
            payload,
            settle,
            deadline,
        }
    }
}

/// Whether the worker took a copy (and set the selection) or its deadline
/// passed first. Whichever comes first decides: a copy the compositor got is
/// never reported as failed, and one reported as failed never becomes the
/// selection later.
#[derive(Default)]
struct PublicationClaim(std::sync::atomic::AtomicU8);

impl PublicationClaim {
    const PENDING: u8 = 0;
    const TAKEN: u8 = 1;
    const ABANDONED: u8 = 2;

    fn decide(&self, outcome: u8) -> bool {
        use std::sync::atomic::Ordering;
        self.0
            .compare_exchange(Self::PENDING, outcome, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// The worker publishes, unless the deadline already gave up on it.
    fn take(&self) -> bool {
        self.decide(Self::TAKEN)
    }

    /// The deadline gives up, unless the worker already published.
    fn abandon(&self) -> bool {
        self.decide(Self::ABANDONED)
    }

    fn taken(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire) == Self::TAKEN
    }
}

fn recv_bounded<T>(
    receiver: &std::sync::mpsc::Receiver<T>,
    budget: tilo_clock::Duration,
) -> Option<T> {
    receiver.recv_timeout(budget).ok()
}

// Field order matters: smithay joins its protocol worker before releasing
// the display owner. Detached loaders clone this entire wrapper, not just the
// mutex, so they cannot outlive the connection they use.
struct DisplayReader {
    clipboard: std::sync::Mutex<smithay_clipboard::Clipboard>,
    _display: Option<Arc<dyn Send + Sync>>,
}

/// `wl_data_device` + `zwp_primary_selection_device_v1` on the app's own
/// Wayland connection. The focused client's connection is required for the
/// compositor to accept copies; a separate connection cannot substitute.
/// Prefer [`Self::from_owned_display`] so every protocol worker and retained
/// loader owns the display it uses.
pub struct WaylandClipboard {
    /// `smithay_clipboard::Clipboard` owns an mpsc receiver, so it is `Send`
    /// but not `Sync`. Each target has its own instance and read thread: a
    /// peer that never closes its clipboard pipe can therefore stall at most
    /// one bounded reader and can never starve the other target.
    read_clipboard: Option<Arc<DisplayReader>>,
    read_primary: Option<Arc<DisplayReader>>,
    write_inner: Option<Arc<DisplayReader>>,
    publication: PublicationClipboard,
    clipboard_cache: Arc<std::sync::Mutex<Option<String>>>,
    primary_cache: Arc<std::sync::Mutex<Option<String>>>,
    clipboard_refresh_inflight: Arc<std::sync::atomic::AtomicBool>,
    primary_refresh_inflight: Arc<std::sync::atomic::AtomicBool>,
    clipboard_refresh_generation: Arc<std::sync::atomic::AtomicU64>,
    primary_refresh_generation: Arc<std::sync::atomic::AtomicU64>,
    pasted: PastedImage,
    rich: Arc<RichClipboard>,
    wake: tilo_rt::wake::WakeHandle,
    explicit_read_budget: tilo_clock::Duration,
}

struct PublicationClipboard {
    sender: smithay_client_toolkit::reexports::calloop::channel::Sender<ClipboardCommand>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PublicationClipboard {
    /// Stop the worker and wait until it has. The worker drives the
    /// application's own `wl_display`. Once initialized, Exit wakes its
    /// event loop and its pipes are nonblocking sources. Initialization can
    /// still wait on a registry roundtrip, so runner attachments arrange
    /// for this wait off the UI thread while retaining the display owner.
    fn shut_down(&self) {
        let _ = self.sender.send(ClipboardCommand::Exit);
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

enum ClipboardCommand {
    Publish(Publication),
    Exit,
    Fence(std::sync::mpsc::Sender<()>),
    ReadText(std::sync::mpsc::Sender<Option<String>>, tilo_clock::Instant),
    #[cfg(test)]
    CommandProcessed(std::sync::mpsc::Sender<()>),
}

impl WaylandClipboard {
    /// Attach to the `wl_display` behind `display_ptr`.
    ///
    /// # Safety
    ///
    /// `display_ptr` must remain live until this value AND every outstanding
    /// loader have finished. A timed-out text read can retain a loader after
    /// this value is dropped. Use [`Self::from_owned_display`] when that
    /// independent lifetime cannot be guaranteed.
    pub unsafe fn new(display_ptr: *mut std::ffi::c_void) -> Self {
        unsafe { Self::new_with_owner(display_ptr, None) }
    }

    /// Attach while retaining the actual display owner for every worker,
    /// including loaders that survive runner close.
    pub fn from_owned_display(
        handle: impl raw_window_handle::HasDisplayHandle + Send + Sync + 'static,
    ) -> Option<Self> {
        let owner = Arc::new(handle);
        let display = owner.display_handle().ok()?;
        let raw_window_handle::RawDisplayHandle::Wayland(display) = display.as_raw() else {
            return None;
        };
        let pointer = display.display.as_ptr();
        // SAFETY: each foreign-display worker retains an owner clone until
        // its protocol objects have been destroyed and its thread joined.
        Some(unsafe { Self::new_with_owner(pointer, Some(owner)) })
    }

    unsafe fn new_with_owner(
        display_ptr: *mut std::ffi::c_void,
        owner: Option<Arc<dyn Send + Sync>>,
    ) -> Self {
        let reader = || {
            Some(Arc::new(DisplayReader {
                clipboard: std::sync::Mutex::new(unsafe {
                    smithay_clipboard::Clipboard::new(display_ptr)
                }),
                _display: owner.clone(),
            }))
        };
        let (read_clipboard, read_primary, write_inner) = (reader(), reader(), reader());
        let mut clipboard = unsafe {
            Self::with_publication_worker(
                display_ptr,
                owner,
                CLIPBOARD_READ_BUDGET,
                "tilo-wayland-clipboard",
            )
        };
        clipboard.read_clipboard = read_clipboard;
        clipboard.read_primary = read_primary;
        clipboard.write_inner = write_inner;
        clipboard
    }

    /// Only the publication worker, on its own backend for the display. It
    /// shares the application's socket but never its event queue: dispatching
    /// that queue could consume an application's roundtrip reply just before
    /// the application waits for it.
    ///
    /// # Safety
    ///
    /// `display_ptr` must outlive the worker thread, unless `owner` keeps it
    /// alive.
    unsafe fn with_publication_worker(
        display_ptr: *mut std::ffi::c_void,
        owner: Option<Arc<dyn Send + Sync>>,
        rich_read_budget: tilo_clock::Duration,
        name: &str,
    ) -> Self {
        let backend = unsafe {
            smithay_client_toolkit::reexports::client::backend::Backend::from_foreign_display(
                display_ptr.cast(),
            )
        };
        let connection =
            smithay_client_toolkit::reexports::client::Connection::from_backend(backend);
        let (sender, receiver) = smithay_client_toolkit::reexports::calloop::channel::channel();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_alive = std::sync::Arc::clone(&alive);
        let pasted = PastedImage::default();
        let worker_pasted = Arc::clone(&pasted);
        let rich = Arc::new(RichClipboard::new());
        let worker_rich = Arc::clone(&rich);
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                image_worker_impl::run(
                    connection,
                    receiver,
                    worker_pasted,
                    worker_rich,
                    rich_read_budget,
                );
                drop(owner);
                worker_alive.store(false, std::sync::atomic::Ordering::Release);
            })
            .ok();
        if thread.is_none() {
            alive.store(false, std::sync::atomic::Ordering::Release);
        }
        Self {
            read_clipboard: None,
            read_primary: None,
            write_inner: None,
            publication: PublicationClipboard {
                sender,
                thread: std::sync::Mutex::new(thread),
                alive,
            },
            clipboard_cache: Arc::new(std::sync::Mutex::new(None)),
            primary_cache: Arc::new(std::sync::Mutex::new(None)),
            clipboard_refresh_inflight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            primary_refresh_inflight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            clipboard_refresh_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            primary_refresh_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            pasted,
            rich,
            wake: tilo_rt::wake::WakeHandle::new(),
            explicit_read_budget: EXPLICIT_READ_BUDGET,
        }
    }

    /// Construct a publication backend around an already-connected Wayland
    /// client. This is intentionally public only as a protocol test seam: the
    /// normal application path uses [`Self::new`] with winit's live display.
    #[doc(hidden)]
    pub fn new_for_wayland_test(
        connection: smithay_client_toolkit::reexports::client::Connection,
    ) -> Self {
        Self::new_for_wayland_test_with_read_budget(connection, CLIPBOARD_READ_BUDGET)
    }

    /// Wait until the compositor has processed this worker's seat and device
    /// bindings before a protocol fixture maps its window and acquires focus.
    #[doc(hidden)]
    pub fn wait_ready_for_wayland_test(&self, budget: tilo_clock::Duration) -> bool {
        let Some(end) = tilo_clock::now().checked_add(budget) else {
            return false;
        };
        // The first reply follows seat capabilities; their callbacks bind the
        // keyboard and data device. The second follows those new requests.
        for _ in 0..2 {
            let (reply, received) = std::sync::mpsc::channel();
            if self
                .publication
                .sender
                .send(ClipboardCommand::Fence(reply))
                .is_err()
                || recv_bounded(&received, end.saturating_duration_since(tilo_clock::now()))
                    .is_none()
            {
                return false;
            }
        }
        true
    }

    /// Use the production worker with an explicit transfer deadline in protocol
    /// tests. Cancellation tests must finish before this deadline, so expiry
    /// cannot satisfy an assertion that focus loss or NULL cleared an offer.
    #[doc(hidden)]
    pub fn new_for_wayland_test_with_read_budget(
        connection: smithay_client_toolkit::reexports::client::Connection,
        rich_read_budget: tilo_clock::Duration,
    ) -> Self {
        let display_ptr = connection.backend().display_ptr().cast();
        // SAFETY: the worker retains the connection that owns the display.
        let mut clipboard = unsafe {
            Self::with_publication_worker(
                display_ptr,
                Some(Arc::new(connection)),
                rich_read_budget,
                "tilo-wayland-clipboard-test",
            )
        };
        // An explicit paste is bounded by the longer of the two, so a
        // test's budget also holds for its explicit pastes.
        clipboard.explicit_read_budget = rich_read_budget.max(EXPLICIT_READ_BUDGET);
        clipboard
    }
}

pub(crate) fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || rgba.len() > MAX_CLIPBOARD_IMAGE_INPUT_BYTES {
        return None;
    }
    let width_u32 = u32::try_from(width).ok()?;
    let height_u32 = u32::try_from(height).ok()?;
    let expected_len = width.checked_mul(height)?.checked_mul(4)?;
    if rgba.len() != expected_len {
        return None;
    }
    let mut bytes = Vec::new();
    {
        use image::ImageEncoder as _;
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(rgba, width_u32, height_u32, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    (bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES).then_some(bytes)
}

mod image_worker_impl {
    const MAX_INFLIGHT_TRANSFERS: usize = 16;
    const MAX_INFLIGHT_BYTES: usize = 32 * 1024 * 1024;
    use std::collections::HashMap;
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    use std::sync::Arc;

    use sctk::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
    use sctk::data_device_manager::data_offer::{DataOfferData, DataOfferHandler, DragOffer};
    use sctk::data_device_manager::data_source::DataSourceHandler;
    use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
    use sctk::reexports::calloop::{self, EventLoop, LoopHandle, PostAction};
    use sctk::reexports::calloop_wayland_source::WaylandSource;
    use sctk::reexports::client::backend::ObjectId;
    use sctk::reexports::client::globals::registry_queue_init;
    use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
    use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
    use sctk::reexports::client::protocol::wl_data_offer::WlDataOffer;
    use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
    use sctk::reexports::client::protocol::wl_keyboard::WlKeyboard;
    use sctk::reexports::client::protocol::wl_pointer::WlPointer;
    use sctk::reexports::client::protocol::wl_seat::WlSeat;
    use sctk::reexports::client::protocol::wl_touch::WlTouch;
    use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
    use sctk::registry::{ProvidesRegistryState, RegistryState};
    use sctk::seat::pointer::{PointerData, PointerEvent, PointerEventKind, PointerHandler};
    use sctk::seat::touch::{TouchData, TouchHandler};
    use sctk::seat::{Capability, SeatHandler, SeatState};
    use sctk::{delegate_pointer, delegate_registry, delegate_seat, delegate_touch};
    use smithay_client_toolkit as sctk;

    use super::{
        ClipboardCommand, PastedImage, Publication, PublicationClaim, PublicationPayload,
        RichClipboard, SharedSettle, settle_once,
    };

    #[derive(Debug, Default)]
    struct SeatData {
        keyboard: Option<WlKeyboard>,
        pointer: Option<WlPointer>,
        touch: Option<WlTouch>,
        data_device: Option<DataDevice>,
        keyboard_focus: bool,
        rich_offer_seen: bool,
        latest_serial: u32,
    }

    struct State {
        data_device_manager: DataDeviceManagerState,
        registry: RegistryState,
        seat_registry: SeatState,
        seats: HashMap<WlSeat, SeatData>,
        latest_seat: Option<WlSeat>,
        queue_handle: QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
        data_sources: Vec<sctk::data_device_manager::data_source::CopyPasteSource>,
        source_payloads: HashMap<ObjectId, SourcePayload>,
        text_contents: Option<Arc<[u8]>>,
        /// Whether `text_contents` is a secret, offered with the markers that
        /// keep it out of clipboard histories and device sync.
        text_secret: bool,
        /// The formatting of the text copied, when the copy was rich.
        html_contents: Option<Arc<[u8]>>,
        pending_image: Option<Arc<[u8]>>,
        inflight_transfers: usize,
        inflight_bytes: usize,
        /// The image on the current selection, once read.
        pasted: PastedImage,
        /// Counts selections, so a slow read of an older one never lands.
        selection_generation: u64,
        rich: Arc<RichClipboard>,
        rich_seat: Option<WlSeat>,
        rich_active_seat: Option<WlSeat>,
        rich_pending: Option<PendingRich>,
        plain_pending: Option<PendingPlain>,
        rich_read_budget: tilo_clock::Duration,
        /// Dispatched claims must be settled even when the shared display
        /// retains their callback userdata after this worker stops.
        pending_publications: HashMap<usize, (Arc<PublicationClaim>, SharedSettle)>,
        exit: bool,
    }

    pub(super) struct SourcePayload {
        pub(super) image: Option<Arc<[u8]>>,
        pub(super) text: Option<Arc<[u8]>>,
        /// The formatting of a rich copy, offered as `text/html`.
        pub(super) html: Option<Arc<[u8]>>,
        pub(super) secret: bool,
    }

    impl SourcePayload {
        pub(super) fn bytes_for_mime(&self, mime: &str) -> Option<Arc<[u8]>> {
            match mime {
                "image/png" => self.image.clone(),
                "text/html" | "text/html;charset=utf-8" => self.html.clone(),
                "text/plain;charset=utf-8" | "text/plain" | "UTF8_STRING" | "STRING" | "TEXT" => {
                    self.text.clone()
                }
                crate::SECRET_MIME | crate::PASSWORD_MANAGER_HINT_MIME if self.secret => {
                    Some(Arc::from(crate::SECRET_MARKER))
                }
                _ => None,
            }
        }
    }

    pub fn run(
        connection: Connection,
        commands: calloop::channel::Channel<ClipboardCommand>,
        pasted: PastedImage,
        rich: Arc<RichClipboard>,
        rich_read_budget: tilo_clock::Duration,
    ) {
        struct ClearRich(Arc<RichClipboard>);
        impl Drop for ClearRich {
            fn drop(&mut self) {
                self.0.invalidate();
            }
        }
        let _clear = ClearRich(Arc::clone(&rich));
        let Ok((globals, queue)) = registry_queue_init(&connection) else {
            return;
        };
        let Ok(mut event_loop) = EventLoop::<State>::try_new() else {
            return;
        };
        let loop_handle = event_loop.handle();
        let qh = queue.handle();
        let Ok(data_device_manager) = DataDeviceManagerState::bind(&globals, &qh) else {
            return;
        };
        let seat_state = SeatState::new(&globals, &qh);
        #[allow(
            clippy::mutable_key_type,
            reason = "Wayland object identity is the protocol key used by SCTK callbacks"
        )]
        let seats = seat_state
            .seats()
            .map(|seat| (seat, SeatData::default()))
            .collect();
        let mut state = State {
            data_device_manager,
            registry: RegistryState::new(&globals),
            seat_registry: seat_state,
            seats,
            latest_seat: None,
            queue_handle: qh,
            loop_handle: loop_handle.clone(),
            data_sources: Vec::new(),
            source_payloads: HashMap::new(),
            text_contents: None,
            text_secret: false,
            html_contents: None,
            pending_image: None,
            inflight_transfers: 0,
            inflight_bytes: 0,
            pasted,
            selection_generation: 0,
            rich,
            rich_seat: None,
            rich_active_seat: None,
            rich_pending: None,
            plain_pending: None,
            rich_read_budget,
            pending_publications: HashMap::new(),
            exit: false,
        };
        let fence_connection = connection.clone();
        let _ = loop_handle.insert_source(commands, move |event, (), state| {
            if let calloop::channel::Event::Msg(command) = event {
                match command {
                    ClipboardCommand::Publish(publication) => {
                        let pending = PendingPublication::new(publication);
                        state.pending_publications.insert(
                            Arc::as_ptr(&pending.settle) as usize,
                            (Arc::clone(&pending.claim), Arc::clone(&pending.settle)),
                        );
                        state.arm_publication_deadline(&pending);
                        fence_connection
                            .display()
                            .sync(&state.queue_handle, pending);
                        let _ = fence_connection.flush();
                    }
                    ClipboardCommand::Exit => state.exit = true,
                    #[cfg(test)]
                    ClipboardCommand::CommandProcessed(reply) => {
                        let _ = reply.send(());
                    }
                    ClipboardCommand::Fence(reply) => {
                        // A protocol sync reply shares the worker's event queue.
                        // It proves earlier selection/focus events were handled,
                        // unlike observing a command-channel acknowledgement.
                        fence_connection.display().sync(&state.queue_handle, reply);
                        let _ = fence_connection.flush();
                    }
                    ClipboardCommand::ReadText(reply, deadline) => {
                        state.request_plain_read(&fence_connection, reply, deadline);
                    }
                }
            }
        });
        let _ = WaylandSource::new(connection, queue).insert(loop_handle);
        while !state.exit {
            if event_loop.dispatch(None, &mut state).is_err() {
                break;
            }
        }
        state.invalidate_rich();
        for (_, (claim, settle)) in state.pending_publications.drain() {
            claim.abandon();
            settle_once(&settle, claim.taken());
        }
    }

    struct PendingPublication {
        payload: std::sync::Mutex<Option<PublicationPayload>>,
        settle: SharedSettle,
        deadline: tilo_clock::Instant,
        claim: Arc<PublicationClaim>,
    }

    struct PublishedSelection {
        settle: SharedSettle,
    }

    struct PlainRequest {
        reply: std::sync::Mutex<Option<std::sync::mpsc::Sender<Option<String>>>>,
        deadline: tilo_clock::Instant,
    }

    struct PendingPlain {
        seat: WlSeat,
        generation: u64,
        reply: std::sync::mpsc::Sender<Option<String>>,
        reader: Option<calloop::RegistrationToken>,
        timer: Option<calloop::RegistrationToken>,
    }

    impl Dispatch<sctk::reexports::client::protocol::wl_callback::WlCallback, PlainRequest> for State {
        fn event(
            state: &mut Self,
            _: &sctk::reexports::client::protocol::wl_callback::WlCallback,
            _: sctk::reexports::client::protocol::wl_callback::Event,
            request: &PlainRequest,
            connection: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let Ok(mut reply) = request.reply.lock()
                && let Some(reply) = reply.take()
            {
                state.start_plain_read(connection, reply, request.deadline);
            }
        }
    }

    impl PendingPublication {
        fn new(publication: Publication) -> Self {
            Self {
                payload: std::sync::Mutex::new(Some(publication.payload)),
                settle: Arc::new(std::sync::Mutex::new(Some(publication.settle))),
                deadline: publication.deadline,
                claim: Arc::default(),
            }
        }
    }

    impl State {
        /// At the deadline, refuse a copy the focus check has not taken. One
        /// it took already set the selection: give the compositor's
        /// confirmation one more budget, then report it copied either way.
        fn arm_publication_deadline(&mut self, pending: &PendingPublication) {
            let claim = Arc::clone(&pending.claim);
            let settle = Arc::clone(&pending.settle);
            let mut confirming = false;
            let armed = self.loop_handle.insert_source(
                calloop::timer::Timer::from_deadline(pending.deadline),
                move |_, (), state| {
                    if confirming {
                        state.finish_publication(&settle, true);
                        return calloop::timer::TimeoutAction::Drop;
                    }
                    if claim.abandon() || !claim.taken() {
                        state.finish_publication(&settle, false);
                        return calloop::timer::TimeoutAction::Drop;
                    }
                    confirming = true;
                    calloop::timer::TimeoutAction::ToDuration(super::PUBLICATION_BUDGET)
                },
            );
            if armed.is_err() {
                // Without a deadline the copy could stay pending for good.
                pending.claim.abandon();
                self.finish_publication(&pending.settle, false);
            }
        }

        fn finish_publication(&mut self, settle: &SharedSettle, copied: bool) {
            // The allocation's identity stays unique while callback userdata
            // retains it; remove settled claims rather than retaining a history.
            self.pending_publications
                .remove(&(Arc::as_ptr(settle) as usize));
            settle_once(settle, copied);
        }
    }

    impl Dispatch<sctk::reexports::client::protocol::wl_callback::WlCallback, PendingPublication>
        for State
    {
        fn event(
            state: &mut Self,
            _: &sctk::reexports::client::protocol::wl_callback::WlCallback,
            _: sctk::reexports::client::protocol::wl_callback::Event,
            pending: &PendingPublication,
            connection: &Connection,
            queue_handle: &QueueHandle<Self>,
        ) {
            // This callback follows all earlier focus events on the same
            // protocol queue. A timed-out or unfocused copy must not become
            // a selection later when the application regains focus.
            let payload = pending
                .payload
                .lock()
                .ok()
                .and_then(|mut payload| payload.take());
            let ok = if tilo_clock::now() < pending.deadline
                && state.can_publish()
                && pending.claim.take()
            {
                match payload {
                    Some(PublicationPayload::Text(text)) => {
                        state.store_text(text, false);
                        state.publish_current()
                    }
                    Some(PublicationPayload::SecretText(text)) => {
                        state.store_text(text, true);
                        state.publish_current()
                    }
                    Some(PublicationPayload::Rich { html, plain }) => {
                        state.store_text(plain, false);
                        state.html_contents = Some(Arc::from(html));
                        state.publish_current()
                    }
                    Some(PublicationPayload::Image(png)) => {
                        state.store_image(png);
                        state.publish_current()
                    }
                    None => false,
                }
            } else {
                false
            };
            if ok {
                // The new sink may take focus as soon as the copy settles.
                // Fence set_selection on this connection before reporting a
                // successful copy, so a caller's next focus change cannot
                // overtake its own publication.
                connection.display().sync(
                    queue_handle,
                    PublishedSelection {
                        settle: Arc::clone(&pending.settle),
                    },
                );
                let _ = connection.flush();
            } else {
                pending.claim.abandon();
                state.finish_publication(&pending.settle, false);
            }
        }
    }

    impl Dispatch<sctk::reexports::client::protocol::wl_callback::WlCallback, PublishedSelection>
        for State
    {
        fn event(
            state: &mut Self,
            _: &sctk::reexports::client::protocol::wl_callback::WlCallback,
            _: sctk::reexports::client::protocol::wl_callback::Event,
            published: &PublishedSelection,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            state.finish_publication(&published.settle, true);
        }
    }

    impl State {
        fn can_publish(&self) -> bool {
            self.latest_seat
                .as_ref()
                .and_then(|seat| self.seats.get(seat))
                .is_some_and(|seat| seat.keyboard_focus && seat.data_device.is_some())
        }

        fn store_text(&mut self, text: Vec<u8>, secret: bool) {
            self.text_contents = Some(Arc::from(text));
            self.text_secret = secret;
            self.html_contents = None;
            self.pending_image = None;
        }

        fn store_image(&mut self, contents: Vec<u8>) {
            // A copy replaces what was copied before, as a text copy does.
            // Text kept beside the image would be what every app that
            // prefers text pastes, and a secret would ride along under its
            // markers.
            self.text_contents = None;
            self.text_secret = false;
            self.html_contents = None;
            self.pending_image = Some(Arc::from(contents));
        }

        fn publish_current(&mut self) -> bool {
            let Some(seat) = self.latest_seat.clone() else {
                return false;
            };
            let Some(seat_data) = self.seats.get(&seat) else {
                return false;
            };
            if !seat_data.keyboard_focus {
                return false;
            }
            if seat_data.data_device.is_none() {
                return false;
            }
            let serial = seat_data.latest_serial;
            let image = self.pending_image.clone();
            let mut mime_types = Vec::new();
            if image.is_some() {
                mime_types.push("image/png");
            }
            // Formatting first: an app that reads it prefers it.
            if self.html_contents.is_some() && !self.text_secret {
                mime_types.extend(["text/html", "text/html;charset=utf-8"]);
            }
            if self.text_contents.is_some() {
                mime_types.extend([
                    "text/plain;charset=utf-8",
                    "text/plain",
                    "UTF8_STRING",
                    "STRING",
                    "TEXT",
                ]);
                if self.text_secret {
                    mime_types.extend([crate::SECRET_MIME, crate::PASSWORD_MANAGER_HINT_MIME]);
                }
            }
            if mime_types.is_empty() {
                return false;
            }
            let source = self
                .data_device_manager
                .create_copy_paste_source(&self.queue_handle, mime_types);
            let source_id = source.inner().id();
            self.source_payloads.insert(
                source_id,
                SourcePayload {
                    image,
                    text: self.text_contents.clone(),
                    html: self.html_contents.clone(),
                    secret: self.text_secret,
                },
            );
            let Some(seat_data) = self.seats.get(&seat) else {
                return false;
            };
            let Some(device) = seat_data.data_device.as_ref() else {
                return false;
            };
            source.set_selection(device, serial);
            self.data_sources.push(source);
            // Keep the accepted payload available for protocol transfers and
            // subsequent focus/serial updates.
            true
        }

        fn send(&mut self, source: &WlDataSource, mime: &str, fd: WritePipe) {
            let Some(payload) = self.source_payloads.get(&source.id()) else {
                return;
            };
            let Some(contents) = payload.bytes_for_mime(mime) else {
                return;
            };
            if self.inflight_transfers >= MAX_INFLIGHT_TRANSFERS
                || self.inflight_bytes.saturating_add(contents.len()) > MAX_INFLIGHT_BYTES
            {
                return;
            }
            let Ok(flags) = get_fd_flags(fd.as_raw_fd()) else {
                return;
            };
            if set_fd_flags(fd.as_raw_fd(), flags | libc::O_NONBLOCK) {
                self.inflight_transfers = self.inflight_transfers.saturating_add(1);
                self.inflight_bytes = self.inflight_bytes.saturating_add(contents.len());
                let transfer_len = contents.len();
                let mut written = 0usize;
                let inserted = self.loop_handle.insert_source(fd, move |(), pipe, state| {
                    while written < contents.len() {
                        // `WritePipe` is wrapped in calloop's `NoIoDrop`; the
                        // source owns it for this callback, so temporarily
                        // borrow the underlying file for the write.
                        let Some(remaining) = contents.get(written..) else {
                            return PostAction::Remove;
                        };
                        match unsafe { pipe.get_mut() }.write(remaining) {
                            Ok(0) => {
                                state.inflight_transfers =
                                    state.inflight_transfers.saturating_sub(1);
                                state.inflight_bytes =
                                    state.inflight_bytes.saturating_sub(transfer_len);
                                return PostAction::Remove;
                            }
                            Ok(n) => written = written.saturating_add(n),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                return PostAction::Continue;
                            }
                            Err(_) => {
                                state.inflight_transfers =
                                    state.inflight_transfers.saturating_sub(1);
                                state.inflight_bytes =
                                    state.inflight_bytes.saturating_sub(transfer_len);
                                return PostAction::Remove;
                            }
                        }
                    }
                    state.inflight_transfers = state.inflight_transfers.saturating_sub(1);
                    state.inflight_bytes = state.inflight_bytes.saturating_sub(transfer_len);
                    PostAction::Remove
                });
                if inserted.is_err() {
                    self.inflight_transfers = self.inflight_transfers.saturating_sub(1);
                    self.inflight_bytes = self.inflight_bytes.saturating_sub(transfer_len);
                }
            }
        }
    }

    impl State {
        /// Read the image the new selection offers, if it offers one, into
        /// [`PastedImage`] without blocking the worker: the copying app writes
        /// it at its own pace, and the read is dropped when the selection
        /// changes again or the image is larger than a paste may be.
        fn read_selection_image(&mut self, conn: &Connection, device: &WlDataDevice) {
            self.selection_generation = self.selection_generation.wrapping_add(1);
            let generation = self.selection_generation;
            if let Ok(mut pasted) = self.pasted.lock() {
                *pasted = None;
            }
            let Some(offer) = device
                .data::<sctk::data_device_manager::data_device::DataDeviceData>()
                .and_then(sctk::data_device_manager::data_device::DataDeviceData::selection_offer)
            else {
                return;
            };
            let Some(mime) = offer.with_mime_types(|types| {
                crate::preferred_image_type(types.iter().map(String::as_str))
            }) else {
                return;
            };
            let Ok(pipe) = offer.receive(mime.to_owned()) else {
                return;
            };
            let _ = conn.flush();
            let Ok(flags) = get_fd_flags(pipe.as_raw_fd()) else {
                return;
            };
            if !set_fd_flags(pipe.as_raw_fd(), flags | libc::O_NONBLOCK) {
                return;
            }
            let mut bytes = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            let _ = self
                .loop_handle
                .insert_source(pipe, move |(), pipe, state| {
                    use std::io::Read as _;
                    if state.selection_generation != generation {
                        return PostAction::Remove;
                    }
                    loop {
                        // SAFETY: the source owns the pipe for this callback; the
                        // borrow ends before it returns.
                        match unsafe { pipe.get_mut() }.read(&mut chunk) {
                            Ok(0) => {
                                if !bytes.is_empty()
                                    && let Ok(mut pasted) = state.pasted.lock()
                                {
                                    *pasted = Some(crate::ClipboardImage {
                                        mime,
                                        bytes: std::mem::take(&mut bytes),
                                    });
                                }
                                return PostAction::Remove;
                            }
                            Ok(read) => {
                                bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
                                if bytes.len() > crate::MAX_PASTED_IMAGE_BYTES {
                                    return PostAction::Remove;
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                return PostAction::Continue;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(_) => return PostAction::Remove,
                        }
                    }
                });
        }
    }

    #[derive(Clone, Copy)]
    pub(super) enum RichPart {
        Html,
        Plain,
    }

    impl RichPart {
        fn limit(self) -> usize {
            match self {
                Self::Html => crate::MAX_CLIPBOARD_HTML_BYTES,
                Self::Plain => crate::MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES,
            }
        }
    }

    pub(super) enum RichRead {
        Pending,
        Finished(Option<String>),
    }

    /// One bounded nonblocking read callback, including interrupted syscalls.
    pub(super) struct RichPipe {
        bytes: Vec<u8>,
        limit: usize,
    }

    impl RichPipe {
        pub(super) fn new(part: RichPart) -> Self {
            Self {
                bytes: Vec::new(),
                limit: part.limit(),
            }
        }

        pub(super) fn read(&mut self, pipe: &mut impl std::io::Read) -> RichRead {
            let mut chunk = [0u8; 8192];
            for _ in 0..8 {
                match pipe.read(&mut chunk) {
                    Ok(0) => {
                        return RichRead::Finished(
                            String::from_utf8(std::mem::take(&mut self.bytes)).ok(),
                        );
                    }
                    Ok(read) => {
                        if self.bytes.len().saturating_add(read) > self.limit {
                            return RichRead::Finished(None);
                        }
                        self.bytes
                            .extend_from_slice(chunk.get(..read).unwrap_or_default());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return RichRead::Pending;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => return RichRead::Finished(None),
                }
            }
            RichRead::Pending
        }
    }

    struct PendingRich {
        deadline: tilo_clock::Instant,
        html: Option<String>,
        plain: Option<String>,
        html_done: bool,
        plain_done: bool,
        html_reader: Option<calloop::RegistrationToken>,
        plain_reader: Option<calloop::RegistrationToken>,
        timer: Option<calloop::RegistrationToken>,
    }

    impl PendingRich {
        fn reader(&mut self, part: RichPart) -> &mut Option<calloop::RegistrationToken> {
            match part {
                RichPart::Html => &mut self.html_reader,
                RichPart::Plain => &mut self.plain_reader,
            }
        }
    }

    impl State {
        fn request_plain_read(
            &self,
            connection: &Connection,
            reply: std::sync::mpsc::Sender<Option<String>>,
            deadline: tilo_clock::Instant,
        ) {
            // Read only after this queue has handled preceding selection and
            // keyboard-focus events, including a queued focus revocation.
            connection.display().sync(
                &self.queue_handle,
                PlainRequest {
                    reply: std::sync::Mutex::new(Some(reply)),
                    deadline,
                },
            );
            let _ = connection.flush();
        }

        fn finish_plain_read(&mut self, value: Option<String>) {
            if let Some(pending) = self.plain_pending.take() {
                for token in [pending.reader, pending.timer].into_iter().flatten() {
                    self.loop_handle.remove(token);
                }
                let _ = pending.reply.send(value);
                self.rich.wake.wake();
            }
        }

        fn start_plain_read(
            &mut self,
            connection: &Connection,
            reply: std::sync::mpsc::Sender<Option<String>>,
            deadline: tilo_clock::Instant,
        ) {
            // One explicit paste can own one bounded pipe. A newer request
            // cancels the previous transfer rather than accumulating readers.
            self.finish_plain_read(None);
            let offered = self.latest_seat.as_ref().and_then(|seat| {
                let data = self.seats.get(seat)?;
                if !data.keyboard_focus || !data.rich_offer_seen {
                    return None;
                }
                let device = data.data_device.as_ref()?;
                let offer = device.data().selection_offer()?;
                let mime = offer.with_mime_types(|types| {
                    [
                        "text/plain;charset=utf-8",
                        "text/plain",
                        "UTF8_STRING",
                        "STRING",
                        "TEXT",
                    ]
                    .into_iter()
                    .find(|mime| types.iter().any(|offered| offered == mime))
                })?;
                Some((seat.clone(), offer.receive(mime.into()).ok()?))
            });
            let Some((seat, pipe)) = offered.filter(|_| tilo_clock::now() < deadline) else {
                let _ = reply.send(None);
                self.rich.wake.wake();
                return;
            };
            let generation = self.rich.generation();
            self.plain_pending = Some(PendingPlain {
                seat,
                generation,
                reply,
                reader: None,
                timer: None,
            });
            if !self.register_plain_reader(pipe, generation, deadline) {
                self.finish_plain_read(None);
                return;
            }
            let timer = self.loop_handle.insert_source(
                calloop::timer::Timer::from_deadline(deadline),
                move |_, (), state| {
                    if let Some(pending) = state.plain_pending.as_mut()
                        && pending.generation == generation
                    {
                        pending.timer = None;
                        state.finish_plain_read(None);
                    }
                    calloop::timer::TimeoutAction::Drop
                },
            );
            if let Ok(timer) = timer {
                if let Some(pending) = self.plain_pending.as_mut() {
                    pending.timer = Some(timer);
                }
            } else {
                self.finish_plain_read(None);
            }
            if connection.flush().is_err() {
                self.finish_plain_read(None);
            }
        }

        fn register_plain_reader(
            &mut self,
            pipe: sctk::data_device_manager::ReadPipe,
            generation: u64,
            deadline: tilo_clock::Instant,
        ) -> bool {
            let Ok(flags) = get_fd_flags(pipe.as_raw_fd()) else {
                return false;
            };
            if !set_fd_flags(pipe.as_raw_fd(), flags | libc::O_NONBLOCK) {
                return false;
            }
            let mut reader = RichPipe::new(RichPart::Plain);
            let inserted = self
                .loop_handle
                .insert_source(pipe, move |(), pipe, state| {
                    let Some(pending) = state.plain_pending.as_ref() else {
                        return PostAction::Remove;
                    };
                    if pending.generation != generation {
                        return PostAction::Remove;
                    }
                    let current = generation == state.rich.generation()
                        && tilo_clock::now() < deadline
                        && state
                            .seats
                            .get(&pending.seat)
                            .is_some_and(|seat| seat.keyboard_focus);
                    // SAFETY: this source owns the nonblocking pipe throughout
                    // this callback, and the borrow ends before returning.
                    let result = if current {
                        reader.read(unsafe { pipe.get_mut() })
                    } else {
                        RichRead::Finished(None)
                    };
                    match result {
                        RichRead::Pending => PostAction::Continue,
                        RichRead::Finished(value) => {
                            if let Some(pending) = state.plain_pending.as_mut() {
                                pending.reader = None;
                            }
                            state.finish_plain_read(value);
                            PostAction::Remove
                        }
                    }
                });
            let Ok(token) = inserted else {
                return false;
            };
            if let Some(pending) = self.plain_pending.as_mut() {
                pending.reader = Some(token);
            }
            true
        }

        fn stop_rich_readers(&mut self) {
            if let Some(pending) = self.rich_pending.take() {
                for token in [pending.html_reader, pending.plain_reader, pending.timer]
                    .into_iter()
                    .flatten()
                {
                    self.loop_handle.remove(token);
                }
            }
        }

        fn invalidate_rich(&mut self) {
            self.finish_plain_read(None);
            self.rich.invalidate();
            self.stop_rich_readers();
            self.rich_seat = None;
        }

        fn invalidate_rich_seat(&mut self, seat: &WlSeat) {
            if self.rich_seat.as_ref() == Some(seat)
                || self
                    .plain_pending
                    .as_ref()
                    .is_some_and(|pending| &pending.seat == seat)
            {
                self.invalidate_rich();
            }
        }

        fn activate_rich_seat(&mut self, seat: &WlSeat) -> bool {
            if !self.seats.get(seat).is_some_and(|data| data.keyboard_focus)
                || self.rich_active_seat.as_ref() == Some(seat)
            {
                return false;
            }
            self.invalidate_rich();
            self.rich_active_seat = Some(seat.clone());
            true
        }

        fn deactivate_rich_seat(&mut self, seat: &WlSeat) {
            if self.rich_active_seat.as_ref() == Some(seat) {
                self.rich_active_seat = None;
            }
            self.invalidate_rich_seat(seat);
        }

        fn finish_rich_part(&mut self, part: RichPart, value: Option<String>, generation: u64) {
            let Some(pending) = self.rich_pending.as_mut() else {
                return;
            };
            // This callback returns Remove for its own source. Other readers
            // and the deadline are explicitly removed on completion/refusal.
            *pending.reader(part) = None;
            match part {
                RichPart::Html => {
                    pending.html = value.filter(|html| !html.is_empty());
                    if pending.html.is_none() {
                        self.invalidate_rich();
                        return;
                    }
                    pending.html_done = true;
                }
                RichPart::Plain => {
                    pending.plain = value;
                    pending.plain_done = true;
                }
            }
            if pending.html_done && pending.plain_done {
                let offer = pending.html.take().map(|html| crate::RichTextOffer {
                    html,
                    plain: pending.plain.take(),
                });
                self.stop_rich_readers();
                if let Some(offer) = offer {
                    self.rich.publish(generation, offer);
                }
            }
        }

        fn register_rich_reader(
            &mut self,
            pipe: sctk::data_device_manager::ReadPipe,
            part: RichPart,
            generation: u64,
        ) -> bool {
            let Ok(flags) = get_fd_flags(pipe.as_raw_fd()) else {
                return false;
            };
            if !set_fd_flags(pipe.as_raw_fd(), flags | libc::O_NONBLOCK) {
                return false;
            }
            let mut reader = RichPipe::new(part);
            let inserted = self
                .loop_handle
                .insert_source(pipe, move |(), pipe, state| {
                    if state.rich.generation() != generation {
                        return PostAction::Remove;
                    }
                    let Some(pending) = state.rich_pending.as_ref() else {
                        return PostAction::Remove;
                    };
                    if tilo_clock::now() >= pending.deadline {
                        state.invalidate_rich();
                        return PostAction::Remove;
                    }
                    // SAFETY: this source owns the nonblocking pipe throughout
                    // this callback, and the borrow ends before returning.
                    let result = reader.read(unsafe { pipe.get_mut() });
                    if state
                        .rich_pending
                        .as_ref()
                        .is_none_or(|pending| tilo_clock::now() >= pending.deadline)
                    {
                        state.invalidate_rich();
                        return PostAction::Remove;
                    }
                    match result {
                        RichRead::Pending => PostAction::Continue,
                        RichRead::Finished(value) => {
                            state.finish_rich_part(part, value, generation);
                            PostAction::Remove
                        }
                    }
                });
            let Ok(token) = inserted else {
                return false;
            };
            if let Some(pending) = self.rich_pending.as_mut() {
                *pending.reader(part) = Some(token);
                true
            } else {
                self.loop_handle.remove(token);
                false
            }
        }

        fn rich_selection_event(&mut self, conn: &Connection, device: &WlDataDevice) {
            if let Some(data) = self.seats.values_mut().find(|data| {
                data.data_device
                    .as_ref()
                    .is_some_and(|data| data.inner() == device)
            }) {
                // Selection precedes Enter. Retain offer identity, including
                // NULL; reading still requires this seat's keyboard focus.
                data.rich_offer_seen = true;
            }
            self.read_selection_rich(conn, device);
        }

        fn read_selection_rich(&mut self, conn: &Connection, device: &WlDataDevice) {
            let Some(seat) = self.seats.iter().find_map(|(seat, data)| {
                data.data_device
                    .as_ref()
                    .filter(|data| data.inner() == device)
                    .map(|_| seat.clone())
            }) else {
                return;
            };
            // A stale seat must neither clear another seat's offer nor use
            // pointer/touch focus to authorize a keyboard clipboard read.
            if self.rich_active_seat.as_ref() != Some(&seat)
                || !self
                    .seats
                    .get(&seat)
                    .is_some_and(|data| data.keyboard_focus && data.rich_offer_seen)
            {
                self.invalidate_rich_seat(&seat);
                return;
            }
            let Some(offer) = device
                .data::<sctk::data_device_manager::data_device::DataDeviceData>()
                .and_then(sctk::data_device_manager::data_device::DataDeviceData::selection_offer)
            else {
                self.invalidate_rich_seat(&seat);
                return;
            };
            self.invalidate_rich();
            let (html, plain) = offer.with_mime_types(|types| {
                (
                    types.iter().any(|mime| mime == "text/html"),
                    ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"]
                        .into_iter()
                        .find(|mime| types.iter().any(|offered| offered == mime)),
                )
            });
            if !html {
                return;
            }
            let Ok(html_pipe) = offer.receive("text/html".into()) else {
                return;
            };
            let plain_pipe = plain.and_then(|mime| offer.receive(mime.into()).ok());
            let generation = self.rich.generation();
            let now = tilo_clock::now();
            let deadline = now.checked_add(self.rich_read_budget).unwrap_or(now);
            self.rich_seat = Some(seat);
            self.rich_pending = Some(PendingRich {
                deadline,
                html: None,
                plain: None,
                html_done: false,
                plain_done: plain_pipe.is_none(),
                html_reader: None,
                plain_reader: None,
                timer: None,
            });
            let timer = self.loop_handle.insert_source(
                calloop::timer::Timer::from_deadline(deadline),
                move |_, (), state| {
                    if state.rich.generation() == generation {
                        if let Some(pending) = state.rich_pending.as_mut() {
                            pending.timer = None;
                        }
                        state.invalidate_rich();
                    }
                    calloop::timer::TimeoutAction::Drop
                },
            );
            let Ok(timer) = timer else {
                self.invalidate_rich();
                return;
            };
            if let Some(pending) = self.rich_pending.as_mut() {
                pending.timer = Some(timer);
            }
            if !self.register_rich_reader(html_pipe, RichPart::Html, generation) {
                self.invalidate_rich();
                return;
            }
            if let Some(pipe) = plain_pipe
                && !self.register_rich_reader(pipe, RichPart::Plain, generation)
            {
                self.finish_rich_part(RichPart::Plain, None, generation);
            }
            if conn.flush().is_err() {
                self.invalidate_rich();
            }
        }
    }

    fn get_fd_flags(fd: i32) -> std::io::Result<i32> {
        // SAFETY: fcntl only reads flags for this live descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        (flags >= 0)
            .then_some(flags)
            .ok_or_else(std::io::Error::last_os_error)
    }

    fn set_fd_flags(fd: i32, flags: i32) -> bool {
        // SAFETY: fd is owned by the active WritePipe and flags came from F_GETFL.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) >= 0 }
    }

    impl DataSourceHandler for State {
        fn accept_mime(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlDataSource,
            _: Option<String>,
        ) {
        }

        fn send_request(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            source: &WlDataSource,
            mime: String,
            fd: WritePipe,
        ) {
            self.send(source, &mime, fd);
        }

        fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
            self.source_payloads.remove(&source.id());
            self.data_sources.retain(|item| item.inner() != source);
        }

        fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
        fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
        fn action(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlDataSource,
            _: DndAction,
        ) {
        }
    }

    impl DataDeviceHandler for State {
        fn enter(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlDataDevice,
            _: f64,
            _: f64,
            _: &sctk::reexports::client::protocol::wl_surface::WlSurface,
        ) {
        }
        fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
        fn motion(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlDataDevice,
            _: f64,
            _: f64,
        ) {
        }
        fn selection(&mut self, conn: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
            self.read_selection_image(conn, device);
            self.rich_selection_event(conn, device);
        }
        fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    }

    impl DataOfferHandler for State {
        fn source_actions(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &mut DragOffer,
            _: DndAction,
        ) {
        }
        fn selected_action(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &mut DragOffer,
            _: DndAction,
        ) {
        }
    }

    impl SeatHandler for State {
        fn seat_state(&mut self) -> &mut SeatState {
            &mut self.seat_registry
        }
        fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
            self.seats.entry(seat).or_default();
        }
        fn new_capability(
            &mut self,
            _: &Connection,
            qh: &QueueHandle<Self>,
            seat: WlSeat,
            capability: Capability,
        ) {
            let Some(data) = self.seats.get_mut(&seat) else {
                return;
            };
            match capability {
                Capability::Keyboard => {
                    data.keyboard = Some(seat.get_keyboard(qh, seat.clone()));
                    if data.data_device.is_none() {
                        data.rich_offer_seen = false;
                        data.data_device =
                            Some(self.data_device_manager.get_data_device(qh, &seat));
                    }
                }
                Capability::Pointer => {
                    data.pointer = self.seat_registry.get_pointer(qh, &seat).ok();
                    if data.data_device.is_none() {
                        data.rich_offer_seen = false;
                        data.data_device =
                            Some(self.data_device_manager.get_data_device(qh, &seat));
                    }
                }
                Capability::Touch => {
                    data.touch = self.seat_registry.get_touch(qh, &seat).ok();
                    if data.data_device.is_none() {
                        data.rich_offer_seen = false;
                        data.data_device =
                            Some(self.data_device_manager.get_data_device(qh, &seat));
                    }
                }
                _ => {}
            }
        }
        fn remove_capability(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            seat: WlSeat,
            capability: Capability,
        ) {
            if let Some(data) = self.seats.get_mut(&seat) {
                match capability {
                    Capability::Keyboard => {
                        data.keyboard = None;
                        data.keyboard_focus = false;
                    }
                    Capability::Pointer => data.pointer = None,
                    Capability::Touch => data.touch = None,
                    _ => {}
                }
            }
            if capability == Capability::Keyboard {
                self.deactivate_rich_seat(&seat);
            }
        }
        fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
            self.deactivate_rich_seat(&seat);
            self.seats.remove(&seat);
        }
    }

    impl PointerHandler for State {
        fn pointer_frame(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            pointer: &WlPointer,
            events: &[PointerEvent],
        ) {
            let Some(pointer_data) = pointer.data::<PointerData>() else {
                return;
            };
            let seat = pointer_data.seat();
            let Some(data) = self.seats.get_mut(seat) else {
                return;
            };
            // Pointer focus is not copy permission: the compositor accepts a
            // selection only from the keyboard-focused client, and the pointer
            // may leave a window that keeps keyboard focus.
            for event in events {
                if let PointerEventKind::Press { serial, .. }
                | PointerEventKind::Release { serial, .. } = event.kind
                {
                    data.latest_serial = serial;
                    self.latest_seat = Some(seat.clone());
                }
            }
        }
    }

    impl TouchHandler for State {
        fn down(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            touch: &WlTouch,
            serial: u32,
            _: u32,
            _: sctk::reexports::client::protocol::wl_surface::WlSurface,
            _: i32,
            _: (f64, f64),
        ) {
            let Some(touch_data) = touch.data::<TouchData>() else {
                return;
            };
            let seat = touch_data.seat().clone();
            let Some(data) = self.seats.get_mut(&seat) else {
                return;
            };
            data.latest_serial = serial;
            self.latest_seat = Some(seat);
        }

        fn up(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlTouch,
            _: u32,
            _: u32,
            _: i32,
        ) {
        }

        fn motion(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlTouch,
            _: u32,
            _: i32,
            _: (f64, f64),
        ) {
        }

        fn shape(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlTouch,
            _: i32,
            _: f64,
            _: f64,
        ) {
        }

        fn orientation(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &WlTouch,
            _: i32,
            _: f64,
        ) {
        }

        fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch) {}
    }

    impl Dispatch<WlKeyboard, WlSeat, State> for State {
        fn event(
            state: &mut State,
            _: &WlKeyboard,
            event: <WlKeyboard as Proxy>::Event,
            seat: &WlSeat,
            conn: &Connection,
            _: &QueueHandle<Self>,
        ) {
            use sctk::reexports::client::protocol::wl_keyboard::Event;
            let Some(data) = state.seats.get_mut(seat) else {
                return;
            };
            match event {
                Event::Key { serial, .. } | Event::Modifiers { serial, .. } => {
                    data.latest_serial = serial;
                    state.latest_seat = Some(seat.clone());
                    if state.activate_rich_seat(seat) {
                        let device = state
                            .seats
                            .get(seat)
                            .and_then(|data| data.data_device.as_ref())
                            .map(|data| data.inner().clone());
                        if let Some(device) = device {
                            state.read_selection_rich(conn, &device);
                        }
                    }
                }
                Event::Enter { serial, .. } => {
                    data.latest_serial = serial;
                    data.keyboard_focus = true;
                    state.latest_seat = Some(seat.clone());
                    state.activate_rich_seat(seat);
                    // The selection was delivered before Enter. A same-client
                    // surface switch may retain that offer without resending it.
                    let device = state
                        .seats
                        .get(seat)
                        .filter(|data| data.rich_offer_seen)
                        .and_then(|data| data.data_device.as_ref())
                        .map(|data| data.inner().clone());
                    if let Some(device) = device {
                        state.read_selection_rich(conn, &device);
                    }
                }
                Event::Leave { .. } => {
                    data.latest_serial = 0;
                    data.keyboard_focus = false;
                    state.deactivate_rich_seat(seat);
                }
                _ => {}
            }
        }
    }

    impl
        Dispatch<
            sctk::reexports::client::protocol::wl_callback::WlCallback,
            std::sync::mpsc::Sender<()>,
        > for State
    {
        fn event(
            _: &mut Self,
            _: &sctk::reexports::client::protocol::wl_callback::WlCallback,
            _: sctk::reexports::client::protocol::wl_callback::Event,
            reply: &std::sync::mpsc::Sender<()>,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let _ = reply.send(());
        }
    }

    impl ProvidesRegistryState for State {
        sctk::registry_handlers![SeatState];

        fn registry(&mut self) -> &mut RegistryState {
            &mut self.registry
        }
    }

    delegate_seat!(State);
    delegate_pointer!(State);
    // Keep SCTK's offer/source bookkeeping and child-object initialization.
    // Its pinned 0.20 handler omits DataDeviceHandler::selection for NULL,
    // although NULL must cancel the rich cache and both pending readers.
    sctk::reexports::client::delegate_dispatch!(State: [
        sctk::reexports::client::protocol::wl_data_device_manager::WlDataDeviceManager: sctk::globals::GlobalData
    ] => DataDeviceManagerState);
    sctk::reexports::client::delegate_dispatch!(State: [
        WlDataOffer: DataOfferData
    ] => DataDeviceManagerState);
    sctk::reexports::client::delegate_dispatch!(State: [
        WlDataSource: sctk::data_device_manager::data_source::DataSourceData
    ] => DataDeviceManagerState);

    impl Dispatch<WlDataDevice, DataDeviceData> for State {
        sctk::reexports::client::event_created_child!(State, WlDataDevice, [
            0 => (WlDataOffer, DataOfferData::default())
        ]);

        fn event(
            state: &mut Self,
            device: &WlDataDevice,
            event: sctk::reexports::client::protocol::wl_data_device::Event,
            data: &DataDeviceData,
            conn: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            let cleared = matches!(
                event,
                sctk::reexports::client::protocol::wl_data_device::Event::Selection { id: None }
            );
            <DataDeviceManagerState as Dispatch<WlDataDevice, DataDeviceData, Self>>::event(
                state, device, event, data, conn, qh,
            );
            if cleared {
                state.rich_selection_event(conn, device);
            }
        }
    }
    delegate_registry!(State);
    delegate_touch!(State);
}

impl ClipboardBackend for WaylandClipboard {
    fn request_text(&self, target: ClipboardTarget) -> std::sync::mpsc::Receiver<Option<String>> {
        let (reply, result) = std::sync::mpsc::channel();
        if target == ClipboardTarget::Primary {
            let _ = reply.send(self.text(target));
        } else {
            let now = tilo_clock::now();
            let deadline = now.checked_add(self.explicit_read_budget).unwrap_or(now);
            if !self.send_publication(ClipboardCommand::ReadText(reply.clone(), deadline)) {
                let _ = reply.send(None);
            }
        }
        result
    }

    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        match target {
            ClipboardTarget::Clipboard => {
                let ok = self.publish(PublicationPayload::Text(text.as_bytes().to_vec()));
                if ok && let Ok(mut cache) = self.clipboard_cache.lock() {
                    *cache = Some(text.to_owned());
                }
                ok
            }
            ClipboardTarget::Primary => {
                let Some(inner) = self.write_inner.as_ref() else {
                    return false;
                };
                let Ok(inner) = inner.clipboard.try_lock() else {
                    return false;
                };
                inner.store_primary(text.to_string());
                if let Ok(mut cache) = self.primary_cache.lock() {
                    *cache = Some(text.to_owned());
                }
                true
            }
        }
    }

    fn set_rich_text(&self, html: &str, plain: &str) -> bool {
        let ok = self.publish(PublicationPayload::Rich {
            html: html.as_bytes().to_vec(),
            plain: plain.as_bytes().to_vec(),
        });
        if ok && let Ok(mut cache) = self.clipboard_cache.lock() {
            *cache = Some(plain.to_owned());
        }
        ok
    }

    fn set_secret_text(&self, text: &str) -> bool {
        let ok = self.publish(PublicationPayload::SecretText(text.as_bytes().to_vec()));
        if ok && let Ok(mut cache) = self.clipboard_cache.lock() {
            *cache = None;
        }
        ok
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        // An explicit paste must never substitute an older cached selection.
        // The protocol read runs on its worker; this call waits only for the
        // small interaction budget, then reports no text.
        self.refresh_external_text(target).flatten()
    }

    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let Some(png) = encode_png(width, height, rgba) else {
            return false;
        };
        self.publish(PublicationPayload::Image(png))
    }

    fn copy(&self, content: crate::CopyContent<'_>) -> crate::ClipboardCopy {
        let (copy, settler) = crate::ClipboardCopy::pending();
        self.copy_then(content, move |copied| settler.settle(copied));
        copy
    }

    fn rich_text(&self) -> Option<crate::RichTextOffer> {
        if !self
            .publication
            .alive
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        self.rich.read()
    }

    fn image(&self) -> Option<ClipboardImage> {
        self.pasted.lock().ok()?.clone()
    }

    fn name(&self) -> &'static str {
        "wayland-data-device"
    }
}

impl WaylandClipboard {
    #[allow(
        clippy::option_option,
        reason = "outer None is a timed-out/unavailable read; inner None is a completed non-text selection"
    )]
    fn refresh_external_text(&self, target: ClipboardTarget) -> Option<Option<String>> {
        use std::sync::atomic::Ordering;
        let inner = match target {
            ClipboardTarget::Clipboard => self.read_clipboard.as_ref(),
            ClipboardTarget::Primary => self.read_primary.as_ref(),
        }
        .cloned()?;
        let (refresh_inflight, refresh_generation) = match target {
            ClipboardTarget::Clipboard => (
                &self.clipboard_refresh_inflight,
                &self.clipboard_refresh_generation,
            ),
            ClipboardTarget::Primary => (
                &self.primary_refresh_inflight,
                &self.primary_refresh_generation,
            ),
        };
        if !begin_external_refresh(refresh_inflight) {
            return None;
        }
        let generation = refresh_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let cache = match target {
            ClipboardTarget::Clipboard => Arc::clone(&self.clipboard_cache),
            ClipboardTarget::Primary => Arc::clone(&self.primary_cache),
        };
        let inflight = Arc::clone(refresh_inflight);
        let worker_inflight = Arc::clone(&inflight);
        let current_generation = Arc::clone(refresh_generation);
        let wake = self.wake.clone();
        let (immediate_tx, immediate_rx) = std::sync::mpsc::sync_channel(1);
        if std::thread::Builder::new()
            .name("tilo-wayland-clipboard-read".into())
            .spawn(move || {
                // `smithay_clipboard::Clipboard::load` waits for the compositor's
                // transfer to finish. Keep exactly one loader per target instead
                // of a timeout thread that clears `inflight` while the loader is
                // still blocked; the latter creates an unbounded pile of blocked
                // threads when a peer never closes its pipe.
                let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    inner
                        .clipboard
                        .lock()
                        .ok()
                        .and_then(|clipboard| match target {
                            ClipboardTarget::Clipboard => clipboard.load().ok(),
                            ClipboardTarget::Primary => clipboard.load_primary().ok(),
                        })
                }))
                .ok()
                .flatten();
                let _ = immediate_tx.send(value.clone());
                if current_generation.load(Ordering::Acquire) == generation {
                    if let Some(value) = value
                        && let Ok(mut slot) = cache.lock()
                    {
                        *slot = Some(value);
                    }
                    worker_inflight.store(false, Ordering::Release);
                }
                wake.wake();
            })
            .is_err()
        {
            inflight.store(false, Ordering::Release);
            return None;
        }
        recv_bounded(&immediate_rx, CLIPBOARD_READ_BUDGET)
    }

    /// Queue a copy; `settle` reports its outcome from the worker: refused,
    /// or published and confirmed by the compositor. The worker settles it by
    /// the deadline plus one more budget at the latest.
    fn publish_async(&self, payload: PublicationPayload, settle: Settle) {
        let publication = Publication::new(payload, settle, publication_deadline());
        // A refused send drops the publication, and with it the settle,
        // which reports the failure.
        let _ = self.send_publication(ClipboardCommand::Publish(publication));
    }

    /// [`Self::publish_async`] for a blocking caller.
    fn publish(&self, payload: PublicationPayload) -> bool {
        let (reply, result) = std::sync::mpsc::channel();
        self.publish_async(
            payload,
            Settle::new(move |copied| {
                let _ = reply.send(copied);
            }),
        );
        // The worker answers by the deadline plus one budget; the third
        // budget only bounds a worker that stopped running its loop.
        recv_bounded(&result, PUBLICATION_BUDGET.saturating_mul(3)).unwrap_or(false)
    }

    /// Queue `content`; `then` reports its outcome from the worker.
    fn copy_then(&self, content: crate::CopyContent<'_>, then: impl FnOnce(bool) + Send + 'static) {
        let payload = match content {
            crate::CopyContent::Text(text) | crate::CopyContent::SharedText(text) => {
                PublicationPayload::Text(text.as_bytes().to_vec())
            }
            crate::CopyContent::SecretText(text) => {
                PublicationPayload::SecretText(text.as_bytes().to_vec())
            }
            crate::CopyContent::Rich { html, plain } => PublicationPayload::Rich {
                html: html.as_bytes().to_vec(),
                plain: plain.as_bytes().to_vec(),
            },
            crate::CopyContent::Image {
                width,
                height,
                rgba,
            } => {
                let Some(png) = encode_png(width, height, rgba) else {
                    then(false);
                    return;
                };
                PublicationPayload::Image(png)
            }
        };
        self.publish_async(payload, Settle::new(then));
    }

    fn send_publication(&self, command: ClipboardCommand) -> bool {
        use std::sync::atomic::Ordering;
        if !self.publication.alive.load(Ordering::Acquire) {
            return false;
        }
        if self.publication.sender.send(command).is_err() {
            return false;
        }
        // A worker that has already terminated is a known failure. A race
        // after this check is still an asynchronous Wayland failure, matching
        // the established text backend contract.
        self.publication.alive.load(Ordering::Acquire)
    }
}

fn begin_external_refresh(inflight: &std::sync::atomic::AtomicBool) -> bool {
    use std::sync::atomic::Ordering;

    inflight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

impl Drop for WaylandClipboard {
    fn drop(&mut self) {
        self.rich.invalidate();
        self.publication.shut_down();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_cache_refuses_old_generations_and_never_waits_for_its_lock() {
        let cache = RichClipboard::new();
        let offer = crate::RichTextOffer {
            html: "<b>文🙂</b>".into(),
            plain: Some("文🙂".into()),
        };
        let first = cache.generation();
        cache.publish(first, offer.clone());
        assert_eq!(cache.read(), Some(offer.clone()));
        {
            let _held = cache.offer.lock().expect("cache lock");
            assert!(cache.read().is_none());
        }
        cache.invalidate();
        cache.publish(first, offer.clone());
        assert!(cache.read().is_none());
        cache.publish(cache.generation(), offer.clone());
        assert_eq!(cache.read(), Some(offer));
        cache.invalidate();
        assert!(cache.read().is_none());
    }

    #[test]
    fn clipboard_images_encode_as_bounded_lossless_png() {
        // phase: rgba_image_is_encoded_as_lossless_png
        {
            let rgba = [0xff, 0x00, 0x80, 0xff, 0x10, 0x20, 0x30, 0x40];
            let png = encode_png(2, 1, &rgba).expect("valid RGBA image");
            let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .expect("complete PNG")
                .into_rgba8();
            assert_eq!(decoded.dimensions(), (2, 1));
            assert_eq!(decoded.as_raw().as_slice(), rgba);
        }
        // phase: png_encoding_rejects_invalid_dimensions_and_oversized_input
        {
            assert!(encode_png(0, 1, &[]).is_none());
            assert!(encode_png(1, 1, &[]).is_none());
            assert!(encode_png(usize::MAX, 1, &[0; 4]).is_none());
            assert!(encode_png(MAX_CLIPBOARD_IMAGE_INPUT_BYTES / 4 + 1, 1, &[0; 4]).is_none());
        }
    }

    #[test]
    fn clipboard_transfers_are_single_flight_bounded_and_mime_exact() {
        // phase: external_readers_are_single_flight_but_targets_are_independent
        {
            use std::sync::atomic::AtomicBool;

            let clipboard = AtomicBool::new(false);
            let primary = AtomicBool::new(false);
            assert!(begin_external_refresh(&clipboard));
            assert!(!begin_external_refresh(&clipboard));
            assert!(begin_external_refresh(&primary));
            clipboard.store(false, std::sync::atomic::Ordering::Release);
            assert!(begin_external_refresh(&clipboard));
        }
        // phase: bounded_result_wait_returns_fresh_data_and_times_out_without_stale_fallback
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let (request_tx, request_rx) = std::sync::mpsc::channel::<()>();
            let owner = std::thread::spawn(move || {
                // Answer only once the reader is waiting, so the budget is what
                // bounds the wait rather than the answer already being queued.
                request_rx.recv().unwrap();
                let _ = tx.send(Some("fresh".to_string()));
            });
            request_tx.send(()).unwrap();
            assert_eq!(
                recv_bounded(&rx, tilo_test_env::hang_bound()),
                Some(Some("fresh".to_string()))
            );
            owner.join().unwrap();

            let (_tx, rx) = std::sync::mpsc::channel::<Option<String>>();
            assert_eq!(
                recv_bounded(&rx, std::time::Duration::from_millis(1)),
                None,
                "a slow external owner must not make paste return an unrelated cached value"
            );
        }
        // phase: shutdown_returns_only_after_the_worker_has_stopped
        {
            // A worker slower to stop than the old fixed budget: shutting
            // down still waits for it, since it would use the display after.
            let (sender, commands) =
                smithay_client_toolkit::reexports::calloop::channel::channel::<ClipboardCommand>();
            let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_stopped = Arc::clone(&stopped);
            let worker = std::thread::spawn(move || {
                loop {
                    match commands.try_recv() {
                        Ok(ClipboardCommand::Exit)
                        | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                        Ok(_) | Err(std::sync::mpsc::TryRecvError::Empty) => {
                            std::thread::yield_now();
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
                worker_stopped.store(true, std::sync::atomic::Ordering::Release);
            });
            let publication = PublicationClipboard {
                sender,
                thread: std::sync::Mutex::new(Some(worker)),
                alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            };
            publication.shut_down();
            assert!(
                stopped.load(std::sync::atomic::Ordering::Acquire),
                "shutting down returned while the worker could still use the display"
            );
        }
        // phase: source_payload_rejects_unadvertised_mime_without_falling_back_to_text
        {
            use image_worker_impl::SourcePayload;

            let payload = SourcePayload {
                image: Some(Arc::from([1u8, 2, 3].as_slice())),
                text: Some(Arc::from(b"caption".as_slice())),
                html: None,
                secret: false,
            };
            assert_eq!(
                payload.bytes_for_mime("image/png").as_deref(),
                Some(&[1, 2, 3][..])
            );
            assert_eq!(
                payload.bytes_for_mime("text/plain").as_deref(),
                Some(&b"caption"[..])
            );
            assert!(payload.bytes_for_mime("application/octet-stream").is_none());
            assert!(
                payload.bytes_for_mime(crate::SECRET_MIME).is_none(),
                "only a secret copy answers for the secret markers"
            );
        }
    }

    #[test]
    fn rich_pipe_preserves_utf8_and_refuses_invalid_or_oversized_payloads() {
        use image_worker_impl::{RichPart, RichPipe, RichRead};
        for (bytes, accepted) in [
            ("Ä😀\n".as_bytes().to_vec(), true),
            (vec![b'a'; crate::MAX_CLIPBOARD_HTML_BYTES], true),
            (vec![b'a'; crate::MAX_CLIPBOARD_HTML_BYTES + 1], false),
            (vec![0xff, 0xfe], false),
        ] {
            let mut reader = RichPipe::new(RichPart::Html);
            let mut input = std::io::Cursor::new(&bytes);
            let mut finished = false;
            for _ in 0..20 {
                if let RichRead::Finished(value) = reader.read(&mut input) {
                    assert_eq!(value.is_some(), accepted);
                    if let Some(value) = value {
                        assert_eq!(value.as_bytes(), bytes);
                    }
                    finished = true;
                    break;
                }
            }
            assert!(finished, "bounded fixture must finish without a busy loop");
        }
    }

    #[test]
    fn rich_pipe_bounds_callback_bytes_and_interrupted_syscalls() {
        use image_worker_impl::{RichPart, RichPipe, RichRead};
        struct Interrupted(usize);
        impl std::io::Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(1);
                Err(std::io::ErrorKind::Interrupted.into())
            }
        }
        let mut reader = RichPipe::new(RichPart::Plain);
        let mut input = std::io::Cursor::new(vec![b'p'; 100_000]);
        assert!(matches!(reader.read(&mut input), RichRead::Pending));
        assert_eq!(input.position(), 65_536);
        let RichRead::Finished(value) = reader.read(&mut input) else {
            panic!("remaining bytes and EOF");
        };
        assert_eq!(value.unwrap().len(), 100_000);

        let mut interrupted = Interrupted(0);
        assert!(matches!(
            RichPipe::new(RichPart::Html).read(&mut interrupted),
            RichRead::Pending
        ));
        assert_eq!(interrupted.0, 8, "signals cannot monopolize the event loop");
    }

    #[test]
    fn rich_pipe_retains_partial_utf8_across_readiness_callbacks() {
        use image_worker_impl::{RichPart, RichPipe, RichRead};
        use std::io::Write as _;
        let (mut source, mut sink) = std::os::unix::net::UnixStream::pair().unwrap();
        sink.set_nonblocking(true).unwrap();
        let mut reader = RichPipe::new(RichPart::Html);
        source.write_all(&[0xf0, 0x9f]).unwrap();
        assert!(matches!(reader.read(&mut sink), RichRead::Pending));
        source.write_all(&[0x98, 0x80]).unwrap();
        drop(source);
        let RichRead::Finished(value) = reader.read(&mut sink) else {
            panic!("closed producer must complete");
        };
        assert_eq!(value.as_deref(), Some("😀"));
    }
}

#[cfg(test)]
#[path = "wayland_protocol_tests.rs"]
mod protocol_tests;
