//! Keys and taps for the boot-failure screen.
//!
//! The screen is drawn by plymouth, before any compositor exists, and plymouth reads a
//! keyboard only. This reads every evdev device instead and prints one line per press or
//! tap, which `tilo-boot-failure` hit-tests against the layout it gave plymouth:
//!
//! ```text
//! key up | key down | key left | key right | key tab | key enter | key space | key 1..4
//! tap 0.5120 0.7340      x and y as fractions of the screen
//! ```
//!
//! A touchscreen or an absolute pointer (a tablet, a VM's tablet device) taps where it is
//! released; a relative mouse does nothing, since the screen draws no pointer. Devices that
//! appear later (a keyboard plugged in once the screen is up) are picked up on the next scan.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const BTN_LEFT: u16 = 0x110;
const BTN_TOUCH: u16 = 0x14a;

const RESCAN: Duration = Duration::from_secs(2);

/// What one key press means on the screen, by Linux key code.
fn key_name(code: u16) -> Option<&'static str> {
    Some(match code {
        103 => "up",
        108 => "down",
        105 => "left",
        106 => "right",
        15 => "tab",
        28 | 96 => "enter",
        57 => "space",
        2 | 79 => "1",
        3 | 80 => "2",
        4 | 81 => "3",
        5 | 75 => "4",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Range {
    min: i32,
    max: i32,
}

impl Range {
    fn fraction(self, value: i32) -> f64 {
        let span = f64::from(self.max) - f64::from(self.min);
        if span <= 0.0 {
            return 0.0;
        }
        ((f64::from(value) - f64::from(self.min)) / span).clamp(0.0, 1.0)
    }
}

/// The axes a device reports positions on: multitouch slots or a single absolute pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Axes {
    x_code: u16,
    y_code: u16,
    x: Range,
    y: Range,
}

/// One device's state between reports.
#[derive(Debug)]
struct Pointer {
    axes: Option<Axes>,
    x: Option<i32>,
    y: Option<i32>,
    released: bool,
}

impl Pointer {
    fn new(axes: Option<Axes>) -> Self {
        Self {
            axes,
            x: None,
            y: None,
            released: false,
        }
    }

    /// The line one event completes, if any.
    fn event(&mut self, kind: u16, code: u16, value: i32) -> Option<String> {
        match kind {
            EV_KEY if code == BTN_TOUCH || code == BTN_LEFT => {
                if value == 0 {
                    self.released = true;
                }
                None
            }
            EV_KEY if value == 1 => key_name(code).map(|name| format!("key {name}")),
            EV_ABS => {
                let axes = self.axes?;
                if code == axes.x_code {
                    self.x = Some(value);
                } else if code == axes.y_code {
                    self.y = Some(value);
                }
                None
            }
            EV_SYN if code == SYN_REPORT && self.released => {
                self.released = false;
                let axes = self.axes?;
                let (x, y) = (self.x?, self.y?);
                Some(format!(
                    "tap {:.4} {:.4}",
                    axes.x.fraction(x),
                    axes.y.fraction(y)
                ))
            }
            _ => None,
        }
    }
}

// The evdev ioctl numbers, as <linux/input.h> builds them.
const IOC_READ: libc::c_ulong = 2;
const fn ioc(dir: libc::c_ulong, nr: libc::c_ulong, size: libc::c_ulong) -> libc::c_ulong {
    (dir << 30) | (size << 16) | ((b'E' as libc::c_ulong) << 8) | nr
}
// The numbers are `0x20 | ev` and `0x40 | abs`: event types stay below 0x20 and absolute
// axes below 0x40, so the or is the C macros' addition.
const fn eviocgbit(ev: u16, len: usize) -> libc::c_ulong {
    ioc(IOC_READ, 0x20 | ev as libc::c_ulong, len as libc::c_ulong)
}
const fn eviocgabs(abs: u16) -> libc::c_ulong {
    ioc(
        IOC_READ,
        0x40 | abs as libc::c_ulong,
        std::mem::size_of::<libc::input_absinfo>() as libc::c_ulong,
    )
}

fn bits(fd: &OwnedFd, ev: u16) -> [u8; 96] {
    let mut buf = [0u8; 96];
    // SAFETY: EVIOCGBIT writes at most `len` bytes into the buffer it is given.
    let rc = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            eviocgbit(ev, buf.len()) as _,
            buf.as_mut_ptr(),
        )
    };
    if rc < 0 { [0u8; 96] } else { buf }
}

