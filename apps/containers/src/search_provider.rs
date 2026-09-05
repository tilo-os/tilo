//! Unified-search projection of the local container engine.
//!
//! Results address the same exported resource operations used by agents and
//! collaboration. The provider snapshots the already-loaded engine and never
//! invokes podman on the palette's latency-sensitive query path.

use ui::icons::Icon;
use ui::qos_actions::ActionAddress;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

use crate::model::Engine;

pub struct ContainerSearchProvider {
    engine: Engine,
}

impl ContainerSearchProvider {
    pub fn new(engine: &Engine) -> Self {
        Self {
            engine: engine.clone(),
        }
    }

    fn collect(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.to_ascii_lowercase();
        let mut hits = Vec::new();

        for container in &self.engine.containers {
            if hits.len() >= limit {
                break;
            }
            if container.matches(&query) {
                hits.push(
                    SearchResult::for_action(
                        "Containers",
                        &container.name,
                        ActionAddress::new(format!("container:{}", container.id), "select"),
                    )
                    .with_subtitle(format!(
                        "{} · {}",
                        container.image,
                        container.status.label()
                    ))
                    .with_icon(Icon::Square),
                );
            } else if container
                .logs
                .iter()
                .any(|line| line.to_ascii_lowercase().contains(&query))
            {
                hits.push(
                    SearchResult::for_action(
                        "Container logs",
                        format!("{} logs", container.name),
                        ActionAddress::new(format!("container:{}:logs", container.id), "open"),
                    )
                    .with_subtitle("Matching recent log output")
                    .with_icon(Icon::FileText),
                );
            }
        }
        for image in &self.engine.images {
            if hits.len() >= limit {
                break;
            }
            if image.matches(&query) {
                let reference = image.reference();
                hits.push(
                    SearchResult::for_action(
                        "Container images",
                        &reference,
                        ActionAddress::new(format!("image:{reference}"), "open"),
                    )
                    .with_subtitle(ui::format::bytes(image.size_bytes))
                    .with_icon(Icon::Layers),
                );
            }
        }
        for volume in &self.engine.volumes {
            if hits.len() >= limit {
                break;
            }
            if volume.matches(&query) {
                hits.push(
                    SearchResult::for_action(
                        "Container volumes",
                        &volume.name,
                        ActionAddress::new(format!("volume:{}", volume.name), "open"),
                    )
                    .with_subtitle(format!("{} · used by {}", volume.driver, volume.in_use_by))
                    .with_icon(Icon::HardDrive),
                );
            }
        }
        for distrobox in &self.engine.distroboxes {
            if hits.len() >= limit {
                break;
            }
            if distrobox.matches(&query) {
                hits.push(
                    SearchResult::for_action(
                        "Distrobox",
                        &distrobox.name,
                        ActionAddress::new(format!("distrobox:{}", distrobox.name), "open"),
                    )
                    .with_subtitle(format!(
                        "{} · {}",
                        distrobox.image,
                        distrobox.status.label()
                    ))
                    .with_icon(Icon::Terminal),
                );
            }
        }
        hits
    }
}

impl SearchProvider for ContainerSearchProvider {
    fn name(&self) -> &str {
        "Containers"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.collect(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.collect(query, limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_objects_and_log_content_with_canonical_addresses() {
        let provider = ContainerSearchProvider::new(&crate::model::sample_engine());
        let container = provider.search("web-frontend").remove(0);
        assert_eq!(container.kind, "Containers");
        assert!(container.action_address().is_some());

        let logs = provider.search("sigkill");
        assert!(logs.iter().any(|hit| hit.kind == "Container logs"));
        assert!(logs.iter().all(|hit| hit.action_address().is_some()));
    }

    #[test]
    fn limited_search_stops_materialising_at_budget() {
        let provider = ContainerSearchProvider::new(&crate::model::sample_engine());
        assert_eq!(provider.search_limited("", 3).len(), 3);
        assert!(provider.search_limited("", 0).is_empty());
    }
}
