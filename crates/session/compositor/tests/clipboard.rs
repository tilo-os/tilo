//! The clipboard on a headless compositor with real Wayland clients: what a
//! client copies is read as its bytes arrive, secret offers are kept out of
//! the history, and only clients the session trusts see every selection
//! through data control.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_compositor::clipboard_history::{
    History, PASSWORD_MANAGER_HINT_MIME, Pulled, SECRET_MIME, TEXT_MIME, kept_mime,
};
use tilo_compositor::{Compositor, OutputConfig, Selection, StackPolicy};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_data_device, wl_data_device::WlDataDevice,
    wl_data_device_manager::WlDataDeviceManager, wl_data_offer, wl_data_offer::WlDataOffer,
    wl_data_source, wl_data_source::WlDataSource, wl_registry, wl_registry::WlRegistry,
    wl_seat::WlSeat, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

#[derive(Default)]
struct Seen {
    globals: Vec<String>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    data_manager: Option<WlDataDeviceManager>,
    configured: bool,
    /// What the data source serves when asked.
    payload: Vec<u8>,
    /// The formats the compositor asked our source for.
    asked: Vec<String>,
}

struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    data_device: Option<WlDataDevice>,
}

impl TestClient {
    fn connect(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client().expect("client socket")
        } else {
            compositor.connect_client().expect("client socket")
        };
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            surface: None,
            data_device: None,
        }
    }

    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    /// Map a 200x100 window, which the policy puts in front and focuses.
    fn open_window(&mut self) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("editor".into());
        surface.commit();
        self.surface = Some(surface);
        self.data_device = Some(self.seen.data_manager.as_ref().unwrap().get_data_device(
            self.seen.seat.as_ref().unwrap(),
            &qh,
            (),
        ));
    }

    fn show(&self) {
        let qh = self.queue.handle();
        // A picture the size the compositor draws, in pixels.
        let size = |pixels: usize| i32::try_from(pixels).expect("a screen-sized length");
        let (width, height) = (200_usize, 100_usize);
        let (wide, high) = (size(width), size(height));
        let fd = rustix::fs::memfd_create("tilo-clipboard-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        file.write_all(&[0, 255, 0, 255].repeat(width * height))
            .unwrap();
        let pool = self.seen.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            size(width * height * 4),
            &qh,
            (),
        );
        let buffer = pool.create_buffer(
            0,
            wide,
            high,
            size(width * 4),
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        let surface = self.surface.as_ref().unwrap();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, wide, high);
        surface.commit();
        pool.destroy();
    }

    /// Offer `formats` as the clipboard, serving `payload` for each.
    fn copy(&mut self, formats: &[&str], payload: &[u8]) {
        let qh = self.queue.handle();
        let source = self
            .seen
            .data_manager
            .as_ref()
            .unwrap()
            .create_data_source(&qh, ());
        for mime in formats {
            source.offer((*mime).into());
        }
        self.seen.payload = payload.to_vec();
        // Data control's serials and this one are not tracked: what the
        // protocol asks of a copy is that its client has the keyboard.
        self.data_device
            .as_ref()
            .unwrap()
            .set_selection(Some(&source), 0);
    }
}

fn pump(
    compositor: &mut Compositor,
    client: &mut TestClient,
    mut done: impl FnMut(&Compositor, &TestClient) -> bool,
) {
    for _ in 0..500 {
        client.exchange();
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        client.exchange();
        if done(compositor, client) {
            return;
        }
    }
    panic!("condition not reached");
}

/// A compositor with one output and a client whose window has the keyboard.
fn setup() -> (Compositor, TestClient) {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    let mut client = TestClient::connect(&mut compositor, false);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.wm_base.is_some()
            && client.seen.seat.is_some()
            && client.seen.data_manager.is_some()
    });
    client.open_window();
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.configured
    });
    client.show();
    pump(&mut compositor, &mut client, |compositor, _| {
        compositor.windows().len() == 1
    });
    (compositor, client)
}

