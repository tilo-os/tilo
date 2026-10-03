//! Pixel-level computer use: a person's pointer, keyboard and eyes on a
//! window. The fallback for surfaces that publish no typed action, such as a
//! legacy application's window, and how an agent tries an app's own UI.
//!
//! An action naming an `app_id` acts in that app's window alone: the window
//! takes focus, coordinates are its pixels from its top-left (the same
//! pixels its `render` view shows) and screenshots show just the window.
//!
//! The canonical action schema is the one hosted computer-use providers and
//! model vendors share (screenshot, click, type, key, scroll, drag, zoom…), so
//! a provider's existing `computer` tool can be routed here unchanged. Every
//! action is translated into the compositor's control protocol and sent to its
//! socket; this crate stays free of the compositor.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const CONTROL_ENV: &str = "TILO_RUNTIME_COMPUTER_CONTROL";
/// Longest side of a returned screenshot; larger outputs are scaled down and
/// coordinates are scaled back up.
const MAX_FRAME: (u32, u32) = (1280, 800);
/// The same for one app's window, larger so that a window on an ordinary
/// screen keeps its own pixels, which its `render` view shares.
const MAX_WINDOW_FRAME: (u32, u32) = (2048, 2048);
const MAX_BATCH: usize = 50;
const MAX_REPEAT: u32 = 100;
const MAX_WAIT: Duration = Duration::from_secs(30);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

/// One compositor reached through its control socket, shared by every agent
/// of the machine.
///
/// The compositor gives each agent named on a command its own seat and strip:
/// on its turn that strip fills the screen its coordinates and screenshots
/// cover, and the keys and buttons it holds never reach another agent's
/// window. What this side remembers per
/// agent (pointer, last frame's geometry) is kept per agent for the same
/// reason.
pub struct Computer {
    socket: PathBuf,
    scratch: PathBuf,
    /// Screenshot file names, unique across agents.
    shots: AtomicU64,
    anonymous: Arc<Mutex<State>>,
    agents: Mutex<HashMap<String, Arc<Mutex<State>>>>,
}

/// Who a run acts for: an agent run of the MCP layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    /// Human name, shown on the agent's strip.
    pub label: String,
}

/// One run of actions for one agent.
struct Turn<'a> {
    computer: &'a Computer,
    agent: Option<&'a Agent>,
    state: &'a Mutex<State>,
    /// The app whose window the current step acts in; `None` for the screen.
    window: std::cell::RefCell<Option<String>>,
}

#[derive(Default)]
struct State {
    /// Pointer position in screen pixels, as last moved by this host.
    cursor: (f64, f64),
    frame: u64,
    /// Screen size and returned frame size of the last screenshot.
    geometry: Option<Geometry>,
}

#[derive(Clone, Debug, PartialEq)]
struct Geometry {
    screen: (u32, u32),
    frame: (u32, u32),
    /// The app whose window this is; `None` for the screen.
    window: Option<String>,
}

impl Geometry {
    /// A frame of `screen` pixels, scaled to fit the returned frame size.
    fn fit(screen: (u32, u32), window: Option<String>) -> Self {
        let max = if window.is_some() {
            MAX_WINDOW_FRAME
        } else {
            MAX_FRAME
        };
        Self {
            screen,
            frame: fit(screen, max),
            window,
        }
    }
    fn to_screen(&self, [x, y]: [f64; 2]) -> (f64, f64) {
        let sx = f64::from(self.screen.0) / f64::from(self.frame.0.max(1));
        let sy = f64::from(self.screen.1) / f64::from(self.frame.1.max(1));
        (x * sx, y * sy)
    }
    fn to_frame(&self, (x, y): (f64, f64)) -> [f64; 2] {
        let sx = f64::from(self.frame.0) / f64::from(self.screen.0.max(1));
        let sy = f64::from(self.frame.1) / f64::from(self.screen.1.max(1));
        [(x * sx).round(), (y * sy).round()]
    }
}

