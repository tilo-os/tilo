//! Tilo on a phone (`so.tilo.phone`): the device daemon every Tilo computer runs, started on this app's
//! own threads, since a phone runs no daemon process of its own; and the shell's `phone` view,
//! drawn natively over what that daemon reads. A person signs in with a ticket another of their
//! devices printed, picks one of their workspaces, and opens its apps, each the tilolang app
//! the APK carries, run in tilolang-process's in-process window. Every view here is the shell's
//! tl package's; this crate hosts it and draws nothing of its own. What an app's window kept
//! while the device was nobody's is carried into the person's own workspace at sign-in, before
//! any of their windows opens again.
//!
//! The daemon keeps its profile and the device's identity under the app's private files, where
//! the Android launch environment points `HOME` and the XDG directories.

#![allow(
    clippy::cast_possible_truncation,
    reason = "see crates/lang/tilolang-tilo: layout weights and sizes are small values"
)]

pub mod bundles;
// The receiver's call into Rust is Android's; it is compiled on the host too by the unit
// tests, which type-check it without a device.
#[cfg(any(target_os = "android", test))]
mod push_jni;
pub mod telling;
pub mod unified_push;
pub mod view;
pub mod worker;
/// The person's workspace files as the phone's file provider shows them.
pub mod workspace_files;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub(crate) use android::files_changed;

/// The folders of a watched workspace whose entries moved: what the provider showing them is
/// told, so the system reads them again. A phone off Android shows no workspace files to one.
#[cfg(not(target_os = "android"))]
pub(crate) fn files_changed(_workspace: &str, _folders: &[String]) {}

use std::path::PathBuf;
use std::sync::OnceLock;

use ui::framework::{ResourceApp, ViewCx};
use ui::widget::Element;

use tilolang_process::window::resource::Workspace;

/// The app. See the module docs.
pub struct Phone;

/// The daemon's profile on this device: this computer's own, as `tilo` names it.
#[must_use]
pub fn profile() -> PathBuf {
    tilo_machine::account_daemon_profile().unwrap_or_default()
}

/// The device's worker, started once for the process: the daemon outlives every window.
fn worker() -> Result<&'static worker::Worker, String> {
    static WORKER: OnceLock<Result<worker::Worker, String>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let runs_apps = declare_carried_runtimes().inspect_err(|why| {
                tracing::error!(why, "this phone runs no workspace apps");
            });
            worker::Worker::start(&profile(), notifier(), runs_apps, || {
                window_wake()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .wake();
            })
            .map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// What wakes the phone's window when its device moved: the window open now, which takes its
/// place here as it opens ([`opened`]). The worker starts before any window, and outlives each
/// one a rebuild replaces.
fn window_wake() -> &'static std::sync::Mutex<ui::wake::WakeHandle> {
    static WAKE: OnceLock<std::sync::Mutex<ui::wake::WakeHandle>> = OnceLock::new();
    WAKE.get_or_init(|| std::sync::Mutex::new(ui::wake::WakeHandle::detached()))
}

/// The open window's place in [`window_wake`], for as long as it is open: when it closes, the
/// device wakes no window until the next one opens.
pub struct WindowWake(Option<usize>);

impl WindowWake {
    fn take_place(wake: ui::wake::WakeHandle) -> Self {
        let key = wake.scope_key();
        *window_wake()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = wake;
        Self(key)
    }
}

impl Drop for WindowWake {
    fn drop(&mut self) {
        let mut held = window_wake()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A window built since holds the place now: it stays.
        if held.scope_key() == self.0 {
            *held = ui::wake::WakeHandle::detached();
        }
    }
}

/// Start this device's daemon in the app's own process, as the app's launch does before its window
/// opens: the phone is Tilo on this device, so its window shows a device that already runs, and
/// a file provider Android called first finds the same one.
///
/// The phone is its own launcher, so once a person's identity has the device it makes its window's
/// connection to the workspace here, as a launcher makes one for a process it starts, and hands it
/// to the window ([`ui::launch::hand_connection`]): what the window keeps is then the workspace's.
/// Before that the device is no one's, and the sign-in window keeps what it has on the phone.
///
/// # Errors
/// The daemon did not start: what the window shows in its place.
pub fn start() -> Result<(), String> {
    static HANDED: OnceLock<()> = OnceLock::new();
    let worker = worker()?;
    HANDED.get_or_init(|| hand_window_connection(worker));
    Ok(())
}