#[test]
fn only_a_trusted_client_is_offered_the_selection_data_control() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    let mut untrusted = TestClient::connect(&mut compositor, false);
    pump(&mut compositor, &mut untrusted, |_, client| {
        !client.seen.globals.is_empty()
    });
    let globals = untrusted.seen.globals.clone();
    assert!(
        globals
            .iter()
            .any(|global| global == "wl_data_device_manager"),
        "every client copies and pastes: {globals:?}"
    );
    assert!(
        !globals
            .iter()
            .any(|global| global == "zwlr_data_control_manager_v1"
                || global == "ext_data_control_manager_v1"),
        "an untrusted app must not see every selection: {globals:?}"
    );

    let mut trusted = TestClient::connect(&mut compositor, true);
    pump(&mut compositor, &mut trusted, |_, client| {
        !client.seen.globals.is_empty()
    });
    let globals = &trusted.seen.globals;
    assert!(
        globals
            .iter()
            .any(|global| global == "zwlr_data_control_manager_v1"),
        "a clipboard manager the session trusts sees it: {globals:?}"
    );
    assert!(
        globals
            .iter()
            .any(|global| global == "ext_data_control_manager_v1"),
        "and its successor: {globals:?}"
    );
}

#[test]
fn a_clients_copy_is_read_as_its_bytes_arrive_and_secrets_are_kept_out() {
    let (mut compositor, mut client) = setup();
    let before = compositor.selection_changes(Selection::Clipboard);

    client.copy(&[TEXT_MIME], b"copied in the editor");
    pump(&mut compositor, &mut client, |compositor, _| {
        compositor.selection_changes(Selection::Clipboard) != before
    });
    let mimes = compositor.selection_formats(Selection::Clipboard);
    assert_eq!(mimes, [TEXT_MIME.to_owned()]);
    let mime = kept_mime(&mimes).expect("text is kept");

    // The read is the loop's: nothing arrives until the app is asked, and
    // asking never blocks the loop.
    let mut pull = compositor
        .pull_selection(Selection::Clipboard, mime)
        .expect("the read starts");
    assert_eq!(
        pull.drain(),
        Pulled::Pending,
        "the app has not answered yet"
    );

    let mut bytes = None;
    for _ in 0..500 {
        match pull.drain() {
            Pulled::Ready(read) => {
                bytes = Some(read);
                break;
            }
            Pulled::Pending => {
                client.exchange();
                compositor
                    .dispatch(Duration::from_millis(2))
                    .expect("dispatch");
            }
            Pulled::Lost => panic!("the copy was lost"),
        }
    }
    let bytes = bytes.expect("the copy arrived");
    assert_eq!(bytes, b"copied in the editor");
    assert_eq!(client.seen.asked, [TEXT_MIME.to_owned()]);

    let mut history = History::default();
    assert!(history.record(&mimes, mime, bytes, 1));
    assert_eq!(
        history.copies().next().map(|copy| copy.text_preview()),
        Some(Some("copied in the editor"))
    );

    // The same app copies a password: the marker travels with the copy, and
    // the history keeps none of it.
    let marked = compositor.selection_changes(Selection::Clipboard);
    client.copy(
        &[TEXT_MIME, SECRET_MIME, PASSWORD_MANAGER_HINT_MIME],
        b"hunter2",
    );
    pump(&mut compositor, &mut client, |compositor, _| {
        compositor.selection_changes(Selection::Clipboard) != marked
    });
    let mimes = compositor.selection_formats(Selection::Clipboard);
    assert!(mimes.iter().any(|mime| mime == SECRET_MIME));
    assert_eq!(kept_mime(&mimes), None);
    assert_eq!(
        history.len(),
        1,
        "only the copy the person may keep is kept"
    );
}

impl Dispatch<wl_registry::WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            seen.globals.push(interface.clone());
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "wl_data_device_manager" => {
                    seen.data_manager = Some(registry.bind(name, 3, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.configured = true;
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    // The compositor hands an offer for what it offers, which this client
    // never takes.
    event_created_child!(Seen, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        _: wl_data_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataSource, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_source::Event::Send { mime_type, fd } = event {
            std::fs::File::from(fd).write_all(&seen.payload).unwrap();
            seen.asked.push(mime_type);
        }
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore XdgToplevel);
delegate_noop!(Seen: ignore WlDataDeviceManager);
delegate_noop!(Seen: ignore WlSeat);
