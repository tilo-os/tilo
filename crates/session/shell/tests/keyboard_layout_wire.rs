//! The real compositor keymap/masks travel through Winit, including several
//! changes queued before the client dispatches them. No logical-key inference.
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use shell::compositor::state::{ClientState, State};
use shell::shell::state::TileContent;
use smithay::input::keyboard::XkbConfig;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::{Display, Resource};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop as ClientLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::wayland::{EventLoopBuilderExtWayland, WindowAttributesExtWayland};
use winit::window::{Window, WindowId};

const CHILD: &str = "TILO_KEYBOARD_LAYOUT_WIRE_CHILD";
const APP: &str = "org.tilo.KeyboardLayoutWire";
const TEST: &str =
    "keyboard_layout_wire::focused_seat_symbols_keep_queued_group_and_modifier_identity";

#[derive(Default)]
struct NativeClient {
    window: Option<Window>,
    next: usize,
}
impl ApplicationHandler for NativeClient {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.window = Some(
            event_loop
                .create_window(Window::default_attributes().with_name(APP, APP))
                .unwrap(),
        );
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let WindowEvent::KeyboardLayoutChanged(Some(layout)) = event else {
            return;
        };
        if self.next == 6 {
            return;
        }
        let symbol = layout
            .keys
            .iter()
            .find(|key| key.physical == PhysicalKey::Code(KeyCode::KeyY))
            .and_then(|key| key.symbols.first())
            .unwrap();
        let expected = [(0, "y"), (0, "Y"), (0, "y"), (1, "z"), (1, "Z"), (1, "z")];
        println!("LAYOUT {} {}", layout.group, symbol.text);
        if (layout.group, symbol.text.as_str()) == expected[self.next] {
            self.next = self
                .next
                .checked_add(1)
                .expect("bounded native transitions");
            if self.next == expected.len() {
                event_loop.exit();
            }
        }
    }
}

fn foreign_id(state: &State) -> Option<u64> {
    state
        .workspace_tree
        .read()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find_map(|tab| match &tab.tile {
            TileContent::Foreign { app_id, surface_id } if app_id == APP => Some(*surface_id),
            _ => None,
        })
}

struct WireChild {
    process: Child,
    messages: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl WireChild {
    fn spawn(socket: &Path, runtime: &Path) -> Self {
        Self::spawn_for(socket, runtime, TEST, CHILD, "1")
    }

    fn spawn_for(socket: &Path, runtime: &Path, test: &str, variable: &str, mode: &str) -> Self {
        let mut process = Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture"])
            .env(variable, mode)
            .env("WAYLAND_DISPLAY", socket)
            .env_remove("WAYLAND_SOCKET")
            .env("XDG_RUNTIME_DIR", runtime)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = process.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            process,
            messages,
            reader: Some(reader),
        }
    }
}

