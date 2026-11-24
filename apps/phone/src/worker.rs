//! The phone's device off the window's thread: the daemon's API is reached over a socket and a
//! login goes out to another device, which the window must never wait on (Android ends an app
//! whose window stops answering). The window asks; the worker answers into [`Seen`], which the
//! phone's origin hands the shell's view as its readings.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tilo_runtime::in_process::{App, Device, Devices};

/// How long the worker waits for an ask before reading the device again: a login another device
/// approved, a workspace joined there, an app installed, all reach the phone within it.
const READ_AGAIN: Duration = Duration::from_secs(2);

/// What the phone's view asks of its device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Sign in with a ticket another of the person's devices printed.
    LogIn(String),
    /// Start a new person on this device.
    Found,
    /// Show the apps of this workspace.
    Workspace(String),
}

/// What the worker last read of the device, and how often it changed: the origin writes it
/// into the view's readings when it moved.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub generation: u64,
    pub devices: Devices,
    /// The workspace the person chose, and its apps.
    pub workspace: Option<String>,
    pub apps: Vec<App>,
    /// Why the last ask failed, in the words the person reads.
    pub refused: Option<String>,
}

/// The worker's side the window holds: where it asks, and what it reads.
#[derive(Clone)]
pub struct Worker {
    asks: Sender<Ask>,
    seen: Arc<Mutex<Seen>>,
}

/// Mount on this process's daemon what the `tilo-runtime` program mounts on its own, once.
fn mount() {
    static MOUNTED: std::sync::Once = std::sync::Once::new();
    MOUNTED.call_once(|| tilo_runtime::device::capabilities::mount(tilo_daemon::capabilities()));
}

impl Worker {
    /// Start the device's daemon on `profile` and the thread that serves the phone's asks; `wake`
    /// wakes the window when what it shows moved.
    ///
    /// # Errors
    /// The daemon did not start.
    pub fn start(profile: &Path, wake: impl Fn() + Send + 'static) -> anyhow::Result<Self> {
        mount();
        let device = Device::start(profile)?;
        let (asks, asked) = std::sync::mpsc::channel();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let serving = Arc::clone(&seen);
        std::thread::Builder::new()
            .name("tilo-phone".into())
            .spawn(move || serve(device, &asked, &serving, &wake))?;
        Ok(Self { asks, seen })
    }

    /// Ask the device for `ask`; the answer arrives as what [`Self::seen`] reads.
    pub fn ask(&self, ask: Ask) {
        // The worker ends only with the process; an ask after that has no one to answer it.
        let _ = self.asks.send(ask);
    }

    /// What the worker last read.
    #[must_use]
    pub fn seen(&self) -> Seen {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn serve(mut device: Device, asked: &Receiver<Ask>, seen: &Mutex<Seen>, wake: &dyn Fn()) {
    let mut workspace: Option<String> = None;
    loop {
        let mut refused = None;
        match asked.recv_timeout(READ_AGAIN) {
            Ok(Ask::LogIn(ticket)) => {
                if let Err(error) = device.log_in(&ticket, &label()) {
                    refused = Some(format!("{error:#}"));
                }
            }
            Ok(Ask::Found) => {
                if let Err(error) = device.found() {
                    refused = Some(format!("{error:#}"));
                }
            }
            Ok(Ask::Workspace(chosen)) => workspace = Some(chosen),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let devices = device.devices().unwrap_or_else(|error| {
            refused.get_or_insert_with(|| format!("{error:#}"));
            Devices::default()
        });
        let apps = match &workspace {
            Some(workspace) if devices.signed_in => {
                device.apps(workspace).unwrap_or_else(|error| {
                    refused.get_or_insert_with(|| format!("{error:#}"));
                    Vec::new()
                })
            }
            _ => Vec::new(),
        };
        let mut now = seen.lock().unwrap_or_else(PoisonError::into_inner);
        let moved = now.devices != devices
            || now.workspace != workspace
            || now.apps != apps
            || now.refused != refused;
        if moved {
            *now = Seen {
                generation: now.generation.wrapping_add(1),
                devices,
                workspace: workspace.clone(),
                apps,
                refused,
            };
            drop(now);
            wake();
        }
    }
    if let Err(error) = device.stop() {
        tracing::error!(%error, "the device daemon did not stop cleanly");
    }
}

/// What the person's other devices call this one: the phone's model, as Android names it.
fn label() -> String {
    std::env::var("TILO_DEVICE_LABEL").unwrap_or_else(|_| "Phone".to_owned())
}