fn has(bits: &[u8; 96], code: u16) -> bool {
    bits.get(usize::from(code / 8))
        .is_some_and(|byte| byte & (1 << (code % 8)) != 0)
}

fn range(fd: &OwnedFd, code: u16) -> Option<Range> {
    // SAFETY: input_absinfo is plain integers; all-zero is a valid value.
    let mut info: libc::input_absinfo = unsafe { std::mem::zeroed() };
    // SAFETY: EVIOCGABS fills exactly one input_absinfo.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), eviocgabs(code) as _, &mut info) };
    (rc == 0 && info.maximum > info.minimum).then_some(Range {
        min: info.minimum,
        max: info.maximum,
    })
}

fn axes(fd: &OwnedFd) -> Option<Axes> {
    let abs = bits(fd, EV_ABS);
    let pair = if has(&abs, ABS_MT_POSITION_X) && has(&abs, ABS_MT_POSITION_Y) {
        (ABS_MT_POSITION_X, ABS_MT_POSITION_Y)
    } else if has(&abs, ABS_X) && has(&abs, ABS_Y) {
        (ABS_X, ABS_Y)
    } else {
        return None;
    };
    Some(Axes {
        x_code: pair.0,
        y_code: pair.1,
        x: range(fd, pair.0)?,
        y: range(fd, pair.1)?,
    })
}

fn open(path: &Path) -> Option<OwnedFd> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: a valid NUL-terminated path; the descriptor is owned from here on.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    // SAFETY: open returned a fresh descriptor that nothing else owns.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

struct Device {
    fd: OwnedFd,
    pointer: Pointer,
}

fn scan(dir: &Path, devices: &mut BTreeMap<PathBuf, Device>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || devices.contains_key(&path) {
            continue;
        }
        if let Some(fd) = open(&path) {
            let pointer = Pointer::new(axes(&fd));
            devices.insert(path, Device { fd, pointer });
        }
    }
}

const EVENT_SIZE: usize = std::mem::size_of::<libc::input_event>();

/// Every whole event in `buf`, as (type, code, value).
/// The last 8 bytes of a kernel `input_event`: type, code, value.
fn events(buf: &[u8]) -> impl Iterator<Item = (u16, u16, i32)> + '_ {
    buf.chunks_exact(EVENT_SIZE).filter_map(|raw| {
        let tail: &[u8; 8] = raw.last_chunk()?;
        let [t0, t1, c0, c1, v0, v1, v2, v3] = *tail;
        Some((
            u16::from_ne_bytes([t0, t1]),
            u16::from_ne_bytes([c0, c1]),
            i32::from_ne_bytes([v0, v1, v2, v3]),
        ))
    })
}

