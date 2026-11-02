//! The portable query model: sources answer a needle with hits, and
//! [`search_stream`] merges them on the shared ranking into kind groups,
//! a frame at a time.

use std::collections::HashMap;

use tilo_actions::{ActionAddress, ActionDescriptor, ActionSpec, ResourceId};

use ui::search::system::{self, KindGroup, ResourceKind};
use ui::time::{Duration, Instant};

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
    /// Ask a running invocation of `app` in `workspace` to stop: a job's own
    /// cancel row ([`crate::providers::jobs`]).
    Cancel {
        app: String,
        workspace: String,
        invocation: String,
    },
    /// Search `source` from now on, or stop: the person's own choice, kept
    /// with their settings by the host ([`crate::preferences`]).
    Prefer { source: String, enabled: bool },
}

/// The provider name of what the desktop itself publishes (its chrome's and
/// its desk's actions): a row the desktop runs on itself, by address, as
/// every other caller of the action protocol does.
pub const DESKTOP_APP: &str = "tilo.desktop";

/// The resource of the desktop's chrome above the windows: what its own
/// commands are published on.
pub const CHROME_RESOURCE: &str = "desktop:chrome";

/// The chrome's action that opens or closes who is here (the people
/// panel).
pub const SHOW_COLLABORATORS: &str = "toggle-collaborators";

/// The desktop command that goes to workspace `id` (`desktop.workspace:id`).
pub const WORKSPACE_COMMAND_PREFIX: &str = "desktop.workspace:";

/// The desktop command that lists every application, each by its name: the
/// catalogue behind "All apps".
pub const ALL_APPS_COMMAND: &str = "desktop.all-apps";

/// The action every framework app publishes to show the item at an anchor
/// (`tilo_app::framework`'s `open`), and its parameter.
pub const OPEN_ACTION: &str = "open";
pub const OPEN_ANCHOR: &str = "anchor";

/// Where a source's rows stand in the empty palette when it does not say:
/// after everything that answers where it belongs there
/// ([`Source::browse_rank`]).
pub const BROWSE_RANK_LATER: u8 = 16;

/// The rows about what is happening now: who is where, and the calls to
/// join. The empty palette opens with them.
pub const BROWSE_RANK_NOW: u8 = 0;
/// The people doing something in the workspace.
pub const BROWSE_RANK_PEOPLE: u8 = 1;
/// What the person reached for last.
pub const BROWSE_RANK_RECENT: u8 = 2;
/// The ways in: the apps they have, then the windows open.
pub const BROWSE_RANK_APPS: u8 = 3;
pub const BROWSE_RANK_WINDOWS: u8 = 4;

/// An application action as the palette offers it.
#[derive(Clone)]
pub struct ActionTarget {
    pub resource: ResourceId,
    pub resource_kind: String,
    pub resource_label: String,
    pub spec: ActionSpec,
    /// The argument the row runs with, as `(parameter, value)`: choosing a
    /// prompted action fills in the value the person typed, and an app's
    /// `open` invoked for one search hit carries the hit's anchor, so
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
/// `workspace`, launching the app first when it is closed.
#[derive(Clone, Debug, PartialEq)]
pub struct InvokeTarget {
    /// The app publishing the action.
    pub app: String,
    /// The workspace the app publishes it in.
    pub workspace: String,
    pub address: ActionAddress,
    /// What the row carries or collected, by parameter name: the anchor an
    /// `open` shows, the one line typed for a prompted action. Empty for
    /// the nullary majority.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The published action, when the row is the action itself rather than
    /// something it opens: what the row shows and asks for.
    pub descriptor: Option<ActionDescriptor>,
}

impl InvokeTarget {
    /// `app`'s `open` at `anchor`, in `workspace`: the item an anchor names, the
    /// app launched first when it is closed.
    #[must_use]
    pub fn open(
        app: impl Into<String>,
        workspace: impl Into<String>,
        anchor: impl Into<String>,
    ) -> Self {
        let anchor = anchor.into();
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            OPEN_ANCHOR.into(),
            serde_json::Value::String(anchor.clone()),
        );
        Self {
            app: app.into(),
            workspace: workspace.into(),
            address: ActionAddress::new(anchor, OPEN_ACTION),
            arguments,
            descriptor: None,
        }
    }

    /// The action `action` of resource `resource` that `app` publishes in
    /// `workspace`, run with no arguments.
    #[must_use]
    pub fn action(
        app: impl Into<String>,
        workspace: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            workspace: workspace.into(),
            address: ActionAddress::new(resource, action),
            arguments: serde_json::Map::new(),
            descriptor: None,
        }
    }

    /// With argument `name` set to `value`.
    #[must_use]
    pub fn with_argument(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        self.arguments.insert(name.to_owned(), value.into());
        self
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
    /// The row's own picture, when its source gives one (an app's hit, an
    /// action's declared icon); else its kind's.
    pub icon: Option<ui::icons::Icon>,
    /// Faces laid out beside the row: who is there, who is in a call.
    pub faces: Vec<ui::search::RowFace>,
    /// What choosing the row gets you, when it is worth saying ("Open",
    /// "Join"); the kind's own outcome otherwise.
    pub outcome: Option<String>,
    /// The row its app draws for the hit, when its source says: a shaped
    /// row (a message, a video, a match in a line), or the reference to the
    /// item its app keeps ([`ui::search::SearchRow::Item`]), which the
    /// palette draws as that app's list draws the item. `None` draws the
    /// generic row, as does a row the palette cannot paint.
    pub row: Option<ui::search::SearchRow>,
    /// What the hit looks like in its app, for the band under the list.
    pub preview: Option<ui::search::SearchPreview>,
    pub target: Target,
}

