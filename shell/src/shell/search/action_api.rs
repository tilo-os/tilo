//! Headless action API — a programmatic, render-free facade over the
//! [`SearchRegistry`].
//!
//! The Ctrl+K palette is one consumer of the registry: it *renders* the
//! results as rows and fires a callback on Enter. This module is the
//! other consumer — an automation client (an LLM navigator, an
//! integration test, a script) that wants to drive the exact same
//! providers with **no window and no scene**. That separation is the
//! whole point: an action is not a pixel. The same
//! [`crate::shell::search::SearchProvider`]s that feed the palette
//! export their resources here as typed, addressable [`ExportedAction`]s.
//!
//! The flow is three explicit steps, mirroring how a caller reasons:
//!
//! 1. **enumerate** — [`ActionCatalog::enumerate`] answers "what can I
//!    do right now, given the focused app?" Each entry is a typed
//!    descriptor with a stable [`ResourceRef`] — no callback is exposed,
//!    so "what can I do" never accidentally *does* anything.
//! 2. **select** — [`ActionCatalog::select`] resolves a [`ResourceRef`]
//!    back to the live result, or `None` if it isn't in this snapshot
//!    (or the caller isn't allowed to see it).
//! 3. **invoke** — [`SelectedAction::invoke`] runs the provider's
//!    side-effect against the live CRDT + chrome. This is byte-for-byte
//!    the callback the palette would fire on Enter, minus the palette.
//!
//! ## Permission-awareness
//!
//! [`ActionCatalog::build`] takes an [`ActionVisibility`] gate. Full
//! role/capability enforcement lives in the permission subsystem; this
//! is the seam that lets a restricted navigator only enumerate — and
//! only select — what it is allowed to see. The gate is applied at build
//! time so a hidden resource is invisible to *both* `enumerate` and
//! `select`: you cannot invoke what you were never shown.

use automerge::{AutoCommit, ObjId};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};

use super::{ResourceKind, SearchRegistry, SearchResult};

/// Stable, typed address of one exported action or resource: the
/// `(provider, id)` pair every [`SearchResult`] already carries.
///
/// Unlike a rendered palette row (which is positional and vanishes when
/// the query changes), a `ResourceRef` is durable — it keeps addressing
/// the same thing for as long as the owning provider keeps emitting that
/// id. This is what makes the API navigable by something that can't see
/// the screen.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceRef {
    /// The [`SearchProvider::id`](super::SearchProvider::id) that owns it.
    pub provider: String,
    /// The provider-local stable id (`settings.open`, a note's object id,
    /// an absolute file path, …).
    pub id: String,
}

impl ResourceRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self { provider: provider.into(), id: id.into() }
    }
}

/// A typed, render-free descriptor of one thing the focused app (or the
/// shell chrome) exports.
///
/// Deliberately carries **no** invoke callback — enumeration answers
/// "what is available", and invoking is a separate, explicit step routed
/// through [`ActionCatalog::select`]. That keeps a read-only survey
/// (which an LLM navigator does constantly) free of any risk of
/// triggering a side-effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: ResourceRef,
    pub kind: ResourceKind,
    pub title: String,
    pub subtitle: Option<String>,
    /// Provider-supplied rank within its kind. Higher sorts first; the
    /// catalog preserves the registry's merge order so a caller that
    /// just wants "the best match" can take the first entry of a kind.
    pub score: i32,
}

/// Permission hook: decides whether a caller may *see* a given resource.
///
/// This is intentionally minimal — it does not know about roles or ops.
/// The permission subsystem ([`ui::permissions`]) owns the real policy;
/// a caller wires that in by implementing this trait (or passing a
/// closure) that consults a `PermissionContext`. Leaving the seam here
/// means the headless API is permission-aware by construction even
/// before full enforcement lands.
pub trait ActionVisibility {
    /// Return `true` if the caller is allowed to see (and therefore
    /// select + invoke) the resource of `kind` addressed by `resource`.
    fn can_see(&self, resource: &ResourceRef, kind: &ResourceKind) -> bool;
}

/// Any `Fn(&ResourceRef, &ResourceKind) -> bool` is a visibility gate —
/// the ergonomic path for ad-hoc filters and tests.
impl<F> ActionVisibility for F
where
    F: Fn(&ResourceRef, &ResourceKind) -> bool,
{
    fn can_see(&self, resource: &ResourceRef, kind: &ResourceKind) -> bool {
        self(resource, kind)
    }
}

