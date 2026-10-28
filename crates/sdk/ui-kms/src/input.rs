//! libinput over the input devices the host was handed: it opens each one
//! from the descriptors given, never from the filesystem, so a host with no
//! right to open `/dev/input` still reads them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use input::event::keyboard::KeyboardEventTrait as _;
use input::event::pointer::{Axis, ButtonState, PointerScrollEvent as _};
use input::event::touch::{TouchEventPosition as _, TouchEventSlot as _};
use input::event::{DeviceEvent, EventTrait as _, PointerEvent, TouchEvent};
use input::{DeviceCapability, Libinput, LibinputInterface};
use winit::dpi::PhysicalPosition;
use winit::event::{
    DeviceId, ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent,
};

use crate::keyboard::{Keyboard, Layout};

/// The devices libinput may open: each path the host was given, answered
/// with a copy of its descriptor.
#[derive(Clone, Default)]
struct Given(Rc<RefCell<HashMap<PathBuf, OwnedFd>>>);

impl LibinputInterface for Given {
    fn open_restricted(&mut self, path: &Path, _flags: i32) -> Result<OwnedFd, i32> {
        let given = self.0.borrow();
        let fd = given
            .get(path)
            .ok_or(rustix::io::Errno::ACCESS.raw_os_error())?;
        fd.try_clone().map_err(|error| {
            error
                .raw_os_error()
                .unwrap_or(rustix::io::Errno::IO.raw_os_error())
        })
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd);
    }
}

/// What the attached devices are, for the toolkit's interaction model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Present {
    /// A keyboard with letter keys: not a power button or a lid switch,
    /// which libinput also calls keyboards.
    pub keyboard: bool,
    pub pointer: bool,
    pub touch: bool,
}

/// evdev codes from linux/input-event-codes.h.
const KEY_A: u32 = 30;
const KEY_Z: u32 = 44;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

pub struct Input {
    libinput: Libinput,
    given: Given,
    devices: Vec<input::Device>,
    keyboard: Option<Keyboard>,
    /// The pointer, in physical pixels, clamped to the display.
    pointer: (f64, f64),
    size: (u32, u32),
}

impl Input {
    /// libinput over `devices` (path and open descriptor each), on a display
    /// `size` pixels large.
    pub fn new(devices: Vec<(PathBuf, OwnedFd)>, layout: &Layout, size: (u32, u32)) -> Self {
        let given = Given::default();
        let mut input = Self {
            libinput: Libinput::new_from_path(given.clone()),
            given,
            devices: Vec::new(),
            keyboard: Keyboard::new(layout),
            pointer: (f64::from(size.0) / 2.0, f64::from(size.1) / 2.0),
            size,
        };
        for (path, fd) in devices {
            input.add(path, fd);
        }
        input
    }

    /// Read the device at `path` through `fd`.
    pub fn add(&mut self, path: PathBuf, fd: OwnedFd) {
        let Some(name) = path.to_str().map(str::to_owned) else {
            return;
        };
        self.given.0.borrow_mut().insert(path, fd);
        if let Some(device) = self.libinput.path_add_device(&name) {
            self.devices.push(device);
        } else {
            tracing::warn!(device = name, "libinput does not read this device");
        }
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.libinput.as_fd()
    }

    /// What kinds of device are attached now.
    pub fn present(&self) -> Present {
        let mut present = Present::default();
        for device in &self.devices {
            if device.has_capability(DeviceCapability::Keyboard)
                && device.keyboard_has_key(KEY_A).unwrap_or(false)
                && device.keyboard_has_key(KEY_Z).unwrap_or(false)
            {
                present.keyboard = true;
            }
            present.pointer |= device.has_capability(DeviceCapability::Pointer);
            present.touch |= device.has_capability(DeviceCapability::Touch);
        }
        present
    }

    /// When a held key next repeats.
    pub fn deadline(&self) -> Option<Instant> {
        self.keyboard.as_ref().and_then(Keyboard::repeat_deadline)
    }

    /// Key repeats due by `now`.
    pub fn repeats(&mut self, now: Instant) -> Vec<Translated> {
        self.keyboard
            .as_mut()
            .map(|keyboard| keyboard.repeats(now))
            .unwrap_or_default()
            .into_iter()
            .map(Translated::Event)
            .collect()
    }

    /// Read what the devices said, as the toolkit's window events.
    pub fn dispatch(&mut self, now: Instant) -> Vec<Translated> {
        if let Err(error) = self.libinput.dispatch() {
            tracing::warn!(%error, "libinput could not read its devices");
        }
        let mut out = Vec::new();
        while let Some(event) = self.libinput.next() {
            match event {
                input::Event::Device(DeviceEvent::Added(_)) => out.push(Translated::Devices),
                input::Event::Device(DeviceEvent::Removed(removed)) => {
                    let gone = removed.device();
                    self.devices.retain(|device| *device != gone);
                    if let Some(keyboard) = self.keyboard.as_mut() {
                        out.extend(keyboard.release_all().into_iter().map(Translated::Event));
                    }
                    out.push(Translated::Devices);
                }
                input::Event::Keyboard(key) => {
                    let pressed = key.key_state() == input::event::keyboard::KeyState::Pressed;
                    if let Some(keyboard) = self.keyboard.as_mut() {
                        out.extend(
                            keyboard
                                .key(key.key(), pressed, now)
                                .into_iter()
                                .map(Translated::Event),
                        );
                    }
                }
                input::Event::Pointer(pointer) => self.pointer(pointer, &mut out),
                input::Event::Touch(touch) => self.touch(&touch, &mut out),
                _ => {}
            }
        }
        out
    }