/// Hand the phone's window its connection to the person's workspace, once a person's identity has
/// the device, or why it has none: the window then does not open rather than keep the person's
/// records on the phone alone. Nothing before anyone signed in.
fn hand_window_connection(worker: &worker::Worker) {
    let made = worker
        .files()
        .ask(|device| {
            let devices = device.devices().map_err(|error| format!("{error:#}"))?;
            if !devices.signed_in {
                return Ok(None);
            }
            // Before the person picks a workspace, the phone's window is in their own, once the
            // phone holds it: until then the window opens on the phone, and joins it when it
            // arrives (`PhoneView::follow_workspace`).
            let workspace = devices
                .own
                .clone()
                .ok_or("this phone holds no certificate of yours yet")?;
            if !holds(&devices, &workspace) {
                return Ok(None);
            }
            window_connection(device, &workspace).map(|handed| Some((handed, workspace)))
        })
        .and_then(|made| made);
    let (handed, workspace) = match made {
        Ok(None) => return,
        Ok(Some(made)) => made,
        Err(why) => {
            ui::launch::hand_connection(Err(unkept(&why)));
            return;
        }
    };
    // What the apps kept before anyone signed in is the person's now: it goes into their own
    // workspace, which every device of theirs finds by their key alone, before any of their
    // windows opens again.
    carry_kept(worker, &workspace);
    ui::launch::hand_connection(Ok(handed));
}

/// Whether the phone holds `workspace`, so a window can join it: one it lists.
pub(crate) fn holds(devices: &tilo_runtime::in_process::Devices, workspace: &str) -> bool {
    devices.workspaces.iter().any(|held| held.key == workspace)
}

/// The connection the phone's window joins `workspace` over, made by the phone's `device`.
///
/// # Errors
/// The device made none, or `workspace` names no workspace.
pub(crate) fn window_connection(
    device: &tilo_runtime::in_process::Device,
    workspace: &str,
) -> Result<ui::collaboration::HandedConnection, String> {
    let connection = device
        .window_connection(TILO_PHONE, workspace)
        .map_err(|error| format!("{error:#}"))?;
    let workspace = uuid::Uuid::parse_str(workspace)
        .map_err(|_| format!("{workspace} does not name a workspace"))?;
    Ok(ui::collaboration::HandedConnection {
        collaboration: connection.collaboration,
        identity: connection.identity,
        app: TILO_PHONE.to_owned(),
        workspace,
    })
}

/// What the window says in place of itself when it has no connection to its workspace: `why`.
pub(crate) fn unkept(why: &str) -> String {
    tracing::error!(why, "the phone's window has no connection to its workspace");
    format!("Tilo couldn't connect to your workspace, so nothing here would be kept: {why}")
}

/// Carry what the phone's apps kept before anyone signed in into the person's own workspace,
/// which every device of theirs finds by their key alone, installing each app there first and
/// then through the connection its window would join. Say for each app whose install or carry
/// could not be made why: they stay on the phone until they are.
fn carry_kept(worker: &worker::Worker, workspace: &str) {
    let Some(dir) = storage_dir() else {
        return;
    };
    if !dir.is_dir() {
        return;
    }
    let bundles = match bundles() {
        Ok(bundles) => bundles,
        Err(why) => {
            tracing::error!(why, "what the apps kept before sign-in is not carried");
            return;
        }
    };
    let id = view::workspace_id(workspace);
    let made = |app: &str| {
        let bundle = view::bundle_of(app, &bundles)?;
        let (person, collaboration, identity) = worker.files().ask({
            let app = app.to_owned();
            let workspace = workspace.to_owned();
            move |device| {
                device
                    .window_connection(&app, &workspace)
                    .map(|connection| {
                        (
                            connection.person,
                            connection.collaboration,
                            connection.identity,
                        )
                    })
                    .map_err(|error| format!("{error:#}"))
            }
        })??;
        let route = tilolang_process::route_of(&bundle.meta, id, &person);
        // A window's connection is served as the app it runs: its writes are the app's own, and
        // a workspace keeps them by the grant installing the app gave it.
        let as_app = Some(bundle.meta.app.clone());
        Ok(tilolang_process::carry::Handed {
            bundle,
            route,
            collaboration,
            identity,
            person,
            as_app,
        })
    };
    // The app goes into the person's own workspace as the person, from the package this phone
    // verified, before any of what it kept is written there.
    let installed = |app: &str| {
        worker.files().ask({
            let app = app.to_owned();
            let workspace = workspace.to_owned();
            move |device| {
                device
                    .install_app(&workspace, &app)
                    .map_err(|error| format!("{error:#}"))
            }
        })?
    };
    let carried = tilolang_process::carry::signed_out(&dir, made, installed);
    match carried {
        Ok(carried) => {
            for (app, why) in carried.refused() {
                tracing::error!(
                    app,
                    why,
                    "what the app kept before sign-in is still on the phone"
                );
            }
        }
        Err(why) => tracing::error!(why, "what the apps kept before sign-in is not carried"),
    }
}

/// Where the phone keeps what its apps write before anyone signed in: this app's data directory,
/// then `tilolang`. The window's view keeps an app's documents there, and the carry reads them
/// from there once a person's identity has the device.
fn storage_dir() -> Option<PathBuf> {
    tilo_paths::AppPaths::for_app(TILO_PHONE)
        .data()
        .native_path()
        .ok()
        .map(|data| data.join("tilolang"))
}

