//! What a Wayland client sees when a remote participant types into it.
//!
//! The client decodes its `wl_keyboard` stream into text with a US layout
//! (Shift from the modifiers event), so the assertion is on the text the
//! person typed, exactly. A remote key repeats only as its sender repeats
//! it: the client's own repeat is off while a participant types, so a
//! release that crosses the network late cannot type the key meanwhile.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::network::{InputEvent, ShellView, StreamingShare};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, PhysicalKey};

const SHIFT: u32 = 42;
const ROWS: [(&str, &str, u32); 4] = [
    ("1234567890", "!@#$%^&*()", 2),
    ("qwertyuiop", "QWERTYUIOP", 16),
    ("asdfghjkl;", "ASDFGHJKL:", 30),
    ("zxcvbnm,./", "ZXCVBNM<>?", 44),
];

/// The US-layout evdev keycode for `c`, and whether it takes Shift.
fn stroke(c: char) -> (u32, bool) {
    if c == ' ' {
        return (57, false);
    }
    ROWS.iter()
        .find_map(|(plain, shifted, first)| {
            let at = |row: &str| {
                row.chars()
                    .position(|other| other == c)
                    .map(|at| first.checked_add(u32::try_from(at).unwrap()).unwrap())
            };
            at(plain)
                .map(|code| (code, false))
                .or_else(|| at(shifted).map(|code| (code, true)))
        })
        .expect("a US key types it")
}

/// What `code` types on a US layout.
fn character(code: u32, shift: bool) -> Option<char> {
    if code == 57 {
        return Some(' ');
    }
    ROWS.iter().find_map(|(plain, shifted, first)| {
        let at = usize::try_from(code.checked_sub(*first)?).ok()?;
        (if shift { shifted } else { plain }).chars().nth(at)
    })
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    presentation_clock: Option<u32>,
    presentation_seen: std::collections::HashMap<u32, PresentationSeen>,
    shift: bool,
    text: String,
    repeat_rate: Option<i32>,
    focus: Option<wl_surface::WlSurface>,
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

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => state.focus = Some(surface),
            wl_keyboard::Event::Leave { .. } => state.focus = None,
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                state.shift = mods_depressed & 1 != 0;
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                if let Some(c) = character(key, state.shift) {
                    state.text.push(c);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, .. } => state.repeat_rate = Some(rate),
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
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

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

struct Harness {
    display: Display<shell::compositor::state::State>,
    state: shell::compositor::state::State,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    _event_loop: EventLoop<'static, shell::compositor::state::State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = shell::compositor::state::State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(
                server,
                Arc::new(shell::compositor::state::ClientState::default()),
            )
            .expect("client insert");
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        Self {
            display,
            state,
            queue,
            client: Client::default(),
            conn,
            _event_loop: event_loop,
        }
    }

    fn pump(&mut self) {
        for _ in 0..6 {
            let _ = self.queue.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.client);
        }
    }

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .expect("advertised global")
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version.min(advertised), &self.queue.handle(), ())
    }
}

fn foreign_id(state: &shell::compositor::state::State, app_id: &str) -> u64 {
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
            shell::shell::state::TileContent::Foreign {
                app_id: actual,
                surface_id,
            } if actual == app_id => Some(*surface_id),
            _ => None,
        })
        .expect("foreign surface was adopted")
}

fn key(keysym: u32, pressed: bool) -> InputEvent {
    InputEvent::Key {
        keysym,
        pressed,
        modifiers: 0,
    }
}

/// A key of the machine's own keyboard, as the backend hands it over.
fn local_key(code: winit::keyboard::KeyCode, pressed: bool) -> WindowEvent {
    // `KeyEvent` keeps platform data private; zero it and set what the
    // compositor reads.
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = Key::Character("a".into());
    event.physical_key = PhysicalKey::Code(code);
    event.location = KeyLocation::Standard;
    event.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event,
        is_synthetic: false,
    }
}