/// The permissive default — every resource is visible. Use this when the
/// caller is trusted (local single-user, or a test) and the permission
/// layer isn't gating enumeration yet.
pub struct AllVisible;

impl ActionVisibility for AllVisible {
    fn can_see(&self, _resource: &ResourceRef, _kind: &ResourceKind) -> bool {
        true
    }
}

/// A headless snapshot of the actions exported for one query.
///
/// Holds the live [`SearchResult`]s (which own the invoke callbacks) so
/// that `enumerate`, `select`, and `invoke` all operate over the exact
/// same enumeration the caller inspected — no re-query race between
/// "what can I do" and "do it".
pub struct ActionCatalog {
    /// All results the registry returned, in merge order.
    results: Vec<SearchResult>,
    /// Indices into `results` that passed the visibility gate. Every
    /// public accessor works through this so a denied resource is
    /// uniformly absent from enumeration *and* selection.
    visible: Vec<usize>,
}

impl ActionCatalog {
    /// Build a catalog by fanning `query` out to `registry` under
    /// `scope`, then applying `visibility`.
    ///
    /// This is the same [`SearchRegistry::query`] the palette calls — the
    /// action path and the presentation path share one substrate. Pass
    /// `SearchScope::ActiveApp` with the focused tab's provider id
    /// (`active_provider_id`) to enumerate only what the *focused app*
    /// exports; pass `SearchScope::All` / `None` to survey everything.
    ///
    /// An empty `query` returns every provider's default set (score 0),
    /// so a caller can list the full surface without guessing a needle.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        registry: &SearchRegistry,
        query: &str,
        scope: SearchScope,
        active_provider_id: Option<&str>,
        chrome: &ChromeState,
        doc: &AutoCommit,
        doc_root: &ObjId,
        visibility: &dyn ActionVisibility,
    ) -> Self {
        // Empty `wanted_kinds` ⇒ "any kind"; the action API never filters
        // by chip, it surveys the whole exported surface.
        let results = registry.query(
            query,
            scope,
            active_provider_id,
            &[],
            chrome,
            doc,
            doc_root,
        );
        let visible = results
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                visibility.can_see(&ResourceRef::new(&r.provider_id, &r.id), &r.kind)
            })
            .map(|(i, _)| i)
            .collect();
        Self { results, visible }
    }

    /// Enumerate the typed descriptors the caller may see. Render-free —
    /// no scene is built, no widget is touched.
    pub fn enumerate(&self) -> Vec<ExportedAction> {
        self.visible
            .iter()
            .map(|&i| {
                let r = &self.results[i];
                ExportedAction {
                    resource: ResourceRef::new(&r.provider_id, &r.id),
                    kind: r.kind.clone(),
                    title: r.title.clone(),
                    subtitle: r.subtitle.clone(),
                    score: r.score,
                }
            })
            .collect()
    }

    /// Number of visible actions.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Whether the caller can see nothing (empty query with no providers,
    /// or everything gated out). Empty stays empty — the API never
    /// fabricates a placeholder row.
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Whether `resource` is present and visible in this snapshot.
    pub fn contains(&self, resource: &ResourceRef) -> bool {
        self.select(resource).is_some()
    }

    /// Resolve a [`ResourceRef`] back to the live action so it can be
    /// invoked. `None` when the id isn't in this snapshot *or* the
    /// visibility gate hid it — you cannot select what you can't see.
    pub fn select(&self, resource: &ResourceRef) -> Option<SelectedAction<'_>> {
        self.visible
            .iter()
            .map(|&i| &self.results[i])
            .find(|r| r.provider_id == resource.provider && r.id == resource.id)
            .map(|result| SelectedAction { result })
    }
}

/// A resolved action, ready to invoke. A distinct type so "I picked X"
/// and "run X" are separate, auditable steps — a caller can inspect
/// [`SelectedAction::kind`] / [`SelectedAction::title`] before committing
/// to the side-effect.
pub struct SelectedAction<'a> {
    result: &'a SearchResult,
}

