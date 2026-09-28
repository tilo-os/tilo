//! `wl_data_device` + `zwp_primary_selection_device_v1` on the app's own
//! Wayland connection — the native clipboard for a Wayland app; see
//! the crate docs for why this, rather than `arboard`, serves Wayland.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the Wayland clipboard owns its event thread"
)]

use std::sync::Arc;

use crate::{
    ClipboardBackend, ClipboardTarget, MAX_CLIPBOARD_IMAGE_BYTES, MAX_CLIPBOARD_IMAGE_INPUT_BYTES,
};

const CLIPBOARD_READ_BUDGET: tilo_clock::Duration = tilo_clock::Duration::from_millis(40);
const CLIPBOARD_SHUTDOWN_BUDGET: tilo_clock::Duration = tilo_clock::Duration::from_millis(250);

fn recv_bounded<T>(
    receiver: &std::sync::mpsc::Receiver<T>,
    budget: tilo_clock::Duration,
) -> Option<T> {
    receiver.recv_timeout(budget).ok()
}

fn join_bounded(thread: std::thread::JoinHandle<()>, budget: tilo_clock::Duration) -> bool {
    let now = tilo_clock::now();
    let deadline = now.checked_add(budget).unwrap_or(now);
    while !thread.is_finished() && tilo_clock::now() < deadline {
        std::thread::yield_now();
    }
    if thread.is_finished() {
        let _ = thread.join();
        true
    } else {
        false
    }
}

/// `wl_data_device` + `zwp_primary_selection_device_v1` on the app's own
/// Wayland connection.
///
/// Built on `smithay-clipboard`, which runs the selection protocol on a
/// dedicated event queue over the *existing* connection. Sharing the
/// connection is the point: the compositor gates `set_selection` on keyboard
/// focus, and only the connection that owns the focused surface has it. A
/// second, separate connection would be a different client, would never hold
/// focus, and every copy from it would be refused.
pub struct WaylandClipboard {
    /// `smithay_clipboard::Clipboard` owns an mpsc receiver, so it is `Send`
    /// but not `Sync`. Each target has its own instance and read thread: a
    /// peer that never closes its clipboard pipe can therefore stall at most
    /// one bounded reader and can never starve the other target.
    read_clipboard: Option<Arc<std::sync::Mutex<smithay_clipboard::Clipboard>>>,
    read_primary: Option<Arc<std::sync::Mutex<smithay_clipboard::Clipboard>>>,
    write_inner: Option<Arc<std::sync::Mutex<smithay_clipboard::Clipboard>>>,
    publication: PublicationClipboard,
    clipboard_cache: Arc<std::sync::Mutex<Option<String>>>,
    primary_cache: Arc<std::sync::Mutex<Option<String>>>,
    clipboard_refresh_inflight: Arc<std::sync::atomic::AtomicBool>,
    primary_refresh_inflight: Arc<std::sync::atomic::AtomicBool>,
    clipboard_refresh_generation: Arc<std::sync::atomic::AtomicU64>,
    primary_refresh_generation: Arc<std::sync::atomic::AtomicU64>,
    wake: tilo_rt::wake::WakeHandle,
}

struct PublicationClipboard {
    sender: smithay_client_toolkit::reexports::calloop::channel::Sender<ClipboardCommand>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

enum ClipboardCommand {
    SetText(Vec<u8>, std::sync::mpsc::Sender<bool>),
    SetImage(Vec<u8>, std::sync::mpsc::Sender<bool>),
    Exit,
}

impl WaylandClipboard {
    /// Attach to the `wl_display` behind `display_ptr`.
    ///
    /// # Safety
    ///
    /// `display_ptr` must be a live `wl_display *` that outlives the returned
    /// value. In practice it comes from winit's display handle and lives as
    /// long as the event loop, which outlives every widget that could copy.
    pub unsafe fn new(display_ptr: *mut std::ffi::c_void) -> Self {
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
        let thread = std::thread::Builder::new()
            .name("tilo-wayland-clipboard".into())
            .spawn(move || {
                publication_worker(connection, receiver);
                worker_alive.store(false, std::sync::atomic::Ordering::Release);
            })
            .ok();
        if thread.is_none() {
            alive.store(false, std::sync::atomic::Ordering::Release);
        }
        Self {
            read_clipboard: Some(Arc::new(std::sync::Mutex::new(unsafe {
                smithay_clipboard::Clipboard::new(display_ptr)
            }))),
            read_primary: Some(Arc::new(std::sync::Mutex::new(unsafe {
                smithay_clipboard::Clipboard::new(display_ptr)
            }))),
            write_inner: Some(Arc::new(std::sync::Mutex::new(unsafe {
                smithay_clipboard::Clipboard::new(display_ptr)
            }))),
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
            wake: tilo_rt::wake::WakeHandle::new(),
        }
    }

