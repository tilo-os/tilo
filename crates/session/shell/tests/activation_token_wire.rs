//! A native app's activation token carries the press it came from, so the
//! compositor hands it out after the person pressed a key in the window and
//! not before: real Smithay, real Winit, no hand-built protocol requests.
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use shell::compositor::state::{ClientState, State};
use shell::shell::state::TileContent;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::xdg_activation::XdgActivationToken;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop as ClientLoop};
use winit::platform::startup_notify::WindowExtStartupNotify;
use winit::platform::wayland::{EventLoopBuilderExtWayland, WindowAttributesExtWayland};
use winit::window::{Window, WindowId};

const CHILD: &str = "TILO_ACTIVATION_TOKEN_WIRE_CHILD";
const APP: &str = "org.tilo.ActivationTokenWire";
const TEST: &str = "activation_token_wire::a_token_carries_the_press_it_was_asked_for_after";

/// Asks for one token once the window has focus, before any press, and one
/// after the first key press; prints each as `TOKEN <when> <token>`.
#[derive(Default)]
struct NativeClient {
    window: Option<Window>,
    asked_before: bool,
    asked_after: bool,
    answered: usize,
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
        let Some(window) = self.window.as_ref() else {
            return;
        };
        match event {
            WindowEvent::Focused(true) if !self.asked_before => {
                self.asked_before = true;
                window.request_activation_token().unwrap();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && self.answered == 1
                    && !self.asked_after =>
            {
                self.asked_after = true;
                window.request_activation_token().unwrap();
            }
            WindowEvent::ActivationTokenDone { token, .. } => {
                let when = if self.answered == 0 {
                    "before"
                } else {
                    "after"
                };
                println!("TOKEN {when} {}", token.into_raw());
                self.answered = self.answered.saturating_add(1);
                if self.answered == 2 {
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

/// Whether the compositor accepted `token`: Smithay answers every request
/// with a token, and keeps only those its policy let through.
fn accepted(state: &State, token: &str) -> bool {
    state
        .xdg_activation_state
        .data_for_token(&XdgActivationToken::from(token.to_owned()))
        .is_some()
}

#[test]
fn a_token_carries_the_press_it_was_asked_for_after() {
    if std::env::var(CHILD).as_deref() == Ok("1") {
        let event_loop = ClientLoop::builder()
            .with_wayland()
            .with_any_thread(true)
            .build()
            .unwrap();
        let mut client = NativeClient::default();
        event_loop.run_app(&mut client).unwrap();
        assert_eq!(client.answered, 2, "both tokens are answered");
        return;
    }
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime.path().join("activation-wire.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let event_loop = EventLoop::try_new().unwrap();
    let mut display = Display::new().unwrap();
    let mut state = State::new(event_loop.handle(), display.handle());
    let mut child = WireChild::spawn(&socket, runtime.path());
    let deadline = tilo_test_env::hang_deadline();
    let mut transcript = Vec::new();
    let mut focused = false;
    let mut before = None;
    let mut after = None;
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
            if let Some(token) = line.strip_prefix("TOKEN before ") {
                before = Some(accepted(&state, token));
                // The person presses a key in the window.
                assert!(state.deliver_client_key(30, true, true));
                assert!(state.deliver_client_key(30, false, true));
            } else if let Some(token) = line.strip_prefix("TOKEN after ") {
                after = Some(accepted(&state, token));
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
        result.is_some_and(|status| status.success()),
        "the native client finished: {transcript:?}"
    );
    assert_eq!(
        before,
        Some(false),
        "a token asked for before any press is refused: {transcript:?}"
    );
    assert_eq!(
        after,
        Some(true),
        "a token asked for after the person's key press is handed out: {transcript:?}"
    );
}
