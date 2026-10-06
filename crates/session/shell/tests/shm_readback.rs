//! Real Wayland SHM pools through both guarded readback and worker capture.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use shell::compositor::foreign::store;
use shell::compositor::state::{ClientState, State};
use shell::compositor::surface_readback::{
    SurfaceTracker, buffer_to_rgba, copy_shm_frame, owned_shm_to_rgba,
};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::{Client, Display, protocol::wl_buffer};
use wayland_client::protocol::{
    wl_buffer as client_buffer, wl_callback, wl_registry, wl_shm, wl_shm_pool,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};

#[derive(Default)]
struct Peer {
    shm: Option<wl_shm::WlShm>,
    synced: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        peer: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "wl_shm"
        {
            peer.shm = Some(registry.bind(name, 1, qh, ()));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Peer {
    fn event(
        peer: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        peer.synced = true;
    }
}

delegate_noop!(Peer: ignore wl_shm::WlShm);
delegate_noop!(Peer: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Peer: ignore client_buffer::WlBuffer);

struct Harness {
    display: Display<State>,
    state: State,
    client: Client,
    conn: Connection,
    queue: wayland_client::EventQueue<Peer>,
    peer: Peer,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("Wayland display");
        let state = State::new(event_loop.handle(), display.handle());
        let (server, peer) = UnixStream::pair().expect("socketpair");
        let client = display
            .handle()
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("server client");
        peer.set_nonblocking(true).expect("nonblocking client");
        let conn = Connection::from_socket(peer).expect("client connection");
        let queue = conn.new_event_queue();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        let mut harness = Self {
            display,
            state,
            client,
            conn,
            queue,
            peer: Peer::default(),
            _event_loop: event_loop,
        };
        harness.roundtrip();
        harness
    }

    fn roundtrip(&mut self) {
        self.peer.synced = false;
        let _sync = self.conn.display().sync(&self.queue.handle(), ());
        let deadline = tilo_test_env::hang_deadline();
        while !self.peer.synced {
            assert!(
                Instant::now() < deadline,
                "Wayland roundtrip never completed"
            );
            self.conn.flush().expect("flush client");
            self.display
                .dispatch_clients(&mut self.state)
                .expect("dispatch server");
            self.display.flush_clients().expect("flush server");
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read(); // Nonblocking reads may have no events yet.
            }
            self.queue
                .dispatch_pending(&mut self.peer)
                .expect("dispatch client");
            if !self.peer.synced {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

#[test]
fn padded_wayland_shm_buffers_reach_streaming_and_worker_capture_without_pixel_changes() {
    let mut h = Harness::new();
    let shm = h.peer.shm.clone().expect("production wl_shm global");
    let qh = h.queue.handle();
    for (format, opaque, id) in [
        (wl_shm::Format::Argb8888, false, 0x53_48_4d_01),
        (wl_shm::Format::Xrgb8888, true, 0x53_48_4d_02),
    ] {
        // Odd width, nonzero pool offset, row padding, and varying alpha.
        let mut bytes = vec![0xde; 247];
        let mut expected = Vec::new();
        for y in 0_usize..3 {
            for x in 0_usize..17 {
                let byte = u8::try_from(y.saturating_mul(17).saturating_add(x)).unwrap();
                let pixel = [
                    byte,
                    byte.wrapping_mul(3),
                    byte.wrapping_mul(7),
                    byte.wrapping_mul(11),
                ];
                let start = 7_usize
                    .saturating_add(y.saturating_mul(80))
                    .saturating_add(x.saturating_mul(4));
                bytes[start..start.saturating_add(4)].copy_from_slice(&pixel);
                expected.extend_from_slice(&[
                    pixel[2],
                    pixel[1],
                    pixel[0],
                    if opaque { 255 } else { pixel[3] },
                ]);
            }
        }
        let mut file = tempfile::tempfile().expect("SHM pool backing file");
        file.write_all(&bytes).expect("write SHM fixture");
        let pool = shm.create_pool(file.as_fd(), 247, &qh, ());
        let buffer = pool.create_buffer(7, 17, 3, 80, format, &qh, ());
        h.roundtrip();
        let server_buffer = h
            .client
            .object_from_protocol_id::<wl_buffer::WlBuffer>(
                &h.display.handle(),
                buffer.id().protocol_id(),
            )
            .expect("actual server-side SHM buffer");

        let mut rgba = vec![0xff];
        assert_eq!(buffer_to_rgba(&server_buffer, &mut rgba).unwrap(), (17, 3));
        assert_eq!(rgba, expected);
        let copied = copy_shm_frame(&server_buffer).expect("guarded pool copy");
        assert_eq!(owned_shm_to_rgba(&copied, &mut rgba).unwrap(), (17, 3));
        assert_eq!(rgba, expected);
        let mut tracker = SurfaceTracker::new();
        assert_eq!(tracker.on_commit(id, &server_buffer).unwrap(), (17, 3));
        assert_eq!(
            tracker.take_latest(id).expect("streaming frame").rgba,
            expected
        );

        store().forget(id);
        assert!(
            store().prepare_shm_capture(id, copied).is_some(),
            "capture worker starts"
        );
        let deadline = tilo_test_env::hang_deadline();
        while store().frame_seq(id) == 0 {
            assert!(
                Instant::now() < deadline,
                "worker never published the copied SHM frame"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let frame = store().snapshot(id).expect("published worker pixels");
        assert_eq!((frame.width, frame.height), (17, 3));
        assert_eq!(frame.rgba, expected);
        store().forget(id);
        buffer.destroy();
        pool.destroy();
        h.roundtrip();
    }
}
