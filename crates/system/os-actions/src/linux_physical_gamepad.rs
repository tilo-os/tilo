//! Strict Linux evdev source for locally attached game controllers.
//!
//! Raw evdev details stop at this module boundary. Callers can discover an
//! opaque local device id and receive only the fixed [`ForwardedGamepadEvent`]
//! vocabulary used by Tilo's peripheral-forwarding broker.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _};
use std::mem::size_of;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::slice;

use crate::linux_gamepad_evdev::{self as evdev, ABS_HAT0X, ABS_HAT0Y, EV_ABS, EV_KEY, EV_SYN};
use crate::linux_virtual_gamepad::{FORWARDED_GAMEPAD_PRODUCT, FORWARDED_GAMEPAD_VENDOR};
use crate::{ForwardedGamepadEvent, GamepadAxis};

const SYN_DROPPED: u16 = 3;

// Nintendo's Joy-Con halves as the kernel hid-nintendo driver exposes them.
// joycond claims exactly these (os/usr/lib/udev/rules.d/72-joycond.rules).
const NINTENDO_VENDOR: u16 = 0x057e;
const JOYCON_LEFT_PRODUCT: u16 = 0x2006;
const JOYCON_RIGHT_PRODUCT: u16 = 0x2007;
const JOYCON_GRIP_PRODUCT: u16 = 0x200e;
const JOYCOND_UDEV_TAG: &str = "joycond";

/// Opaque device-local identifier. It is never serialized or sent to a peer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalGamepadId(PathBuf);

/// A controller approved by the strict local udev classifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadInfo {
    pub id: PhysicalGamepadId,
    pub name: String,
    /// USB/Bluetooth vendor id, when the kernel reports one. It names the
    /// printed button legends more reliably than the product string.
    pub vendor_id: Option<u16>,
}

/// Production discovery roots. Custom roots exist only to make classification
/// independently testable without real input hardware.
#[derive(Clone, Debug)]
pub struct LinuxPhysicalGamepadCatalog {
    input: PathBuf,
    udev_data: PathBuf,
    sys_class_input: PathBuf,
}

impl Default for LinuxPhysicalGamepadCatalog {
    fn default() -> Self {
        Self {
            input: PathBuf::from("/dev/input"),
            udev_data: PathBuf::from("/run/udev/data"),
            sys_class_input: PathBuf::from("/sys/class/input"),
        }
    }
}

