//! The development test-control socket: `tilo-control` drives a session it was started for
//! (`tilo-session --test-control PATH`, or `$TILO_TEST_CONTROL`, honoured by development builds
//! only, at `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` when neither names one) over the
//! protocol it speaks to the shell's (`shell::shell::test_control`): one JSON
//! request per line, tagged by `op`, and one answer per line, `{"ok": {"frame": N}}` or
//! `{"err": {"code": .., "message": ..}}`.
//!
//! What it answers, as the tools that drive a desktop need it:
//!
//! - `snapshot` (`tilo-control snapshot PATH`): a PNG of every display, each at its place in the
//!   layout, as the portal's screenshot of the screen takes it ([`crate::capture::picture`]);
//! - `provision_test_identity` (`tilo-control raw '{"op":"provision_test_identity"}'`), which
//!   mints the throwaway identity a development session opens its desk with instead of first-use
//!   setup, as the shell's does for a dev image;
//! - `list_targets` and `click_named`: every control the session's hosted surfaces show, by the
//!   name they go by (a tab's pill, a place's switch, `onboarding.begin.button`), and a press of
//!   one of them through the same action route an assistive technology activates it with — never
//!   a point aimed at;
//! - the pointer: `pointer_move`, `pointer_button` (left, right or middle), `click_at` and
//!   `scroll`;
//! - `drag`: a press at one place, interpolated motion, and the release at another, which is how
//!   a tab moves into another group or onto another strip;
//! - the keyboard: `key`, `key_down`/`key_up` (a held modifier across a keystroke) and `key`'s
//!   `modifiers` mask, and `type`, each character typed by the key the seat's layout puts it on,
//!   or written as text where the layout has no key for it;
//! - `state_dump` and `windows`: the desk as it is now, and one row per open window.
//!
//! Each connection is read on a thread of its own; what it asks is answered on the loop's pass,
//! the only place that may touch the compositor, and the loop is woken for it.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value as Json, json};
use tilo_compositor::{Compositor, InputEvent};
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_surface::DesktopHandle;
use tilo_service::Wake;
use tilo_surface_host::Doors;

use crate::engine::Engine;

/// The variable `tilo-control` reads the test-control socket's path from: what a development
/// session binds the socket at when its command line names none and the default is not wanted,
/// and what it prints with the path it opened.
pub const ENV: &str = "TILO_TEST_CONTROL";

/// Whether this build opens the test-control socket it is given: development builds, and the
/// dev image's, whose binaries are release ones built with `dev-overrides`. A production build
/// leaves the flag without effect, so nothing it ships drives a session as the person through a
/// socket.
pub const OPENED_BY_THIS_BUILD: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// Where the socket is opened, given whether this build opens one (`opens`,
/// [`OPENED_BY_THIS_BUILD`]) and the path the command line named: `None` in a build that opens
/// none, whatever was named; the named path, else `$TILO_TEST_CONTROL`'s, else the default
/// `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` the tools that drive a desktop go to —
/// what a dev image's login binds with nothing named, so `tilo-vmd` finds it where it looks.
#[must_use]
pub fn path(opens: bool, named: Option<&str>) -> Option<PathBuf> {
    path_in(
        opens,
        named,
        std::env::var_os(ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
    )
}

/// [`path`] for the values it reads, so what it decides can be checked without the environment.
fn path_in(
    opens: bool,
    named: Option<&str>,
    env: Option<OsString>,
    runtime_dir: Option<OsString>,
) -> Option<PathBuf> {
    if !opens {
        return None;
    }
    let named = named
        .map(PathBuf::from)
        .or_else(|| env.map(PathBuf::from))
        .filter(|path| !path.as_os_str().is_empty());
    named.or_else(|| {
        tilo_paths::login::session_dir(runtime_dir)
            .map(|directory| directory.join(tilo_paths::login::TEST_CONTROL_SOCKET_FILE))
    })
}

/// The socket at `path`, when one is named, opened; `None`, saying why, when
/// it does not open.
pub(crate) fn open_named(path: Option<&Path>, wake: Wake) -> Option<TestControl> {
    let path = path?;
    match TestControl::open(path, wake) {
        Ok(control) => Some(control),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the test-control socket does not open");
            None
        }
    }
}

