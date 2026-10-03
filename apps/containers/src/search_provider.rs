//! Unified-search projection of the local container engine.
//!
//! Results address the same exported resource operations used by agents and
//! collaboration. The provider snapshots the already-loaded engine and never
//! invokes podman on the palette's latency-sensitive query path.

use std::sync::{Arc, RwLock};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use ui::icons::Icon;
use ui::tilo_actions::ActionAddress;

use crate::model::Engine;

fn index_containers(catalog: &SearchCatalog, engine: &Engine) {
    for container in &engine.containers {
        let resource_id = format!("container:{}", container.id);
        let result = SearchResult::for_action(
            "Containers",
            &container.name,
            ActionAddress::new(&resource_id, "select"),
        )
        .with_subtitle(format!(
            "{} · {}",
            container.image,
            container.status.label()
        ))
        .with_icon(Icon::Square);
        let searchable = format!(
            "{} {} {} {}",
            container.name,
            container.id,
            container.image,
            container.status.label()
        );
        let _ = catalog.upsert(&resource_id, searchable, result);

        let log_resource_id = format!("container:{}:logs", container.id);
        let log_result = SearchResult::for_action(
            "Container logs",
            format!("{} logs", container.name),
            ActionAddress::new(&log_resource_id, "open"),
        )
        .with_subtitle("Matching recent log output")
        .with_icon(Icon::FileText);
        let log_searchable = format!("{} {}", container.name, container.logs.join("\n"));
        let _ = catalog.upsert(&log_resource_id, log_searchable, log_result);
    }
}

fn index_images(catalog: &SearchCatalog, engine: &Engine) {
    for image in &engine.images {
        let reference = image.reference();
        let resource_id = format!("image:{reference}");
        let result = SearchResult::for_action(
            "Container images",
            &reference,
            ActionAddress::new(&resource_id, "open"),
        )
        .with_subtitle(ui::format::bytes(image.size_bytes))
        .with_icon(Icon::Layers);
        let _ = catalog.upsert(
            &resource_id,
            format!("{} {} {}", reference, image.repository, image.tag),
            result,
        );
    }
}

fn index_volumes(catalog: &SearchCatalog, engine: &Engine) {
    for volume in &engine.volumes {
        let resource_id = format!("volume:{}", volume.name);
        let result = SearchResult::for_action(
            "Container volumes",
            &volume.name,
            ActionAddress::new(&resource_id, "open"),
        )
        .with_subtitle(format!("{} · used by {}", volume.driver, volume.in_use_by))
        .with_icon(Icon::HardDrive);
        let _ = catalog.upsert(
            &resource_id,
            format!("{} {}", volume.name, volume.driver),
            result,
        );
    }
}

fn index_distroboxes(catalog: &SearchCatalog, engine: &Engine) {
    for distrobox in &engine.distroboxes {
        let resource_id = format!("distrobox:{}", distrobox.name);
        let result = SearchResult::for_action(
            "Distrobox",
            &distrobox.name,
            ActionAddress::new(&resource_id, "open"),
        )
        .with_subtitle(format!(
            "{} · {}",
            distrobox.image,
            distrobox.status.label()
        ))
        .with_icon(Icon::Terminal);
        let _ = catalog.upsert(
            &resource_id,
            format!(
                "{} {} {}",
                distrobox.name,
                distrobox.image,
                distrobox.status.label()
            ),
            result,
        );
    }
}

#[derive(Default)]
struct IndexState {
    revision: Option<[u8; 32]>,
    catalog: Option<SearchCatalog>,
}

/// Retained search projection updated when the background engine snapshot
/// changes. Palette keystrokes only advance bounded index pages; they never
/// rescan containers, images, volumes, distroboxes, or log lines.
#[derive(Clone, Default)]
pub struct ContainerSearchIndex {
    state: Arc<RwLock<IndexState>>,
}

impl ContainerSearchIndex {
    pub fn new(engine: &Engine) -> Self {
        let index = Self::default();
        index.sync(engine);
        index
    }

