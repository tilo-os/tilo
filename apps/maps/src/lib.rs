#![allow(
    clippy::manual_midpoint,
    reason = "migration stack lint debt is scoped to this target"
)]
// App-local arithmetic checks are relaxed in an isolated process; panic lints stay denied.
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
pub mod basemap;
pub mod canvas;
pub mod draw;
mod endpoint_editor;
mod failure;
pub mod geo;
mod hand_laid;
mod live;
mod map_controls;
mod map_info_sheet;
pub mod model;
pub mod net;
pub mod offline_areas;
mod offline_panel;
pub mod place_search;
pub mod rank;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;
#[cfg(feature = "samples")]
mod scenario;

pub mod tiles;
pub mod traffic;
pub mod workspace;

pub use app::MapsApp;
pub use model::{Category, LatLon, MapModel, Panel, Place, Route, TransitJourney, TravelMode};

pub use resource::MapsShell;

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

pub use workspace::Workspace;

ui::dev_reload_entry!(MapsShell);

#[cfg(test)]
mod locale_tests {
    use std::collections::BTreeSet;

    fn keys(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .filter(|line| !line.starts_with(['#', ' ', '\t']) && line.contains(" = "))
            .filter_map(|line| line.split(" = ").next())
            .collect()
    }

    #[test]
    fn the_german_catalogue_carries_every_key() {
        let english = include_str!("../locales/en-US.ftl");
        let german = include_str!("../locales/de-DE.ftl");
        let english = keys(english);
        let german = keys(german);
        let missing: Vec<_> = english.difference(&german).collect();
        assert!(
            missing.is_empty(),
            "de-DE is missing {} key(s): {missing:?}",
            missing.len()
        );
        let extra: Vec<_> = german.difference(&english).collect();
        assert!(extra.is_empty(), "de-DE has unknown key(s): {extra:?}");
        assert!(!english.is_empty() && !german.is_empty());
    }
}
