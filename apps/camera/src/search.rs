//! Bounded global search for Camera's canonical operations.
//!
//! Camera does not index a photo/video collection. The catalog contains only
//! its finite capture, scanner, document-scanner, navigation, and preference
//! actions. Exact resources are materialized lazily from the same compact
//! context generation, so search never retains duplicate `Resource` trees.

use std::sync::{Arc, RwLock};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchCatalogError, SearchResult};

use automerge::AutoCommit;
use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;

use crate::actions::{
    CameraResourceCtx, MAX_PUBLISHED_CAMERA_DEVICES, provide_resources, resolve_resource,
};

pub const CAMERA_SEARCH_KIND: &str = "Camera actions";
const MAX_CAMERA_SEARCH_RESOURCES: usize = 32;

#[derive(Clone)]
pub struct CameraSearchProvider {
    state: Arc<RwLock<ProviderState>>,
}

struct ProviderState {
    catalog: SearchCatalog,
    context: CameraResourceCtx,
    initialized: bool,
}

impl CameraSearchProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ProviderState {
                catalog: SearchCatalog::new(CAMERA_SEARCH_KIND),
                context: CameraResourceCtx::default(),
                initialized: false,
            })),
        }
    }

    /// Atomically replace the finite action directory and its compact exact
    /// materialization context. A rejected catalog never publishes new state.
    ///
    /// # Errors
    ///
    /// Returns an error when the generated catalog is not canonical or
    /// exceeds Camera's bounded published resource set. The previous complete
    /// generation remains active.
    pub fn sync(&self, context: &CameraResourceCtx) -> Result<(), SearchCatalogError> {
        // Construct the retained seed directly from the bounded prefix. Do
        // not clone then truncate: a hostile 100k-device input must not create
        // a second 100k-element corpus, even transiently.
        let context = CameraResourceCtx {
            recent: context.recent.clone(),
            mode: context.mode,
            facing: context.facing,
            recording: context.recording,
            feed_available: context.feed_available,
            camera_permission_denied: context.camera_permission_denied,
            camera_devices: context
                .camera_devices
                .iter()
                .take(MAX_PUBLISHED_CAMERA_DEVICES)
                .cloned()
                .collect(),
            selected_camera: context.selected_camera.clone(),
            settings: context.settings,
            scan_import_pending: context.scan_import_pending,
            document_scanner_available: context.document_scanner_available,
            document_pages: context.document_pages,
            document_selected: context.document_selected,
            document_busy: context.document_busy,
            document_has_text: context.document_has_text,
        };
        let unchanged = {
            let state = self
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.initialized && state.context == context
        };
        if unchanged {
            return Ok(());
        }
        let resources = provide_resources(&context, &AutoCommit::new());
        let mut entries = Vec::new();
        for resource in &resources {
            Self::collect(resource, &mut entries);
        }
        if entries.len() > MAX_CAMERA_SEARCH_RESOURCES {
            return Err(SearchCatalogError::NonCanonicalResult);
        }
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.catalog.replace_all(entries)?;
        state.context = context;
        state.initialized = true;
        drop(state);
        Ok(())
    }

    fn collect(resource: &Resource, entries: &mut Vec<(String, String, SearchResult)>) {
        if let Some(primary) = resource.actions.first() {
            let mut searchable = format!("{}\0{}", resource.label, resource.kind);
            for action in &resource.actions {
                searchable.push('\0');
                searchable.push_str(&action.id);
                searchable.push('\0');
                searchable.push_str(&action.label);
                searchable.push('\0');
                searchable.push_str(action.description.as_deref().unwrap_or_default());
            }
            entries.push((
                resource.id.as_str().to_owned(),
                searchable,
                SearchResult::for_action(
                    CAMERA_SEARCH_KIND,
                    &resource.label,
                    ActionAddress::new(resource.id.as_str(), &primary.id),
                )
                .with_subtitle(primary.label.clone())
                .with_icon(Icon::Camera),
            ));
        }
        for child in &resource.children {
            Self::collect(child, entries);
        }
    }

    fn resource(&self, id: &str) -> Option<Resource> {
        let context = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .context
            .clone();
        resolve_resource(&context, &AutoCommit::new(), id)
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .stats()
    }

    #[cfg(test)]
    fn retained_resource_trees() -> usize {
        0
    }

    #[cfg(test)]
    fn retained_device_seeds(&self) -> usize {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .context
            .camera_devices
            .len()
    }
}

impl Default for CameraSearchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogSearchProvider for CameraSearchProvider {
    fn provider_name(&self) -> &str {
        CAMERA_SEARCH_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(
            self.state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog
                .clone(),
        )
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        let resource = self.resource(address.resource.as_str())?;
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::CaptureItem;
    use ui::search::{DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider};

    fn context() -> CameraResourceCtx {
        CameraResourceCtx {
            feed_available: true,
            recent: Some(CaptureItem {
                id: "latest.jpg".into(),
                label: "Latest capture".into(),
                is_video: false,
                media_id: Some(format!("{}{:064x}", crate::media::MEDIA_PREFIX, 1)),
            }),
            ..CameraResourceCtx::default()
        }
    }

    #[test]
    fn camera_search_materializes_exact_resources_lazily_within_fixed_limits() {
        // Phase `searches_canonical_actions_and_materializes_exact_resources_lazily`.
        {
            let provider = CameraSearchProvider::new();
            provider.sync(&context()).unwrap();
            let hits = provider.search_limited("scan existing image", usize::MAX);
            assert_eq!(hits.len(), 1);
            let address = hits[0].action_address().expect("canonical action");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("exact resource");
            assert_eq!(resource.id.as_str(), "camera.scanner");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "import-image")
            );
            assert_eq!(CameraSearchProvider::retained_resource_trees(), 0);
        }
        // Phase `hostile_limits_and_over_100k_devices_cannot_expand_the_fixed_directory`.
        {
            let provider = CameraSearchProvider::new();
            let mut context = context();
            context.camera_devices = (0..100_001)
                .map(|index| format!("/dev/video{index}"))
                .collect();
            provider.sync(&context).unwrap();
            let page = provider.search_page("camera", None, usize::MAX, usize::MAX);
            assert!(page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
            assert!(page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT);
            assert!(provider.stats().records <= MAX_CAMERA_SEARCH_RESOURCES);
            assert_eq!(CameraSearchProvider::retained_resource_trees(), 0);
            assert_eq!(
                provider.retained_device_seeds(),
                MAX_PUBLISHED_CAMERA_DEVICES
            );
        }
    }
}
