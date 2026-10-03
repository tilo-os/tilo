//! A window always gets the frame callbacks it waits on, and the compositor
//! knows when it finished answering input.
//!
//! A window the user closes can always finish its last frame.
//!
//! Closing a window removes its tab before the client hears `close`, and the
//! compositor only releases frame callbacks for surfaces it shows. A client
//! whose swapchain waits for the previous frame's callback before it presents
//! again (Mesa's Vulkan WSI in FIFO mode) then blocked forever on the frame it
//! was drawing when the close arrived, so its render thread never joined and
//! the app lived on windowless. Files did this at 154 MB.
//!
//! One real Wayland client against the compositor `State`: it draws, is
//! closed through the production close path, and must still see the frame
//! callback it was waiting on, and the ones for any frame it draws while it
//! shuts down, with or without a paint pass.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "frame counters and a 64x64 buffer size, all far from overflow"
)]

use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::compositor::state::{ClientState, State};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_subcompositor,
    wl_subsurface, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};

const APP_ID: &str = "org.example.Closing";
const SIZE: i32 = 64;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    configured: bool,
    close_requested: bool,
    /// Frame numbers whose `wl_callback.done` arrived.
    frames_done: Vec<u32>,
    /// Pings answered.
    pongs: u32,
    /// While set, pings are left unanswered: a client busy drawing.
    busy: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
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
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Client {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        frame: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frames_done.push(*frame);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        state: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            assert!(!state.busy, "a busy client reads no events");
            base.pong(serial);
            state.pongs += 1;
        }
    }
}

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

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            state.close_requested = true;
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(Client: ignore wl_subsurface::WlSubsurface);
delegate_noop!(Client: ignore xdg_positioner::XdgPositioner);
delegate_noop!(Client: ignore xdg_popup::XdgPopup);

struct Harness {
    display: Display<State>,
    state: State,
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state.set_output_layout(&[shell::compositor::backend::Output::new(
            shell::compositor::backend::OutputId(0),
            (1920, 1080),
            1.0,
        )]);
        let (server, client) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("client insert");
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        let mut harness = Self {
            display,
            state,
            conn,
            queue,
            client: Client::default(),
            _event_loop: event_loop,
        };
        harness.pump();
        harness
    }

    fn bind<I: wayland_client::Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .cloned()
            .unwrap_or_else(|| panic!("{interface} is advertised"));
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version.min(advertised), &self.queue.handle(), ())
    }

    /// Exchange messages both ways until neither side has anything queued.
    /// Never runs a paint pass: whatever the client sees here, it saw without
    /// the compositor drawing a frame.
    fn pump(&mut self) {
        for _ in 0..8 {
            let _ = self.queue.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            if !self.client.busy {
                let _ = self.queue.dispatch_pending(&mut self.client);
            }
        }
    }

    fn pump_until(&mut self, what: &str, done: impl Fn(&Client) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(&self.client) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            self.pump();
            std::thread::yield_now();
        }
    }
}

/// Draw frame `n`: attach `buffer`, ask for the frame's callback, commit.
fn draw(
    surface: &wl_surface::WlSurface,
    buffer: &wl_buffer::WlBuffer,
    qh: &QueueHandle<Client>,
    n: u32,
) {
    surface.attach(Some(buffer), 0, 0);
    surface.damage(0, 0, SIZE, SIZE);
    surface.frame(qh, n);
    surface.commit();
}

fn shm_buffer(h: &Harness) -> wl_buffer::WlBuffer {
    let shm = h.bind::<wl_shm::WlShm>("wl_shm", 1);
    let stride = SIZE * 4;
    let len = usize::try_from(stride * SIZE).expect("buffer size");
    // SAFETY: a null-terminated name and a valid flag word are the whole
    // contract of `memfd_create`.
    let raw = unsafe { libc::memfd_create(c"tilo-closing-window".as_ptr(), 0) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `memfd_create` just handed us this descriptor.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.set_len(len as u64).expect("size the shm pool");
    let qh = h.queue.handle();
    let pool = shm.create_pool(
        file.as_fd(),
        i32::try_from(len).expect("pool size"),
        &qh,
        (),
    );
    pool.create_buffer(0, SIZE, SIZE, stride, wl_shm::Format::Argb8888, &qh, ())
}

fn adopted_surface(state: &State) -> u64 {
    state
        .workspace_tree
        .read()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            shell::shell::state::TileContent::Foreign { app_id, surface_id }
                if app_id == APP_ID =>
            {
                Some(*surface_id)
            }
            _ => None,
        })
        .expect("the toplevel was adopted as a tab")
}