impl LinuxPhysicalGamepadCatalog {
    /// Enumerate only evdev character devices classified exclusively as a
    /// joystick/game controller by udev.
    ///
    /// # Errors
    ///
    /// Returns an error when the local input directory cannot be enumerated.
    pub fn discover(&self) -> io::Result<Vec<PhysicalGamepadInfo>> {
        let mut controllers = Vec::new();
        let entries = match fs::read_dir(&self.input) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(controllers),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_name.starts_with("event") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.file_type().is_char_device() {
                continue;
            }
            let Some(udev_properties) = self.controller_udev_properties(metadata.rdev()) else {
                continue;
            };
            let device = self.sys_class_input.join(file_name).join("device");
            let vendor_id = read_hex_id(&device.join("id/vendor"));
            let product_id = read_hex_id(&device.join("id/product"));
            let descriptor = EvdevDescriptor {
                vendor_id,
                product_id,
                udev_properties: &udev_properties,
                mode: metadata.mode(),
            };
            if !is_room_controller(&descriptor) {
                continue;
            }
            let name = fs::read_to_string(device.join("name"))
                .ok()
                .and_then(|name| sanitize_local_name(&name))
                .unwrap_or_else(|| "Game controller".into());
            controllers.push(PhysicalGamepadInfo {
                id: PhysicalGamepadId(path),
                name,
                vendor_id,
            });
        }
        controllers.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(controllers)
    }

    /// Open a previously discovered controller, rechecking its identity and
    /// udev classification to close replacement races.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is outside the catalog, no longer names an
    /// exclusive controller, cannot be opened, or has invalid axis metadata.
    pub fn open(&self, id: &PhysicalGamepadId) -> io::Result<LinuxPhysicalGamepadSource> {
        if id.0.parent() != Some(self.input.as_path())
            || !id
                .0
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "gamepad id is outside the local evdev catalog",
            ));
        }
        let metadata = fs::metadata(&id.0)?;
        if !metadata.file_type().is_char_device() || !self.is_controller_rdev(metadata.rdev()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "input device is not an exclusive game controller",
            ));
        }
        LinuxPhysicalGamepadSource::open(&id.0)
    }

    /// Watch udev's device database, which is written once a newly attached
    /// input device has been classified, so callers rescan on hotplug
    /// instead of polling.
    ///
    /// # Errors
    ///
    /// Returns an error when inotify is unavailable or the udev database
    /// directory cannot be watched.
    pub fn hotplug_watch(&self) -> io::Result<PhysicalGamepadHotplug> {
        use rustix::fs::inotify;
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
        inotify::add_watch(
            &fd,
            &self.udev_data,
            inotify::WatchFlags::CLOSE_WRITE
                | inotify::WatchFlags::MOVED_TO
                | inotify::WatchFlags::DELETE,
        )?;
        Ok(PhysicalGamepadHotplug { fd })
    }

    fn is_controller_rdev(&self, rdev: u64) -> bool {
        self.controller_udev_properties(rdev).is_some()
    }

    /// The udev database record of an exclusive controller, or `None` when
    /// the device is not one.
    fn controller_udev_properties(&self, rdev: u64) -> Option<String> {
        let major = libc::major(rdev as libc::dev_t);
        let minor = libc::minor(rdev as libc::dev_t);
        let path = self.udev_data.join(format!("c{major}:{minor}"));
        fs::read_to_string(path)
            .ok()
            .filter(|properties| is_exclusive_controller(properties))
    }
}

/// Nonblocking readiness source for input-device hotplug.
pub struct PhysicalGamepadHotplug {
    fd: OwnedFd,
}

impl PhysicalGamepadHotplug {
    /// A duplicate of the watch descriptor for an event loop to poll.
    ///
    /// # Errors
    ///
    /// Returns an error when the descriptor cannot be duplicated.
    pub fn wake_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// Consume every queued notification. Returns whether any of them
    /// concerned an input device (udev records `c13:<minor>` for evdev).
    pub fn take_changes(&mut self) -> bool {
        use rustix::fs::inotify;
        let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buffer);
        let mut changed = false;
        loop {
            match reader.next() {
                Ok(event) => {
                    changed |= event
                        .file_name()
                        .is_some_and(|name| name.to_bytes().starts_with(b"c13:"));
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return changed,
            }
        }
    }
}

/// Nonblocking typed reader for one classified physical controller.
pub struct LinuxPhysicalGamepadSource {
    file: File,
    calibrations: BTreeMap<u16, AxisCalibration>,
    hat_x: i8,
    hat_y: i8,
}

/// One bounded nonblocking read. `saturated` means the raw kernel-event
/// budget was fully consumed and more input may remain queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadDrain {
    pub events: Vec<ForwardedGamepadEvent>,
    pub saturated: bool,
}