impl SelectedAction<'_> {
    /// The stable address of the action this resolves to.
    pub fn resource(&self) -> ResourceRef {
        ResourceRef::new(&self.result.provider_id, &self.result.id)
    }

    /// The typed kind (Action / Note / File / …).
    pub fn kind(&self) -> &ResourceKind {
        &self.result.kind
    }

    /// Human-readable title.
    pub fn title(&self) -> &str {
        &self.result.title
    }

    /// Invoke the action headlessly: run the provider's side-effect
    /// against the live CRDT (`doc`) and `chrome`.
    ///
    /// This is the same callback the palette fires on Enter — the whole
    /// contract of the action API is that it reaches this side-effect
    /// **without** ever constructing a scene, a window, or a widget
    /// tree. Whatever the provider mutates (a chrome overlay flag, a
    /// CRDT splice, a tab activation) is observable immediately.
    pub fn invoke(&self, doc: &mut AutoCommit, root: &ObjId, chrome: &Chrome) {
        (self.result.on_open)(doc, root, chrome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::providers::{ActionsProvider, NotesProvider};
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use parking_lot::RwLock;
    use std::rc::Rc;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn registry_with_actions() -> SearchRegistry {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(ActionsProvider::new()));
        reg
    }

    /// The headline covenant: enumerate → select by id → invoke, entirely
    /// headless (no GPU, no scene, no widget), and the invocation mutates
    /// real chrome state through the production `ActionsProvider` path.
    #[test]
    fn enumerate_select_invoke_mutates_state_with_no_rendering() {
        let reg = registry_with_actions();
        let chrome = chrome_rc();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // 1. ENUMERATE — survey what the shell exports. No side-effect.
        let catalog = ActionCatalog::build(
            &reg,
            "settings",
            SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &root,
            &AllVisible,
        );
        let actions = catalog.enumerate();
        assert!(!actions.is_empty(), "enumeration surfaces the exported actions");
        let target = ResourceRef::new("actions", "settings.open");
        let listed = actions
            .iter()
            .find(|a| a.resource == target)
            .expect("the exported 'Open workspace settings' action is enumerable by id");
        assert_eq!(listed.kind, ResourceKind::Action, "descriptors are typed");
        assert_eq!(listed.title, "Open workspace settings");

        // 2. SELECT — resolve the id to a live, invokable handle.
        let selected = catalog
            .select(&target)
            .expect("a listed resource is addressable by its ResourceRef");
        assert_eq!(selected.kind(), &ResourceKind::Action);
        assert_eq!(selected.resource(), target);

        // Precondition: the overlay the action toggles is off.
        assert!(
            !chrome.borrow().overlays.ws_settings,
            "workspace-settings overlay starts closed"
        );

        // 3. INVOKE — run the side-effect. No scene is ever constructed.
        selected.invoke(&mut doc, &root, &chrome);

        // The production provider's callback flipped real chrome state.
        assert!(
            chrome.borrow().overlays.ws_settings,
            "invoking the exported action mutated chrome state, entirely headless"
        );
    }

    /// The permission seam gates enumeration *and* selection uniformly —
    /// a hidden resource can neither be listed nor invoked.
    #[test]
    fn visibility_gate_hides_resources_from_enumerate_and_select() {
        let mut reg = registry_with_actions();
        reg.register(Rc::new(NotesProvider::new()));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let root = automerge::ROOT;

        // Deny everything the "actions" provider exports.
        let deny_actions = |r: &ResourceRef, _k: &ResourceKind| r.provider != "actions";
        let catalog = ActionCatalog::build(
            &reg,
            "",
            SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &root,
            &deny_actions,
        );

        assert!(
            catalog
                .enumerate()
                .iter()
                .all(|a| a.resource.provider != "actions"),
            "gated provider is absent from enumeration"
        );
        assert!(
            catalog
                .select(&ResourceRef::new("actions", "settings.open"))
                .is_none(),
            "a resource the caller can't see is not selectable (and so not invokable)"
        );
    }

    /// `ActiveApp` scope narrows the survey to the focused app's provider,
    /// so an LLM navigator "enumerates what the focused app exports".
    #[test]
    fn active_app_scope_narrows_to_the_focused_provider() {
        let mut reg = registry_with_actions();
        reg.register(Rc::new(NotesProvider::new()));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let root = automerge::ROOT;

        let catalog = ActionCatalog::build(
            &reg,
            "",
            SearchScope::ActiveApp,
            Some("actions"),
            &chrome.borrow(),
            &doc,
            &root,
            &AllVisible,
        );
        assert!(
            catalog
                .enumerate()
                .iter()
                .all(|a| a.resource.provider == "actions"),
            "ActiveApp scope enumerates only the focused provider's exports"
        );
    }

    /// Empty stays empty — no fabricated rows when nothing matches.
    #[test]
    fn no_match_yields_an_empty_catalog() {
        let reg = registry_with_actions();
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        let catalog = ActionCatalog::build(
            &reg,
            "zzz-nothing-matches-this-needle-zzz",
            SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &root,
            &AllVisible,
        );
        assert!(catalog.is_empty(), "an unmatched query enumerates nothing");
    }
}
