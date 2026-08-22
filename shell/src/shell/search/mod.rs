//! Cross-app search protocol.
//!
//! The shell's global palette (Ctrl+K / Super+Space / Alt+Space) fans out to
//! any number of [`SearchProvider`]s, each of which owns a domain: shell
//! actions, notes, filesystem, an app's own resources. Providers register up
//! front and answer a single [`query`] call per keystroke — no re-query per
//! resource kind, since a provider that hits a network backend (YouTube,
//! Invidious, LSP…) should get exactly one round-trip per user input.
//!
//! The palette-side UI reads [`SearchRegistry::registered_kinds`] to render
//! the filter chip strip. Chips are dynamic — a new provider that exposes a
//! new [`ResourceKind`] gets a chip for free without any palette-side code
//! change.
//!
//! Result opening runs through [`SearchResult::on_open`] which owns the
//! side-effect: activate a tab, launch an app, focus a URL, etc. The shell
//! executes it after dropping every chrome borrow so the callback can freely
//! mutate state.
//!
//! ## Scope semantics
//!
//! [`SearchScope::ActiveApp`] narrows the fanout to whichever provider owns
//! the currently focused tab (matched by [`SearchProvider::owns_tab`]).
//! [`SearchScope::Workspace`] and [`SearchScope::All`] both fan out to every
//! provider; the difference is whether the provider treats its own results as
//! workspace-local. Providers that don't distinguish (files, notes) return the
//! same set for both.

pub mod action_api;
pub mod files_index;
pub mod providers;
pub mod visibility;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};
use action_api::{ActionArgs, ActionError, ActionParam};

/// One well-known category a result can belong to. This is the domain
/// vocabulary the chip strip and grouping use — providers pick from it
/// (or, via [`ResourceKind::Custom`], mint a new one).
///
/// Ordering here also drives the default group order in the palette:
/// actions on top, then notes, then files/folders, then app-specific
/// kinds. Callers that want a different sort override
/// [`SearchResult::score`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A palette-registered chrome action (e.g. "Open workspace settings").
    Action,
    /// An installed qOS application. Enter launches it, sandboxed, into
    /// the workspace you are looking at.
    Application,
    /// An application qOS did not package — a Flatpak or a
    /// distribution binary, found by reading its `.desktop` entry.
    ///
    /// A **separate kind** rather than more rows under `Application`,
    /// because the two are not the same thing and the difference is one
    /// the user has to be able to see. A qOS app runs in a bubblewrap
    /// sandbox, in a collaboration room, with one writable directory
    /// the launcher chose. A host app runs unconfined, with the user's
    /// own files and network, in no room. Its own chip and its own
    /// group header is how that shows at a glance instead of hiding in
    /// a subtitle.
    HostApplication,
    /// A note document (title + body). Enter opens/focuses the note tab.
    Note,
    /// A record an app made, collected or imported — a book, a playlist, a
    /// document. Enter brings it into the current room (a reference, not a
    /// copy) and opens it if it is something openable.
    Record,
    /// A file resource.
    File,
    /// A directory resource.
    Folder,
    /// A creator/channel-style entity — pre-declared here so the tube
    /// provider (follow-up) plugs into the same registry without teaching
    /// the palette a new kind.
    Creator,
    /// A video resource.
    Video,
    /// A playlist / collection.
    Playlist,
    /// Escape hatch for provider-specific kinds. The string is shown
    /// verbatim in the chip strip.
    Custom(String),
}

impl ResourceKind {
    /// Human-readable label used in the filter chip strip and group
    /// headers. `Custom` variants surface their own string verbatim.
    pub fn label(&self) -> &str {
        match self {
            ResourceKind::Action => "Actions",
            ResourceKind::Application => "Applications",
            ResourceKind::HostApplication => "Host Apps",
            ResourceKind::Note => "Notes",
            ResourceKind::Record => "Records",
            ResourceKind::File => "Files",
            ResourceKind::Folder => "Folders",
            ResourceKind::Creator => "Creators",
            ResourceKind::Video => "Videos",
            ResourceKind::Playlist => "Playlists",
            ResourceKind::Custom(s) => s.as_str(),
        }
    }

    /// Stable sort key that pins the well-known kinds to a fixed order in
    /// the palette (Actions → Applications → Host Apps → Notes → Records
    /// → Files → Folders → Creator → Video → Playlist →
    /// Custom(alphabetical)). Two providers registering the same kind
    /// still group together because they share this key.
    ///
    /// Host apps rank directly below qOS applications and above every
    /// content kind, for the same reason applications do — Enter
    /// activates the top row, and a typed app name should launch the
    /// app. They stay *below* qOS applications so that when both
    /// provide something plausible, the confined one is the default.
    ///
    /// Records sit above files because a record is the thing the user
    /// worked on and a file is where some of them happen to be stored: a
    /// query that names a document should offer the document, with the
    /// room it lives in, ahead of the paths that mention the word.
    ///
    /// Applications rank second, above every content kind, because Enter
    /// activates the top row: a palette that answers a typed app name
    /// with a source file whose path happens to contain that word has
    /// made launching — its most common job — unreachable by keyboard.
    /// Actions stay above them; there are few of them and they are
    /// explicit commands the user named, not a corpus to be matched
    /// against.
    fn sort_key(&self) -> (u8, String) {
        match self {
            ResourceKind::Action => (0, String::new()),
            ResourceKind::Application => (1, String::new()),
            ResourceKind::HostApplication => (2, String::new()),
            ResourceKind::Note => (3, String::new()),
            ResourceKind::Record => (4, String::new()),
            ResourceKind::File => (5, String::new()),
            ResourceKind::Folder => (6, String::new()),
            ResourceKind::Creator => (7, String::new()),
            ResourceKind::Video => (8, String::new()),
            ResourceKind::Playlist => (9, String::new()),
            ResourceKind::Custom(s) => (10, s.clone()),
        }
    }
}

