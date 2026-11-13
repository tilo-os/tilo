//! Search: the launcher the desktop opens with Super, Super+Space and an app's own search,
//! as an ordinary app.

pub mod fixture;
pub mod i18n;
pub mod resource;
pub mod source;
pub mod surface;

pub use resource::{Launcher, LauncherEffect, LauncherShell};
pub use source::{
    Answer, Ask, CardFit, LauncherService, LauncherSource, Link, ResourceKind, Scope, Search,
};
pub use surface::LauncherSurface;

pub const APP_ID: &str = "so.tilo.launcher";

ui::dev_reload_entry!(LauncherShell);
