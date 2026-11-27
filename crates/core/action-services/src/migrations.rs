use {
    tilo_actions::ActionAddress,
    tilo_actions::remote::{CatalogueQuery, CatalogueRow, RemoteActionTarget},
};

/// The provider that answers a device's migrations: the engine's own name, which the Migrations
/// app calls its actions by.
pub const PROVIDER: &str = "so.tilo.migrate.engine";
/// The kind of a device's migrations resource.
pub const RESOURCE_KIND: &str = "devices.migrate";
/// What a source holds; a cloud drive a folder and a page at a time.
pub const REVIEW: &str = "review";
/// What of a source comes over.
pub const CHOOSE: &str = "choose";
/// Bring the chosen things over, as one run.
pub const START: &str = "start";
/// How a source's migration stands: what failed, what was left out, where it went.
pub const STATUS: &str = "status";

/// The catalogue query that finds this device's migrations.
#[must_use]
pub fn query() -> CatalogueQuery {
    CatalogueQuery {
        resource_name: Some(RESOURCE_KIND.to_owned()),
        ..CatalogueQuery::default()
    }
}

/// The migrations resource among `rows`, as the provider token and the resource id the
/// steps are actions of.
#[must_use]
pub fn found(rows: &[CatalogueRow]) -> Option<(String, String)> {
    rows.iter()
        .find(|row| row.resource.kind == RESOURCE_KIND)
        .map(|row| (row.provider.clone(), row.resource.id.clone()))
}

/// The step `step` of the migrations `found` names.
#[must_use]
pub fn target((provider, resource): &(String, String), step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: provider.clone(),
        address: ActionAddress::new(resource.as_str(), step),
    }
}
