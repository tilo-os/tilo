//! Which radios this device has: a Bluetooth adapter, a Wi-Fi radio, a mobile
//! broadband modem. Settings offers a radio's actions only while the device has
//! the radio: with no Bluetooth adapter, "turn Bluetooth on" is published
//! unavailable with the reason, not as a switch that can only fail.

use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Present {
    pub bluetooth: bool,
    pub wifi: bool,
    pub wwan: bool,
}

impl Present {
    #[cfg(test)]
    pub const ALL: Self = Self {
        bluetooth: true,
        wifi: true,
        wwan: true,
    };

    #[cfg(test)]
    pub const NONE: Self = Self {
        bluetooth: false,
        wifi: false,
        wwan: false,
    };

    #[cfg(feature = "tilo-os")]
    fn of_device() -> Self {
        Self {
            bluetooth: os_actions::bluez::has_adapter(),
            wifi: os_actions::network::has_wifi_radio(),
            wwan: os_actions::network::has_wwan_modem(),
        }
    }

    #[cfg(not(feature = "tilo-os"))]
    fn of_device() -> Self {
        Self {
            bluetooth: false,
            wifi: false,
            wwan: false,
        }
    }
}

#[derive(Clone)]
pub struct Radios {
    latest: Arc<Mutex<Option<Present>>>,
    taken: Arc<Mutex<Option<Present>>>,
    again: Option<Arc<Sender<()>>>,
}

impl Radios {
    /// Never read: a screenshot's, a test's, a detached pane's. Radios
    /// not read keep every action offered, and what `NetworkManager`
    /// reports decides alone.
    pub fn unread() -> Self {
        Self {
            latest: Arc::new(Mutex::new(None)),
            taken: Arc::new(Mutex::new(None)),
            again: None,
        }
    }

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
            tracing::warn!(target: "tilo::settings", %error, "the device's radios cannot be watched");
        }
        Self {
            latest,
            taken: Arc::new(Mutex::new(None)),
            again: Some(Arc::new(again)),
        }
    }

    /// The radios as last read; `None` until the first read ends. While a
    /// frame is laid out on this thread, as they were when it began: a read
    /// that ends meanwhile is taken in by the next frame, which it wakes
    /// ([`ui::motion::frame_laying_out`]).
    pub fn now(&self) -> Option<Present> {
        let Ok(mut taken) = self.taken.lock() else {
            return None;
        };
        if !ui::motion::frame_laying_out() {
            *taken = self.latest.lock().ok().and_then(|latest| *latest);
        }
        *taken
    }

    pub fn look_again(&self) {
        if let Some(again) = &self.again {
            let _ = again.send(());
        }
    }

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

    fn settles_on(radios: &Radios, expected: Present) -> bool {
        tilo_test_env::wait_until(format!("the radios to read {expected:?}"), || {
            radios.now() == Some(expected)
        });
        true
    }

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
