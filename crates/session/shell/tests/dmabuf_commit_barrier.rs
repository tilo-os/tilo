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
use std::sync::mpsc;

use vello::kurbo::Rect;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_surface};
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
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
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

/// `true` once the dmabuf commit has been dispatched, `false` when this
/// machine advertises no dmabuf global.
fn commit_first_dmabuf() -> bool {
    let mut h = Harness::new();
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

    let memfd = linear_argb_memfd();
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

    std::mem::forget((surface, xdg_surface, toplevel, buffer, params, wm_base));
    true
}

#[test]
fn the_first_dmabuf_commit_returns_to_the_compositor_loop() {
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(commit_first_dmabuf());
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
