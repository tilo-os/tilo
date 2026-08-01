//! Software — qOS native app store (CRDT-native, built on the `ui`
//! toolkit).
//!
//! Browses and installs Flatpak / OCI applications for the immutable
//! bootc OS. The mutable state (installed set, in-flight installs) lives
//! in the shared Automerge document; the catalog and ephemeral navigation
//! live in the root [`app::SoftwareStore`] widget.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer (see `main.rs`).
//!
//! # Module map
//!
//! * [`model`]     — pure data: catalog, [`AppEntry`], install-state
//!                   derivation, reviews, compat notices, the qOS app model.
//! * [`catalog`]   — the bundled sample catalog (a dozen+ apps + 2 qOS
//!                   apps; GIMP is the fully-fleshed detail page).
//! * [`doc`]       — Automerge document helpers (installed / installing).
//! * [`flatpak`]   — the data-source layer: `flatpak` CLI parsing +
//!                   Flathub API v2 JSON. **Only ever called on the worker
//!                   thread.**
//! * [`flatpakref`]— `.flatpakref` (INI keyfile) parsing + the launch-arg
//!                   scan that makes this the handler for that file type.
//! * [`worker`]    — the background thread + `mpsc` channels. The single
//!                   rule: nothing that can block a frame runs on the UI
//!                   thread. Mirrors the maps tile worker.
//! * [`app`]       — the root widget: five-pass frame, all screens, the
//!                   async drain/dispatch glue, gated install confirms.
//! * [`actions`]   — resource + action-model status: the honest finding that
//!                   the store's CRDT state is a projection of the external
//!                   package manager, so it exports no actions (proof tests).
//!
//! # Registering as the `.flatpakref` handler
//!
//! The store advertises itself as the handler for
//! `application/vnd.flatpak.ref` so opening a `.flatpakref` from the file
//! explorer / a download launches it straight into the install-confirm
//! flow. The qOS app `.desktop` entry (shipped under `os/`) carries:
//!
//! ```desktop
//! [Desktop Entry]
//! Name=Software
//! Exec=software_store %f
//! MimeType=application/vnd.flatpak.ref;
//! ```
//!
//! `%f` passes the opened file path; [`flatpakref::ref_path_from_args`]
//! picks it out of `argv`, and `app_root()` opens the confirm sheet.

pub mod actions;
pub mod app;
pub mod catalog;
pub mod doc;
pub mod draw;
pub mod flatpak;
pub mod flatpakref;
pub mod model;
pub mod probe;
pub mod worker;

pub use app::SoftwareStore;
pub use flatpakref::FlatpakRef;
pub use model::{
    AppEntry, Catalog, Category, InstallPhase, InstallState, QosAppModel, UpdateProgress,
};
pub use worker::{Request, Worker, WorkerMsg};

/// Build and return the app's root widget tree.
///
/// In the live app this spawns the background [`Worker`] (which queries
/// `flatpak` + the Flathub API off the UI thread) and runs a best-effort
/// `flatpak` / `podman` probe so the install state reflects the system. In
/// the `--shots` path (detected by the `--shots` argument) both are
/// skipped, so the screenshot renders deterministically from the bundled
/// sample catalog + default seed — fully populated, fully offline.
///
/// If the process was launched with a `.flatpakref` path argument, the
/// store opens straight into the install-confirmation flow for that ref
/// (the store registers as the `application/vnd.flatpak.ref` handler).
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    let args: Vec<String> = std::env::args().collect();
    let is_shots = args.iter().any(|a| a == "--shots" || a.starts_with("--shots="));

    // `SOFTWARE_STORE_SHOT_LIVE` renders the real live cold-start (empty
    // catalog → "Loading apps from Flathub…") for verification, instead of the
    // offline sample. The single shot frame captures the loading state before
    // any network result can land.
    let shot_live = is_shots && std::env::var_os("SOFTWARE_STORE_SHOT_LIVE").is_some();
    let (probe, worker) = if shot_live {
        (None, worker::Worker::spawn())
    } else if is_shots {
        (None, worker::Worker::offline())
    } else {
        (Some(probe::probe_system()), worker::Worker::spawn())
    };
    let mut store = app::SoftwareStore::with_worker(probe, worker);

    // `.flatpakref` handler: opening one drives the install flow for it.
    if let Some(path) = flatpakref::ref_path_from_args(&args) {
        if let Some(r) = flatpakref::FlatpakRef::from_path(&path) {
            store.open_flatpakref(r);
        }
    }

    if is_shots {
        store.apply_shot_view_env();
    }
    Box::new(store)
}
