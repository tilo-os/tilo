//! How a widget contributes to the unified search: providers folded into the
//! Ctrl+K palette and system search, and activating one of its hits.

use crate::widget::capability::{capability, capability_mut};
use crate::widget::{AppModel, ModelPath, Widget};

/// A widget whose content is searchable.
pub trait SearchSource: Widget {
    /// One [`SearchProvider`](super::SearchProvider) per findable kind.
    fn search_providers(&self) -> Vec<Box<dyn super::SearchProvider>> {
        Vec::new()
    }

    /// Activate a hit: presentation (navigation), not a document action;
    /// mutate the model only if selecting changes shared state.
    fn activate_search_result(&mut self, _action_id: &str, _doc: &mut AppModel, _path: &ModelPath) {
    }
}

/// `widget`'s search providers.
pub fn search_providers(widget: &(impl Widget + ?Sized)) -> Vec<Box<dyn super::SearchProvider>> {
    capability::<dyn SearchSource>(widget).map_or_else(Vec::new, SearchSource::search_providers)
}

/// Activate hit `action_id` in `widget`.
pub fn activate_search_result(
    widget: &mut (impl Widget + ?Sized),
    action_id: &str,
    doc: &mut AppModel,
    path: &ModelPath,
) {
    if let Some(source) = capability_mut::<dyn SearchSource>(widget) {
        source.activate_search_result(action_id, doc, path);
    }
}
