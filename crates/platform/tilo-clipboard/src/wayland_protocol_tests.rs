//! Real protocol events against the production worker, over a private socket
//! pair. The tiny server controls ordering and two seats; compositor permission
//! enforcement remains covered by shell's `clipboard_e2e` tests.

use std::io::Write as _;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::client;
use wayland_server::protocol::{
    wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
    wl_keyboard, wl_pointer, wl_seat, wl_surface,
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::{ClipboardCommand, Publication, PublicationPayload, Settle, WaylandClipboard};
use crate::ClipboardBackend as _;

// Owns a real libwayland connection over the fixture's private socket; never
// borrows the user's display or clipboard.
struct OwnedTestDisplay(client::Connection);

impl raw_window_handle::HasDisplayHandle for OwnedTestDisplay {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        let pointer = std::ptr::NonNull::new(self.0.backend().display_ptr().cast())
            .expect("live owned fixture display");
        let raw = raw_window_handle::WaylandDisplayHandle::new(pointer);
        // SAFETY: this connection owns the display throughout the borrow.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw.into()) })
    }
}

const PLAIN: &str = "text/plain;charset=utf-8";

/// A copy deadline no test outlives, for tests about the order of the
/// compositor's answers rather than the copy's budget.
fn outlasting_deadline() -> tilo_clock::Instant {
    tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound().saturating_mul(2))
        .expect("publication deadline beyond this test")
}

/// A copy queued directly, with its own deadline, as `publish` queues one.
fn publish(
    payload: PublicationPayload,
    reply: mpsc::Sender<bool>,
    deadline: tilo_clock::Instant,
) -> ClipboardCommand {
    ClipboardCommand::Publish(Publication::new(
        payload,
        Settle::new(move |copied| {
            let _ = reply.send(copied);
        }),
        deadline,
    ))
}
type Held = Vec<(String, OwnedFd)>;
type Operation = Box<dyn FnOnce(&mut Server, &DisplayHandle) + Send>;

#[derive(Default)]
struct Seat {
    keyboards: Vec<wl_keyboard::WlKeyboard>,
    devices: Vec<wl_data_device::WlDataDevice>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    device: Option<wl_data_device::WlDataDevice>,
    surface: Option<wl_surface::WlSurface>,
}

#[derive(Default)]
struct Server {
    seats: [Seat; 2],
    surfaces: Vec<wl_surface::WlSurface>,
    held: Held,
    received: Vec<(String, String)>,
    serial: u32,
    hold_flush: bool,
    hold_flush_after_selection: bool,
    publications: Vec<u32>,
    /// The source of the newest client selection.
    selection: Option<wl_data_source::WlDataSource>,
    selection_gate: Option<SelectionGate>,
}

struct SelectionGate {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Offer {
    label: String,
    hold: bool,
}

impl Server {
    fn seat(&mut self, index: usize) -> &mut Seat {
        self.seats.get_mut(index).expect("fixture seat")
    }

    fn select(&mut self, handle: &DisplayHandle, index: usize, offer: Option<Offer>) {
        let device = self.seat(index).device.clone().expect("bound data device");
        let resource = offer.map(|offer| {
            let resource = device
                .client()
                .expect("live client")
                .create_resource::<wl_data_offer::WlDataOffer, _, Self>(handle, 3, offer)
                .expect("server offer");
            device.data_offer(&resource);
            resource.offer("text/html".into());
            resource.offer(PLAIN.into());
            resource
        });
        device.selection(resource.as_ref());
    }

    fn enter(&mut self, index: usize, surface: usize) {
        self.serial = self.serial.checked_add(1).expect("fixture serial");
        let serial = self.serial;
        let surface = self
            .surfaces
            .get(surface)
            .expect("real client surface")
            .clone();
        let seat = self.seat(index);
        seat.keyboard
            .as_ref()
            .expect("bound keyboard")
            .enter(serial, &surface, Vec::new());
        seat.surface = Some(surface);
    }

    fn leave(&mut self, index: usize) {
        self.serial = self.serial.checked_add(1).expect("fixture serial");
        let serial = self.serial;
        let seat = self.seat(index);
        let surface = seat.surface.take().expect("focused fixture seat");
        seat.keyboard
            .as_ref()
            .expect("bound keyboard")
            .leave(serial, &surface);
    }

    fn modifiers(&mut self, index: usize) {
        self.serial = self.serial.checked_add(1).expect("fixture serial");
        let serial = self.serial;
        self.seat(index)
            .keyboard
            .as_ref()
            .expect("bound keyboard")
            .modifiers(serial, 0, 0, 0, 0);
    }

    /// The pointer enters `surface` and leaves it again, as when it moves
    /// across a window onto desktop chrome. Keyboard focus stays put.
    fn pointer_crossing(&mut self, index: usize, surface: usize) {
        let surface = self
            .surfaces
            .get(surface)
            .expect("real client surface")
            .clone();
        let pointer = self.seat(index).pointer.clone().expect("bound pointer");
        self.serial = self.serial.checked_add(1).expect("fixture serial");
        pointer.enter(self.serial, &surface, 1.0, 1.0);
        pointer.frame();
        self.serial = self.serial.checked_add(1).expect("fixture serial");
        pointer.leave(self.serial, &surface);
        pointer.frame();
    }
}

impl GlobalDispatch<wl_seat::WlSeat, usize> for Server {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_seat::WlSeat>,
        index: &usize,
        init: &mut DataInit<'_, Self>,
    ) {
        let seat = init.init(resource, *index);
        seat.name(format!("fixture-seat-{index}"));
        seat.capabilities(wl_seat::Capability::Keyboard | wl_seat::Capability::Pointer);
    }
}