/// What the session is, for the answers that read it: the doors every hosted surface's actions
/// go through, the desk's tree, what stands over it, the workspaces this computer holds, and
/// whether the person locked the screen.
#[derive(Clone, Copy)]
pub(crate) struct Desk<'a> {
    pub(crate) doors: &'a Doors,
    pub(crate) handle: &'a DesktopHandle,
    pub(crate) overlays: &'a Overlays,
    pub(crate) engine: Option<&'a Engine>,
    pub(crate) locked: bool,
}

/// Answer what `control`, when there is one, was asked since the last pass.
pub(crate) fn serve(control: Option<&TestControl>, compositor: &mut Compositor, desk: Desk<'_>) {
    if let Some(control) = control {
        control.serve(compositor, &desk);
    }
}

/// One request a connection sent, and where its answer goes.
struct Asked {
    request: String,
    answer: Sender<String>,
}

/// The socket, served on the loop's pass.
pub(crate) struct TestControl {
    listener: UnixListener,
    path: PathBuf,
    asks: Sender<Asked>,
    asked: Receiver<Asked>,
    wake: Wake,
    /// The keys a `key_down` holds, so `key_up` releases them and `modifiers` knows what to
    /// press and release to reach the mask it names.
    held: RefCell<BTreeSet<u32>>,
}

impl TestControl {
    /// Open the socket at `path`, for this account alone; `wake` wakes the loop when a
    /// connection asks something.
    ///
    /// # Errors
    /// The path is not a socket, another session answers on it, or it cannot be bound.
    pub(crate) fn open(path: &Path, wake: Wake) -> io::Result<Self> {
        let listener = crate::agent_door::bind_private(path)?;
        listener.set_nonblocking(true)?;
        let (asks, asked) = channel();
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            asks,
            asked,
            wake,
            held: RefCell::new(BTreeSet::new()),
        })
    }

    /// Take the connections waiting and answer what was asked since the last pass.
    pub(crate) fn serve(&self, compositor: &mut Compositor, desk: &Desk<'_>) {
        self.accept();
        while let Ok(Asked { request, answer }) = self.asked.try_recv() {
            let _ = answer.send(answered(&request, compositor, desk, &self.held).to_string());
        }
    }

    /// Accept every waiting connection, each read on its own thread.
    fn accept(&self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    tracing::warn!(%error, "test control: accepting a connection failed");
                    return;
                }
            };
            let asks = self.asks.clone();
            let wake = std::sync::Arc::clone(&self.wake);
            let spawned = std::thread::Builder::new()
                .name("tilo-test-control".into())
                .spawn(move || serve_connection(stream, &asks, &wake));
            if let Err(error) = spawned {
                tracing::warn!(%error, "test control: a connection could not be served");
            }
        }
    }
}

impl Drop for TestControl {
    fn drop(&mut self) {
        // Nothing answers there once the session is gone.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read `stream`'s requests one line at a time, hand each to the loop and write its answer,
/// until the client closes it or the session ends.
fn serve_connection(stream: UnixStream, asks: &Sender<Asked>, wake: &Wake) {
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::debug!(%error, "test control: a connection cannot block");
        return;
    }
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(request) = line else {
            return;
        };
        if request.trim().is_empty() {
            continue;
        }
        let (answer, answered) = channel();
        if asks.send(Asked { request, answer }).is_err() {
            return;
        }
        wake();
        // The loop answers every request it takes; a session that ends drops it unanswered.
        let Ok(reply) = answered.recv() else {
            return;
        };
        if writer.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return;
        }
    }
}

