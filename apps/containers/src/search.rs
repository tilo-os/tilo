//! The local container engine as search entries.
//!
//! Hits are anchored at the same live rows agents and collaborators use;
//! opening one shows its row. The entries are read from the already-loaded engine, so a search
//! never invokes podman on the palette's latency-sensitive query path.

use ui::tilo_resource::SearchEntry;

use crate::model::Engine;

/// Everything the engine runs and keeps, as search hits: containers and
/// their recent log output, images, volumes and distroboxes, each opening
/// its live row.
pub fn engine_entries(engine: &Engine) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for container in &engine.containers {
        let resource_id = format!("container:{}", container.id);
        entries.push(
            SearchEntry::new(
                container.name.clone(),
                format!("{} {}", container.id, container.status.label()),
            )
            .anchored(resource_id.clone())
            .grouped("Containers")
            .subtitled(format!(
                "Image {0}, currently {1}.",
                container.image,
                container.status.label()
            ))
            .with_icon("square"),
        );
        let log_resource_id = format!("container:{}:logs", container.id);
        entries.push(
            SearchEntry::new(
                format!("{} logs", container.name),
                container.logs.join("\n"),
            )
            .anchored(log_resource_id.clone())
            .grouped("Container logs")
            .subtitled("Matching recent log output")
            .with_icon("file-text"),
        );
    }
    for image in &engine.images {
        let reference = image.reference();
        let resource_id = format!("image:{reference}");
        entries.push(
            SearchEntry::new(
                reference.clone(),
                format!("{} {}", image.repository, image.tag),
            )
            .anchored(resource_id.clone())
            .grouped("Container images")
            .subtitled(ui::format::memory(image.size_bytes))
            .with_icon("layers"),
        );
    }
    for volume in &engine.volumes {
        let resource_id = format!("volume:{}", volume.name);
        entries.push(
            SearchEntry::new(volume.name.clone(), volume.driver.clone())
                .anchored(resource_id.clone())
                .grouped("Container volumes")
                .subtitled(format!(
                    "Driver {0}, used by {1}.",
                    volume.driver, volume.in_use_by
                ))
                .with_icon("hard-drive"),
        );
    }
    for distrobox in &engine.distroboxes {
        let resource_id = format!("distrobox:{}", distrobox.name);
        entries.push(
            SearchEntry::new(distrobox.name.clone(), distrobox.status.label())
                .anchored(resource_id.clone())
                .grouped("Distrobox")
                .subtitled(format!(
                    "Image {0}, currently {1}.",
                    distrobox.image,
                    distrobox.status.label()
                ))
                .with_icon("terminal"),
        );
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    /// The app's search source over `engine`, as the framework builds it.
    fn source(engine: &Engine) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::resource::Containers>(engine_entries(engine))
    }

    /// Search finds objects and log content with canonical addresses, stops at its budget and reaches late matches within the page budget.
    #[test]
    fn search_finds_objects_and_logs_within_page_budgets() {
        // --- phase: searches_objects_and_log_content_with_canonical_addresses ---
        {
            let provider = source(&crate::model::sample_engine());
            let container = provider.search("web-frontend").remove(0);
            assert_eq!(container.kind, "Containers");
            assert!(container.action_id.starts_with("container:"));

            let logs = provider.search("sigkill");
            assert!(logs.iter().any(|hit| hit.kind == "Container logs"));
            assert!(
                logs.iter()
                    .filter(|hit| hit.kind == "Container logs")
                    .all(|hit| hit.action_id.ends_with(":logs"))
            );
        }

        // --- phase: limited_search_stops_materialising_at_budget ---
        {
            let provider = source(&crate::model::sample_engine());
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
            let provider = source(&engine);
            let mut cursor = None;
            let mut found = false;
            loop {
                let page = provider.search_page(
                    &ui::search::SearchQuery::new("needle"),
                    cursor.as_ref(),
                    7,
                    3,
                );
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
