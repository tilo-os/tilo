//! A client's first dmabuf commit must not freeze the compositor.
//!
//! Every commit of a tiled toplevel runs a pre-commit hook that queues an
//! off-thread readback of the pending dmabuf and installs a Smithay
//! transaction barrier until that read is done. Smithay's `with_states`
//! holds the surface's non-reentrant state mutex for the whole callback and
//! `add_blocker` takes the same mutex, so installing the barrier from inside
//! that callback deadlocks the compositor thread on the first GPU-backed
//! commit — the whole session freezes as soon as an app like Chat presents.
//!
//! Drives the real path over a socket pair: a toplevel is configured, then
//! commits a single-plane linear ARGB dmabuf. The descriptor is a memfd,
//! which the CPU-mapping importer reads exactly like a linear PRIME buffer.
//! The server side runs on its own thread so a regression fails the test
//! instead of hanging the suite.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use shell::compositor::state::State;

use vello::kurbo::Rect;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// `DRM_FORMAT_ARGB8888`, one of the formats the compositor advertises.
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
const WIDTH: i32 = 64;
const HEIGHT: i32 = 48;
const STRIDE: u32 = WIDTH.unsigned_abs() * 4;
const BUFFER_LEN: u32 = STRIDE * HEIGHT.unsigned_abs();

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configured: bool,
    roundtrip_done: bool,
    buffer_released: bool,
}

crate::support::wayland::test_client!(Client: registry, ping);

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
impl Dispatch<wl_callback::WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.roundtrip_done = true;
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.buffer_released = true;
    }
}
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1);
delegate_noop!(Client: ignore zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1);

type Harness = crate::support::wayland::Harness<Client>;

fn linear_argb_memfd() -> OwnedFd {
    let name = c"dmabuf-commit-barrier";
    // SAFETY: memfd_create with a static nul-terminated name and MFD_CLOEXEC.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: memfd_create returned a new descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let file = std::fs::File::from(fd);
    file.set_len(u64::from(BUFFER_LEN)).expect("size memfd");
    OwnedFd::from(file)
}

fn configure_delayed_fence_gpu(h: &mut Harness) {
    let mut context = ui::gpu::RenderContext::new();
    let device = pollster::block_on(context.device(None)).expect("actual capture GPU adapter");
    let handle = &context.devices[device];
    shell::shell::gpu_frames::publish_present_device(&handle.device, &handle.queue);
    if h.state.dmabuf_global.is_none() {
        use smithay::backend::allocator::{Format, Fourcc, Modifier};
        h.state.dmabuf_global = Some(h.state.dmabuf_state.create_global::<State>(
            &h.display.handle(),
            vec![Format {
                code: Fourcc::Argb8888,
                modifier: Modifier::Linear,
            }],
        ));
    }
}

fn finish_delayed_capture(
    h: &mut Harness,
    surface: &wl_surface::WlSurface,
    mut producer: UnixStream,
    qh: &QueueHandle<Client>,
) {
    use std::io::Write;
    // The real pollable descriptor is intentionally not signalled yet. It
    // follows protocol admission and the production pre-commit/worker path;
    // it is not asserted to be valid GPU pixel storage.
    let root = h.state.xdg_shell_state.toplevel_surfaces()[0].wl_surface();
    let id = shell::compositor::state::surface_id(root);
    let client_index = u32::try_from(id >> 32).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(shell::compositor::foreign::store().capturing_for_client(client_index));
    assert!(!h.client.buffer_released);
    h.conn.display().sync(qh, ());
    h.pump();
    assert!(
        h.client.roundtrip_done,
        "unrelated real Wayland dispatch must finish while the capture fence is blocked"
    );
    assert!(shell::compositor::foreign::store().capturing_for_client(client_index));
    producer.write_all(&[1]).unwrap();
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(5))
        .unwrap();
    while shell::compositor::foreign::store().capturing_for_client(client_index) {
        assert!(
            std::time::Instant::now() < deadline,
            "the real capture result must settle after fence readiness"
        );
        h.state.pump_foreign_capture_barriers(&h.display.handle());
        h.pump();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // Invalid GPU storage must never invent captured pixels or a physical
    // presentation. Its failed import still settles the exact commit.
    assert!(
        shell::compositor::foreign::store()
            .presentation_pixels(id)
            .is_none()
    );
    surface.attach(None, 0, 0);
    surface.commit();
    h.pump();
    assert!(
        h.client.buffer_released,
        "the buffer lease must be released after actual settled detach"
    );
}

/// `true` once the dmabuf commit has been dispatched, `false` when this
/// machine advertises no dmabuf global.
fn commit_first_dmabuf(delayed_fence: bool) -> bool {
    let mut h = Harness::new();
    if delayed_fence {
        configure_delayed_fence_gpu(&mut h);
        // The delayed fixture creates its global after the shared harness's
        // initial registry exchange. Hear that announcement before binding.
        h.pump();
    }
    let Some(dmabuf) =
        h.try_bind::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>("zwp_linux_dmabuf_v1", 4)
    else {
        return false;
    };
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm_base = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id("org.tilo.test.dmabuf-commit".into());
    surface.commit();
    h.pump();

    for toplevel in h.state.xdg_shell_state.toplevel_surfaces().to_vec() {
        let id = shell::compositor::state::surface_id(toplevel.wl_surface());
        shell::compositor::foreign::store().note_placement(id, Rect::new(0.0, 0.0, 640.0, 480.0));
    }
    shell::compositor::foreign::flush_configures(&mut h.state);
    h.pump();
    assert!(
        h.client.configured,
        "the toplevel was never configured, so the dmabuf commit below would be a protocol error rather than the path under test"
    );

    let delayed = delayed_fence.then(|| UnixStream::pair().unwrap());
    let memfd = if let Some((read, _)) = delayed.as_ref() {
        read.as_fd().try_clone_to_owned().unwrap()
    } else {
        linear_argb_memfd()
    };
    let params = dmabuf.create_params(&qh, ());
    params.add(memfd.as_fd(), 0, 0, STRIDE, 0, 0);
    let buffer = params.create_immed(
        WIDTH,
        HEIGHT,
        DRM_FORMAT_ARGB8888,
        zwp_linux_buffer_params_v1::Flags::empty(),
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, WIDTH, HEIGHT);
    surface.commit();
    // Dispatching this commit runs the capture pre-commit hook.
    h.pump();

    if let Some((_, producer)) = delayed {
        finish_delayed_capture(&mut h, &surface, producer, &qh);
    } else {
        std::mem::forget((surface, xdg_surface, toplevel, buffer, params, wm_base));
    }
    true
}

#[test]
fn the_first_dmabuf_commit_returns_to_the_compositor_loop() {
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(commit_first_dmabuf(false));
    });
    match finished.recv_timeout(tilo_test_env::hang_bound()) {
        Ok(true) => {}
        Ok(false) => eprintln!("skipping: no advertisable render node on this machine"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "dispatching a dmabuf commit never returned: installing the capture barrier re-entered the surface state lock and deadlocked the compositor thread"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the compositor thread panicked while committing a dmabuf")
        }
    }
}

#[test]
fn delayed_real_producer_fence_keeps_the_compositor_dispatch_responsive() {
    assert!(
        commit_first_dmabuf(true),
        "the native fixture must expose its configured dma-buf path"
    );
}