/// The answer to `request`, run against `compositor`.
/// The answer to `request`, run against `compositor`.
fn answered(
    request: &str,
    compositor: &mut Compositor,
    desk: &Desk<'_>,
    held: &RefCell<BTreeSet<u32>>,
) -> Json {
    let request: Json = match serde_json::from_str(request) {
        Ok(request) => request,
        Err(error) => return refusal("parse_error", &error.to_string()),
    };
    let Some(op) = request.get("op").and_then(Json::as_str) else {
        return refusal("parse_error", "a request names its `op`");
    };
    if let Some(answer) = asked_of_desk(op, &request, compositor, desk) {
        return answer;
    }
    if let Some(answer) = asked_of_pointer(op, &request, compositor) {
        return answer;
    }
    if let Some(answer) = asked_of_keys(op, &request, compositor, held) {
        return answer;
    }
    refusal(
        "unsupported",
        &format!(
            "this session's test control answers `snapshot`, `click_at`, `click_named`, \
             `drag`, `key`, `key_down`, `key_up`, `list_targets`, `modifiers`, \
             `pointer_button`, `pointer_move`, `provision_test_identity`, `scroll`, \
             `state_dump`, `type` and `windows`, not `{op}`"
        ),
    )
}

/// The answer to what `op` asks of the desk and its hosted surfaces, when it asks of them.
fn asked_of_desk(
    op: &str,
    request: &Json,
    compositor: &mut Compositor,
    desk: &Desk<'_>,
) -> Option<Json> {
    Some(match op {
        "snapshot" => {
            let path = request
                .get("path")
                .and_then(Json::as_str)
                .unwrap_or_default();
            match snapshot(compositor, path) {
                Ok(()) => json!({"ok": {"frame": 0}}),
                Err((code, message)) => refusal(code, &message),
            }
        }
        "provision_test_identity" => provision_test_identity(),
        "list_targets" => {
            let targets: Vec<Json> = desk
                .doors
                .controls()
                .into_iter()
                .map(|(name, (x, y, w, h))| json!({"name": name, "rect": [x, y, w, h]}))
                .collect();
            json!({"targets": targets})
        }
        "click_named" => {
            let Some(name) = request.get("name").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`click_named` takes `name`"));
            };
            match desk.doors.press(name) {
                Ok(()) => json!({"ok": {}}),
                Err(error) => refusal("unknown_target", &error),
            }
        }
        "state_dump" => desk.handle.read(|tree| state_dump(tree, desk)),
        "windows" => desk
            .handle
            .read(|tree| json!({"answer": {"windows": window_rows(tree)}})),
        _ => return None,
    })
}

/// The answer to what `op` asks of the pointer, when it asks of it.
fn asked_of_pointer(op: &str, request: &Json, compositor: &mut Compositor) -> Option<Json> {
    Some(match op {
        "click_at" => {
            let (Some(x), Some(y)) = (number(request, "x"), number(request, "y")) else {
                return Some(refusal("parse_error", "`click_at` takes `x` and `y`"));
            };
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return Some(refusal),
            };
            if let Err(error) = compositor.input(InputEvent::PointerMotion { x, y }) {
                return Some(refusal("input_failed", &error.to_string()));
            }
            tap(compositor, button)
        }
        "pointer_move" => {
            let (Some(x), Some(y)) = (number(request, "x"), number(request, "y")) else {
                return Some(refusal("parse_error", "`pointer_move` takes `x` and `y`"));
            };
            answer(compositor.input(InputEvent::PointerMotion { x, y }))
        }
        "pointer_button" => {
            let Some(pressed) = request.get("pressed").and_then(Json::as_bool) else {
                return Some(refusal("parse_error", "`pointer_button` takes `pressed`"));
            };
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return Some(refusal),
            };
            answer(compositor.input(InputEvent::PointerButton { button, pressed }))
        }
        "drag" => {
            let (Some(x_from), Some(y_from), Some(x_to), Some(y_to)) = (
                number(request, "x_from"),
                number(request, "y_from"),
                number(request, "x_to"),
                number(request, "y_to"),
            ) else {
                return Some(refusal(
                    "parse_error",
                    "`drag` takes `x_from`, `y_from`, `x_to` and `y_to`",
                ));
            };
            let steps = request
                .get("steps")
                .and_then(Json::as_u64)
                .unwrap_or(8)
                .clamp(1, 1_000);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a drag's steps are clamped to a thousand"
            )]
            let steps = steps as u32;
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return Some(refusal),
            };
            match drag(compositor, button, (x_from, y_from), (x_to, y_to), steps) {
                Ok(()) => json!({"ok": {}}),
                Err(error) => refusal("input_failed", &error),
            }
        }
        "scroll" => {
            let dx = number(request, "dx").unwrap_or(0.0);
            let dy = number(request, "dy").unwrap_or(0.0);
            // A notch up is the old wire's positive `dy`; the compositor speaks the protocol's
            // axis, which counts down as positive.
            answer(compositor.input(InputEvent::PointerAxis {
                horizontal: -dx,
                vertical: -dy,
            }))
        }
        _ => return None,
    })
}

