//! Tilo packages: a registry's `app list --json` row, joined with the fleet's
//! manifest metadata.

use registry_types::RegistryImageKind;
use registry_types::catalogue::CatalogueApp;

use crate::entry::{CatalogueAction, CatalogueEntry, ResourceType, Source};
use crate::fleet;

/// The catalogue entry for the registry row `app`.
///
/// What the registry signed (name, tag line, description, artwork,
/// category) comes first; the fleet's manifest metadata adds the other
/// names, keywords, documents and offered actions, and stands in for a tag
/// line the registry left empty.
#[must_use]
pub fn entry(app: &CatalogueApp) -> CatalogueEntry {
    let manifest = fleet::metadata(&app.package_id);
    let tagline = if app.tagline.trim().is_empty() {
        manifest.map_or("", |meta| meta.tagline).to_owned()
    } else {
        app.tagline.trim().to_owned()
    };
    CatalogueEntry {
        id: app.package_id.clone(),
        source: Source::Tilo {
            registry: app.registry.clone(),
        },
        name: app.display_name.clone(),
        other_names: manifest
            .map(|meta| meta.names.iter().map(|name| (*name).to_owned()).collect())
            .unwrap_or_default(),
        tagline,
        description: app.description.clone(),
        keywords: manifest
            .map(|meta| {
                meta.keywords
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        category: app.category.clone(),
        icon_file: None,
        icon_url: app
            .images
            .iter()
            .find(|image| image.kind == RegistryImageKind::Icon)
            .map(|image| image.url.clone()),
        resource_types: manifest
            .map(|meta| {
                meta.resource_types
                    .iter()
                    .map(|(kind, label)| ResourceType {
                        kind: (*kind).to_owned(),
                        label: (*label).to_owned(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        actions: manifest
            .map(|meta| {
                meta.actions
                    .iter()
                    .map(|action| CatalogueAction {
                        resource: action.resource.to_owned(),
                        action: action.action.to_owned(),
                        label: action.label.to_owned(),
                        description: (!action.description.is_empty())
                            .then(|| action.description.to_owned()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_row_carries_the_fleets_documents_and_actions() {
        let row = CatalogueApp {
            package_id: "so.tilo.calendar".into(),
            display_name: "Calendar".into(),
            version: "1.0.0".into(),
            registry: "Tilo".into(),
            trust: "publisher-signed".into(),
            ..CatalogueApp::default()
        };
        let entry = entry(&row);
        assert_eq!(
            entry.source,
            Source::Tilo {
                registry: "Tilo".into()
            }
        );
        assert!(!entry.source.needs_install(), "a Tilo app just opens");
        assert!(!entry.tagline.is_empty(), "the fleet's tag line stands in");
        assert!(
            entry
                .actions
                .iter()
                .any(|action| action.label == "Show tasks")
        );
    }
}