impl Dispatch<wl_seat::WlSeat, usize> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_seat::WlSeat,
        request: wl_seat::Request,
        index: &usize,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wl_seat::Request::GetKeyboard { id } = request {
            let keyboard = init.init(id, ());
            let empty = std::fs::File::open("/dev/null").expect("empty keymap descriptor");
            keyboard.keymap(wl_keyboard::KeymapFormat::NoKeymap, empty.as_fd(), 0);
            keyboard.repeat_info(25, 600);
            state.seat(*index).keyboards.push(keyboard.clone());
            state.seat(*index).keyboard = Some(keyboard);
        } else if let wl_seat::Request::GetPointer { id } = request {
            state.seat(*index).pointer = Some(init.init(id, ()));
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_pointer::WlPointer,
        _: wl_pointer::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<wl_data_device_manager::WlDataDeviceManager, ()> for Server {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_data_device_manager::WlDataDeviceManager>,
        (): &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_data_device_manager::WlDataDeviceManager,
        request: wl_data_device_manager::Request,
        (): &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_data_device_manager::Request::GetDataDevice { id, seat } => {
                let index = *seat.data::<usize>().expect("seat identity");
                let device = init.init(id, ());
                state.seat(index).devices.push(device.clone());
                state.seat(index).device = Some(device);
            }
            wl_data_device_manager::Request::CreateDataSource { id } => {
                init.init(id, SourceMimes::default());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_offer::WlDataOffer, Offer> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_data_offer::WlDataOffer,
        request: wl_data_offer::Request,
        offer: &Offer,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wl_data_offer::Request::Receive { mime_type, fd } = request {
            state
                .received
                .push((offer.label.clone(), mime_type.clone()));
            if offer.hold {
                state.held.push((mime_type, fd));
            } else {
                let bytes = match mime_type.as_str() {
                    "text/html" => format!("<b>{}</b>", offer.label),
                    PLAIN => format!("plain:{}", offer.label),
                    other => panic!("unexpected MIME {other}"),
                };
                std::fs::File::from(fd)
                    .write_all(bytes.as_bytes())
                    .expect("small fixture payload");
            }
        }
    }
}

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for Server {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<wl_compositor::WlCompositor>,
        (): &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        (): &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wl_compositor::Request::CreateSurface { id } = request {
            state.surfaces.push(init.init(id, ()));
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_keyboard::WlKeyboard,
        _: wl_keyboard::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
impl Dispatch<wl_data_device::WlDataDevice, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &wl_data_device::WlDataDevice,
        request: wl_data_device::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wl_data_device::Request::SetSelection {
            source: Some(source),
            serial,
        } = request
        {
            state.selection = Some(source);
            assert!(state.seats.iter().any(|seat| seat.surface.is_some()));
            if let Some(gate) = &state.selection_gate {
                gate.entered.store(true, Ordering::Release);
                while !gate.release.load(Ordering::Acquire) {
                    std::thread::park_timeout(Duration::from_millis(1));
                }
            }
            state.publications.push(serial);
            if state.hold_flush_after_selection {
                state.hold_flush = true;
            }
        }
    }
}
/// The MIME types a client source offered, in order.
#[derive(Default)]
struct SourceMimes(std::sync::Mutex<Vec<String>>);

impl Dispatch<wl_data_source::WlDataSource, SourceMimes> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_data_source::WlDataSource,
        request: wl_data_source::Request,
        mimes: &SourceMimes,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wl_data_source::Request::Offer { mime_type } = request {
            mimes.0.lock().expect("source MIMEs").push(mime_type);
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_surface::WlSurface,
        _: wl_surface::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

#[derive(Default)]
struct Host {
    surfaces: Vec<client::protocol::wl_surface::WlSurface>,
}

impl client::Dispatch<client::protocol::wl_registry::WlRegistry, ()> for Host {
    fn event(
        state: &mut Self,
        registry: &client::protocol::wl_registry::WlRegistry,
        event: client::protocol::wl_registry::Event,
        (): &(),
        _: &client::Connection,
        queue: &client::QueueHandle<Self>,
    ) {
        if let client::protocol::wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "wl_compositor"
        {
            let compositor = registry.bind::<client::protocol::wl_compositor::WlCompositor, _, _>(
                name,
                1,
                queue,
                (),
            );
            state.surfaces.push(compositor.create_surface(queue, ()));
            state.surfaces.push(compositor.create_surface(queue, ()));
        }
    }
}

client::delegate_noop!(Host: ignore client::protocol::wl_compositor::WlCompositor);
client::delegate_noop!(Host: ignore client::protocol::wl_surface::WlSurface);

struct Harness {
    clipboard: Option<WaylandClipboard>,
    operations: Option<mpsc::Sender<Operation>>,
    thread: Option<std::thread::JoinHandle<()>>,
    deadline: Instant,
    connection: client::Connection,
    _host: Host,
    _queue: client::EventQueue<Host>,
}

#[test]
fn runner_attachment_retains_a_real_display_for_an_inflight_native_reader() {
    let _system = crate::tests::SYSTEM.lock();
    let fixture = Harness::with_deadline(ReadDeadline::NoWorker);
    let display = Arc::new(OwnedTestDisplay(fixture.connection.clone()));
    let retained_display = Arc::downgrade(&display);
    let attachment = crate::attach_wayland_display(display.clone()).expect("native attachment");
    assert!(
        crate::attach_wayland_display(display.clone()).is_none(),
        "only one attachment"
    );
    drop(display);
    fixture.wait(
        || {
            fixture.query(|state| {
                state.seats.iter().all(|seat| {
                    seat.devices
                        .iter()
                        .filter(|device| device.is_alive())
                        .count()
                        == 4
                        && seat
                            .keyboards
                            .iter()
                            .filter(|keyboard| keyboard.is_alive())
                            .count()
                            == 4
                })
            })
        },
        "all production readers and publisher",
    );
    fixture.assert_attached_secret_copy();
    let clipboard = crate::system();
    fixture.operation(|state, handle| {
        let surface = state.surfaces[0].clone();
        for seat in &state.seats {
            for keyboard in seat.keyboards.iter().filter(|keyboard| keyboard.is_alive()) {
                keyboard.enter(10, &surface, Vec::new());
                // Smithay chooses its active seat on key/modifier events,
                // not Enter. Activate every reader binding, not just the
                // last keyboard retained by the fixture's seat helper.
                keyboard.modifiers(11, 0, 0, 0, 0);
            }
            for device in seat.devices.iter().filter(|device| device.is_alive()) {
                let offer = device
                    .client()
                    .unwrap()
                    .create_resource::<wl_data_offer::WlDataOffer, _, Server>(
                        handle,
                        3,
                        Offer {
                            label: "held-native-read".into(),
                            hold: true,
                        },
                    )
                    .unwrap();
                device.data_offer(&offer);
                offer.offer(PLAIN.into());
                device.selection(Some(&offer));
            }
        }
    });
    // A first call may race delivery of focus/selection. Retry only until the
    // server has actually received a production smithay loader's pipe.
    fixture.wait(
        || {
            let _ = clipboard.text(crate::ClipboardTarget::Clipboard);
            fixture.query(|state| !state.held.is_empty())
        },
        "production native reader pipe",
    );
    drop(attachment);
    fixture.wait(
        crate::native::reaped_wayland_for_test,
        "parent backend and unrelated workers reaped",
    );
    assert!(
        retained_display.upgrade().is_some(),
        "loader lost its display owner"
    );
    assert!(clipboard.text(crate::ClipboardTarget::Clipboard).is_none());
    assert!(!clipboard.set_text(crate::ClipboardTarget::Clipboard, "late copy"));
    assert!(!clipboard.set_shared_text(crate::ClipboardTarget::Primary, "late secret"));
    assert!(!clipboard.set_secret_text("late secret"));
    assert_closed_copies(clipboard.as_ref());
    assert!(!clipboard.set_image(1, 1, &[0; 4]));
    assert!(clipboard.image().is_none());
    assert!(clipboard.rich_text().is_none());
    assert_eq!(
        clipboard
            .request_text(crate::ClipboardTarget::Clipboard)
            .recv_timeout(fixture.remaining())
            .unwrap(),
        None
    );
    assert!(
        crate::attach_wayland_display(OwnedTestDisplay(fixture.connection.clone())).is_none(),
        "close must not reopen readers or increase the worker ceiling"
    );
    fixture.operation(|state, _| state.held.clear());
    fixture.wait(
        || retained_display.upgrade().is_none(),
        "all display owners released",
    );
    crate::native::clear_wayland_for_test();
}

fn assert_closed_copies(clipboard: &dyn crate::ClipboardBackend) {
    for content in [
        crate::CopyContent::Text("late copy"),
        crate::CopyContent::SharedText("late shared copy"),
        crate::CopyContent::SecretText("late secret"),
        crate::CopyContent::Image {
            width: 1,
            height: 1,
            rgba: &[0; 4],
        },
    ] {
        assert_eq!(clipboard.copy(content).outcome(), Some(false));
    }
}

#[test]
fn runner_close_does_not_wait_for_initial_registry_replies() {
    let _system = crate::tests::SYSTEM.lock();
    let fixture = Harness::with_deadline(ReadDeadline::NoWorker);
    fixture.operation(|state, _| state.hold_flush = true);
    fixture.query(|_| ());
    let display = Arc::new(OwnedTestDisplay(fixture.connection.clone()));
    let retained_display = Arc::downgrade(&display);
    let attachment = crate::attach_wayland_display(display).expect("native attachment");
    let (notification, received) = mpsc::channel();
    let closer = std::thread::spawn(move || {
        drop(attachment);
        notification.send(()).unwrap();
    });
    let result = received.recv_timeout(tilo_test_env::hang_bound() / 2);
    assert!(
        retained_display.upgrade().is_some(),
        "initializing workers lost their display"
    );
    // Release even when close failed, so reporting the assertion cannot itself
    // hang fixture teardown in an uninterruptible initial registry roundtrip.
    fixture.operation(|state, _| state.hold_flush = false);
    assert!(
        result.is_ok(),
        "runner close waited for protocol initialization"
    );
    closer.join().unwrap();
    fixture.wait(
        || retained_display.upgrade().is_none(),
        "initialized workers reaped",
    );
    crate::native::clear_wayland_for_test();
}

#[derive(Clone, Copy)]
enum ReadDeadline {
    Production,
    AfterTest,
    NoWorker,
}

impl Harness {
    fn new() -> Self {
        Self::with_deadline(ReadDeadline::AfterTest)
    }

    fn with_deadline(read_deadline: ReadDeadline) -> Self {
        let bound = tilo_test_env::hang_bound();
        let deadline = Instant::now().checked_add(bound).expect("test deadline");
        // A cancellation test must fail before a transfer timeout could make
        // its closure assertion pass. Every wait shares this absolute deadline.
        let read_budget = bound.checked_mul(2).expect("cancellation read budget");
        let (server_socket, client_socket) = UnixStream::pair().expect("private protocol socket");
        let (send, receive) = mpsc::channel::<Operation>();
        let thread = std::thread::spawn(move || {
            let mut display = Display::<Server>::new().expect("fixture display");
            let mut handle = display.handle();
            handle.create_global::<Server, wl_compositor::WlCompositor, _>(1, ());
            handle.create_global::<Server, wl_data_device_manager::WlDataDeviceManager, _>(3, ());
            for index in 0..2 {
                handle.create_global::<Server, wl_seat::WlSeat, _>(7, index);
            }
            let _client = handle
                .insert_client(server_socket, Arc::new(()))
                .expect("fixture client");
            let mut state = Server::default();
            while Instant::now() < deadline {
                display
                    .dispatch_clients(&mut state)
                    .expect("dispatch protocol requests");
                match receive.recv_timeout(Duration::from_millis(1)) {
                    Ok(operation) => operation(&mut state, &handle),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if !state.hold_flush {
                    display.flush_clients().expect("flush fixture events");
                }
            }
        });
        let connection =
            client::Connection::from_socket(client_socket).expect("fixture connection");
        let mut queue = connection.new_event_queue::<Host>();
        let mut host = Host::default();
        connection.display().get_registry(&queue.handle(), ());
        queue.roundtrip(&mut host).expect("host registry");
        queue.roundtrip(&mut host).expect("real client surfaces");
        let clipboard = match read_deadline {
            ReadDeadline::Production => {
                Some(WaylandClipboard::new_for_wayland_test(connection.clone()))
            }
            ReadDeadline::AfterTest => {
                Some(WaylandClipboard::new_for_wayland_test_with_read_budget(
                    connection.clone(),
                    read_budget,
                ))
            }
            ReadDeadline::NoWorker => None,
        };
        let harness = Self {
            clipboard,
            operations: Some(send),
            thread: Some(thread),
            deadline,
            connection,
            _host: host,
            _queue: queue,
        };
        if harness.clipboard.is_some() {
            harness.wait(
                || {
                    harness.query(|state| {
                        state.seats.iter().all(|seat| {
                            seat.keyboard.is_some()
                                && seat.pointer.is_some()
                                && seat.device.is_some()
                        }) && state.surfaces.len() == 2
                    })
                },
                "worker bindings",
            );
            harness.fence();
        }
        harness
    }

    fn assert_attached_secret_copy(&self) {
        self.operation(|state, _| {
            state.enter(0, 0);
            let surface = &state.surfaces[0];
            for seat in &state.seats {
                for keyboard in seat.keyboards.iter().filter(|keyboard| keyboard.is_alive()) {
                    keyboard.enter(10, surface, Vec::new());
                }
            }
        });
        self.query(|_| ());
        assert!(crate::system().set_secret_text("attached secret"));
        let mimes = self.selection_mimes();
        for marker in [crate::SECRET_MIME, crate::PASSWORD_MANAGER_HINT_MIME] {
            assert!(mimes.iter().any(|mime| mime == marker));
            assert_eq!(self.selection_bytes(marker), crate::SECRET_MARKER);
        }
        assert_eq!(self.selection_bytes("text/plain"), b"attached secret");
    }

    fn remaining(&self) -> Duration {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "fixture exceeded its absolute failure deadline"
        );
        remaining
    }

    fn backend(&self) -> &WaylandClipboard {
        self.clipboard.as_ref().expect("live backend")
    }

    fn operation(&self, operation: impl FnOnce(&mut Server, &DisplayHandle) + Send + 'static) {
        self.operations
            .as_ref()
            .expect("live server")
            .send(Box::new(operation))
            .expect("server operation");
    }

    fn query<T: Send + 'static>(&self, query: impl FnOnce(&mut Server) -> T + Send + 'static) -> T {
        let (send, receive) = mpsc::channel();
        self.operation(move |state, _| {
            send.send(query(state)).expect("fixture query receiver");
        });
        receive
            .recv_timeout(self.remaining())
            .expect("fixture query")
    }

    fn fence(&self) {
        // First make every server operation preceding this query visible.
        self.query(|_| ());
        let (send, receive) = mpsc::channel();
        self.backend()
            .publication
            .sender
            .send(ClipboardCommand::Fence(send))
            .expect("worker fence request");
        receive
            .recv_timeout(self.remaining())
            .expect("worker processed preceding protocol events");
    }

    fn wait(&self, condition: impl Fn() -> bool, what: &str) {
        while !condition() {
            assert!(
                Instant::now() < self.deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        self.remaining();
    }

    fn select(&self, index: usize, label: Option<&str>, hold: bool) {
        let offer = label.map(|label| Offer {
            label: label.into(),
            hold,
        });
        self.operation(move |state, handle| state.select(handle, index, offer));
    }

    fn enter(&self, index: usize, surface: usize) {
        self.operation(move |state, _| state.enter(index, surface));
    }
    fn leave(&self, index: usize) {
        self.operation(move |state, _| state.leave(index));
    }

    /// The MIME types of the newest client selection.
    fn selection_mimes(&self) -> Vec<String> {
        self.query(|state| {
            state
                .selection
                .as_ref()
                .and_then(|source| source.data::<SourceMimes>())
                .map(|mimes| mimes.0.lock().expect("source MIMEs").clone())
                .unwrap_or_default()
        })
    }

    /// Ask the newest client selection for `mime`, as a pasting client does,
    /// and read the whole answer.
    fn selection_bytes(&self, mime: &str) -> Vec<u8> {
        let (mut reader, writer) = std::io::pipe().expect("transfer pipe");
        let mime = mime.to_owned();
        self.operation(move |state, _| {
            state
                .selection
                .as_ref()
                .expect("a client selection")
                .send(mime, OwnedFd::from(writer).as_fd());
        });
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut reader, &mut bytes);
            let _ = send.send(bytes);
        });
        receive
            .recv_timeout(self.remaining())
            .expect("the source finished its transfer")
    }

    fn assert_offer(&self, label: &str) {
        let html = format!("<b>{label}</b>");
        self.wait(
            || {
                self.backend()
                    .rich_text()
                    .is_some_and(|offer| offer.html == html)
            },
            "paired rich offer",
        );
        let offer = self.backend().rich_text().expect("stable completed offer");
        assert_eq!(offer.html, html);
        assert_eq!(offer.plain, Some(format!("plain:{label}")));
    }

    fn take_pending(&self) -> Held {
        self.wait(
            || self.query(|state| state.held.len()) == 2,
            "both worker pipes",
        );
        self.fence();
        self.query(|state| std::mem::take(&mut state.held))
    }

    fn assert_closed(&self, pipes: Held, phase: &str) {
        self.remaining();
        assert_eq!(pipes.len(), 2);
        assert!(pipes.iter().any(|(mime, _)| mime == "text/html"));
        assert!(pipes.iter().any(|(mime, _)| mime == PLAIN));
        for (mime, fd) in pipes {
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: the owned descriptor stays alive for the single pollfd.
            assert_eq!(
                unsafe { libc::poll(&raw mut poll, 1, 0) },
                1,
                "{phase}: reader closed for {mime}"
            );
            assert_ne!(
                poll.revents & libc::POLLERR,
                0,
                "{phase}: reader retained for {mime}"
            );
            let error = std::fs::File::from(fd)
                .write_all(b"late")
                .expect_err("late transfer must fail");
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        drop(self.clipboard.take());
        drop(self.operations.take());
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("protocol fixture thread");
            }
        }
    }
}

