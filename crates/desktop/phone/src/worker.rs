//! The phone's device off the window's thread: the daemon's API is reached over a socket and a
//! login goes out to another device, which the window must never wait on (Android ends an app
//! whose window stops answering). The window asks; the worker answers into [`Seen`], which the
//! phone's origin hands the shell's view as its readings.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tilo_person_services::person_telling;
use tilo_process_services::notify::Notifier;
use tilo_runtime::in_process::{App, Device, Devices, FileWatch};

use crate::telling::Watching;
use crate::unified_push;

/// How long the worker waits for an ask before reading the device again: a login another device
/// approved, a workspace joined there, an app installed, all reach the phone within it.
const READ_AGAIN: Duration = Duration::from_secs(2);

/// How long a file provider's ask waits for its answer: a change of the workspace waits for the
/// devices that keep it to take it, and one that took longest still answers here.
const ANSWER_WITHIN: Duration = Duration::from_secs(90);

/// How long after the last look at a folder the watch of its workspace ends: nobody shows one.
const WATCHING_FOR: Duration = Duration::from_secs(60);

/// What the phone's view, or a file provider outside it, asks of its device.
pub enum Ask {
    /// Sign in with a ticket another of the person's devices printed.
    LogIn(String),
    /// Start a new person on this device.
    Found,
    /// Show the apps of this workspace.
    Workspace(String),
    /// Work on the daemon that another thread waits on, answered where the daemon lives: what a
    /// file provider asks when Android calls it, off the view's own loop.
    Files(Box<dyn FnOnce(&mut Device) + Send>),
    /// The folders `workspace` holds whose entries moved: what a file provider shows of them is
    /// out of date.
    FilesChanged {
        workspace: String,
        folders: Vec<String>,
    },
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
    /// Why this device runs none of a workspace's apps, in the words the person reads: what its
    /// Apps page says above them.
    pub apps_refused: Option<String>,
}

/// The worker's side the window holds: where it asks, and what it reads.
#[derive(Clone)]
pub struct Worker {
    asks: Sender<Ask>,
    seen: Arc<Mutex<Seen>>,
    /// What a tap on a notification asked the view to open, taken on its next frame.
    open: Arc<Mutex<Option<String>>>,
    /// The workspaces a file provider shows, watched while it does.
    watches: Arc<Held>,
}

/// Mount on this process's daemon what the `tilo-runtime` program mounts on its own, once, and
/// the phone's own notifications as the wakeups an app's moment is armed with: an alarm set for
/// this phone rings from the platform when the phone is offline or the app asleep.
fn mount() {
    static MOUNTED: std::sync::Once = std::sync::Once::new();
    MOUNTED.call_once(|| {
        tilo_runtime::device::capabilities::mount(capabilities(
            ui::process::current().notifications,
        ));
    });
}

/// What the phone mounts on its daemon: the program's own, and `alarms`, the notifications the
/// moments its apps land on this phone are armed with.
#[must_use]
pub fn capabilities(
    alarms: Arc<dyn ui::notify::Notifier>,
) -> tilo_runtime::device::capabilities::Capabilities {
    tilo_runtime::device::capabilities::Capabilities {
        alarms: Some(alarms),
        ..tilo_daemon::capabilities()
    }
}

impl Worker {
    /// Start the device's daemon on `profile` and the thread that serves the phone's asks; what
    /// the person is told is shown through `notifier`, and `wake` wakes the window when what it
    /// shows moved. `runs_apps` is whether this device can run a workspace's apps, or why not.
    ///
    /// # Errors
    /// The daemon did not start.
    pub fn start(
        profile: &Path,
        notifier: Arc<dyn Notifier>,
        runs_apps: Result<(), String>,
        wake: impl Fn() + Send + 'static,
    ) -> anyhow::Result<Self> {
        mount();
        let device = Device::start(profile)?;
        let (asks, asked) = std::sync::mpsc::channel();
        let seen = Arc::new(Mutex::new(Seen {
            apps_refused: runs_apps.err(),
            ..Seen::default()
        }));
        let open = Arc::new(Mutex::new(None));
        let watches = Arc::new(Held::default());
        let serving = Arc::clone(&seen);
        let opened = Arc::clone(&open);
        let watching = Arc::clone(&watches);
        std::thread::Builder::new()
            .name("tilo-phone".into())
            .spawn(move || {
                serve(
                    device,
                    &asked,
                    &serving,
                    &opened,
                    &watching,
                    notifier.as_ref(),
                    &wake,
                );
            })?;
        Ok(Self {
            asks,
            seen,
            open,
            watches,
        })
    }