/// What a row is about, whichever source offers it: one result per thing,
/// so an app, a window and the document they show are listed once and not
/// three times.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Thing {
    /// The item an anchor names: a document, a page, a folder, a video.
    Resource(String),
    /// An application.
    App(String),
    /// A window, by its tab.
    Window(u64),
    /// One action of one resource, with the argument it runs when it
    /// carries one: the same action offered by two sources is one row.
    Action {
        resource: String,
        action: String,
        preset: Option<String>,
    },
    /// A command of the desktop or of a host, by id.
    Command(String),
    /// Anything else, by the row's own provider and id.
    Row(String, String),
}

impl Hit {
    /// What this row is about: two rows of the same [`Thing`] are one
    /// result offered twice, and the stronger one is kept.
    #[must_use]
    pub fn thing(&self) -> Thing {
        match &self.target {
            Target::Launch { app } if self.kind == ResourceKind::Application => {
                Thing::App(app.clone())
            }
            // The window row itself is the window; a row that only goes to
            // a window when chosen (a note shown in it, a place the person
            // was in) is its own thing, or every item one window shows
            // would be one row.
            Target::Window { tab, .. }
                if self.kind == ResourceKind::Custom(crate::providers::WINDOWS_KIND.into()) =>
            {
                Thing::Window(*tab)
            }
            Target::Command { id } => Thing::Command(id.clone()),
            // A row that opens an app's item at its anchor is the item, as
            // the same item opened through the app's `open` is.
            Target::Action(action) => match anchor_of(action) {
                Some(anchor) => Thing::Resource(anchor),
                None => Thing::Action {
                    resource: action.resource.as_str().to_owned(),
                    action: action.spec.id.clone(),
                    preset: action.preset.as_ref().map(|(_, value)| value.clone()),
                },
            },
            Target::Invoke(invoke) => {
                // A hit the app opens at its anchor (a document a provider
                // found) is the item, not the `open` it runs through.
                match opened_anchor(invoke) {
                    Some(anchor) => Thing::Resource(anchor),
                    None => Thing::Action {
                        resource: invoke.address.resource.clone(),
                        action: invoke.address.action.clone(),
                        preset: None,
                    },
                }
            }
            Target::Copy { .. }
            | Target::Open { .. }
            | Target::Cancel { .. }
            | Target::Prefer { .. }
            | Target::Launch { .. }
            | Target::Window { .. } => Thing::Row(self.provider.clone(), self.id.clone()),
        }
    }
}

/// The anchor `action` shows, when it is the app's `open` of one item.
fn anchor_of(action: &ActionTarget) -> Option<String> {
    if action.spec.id != OPEN_ACTION {
        return None;
    }
    action
        .preset
        .as_ref()
        .filter(|(param, value)| param == OPEN_ANCHOR && !value.is_empty())
        .map(|(_, value)| value.clone())
}

/// The anchor `invoke` opens, when it is the app's `open` of one item.
fn opened_anchor(invoke: &InvokeTarget) -> Option<String> {
    if invoke.descriptor.is_some() || invoke.address.action != OPEN_ACTION {
        return None;
    }
    invoke
        .arguments
        .get(OPEN_ANCHOR)
        .and_then(serde_json::Value::as_str)
        .filter(|anchor| !anchor.is_empty())
        .map(str::to_owned)
}

/// A source of hits for one domain (applications, windows, actions, …).
pub trait Source {
    fn id(&self) -> &str;
    /// What people call it, for the row that switches it off and on again
    /// ([`crate::providers::Switches`]). A source that does not say is
    /// listed by its id.
    fn name(&self) -> String {
        self.id().to_owned()
    }
    /// Top of this source's score scale; see [`system::merged_strength`].
    fn max_score(&self) -> i32 {
        system::MAX_PROVIDER_SCORE
    }
    /// Hits for `needle`. An empty needle asks for the source's browse set.
    fn query(&self, needle: &str) -> Vec<Hit>;
    /// Where its rows stand in the empty palette, lowest first: the search
    /// with nothing typed lists what is happening now before what the person
    /// reached for last, and the ways into everything else after. Sources
    /// that tie keep the order they are asked in. It says nothing about a
    /// typed query, which is ranked by the match.
    fn browse_rank(&self) -> u8 {
        BROWSE_RANK_LATER
    }
    /// How much plainer than its title's match `hit` answers what was
    /// typed, added to its strength at the merge: a query that names a
    /// service and asks it something leads, a row an app serves better
    /// gives way.
    fn intent(&self, _hit: &Hit) -> f32 {
        0.0
    }
    /// Whether it is asked what was typed, word for word: a sum, a web
    /// search. Every other source is asked it without the words a query
    /// is never required to match ([`system::without_filler`]).
    fn verbatim(&self) -> bool {
        false
    }
    /// Take in what arrived since the rows were built that changes them (an
    /// answer still being written), and say whether it did: the palette
    /// builds the rows again when one says it did.
    fn poll(&self) -> bool {
        false
    }
}

