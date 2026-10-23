//! Owned radio work shared by the panel and its public action provider.
//! Admission and parameter validation stay on the session thread. Workers
//! carry only a prepared typed operation; completion never means a modeset,
//! successful association, or a silently inferred hardware state.

use parking_lot::Mutex;
use std::sync::Arc;

use os_actions::{RadioChange, RadioState, RadioWork};

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::t;

type RadioResult = Result<Option<RadioState>, String>;

struct Completion {
    result: Option<RadioResult>,
    applied: bool,
    finished: bool,
}

#[derive(Clone)]
pub(crate) struct RadioTicket {
    pub(crate) change: RadioChange,
    completion: Arc<Mutex<Completion>>,
}

impl RadioTicket {
    pub(crate) fn start(
        change: RadioChange,
        work: RadioWork,
        host_wake: Option<crate::session_runtime::Wake>,
    ) -> Result<Self, String> {
        let ticket = Self {
            change,
            completion: Arc::new(Mutex::new(Completion {
                result: None,
                applied: false,
                finished: false,
            })),
        };
        let completion = Arc::clone(&ticket.completion);
        let ui_wake = ui::WakeHandle::new();
        std::thread::Builder::new()
            .name("quick-settings-radio".into())
            .spawn(move || {
                // Replacement can retire an admitted task before it starts.
                let should_run = completion.lock().result.is_none();
                let result = should_run.then(work);
                {
                    let mut state = completion.lock();
                    if state.result.is_none() {
                        state.result = result;
                    }
                    state.finished = true;
                }
                ui_wake.wake();
                if let Some(wake) = host_wake {
                    wake();
                }
            })
            .map_err(|error| format!("radio worker could not start: {error}"))?;
        Ok(ticket)
    }

    /// Whether the change happened, once the worker is done: its refusal,
    /// or — when it read the radios back — whether they ended up as asked.
    pub(crate) fn result(&self) -> Option<Result<(), String>> {
        let result = self.completion.lock().result.clone()?;
        Some(result.and_then(|radios| radios.map_or(Ok(()), |radios| self.change.outcome(&radios))))
    }

    pub(crate) fn finished(&self) -> bool {
        self.completion.lock().finished
    }

    pub(crate) fn retire(&self) {
        self.completion.lock().result = Some(Err("Quick Settings dispatcher was replaced".into()));
    }

    /// Either the ordinary host update or the public pending invocation may
    /// observe completion first. Apply its visible state/error exactly once.
    pub(crate) fn apply(&self, chrome: &Chrome) {
        let result = {
            let mut completion = self.completion.lock();
            if completion.applied || !completion.finished {
                return;
            }
            let Some(result) = completion.result.clone() else {
                return;
            };
            completion.applied = true;
            result
        };
        apply(self.change, result, chrome);
    }
}

/// Why Wi-Fi is not in the state the person asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiProblem {
    HardwareSwitch,
    /// Asked to turn on, the radio stayed off.
    StayedOff,
    /// The change could not be made at all.
    Failed,
}

impl WifiProblem {
    /// The reason in the person's words.
    pub fn reason(self) -> String {
        t(match self {
            Self::HardwareSwitch => "qs-wifi-problem-hardware-switch",
            Self::StayedOff => "qs-wifi-problem-stayed-off",
            Self::Failed => "qs-wifi-problem-failed",
        })
    }
}

/// Land a finished radio change in the panel: the radios as they were read
/// back, never the request. A change that did not happen says why.
pub(crate) fn apply(change: RadioChange, result: RadioResult, chrome: &Chrome) {
    let mut state = chrome.borrow_mut();
    let qs = &mut state.quick_settings;
    qs.radio_change_pending = false;
    qs.radio_change = None;
    let failed = match result {
        Ok(Some(radios)) => {
            let outcome = change.outcome(&radios);
            qs.wifi_problem = match (change, &outcome) {
                (_, Ok(())) => None,
                (RadioChange::Wifi(true) | RadioChange::Airplane(false), Err(_))
                    if radios.wifi_hardware_blocked =>
                {
                    Some(WifiProblem::HardwareSwitch)
                }
                (RadioChange::Wifi(true) | RadioChange::Airplane(false), Err(_)) => {
                    Some(WifiProblem::StayedOff)
                }
                (_, Err(_)) => qs.wifi_problem,
            };
            if radios.wifi_available {
                qs.wifi = radios.wifi_enabled;
            }
            qs.radios = radios;
            outcome.err()
        }
        // A dispatcher that cannot read the radios back reported success:
        // show it, and the read-back the host kicks next corrects it.
        Ok(None) => {
            match change {
                RadioChange::Wifi(enabled) => {
                    qs.wifi = enabled;
                    qs.radios.wifi_enabled = enabled;
                }
                RadioChange::Airplane(enabled) => {
                    qs.radios.airplane_mode = enabled;
                }
            }
            qs.wifi_problem = None;
            None
        }
        Err(error) => {
            if matches!(change, RadioChange::Wifi(_)) {
                qs.wifi_problem = Some(WifiProblem::Failed);
            }
            Some(error)
        }
    };
    if !qs.wifi {
        qs.wifi_password = None;
        qs.wifi_networks.clear();
    }
    if let Some(error) = failed {
        tracing::warn!(target: "tilo::os_actions", op = ?change, %error, "radio change failed");
        state.push_notification(
            t(match change {
                RadioChange::Wifi(_) => "host-qs-change-failed-wifi",
                RadioChange::Airplane(_) => "host-qs-change-failed-airplane",
            }),
            t("host-qs-change-failed-state-restored"),
        );
    }
    state.bump();
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::*;
    use std::sync::mpsc::{Receiver, Sender, channel};

    #[derive(Clone)]
    pub(crate) struct Dispatcher {
        entered: Sender<RadioChange>,
        release: Arc<Mutex<Receiver<Result<(), String>>>>,
    }

    pub(crate) struct Fixture {
        pub(crate) dispatcher: Dispatcher,
        pub(crate) entered: Receiver<RadioChange>,
        pub(crate) release: Sender<Result<(), String>>,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let (entered_tx, entered) = channel();
            let (release, release_rx) = channel();
            Self {
                dispatcher: Dispatcher {
                    entered: entered_tx,
                    release: Arc::new(Mutex::new(release_rx)),
                },
                entered,
                release,
            }
        }
    }

    impl Dispatcher {
        fn execute(&self, change: RadioChange) -> Result<(), String> {
            self.entered
                .send(change)
                .map_err(|error| error.to_string())?;
            self.release
                .lock()
                .recv()
                .map_err(|error| error.to_string())?
        }
    }

    impl os_actions::Dispatcher for Dispatcher {
        fn prepare_radio_change(&self, change: RadioChange) -> Option<RadioWork> {
            let dispatcher = self.clone();
            Some(Box::new(move || dispatcher.execute(change).map(|()| None)))
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.execute(RadioChange::Wifi(enabled))
        }
        fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
            self.execute(RadioChange::Airplane(enabled))
        }
        fn set_brightness(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }
}