#[test]
fn selection_before_enter_and_same_client_surface_switch_keep_one_offer() {
    let h = Harness::new();
    h.select(0, Some("文🙂"), false);
    h.fence();
    assert!(
        h.query(|state| state.received.is_empty()),
        "selection alone cannot authorize a read"
    );
    assert!(h.backend().rich_text().is_none());
    h.enter(0, 0);
    h.assert_offer("文🙂");
    assert_eq!(h.query(|state| state.received.len()), 2);
    h.leave(0);
    h.fence();
    assert!(h.backend().rich_text().is_none());
    // A same-client surface switch need not repeat the selection event.
    h.enter(0, 1);
    h.assert_offer("文🙂");
    assert_eq!(h.query(|state| state.received.len()), 4);
}

/// Roundtrip completion runs at the backend level, before any event queue
/// dispatch. The worker must not dispatch it for the application: that can
/// complete the sync between the application's completion check and its poll,
/// leaving the application asleep on a reply that was already consumed.
#[test]
fn worker_leaves_application_callbacks_to_the_application() {
    use client::backend::{Backend, ObjectData, ObjectId, protocol::Message};

    struct Completion(mpsc::Sender<std::thread::ThreadId>);
    impl ObjectData for Completion {
        fn event(
            self: Arc<Self>,
            _: &Backend,
            _: Message<ObjectId, OwnedFd>,
        ) -> Option<Arc<dyn ObjectData>> {
            self.0
                .send(std::thread::current().id())
                .expect("completion receiver");
            None
        }

        fn destroyed(&self, _: ObjectId) {}
    }

    let h = Harness::new();
    let (completed, received) = mpsc::channel();
    h.connection
        .send_request(
            &h.connection.display(),
            client::protocol::wl_display::Request::Sync {},
            Some(Arc::new(Completion(completed))),
        )
        .expect("application sync request");
    h.connection.flush().expect("flush application sync");
    // The worker's own sync reply follows the application's on the same
    // socket, so once the fence returns the application's reply was read.
    h.fence();
    assert_eq!(
        received.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "the clipboard worker dispatched the application's sync callback"
    );
    h.connection
        .backend()
        .dispatch_inner_queue()
        .expect("application dispatch");
    assert_eq!(
        received.recv_timeout(h.remaining()),
        Ok(std::thread::current().id())
    );
}

