//! The sheets the desktop shows over everything, as ordinary apps.
//!
//! Each sheet is a framework app: a resource that publishes its actions, a
//! view built from toolkit widgets, and scenarios that render it headless.
//! The shell mounts each one in its own process and shows it over the
//! desktop; what only the shell can do for a sheet (power the machine down,
//! say whether the sheet is up) it lends the app as a service of plain data
//! in and requests out ([`ui::application::HostContext::lend`]).

pub mod add_app;
pub mod delete_workspace;
pub mod dictation;
pub mod directory;
pub mod display_confirm;
pub mod displays;
pub mod file_picker;
pub mod invite;
pub mod keyboard_shortcuts;
pub mod lock;
pub mod pair_device;
pub mod portal;
pub mod power;
mod presentation;
pub mod shortcut_guide;
pub mod uninstall;
pub mod workspace_settings;

pub use presentation::{Phase, Presentation, Seat};

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// The sheet's word for `key`, in the person's language: for what the
/// session composes into a sheet it shows here.
#[must_use]
pub fn word(key: &str) -> String {
    t(key)
}

/// The widest a sheet draws a link as a code, quiet zone and all: big enough for a phone held near
/// the screen to read, and small enough that the sheet is about the link, not the code.
pub const CODE_MAX_PX: usize = 200;

/// `link` as a code a phone camera reads: kept out of screenshots, since it admits whoever scans it.
pub(crate) fn code(link: &str, id: &str, alt: &str) -> ui::widget::QrWidget {
    ui::widget::QrWidget::new(link.as_bytes().to_vec())
        .with_secret()
        .with_id(id)
        .with_alt(alt)
        .with_width_at_most(CODE_MAX_PX)
}

/// `code` centred across its sheet.
pub(crate) fn centred(code: ui::widget::QrWidget) -> ui::widget::Element {
    let row = ui::widget::Row::new().with_justify(ui::taffy::JustifyContent::Center);
    Box::new(row.push(Box::new(code)))
}

/// `text` as small print: what a sheet says under a part of it.
pub(crate) fn caption(text: &str) -> ui::widget::Element {
    Box::new(ui::widget::Text::new(text).with_role(ui::prelude::TextRole::Caption))
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