    /// Construct a publication backend around an already-connected Wayland
    /// client. This is intentionally public only as a protocol test seam: the
    /// normal application path uses [`Self::new`] with winit's live display.
    #[doc(hidden)]
    pub fn new_for_wayland_test(
        connection: smithay_client_toolkit::reexports::client::Connection,
    ) -> Self {
        let (sender, receiver) = smithay_client_toolkit::reexports::calloop::channel::channel();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_alive = std::sync::Arc::clone(&alive);
        let thread = std::thread::Builder::new()
            .name("tilo-wayland-clipboard-test".into())
            .spawn(move || {
                publication_worker(connection, receiver);
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
            wake: tilo_rt::wake::WakeHandle::new(),
        }
    }
}

fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
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

    use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
    use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
    use sctk::data_device_manager::data_source::DataSourceHandler;
    use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
    use sctk::reexports::calloop::{self, EventLoop, LoopHandle, PostAction};
    use sctk::reexports::calloop_wayland_source::WaylandSource;
    use sctk::reexports::client::backend::ObjectId;
    use sctk::reexports::client::globals::registry_queue_init;
    use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
    use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
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
    use sctk::{
        delegate_data_device, delegate_pointer, delegate_registry, delegate_seat, delegate_touch,
    };
    use smithay_client_toolkit as sctk;

    use super::ClipboardCommand;

    #[derive(Debug, Default)]
    struct SeatData {
        keyboard: Option<WlKeyboard>,
        pointer: Option<WlPointer>,
        touch: Option<WlTouch>,
        data_device: Option<DataDevice>,
        has_focus: bool,
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
        pending_image: Option<Arc<[u8]>>,
        inflight_transfers: usize,
        inflight_bytes: usize,
        exit: bool,
    }

    pub(super) struct SourcePayload {
        pub(super) image: Option<Arc<[u8]>>,
        pub(super) text: Option<Arc<[u8]>>,
    }

    impl SourcePayload {
        pub(super) fn bytes_for_mime(&self, mime: &str) -> Option<Arc<[u8]>> {
            match mime {
                "image/png" => self.image.clone(),
                "text/plain;charset=utf-8" | "text/plain" | "UTF8_STRING" | "STRING" | "TEXT" => {
                    self.text.clone()
                }
                _ => None,
            }
        }
    }

    pub fn run(connection: Connection, commands: calloop::channel::Channel<ClipboardCommand>) {
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
            pending_image: None,
            inflight_transfers: 0,
            inflight_bytes: 0,
            exit: false,
        };
        let _ = loop_handle.insert_source(commands, |event, (), state| {
            if let calloop::channel::Event::Msg(command) = event {
                match command {
                    ClipboardCommand::SetText(text, reply) => {
                        state.store_text(text);
                        let _ = reply.send(state.publish_current());
                    }
                    ClipboardCommand::SetImage(png, reply) => {
                        state.store_image(png);
                        let _ = reply.send(state.publish_current());
                    }
                    ClipboardCommand::Exit => state.exit = true,
                }
            }
        });
        let _ = WaylandSource::new(connection, queue).insert(loop_handle);
        while !state.exit {
            if event_loop.dispatch(None, &mut state).is_err() {
                break;
            }
        }
    }

    impl State {
        fn store_text(&mut self, text: Vec<u8>) {
            self.text_contents = Some(Arc::from(text));
            self.pending_image = None;
        }

        fn store_image(&mut self, contents: Vec<u8>) {
            self.pending_image = Some(Arc::from(contents));
        }