#[test]
fn leave_and_null_cancel_both_pending_pipes_before_the_deadline() {
    let h = Harness::new();
    h.select(0, Some("stalled"), true);
    h.enter(0, 0);
    let pipes = h.take_pending();
    h.leave(0);
    h.fence();
    h.assert_closed(pipes, "Leave");
    assert!(h.backend().rich_text().is_none());
    h.select(0, Some("fresh"), false);
    h.enter(0, 1);
    h.assert_offer("fresh");
    h.select(0, Some("replaced"), true);
    let pipes = h.take_pending();
    h.select(0, None, false);
    h.fence();
    h.assert_closed(pipes, "NULL");
    assert!(h.backend().rich_text().is_none());
    h.select(0, Some("after NULL"), false);
    h.assert_offer("after NULL");
}

#[test]
fn active_seat_switch_and_stale_seat_events_preserve_authority() {
    let h = Harness::new();
    h.select(0, Some("first"), false);
    h.enter(0, 0);
    h.assert_offer("first");
    h.select(1, None, false);
    h.enter(1, 1);
    h.fence();
    assert!(
        h.backend().rich_text().is_none(),
        "a new seat with NULL cannot retain the previous seat's offer"
    );
    h.select(1, Some("second"), false);
    h.assert_offer("second");
    h.leave(0);
    h.select(0, None, false);
    h.fence();
    h.assert_offer("second");
    h.enter(0, 0);
    h.fence();
    assert!(h.backend().rich_text().is_none());
    h.select(1, Some("inactive"), false);
    h.fence();
    assert!(h.backend().rich_text().is_none());
    assert!(!h.query(|state| state.received.iter().any(|(label, _)| label == "inactive")));
    h.operation(|state, _| state.modifiers(1));
    h.assert_offer("inactive");
}

