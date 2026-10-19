//! How a widget contributes to the unified search: providers folded into the
//! Ctrl+K palette and system search, and activating one of its hits.

use crate::widget::capability::{capability, capability_mut};
use crate::widget::{ModelPath, Widget};

/// A widget whose content is searchable.
pub trait SearchSource: Widget {
    /// One [`SearchProvider`](super::SearchProvider) per findable kind.
    fn search_providers(&self) -> Vec<Box<dyn super::SearchProvider>> {
        Vec::new()
    }

    /// Who each of [`search_providers`](Self::search_providers) is, as the
    /// action directory's witness reads them. A source whose providers are
    /// costly to make (one that builds an index to answer from) overrides
    /// this to describe them without making them: the witness is read on
    /// every frame that publishes the action surface, a search only when
    /// someone searches.
    fn search_provider_descriptors(&self) -> Vec<SearchProviderDescriptor> {
        self.search_providers()
            .iter()
            .map(|provider| SearchProviderDescriptor::of(provider.as_ref()))
            .collect()
    }

    /// What this source's entries for the system's persistent search index
    /// are of now; `None` for a source that publishes none. Read every
    /// frame that publishes, so it must not build anything:
    /// [`search_index_entries`](Self::search_index_entries) is asked only
    /// when this changes.
    fn search_index_state(&self) -> Option<super::index_publication::IndexState> {
        None
    }

    /// The entries the system's persistent search index keeps for this
    /// source: short metadata keyed by the stable resource id a hit opens,
    /// found after the app closes too.
    fn search_index_entries(&self) -> Vec<super::index_publication::IndexEntry> {
        Vec::new()
    }

    /// Activate a hit: presentation (navigation), not a document action;
    /// mutate the model only if selecting changes shared state.
    fn activate_search_result(
        &mut self,
        _action_id: &str,
        _doc: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) {
    }
}

/// `widget`'s search providers.
pub fn search_providers(widget: &(impl Widget + ?Sized)) -> Vec<Box<dyn super::SearchProvider>> {
    capability::<dyn SearchSource>(widget).map_or_else(Vec::new, SearchSource::search_providers)
}

/// `widget`'s search providers as the action directory's witness reads them,
/// without querying them.
pub fn search_provider_descriptors(
    widget: &(impl Widget + ?Sized),
) -> Vec<SearchProviderDescriptor> {
    capability::<dyn SearchSource>(widget)
        .map_or_else(Vec::new, SearchSource::search_provider_descriptors)
}

/// What the action directory's witness reads of a search provider: who it
/// is and which build of its content answers for it. Two descriptors differ
/// whenever a directory page from one provider could differ from the other's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchProviderDescriptor {
    pub(crate) name: String,
    /// The component lifetime the provider is a proxy into, if any.
    pub(crate) owner: Option<uuid::Uuid>,
    pub(crate) generation: Option<u64>,
    pub(crate) invocation_domain: crate::widget::ActionDirectoryInvocationDomain,
}

impl SearchProviderDescriptor {
    /// A provider listed as `name` whose content is the build `generation`
    /// names (as [`SearchProvider::action_directory_generation`]), over
    /// `invocation_domain` (as
    /// [`SearchProvider::action_directory_invocation_domain`]).
    ///
    /// [`SearchProvider::action_directory_generation`]: super::SearchProvider::action_directory_generation
    /// [`SearchProvider::action_directory_invocation_domain`]: super::SearchProvider::action_directory_invocation_domain
    pub fn new(
        name: impl Into<String>,
        generation: Option<u64>,
        invocation_domain: crate::widget::ActionDirectoryInvocationDomain,
    ) -> Self {
        Self {
            name: name.into(),
            owner: None,
            generation,
            invocation_domain,
        }
    }

    /// `provider`, as it describes itself.
    pub fn of(provider: &(dyn super::SearchProvider + '_)) -> Self {
        Self {
            name: provider.name().to_owned(),
            owner: provider
                .callback_binding()
                .map(|binding| binding.lifetime_id),
            generation: provider.action_directory_generation(),
            invocation_domain: provider.action_directory_invocation_domain(),
        }
    }

    /// This provider reached through the component lifetime `owner`.
    pub(crate) fn owned_by(self, owner: uuid::Uuid) -> Self {
        Self {
            owner: Some(owner),
            ..self
        }
    }
}

/// What `widget`'s entries for the system's persistent search index are of
/// now, if it publishes any.
pub fn search_index_state(
    widget: &(impl Widget + ?Sized),
) -> Option<super::index_publication::IndexState> {
    capability::<dyn SearchSource>(widget).and_then(SearchSource::search_index_state)
}

/// `widget`'s entries for the system's persistent search index.
pub fn search_index_entries(
    widget: &(impl Widget + ?Sized),
) -> Vec<super::index_publication::IndexEntry> {
    capability::<dyn SearchSource>(widget).map_or_else(Vec::new, SearchSource::search_index_entries)
}

/// Activate hit `action_id` in `widget`.
pub fn activate_search_result(
    widget: &mut (impl Widget + ?Sized),
    action_id: &str,
    doc: &mut crate::widget::Edits<'_>,
    path: &ModelPath,
) {
    if let Some(source) = capability_mut::<dyn SearchSource>(widget) {
        source.activate_search_result(action_id, doc, path);
    }
}
