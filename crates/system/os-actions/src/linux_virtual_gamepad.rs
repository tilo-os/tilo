//! Linux virtual gamepad implementation for the typed peripheral broker.
//!
//! The public device seam accepts only [`ForwardedGamepadEvent`]. It cannot
//! express raw HID reports, arbitrary evdev codes, descriptors, serials,
//! keyboards, pointers, storage, or security keys. The production provider
//! creates one fixed uinput gamepad profile; tests inject an in-memory device
//! and never require `/dev/uinput`.

use std::io;

use crate::peripheral_forwarding::{
    ForwardedGamepadEvent, VirtualGamepad, VirtualGamepadFactory, VirtualGamepadPlan,
};

/// USB vendor of the fixed virtual gamepad a granted lease creates. Local
/// discovery uses it to tell that device apart from hardware in the room.
pub const FORWARDED_GAMEPAD_VENDOR: u16 = 0x1d6b;
/// USB product of the fixed virtual gamepad; see [`FORWARDED_GAMEPAD_VENDOR`].
pub const FORWARDED_GAMEPAD_PRODUCT: u16 = 0x5153;

/// A local Linux device restricted to the broker's sanitized gamepad events.
pub trait LinuxGamepadDevice: Send {
    /// Emit one typed gamepad event and synchronize it to local consumers.
    ///
    /// # Errors
    ///
    /// Returns an error if the local virtual device cannot accept the event.
    fn emit(&mut self, event: ForwardedGamepadEvent) -> io::Result<()>;

    /// Neutralize all fixed axes and hats and release every fixed button.
    ///
    /// # Errors
    ///
    /// Returns an error if the release batch cannot be delivered.
    fn release_all(&mut self) -> io::Result<()>;

    /// Destroy the local virtual device. Implementations must be idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if explicit device destruction fails.
    fn destroy(&mut self) -> io::Result<()>;
}

/// Opens exactly the fixed Tilo gamepad profile, with no caller-supplied
/// capabilities or identity strings.
pub trait LinuxGamepadDeviceProvider: Send + Sync {
    type Device: LinuxGamepadDevice + 'static;

    /// Open a fixed-profile local virtual gamepad.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable or setup fails.
    fn open_fixed_gamepad(&self) -> io::Result<Self::Device>;
}

/// Adapts a constrained Linux device provider to the privileged broker.
pub struct LinuxVirtualGamepadFactory<P> {
    provider: P,
}

impl<P> LinuxVirtualGamepadFactory<P> {
    #[must_use]
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P> VirtualGamepadFactory for LinuxVirtualGamepadFactory<P>
where
    P: LinuxGamepadDeviceProvider,
{
    fn create(&self, plan: VirtualGamepadPlan) -> Result<Box<dyn VirtualGamepad>, String> {
        if plan != VirtualGamepadPlan::default() {
            return Err("only the fixed Tilo virtual-gamepad profile is allowed".into());
        }
        let device = self
            .provider
            .open_fixed_gamepad()
            .map_err(|error| format!("could not create virtual gamepad: {error}"))?;
        Ok(Box::new(ManagedLinuxGamepad {
            device,
            destroyed: false,
        }))
    }
}

struct ManagedLinuxGamepad<D: LinuxGamepadDevice> {
    device: D,
    destroyed: bool,
}

impl<D: LinuxGamepadDevice> VirtualGamepad for ManagedLinuxGamepad<D> {
    fn apply(&mut self, event: ForwardedGamepadEvent) -> Result<(), String> {
        if !event.is_valid() {
            return Err("invalid sanitized gamepad event".into());
        }
        self.device.emit(event).map_err(|error| error.to_string())
    }

