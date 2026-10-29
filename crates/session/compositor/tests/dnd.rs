//! Drag-and-drop between real Wayland clients and hosted surface apps on a
//! headless compositor: files travel as `text/uri-list`, byte for byte,
//! dragged with the pointer or a finger; a drag's icon is drawn under it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;
use tilo_compositor::{Compositor, HostLayer, InputEvent, OutputConfig, StackPolicy, SurfaceHost};
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_data_device, wl_data_device::WlDataDevice,
    wl_data_device_manager::WlDataDeviceManager, wl_data_offer, wl_data_offer::WlDataOffer,
    wl_data_source, wl_data_source::WlDataSource, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_touch, wl_touch::WlTouch,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const BTN_LEFT: u32 = 0x110;
const URI_LIST: &str = "text/uri-list";
/// Two files, one with an escaped space, as file managers drag them.
const FILES: &[u8] = b"file:///home/someone/a%20b.txt\r\nfile:///home/someone/c.png\r\n";

#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    touch: Option<WlTouch>,
    /// The serial of the last touch down on the window.
    touched: Option<u32>,
    data_manager: Option<WlDataDeviceManager>,
    configured: bool,
    /// The serial of the last button press on the window.
    pressed: Option<u32>,
    /// The formats of the latest offer.
    offered: Vec<String>,
    /// A drag entered the window with this offer.
    entered: Option<WlDataOffer>,
    dropped: bool,
    /// What our data source was asked for.
    sent: Vec<String>,
    /// What our data source serves.
    payload: Vec<u8>,
    /// How our data source's drag ended: `action move`, `performed`,
    /// `finished`, `cancelled`.
    source_events: Vec<String>,
}

struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    data_device: Option<WlDataDevice>,
}

impl TestClient {
    fn connect(compositor: &mut Compositor) -> Self {
        let socket = compositor.connect_client().expect("client socket");
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

    /// Map a 200x100 window (centred: 220,190) and bind a data device.
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
        toplevel.set_app_id("files".into());
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
        let (width, height) = (200, 100);
        let fd =
            rustix::fs::memfd_create("tilo-dnd-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let mut file = std::fs::File::from(fd);
        file.write_all(&[0, 0, 255, 255].repeat(width * height))
            .unwrap();
        let (w, h) = (
            i32::try_from(width).unwrap(),
            i32::try_from(height).unwrap(),
        );
        let pool = self
            .seen
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), w * h * 4, &qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
        let surface = self.surface.as_ref().unwrap();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        pool.destroy();
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

/// Let compositor and client exchange a few rounds.
fn settle(compositor: &mut Compositor, client: &mut TestClient) {
    let mut rounds = 0;
    pump(compositor, client, |_, _| {
        rounds += 1;
        rounds >= 5
    });
}

fn setup(log: &Rc<RefCell<DropLog>>) -> (Compositor, TestClient) {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.add_host(Box::new(DropZone::new(Rc::clone(log))));
    let mut client = TestClient::connect(&mut compositor);
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
    assert_eq!(compositor.windows()[0].1, (220, 190, 200, 100));
    (compositor, client)
}

fn input(compositor: &mut Compositor, event: InputEvent) {
    compositor.input(event).expect("input");
}

#[derive(Default)]
struct DropLog {
    offered: Vec<Vec<String>>,
    left: u32,
    dropped: Vec<(String, Vec<u8>)>,
    buttons: Vec<(u32, bool)>,
    drag_ended: Option<bool>,
}

/// A hosted surface at (500, 20, 100, 100) that takes file lists.
struct DropZone {
    buffer: MemoryRenderBuffer,
    log: Rc<RefCell<DropLog>>,
}

impl DropZone {
    const RECT: (f64, f64, f64, f64) = (500.0, 20.0, 100.0, 100.0);

    fn new(log: Rc<RefCell<DropLog>>) -> Self {
        let buffer = MemoryRenderBuffer::from_slice(
            &[0, 255, 0, 255].repeat(100 * 100),
            Fourcc::Argb8888,
            (100, 100),
            1,
            Transform::Normal,
            None,
        );
        Self { buffer, log }
    }
}

impl SurfaceHost for DropZone {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: (500, 20),
            size: (100, 100),
            buffer: self.buffer.clone(),
        }]
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (rx, ry, w, h) = Self::RECT;
        x >= rx && y >= ry && x < rx + w && y < ry + h
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.log.borrow_mut().buttons.push((button, pressed));
    }

    fn drop_offered(&mut self, mimes: &[String], _x: f64, _y: f64) -> Option<String> {
        self.log.borrow_mut().offered.push(mimes.to_vec());
        mimes.iter().find(|mime| *mime == URI_LIST).cloned()
    }

    fn drop_left(&mut self) {
        self.log.borrow_mut().left += 1;
    }

    fn drop_data(&mut self, mime: &str, data: Vec<u8>) {
        self.log.borrow_mut().dropped.push((mime.to_owned(), data));
    }

    fn drag_ended(&mut self, dropped: bool) {
        self.log.borrow_mut().drag_ended = Some(dropped);
    }
}

