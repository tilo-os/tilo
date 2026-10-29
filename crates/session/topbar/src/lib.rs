//! The desktop's top bar as an app: workspace and strip navigation, the
//! people here, the time (or a live activity over it), the status tray and
//! the running applications' tray items, the workspace menu and the profile
//! card, on the full bar of the display in use and the compact bar of
//! every other display.
//!
//! The bar draws what the desktop lends it (its workspaces, status and
//! people, and where each display's bar goes, [`link`]) and publishes what
//! can be done from it as its actions ([`TopBarEffect`]). The shell mounts
//! it in its own process over a feed of what it knows; on its own, in a
//! capture or a test, it shows a fixed desktop ([`fixture`]).

pub mod fixture;
pub mod link;
mod prepare;
pub mod resource;
pub mod surface;

pub use link::{Ask, BarFeed, BarPlace, BarService, Command, Link, MediaCommand, Placement, Shown};
pub use prepare::prepare;
pub use resource::{TopBar, TopBarApp, TopBarEffect};
pub use surface::TopBarSurface;

/// The bar's package id.
pub const APP_ID: &str = "so.tilo.topbar";

/// The bar's toolkit definition, the one every host mounts, carrying its
/// committed surface.
#[must_use]
pub fn definition() -> &'static ui::prelude::ApplicationDefinition {
    ui::built_in_definition!(TopBarApp)
}