#[test]
fn a_remote_participant_types_exactly_its_text_and_repeats_only_as_its_sender_does() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Writer".into());
    h.pump();

    let window = foreign_id(&h.state, "org.example.Writer");
    h.state.set_client_keyboard_focus(Some(window));
    h.pump();
    let configured = h.client.repeat_rate.expect("the seat's repeat info");
    assert!(configured > 0, "clients repeat the machine's own keys");

    let peer = 9;
    h.state
        .share_registry
        .insert(peer, window, StreamingShare::new());
    h.state.remote_input_firewall.publish_view(ShellView::new(
        vec![(window, Rect::new(0.0, 0.0, 800.0, 600.0))],
        Some(window),
        false,
        false,
    ));
    assert!(h.state.apply_remote_input(
        peer,
        window,
        InputEvent::KeyboardFocus {
            surface: window,
            focused: true,
        },
    ));

    let text = "Hello, World! example.com: 42?";
    for c in text.chars() {
        let (code, shift) = stroke(c);
        if shift {
            assert!(h.state.apply_remote_input(peer, window, key(SHIFT, true)));
        }
        assert!(h.state.apply_remote_input(peer, window, key(code, true)));
        assert!(h.state.apply_remote_input(peer, window, key(code, false)));
        if shift {
            assert!(h.state.apply_remote_input(peer, window, key(SHIFT, false)));
        }
    }
    h.pump();
    assert_eq!(
        h.client.text, text,
        "every stroke, in order, with its Shift"
    );
    assert_eq!(
        h.client.repeat_rate,
        Some(0),
        "the client does not repeat a remote key on its own"
    );

    // The sender holds "o" and repeats it twice; then Shift, which never repeats.
    h.client.text.clear();
    let (o, _) = stroke('o');
    for _ in 0..3 {
        assert!(h.state.apply_remote_input(peer, window, key(o, true)));
    }
    assert!(h.state.apply_remote_input(peer, window, key(o, false)));
    assert!(h.state.apply_remote_input(peer, window, key(SHIFT, true)));
    let _ = h.state.apply_remote_input(peer, window, key(SHIFT, true));
    assert!(h.state.apply_remote_input(peer, window, key(SHIFT, false)));
    h.pump();
    assert_eq!(h.client.text, "ooo");

    // The person at the machine types: clients repeat its keys again.
    let mut frame = shell::compositor::frame::FrameBuilder::new((1600, 900));
    for pressed in [true, false] {
        let _ = shell::compositor::client_input::route_input(
            &mut h.state,
            &mut frame,
            &local_key(winit::keyboard::KeyCode::KeyA, pressed),
            &Modifiers::default(),
        );
    }
    h.pump();
    assert_eq!(h.client.repeat_rate, Some(configured));
}

#[test]
fn headless_workspace_host_focus_reaches_the_requested_wayland_window() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();
    let mut windows = Vec::new();
    for app in ["org.example.Browser", "org.example.Writer"] {
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app.into());
        windows.push((surface, xdg, toplevel));
    }
    h.pump();
    let old = foreign_id(&h.state, "org.example.Browser");
    let current = foreign_id(&h.state, "org.example.Writer");
    let host = 9;
    for surface in [old, current] {
        h.state
            .share_registry
            .insert(host, surface, StreamingShare::new());
    }
    h.state
        .set_headless_host_viewers([(host, old), (host, current)].into_iter().collect());
    h.state.set_client_keyboard_focus(Some(old));
    h.state.remote_input_firewall.publish_view(ShellView::new(
        vec![
            (old, Rect::new(0.0, 0.0, 400.0, 600.0)),
            (current, Rect::new(400.0, 0.0, 800.0, 600.0)),
        ],
        Some(old),
        false,
        false,
    ));
    assert!(h.state.apply_remote_input(
        host,
        old,
        InputEvent::KeyboardFocus {
            surface: old,
            focused: true
        }
    ));
    assert!(h.state.apply_remote_input(host, old, key(SHIFT, true)));
    assert!(h.state.apply_remote_input(
        host,
        current,
        InputEvent::KeyboardFocus {
            surface: current,
            focused: true
        }
    ));
    h.pump();
    assert_eq!(
        h.client.focus.as_ref(),
        Some(&windows[1].0),
        "the accepted headless host focus must enter the window it names"
    );
    assert!(
        !h.client.shift,
        "held modifiers from the previous window must be retired"
    );
    assert!(h.state.apply_remote_input(
        host,
        old,
        InputEvent::KeyboardFocus {
            surface: old,
            focused: false
        }
    ));
    let text = "MiXeD aaBB1122,!?";
    for c in text.chars() {
        let (code, shift) = stroke(c);
        if shift {
            assert!(h.state.apply_remote_input(host, current, key(SHIFT, true)));
        }
        assert!(h.state.apply_remote_input(host, current, key(code, true)));
        assert!(h.state.apply_remote_input(host, current, key(code, false)));
        if shift {
            assert!(h.state.apply_remote_input(host, current, key(SHIFT, false)));
        }
    }
    h.pump();
    assert_eq!(
        h.client.text, text,
        "the new window receives each stroke exactly"
    );
    assert_host_focus_permissions(&mut h, [old, current], &windows[1].0, host);
}