#[test]
fn files_dragged_out_of_a_window_drop_on_a_hosted_surface() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.pressed.is_some()
    });

    // The window starts a drag of two files from its press.
    let qh = client.queue.handle();
    let source = client
        .seen
        .data_manager
        .as_ref()
        .unwrap()
        .create_data_source(&qh, ());
    source.offer(URI_LIST.into());
    source.offer("text/plain;charset=utf-8".into());
    source.set_actions(DndAction::Move);
    client.seen.payload = FILES.to_vec();
    client.data_device.as_ref().unwrap().start_drag(
        Some(&source),
        client.surface.as_ref().unwrap(),
        None,
        client.seen.pressed.unwrap(),
    );
    settle(&mut compositor, &mut client);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 550.0, y: 70.0 },
    );
    settle(&mut compositor, &mut client);
    let offered = log.borrow().offered.last().cloned().expect("drop offered");
    assert!(offered.contains(&URI_LIST.to_owned()), "{offered:?}");

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    pump(&mut compositor, &mut client, |_, _| {
        !log.borrow().dropped.is_empty()
    });
    assert_eq!(client.seen.sent, vec![URI_LIST.to_owned()]);
    let dropped = log.borrow().dropped.clone();
    assert_eq!(dropped, vec![(URI_LIST.to_owned(), FILES.to_vec())]);
    pump(&mut compositor, &mut client, |_, client| {
        client
            .seen
            .source_events
            .iter()
            .any(|event| event == "finished")
    });
    let events = &client.seen.source_events;
    assert!(
        !events.iter().any(|event| event == "cancelled"),
        "the source moves its files, it is not told the drag failed: {events:?}"
    );
    let performed = events
        .iter()
        .position(|event| event == "performed")
        .unwrap();
    assert!(
        events[..performed]
            .iter()
            .any(|event| event.contains("Move")),
        "{events:?}"
    );
    assert_eq!(
        log.borrow().buttons,
        Vec::<(u32, bool)>::new(),
        "the drag is no click on the surface"
    );
}

#[test]
fn files_dragged_out_of_a_hosted_surface_drop_on_a_window() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 550.0, y: 70.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    assert!(
        compositor
            .start_drag(vec![(URI_LIST.to_owned(), FILES.to_vec())])
            .unwrap()
    );
    // The surface it started on is offered it too, and refuses nothing.
    assert!(!log.borrow().offered.is_empty());

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.entered.is_some()
    });
    assert!(
        client.seen.offered.contains(&URI_LIST.to_owned()),
        "{:?}",
        client.seen.offered
    );
    assert_eq!(log.borrow().left, 1, "the drag left the hosted surface");

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.dropped
    });
    assert_eq!(log.borrow().drag_ended, Some(true));
    assert_eq!(
        log.borrow().buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)],
        "the surface that started the drag sees its button come up"
    );

    let offer = client.seen.entered.clone().unwrap();
    let (mut reader, writer) = std::io::pipe().unwrap();
    offer.receive(URI_LIST.into(), writer.as_fd());
    drop(writer);
    settle(&mut compositor, &mut client);
    let mut received = Vec::new();
    reader.read_to_end(&mut received).unwrap();
    assert_eq!(received, FILES);
    offer.finish();
    settle(&mut compositor, &mut client);
}

/// Start a drag of `FILES` from the window, from the press or touch with
/// `serial`, showing `icon`.
fn drag_files(client: &mut TestClient, serial: u32, icon: Option<&WlSurface>) -> WlDataSource {
    let qh = client.queue.handle();
    let source = client
        .seen
        .data_manager
        .as_ref()
        .unwrap()
        .create_data_source(&qh, ());
    source.offer(URI_LIST.into());
    source.set_actions(DndAction::Copy);
    client.seen.payload = FILES.to_vec();
    client.data_device.as_ref().unwrap().start_drag(
        Some(&source),
        client.surface.as_ref().unwrap(),
        icon,
        serial,
    );
    source
}

