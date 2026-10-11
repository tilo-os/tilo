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

use shell::network::{Denial, InputEvent, ShellView, StreamingShare};
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, PhysicalKey};

use crate::support::wayland::foreign_id;

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

crate::support::wayland::test_client!(Client);

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

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

type Harness = crate::support::wayland::Harness<Client>;

impl Harness {
    fn pump_extra(
        &mut self,
        conn: &Connection,
        queue: &mut wayland_client::EventQueue<Client>,
        client: &mut Client,
    ) {
        for _ in 0..6 {
            let _ = queue.flush();
            self.pump();
            if let Some(guard) = conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = queue.dispatch_pending(client);
        }
    }
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
    let mut event = ui::synthetic_input::key_event(
        Key::Character("a".into()),
        None,
        if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
    );
    event.physical_key = PhysicalKey::Code(code);
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

#[test]
fn a_remote_participant_types_exactly_its_text_and_repeats_only_as_its_sender_does() {
    let mut h = Harness::new();
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
    assert_hover_keeps_explicit_focus(&mut h, host, old);
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
    assert_blur_and_legacy_hover(&mut h, host, current, text);
    assert_host_focus_permissions(&mut h, [old, current], &windows[1].0, host);
}

fn assert_hover_keeps_explicit_focus(h: &mut Harness, host: u64, old: u64) {
    // A layout shift can move an unfocused streamed window under the
    // stationary pointer. Hover must not replace an explicit keyboard target.
    assert!(
        h.state
            .apply_remote_input(host, old, InputEvent::PointerEnter { surface: old })
    );
    assert!(
        h.state
            .apply_remote_input(host, old, InputEvent::PointerMotion { x: 5.0, y: 6.0 })
    );
    assert!(
        h.state
            .apply_remote_input(host, old, InputEvent::PointerLeave { surface: old })
    );
    assert_eq!(
        h.state.review_remote_input(host, old, key(30, true)),
        Err(Denial::FocusMuted)
    );
}

fn assert_blur_and_legacy_hover(h: &mut Harness, host: u64, current: u64, text: &str) {
    assert!(h.state.apply_remote_input(
        host,
        current,
        InputEvent::KeyboardFocus {
            surface: current,
            focused: false
        }
    ));
    assert!(h.state.apply_remote_input(
        host,
        current,
        InputEvent::PointerEnter { surface: current }
    ));
    assert_eq!(
        h.state.review_remote_input(host, current, key(30, true)),
        Err(Denial::FocusMuted),
        "hover cannot reactivate an explicitly blurred keyboard"
    );
    assert!(h.state.apply_remote_input(
        host,
        current,
        InputEvent::KeyboardFocus {
            surface: current,
            focused: true
        }
    ));
    // A viewer predating KeyboardFocus still uses its original handshake.
    let legacy = 12;
    h.state
        .share_registry
        .insert(legacy, current, StreamingShare::new());
    assert!(h.state.apply_remote_input(
        legacy,
        current,
        InputEvent::PointerEnter { surface: current }
    ));
    assert!(h.state.apply_remote_input(legacy, current, key(30, true)));
    assert!(h.state.apply_remote_input(legacy, current, key(30, false)));
    h.pump();
    assert_eq!(h.client.text, format!("{text}a"));
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

/// Focus diagnostics observe the protocol seat, including differences from
/// the router and text-input focus, without serializing application content.
#[test]
fn seat_focus_metadata_tracks_actual_wayland_focus_and_clearing() {
    use smithay::wayland::text_input::TextInputSeat as _;
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let surface = compositor.create_surface(&h.queue.handle(), ());
    let xdg = wm.get_xdg_surface(&surface, &h.queue.handle(), ());
    let toplevel = xdg.get_toplevel(&h.queue.handle(), ());
    toplevel.set_app_id("org.example.Focus".into());
    toplevel.set_title("private window content must stay private".into());
    h.pump();
    let window = foreign_id(&h.state, "org.example.Focus");
    h.state.set_client_keyboard_focus(Some(window));
    let metadata = h.state.seat_focus_json();
    assert_eq!(metadata["keyboard"]["surface"], window);
    assert_eq!(metadata["keyboard"]["app_id"], "org.example.Focus");
    assert_eq!(metadata["keyboard"]["role"], "toplevel");
    assert_eq!(metadata["routed_keyboard_surface"], window);
    assert!(!metadata.to_string().contains("private window content"));

    // Read the actual text-input handle independently of keyboard routing.
    let protocol_surface = h.state.client_input.surface(window).unwrap().clone();
    h.state.seat.text_input().set_focus(Some(protocol_surface));
    assert_eq!(h.state.seat_focus_json()["text_input"]["surface"], window);
    h.state.seat.get_keyboard().unwrap().set_focus(
        &mut h.state,
        None,
        smithay::utils::SERIAL_COUNTER.next_serial(),
    );
    let metadata = h.state.seat_focus_json();
    assert_eq!(metadata["keyboard"], serde_json::Value::Null);
    assert_eq!(
        metadata["routed_keyboard_surface"], window,
        "actual seat focus must not be inferred from the routing cache"
    );
    h.state.seat.text_input().set_focus(None);
    assert_eq!(
        h.state.seat_focus_json()["text_input"],
        serde_json::Value::Null
    );
    h.state.set_client_keyboard_focus(None);
    assert_eq!(
        h.state.seat_focus_json()["routed_keyboard_surface"],
        serde_json::Value::Null
    );
}

#[test]
fn seat_focus_metadata_tracks_pointer_custody_without_changing_it() {
    use shell::compositor::{client_input::route_input, frame::FrameBuilder};
    use winit::event::MouseButton;

    let (mut h, surface, _presentation, buffer, _next_buffer, window) = presentation_fixture();
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    wait_for_presentation_pixels(&mut h, window);
    let mut frame = FrameBuilder::new((1600, 900));
    frame.set_transient_surfaces(&[(window, Rect::new(500.0, 300.0, 532.0, 332.0))]);
    let modifiers = Modifiers::default();
    let moved = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(516.0, 316.0),
    };
    assert!(route_input(&mut h.state, &mut frame, &moved, &modifiers));
    let metadata = h.state.seat_focus_json();
    assert_eq!(metadata["pointer_observable"], true);
    assert_eq!(metadata["pointer"]["surface"], window);
    assert_eq!(metadata["routed_pointer_surface"], window);
    assert_eq!(metadata["pointer_grab"]["kind"], "none");
    assert_eq!(metadata["pointer_grab"]["surface"], serde_json::Value::Null);
    assert_eq!(metadata["pointer_buttons_down"], 0);

    let button = |state| WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    };
    assert!(route_input(
        &mut h.state,
        &mut frame,
        &button(ElementState::Pressed),
        &modifiers
    ));
    h.state.set_client_keyboard_focus(None);
    let held = h.state.seat_focus_json();
    assert_eq!(held["keyboard"], serde_json::Value::Null);
    assert_eq!(held["pointer"]["surface"], window);
    assert_eq!(held["pointer_grab"]["kind"], "client");
    assert_eq!(held["pointer_grab"]["surface"], window);
    assert_eq!(held["pointer_buttons_down"], 1);
    let pointer = h.state.seat.get_pointer().unwrap();
    let actual_focus = pointer.current_focus();
    assert_eq!(
        h.state.seat_focus_json(),
        held,
        "inspection must not retire a held gesture"
    );
    assert_eq!(pointer.current_focus(), actual_focus);
    assert_eq!(h.state.client_input.pointer_focus(), Some(window));
    assert!(route_input(
        &mut h.state,
        &mut frame,
        &button(ElementState::Released),
        &modifiers
    ));
    assert_eq!(h.state.seat_focus_json()["pointer_grab"]["kind"], "none");
    assert_eq!(h.state.seat_focus_json()["pointer_buttons_down"], 0);