/// A source a host keeps between searches (its index built once) is
/// searched through a shared handle.
impl<S: Source + ?Sized> Source for std::rc::Rc<S> {
    fn id(&self) -> &str {
        (**self).id()
    }
    fn name(&self) -> String {
        (**self).name()
    }
    fn max_score(&self) -> i32 {
        (**self).max_score()
    }
    fn query(&self, needle: &str) -> Vec<Hit> {
        (**self).query(needle)
    }
    fn browse_rank(&self) -> u8 {
        (**self).browse_rank()
    }
    fn intent(&self, hit: &Hit) -> f32 {
        (**self).intent(hit)
    }
    fn verbatim(&self) -> bool {
        (**self).verbatim()
    }
    fn poll(&self) -> bool {
        (**self).poll()
    }
}

/// How long one frame may spend asking sources: the rows of the fast ones
/// are on screen while a slow source is still answering.
pub const FRAME_BUDGET: Duration = Duration::from_millis(8);

/// The instant a frame that starts now may wait until: `budget` from now,
/// or the clock's own end when that is nearer.
#[must_use]
pub fn frame_deadline(budget: Duration) -> Instant {
    let now = ui::time::now();
    now.checked_add(budget).unwrap_or(now)
}

/// How long a source is assumed to take before it was measured: one that
/// has not answered yet is asked with the fast ones.
const UNMEASURED: Duration = Duration::from_millis(1);

/// What a source took, as it is asked again: the search calls a source
/// that answered in a frame's time here, and defers one that took longer
/// to a frame of its own, so a slow source never holds up the rows.
#[derive(Clone, Debug, Default)]
pub struct SourceCosts {
    took: HashMap<String, Duration>,
}

impl SourceCosts {
    /// How long `id` took last time, or what an unmeasured source is
    /// assumed to take.
    #[must_use]
    pub fn estimate(&self, id: &str) -> Duration {
        self.took.get(id).copied().unwrap_or(UNMEASURED)
    }

    /// How long `id` took last time, if it answered before.
    #[must_use]
    pub fn measured(&self, id: &str) -> Option<Duration> {
        self.took.get(id).copied()
    }

    /// Record that `id` took `took`, smoothing with what it took before so
    /// one slow answer does not hide a source that is usually fast.
    pub fn record(&mut self, id: &str, took: Duration) {
        let next = match self.took.get(id) {
            Some(before) => {
                let both = before.saturating_add(took);
                Duration::from_secs_f64(both.as_secs_f64() / 2.0)
            }
            None => took,
        };
        self.took.insert(id.to_owned(), next);
    }

    /// Whether `id` answered slower than a frame may spend on it.
    #[must_use]
    pub fn is_slow(&self, id: &str) -> bool {
        self.estimate(id) > FRAME_BUDGET
    }
}

/// One search, as it fills in: the hits of the sources asked so far,
/// merged on the shared ranking, one row per [`Thing`]. The palette asks
/// for another frame's worth with [`resume`](Self::resume) until nothing
/// is [`pending`](Self::pending); the rows it has are always the best
/// answer of what came in.
pub struct SearchStream {
    needle: String,
    row_budget: usize,
    /// The sources not asked yet, by their place among the sources: two
    /// sources may search the same kind of thing under one id (an app's
    /// open providers and the index of what apps published), and each is
    /// asked for its own rows.
    pending: Vec<usize>,
    /// The rows so far, each with its strength on the shared scale.
    merged: Vec<(Hit, f32)>,
    /// Where the row of a thing stands in `merged`, so two sources offering
    /// the same thing give one row, the stronger.
    things: HashMap<Thing, usize>,
    /// Whether any source answered already: a source measured slower than
    /// a frame is asked in a later frame, never in the one that first lists
    /// rows.
    answered: bool,
}

impl SearchStream {
    fn new(needle: &str, sources: &[&dyn Source], row_budget: usize) -> Self {
        let bounded = system::query_input_is_bounded(needle);
        Self {
            needle: needle.to_owned(),
            row_budget,
            pending: if bounded {
                (0..sources.len()).collect()
            } else {
                Vec::new()
            },
            merged: Vec::new(),
            things: HashMap::new(),
            answered: false,
        }
    }

    /// Whether a source is still to answer.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The hits that came in, in the ranking's order, one row per thing.
    #[must_use]
    pub fn rows(&self) -> Vec<Hit> {
        let mut merged = self.kept();
        system::order_by_kind(&mut merged, |hit| &hit.kind);
        merged.into_iter().map(|(hit, _)| hit).collect()
    }

    /// The rows so far, grouped by kind under the search's row budget.
    #[must_use]
    pub fn groups(&self) -> Vec<KindGroup<Hit>> {
        let mut merged = self.kept();
        system::order_by_kind(&mut merged, |hit| &hit.kind);
        system::group_by_kind(
            merged.into_iter().map(|(hit, _)| hit).collect(),
            |hit| &hit.kind,
            self.row_budget,
        )
    }

    /// The rows so far without the windows that only show a document the
    /// search found anyway: a document is one result, whether it is open in
    /// a window or not.
    fn kept(&self) -> Vec<(Hit, f32)> {
        let shown: Vec<String> = self
            .merged
            .iter()
            .filter(|(hit, _)| {
                matches!(hit.thing(), Thing::Resource(_))
                    && hit.kind != ResourceKind::Custom(crate::providers::WINDOWS_KIND.into())
                    && !hit.title.trim().is_empty()
            })
            .map(|(hit, _)| ui::search::fold_search_text(&hit.title))
            .collect();
        self.merged
            .iter()
            .filter(|(hit, _)| {
                !matches!(hit.thing(), Thing::Window(_))
                    || !shown.contains(&ui::search::fold_search_text(&hit.title))
            })
            .cloned()
            .collect()
    }

