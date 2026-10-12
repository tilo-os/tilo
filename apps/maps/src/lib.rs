#![allow(
    clippy::manual_clamp,
    clippy::manual_midpoint,
    clippy::needless_pass_by_value,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Maps — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! An OpenStreetMap-style maps app with search (Nominatim), turn-by-turn
//! routing (OSRM) across car/bike/foot, and public-transport journey
//! planning (Transitous/MOTIS). The map itself is a custom Vello-drawn
//! canvas: a stylised vector basemap, a drawn route polyline, and place
//! markers, all coloured from the OS `Theme`.
//!
//! [`MapsShell`] is the app every host runs and mounts ([`definition`]). A
//! live window fetches real data on background threads; a `--shots`
//! capture renders the bundled sample city from its scenario's fixture and
//! actions, so the screenshot is fully populated offline. Finding places is
//! the platform search ([`place_search`]).

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
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod canvas;
pub mod draw;
mod endpoint_editor;
pub mod geo;
mod hand_laid;
mod map_controls;
mod map_info_sheet;
pub mod model;
pub mod net;
pub mod offline_areas;
mod offline_panel;
pub mod place_search;
pub mod rank;
pub mod resource;
pub mod sample;
mod scenario;
pub mod tiles;
pub mod traffic;
pub mod workspace;

pub use app::MapsApp;
pub use model::{Category, LatLon, MapModel, Panel, Place, Route, TransitJourney, TravelMode};

pub use resource::MapsShell;

/// The map's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

/// The map's messages, in every host that mounts it.
pub(crate) static CATALOG: ui::i18n::EmbeddedCatalog =
    ui::i18n::EmbeddedCatalog::new("en-US", LOCALES).with_system_locale();
pub use workspace::Workspace;

ui::dev_reload_entry!(MapsShell);
