//! Tilo on a phone (`so.tilo.phone`): the device daemon every Tilo computer runs, started on this app's
//! own threads, since a phone runs no daemon process of its own; and the shell's `phone` view,
//! drawn natively over what that daemon reads. A person signs in with a ticket another of their
//! devices printed, picks one of their workspaces, and opens its apps, each the tilolang app
//! the APK carries, run in tilolang-process's in-process window. Every view here is the shell's
//! tl package's; this crate hosts it and draws nothing of its own.
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
    tilo_paths::machine::account_daemon_profile().unwrap_or_default()
}

/// The device's worker, started once for the process: the daemon outlives every window.
fn worker() -> Result<&'static worker::Worker, String> {
    static WORKER: OnceLock<Result<worker::Worker, String>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            declare_carried_runtimes();
            let wake = ui::wake::WakeHandle::new();
            worker::Worker::start(&profile(), notifier(), move || wake.wake())
                .map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Declare the runtime builds this app carries in itself, as the signed manifest the APK it runs
/// from carries names them: the exact Tilo runtime build it links and the tilolang runtime it
/// runs tilolang apps on. A workspace app that names one of them then installs on this phone
/// without it being fetched, and one that names another is refused with both named.
fn declare_carried_runtimes() {
    let Some(manifest) = signed_manifest() else {
        // A developer's build has no APK: its runtimes are the device's own, as a desktop's are.
        return;
    };
    if let Err(error) = tilo_runtime::device::packages::carry_runtimes(&manifest) {
        tracing::warn!(%error, "the runtimes this app carries are not declared");
    }
}

/// This app's own signed package manifest, as the APK it was installed from carries it: the
/// build's statement of the runtime it links, which packaging stamps and signs. `None` where the
/// app runs from a developer's build, or the manifest does not verify.
#[cfg(target_os = "android")]
fn signed_manifest() -> Option<tilo_app_package::Manifest> {
    let bytes = ui::launch::android::read_asset("tilo/manifest.toml")
        .inspect_err(|error| tracing::warn!(%error, "this app carries no signed manifest"))
        .ok()?;
    let text = String::from_utf8(bytes)
        .inspect_err(|error| tracing::warn!(%error, "this app's manifest is not text"))
        .ok()?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&text)
        .inspect_err(|error| tracing::warn!(%error, "this app's manifest does not parse"))
        .ok()?;
    manifest
        .verify_signature()
        .inspect_err(|error| tracing::warn!(%error, "this app's manifest is not its build's"))
        .ok()?;
    Some(manifest)
}

/// `None` off Android: a build with no APK carries no manifest of its own.
#[cfg(not(target_os = "android"))]
fn signed_manifest() -> Option<tilo_app_package::Manifest> {
    None
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

fn opened(cx: &ViewCx<Workspace>) -> Result<view::PhoneView, String> {
    let worker = worker()?.clone();
    let storage = cx
        .storage()
        .files()
        .paths()
        .data()
        .native_path()
        .ok()
        .map(|data| data.join("tilolang"));
    let wake = ui::wake::WakeHandle::new();
    view::PhoneView::new(
        worker,
        bundles()?,
        storage,
        std::sync::Arc::new(move || wake.wake()),
    )
}

impl ResourceApp for Phone {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.phone";
    const TITLE: &'static str = "Tilo";
    const ICON: Option<&'static str> = Some("tilo");
    const WORKSPACE_RESOURCE: bool = true;

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

ui::dev_reload_entry!(Phone);