impl Drop for WireChild {
    fn drop(&mut self) {
        // Even a failed assertion must retire the disposable native client.
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn focused_seat_symbols_keep_queued_group_and_modifier_identity() {
    if std::env::var(CHILD).as_deref() == Ok("1") {
        let event_loop = ClientLoop::builder()
            .with_wayland()
            .with_any_thread(true)
            .build()
            .unwrap();
        let mut client = NativeClient::default();
        event_loop.run_app(&mut client).unwrap();
        assert_eq!(
            client.next, 6,
            "the actual native producer must preserve every selected transition"
        );
        return;
    }
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("keyboard-wire.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let event_loop = EventLoop::try_new().unwrap();
    let mut display = Display::new().unwrap();
    let mut state = State::new(event_loop.handle(), display.handle());
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard
        .set_xkb_config(
            &mut state,
            XkbConfig {
                layout: "us,de",
                options: Some("grp:alt_shift_toggle".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let mut child = WireChild::spawn(&socket, runtime.path());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("native wire deadline");
    let mut transcript = Vec::new();
    let mut focused = false;
    let mut sent = false;
    let result = loop {
        if let Ok((stream, _)) = listener.accept() {
            display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        }
        display.dispatch_clients(&mut state).unwrap();
        shell::compositor::foreign::flush_configures(&mut state);
        display.flush_clients().unwrap();
        if !focused && let Some(surface) = foreign_id(&state) {
            state.set_client_keyboard_focus(Some(surface));
            focused = true;
        }
        for line in child.messages.try_iter() {
            if transcript.len() < 32 {
                transcript.push(line.clone());
            }
            if line.ends_with("LAYOUT 0 y") && !sent {
                // One server flush for all transitions: later mutable masks
                // must never relabel an earlier queued client event.
                for (key, pressed) in [
                    (42, true),
                    (42, false),
                    (56, true),
                    (42, true),
                    (42, false),
                    (56, false),
                    (58, true),
                    (58, false),
                    (42, true),
                ] {
                    assert!(state.deliver_client_key(key, pressed, true));
                }
                sent = true;
            }
        }
        if let Some(status) = child.process.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(
        sent,
        "actual Winit focus/keymap was not observable: {transcript:?}"
    );
    assert!(
        result.is_some_and(|status| status.success()),
        "native event wire proof failed or timed out"
    );
}

const POINTER_CHILD: &str = "TILO_OBSERVED_POINTER_WIRE_CHILD";
const POINTER_TEST: &str = "keyboard_layout_wire::observed_pointer_wire_preserves_immutable_origin_and_one_ordinary_delivery";

#[derive(Default)]
struct PointerClient {
    window: Option<Window>,
    observed: bool,
    received: usize,
    anchor: Option<(u32, u32)>,
}

impl ApplicationHandler for PointerClient {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        use winit::platform::wayland::WindowExtWayland;
        let window = event_loop
            .create_window(Window::default_attributes().with_name(APP, APP))
            .unwrap();
        if self.observed {
            assert!(window.observe_pointer_motion(true));
        }
        self.window = Some(window);
        println!("POINTER READY");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let WindowEvent::CursorMovedObserved {
            pointer_id,
            enter_serial,
            motion_ordinal,
            time_ms,
            ..
        } = &event
        {
            assert!(self.observed);
            assert_eq!(*motion_ordinal, u64::try_from(self.received).unwrap());
            assert_eq!(time_ms.is_some(), self.received > 0);
            let anchor = (*pointer_id, *enter_serial);
            assert_eq!(*self.anchor.get_or_insert(anchor), anchor);
        } else if matches!(event, WindowEvent::CursorMoved { .. }) {
            assert!(
                !self.observed,
                "observed mode never duplicates ordinary delivery"
            );
        } else {
            return;
        }
        let ordinary = ui::NativePhysicalInputTrace::ordinary_pointer_event(event);
        let WindowEvent::CursorMoved { position, .. } = ordinary else {
            panic!("each native observed event routes one ordinary cursor movement");
        };
        let expected = [(10.0, 10.0), (11.0, 12.0), (14.0, 15.0)];
        assert_eq!((position.x, position.y), expected[self.received]);
        self.received = self.received.checked_add(1).expect("bounded motion count");
        println!("POINTER MOTION {}", self.received);
        if self.received == expected.len() {
            event_loop.exit();
        }
    }
}

#[test]
fn observed_pointer_wire_preserves_immutable_origin_and_one_ordinary_delivery() {
    if let Ok(mode) = std::env::var(POINTER_CHILD) {
        let event_loop = ClientLoop::builder()
            .with_wayland()
            .with_any_thread(true)
            .build()
            .unwrap();
        let mut client = PointerClient {
            observed: mode == "observed",
            ..Default::default()
        };
        event_loop.run_app(&mut client).unwrap();
        assert_eq!(client.received, 3);
        assert_eq!(client.anchor.is_some(), client.observed);
        return;
    }
    for mode in ["ordinary", "observed"] {
        pointer_wire_mode(mode);
    }
}

fn pointer_wire_mode(mode: &str) {
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("pointer-wire.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let event_loop = EventLoop::try_new().unwrap();
    let mut display = Display::new().unwrap();
    let mut state = State::new(event_loop.handle(), display.handle());
    let mut child =
        WireChild::spawn_for(&socket, runtime.path(), POINTER_TEST, POINTER_CHILD, mode);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("native wire deadline");
    let mut ready = false;
    let mut sent = false;
    let mut transcript = Vec::new();
    let result = loop {
        if let Ok((stream, _)) = listener.accept() {
            display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        }
        display.dispatch_clients(&mut state).unwrap();
        shell::compositor::foreign::flush_configures(&mut state);
        display.flush_clients().unwrap();
        for line in child.messages.try_iter() {
            ready |= line.ends_with("POINTER READY");
            if transcript.len() < 32 {
                transcript.push(line);
            }
        }
        if ready
            && !sent
            && let Some(surface_id) = foreign_id(&state)
        {
            let client = state
                .client_input
                .surface(surface_id)
                .and_then(Resource::client);
            if client.is_some_and(|client| {
                state
                    .seat
                    .get_pointer()
                    .unwrap()
                    .client_pointers(&client)
                    .next()
                    .is_some()
            }) {
                for point in [(10.0, 10.0), (11.0, 12.0), (14.0, 15.0)] {
                    state.forward_client_pointer_motion(point, Some((surface_id, (0.0, 0.0))));
                }
                sent = true;
            }
        }
        if let Some(status) = child.process.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(
        sent,
        "the actual native pointer was not observable: {transcript:?}"
    );
    assert!(
        result.is_some_and(|status| status.success()),
        "native {mode} pointer wire failed: {transcript:?}"
    );
}
