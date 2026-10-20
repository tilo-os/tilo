//! Search over the rooms' tilolang resources: the notes, the messages, the todos.
//!
//! Search is a system service: the window registers one provider over a catalogue it keeps as the
//! record moves, and says what each hit is. A hit's action is the `open` view action of the
//! resource it names, so a person's click and an agent's call land in the same place.

use tilolang_tilo::OpenEffect;
use tilolang_tilo::host::Host;
use tilolang_tilo::search::hits;
use ui::search::index_publication::{IndexAction, IndexEntry, IndexWeight};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};
use ui::tilo_resource::Effect as _;

/// The provider the window registers.
pub struct Provider {
    catalog: SearchCatalog,
}

impl Provider {
    #[must_use]
    pub const fn over(catalog: SearchCatalog) -> Self {
        Self { catalog }
    }
}

impl CatalogSearchProvider for Provider {
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "the trait names the provider by a borrowed str"
    )]
    fn provider_name(&self) -> &str {
        "tilolang"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

/// Index what the room holds now: every resource with text, found by that text. Returns what the
/// system's persistent index keeps of it: short metadata only, never a prose body.
pub fn refresh(catalog: &SearchCatalog, host: &Host) -> Vec<IndexEntry> {
    let mut rows = Vec::new();
    let mut entries = Vec::new();
    for (package, _, doc) in host.records() {
        let Ok(record) = host.store().record(&doc) else {
            continue;
        };
        for hit in hits(&package.meta, &record, &doc) {
            let address = OpenEffect::Open {
                anchor: hit.resource.clone(),
            }
            .address_on(&hit.resource);
            let mut result = SearchResult::for_action(&hit.app_title, &hit.title, &address)
                .with_subtitle(&hit.type_name);
            if let Some(preview) = hit.texts.get(1..).filter(|rest| !rest.is_empty()) {
                result = result.with_preview(preview.join(" "));
            }
            entries.push(IndexEntry {
                key: hit.resource.clone(),
                resource: hit.resource.clone(),
                action: Some(IndexAction {
                    resource: hit.resource.clone(),
                    action: "open".to_owned(),
                }),
                title: hit.title.clone(),
                subtitle: Some(hit.type_name.clone()),
                group: Some(hit.app_title.clone()),
                icon: None,
                boost: 0,
                fields: hit
                    .texts
                    .iter()
                    .skip(1)
                    .filter(|text| text.chars().count() <= SHORT)
                    .map(|text| (text.clone(), IndexWeight::Metadata))
                    .collect(),
                secret: false,
            });
            rows.push((hit.resource.clone(), hit.texts.join("\n"), result));
        }
    }
    if let Err(error) = catalog.replace_all(rows) {
        tracing::warn!(?error, "the room could not be indexed for search");
    }
    entries
}

/// The longest text the persistent index keeps of a resource: a name, a subject, an address.
const SHORT: usize = 80;