#[test]
fn production_deadline_closes_both_pipes_without_focus_or_selection_changes() {
    let h = Harness::with_deadline(ReadDeadline::Production);
    h.select(0, Some("timeout"), true);
    h.enter(0, 0);
    let pipes = h.take_pending();
    h.wait(
        || {
            pipes.iter().all(|(_, fd)| {
                let mut poll = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: the pipe owns the descriptor throughout this poll.
                let count = unsafe { libc::poll(&raw mut poll, 1, 0) };
                assert!(count >= 0, "poll pending pipe");
                count > 0 && poll.revents & libc::POLLERR != 0
            })
        },
        "production read deadline",
    );
    h.assert_closed(pipes, "production deadline");
    assert!(h.backend().rich_text().is_none());
    h.select(0, Some("after timeout"), false);
    h.assert_offer("after timeout");
}

#[test]
fn image_copy_acknowledgement_follows_server_selection() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();

    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let gate = SelectionGate {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    harness.operation(move |state, _| state.selection_gate = Some(gate));
    harness.query(|state| assert!(state.selection_gate.is_some()));

    let png = super::encode_png(1, 1, &[0xff, 0, 0x80, 0xff]).expect("small image");
    let (reply, result) = mpsc::channel();
    let deadline = outlasting_deadline();
    assert!(harness.backend().send_publication(publish(
        PublicationPayload::Image(png),
        reply,
        deadline
    )));
    harness.wait(
        || entered.load(Ordering::Acquire),
        "server receiving the image selection",
    );
    let early = result.try_recv();
    release.store(true, Ordering::Release);
    assert!(
        matches!(early, Err(mpsc::TryRecvError::Empty)),
        "copy acknowledged before the server processed its selection: {early:?}"
    );
    assert!(
        result
            .recv_timeout(harness.remaining())
            .expect("image copy acknowledgement")
    );
    harness.wait(
        || harness.query(|state| state.publications == [state.serial]),
        "server-side image selection",
    );
}