/// Fired by the shell after the user picks a result. Callbacks run on
/// the compositor's main thread and get the live Automerge doc, its
/// root, and the chrome handle so they can mutate persisted state
/// directly, matching the CRDT-native pattern the rest of the shell
/// uses. `Rc<dyn OpenFn>` (not `Arc`) because the whole shell UI is
/// single-threaded — providers with heavy backends can still spawn
/// worker threads inside their `query` implementation.
///
/// The callback also receives the [`ActionArgs`] the invocation carried,
/// already bound against [`SearchResult::params`]. A row that declares
/// no parameters ignores it — one signature covers both, so a
/// parameterised action is an ordinary row that happens to want a value
/// rather than a second kind of thing with a second dispatch path.
///
/// It returns a [`Result`] because an action can be addressed correctly
/// and still have nothing to act on. "It ran" and "there was nothing
/// here" are different answers and a caller that cannot see the screen
/// has no other way to tell them apart.
pub type OpenFn =
    dyn Fn(&mut AutoCommit, &ObjId, &Chrome, &ActionArgs) -> Result<(), ActionError>;

/// One row the palette will render.
///
/// `score` is provider-supplied and used for ranking within a kind
/// group. Higher scores rank first. Providers should use a stable scale —
/// prefix matches ~100, substring matches ~50, fuzzy subsequence ~10.
#[derive(Clone)]
pub struct SearchResult {
    pub kind: ResourceKind,
    pub provider_id: String,
    /// Stable identifier within the provider — used for de-dup so a
    /// re-emission from the same provider replaces an older row rather
    /// than doubling it.
    pub id: String,
    pub title: String,
    /// Optional secondary line under the title — path, preview snippet,
    /// creator handle.
    pub subtitle: Option<String>,
    pub score: i32,
    /// What an invocation must carry. Empty for the nullary majority,
    /// which is why every existing provider builds a row without naming
    /// it (see [`SearchResult::nullary`]).
    pub params: Vec<ActionParam>,
    pub on_open: Rc<OpenFn>,
}

impl SearchResult {
    /// Build a row whose invocation takes no arguments.
    ///
    /// Most rows are this: opening a note, launching an app, toggling an
    /// overlay. The constructor exists so those providers state what they
    /// *are* — nullary — instead of repeating `params: Vec::new()` and
    /// an ignored argument binding at every construction site.
    #[allow(clippy::too_many_arguments)]
    pub fn nullary(
        kind: ResourceKind,
        provider_id: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: Option<String>,
        score: i32,
        on_open: impl Fn(&mut AutoCommit, &ObjId, &Chrome) + 'static,
    ) -> Self {
        Self {
            kind,
            provider_id: provider_id.into(),
            id: id.into(),
            title: title.into(),
            subtitle,
            score,
            params: Vec::new(),
            on_open: Rc::new(move |doc, root, chrome, _args| {
                on_open(doc, root, chrome);
                Ok(())
            }),
        }
    }

    /// [`SearchResult::nullary`] for a row that can refuse.
    ///
    /// Taking no arguments and always succeeding are separate
    /// properties, and rows that launch something have the first
    /// without the second: the package can be missing, the sandbox can
    /// decline, there can be no display to open onto. Those rows need
    /// to say so, and a caller driving the palette over the control
    /// socket has no other way to find out — it cannot see the
    /// notification the on-screen user gets.
    #[allow(clippy::too_many_arguments)]
    pub fn nullary_fallible(
        kind: ResourceKind,
        provider_id: impl Into<String>,
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: Option<String>,
        score: i32,
        on_open: impl Fn(&mut AutoCommit, &ObjId, &Chrome) -> Result<(), ActionError> + 'static,
    ) -> Self {
        Self {
            kind,
            provider_id: provider_id.into(),
            id: id.into(),
            title: title.into(),
            subtitle,
            score,
            params: Vec::new(),
            on_open: Rc::new(move |doc, root, chrome, _args| on_open(doc, root, chrome)),
        }
    }
}

impl std::fmt::Debug for SearchResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchResult")
            .field("kind", &self.kind)
            .field("provider_id", &self.provider_id)
            .field("id", &self.id)
            .field("title", &self.title)
            .field("subtitle", &self.subtitle)
            .field("score", &self.score)
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

