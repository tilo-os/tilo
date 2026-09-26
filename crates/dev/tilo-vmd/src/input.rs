//! Per-seat uinput devices for headful slots.
//!
//! A uinput device lives as long as the file descriptor that created it,
//! and every `tilo-vmd rpc` is a short-lived process, so the devices are
//! owned by `tilo-vmd input-serve`, a transient system unit
//! (`tilo-vm-input-<slot>.service`, started with `systemd-run`). It creates
//! a keyboard and an absolute pointer at once, and a touchscreen, a pen and
//! a gamepad the first time an action needs one, so a slot only has the
//! hardware its test plugged in. Every device's `phys` is the slot's logind
//! seat; `73-uinput-seat.rules` tags them `ID_SEAT=<phys>`, so the slot's
//! compositor gets them like hardware plugged into its seat (libinput for
//! keyboard, pointer, touch and pen; the seat's evdev ACL for the gamepad),
//! and no other seat sees them. `rpc` sends it actions over a Unix socket,
//! one JSON line per request.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::InputAction;
use tilo_vm_protocol::input::{
    ABS_X, ABS_Y, GamepadInput, KEY_LEFTSHIFT, combo_codes, gamepad_combo, text_strokes,
};

use crate::error::DaemonError;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_ABS: u16 = 3;
pub const ABS_PRESSURE: u16 = 0x18;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const BTN_TOOL_PEN: u16 = 0x140;
pub const BTN_TOOL_FINGER: u16 = 0x145;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_STYLUS: u16 = 0x14b;
pub const BTN_TOOL_DOUBLETAP: u16 = 0x14d;

/// Touch contacts the touchscreen tracks at once.
pub const TOUCH_SLOTS: i32 = 10;
pub const STICK_MIN: i32 = -32_768;
pub const STICK_MAX: i32 = 32_767;
pub const TRIGGER_MAX: i32 = 255;
pub const PRESSURE_MAX: i32 = 4095;
/// How often a moving finger or pen reports, like a 100 Hz digitiser.
pub const MOTION_STEP_MS: u32 = 10;
/// How long a tap's finger or pen stays down.
pub const TAP_MS: u32 = 60;
/// How long a gamepad control stays pressed, and the pause after it.
pub const PRESS_MS: u32 = 60;

/// Which device an event goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Device {
    Keyboard,
    Pointer,
    Touch,
    Pen,
    Gamepad,
}

/// One kernel input event, before its timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub device: Device,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// What `input-serve` does, in order: write an event, or wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Emit(Event),
    WaitMs(u32),
}

impl Step {
    pub fn event(self) -> Option<Event> {
        match self {
            Self::Emit(event) => Some(event),
            Self::WaitMs(_) => None,
        }
    }
}

const fn event(device: Device, kind: u16, code: u16, value: i32) -> Step {
    Step::Emit(Event {
        device,
        kind,
        code,
        value,
    })
}

