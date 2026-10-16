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
    wl_buffer as client_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Peer {
    shm: Option<wl_shm::WlShm>,
    compositor: Option<wl_compositor::WlCompositor>,
    desktop: Option<xdg_wm_base::XdgWmBase>,
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
        {
            match interface.as_str() {
                "wl_shm" => peer.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_compositor" => peer.compositor = Some(registry.bind(name, 4, qh, ())),
                "xdg_wm_base" => peer.desktop = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
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
delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
delegate_noop!(Peer: ignore wl_surface::WlSurface);
delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);

crate::support::wayland::test_client!(Peer: ping, configure);

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
    fn wait_frame(&mut self, id: u64, previous: u64) -> u64 {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.roundtrip();
            let current = store().frame_seq(id);
            if current > previous {
                return current;
            }
            assert!(
                Instant::now() < deadline,
                "committed SHM frame never captured"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

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

fn wire_buffer(h: &mut Harness, bytes: &[u8]) -> (wl_shm_pool::WlShmPool, client_buffer::WlBuffer) {
    wire_buffer_in(h, bytes, wl_shm::Format::Argb8888)
}

/// A 64×64 SHM buffer of `format` holding `bytes`.
fn wire_buffer_in(
    h: &mut Harness,
    bytes: &[u8],
    format: wl_shm::Format,
) -> (wl_shm_pool::WlShmPool, client_buffer::WlBuffer) {
    let mut file = tempfile::tempfile().expect("SHM buffer backing file");
    file.write_all(bytes).unwrap();
    let qh = h.queue.handle();
    let pool = h
        .peer
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 16_384, &qh, ());
    let buffer = pool.create_buffer(0, 64, 64, 256, format, &qh, ());
    h.roundtrip();
    (pool, buffer)
}

/// A damaged commit whose buffer has another format than the last one is
/// copied whole: the old `Xrgb8888` pixels were forced opaque, and laying
/// the new `Argb8888` damage over them would leave the rest opaque.
#[test]
fn a_damaged_commit_in_another_format_is_copied_whole() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let surface = h.peer.compositor.as_ref().unwrap().create_surface(&qh, ());
    let role = h
        .peer
        .desktop
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = role.get_toplevel(&qh, ());
    toplevel.set_app_id("damage-format".into());
    surface.commit();
    h.roundtrip();
    let server = h.client.object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>(
        &h.display.handle(), surface.id().protocol_id()).unwrap();
    let id = shell::compositor::state::surface_id(&server);
    // Wire bytes are [B, G, R, A]; the X byte of an Xrgb8888 buffer is ignored.
    let bytes = [0, 0, 0x80, 0x80].repeat(4096);
    let (pool, opaque) = wire_buffer_in(&mut h, &bytes, wl_shm::Format::Xrgb8888);
    surface.attach(Some(&opaque), 0, 0);
    surface.damage_buffer(0, 0, 64, 64);
    surface.commit();
    let previous = h.wait_frame(id, 0);
    let first = store().snapshot(id).unwrap().rgba;
    assert!(
        first
            .chunks_exact(4)
            .all(|pixel| pixel == [0x80, 0, 0, 0xff])
    );
    let (translucent_pool, translucent) = wire_buffer_in(&mut h, &bytes, wl_shm::Format::Argb8888);
    surface.attach(Some(&translucent), 0, 0);
    surface.damage_buffer(10, 10, 4, 4);
    surface.commit();
    h.wait_frame(id, previous);
    let frame = store().snapshot(id).unwrap().rgba;
    assert!(
        frame
            .chunks_exact(4)
            .all(|pixel| pixel == [0x80, 0, 0, 0x80]),
        "every pixel takes the new format's alpha"
    );
    store().forget(id);
    toplevel.destroy();
    role.destroy();
    surface.destroy();
    translucent.destroy();
    translucent_pool.destroy();
    opaque.destroy();
    pool.destroy();
    h.roundtrip();
}

#[test]
fn committed_buffer_damage_stays_bound_to_actual_worker_pixels() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let surface = h.peer.compositor.as_ref().unwrap().create_surface(&qh, ());
    let role = h
        .peer
        .desktop
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = role.get_toplevel(&qh, ());
    toplevel.set_app_id("damage-wire".into());
    surface.commit();
    h.roundtrip();
    let server = h.client.object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>(
        &h.display.handle(), surface.id().protocol_id()).unwrap();
    let id = shell::compositor::state::surface_id(&server);
    let mut bytes = [0, 0, 255, 255].repeat(4096);
    let (pool, first) = wire_buffer(&mut h, &bytes);
    surface.attach(Some(&first), 0, 0);
    surface.damage_buffer(0, 0, 64, 64);
    surface.commit();
    let previous = h.wait_frame(id, 0);
    assert_eq!(&store().snapshot(id).unwrap().rgba[..4], &[255, 0, 0, 255]);
    for y in 10_usize..14 {
        for x in 10_usize..14 {
            let offset = y.saturating_mul(64).saturating_add(x).saturating_mul(4);
            bytes[offset..offset.saturating_add(4)].copy_from_slice(&[0, 255, 0, 255]);
        }
    }
    let (changed_pool, changed) = wire_buffer(&mut h, &bytes);
    surface.attach(Some(&changed), 0, 0);
    surface.damage_buffer(10, 10, 4, 4);
    surface.commit();
    let current = h.wait_frame(id, previous);
    assert_eq!(
        store().frame_damage_between(id, previous, current, (64, 64)),
        Some(vec![vello::kurbo::Rect::new(10.0, 10.0, 14.0, 14.0)])
    );
    let pixels = store().snapshot(id).unwrap().rgba;
    assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
    let inside = 10_usize
        .saturating_mul(64)
        .saturating_add(10)
        .saturating_mul(4);
    assert_eq!(&pixels[inside..inside.saturating_add(4)], &[0, 255, 0, 255]);
    surface.attach(Some(&changed), 0, 0);
    surface.commit();
    let unknown = h.wait_frame(id, current);
    assert!(
        store()
            .frame_damage_between(id, current, unknown, (64, 64))
            .is_none(),
        "a commit without certified damage must remain whole-buffer"
    );
    store().forget(id);
    toplevel.destroy();
    role.destroy();
    surface.destroy();
    changed.destroy();
    changed_pool.destroy();
    first.destroy();
    pool.destroy();
    h.roundtrip();
}

/// Only what a commit damages is copied out of the client's pool: pixels the
/// client changed outside its damage keep what the previous frame showed,
/// which `wl_surface.damage_buffer` promises they still are. A commit with
/// no damage is copied whole.
#[test]
fn a_damaged_shm_commit_copies_only_its_damage() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let surface = h.peer.compositor.as_ref().unwrap().create_surface(&qh, ());
    let role = h
        .peer
        .desktop
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = role.get_toplevel(&qh, ());
    toplevel.set_app_id("damage-copy".into());
    surface.commit();
    h.roundtrip();
    let server = h.client.object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>(
        &h.display.handle(), surface.id().protocol_id()).unwrap();
    let id = shell::compositor::state::surface_id(&server);
    // Wire bytes are [B, G, R, A].
    let (pool, red) = wire_buffer(&mut h, &[0, 0, 255, 255].repeat(4096));
    surface.attach(Some(&red), 0, 0);
    surface.damage_buffer(0, 0, 64, 64);
    surface.commit();
    let previous = h.wait_frame(id, 0);
    let (blue_pool, blue) = wire_buffer(&mut h, &[255, 0, 0, 255].repeat(4096));
    surface.attach(Some(&blue), 0, 0);
    surface.damage_buffer(10, 10, 4, 4);
    surface.commit();
    let current = h.wait_frame(id, previous);
    let frame = store().snapshot(id).unwrap();
    assert_eq!((frame.width, frame.height), (64, 64));
    for (index, pixel) in frame.rgba.chunks_exact(4).enumerate() {
        let (x, y) = (index % 64, index / 64);
        let damaged = (10..14).contains(&x) && (10..14).contains(&y);
        let expected: &[u8] = if damaged {
            &[0, 0, 255, 255]
        } else {
            &[255, 0, 0, 255]
        };
        assert_eq!(pixel, expected, "pixel ({x}, {y})");
    }
    surface.attach(Some(&blue), 0, 0);
    surface.commit();
    h.wait_frame(id, current);
    let whole = store().snapshot(id).unwrap().rgba;
    assert!(
        whole.chunks_exact(4).all(|pixel| pixel == [0, 0, 255, 255]),
        "a commit without damage is copied whole"
    );
    store().forget(id);
    toplevel.destroy();
    role.destroy();
    surface.destroy();
    blue.destroy();
    blue_pool.destroy();
    red.destroy();
    pool.destroy();
    h.roundtrip();
}

