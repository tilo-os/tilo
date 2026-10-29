//! The portable query model: sources answer a needle with hits, and
//! [`search`] merges them on the shared ranking into kind groups.

use std::collections::HashMap;

use tilo_actions::{ActionAddress, ActionDescriptor, ActionSpec, ResourceId};

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
    /// Run an action an application publishes through the session's action
    /// host, by its address at the provider that publishes it, with the
    /// arguments the row collected.
    Invoke(Box<InvokeTarget>),
    /// A host command, by id.
    Command { id: String },
    /// Put `text` on the clipboard: a worked-out answer.
    Copy { text: String },
    /// Open `uri`, a web address, where the host opens addresses: the
    /// native desktop's browser, a new tab of the browser the web desktop
    /// runs in. Where nothing takes `uri`'s scheme (`web+search:`, an app's
    /// own), `fallback` opens instead, when there is one.
    Open {
        uri: String,
        fallback: Option<String>,
    },
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

/// An action an app publishes through the session's action host, run there
/// by its address: inert data, no closure. The host routes it to the app in
/// `room`, launching the app first when it is closed.
#[derive(Clone, Debug, PartialEq)]
pub struct InvokeTarget {
    /// The app publishing the action.
    pub app: String,
    /// The room (workspace) the app publishes it in.
    pub room: String,
    pub address: ActionAddress,
    /// What the row carries or collected, by parameter name: the anchor an
    /// `open` shows, the one line typed for a prompted action. Empty for
    /// the nullary majority.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The published action, when the row is the action itself rather than
    /// something it opens: what the row shows and asks for.
    pub descriptor: Option<ActionDescriptor>,
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
    /// The row's own picture, when its source gives one (an app's hit, an
    /// action's declared icon); else its kind's.
    pub icon: Option<ui::icons::Icon>,
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
    /// How much plainer than its title's match `hit` answers what was
    /// typed, added to its strength at the merge: a query that names a
    /// service and asks it something leads, a row an app serves better
    /// gives way.
    fn intent(&self, _hit: &Hit) -> f32 {
        0.0
    }
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
                source.intent(&hit),
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
