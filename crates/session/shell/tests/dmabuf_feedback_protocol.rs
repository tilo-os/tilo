//! End-to-end `zwp_linux_dmabuf_v1` feedback walk — the handshake a GPU
//! client performs before it can create its first window.
//!
//! Mesa removed `wl_drm`, so `dmabuf_feedback.main_device` is the only
//! way a client learns which GPU to allocate on. A compositor advertising
//! a version-3 dmabuf global offers the protocol without that event;
//! Mesa reports `libEGL warning: failed to get driver name for fd -1`
//! then fails to create the window silently — the client connects, stays
//! alive, and never maps a toplevel, with no compositor-side error.
//!
//! Runs an in-process wayland-client through a `UnixStream` pair into a
//! real headless [`State`], asserting: the global is version >= 4;
//! `get_default_feedback` yields a `main_device` naming a real, openable
//! DRM node; the format table offers only buffers
//! [`shell::compositor::foreign::validate_importable`] would accept.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use shell::compositor::render_node;
use shell::compositor::state::{ClientState, State};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, protocol::wl_registry,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1},
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TableEntry {
    fourcc: u32,
    modifier: u64,
}

#[derive(Default)]
struct ClientAppState {
    dmabuf: Option<ZwpLinuxDmabufV1>,
    /// Version the *compositor* advertised. `< 4` means no feedback, i.e.
    /// the bug this test guards.
    dmabuf_version: Option<u32>,
    main_device: Option<u64>,
    formats: Vec<TableEntry>,
    /// Set once `zwp_linux_dmabuf_feedback_v1.done` arrives — the protocol's
    /// "this batch is complete" marker.
    feedback_done: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ClientAppState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
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
            && interface == "zwp_linux_dmabuf_v1"
        {
            state.dmabuf_version = Some(version);
            // Bind at `min(version, 4)`, so a v3 compositor still binds successfully
            // and then never sends feedback — the failure the assertions below catch.
            state.dmabuf =
                Some(registry.bind::<ZwpLinuxDmabufV1, _, _>(name, version.min(4), qh, ()));
        }
    }
}

delegate_noop!(ClientAppState: ignore ZwpLinuxDmabufV1);

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for ClientAppState {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                // `dev_t` on the wire, native-endian, 8 bytes.
                let mut buf = [0u8; 8];
                let n = device.len().min(8);
                buf[..n].copy_from_slice(&device[..n]);
                state.main_device = Some(u64::from_ne_bytes(buf));
            }
            zwp_linux_dmabuf_feedback_v1::Event::FormatTable { fd, size } => {
                // The table is a sealed memfd of 16-byte
                // `{u32 format, u32 pad, u64 modifier}` records.
                let len = size as usize;
                let map = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE,
                        std::os::fd::AsRawFd::as_raw_fd(&fd),
                        0,
                    )
                };
                assert_ne!(map, libc::MAP_FAILED, "format table memfd must be mappable");
                let bytes = unsafe { std::slice::from_raw_parts(map as *const u8, len) };
                for rec in bytes.chunks_exact(16) {
                    state.formats.push(TableEntry {
                        fourcc: u32::from_ne_bytes(rec[0..4].try_into().unwrap()),
                        modifier: u64::from_ne_bytes(rec[8..16].try_into().unwrap()),
                    });
                }
                unsafe {
                    libc::munmap(map, len);
                }
            }
            zwp_linux_dmabuf_feedback_v1::Event::Done => state.feedback_done = true,
            _ => {}
        }
    }
}

fn pump(
    conn: &Connection,
    ev: &mut wayland_client::EventQueue<ClientAppState>,
    app: &mut ClientAppState,
    display: &mut Display<State>,
    server_state: &mut State,
    predicate: impl Fn(&ClientAppState) -> bool,
    label: &str,
) {
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        let _ = conn.flush();
        let _ = display.dispatch_clients(server_state);
        let _ = display.flush_clients();
        let _ = ev.dispatch_pending(app);
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = ev.dispatch_pending(app);
        if predicate(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "dmabuf feedback walk stalled at '{label}' \
         (global_version={:?}, main_device={:?}, formats={}, done={})",
        app.dmabuf_version,
        app.main_device,
        app.formats.len(),
        app.feedback_done,
    );
}