    // The seat can lose focus independently of the router's cached target.
    pointer.motion(
        &mut h.state,
        None,
        &smithay::input::pointer::MotionEvent {
            location: (516.0, 316.0).into(),
            serial: smithay::utils::SERIAL_COUNTER.next_serial(),
            time: 0,
        },
    );
    let metadata = h.state.seat_focus_json();
    assert_eq!(metadata["pointer"], serde_json::Value::Null);
    assert_eq!(metadata["routed_pointer_surface"], window);
    let left = WindowEvent::CursorLeft {
        device_id: winit::event::DeviceId::dummy(),
    };
    assert!(!route_input(&mut h.state, &mut frame, &left, &modifiers));
    frame.set_transient_surfaces(&[]);
    assert!(!route_input(
        &mut h.state,
        &mut frame,
        &button(ElementState::Pressed),
        &modifiers
    ));
    let metadata = h.state.seat_focus_json();
    assert_eq!(metadata["pointer_grab"]["kind"], "chrome");
    assert_eq!(metadata["pointer_grab"]["surface"], serde_json::Value::Null);
    assert_eq!(metadata["pointer_buttons_down"], 1);
    assert!(!route_input(
        &mut h.state,
        &mut frame,
        &button(ElementState::Released),
        &modifiers
    ));
    drop(frame);

