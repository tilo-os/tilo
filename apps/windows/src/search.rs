//! Platform search over the installed Windows apps.
//!
//! The catalogue is a live projection of the compatibility service, not
//! document state, so the window keeps a search catalogue of its apps in
//! step with each refresh. A hit invokes the app row's `show` action, which
//! shows the app as choosing its row does.

use std::collections::BTreeSet;

use ui::search::{CatalogSearchProvider, FieldWeight, SearchCatalog, SearchRecord, SearchResult};
use ui::tilo_actions::ActionAddress;

use crate::model::Catalogue;

const KIND: &str = "Windows apps";

/// The published row of app `id`.
pub fn app_resource_id(id: &str) -> String {
    format!("windows.app:{id}")
}

/// The action a hit for app `id` invokes: show it in the window.
pub fn show_app_address(id: &str) -> ActionAddress {
    ActionAddress::new(app_resource_id(id), "show")
}

/// The apps as search finds them, updated when the catalogue changes.
#[derive(Clone, Debug)]
pub struct AppsSearch {
    catalog: SearchCatalog,
    ids: BTreeSet<String>,
}

impl AppsSearch {
    pub fn new(catalogue: &Catalogue) -> Self {
        let mut search = Self {
            catalog: SearchCatalog::new(KIND),
            ids: BTreeSet::new(),
        };
        search.sync(catalogue);
        search
    }

    /// Take the catalogue's apps. Called on a catalogue refresh, never from
    /// a keypress; an unchanged app leaves the catalogue as it is.
    pub fn sync(&mut self, catalogue: &Catalogue) {
        let ids: BTreeSet<String> = catalogue.apps.iter().map(|app| app.id.clone()).collect();
        for gone in self.ids.difference(&ids) {
            self.catalog.remove(&app_resource_id(gone));
        }
        for app in &catalogue.apps {
            let result = SearchResult::for_action(KIND, &app.name, show_app_address(&app.id))
                .with_subtitle(app.publisher.clone());
            let record = SearchRecord::new(app_resource_id(&app.id), result)
                .field(app.name.clone(), FieldWeight::TITLE)
                .field(app.publisher.clone(), FieldWeight::SUBTITLE)
                .field(app.route.label(), FieldWeight::METADATA);
            let _ = self.catalog.upsert_record(record);
        }
        self.ids = ids;
    }

    pub fn provider(&self) -> AppsSearchProvider {
        AppsSearchProvider {
            catalog: self.catalog.clone(),
        }
    }
}

pub struct AppsSearchProvider {
    catalog: SearchCatalog,
}

impl CatalogSearchProvider for AppsSearchProvider {
    fn provider_name(&self) -> &str {
        KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}