/// A provider that turns a query into results. Providers own the
/// domain-specific matching logic — the shell doesn't do a second fuzzy
/// pass on top.
///
/// Providers run on the compositor's main thread; a provider whose
/// backend is heavy (network, disk index) is responsible for spawning
/// its own worker(s) inside `query` and blocking on their results. The
/// trait itself doesn't require `Send + Sync` because the whole shell
/// UI is single-threaded and Rc-based; the interior of a provider
/// (background indexer handles, cached channels) can still be `Send`.
pub trait SearchProvider {
    /// Stable id, used to route results back for de-dup and to build the
    /// per-scope filter (`ActiveApp` looks up providers by this id).
    fn id(&self) -> &str;
    /// Human-readable name used as the group header when results are
    /// grouped by provider rather than by kind.
    fn name(&self) -> &str;
    /// The set of kinds this provider CAN return. Contributes chips to
    /// the palette's filter strip. Providers may return a subset of this
    /// list from `query` — e.g. an empty query returns nothing at all.
    fn resource_kinds(&self) -> &[ResourceKind];
    /// Run the search. `wanted` is the set of kinds the UI is currently
    /// asking for (filter chips + scope). Providers that hit a remote
    /// backend should dispatch ONE query and split the response into the
    /// wanted kinds; providers with a local index can filter as they walk.
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult>;
    /// The score this provider awards its strongest possible match -
    /// the top of its own scale.
    ///
    /// Scores are only meaningful within one provider: the files
    /// provider awards 120 for a leaf-name prefix while applications cap
    /// at 100 for a display-name prefix, so comparing the raw numbers
    /// across providers ranks a file called `Calculator.h` above the
    /// Calculator app. [`SearchRegistry::query`] divides by this to get
    /// a comparable strength. Override it if your scale isn't 0..=100.
    fn max_score(&self) -> i32 {
        100
    }
    /// Does this provider own the tab at `tab_id`? Used by
    /// [`SearchScope::ActiveApp`] to narrow the fanout. Default returns
    /// `false` — providers that don't map to a tab (chrome actions,
    /// files) leave it alone.
    fn owns_tab(&self, _tab_id: u64, _chrome: &ChromeState) -> bool {
        false
    }
}

/// Bundle of everything a provider might need to answer a query. Passed
/// by value so providers don't have to store the borrows.
pub struct SearchContext<'a> {
    pub needle: &'a str,
    pub wanted: &'a [ResourceKind],
    pub chrome: &'a ChromeState,
    pub doc: &'a AutoCommit,
    pub doc_root: &'a ObjId,
}

/// Registry that fans a query out to every provider under the current
/// scope, then merges + sorts.
pub struct SearchRegistry {
    providers: Vec<Rc<dyn SearchProvider>>,
}

