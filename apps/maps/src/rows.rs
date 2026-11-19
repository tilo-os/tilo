//! What the map window shows that the shared map does not hold, published
//! beneath the resource as rows of declared types: a search result, a point
//! of interest, a saved place, a past search and an offline area. Each row is
//! an item of its type, so search names what it is and the effects it offers
//! are the type's, bound to it.

use ui::framework::LiveRow;
use ui::tilo_resource;

use crate::resource::MapsEffect;

/// One place a search found.
#[derive(tilo_resource::Row)]
#[row(
    kind = "maps.search-result",
    label = "Search result",
    effects = MapsEffect
)]
pub(crate) struct SearchResult;

/// One point of interest the map shows nearby.
#[derive(tilo_resource::Row)]
#[row(
    kind = "maps.poi",
    label = "Point of interest",
    effects = MapsEffect
)]
pub(crate) struct Poi;

/// One place the person saved.
#[derive(tilo_resource::Row)]
#[row(kind = "maps.place", label = "Place", effects = MapsEffect)]
pub(crate) struct Place;

/// A search the person made before.
#[derive(tilo_resource::Row)]
#[row(
    kind = "maps.search-history",
    label = "Past search",
    effects = MapsEffect
)]
pub(crate) struct PastSearch;

/// A region kept offline on this device.
#[derive(tilo_resource::Row)]
#[row(
    kind = "maps.region",
    label = "Offline region",
    effects = MapsEffect
)]
pub(crate) struct OfflineRegion;

pub(crate) fn search_result_row(id: &str, name: &str) -> LiveRow {
    LiveRow::typed::<SearchResult>(format!("maps.search-result:{id}"), name)
}

pub(crate) fn poi_row(id: &str, name: &str) -> LiveRow {
    LiveRow::typed::<Poi>(format!("maps.poi:{id}"), name)
}

pub(crate) fn place_row(id: &str, name: &str) -> LiveRow {
    LiveRow::typed::<Place>(format!("maps.place:{id}"), name)
}

pub(crate) fn history_row(query: &str) -> LiveRow {
    LiveRow::typed::<PastSearch>(format!("maps.search-history:{query}"), query)
}

pub(crate) fn area_row(id: &str, label: &str) -> LiveRow {
    LiveRow::typed::<OfflineRegion>(format!("maps.region:{id}"), label)
}