#[test]
fn copy_acknowledgement_waits_for_already_sent_keyboard_focus() {
    let harness = Harness::new();
    // The compositor has sent Enter, but its transport has not flushed it yet.
    // Do not force any focus state into the production worker.
    harness.query(|state| {
        state.hold_flush = true;
        state.enter(0, 0);
    });
    let (result_tx, result_rx) = mpsc::channel();
    assert!(harness.backend().send_publication(publish(
        PublicationPayload::Text(b"caption".to_vec()),
        result_tx,
        outlasting_deadline()
    )));
    // Observe FIFO processing of the actual production copy command. This
    // neither dispatches nor changes the worker's focus; a protocol fence
    // would flush the very race under test before the copy is processed.
    let (processed_tx, processed_rx) = mpsc::channel();
    harness
        .backend()
        .publication
        .sender
        .send(ClipboardCommand::CommandProcessed(processed_tx))
        .expect("worker command observation");
    processed_rx
        .recv_timeout(harness.remaining())
        .expect("copy command processed");
    let early_result = result_rx.try_recv();
    harness.query(|state| state.hold_flush = false);
    assert!(
        matches!(early_result, Err(mpsc::TryRecvError::Empty)),
        "copy must wait for the queued focus event instead of acknowledging failure: {early_result:?}"
    );
    assert!(
        result_rx
            .recv_timeout(harness.remaining())
            .expect("copy acknowledgement")
    );
    harness.wait(
        || harness.query(|state| state.publications == [state.serial]),
        "focus-gated publication",
    );
}

#[test]
fn copy_does_not_publish_after_an_already_sent_keyboard_leave() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    harness.query(|state| {
        state.hold_flush = true;
        state.leave(0);
    });
    let (reply, result) = mpsc::channel();
    assert!(harness.backend().send_publication(publish(
        PublicationPayload::Text(b"unfocused".to_vec()),
        reply,
        outlasting_deadline(),
    )));
    let (processed, observed) = mpsc::channel();
    harness
        .backend()
        .publication
        .sender
        .send(ClipboardCommand::CommandProcessed(processed))
        .expect("worker command observation");
    observed
        .recv_timeout(harness.remaining())
        .expect("copy processed");
    let early = result.try_recv();
    harness.query(|state| state.hold_flush = false);
    assert!(
        matches!(early, Err(mpsc::TryRecvError::Empty)),
        "copy used stale focus: {early:?}"
    );
    assert!(
        !result
            .recv_timeout(harness.remaining())
            .expect("copy refused")
    );
    harness.enter(0, 0);
    harness.fence();
    assert!(
        harness.query(|state| state.publications.is_empty()),
        "refused copy must not publish on later focus"
    );
}

#[test]
fn worker_shutdown_settles_dispatched_copies_with_the_connection_retained() {
    for published in [false, true] {
        let mut harness = Harness::new();
        harness.enter(0, 0);
        harness.fence();
        harness.query(move |state| {
            state.hold_flush = !published;
            state.hold_flush_after_selection = published;
        });
        let (reply, result) = mpsc::channel();
        let deadline = outlasting_deadline();
        assert!(harness.backend().send_publication(publish(
            PublicationPayload::Text(b"pending at shutdown".to_vec()),
            reply,
            deadline,
        )));
        let (processed, observed) = mpsc::channel();
        harness
            .backend()
            .publication
            .sender
            .send(ClipboardCommand::CommandProcessed(processed))
            .expect("worker command observation");
        observed
            .recv_timeout(harness.remaining())
            .expect("publication dispatched before shutdown");
        if published {
            harness.wait(
                || harness.query(|state| !state.publications.is_empty()),
                "selection with its confirmation withheld",
            );
        }
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));

        // The host queue and connection stay live in the harness. Callback
        // userdata therefore cannot be relied on to drop and report failure.
        drop(harness.clipboard.take());
        assert_eq!(
            result
                .recv_timeout(harness.remaining())
                .expect("shutdown must explicitly settle a dispatched copy"),
            published,
        );
        assert!(matches!(
            result.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        harness.query(move |state| {
            assert_eq!(state.publications.len(), usize::from(published));
            state.hold_flush = false;
        });
    }
}