    /// Ask the sources that still owe an answer, until the frame's
    /// `deadline` (or until `deadline` has passed, for the caller that
    /// wants everything at once). Returns whether a source was asked, so a
    /// host knows to ask again for the rows it brought.
    pub fn resume(
        &mut self,
        sources: &[&dyn Source],
        costs: &mut SourceCosts,
        deadline: Instant,
        used: &dyn Fn(&Hit) -> f32,
    ) -> bool {
        let listed = self.answered;
        let mut did_ask = false;
        while let Some(index) = self.next_index(sources, costs, listed, deadline) {
            if !self.ask(index, sources, costs, used) {
                continue;
            }
            did_ask = true;
        }
        // A frame with nothing to show, or only the row every query gets,
        // asks the sources still to answer, fastest first, until one brings
        // a row of its own: the instant answer (a sum, a conversion) is
        // then in the frame that would otherwise say nothing matched or
        // lead with the web. Every source measured within a frame is asked;
        // one slower only when it is the first asked of this frame, so a
        // search with nothing to show still moves and no frame waits long.
        let mut first = true;
        while self.merged.is_empty()
            || self
                .merged
                .iter()
                .all(|(hit, _)| crate::providers::WebSearch::is_fallback(hit))
        {
            let Some((index, took)) = self.fastest_pending(sources, costs) else {
                break;
            };
            if !first && took > FRAME_BUDGET {
                break;
            }
            first = false;
            if !self.ask(index, sources, costs, used) {
                continue;
            }
            did_ask = true;
        }
        did_ask
    }

    /// The source still to answer that is measured fastest, with what it
    /// took: one never measured counts as instant.
    fn fastest_pending(
        &self,
        sources: &[&dyn Source],
        costs: &SourceCosts,
    ) -> Option<(usize, Duration)> {
        self.pending
            .iter()
            .enumerate()
            .min_by_key(|(index, at)| {
                let took = sources
                    .get(**at)
                    .and_then(|source| costs.measured(source.id()))
                    .unwrap_or_default();
                (took, *index)
            })
            .map(|(index, at)| {
                let took = sources
                    .get(*at)
                    .and_then(|source| costs.measured(source.id()))
                    .unwrap_or_default();
                (index, took)
            })
    }

    /// Ask the `index`th source still to answer, keeping its rows and what
    /// it took. Returns whether it was asked at all.
    fn ask(
        &mut self,
        index: usize,
        sources: &[&dyn Source],
        costs: &mut SourceCosts,
        used: &dyn Fn(&Hit) -> f32,
    ) -> bool {
        let at = self.pending.remove(index);
        let Some(source) = sources.get(at).copied() else {
            return false;
        };
        let started = ui::time::now();
        let top = source.max_score().max(1);
        // "my mails" asks for mails: possessives and filler never have to
        // match.
        let typed = self.needle.clone();
        let unasked_filler = system::without_filler(&typed);
        let query: &str = if source.verbatim() {
            typed.as_str()
        } else {
            unasked_filler.as_ref()
        };
        for hit in source.query(query) {
            let strength = |needle: &str| {
                system::merged_strength(
                    needle,
                    &hit.kind,
                    &hit.title,
                    hit.subtitle.as_deref(),
                    hit.score.min(top),
                    top,
                    source.intent(&hit) + used(&hit),
                )
            };
            // A title is matched as typed too: "The Beatles" is still the
            // band's whole name with its "the".
            let strength = if query == typed {
                strength(&typed)
            } else {
                strength(query).max(strength(&typed))
            };
            self.keep(hit, strength);
        }
        costs.record(
            source.id(),
            ui::time::now().saturating_duration_since(started),
        );
        self.answered = true;
        true
    }

    /// The source to ask next: the first one in the search's own order
    /// that fits this frame. A source never measured is asked whatever the
    /// frame's time, one measured faster than the budget whenever it still
    /// fits, and one measured slower only in a frame after the search
    /// listed rows, so a slow source never holds up the first rows.
    fn next_index(
        &self,
        sources: &[&dyn Source],
        costs: &SourceCosts,
        listed: bool,
        deadline: Instant,
    ) -> Option<usize> {
        for (index, at) in self.pending.iter().enumerate() {
            let Some(source) = sources.get(*at) else {
                continue;
            };
            let Some(took) = costs.measured(source.id()) else {
                return Some(index);
            };
            if took <= deadline.saturating_duration_since(ui::time::now()) {
                return Some(index);
            }
            if took > FRAME_BUDGET && listed {
                return Some(index);
            }
        }
        None
    }

    /// Keep `hit` as the row of its thing, when it is the strongest so far.
    fn keep(&mut self, hit: Hit, strength: f32) {
        let thing = hit.thing();
        if let Some(at) = self.things.get(&thing).copied()
            && let Some(row) = self.merged.get_mut(at)
        {
            if strength > row.1 {
                *row = (hit, strength);
            }
            return;
        }
        self.things.insert(thing, self.merged.len());
        self.merged.push((hit, strength));
    }
}

impl std::fmt::Debug for SearchStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchStream")
            .field("needle", &self.needle)
            .field("pending", &self.pending)
            .field("rows", &self.merged.len())
            .finish_non_exhaustive()
    }
}

