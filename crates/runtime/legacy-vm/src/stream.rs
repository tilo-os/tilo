//! `tilo-legacy relay-stream`: one legacy app's windows as an encoded
//! stream, for a relay's KVM worker.
//!
//! A worker has no screen. It runs the user's headless Tilo shell (the same
//! `shell --backend headless` a hosted machine runs) as the compositor the
//! legacy VM's windows connect to, and drives it through its agent-control
//! socket: the foreign-frame inventory says when the app's window painted,
//! a window screenshot gives its pixels, which are encoded as H.264 and
//! written as [`LegacyFrame`]s on stdout. [`WindowInput`]s read from stdin
//! go back through the same socket as pointer and keyboard input aimed at
//! that window. The app itself runs through `tilo-legacy run`, whose exit is
//! the stream's end.
//!
//! Frames on both pipes are [`FRAMING`] frames carrying postcard values.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::environment::{LegacyFrame, Refusal, WindowInput};
use tilo_relay_protocol::legacy_window::{self, Frame, InputEvent, Stream};
use wire_frame::Framing;

/// Both pipes: little-endian length prefix, postcard payload.
pub const FRAMING: Framing =
    Framing::little_endian(tilo_relay_protocol::environment::LEGACY_PIPE_FRAME_BYTES);

/// The headless screen the worker's shell lays windows out on.
pub const SCREEN: (u32, u32) = (1600, 900);

/// How often the app's window is looked at.
const FRAME_INTERVAL: Duration = Duration::from_millis(80);

/// The agent-control socket of a headless shell.
pub struct AgentControl {
    socket: PathBuf,
}