fn assert_host_focus_permissions(
    h: &mut Harness,
    windows: [u64; 2],
    expected_focus: &wl_surface::WlSurface,
    host: u64,
) {
    let [old, current] = windows;
    let ordinary = 10;
    h.state
        .share_registry
        .insert(ordinary, old, StreamingShare::new());
    assert!(h.state.apply_remote_input(
        ordinary,
        old,
        InputEvent::KeyboardFocus {
            surface: old,
            focused: true
        }
    ));
    assert!(
        !h.state.apply_remote_input(ordinary, old, key(30, true)),
        "a regular share cannot move the local seat to another window"
    );
    h.pump();
    assert_eq!(h.client.focus.as_ref(), Some(expected_focus));

    let read_only = 11;
    let mut share = StreamingShare::new();
    share.read_only = true;
    h.state.share_registry.insert(read_only, old, share);
    let unshared = 12;
    let hidden = 999;
    h.state
        .share_registry
        .insert(host, hidden, StreamingShare::new());
    h.state.set_headless_host_viewers(
        [
            (host, old),
            (host, current),
            (host, hidden),
            (read_only, old),
            (unshared, old),
        ]
        .into_iter()
        .collect(),
    );
    for (peer, surface) in [(read_only, old), (unshared, old), (host, hidden)] {
        assert!(
            !h.state.apply_remote_input(
                peer,
                surface,
                InputEvent::KeyboardFocus {
                    surface,
                    focused: true
                }
            ),
            "headless host focus still requires write permission and a visible share"
        );
    }
    h.pump();
    assert_eq!(h.client.focus.as_ref(), Some(expected_focus));

    h.state.remote_input_firewall.publish_view(ShellView::new(
        vec![
            (old, Rect::new(0.0, 0.0, 400.0, 600.0)),
            (current, Rect::new(400.0, 0.0, 800.0, 600.0)),
        ],
        Some(current),
        true,
        false,
    ));
    assert!(h.state.apply_remote_input(
        host,
        old,
        InputEvent::KeyboardFocus {
            surface: old,
            focused: true
        }
    ));
    assert!(
        !h.state.apply_remote_input(host, old, key(30, true)),
        "trusted chrome still mutes headless-host typing"
    );
    h.pump();
    assert_eq!(h.client.focus.as_ref(), Some(expected_focus));
}

#[derive(Debug, PartialEq, Eq)]
enum PresentationSeen {
    Discarded,
    Presented(std::time::Duration, u64, u32, u32),
}

impl Dispatch<wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation, ()>
    for Client
{
    fn event(
        state: &mut Self,
        _: &wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation,
        event: wayland_protocols::wp::presentation_time::client::wp_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::wp::presentation_time::client::wp_presentation::Event::ClockId {
            clk_id,
        } = event
        {
            state.presentation_clock = Some(clk_id);
        }
    }
}