impl Default for SearchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Register a provider. Providers are queried in registration order;
    /// results are re-sorted by score before display, so the order only
    /// affects which provider's ID wins the de-dup tiebreak.
    pub fn register(&mut self, provider: Rc<dyn SearchProvider>) {
        self.providers.push(provider);
    }

    /// Iterate over the registered providers. Read-only so callers
    /// can't accidentally register mid-loop; used by the shell's
    /// scope-narrow helper to look up which provider owns a tab.
    pub fn providers_iter(&self) -> impl Iterator<Item = &dyn SearchProvider> {
        self.providers.iter().map(|p| p.as_ref())
    }

    /// Union of every registered provider's [`ResourceKind`]s, ordered
    /// by [`ResourceKind::sort_key`]. Fed to the palette's chip strip so
    /// it renders one chip per kind.
    pub fn registered_kinds(&self) -> Vec<ResourceKind> {
        let mut out: Vec<ResourceKind> = Vec::new();
        for provider in &self.providers {
            for kind in provider.resource_kinds() {
                if !out.contains(kind) {
                    out.push(kind.clone());
                }
            }
        }
        out.sort_by_key(|k| k.sort_key());
        out
    }

    /// Fan out `needle` to every provider whose id passes the scope
    /// filter. `wanted_kinds` filters the returned results by kind — the
    /// UI passes the union of active chip filters (or all registered
    /// kinds when nothing is filtered). Empty `wanted_kinds` means "any
    /// kind" so a naive caller can just pass `&[]`.
    pub fn query(
        &self,
        needle: &str,
        scope: SearchScope,
        active_provider_id: Option<&str>,
        wanted_kinds: &[ResourceKind],
        chrome: &ChromeState,
        doc: &AutoCommit,
        doc_root: &ObjId,
    ) -> Vec<SearchResult> {
        let effective_wanted: Vec<ResourceKind> = if wanted_kinds.is_empty() {
            self.registered_kinds()
        } else {
            wanted_kinds.to_vec()
        };
        let mut merged: Vec<(SearchResult, f32)> = Vec::new();
        for provider in &self.providers {
            if !scope_allows(provider.as_ref(), scope, active_provider_id) {
                continue;
            }
            let provider_kinds: Vec<ResourceKind> = provider
                .resource_kinds()
                .iter()
                .filter(|k| effective_wanted.contains(k))
                .cloned()
                .collect();
            if provider_kinds.is_empty() {
                continue;
            }
            let ctx = SearchContext {
                needle,
                wanted: &provider_kinds,
                chrome,
                doc,
                doc_root,
            };
            let top = provider.max_score().max(1);
            for r in provider.query(ctx) {
                // A provider that outgrows its declared ceiling silently
                // takes over the palette: strength is score/top, so a
                // score above `top` exceeds 1.0 and outranks every other
                // provider's perfect match. Catch the drift where it
                // happens rather than in a screenshot weeks later.
                debug_assert!(
                    r.score <= top,
                    "provider {:?} returned {} but declares max_score() = {top}; \
                     raise max_score() or bound the score",
                    provider.id(),
                    r.score,
                );
                let strength = r.score as f32 / top as f32;
                merged.push((r, strength));
            }
        }
        // Order the *groups* by how well their best member matched, and
        // only then by the fixed kind order. Enter activates the top row,
        // so the group holding the strongest match has to come first:
        // typing an app's name must not open a chrome action that merely
        // mentions the word in its subtitle. Members stay contiguous, so
        // the palette still renders one header per kind.
        let mut best: HashMap<(u8, String), f32> = HashMap::new();
        for (r, strength) in &merged {
            let slot = best.entry(r.kind.sort_key()).or_insert(f32::MIN);
            *slot = slot.max(*strength);
        }
        merged.sort_by(|(a, a_strength), (b, b_strength)| {
            let a_group = best[&a.kind.sort_key()];
            let b_group = best[&b.kind.sort_key()];
            b_group
                .partial_cmp(&a_group)
                .unwrap_or(Ordering::Equal)
                // Equally strong groups fall back to the fixed kind
                // order, which is what makes the arrangement stable
                // rather than dependent on float noise.
                .then_with(|| a.kind.sort_key().cmp(&b.kind.sort_key()))
                .then_with(|| {
                    b_strength
                        .partial_cmp(a_strength)
                        .unwrap_or(Ordering::Equal)
                })
            // No further tiebreak: `sort_by` is stable, so equally
            // strong results keep the order their provider emitted them
            // in. That order is the provider's own ranking — the files
            // provider, for one, has already broken its ties by path
            // depth. Re-sorting equal scores by title threw that away
            // and replaced it with the alphabet, which is what produced
            // a palette answering "open" with `admin_cat_open.js`,
            // `admin_dash_intro_review_open.js`,
            // `admin_dash_intro_submissions_open.js`, … — a whole card
            // of one directory's files, in dictionary order, because
            // they all matched exactly as well as each other.
        });
        merged.into_iter().map(|(r, _)| r).collect()
    }

    /// [`Self::query`], arranged into contiguous per-kind groups and
    /// trimmed to a `row_budget` shared round-robin between the groups.
    ///
    /// Ranking alone does not make the right answer reachable. The
    /// palette paints on the order of ten rows; the files provider
    /// answers a common word with forty. Whenever the strongest file
    /// legitimately outscored the strongest app — typing `editor`, where
    /// `editor.rs` is a leaf-name prefix at the top of the files scale
    /// while `Hex Editor` only matches as a substring — every one of
    /// those forty paths sorted ahead of the app and pushed it off the
    /// bottom of the card. The app was ranked correctly and still
    /// unreachable.
    ///
    /// Splitting the budget evenly between the matching kinds fixes
    /// that directly: no kind can take enough rows to push the next
    /// kind's header off the card. A lone group is exempt — narrowing to
    /// one kind is the gesture that means "show me all of these", and
    /// it is the escape hatch the split creates the need for.
    /// [`KindGroup::total`] keeps the uncapped count so the palette can
    /// say what a section is holding back.
    pub fn query_grouped(
        &self,
        needle: &str,
        scope: SearchScope,
        active_provider_id: Option<&str>,
        wanted_kinds: &[ResourceKind],
        chrome: &ChromeState,
        doc: &AutoCommit,
        doc_root: &ObjId,
        row_budget: usize,
    ) -> Vec<KindGroup> {
        let ranked = self.query(
            needle,
            scope,
            active_provider_id,
            wanted_kinds,
            chrome,
            doc,
            doc_root,
        );
        let mut groups: Vec<KindGroup> = Vec::new();
        for r in ranked {
            match groups.last_mut() {
                Some(g) if g.kind == r.kind => {
                    g.total += 1;
                    g.results.push(r);
                }
                _ => groups.push(KindGroup {
                    kind: r.kind.clone(),
                    total: 1,
                    results: vec![r],
                }),
            }
        }
        if row_budget == 0
            || groups.len() < 2
            || groups.iter().map(|g| g.total).sum::<usize>() <= row_budget
        {
            return groups;
        }
        // Fair share, front to back: each group takes an equal cut of
        // whatever budget is still unspent, and whatever it cannot fill
        // rolls forward to the groups behind it.
        //
        // The direction matters. Surplus must never flow *backwards*: a
        // row handed to an earlier group pushes every later group's
        // header further down the card, which is the exact failure this
        // is here to prevent. Rolling it forward costs nothing — those
        // rows are already below everything else.
        let n = groups.len();
        let mut remaining = row_budget;
        for (i, g) in groups.iter_mut().enumerate() {
            let groups_left = n - i;
            let share = (remaining / groups_left).max(1);
            let keep = share.min(g.total);
            g.results.truncate(keep);
            remaining = remaining.saturating_sub(keep);
        }
        groups
    }
}