fn clamp(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// Positions from `from` (excluded) to `to` (included), one per
/// [`MOTION_STEP_MS`] of `duration_ms`.
pub fn path(from: (u32, u32), to: (u32, u32), duration_ms: u32) -> Vec<(u32, u32)> {
    let steps = (duration_ms / MOTION_STEP_MS).max(1);
    (1..=steps)
        .map(|step| {
            (
                lerp(from.0, to.0, step, steps),
                lerp(from.1, to.1, step, steps),
            )
        })
        .collect()
}

fn lerp(from: u32, to: u32, step: u32, steps: u32) -> u32 {
    let (from, to) = (i64::from(from), i64::from(to));
    let delta = to
        .saturating_sub(from)
        .saturating_mul(i64::from(step))
        .checked_div(i64::from(steps))
        .unwrap_or(0);
    let value = from.saturating_add(delta);
    u32::try_from(value).unwrap_or(0)
}

/// Turns actions into kernel events. Touch tracking ids keep counting
/// across requests, as a real touchscreen's do.
#[derive(Debug, Default)]
pub struct Encoder {
    next_tracking_id: i32,
    out: Vec<Step>,
}

impl Encoder {
    fn emit(&mut self, device: Device, kind: u16, code: u16, value: i32) {
        self.out.push(event(device, kind, code, value));
    }

    fn syn(&mut self, device: Device) {
        self.emit(device, EV_SYN, 0, 0);
    }

    fn wait(&mut self, ms: u32) {
        if ms > 0 {
            self.out.push(Step::WaitMs(ms));
        }
    }

    /// A key press or release followed by its report.
    fn key(&mut self, code: u16, down: bool) {
        self.emit(Device::Keyboard, EV_KEY, code, i32::from(down));
        self.syn(Device::Keyboard);
    }

    fn pointer_to(&mut self, x: u32, y: u32) {
        self.emit(Device::Pointer, EV_ABS, ABS_X, clamp(x));
        self.emit(Device::Pointer, EV_ABS, ABS_Y, clamp(y));
        self.syn(Device::Pointer);
    }

    fn fingers_at(&mut self, fingers: &[(u32, u32)]) {
        for (slot, (x, y)) in (0..).zip(fingers) {
            self.emit(Device::Touch, EV_ABS, ABS_MT_SLOT, slot);
            self.emit(Device::Touch, EV_ABS, ABS_MT_POSITION_X, clamp(*x));
            self.emit(Device::Touch, EV_ABS, ABS_MT_POSITION_Y, clamp(*y));
        }
        if let Some((x, y)) = fingers.first() {
            self.emit(Device::Touch, EV_ABS, ABS_X, clamp(*x));
            self.emit(Device::Touch, EV_ABS, ABS_Y, clamp(*y));
        }
    }

    fn tool(fingers: usize) -> u16 {
        if fingers > 1 {
            BTN_TOOL_DOUBLETAP
        } else {
            BTN_TOOL_FINGER
        }
    }

    /// Fingers touch down, one slot each (type B multitouch).
    fn touch_down(&mut self, fingers: &[(u32, u32)]) {
        for slot in (0..).take(fingers.len()) {
            self.emit(Device::Touch, EV_ABS, ABS_MT_SLOT, slot);
            let id = self.next_tracking_id;
            self.next_tracking_id = id.checked_add(1).filter(|n| *n < 65_536).unwrap_or(0);
            self.emit(Device::Touch, EV_ABS, ABS_MT_TRACKING_ID, id);
        }
        self.fingers_at(fingers);
        self.emit(Device::Touch, EV_KEY, BTN_TOUCH, 1);
        self.emit(Device::Touch, EV_KEY, Self::tool(fingers.len()), 1);
        self.syn(Device::Touch);
    }

    fn touch_move(&mut self, fingers: &[(u32, u32)]) {
        self.fingers_at(fingers);
        self.syn(Device::Touch);
        self.wait(MOTION_STEP_MS);
    }

    fn touch_up(&mut self, fingers: usize) {
        for slot in (0..).take(fingers) {
            self.emit(Device::Touch, EV_ABS, ABS_MT_SLOT, slot);
            self.emit(Device::Touch, EV_ABS, ABS_MT_TRACKING_ID, -1);
        }
        self.emit(Device::Touch, EV_KEY, BTN_TOUCH, 0);
        self.emit(Device::Touch, EV_KEY, Self::tool(fingers), 0);
        self.syn(Device::Touch);
    }

    fn pen_down(&mut self, (x, y): (u32, u32), pressure: u8) {
        // Proximity first, then contact, as a real pen reports it.
        self.emit(Device::Pen, EV_KEY, BTN_TOOL_PEN, 1);
        self.emit(Device::Pen, EV_ABS, ABS_X, clamp(x));
        self.emit(Device::Pen, EV_ABS, ABS_Y, clamp(y));
        self.syn(Device::Pen);
        self.emit(Device::Pen, EV_KEY, BTN_TOUCH, 1);
        let level = i32::from(pressure.min(100)).saturating_mul(PRESSURE_MAX) / 100;
        self.emit(Device::Pen, EV_ABS, ABS_PRESSURE, level);
        self.syn(Device::Pen);
    }

    fn pen_to(&mut self, (x, y): (u32, u32)) {
        self.emit(Device::Pen, EV_ABS, ABS_X, clamp(x));
        self.emit(Device::Pen, EV_ABS, ABS_Y, clamp(y));
        self.syn(Device::Pen);
        self.wait(MOTION_STEP_MS);
    }

    fn pen_up(&mut self) {
        self.emit(Device::Pen, EV_KEY, BTN_TOUCH, 0);
        self.emit(Device::Pen, EV_ABS, ABS_PRESSURE, 0);
        self.syn(Device::Pen);
        self.emit(Device::Pen, EV_KEY, BTN_TOOL_PEN, 0);
        self.syn(Device::Pen);
    }

    fn gamepad(&mut self, input: GamepadInput, down: bool) {
        let (kind, code, value) = match input {
            GamepadInput::Button(code) => (EV_KEY, code, i32::from(down)),
            GamepadInput::Trigger(axis) => (EV_ABS, axis, if down { TRIGGER_MAX } else { 0 }),
            GamepadInput::Hat(axis, direction) => (EV_ABS, axis, if down { direction } else { 0 }),
        };
        self.emit(Device::Gamepad, kind, code, value);
        self.syn(Device::Gamepad);
    }

    fn pinch_fingers(center: (u32, u32), distance: u32) -> [(u32, u32); 2] {
        let half = distance / 2;
        [
            (center.0.saturating_sub(half), center.1),
            (center.0.saturating_add(half), center.1),
        ]
    }

    /// The steps for `actions`, each event report its own so the
    /// compositor sees presses, releases and motion in order.
    ///
    /// # Errors
    /// A combo, character or gamepad control that does not map (callers
    /// validate first).
    pub fn encode(&mut self, actions: &[InputAction]) -> Result<Vec<Step>, String> {
        for action in actions {
            self.action(action)?;
        }
        Ok(std::mem::take(&mut self.out))
    }

    #[expect(clippy::too_many_lines, reason = "one arm per input action")]
    fn action(&mut self, action: &InputAction) -> Result<(), String> {
        match action {
            InputAction::Key { combos } => {
                for combo in combos {
                    let codes = combo_codes(combo)?;
                    for code in &codes {
                        self.key(*code, true);
                    }
                    for code in codes.iter().rev() {
                        self.key(*code, false);
                    }
                }
            }
            InputAction::Type { text } => {
                for stroke in text_strokes(text).map_err(String::from)? {
                    if stroke.shift {
                        self.key(KEY_LEFTSHIFT, true);
                    }
                    self.key(stroke.code, true);
                    self.key(stroke.code, false);
                    if stroke.shift {
                        self.key(KEY_LEFTSHIFT, false);
                    }
                }
            }
            InputAction::Move { x, y } => self.pointer_to(*x, *y),
            InputAction::Click { button, at } => {
                if let Some((x, y)) = at {
                    self.pointer_to(*x, *y);
                }
                self.emit(Device::Pointer, EV_KEY, button.code(), 1);
                self.syn(Device::Pointer);
                self.emit(Device::Pointer, EV_KEY, button.code(), 0);
                self.syn(Device::Pointer);
            }
            InputAction::TouchTap { x, y } => {
                self.touch_down(&[(*x, *y)]);
                self.wait(TAP_MS);
                self.touch_up(1);
            }
            InputAction::TouchHold { x, y, duration_ms } => {
                self.touch_down(&[(*x, *y)]);
                self.wait(*duration_ms);
                self.touch_up(1);
            }
            InputAction::TouchDrag {
                from,
                to,
                duration_ms,
            } => {
                self.touch_down(&[*from]);
                for point in path(*from, *to, *duration_ms) {
                    self.touch_move(&[point]);
                }
                self.touch_up(1);
            }
            InputAction::TouchPinch {
                center,
                from_distance,
                to_distance,
                duration_ms,
            } => {
                self.touch_down(&Self::pinch_fingers(*center, *from_distance));
                for (distance, _) in path((*from_distance, 0), (*to_distance, 0), *duration_ms) {
                    self.touch_move(&Self::pinch_fingers(*center, distance));
                }
                self.touch_up(2);
            }
            InputAction::PenTap { x, y, pressure } => {
                self.pen_down((*x, *y), *pressure);
                self.wait(TAP_MS);
                self.pen_up();
            }
            InputAction::PenDrag {
                from,
                to,
                duration_ms,
                pressure,
            } => {
                self.pen_down(*from, *pressure);
                for point in path(*from, *to, *duration_ms) {
                    self.pen_to(point);
                }
                self.pen_up();
            }
            InputAction::GamepadPress { combos } => {
                for combo in combos {
                    let inputs = gamepad_combo(combo)
                        .map_err(|part| format!("no gamepad control {part}"))?;
                    for input in &inputs {
                        self.gamepad(*input, true);
                    }
                    self.wait(PRESS_MS);
                    for input in inputs.iter().rev() {
                        self.gamepad(*input, false);
                    }
                    self.wait(PRESS_MS);
                }
            }
            InputAction::GamepadStick {
                stick,
                x,
                y,
                duration_ms,
            } => {
                let (x_axis, y_axis) = stick.axes();
                self.emit(Device::Gamepad, EV_ABS, x_axis, stick_value(*x));
                self.emit(Device::Gamepad, EV_ABS, y_axis, stick_value(*y));
                self.syn(Device::Gamepad);
                self.wait(*duration_ms);
                self.emit(Device::Gamepad, EV_ABS, x_axis, 0);
                self.emit(Device::Gamepad, EV_ABS, y_axis, 0);
                self.syn(Device::Gamepad);
                self.wait(PRESS_MS);
            }
        }
        Ok(())
    }
}

/// A stick axis value for `percent` of full deflection.
pub fn stick_value(percent: i8) -> i32 {
    let percent = i32::from(percent.clamp(-100, 100));
    if percent < 0 {
        percent.saturating_mul(STICK_MIN) / -100
    } else {
        percent.saturating_mul(STICK_MAX) / 100
    }
}

/// The steps for `actions` from a fresh encoder.
///
/// # Errors
/// See [`Encoder::encode`].
pub fn events(actions: &[InputAction]) -> Result<Vec<Step>, String> {
    Encoder::default().encode(actions)
}

/// `(width, height)` of the output a seat shows, from its DRM connector's
/// preferred (first) mode; the pointer's absolute range matches it, so
/// `move X Y` lands on output pixel X, Y.
pub fn output_size(seat: &str) -> (u32, u32) {
    let cards: Vec<PathBuf> = if seat == "seat0" {
        std::fs::read_dir("/sys/class/drm")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect()
    } else {
        let drm = Path::new("/sys/devices/faux").join(seat).join("drm");
        std::fs::read_dir(drm)
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|card| {
                std::fs::read_dir(card.path())
                    .into_iter()
                    .flatten()
                    .flatten()
            })
            .map(|e| e.path())
            .collect()
    };
    cards
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p.join("status")).is_ok_and(|s| s.trim() == "connected")
        })
        .find_map(|p| parse_mode(&std::fs::read_to_string(p.join("modes")).ok()?))
        .unwrap_or((1024, 768))
}

