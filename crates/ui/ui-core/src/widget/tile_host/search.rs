//! A tile's search: what the app it shows makes findable, in the palette and
//! in the session's persistent index, and activating one of its hits.

use super::{TileContent, TileHost};
use crate::widget::ModelPath;

impl crate::search::capability::SearchSource for TileHost {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        match &self.content {
            TileContent::App(app) => crate::search::capability::search_providers(app.as_ref()),
            _ => Vec::new(),
        }
    }
    fn search_provider_descriptors(
        &self,
    ) -> Vec<crate::search::capability::SearchProviderDescriptor> {
        match &self.content {
            TileContent::App(app) => {
                crate::search::capability::search_provider_descriptors(app.as_ref())
            }
            _ => Vec::new(),
        }
    }
    fn search_index_state(&self) -> Option<crate::search::index_publication::IndexState> {
        match &self.content {
            TileContent::App(app) => crate::search::capability::search_index_state(app.as_ref()),
            _ => None,
        }
    }
    fn search_index_entries(&self) -> Vec<crate::search::index_publication::IndexEntry> {
        match &self.content {
            TileContent::App(app) => crate::search::capability::search_index_entries(app.as_ref()),
            _ => Vec::new(),
        }
    }
    fn activate_search_result(
        &mut self,
        action_id: &str,
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        if let TileContent::App(app) = &mut self.content {
            crate::search::capability::activate_search_result(app.as_mut(), action_id, doc, path);
        }
    }
}