#[test]
fn a_closed_window_gets_the_frame_callbacks_its_last_frames_wait_on() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(APP_ID.into());
    surface.commit();
    h.pump();
    let id = adopted_surface(&h.state);
    // What the paint pass does once layout has placed the new tile.
    shell::compositor::foreign::store()
        .note_placement(id, vello::kurbo::Rect::new(0.0, 0.0, 640.0, 480.0));
    shell::shell::foreign::flush_configures(&mut h.state);
    h.pump_until("the first configure", |c| c.configured);
    let buffer = shm_buffer(&h);

    // Frame 1 is on screen: a paint pass releases it.
    draw(&surface, &buffer, &qh, 1);
    h.pump();
    shell::shell::foreign::release_frame_throttle(&mut h.state);
    h.pump_until("frame 1's callback from a paint", |c| {
        c.frames_done.contains(&1)
    });

    // Frame 2 is in flight when the user closes the window.
    draw(&surface, &buffer, &qh, 2);
    h.pump();
    assert!(
        !h.client.frames_done.contains(&2),
        "nothing painted yet, so frame 2 is still throttled"
    );
    h.state.close_surface_on_request(id);
    h.pump_until("the close request", |c| c.close_requested);
    h.pump_until("frame 2's callback at the close", |c| {
        c.frames_done.contains(&2)
    });

    // The client draws once more while it shuts down; its tab is gone, so no
    // paint would ever show it. The callback must still come, without one.
    draw(&surface, &buffer, &qh, 3);
    h.pump_until("frame 3's callback on commit", |c| {
        c.frames_done.contains(&3)
    });

    // And a paint pass does not skip it either.
    surface.frame(&qh, 4);
    surface.commit();
    h.pump();
    shell::shell::foreign::release_frame_throttle(&mut h.state);
    h.pump_until("frame 4's callback", |c| c.frames_done.contains(&4));
}

/// A client may ask for its next frame on a commit that brings no buffer
/// (a configure ack, a title). Nothing on screen changes. Releasing the
/// callback used to require a full present, so an idle app that asked
/// again kept the shell building frames. The callback has to fire or a
/// FIFO swapchain waits forever; it fires here without a present.
#[test]
fn a_frame_asked_for_without_a_buffer_does_not_need_a_paint() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(APP_ID.into());
    surface.commit();
    h.pump();
    let id = adopted_surface(&h.state);
    shell::compositor::foreign::store()
        .note_placement(id, vello::kurbo::Rect::new(0.0, 0.0, 640.0, 480.0));
    shell::shell::foreign::flush_configures(&mut h.state);
    h.pump_until("the first configure", |c| c.configured);
    let buffer = shm_buffer(&h);

    draw(&surface, &buffer, &qh, 1);
    h.pump();
    shell::shell::foreign::release_frame_throttle(&mut h.state);
    h.pump_until("frame 1's callback from a paint", |c| {
        c.frames_done.contains(&1)
    });
    assert!(
        !shell::shell::foreign::frame_callbacks_waiting(&h.state),
        "every callback was released: an idle client asks for no paint"
    );

    surface.frame(&qh, 2);
    surface.commit();
    h.pump();
    assert!(
        shell::shell::foreign::frame_callbacks_waiting(&h.state),
        "frame 2's callback came without a buffer"
    );
    assert!(
        shell::shell::foreign::release_idle_frame_callbacks(&mut h.state),
        "an unchanged commit releases its callback without a present"
    );
    h.pump_until("frame 2's callback", |c| c.frames_done.contains(&2));
    assert!(
        !shell::shell::foreign::frame_callbacks_waiting(&h.state),
        "the idle release fired the callback"
    );
}

/// A mapped toplevel of [`APP_ID`], placed and configured, with a buffer to
/// draw with.
fn shown_window(
    h: &mut Harness,
) -> (
    wl_compositor::WlCompositor,
    xdg_wm_base::XdgWmBase,
    wl_surface::WlSurface,
    xdg_surface::XdgSurface,
    wl_buffer::WlBuffer,
) {
    let qh = h.queue.handle();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(APP_ID.into());
    surface.commit();
    h.pump();
    let id = adopted_surface(&h.state);
    shell::compositor::foreign::store()
        .note_placement(id, vello::kurbo::Rect::new(0.0, 0.0, 640.0, 480.0));
    shell::shell::foreign::flush_configures(&mut h.state);
    h.pump_until("the first configure", |c| c.configured);
    let buffer = shm_buffer(h);
    draw(&surface, &buffer, &qh, 1);
    h.pump();
    shell::shell::foreign::release_frame_throttle(&mut h.state);
    h.pump_until("frame 1's callback", |c| c.frames_done.contains(&1));
    // Frame 1 is captured too: a window handed out here has nothing in
    // flight, whatever the load on the machine.
    let client_index = u32::try_from(id >> 32).expect("a client index");
    let deadline = tilo_test_env::hang_deadline();
    while shell::compositor::foreign::store().capturing_for_client(client_index) {
        assert!(
            std::time::Instant::now() < deadline,
            "frame 1 was never captured"
        );
        h.pump();
        std::thread::yield_now();
    }
    (compositor, wm, surface, xdg, buffer)
}

