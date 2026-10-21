//! Which radios this device has: a Bluetooth adapter, a Wi-Fi radio, a
//! mobile broadband modem.
//!
//! Settings offers a radio's actions only while the device has the radio:
//! with no Bluetooth adapter, "turn Bluetooth on" is published unavailable
//! with the reason, not as a switch that can only fail. What the device has
//! is read from the kernel's device classes on a thread of its own, never
//! on the UI thread, and read again every few seconds, so a dongle plugged
//! in while Settings runs makes its actions available. The window is woken
//! only when the answer changes.

use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the device's radios are read again.
const EVERY: Duration = Duration::from_secs(3);

/// The radios a device has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Present {
    pub bluetooth: bool,
    pub wifi: bool,
    pub wwan: bool,
}

impl Present {
    /// Every radio.
    #[cfg(test)]
    pub const ALL: Self = Self {
        bluetooth: true,
        wifi: true,
        wwan: true,
    };

    /// No radio at all: a desktop or a VM without a dongle.
    #[cfg(test)]
    pub const NONE: Self = Self {
        bluetooth: false,
        wifi: false,
        wwan: false,
    };

    /// What the kernel lists on this device now: a few directory listings,
    /// no D-Bus and no subprocess.
    fn of_device() -> Self {
        Self {
            bluetooth: os_actions::bluez::has_adapter(),
            wifi: os_actions::network::has_wifi_radio(),
            wwan: os_actions::network::has_wwan_modem(),
        }
    }
}

/// What the window knows of the device's radios. Cheap to clone; clones
/// share one reading, and the thread reading the device ends with the
/// last clone.
#[derive(Clone)]
pub struct Radios {
    latest: Arc<Mutex<Option<Present>>>,
    /// Asks the reading thread to look again now; `None` for a fixed answer.
    again: Option<Arc<Sender<()>>>,
}

impl Radios {
    /// Never read: a screenshot's, a test's, a detached pane's. Radios
    /// not read keep every action offered, and what `NetworkManager`
    /// reports decides alone.
    pub fn unread() -> Self {
        Self {
            latest: Arc::new(Mutex::new(None)),
            again: None,
        }
    }

    /// This device's radios, read off the UI thread.
    pub fn of_device() -> Self {
        Self::watching(Present::of_device, EVERY)
    }

    /// Read `probe` on a thread of its own at once and again `every` so
    /// often or when asked ([`Self::look_again`]); the window is woken
    /// when what it says changes. Must be called on the UI thread, in the
    /// window's wake scope.
    pub fn watching(probe: impl Fn() -> Present + Send + 'static, every: Duration) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let (again, asked) = std::sync::mpsc::channel::<()>();
        let wake = ui::WakeHandle::new();
        let reading = Arc::clone(&latest);
        let started = crate::device_read::thread(
            "settings-radios",
            Box::new(move || {
                loop {
                    let now = Some(probe());
                    let changed = reading.lock().is_ok_and(|mut latest| {
                        let changed = *latest != now;
                        *latest = now;
                        changed
                    });
                    if changed {
                        wake.wake();
                    }
                    match asked.recv_timeout(every) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            }),
        );
        if let Err(error) = started {
            // Unknown radios keep every action offered, as before Settings
            // could tell.
            tracing::warn!(target: "tilo::settings", %error, "the device's radios cannot be watched");
        }
        Self {
            latest,
            again: Some(Arc::new(again)),
        }
    }

    /// The radios as last read; `None` until the first read ends.
    pub fn now(&self) -> Option<Present> {
        self.latest.lock().ok().and_then(|latest| *latest)
    }

    /// Read the device again now rather than at the next interval.
    pub fn look_again(&self) {
        if let Some(again) = &self.again {
            let _ = again.send(());
        }
    }

    /// Say the device has `present` from now on, as a dongle plugged in or
    /// pulled out would.
    #[cfg(test)]
    pub fn set(&self, present: Present) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = Some(present);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `radios` comes to say `expected`; a hang fails the test.
    fn settles_on(radios: &Radios, expected: Present) -> bool {
        tilo_test_env::wait_until(format!("the radios to read {expected:?}"), || {
            radios.now() == Some(expected)
        });
        true
    }

    /// The device is read off the calling thread; an adapter plugged in
    /// later is seen when asked, and again on the interval without asking.
    #[test]
    fn a_radio_plugged_in_later_is_seen_without_a_restart() {
        let device = Arc::new(Mutex::new(Present::NONE));
        let probe = {
            let device = Arc::clone(&device);
            move || *device.lock().unwrap()
        };
        let radios = Radios::watching(probe, Duration::from_secs(3600));
        assert!(settles_on(&radios, Present::NONE), "the first read");

        *device.lock().unwrap() = Present {
            bluetooth: true,
            ..Present::NONE
        };
        radios.look_again();
        assert!(
            settles_on(
                &radios,
                Present {
                    bluetooth: true,
                    ..Present::NONE
                }
            ),
            "a dongle plugged in is seen when asked"
        );

        let polled = Radios::watching(
            {
                let device = Arc::clone(&device);
                move || *device.lock().unwrap()
            },
            Duration::from_millis(10),
        );
        *device.lock().unwrap() = Present::ALL;
        assert!(
            settles_on(&polled, Present::ALL),
            "and on the interval without asking"
        );
    }
}
