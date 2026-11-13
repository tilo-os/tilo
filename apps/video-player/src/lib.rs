//! A local video player on the shared media streaming layer and the toolkit's
//! [`ui::widget::VideoPlayer`] widget.

// An app runs as its own sandboxed process: a crash costs one app, not the desktop,
// so wrapping arithmetic and casts are allowed here. The panic lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod app;
mod i18n;
mod kept;
mod preferences;
pub mod resource;
pub mod shared;
pub use app::PlayerApp;
pub use app::source::{source_for_argument, source_for_path};
pub use resource::VideoPlayerShell;
pub use shared::Library;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(VideoPlayerShell)
}
