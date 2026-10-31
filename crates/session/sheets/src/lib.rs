//! The sheets the desktop shows over everything, as ordinary apps.
//!
//! Each sheet is a framework app: a resource that publishes its actions, a
//! view built from toolkit widgets, and scenarios that render it headless.
//! The shell mounts each one in its own process and shows it over the
//! desktop; what only the shell can do for a sheet (power the machine down,
//! say whether the sheet is up) it lends the app as a service of plain data
//! in and requests out ([`ui::application::HostContext::lend`]).

pub mod dictation;
pub mod directory;
pub mod display_confirm;
pub mod displays;
pub mod file_picker;
pub mod lock;
pub mod portal;
pub mod power;
mod presentation;
pub mod shortcut_guide;
pub mod workspace_settings;

pub use presentation::{Phase, Presentation, Seat};

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// The sheet's word for `key`, in the person's language: for what the
/// session composes into a sheet it shows here.
#[must_use]
pub fn word(key: &str) -> String {
    t(key)
}

/// The same, with `args` filled in.
#[must_use]
pub fn words(key: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, ui::i18n::ArgValue::from(*value)))
        .collect();
    t_args(key, &args)
}
