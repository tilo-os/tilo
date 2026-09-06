//! Cross-app search protocol.
//!
//! The shell's global palette fans out to any number of [`SearchProvider`]s,
//! each owning a domain (shell actions, notes, filesystem, an app's own
//! resources). Providers answer a single [`query`] call per keystroke — no
//! re-query per resource kind, so a network-backed provider gets exactly one
//! round-trip per input. [`SearchScope::ActiveApp`] narrows the fanout to
//! whichever provider owns the focused tab ([`SearchProvider::owns_tab`]);
//! `Workspace` and `All` fan out to every provider.

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

/// One well-known category a result can belong to — the domain vocabulary
/// the chip strip and grouping use. Providers pick from it or, via
/// [`ResourceKind::Custom`], mint a new one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A palette-registered chrome action (e.g. "Open workspace settings").
    Action,
    /// An installed qOS application. Enter launches it, sandboxed, into
    /// the workspace you are looking at.
    Application,
    /// An app qOS did not package (Flatpak, distro binary). Its own kind
    /// because a qOS app runs sandboxed in a room; a host app runs unconfined.
    HostApplication,
    /// A note document (title + body). Enter opens/focuses the note tab.
    Note,
    /// A record an app made, collected or imported. Enter brings it into
    /// the current room (a reference, not a copy) and opens it if openable.
    Record,
    File,
    Folder,
    /// A creator/channel-style entity — pre-declared so a future provider
    /// plugs into the same registry without teaching the palette a new kind.
    Creator,
    Video,
    Playlist,
    /// Escape hatch for provider-specific kinds, shown verbatim in the chip strip.
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

    /// Stable sort key pinning the well-known kinds to a fixed palette
    /// order. Applications rank above content kinds because Enter
    /// activates the top row — a typed app name must launch the app, not
    /// surface a source file that merely contains the word.
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

/// Fired by the shell after the user picks a result, with the live
/// Automerge doc, its root, and the chrome handle to mutate persisted state
/// directly. Returns a [`Result`] because an action can be addressed
/// correctly and still have nothing to act on — "it ran" and "there was
/// nothing here" are different answers a headless caller must tell apart.
pub type OpenFn = dyn Fn(&mut AutoCommit, &ObjId, &Chrome, &ActionArgs) -> Result<(), ActionError>;
pub type InvokeFn =
    dyn Fn(&mut AutoCommit, &ObjId, &Chrome, &ActionArgs) -> Result<ActionReceipt, ActionError>;
/// A non-blocking admission poll. `None` means the broker has not answered
/// yet; the compositor keeps the ticket and polls again next frame.
pub type PendingInvocation = Rc<dyn Fn() -> Result<Option<ActionReceipt>, ActionError>>;
pub enum InvocationDisposition {
    Accepted(ActionReceipt),
    Pending(PendingInvocation),
}
pub type BeginInvokeFn = dyn Fn(
    &mut AutoCommit,
    &ObjId,
    &Chrome,
    &ActionArgs,
) -> Result<InvocationDisposition, ActionError>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActionReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_invocation_id: Option<u64>,
    pub update: qos_actions::InvocationUpdate,
}

impl ActionReceipt {
    pub fn completed() -> Self {
        Self {
            broker_invocation_id: None,
            update: qos_actions::InvocationUpdate::Succeeded {
                invocation: qos_actions::InvocationId::new(),
                output: qos_actions::ActionOutput::default(),
            },
        }
    }
}

/// One row the palette will render. `score` is provider-supplied, higher
/// ranks first; a stable scale is expected — prefix matches ~100,
/// substring matches ~50, fuzzy subsequence ~10.
#[derive(Clone)]
pub struct SearchResult {
    pub kind: ResourceKind,
    pub provider_id: String,
    /// Stable identifier within the provider, used for de-dup.
    pub id: String,
    pub title: String,
    /// Optional secondary line under the title — path, preview snippet,
    /// creator handle.
    pub subtitle: Option<String>,
    pub score: i32,
    /// What an invocation must carry; empty for the nullary majority (see
    /// [`SearchResult::nullary`]).
    pub params: Vec<ActionParam>,
    /// Full operational semantics when the provider represents a canonical
    /// qOS action. Ordinary content/open rows leave this absent.
    pub contract: Option<qos_actions::ActionContract>,
    pub on_open: Rc<OpenFn>,
    pub on_invoke: Option<Rc<InvokeFn>>,
    /// Used only when admission itself is asynchronous. It must not claim an
    /// accepted action until its retained ticket resolves current authority.
    pub on_begin_invoke: Option<Rc<BeginInvokeFn>>,
}