/// The first `WxH` line of a DRM `modes` file.
pub fn parse_mode(modes: &str) -> Option<(u32, u32)> {
    let (w, h) = modes.lines().next()?.trim().split_once('x')?;
    let h: String = h.chars().take_while(char::is_ascii_digit).collect();
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The coordinate spaces of a slot made with `--outputs`: the absolute
/// pointer spans the logical desktop; touch and pen span the first
/// (primary) output, which is where the compositor puts a touchscreen that
/// names no output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub desktop: (u32, u32),
    pub touch: (u32, u32),
}

/// Where `input-serve` for a slot listens.
pub fn socket_path(slot: &str) -> PathBuf {
    Path::new("/run/tilo-vm-input").join(format!("{slot}.sock"))
}

pub fn unit_name(slot: &str) -> String {
    format!("tilo-vm-input-{slot}.service")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub actions: Vec<InputAction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub seat: String,
    /// The coordinate spaces the devices were made with, when given.
    #[serde(default)]
    pub geometry: Option<Geometry>,
    #[serde(default)]
    pub events: u64,
    #[serde(default)]
    pub error: Option<String>,
}

/// Send `actions` to a running `input-serve`.
///
/// # Errors
/// No server listening, or the server's own failure.
pub fn send(socket: &Path, actions: &[InputAction]) -> std::io::Result<Reply> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    let mut line = serde_json::to_vec(&Request {
        actions: actions.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(std::io::Error::other)
}

/// The uinput devices of one seat, created on first use.
struct Devices {
    seat: String,
    desktop: (u32, u32),
    touch: (u32, u32),
    open: BTreeMap<Device, std::fs::File>,
}

impl Devices {
    fn get(&mut self, device: Device) -> std::io::Result<&std::fs::File> {
        if !self.open.contains_key(&device) {
            let seat = &self.seat;
            let file = match device {
                Device::Keyboard => uinput::keyboard(seat)?,
                Device::Pointer => uinput::pointer(seat, self.desktop.0, self.desktop.1)?,
                Device::Touch => uinput::touchscreen(seat, self.touch.0, self.touch.1)?,
                Device::Pen => uinput::pen(seat, self.touch.0, self.touch.1)?,
                Device::Gamepad => uinput::gamepad(seat)?,
            };
            if !matches!(device, Device::Keyboard | Device::Pointer) {
                // Plugged in just now: let udev tag it for the seat and the
                // compositor open it before the first event.
                let _ = std::process::Command::new("udevadm").arg("settle").status();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            self.open.insert(device, file);
        }
        self.open
            .get(&device)
            .ok_or_else(|| std::io::Error::other("device vanished"))
    }

    fn run(&mut self, steps: &[Step]) -> (u64, Option<String>) {
        let mut written: u64 = 0;
        for step in steps {
            match step {
                Step::WaitMs(ms) => {
                    std::thread::sleep(std::time::Duration::from_millis(u64::from(*ms)));
                }
                Step::Emit(ev) => {
                    let result = self
                        .get(ev.device)
                        .and_then(|file| uinput::write(file, ev.kind, ev.code, ev.value));
                    if let Err(e) = result {
                        return (written, Some(format!("{:?} device: {e}", ev.device)));
                    }
                    written = written.saturating_add(1);
                    if ev.kind == EV_SYN {
                        // Give the compositor distinct reports, as real hardware does.
                        std::thread::sleep(std::time::Duration::from_millis(4));
                    }
                }
            }
        }
        (written, None)
    }
}

/// Serve input requests for `seat` on `socket` until killed. The pointer
/// spans `geometry.desktop`, touch and pen `geometry.touch`; without a
/// geometry both span the seat's output.
///
/// # Errors
/// Device creation and socket failures.
pub fn serve(seat: &str, socket: &Path, geometry: Option<Geometry>) -> Result<(), DaemonError> {
    let fallback = || output_size(seat);
    let mut devices = Devices {
        seat: seat.to_owned(),
        desktop: geometry.map_or_else(fallback, |g| g.desktop),
        touch: geometry.map_or_else(fallback, |g| g.touch),
        open: BTreeMap::new(),
    };
    devices.get(Device::Keyboard).map_err(DaemonError::io(
        "create the uinput keyboard (is /dev/uinput there?)",
    ))?;
    devices
        .get(Device::Pointer)
        .map_err(DaemonError::io("create the uinput pointer"))?;
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).map_err(DaemonError::io("create the input socket dir"))?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = std::os::unix::net::UnixListener::bind(socket)
        .map_err(DaemonError::io(format!("listen on {}", socket.display())))?;
    let mut encoder = Encoder::default();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        let (events, error) = match serde_json::from_str::<Request>(&line)
            .map_err(|e| e.to_string())
            .and_then(|request| encoder.encode(&request.actions))
        {
            Ok(steps) => devices.run(&steps),
            Err(error) => (0, Some(error)),
        };
        let reply = Reply {
            seat: seat.to_owned(),
            geometry,
            events,
            error,
        };
        let mut out = serde_json::to_vec(&reply).unwrap_or_default();
        out.push(b'\n');
        let _ = (&stream).write_all(&out);
    }
    Ok(())
}