/// One frame of a search: ask the sources whose answer fits `deadline` and
/// merge what they found, one row per thing ([`Thing`]).
pub fn search_stream(
    needle: &str,
    sources: &[&dyn Source],
    row_budget: usize,
    costs: &mut SourceCosts,
    deadline: Instant,
    used: &dyn Fn(&Hit) -> f32,
) -> SearchStream {
    let mut stream = SearchStream::new(needle, sources, row_budget);
    stream.resume(sources, costs, deadline, used);
    stream
}

/// Fan `needle` out to `sources`, merge their hits on the shared ranking
/// and group them by kind under a shared `row_budget` (zero keeps all).
///
/// Each hit's strength is raised by `used`: what the person's earlier
/// choices add to it ([`crate::usage::Usage::strength`]).
pub fn search(
    needle: &str,
    sources: &[&dyn Source],
    row_budget: usize,
    used: &dyn Fn(&Hit) -> f32,
) -> Vec<KindGroup<Hit>> {
    // Everything at once: this caller has no frames, so no source is
    // deferred.
    let mut stream = SearchStream::new(needle, sources, row_budget);
    let mut costs = SourceCosts::default();
    while stream.pending() {
        stream.resume(sources, &mut costs, frame_deadline(ONE_PASS), used);
    }
    stream.groups()
}