    pub fn sync(&self, engine: &Engine) {
        let revision = engine_revision(engine);
        {
            let state = self
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.revision == Some(revision) && state.catalog.is_some() {
                return;
            }
        }

        let catalog = SearchCatalog::new("Containers");
        index_containers(&catalog, engine);
        index_images(&catalog, engine);
        index_volumes(&catalog, engine);
        index_distroboxes(&catalog, engine);

        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = Some(revision);
        state.catalog = Some(catalog);
    }

    pub fn provider(&self) -> ContainerSearchProvider {
        ContainerSearchProvider {
            index: self.clone(),
        }
    }
}

pub struct ContainerSearchProvider {
    index: ContainerSearchIndex,
}

impl CatalogSearchProvider for ContainerSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Containers"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .clone()
    }

    fn limited_work_budget(&self, limit: usize) -> usize {
        // A one-shot query inspects no more candidates than results it asked for.
        limit
    }
}

fn engine_revision(engine: &Engine) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    let mut field = |value: &[u8]| {
        let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
        hasher.update(&length.to_le_bytes());
        hasher.update(value);
    };
    for container in &engine.containers {
        for value in [
            container.id.as_bytes(),
            container.name.as_bytes(),
            container.image.as_bytes(),
            container.status.label().as_bytes(),
        ] {
            field(value);
        }
        for line in &container.logs {
            field(line.as_bytes());
        }
    }
    for image in &engine.images {
        for value in [
            image.id.as_bytes(),
            image.repository.as_bytes(),
            image.tag.as_bytes(),
            image.size_bytes.to_le_bytes().as_slice(),
        ] {
            field(value);
        }
    }
    for volume in &engine.volumes {
        for value in [
            volume.name.as_bytes(),
            volume.driver.as_bytes(),
            volume.in_use_by.to_le_bytes().as_slice(),
        ] {
            field(value);
        }
    }
    for distrobox in &engine.distroboxes {
        for value in [
            distrobox.name.as_bytes(),
            distrobox.image.as_bytes(),
            distrobox.status.label().as_bytes(),
        ] {
            field(value);
        }
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    /// Search finds objects and log content with canonical addresses, stops at its budget and reaches late matches within the page budget.
    #[test]
    fn search_finds_objects_and_logs_within_page_budgets() {
        // --- phase: searches_objects_and_log_content_with_canonical_addresses ---
        {
            let provider = ContainerSearchIndex::new(&crate::model::sample_engine()).provider();
            let container = provider.search("web-frontend").remove(0);
            assert_eq!(container.kind, "Containers");
            assert!(container.action_address().is_some());

            let logs = provider.search("sigkill");
            assert!(logs.iter().any(|hit| hit.kind == "Container logs"));
            assert!(logs.iter().all(|hit| hit.action_address().is_some()));
        }

        // --- phase: limited_search_stops_materialising_at_budget ---
        {
            let provider = ContainerSearchIndex::new(&crate::model::sample_engine()).provider();
            assert_eq!(provider.search_limited("", 3).len(), 3);
            assert!(provider.search_limited("", 0).is_empty());
        }

        // --- phase: late_matches_are_reachable_without_exceeding_the_page_budget ---
        {
            let mut engine = Engine::default();
            for index in 0..300 {
                let mut container = crate::model::sample_engine().containers[0].clone();
                container.id = format!("container-{index:03}");
                container.name = if index == 299 {
                    "needle-at-the-end".into()
                } else {
                    format!("container-{index:03}")
                };
                engine.containers.push(container);
            }
            let provider = ContainerSearchIndex::new(&engine).provider();
            let mut cursor = None;
            let mut found = false;
            loop {
                let page = provider.search_page("needle", cursor.as_ref(), 7, 3);
                assert!(page.inspected <= 7);
                found |= page
                    .results
                    .iter()
                    .any(|hit| hit.title == "needle-at-the-end");
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert!(found);
        }
    }
}