#[test]
fn pointer_crossing_neither_revokes_nor_grants_copy_permission() {
    let h = Harness::new();
    h.enter(0, 0);
    h.fence();
    assert!(
        h.backend()
            .set_text(crate::ClipboardTarget::Clipboard, "caption")
    );
    h.operation(|state, _| state.pointer_crossing(0, 0));
    h.fence();
    assert!(
        h.backend().set_image(1, 1, &[0xff, 0, 0x80, 0xff]),
        "pointer leave refused a copy from the keyboard-focused client"
    );
    assert_eq!(h.query(|state| state.publications.len()), 2);

    h.leave(0);
    h.operation(|state, _| state.pointer_crossing(0, 0));
    h.fence();
    assert!(
        !h.backend().set_image(1, 1, &[0xff, 0, 0x80, 0xff]),
        "pointer focus authorized a copy without keyboard focus"
    );
    assert_eq!(h.query(|state| state.publications.len()), 2);
}

#[test]
fn input_after_a_copy_does_not_publish_it_again() {
    let h = Harness::new();
    h.enter(0, 0);
    h.fence();
    assert!(
        h.backend()
            .set_text(crate::ClipboardTarget::Clipboard, "caption")
    );
    // Another client copies, which cancels this client's source. The
    // earlier copy must not take the selection back when this client sees
    // input or focus again.
    h.operation(|state, _| {
        state
            .selection
            .take()
            .expect("published source")
            .cancelled();
        state.modifiers(0);
        state.pointer_crossing(0, 0);
        state.leave(0);
        state.enter(0, 0);
    });
    h.fence();
    assert_eq!(h.query(|state| state.publications.len()), 1);
}

#[test]
fn expired_copy_does_not_publish_on_current_or_later_focus() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    let (reply, result) = mpsc::channel();
    assert!(harness.backend().send_publication(publish(
        PublicationPayload::Text(b"expired".to_vec()),
        reply,
        tilo_clock::now(),
    )));
    assert!(
        !result
            .recv_timeout(harness.remaining())
            .expect("expired copy refused")
    );
    harness.leave(0);
    harness.enter(0, 0);
    harness.fence();
    assert!(
        harness.query(|state| state.publications.is_empty()),
        "expired copy must never publish"
    );
}

#[test]
fn explicit_plain_paste_survives_a_delayed_transfer_without_blocking_the_worker() {
    let h = Harness::new();
    h.select(0, Some("slow"), true);
    h.enter(0, 0);
    h.fence();
    let result = h.backend().request_text(crate::ClipboardTarget::Clipboard);
    h.wait(
        || h.query(|state| state.held.len() >= 3),
        "explicit plain receive",
    );
    // The fixture holds every pipe until it is written below.
    assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
    h.fence(); // A pending clipboard transfer must not stall protocol dispatch.
    h.query(|state| {
        let (_, fd) = state.held.pop().expect("explicit paste pipe");
        std::fs::File::from(fd)
            .write_all(b"delayed selection")
            .unwrap();
    });
    assert_eq!(
        result.recv_timeout(h.remaining()).unwrap(),
        Some("delayed selection".into())
    );
}

#[test]
fn explicit_plain_paste_is_revoked_on_keyboard_leave() {
    let h = Harness::new();
    h.select(0, Some("private"), true);
    h.enter(0, 0);
    h.fence();
    let result = h.backend().request_text(crate::ClipboardTarget::Clipboard);
    h.wait(
        || h.query(|state| state.held.len() >= 3),
        "explicit plain receive",
    );
    h.leave(0);
    h.fence();
    assert_eq!(result.recv_timeout(h.remaining()).unwrap(), None);
    let pipes = h.query(|state| std::mem::take(&mut state.held));
    assert_eq!(pipes.len(), 3);
    for (_, fd) in pipes {
        assert_eq!(
            std::fs::File::from(fd)
                .write_all(b"late")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }
}

#[test]
fn a_secret_copy_pastes_as_plain_text_and_carries_the_history_markers() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();

    assert!(harness.backend().set_secret_text("hunter2"));
    let mimes = harness.selection_mimes();
    for mime in [
        PLAIN,
        "text/plain",
        crate::SECRET_MIME,
        crate::PASSWORD_MANAGER_HINT_MIME,
    ] {
        assert!(
            mimes.iter().any(|offered| offered == mime),
            "a secret copy must offer {mime}: {mimes:?}"
        );
    }
    assert_eq!(harness.selection_bytes("text/plain"), b"hunter2");
    assert_eq!(
        harness.selection_bytes(crate::PASSWORD_MANAGER_HINT_MIME),
        crate::SECRET_MARKER
    );

    // The next ordinary copy is not a secret, and an image never rides along
    // under (or keeps offering) an earlier secret.
    assert!(
        harness
            .backend()
            .set_text(crate::ClipboardTarget::Clipboard, "next")
    );
    let mimes = harness.selection_mimes();
    assert!(
        !mimes
            .iter()
            .any(|mime| mime == crate::SECRET_MIME || mime == crate::PASSWORD_MANAGER_HINT_MIME),
        "an ordinary copy must not inherit the secret markers: {mimes:?}"
    );
    assert!(harness.backend().set_secret_text("hunter3"));
    assert!(harness.backend().set_image(1, 1, &[0xff, 0, 0x80, 0xff]));
    assert_eq!(harness.selection_mimes(), ["image/png"]);
}