/// Hard ceilings for one provider page.  These are deliberately expressed in
/// the protocol rather than as palette constants: the same limits protect the
/// visual palette, headless agents, and a future remote directory source.
/// A provider may stop sooner, but must never exceed any non-zero ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchBudget {
    pub max_results: usize,
    pub max_bytes: usize,
    pub max_work: usize,
}

impl SearchBudget {
    pub const PALETTE: Self = Self {
        max_results: 48,
        max_bytes: 48 * 1024,
        max_work: 4_096,
    };
}

/// An opaque provider-owned continuation.  The revision is carried alongside
/// the provider-local payload, so a result page can never silently advance
/// across a changed ordering.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchCursor {
    pub revision: String,
    pub token: String,
}

/// One bounded answer from a [`SearchProvider`].  `stale_cursor` is a
/// protocol outcome rather than an empty page: callers restart explicitly,
/// preserving the distinction between "there are no more hits" and "the
/// source changed while you were paging".
#[derive(Clone)]
pub struct SearchPage {
    pub revision: String,
    pub results: Vec<SearchResult>,
    pub next_cursor: Option<SearchCursor>,
    pub exhausted: bool,
    pub stale_cursor: bool,
    pub budget_exhausted: bool,
}

impl std::fmt::Debug for SearchPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchPage")
            .field("revision", &self.revision)
            .field("results", &self.results)
            .field("next_cursor", &self.next_cursor)
            .field("exhausted", &self.exhausted)
            .field("stale_cursor", &self.stale_cursor)
            .field("budget_exhausted", &self.budget_exhausted)
            .finish()
    }
}

impl SearchPage {
    pub fn exhausted(revision: impl Into<String>, results: Vec<SearchResult>) -> Self {
        Self {
            revision: revision.into(),
            results,
            next_cursor: None,
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        }
    }

    pub fn stale(revision: impl Into<String>) -> Self {
        Self {
            revision: revision.into(),
            results: Vec::new(),
            next_cursor: None,
            exhausted: false,
            stale_cursor: true,
            budget_exhausted: false,
        }
    }
}

impl SearchResult {
    /// Build a row whose invocation takes no arguments — most rows: opening
    /// a note, launching an app, toggling an overlay.
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
            contract: None,
            on_open: Rc::new(move |doc, root, chrome, _args| {
                on_open(doc, root, chrome);
                Ok(())
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }

    /// [`SearchResult::nullary`] for a row that can refuse — a launch can
    /// still fail (missing package, declined sandbox, no display), and the
    /// return value is how a control-socket caller learns that.
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
            contract: None,
            on_open: Rc::new(move |doc, root, chrome, _args| on_open(doc, root, chrome)),
            on_invoke: None,
            on_begin_invoke: None,
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
            .field("contract", &self.contract)
            .field("has_invoke_receipt", &self.on_invoke.is_some())
            .field("has_pending_invoke", &self.on_begin_invoke.is_some())
            .finish_non_exhaustive()
    }
}

/// A provider that turns a query into results, owning its own
/// domain-specific matching logic — a heavy backend (network, disk index)
/// spawns its own worker(s) inside `query`.
pub trait SearchProvider {
    /// Stable id, used to route results back for de-dup and to build the
    /// per-scope filter (`ActiveApp` looks up providers by this id).
    fn id(&self) -> &str;
    /// Group header when results are grouped by provider rather than kind.
    fn name(&self) -> &str;
    /// The set of kinds this provider CAN return; contributes chips to
    /// the palette's filter strip.
    fn resource_kinds(&self) -> &[ResourceKind];
    /// Run the search. `wanted` is the set of kinds currently asked for
    /// (filter chips + scope); a remote-backed provider should dispatch
    /// ONE query and split the response into the wanted kinds.
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult>;

