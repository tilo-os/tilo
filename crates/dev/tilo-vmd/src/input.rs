//! Per-seat uinput devices for headful slots.
//!
//! A uinput device lives as long as the file descriptor that created it,
//! and every `tilo-vmd rpc` is a short-lived process, so the devices are
//! owned by `tilo-vmd input-serve`, a transient system unit
//! (`tilo-vm-input-<slot>.service`, started with `systemd-run`). It creates
//! one keyboard and one absolute pointer whose `phys` is the slot's logind
//! seat; `73-uinput-seat.rules` tags them `ID_SEAT=<phys>`, so the slot's
//! compositor gets them through libinput like hardware plugged into its
//! seat, and no other seat sees them. `rpc` sends it actions over a Unix
//! socket, one JSON line per request.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::InputAction;
use tilo_vm_protocol::input::{KEY_LEFTSHIFT, combo_codes, text_strokes};

use crate::error::DaemonError;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_ABS: u16 = 3;
pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;

/// Which device an event goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Keyboard,
    Pointer,
}

/// One kernel input event, before its timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub device: Device,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

const fn event(device: Device, kind: u16, code: u16, value: i32) -> Event {
    Event {
        device,
        kind,
        code,
        value,
    }
}

const fn syn(device: Device) -> Event {
    event(device, EV_SYN, 0, 0)
}

/// A key press or release followed by its report.
fn key(out: &mut Vec<Event>, code: u16, down: bool) {
    out.push(event(Device::Keyboard, EV_KEY, code, i32::from(down)));
    out.push(syn(Device::Keyboard));
}

fn pointer_to(out: &mut Vec<Event>, x: u32, y: u32) {
    let clamp = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    out.push(event(Device::Pointer, EV_ABS, ABS_X, clamp(x)));
    out.push(event(Device::Pointer, EV_ABS, ABS_Y, clamp(y)));
    out.push(syn(Device::Pointer));
}

/// The kernel events for `actions`, each one a report of its own so the
/// compositor sees presses and releases in order.
///
/// # Errors
/// A combo or character that has no key (callers validate first).
pub fn events(actions: &[InputAction]) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    for action in actions {
        match action {
            InputAction::Key { combos } => {
                for combo in combos {
                    let codes = combo_codes(combo)?;
                    for code in &codes {
                        key(&mut out, *code, true);
                    }
                    for code in codes.iter().rev() {
                        key(&mut out, *code, false);
                    }
                }
            }
            InputAction::Type { text } => {
                let strokes = text_strokes(text).map_err(String::from)?;
                for stroke in strokes {
                    if stroke.shift {
                        key(&mut out, KEY_LEFTSHIFT, true);
                    }
                    key(&mut out, stroke.code, true);
                    key(&mut out, stroke.code, false);
                    if stroke.shift {
                        key(&mut out, KEY_LEFTSHIFT, false);
                    }
                }
            }
            InputAction::Move { x, y } => pointer_to(&mut out, *x, *y),
            InputAction::Click { button, at } => {
                if let Some((x, y)) = at {
                    pointer_to(&mut out, *x, *y);
                }
                out.push(event(Device::Pointer, EV_KEY, button.code(), 1));
                out.push(syn(Device::Pointer));
                out.push(event(Device::Pointer, EV_KEY, button.code(), 0));
                out.push(syn(Device::Pointer));
            }
        }
    }
    Ok(out)
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
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
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