        fn publish_current(&mut self) -> bool {
            let Some(seat) = self.latest_seat.clone() else {
                return false;
            };
            let Some(seat_data) = self.seats.get(&seat) else {
                return false;
            };
            if !seat_data.has_focus {
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
            if self.text_contents.is_some() {
                mime_types.extend([
                    "text/plain;charset=utf-8",
                    "text/plain",
                    "UTF8_STRING",
                    "STRING",
                    "TEXT",
                ]);
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
            // Keep the latest image around just like text. If this command
            // arrived before the seat's focus/serial event, publish_current
            // will be retried from that event rather than silently dropping
            // the user's copy.
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
        fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
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
                        data.data_device =
                            Some(self.data_device_manager.get_data_device(qh, &seat));
                    }
                }
                Capability::Pointer => {
                    data.pointer = self.seat_registry.get_pointer(qh, &seat).ok();
                    if data.data_device.is_none() {
                        data.data_device =
                            Some(self.data_device_manager.get_data_device(qh, &seat));
                    }
                }
                Capability::Touch => {
                    data.touch = self.seat_registry.get_touch(qh, &seat).ok();
                    if data.data_device.is_none() {
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
                    Capability::Keyboard => data.keyboard = None,
                    Capability::Pointer => data.pointer = None,
                    Capability::Touch => data.touch = None,
                    _ => {}
                }
            }
        }
        fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
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
            for event in events {
                match event.kind {
                    PointerEventKind::Enter { serial } => {
                        data.latest_serial = serial;
                        data.has_focus = true;
                    }
                    PointerEventKind::Leave { .. } => data.has_focus = false,
                    _ => {}
                }
                if let PointerEventKind::Press { serial, .. }
                | PointerEventKind::Release { serial, .. } = event.kind
                {
                    data.latest_serial = serial;
                    self.latest_seat = Some(seat.clone());
                }
            }
            self.publish_current();
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
            data.has_focus = true;
            self.latest_seat = Some(seat);
            self.publish_current();
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

        fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, touch: &WlTouch) {
            let Some(touch_data) = touch.data::<TouchData>() else {
                return;
            };
            if let Some(data) = self.seats.get_mut(touch_data.seat()) {
                data.has_focus = false;
            }
        }
    }

    impl Dispatch<WlKeyboard, WlSeat, State> for State {
        fn event(
            state: &mut State,
            _: &WlKeyboard,
            event: <WlKeyboard as Proxy>::Event,
            seat: &WlSeat,
            _: &Connection,
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
                }
                Event::Enter { serial, .. } => {
                    data.latest_serial = serial;
                    data.has_focus = true;
                    state.latest_seat = Some(seat.clone());
                }
                Event::Leave { .. } => {
                    data.latest_serial = 0;
                    data.has_focus = false;
                }
                _ => {}
            }
            state.publish_current();
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
    delegate_data_device!(State);
    delegate_registry!(State);
    delegate_touch!(State);
}

fn publication_worker(
    connection: smithay_client_toolkit::reexports::client::Connection,
    commands: smithay_client_toolkit::reexports::calloop::channel::Channel<ClipboardCommand>,
) {
    image_worker_impl::run(connection, commands);
}

impl ClipboardBackend for WaylandClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        match target {
            ClipboardTarget::Clipboard => {
                let (reply, result) = std::sync::mpsc::channel();
                let ok = self
                    .send_publication(ClipboardCommand::SetText(text.as_bytes().to_vec(), reply));
                if ok && let Ok(mut cache) = self.clipboard_cache.lock() {
                    *cache = Some(text.to_owned());
                }
                ok && recv_bounded(&result, CLIPBOARD_READ_BUDGET).unwrap_or(false)
            }
            ClipboardTarget::Primary => {
                let Some(inner) = self.write_inner.as_ref() else {
                    return false;
                };
                let Ok(inner) = inner.try_lock() else {
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
        let (reply, result) = std::sync::mpsc::channel();
        self.send_publication(ClipboardCommand::SetImage(png, reply))
            && recv_bounded(&result, CLIPBOARD_READ_BUDGET).unwrap_or(false)
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
                    inner.lock().ok().and_then(|clipboard| match target {
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
        let _ = self.publication.sender.send(ClipboardCommand::Exit);
        if let Ok(mut thread) = self.publication.thread.lock()
            && let Some(thread) = thread.take()
        {
            let _ = join_bounded(thread, CLIPBOARD_SHUTDOWN_BUDGET);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                recv_bounded(&rx, std::time::Duration::from_secs(30)),
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
        // phase: shutdown_join_is_bounded_when_a_protocol_worker_is_stuck
        {
            // The worker stays blocked until this test releases it, so a
            // `join_bounded` that returns at all has given up on its own budget.
            let (release, stuck) = std::sync::mpsc::channel::<()>();
            let worker = std::thread::spawn(move || {
                let _ = stuck.recv();
            });
            assert!(!join_bounded(worker, std::time::Duration::from_millis(2)));
            drop(release);

            let finished = std::thread::spawn(|| {});
            assert!(
                join_bounded(finished, tilo_test_env::hang_bound()),
                "a worker that exits is joined within the budget"
            );
        }
        // phase: source_payload_rejects_unadvertised_mime_without_falling_back_to_text
        {
            use image_worker_impl::SourcePayload;

            let payload = SourcePayload {
                image: Some(Arc::from([1u8, 2, 3].as_slice())),
                text: Some(Arc::from(b"caption".as_slice())),
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
        }
    }
}