    /// What a tap on one of the phone's notifications asked to open, once.
    pub fn take_open(&self) -> Option<String> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Ask the device for `ask`; the answer arrives as what [`Self::seen`] reads.
    pub fn ask(&self, ask: Ask) {
        // The worker ends only with the process; an ask after that has no one to answer it.
        let _ = self.asks.send(ask);
    }

    /// The worker's side a file provider holds: its asks run on the thread that keeps the
    /// daemon.
    #[must_use]
    pub fn files(&self) -> Files {
        Files {
            asks: self.asks.clone(),
            watches: Arc::clone(&self.watches),
        }
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

/// A file provider's side of the phone's device: each ask runs on the thread that keeps the
/// daemon, and this waits for its answer. A person signs in and their workspaces arrive there,
/// so a provider started before both sees them as soon as the view does.
#[derive(Clone)]
pub struct Files {
    asks: Sender<Ask>,
    watches: Arc<Held>,
}

impl Files {
    /// Do `work` with the device, on the thread that keeps it, and answer what it answered.
    ///
    /// # Errors
    /// The phone's daemon is not running, or it ended before it answered.
    pub fn ask<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Device) -> T + Send + 'static,
    ) -> Result<T, String> {
        let (answer, answered) = std::sync::mpsc::channel();
        self.asks
            .send(Ask::Files(Box::new(move |device| {
                let _ = answer.send(work(device));
            })))
            .map_err(|_| "the phone's daemon is not running".to_owned())?;
        answered
            .recv_timeout(ANSWER_WITHIN)
            .map_err(|_| "the phone's daemon did not answer in time".to_owned())
    }

    /// Watch `workspace`'s files while a file provider shows a folder of it: the folders whose
    /// entries move are told to the provider, from the thread that keeps the daemon. A
    /// workspace already watched has the look noted, so a folder shown again starts no second
    /// watch.
    ///
    /// # Errors
    /// The phone's daemon is not running, the workspace is not one it holds, or its tree does
    /// not open.
    pub fn watch(&self, workspace: &str) -> Result<(), String> {
        let watches = Arc::clone(&self.watches);
        let moved = self.asks.clone();
        let chosen = workspace.to_owned();
        self.ask(move |device| {
            {
                let mut held = watches.0.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(watch) = held.get_mut(&chosen) {
                    watch.looked = Instant::now();
                    return Ok(());
                }
            }
            let told = moved;
            let changed_in = chosen.clone();
            // The watch is started with no lock held: opening the workspace's tree takes a while
            // and a cursor closing meanwhile has no reason to wait for it.
            let files = crate::workspace_files::watch(device, &chosen, move |folders| {
                // The worker tells the provider, on the thread Java's calls already run on; an
                // ask after the worker ended has no one to answer it.
                let _ = told.send(Ask::FilesChanged {
                    workspace: changed_in.clone(),
                    folders,
                });
            })?;
            let _ = watches
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(
                    chosen,
                    Watch {
                        files,
                        looked: Instant::now(),
                    },
                );
            Ok(())
        })?
    }

    /// Nothing shows a folder of `workspace` any more: its watch ends.
    pub fn unwatch(&self, workspace: &str) {
        let _ = self
            .watches
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(workspace);
    }
}

/// The workspaces a file provider shows, watched while it does: one watch per workspace, and
/// when the system last asked for a folder of it.
#[derive(Default)]
struct Held(Mutex<HashMap<String, Watch>>);

/// One workspace's watch, and when a folder of it was last shown.
struct Watch {
    #[allow(dead_code, reason = "held for its drop, which ends the watch")]
    files: FileWatch,
    looked: Instant,
}

