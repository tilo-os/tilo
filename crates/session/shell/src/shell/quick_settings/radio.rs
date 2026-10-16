//! Owned radio work shared by the panel and its public action provider.
//! Admission and parameter validation stay on the session thread. Workers
//! carry only a prepared typed operation; completion never means a modeset,
//! successful association, or a silently inferred hardware state.

use parking_lot::Mutex;
use std::sync::Arc;

use os_actions::{RadioChange, RadioWork};

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::t;

struct Completion {
    result: Option<Result<(), String>>,
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

    pub(crate) fn result(&self) -> Option<Result<(), String>> {
        self.completion.lock().result.clone()
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

pub(crate) fn apply(change: RadioChange, result: Result<(), String>, chrome: &Chrome) {
    let mut state = chrome.borrow_mut();
    state.quick_settings.radio_change_pending = false;
    match result {
        Ok(()) => match change {
            RadioChange::Wifi(enabled) => {
                state.quick_settings.wifi = enabled;
                state.quick_settings.radios.wifi_enabled = enabled;
                state.quick_settings.wifi_password = None;
                if !enabled {
                    state.quick_settings.wifi_networks.clear();
                }
            }
            RadioChange::Airplane(enabled) => {
                state.quick_settings.radios.airplane_mode = enabled;
            }
        },
        Err(error) => {
            tracing::warn!(target: "tilo::os_actions", op = ?change, %error, "radio change failed");
            state.push_notification(
                t(match change {
                    RadioChange::Wifi(_) => "host-qs-change-failed-wifi",
                    RadioChange::Airplane(_) => "host-qs-change-failed-airplane",
                }),
                t("host-qs-change-failed-state-restored"),
            );
        }
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
            Some(Box::new(move || dispatcher.execute(change)))
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