    fn moved(&mut self, x: f64, y: f64, out: &mut Vec<Translated>) {
        let (width, height) = self.size;
        self.pointer = (
            x.clamp(0.0, f64::from(width.saturating_sub(1))),
            y.clamp(0.0, f64::from(height.saturating_sub(1))),
        );
        out.push(Translated::Pointer(self.pointer));
        out.push(Translated::Event(WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(self.pointer.0, self.pointer.1),
        }));
    }

    fn pointer(&mut self, event: PointerEvent, out: &mut Vec<Translated>) {
        match event {
            PointerEvent::Motion(motion) => {
                let (x, y) = self.pointer;
                self.moved(x + motion.dx(), y + motion.dy(), out);
            }
            PointerEvent::MotionAbsolute(motion) => {
                let (width, height) = self.size;
                self.moved(
                    motion.absolute_x_transformed(width),
                    motion.absolute_y_transformed(height),
                    out,
                );
            }
            PointerEvent::Button(button) => {
                let state = match button.button_state() {
                    ButtonState::Pressed => ElementState::Pressed,
                    ButtonState::Released => ElementState::Released,
                };
                let button = match button.button() {
                    BTN_LEFT => MouseButton::Left,
                    BTN_RIGHT => MouseButton::Right,
                    BTN_MIDDLE => MouseButton::Middle,
                    BTN_SIDE => MouseButton::Back,
                    BTN_EXTRA => MouseButton::Forward,
                    other => MouseButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
                };
                out.push(Translated::Pointer(self.pointer));
                out.push(Translated::Event(WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state,
                    button,
                }));
            }
            PointerEvent::ScrollWheel(wheel) => {
                // A notch is 120 in v120 units: one line.
                let lines = |axis| {
                    if wheel.has_axis(axis) {
                        -wheel.scroll_value_v120(axis) / 120.0
                    } else {
                        0.0
                    }
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a wheel moves a handful of lines at a time"
                )]
                let delta = MouseScrollDelta::LineDelta(
                    lines(Axis::Horizontal) as f32,
                    lines(Axis::Vertical) as f32,
                );
                out.push(Translated::Event(WindowEvent::MouseWheel {
                    device_id: DeviceId::dummy(),
                    delta,
                    phase: TouchPhase::Moved,
                }));
            }
            PointerEvent::ScrollFinger(finger) => {
                let pixels = |axis| {
                    if finger.has_axis(axis) {
                        -finger.scroll_value(axis)
                    } else {
                        0.0
                    }
                };
                out.push(Translated::Event(WindowEvent::MouseWheel {
                    device_id: DeviceId::dummy(),
                    delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                        pixels(Axis::Horizontal),
                        pixels(Axis::Vertical),
                    )),
                    phase: TouchPhase::Moved,
                }));
            }
            _ => {}
        }
    }

    fn touch(&self, event: &TouchEvent, out: &mut Vec<Translated>) {
        let (width, height) = self.size;
        let (id, phase, location) = match event {
            TouchEvent::Down(down) => (
                down.seat_slot(),
                TouchPhase::Started,
                Some((down.x_transformed(width), down.y_transformed(height))),
            ),
            TouchEvent::Motion(motion) => (
                motion.seat_slot(),
                TouchPhase::Moved,
                Some((motion.x_transformed(width), motion.y_transformed(height))),
            ),
            TouchEvent::Up(up) => (up.seat_slot(), TouchPhase::Ended, None),
            TouchEvent::Cancel(cancel) => (cancel.seat_slot(), TouchPhase::Cancelled, None),
            _ => return,
        };
        out.push(Translated::Touch {
            id: u64::from(id),
            phase,
            location,
        });
    }
}

/// What the devices said, for the host to hand on.
#[derive(Debug)]
pub enum Translated {
    /// A window event the app takes as it is.
    Event(WindowEvent),
    /// The pointer is here now (physical pixels).
    Pointer((f64, f64)),
    /// A finger, at `location` (physical pixels) unless it lifted; the host
    /// keeps the last place of each, which a lift reports.
    Touch {
        id: u64,
        phase: TouchPhase,
        location: Option<(f64, f64)>,
    },
    /// A device came or went.
    Devices,
}

/// The toolkit's contact for a finger `id` at `location`.
pub fn touch(id: u64, phase: TouchPhase, location: (f64, f64)) -> Touch {
    Touch {
        device_id: DeviceId::dummy(),
        phase,
        location: PhysicalPosition::new(location.0, location.1),
        force: None,
        id,
    }
}
