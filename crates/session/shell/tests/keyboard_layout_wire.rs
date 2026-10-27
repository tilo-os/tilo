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
use smithay::reexports::wayland_server::Display;
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
        let mut process = Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture"])
            .env(CHILD, "1")
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
    let deadline = tilo_test_env::hang_deadline();
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