/// Serve input requests for `seat` on `socket` until killed.
///
/// # Errors
/// Device creation and socket failures.
pub fn serve(seat: &str, socket: &Path) -> Result<(), DaemonError> {
    let (width, height) = output_size(seat);
    let keyboard = uinput::keyboard(seat).map_err(DaemonError::io(
        "create the uinput keyboard (is /dev/uinput there?)",
    ))?;
    let pointer = uinput::pointer(seat, width, height)
        .map_err(DaemonError::io("create the uinput pointer"))?;
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).map_err(DaemonError::io("create the input socket dir"))?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = std::os::unix::net::UnixListener::bind(socket)
        .map_err(DaemonError::io(format!("listen on {}", socket.display())))?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line)
            .map_err(|e| e.to_string())
            .and_then(|request| events(&request.actions))
        {
            Ok(events) => {
                let mut written: u64 = 0;
                let mut error = None;
                for ev in &events {
                    let device = match ev.device {
                        Device::Keyboard => &keyboard,
                        Device::Pointer => &pointer,
                    };
                    if let Err(e) = uinput::write(device, ev.kind, ev.code, ev.value) {
                        error = Some(e.to_string());
                        break;
                    }
                    written = written.saturating_add(1);
                    if ev.kind == EV_SYN {
                        // Give the compositor distinct reports, as real hardware does.
                        std::thread::sleep(std::time::Duration::from_millis(4));
                    }
                }
                Reply {
                    seat: seat.to_owned(),
                    events: written,
                    error,
                }
            }
            Err(error) => Reply {
                seat: seat.to_owned(),
                events: 0,
                error: Some(error),
            },
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
    use tilo_vm_protocol::input::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, KEY_MAX_USED};

    use super::{ABS_X, ABS_Y, EV_ABS, EV_KEY, EV_SYN};

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
    const BUS_VIRTUAL: u16 = 0x06;

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

    fn open() -> std::io::Result<File> {
        let _ = std::process::Command::new("modprobe")
            .arg("uinput")
            .status();
        OpenOptions::new().write(true).open("/dev/uinput")
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
        let file = open()?;
        ioctl(&file, UI_SET_EVBIT, i32::from(EV_KEY))?;
        ioctl(&file, UI_SET_EVBIT, i32::from(EV_SYN))?;
        for code in 1..=KEY_MAX_USED {
            ioctl(&file, UI_SET_KEYBIT, i32::from(code))?;
        }
        create(&file, &format!("tilo-vm keyboard ({seat})"), 1, seat)?;
        Ok(file)
    }

    pub fn pointer(seat: &str, width: u32, height: u32) -> std::io::Result<File> {
        let file = open()?;
        ioctl(&file, UI_SET_EVBIT, i32::from(EV_KEY))?;
        ioctl(&file, UI_SET_EVBIT, i32::from(EV_ABS))?;
        ioctl(&file, UI_SET_EVBIT, i32::from(EV_SYN))?;
        for button in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
            ioctl(&file, UI_SET_KEYBIT, i32::from(button))?;
        }
        for (code, extent) in [(ABS_X, width), (ABS_Y, height)] {
            ioctl(&file, UI_SET_ABSBIT, i32::from(code))?;
            let abs = AbsSetup {
                code,
                value: 0,
                minimum: 0,
                maximum: i32::try_from(extent.saturating_sub(1)).unwrap_or(i32::MAX),
                fuzz: 0,
                flat: 0,
                resolution: 0,
            };
            ioctl_ptr(&file, UI_ABS_SETUP, (&raw const abs).cast())?;
        }
        create(&file, &format!("tilo-vm pointer ({seat})"), 2, seat)?;
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
    pub fn write(_file: &File, _kind: u16, _code: u16, _value: i32) -> std::io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use tilo_vm_protocol::Button;

    use super::*;

    #[test]
    fn combos_press_in_order_and_release_in_reverse() {
        let evs = events(&[InputAction::Key {
            combos: vec!["ctrl+t".into()],
        }])
        .unwrap();
        let keys: Vec<(u16, i32)> = evs
            .iter()
            .filter(|e| e.kind == EV_KEY)
            .map(|e| (e.code, e.value))
            .collect();
        assert_eq!(keys, [(29, 1), (20, 1), (20, 0), (29, 0)]);
        assert!(evs.iter().all(|e| e.device == Device::Keyboard));
        assert_eq!(evs.iter().filter(|e| e.kind == EV_SYN).count(), 4);
    }

    #[test]
    fn typing_holds_shift_only_for_shifted_characters() {
        let evs = events(&[InputAction::Type { text: "aB".into() }]).unwrap();
        let keys: Vec<(u16, i32)> = evs
            .iter()
            .filter(|e| e.kind == EV_KEY)
            .map(|e| (e.code, e.value))
            .collect();
        assert_eq!(keys, [(30, 1), (30, 0), (42, 1), (48, 1), (48, 0), (42, 0)]);
    }

    #[test]
    fn clicks_move_first_and_use_the_pointer() {
        let evs = events(&[InputAction::Click {
            button: Button::Right,
            at: Some((10, 20)),
        }])
        .unwrap();
        assert!(evs.iter().all(|e| e.device == Device::Pointer));
        assert_eq!(evs[0], event(Device::Pointer, EV_ABS, ABS_X, 10));
        assert_eq!(evs[1], event(Device::Pointer, EV_ABS, ABS_Y, 20));
        assert!(evs.contains(&event(Device::Pointer, EV_KEY, 0x111, 1)));
        assert_eq!(evs.last(), Some(&syn(Device::Pointer)));
    }

    #[test]
    fn modes_parse() {
        assert_eq!(parse_mode("1024x768\n800x600\n"), Some((1024, 768)));
        assert_eq!(parse_mode("1920x1080i\n"), Some((1920, 1080)));
        assert_eq!(parse_mode(""), None);
    }

    #[test]
    fn unit_and_socket_follow_the_slot() {
        assert_eq!(unit_name("a"), "tilo-vm-input-a.service");
        assert_eq!(socket_path("a"), Path::new("/run/tilo-vm-input/a.sock"));
    }
}
