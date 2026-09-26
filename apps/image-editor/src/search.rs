//! Retained, canonical global-search projection of the image graph.
//!
//! Search never gets an opaque widget id. Each row names an exported
//! resource/action, so the palette, agents and history use the same door as
//! the visible adjustment stack. `SearchCatalog` bounds materialisation and
//! provides generation-safe continuations for remote discovery.

use crate::{
    actions::{
        EXPORT_RESOURCE_ID, PROJECT_RESOURCE_ID, SOURCE_RESOURCE_ID, adjustment_resource_id,
    },
    model::{Operation, Project},
};
use tilo_actions::ActionAddress;
use ui::{
    icons::Icon,
    search::{CatalogSearchProvider, SearchCatalog, SearchResult},
};

pub const SEARCH_KIND: &str = "Image Editor";

#[derive(Clone)]
pub struct ProjectSearch {
    catalog: SearchCatalog,
}

impl ProjectSearch {
    #[must_use]
    pub fn new(project: &Project) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(SEARCH_KIND),
        };
        provider.refresh(project);
        provider
    }

    /// Replace one immutable collaborative-document snapshot, rather than
    /// rebuilding an index during an individual query or frame.
    pub fn refresh(&self, project: &Project) {
        let mut rows = vec![
            (
                PROJECT_RESOURCE_ID.to_owned(),
                format!(
                    "{}\0image editor\0open image\0adjustments\0export",
                    project.name
                ),
                SearchResult::for_action(
                    SEARCH_KIND,
                    &project.name,
                    ActionAddress::new(PROJECT_RESOURCE_ID, "open"),
                )
                .with_subtitle("Image project · open or add an adjustment")
                .with_icon(Icon::Image),
            ),
            (
                SOURCE_RESOURCE_ID.to_owned(),
                format!("{}\0source\0original image", source_label(project)),
                SearchResult::for_action(
                    SEARCH_KIND,
                    source_label(project),
                    ActionAddress::new(SOURCE_RESOURCE_ID, "select"),
                )
                .with_subtitle("Original source")
                .with_icon(Icon::FolderOpen),
            ),
            (
                EXPORT_RESOURCE_ID.to_owned(),
                "export png\0final result\0image".into(),
                SearchResult::for_action(
                    SEARCH_KIND,
                    "Export PNG",
                    ActionAddress::new(EXPORT_RESOURCE_ID, "reveal"),
                )
                .with_subtitle("Final image stage")
                .with_icon(Icon::Download),
            ),
        ];
        rows.extend(
            project
                .graph
                .nodes
                .iter()
                .filter(|(id, _)| *id != "source" && *id != "output")
                .map(|(id, node)| {
                    let resource_id = adjustment_resource_id(id);
                    let operation = operation_name(project.operations.get(id));
                    (
                        resource_id.clone(),
                        format!(
                            "{}\0{operation}\0adjustment\0image step\0editable",
                            node.title
                        ),
                        SearchResult::for_action(
                            SEARCH_KIND,
                            &node.title,
                            ActionAddress::new(resource_id, "select"),
                        )
                        .with_subtitle(format!("{operation} · editable adjustment"))
                        .with_icon(Icon::Settings2),
                    )
                }),
        );
        // Rows are generated from local stable ids, and every address names
        // the resource exported by `actions::resource` in this same snapshot.
        let _ = self.catalog.replace_all(rows);
    }
}

impl CatalogSearchProvider for ProjectSearch {
    fn provider_name(&self) -> &str {
        SEARCH_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

fn source_label(project: &Project) -> &str {
    match project.operations.get("source") {
        Some(Operation::Source { path }) if !path.is_empty() => std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Source image"),
        _ => "No source image",
    }
}

fn operation_name(operation: Option<&Operation>) -> &'static str {
    match operation {
        Some(Operation::Exposure { .. }) => "Exposure",
        Some(Operation::Contrast { .. }) => "Contrast",
        Some(Operation::Saturation { .. }) => "Colour",
        Some(Operation::Grayscale) => "Black and white",
        Some(Operation::Rotate { .. }) => "Rotate",
        Some(Operation::Crop { .. }) => "Crop",
        Some(Operation::Source { .. }) => "Source",
        Some(Operation::Output) | None => "Image step",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchProvider;

    #[test]
    fn search_rows_address_canonical_resources_and_page_generation_safely() {
        search_rows_use_the_same_canonical_resources_as_the_graph();
        search_is_cursor_paged_and_generation_safe();
    }

    fn search_rows_use_the_same_canonical_resources_as_the_graph() {
        let mut project = Project::blank();
        assert!(project.insert_before_output(
            "exposure-1",
            "Exposure",
            Operation::Exposure { stops: 0.5 },
        ));
        let provider = ProjectSearch::new(&project);
        let hit = provider.search_limited("exposure", 4).pop().unwrap();
        assert_eq!(
            hit.action_address(),
            Some(ActionAddress::new(
                adjustment_resource_id("exposure-1"),
                "select"
            ))
        );
    }

    fn search_is_cursor_paged_and_generation_safe() {
        let mut project = Project::blank();
        for index in 0..4 {
            assert!(project.insert_before_output(
                &format!("contrast-{index}"),
                "Contrast",
                Operation::Contrast { amount: 0.1 },
            ));
        }
        let provider = ProjectSearch::new(&project);
        let first = provider.search_page("contrast", None, 1, 1);
        assert_eq!(first.results.len(), 1);
        assert!(first.next.is_some());
        assert!(provider.action_directory_cursor_is_current(first.next.as_ref().unwrap()));
        assert!(project.insert_before_output(
            "contrast-final",
            "Contrast",
            Operation::Contrast { amount: 0.2 },
        ));
        provider.refresh(&project);
        assert!(!provider.action_directory_cursor_is_current(first.next.as_ref().unwrap()));
    }
}