    h.state.seat.remove_pointer();
    let metadata = h.state.seat_focus_json();
    assert_eq!(metadata["pointer_observable"], false);
    assert_eq!(metadata["pointer"], serde_json::Value::Null);
    assert_eq!(metadata["routed_pointer_surface"], serde_json::Value::Null);
    assert_eq!(metadata["pointer_grab"], serde_json::Value::Null);
    assert_eq!(metadata["pointer_buttons_down"], serde_json::Value::Null);
}

#[test]
fn a_trusted_osk_toplevel_never_adopts_a_tab_before_its_app_id_arrives() {
    use shell::compositor::input_method::TrustedInputMethod;
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let surface = compositor.create_surface(&h.queue.handle(), ());
    let xdg = wm.get_xdg_surface(&surface, &h.queue.handle(), ());
    let toplevel = xdg.get_toplevel(&h.queue.handle(), ());
    toplevel.set_app_id("org.example.Writer".into());
    h.pump();
    let window = foreign_id(&h.state, "org.example.Writer");
    // Leave a window to the right: removing an anonymous OSK group must
    // not choose that neighbour instead of the person's previous window.
    let neighbour = compositor.create_surface(&h.queue.handle(), ());
    let neighbour_xdg = wm.get_xdg_surface(&neighbour, &h.queue.handle(), ());
    let neighbour_toplevel = neighbour_xdg.get_toplevel(&h.queue.handle(), ());
    neighbour_toplevel.set_app_id("org.example.Reader".into());
    h.pump();
    assert!(shell::compositor::tab_binding::activate_surface(
        &mut h.state.workspace_tree.write(),
        window
    ));
    h.state.set_client_keyboard_focus(Some(window));
    let before = serde_json::to_value(&*h.state.workspace_tree.read()).unwrap();
    let focus = h.state.seat_focus_json();

    let (server, client) = UnixStream::pair().unwrap();
    h.display
        .handle()
        .insert_client(
            server,
            shell::compositor::state::ClientState::input_method_arc(
                TrustedInputMethod::OnScreenKeyboard,
            ),
        )
        .unwrap();
    client.set_nonblocking(true).unwrap();
    let conn = Connection::from_socket(client).unwrap();
    let mut queue = conn.new_event_queue::<Client>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut client = Client::default();
    h.pump_extra(&conn, &mut queue, &mut client);
    let named = |interface: &str| {
        client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .unwrap()
            .0
    };
    let compositor =
        registry.bind::<wl_compositor::WlCompositor, _, _>(named("wl_compositor"), 4, &qh, ());
    let wm = registry.bind::<xdg_wm_base::XdgWmBase, _, _>(named("xdg_wm_base"), 1, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    // Flush the actual new_toplevel request BEFORE setting an app id.
    h.pump_extra(&conn, &mut queue, &mut client);
    assert_eq!(
        serde_json::to_value(&*h.state.workspace_tree.read()).unwrap(),
        before,
        "a trusted keyboard must never be adopted as an anonymous ordinary window"
    );
    assert_eq!(h.state.seat_focus_json()["keyboard"], focus["keyboard"]);
    assert!(
        h.state.osk_surface.is_none(),
        "overlay authority still requires the exact app-id"
    );
    toplevel.set_app_id("so.tilo.osk".into());
    h.pump_extra(&conn, &mut queue, &mut client);
    assert_eq!(
        serde_json::to_value(&*h.state.workspace_tree.read()).unwrap(),
        before
    );
    assert_eq!(h.state.seat_focus_json()["keyboard"], focus["keyboard"]);

    // Merely copying the app-id on a public connection grants no OSK role.
    let public_compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let public_wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let spoof = public_compositor.create_surface(&h.queue.handle(), ());
    let spoof_xdg = public_wm.get_xdg_surface(&spoof, &h.queue.handle(), ());
    let spoof_toplevel = spoof_xdg.get_toplevel(&h.queue.handle(), ());
    spoof_toplevel.set_app_id("so.tilo.osk".into());
    h.pump();
    let public_window = foreign_id(&h.state, "so.tilo.osk");
    assert_ne!(h.state.osk_surface, Some(public_window));
}
