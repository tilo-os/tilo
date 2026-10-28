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
use wayland_client::delegate_noop;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_surface};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

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

crate::support::wayland::test_client!(Client: registry, ping, configured);

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
        shell::shell::foreign::store().note_placement(id, Rect::new(0.0, 0.0, 640.0, 480.0));
    }
    shell::shell::foreign::flush_configures(&mut h.state);
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

/// The app id of the window restored into its saved tab below.
const RESTORED_APP: &str = "org.tilo.test.restored-dmabuf";

/// A tab saved from the last session, still showing its opening card, which
/// the next window of [`RESTORED_APP`] binds into: what restoring a window at
/// shell start leaves for the relaunched app.
fn restore_a_saved_tab(h: &Harness) -> shell::shell::state::TabId {
    use shell::shell::state::TileContent;
    use shell::shell::tab_binding::{self, Placement};
    let target = {
        let mut tree = h.state.workspace_tree.write();
        // A placeholder surface id that no client owns stands in for the tab
        // the saved session carried.
        let tab = tab_binding::adopt_toplevel_at(
            &mut tree,
            0x7e57_0000_0001,
            Some(RESTORED_APP.into()),
            Placement::NewGroup,
        );
        for group in tree
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.strips.iter_mut())
            .flat_map(|strip| strip.window_groups.iter_mut())
        {
            for saved in group.tabs.iter_mut().filter(|saved| saved.id == tab) {
                saved.tile = TileContent::PeerWindow {
                    app_id: RESTORED_APP.into(),
                    label: "Restored".into(),
                    note: "Opening here".into(),
                    missing: None,
                };
            }
        }
        drop(tree);
        tab
    };
    // The opening card was laid out, so the window is configured to its
    // size in the dispatch that binds it.
    shell::shell::foreign::store().note_opening_tab(target, Rect::new(0.0, 0.0, 640.0, 480.0));
    shell::shell::launcher::set_pending_placement(RESTORED_APP, Placement::IntoTab(target));
    target
}

/// Apply released capture barriers until `id` shows a frame.
fn wait_for_first_frame(h: &mut Harness, id: u64) -> u64 {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        h.pump();
        let display = h.display.handle();
        h.state.pump_foreign_capture_barriers(&display);
        let seq = shell::shell::foreign::store().frame_seq(id);
        if seq != 0 {
            return seq;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the restored window's first dmabuf frame never reached its tab: an idle client draws nothing more, so the tab would say the window can't be shown"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A window restored at shell start binds into its saved tab and commits its
/// first dmabuf frame. That frame is the one its tab shows: the client is
/// idle afterwards and draws nothing more. `commit_on_map` commits it in the
/// same flush as the configure's ack, as a client that draws immediately
/// does; otherwise the client waits for a second configure first.
/// `gpu_after_commit` opens the presenting device only once the frame is
/// committed, as a laptop's renderer comes up after the first apps drew: the
/// frame's capture starts without a presenting GPU, and the GPU's probe and
/// the dmabuf feedback it changes land while that capture is pending.
fn restored_window_shows_its_first_frame(commit_on_map: bool, gpu_after_commit: bool) -> bool {
    let mut h = Harness::new();
    let Some(dmabuf) =
        h.try_bind::<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>("zwp_linux_dmabuf_v1", 4)
    else {
        return false;
    };
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm_base = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    h.pump();
    let target = restore_a_saved_tab(&h);

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id(RESTORED_APP.into());
    surface.commit();
    h.pump();
    let id = h
        .state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(|toplevel| shell::compositor::state::surface_id(toplevel.wl_surface()))
        .next()
        .expect("the restored window's toplevel");
    {
        let tree = h.state.workspace_tree.read();
        assert_eq!(
            shell::shell::tab_binding::app_id_for_surface(&tree, id),
            Some(RESTORED_APP),
            "the window bound into a tab"
        );
        assert!(
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .any(|tab| tab.id == target
                    && matches!(&tab.tile, shell::shell::state::TileContent::Foreign { surface_id, .. } if *surface_id == id)),
            "the window took its saved tab"
        );
        drop(tree);
    }
    shell::shell::foreign::flush_configures(&mut h.state);
    h.pump();
    assert!(h.client.configured, "the restored window was configured");
    if !commit_on_map {
        h.client.configured = false;
        shell::shell::foreign::store().note_placement(id, Rect::new(0.0, 0.0, 600.0, 400.0));
        shell::shell::foreign::flush_configures(&mut h.state);
        h.pump();
        assert!(h.client.configured, "the second configure arrived");
    }

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
    if gpu_after_commit {
        let mut context = ui::gpu::RenderContext::new();
        let Some(device) = pollster::block_on(context.device(None)) else {
            return false;
        };
        let Some(handle) = context.devices.get(device) else {
            return false;
        };
        shell::shell::gpu_frames::publish_present_device(&handle.device, &handle.queue);
    }
    let seq = wait_for_first_frame(&mut h, id);
    assert!(
        shell::shell::foreign::store().is_mapped(id),
        "the frame's commit applied"
    );
    assert_ne!(seq, 0);
    shell::shell::launcher::clear_pending_placements();
    std::mem::forget((
        surface,
        xdg_surface,
        toplevel,
        buffer,
        params,
        wm_base,
        memfd,
    ));
    true
}

#[test]
#[serial_test::serial(process_env)]
fn a_restored_window_that_commits_on_map_shows_its_first_frame() {
    if !restored_window_shows_its_first_frame(true, false) {
        eprintln!("skipping: no advertisable render node on this machine");
    }
}

#[test]
#[serial_test::serial(process_env)]
fn a_restored_window_that_waits_for_its_tab_size_shows_its_first_frame() {
    if !restored_window_shows_its_first_frame(false, false) {
        eprintln!("skipping: no advertisable render node on this machine");
    }
}

#[test]
#[serial_test::serial(process_env)]
fn a_restored_window_keeps_its_first_frame_when_the_presenting_gpu_arrives() {
    if !restored_window_shows_its_first_frame(true, true) {
        eprintln!("skipping: no advertisable render node or no GPU adapter on this machine");
    }
}