/// The deadline a caller without frames gives its one pass: far enough that
/// every source is asked whatever it takes.
const ONE_PASS: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that answers with its rows for any needle naming one of
    /// them, or all of them for the empty one.
    struct Fixed {
        id: &'static str,
        rank: u8,
        hits: Vec<Hit>,
    }

    impl Source for Fixed {
        fn id(&self) -> &str {
            self.id
        }

        fn browse_rank(&self) -> u8 {
            self.rank
        }

        fn query(&self, needle: &str) -> Vec<Hit> {
            let typed = ui::search::fold_search_text(needle.trim());
            self.hits
                .iter()
                .filter(|hit| {
                    typed.is_empty() || ui::search::fold_search_text(&hit.title).contains(&typed)
                })
                .cloned()
                .collect()
        }
    }

    fn hit(
        provider: &str,
        id: &str,
        title: &str,
        kind: ResourceKind,
        score: i32,
        target: Target,
    ) -> Hit {
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind,
            provider: provider.into(),
            id: id.into(),
            title: title.into(),
            subtitle: None,
            facts: Vec::new(),
            score,
            icon: None,
            target,
        }
    }

    /// The `open` every framework app publishes: it shows the item its
    /// anchor names.
    fn open_action() -> ActionSpec {
        use tilo_actions::{EncodedAction, OperationKind};
        ActionSpec::typed_operation(
            OPEN_ACTION,
            "Open",
            "Item to open",
            OperationKind::View,
            tilo_actions::remote::APPLICATION_OPEN_TYPE,
            vec![tilo_actions::ActionParam::required(
                OPEN_ANCHOR,
                tilo_actions::ParamType::Text,
                "Item to open",
            )],
            |_| EncodedAction {
                type_id: tilo_actions::remote::APPLICATION_OPEN_TYPE.into(),
                payload: Vec::new(),
            },
        )
    }

    /// The page anchor `id`, as an app opens it.
    fn page_row(
        provider: &str,
        id: &str,
        title: &str,
        kind: ResourceKind,
        score: i32,
        anchor: &str,
    ) -> Hit {
        let mut target = ActionTarget {
            resource: ResourceId::new("so.tilo.notes"),
            resource_kind: "notes.book".into(),
            resource_label: "Notes".into(),
            spec: open_action(),
            preset: None,
        };
        let param = target
            .spec
            .params()
            .first()
            .map(|param| param.name.to_string());
        target.preset = param.map(|param| (param, anchor.to_owned()));
        hit(
            provider,
            id,
            title,
            kind,
            score,
            Target::Action(Box::new(target)),
        )
    }

    fn continue_row(title: &str, anchor: &str) -> Hit {
        hit(
            "continue",
            &format!("place:{anchor}"),
            title,
            crate::providers::places::continue_kind(),
            60,
            Target::Invoke(Box::new(InvokeTarget::open(
                "so.tilo.notes",
                "workspace",
                anchor,
            ))),
        )
    }

    fn window_row(title: &str, tab: u64) -> Hit {
        hit(
            "desktop-windows",
            &tab.to_string(),
            title,
            ResourceKind::Custom(crate::providers::WINDOWS_KIND.into()),
            60,
            Target::Window {
                tab,
                app: Some("so.tilo.notes".into()),
            },
        )
    }

    fn rows(groups: &[KindGroup<Hit>]) -> Vec<String> {
        groups
            .iter()
            .flat_map(|group| group.results.iter())
            .map(|hit| format!("{}:{}", hit.provider, hit.title))
            .collect()
    }

    struct World {
        applications: Fixed,
        windows: Fixed,
        content: Fixed,
        places: Fixed,
        actions: Fixed,
        commands: Fixed,
    }

    impl World {
        fn all(&self) -> Vec<&dyn Source> {
            vec![
                &self.applications,
                &self.windows,
                &self.content,
                &self.places,
                &self.actions,
                &self.commands,
            ]
        }
    }

    /// The `open` of a note at an anchor, as an app publishes it.
    fn note_open(id: &str, title: &str, score: i32, anchor: &str) -> Hit {
        page_row(
            "app-content",
            id,
            title,
            ResourceKind::Note,
            score,
            &format!("so.tilo.notes\u{1f}{anchor}"),
        )
    }

    fn share_row() -> Hit {
        hit(
            "window-actions",
            "share",
            "Share",
            ResourceKind::Action,
            100,
            Target::Action(Box::new(ActionTarget {
                resource: ResourceId::new("so.tilo.notes"),
                resource_kind: "notes.book".into(),
                resource_label: "Notes".into(),
                spec: ActionSpec::immediate_encoded(
                    "share",
                    "Share",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    tilo_actions::EncodedAction {
                        type_id: "test.action".into(),
                        payload: Vec::new(),
                    },
                ),
                preset: None,
            })),
        )
    }

    fn toggle_theme_row() -> Hit {
        hit(
            "desktop-commands",
            "toggle-theme",
            "Toggle theme",
            ResourceKind::Action,
            100,
            Target::Command {
                id: "desktop.toggle-theme".into(),
            },
        )
    }

    fn fixed(id: &'static str, rank: u8, hits: Vec<Hit>) -> Fixed {
        Fixed { id, rank, hits }
    }

    /// One small desktop with every kind of row in it: the apps and the
    /// windows on it, a note that both the content index and the person's
    /// places offer, an action, and a command.
    fn world() -> World {
        World {
            applications: fixed(
                "desktop-applications",
                BROWSE_RANK_APPS,
                vec![
                    hit(
                        "desktop-applications",
                        "so.tilo.notes",
                        "Notes",
                        ResourceKind::Application,
                        100,
                        Target::Launch {
                            app: "so.tilo.notes".into(),
                        },
                    ),
                    hit(
                        "desktop-applications",
                        "so.tilo.sheets",
                        "Sheets",
                        ResourceKind::Application,
                        100,
                        Target::Launch {
                            app: "so.tilo.sheets".into(),
                        },
                    ),
                ],
            ),
            windows: fixed(
                "desktop-windows",
                BROWSE_RANK_WINDOWS,
                vec![window_row("Groceries", 3), window_row("Budget", 4)],
            ),
            content: fixed(
                "app-content",
                BROWSE_RANK_LATER,
                vec![
                    note_open("page:1\u{1f}open", "Groceries", 10, "page:1"),
                    note_open("page:2\u{1f}open", "Trip plan", 10, "page:2"),
                ],
            ),
            places: fixed(
                "continue",
                BROWSE_RANK_RECENT,
                vec![
                    continue_row("Groceries", "so.tilo.notes\u{1f}page:1"),
                    continue_row("Trip plan", "so.tilo.notes\u{1f}page:2"),
                ],
            ),
            actions: fixed("window-actions", BROWSE_RANK_LATER, vec![share_row()]),
            commands: fixed(
                "desktop-commands",
                BROWSE_RANK_LATER,
                vec![toggle_theme_row()],
            ),
        }
    }

    /// One document is one row, however many sources offer it: the window
    /// showing it, the app asking for it and the place the person left are
    /// the same result, and the strongest offer is the row.
    #[test]
    fn a_document_is_one_row_and_not_one_per_source() {
        let world = world();
        let groups = search("", &world.all(), 0, &|_| 0.0);
        let listed = rows(&groups);
        assert_eq!(
            listed
                .iter()
                .filter(|row| row.ends_with(":Groceries"))
                .count(),
            1,
            "the window showing Groceries is the Groceries row, listed once: {listed:?}"
        );
        assert!(
            !listed.iter().any(|row| row == "desktop-windows:Groceries"),
            "the window is not listed beside the document: {listed:?}"
        );
        assert!(
            listed.iter().any(|row| row == "desktop-windows:Budget"),
            "a window showing something else is its own row: {listed:?}"
        );
        assert_eq!(
            groups.iter().map(|group| group.total).sum::<usize>(),
            groups
                .iter()
                .map(|group| group.results.len())
                .sum::<usize>(),
            "no group counts a row it does not show"
        );
    }

    /// The same action published by two sources is one row.
    #[test]
    fn an_action_offered_twice_is_one_row() {
        let mut world = world();
        let shared = world.actions.hits[0].clone();
        world.commands.hits.push(shared);
        let groups = search("share", &world.all(), 0, &|_| 0.0);
        assert_eq!(rows(&groups), ["window-actions:Share"]);
    }

    /// Two sources that search the same kind of thing under one id (an
    /// app's open providers and the index of what apps published) both
    /// answer: a search asks each source it was given, never the first one
    /// that carries the id.
    #[test]
    fn two_sources_sharing_an_id_both_answer() {
        let one = Fixed {
            id: "app-content",
            rank: 0,
            hits: vec![hit(
                "app-content",
                "n1",
                "Groceries for the weekend",
                ResourceKind::Note,
                20,
                Target::Cancel {
                    app: "notes".into(),
                    workspace: "Main".into(),
                    invocation: "close".into(),
                },
            )],
        };
        let two = Fixed {
            id: "app-content",
            rank: 0,
            hits: vec![hit(
                "app-content",
                "n2",
                "Groceries list",
                ResourceKind::Note,
                20,
                Target::Cancel {
                    app: "notes".into(),
                    workspace: "Main".into(),
                    invocation: "close".into(),
                },
            )],
        };
        let sources: Vec<&dyn Source> = vec![&one, &two];
        let groups = search("groceries", &sources, 0, &|_| 0.0);
        assert_eq!(
            rows(&groups),
            [
                "app-content:Groceries for the weekend",
                "app-content:Groceries list"
            ],
            "both sources answered"
        );
    }

    /// What the person chose before leads among equal matches: the ranking
    /// a tool that places text uses is the palette's.
    #[test]
    fn what_the_person_chose_before_leads_equal_matches() {
        let world = world();
        let used = |hit: &Hit| if hit.title == "Trip plan" { 0.6 } else { 0.0 };
        let groups = search("", &world.all(), 0, &used);
        let listed = rows(&groups);
        let tabs: Vec<&str> = listed.iter().take(3).map(String::as_str).collect();
        assert_eq!(
            tabs[0], "continue:Trip plan",
            "the place the person chose before leads the equal ones: {tabs:?}"
        );
    }

    /// The one ranking, as a table: what a person types, and the rows they
    /// see in order, for every entry point (the palette, a window, the
    /// text placed on the canvas, an agent's `tilo` tool). Two desktops
    /// must agree on it.
    #[test]
    fn the_one_ranking_table() {
        let world = world();
        let table: &[(&str, &[&str])] = &[
            (
                "",
                &[
                    "window-actions:Share",
                    "desktop-commands:Toggle theme",
                    "desktop-applications:Notes",
                    "desktop-applications:Sheets",
                    "continue:Groceries",
                    "continue:Trip plan",
                    "desktop-windows:Budget",
                ],
            ),
            ("notes", &["desktop-applications:Notes"]),
            ("groceries", &["continue:Groceries"]),
            ("share", &["window-actions:Share"]),
            ("toggle", &["desktop-commands:Toggle theme"]),
            ("budget", &["desktop-windows:Budget"]),
            // Nothing matches, and nothing is invented for it: a web
            // search is another source's row, not this ranking's.
            ("zzz", &[]),
        ];
        for (query, expected) in table {
            assert_eq!(
                rows(&search(query, &world.all(), 0, &|_| 0.0)),
                *expected,
                "query {query:?}"
            );
        }
    }

    #[test]
    fn a_query_is_ranked_by_its_match() {
        let world = world();
        assert_eq!(
            rows(&search("grocer", &world.all(), 0, &|_| 0.0)),
            ["continue:Groceries"]
        );
        assert_eq!(
            rows(&search("sheet", &world.all(), 0, &|_| 0.0)),
            ["desktop-applications:Sheets"]
        );
        assert_eq!(
            rows(&search("budge", &world.all(), 0, &|_| 0.0)),
            ["desktop-windows:Budget"]
        );
    }

    #[test]
    fn an_empty_query_keeps_the_browse_order_of_the_palette() {
        let world = world();
        let groups = search("", &world.all(), 0, &|_| 0.0);
        let kinds: Vec<String> = groups
            .iter()
            .map(|group| group.kind.label().to_owned())
            .collect();
        assert_eq!(
            kinds.len(),
            kinds.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }

    /// A source that takes 20 ms of the frame it is asked in.
    struct Slow(Rc<std::cell::Cell<usize>>);

    impl Source for Slow {
        fn id(&self) -> &'static str {
            "slow"
        }
        fn query(&self, _: &str) -> Vec<Hit> {
            self.0.set(self.0.get().saturating_add(1));
            std::thread::sleep(Duration::from_millis(20));
            vec![hit(
                "slow",
                "late",
                "Late answer",
                ResourceKind::Note,
                100,
                Target::Copy {
                    text: "late".into(),
                },
            )]
        }
    }

    /// A source measured slower than a frame is asked in a frame of its
    /// own: the frame that first lists rows returns without waiting for it,
    /// and its rows fill in after, without moving anything else.
    #[test]
    fn a_slow_source_never_delays_the_frame_that_lists_rows() {
        let world = world();
        let asked = Rc::new(std::cell::Cell::new(0));
        let slow = Slow(Rc::clone(&asked));
        let mut all = world.all();
        all.push(&slow);
        let mut costs = SourceCosts::default();
        // The first search of a session meets the source and pays for it
        // once; from then on it is known.
        let mut first = search_stream(
            "",
            &all,
            0,
            &mut costs,
            frame_deadline(FRAME_BUDGET),
            &|_| 0.0,
        );
        while first.pending() {
            first.resume(&all, &mut costs, frame_deadline(FRAME_BUDGET), &|_| 0.0);
        }
        assert!(costs.is_slow("slow"), "20 ms is more than a frame");

        let mut second = search_stream(
            "",
            &all,
            0,
            &mut costs,
            frame_deadline(FRAME_BUDGET),
            &|_| 0.0,
        );
        assert!(
            second.pending(),
            "the slow source waits for a frame of its own"
        );
        assert!(
            !rows(&second.groups()).is_empty(),
            "the frame that returns first holds what came in"
        );
        assert!(
            !rows(&second.groups())
                .iter()
                .any(|row| row.contains("Late")),
            "the slow source's rows are not in it"
        );
        let frames = std::cell::Cell::new(0);
        while second.pending() {
            frames.set(frames.get() + 1);
            assert!(
                second.resume(&all, &mut costs, frame_deadline(FRAME_BUDGET), &|_| 0.0),
                "every frame asks something"
            );
            assert!(
                frames.get() < 10,
                "the slow source answers on a later frame"
            );
        }
        assert!(
            rows(&second.groups())
                .iter()
                .any(|row| row == "slow:Late answer"),
            "its rows are there once it answered"
        );
    }

    /// A desktop with a few thousand names in it is still ranked into its
    /// first frame: the person's first row arrives well inside one frame of
    /// typing, whatever the size of the index behind the sources.
    #[test]
    fn the_first_frame_of_a_large_desktop_is_well_inside_a_frame() {
        struct Many {
            id: &'static str,
            rank: u8,
            hits: Vec<Hit>,
        }

        impl Source for Many {
            fn id(&self) -> &str {
                self.id
            }

            fn browse_rank(&self) -> u8 {
                self.rank
            }

            fn query(&self, needle: &str) -> Vec<Hit> {
                let typed = ui::search::fold_search_text(needle.trim());
                self.hits
                    .iter()
                    .filter(|hit| {
                        typed.is_empty()
                            || ui::search::fold_search_text(&hit.title).contains(&typed)
                    })
                    .cloned()
                    .collect()
            }
        }

        let providers = [
            "desktop-applications",
            "continue",
            "desktop-windows",
            "content",
        ];
        let sources: Vec<Many> = providers
            .iter()
            .enumerate()
            .map(|(rank, provider)| Many {
                id: provider,
                rank: u8::try_from(rank).unwrap_or(u8::MAX),
                hits: (0..500)
                    .map(|i| {
                        let anchor = format!("{provider}-{i}");
                        let mut row = hit(
                            provider,
                            &anchor,
                            &format!("Note {i}"),
                            ResourceKind::Note,
                            40,
                            Target::Invoke(Box::new(InvokeTarget::open(
                                "so.tilo.notes",
                                "workspace",
                                &anchor,
                            ))),
                        );
                        row.facts = vec![format!("{provider}-{i}")];
                        row
                    })
                    .collect(),
            })
            .collect();
        let refs: Vec<&dyn Source> = sources.iter().map(|source| source as &dyn Source).collect();
        let mut samples = Vec::new();
        for round in 0..100 {
            let needle = if round % 2 == 0 {
                String::new()
            } else {
                format!("note {round}")
            };
            // A cold session: nothing has been measured yet, so no source
            // is deferred and every one of the four answers this frame.
            let mut costs = SourceCosts::default();
            let started = ui::time::now();
            let stream = search_stream(
                &needle,
                &refs,
                12,
                &mut costs,
                frame_deadline(FRAME_BUDGET),
                &|_| 0.0,
            );
            let groups = stream.groups();
            let took = ui::time::now().saturating_duration_since(started);
            assert!(
                rows(&groups).iter().any(|row| row.starts_with("content:")),
                "the frame lists rows: {needle:?}"
            );
            samples.push(took);
        }
        samples.sort();
        let p95 = samples[samples.len() * 95 / 100];
        eprintln!("p95 first frame over 2000 rows: {p95:?}");
        assert!(
            p95 < Duration::from_millis(30),
            "p95 first frame over 2000 rows is {p95:?}"
        );
    }

    /// A source whose rows are worked out already, for any needle: an
    /// answer or a web search answers what was typed, word for word.
    struct Ready {
        id: &'static str,
        hits: Vec<Hit>,
    }

    impl Source for Ready {
        fn id(&self) -> &str {
            self.id
        }

        fn query(&self, _needle: &str) -> Vec<Hit> {
            self.hits.clone()
        }
    }

    /// The answer a source that answers at once brings is in the frame,
    /// ahead of the row every query gets, even while a slower source is
    /// still out: a frame holding nothing but the web row is not the
    /// answer.
    #[test]
    fn an_instant_answer_is_in_the_frame_with_the_web_row() {
        use crate::providers::{WebSearch, answers_kind};
        let web = WebSearch::default();
        let answers = Ready {
            id: "quick-answers",
            hits: vec![hit(
                "quick-answers",
                "sum:12*7",
                "84",
                answers_kind(),
                system::MAX_PROVIDER_SCORE,
                Target::Copy { text: "84".into() },
            )],
        };
        let sources: Vec<&dyn Source> = vec![&web, &answers];
        let mut costs = SourceCosts::default();
        costs.record("web", Duration::from_micros(5));
        // Measured slower than a frame: not asked with the fast ones.
        costs.record("quick-answers", FRAME_BUDGET + Duration::from_millis(20));
        let stream = search_stream(
            "12*7",
            &sources,
            0,
            &mut costs,
            frame_deadline(FRAME_BUDGET),
            &|_| 0.0,
        );
        let names: Vec<String> = stream.rows().into_iter().map(|row| row.title).collect();
        assert_eq!(
            names.first().map(String::as_str),
            Some("84"),
            "the sum leads: {names:?}"
        );
        assert!(
            names.iter().any(|name| name.starts_with("Search the web")),
            "the web row is still offered: {names:?}"
        );
    }

    /// A window is one row; the items it shows are their own rows, or
    /// every item one window shows would collapse into it.
    #[test]
    fn items_shown_in_one_window_are_their_own_rows() {
        use crate::providers::WINDOWS_KIND;
        let to_window = || Target::Window { tab: 0, app: None };
        let window = hit(
            "windows",
            "window:0",
            "Notes",
            ResourceKind::Custom(WINDOWS_KIND.into()),
            10,
            to_window(),
        );
        let groceries = hit(
            "app-actions",
            "note:one",
            "Groceries",
            ResourceKind::Note,
            10,
            to_window(),
        );
        let bills = hit(
            "app-actions",
            "note:two",
            "Bills",
            ResourceKind::Note,
            10,
            to_window(),
        );
        assert_eq!(window.thing(), Thing::Window(0));
        assert_ne!(groceries.thing(), bills.thing());
        assert_ne!(groceries.thing(), Thing::Window(0));
    }

    use std::rc::Rc;
}
