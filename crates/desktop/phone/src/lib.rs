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
pub mod view;
pub mod worker;

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
            let wake = ui::wake::WakeHandle::new();
            worker::Worker::start(&profile(), move || wake.wake()).map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

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