#[test]
fn a_drag_shows_its_icon_under_the_pointer_until_it_drops() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.pressed.is_some()
    });

    // A 10x10 blue icon, its centre on the pointer.
    let qh = client.queue.handle();
    let icon = client
        .seen
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let fd = rustix::fs::memfd_create("tilo-dnd-icon", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let mut file = std::fs::File::from(fd);
    file.write_all(&[255, 0, 0, 255].repeat(100)).unwrap();
    let pool = client
        .seen
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 400, &qh, ());
    let buffer = pool.create_buffer(0, 10, 10, 40, wl_shm::Format::Argb8888, &qh, ());
    let serial = client.seen.pressed.unwrap();
    let _source = drag_files(&mut client, serial, Some(&icon));
    icon.attach(Some(&buffer), 0, 0);
    icon.offset(-5, -5);
    icon.damage_buffer(0, 0, 10, 10);
    icon.commit();
    settle(&mut compositor, &mut client);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 100.0, y: 400.0 },
    );
    settle(&mut compositor, &mut client);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let blue = |x, y| frame.pixel(x, y) == Some([0, 0, 255, 255]);
    assert!(
        blue(96, 396) && blue(100, 400) && blue(104, 404),
        "the icon"
    );
    assert!(!blue(106, 406) && !blue(94, 394), "only the icon");

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    settle(&mut compositor, &mut client);
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(100, 400),
        Some([0, 0, 255, 255]),
        "gone with the drop"
    );
}

#[test]
fn files_dragged_out_of_a_window_with_a_finger_drop_on_a_hosted_surface() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.touch.is_some()
    });
    input(
        &mut compositor,
        InputEvent::TouchDown {
            slot: 0,
            x: 300.0,
            y: 240.0,
        },
    );
    input(&mut compositor, InputEvent::TouchFrame);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.touched.is_some()
    });
    let serial = client.seen.touched.unwrap();
    let _source = drag_files(&mut client, serial, None);
    settle(&mut compositor, &mut client);

    input(
        &mut compositor,
        InputEvent::TouchMotion {
            slot: 0,
            x: 550.0,
            y: 70.0,
        },
    );
    settle(&mut compositor, &mut client);
    assert!(
        !log.borrow().offered.is_empty(),
        "the finger carries the drag"
    );
    input(&mut compositor, InputEvent::TouchUp { slot: 0 });
    pump(&mut compositor, &mut client, |_, _| {
        !log.borrow().dropped.is_empty()
    });
    assert_eq!(
        log.borrow().dropped.clone(),
        vec![(URI_LIST.to_owned(), FILES.to_vec())]
    );
}

#[test]
fn a_hosted_surface_drags_files_with_the_finger_holding_it() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);
    input(
        &mut compositor,
        InputEvent::TouchDown {
            slot: 3,
            x: 550.0,
            y: 70.0,
        },
    );
    assert!(
        compositor
            .start_drag(vec![(URI_LIST.to_owned(), FILES.to_vec())])
            .unwrap()
    );
    input(
        &mut compositor,
        InputEvent::TouchMotion {
            slot: 3,
            x: 300.0,
            y: 240.0,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.entered.is_some()
    });
    input(&mut compositor, InputEvent::TouchUp { slot: 3 });
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.dropped
    });
    assert_eq!(log.borrow().drag_ended, Some(true));
    assert_eq!(
        log.borrow().buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)],
        "the surface the finger held sees it lift"
    );
}

impl Dispatch<WlRegistry, ()> for Seen {
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
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "wl_data_device_manager" => {
                    seen.data_manager = Some(registry.bind(name, version.min(3), qh, ()));
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

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Touch) && seen.touch.is_none() {
                seen.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            seen.pressed = Some(serial);
        }
    }
}

impl Dispatch<WlTouch, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_touch::Event::Down { serial, .. } = event {
            seen.touched = Some(serial);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { .. } => seen.offered.clear(),
            wl_data_device::Event::Enter {
                serial,
                id: Some(offer),
                ..
            } => {
                offer.accept(serial, Some(URI_LIST.into()));
                offer.set_actions(DndAction::Copy, DndAction::Copy);
                seen.entered = Some(offer);
            }
            wl_data_device::Event::Drop => seen.dropped = true,
            _ => {}
        }
    }

    event_created_child!(Seen, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            seen.offered.push(mime_type);
        }
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
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                std::fs::File::from(fd).write_all(&seen.payload).unwrap();
                seen.sent.push(mime_type);
            }
            wl_data_source::Event::Action { dnd_action } => {
                seen.source_events.push(format!("action {dnd_action:?}"));
            }
            wl_data_source::Event::DndDropPerformed => seen.source_events.push("performed".into()),
            wl_data_source::Event::DndFinished => seen.source_events.push("finished".into()),
            wl_data_source::Event::Cancelled => seen.source_events.push("cancelled".into()),
            _ => {}
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