/// The launch's [`start`]: a daemon that did not start is what the window shows, so the window
/// opens either way. Only Android's entry calls it: elsewhere nothing launches the phone's
/// window but a test or a developer's host, which calls [`start`].
#[cfg(target_os = "android")]
fn start_device() {
    if let Err(why) = start() {
        tracing::error!(why, "the phone's device did not start");
    }
}

/// Declare the runtime builds this app carries in itself, as the signed manifest the APK it runs
/// from carries names them: the exact Tilo runtime build it links and the tilolang runtime it
/// runs tilolang apps on. A workspace app that names one of them then installs on this phone
/// without it being fetched, and one that names another is refused with both named.
///
/// # Errors
/// The APK's manifest is missing, unreadable or not its build's, or names no exact build: why
/// this phone runs none of a workspace's apps, which its Apps page shows.
fn declare_carried_runtimes() -> Result<(), String> {
    let Some(manifest) = signed_manifest()? else {
        // A developer's build has no APK: its runtimes are the device's own, as a desktop's are.
        return Ok(());
    };
    tilo_runtime::device::packages::carry_runtimes(&manifest)
}

/// This app's own signed package manifest, as the APK it was installed from carries it: the
/// build's statement of the runtime it links, which packaging stamps and signs.
///
/// # Errors
/// The APK carries none, or it does not read or verify.
#[cfg(target_os = "android")]
fn signed_manifest() -> Result<Option<tilo_app_package::Manifest>, String> {
    let bytes = ui::platform::android::read_asset("tilo/manifest.toml")
        .map_err(|error| format!("this app's own manifest can't be read: {error}"))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("this app's own manifest is not text: {error}"))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&text)
        .map_err(|error| format!("this app's own manifest does not parse: {error}"))?;
    manifest
        .verify_signature()
        .map_err(|error| format!("this app's own manifest is not its build's: {error}"))?;
    Ok(Some(manifest))
}

/// `None` off Android: a build with no APK carries no manifest of its own.
#[cfg(not(target_os = "android"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Android's reads the APK, which can fail"
)]
fn signed_manifest() -> Result<Option<tilo_app_package::Manifest>, String> {
    Ok(None)
}

/// The phone's device as a file provider reaches it: the same worker the view asks, started here
/// when Android calls a provider before any window of the app opened in this process. A person
/// signs in in the app, and the provider's roots are theirs as soon as the view's are.
///
/// # Errors
/// The daemon did not start.
pub fn files() -> Result<worker::Files, String> {
    worker().map(worker::Worker::files)
}

/// What the phone shows a notification through: the system's, on Android; elsewhere the
/// platform's own, which is this computer's for a developer running the phone app on it.
fn notifier() -> std::sync::Arc<dyn tilo_process_services::notify::Notifier> {
    #[cfg(target_os = "android")]
    {
        tilo_process_services::native::android::services(Some(TILO_PHONE)).notifications
    }
    #[cfg(not(target_os = "android"))]
    {
        tilo_process_services::notify::system(Some(TILO_PHONE))
    }
}

/// The phone app's package id, as the platform knows it.
const TILO_PHONE: &str = "so.tilo.phone";

/// The apps the phone opens: those the APK carries; elsewhere (a test, a developer's computer)
/// those under `TILO_PHONE_APPS`.
fn bundles()
-> Result<std::collections::BTreeMap<String, std::sync::Arc<tilolang_tilo::bundle::Bundle>>, String>
{
    #[cfg(target_os = "android")]
    {
        let data = tilo_paths::data_dir().ok_or("no app data directory")?;
        bundles::installed(&data)
    }
    #[cfg(not(target_os = "android"))]
    {
        std::env::var_os("TILO_PHONE_APPS").map_or_else(
            || Ok(std::collections::BTreeMap::default()),
            |dir| bundles::read(std::path::Path::new(&dir)),
        )
    }
}

fn opened(_cx: &ViewCx<Workspace>) -> Result<view::PhoneView, String> {
    let worker = worker()?.clone();
    let storage = storage_dir();
    let wake = ui::wake::WakeHandle::new();
    let woken = WindowWake::take_place(wake.clone());
    view::PhoneView::new(
        worker,
        bundles()?,
        storage,
        std::sync::Arc::new(move || wake.wake()),
        woken,
    )
}

impl ResourceApp for Phone {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.phone";
    const TITLE: &'static str = "Tilo";
    const ICON: Option<&'static str> = Some("tilo");
    const WORKSPACE_RESOURCE: bool = true;
    /// The phone runs the device's daemon in its own process ([`start`]): there is no runtime
    /// beside it to attach to. Once a person signed in, its window joins their workspace over
    /// the connection [`start`] handed it; before that its records are this device's own.
    const ATTACHES_RUNTIME: bool = false;

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        match opened(cx) {
            Ok(view) => cx.surface(view),
            Err(why) => {
                tracing::error!(why, "the phone's view did not open");
                Box::new(ui::widget::EmptyState::new(why))
            }
        }
    }
}

ui::dev_reload_entry!(Phone, init = start_device);
