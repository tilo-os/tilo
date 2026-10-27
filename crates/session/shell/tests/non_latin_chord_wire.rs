//! A chord typed on a non-Latin source names the key the person's own Latin
//! arrangement puts there, through the real compositor keymap and real Winit.
//! With German and Russian configured and Russian typing, Ctrl and the key
//! labelled Z (a US Y) is undo, not Ctrl+Y as a US position would make it.
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
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop as ClientLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::platform::wayland::{EventLoopBuilderExtWayland, WindowAttributesExtWayland};
use winit::window::{Window, WindowId};

const CHILD: &str = "TILO_NON_LATIN_CHORD_WIRE_CHILD";
const APP: &str = "org.tilo.NonLatinChordWire";
const TEST: &str = "non_latin_chord_wire::a_chord_on_a_russian_source_keeps_the_german_letter";

/// evdev codes: the key a US keyboard labels Y (German Z) and Z (German Y).
const KEY_US_Y: u32 = 21;
const KEY_US_Z: u32 = 44;
const CTRL: u32 = 29;
const ALT: u32 = 56;
const SHIFT: u32 = 42;

#[derive(Default)]
struct NativeClient {
    window: Option<Window>,
    ready: bool,
    modifiers: ModifiersState,
    /// (logical key, whether the app runner would rename it again)
    pressed: Vec<(String, bool)>,
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
        match &event {
            WindowEvent::KeyboardLayoutChanged(Some(_)) if !self.ready => {
                self.ready = true;
                println!("READY");
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event: key,
                is_synthetic: false,
                ..
            } if key.state == ElementState::Pressed => {
                let Key::Character(logical) = &key.logical_key else {
                    return;
                };
                let renamed_again = ui::latin_chord::renamed(&event, self.modifiers).is_some();
                println!("KEY {logical} {renamed_again}");
                self.pressed.push((logical.to_string(), renamed_again));
                if self.pressed.len() == 3 {
                    event_loop.exit();
                }
            }
            _ => {}
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
fn a_chord_on_a_russian_source_keeps_the_german_letter() {
    if std::env::var(CHILD).as_deref() == Ok("1") {
        let event_loop = ClientLoop::builder()
            .with_wayland()
            .with_any_thread(true)
            .build()
            .unwrap();
        let mut client = NativeClient::default();
        event_loop.run_app(&mut client).unwrap();
        assert_eq!(
            client.pressed,
            [
                ("z".to_string(), false),
                ("y".to_string(), false),
                ("н".to_string(), false),
            ],
            "Ctrl and the German Z key is Ctrl+Z, the German Y key Ctrl+Y, and \
             the same key unmodified types the Russian letter"
        );
        return;
    }
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("non-latin-chord-wire.sock");
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
                layout: "de,ru",
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
            if line.ends_with("READY") && !sent {
                for (key, pressed) in [
                    // Alt+Shift moves to the second source, Russian.
                    (ALT, true),
                    (SHIFT, true),
                    (SHIFT, false),
                    (ALT, false),
                    (CTRL, true),
                    (KEY_US_Y, true),
                    (KEY_US_Y, false),
                    (KEY_US_Z, true),
                    (KEY_US_Z, false),
                    (CTRL, false),
                    (KEY_US_Y, true),
                    (KEY_US_Y, false),
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
        "native chord wire proof failed or timed out: {transcript:?}"
    );
}