/// The kernel's uinput interface (`linux/uinput.h`), through ioctls.
#[cfg(target_os = "linux")]
mod uinput {
    use std::fs::{File, OpenOptions};
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    use libc::Ioctl;
    use tilo_vm_protocol::input::{
        ABS_HAT0X, ABS_HAT0Y, ABS_RX, ABS_RY, ABS_RZ, ABS_Z, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT,
        KEY_MAX_USED, gamepad_buttons,
    };

    use super::{
        ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_PRESSURE, ABS_X,
        ABS_Y, BTN_STYLUS, BTN_TOOL_DOUBLETAP, BTN_TOOL_FINGER, BTN_TOOL_PEN, BTN_TOUCH, EV_ABS,
        EV_KEY, EV_SYN, PRESSURE_MAX, STICK_MAX, STICK_MIN, TOUCH_SLOTS, TRIGGER_MAX,
    };

    const fn io(nr: Ioctl) -> Ioctl {
        ((b'U' as Ioctl) << 8) | nr
    }
    const fn iow(nr: Ioctl, size: usize) -> Ioctl {
        (1 << 30) | ((size as Ioctl) << 16) | io(nr)
    }
    const UI_DEV_CREATE: Ioctl = io(1);
    const UI_DEV_SETUP: Ioctl = iow(3, size_of::<Setup>());
    const UI_ABS_SETUP: Ioctl = iow(4, size_of::<AbsSetup>());
    const UI_SET_EVBIT: Ioctl = iow(100, size_of::<libc::c_int>());
    const UI_SET_KEYBIT: Ioctl = iow(101, size_of::<libc::c_int>());
    const UI_SET_ABSBIT: Ioctl = iow(103, size_of::<libc::c_int>());
    const UI_SET_PHYS: Ioctl = iow(108, size_of::<*const libc::c_char>());
    const UI_SET_PROPBIT: Ioctl = iow(110, size_of::<libc::c_int>());
    const BUS_VIRTUAL: u16 = 0x06;
    /// A touchscreen or pen display: positions are on the screen itself.
    const INPUT_PROP_DIRECT: libc::c_int = 0x01;
    /// Units per millimetre on touch and pen axes (about 100 dpi); libinput
    /// needs a resolution to treat a device as a tablet.
    const RESOLUTION: i32 = 4;