fn serve(
    mut device: Device,
    asked: &Receiver<Ask>,
    seen: &Mutex<Seen>,
    open: &Mutex<Option<String>>,
    watches: &Held,
    notifier: &dyn Notifier,
    wake: &dyn Fn(),
) {
    let mut workspace: Option<String> = None;
    let mut watching = Watching::default();
    let mut tapped: Option<String> = None;
    // Why the person's last sign-in or start was refused: said until they ask again, not only
    // until the next reading of the device, which would take the banner away before it is read.
    let mut told: Option<String> = None;
    loop {
        // Nothing has shown a folder of a workspace for a while: its watch ends. A cursor the
        // system never closes leaves no other word that the folder went away.
        watches
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, watch| watch.looked.elapsed() < WATCHING_FOR);
        match asked.recv_timeout(READ_AGAIN) {
            Ok(Ask::LogIn(ticket)) => {
                told = device
                    .log_in(&ticket, &label())
                    .err()
                    .map(|error| format!("{error:#}"));
            }
            Ok(Ask::Found) => told = device.found().err().map(|error| format!("{error:#}")),
            Ok(Ask::Workspace(chosen)) => workspace = Some(chosen),
            // A file provider's ask is answered here and changes nothing the view shows.
            Ok(Ask::Files(work)) => {
                work(&mut device);
                continue;
            }
            // A workspace's files moved where this phone holds them: the provider showing its
            // folders tells the system, which reads them again.
            Ok(Ask::FilesChanged { workspace, folders }) => {
                crate::files_changed(&workspace, &folders);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let mut refused = told.clone();
        let devices = device.devices().unwrap_or_else(|error| {
            refused.get_or_insert_with(|| format!("{error:#}"));
            Devices::default()
        });
        if let Some(chosen) = workspace.as_deref().filter(|_| devices.signed_in) {
            register(&device, chosen, &mut watching);
            if let Err(error) = watching.catch_up(&device, chosen, notifier) {
                tracing::debug!(%error, "what the person was told is not read");
            }
            // A tap written by the Android side (`TiloPush`) is read here, once.
            if let Some(link) = read_file(unified_push::tap_file().as_deref())
                && tapped.as_deref() != Some(link.as_str())
            {
                tapped = Some(link.clone());
                tap(&device, Some(chosen), &watching, &link, open);
            }
        }
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
                apps_refused: now.apps_refused.take(),
            };
            drop(now);
            wake();
        }
    }
    if let Err(error) = device.stop() {
        tracing::error!(%error, "the device daemon did not stop cleanly");
    }
}

/// Hand this device's push endpoint to the host, once it is one no address was handed over with:
/// the `UnifiedPush` distributor leaves it where the app's Android half writes it, and a tap or a
/// push elsewhere uses the same key.
fn register(device: &Device, workspace: &str, watching: &mut Watching) {
    let (Some(path), Some(data)) = (unified_push::endpoint_file(), tilo_paths::data_dir()) else {
        return;
    };
    let Some(endpoint) = read_file(Some(&path)) else {
        return;
    };
    let Ok(mut key) = unified_push::Key::load_or_create(&data) else {
        tracing::warn!("the phone's push key could not be read or made");
        return;
    };
    if key.endpoint == endpoint {
        if let Ok(auth) = key.auth_bytes() {
            watching.holding(auth);
        }
        return;
    }
    let (p256dh, auth) = key.handed_over();
    match device.register_web_push_address(workspace, &endpoint, &p256dh, &auth) {
        Ok(_) => {
            key.endpoint.clone_from(&endpoint);
            if let Err(error) = key.save(&data) {
                tracing::warn!(%error, "the phone's push endpoint is not written down");
            }
            if let Ok(auth) = key.auth_bytes() {
                watching.holding(auth);
            }
            tracing::info!("the phone's push endpoint is handed to the workspace's host");
        }
        Err(error) => {
            tracing::debug!(%error, "the phone's push endpoint is not taken yet");
        }
    }
}

/// The person tapped one of the phone's notifications: the thing it is about is acknowledged as
/// the button says, and the phone opens what the thing is about.
fn tap(
    device: &Device,
    workspace: Option<&str>,
    watching: &Watching,
    link: &str,
    open: &Mutex<Option<String>>,
) {
    let Some(tap) = watching.tapped(link) else {
        return;
    };
    let Some(workspace) = workspace else {
        return;
    };
    let by = tap
        .action
        .clone()
        .unwrap_or_else(|| person_telling::BY_OPENED.to_owned());
    if let Err(error) = device.acknowledge_told(workspace, &tap.told, &label(), &by) {
        tracing::debug!(%error, "a thing tapped is not acknowledged");
    }
    if let Some(app) = watching
        .about_of(&tap.told)
        .map(|about| about.app.clone())
        .filter(|app| !app.is_empty())
    {
        *open.lock().unwrap_or_else(PoisonError::into_inner) = Some(app);
    }
}

/// What the file at `path` says, trimmed; none where there is no file or it is empty.
fn read_file(path: Option<&Path>) -> Option<String> {
    let text = std::fs::read_to_string(path?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// What the person's other devices call this one: the phone's model, as Android names it.
fn label() -> String {
    std::env::var("TILO_DEVICE_LABEL").unwrap_or_else(|_| "Phone".to_owned())
}