impl AgentControl {
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Send one command, optionally aimed at `window`, and return its
    /// answer's `ok`/payload object.
    pub fn send(&self, mut command: Value, window: Option<&str>) -> Result<Value, String> {
        if let (Some(window), Some(object)) = (window, command.as_object_mut()) {
            object.insert("window".into(), Value::String(window.to_string()));
        }
        let mut stream = UnixStream::connect(&self.socket).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        let mut line = serde_json::to_vec(&command).map_err(|e| e.to_string())?;
        line.push(b'\n');
        stream.write_all(&line).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream)
            .take(64 * 1024 * 1024)
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?;
        let value: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        if let Some(error) = value.get("err") {
            return Err(error.to_string());
        }
        Ok(value)
    }

    fn target_names(&self) -> Vec<(String, [f64; 4])> {
        let Ok(value) = self.send(json!({"op": "list_targets"}), None) else {
            return Vec::new();
        };
        value
            .get("targets")
            .and_then(Value::as_array)
            .map(|targets| {
                targets
                    .iter()
                    .filter_map(|target| {
                        let name = target.get("name")?.as_str()?.to_string();
                        let rect = target.get("rect")?.as_array()?;
                        let mut out = [0.0; 4];
                        for (slot, value) in out.iter_mut().zip(rect) {
                            *slot = value.as_f64()?;
                        }
                        Some((name, out))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn click_target(&self, name: &str) -> bool {
        let Some((_, [x, y, w, h])) = self
            .target_names()
            .into_iter()
            .find(|(target, _)| target == name)
        else {
            return false;
        };
        self.send(
            json!({"op": "click_at", "x": x + w / 2.0, "y": y + h / 2.0}),
            None,
        )
        .is_ok()
    }
}

fn wait_for<T>(deadline: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let started = Instant::now();
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if started.elapsed() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The user's headless shell, started when it is not running yet. Its
/// agent-control socket and Wayland display live in `runtime`.
pub fn ensure_headless_shell(
    shell: &Path,
    runtime: &Path,
    log: &Path,
) -> Result<AgentControl, String> {
    let socket = runtime.join("agent-control.sock");
    let control = AgentControl::new(socket.clone());
    let display = runtime.join("tilo-session/wayland-display");
    if control.send(json!({"op": "foreign_frames"}), None).is_ok() && display.exists() {
        return Ok(control);
    }
    std::fs::create_dir_all(runtime).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(shell);
    command
        .args(["--mode", "native", "--backend", "headless"])
        .env("XDG_RUNTIME_DIR", runtime)
        .env("TILO_AGENT_CONTROL", &socket)
        .env("TILO_HEADLESS_SIZE", format!("{}x{}", SCREEN.0, SCREEN.1))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // SAFETY: `setsid` is async-signal-safe; the shell outlives this launch.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|e| format!("start the headless shell: {e}"))?;
    wait_for(Duration::from_secs(120), || {
        control.send(json!({"op": "foreign_frames"}), None).ok()
    })
    .ok_or("the headless shell did not answer on its agent-control socket")?;
    complete_onboarding(&control);
    wait_for(Duration::from_secs(60), || display.exists().then_some(()))
        .ok_or("the headless shell published no Wayland display")?;
    Ok(control)
}

/// A fresh profile opens on the first-run screens; walk through them the
/// way a person would, so windows land on a desktop.
fn complete_onboarding(control: &AgentControl) {
    let names = || {
        control
            .target_names()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    };
    let Some(first) = wait_for(Duration::from_secs(60), || {
        let names = names();
        (!names.is_empty()).then_some(names)
    }) else {
        return;
    };
    if !first.iter().any(|name| name == "onboarding.begin.button") {
        return;
    }
    control.click_target("onboarding.begin.button");
    wait_for(Duration::from_secs(30), || {
        names()
            .iter()
            .any(|name| name == "onboarding.create.button")
            .then_some(())
    });
    // The consent checkbox has no target of its own; its place on the fixed
    // headless screen is known.
    let _ = control.send(json!({"op": "click_at", "x": 480, "y": 618}), None);
    control.click_target("onboarding.create.button");
    wait_for(Duration::from_secs(30), || {
        names()
            .iter()
            .any(|name| name == "onboarding.finish.button")
            .then_some(())
    });
    control.click_target("onboarding.finish.button");
    wait_for(Duration::from_secs(30), || {
        (!names()
            .iter()
            .any(|name| name == "onboarding.finish.button"))
        .then_some(())
    });
}

/// The legacy window currently on screen: (surface, app id, frame seq).
fn legacy_window(control: &AgentControl) -> Option<(u64, String, u64)> {
    let value = control.send(json!({"op": "foreign_frames"}), None).ok()?;
    let frames = value
        .get("foreign_frames")
        .and_then(|frames| frames.get("frames"))
        .and_then(Value::as_array)?;
    frames.iter().rev().find_map(|frame| {
        let app = frame.get("app_id")?.as_str()?;
        legacy_agent_protocol::window::split_host_app_id(app)?;
        let seq = frame.get("frame_seq")?.as_u64()?;
        (seq > 0).then(|| {
            (
                frame.get("surface_id").and_then(Value::as_u64).unwrap_or(0),
                app.to_string(),
                seq,
            )
        })
    })
}

/// The window's pixels, cropped to even dimensions (H.264 needs them).
fn window_rgba(control: &AgentControl, app: &str, scratch: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let _ = std::fs::remove_file(scratch);
    control
        .send(
            json!({"op": "snapshot", "path": scratch.display().to_string()}),
            Some(app),
        )
        .ok()?;
    wait_for(Duration::from_secs(5), || scratch.exists().then_some(()))?;
    let image =
        image::load_from_memory_with_format(&std::fs::read(scratch).ok()?, image::ImageFormat::Png)
            .ok()?
            .into_rgba8();
    let (width, height) = (image.width() & !1, image.height() & !1);
    if width < 2 || height < 2 {
        return None;
    }
    let cropped = image::imageops::crop_imm(&image, 0, 0, width, height).to_image();
    Some((width, height, cropped.into_raw()))
}

/// One window's encoder, recreated when the window's size changes.
struct WindowEncoder {
    surface: u64,
    width: u32,
    height: u32,
    encoder: tilo_callstream::VideoEncoder,
    seq: u32,
    started: Instant,
}

/// Where frames go: stdout, one [`FRAMING`] frame per [`LegacyFrame`].
type FrameSink = Arc<Mutex<Box<dyn Write + Send>>>;

fn emit(sink: &FrameSink, frame: &LegacyFrame) -> bool {
    let mut out = sink.lock_recover();
    FRAMING.write_value(&mut *out, frame).is_ok() && out.flush().is_ok()
}

fn emit_window(sink: &FrameSink, frame: &Frame) -> bool {
    frame
        .encode()
        .is_ok_and(|bytes| emit(sink, &LegacyFrame::Window(bytes)))
}

fn encode_window(
    sink: &FrameSink,
    current: &mut Option<WindowEncoder>,
    (surface, app): (u64, &str),
    (width, height, rgba): (u32, u32, Vec<u8>),
) -> Result<(), String> {
    let fresh = current
        .as_ref()
        .is_none_or(|enc| enc.surface != surface || enc.width != width || enc.height != height);
    if fresh {
        if current.take().is_some() {
            emit_window(sink, &Frame::End);
        }
        let encoder = tilo_callstream::VideoEncoder::with_options(
            tilo_callstream::StreamCodec::H264,
            width,
            height,
            12,
            2_000_000,
            48,
            true,
        )
        .map_err(|e| format!("no H.264 encoder: {e}"))?;
        emit_window(
            sink,
            &Frame::Start(Stream::new(surface, app, "h264", width, height)),
        );
        *current = Some(WindowEncoder {
            surface,
            width,
            height,
            encoder,
            seq: 0,
            started: Instant::now(),
        });
    }
    let Some(enc) = current.as_mut() else {
        return Ok(());
    };
    if fresh {
        enc.encoder.force_keyframe();
    }
    let units = enc.encoder.encode(&rgba).map_err(|e| e.to_string())?;
    for unit in units {
        let ts_us = u64::try_from(enc.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let key = unit.keyframe || legacy_window::h264_keyframe(&unit.data);
        for chunk in legacy_window::chunks(&unit.data, key, enc.seq, ts_us).unwrap_or_default() {
            if !emit(sink, &LegacyFrame::Window(chunk)) {
                return Err("the worker stopped reading".into());
            }
        }
        enc.seq = enc.seq.wrapping_add(1);
    }
    Ok(())
}

/// The agent-control command for one input event, aimed at the window.
#[must_use]
pub fn input_command(event: &InputEvent) -> Option<Value> {
    Some(match event {
        InputEvent::PointerMotion { x, y } => json!({"op": "pointer_move", "x": x, "y": y}),
        InputEvent::PointerButton { button, pressed } => {
            let kind = match button {
                273 => "right",
                274 => "middle",
                _ => "left",
            };
            json!({"op": "pointer_button", "pressed": pressed, "button": kind})
        }
        InputEvent::PointerAxis { dx, dy } => {
            json!({"op": "scroll", "dx": dx / 15.0, "dy": -dy / 15.0})
        }
        InputEvent::Key {
            keysym, pressed, ..
        } => {
            let key = evdev_key_name(*keysym)?;
            let op = if *pressed { "key_down" } else { "key_up" };
            json!({"op": op, "key": key})
        }
        InputEvent::ClipboardText { text } | InputEvent::Text { text } => {
            json!({"op": "type", "text": text})
        }
        _ => return None,
    })
}

/// The agent-control name of a Linux evdev key code.
#[must_use]
pub fn evdev_key_name(code: u32) -> Option<String> {
    const LETTERS: [(u32, char); 26] = [
        (30, 'a'),
        (48, 'b'),
        (46, 'c'),
        (32, 'd'),
        (18, 'e'),
        (33, 'f'),
        (34, 'g'),
        (35, 'h'),
        (23, 'i'),
        (36, 'j'),
        (37, 'k'),
        (38, 'l'),
        (50, 'm'),
        (49, 'n'),
        (24, 'o'),
        (25, 'p'),
        (16, 'q'),
        (19, 'r'),
        (31, 's'),
        (20, 't'),
        (22, 'u'),
        (47, 'v'),
        (17, 'w'),
        (45, 'x'),
        (21, 'y'),
        (44, 'z'),
    ];
    const DIGITS: [(u32, char); 10] = [
        (11, '0'),
        (2, '1'),
        (3, '2'),
        (4, '3'),
        (5, '4'),
        (6, '5'),
        (7, '6'),
        (8, '7'),
        (9, '8'),
        (10, '9'),
    ];
    const NAMED: [(u32, &str); 16] = [
        (1, "Escape"),
        (14, "Backspace"),
        (15, "Tab"),
        (28, "Enter"),
        (57, " "),
        (12, "-"),
        (13, "="),
        (52, "."),
        (51, ","),
        (53, "/"),
        (42, "Shift"),
        (54, "Shift"),
        (29, "Control"),
        (97, "Control"),
        (56, "Alt"),
        (125, "Super"),
    ];
    const ARROWS: [(u32, &str); 4] = [
        (103, "ArrowUp"),
        (108, "ArrowDown"),
        (105, "ArrowLeft"),
        (106, "ArrowRight"),
    ];
    LETTERS
        .iter()
        .chain(DIGITS.iter())
        .find(|(key, _)| *key == code)
        .map(|(_, ch)| ch.to_string())
        .or_else(|| {
            NAMED
                .iter()
                .chain(ARROWS.iter())
                .find(|(key, _)| *key == code)
                .map(|(_, name)| (*name).to_string())
        })
}

/// Everything `relay-stream` needs.
pub struct StreamArgs {
    pub shell: PathBuf,
    /// This launch's runtime directory (shell sockets, scratch).
    pub runtime: PathBuf,
    /// `tilo-legacy run` and its arguments, which runs the app.
    pub run: Vec<std::ffi::OsString>,
}

/// Run one launch to its end, writing frames to `out` and reading input
/// from `input`. Returns the app's exit code.
pub fn relay_stream(
    args: &StreamArgs,
    out: Box<dyn Write + Send>,
    mut input: Box<dyn Read + Send>,
) -> Option<i32> {
    let sink: FrameSink = Arc::new(Mutex::new(out));
    let refuse = |detail: String| {
        emit(
            &sink,
            &LegacyFrame::Refused {
                reason: Refusal::LaunchFailed,
                detail,
            },
        );
        None
    };
    let control =
        match ensure_headless_shell(&args.shell, &args.runtime, &args.runtime.join("shell.log")) {
            Ok(control) => Arc::new(control),
            Err(error) => return refuse(error),
        };
    let _ = control.send(json!({"op": "watch", "watching": true}), None);
    let mut app = match start_app(args) {
        Ok(app) => app,
        Err(error) => return refuse(error),
    };
    emit(&sink, &LegacyFrame::Started);

    // The window the input is aimed at, once the app has one.
    let target: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (done_tx, done) = mpsc::channel::<()>();
    {
        let control = Arc::clone(&control);
        let target = Arc::clone(&target);
        std::thread::spawn(move || {
            while let Ok(Some(event)) = FRAMING.read_value::<_, WindowInput>(&mut input) {
                let window = target.lock_recover().clone();
                for command in event.events.iter().filter_map(input_command) {
                    let _ = control.send(command, window.as_deref());
                }
            }
            drop(done_tx);
        });
    }
    let scratch = args.runtime.join("window.png");
    let mut encoder: Option<WindowEncoder> = None;
    let mut last_seq = 0;
    let code = loop {
        if let Ok(Some(status)) = app.try_wait() {
            break status.code();
        }
        if matches!(done.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
            // The worker hung up: nobody watches the app any more.
            let _ = app.kill();
            break app.wait().ok().and_then(|status| status.code());
        }
        if let Some((surface, app_id, seq)) = legacy_window(&control) {
            *target.lock_recover() = Some(app_id.clone());
            if seq != last_seq
                && let Some(pixels) = window_rgba(&control, &app_id, &scratch)
            {
                last_seq = seq;
                if let Err(error) = encode_window(&sink, &mut encoder, (surface, &app_id), pixels) {
                    let _ = app.kill();
                    return refuse(error);
                }
            }
        }
        std::thread::sleep(FRAME_INTERVAL);
    };
    if encoder.is_some() {
        emit_window(&sink, &Frame::End);
    }
    emit(&sink, &LegacyFrame::Exited { code });
    code
}

fn start_app(args: &StreamArgs) -> Result<Child, String> {
    let (program, rest) = args.run.split_first().ok_or("no app to run")?;
    let wayland = std::fs::read_to_string(args.runtime.join("tilo-session/wayland-display"))
        .map_err(|e| format!("the headless shell's display: {e}"))?;
    // An absolute display: the VM's forwarder connects to this shell, while
    // the VM's own files stay where every other launch finds them.
    Command::new(program)
        .args(rest)
        .env("WAYLAND_DISPLAY", args.runtime.join(wayland.trim()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("start the app: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_input_becomes_agent_input_aimed_at_the_window() {
        assert_eq!(
            input_command(&InputEvent::PointerMotion { x: 3.0, y: 4.0 }),
            Some(json!({"op": "pointer_move", "x": 3.0, "y": 4.0}))
        );
        assert_eq!(
            input_command(&InputEvent::PointerButton {
                button: 272,
                pressed: true
            }),
            Some(json!({"op": "pointer_button", "pressed": true, "button": "left"}))
        );
        assert_eq!(
            input_command(&InputEvent::Key {
                keysym: 35,
                pressed: true,
                modifiers: 0
            }),
            Some(json!({"op": "key_down", "key": "h"}))
        );
        assert_eq!(
            input_command(&InputEvent::Key {
                keysym: 28,
                pressed: false,
                modifiers: 0
            }),
            Some(json!({"op": "key_up", "key": "Enter"}))
        );
        assert_eq!(
            input_command(&InputEvent::Text {
                text: "日本".into()
            }),
            Some(json!({"op": "type", "text": "日本"}))
        );
        assert_eq!(evdev_key_name(12).as_deref(), Some("-"));
        assert_eq!(evdev_key_name(999), None);
        assert_eq!(
            input_command(&InputEvent::KeyboardFocus {
                surface: 1,
                focused: true
            }),
            None
        );
    }
}