/// An app's image copy (a photo, a region of a PDF page) replaces what the
/// app copied before. Text left beside the image would be what every app
/// that prefers text pastes, so the person would get the old text instead.
#[test]
fn an_image_copy_replaces_the_text_copied_before_it() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();

    assert!(
        harness
            .backend()
            .set_text(crate::ClipboardTarget::Clipboard, "copied before")
    );
    assert!(harness.backend().set_image(1, 1, &[0xff, 0, 0x80, 0xff]));
    assert_eq!(
        harness.selection_mimes(),
        ["image/png"],
        "the image copy offers the image and nothing copied before it"
    );
    let png = harness.selection_bytes("image/png");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    // And a text copy after it replaces the image in turn.
    assert!(
        harness
            .backend()
            .set_text(crate::ClipboardTarget::Clipboard, "copied after")
    );
    assert!(
        !harness
            .selection_mimes()
            .iter()
            .any(|mime| mime == "image/png"),
        "the text copy replaced the image"
    );
    assert_eq!(harness.selection_bytes("text/plain"), b"copied after");
}

/// Tilo's compositor answers a new selection with work of its own (it mirrors
/// the selection, takes it over, repaints) in the iteration that confirms it.
/// A copy whose confirmation comes back after the copy's budget was still
/// published, and must be reported as copied: an app that said "could not
/// copy" would leave a secret on the clipboard with no clear scheduled.
#[test]
fn a_copy_the_compositor_took_is_reported_as_copied_however_late_it_confirms() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let gate = SelectionGate {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    harness.operation(move |state, _| state.selection_gate = Some(gate));
    harness.query(|state| assert!(state.selection_gate.is_some()));

    std::thread::scope(|scope| {
        let copy = scope.spawn(|| harness.backend().set_image(1, 1, &[0xff, 0, 0x80, 0xff]));
        harness.wait(
            || entered.load(Ordering::Acquire),
            "server receiving the image selection",
        );
        let taken = Instant::now();
        harness.wait(
            || taken.elapsed() > super::PUBLICATION_BUDGET * 3,
            "the copy's budgets to pass",
        );
        release.store(true, Ordering::Release);
        assert!(
            copy.join().expect("copy thread"),
            "a selection the compositor took was reported as a failed copy"
        );
    });
    harness.wait(
        || harness.query(|state| state.publications == [state.serial]),
        "server-side image selection",
    );
}

/// The other order: the caller gives up before the worker reaches the copy.
/// The copy is reported as failed and must then never become the selection.
#[test]
fn a_copy_reported_as_failed_never_becomes_the_selection() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    // The focus check's answer is held until the copy gave up on it.
    harness.query(|state| state.hold_flush = true);
    let started = Instant::now();
    assert!(
        !harness
            .backend()
            .set_text(crate::ClipboardTarget::Clipboard, "too late"),
        "the copy cannot be confirmed while the compositor does not answer"
    );
    assert!(started.elapsed() >= super::PUBLICATION_BUDGET);
    harness.query(|state| state.hold_flush = false);
    harness.fence();
    assert!(
        harness.query(|state| state.publications.is_empty()),
        "a copy reported as failed became the selection"
    );
}

/// The app's UI thread never waits on the compositor: the copy returns at
/// once, and its outcome settles when the compositor has confirmed it.
#[test]
fn a_copy_settles_once_the_compositor_answers_and_not_before() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    // Nothing the compositor says reaches the app until released. The copy's
    // deadline outlasts the test, so only the compositor's answer settles it.
    harness.query(|state| state.hold_flush = true);
    let (copy, settler) = crate::ClipboardCopy::pending();
    let deadline = tilo_clock::now()
        .checked_add(harness.remaining())
        .expect("bounded publication");
    assert!(
        harness
            .backend()
            .send_publication(ClipboardCommand::Publish(Publication::new(
                PublicationPayload::SecretText(b"hunter2".to_vec()),
                Settle::new(move |copied| settler.settle(copied)),
                deadline,
            )))
    );
    assert_eq!(
        copy.outcome(),
        None,
        "the copy cannot be settled before the compositor answers"
    );
    harness.query(|state| state.hold_flush = false);
    harness.wait(|| copy.outcome().is_some(), "the copy's outcome");
    assert_eq!(copy.outcome(), Some(true));
    assert_eq!(harness.query(|state| state.publications.len()), 1);
}

/// A copy the deadline refused settles as failed without anyone waiting on
/// it, and never becomes the selection later.
#[test]
fn an_unanswered_copy_settles_as_failed_at_its_deadline() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    harness.query(|state| state.hold_flush = true);
    let copy = harness.backend().copy(crate::CopyContent::Text("too late"));
    harness.wait(|| copy.outcome().is_some(), "the copy's deadline");
    assert_eq!(copy.outcome(), Some(false));
    harness.query(|state| state.hold_flush = false);
    harness.fence();
    assert!(
        harness.query(|state| state.publications.is_empty()),
        "a copy settled as failed became the selection"
    );
}

#[test]
fn a_rich_copy_offers_its_html_and_its_words() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();

    let html = "<p>plain <b>bold</b></p>";
    assert!(harness.backend().set_rich_text(html, "plain bold"));
    let mimes = harness.selection_mimes();
    for mime in ["text/html", PLAIN, "text/plain"] {
        assert!(
            mimes.iter().any(|offered| offered == mime),
            "a rich copy must offer {mime}: {mimes:?}"
        );
    }
    assert_eq!(harness.selection_bytes("text/html"), html.as_bytes());
    assert_eq!(harness.selection_bytes("text/plain"), b"plain bold");

    // The next plain copy offers no stale formatting.
    assert!(
        harness
            .backend()
            .set_text(crate::ClipboardTarget::Clipboard, "next")
    );
    let mimes = harness.selection_mimes();
    assert!(
        !mimes.iter().any(|mime| mime.starts_with("text/html")),
        "a plain copy must not keep the formatting of the one before: {mimes:?}"
    );
}