/// The answer to what `op` asks of the keyboard, when it asks of it.
fn asked_of_keys(
    op: &str,
    request: &Json,
    compositor: &mut Compositor,
    held: &RefCell<BTreeSet<u32>>,
) -> Option<Json> {
    Some(match op {
        "key" => match key_code(request, compositor) {
            Some(code) => {
                for pressed in [true, false] {
                    if let Err(error) = compositor.input(InputEvent::Key {
                        keycode: code,
                        pressed,
                    }) {
                        return Some(refusal("input_failed", &error.to_string()));
                    }
                }
                json!({"ok": {}})
            }
            None => refusal(
                "parse_error",
                "`key` takes a named key or one character the keyboard types",
            ),
        },
        "key_down" => match key_code(request, compositor) {
            Some(code) => {
                if let Err(error) = compositor.input(InputEvent::Key {
                    keycode: code,
                    pressed: true,
                }) {
                    return Some(refusal("input_failed", &error.to_string()));
                }
                held.borrow_mut().insert(code);
                json!({"ok": {}})
            }
            None => refusal("parse_error", "`key_down` takes a named key"),
        },
        "key_up" => match key_code(request, compositor) {
            Some(code) => {
                if let Err(error) = compositor.input(InputEvent::Key {
                    keycode: code,
                    pressed: false,
                }) {
                    return Some(refusal("input_failed", &error.to_string()));
                }
                held.borrow_mut().remove(&code);
                json!({"ok": {}})
            }
            None => refusal("parse_error", "`key_up` takes a named key"),
        },
        "modifiers" => {
            let wanted = [
                (KEY_CTRL, request.get("ctrl").and_then(Json::as_bool)),
                (KEY_SHIFT, request.get("shift").and_then(Json::as_bool)),
                (KEY_ALT, request.get("alt").and_then(Json::as_bool)),
                (KEY_SUPER, request.get("logo").and_then(Json::as_bool)),
            ];
            for (code, pressed) in wanted {
                let pressed = pressed.unwrap_or(false);
                let was = held.borrow().contains(&code);
                if pressed == was {
                    continue;
                }
                if let Err(error) = compositor.input(InputEvent::Key {
                    keycode: code,
                    pressed,
                }) {
                    return Some(refusal("input_failed", &error.to_string()));
                }
                if pressed {
                    held.borrow_mut().insert(code);
                } else {
                    held.borrow_mut().remove(&code);
                }
            }
            json!({"ok": {}})
        }
        "type" => {
            let Some(text) = request.get("text").and_then(Json::as_str) else {
                return Some(refusal("parse_error", "`type` takes `text`"));
            };
            match type_text(compositor, text) {
                Ok(()) => json!({"ok": {}}),
                Err(error) => refusal("input_failed", &error),
            }
        }
        _ => return None,
    })
}

/// `request`'s `name` field, when it is a number.
fn number(request: &Json, name: &str) -> Option<f64> {
    request.get(name).and_then(Json::as_f64)
}