impl Dispatch<wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::WpPresentationFeedback, u32> for Client {
    fn event(state: &mut Self, _: &wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::WpPresentationFeedback,
        event: wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::Event,
        tag: &u32, _: &Connection, _: &QueueHandle<Self>) {
        use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::Event;
        let outcome = match event {
            Event::Discarded => PresentationSeen::Discarded,
            Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, seq_hi, seq_lo, flags, refresh, .. } => {
                let seconds = u64::from(tv_sec_hi).wrapping_shl(32) | u64::from(tv_sec_lo);
                let sequence = u64::from(seq_hi).wrapping_shl(32) | u64::from(seq_lo);
                PresentationSeen::Presented(std::time::Duration::new(seconds, tv_nsec), sequence, match flags { wayland_client::WEnum::Value(flags) => flags.bits(), wayland_client::WEnum::Unknown(_) => panic!("known standard hardware presentation flags") }, refresh)
            }
            _ => return,
        };
        assert!(state.presentation_seen.insert(*tag, outcome).is_none(), "feedback is one-shot");
    }
}

delegate_noop!(Client: ignore wayland_client::protocol::wl_shm::WlShm);
delegate_noop!(Client: ignore wayland_client::protocol::wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wayland_client::protocol::wl_buffer::WlBuffer);

fn draw_presentation_pixels(id: u64) -> vello::Scene {
    use ui::widget::Widget;
    let theme = ui::Theme::dark();
    let mut widget =
        shell::shell::foreign::ForeignSurfaceWidget::new(id, "org.example.Presentation".into());
    let mut layout = taffy::TaffyTree::<()>::new();
    let node = widget.build_layout(&mut layout, &theme, &mut std::collections::HashMap::new());
    layout
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(32.0),
                height: taffy::AvailableSpace::Definite(32.0),
            },
        )
        .unwrap();
    widget.apply_layout(&layout, node, vello::kurbo::Point::ZERO);
    let mut scene = vello::Scene::new();
    shell::shell::foreign::store().begin_presentation_frame(Rect::new(0.0, 0.0, 32.0, 32.0));
    widget.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut ui::TextRenderer::new(),
        &theme,
        &ui::presence::PresenceCtx::empty(),
    );
    scene
}

fn presentation_output() -> smithay::output::Output {
    smithay::output::Output::new(
        "fixture-panel".into(),
        smithay::output::PhysicalProperties {
            size: (1, 1).into(),
            subpixel: smithay::output::Subpixel::Unknown,
            make: "fixture".into(),
            model: "fixture".into(),
        },
    )
}

