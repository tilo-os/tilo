//! Getting the engine when it is missing: the Browser asks the host to
//! install the runtime it declares and follows the progress, then reopens so
//! the launcher mounts the new engine. A person sees a progress state, never
//! a dead end and never a command to run.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ui::lock::MutexRecover as _;
use ui::webview::chromium::install::{EngineInstaller, InstallPhase, InstallStatus};

/// How often the host is asked where the install stands.
const LOOK_EVERY: Duration = Duration::from_millis(300);

/// What the page area shows while the engine is being set up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setup {
    Fetching {
        phase: InstallPhase,
        received: u64,
        total: Option<u64>,
    },
    /// Installed, and a fresh Browser was started that has it.
    Reopened,
    /// It did not work; the person can try again.
    Failed(String),
}

impl Setup {
    /// The fraction fetched, when the host has said how much there is.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            Self::Fetching {
                phase: InstallPhase::Downloading,
                received,
                total: Some(total),
            } if *total > 0 => Some((*received as f64 / *total as f64).clamp(0.0, 1.0) as f32),
            Self::Fetching {
                phase: InstallPhase::Installing,
                ..
            } => Some(1.0),
            _ => None,
        }
    }
}

/// One setup in flight: a worker follows the host and publishes its state.
pub struct EngineSetup {
    state: Arc<Mutex<Setup>>,
}

impl EngineSetup {
    /// Ask the host for the engine and follow it. Once installed, start a
    /// fresh Browser on `reopen` (the page being opened, if any).
    pub fn start(
        mut installer: Box<dyn EngineInstaller>,
        reopen: Option<String>,
        wake: ui::WakeHandle,
    ) -> Self {
        let state = Arc::new(Mutex::new(Setup::Fetching {
            phase: InstallPhase::Preparing,
            received: 0,
            total: None,
        }));
        let shared = Arc::clone(&state);
        let publish = move |next: Setup| {
            *shared.lock_recover() = next;
            wake.wake();
        };
        let spawned = std::thread::Builder::new()
            .name("browser-engine-setup".into())
            .spawn(move || {
                loop {
                    match installer.status() {
                        Ok(InstallStatus::Installing {
                            phase,
                            received,
                            total,
                        }) => publish(Setup::Fetching {
                            phase,
                            received,
                            total,
                        }),
                        Ok(InstallStatus::Installed) => {
                            publish(match installer.relaunch(reopen.as_deref()) {
                                Ok(()) => Setup::Reopened,
                                Err(error) => Setup::Failed(error),
                            });
                            return;
                        }
                        Ok(InstallStatus::Failed(message)) => {
                            publish(Setup::Failed(message));
                            return;
                        }
                        Err(error) => {
                            publish(Setup::Failed(error));
                            return;
                        }
                    }
                    std::thread::sleep(LOOK_EVERY);
                }
            });
        if let Err(error) = spawned {
            *state.lock_recover() = Setup::Failed(error.to_string());
        }
        Self { state }
    }

    pub fn state(&self) -> Setup {
        self.state.lock_recover().clone()
    }
}