    fn release_all(&mut self) -> Result<(), String> {
        self.device.release_all().map_err(|error| error.to_string())
    }
}

impl<D: LinuxGamepadDevice> Drop for ManagedLinuxGamepad<D> {
    fn drop(&mut self) {
        if self.destroyed {
            return;
        }
        // A failed neutralization must not prevent destruction. The broker
        // receives release errors during explicit revocation; Drop is the
        // final best-effort safety net for unwinding and factory consumers.
        let _ = self.device.release_all();
        let _ = self.device.destroy();
        self.destroyed = true;
    }
}

/// Production provider for Linux uinput. Opening is deferred until a verified
/// broker lease is acquired, so constructing this value has no side effects.
#[derive(Clone, Copy, Debug, Default)]
pub struct UinputGamepadDeviceProvider;

#[cfg(target_os = "linux")]
mod uinput {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write as _};
    use std::mem::size_of;
    use std::os::fd::{AsFd as _, BorrowedFd};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::slice;

    use rustix::ioctl::{IntegerSetter, NoArg, Opcode, ioctl, opcode};

    use super::{
        ForwardedGamepadEvent, LinuxGamepadDevice, LinuxGamepadDeviceProvider,
        UinputGamepadDeviceProvider,
    };
    use crate::linux_gamepad_evdev::{
        ABS_HAT0X, ABS_HAT0Y, AXES, BUTTONS, EV_ABS, EV_KEY, EV_SYN, axis_code, button_code,
    };

    const UINPUT_IOCTL_BASE: u8 = b'U';
    const SYN_REPORT: u16 = 0;
    const BUS_VIRTUAL: u16 = 0x06;

    const UI_SET_EVBIT: Opcode = opcode::write::<libc::c_int>(UINPUT_IOCTL_BASE, 100);
    const UI_SET_KEYBIT: Opcode = opcode::write::<libc::c_int>(UINPUT_IOCTL_BASE, 101);
    const UI_SET_ABSBIT: Opcode = opcode::write::<libc::c_int>(UINPUT_IOCTL_BASE, 103);
    const UI_DEV_CREATE: Opcode = opcode::none(UINPUT_IOCTL_BASE, 1);
    const UI_DEV_DESTROY: Opcode = opcode::none(UINPUT_IOCTL_BASE, 2);

    pub struct UinputGamepadDevice {
        file: File,
        destroyed: bool,
    }

    impl LinuxGamepadDeviceProvider for UinputGamepadDeviceProvider {
        type Device = UinputGamepadDevice;

        fn open_fixed_gamepad(&self) -> io::Result<Self::Device> {
            UinputGamepadDevice::open()
        }
    }

    impl UinputGamepadDevice {
        fn open() -> io::Result<Self> {
            let mut file = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open("/dev/uinput")?;
            let fd = file.as_fd();

            set_capability::<UI_SET_EVBIT>(fd, EV_KEY)?;
            set_capability::<UI_SET_EVBIT>(fd, EV_ABS)?;
            for button in BUTTONS {
                set_capability::<UI_SET_KEYBIT>(fd, button_code(button))?;
            }
            for code in axis_codes() {
                set_capability::<UI_SET_ABSBIT>(fd, code)?;
            }

            // SAFETY: `uinput_user_dev` is a C POD whose all-zero bit pattern
            // is valid. Every field consumed by the kernel is initialized
            // below before its exact byte representation is written.
            let mut setup: libc::uinput_user_dev = unsafe { std::mem::zeroed() };
            let name = b"Tilo Forwarded Gamepad\0";
            for (target, source) in setup.name.iter_mut().zip(name.iter().copied()) {
                *target = libc::c_char::try_from(source).unwrap_or_default();
            }
            setup.id.bustype = BUS_VIRTUAL;
            setup.id.vendor = super::FORWARDED_GAMEPAD_VENDOR;
            setup.id.product = super::FORWARDED_GAMEPAD_PRODUCT;
            setup.id.version = 1;
            configure_axes(&mut setup)?;

            // SAFETY: `setup` remains alive and immutable for this call; the
            // byte slice covers exactly its initialized C representation.
            let bytes = unsafe {
                slice::from_raw_parts(
                    std::ptr::from_ref(&setup).cast::<u8>(),
                    size_of::<libc::uinput_user_dev>(),
                )
            };
            file.write_all(bytes)?;
            // SAFETY: fd refers to `/dev/uinput`; this no-argument ioctl is
            // issued only after the fixed capability/setup payload above.
            unsafe { ioctl(&file, NoArg::<UI_DEV_CREATE>::new()) }?;

            Ok(Self {
                file,
                destroyed: false,
            })
        }

