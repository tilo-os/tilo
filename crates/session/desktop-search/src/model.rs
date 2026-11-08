//! The portable query model: sources answer a needle with hits, and
//! [`search_stream`] merges them on the shared ranking into kind groups, a frame at
//! a time.
//! No source runs on the frame thread: a frame hands the sources that can move
//! ([`Source::shared`]) to a pool of workers and takes the answers that are in, so a
//! source reading a disk index, a mailbox or a service never holds up the frame that
//! typed into it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_actions::{ActionAddress, ActionDescriptor, ActionSpec, ResourceId};

use ui::search::system::{self, KindGroup, ResourceKind};

use crate::workers::{Answer, SearchWorkers};

/// Where the view of an application a row opens goes: what a modifier held as the row
/// is chosen asks of it ([`ActivationModifier`]), and what the selected row says it
/// will do before it is.
// [`ActivationModifier`]: ui::search::ActivationModifier
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchPlace {
    /// In the window the app already has, raised, or a window of its own
    /// when it has none: what choosing without a modifier does.
    #[default]
    Focus,
    /// As a tab of the window in front.
    TabHere,
    /// As a window of its own, beside the one in front.
    Beside,
}

/// What choosing a hit does. The host performs it exactly as its other
/// controls do: a launch opens a window, a window hit focuses it, an action
/// runs through the host's action door.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Open the application where the person asks of it (`place`).
    Launch { app: String, place: LaunchPlace },
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

