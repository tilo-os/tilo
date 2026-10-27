//! The portable query model: sources answer a needle with hits, and
//! [`search`] merges them on the shared ranking into kind groups.

use std::collections::HashMap;

use tilo_actions::{ActionAddress, ActionSpec, ResourceId};

use ui::search::system::{self, KindGroup, ResourceKind};

/// What choosing a hit does. The host performs it exactly as its other
/// controls do: a launch opens a window, a window hit focuses it, an action
/// runs through the host's action door.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Open the application in a new window beside the focused one.
    Launch { app: String },
    /// Focus the window showing this tab, of the app `app` when it is one.
    Window { tab: u64, app: Option<String> },
    /// Run an action an application publishes, resolved against the live
    /// resource tree when it runs.
    Action(Box<ActionTarget>),
    /// A host command, by id.
    Command { id: String },
}

/// An application action as the palette offers it.
#[derive(Clone)]
pub struct ActionTarget {
    pub resource: ResourceId,
    pub resource_kind: String,
    pub resource_label: String,
    pub spec: ActionSpec,
    /// The argument the row already carries, as `(parameter, value)`: an
    /// app's `open` invoked for one search hit carries the hit's anchor, so
    /// choosing the row runs it without asking.
    pub preset: Option<(String, String)>,
}

impl ActionTarget {
    pub fn address(&self) -> ActionAddress {
        ActionAddress::new(self.resource.as_str(), &self.spec.id)
    }
}

impl PartialEq for ActionTarget {
    fn eq(&self, other: &Self) -> bool {
        self.address() == other.address() && self.preset == other.preset
    }
}

impl std::fmt::Debug for ActionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionTarget")
            .field("address", &self.address())
            .field("preset", &self.preset)
            .field("resource_label", &self.resource_label)
            .field("label", &self.spec.label)
            .finish_non_exhaustive()
    }
}

/// One row a source offers for a needle. `score` is on the source's own
/// scale, topped by [`Source::max_score`].
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub kind: ResourceKind,
    pub provider: String,
    /// Stable within the provider; `(provider, id)` de-duplicates.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    /// The second line as separate facts (where a window is, who holds
    /// it), laid out apart rather than joined; empty when `subtitle` says it.
    pub facts: Vec<String>,
    pub score: i32,
    pub target: Target,
}

/// A source of hits for one domain (applications, windows, actions, …).
pub trait Source {
    fn id(&self) -> &str;
    /// Top of this source's score scale; see [`system::merged_strength`].
    fn max_score(&self) -> i32 {
        system::MAX_PROVIDER_SCORE
    }
    /// Hits for `needle`. An empty needle asks for the source's browse set.
    fn query(&self, needle: &str) -> Vec<Hit>;
}

/// Fan `needle` out to `sources`, merge their hits on the shared ranking
/// and group them by kind under a shared `row_budget` (zero keeps all).
pub fn search(needle: &str, sources: &[&dyn Source], row_budget: usize) -> Vec<KindGroup<Hit>> {
    if !system::query_input_is_bounded(needle) {
        return Vec::new();
    }
    let mut merged: Vec<(Hit, f32)> = Vec::new();
    let mut rows: HashMap<(String, String), usize> = HashMap::new();
    for source in sources {
        let top = source.max_score().max(1);
        for hit in source.query(needle) {
            let strength = system::merged_strength(
                needle,
                &hit.kind,
                &hit.title,
                hit.subtitle.as_deref(),
                hit.score.min(top),
                top,
                0.0,
            );
            let key = (hit.provider.clone(), hit.id.clone());
            if let Some(index) = rows.get(&key).copied() {
                if let Some(slot) = merged.get_mut(index)
                    && strength > slot.1
                {
                    *slot = (hit, strength);
                }
            } else {
                rows.insert(key, merged.len());
                merged.push((hit, strength));
            }
        }
    }
    system::order_by_kind(&mut merged, |hit| &hit.kind);
    system::group_by_kind(
        merged.into_iter().map(|(hit, _)| hit).collect(),
        |hit| &hit.kind,
        row_budget,
    )
}
