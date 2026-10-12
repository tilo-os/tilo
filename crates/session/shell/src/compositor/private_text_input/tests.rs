//! Real client/server exchanges through the production global and dispatch.
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::reexports::wayland_server::Resource;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use winit::platform::wayland::private_text_input_protocol::{
    tilo_text_input_manager_v1, tilo_text_input_v1,
};

use super::{Failure, Identity, Kind, State};
use crate::compositor::state::ClientState;

#[derive(Default)]
struct Peer {
    globals: Vec<(u32, String, u32)>,
    operations: Vec<(u32, u32, Vec<u8>)>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        peer: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            peer.globals.push((name, interface, version));
        }
    }
}
impl Dispatch<tilo_text_input_v1::TiloTextInputV1, ()> for Peer {
    fn event(
        peer: &mut Self,
        _: &tilo_text_input_v1::TiloTextInputV1,
        event: tilo_text_input_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let tilo_text_input_v1::Event::Operation {
            serial,
            kind,
            snapshot,
        } = event
        else {
            panic!("unexpected private input operation")
        };
        peer.operations.push((serial, u32::from(kind), snapshot));
    }
}
delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
delegate_noop!(Peer: ignore wl_surface::WlSurface);
delegate_noop!(Peer: ignore tilo_text_input_manager_v1::TiloTextInputManagerV1);

struct Harness {
    display: Display<State>,
    state: State,
    conn: Connection,
    queue: EventQueue<Peer>,
    peer: Peer,
    field: tilo_text_input_v1::TiloTextInputV1,
    surface: wl_surface::WlSurface,
    _event_loop: EventLoop<'static, State>,
}
impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let mut display = Display::new().expect("Wayland display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().expect("isolated socket pair");
        display
            .handle()
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("ordinary client");
        client.set_nonblocking(true).expect("nonblocking client");
        let conn = Connection::from_socket(client).expect("client connection");
        let mut queue = conn.new_event_queue::<Peer>();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut peer = Peer::default();
        Self::exchange(&mut display, &mut state, &conn, &mut queue, &mut peer);
        let bind = |interface: &str| {
            peer.globals
                .iter()
                .find(|(_, name, _)| name == interface)
                .map(|(name, _, _)| *name)
                .expect("production global")
        };
        let compositor: wl_compositor::WlCompositor =
            registry.bind(bind("wl_compositor"), 4, &queue.handle(), ());
        let manager: tilo_text_input_manager_v1::TiloTextInputManagerV1 =
            registry.bind(bind("tilo_text_input_manager_v1"), 1, &queue.handle(), ());
        let surface = compositor.create_surface(&queue.handle(), ());
        let field = manager.get_field(&surface, &queue.handle(), ());
        field.update(vec![3; 32]);
        Self::exchange(&mut display, &mut state, &conn, &mut queue, &mut peer);
        Self {
            display,
            state,
            conn,
            queue,
            peer,
            field,
            surface,
            _event_loop: event_loop,
        }
    }
    fn exchange(
        display: &mut Display<State>,
        state: &mut State,
        conn: &Connection,
        queue: &mut EventQueue<Peer>,
        peer: &mut Peer,
    ) {
        for _ in 0..8 {
            queue.flush().expect("flush client requests");
            display
                .dispatch_clients(state)
                .expect("dispatch production requests");
            display.flush_clients().expect("flush production events");
            if let Some(reader) = conn.prepare_read()
                && let Err(error) = reader.read()
            {
                assert!(
                    matches!(error, wayland_client::backend::WaylandError::Io(ref io) if io.kind() == std::io::ErrorKind::WouldBlock),
                    "read failed: {error}"
                );
            }
            queue
                .dispatch_pending(peer)
                .expect("dispatch client events");
        }
    }
    fn pump(&mut self) {
        Self::exchange(
            &mut self.display,
            &mut self.state,
            &self.conn,
            &mut self.queue,
            &mut self.peer,
        );
    }
    fn identity(&self) -> Identity {
        let surface = self
            .state
            .private_text_input
            .fields
            .values()
            .next()
            .expect("registered surface")
            .surface
            .id();
        self.state
            .private_text_input
            .identity(&surface)
            .expect("published original field")
    }
    fn request(&mut self, kind: Kind) -> (u32, super::Receipt) {
        let identity = self.identity();
        let receipt = self
            .state
            .private_text_input
            .request(&identity, kind)
            .expect("original request admitted");
        self.pump();
        let (serial, observed, stamp) = self.peer.operations.last().expect("wire operation");
        assert_eq!(*observed, kind as u32);
        assert_eq!(stamp, &vec![3; 32]);
        (*serial, receipt)
    }
    fn acknowledge(&mut self, serial: u32, outcome: tilo_text_input_v1::Outcome) {
        self.field.acknowledge(serial, outcome);
        self.pump();
    }
}