/// A line as the palette compares it: folded, and without the sentence
/// punctuation that ends it, so a description that repeats its own name up
/// to a full stop is still the same line said twice.
#[must_use]
pub fn spoken(text: &str) -> String {
    ui::search::fold_search_text(text.trim())
        .trim_end_matches(['.', ',', ';', ':', '!', '?', '\u{2026}', ' '])
        .to_owned()
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
/// What the other workspaces hold: their records, which one Enter brings
/// into this one.
pub const BROWSE_RANK_RECORDS: u8 = 5;
/// What a person can do from here: the workspace's own verbs and the
/// session's, then the window in front's and the other workspaces'.
pub const BROWSE_RANK_ACTIONS: u8 = 6;

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
    /// The row its app draws for the hit, when its source says: a shaped row, or the
    /// reference to the item its app keeps ([`ui::search::SearchRow::Item`]), which the
    /// palette draws as that app's list draws the item. `None` draws the generic row, as
    /// does a row the palette cannot paint.
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
    /// An application, by [`app_identity`]: the same app is one row however
    /// a source starts it (the session's own package, the machine's desktop
    /// entry, the Store page that installs it).
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

/// The id of the source that offers what the person would get from the
/// Store. Its rows are the only ones that are not already theirs.
pub const CATALOGUE_SOURCE: &str = "store-catalogue";

impl Hit {
    /// How far this row is from the person: a row taken from the catalogue
    /// offers something they do not have, so the merge lists it after every
    /// row they do ([`system::Availability`]).
    #[must_use]
    pub fn availability(&self) -> system::Availability {
        use system::Availability;
        // A row the catalogue offers is what the person would get; a Store
        // page is what they would first install.
        if self.provider != CATALOGUE_SOURCE {
            return Availability::OnDevice;
        }
        if matches!(self.target, Target::Open { .. }) {
            Availability::ThirdPartyCatalogue
        } else {
            Availability::TiloCatalogue
        }
    }

    /// What this row is about: two rows of the same [`Thing`] are one
    /// result offered twice, and the stronger one is kept.
    #[must_use]
    pub fn thing(&self) -> Thing {
        match &self.target {
            Target::Launch { app, .. } if self.kind == ResourceKind::Application => {
                Thing::App(app_identity(app))
            }
            // The window row is the window, and so is the place the person continues from when
            // that place is the window still open: one row, the one offering most. A row that
            // only goes to a window when chosen is its own thing, or every item one window
            // shows would be one row.
            Target::Window { tab, .. }
                if self.kind == ResourceKind::Custom(crate::providers::WINDOWS_KIND.into())
                    || self.kind == crate::providers::places::continue_kind() =>
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
            // A Store page is the app it installs, not a page of its own: a
            // person who has the app gets their app's row instead of the
            // same app again as something to get.
            Target::Open { uri, .. } => match store_page_app(uri) {
                Some(app) => Thing::App(app_identity(app)),
                None => Thing::Row(self.provider.clone(), self.id.clone()),
            },
            Target::Copy { .. }
            | Target::Cancel { .. }
            | Target::Prefer { .. }
            | Target::Launch { .. }
            | Target::Window { .. } => Thing::Row(self.provider.clone(), self.id.clone()),
        }
    }
}

/// The prefix of a link to an app's page in the Store.
pub const STORE_PAGE_PREFIX: &str = "tilo-store:app/";

/// The app the Store link `uri` opens, when it opens one.
#[must_use]
pub fn store_page_app(uri: &str) -> Option<&str> {
    uri.strip_prefix(STORE_PAGE_PREFIX)
        .filter(|id| !id.is_empty())
}

/// The identity of the application `app` is started by: its package id, or the
/// machine's desktop entry (`host:<entry>`), without the `.desktop` a launch adds and
/// in one case, so the app is one [`Thing::App`] whichever row starts it.
#[must_use]
pub fn app_identity(app: &str) -> String {
    let app = app.trim();
    let app = app
        .strip_prefix(crate::providers::host_apps::HOST_APP_PREFIX)
        .unwrap_or(app);
    app.strip_suffix(".desktop")
        .unwrap_or(app)
        .to_ascii_lowercase()
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
    /// Where its rows stand in the empty palette, lowest first: what is happening now
    /// before what the person reached for last, and the ways into everything else after.
    /// Sources that tie keep the order they are asked in; a typed query is ranked by the
    /// match.
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
    /// This source as a worker asks it, when nothing in it belongs to the frame thread:
    /// a query then runs off the frame, and a frame merely collects what came back.
    /// `None` (the default) means the source holds frame state — a running app's own
    /// providers, through component proxies that hold `Rc`s — and is asked on the frame,
    /// one source at a time. A desktop keeps what it built once in an [`Arc`], and
    /// [`Source`] is implemented for one: that is how a source moves to a worker.
    fn shared(&self) -> Option<Arc<dyn Source + Send + Sync>> {
        None
    }
}

/// A source a host keeps between searches (its index built once) is
/// searched through a shared handle: one held in an [`Arc`] can move to a
/// worker, so a worker is handed the handle rather than the source.
impl<S: Source + Send + Sync + 'static> Source for Arc<S> {
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
    fn shared(&self) -> Option<Arc<dyn Source + Send + Sync>> {
        Some(Arc::clone(self) as Arc<dyn Source + Send + Sync>)
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

/// One search, as it fills in: the hits of the sources whose answers came back,
/// merged on the shared ranking, one row per [`Thing`]. The palette asks for another
/// frame's worth with [`resume`](Self::resume) until nothing is
/// [`pending`](Self::pending); the rows it has are always the best answer of what
/// came in.
pub struct SearchStream {
    needle: String,
    row_budget: usize,
    /// Each source the search asks, by its place among the sources it was
    /// collected from, as `(id, how it is asked)`. Two sources may search
    /// the same kind of thing under one id (an app's open providers and the
    /// index of what apps published), and each is asked for its own rows.
    sources: Vec<(String, Asking)>,
    /// The moved sources not asked yet, by their place in `sources`.
    to_ask: Vec<usize>,
    /// The sources that stay on the frame, by their place in `sources`:
    /// asked from the sources the caller hands each frame, one per frame.
    here: Vec<usize>,
    /// The sources whose answer has not come back yet.
    outstanding: usize,
    /// The rows so far, each with its strength on the shared scale.
    merged: Vec<(Hit, f32)>,
    /// Where the row of a thing stands in `merged`, so two sources offering
    /// the same thing give one row, the stronger.
    things: HashMap<Thing, usize>,
    /// The answers the workers sent: what arrived is merged on the frame
    /// that collects it.
    answers: Receiver<Answer>,
    /// Where `answers` are sent from: a worker holds a clone.
    sender: Sender<Answer>,
    /// Which search this stream is, to the pool: a newer one replaces it,
    /// and a query still waiting for a source of an older one is not run.
    generation: u64,
}

/// How a search asks one of its sources.
enum Asking {
    /// It moves to a worker, and this is the handle the worker asks.
    Moved(Arc<dyn Source + Send + Sync>),
    /// It holds the frame's state: asked on the frame, in a frame of its
    /// own, from the sources the caller hands each frame.
    Here,
}

impl SearchStream {
    fn new(
        needle: &str,
        sources: &[&dyn Source],
        row_budget: usize,
        workers: &SearchWorkers,
    ) -> Self {
        let (sender, answers) = channel();
        let bounded = system::query_input_is_bounded(needle);
        // A source that says how it moves is asked on a worker; one that
        // does not, and so holds the frame's state, is asked here.
        let asked: Vec<(String, Asking)> = sources
            .iter()
            .map(|source| match source.shared() {
                Some(moved) => (source.id().to_owned(), Asking::Moved(moved)),
                None => (source.id().to_owned(), Asking::Here),
            })
            .collect();
        let to_ask = if bounded {
            asked
                .iter()
                .enumerate()
                .filter(|(_, (_, asking))| matches!(asking, Asking::Moved(_)))
                .map(|(at, _)| at)
                .collect()
        } else {
            Vec::new()
        };
        let here: Vec<usize> = if bounded {
            asked
                .iter()
                .enumerate()
                .filter(|(_, (_, asking))| matches!(asking, Asking::Here))
                .map(|(at, _)| at)
                .collect()
        } else {
            Vec::new()
        };
        Self {
            needle: needle.to_owned(),
            row_budget,
            sources: asked,
            to_ask,
            here,
            outstanding: 0,
            merged: Vec::new(),
            things: HashMap::new(),
            answers,
            sender,
            generation: workers.begin(),
        }
    }

    /// Whether a source is still to answer.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.to_ask.is_empty() || !self.here.is_empty() || self.outstanding > 0
    }

    /// Whether a source that holds the frame's state is still to ask: the
    /// desk asks one such source per frame, so it owes the frames that ask
    /// them. A source on a worker is not this: it answers on its own, and
    /// wakes the desk when it does.
    #[must_use]
    pub fn awaiting_here(&self) -> bool {
        !self.here.is_empty()
    }

    /// The hits that came in, in the ranking's order, one row per thing.
    #[must_use]
    pub fn rows(&self) -> Vec<Hit> {
        let mut merged = self.kept();
        system::order_hits(&mut merged, |hit| (&hit.kind, hit.availability()));
        merged.into_iter().map(|(hit, _)| hit).collect()
    }

    /// The rows so far, grouped by kind under the search's row budget.
    #[must_use]
    pub fn groups(&self) -> Vec<KindGroup<Hit>> {
        let mut merged = self.kept();
        system::order_hits(&mut merged, |hit| (&hit.kind, hit.availability()));
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

    /// Hand the sources not asked yet to the workers, ask one source that stays on the
    /// frame, and take the answers that came. Never waits for a source: an answer that is
    /// not in yet wakes the desk when it is. `sources` is the slice the search was made
    /// from, and a source that cannot move is asked from it at the end. Returns whether
    /// the rows changed.
    pub fn resume(
        &mut self,
        sources: &[&dyn Source],
        workers: &SearchWorkers,
        used: &dyn Fn(&Hit) -> f32,
    ) -> bool {
        // A new needle replaced this search: the answers still coming are
        // for text nobody shows, and are never merged.
        if workers.retired(self.generation) {
            self.to_ask.clear();
            self.here.clear();
            self.outstanding = 0;
            while self.answers.try_recv().is_ok() {}
            return false;
        }
        let asking = std::mem::take(&mut self.to_ask);
        for index in asking {
            let Some((key, Asking::Moved(moved))) = self.sources.get(index) else {
                continue;
            };
            let query = self.asked(moved.as_ref());
            let moved = Arc::clone(moved);
            self.outstanding = self.outstanding.saturating_add(1);
            workers.ask(
                self.generation,
                index,
                key,
                Box::new(move || moved.query(&query)),
                self.sender.clone(),
            );
        }
        // One source that cannot move is asked first, here on the frame:
        // its answer is the SDK's own bounded page, and the sources that
        // moved are still on their way. Its rows are in the frame that
        // typed.
        let mut changed = false;
        if let Some(index) = self.here.first().copied() {
            self.here.remove(0);
            if let Some(source) = sources.get(index).copied() {
                changed |= self.ask_here(source, used);
            }
        }
        // Every answer that is in, merged: the rows are the best answer of
        // what came back so far, and the ones still out come on the frame
        // their wake draws.
        while let Ok(answer) = self.answers.try_recv() {
            changed |= self.collect(answer, used);
        }
        changed
    }

    /// Take the next answer the workers send, waiting for it when it is not
    /// in yet, and merge it: what a caller with no frame to wake does
    /// ([`search`]), and the one place a search waits on a source.
    fn wait_for_answer(&mut self, used: &dyn Fn(&Hit) -> f32) {
        if let Ok(answer) = self.answers.recv() {
            self.collect(answer, used);
        }
    }

    /// What a source is asked: what was typed, or what was typed without
    /// the words a query never has to match, unless it is asked verbatim.
    fn asked(&self, source: &dyn Source) -> String {
        if source.verbatim() {
            self.needle.clone()
        } else {
            system::without_filler(&self.needle).into_owned()
        }
    }

    /// Ask `source` here, on the frame, and merge its rows.
    fn ask_here(&mut self, source: &dyn Source, used: &dyn Fn(&Hit) -> f32) -> bool {
        let query = self.asked(source);
        let top = source.max_score().max(1);
        let verbatim = source.verbatim();
        let hits = source.query(&query);
        // The source's own reading of each row, read here where the source
        // is reachable.
        let intent: HashMap<String, f32> = hits
            .iter()
            .map(|hit| (hit.id.clone(), source.intent(hit)))
            .collect();
        self.merge(hits, top, verbatim, &intent, used)
    }

    /// Merge one source's answer: its rows ranked on the shared scale, the
    /// stronger answer of a thing keeping its place.
    fn collect(&mut self, answer: Answer, used: &dyn Fn(&Hit) -> f32) -> bool {
        self.outstanding = self.outstanding.saturating_sub(1);
        let Some((_, Asking::Moved(moved))) = self.sources.get(answer.source) else {
            return false;
        };
        let top = moved.max_score().max(1);
        let verbatim = moved.verbatim();
        // The source's own reading of each row, on the frame: a worker asks
        // it, and never calls it.
        let intent: HashMap<String, f32> = answer
            .hits
            .iter()
            .map(|hit| (hit.id.clone(), moved.intent(hit)))
            .collect();
        self.merge(answer.hits, top, verbatim, &intent, used)
    }

    /// The rows `hits` of a source, merged into the search's rows.
    fn merge(
        &mut self,
        hits: Vec<Hit>,
        top: i32,
        verbatim: bool,
        intent: &HashMap<String, f32>,
        used: &dyn Fn(&Hit) -> f32,
    ) -> bool {
        let typed = self.needle.clone();
        let unasked_filler = system::without_filler(&typed);
        let query: &str = if verbatim {
            typed.as_str()
        } else {
            unasked_filler.as_ref()
        };
        for hit in hits {
            let here = intent.get(&hit.id).copied().unwrap_or(0.0);
            let strength = |needle: &str| {
                system::merged_strength(
                    needle,
                    &hit.kind,
                    &hit.title,
                    hit.subtitle.as_deref(),
                    hit.score.min(top),
                    top,
                    here + used(&hit),
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
        true
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
            .field("unasked", &self.to_ask.len())
            .field("outstanding", &self.outstanding)
            .field("rows", &self.merged.len())
            .finish_non_exhaustive()
    }
}

/// One frame of a search: hand the sources to `workers`, take the answers
/// that came back, and merge them, one row per thing ([`Thing`]). The sources
/// still out wake the desk when they answer.
pub fn search_stream(
    needle: &str,
    sources: &[&dyn Source],
    row_budget: usize,
    workers: &SearchWorkers,
    used: &dyn Fn(&Hit) -> f32,
) -> SearchStream {
    let mut stream = SearchStream::new(needle, sources, row_budget, workers);
    stream.resume(sources, workers, used);
    stream
}

/// Fan `needle` out to `sources`, merge their hits on the shared ranking and group
/// them by kind under a shared `row_budget` (zero keeps all). Each hit's strength is
/// raised by `used`: what the person's earlier choices add to it
/// ([`crate::usage::Usage::strength`]).
/// This caller has no frame to keep, so it waits for every source, which is the one
/// place waiting for a source is right (an agent's find, text placed on the canvas).
pub fn search(
    needle: &str,
    sources: &[&dyn Source],
    row_budget: usize,
    used: &dyn Fn(&Hit) -> f32,
) -> Vec<KindGroup<Hit>> {
    let workers = SearchWorkers::new();
    let mut stream = SearchStream::new(needle, sources, row_budget, &workers);
    while stream.pending() {
        stream.resume(sources, &workers, used);
        if !stream.pending() {
            break;
        }
        if stream.outstanding > 0 {
            // With nothing left to ask, only a source still answering is
            // waited for: this caller has no frame the answer could wake.
            stream.wait_for_answer(used);
        }
    }
    stream.groups()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use ui::time::Duration;

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

    /// A place the person was in, whose window is still open: what
    /// Continue offers as the window it comes back to.
    fn place_in_window(title: &str, tab: u64) -> Hit {
        hit(
            "continue",
            &format!("place:{tab}"),
            title,
            crate::providers::places::continue_kind(),
            system::MAX_PROVIDER_SCORE,
            Target::Window {
                tab,
                app: Some("so.tilo.file-explorer".into()),
            },
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
                            place: LaunchPlace::Focus,
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
                            place: LaunchPlace::Focus,
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

    /// The place the person continues from, when that place is a window
    /// still open, is that window: one row, offered by Continue, never the
    /// same window twice.
    #[test]
    fn a_place_that_is_an_open_window_is_that_window() {
        let mut world = world();
        let window = window_row("Home", 9);
        let place = place_in_window("Home", 9);
        world.windows.hits.push(window);
        world.places.hits = vec![place];
        let groups = search("", &world.all(), 0, &|_| 0.0);
        let listed = rows(&groups);
        assert_eq!(
            listed
                .iter()
                .filter(|row| row.ends_with(":Home"))
                .collect::<Vec<_>>(),
            ["continue:Home"],
            "the window is listed once, as where the person continues: {listed:?}"
        );
        assert!(
            !listed.iter().any(|row| row == "desktop-windows:Home"),
            "the window wearing the same name is not listed beside it: {listed:?}"
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
    fn an_app_the_person_has_is_one_row_and_its_store_page_gives_way() {
        // The machine's Notes, as its desktop entry starts it, and the same
        // app as the Store would install it: one app, one row, the one the
        // person already has.
        let had = fixed(
            "desktop-applications",
            BROWSE_RANK_APPS,
            vec![hit(
                "desktop-applications",
                "host:org.gnome.Notes.desktop",
                "Notes",
                ResourceKind::Application,
                100,
                Target::Launch {
                    app: "host:org.gnome.Notes.desktop".into(),
                    place: LaunchPlace::Focus,
                },
            )],
        );
        let store = fixed(
            "store-catalogue",
            BROWSE_RANK_LATER,
            vec![Hit {
                row: None,
                preview: None,
                faces: Vec::new(),
                outcome: Some("In the Store".into()),
                kind: ResourceKind::Custom("Store".into()),
                provider: "store-catalogue".into(),
                id: "app:org.gnome.Notes".into(),
                title: "Notes".into(),
                subtitle: None,
                facts: vec!["Flathub".into()],
                score: 97,
                icon: None,
                target: Target::Open {
                    uri: "tilo-store:app/org.gnome.Notes".into(),
                    fallback: None,
                },
            }],
        );
        assert_eq!(
            rows(&search("notes", &[&had, &store], 0, &|_| 0.0)),
            ["desktop-applications:Notes"],
            "the app is listed once, as the app the person has"
        );
        assert_eq!(
            rows(&search("notes", &[&store], 0, &|_| 0.0)),
            ["store-catalogue:Notes"],
            "an app the person does not have is still offered"
        );
    }

    #[test]
    fn a_store_offer_follows_every_row_the_person_has() {
        // A Flathub page matching the query better than any file named
        // files: it still lists after the file, which is already here.
        let files = fixed(
            "search-files",
            BROWSE_RANK_LATER,
            vec![hit(
                "search-files",
                "file:notes.txt",
                "files.txt",
                ResourceKind::File,
                30,
                Target::Open {
                    uri: "file:///home/q/files.txt".into(),
                    fallback: None,
                },
            )],
        );
        let store = fixed(
            CATALOGUE_SOURCE,
            BROWSE_RANK_LATER,
            vec![hit(
                CATALOGUE_SOURCE,
                "app:com.example.Files",
                "Files",
                ResourceKind::Custom("Store".into()),
                100,
                Target::Open {
                    uri: "tilo-store:app/com.example.Files".into(),
                    fallback: None,
                },
            )],
        );
        assert_eq!(
            rows(&search("files", &[&files, &store], 0, &|_| 0.0)),
            ["search-files:files.txt", "store-catalogue:Files"]
        );
    }

    #[test]
    fn a_catalogue_row_knows_what_the_person_would_have_to_do() {
        let offer = |target| Hit {
            target,
            ..hit(
                CATALOGUE_SOURCE,
                "app:so.tilo.sheets",
                "Sheets",
                ResourceKind::Custom("Store".into()),
                90,
                Target::Open {
                    uri: "tilo-store:app/so.tilo.sheets".into(),
                    fallback: None,
                },
            )
        };
        assert_eq!(
            offer(Target::Launch {
                app: "so.tilo.sheets".into(),
                place: LaunchPlace::Focus,
            })
            .availability(),
            system::Availability::TiloCatalogue,
            "a Tilo app opens at once"
        );
        assert_eq!(
            offer(Target::Open {
                uri: "tilo-store:app/com.example.App".into(),
                fallback: None,
            })
            .availability(),
            system::Availability::ThirdPartyCatalogue,
            "a Flathub app is a Store page first"
        );
        assert_eq!(
            hit(
                "search-files",
                "file:notes.txt",
                "notes",
                ResourceKind::File,
                30,
                Target::Open {
                    uri: "file:///home/q/notes.txt".into(),
                    fallback: None,
                },
            )
            .availability(),
            system::Availability::OnDevice,
            "a row of the person's own is theirs"
        );
    }

    #[test]
    fn an_app_identity_is_one_however_its_rows_start_it() {
        for id in [
            "so.tilo.notes",
            " host:so.tilo.notes.desktop ",
            "SO.TILO.NOTES.desktop",
        ] {
            assert_eq!(app_identity(id), "so.tilo.notes", "{id}");
        }
    }

    #[test]
    fn a_store_link_names_the_app_it_installs() {
        assert_eq!(
            store_page_app("tilo-store:app/org.gnome.Notes"),
            Some("org.gnome.Notes")
        );
        assert_eq!(store_page_app("https://example.com"), None);
        assert_eq!(store_page_app("tilo-store:app/"), None);
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

    /// A source that takes `delay` to answer, as a source reading a large
    /// disk index or a mail server does: it answers what was typed with one
    /// row titled so the answer is recognisable.
    struct Sleepy {
        id: &'static str,
        delay: Duration,
        asked: Arc<Mutex<Vec<String>>>,
        reached: Arc<Mutex<bool>>,
    }

    impl Sleepy {
        fn new(id: &'static str, delay: Duration) -> Self {
            Self {
                id,
                delay,
                asked: Arc::default(),
                reached: Arc::default(),
            }
        }

        fn needles(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }

        fn answered(&self) -> bool {
            *self.reached.lock().unwrap()
        }
    }

    impl Source for Sleepy {
        fn id(&self) -> &str {
            self.id
        }
        fn shared(&self) -> Option<Arc<dyn Source + Send + Sync>> {
            Some(Arc::new(Mirror {
                id: self.id,
                delay: self.delay,
                asked: Arc::clone(&self.asked),
                reached: Arc::clone(&self.reached),
            }))
        }
        fn query(&self, _: &str) -> Vec<Hit> {
            panic!("a source that moves is never asked on the frame")
        }
    }

    /// [`Sleepy`] as the worker sees it: the same answer, worked out off the
    /// frame.
    struct Mirror {
        id: &'static str,
        delay: Duration,
        asked: Arc<Mutex<Vec<String>>>,
        reached: Arc<Mutex<bool>>,
    }

    impl Source for Mirror {
        fn id(&self) -> &str {
            self.id
        }
        fn query(&self, needle: &str) -> Vec<Hit> {
            self.asked.lock().unwrap().push(needle.to_owned());
            std::thread::sleep(self.delay);
            *self.reached.lock().unwrap() = true;
            vec![hit(
                self.id,
                "late",
                &format!("{} answer", self.id),
                ResourceKind::Note,
                100,
                Target::Copy {
                    text: self.id.into(),
                },
            )]
        }
    }

    /// How long a frame may take while a slow source is out: far under the
    /// two seconds a source takes, and generous for a loaded machine.
    const FRAME: Duration = Duration::from_millis(250);

    /// The rows `stream` holds, as `provider:title`.
    fn frame_rows(stream: &SearchStream) -> Vec<String> {
        stream
            .groups()
            .iter()
            .flat_map(|group| group.results.iter())
            .map(|hit| format!("{}:{}", hit.provider, hit.title))
            .collect()
    }

    /// One frame of the stream, and how long it took.
    fn frame(stream: &mut SearchStream, all: &[&dyn Source], workers: &SearchWorkers) -> Duration {
        let started = ui::time::now();
        stream.resume(all, workers, &|_| 0.0);
        ui::time::now().saturating_duration_since(started)
    }

    /// "alacritty" into the search: a source that takes two seconds never
    /// runs on the frame thread. Every frame returns inside the frame
    /// budget while it is out, the fast sources' rows are in the first
    /// frame, and the slow source's row joins when it answers.
    #[test]
    fn a_source_that_takes_seconds_never_holds_up_a_frame() {
        let world = world();
        let slow = Sleepy::new("slow", Duration::from_secs(2));
        let mut all = world.all();
        all.push(&slow);
        let workers = SearchWorkers::default();
        let mut stream = search_stream("notes", &all, 0, &workers, &|_| 0.0);
        // The frame that typed the first character: the fast sources are
        // in, the slow one is out, and this frame did not wait for it.
        let took = frame(&mut stream, &all, &workers);
        // A frame collects the answers that arrived by its deadline, so it
        // may spend the budget waiting: what it never does is wait for a
        // source that takes seconds.
        assert!(
            took < Duration::from_millis(100),
            "the first frame took {took:?}, not the two-second source"
        );
        assert!(
            frame_rows(&stream)
                .iter()
                .any(|row| row == "desktop-applications:Notes"),
            "the fast sources' rows are in the first frame: {:?}",
            frame_rows(&stream)
        );
        assert!(
            !frame_rows(&stream)
                .iter()
                .any(|row| row.starts_with("slow:")),
            "the slow source has not answered yet"
        );
        // Every frame while it is still out returns at once, however long
        // the source takes.
        let mut frames = 0;
        let typing = ui::time::now() + Duration::from_secs(15);
        while !slow.answered() {
            let took = frame(&mut stream, &all, &workers);
            assert!(
                took < FRAME,
                "frame {frames} spent {took:?} on the two-second source"
            );
            frames += 1;
            assert!(
                ui::time::now() < typing,
                "a frame never waits the source out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // Its rows join once it answered.
        let deadline = ui::time::now() + Duration::from_secs(10);
        while stream.pending() && ui::time::now() < deadline {
            frame(&mut stream, &all, &workers);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!stream.pending(), "the slow source answered");
        assert!(
            frame_rows(&stream)
                .iter()
                .any(|row| row == "slow:slow answer"),
            "its row is there once it answered: {:?}",
            frame_rows(&stream)
        );
        assert_eq!(
            slow.needles().last().map(String::as_str),
            Some("notes"),
            "it was asked what was typed, off the frame"
        );
    }

    /// Typing on while a slow source answers: the answer of the needle that
    /// was replaced never shows, and the new needle's answer does.
    #[test]
    fn an_answer_for_a_replaced_needle_is_dropped() {
        let slow = Sleepy::new("slow", Duration::from_millis(400));
        let all: Vec<&dyn Source> = vec![&slow];
        let workers = SearchWorkers::default();
        // "ala" is asked, and the person types on to "alac" before it can
        // answer.
        let mut older = search_stream("ala", &all, 0, &workers, &|_| 0.0);
        frame(&mut older, &all, &workers);
        assert!(
            older.pending(),
            "the first needle is still being worked out"
        );
        let mut newer = search_stream("alac", &all, 0, &workers, &|_| 0.0);
        let deadline = ui::time::now() + Duration::from_secs(10);
        while newer.pending() && ui::time::now() < deadline {
            frame(&mut newer, &all, &workers);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!newer.pending(), "the new needle answers");
        assert_eq!(
            frame_rows(&newer),
            ["slow:slow answer"],
            "the new needle's answer is shown"
        );
        // The search that was replaced never shows the answer it still
        // held: what comes back is for text nobody sees.
        for _ in 0..40 {
            frame(&mut older, &all, &workers);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            frame_rows(&older).is_empty(),
            "the replaced needle's answer never shows: {:?}",
            frame_rows(&older)
        );
        assert_eq!(
            slow.needles().last().map(String::as_str),
            Some("alac"),
            "the source was asked the needle showing last: {:?}",
            slow.needles()
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
            // Every source is asked on a worker: the frame that types only
            // hands them over and collects what already arrived.
            let workers = SearchWorkers::default();
            let started = ui::time::now();
            let _stream = search_stream(&needle, &refs, 12, &workers, &|_| 0.0);
            let took = ui::time::now().saturating_duration_since(started);
            // The frame that typed never holds the rows of the index: the
            // four sources are out on the workers, and the frame returns
            // with whatever had already arrived, usually nothing.
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
        let workers = SearchWorkers::default();
        let mut stream = search_stream("12*7", &sources, 0, &workers, &|_| 0.0);
        // The frames until both answered: an answer that comes back a frame
        // later is still in the frame that shows it, ahead of the web row.
        let deadline = ui::time::now() + Duration::from_secs(10);
        while stream.pending() && ui::time::now() < deadline {
            stream.resume(&sources, &workers, &|_| 0.0);
            std::thread::sleep(Duration::from_millis(1));
        }
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
}