fn main() {
    let dir = PathBuf::from(
        std::env::var_os("TILO_BOOT_FAILURE_INPUT_DIR").unwrap_or_else(|| "/dev/input".into()),
    );
    let mut devices = BTreeMap::new();
    let mut scanned: Option<Instant> = None;
    let mut out = std::io::stdout().lock();
    let mut buf = vec![0u8; EVENT_SIZE * 64];
    loop {
        if scanned.is_none_or(|at| at.elapsed() >= RESCAN) {
            scan(&dir, &mut devices);
            scanned = Some(Instant::now());
        }
        let paths: Vec<PathBuf> = devices.keys().cloned().collect();
        let mut fds: Vec<libc::pollfd> = devices
            .values()
            .map(|device| libc::pollfd {
                fd: device.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is a live, correctly sized array of pollfd for the call's duration.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 500) };
        if ready <= 0 {
            continue;
        }
        for (pollfd, path) in fds.iter().zip(&paths) {
            if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                devices.remove(path);
                continue;
            }
            if pollfd.revents & libc::POLLIN == 0 {
                continue;
            }
            let Some(device) = devices.get_mut(path) else {
                continue;
            };
            // SAFETY: reads into an owned buffer of the length given.
            let n =
                unsafe { libc::read(device.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            let Ok(n) = usize::try_from(n) else {
                continue;
            };
            for (kind, code, value) in events(buf.get(..n).unwrap_or_default()) {
                if let Some(line) = device.pointer.event(kind, code, value)
                    && (writeln!(out, "{line}").is_err() || out.flush().is_err())
                {
                    // Whoever reads us is gone; so is the screen.
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tablet() -> Pointer {
        Pointer::new(Some(Axes {
            x_code: ABS_X,
            y_code: ABS_Y,
            x: Range { min: 0, max: 32767 },
            y: Range { min: 0, max: 32767 },
        }))
    }

    #[test]
    fn a_key_press_is_one_line_and_its_release_none() {
        let mut keyboard = Pointer::new(None);
        assert_eq!(keyboard.event(EV_KEY, 28, 1).as_deref(), Some("key enter"));
        assert_eq!(keyboard.event(EV_KEY, 28, 0), None);
        assert_eq!(
            keyboard.event(EV_KEY, 108, 2),
            None,
            "autorepeat is not a press"
        );
        assert_eq!(keyboard.event(EV_KEY, 3, 1).as_deref(), Some("key 2"));
        assert_eq!(
            keyboard.event(EV_KEY, 30, 1),
            None,
            "a key the screen has no use for"
        );
    }

    #[test]
    fn a_tap_is_where_the_touch_is_released_as_screen_fractions() {
        let mut touch = tablet();
        assert_eq!(touch.event(EV_KEY, BTN_TOUCH, 1), None);
        assert_eq!(touch.event(EV_ABS, ABS_X, 16384), None);
        assert_eq!(touch.event(EV_ABS, ABS_Y, 8192), None);
        assert_eq!(touch.event(EV_SYN, SYN_REPORT, 0), None, "still down");
        assert_eq!(touch.event(EV_KEY, BTN_TOUCH, 0), None);
        assert_eq!(
            touch.event(EV_SYN, SYN_REPORT, 0).as_deref(),
            Some("tap 0.5000 0.2500")
        );
        assert_eq!(
            touch.event(EV_SYN, SYN_REPORT, 0),
            None,
            "one tap per release"
        );
    }

    #[test]
    fn a_pointer_click_on_an_absolute_device_is_a_tap() {
        let mut pointer = tablet();
        pointer.event(EV_ABS, ABS_X, 32767);
        pointer.event(EV_ABS, ABS_Y, 0);
        pointer.event(EV_KEY, BTN_LEFT, 0);
        assert_eq!(
            pointer.event(EV_SYN, SYN_REPORT, 0).as_deref(),
            Some("tap 1.0000 0.0000")
        );
    }

    #[test]
    fn a_device_without_absolute_axes_never_taps() {
        let mut mouse = Pointer::new(None);
        mouse.event(EV_KEY, BTN_LEFT, 0);
        assert_eq!(mouse.event(EV_SYN, SYN_REPORT, 0), None);
    }

    #[test]
    fn events_are_read_from_the_kernel_layout() {
        let mut raw = vec![0u8; EVENT_SIZE.saturating_sub(8)];
        raw.extend_from_slice(&EV_KEY.to_ne_bytes());
        raw.extend_from_slice(&28u16.to_ne_bytes());
        raw.extend_from_slice(&1i32.to_ne_bytes());
        raw.extend(std::iter::repeat_n(0u8, EVENT_SIZE));
        let parsed: Vec<_> = events(&raw).collect();
        assert_eq!(parsed, vec![(EV_KEY, 28, 1), (0, 0, 0)]);
    }

    #[test]
    fn ioctl_numbers_match_linux_input_h() {
        // EVIOCGBIT(EV_ABS, 96) and EVIOCGABS(ABS_MT_POSITION_X) as the C macros give them.
        assert_eq!(eviocgbit(EV_ABS, 96), 0x8060_4523);
        assert_eq!(eviocgabs(ABS_MT_POSITION_X), 0x8018_4575);
    }
}