/// One canonical action.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub action: String,
    #[serde(default)]
    pub coordinate: Option<[f64; 2]>,
    #[serde(default)]
    pub start_coordinate: Option<[f64; 2]>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub modifier: Option<String>,
    #[serde(default)]
    pub repeat: Option<u32>,
    #[serde(default)]
    pub scroll_direction: Option<String>,
    #[serde(default)]
    pub scroll_amount: Option<u32>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub region: Option<[f64; 4]>,
    #[serde(default)]
    pub no_screenshot: Option<bool>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub quality: Option<u8>,
    /// Act in this app's window alone (an app id from `tilo_list_apps`).
    #[serde(default)]
    pub app_id: Option<String>,
}

/// A step's outcome: text for the model and, unless suppressed, a frame.
pub struct Outcome {
    pub text: Value,
    pub image: Option<Frame>,
}

pub struct Frame {
    pub mime: &'static str,
    pub data: Vec<u8>,
}

impl Computer {
    /// The computer named by the environment, if any.
    ///
    /// # Errors
    ///
    /// Rejects a relative socket path and scratch-directory creation errors.
    pub fn from_env(profile: &Path) -> io::Result<Option<Self>> {
        let Some(socket) = std::env::var_os(CONTROL_ENV) else {
            return Ok(None);
        };
        let socket = PathBuf::from(socket);
        if !socket.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{CONTROL_ENV} must be absolute"),
            ));
        }
        let scratch = profile.join("cache").join("computer");
        crate::unix::private_directory(&scratch)?;
        Ok(Some(Self::new(socket, scratch)))
    }

    #[must_use]
    pub fn new(socket: PathBuf, scratch: PathBuf) -> Self {
        Self {
            socket,
            scratch,
            shots: AtomicU64::new(0),
            anonymous: Arc::default(),
            agents: Mutex::default(),
        }
    }

    /// [`Self::run_as`] for a caller that names no agent: the compositor's
    /// default agent.
    ///
    /// # Errors
    ///
    /// The failing step's index and reason.
    pub fn run(&self, actions: &[Action], batch: bool) -> Result<Vec<Outcome>, String> {
        self.run_as(None, actions, batch)
    }

    /// Run a sequence of actions for `agent` in order, stopping at the first
    /// failure. Only the last step carries a screenshot unless a step asks
    /// for one.
    ///
    /// # Errors
    ///
    /// The failing step's index and reason.
    pub fn run_as(
        &self,
        agent: Option<&Agent>,
        actions: &[Action],
        batch: bool,
    ) -> Result<Vec<Outcome>, String> {
        let state = self.state_of(agent);
        Turn {
            computer: self,
            agent,
            state: &state,
            window: std::cell::RefCell::default(),
        }
        .run(actions, batch)
    }

    /// Give `agent` the compositor's seat without acting, so the window of an
    /// app it opens next lands in its strip.
    ///
    /// # Errors
    ///
    /// The compositor's refusal, or why it could not be reached.
    pub fn seat(&self, agent: &Agent) -> Result<(), String> {
        let state = self.state_of(Some(agent));
        Turn {
            computer: self,
            agent: Some(agent),
            state: &state,
            window: std::cell::RefCell::default(),
        }
        .send(&json!({"op":"agent_seat"}))
    }

    /// End `agent`: the compositor releases whatever it still holds, and this
    /// side forgets its pointer and frame.
    ///
    /// # Errors
    ///
    /// The compositor's refusal, or why it could not be reached.
    pub fn leave(&self, agent: &Agent) -> Result<(), String> {
        let state = self.lock_agents().remove(&agent.id).unwrap_or_default();
        Turn {
            computer: self,
            agent: Some(agent),
            state: &state,
            window: std::cell::RefCell::default(),
        }
        .send(&json!({"op":"agent_leave"}))
    }

    fn lock_agents(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Mutex<State>>>> {
        self.agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn state_of(&self, agent: Option<&Agent>) -> Arc<Mutex<State>> {
        match agent {
            Some(agent) => Arc::clone(self.lock_agents().entry(agent.id.clone()).or_default()),
            None => Arc::clone(&self.anonymous),
        }
    }
}

impl Turn<'_> {
    fn run(&self, actions: &[Action], batch: bool) -> Result<Vec<Outcome>, String> {
        if actions.is_empty() || actions.len() > MAX_BATCH {
            return Err(format!("between 1 and {MAX_BATCH} actions"));
        }
        let mut outcomes = Vec::with_capacity(actions.len());
        let last = actions.len().saturating_sub(1);
        for (index, action) in actions.iter().enumerate() {
            let wants_frame = match action.no_screenshot {
                _ if action.action == "screenshot" => true,
                Some(skip) => !skip,
                None => !batch || index == last,
            };
            let outcome = self.step(action, wants_frame).map_err(|error| {
                if batch {
                    format!("[{index}] {error}")
                } else {
                    error
                }
            })?;
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    fn step(&self, action: &Action, wants_frame: bool) -> Result<Outcome, String> {
        self.window.replace(action.app_id.clone());
        let repeat = action.repeat.unwrap_or(1).clamp(1, MAX_REPEAT);
        let mut detail = Value::Null;
        match action.action.as_str() {
            "screenshot" => {}
            "zoom" => {
                let region = action.region.ok_or("zoom needs region")?;
                let image = self.capture(Some(region), action)?;
                return Ok(self.outcome(action, detail, Some(image)));
            }
            "cursor_position" => {
                let (cursor, geometry) = self.cursor();
                detail = json!({"coordinate": geometry.map_or([cursor.0, cursor.1], |g| g.to_frame(cursor))});
            }
            "mouse_move" => {
                let at = self.point(action.coordinate)?;
                self.with_modifier(action.modifier.as_deref(), || self.move_to(at))?;
            }
            "left_click" | "right_click" | "middle_click" | "double_click" | "triple_click" => {
                self.click(action, repeat)?;
            }
            "left_mouse_down" | "left_mouse_up" => {
                let at = action
                    .coordinate
                    .map(|coordinate| self.point(Some(coordinate)))
                    .transpose()?;
                self.with_modifier(action.modifier.as_deref(), || {
                    if let Some(at) = at {
                        self.move_to(at)?;
                    }
                    self.button("left", action.action == "left_mouse_down")
                })?;
            }
            "left_click_drag" => {
                let from = match action.start_coordinate {
                    Some(start) => self.point(Some(start))?,
                    None => self.cursor().0,
                };
                let to = self.point(action.coordinate)?;
                self.with_modifier(action.modifier.as_deref(), || {
                    self.send(&json!({"op":"drag","x_from":from.0,"y_from":from.1,"x_to":to.0,"y_to":to.1,"steps":12}))
                })?;
                self.set_cursor(to);
            }
            "scroll" => self.scroll(action)?,
            "type" => {
                let text = action.text.as_deref().ok_or("type needs text")?;
                self.send(&json!({"op":"type","text":text}))?;
            }
            "key" => {
                let combo = action
                    .text
                    .as_deref()
                    .or(action.key.as_deref())
                    .ok_or("key needs text")?;
                let chord = Chord::parse(combo)?;
                for _ in 0..repeat {
                    self.chord(&chord, None)?;
                }
            }
            "hold_key" => {
                let combo = action
                    .text
                    .as_deref()
                    .or(action.key.as_deref())
                    .ok_or("hold_key needs text")?;
                let chord = Chord::parse(combo)?;
                self.chord(&chord, Some(wait_for(action.duration)))?;
            }
            "wait" => std::thread::sleep(wait_for(action.duration.or(Some(1.0)))),
            other => return Err(format!("unknown action {other:?}")),
        }
        let image = if wants_frame {
            Some(self.capture(None, action)?)
        } else {
            None
        };
        Ok(self.outcome(action, detail, image))
    }

    fn click(&self, action: &Action, repeat: u32) -> Result<(), String> {
        if let Some(coordinate) = action.coordinate {
            let at = self.point(Some(coordinate))?;
            self.move_to(at)?;
        }
        let button = match action.action.as_str() {
            "right_click" => "right",
            "middle_click" => "middle",
            _ => "left",
        };
        let clicks = match action.action.as_str() {
            "double_click" => 2u32,
            "triple_click" => 3,
            _ => 1,
        }
        .saturating_mul(repeat);
        self.with_modifier(action.modifier.as_deref(), || {
            for _ in 0..clicks {
                self.button(button, true)?;
                self.button(button, false)?;
            }
            Ok(())
        })
    }

    fn scroll(&self, action: &Action) -> Result<(), String> {
        if let Some(coordinate) = action.coordinate {
            let at = self.point(Some(coordinate))?;
            self.move_to(at)?;
        }
        let amount = f64::from(action.scroll_amount.unwrap_or(3).clamp(1, 100));
        let (dx, dy) = match action.scroll_direction.as_deref().unwrap_or("down") {
            "up" => (0.0, amount),
            "down" => (0.0, -amount),
            "left" => (amount, 0.0),
            "right" => (-amount, 0.0),
            other => return Err(format!("unknown scroll_direction {other:?}")),
        };
        self.with_modifier(action.modifier.as_deref(), || {
            self.send(&json!({"op":"scroll","dx":dx,"dy":dy}))
        })
    }

    fn outcome(&self, action: &Action, detail: Value, image: Option<Frame>) -> Outcome {
        let (frame, (width, height)) = {
            let state = self.lock();
            (
                state.frame,
                state.geometry.as_ref().map_or((0, 0), |g| g.frame),
            )
        };
        let mut text =
            json!({"action":action.action,"frame_id":frame,"width":width,"height":height});
        if let (Some(app_id), Some(fields)) = (&action.app_id, text.as_object_mut()) {
            fields.insert("app_id".into(), json!(app_id));
        }
        if !detail.is_null()
            && let Some(fields) = text.as_object_mut()
        {
            fields.insert("detail".into(), detail);
        }
        Outcome { text, image }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cursor(&self) -> ((f64, f64), Option<Geometry>) {
        let state = self.lock();
        (state.cursor, state.geometry.clone())
    }

    fn set_cursor(&self, at: (f64, f64)) {
        self.lock().cursor = at;
    }

    /// A model coordinate in the last frame, in the pixels the compositor
    /// takes: the screen's, or the aimed-at window's from its top-left.
    fn point(&self, coordinate: Option<[f64; 2]>) -> Result<(f64, f64), String> {
        let coordinate = coordinate.ok_or("coordinate is required")?;
        if coordinate
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err("coordinate must be non-negative".into());
        }
        let window = self.window.borrow().clone();
        let geometry = match window {
            // A window's size is asked for, not remembered: it may have been
            // resized since, and its pixels are known without a screenshot.
            Some(app_id) => self.window_geometry(app_id)?,
            None => self
                .lock()
                .geometry
                .clone()
                .filter(|geometry| geometry.window.is_none())
                .ok_or("take a screenshot before acting on screen coordinates (or name app_id to use an app's window pixels)")?,
        };
        let (x, y) = geometry.to_screen(coordinate);
        if x >= f64::from(geometry.screen.0) || y >= f64::from(geometry.screen.1) {
            let [width, height] =
                geometry.to_frame((f64::from(geometry.screen.0), f64::from(geometry.screen.1)));
            let what = geometry
                .window
                .as_ref()
                .map_or_else(|| "the screen".to_owned(), |app| format!("{app}'s window"));
            return Err(format!(
                "coordinate is outside {what}, which is {width}x{height}"
            ));
        }
        Ok((x, y))
    }

    /// The aimed-at window's size, as its frames are returned, from the
    /// compositor; the window takes focus.
    fn window_geometry(&self, app_id: String) -> Result<Geometry, String> {
        let answer = self.answer(&json!({"op":"agent_seat"}))?;
        let size = answer
            .get("viewport")
            .and_then(Value::as_array)
            .and_then(|viewport| Some((viewport.get(2)?.as_f64()?, viewport.get(3)?.as_f64()?)))
            .ok_or("the compositor did not say how large the window is")?;
        let geometry = Geometry::fit(
            (
                clamp_u32(size.0, u32::MAX).max(1),
                clamp_u32(size.1, u32::MAX).max(1),
            ),
            Some(app_id),
        );
        self.lock().geometry = Some(geometry.clone());
        Ok(geometry)
    }

    fn move_to(&self, (x, y): (f64, f64)) -> Result<(), String> {
        self.send(&json!({"op":"pointer_move","x":x,"y":y}))?;
        self.set_cursor((x, y));
        Ok(())
    }

    fn button(&self, button: &str, pressed: bool) -> Result<(), String> {
        self.send(&json!({"op":"pointer_button","pressed":pressed,"button":button}))
    }

    fn with_modifier(
        &self,
        modifier: Option<&str>,
        body: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let held = match modifier {
            Some(modifier) => Chord::parse(modifier)?.modifiers,
            None => Vec::new(),
        };
        for key in &held {
            self.send(&json!({"op":"key_down","key":key}))?;
        }
        let result = body();
        for key in held.iter().rev() {
            let _ = self.send(&json!({"op":"key_up","key":key}));
        }
        result
    }

    fn chord(&self, chord: &Chord, hold: Option<Duration>) -> Result<(), String> {
        for key in &chord.modifiers {
            self.send(&json!({"op":"key_down","key":key}))?;
        }
        let result = match (&chord.key, hold) {
            (Some(key), Some(hold)) => self
                .send(&json!({"op":"key_down","key":key}))
                .map(|()| std::thread::sleep(hold))
                .and_then(|()| self.send(&json!({"op":"key_up","key":key}))),
            (Some(key), None) => self.send(&json!({"op":"key","key":key})),
            (None, hold) => {
                std::thread::sleep(hold.unwrap_or_default());
                Ok(())
            }
        };
        for key in chord.modifiers.iter().rev() {
            let _ = self.send(&json!({"op":"key_up","key":key}));
        }
        result
    }

    /// Send one control command, for this turn's agent and window, and
    /// require an `ok` reply.
    fn send(&self, command: &Value) -> Result<(), String> {
        self.answer(command).map(drop)
    }

    /// [`Self::send`], answering the reply's payload.
    fn answer(&self, command: &Value) -> Result<Value, String> {
        let mut command = command.clone();
        if let Some(fields) = command.as_object_mut() {
            if let Some(agent) = self.agent {
                fields.insert("agent".into(), json!(agent.id));
                fields.insert("agent_label".into(), json!(agent.label));
            }
            if let Some(app_id) = self.window.borrow().as_ref() {
                fields.insert("window".into(), json!(app_id));
            }
        }
        let reply = self.request(&command).map_err(|error| error.to_string())?;
        if let Some(error) = reply.get("err") {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(format!("{code}: {message}"));
        }
        Ok(reply.get("answer").cloned().unwrap_or(reply))
    }

    fn request(&self, command: &Value) -> io::Result<Value> {
        let mut stream = UnixStream::connect(&self.computer.socket)?;
        stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
        stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
        let mut line = serde_json::to_vec(command).map_err(io::Error::other)?;
        line.push(b'\n');
        stream.write_all(&line)?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        serde_json::from_str(&reply)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// One snapshot from the compositor: the screen, or the aimed-at window.
    /// After input the compositor writes it once the windows in it answered
    /// that input, so it shows what the input did.
    fn snapshot(&self) -> Result<image::RgbaImage, String> {
        let shot = self.computer.shots.fetch_add(1, Ordering::Relaxed);
        let path = self.computer.scratch.join(format!("frame-{shot}.png"));
        let _ = std::fs::remove_file(&path);
        self.send(&json!({"op":"snapshot","path":path.to_string_lossy()}))?;
        let image = read_png(&path, SNAPSHOT_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        image
    }

    /// A fresh frame of the whole screen, or of `region` scaled up to the
    /// frame size, encoded as the action asks.
    fn capture(&self, region: Option<[f64; 4]>, action: &Action) -> Result<Frame, String> {
        {
            let mut state = self.lock();
            state.frame = state.frame.saturating_add(1);
        }
        let screen = self.snapshot()?;
        let screen_size = (screen.width(), screen.height());
        let window = self.window.borrow().clone();
        let current = Geometry::fit(screen_size, window);
        let frame_size = current.frame;
        let previous = {
            let mut state = self.lock();
            let previous = state.geometry.clone();
            if region.is_none() {
                state.geometry = Some(current.clone());
            }
            previous
        };
        let image = match region {
            None => resize(&screen, frame_size),
            Some([x0, y0, x1, y1]) => {
                let geometry = previous
                    .filter(|previous| previous.window == current.window)
                    .ok_or("take a screenshot (of the same window) before zooming")?;
                let (sx0, sy0) = geometry.to_screen([x0, y0]);
                let (sx1, sy1) = geometry.to_screen([x1, y1]);
                if !(sx1 > sx0 && sy1 > sy0) {
                    return Err("region must be [x0, y0, x1, y1] with x1 > x0 and y1 > y0".into());
                }
                let (x, y) = (clamp_u32(sx0, screen_size.0), clamp_u32(sy0, screen_size.1));
                let width = clamp_u32(sx1, screen_size.0).saturating_sub(x).max(1);
                let height = clamp_u32(sy1, screen_size.1).saturating_sub(y).max(1);
                let crop = image::imageops::crop_imm(&screen, x, y, width, height).to_image();
                resize(&crop, geometry.frame)
            }
        };
        encode(&image, action.format.as_deref(), action.quality)
    }
}

impl crate::watch::WatchSink for Computer {
    fn set_watching(&self, watching: bool) -> Result<(), String> {
        // Watch is session-wide paint policy, not per-agent input: use the
        // default (no agent) turn so the command still reaches the socket.
        let state = self.state_of(None);
        Turn {
            computer: self,
            agent: None,
            state: &state,
            window: std::cell::RefCell::default(),
        }
        .send(&json!({"op":"watch","watching":watching}))
    }
}

/// xdotool-style key combination, e.g. `ctrl+shift+Return`.
#[derive(Debug, PartialEq, Eq)]
struct Chord {
    modifiers: Vec<&'static str>,
    key: Option<String>,
}

impl Chord {
    fn parse(combo: &str) -> Result<Self, String> {
        let mut modifiers = Vec::new();
        let mut key = None;
        for part in combo
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            if let Some(modifier) = modifier_name(part) {
                modifiers.push(modifier);
            } else if key.replace(key_name(part)?).is_some() {
                return Err(format!("{combo:?} names more than one non-modifier key"));
            }
        }
        if modifiers.is_empty() && key.is_none() {
            return Err("empty key".into());
        }
        Ok(Self { modifiers, key })
    }
}

fn modifier_name(part: &str) -> Option<&'static str> {
    match part.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "control_l" | "control_r" => Some("Control"),
        "shift" | "shift_l" | "shift_r" => Some("Shift"),
        "alt" | "alt_l" | "alt_r" | "option" => Some("Alt"),
        "super" | "super_l" | "super_r" | "meta" | "cmd" | "win" | "logo" => Some("Super"),
        _ => None,
    }
}

/// The control protocol's name for an xdotool or DOM-style key name.
fn key_name(part: &str) -> Result<String, String> {
    let named = match part.to_ascii_lowercase().as_str() {
        "return" | "enter" | "kp_enter" => "Enter",
        "escape" | "esc" => "Escape",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "home" => "Home",
        "end" => "End",
        "page_up" | "pageup" | "prior" => "PageUp",
        "page_down" | "pagedown" | "next" => "PageDown",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "space" => "Space",
        "f1" => "F1",
        "f4" => "F4",
        "print" | "printscreen" => "PrintScreen",
        _ => {
            let mut characters = part.chars();
            return match (characters.next(), characters.next()) {
                (Some(character), None) => Ok(character.to_string()),
                _ => Err(format!("unsupported key {part:?}")),
            };
        }
    };
    Ok(named.to_owned())
}

fn wait_for(seconds: Option<f64>) -> Duration {
    let seconds = seconds.unwrap_or(0.5);
    if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds).min(MAX_WAIT)
    } else {
        Duration::ZERO
    }
}

