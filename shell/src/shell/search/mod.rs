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

use std::rc::Rc;

use automerge::{AutoCommit, ObjId};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};

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
    /// An installed application. Enter launches it.
    Application,
    /// A note document (title + body). Enter opens/focuses the note tab.
    Note,
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
            ResourceKind::Note => "Notes",
            ResourceKind::File => "Files",
            ResourceKind::Folder => "Folders",
            ResourceKind::Creator => "Creators",
            ResourceKind::Video => "Videos",
            ResourceKind::Playlist => "Playlists",
            ResourceKind::Custom(s) => s.as_str(),
        }
    }

    /// Stable sort key that pins the well-known kinds to a fixed order in
    /// the palette (Actions → Applications → Notes → Files → Folders →
    /// Creator → Video → Playlist → Custom(alphabetical)). Two providers
    /// registering the same kind still group together because they share
    /// this key.
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
            ResourceKind::Note => (2, String::new()),
            ResourceKind::File => (3, String::new()),
            ResourceKind::Folder => (4, String::new()),
            ResourceKind::Creator => (5, String::new()),
            ResourceKind::Video => (6, String::new()),
            ResourceKind::Playlist => (7, String::new()),
            ResourceKind::Custom(s) => (8, s.clone()),
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
pub type OpenFn = dyn Fn(&mut AutoCommit, &ObjId, &Chrome);

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
    pub on_open: Rc<OpenFn>,
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
        Self { providers: Vec::new() }
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
        let mut merged: Vec<SearchResult> = Vec::new();
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
            let results = provider.query(ctx);
            merged.extend(results);
        }
        // Sort: kind sort_key ascending, then score descending, then
        // title ascending. Stable so providers registered earlier retain
        // order on ties.
        merged.sort_by(|a, b| {
            a.kind
                .sort_key()
                .cmp(&b.kind.sort_key())
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.title.cmp(&b.title))
        });
        merged
    }
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
    }

    fn mk_result(kind: ResourceKind, provider: &str, id: &str, title: &str, score: i32) -> SearchResult {
        SearchResult {
            kind,
            provider_id: provider.to_string(),
            id: id.to_string(),
            title: title.to_string(),
            subtitle: None,
            score,
            on_open: Rc::new(|_, _, _| {}),
        }
    }

    fn mk_chrome() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle =
            StdArc::new(RwLock::new(WorkspaceTree::default()));
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
        }));
        reg.register(Rc::new(FakeProvider {
            id: "actions".into(),
            kinds: vec![ResourceKind::Action],
            results: vec![mk_result(ResourceKind::Action, "actions", "a1", "act", 30)],
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
        // Actions is sort_key 0 so it comes first regardless of score.
        assert_eq!(out[0].kind, ResourceKind::Action);
        // Within Notes: higher score first.
        assert_eq!(out[1].id, "n2");
        assert_eq!(out[2].id, "n1");
    }

    #[test]
    fn active_app_scope_narrows_to_matching_provider() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "notes".into(),
            kinds: vec![ResourceKind::Note],
            results: vec![mk_result(ResourceKind::Note, "notes", "n1", "n", 10)],
        }));
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: vec![mk_result(ResourceKind::File, "files", "f1", "f", 10)],
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

    #[test]
    fn registered_kinds_is_deduped_and_ordered() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "a".into(),
            kinds: vec![ResourceKind::File, ResourceKind::Note],
            results: vec![],
        }));
        reg.register(Rc::new(FakeProvider {
            id: "b".into(),
            kinds: vec![ResourceKind::Note, ResourceKind::Action],
            results: vec![],
        }));
        let kinds = reg.registered_kinds();
        assert_eq!(
            kinds,
            vec![
                ResourceKind::Action,
                ResourceKind::Note,
                ResourceKind::File,
            ]
        );
    }
}