/// The button `request`'s `name` field names: `left` unless it says `right` or `middle`.
fn button(request: &Json, name: &str) -> Result<u32, Json> {
    match request.get(name).and_then(Json::as_str) {
        None | Some("left") => Ok(BUTTON_LEFT),
        Some("right") => Ok(BUTTON_RIGHT),
        Some("middle") => Ok(BUTTON_MIDDLE),
        Some(other) => Err(refusal(
            "parse_error",
            &format!("`{name}` is `left`, `right` or `middle`, not `{other}`"),
        )),
    }
}

/// The evdev codes the pointer's buttons go by.
const BUTTON_LEFT: u32 = 0x110;
const BUTTON_RIGHT: u32 = 0x111;
const BUTTON_MIDDLE: u32 = 0x112;

/// The evdev codes of the keys a `modifiers` request holds.
const KEY_CTRL: u32 = 29;
const KEY_SHIFT: u32 = 42;
const KEY_ALT: u32 = 56;
const KEY_SUPER: u32 = 125;

/// `ok` when the input took `result`, its refusal otherwise.
fn answer(result: Result<(), tilo_compositor::Error>) -> Json {
    match result {
        Ok(()) => json!({"ok": {}}),
        Err(error) => refusal("input_failed", &error.to_string()),
    }
}

/// One place of a drag: `from` to `to`, `at` of the way.
fn drag_step(from: (f64, f64), to: (f64, f64), at: f64) -> (f64, f64) {
    (from.0 + (to.0 - from.0) * at, from.1 + (to.1 - from.1) * at)
}

/// Press and release `button` where the pointer is, as the person clicks there.
fn tap(compositor: &mut Compositor, button: u32) -> Json {
    for pressed in [true, false] {
        if let Err(error) = compositor.input(InputEvent::PointerButton { button, pressed }) {
            return refusal("input_failed", &error.to_string());
        }
    }
    json!({"ok": {}})
}

