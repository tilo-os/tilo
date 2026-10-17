#![allow(
    clippy::cast_lossless,
    clippy::cloned_ref_to_slice_refs,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::double_ended_iterator_last,
    clippy::drop_non_drop,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::ignored_unit_patterns,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_let_else,
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_const_for_thread_local,
    clippy::missing_errors_doc,
    clippy::needless_continue,
    clippy::needless_lifetimes,
    clippy::needless_option_as_deref,
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    clippy::nonminimal_bool,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::suspicious_operation_groupings,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_mut_passed,
    clippy::unnecessary_semicolon,
    clippy::unnecessary_wraps,
    clippy::unreachable,
    clippy::unreadable_literal,
    clippy::unused_io_amount,
    clippy::unused_self,
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    clippy::wildcard_imports,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Disk Imager — Tilo native app for writing an OS image to a drive, and for
//! installing Tilo (CRDT-native, built on the `ui` toolkit).
//!
//! A single guided **wizard** with a six-step stepper, covering both
//! "write this image to a USB stick" and "install Tilo onto a disk":
//!
//!   1. **Source** — pick a local `.iso` / `.img` via the platform's native
//!      file picker, or a known Tilo release.
//!   2. **Drive** — choose a target from real platform block-device discovery;
//!      internal disks are hidden behind an explicit opt-in, the **system disk**
//!      needs a second acknowledgement before it can even be selected, and an
//!      honest empty state asks the user to insert a drive.
//!   3. **Customize** (Tilo only) — the guided-customization step (hostname,
//!      first user and region), shared by the write and install flows. Plain
//!      ISOs skip it. Only the secret-free settings the live installer applies
//!      are offered; encryption and login secrets stay on the target device.
//!   4. **Confirm** — a read-only review of every choice, an erase warning that
//!      escalates for an internal / system disk, and the typed device-name gate.
//!   5. **Write / Install** — a real, byte-for-byte streaming copy with live
//!      progress, then a read-back **verify** pass. (The caption reads "Write"
//!      for a removable drive, "Install" for a disk install.)
//!   6. **Done** — the verified / failed outcome, with a fresh-start affordance.
//!
//! Writing is doubly gated: the typed confirmation, then platform-native
//! authentication for a short-lived helper. The wizard, downloads, and
//! customization remain in the user's unprivileged session.
//!
//! [`ImagerShell`] is the app: the wizard, the guided setup (`--setup`) and
//! the folder imports (`--import-folders`) are its flows over one resource. A
//! window someone uses probes real drives and starts genuinely blank; a
//! screenshot scenario stages sample content so it is deterministic offline.
//! Immediately before writing, the live inventory is read again and its
//! stable device identity, size, and system-disk status must still match the
//! confirmed target.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod customize;
pub mod customize_image;
pub mod handoff;
mod helpers;
mod host;
#[cfg(target_os = "linux")]
mod media_capacity;
#[cfg(target_os = "linux")]
pub mod media_layout;
pub mod model;
mod page;
pub mod probe;
pub mod profile;
pub mod profile_cli;
pub mod release;
pub mod resource;
pub mod setup;
pub(crate) mod windows_media;
pub mod write;
pub mod write_setup;

pub use app::ImagerApp;
pub use model::{
    Action, Filesystem, InstallProfile, Source, SourceKind, Step, TargetDrive, TargetKind,
};

pub use resource::{Imager, ImagerShell};

pub mod export_dialog;

#[cfg(target_os = "linux")]
pub use setup::media_setup;

/// Entry point for the signed, confined home-export service.
#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_home_export_service() -> std::io::Result<()> {
    setup::home_capture::run_provider()
}
/// Qualification advertises the protocol compiled into this exact worker.
#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn home_export_worker_protocol() -> u32 {
    setup::home_capture::worker_protocol()
}

/// Internal fixed retained-source worker entry, confined by the canonical launcher.
#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_retained_source_io_service() -> std::io::Result<()> {
    setup::retained_source_io::run(false)
}

/// Fixed Home-directory worker; launcher admission independently seals this mode.
#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_retained_home_io_service() -> std::io::Result<()> {
    setup::retained_source_io::run(true)
}

/// Internal target-credential FUSE worker; never accepts a mount path or grant.
#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_retained_target_projection_service() -> std::io::Result<()> {
    setup::retained_target_projection::run()
}