/// A window's subsurfaces and popups are drawn with it. A toolkit that draws
/// a menu on its own swapchain waits on that surface's frame callback before
/// it presents anything else, so they are answered with the window's.
#[test]
fn a_shown_windows_subsurface_and_popup_get_their_frame_callbacks() {
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let (compositor, wm, surface, xdg, buffer) = shown_window(&mut h);

    let subcompositor = h.bind::<wl_subcompositor::WlSubcompositor>("wl_subcompositor", 1);
    let child = compositor.create_surface(&qh, ());
    let _subsurface = subcompositor.get_subsurface(&child, &surface, &qh, ());
    draw(&child, &buffer, &qh, 10);
    surface.commit();
    h.pump();

    let positioner = wm.create_positioner(&qh, ());
    positioner.set_size(32, 32);
    positioner.set_anchor_rect(0, 0, 8, 8);
    let menu = compositor.create_surface(&qh, ());
    let menu_xdg = wm.get_xdg_surface(&menu, &qh, ());
    let _popup = menu_xdg.get_popup(Some(&xdg), &positioner, &qh, ());
    menu.commit();
    h.pump();
    draw(&menu, &buffer, &qh, 20);
    h.pump();
    assert!(
        shell::shell::foreign::frame_callbacks_waiting(&h.state),
        "the subsurface and the popup wait on their callbacks"
    );

    shell::shell::foreign::release_frame_throttle(&mut h.state);
    h.pump_until("the subsurface's and the popup's callbacks", |c| {
        c.frames_done.contains(&10) && c.frames_done.contains(&20)
    });
    assert!(!shell::shell::foreign::frame_callbacks_waiting(&h.state));
}

/// Drive `settle` until it is no longer waiting, pumping the client between
/// steps.
fn settle(
    h: &mut Harness,
    settle: &mut shell::compositor::client_settle::Settle,
) -> shell::compositor::client_settle::Progress {
    use shell::compositor::client_settle::Progress;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        h.pump();
        let progress = settle.advance(&mut h.state, std::time::Instant::now());
        if progress != Progress::Waiting {
            return progress;
        }
        assert!(std::time::Instant::now() < deadline, "timed out settling");
        std::thread::yield_now();
    }
}

/// A screenshot after input waits for the window to answer two pings in
/// turn, and for the compositor to capture the frame it drew; an idle window
/// answers at once, a busy one is waited for.
#[test]
fn a_window_has_answered_input_after_two_pings_and_a_captured_frame() {
    use shell::compositor::client_settle::{Progress, Settle};
    let mut h = Harness::new();
    let qh = h.queue.handle();
    let (_compositor, _wm, surface, _xdg, buffer) = shown_window(&mut h);
    let id = adopted_surface(&h.state);

    // An idle window: nothing to draw, two round trips and it is settled.
    let server_surface =
        shell::shell::foreign::toplevel_surface(&h.state, id).expect("the window's surface");
    let mut idle = Settle::begin(
        &mut h.state,
        std::slice::from_ref(&server_surface),
        std::time::Instant::now(),
    );
    let before = h.client.pongs;
    assert_eq!(settle(&mut h, &mut idle), Progress::Settled);
    assert_eq!(h.client.pongs - before, 2, "two pings, in turn");

    // A window drawing on its own thread answers pings meanwhile; the frame
    // callback it asked for and has not committed says a frame is coming.
    surface.frame(&qh, 5);
    let mut drawing = Settle::begin(
        &mut h.state,
        std::slice::from_ref(&server_surface),
        std::time::Instant::now(),
    );
    for _ in 0..4 {
        h.pump();
        assert_eq!(
            drawing.advance(&mut h.state, std::time::Instant::now()),
            Progress::Waiting,
            "an announced frame keeps the screenshot waiting"
        );
    }
    let seq = shell::compositor::foreign::store().frame_seq(id);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, SIZE, SIZE);
    surface.commit();
    assert_eq!(settle(&mut h, &mut drawing), Progress::Settled);
    assert!(
        shell::compositor::foreign::store().frame_seq(id) > seq,
        "the announced frame is in the screenshot"
    );

    // A window busy drawing reads no events: the screenshot waits for it.
    h.client.busy = true;
    let mut busy = Settle::begin(
        &mut h.state,
        std::slice::from_ref(&server_surface),
        std::time::Instant::now(),
    );
    for _ in 0..4 {
        h.pump();
        assert_eq!(
            busy.advance(&mut h.state, std::time::Instant::now()),
            Progress::Waiting,
            "an unanswered ping keeps the screenshot waiting"
        );
    }
    // It finishes its frame, then reads on: the second answer follows the
    // commit, and the screenshot follows the frame's capture.
    let seq = shell::compositor::foreign::store().frame_seq(id);
    draw(&surface, &buffer, &qh, 2);
    h.client.busy = false;
    assert_eq!(settle(&mut h, &mut busy), Progress::Settled);
    assert!(
        shell::compositor::foreign::store().frame_seq(id) > seq,
        "the frame drawn in answer is captured before the screenshot is taken"
    );
    assert!(
        h.client.frames_done.contains(&2),
        "and its frame callback was not held back meanwhile"
    );

    // A window that never answers is given up on.
    h.client.busy = true;
    let started = std::time::Instant::now();
    let mut hung = Settle::begin(&mut h.state, std::slice::from_ref(&server_surface), started);
    h.pump();
    assert_eq!(
        hung.advance(
            &mut h.state,
            started + shell::compositor::client_settle::SETTLE_LIMIT
        ),
        Progress::TimedOut
    );
}
