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

pub mod app;
pub mod catalog;
pub mod doc;
pub mod draw;
pub mod model;
pub mod probe;

pub use app::SoftwareStore;
pub use model::{AppEntry, Catalog, Category, InstallState};

/// Build and return the app's root widget tree.
///
/// In the live app this runs a best-effort `flatpak` / `podman` probe so
/// the install state reflects what is already on the system. In the
/// `--shots` path (detected by the `--shots` argument) the probe is
/// skipped, so the screenshot renders deterministically from the bundled
/// sample catalog + default seed.
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    let is_shots = std::env::args().any(|a| a == "--shots" || a.starts_with("--shots="));
    let probe = if is_shots { None } else { Some(probe::probe_system()) };
    let mut store = app::SoftwareStore::new(probe);
    if is_shots {
        store.apply_shot_view_env();
    }
    Box::new(store)
}