/// After an unmap the next buffer's damage is relative to nothing: a remap
/// that damages a few pixels still shows its whole buffer, never the frame
/// from before the unmap nor zeros around the damage.
#[test]
fn a_damaged_remap_after_an_unmap_shows_its_whole_buffer() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let surface = h.peer.compositor.as_ref().unwrap().create_surface(&qh, ());
    let role = h
        .peer
        .desktop
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = role.get_toplevel(&qh, ());
    toplevel.set_app_id("damage-remap".into());
    surface.commit();
    h.roundtrip();
    let server = h.client.object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>(
        &h.display.handle(), surface.id().protocol_id()).unwrap();
    let id = shell::compositor::state::surface_id(&server);
    // Wire bytes are [B, G, R, A].
    let (pool, red) = wire_buffer(&mut h, &[0, 0, 255, 255].repeat(4096));
    surface.attach(Some(&red), 0, 0);
    surface.damage_buffer(0, 0, 64, 64);
    surface.commit();
    let mapped = h.wait_frame(id, 0);
    surface.attach(None, 0, 0);
    surface.commit();
    let unmapped = h.wait_frame(id, mapped);
    assert!(store().snapshot(id).is_none(), "the unmap drops the frame");
    // An unmapped xdg surface maps again through a fresh initial commit.
    surface.commit();
    h.roundtrip();
    let (blue_pool, blue) = wire_buffer(&mut h, &[255, 0, 0, 255].repeat(4096));
    surface.attach(Some(&blue), 0, 0);
    surface.damage_buffer(10, 10, 4, 4);
    surface.commit();
    h.wait_frame(id, unmapped);
    let frame = store().snapshot(id).expect("remapped frame");
    assert_eq!((frame.width, frame.height), (64, 64));
    assert!(
        frame
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == [0, 0, 255, 255]),
        "the remap is copied whole"
    );
    store().forget(id);
    toplevel.destroy();
    role.destroy();
    surface.destroy();
    blue.destroy();
    blue_pool.destroy();
    red.destroy();
    pool.destroy();
    h.roundtrip();
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
