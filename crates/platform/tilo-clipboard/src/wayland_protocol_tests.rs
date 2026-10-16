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
    wl_keyboard, wl_seat, wl_surface,
};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
};

use super::{ClipboardCommand, WaylandClipboard};
use crate::ClipboardBackend as _;

const PLAIN: &str = "text/plain;charset=utf-8";
type Held = Vec<(String, OwnedFd)>;
type Operation = Box<dyn FnOnce(&mut Server, &DisplayHandle) + Send>;

#[derive(Default)]
struct Seat {
    keyboard: Option<wl_keyboard::WlKeyboard>,
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
    publications: Vec<u32>,
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
        seat.capabilities(wl_seat::Capability::Keyboard);
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
            state.seat(*index).keyboard = Some(keyboard);
        }
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
                state.seat(index).device = Some(init.init(id, ()));
            }
            wl_data_device_manager::Request::CreateDataSource { id } => {
                init.init(id, ());
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
            source: Some(_),
            serial,
        } = request
        {
            assert!(state.seats.iter().any(|seat| seat.surface.is_some()));
            if let Some(gate) = &state.selection_gate {
                gate.entered.store(true, Ordering::Release);
                while !gate.release.load(Ordering::Acquire) {
                    std::thread::park_timeout(Duration::from_millis(1));
                }
            }
            state.publications.push(serial);
        }
    }
}
impl Dispatch<wl_data_source::WlDataSource, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_data_source::WlDataSource,
        _: wl_data_source::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
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
    _connection: client::Connection,
    _host: Host,
    _queue: client::EventQueue<Host>,
}

#[derive(Clone, Copy)]
enum ReadDeadline {
    Production,
    AfterTest,
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
            ReadDeadline::Production => WaylandClipboard::new_for_wayland_test(connection.clone()),
            ReadDeadline::AfterTest => WaylandClipboard::new_for_wayland_test_with_read_budget(
                connection.clone(),
                read_budget,
            ),
        };
        let harness = Self {
            clipboard: Some(clipboard),
            operations: Some(send),
            thread: Some(thread),
            deadline,
            _connection: connection,
            _host: host,
            _queue: queue,
        };
        harness.wait(
            || {
                harness.query(|state| {
                    state
                        .seats
                        .iter()
                        .all(|seat| seat.keyboard.is_some() && seat.device.is_some())
                        && state.surfaces.len() == 2
                })
            },
            "worker bindings",
        );
        harness.fence();
        harness
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
    let deadline = tilo_clock::now()
        .checked_add(tilo_clock::Duration::from_secs(5))
        .expect("bounded publication");
    assert!(
        harness
            .backend()
            .send_publication(ClipboardCommand::SetImage(png, reply, deadline,))
    );
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
    assert!(
        harness
            .backend()
            .send_publication(ClipboardCommand::SetText(
                b"caption".to_vec(),
                result_tx,
                super::publication_deadline()
            ))
    );
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
    assert!(
        harness
            .backend()
            .send_publication(ClipboardCommand::SetText(
                b"unfocused".to_vec(),
                reply,
                super::publication_deadline(),
            ))
    );
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
fn expired_copy_does_not_publish_on_current_or_later_focus() {
    let harness = Harness::new();
    harness.enter(0, 0);
    harness.fence();
    let (reply, result) = mpsc::channel();
    assert!(
        harness
            .backend()
            .send_publication(ClipboardCommand::SetText(
                b"expired".to_vec(),
                reply,
                tilo_clock::now(),
            ))
    );
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
    assert!(matches!(
        result.recv_timeout(Duration::from_millis(60)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
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