/// `None` when the machine has no advertisable render node — a GPU-less
/// box legitimately advertises nothing.
fn walk() -> Option<(ClientAppState, std::path::PathBuf)> {
    let node = render_node::probe().ok()?;

    let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop event loop");
    let mut display: Display<State> = Display::new().expect("wayland display");
    let display_handle = display.handle();
    let mut server_state = State::new(event_loop.handle(), display_handle);

    let (client_side, server_side) = UnixStream::pair().expect("socket pair");
    let _client = display
        .handle()
        .insert_client(server_side, Arc::new(ClientState::default()))
        .expect("insert client");

    let conn = Connection::from_socket(client_side).expect("client from socket");
    let mut event_queue: wayland_client::EventQueue<ClientAppState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    let mut app = ClientAppState::default();
    pump(
        &conn,
        &mut event_queue,
        &mut app,
        &mut display,
        &mut server_state,
        |a| a.dmabuf.is_some(),
        "registry walk for zwp_linux_dmabuf_v1",
    );

    // The request Mesa makes to find the GPU, and the one a version-3 global
    // cannot answer.
    let dmabuf = app.dmabuf.clone().expect("dmabuf bound");
    assert!(
        dmabuf.version() >= 4,
        "bound zwp_linux_dmabuf_v1 at version {} — get_default_feedback needs 4",
        dmabuf.version()
    );
    let _feedback = dmabuf.get_default_feedback(&qh, ());
    pump(
        &conn,
        &mut event_queue,
        &mut app,
        &mut display,
        &mut server_state,
        |a| a.feedback_done,
        "default dmabuf feedback",
    );
    Some((app, node.path))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn dmabuf_feedback_advertises_the_render_node_and_compositable_formats() {
    // Phase `dmabuf_global_is_advertised_with_feedback`.
    // Version 3 carries no feedback and therefore no device, and Mesa
    // cannot initialise EGL without one.
    {
        fn phase() {
            let Some((app, _node)) = walk() else {
                eprintln!("skipping: no advertisable render node on this machine");
                return;
            };
            let version = app.dmabuf_version.expect("global advertised");
            assert!(
                version >= 4,
                "zwp_linux_dmabuf_v1 advertised at version {version}; a GPU client needs \
                 version 4 feedback to learn the main device, and fails EGL init \
                 (`failed to get driver name for fd -1`) without it"
            );
            assert!(
                app.feedback_done,
                "the compositor never completed a dmabuf_feedback batch"
            );
        }
        phase();
    }

    // Phase `feedback_main_device_is_the_advertised_render_node`.
    // A wrong or absent number sends the client to no GPU at all.
    {
        fn phase() {
            use std::os::unix::fs::MetadataExt as _;

            let Some((app, node)) = walk() else {
                eprintln!("skipping: no advertisable render node on this machine");
                return;
            };
            let expected = std::fs::metadata(&node)
                .expect("probed render node must stat")
                .rdev();
            let got = app
                .main_device
                .expect("dmabuf_feedback carried no main_device event");
            assert_eq!(
                got,
                expected as u64,
                "main_device must be st_rdev of {} — clients resolve the GPU from this \
                 number and get nothing usable if it is wrong",
                node.display()
            );
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&node)
                .unwrap_or_else(|e| {
                    panic!(
                        "advertised main device {} cannot be opened: {e} — a client told to \
                         allocate there would fail the same way",
                        node.display()
                    )
                });
        }
        phase();
    }

    // Phase `every_advertised_format_is_one_the_compositor_can_composite`.
    // Advertising more than the compositor can composite is what produces a
    // client that renders happily into an error tile forever.
    {
        fn phase() {
            use smithay::backend::allocator::{Fourcc, Modifier};

            let Some((app, _node)) = walk() else {
                eprintln!("skipping: no advertisable render node on this machine");
                return;
            };
            assert!(
                !app.formats.is_empty(),
                "the format table is empty; a client has nothing to allocate"
            );
            for entry in &app.formats {
                let fourcc = Fourcc::try_from(entry.fourcc).unwrap_or_else(|_| {
                    panic!("advertised an unrecognised FourCC 0x{:08x}", entry.fourcc)
                });
                // Single-plane RGBA under LINEAR is the whole importable set — see
                // `foreign::validate_importable`, the gate a client buffer passes.
                assert!(
                    matches!(
                        fourcc,
                        Fourcc::Argb8888 | Fourcc::Xrgb8888 | Fourcc::Abgr8888 | Fourcc::Xbgr8888
                    ),
                    "advertised {fourcc:?}, which the tile importer cannot composite; \
                     a client that picks it renders into an error tile"
                );
                assert_eq!(
                    Modifier::from(entry.modifier),
                    Modifier::Linear,
                    "advertised modifier 0x{:x} for {fourcc:?}; the importer only handles \
                     LINEAR, so a client using this would produce unreadable buffers",
                    entry.modifier
                );
            }
        }
        phase();
    }
}