fn wait_for_presentation_pixels(h: &mut Harness, id: u64) {
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(5))
        .expect("capture observation deadline fits the monotonic clock");
    loop {
        h.pump();
        if shell::shell::foreign::store()
            .presentation_pixels(id)
            .is_some()
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual committed capture must complete"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn presentation_fixture() -> (
    Harness,
    wl_surface::WlSurface,
    wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation,
    wayland_client::protocol::wl_buffer::WlBuffer,
    wayland_client::protocol::wl_buffer::WlBuffer,
    u64,
) {
    use std::io::Write;
    use std::os::fd::AsFd;
    use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let shm = h.bind::<wayland_client::protocol::wl_shm::WlShm>("wl_shm", 1);
    let presentation = h.bind::<WpPresentation>("wp_presentation", 1);
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_app_id("org.example.Presentation".into());
    surface.commit();
    h.pump();
    let id = foreign_id(&h.state, "org.example.Presentation");
    let mut pixels = tempfile::tempfile().unwrap();
    pixels.write_all(&[0, 0, 255, 255, 0, 255, 0, 255]).unwrap();
    let pool = shm.create_pool(pixels.as_fd(), 8, &qh, ());
    let old = pool.create_buffer(
        0,
        1,
        1,
        4,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    let new = pool.create_buffer(
        4,
        1,
        1,
        4,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    (h, surface, presentation, old, new, id)
}

#[test]
fn presentation_feedback_belongs_to_committed_pixels_and_exact_submitted_frames() {
    let (mut h, surface, presentation, old, new, id) = presentation_fixture();
    let qh = h.queue.handle();
    let _old_feedback = presentation.feedback(&surface, &qh, 0);
    surface.attach(Some(&old), 0, 0);
    surface.commit();
    wait_for_presentation_pixels(&mut h, id);
    let _empty_feedback = presentation.feedback(&surface, &qh, 1);
    surface.commit();
    h.pump();
    let _new_feedback = presentation.feedback(&surface, &qh, 2);
    surface.attach(Some(&new), 0, 0);
    surface.commit();
    wait_for_presentation_pixels(&mut h, id);
    assert_eq!(
        h.client.presentation_clock,
        Some(u32::try_from(libc::CLOCK_MONOTONIC).unwrap())
    );
    let scene = draw_presentation_pixels(id);
    assert!(
        !scene.encoding().is_empty(),
        "production widget composed the committed buffer"
    );
    let frame = h
        .state
        .presentation_feedback
        .composed(shell::shell::foreign::store(), Some(presentation_output()));
    h.state.presentation_feedback.submitted(42, frame);
    let proof = shell::shell::present::PhysicalPresentation {
        timestamp: std::time::Duration::new(987, 123_456_000),
        sequence: 1234,
        refresh: None,
    };
    h.state.presentation_feedback.completed(41, Some(proof));
    h.pump();
    assert_eq!(
        h.client.presentation_seen.get(&0),
        Some(&PresentationSeen::Discarded)
    );
    assert_eq!(
        h.client.presentation_seen.get(&1),
        Some(&PresentationSeen::Discarded)
    );
    assert!(
        !h.client.presentation_seen.contains_key(&2),
        "another frame cannot credit these pixels"
    );
    h.state.presentation_feedback.completed(42, Some(proof));
    h.pump();
    assert_eq!(
        h.client.presentation_seen.get(&2),
        Some(&PresentationSeen::Presented(
            proof.timestamp,
            proof.sequence,
            7,
            0
        ))
    );
    shell::shell::foreign::store().forget(id);
}

#[test]
fn presentation_failures_missing_output_and_timeouts_are_discarded_on_the_wire() {
    for mode in 0..4 {
        let (mut h, surface, presentation, buffer, _, id) = presentation_fixture();
        let _feedback = presentation.feedback(&surface, &h.queue.handle(), 99);
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        wait_for_presentation_pixels(&mut h, id);
        if mode == 3 {
            // The client changed an attached buffer without attaching a newly
            // captured one. Feedback must not certify our older cached pixels.
            let _damaged_feedback = presentation.feedback(&surface, &h.queue.handle(), 100);
            surface.damage_buffer(0, 0, 1, 1);
            surface.commit();
            h.pump();
        }
        let _scene = draw_presentation_pixels(id);
        let output = (mode != 1).then(presentation_output);
        let frame = h
            .state
            .presentation_feedback
            .composed(shell::shell::foreign::store(), output);
        h.state.presentation_feedback.submitted(7, frame);
        match mode {
            0 => h.state.presentation_feedback.completed(7, None),
            1 => h.state.presentation_feedback.completed(
                7,
                Some(shell::shell::present::PhysicalPresentation {
                    timestamp: std::time::Duration::from_secs(123),
                    sequence: 987,
                    refresh: None,
                }),
            ),
            _ => {
                let deadline = h
                    .state
                    .presentation_feedback
                    .next_deadline()
                    .expect("timeout participates in the real idle park");
                h.state.presentation_feedback.expire(deadline);
            }
        }
        h.pump();
        assert_eq!(
            h.client.presentation_seen.get(&99),
            Some(&PresentationSeen::Discarded),
            "failure mode {mode} cannot claim photons"
        );
        if mode == 3 {
            assert_eq!(
                h.client.presentation_seen.get(&100),
                Some(&PresentationSeen::Discarded)
            );
        }
        shell::shell::foreign::store().forget(id);
    }
}