    /// Version of the retained index used by [`Self::query_page`].  A
    /// provider whose data is immutable may keep the default.  Mutable
    /// providers must change it whenever their ordering or disclosure can
    /// change; cursors are then rejected instead of silently skipping rows.
    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        "static".into()
    }

    /// Page a retained search index.  This is the canonical shell query
    /// surface.  `query` remains for small, direct callers and backwards
    /// compatible provider tests; registry/palette/agent discovery always
    /// call this method.
    ///
    /// The conservative default is intentionally only suitable for tiny,
    /// static providers. Keeping the fallback lets third-party providers
    /// upgrade without an ABI-breaking trait change; shell providers that
    /// can grow must override it with retained-index paging.
    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let revision = self.search_revision(&ctx);
        if cursor.is_some_and(|cursor| cursor.revision != revision || !cursor.token.is_empty()) {
            return SearchPage::stale(revision);
        }
        let mut results = self.query(ctx);
        let total = results.len();
        let emitted = total.min(budget.max_results);
        results.truncate(emitted);
        let exhausted = emitted == total;
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision: revision.clone(),
                token: "legacy-truncated".into(),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
    /// Top of this provider's own score scale. Scores are only meaningful
    /// within one provider — files award 120 for a leaf-name prefix while
    /// applications cap at 100, so raw numbers would rank `Calculator.h`
    /// above the Calculator app. [`SearchRegistry::query`] divides by this.
    fn max_score(&self) -> i32 {
        100
    }
    /// Does this provider own the tab at `tab_id`? Used by
    /// [`SearchScope::ActiveApp`] to narrow the fanout.
    fn owns_tab(&self, _tab_id: u64, _chrome: &ChromeState) -> bool {
        false
    }
    /// Keep this provider available while the palette is narrowed to the
    /// focused app. Shell-level commands are still useful in that scope: a
    /// person should not have to switch search scopes just to open Activity,
    /// Settings, or another piece of workspace chrome.
    fn available_in_active_app_scope(&self) -> bool {
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

/// Arguments for [`SearchRegistry::query`]/[`SearchRegistry::query_grouped`]
/// — distinct from [`SearchContext`], which is the narrower, per-provider
/// view built fresh for each provider inside the fan-out loop.
pub struct QueryArgs<'a> {
    pub needle: &'a str,
    pub scope: SearchScope,
    pub active_provider_id: Option<&'a str>,
    pub wanted_kinds: &'a [ResourceKind],
    pub chrome: &'a ChromeState,
    pub doc: &'a AutoCommit,
    pub doc_root: &'a ObjId,
}

