//! The Tilo UI runtime in the browser.
//!
//! The page mounts a real application component (`ui::application`) through
//! the same frame pipeline the native runner uses and presents it with Vello
//! on a WebGPU canvas. `web/host.mjs` owns the DOM: input and IME, clipboard
//! events, the accessibility projection, font fetching and `IndexedDB`; this
//! crate owns the component, its records and the GPU. Everything runs on the
//! page's thread.
//!
//! See `docs/architecture/browser-runtime.md`. Everything is browser-only;
//! native workspace builds compile an empty library. The page's host services
//! are the platform crates' web backends (`tilo-dialogs`,
//! `tilo-clipboard`, `tilo-notify`).
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        clippy::future_not_send,
        reason = "browser futures hold JavaScript handles and run on the page's single thread"
    )
)]

#[cfg(target_arch = "wasm32")]
mod background_work;
#[cfg(target_arch = "wasm32")]
pub(crate) mod page_content;
#[cfg(target_arch = "wasm32")]
pub(crate) use page_content::page_content;
#[cfg(target_arch = "wasm32")]
mod browser;
/// Install an already-admitted feedback transport before mounting its desktop.
/// This Rust host hook is intentionally absent from URL/JSON launch options.
/// # Errors
/// Refuses a missing or mismatched certified peer and duplicate registration.
#[cfg(target_arch = "wasm32")]
pub fn register_feedback_startup(
    peer: u32,
    startup: tilo_desktop::feedback::transport::Startup,
) -> Result<(), String> {
    browser::register_feedback_startup(peer, startup)
}
/// Code the bundle fetches on first use arrived: lay out and draw every
/// mount again, so what it changes (search results, spelling marks,
/// hyphenation) shows.
#[cfg(target_arch = "wasm32")]
pub fn refresh_mounts() {
    browser::refresh_mounts();
}
#[cfg(target_arch = "wasm32")]
mod clipboard;
#[cfg(target_arch = "wasm32")]
mod crash;
#[cfg(target_arch = "wasm32")]
mod desktop;
#[cfg(target_arch = "wasm32")]
mod feedback_delivery;
#[cfg(target_arch = "wasm32")]
mod files;
#[cfg(target_arch = "wasm32")]
mod frame_metrics;
#[cfg(target_arch = "wasm32")]
mod history;
#[cfg(target_arch = "wasm32")]
pub use history::{LoadActivity, defer_activity_pane};
#[cfg(target_arch = "wasm32")]
mod host_services;
#[cfg(target_arch = "wasm32")]
pub use host_services::{MakeModelClients, ModelClients, builtin_model_clients};
#[cfg(all(target_arch = "wasm32", feature = "deferred-model-clients"))]
pub use host_services::{defer_model_clients, install_model_clients, model_clients_load_failed};
#[cfg(target_arch = "wasm32")]
mod link;
#[cfg(target_arch = "wasm32")]
mod mount_service_custody;
#[cfg(target_arch = "wasm32")]
mod page_authors;
#[cfg(target_arch = "wasm32")]
mod peer;
#[cfg(target_arch = "wasm32")]
mod replication;
#[cfg(target_arch = "wasm32")]
mod scratch;
#[cfg(target_arch = "wasm32")]
mod semantics_status;
#[cfg(target_arch = "wasm32")]
mod settings;
#[cfg(target_arch = "wasm32")]
mod shell;
#[cfg(target_arch = "wasm32")]
mod storage;
#[cfg(target_arch = "wasm32")]
mod throttle;
#[cfg(target_arch = "wasm32")]
mod viewport;