        fn write_event(&mut self, kind: u16, code: u16, value: i32) -> io::Result<()> {
            let event = libc::input_event {
                time: libc::timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                type_: kind,
                code,
                value,
            };
            // SAFETY: `event` remains alive and immutable while `write_all`
            // reads exactly the initialized C struct representation.
            let bytes = unsafe {
                slice::from_raw_parts(
                    std::ptr::from_ref(&event).cast::<u8>(),
                    size_of::<libc::input_event>(),
                )
            };
            self.file.write_all(bytes)
        }

        fn synchronize(&mut self) -> io::Result<()> {
            self.write_event(EV_SYN, SYN_REPORT, 0)
        }
    }

    impl LinuxGamepadDevice for UinputGamepadDevice {
        fn emit(&mut self, event: ForwardedGamepadEvent) -> io::Result<()> {
            match event {
                ForwardedGamepadEvent::Axis { axis, value } => {
                    self.write_event(EV_ABS, axis_code(axis), i32::from(value))?;
                }
                ForwardedGamepadEvent::Button { button, pressed } => {
                    self.write_event(EV_KEY, button_code(button), i32::from(pressed))?;
                }
                ForwardedGamepadEvent::Hat { x, y }
                    if (-1..=1).contains(&x) && (-1..=1).contains(&y) =>
                {
                    self.write_event(EV_ABS, ABS_HAT0X, i32::from(x))?;
                    self.write_event(EV_ABS, ABS_HAT0Y, i32::from(y))?;
                }
                ForwardedGamepadEvent::Hat { .. } => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid gamepad hat",
                    ));
                }
            }
            self.synchronize()
        }

        fn release_all(&mut self) -> io::Result<()> {
            for code in axis_codes() {
                self.write_event(EV_ABS, code, 0)?;
            }
            for button in BUTTONS {
                self.write_event(EV_KEY, button_code(button), 0)?;
            }
            self.synchronize()
        }

        fn destroy(&mut self) -> io::Result<()> {
            if self.destroyed {
                return Ok(());
            }
            // SAFETY: fd is the same `/dev/uinput` file used to create this
            // device, and the operation carries no user pointer.
            unsafe { ioctl(&self.file, NoArg::<UI_DEV_DESTROY>::new()) }?;
            self.destroyed = true;
            Ok(())
        }
    }

    impl Drop for UinputGamepadDevice {
        fn drop(&mut self) {
            let _ = self.destroy();
        }
    }

    fn set_capability<const OPCODE: Opcode>(fd: BorrowedFd<'_>, code: u16) -> io::Result<()> {
        // SAFETY: all callers pass a uinput integer-capability opcode and a
        // code from the fixed constants above; no arbitrary ioctl or code
        // enters through the public API.
        unsafe { ioctl(fd, IntegerSetter::<OPCODE>::new_usize(usize::from(code))) }?;
        Ok(())
    }

    fn configure_axes(setup: &mut libc::uinput_user_dev) -> io::Result<()> {
        for axis in AXES {
            let index = usize::from(axis_code(axis));
            *setup.absmin.get_mut(index).ok_or_else(invalid_axis)? = i32::from(i16::MIN);
            *setup.absmax.get_mut(index).ok_or_else(invalid_axis)? = i32::from(i16::MAX);
        }
        for code in [ABS_HAT0X, ABS_HAT0Y] {
            let index = usize::from(code);
            *setup.absmin.get_mut(index).ok_or_else(invalid_axis)? = -1;
            *setup.absmax.get_mut(index).ok_or_else(invalid_axis)? = 1;
        }
        Ok(())
    }

    /// Every absolute axis the device declares: the forwarded axes and the hat.
    fn axis_codes() -> impl Iterator<Item = u16> {
        AXES.into_iter()
            .map(axis_code)
            .chain([ABS_HAT0X, ABS_HAT0Y])
    }

    fn invalid_axis() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "fixed axis outside uinput ABI")
    }
}