/// A press at `from`, `steps` motions spread evenly to `to`, and the release there: the gesture
/// that moves a tab into another group, or a file into a window.
fn drag(
    compositor: &mut Compositor,
    button: u32,
    from: (f64, f64),
    to: (f64, f64),
    steps: u32,
) -> Result<(), String> {
    // The press lands where the pointer is, so it is taken to where the gesture starts.
    compositor
        .input(InputEvent::PointerMotion {
            x: from.0,
            y: from.1,
        })
        .map_err(|error| error.to_string())?;
    compositor
        .input(InputEvent::PointerButton {
            button,
            pressed: true,
        })
        .map_err(|error| error.to_string())?;
    for step in 1..=steps {
        let at = f64::from(step) / f64::from(steps);
        let (x, y) = drag_step(from, to, at);
        compositor
            .input(InputEvent::PointerMotion { x, y })
            .map_err(|error| error.to_string())?;
    }
    compositor
        .input(InputEvent::PointerButton {
            button,
            pressed: false,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The evdev code a request's `key` means: a named key's, or, for one character, the key this
/// seat's layout puts it on.
fn key_code(request: &Json, compositor: &mut Compositor) -> Option<u32> {
    let key = request.get("key").and_then(Json::as_str)?;
    if let Some(code) = named_key(key) {
        return Some(code);
    }
    let mut chars = key.chars();
    let (first, rest) = (chars.next()?, chars.next());
    if rest.is_some() {
        return None;
    }
    let keysym = xkeysym::Keysym::from_char(first);
    compositor.keycode_for_keysym(keysym.raw())
}

/// The evdev code a named key stands on every keyboard.
fn named_key(name: &str) -> Option<u32> {
    Some(match name {
        "Enter" => 28,
        "Escape" => 1,
        "Tab" => 15,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "ArrowUp" => 103,
        "ArrowDown" => 108,
        "Backspace" => 14,
        "Delete" => 111,
        "Home" => 102,
        "End" => 107,
        "PageUp" => 104,
        "PageDown" => 109,
        "F1" => 59,
        "F4" => 62,
        "PrintScreen" => 99,
        "Space" => 57,
        "Shift" => 42,
        "Control" => 29,
        "Alt" => 56,
        "Super" => 125,
        "AudioVolumeUp" => 115,
        "AudioVolumeDown" => 114,
        "AudioVolumeMute" => 113,
        "MicrophoneVolumeMute" => 248,
        "BrightnessUp" => 225,
        "BrightnessDown" => 224,
        "MediaPlayPause" => 164,
        "MediaTrackNext" => 163,
        "MediaTrackPrevious" => 165,
        _ => return None,
    })
}

/// Type `text` as the keyboard attached to the machine would: each character by the key this
/// seat's layout puts it on, or, where the layout has no key for it, written as text into the
/// field that takes it.
fn type_text(compositor: &mut Compositor, text: &str) -> Result<(), String> {
    for ch in text.chars() {
        let typed = compositor
            .type_char(ch)
            .map_err(|error| error.to_string())?;
        if !typed {
            compositor.insert_active_text(&ch.to_string());
        }
    }
    Ok(())
}

/// The desk as it is now: its tree without its history, the workspaces this computer holds, what
/// stands over it, and whether the screen is locked — the shape the tools that drive a desktop
/// read.
fn state_dump(tree: &DesktopTree, desk: &Desk<'_>) -> Json {
    let held: BTreeSet<String> = desk
        .engine
        .map(|engine| {
            engine
                .workspaces()
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let held_workspaces: Vec<Json> = tree
        .workspaces
        .iter()
        .filter(|workspace| held.contains(&workspace.room().to_string()))
        .map(|workspace| {
            json!({
                "workspace": workspace.id.0,
                "room": workspace.room().to_string(),
            })
        })
        .collect();
    let mut open = serde_json::Map::new();
    for slot in Slot::ALL {
        open.insert(slot.as_str().to_owned(), desk.overlays.is_open(slot).into());
    }
    let tree = tilo_desktop_model::history::without_history(|| serde_json::to_value(tree))
        .unwrap_or(Json::Null);
    json!({"state": {
        "workspace_tree": tree,
        "held_workspaces": held_workspaces,
        "chrome": {
            "session_locked": desk.locked,
            "in_transition": false,
            "overlays": open,
        },
        "peers": Json::Null,
        "sharing": Json::Null,
    }})
}

/// One row per tab of `tree` — where it is, which app shows it, and whether it is the tab in
/// front of the active workspace's active strip and group. A workspace or strip without a name
/// is named by its id; `kind` is the tile's variant and `app_id` the app it names, if any.
fn window_rows(tree: &DesktopTree) -> Vec<Json> {
    fn named(name: &str, id: u64) -> Json {
        if name.is_empty() {
            id.into()
        } else {
            name.into()
        }
    }
    let mut rows = Vec::new();
    for workspace in &tree.workspaces {
        let workspace_active = workspace.id == tree.active;
        let active_strip = tree.active_strip.get(&workspace.id);
        for strip in &workspace.strips {
            let strip_active = workspace_active && active_strip == Some(&strip.id);
            let active_group = tree.active_group.get(&strip.id);
            for group in &strip.window_groups {
                let group_active = strip_active && active_group == Some(&group.id);
                for tab in &group.tabs {
                    let (kind, app_id) = tile_kind(&tab.tile);
                    rows.push(json!({
                        "workspace": named(&workspace.name, workspace.id.0),
                        "strip": named(&strip.name, strip.id.0),
                        "group": group.id,
                        "tab": tab.id,
                        "kind": kind,
                        "app_id": app_id,
                        "title": tab.title,
                        "active": group_active && group.active_tab == tab.id,
                    }));
                }
            }
        }
    }
    rows
}

/// A tile's variant name and the `app_id` it carries, as its JSON spells them.
fn tile_kind(tile: &tilo_desktop_model::TileContent) -> (String, Json) {
    match serde_json::to_value(tile).unwrap_or_default() {
        Json::String(kind) => (kind, Json::Null),
        Json::Object(variant) => variant.into_iter().next().map_or_else(
            || (String::new(), Json::Null),
            |(kind, body)| (kind, body.get("app_id").cloned().unwrap_or_default()),
        ),
        _ => (String::new(), Json::Null),
    }
}

/// Write a PNG of every display, each at its place in the layout, to `path`.
fn snapshot(compositor: &mut Compositor, path: &str) -> Result<(), (&'static str, String)> {
    if path.is_empty() {
        return Err(("invalid_path", "snapshot path must not be empty".to_owned()));
    }
    let picture = crate::capture::picture(compositor).map_err(|why| ("snapshot_failed", why))?;
    picture
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|error| ("snapshot_failed", format!("{path}: {error}")))
}

/// Mint a throwaway identity for this session's user, the one a session started after it opens
/// its desk with: what `provision_test_identity` answers on the shell's socket, in the same
/// shape. A real identity is never replaced; a release build mints none.
fn provision_test_identity() -> Json {
    use tilo_identity_host::provisioning::{ProvisionOutcome, THROWAWAY_TEST_IDENTITY_NAME};
    let store = match tilo_onboarding_host::identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => return refusal("provision_failed", &format!("no identity store: {error}")),
    };
    let Some(identity_dir) = store.path().parent() else {
        return refusal("provision_failed", "the identity store has no directory");
    };
    match tilo_onboarding_host::firstboot::ensure_throwaway_test_identity(
        identity_dir,
        &tilo_onboarding_host::firstboot::collaboration_identity_paths(),
    ) {
        Ok(outcome) => json!({"answer": {
            "created": matches!(outcome, ProvisionOutcome::Created { .. }),
            "display_name": THROWAWAY_TEST_IDENTITY_NAME,
            "throwaway": true,
        }}),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            refusal("identity_exists", &error.to_string())
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            refusal("dev_image_only", &error.to_string())
        }
        Err(error) => refusal("provision_failed", &error.to_string()),
    }
}

fn refusal(code: &str, message: &str) -> Json {
    json!({"err": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_development_build_opens_the_socket_it_is_named() {
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            path_in(true, Some("/run/user/1000/control.sock"), None, None),
            Some(PathBuf::from("/run/user/1000/control.sock"))
        );
        assert_eq!(
            path_in(
                false,
                Some("/run/user/1000/control.sock"),
                None,
                runtime.clone()
            ),
            None
        );
        assert_eq!(
            OPENED_BY_THIS_BUILD,
            cfg!(any(debug_assertions, feature = "dev-overrides"))
        );
    }

    #[test]
    fn a_socket_nothing_names_opens_where_the_tools_look() {
        let runtime = Some(OsString::from("/run/user/1000"));
        assert_eq!(
            path_in(true, None, None, runtime),
            Some(PathBuf::from(
                "/run/user/1000/tilo-session/test-control.sock"
            ))
        );
        // A tool that names one keeps it, and the environment names the next.
        assert_eq!(
            path_in(
                true,
                Some("/tmp/named.sock"),
                Some(OsString::from("/tmp/env.sock")),
                None
            ),
            Some(PathBuf::from("/tmp/named.sock"))
        );
        assert_eq!(
            path_in(true, None, Some(OsString::from("/tmp/env.sock")), None),
            Some(PathBuf::from("/tmp/env.sock"))
        );
        assert_eq!(path_in(true, None, None, None), None);
        assert_eq!(path_in(true, None, Some(OsString::new()), None), None);
    }

    #[test]
    fn a_named_key_and_a_character_are_told_apart() {
        assert_eq!(named_key("Enter"), Some(28));
        assert_eq!(named_key("Super"), Some(125));
        assert_eq!(named_key("enter"), None);
        assert_eq!(named_key("a"), None);
    }
}