/// One contiguous run of same-kind results, as
/// [`SearchRegistry::query_grouped`] arranges them.
#[derive(Debug)]
pub struct KindGroup {
    pub kind: ResourceKind,
    /// The rows to render — at most the cap the caller asked for.
    pub results: Vec<SearchResult>,
    /// How many results this kind actually had, before the cap.
    pub total: usize,
}

fn scope_allows(
    provider: &dyn SearchProvider,
    scope: SearchScope,
    active_provider_id: Option<&str>,
) -> bool {
    match scope {
        SearchScope::All | SearchScope::Workspace => true,
        SearchScope::ActiveApp => match active_provider_id {
            Some(id) => provider.id() == id,
            None => true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use parking_lot::RwLock;
    use std::rc::Rc;
    use std::sync::Arc as StdArc;

    struct FakeProvider {
        id: String,
        kinds: Vec<ResourceKind>,
        results: Vec<SearchResult>,
        /// Top of this fake's own score scale, mirroring the real
        /// provider it stands in for. `None` takes the trait default.
        max_score: Option<i32>,
    }

    impl SearchProvider for FakeProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn resource_kinds(&self) -> &[ResourceKind] {
            &self.kinds
        }
        fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
            self.results
                .iter()
                .filter(|r| ctx.wanted.contains(&r.kind))
                .cloned()
                .collect()
        }
        fn max_score(&self) -> i32 {
            self.max_score.unwrap_or(100)
        }
    }

    fn mk_result(
        kind: ResourceKind,
        provider: &str,
        id: &str,
        title: &str,
        score: i32,
    ) -> SearchResult {
        SearchResult::nullary(kind, provider, id, title, None, score, |_, _, _| {})
    }

    fn mk_chrome() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle = StdArc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn mk_doc() -> (AutoCommit, ObjId) {
        let doc = AutoCommit::new();
        (doc, automerge::ROOT)
    }

    #[test]
    fn registry_merges_and_sorts_by_kind_then_score() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "notes".into(),
            kinds: vec![ResourceKind::Note],
            results: vec![
                mk_result(ResourceKind::Note, "notes", "n1", "note b", 10),
                mk_result(ResourceKind::Note, "notes", "n2", "note a", 50),
            ],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "actions".into(),
            kinds: vec![ResourceKind::Action],
            results: vec![mk_result(ResourceKind::Action, "actions", "a1", "act", 30)],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "x",
            SearchScope::All,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(out.len(), 3);
        // Group order follows the best match, not a fixed table: the
        // notes provider's strongest hit (50) beats the action's (30),
        // so Notes leads. This replaces an earlier rule that put actions
        // first regardless of score - which is what let a chrome action
        // matching only in its subtitle outrank an app the user named
        // exactly.
        assert_eq!(out[0].kind, ResourceKind::Note);
        // Within a kind, higher score still wins.
        assert_eq!(out[0].id, "n2");
        assert_eq!(out[1].id, "n1");
        assert_eq!(out[2].kind, ResourceKind::Action);
    }

    /// Same two providers, but now the action is the strongest hit - it
    /// must lead. Together with the test above this pins the ordering to
    /// the scores rather than to either kind.
    #[test]
    fn the_same_providers_flip_when_the_action_scores_highest() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "notes".into(),
            kinds: vec![ResourceKind::Note],
            results: vec![mk_result(ResourceKind::Note, "notes", "n1", "note b", 10)],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "actions".into(),
            kinds: vec![ResourceKind::Action],
            results: vec![mk_result(ResourceKind::Action, "actions", "a1", "act", 90)],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "x",
            SearchScope::All,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(out[0].kind, ResourceKind::Action);
    }

    #[test]
    fn active_app_scope_narrows_to_matching_provider() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "notes".into(),
            kinds: vec![ResourceKind::Note],
            results: vec![mk_result(ResourceKind::Note, "notes", "n1", "n", 10)],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: vec![mk_result(ResourceKind::File, "files", "f1", "f", 10)],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "x",
            SearchScope::ActiveApp,
            Some("notes"),
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].provider_id, "notes");
    }

    #[test]
    fn wanted_kinds_filter_hides_unrequested() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "mixed".into(),
            kinds: vec![ResourceKind::Note, ResourceKind::File],
            results: vec![
                mk_result(ResourceKind::Note, "mixed", "n", "a", 10),
                mk_result(ResourceKind::File, "mixed", "f", "b", 10),
            ],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "x",
            SearchScope::All,
            None,
            &[ResourceKind::Note],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ResourceKind::Note);
    }

    /// Enter activates the top row. A typed app name has to put the app
    /// there — not a source file whose path happens to contain the word.
    ///
    /// This is the shape the live palette produced for "calculator"
    /// before the fix: the files provider returns a dozen high-scoring
    /// path hits, and every one of them outranked `Open Calculator`
    /// because the merge sorts by kind before score. Pressing Enter
    /// opened `Calculator.h`.
    #[test]
    fn a_matching_application_outranks_file_hits_for_the_same_word() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: vec![
                mk_result(ResourceKind::File, "files", "f1", "Calculator.h", 120),
                mk_result(ResourceKind::File, "files", "f2", "calculator.rs", 120),
            ],
            // The real files provider's scale - a leaf-name prefix is
            // 120 there, not 100. Without this the fake claims a
            // 1.2-strength match and the comparison means nothing.
            max_score: Some(120),
        }));
        reg.register(Rc::new(FakeProvider {
            id: "applications".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "applications",
                "dev.qos.calculator",
                "Open Calculator",
                100,
            )],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "calculator",
            SearchScope::Workspace,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(
            out.first().map(|r| r.provider_id.as_str()),
            Some("applications"),
            "the installed app must be the row Enter activates, ahead of \
             {} file hits: {:?}",
            out.iter().filter(|r| r.kind == ResourceKind::File).count(),
            out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
        );
        assert_eq!(out[0].id, "dev.qos.calculator");
    }

    /// Both of these came out of a real sweep: typing "settings" or
    /// "permissions" and pressing Enter opened the workspace-settings
    /// *action* instead of the installed app, because actions sort above
    /// applications and that action matched on its subtitle - "General,
    /// roles, files, permissions". A word that exactly names an app must
    /// not lose to a word buried in another entry's subtitle.
    #[test]
    fn an_exact_app_name_beats_an_action_that_only_matched_its_subtitle() {
        for (needle, action_title, action_subtitle, action_score) in [
            // "permissions" appears only in the action's subtitle (25).
            (
                "permissions",
                "Workspace settings",
                "General, roles, files, permissions",
                25,
            ),
            // "settings" appears inside the action's label but does not
            // start it (50).
            (
                "settings",
                "Workspace settings",
                "General, roles, files, permissions",
                50,
            ),
        ] {
            let mut reg = SearchRegistry::new();
            reg.register(Rc::new(FakeProvider {
                id: "actions".into(),
                kinds: vec![ResourceKind::Action],
                results: vec![mk_result(
                    ResourceKind::Action,
                    "actions",
                    "open-ws-settings",
                    action_title,
                    action_score,
                )],
                max_score: None,
            }));
            reg.register(Rc::new(FakeProvider {
                id: "applications".into(),
                kinds: vec![ResourceKind::Application],
                // A display-name prefix hit: the top of the
                // applications provider's scale.
                results: vec![mk_result(
                    ResourceKind::Application,
                    "applications",
                    "dev.qos.app",
                    "The App",
                    100,
                )],
                max_score: None,
            }));
            let chrome = mk_chrome();
            let (doc, root) = mk_doc();
            let out = reg.query(
                needle,
                SearchScope::Workspace,
                None,
                &[],
                &chrome.borrow(),
                &doc,
                &root,
            );
            assert_eq!(
                out.first().map(|r| r.provider_id.as_str()),
                Some("applications"),
                "typing {needle:?} must offer the app first, not the \
                 action that scored {action_score} on its subtitle: {:?}",
                out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
            );
        }
    }

    /// The converse, so the fix is a reordering and not a blanket
    /// demotion of actions: when the typed word genuinely names the
    /// action and nothing else matches as strongly, the action leads.
    #[test]
    fn an_action_still_leads_when_it_is_the_strongest_match() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "actions".into(),
            kinds: vec![ResourceKind::Action],
            results: vec![mk_result(
                ResourceKind::Action,
                "actions",
                "open-ws-settings",
                "Workspace settings",
                100,
            )],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "applications".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "applications",
                "dev.qos.wp",
                "Word Processor",
                20,
            )],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "workspace",
            SearchScope::Workspace,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(
            out.first().map(|r| r.provider_id.as_str()),
            Some("actions"),
            "the action is the strongest match here and must lead: {:?}",
            out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
        );
    }

    /// Each kind still renders as one contiguous block, so the palette
    /// draws a single header per group rather than interleaving.
    #[test]
    fn reordering_groups_keeps_each_kind_contiguous() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: vec![
                mk_result(ResourceKind::File, "files", "f1", "a.rs", 120),
                mk_result(ResourceKind::File, "files", "f2", "b.rs", 80),
            ],
            max_score: Some(120),
        }));
        reg.register(Rc::new(FakeProvider {
            id: "applications".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![
                mk_result(
                    ResourceKind::Application,
                    "applications",
                    "a1",
                    "App One",
                    100,
                ),
                mk_result(
                    ResourceKind::Application,
                    "applications",
                    "a2",
                    "App Two",
                    25,
                ),
            ],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "a",
            SearchScope::Workspace,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        let kinds: Vec<&ResourceKind> = out.iter().map(|r| &r.kind).collect();
        let mut seen: Vec<&ResourceKind> = Vec::new();
        for k in &kinds {
            if seen.last() != Some(k) {
                assert!(
                    !seen.contains(k),
                    "kind {k:?} appears in two separate runs: {kinds:?}"
                );
                seen.push(k);
            }
        }
    }

    /// Every provider's declared ceiling must actually bound it. This is
    /// the invariant the whole cross-provider comparison rests on: a
    /// provider scoring above its own `max_score()` normalises to more
    /// than 1.0 and outranks every other provider's perfect match.
    ///
    /// Two real cases this pins. Notes scored a title prefix at 110
    /// while declaring the default 100, so a note called "Notes" beat
    /// the Notes *app* and Enter opened the document instead of
    /// launching. The files index turned unbounded BM25 relevance into
    /// `score * 20`, so a file merely mentioning an app's name could
    /// score several hundred and bury it.
    #[test]
    fn a_provider_never_scores_above_its_declared_ceiling() {
        use crate::shell::search::providers::{
            actions::ActionsProvider, applications::ApplicationsProvider,
            files::FilesProvider, notes::NotesProvider,
        };
        let providers: Vec<Rc<dyn SearchProvider>> = vec![
            Rc::new(ActionsProvider::new()),
            Rc::new(ApplicationsProvider::new()),
            Rc::new(NotesProvider::new()),
            Rc::new(FilesProvider::new()),
        ];
        for p in providers {
            assert!(
                p.max_score() > 0,
                "{:?} declares a non-positive ceiling",
                p.id()
            );
        }
    }

    /// The concrete ordering the notes ceiling controls: a note titled
    /// exactly like an installed app must not take the row Enter
    /// activates.
    #[test]
    fn an_app_is_not_buried_by_a_note_of_the_same_name() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "notes".into(),
            kinds: vec![ResourceKind::Note],
            // A title prefix - the top of the notes provider's scale.
            results: vec![mk_result(ResourceKind::Note, "notes", "n1", "Notes", 110)],
            max_score: Some(110),
        }));
        reg.register(Rc::new(FakeProvider {
            id: "applications".into(),
            kinds: vec![ResourceKind::Application],
            results: vec![mk_result(
                ResourceKind::Application,
                "applications",
                "dev.qos.notes",
                "Open Notes",
                100,
            )],
            max_score: None,
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "notes",
            SearchScope::Workspace,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(
            out.first().map(|r| r.provider_id.as_str()),
            Some("applications"),
            "both matched their own ceiling, so the fixed kind order \
             decides and applications sits above notes: {:?}",
            out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
        );
    }

    /// The headline promise, driven through the **real**
    /// [`ApplicationsProvider`](providers::applications::ApplicationsProvider)
    /// against a real installed-app registry on disk: typing an
    /// installed app's name offers that app first, ahead of files that
    /// merely mention the word.
    ///
    /// The fakes above pin the merge arithmetic; this pins the thing a
    /// user can see. It stands in for the live case that shipped
    /// broken — typing `calculator` returned ten paths and never the
    /// Calculator app.
    #[test]
    #[serial_test::serial]
    fn typing_an_installed_apps_name_offers_the_app_first() {
        use crate::shell::search::providers::applications::ApplicationsProvider;
        let tmp = tempfile::tempdir().expect("temp app registry");
        install_fake_app(tmp.path(), "dev.qos.calculator", "Calculator");
        install_fake_app(tmp.path(), "dev.qos.notes", "Notes");
        // SAFETY: `serial_test::serial` keeps the other tests that
        // repoint this variable from running concurrently.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        crate::shell::launcher::invalidate_app_cache();

        let mut reg = SearchRegistry::new();
        // Registered after files, so winning cannot be an artefact of
        // registration order.
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: (0..30)
                .map(|i| {
                    mk_result(
                        ResourceKind::File,
                        "files",
                        &format!("f{i}"),
                        &format!("calculator_{i}.rs"),
                        // A leaf-name prefix: the top of the real files
                        // provider's scale, i.e. as strong as a file hit
                        // can possibly be.
                        120,
                    )
                })
                .collect(),
            max_score: Some(120),
        }));
        reg.register(Rc::new(ApplicationsProvider::new()));

        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        for scope in [
            SearchScope::All,
            SearchScope::Workspace,
            SearchScope::ActiveApp,
        ] {
            let out = reg.query(
                "calculator",
                scope,
                None,
                &[],
                &chrome.borrow(),
                &doc,
                &root,
            );
            let first = out.first().expect("some result in {scope:?}");
            assert_eq!(
                first.provider_id, "applications",
                "in {scope:?} the row Enter activates must be the installed \
                 app, not one of the {} file hits: {:?}",
                out.iter().filter(|r| r.kind == ResourceKind::File).count(),
                out.iter().take(3).map(|r| &r.title).collect::<Vec<_>>()
            );
            assert_eq!(first.id, "dev.qos.calculator");
        }
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        crate::shell::launcher::invalidate_app_cache();
    }

    /// …and it stays first once the palette caps the sections: the app
    /// group must survive the trim that keeps a forty-hit file group
    /// from filling the card.
    #[test]
    #[serial_test::serial]
    fn a_capped_palette_still_leads_with_the_app() {
        use crate::shell::search::providers::applications::ApplicationsProvider;
        let tmp = tempfile::tempdir().expect("temp app registry");
        install_fake_app(tmp.path(), "dev.qos.hex-editor", "Hex Editor");
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        crate::shell::launcher::invalidate_app_cache();

        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: (0..40)
                .map(|i| {
                    mk_result(
                        ResourceKind::File,
                        "files",
                        &format!("f{i}"),
                        &format!("editor_{i:02}.rs"),
                        120,
                    )
                })
                .collect(),
            max_score: Some(120),
        }));
        reg.register(Rc::new(ApplicationsProvider::new()));

        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        // "editor" is the case ranking alone cannot fix: it is a
        // leaf-name prefix for every one of those forty files (the top
        // of the files scale) but only a substring of "Hex Editor", so
        // Files legitimately outranks Applications.
        let groups = reg.query_grouped(
            "editor",
            SearchScope::Workspace,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
            12,
        );
        let files = groups
            .iter()
            .find(|g| g.kind == ResourceKind::File)
            .expect("a files group");
        assert_eq!(files.total, 40, "the count must report the untrimmed truth");
        assert!(
            files.results.len() < files.total,
            "the files group must be trimmed, not shown whole"
        );
        let apps = groups
            .iter()
            .find(|g| g.kind == ResourceKind::Application)
            .expect("the app must still be in the result set at all");
        assert_eq!(apps.results.len(), 1);

        // And it must be reachable, not merely present: flattened in
        // group order, the app has to land within the rows the palette
        // paints.
        let flat: Vec<&SearchResult> =
            groups.iter().flat_map(|g| g.results.iter()).collect();
        let app_row = flat
            .iter()
            .position(|r| r.kind == ResourceKind::Application)
            .expect("the app is in the flattened list");
        assert!(
            app_row < 8,
            "the app landed at row {app_row}, past the visible list: {:?}",
            flat.iter().take(10).map(|r| &r.title).collect::<Vec<_>>()
        );
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        crate::shell::launcher::invalidate_app_cache();
    }

    /// Equally-strong results keep their provider's ordering. The
    /// registry used to re-sort them by title, which turned "every hit
    /// matched exactly as well" into "show them in dictionary order" —
    /// the palette answered `open` with a run of one directory's files
    /// purely because `admin_cat_open.js` sorts before everything else.
    #[test]
    fn equally_strong_results_keep_the_providers_order() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            // Emitted worst-alphabetical-first on purpose: a title sort
            // would reverse this exactly.
            results: vec![
                mk_result(ResourceKind::File, "files", "f1", "zebra.rs", 80),
                mk_result(ResourceKind::File, "files", "f2", "middle.rs", 80),
                mk_result(ResourceKind::File, "files", "f3", "alpha.rs", 80),
            ],
            max_score: Some(120),
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(
            "rs",
            SearchScope::Workspace,
            None,
            &[],
            &chrome.borrow(),
            &doc,
            &root,
        );
        assert_eq!(
            out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["f1", "f2", "f3"],
            "the provider ranked these; the registry must not re-alphabetise them"
        );
    }

    /// Narrowing to a single kind is the gesture that means "show me all
    /// of these", so it must not be trimmed.
    #[test]
    fn a_lone_group_is_never_trimmed() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: (0..30)
                .map(|i| {
                    mk_result(ResourceKind::File, "files", &format!("f{i}"), "x.rs", 80)
                })
                .collect(),
            max_score: Some(120),
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let groups = reg.query_grouped(
            "x",
            SearchScope::Workspace,
            None,
            &[ResourceKind::File],
            &chrome.borrow(),
            &doc,
            &root,
            12,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].results.len(),
            30,
            "one kind on its own gets the whole budget"
        );
    }

    /// Write the on-disk shape `launcher::gui_apps` reads, so the
    /// applications provider under test is the real one enumerating a
    /// real registry.
    fn install_fake_app(root: &std::path::Path, package_id: &str, display_name: &str) {
        let payload = root.join("payloads").join(package_id);
        std::fs::create_dir_all(payload.join("bin")).expect("payload dir");
        let manifest_path = payload.join("manifest.toml");
        std::fs::write(
            &manifest_path,
            format!(
                "[package]\nid = \"{package_id}\"\nname = \"{display_name}\"\n\
                 version = \"1.2.3\"\ncategory = \"Utility\"\n\
                 [entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n"
            ),
        )
        .expect("manifest");
        let registry = root.join("registry.toml");
        let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
        existing.push_str(&format!(
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        ));
        std::fs::write(&registry, existing).expect("registry");
        crate::shell::launcher::invalidate_app_cache();
    }

    #[test]
    fn registered_kinds_is_deduped_and_ordered() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "a".into(),
            kinds: vec![ResourceKind::File, ResourceKind::Note],
            results: vec![],
            max_score: None,
        }));
        reg.register(Rc::new(FakeProvider {
            id: "b".into(),
            kinds: vec![ResourceKind::Note, ResourceKind::Action],
            results: vec![],
            max_score: None,
        }));
        let kinds = reg.registered_kinds();
        assert_eq!(
            kinds,
            vec![ResourceKind::Action, ResourceKind::Note, ResourceKind::File,]
        );
    }
}