    #[repr(C)]
    struct Setup {
        bustype: u16,
        vendor: u16,
        product: u16,
        version: u16,
        name: [u8; 80],
        ff_effects_max: u32,
    }

    #[repr(C)]
    struct AbsSetup {
        code: u16,
        value: i32,
        minimum: i32,
        maximum: i32,
        fuzz: i32,
        flat: i32,
        resolution: i32,
    }

    fn check(rc: libc::c_int) -> std::io::Result<()> {
        if rc < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// An ioctl whose argument is an `int`.
    fn ioctl(file: &File, request: Ioctl, arg: libc::c_int) -> std::io::Result<()> {
        // SAFETY: `request` is a uinput ioctl taking an int by value.
        check(unsafe { libc::ioctl(file.as_raw_fd(), request, arg) })
    }

    /// An ioctl whose argument points at data the kernel reads; the data
    /// outlives the call.
    fn ioctl_ptr(file: &File, request: Ioctl, arg: *const libc::c_void) -> std::io::Result<()> {
        // SAFETY: `request` is a uinput ioctl that reads the struct (or,
        // for UI_SET_PHYS, the NUL-terminated string) `arg` points at.
        check(unsafe { libc::ioctl(file.as_raw_fd(), request, arg) })
    }

    fn open(events: &[u16]) -> std::io::Result<File> {
        let _ = std::process::Command::new("modprobe")
            .arg("uinput")
            .status();
        let file = OpenOptions::new().write(true).open("/dev/uinput")?;
        for kind in events.iter().chain(&[EV_SYN]) {
            ioctl(&file, UI_SET_EVBIT, i32::from(*kind))?;
        }
        Ok(file)
    }

    fn keys(file: &File, codes: impl IntoIterator<Item = u16>) -> std::io::Result<()> {
        for code in codes {
            ioctl(file, UI_SET_KEYBIT, i32::from(code))?;
        }
        Ok(())
    }

    fn axis(
        file: &File,
        code: u16,
        (minimum, maximum): (i32, i32),
        resolution: i32,
    ) -> std::io::Result<()> {
        ioctl(file, UI_SET_ABSBIT, i32::from(code))?;
        let abs = AbsSetup {
            code,
            value: 0,
            minimum,
            maximum,
            fuzz: 0,
            flat: 0,
            resolution,
        };
        ioctl_ptr(file, UI_ABS_SETUP, (&raw const abs).cast())
    }

    fn range(extent: u32) -> (i32, i32) {
        (
            0,
            i32::try_from(extent.saturating_sub(1)).unwrap_or(i32::MAX),
        )
    }

    fn create(file: &File, name: &str, product: u16, phys: &str) -> std::io::Result<()> {
        let phys = std::ffi::CString::new(phys).map_err(std::io::Error::other)?;
        ioctl_ptr(file, UI_SET_PHYS, phys.as_ptr().cast())?;
        let mut setup = Setup {
            bustype: BUS_VIRTUAL,
            vendor: 0x7469,
            product,
            version: 1,
            name: [0; 80],
            ff_effects_max: 0,
        };
        for (slot, byte) in setup.name.iter_mut().zip(name.bytes().take(79)) {
            *slot = byte;
        }
        ioctl_ptr(file, UI_DEV_SETUP, (&raw const setup).cast())?;
        ioctl(file, UI_DEV_CREATE, 0)
    }

    pub fn keyboard(seat: &str) -> std::io::Result<File> {
        let file = open(&[EV_KEY])?;
        keys(&file, 1..=KEY_MAX_USED)?;
        create(&file, &format!("tilo-vm keyboard ({seat})"), 1, seat)?;
        Ok(file)
    }

    pub fn pointer(seat: &str, width: u32, height: u32) -> std::io::Result<File> {
        let file = open(&[EV_KEY, EV_ABS])?;
        keys(&file, [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE])?;
        axis(&file, ABS_X, range(width), 0)?;
        axis(&file, ABS_Y, range(height), 0)?;
        create(&file, &format!("tilo-vm pointer ({seat})"), 2, seat)?;
        Ok(file)
    }

    pub fn touchscreen(seat: &str, width: u32, height: u32) -> std::io::Result<File> {
        let file = open(&[EV_KEY, EV_ABS])?;
        ioctl(&file, UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        keys(&file, [BTN_TOUCH, BTN_TOOL_FINGER, BTN_TOOL_DOUBLETAP])?;
        axis(&file, ABS_X, range(width), RESOLUTION)?;
        axis(&file, ABS_Y, range(height), RESOLUTION)?;
        axis(&file, ABS_MT_SLOT, (0, TOUCH_SLOTS - 1), 0)?;
        axis(&file, ABS_MT_TRACKING_ID, (0, 65_535), 0)?;
        axis(&file, ABS_MT_POSITION_X, range(width), RESOLUTION)?;
        axis(&file, ABS_MT_POSITION_Y, range(height), RESOLUTION)?;
        create(&file, &format!("tilo-vm touchscreen ({seat})"), 3, seat)?;
        Ok(file)
    }

    pub fn pen(seat: &str, width: u32, height: u32) -> std::io::Result<File> {
        let file = open(&[EV_KEY, EV_ABS])?;
        ioctl(&file, UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        keys(&file, [BTN_TOOL_PEN, BTN_TOUCH, BTN_STYLUS])?;
        axis(&file, ABS_X, range(width), RESOLUTION)?;
        axis(&file, ABS_Y, range(height), RESOLUTION)?;
        axis(&file, ABS_PRESSURE, (0, PRESSURE_MAX), 0)?;
        create(&file, &format!("tilo-vm pen ({seat})"), 4, seat)?;
        Ok(file)
    }

    pub fn gamepad(seat: &str) -> std::io::Result<File> {
        let file = open(&[EV_KEY, EV_ABS])?;
        keys(&file, gamepad_buttons())?;
        for code in [ABS_X, ABS_Y, ABS_RX, ABS_RY] {
            axis(&file, code, (STICK_MIN, STICK_MAX), 0)?;
        }
        for code in [ABS_Z, ABS_RZ] {
            axis(&file, code, (0, TRIGGER_MAX), 0)?;
        }
        for code in [ABS_HAT0X, ABS_HAT0Y] {
            axis(&file, code, (-1, 1), 0)?;
        }
        create(&file, &format!("tilo-vm gamepad ({seat})"), 5, seat)?;
        Ok(file)
    }

    /// Write one `struct input_event`; the kernel stamps the time.
    pub fn write(mut file: &File, kind: u16, code: u16, value: i32) -> std::io::Result<()> {
        let mut bytes = vec![0_u8; size_of::<libc::timeval>()];
        bytes.extend(kind.to_ne_bytes());
        bytes.extend(code.to_ne_bytes());
        bytes.extend(value.to_ne_bytes());
        file.write_all(&bytes)
    }
}

#[cfg(not(target_os = "linux"))]
mod uinput {
    use std::fs::File;

    fn unsupported() -> std::io::Error {
        std::io::Error::other("uinput exists only on Linux")
    }
    pub fn keyboard(_seat: &str) -> std::io::Result<File> {
        Err(unsupported())
    }
    pub fn pointer(_seat: &str, _width: u32, _height: u32) -> std::io::Result<File> {
        Err(unsupported())
    }
    pub fn touchscreen(_seat: &str, _width: u32, _height: u32) -> std::io::Result<File> {
        Err(unsupported())
    }
    pub fn pen(_seat: &str, _width: u32, _height: u32) -> std::io::Result<File> {
        Err(unsupported())
    }
    pub fn gamepad(_seat: &str) -> std::io::Result<File> {
        Err(unsupported())
    }
    pub fn write(_file: &File, _kind: u16, _code: u16, _value: i32) -> std::io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use tilo_vm_protocol::input::{ABS_HAT0Y, ABS_RX, ABS_RY, ABS_RZ};
    use tilo_vm_protocol::{Button, Stick};

    use super::*;

    fn emitted(steps: &[Step]) -> Vec<Event> {
        steps.iter().filter_map(|s| s.event()).collect()
    }

    fn on(steps: &[Step], device: Device, kind: u16) -> Vec<(u16, i32)> {
        emitted(steps)
            .into_iter()
            .filter(|e| e.device == device && e.kind == kind)
            .map(|e| (e.code, e.value))
            .collect()
    }

    fn ev(device: Device, kind: u16, code: u16, value: i32) -> Event {
        Event {
            device,
            kind,
            code,
            value,
        }
    }

    #[test]
    fn input_events_press_type_click_and_route_per_slot() {
        // Phase `combos_press_in_order_and_release_in_reverse`.
        {
            let steps = events(&[InputAction::Key {
                combos: vec!["ctrl+t".into()],
            }])
            .unwrap();
            assert_eq!(
                on(&steps, Device::Keyboard, EV_KEY),
                [(29, 1), (20, 1), (20, 0), (29, 0)]
            );
            let evs = emitted(&steps);
            assert!(evs.iter().all(|e| e.device == Device::Keyboard));
            assert_eq!(evs.iter().filter(|e| e.kind == EV_SYN).count(), 4);
        }
        // Phase `typing_holds_shift_only_for_shifted_characters`.
        {
            let steps = events(&[InputAction::Type { text: "aB".into() }]).unwrap();
            assert_eq!(
                on(&steps, Device::Keyboard, EV_KEY),
                [(30, 1), (30, 0), (42, 1), (48, 1), (48, 0), (42, 0)]
            );
        }
        // Phase `clicks_move_first_and_use_the_pointer`.
        {
            let evs = emitted(
                &events(&[InputAction::Click {
                    button: Button::Right,
                    at: Some((10, 20)),
                }])
                .unwrap(),
            );
            assert!(evs.iter().all(|e| e.device == Device::Pointer));
            assert_eq!(evs[0], ev(Device::Pointer, EV_ABS, ABS_X, 10));
            assert_eq!(evs[1], ev(Device::Pointer, EV_ABS, ABS_Y, 20));
            assert!(evs.contains(&ev(Device::Pointer, EV_KEY, 0x111, 1)));
            assert_eq!(evs.last(), Some(&ev(Device::Pointer, EV_SYN, 0, 0)));
        }
        // Phase `modes_parse`.
        {
            assert_eq!(parse_mode("1024x768\n800x600\n"), Some((1024, 768)));
            assert_eq!(parse_mode("1920x1080i\n"), Some((1920, 1080)));
            assert_eq!(parse_mode(""), None);
        }
        // Phase `unit_and_socket_follow_the_slot`.
        {
            assert_eq!(unit_name("a"), "tilo-vm-input-a.service");
            assert_eq!(socket_path("a"), Path::new("/run/tilo-vm-input/a.sock"));
        }
    }

    #[test]
    fn a_tap_is_one_contact_down_then_up() {
        let steps = events(&[InputAction::TouchTap { x: 300, y: 200 }]).unwrap();
        let evs = emitted(&steps);
        assert!(evs.iter().all(|e| e.device == Device::Touch));
        assert_eq!(
            on(&steps, Device::Touch, EV_ABS),
            [
                (ABS_MT_SLOT, 0),
                (ABS_MT_TRACKING_ID, 0),
                (ABS_MT_SLOT, 0),
                (ABS_MT_POSITION_X, 300),
                (ABS_MT_POSITION_Y, 200),
                (ABS_X, 300),
                (ABS_Y, 200),
                (ABS_MT_SLOT, 0),
                (ABS_MT_TRACKING_ID, -1),
            ]
        );
        assert_eq!(
            on(&steps, Device::Touch, EV_KEY),
            [
                (BTN_TOUCH, 1),
                (BTN_TOOL_FINGER, 1),
                (BTN_TOUCH, 0),
                (BTN_TOOL_FINGER, 0)
            ]
        );
        assert!(steps.contains(&Step::WaitMs(TAP_MS)));
    }

    #[test]
    fn tracking_ids_keep_counting_across_contacts() {
        let mut encoder = Encoder::default();
        let ids = |steps: &[Step]| -> Vec<i32> {
            on(steps, Device::Touch, EV_ABS)
                .into_iter()
                .filter(|(code, value)| *code == ABS_MT_TRACKING_ID && *value >= 0)
                .map(|(_, value)| value)
                .collect()
        };
        let first = encoder
            .encode(&[InputAction::TouchTap { x: 1, y: 1 }])
            .unwrap();
        let second = encoder
            .encode(&[InputAction::TouchPinch {
                center: (100, 100),
                from_distance: 40,
                to_distance: 80,
                duration_ms: 20,
            }])
            .unwrap();
        assert_eq!(ids(&first), [0]);
        assert_eq!(ids(&second), [1, 2]);
    }

    #[test]
    fn a_hold_waits_between_down_and_up() {
        let steps = events(&[InputAction::TouchHold {
            x: 5,
            y: 5,
            duration_ms: 800,
        }])
        .unwrap();
        let wait = steps.iter().position(|s| *s == Step::WaitMs(800)).unwrap();
        let up = steps
            .iter()
            .position(|s| *s == event(Device::Touch, EV_ABS, ABS_MT_TRACKING_ID, -1))
            .unwrap();
        assert!(wait < up);
    }

    #[test]
    fn a_drag_moves_in_steps_and_ends_on_target() {
        let steps = events(&[InputAction::TouchDrag {
            from: (0, 0),
            to: (100, 50),
            duration_ms: 100,
        }])
        .unwrap();
        let xs: Vec<i32> = on(&steps, Device::Touch, EV_ABS)
            .into_iter()
            .filter(|(code, _)| *code == ABS_MT_POSITION_X)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(xs, [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        assert_eq!(
            steps
                .iter()
                .filter(|s| matches!(s, Step::WaitMs(_)))
                .count(),
            10
        );
        assert_eq!(path((0, 0), (9, 9), 0), [(9, 9)]);
    }

    #[test]
    fn a_pinch_moves_two_fingers_apart_symmetrically() {
        let steps = events(&[InputAction::TouchPinch {
            center: (400, 300),
            from_distance: 100,
            to_distance: 300,
            duration_ms: 20,
        }])
        .unwrap();
        let abs = on(&steps, Device::Touch, EV_ABS);
        let xs: Vec<i32> = abs
            .iter()
            .filter(|(code, _)| *code == ABS_MT_POSITION_X)
            .map(|(_, v)| *v)
            .collect();
        // Down at 350/450, then 300/500, then 250/550.
        assert_eq!(xs, [350, 450, 300, 500, 250, 550]);
        assert_eq!(
            on(&steps, Device::Touch, EV_KEY),
            [
                (BTN_TOUCH, 1),
                (BTN_TOOL_DOUBLETAP, 1),
                (BTN_TOUCH, 0),
                (BTN_TOOL_DOUBLETAP, 0)
            ]
        );
        let lifted = abs
            .iter()
            .filter(|(code, value)| *code == ABS_MT_TRACKING_ID && *value == -1)
            .count();
        assert_eq!(lifted, 2);
    }

    #[test]
    fn a_pen_comes_into_proximity_before_touching() {
        let steps = events(&[InputAction::PenTap {
            x: 7,
            y: 8,
            pressure: 50,
        }])
        .unwrap();
        let evs = emitted(&steps);
        assert!(evs.iter().all(|e| e.device == Device::Pen));
        assert_eq!(evs[0], ev(Device::Pen, EV_KEY, BTN_TOOL_PEN, 1));
        let touch = evs
            .iter()
            .position(|e| *e == ev(Device::Pen, EV_KEY, BTN_TOUCH, 1))
            .unwrap();
        assert!(evs[..touch].contains(&ev(Device::Pen, EV_SYN, 0, 0)));
        assert!(evs.contains(&ev(Device::Pen, EV_ABS, ABS_PRESSURE, PRESSURE_MAX / 2)));
        assert_eq!(evs[evs.len() - 2], ev(Device::Pen, EV_KEY, BTN_TOOL_PEN, 0));
    }

    #[test]
    fn gamepad_combos_hold_then_release_in_reverse() {
        let steps = events(&[InputAction::GamepadPress {
            combos: vec!["guide+rt".into(), "up".into()],
        }])
        .unwrap();
        let evs: Vec<(u16, u16, i32)> = emitted(&steps)
            .into_iter()
            .filter(|e| e.kind != EV_SYN)
            .map(|e| (e.kind, e.code, e.value))
            .collect();
        assert_eq!(
            evs,
            [
                (EV_KEY, 0x13c, 1),
                (EV_ABS, ABS_RZ, TRIGGER_MAX),
                (EV_ABS, ABS_RZ, 0),
                (EV_KEY, 0x13c, 0),
                (EV_ABS, ABS_HAT0Y, -1),
                (EV_ABS, ABS_HAT0Y, 0),
            ]
        );
        assert!(emitted(&steps).iter().all(|e| e.device == Device::Gamepad));
        assert_eq!(
            steps
                .iter()
                .filter(|s| **s == Step::WaitMs(PRESS_MS))
                .count(),
            4
        );
    }

    #[test]
    fn sticks_deflect_then_spring_back() {
        let steps = events(&[InputAction::GamepadStick {
            stick: Stick::Right,
            x: -100,
            y: 50,
            duration_ms: 250,
        }])
        .unwrap();
        assert_eq!(
            on(&steps, Device::Gamepad, EV_ABS),
            [
                (ABS_RX, STICK_MIN),
                (ABS_RY, 16_383),
                (ABS_RX, 0),
                (ABS_RY, 0)
            ]
        );
        assert!(steps.contains(&Step::WaitMs(250)));
        assert_eq!(stick_value(100), STICK_MAX);
        assert_eq!(stick_value(0), 0);
    }
}
