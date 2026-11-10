//! The Tilo UI runtime in the browser.
//!
//! The page mounts a real application component (`ui::application`) through
//! the same frame pipeline the native runner uses and presents it with Vello
//! on a WebGPU canvas. `web/host.mjs` owns the DOM: input and IME, clipboard
//! events, the accessibility projection, font fetching and `IndexedDB`; this
//! crate owns the component, its records and the GPU. Everything runs on the
//! page's thread.
//!
//! Everything is browser-only;
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

// Native builds compile it for its tests; the page is its only user.
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser page fetches deferred code")
)]
mod app_load;
// The desk's arranging actions in the page: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "only the browser page arranges its desk; the host runs its tests"
    )
)]
mod arranging;
// Native builds compile it for its tests; the page is its only user.
#[cfg(target_arch = "wasm32")]
mod background_work;
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser page opens windows")
)]
mod capture_declarations;
// Holding Console in front in the console layout: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser page holds a window in front")
)]
mod console_hold;
// Content handed to an app, opened as its file: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
mod content_open;
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
    startup: tilo_feedback_transport::Startup,
) -> Result<(), String> {
    feedback_code::register_feedback_startup(peer, startup)
}
/// Prepare the runtime before anything runs in it; the module's start
/// function calls it first.
///
/// The page has one thread, and rayon makes it the only worker of the first
/// pool built on it. When an app's `par_iter` built the global pool first,
/// the CPU pool (`ui::rt::cpu_pool`) could no longer take the thread and
/// panicked the whole desktop; built first, both share it.
///
/// It also has the frame loop render the pixels apps ask for, which the
/// page cannot wait for the GPU to read back
/// ([`ui::offscreen::pixels`]).
///
/// And it has the page's `tracing` warnings and errors reach the console.
#[cfg(target_arch = "wasm32")]
pub fn start() {
    console_log::install();
    let _ = ui::rt::cpu_pool();
    browser::start_pixels();
}
/// Code the bundle fetches on first use arrived: lay out and draw every
/// mount again, so what it changes (search results, spelling marks,
/// hyphenation) shows.
#[cfg(target_arch = "wasm32")]
pub fn refresh_mounts() {
    browser::refresh_mounts();
}
/// Whether a mount has presented its first frame yet.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn first_frame_presented() -> bool {
    browser::first_frame_presented()
}
#[cfg(target_arch = "wasm32")]
mod clipboard;
#[cfg(target_arch = "wasm32")]
mod console_log;
#[cfg(target_arch = "wasm32")]
mod crash;
// The record each window shows on the workspace's shared desk: its tests run
// on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "only the browser page opens windows; the host runs its tests"
    )
)]
#[cfg(target_arch = "wasm32")]
mod desktop;
#[cfg(target_arch = "wasm32")]
mod directory;
#[cfg(target_arch = "wasm32")]
mod dom_code;
#[cfg(target_arch = "wasm32")]
pub use dom_code::{DOM_CODE, DomCode, builtin_dom};
#[cfg(target_arch = "wasm32")]
pub use over_page::{SHEETS, SheetsCode, builtin_sheets};
#[cfg(target_arch = "wasm32")]
mod feedback_code;
#[cfg(target_arch = "wasm32")]
mod feedback_composer;
#[cfg(target_arch = "wasm32")]
mod feedback_crops;
// What the person's agents on a workspace host ask them, on the page.
#[cfg(target_arch = "wasm32")]
mod agent_questions;
#[cfg(target_arch = "wasm32")]
mod guide;
#[cfg(target_arch = "wasm32")]
mod invite;
#[cfg(target_arch = "wasm32")]
mod layers;
#[cfg(target_arch = "wasm32")]
mod over_page;
#[cfg(target_arch = "wasm32")]
mod page_apps;
#[cfg(target_arch = "wasm32")]
mod page_machine;
#[cfg(target_arch = "wasm32")]
mod page_search;
#[cfg(target_arch = "wasm32")]
pub use feedback_code::{
    FEEDBACK, FeedbackCode, builtin_feedback, code_arrived as feedback_arrived,
};
#[cfg(target_arch = "wasm32")]
mod feedback_delivery;
#[cfg(target_arch = "wasm32")]
mod feedback_pick;
#[cfg(target_arch = "wasm32")]
mod feedback_review;
// Which peer carries a desktop's reports: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
mod feedback_route;
#[cfg(all(target_arch = "wasm32", feature = "deferred-feedback-composer"))]
pub use feedback_composer::install_feedback_composer;
#[cfg(target_arch = "wasm32")]
pub use feedback_composer::{FEEDBACK_COMPOSER, FeedbackComposerCode, builtin_feedback_composer};
#[cfg(target_arch = "wasm32")]
mod files;
// What the page says about a fixed chord it cannot serve: its tests run on
// the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "only the browser page answers its chords; the host runs its tests"
    )
)]
mod fixed_chords;
#[cfg(target_arch = "wasm32")]
mod frame_metrics;
#[cfg(target_arch = "wasm32")]
mod page_activity;
#[cfg(target_arch = "wasm32")]
pub use page_activity::{LoadActivity, defer_activity_pane};
#[cfg(target_arch = "wasm32")]
mod host_services;
#[cfg(target_arch = "wasm32")]
pub use host_services::{MODEL_CLIENTS, ModelClients, builtin_model_clients};
#[cfg(target_arch = "wasm32")]
mod link;
// A pairing link drawn as the code a phone reads.
#[cfg(target_arch = "wasm32")]
mod mount_service_custody;
#[cfg(target_arch = "wasm32")]
mod page_authors;
#[cfg(target_arch = "wasm32")]
mod qr_code;
// The page's user root: the envelope it is kept in, and what it signs.
#[cfg(target_arch = "wasm32")]
mod root_identity;
// The page's theme from settings and the browser: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
mod page_theme;
// Which peer speaks for the page's person: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
mod page_person;
#[cfg(target_arch = "wasm32")]
mod peer;
// The person's agent door, as the page carries it to their host.
#[cfg(target_arch = "wasm32")]
mod agent_door;
// The page's collaboration plane: the session's sync over this page's endpoint.
#[cfg(target_arch = "wasm32")]
mod peer_sync;
// The sign-in route's answer to a service (`web/sign-in.mjs`).
#[cfg(target_arch = "wasm32")]
mod sign_in_page;
// The person's own records each window holds: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
mod person_records;
// What a page times of the members' presence itself: its tests run on the
// host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg(target_arch = "wasm32")]
mod replication;
// The workspaces a host governs, as the page administers them.
#[cfg(target_arch = "wasm32")]
mod rooms;
// The workspaces this browser entered, by the rules every device keeps them by.
#[cfg(target_arch = "wasm32")]
mod entered;
// Which way the page takes into the workspace a link names, by the rules every device takes.
#[cfg(target_arch = "wasm32")]
#[cfg(target_arch = "wasm32")]
mod semantics_status;
#[cfg(target_arch = "wasm32")]
mod settings;
#[cfg(target_arch = "wasm32")]
pub(crate) mod shell;
#[cfg(target_arch = "wasm32")]
mod storage;
#[cfg(target_arch = "wasm32")]
mod throttle;
#[cfg(target_arch = "wasm32")]
mod video_pictures;
#[cfg(target_arch = "wasm32")]
mod way_in;
#[cfg(target_arch = "wasm32")]
mod window_menu;
#[cfg(target_arch = "wasm32")]
mod workspace_records;
// When graphics start-up is said to stall: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser draws; the host runs its tests")
)]
mod first_frame;
// Plain arithmetic on canvas sizes: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser draws; the host runs its tests")
)]
mod viewport;
// How many frames the GPU may still be drawing: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser draws; the host runs its tests")
)]
mod frame_pacing;
