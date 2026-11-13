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

//! Disk Imager: writing an OS image to a drive, and installing Tilo.
//!
//! One wizard runs both: a source, a drive, the settings the target keeps, then
//! the write and its read-back. Writing is gated twice: the host's typed
//! confirmation, then the platform authenticating a short-lived helper.
//!
//! [`ImagerShell`] is the app: this wizard plus the guided setup (`--setup`) and
//! the folder imports (`--import-folders`), over one resource.

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
mod contents;
pub mod customize;
pub mod customize_image;
pub mod handoff;
mod helpers;
mod host;
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
mod window_view;
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

#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_home_export_service() -> std::io::Result<()> {
    imager_backend::setup::home_capture::run_provider()
}
#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn home_export_worker_protocol() -> u32 {
    imager_backend::setup::home_capture::worker_protocol()
}

#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_retained_source_io_service() -> std::io::Result<()> {
    imager_backend::retained_source::run(false)
}

#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_retained_home_io_service() -> std::io::Result<()> {
    imager_backend::retained_source::run(true)
}

#[doc(hidden)]
#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub fn run_retained_target_projection_service() -> std::io::Result<()> {
    imager_backend::retained_source::run_target_projection()
}