#[test]
fn starts_and_finals_require_original_private_acknowledgments() {
    let mut h = Harness::new();
    let identity = h.identity();
    assert!(
        h.state
            .private_text_input
            .request(&identity, Kind::Start)
            .is_none()
    );
    let (warm, warming) = h.request(Kind::Prepare);
    assert!(warming.poll().is_none());
    h.acknowledge(warm, tilo_text_input_v1::Outcome::Accepted);
    assert!(matches!(warming.poll(), Some(Ok(None))));
    let (start, starting) = h.request(Kind::Start);
    assert!(starting.poll().is_none());
    assert!(
        h.state
            .private_text_input
            .request(&identity, Kind::Final)
            .is_none()
    );
    h.acknowledge(start, tilo_text_input_v1::Outcome::Accepted);
    let (final_serial, final_receipt) = h.request(Kind::Final);
    assert!(final_receipt.poll().is_none());
    h.acknowledge(final_serial, tilo_text_input_v1::Outcome::Unknown);
    assert!(matches!(final_receipt.poll(), Some(Err(Failure::Unknown))));
    assert!(starting.was_canceled());
    assert!(warming.was_canceled());
}

#[test]
fn canceled_pending_serial_and_late_reply_cannot_admit_a_successor() {
    let mut h = Harness::new();
    let (old, receipt) = h.request(Kind::Prepare);
    receipt.cancel();
    h.state.private_text_input.service();
    h.pump();
    assert!(
        h.peer
            .operations
            .iter()
            .any(|(serial, kind, _)| *serial == old && *kind == Kind::Cancel as u32)
    );
    let (new, successor) = h.request(Kind::Prepare);
    assert_ne!(old, new);
    h.acknowledge(old, tilo_text_input_v1::Outcome::Accepted);
    assert!(successor.poll().is_none());
    let identity = h.identity();
    assert!(
        h.state
            .private_text_input
            .request(&identity, Kind::Start)
            .is_none()
    );
    h.acknowledge(new, tilo_text_input_v1::Outcome::Accepted);
    assert!(matches!(successor.poll(), Some(Ok(None))));
}

#[test]
fn changed_snapshot_retires_start_and_reports_unknown_after_final_admission() {
    let mut h = Harness::new();
    let original = h.identity();
    let (warm, _) = h.request(Kind::Prepare);
    h.acknowledge(warm, tilo_text_input_v1::Outcome::Accepted);
    let (start, starting) = h.request(Kind::Start);
    h.acknowledge(start, tilo_text_input_v1::Outcome::Accepted);
    let (_, final_receipt) = h.request(Kind::Final);
    h.field.update(vec![4; 32]);
    h.pump();
    assert!(starting.was_canceled());
    assert!(matches!(final_receipt.poll(), Some(Err(Failure::Unknown))));
    assert!(
        h.state
            .private_text_input
            .request(&original, Kind::Prepare)
            .is_none()
    );
    let identity = h.identity();
    assert!(
        h.state
            .private_text_input
            .request(&identity, Kind::Start)
            .is_none()
    );
}

#[test]
fn canceled_final_sends_its_own_serial_before_retiring_the_start_grant() {
    let mut h = Harness::new();
    let (prepare, warming) = h.request(Kind::Prepare);
    h.acknowledge(prepare, tilo_text_input_v1::Outcome::Accepted);
    let (start, starting) = h.request(Kind::Start);
    h.acknowledge(start, tilo_text_input_v1::Outcome::Accepted);
    let (final_serial, final_receipt) = h.request(Kind::Final);
    h.peer.operations.clear();
    final_receipt.cancel();
    h.state.private_text_input.service();
    h.pump();
    assert_eq!(
        h.peer.operations,
        vec![(final_serial, Kind::Cancel as u32, vec![3; 32])]
    );
    assert!(!starting.was_canceled());
    assert!(!warming.was_canceled());
    h.acknowledge(final_serial, tilo_text_input_v1::Outcome::Accepted);
    assert!(!starting.was_canceled(), "late final ACK cannot settle it");
}

#[test]
fn destroyed_field_settles_pending_receipt_without_retaining_authority() {
    let mut h = Harness::new();
    let (_, pending) = h.request(Kind::Prepare);
    h.field.destroy();
    h.pump();
    assert!(matches!(pending.poll(), Some(Err(Failure::Unknown))));
    assert!(h.state.private_text_input.fields.is_empty());
}

#[test]
fn destroyed_surface_retires_a_still_live_private_field() {
    let mut h = Harness::new();
    let original = h.identity();
    let (_, pending) = h.request(Kind::Prepare);
    h.surface.destroy();
    h.pump();
    h.state.private_text_input.service();
    h.pump();
    assert!(matches!(pending.poll(), Some(Err(Failure::Unknown))));
    assert!(
        h.state
            .private_text_input
            .request(&original, Kind::Prepare)
            .is_none()
    );
}