#[cfg(not(target_os = "linux"))]
impl LinuxGamepadDeviceProvider for UinputGamepadDeviceProvider {
    type Device = UnsupportedLinuxGamepadDevice;

    fn open_fixed_gamepad(&self) -> io::Result<Self::Device> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux uinput is unavailable on this platform",
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub struct UnsupportedLinuxGamepadDevice;

#[cfg(not(target_os = "linux"))]
impl LinuxGamepadDevice for UnsupportedLinuxGamepadDevice {
    fn emit(&mut self, _event: ForwardedGamepadEvent) -> io::Result<()> {
        Ok(())
    }
    fn release_all(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn destroy(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::peripheral_forwarding::GamepadAxis;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Open,
        Emit(ForwardedGamepadEvent),
        ReleaseAll,
        Destroy,
    }

    struct MockProvider {
        calls: Arc<Mutex<Vec<Call>>>,
        fail_open: bool,
    }

    struct MockDevice {
        calls: Arc<Mutex<Vec<Call>>>,
        destroyed: bool,
    }

    impl LinuxGamepadDeviceProvider for MockProvider {
        type Device = MockDevice;

        fn open_fixed_gamepad(&self) -> io::Result<Self::Device> {
            self.calls.lock().unwrap().push(Call::Open);
            if self.fail_open {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
            }
            Ok(MockDevice {
                calls: Arc::clone(&self.calls),
                destroyed: false,
            })
        }
    }

    impl LinuxGamepadDevice for MockDevice {
        fn emit(&mut self, event: ForwardedGamepadEvent) -> io::Result<()> {
            self.calls.lock().unwrap().push(Call::Emit(event));
            Ok(())
        }

        fn release_all(&mut self) -> io::Result<()> {
            self.calls.lock().unwrap().push(Call::ReleaseAll);
            Ok(())
        }

        fn destroy(&mut self) -> io::Result<()> {
            if !self.destroyed {
                self.calls.lock().unwrap().push(Call::Destroy);
                self.destroyed = true;
            }
            Ok(())
        }
    }

    fn mock_factory(calls: &Arc<Mutex<Vec<Call>>>) -> LinuxVirtualGamepadFactory<MockProvider> {
        LinuxVirtualGamepadFactory::new(MockProvider {
            calls: Arc::clone(calls),
            fail_open: false,
        })
    }

    #[test]
    fn virtual_gamepads_accept_only_typed_events_and_always_neutralize() {
        // Phase `accepts_only_fixed_profile_and_typed_events`.
        {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let factory = mock_factory(&calls);
            assert!(
                factory
                    .create(VirtualGamepadPlan {
                        axes: 6,
                        buttons: 12,
                        hats: 1,
                    })
                    .is_err()
            );
            assert!(calls.lock().unwrap().is_empty());

            let mut device = factory.create(VirtualGamepadPlan::default()).unwrap();
            let event = ForwardedGamepadEvent::Axis {
                axis: GamepadAxis::LeftX,
                value: 123,
            };
            device.apply(event).unwrap();
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                &[Call::Open, Call::Emit(event)]
            );
        }
        // Phase `explicit_release_and_drop_always_neutralize_then_destroy`.
        {
            let calls = Arc::new(Mutex::new(Vec::new()));
            {
                let factory = mock_factory(&calls);
                let mut device = factory.create(VirtualGamepadPlan::default()).unwrap();
                device.release_all().unwrap();
            }
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                &[
                    Call::Open,
                    Call::ReleaseAll,
                    Call::ReleaseAll,
                    Call::Destroy,
                ]
            );
        }
        // Phase `setup_failure_does_not_produce_a_partially_managed_device`.
        {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let factory = LinuxVirtualGamepadFactory::new(MockProvider {
                calls: Arc::clone(&calls),
                fail_open: true,
            });
            assert!(factory.create(VirtualGamepadPlan::default()).is_err());
            assert_eq!(calls.lock().unwrap().as_slice(), &[Call::Open]);
        }
    }
}
