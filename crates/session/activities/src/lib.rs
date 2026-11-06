//! The desk's floating activity cards as an app: every activity's summary
//! drawn in one card, docked in a corner column or dragged loose, over the
//! windows and clear of the bar, the banners and the readout.
//!
//! The layer decides nothing about the activities themselves. The session
//! publishes what the apps show ([`link::Feed`]: their cards and where the
//! layer may stand) and carries out what the person did ([`link::Ask`]).
//! The cards' frame, presses, drag and accessibility are
//! `tilo_desktop_surface::floating_layer`'s; this app is the surface the
//! session mounts on its own layer.

pub mod fixture;
pub mod link;
mod resource;
pub mod surface;

pub use link::{ActivityService, Ask, Feed, Link, Shown};
pub use resource::{Activities, ActivitiesApp, ActivityEffect};
pub use surface::ActivitiesSurface;

/// The layer's package id.
pub const APP_ID: &str = "so.tilo.activities";

/// The layer's toolkit definition, the one every host mounts, carrying its
/// committed surface.
#[must_use]
pub fn definition() -> &'static ui::prelude::ApplicationDefinition {
    ui::built_in_definition!(ActivitiesApp)
}

/// Mounting the layer where the session has no in-process apps of its own to
/// mount it among (a test's or a capture's chrome).
///
/// # Errors
/// The component could not be prepared.
pub fn prepare(
    link: Link,
    device: &str,
    person: &str,
) -> Result<ui::application::AppComponent, ui::launch::LaunchError> {
    ui::application::prepare_private_surface(
        definition(),
        device,
        person,
        "activities",
        0,
        |context| context.lend(ActivityService { link }),
    )
}