/// A [`SearchResult::score`]/[`SearchProvider::max_score`] value as a
/// fraction of `[0.0, 1.0]`-scale `f32`. Provider score scales are
/// small (files top out at 120, apps at 100 — see `max_score`'s doc),
/// well under `f32`'s 24-bit mantissa, so the precision this cast could
/// otherwise lose cannot happen for a real provider.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider score scales are small integers (<=120 by convention); exact precision isn't needed for a 0..1 strength ratio"
)]
fn i32_to_f32_lossy(n: i32) -> f32 {
    n as f32
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

    /// Results are re-sorted by score before display, so registration
    /// order only affects which provider's ID wins the de-dup tiebreak.
    pub fn register(&mut self, provider: Rc<dyn SearchProvider>) {
        self.providers.push(provider);
    }

    /// Used by the shell's scope-narrow helper to look up which provider owns a tab.
    pub fn providers_iter(&self) -> impl Iterator<Item = &dyn SearchProvider> {
        self.providers.iter().map(std::convert::AsRef::as_ref)
    }

    /// Page one named source without flattening every provider into a single
    /// transient vector.  This is the agent-facing primitive behind
    /// hierarchy/navigation UIs: callers retain the opaque cursor and repeat
    /// only the provider they are currently exploring.
    pub fn query_provider_page(
        &self,
        provider_id: &str,
        args: &QueryArgs<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> Option<SearchPage> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id() == provider_id)?;
        if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
            return None;
        }
        let wanted = if args.wanted_kinds.is_empty() {
            provider.resource_kinds().to_vec()
        } else {
            provider
                .resource_kinds()
                .iter()
                .filter(|kind| args.wanted_kinds.contains(kind))
                .cloned()
                .collect()
        };
        if wanted.is_empty() {
            return Some(SearchPage::exhausted("not-wanted", Vec::new()));
        }
        Some(provider.query_page(
            SearchContext {
                needle: args.needle,
                wanted: &wanted,
                chrome: args.chrome,
                doc: args.doc,
                doc_root: args.doc_root,
            },
            cursor,
            budget,
        ))
    }

    /// Union of every registered provider's [`ResourceKind`]s, ordered by
    /// [`ResourceKind::sort_key`] — one chip per kind in the palette.
    pub fn registered_kinds(&self) -> Vec<ResourceKind> {
        let mut out: Vec<ResourceKind> = Vec::new();
        for provider in &self.providers {
            for kind in provider.resource_kinds() {
                if !out.contains(kind) {
                    out.push(kind.clone());
                }
            }
        }
        out.sort_by_key(ResourceKind::sort_key);
        out
    }

    /// Snapshot the revisions of exactly the providers a query can reach.
    /// Overlay refresh uses this together with its text field, so live app
    /// resources appearing/disappearing update an open palette without
    /// making the user type a throwaway character.
    pub fn query_revision(&self, args: &QueryArgs<'_>) -> String {
        let mut out = String::new();
        for provider in &self.providers {
            if !scope_allows(provider.as_ref(), args.scope, args.active_provider_id) {
                continue;
            }
            out.push_str(provider.id());
            out.push('\0');
            out.push_str(&provider.search_revision(&SearchContext {
                needle: args.needle,
                wanted: args.wanted_kinds,
                chrome: args.chrome,
                doc: args.doc,
                doc_root: args.doc_root,
            }));
            out.push('\0');
        }
        out
    }

    /// Fan out `needle` to every provider whose id passes the scope
    /// filter. `wanted_kinds` filters results by kind; empty means "any
    /// kind" so a naive caller can just pass `&[]`.
    pub fn query(&self, args: &QueryArgs<'_>) -> Vec<SearchResult> {
        let QueryArgs {
            needle,
            scope,
            active_provider_id,
            wanted_kinds,
            chrome,
            doc,
            doc_root,
        } = *args;
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
            let page = provider.query_page(ctx, None, SearchBudget::PALETTE);
            // A source changed between the call's routing snapshot and its
            // index read.  Treat it as unavailable for this keystroke rather
            // than presenting a misleading empty continuation; the next
            // input/poll retries from the fresh revision.
            if page.stale_cursor {
                continue;
            }
            for r in page.results {
                // A score above `top` exceeds strength 1.0 and silently
                // outranks every other provider's perfect match.
                debug_assert!(
                    r.score <= top,
                    "provider {:?} returned {} but declares max_score() = {top}; \
                     raise max_score() or bound the score",
                    provider.id(),
                    r.score,
                );
                let strength = i32_to_f32_lossy(r.score) / i32_to_f32_lossy(top);
                merged.push((r, strength));
            }
        }
        // Order groups by how well their best member matched, then the
        // fixed kind order: Enter activates the top row, so an app's name
        // must not open an action that merely mentions the word.
        let mut best: HashMap<(u8, String), f32> = HashMap::new();
        for (r, strength) in &merged {
            let slot = best.entry(r.kind.sort_key()).or_insert(f32::MIN);
            *slot = slot.max(*strength);
        }
        merged.sort_by(|(a, a_strength), (b, b_strength)| {
            // Every key looked up here was inserted by the loop above
            // (it walks the same `merged`), but `.get()` + a matching
            // fallback avoids an avoidable panic in a sort comparator
            // rather than relying on that invariant holding forever.
            let a_group = best.get(&a.kind.sort_key()).copied().unwrap_or(f32::MIN);
            let b_group = best.get(&b.kind.sort_key()).copied().unwrap_or(f32::MIN);
            b_group
                .partial_cmp(&a_group)
                .unwrap_or(Ordering::Equal)
                // Equally strong groups fall back to the fixed kind order.
                .then_with(|| a.kind.sort_key().cmp(&b.kind.sort_key()))
                .then_with(|| {
                    b_strength
                        .partial_cmp(a_strength)
                        .unwrap_or(Ordering::Equal)
                })
            // No further tiebreak: `sort_by` is stable, so equally strong
            // results keep the provider's own order rather than being
            // re-alphabetised by title.
        });
        merged.into_iter().map(|(r, _)| r).collect()
    }

    /// [`Self::query`], arranged into contiguous per-kind groups and
    /// trimmed to a `row_budget` shared round-robin: the palette paints
    /// ~10 rows, and an outscoring kind (forty file hits) can otherwise
    /// push a correctly-ranked app off the bottom. A lone group is exempt
    /// — narrowing to one kind means "show me all of these".
    pub fn query_grouped(&self, args: &QueryArgs<'_>, row_budget: usize) -> Vec<KindGroup> {
        let ranked = self.query(args);
        let mut groups: Vec<KindGroup> = Vec::new();
        for r in ranked {
            match groups.last_mut() {
                Some(g) if g.kind == r.kind => {
                    g.total = g.total.saturating_add(1);
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
        // whatever budget remains, and what it can't fill rolls forward
        // (never backward, or later headers push further down the card).
        let n = groups.len();
        let mut remaining = row_budget;
        for (i, g) in groups.iter_mut().enumerate() {
            let groups_left = n.saturating_sub(i).max(1);
            let share = remaining.checked_div(groups_left).unwrap_or(1).max(1);
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
            Some(id) => provider.id() == id || provider.available_in_active_app_scope(),
            None => true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use parking_lot::RwLock;
    use std::fmt::Write as _;
    use std::rc::Rc;
    use std::sync::Arc as StdArc;

    struct FakeProvider {
        id: String,
        kinds: Vec<ResourceKind>,
        results: Vec<SearchResult>,
        /// Top of this fake's own score scale. `None` takes the trait default.
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
        let out = reg.query(&QueryArgs {
            needle: "x",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
        assert_eq!(out.len(), 3);
        // Notes' strongest hit (50) beats the action's (30).
        assert_eq!(out[0].kind, ResourceKind::Note);
        assert_eq!(out[0].id, "n2");
        assert_eq!(out[1].id, "n1");
        assert_eq!(out[2].kind, ResourceKind::Action);
    }

    /// Same two providers, but now the action is the strongest hit and must lead.
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
        let out = reg.query(&QueryArgs {
            needle: "x",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
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
        let out = reg.query(&QueryArgs {
            needle: "x",
            scope: SearchScope::ActiveApp,
            active_provider_id: Some("notes"),
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].provider_id, "notes");
    }

    #[test]
    fn active_app_scope_keeps_shell_actions_available() {
        use crate::shell::search::providers::actions::ActionsProvider;

        assert!(scope_allows(
            &ActionsProvider::new(),
            SearchScope::ActiveApp,
            Some("notes")
        ));
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
        let out = reg.query(&QueryArgs {
            needle: "x",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[ResourceKind::Note],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ResourceKind::Note);
    }

    /// A typed app name must put the app in the row Enter activates, not a
    /// source file whose path happens to contain the word.
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
        let out = reg.query(&QueryArgs {
            needle: "calculator",
            scope: SearchScope::Workspace,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
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

    /// A word that exactly names an app must not lose to a word buried in
    /// another entry's subtitle, even though actions sort above applications.
    #[test]
    fn an_exact_app_name_beats_an_action_that_only_matched_its_subtitle() {
        for (needle, action_title, _action_subtitle, action_score) in [
            // "permissions" appears only in the action's subtitle (25).
            (
                "permissions",
                "Workspace settings",
                "General, roles, files, permissions",
                25,
            ),
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
            let out = reg.query(&QueryArgs {
                needle,
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            assert_eq!(
                out.first().map(|r| r.provider_id.as_str()),
                Some("applications"),
                "typing {needle:?} must offer the app first, not the \
                 action that scored {action_score} on its subtitle: {:?}",
                out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
            );
        }
    }

    /// The converse: this is a reordering, not a blanket demotion of actions.
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
        let out = reg.query(&QueryArgs {
            needle: "workspace",
            scope: SearchScope::Workspace,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
        assert_eq!(
            out.first().map(|r| r.provider_id.as_str()),
            Some("actions"),
            "the action is the strongest match here and must lead: {:?}",
            out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
        );
    }

    /// Each kind renders as one contiguous block, not interleaved.
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
        let out = reg.query(&QueryArgs {
            needle: "a",
            scope: SearchScope::Workspace,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
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

    /// Every provider's declared ceiling must actually bound it — the
    /// invariant the whole cross-provider comparison rests on.
    #[test]
    fn a_provider_never_scores_above_its_declared_ceiling() {
        use crate::shell::search::providers::{
            actions::ActionsProvider, applications::ApplicationsProvider, files::FilesProvider,
            notes::NotesProvider,
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

    /// A note titled exactly like an installed app must not take the row
    /// Enter activates.
    #[test]
    fn an_app_is_not_buried_by_a_note_of_the_same_name() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "notes".into(),
            kinds: vec![ResourceKind::Note],
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
        let out = reg.query(&QueryArgs {
            needle: "notes",
            scope: SearchScope::Workspace,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
        assert_eq!(
            out.first().map(|r| r.provider_id.as_str()),
            Some("applications"),
            "both matched their own ceiling, so the fixed kind order \
             decides and applications sits above notes: {:?}",
            out.iter().map(|r| (&r.kind, &r.title)).collect::<Vec<_>>()
        );
    }

    /// Driven through the **real** applications provider against a real
    /// installed-app registry on disk, not the fakes above.
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
        // Registered after files, so winning cannot be an artefact of order.
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
            let out = reg.query(&QueryArgs {
                needle: "calculator",
                scope,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            });
            let first = out
                .first()
                .unwrap_or_else(|| panic!("some result in {scope:?}"));
            assert_eq!(
                first.provider_id,
                "applications",
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

    /// The app group must survive the trim that keeps a forty-hit file
    /// group from filling the card.
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
        // "editor" is a leaf-name prefix for all forty files but only a
        // substring of "Hex Editor", so Files legitimately outranks it.
        let groups = reg.query_grouped(
            &QueryArgs {
                needle: "editor",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            },
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

        // Must be reachable, not merely present: within the flattened rows.
        let flat: Vec<&SearchResult> = groups.iter().flat_map(|g| g.results.iter()).collect();
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

    /// Equally-strong results keep their provider's ordering, not re-sorted by title.
    #[test]
    fn equally_strong_results_keep_the_providers_order() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            // Emitted worst-alphabetical-first: a title sort would reverse this.
            results: vec![
                mk_result(ResourceKind::File, "files", "f1", "zebra.rs", 80),
                mk_result(ResourceKind::File, "files", "f2", "middle.rs", 80),
                mk_result(ResourceKind::File, "files", "f3", "alpha.rs", 80),
            ],
            max_score: Some(120),
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let out = reg.query(&QueryArgs {
            needle: "rs",
            scope: SearchScope::Workspace,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &root,
        });
        assert_eq!(
            out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["f1", "f2", "f3"],
            "the provider ranked these; the registry must not re-alphabetise them"
        );
    }

    /// Narrowing to a single kind means "show me all of these" and must not be trimmed.
    #[test]
    fn a_lone_group_is_never_trimmed() {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(FakeProvider {
            id: "files".into(),
            kinds: vec![ResourceKind::File],
            results: (0..30)
                .map(|i| mk_result(ResourceKind::File, "files", &format!("f{i}"), "x.rs", 80))
                .collect(),
            max_score: Some(120),
        }));
        let chrome = mk_chrome();
        let (doc, root) = mk_doc();
        let groups = reg.query_grouped(
            &QueryArgs {
                needle: "x",
                scope: SearchScope::Workspace,
                active_provider_id: None,
                wanted_kinds: &[ResourceKind::File],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &root,
            },
            12,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].results.len(),
            30,
            "one kind on its own gets the whole budget"
        );
    }

    /// Write the on-disk shape `launcher::gui_apps` reads.
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
        write!(
            existing,
            "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest_path.display().to_string()
        )
        .expect("write to String cannot fail");
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