impl LinuxPhysicalGamepadSource {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let mut calibrations = BTreeMap::new();
        for axis in evdev::AXES {
            let code = evdev::axis_code(axis);
            match query_abs_info(file.as_raw_fd(), code) {
                Ok(info) => {
                    calibrations.insert(
                        code,
                        AxisCalibration::from_abs_info(info, AxisKind::of(axis))?,
                    );
                }
                Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTTY)) => {
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Self {
            file,
            calibrations,
            hat_x: 0,
            hat_y: 0,
        })
    }

    /// A duplicate of the evdev descriptor for an event loop to poll. Reads
    /// still go through [`Self::drain`].
    ///
    /// # Errors
    ///
    /// Returns an error when the descriptor cannot be duplicated.
    pub fn wake_fd(&self) -> io::Result<OwnedFd> {
        self.file.as_fd().try_clone_to_owned()
    }

    /// Consume at most `limit` raw kernel events. Unknown event types and
    /// codes are discarded inside this module and cannot cross the API.
    /// `SYN_DROPPED`, short reads, EOF and I/O errors fail closed so the
    /// caller can revoke the forwarding lease and neutralize the target.
    pub fn drain(&mut self, limit: usize) -> io::Result<PhysicalGamepadDrain> {
        let mut events = Vec::with_capacity(limit.min(32));
        let mut consumed = 0;
        while consumed < limit {
            let Some(raw) = read_event(&mut self.file)? else {
                break;
            };
            consumed = consumed.saturating_add(1);
            match (raw.type_, raw.code) {
                (EV_SYN, SYN_DROPPED) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "kernel dropped physical gamepad events",
                    ));
                }
                (EV_KEY, code) => {
                    if let Some(button) = evdev::button(code)
                        && matches!(raw.value, 0 | 1)
                    {
                        events.push(ForwardedGamepadEvent::Button {
                            button,
                            pressed: raw.value == 1,
                        });
                    }
                }
                (EV_ABS, ABS_HAT0X) => {
                    self.hat_x = raw.value.signum() as i8;
                    events.push(ForwardedGamepadEvent::Hat {
                        x: self.hat_x,
                        y: self.hat_y,
                    });
                }
                (EV_ABS, ABS_HAT0Y) => {
                    self.hat_y = raw.value.signum() as i8;
                    events.push(ForwardedGamepadEvent::Hat {
                        x: self.hat_x,
                        y: self.hat_y,
                    });
                }
                (EV_ABS, code) => {
                    let Some(axis) = evdev::axis(code) else {
                        continue;
                    };
                    let Some(calibration) = self.calibrations.get(&code) else {
                        continue;
                    };
                    events.push(ForwardedGamepadEvent::Axis {
                        axis,
                        value: calibration.normalize(raw.value),
                    });
                }
                _ => {}
            }
        }
        Ok(PhysicalGamepadDrain {
            events,
            saturated: consumed == limit && limit != 0,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AxisKind {
    Stick,
    Trigger,
}

impl AxisKind {
    fn of(axis: GamepadAxis) -> Self {
        match axis {
            GamepadAxis::LeftTrigger | GamepadAxis::RightTrigger => Self::Trigger,
            GamepadAxis::LeftX | GamepadAxis::LeftY | GamepadAxis::RightX | GamepadAxis::RightY => {
                Self::Stick
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AxisCalibration {
    minimum: i32,
    maximum: i32,
    center: i32,
    flat: i32,
    kind: AxisKind,
}

impl AxisCalibration {
    fn from_abs_info(info: libc::input_absinfo, kind: AxisKind) -> io::Result<Self> {
        if info.maximum <= info.minimum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "physical gamepad axis has invalid calibration",
            ));
        }
        Ok(Self {
            minimum: info.minimum,
            maximum: info.maximum,
            center: midpoint(info.minimum, info.maximum),
            flat: info.flat.max(0),
            kind,
        })
    }

    fn normalize(self, value: i32) -> i16 {
        let value = value.clamp(self.minimum, self.maximum);
        match self.kind {
            AxisKind::Trigger => scale(value, self.minimum, self.maximum, 0, i32::from(i16::MAX)),
            AxisKind::Stick if value.abs_diff(self.center) <= self.flat.cast_unsigned() => 0,
            AxisKind::Stick if value < self.center => {
                scale(value, self.minimum, self.center, i32::from(i16::MIN), 0)
            }
            AxisKind::Stick => scale(value, self.center, self.maximum, 0, i32::from(i16::MAX)),
        }
    }
}

fn midpoint(minimum: i32, maximum: i32) -> i32 {
    i32::try_from(i64::midpoint(i64::from(minimum), i64::from(maximum))).unwrap_or_default()
}

fn scale(value: i32, from_min: i32, from_max: i32, to_min: i32, to_max: i32) -> i16 {
    let numerator = i64::from(value)
        .saturating_sub(i64::from(from_min))
        .saturating_mul(i64::from(to_max).saturating_sub(i64::from(to_min)));
    let denominator = i64::from(from_max).saturating_sub(i64::from(from_min));
    let mapped = i64::from(to_min).saturating_add(numerator.checked_div(denominator).unwrap_or(0));
    i16::try_from(mapped.clamp(i64::from(i16::MIN), i64::from(i16::MAX))).unwrap_or_default()
}

fn is_exclusive_controller(properties: &str) -> bool {
    let mut joystick = false;
    let mut forbidden = false;
    for line in properties.lines() {
        let property = line.strip_prefix("E:").unwrap_or(line);
        match property {
            "ID_INPUT_JOYSTICK=1" => joystick = true,
            "ID_INPUT_KEYBOARD=1"
            | "ID_INPUT_MOUSE=1"
            | "ID_INPUT_TOUCHPAD=1"
            | "ID_INPUT_TOUCHSCREEN=1"
            | "ID_INPUT_TABLET=1" => forbidden = true,
            _ => {}
        }
    }
    joystick && !forbidden
}

/// What discovery knows about one exclusive-controller evdev node.
struct EvdevDescriptor<'a> {
    vendor_id: Option<u16>,
    product_id: Option<u16>,
    /// The node's udev database record (`E:` properties, `G:`/`Q:` tags).
    udev_properties: &'a str,
    /// The node's `st_mode`.
    mode: u32,
}

/// Whether a classified controller is one a person in this room holds, as
/// opposed to another component's input.
fn is_room_controller(device: &EvdevDescriptor<'_>) -> bool {
    // A granted lease re-creates a remote controller here for one app. It is
    // that app's input, never a controller in this room.
    !is_forwarded_gamepad(device.vendor_id, device.product_id) && !held_by_joycond(device)
}

/// joycond grabs every raw Joy-Con half udev tags for it (on the device's
/// "add" event) and keeps the node root-only until the person decides how to
/// hold it. Pressing both halves' shoulder buttons folds them into one uinput
/// "Nintendo Switch Combined Joy-Cons" device (057e:2008, virtual bus) while
/// both halves stay grabbed. A half used on its own is released: joycond opens
/// its node to everyone and re-announces it with a "change" event, whose rule
/// pass no longer applies the tag. Only the combined device and released
/// halves are controllers; a held half yields no events, and listing it would
/// count one pair as three.
fn held_by_joycond(device: &EvdevDescriptor<'_>) -> bool {
    let joycon_half = device.vendor_id == Some(NINTENDO_VENDOR)
        && matches!(
            device.product_id,
            Some(JOYCON_LEFT_PRODUCT | JOYCON_RIGHT_PRODUCT | JOYCON_GRIP_PRODUCT)
        );
    let opened_to_everyone = device.mode & 0o006 == 0o006;
    joycon_half
        && has_current_udev_tag(device.udev_properties, JOYCOND_UDEV_TAG)
        && !opened_to_everyone
}

/// Whether udev's latest rule pass applied `tag`. The database lists those
/// tags as `Q:` lines and every tag the device ever had as sticky `G:` lines;
/// a record without `Q:` lines predates that split, so `G:` is all there is.
fn has_current_udev_tag(properties: &str, tag: &str) -> bool {
    let tags = |prefix: &str| {
        properties
            .lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .collect::<Vec<_>>()
    };
    let current = tags("Q:");
    let tags = if current.is_empty() {
        tags("G:")
    } else {
        current
    };
    tags.contains(&tag)
}

fn is_forwarded_gamepad(vendor: Option<u16>, product: Option<u16>) -> bool {
    vendor == Some(FORWARDED_GAMEPAD_VENDOR) && product == Some(FORWARDED_GAMEPAD_PRODUCT)
}

fn read_hex_id(path: &Path) -> Option<u16> {
    let text = fs::read_to_string(path).ok()?;
    u16::from_str_radix(text.trim(), 16).ok()
}

fn sanitize_local_name(name: &str) -> Option<String> {
    let name: String = name
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect();
    (!name.is_empty()).then_some(name)
}

fn query_abs_info(fd: libc::c_int, code: u16) -> io::Result<libc::input_absinfo> {
    // SAFETY: `input_absinfo` is a C POD with a valid all-zero state. The
    // request is the kernel EVIOCGABS read ioctl for one fixed axis code, and
    // the pointer remains valid and uniquely borrowed for the call.
    let mut info: libc::input_absinfo = unsafe { std::mem::zeroed() };
    let number = u8::try_from(0x40_u16.saturating_add(code))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "axis outside EVIOCGABS"))?;
    let request = rustix::ioctl::opcode::read::<libc::input_absinfo>(b'E', number);
    let result = unsafe { libc::ioctl(fd, request as libc::Ioctl, &mut info) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

fn read_event(file: &mut File) -> io::Result<Option<libc::input_event>> {
    // SAFETY: `input_event` is a C POD. The mutable byte view covers exactly
    // this initialized value and exists only for the duration of `read`.
    let mut event: libc::input_event = unsafe { std::mem::zeroed() };
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            std::ptr::from_mut(&mut event).cast::<u8>(),
            size_of::<libc::input_event>(),
        )
    };
    match file.read(bytes) {
        Ok(0) => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "physical gamepad disconnected",
        )),
        Ok(read) if read != bytes.len() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short physical gamepad event",
        )),
        Ok(_) => Ok(Some(event)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs_info(minimum: i32, maximum: i32, flat: i32) -> libc::input_absinfo {
        libc::input_absinfo {
            value: 0,
            minimum,
            maximum,
            fuzz: 0,
            flat,
            resolution: 0,
        }
    }

    #[test]
    fn forwarded_virtual_gamepads_are_not_room_hardware() {
        assert!(is_forwarded_gamepad(
            Some(FORWARDED_GAMEPAD_VENDOR),
            Some(FORWARDED_GAMEPAD_PRODUCT)
        ));
        assert!(!is_forwarded_gamepad(Some(0x045e), Some(0x028e)));
        assert!(!is_forwarded_gamepad(Some(FORWARDED_GAMEPAD_VENDOR), None));
        assert!(!is_forwarded_gamepad(None, None));
    }

    #[test]
    fn a_joycond_pair_is_one_controller_and_its_raw_halves_are_hidden() {
        const JOYSTICK: &str = "E:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\nG:uaccess\nQ:uaccess\n";
        const HELD: &str = "E:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\nG:joycond\nQ:joycond\n";
        const RELEASED: &str =
            "E:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\nG:joycond\nG:uaccess\nQ:uaccess\n";
        let device = |product, udev_properties, mode| EvdevDescriptor {
            vendor_id: Some(NINTENDO_VENDOR),
            product_id: Some(product),
            udev_properties,
            mode,
        };
        let room = |devices: &[EvdevDescriptor<'_>]| {
            devices
                .iter()
                .filter(|device| is_room_controller(device))
                .count()
        };

        // Both halves grabbed and combined: only joycond's uinput device.
        assert_eq!(
            room(&[
                device(JOYCON_LEFT_PRODUCT, HELD, 0o020_600),
                device(JOYCON_RIGHT_PRODUCT, HELD, 0o020_600),
                device(0x2008, JOYSTICK, 0o020_660),
            ]),
            1
        );
        assert!(is_room_controller(&device(0x2008, JOYSTICK, 0o020_660)));
        assert!(!is_room_controller(&device(
            JOYCON_GRIP_PRODUCT,
            HELD,
            0o020_600
        )));
        // A half released to be used alone is a controller of its own,
        // whether or not udev's change pass reset the node's mode.
        assert!(is_room_controller(&device(
            JOYCON_LEFT_PRODUCT,
            RELEASED,
            0o020_660
        )));
        assert!(is_room_controller(&device(
            JOYCON_RIGHT_PRODUCT,
            HELD,
            0o020_666
        )));
        // A record from before udev split current tags off still counts.
        assert!(!is_room_controller(&device(
            JOYCON_LEFT_PRODUCT,
            "E:ID_INPUT_JOYSTICK=1\nG:joycond\n",
            0o020_600
        )));
        // Without joycond's tag the kernel device is the only one there is.
        assert!(is_room_controller(&device(
            JOYCON_RIGHT_PRODUCT,
            JOYSTICK,
            0o020_600
        )));
        // The Pro Controller stays with hid-nintendo and Steam; a stray tag
        // on it must not hide it either.
        assert!(is_room_controller(&device(0x2009, HELD, 0o020_600)));
        assert!(!has_current_udev_tag(
            "E:TAGS=:joycond:\nQ:joycondx\n",
            "joycond"
        ));
    }

    #[test]
    fn sysfs_ids_parse_as_hex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vendor");
        fs::write(&path, "057e\n").expect("write vendor");
        assert_eq!(read_hex_id(&path), Some(0x057e));
        fs::write(&path, "garbage").expect("write vendor");
        assert_eq!(read_hex_id(&path), None);
        assert_eq!(read_hex_id(&dir.path().join("missing")), None);
    }

    #[test]
    fn physical_gamepads_classify_and_calibrate_into_the_fixed_schema() {
        {
            assert!(is_exclusive_controller(
                "E:ID_INPUT=1\nE:ID_INPUT_JOYSTICK=1\n"
            ));
            assert!(!is_exclusive_controller("E:ID_INPUT=1\n"));
            for hybrid in [
                "ID_INPUT_KEYBOARD=1",
                "ID_INPUT_MOUSE=1",
                "ID_INPUT_TOUCHPAD=1",
                "ID_INPUT_TOUCHSCREEN=1",
                "ID_INPUT_TABLET=1",
            ] {
                assert!(!is_exclusive_controller(&format!(
                    "E:ID_INPUT_JOYSTICK=1\nE:{hybrid}\n"
                )));
            }
        }
        {
            let axis = AxisCalibration::from_abs_info(abs_info(-100, 300, 12), AxisKind::Stick)
                .expect("valid calibration");
            assert_eq!(axis.center, 100);
            assert_eq!(axis.normalize(-100), i16::MIN);
            assert_eq!(axis.normalize(300), i16::MAX);
            assert_eq!(axis.normalize(89), 0);
            assert_eq!(axis.normalize(112), 0);
            assert!(axis.normalize(50) < 0);
            assert!(axis.normalize(200) > 0);
        }
        {
            let trigger = AxisCalibration::from_abs_info(abs_info(0, 255, 0), AxisKind::Trigger)
                .expect("valid calibration");
            assert_eq!(trigger.normalize(0), 0);
            assert_eq!(trigger.normalize(255), i16::MAX);
            assert!((16_300..=16_500).contains(&trigger.normalize(128)));
        }
        {
            assert!(AxisCalibration::from_abs_info(abs_info(4, 4, 0), AxisKind::Stick).is_err());
            assert!(AxisCalibration::from_abs_info(abs_info(5, 4, 0), AxisKind::Trigger).is_err());
        }
        {
            for raw in [i32::MIN, -2, -1, 0, 1, 2, i32::MAX] {
                let normalized = raw.signum() as i8;
                assert!((-1..=1).contains(&normalized));
                assert!(
                    ForwardedGamepadEvent::Hat {
                        x: normalized,
                        y: -normalized,
                    }
                    .is_valid()
                );
            }
        }
    }
}