/// Wait for the compositor to finish writing a PNG and decode it.
fn read_png(path: &Path, timeout: Duration) -> Result<image::RgbaImage, String> {
    let started = Instant::now();
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(image) = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        {
            return Ok(image.to_rgba8());
        }
        if started.elapsed() >= timeout {
            return Err("the compositor wrote no screenshot".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn fit((width, height): (u32, u32), (max_width, max_height): (u32, u32)) -> (u32, u32) {
    if width <= max_width && height <= max_height {
        return (width.max(1), height.max(1));
    }
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    (
        clamp_u32(f64::from(width) * scale, max_width).max(1),
        clamp_u32(f64::from(height) * scale, max_height).max(1),
    )
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to [0, max] first"
)]
fn clamp_u32(value: f64, max: u32) -> u32 {
    value.round().clamp(0.0, f64::from(max)) as u32
}

fn resize(image: &image::RgbaImage, (width, height): (u32, u32)) -> image::RgbaImage {
    if image.dimensions() == (width, height) {
        return image.clone();
    }
    image::imageops::resize(image, width, height, image::imageops::FilterType::Triangle)
}

fn encode(
    image: &image::RgbaImage,
    format: Option<&str>,
    quality: Option<u8>,
) -> Result<Frame, String> {
    let mut data = Vec::new();
    match format.unwrap_or("jpeg") {
        "png" => {
            image::codecs::png::PngEncoder::new(&mut data)
                .write_image(
                    image,
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|error| error.to_string())?;
            Ok(Frame {
                mime: "image/png",
                data,
            })
        }
        "jpeg" | "jpg" => {
            let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut data,
                quality.unwrap_or(80).clamp(1, 95),
            )
            .encode_image(&rgb)
            .map_err(|error| error.to_string())?;
            Ok(Frame {
                mime: "image/jpeg",
                data,
            })
        }
        other => Err(format!("unknown format {other:?}")),
    }
}

use image::ImageEncoder as _;

/// MCP tool content for a run: each step's text, and the frames it produced.
#[must_use]
pub fn content(outcomes: &[Outcome], batch: bool) -> Value {
    let mut blocks = Vec::new();
    for (index, outcome) in outcomes.iter().enumerate() {
        let text = if batch {
            format!("[{index}] {}", outcome.text)
        } else {
            outcome.text.to_string()
        };
        blocks.push(json!({"type":"text","text":text}));
        if let Some(frame) = &outcome.image {
            blocks.push(json!({"type":"image","mimeType":frame.mime,"data":base64::engine::general_purpose::STANDARD.encode(&frame.data)}));
        }
    }
    json!({"content":blocks,"isError":false})
}

/// The canonical tool definitions this host adds when a computer is present.
#[must_use]
pub fn tool_definitions() -> Vec<Value> {
    let point = json!({"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2});
    let action = json!({
        "type":"object",
        "properties":{
            "action":{"type":"string","enum":["screenshot","left_click","right_click","middle_click","double_click","triple_click","mouse_move","left_click_drag","left_mouse_down","left_mouse_up","scroll","type","key","hold_key","wait","zoom","cursor_position"]},
            "coordinate":point,
            "start_coordinate":point,
            "text":{"type":"string"},
            "key":{"type":"string"},
            "modifier":{"type":"string"},
            "repeat":{"type":"integer","minimum":1,"maximum":MAX_REPEAT},
            "scroll_direction":{"type":"string","enum":["up","down","left","right"]},
            "scroll_amount":{"type":"integer","minimum":1,"maximum":100},
            "duration":{"type":"number","minimum":0},
            "region":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4},
            "no_screenshot":{"type":"boolean"},
            "format":{"type":"string","enum":["jpeg","png"]},
            "quality":{"type":"integer","minimum":1,"maximum":95},
            "app_id":{"type":"string","description":"Act in this app's window alone: it takes focus, screenshots show just it and coordinates are its pixels from its top-left (a point of the app's render plus the render's window_origin)"}
        },
        "required":["action"],
        "additionalProperties":false
    });
    let effect = json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true});
    vec![
        json!({"name":"computer","description":"Use a window as a person does: pointer, keys, scroll. One action; returns a screenshot once the window drew its answer. For work, prefer an app's typed actions (tilo_invoke); this is for trying its UI and for windows without actions. With app_id the action lands in that app's window. Without it coordinates are pixels in the last screenshot's frame: take a screenshot first. modifier (e.g. \"shift\", \"ctrl+alt\") is held during a click, drag, move or scroll; key takes chords like \"ctrl+z\"","inputSchema":action,"annotations":effect}),
        // Each step takes the `computer` tool's arguments; restating that
        // schema here would double what every model turn reads.
        json!({"name":"computer_batch","description":"Up to 50 computer actions in order, each an object of the computer tool's arguments, stopping at the first failure; screenshot steps and the last step return a frame unless no_screenshot says otherwise. app_id applies to every step naming none. A stroke along a path: left_mouse_down, mouse_move steps, left_mouse_up","inputSchema":{"type":"object","properties":{"actions":{"type":"array","items":{"type":"object"},"minItems":1,"maxItems":MAX_BATCH},"app_id":{"type":"string"}},"required":["actions"],"additionalProperties":false},"annotations":effect}),
    ]
}

#[cfg(test)]
#[path = "computer_tests.rs"]
mod tests;
