//! Headless action API — a programmatic, render-free facade over the
//! [`SearchRegistry`], for an automation client that drives the same
//! [`crate::shell::search::SearchProvider`]s the palette does, with no
//! window and no scene: [`ActionCatalog::enumerate`] (no callback exposed,
//! so surveying can't accidentally do anything), [`ActionCatalog::select`]
//! (resolve a [`ResourceRef`] to the live result), and
//! [`SelectedAction::invoke`] (run the provider's side-effect).
//!
//! [`ActionCatalog::build`] takes an [`ActionVisibility`] gate, so a hidden
//! resource is invisible to both `enumerate` and `select`. Typed
//! [`ActionParam`]s are checked by [`ActionArgs::bind`] before anything
//! runs, so a bad call is a refusal rather than a partial mutation.

use automerge::{AutoCommit, ObjId};

// The app protocol owns the action vocabulary. Re-exporting it here keeps the
// shell's public action API stable while making catalogue, palette, app and
// transport binding use the same types and validation rules.
pub use tilo_actions::{ActionArgs, ActionParam, ArgError, ArgValue, ParamType};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};

use super::{
    ActionReceipt, InvocationDisposition, QueryArgs, ResourceKind, SearchRegistry, SearchResult,
};

/// Why an invocation did not happen, or did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// The arguments did not satisfy the declared parameters; nothing ran.
    Arguments(ArgError),
    /// Address and arguments were both good, but there was nothing to act on.
    Refused(String),
}

impl ActionError {
    /// Lets a caller tell "I addressed this wrongly" from "nothing here to do".
    pub fn code(&self) -> &'static str {
        match self {
            ActionError::Arguments(_) => "invalid_arguments",
            ActionError::Refused(_) => "action_failed",
        }
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Arguments(e) => write!(f, "{e}"),
            ActionError::Refused(why) => write!(f, "{why}"),
        }
    }
}

/// Stable, typed address of one exported action or resource: the
/// `(provider, id)` pair every [`SearchResult`] already carries. Unlike a
/// rendered palette row (positional, vanishes when the query changes), it
/// keeps addressing the same thing for as long as the provider emits that id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceRef {
    /// The [`SearchProvider::id`](super::SearchProvider::id) that owns it.
    pub provider: String,
    /// The provider-local stable id (`settings.open`, a note's object id, …).
    pub id: String,
}

impl ResourceRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }
}

/// Semantic identity supplied by the provider independently of its opaque
/// invocation address and presentation labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionHierarchy {
    pub application: String,
    pub room: String,
    pub record: String,
    pub resource: String,
    pub semantic_identity: Option<tilo_actions::ResourceSemanticIdentity>,
    pub operation: String,
}

/// A typed, render-free descriptor of one exported thing. Carries **no**
/// invoke callback — invoking is a separate step through
/// [`ActionCatalog::select`], so a survey is side-effect free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: ResourceRef,
    pub kind: ResourceKind,
    pub operation_kind: tilo_actions::OperationKind,
    pub title: String,
    pub subtitle: Option<String>,
    /// Provider-supplied rank within its kind, higher first.
    pub score: i32,
    /// What an invocation must carry, empty for the nullary majority.
    pub params: Vec<ActionParam>,
    pub contract: Option<tilo_actions::ActionContract>,
    pub hierarchy: Option<ActionHierarchy>,
}

/// Permission hook: decides whether a caller may *see* a given resource.
/// The permission subsystem ([`ui::permissions`]) owns the policy; a
/// caller wires it in against a `PermissionContext`.
pub trait ActionVisibility {
    /// `true` if the caller may see (and so select + invoke) this resource.
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

/// The permissive default — every resource is visible. Use when the caller
/// is trusted (local single-user, or a test).
pub struct AllVisible;

impl ActionVisibility for AllVisible {
    fn can_see(&self, _resource: &ResourceRef, _kind: &ResourceKind) -> bool {
        true
    }
}

/// A headless snapshot of the actions exported for one query — `enumerate`,
/// `select`, and `invoke` all operate over the same live results.
pub struct ActionCatalog {
    /// All results the registry returned, in merge order.
    results: Vec<SearchResult>,
    /// Indices into `results` that passed the visibility gate; every public
    /// accessor works through this.
    visible: Vec<usize>,
    hierarchies: Vec<Option<ActionHierarchy>>,
    completion: super::QueryCompletion,
}

impl ActionCatalog {
    /// Fan `query` out to `registry` under `scope`, then apply `visibility`.
    /// An empty `query` returns every provider's default set (score 0), so
    /// a caller can list the full surface without guessing a needle.
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
        // Empty `wanted_kinds` ⇒ "any kind" — surveys the whole exported surface.
        let args = QueryArgs {
            action_scope: None,
            needle: query,
            scope,
            active_provider_id,
            wanted_kinds: &[],
            chrome,
            doc,
            doc_root,
        };
        Self::build_query(registry, &args, visibility, false)
    }

    /// Explicit bounded machine discovery uses the same visibility gate.
    pub fn build_query(
        registry: &SearchRegistry,
        args: &QueryArgs<'_>,
        visibility: &dyn ActionVisibility,
        include_programmatic: bool,
    ) -> Self {
        if args.action_scope.is_some_and(|scope| !scope.is_valid()) {
            return Self {
                results: Vec::new(),
                visible: Vec::new(),
                hierarchies: Vec::new(),
            };
        }
        // Remote callers do not open the palette, which normally maintains
        // retained provider indexes. Start/poll that same asynchronous work
        // here so subsequent requests can discover cold sources too.
        registry.begin_catalogue_query();
        registry.refresh_indexes(args);
        let (results, completion) = registry.query_catalogue(args, include_programmatic);
        let visible: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| visibility.can_see(&ResourceRef::new(&r.provider_id, &r.id), &r.kind))
            .map(|(i, _)| i)
            .collect();
        let hierarchies = Self::resolve_hierarchies(registry, &results, &visible);
        Self {
            results,
            visible,
            hierarchies,
            completion,
        }
    }

    /// Resolve one exact address through its owning provider. Remote
    /// providers use this path to retain the page that originally disclosed
    /// an opaque id while rechecking its current route and authority at
    /// invocation time.
    pub fn build_exact(
        registry: &SearchRegistry,
        resource: &ResourceRef,
        scope: SearchScope,
        active_provider_id: Option<&str>,
        chrome: &ChromeState,
        doc: &AutoCommit,
        doc_root: &ObjId,
        visibility: &dyn ActionVisibility,
    ) -> Self {
        let args = QueryArgs {
            action_scope: None,
            needle: &resource.id,
            scope,
            active_provider_id,
            wanted_kinds: &[],
            chrome,
            doc,
            doc_root,
        };
        let results = registry
            .query_provider_exact(&resource.provider, &resource.id, &args)
            .into_iter()
            .collect::<Vec<_>>();
        let visible: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, result)| {
                visibility.can_see(
                    &ResourceRef::new(&result.provider_id, &result.id),
                    &result.kind,
                )
            })
            .map(|(index, _)| index)
            .collect();
        let hierarchies = Self::resolve_hierarchies(registry, &results, &visible);
        Self {
            results,
            visible,
            hierarchies,
            completion: super::QueryCompletion::default(),
        }
    }

    fn resolve_hierarchies(
        registry: &SearchRegistry,
        results: &[SearchResult],
        visible: &[usize],
    ) -> Vec<Option<ActionHierarchy>> {
        let mut hierarchies = vec![None; results.len()];
        for &index in visible {
            let Some((result, hierarchy)) = results.get(index).zip(hierarchies.get_mut(index))
            else {
                continue;
            };
            *hierarchy = registry
                .providers_iter()
                .find(|provider| provider.id() == result.provider_id)
                .and_then(|provider| provider.action_hierarchy(&result.id));
        }
        hierarchies
    }

    pub fn build_for_resource(
        registry: &SearchRegistry,
        resource: &ResourceRef,
        scope: SearchScope,
        active_provider_id: Option<&str>,
        chrome: &ChromeState,
        doc: &AutoCommit,
        doc_root: &ObjId,
        visibility: &dyn ActionVisibility,
    ) -> Self {
        let results = registry
            .resolve(
                &resource.provider,
                &resource.id,
                &QueryArgs {
                    action_scope: None,
                    needle: "",
                    scope,
                    active_provider_id,
                    wanted_kinds: &[],
                    chrome,
                    doc,
                    doc_root,
                },
            )
            .into_iter()
            .collect::<Vec<_>>();
        let visible: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, result)| visibility.can_see(resource, &result.kind))
            .map(|(index, _)| index)
            .collect();
        let hierarchies = Self::resolve_hierarchies(registry, &results, &visible);
        Self {
            results,
            visible,
            hierarchies,
            completion: super::QueryCompletion::default(),
        }
    }

    /// Enumerate the typed descriptors the caller may see. Render-free —
    /// no scene is built, no widget is touched.
    pub fn completion(&self) -> &super::QueryCompletion {
        &self.completion
    }

    pub fn enumerate(&self) -> Vec<ExportedAction> {
        self.visible
            .iter()
            .filter_map(|&i| self.results.get(i).map(|result| (i, result)))
            .map(|(i, r)| ExportedAction {
                resource: ResourceRef::new(&r.provider_id, &r.id),
                kind: r.kind.clone(),
                operation_kind: r.operation_kind,
                title: r.title.clone(),
                subtitle: r.subtitle.clone(),
                score: r.score,
                params: r.params.clone(),
                contract: r.contract.clone(),
                hierarchy: self.hierarchies.get(i).cloned().flatten(),
            })
            .collect()
    }

    /// Number of visible actions.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Whether the caller can see nothing. Empty stays empty — the API
    /// never fabricates a placeholder row.
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Whether `resource` is present and visible in this snapshot.
    pub fn contains(&self, resource: &ResourceRef) -> bool {
        self.select(resource).is_some()
    }

    /// Resolve a [`ResourceRef`] to the live action. `None` when the id
    /// isn't in this snapshot or the visibility gate hid it.
    pub fn select(&self, resource: &ResourceRef) -> Option<SelectedAction<'_>> {
        // Two independent identity checks (provider, then id) — matching
        // field names would be a coincidence, not a bug:
        // `SearchResult::provider_id` and `ResourceRef::provider` name
        // the same concept differently by design.
        #[allow(
            clippy::suspicious_operation_groupings,
            reason = "provider_id/provider and id/id are two independent equality checks, not a \
                      swapped field pair"
        )]
        let found = self
            .visible
            .iter()
            .filter_map(|&i| self.results.get(i))
            .find(|r| r.provider_id == resource.provider && r.id == resource.id);
        found.map(|result| SelectedAction { result })
    }
}

/// A resolved action, ready to invoke — a distinct type so "I picked X" and
/// "run X" are separate, auditable steps.
pub struct SelectedAction<'a> {
    result: &'a SearchResult,
}

impl SelectedAction<'_> {
    fn validate_contract(&self, args: &ActionArgs) -> Result<(), ActionError> {
        let Some(contract) = self.result.contract.as_ref() else {
            return Ok(());
        };
        if let tilo_actions::Availability::Unavailable { code, reason } = &contract.availability {
            return Err(ActionError::Refused(format!(
                "action is unavailable ({code}): {reason}"
            )));
        }
        match &contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => Ok(()),
            tilo_actions::ConfirmationPolicy::Confirm if args.boolean("confirm") == Some(true) => {
                Ok(())
            }
            tilo_actions::ConfirmationPolicy::Confirm => Err(ActionError::Refused(
                "explicit confirmation is required".into(),
            )),
            tilo_actions::ConfirmationPolicy::Phrase { phrase }
                if args.text("confirmation_phrase") == Some(phrase.as_str()) =>
            {
                Ok(())
            }
            tilo_actions::ConfirmationPolicy::Phrase { .. } => Err(ActionError::Refused(
                "the exact confirmation phrase is required".into(),
            )),
        }
    }

    fn invoke_bound(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
        bound: &ActionArgs,
    ) -> Result<InvocationDisposition, ActionError> {
        self.validate_contract(bound)?;
        if let Some(begin) = &self.result.on_begin_invoke {
            return begin(doc, root, chrome, bound);
        }
        if let Some(invoke) = &self.result.on_invoke {
            invoke(doc, root, chrome, bound).map(InvocationDisposition::Accepted)
        } else {
            (self.result.on_open)(doc, root, chrome, bound)?;
            Ok(InvocationDisposition::Accepted(ActionReceipt::completed()))
        }
    }

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

    /// What this action's invocation must carry.
    pub fn params(&self) -> &[ActionParam] {
        &self.result.params
    }

    pub fn contract(&self) -> Option<&tilo_actions::ActionContract> {
        self.result.contract.as_ref()
    }

    /// Run the provider's side-effect, the same callback the palette fires
    /// on Enter. A failed bind is refused with the document untouched.
    ///
    /// # Errors
    ///
    /// [`ActionError::Arguments`] when `args` doesn't satisfy this
    /// action's declared params (see [`ActionArgs::bind`]);
    /// [`ActionError::Refused`] when the bind succeeded but the
    /// provider's own callback found nothing to act on.
    pub fn invoke(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
        args: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<InvocationDisposition, ActionError> {
        let bound = ActionArgs::bind(&self.result.params, args).map_err(ActionError::Arguments)?;
        self.invoke_bound(doc, root, chrome, &bound)
    }

    /// Begin an invocation without fabricating success for a remote action
    /// whose current authority is still being checked. Agents can retain and
    /// poll the returned ticket just as the visual palette does.
    ///
    /// # Errors
    ///
    /// Returns an argument-binding or provider refusal error; callers must
    /// poll a pending disposition rather than treating it as success.
    pub fn invoke_disposition(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
        args: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<InvocationDisposition, ActionError> {
        self.invoke(doc, root, chrome, args)
    }

    /// Invoke from the canonical JSON object used by control sockets and MCP.
    /// Unlike lossy JSON-to-enum guessing, binding consults each declared
    /// parameter, preserving resource/date-time/secret semantics and nested or
    /// nullable values.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Arguments`] when the JSON object violates the
    /// declared parameters, or [`ActionError::Refused`] when the provider
    /// cannot perform the bound action.
    pub fn invoke_json(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<InvocationDisposition, ActionError> {
        let bound =
            ActionArgs::bind_json(&self.result.params, args).map_err(ActionError::Arguments)?;
        self.invoke_bound(doc, root, chrome, &bound)
    }

    /// Begin a JSON invocation while preserving asynchronous admission.
    /// Binding happens once and a pending ticket owns the in-flight request;
    /// callers poll that ticket rather than re-resolving or restarting it.
    ///
    /// # Errors
    ///
    /// Returns argument, policy, provider, or admission failures without
    /// falling back to a different invocation callback.
    pub fn invoke_json_disposition(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<InvocationDisposition, ActionError> {
        self.invoke_json(doc, root, chrome, args)
    }

    /// Sugar over [`Self::invoke`] with an empty argument set — a shorter
    /// way to pass nothing, not a way to skip the check.
    ///
    /// # Errors
    ///
    /// Same as [`Self::invoke`] — most commonly
    /// [`ActionError::Arguments`] when this action actually requires
    /// parameters.
    pub fn invoke_nullary(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        chrome: &Chrome,
    ) -> Result<InvocationDisposition, ActionError> {
        self.invoke(doc, root, chrome, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::providers::{ActionsProvider, ApplicationsProvider};
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use std::rc::Rc;

    struct DeferredEmptyCatalogue(std::cell::Cell<bool>);

    impl super::super::SearchProvider for DeferredEmptyCatalogue {
        fn id(&self) -> &'static str {
            "deferred-empty"
        }
        fn name(&self) -> &'static str {
            "Deferred empty"
        }
        fn resource_kinds(&self) -> &[ResourceKind] {
            &[ResourceKind::Action]
        }
        fn query(&self, _: super::super::SearchContext<'_>) -> Vec<SearchResult> {
            Vec::new()
        }
        fn query_incomplete_reason(
            &self,
            _ctx: &super::super::SearchContext<'_>,
        ) -> Option<super::super::QueryIncompleteReason> {
            (!self.0.get()).then_some(super::super::QueryIncompleteReason::Pending)
        }
        fn query_page(
            &self,
            _: super::super::SearchContext<'_>,
            _: Option<&super::super::SearchCursor>,
            _: super::super::SearchBudget,
        ) -> super::super::SearchPage {
            if self.0.get() {
                super::super::SearchPage::exhausted("ready", Vec::new())
            } else {
                super::super::SearchPage {
                    revision: "pending".into(),
                    results: Vec::new(),
                    next_cursor: Some(super::super::SearchCursor {
                        revision: "pending".into(),
                        token: "pending".into(),
                    }),
                    exhausted: false,
                    stale_cursor: false,
                    budget_exhausted: false,
                }
            }
        }
    }

    #[test]
    fn empty_catalogue_retains_pending_and_complete_traversal() {
        let source = Rc::new(DeferredEmptyCatalogue(std::cell::Cell::new(false)));
        let mut registry = SearchRegistry::new();
        registry.register(source.clone());
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let args = QueryArgs {
            needle: "",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let pending = ActionCatalog::build_query(&registry, &args, &AllVisible, true);
        assert!(pending.enumerate().is_empty());
        assert_eq!(
            serde_json::to_value(pending.completion()).unwrap(),
            serde_json::json!({
                "complete": false, "incomplete": [{"provider": "deferred-empty", "reason": "pending"}],
            })
        );
        source.0.set(true);
        let complete = ActionCatalog::build_query(&registry, &args, &AllVisible, true);
        assert!(complete.enumerate().is_empty());
        assert_eq!(
            serde_json::to_value(complete.completion()).unwrap(),
            serde_json::json!({
                "complete": true, "incomplete": [],
            })
        );
    }

    fn chrome_rc() -> Chrome {
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, "Action API test");
        workspace.set_owner(crate::shell::state::local_user_id_for_room(&workspace.room));
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = id;
        let handle: WorkspaceTreeHandle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        ChromeState::new_with_handle(handle)
    }

    fn registry_with_actions() -> SearchRegistry {
        let mut reg = SearchRegistry::new();
        reg.register(Rc::new(ActionsProvider::new()));
        reg
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn headless_catalog_enumerates_selects_and_scopes_visible_resources() {
        // Phase `enumerate_select_invoke_mutates_state_with_no_rendering`.
        // The headline covenant: enumerate → select by id → invoke, entirely
        // headless, mutating chrome state through the production path.
        {
            let reg = registry_with_actions();
            let chrome = chrome_rc();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            // ENUMERATE — survey what the shell exports. No side-effect.
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
            assert!(
                !actions.is_empty(),
                "enumeration surfaces the exported actions"
            );
            let target = ResourceRef::new("actions", "settings.open");
            let listed = actions
                .iter()
                .find(|a| a.resource == target)
                .expect("the exported 'Open workspace settings' action is enumerable by id");
            assert_eq!(listed.kind, ResourceKind::Action, "descriptors are typed");
            assert_eq!(listed.title, "Open workspace settings");

            // SELECT — resolve the id to a live, invokable handle.
            let selected = catalog
                .select(&target)
                .expect("a listed resource is addressable by its ResourceRef");
            assert_eq!(selected.kind(), &ResourceKind::Action);
            assert_eq!(selected.resource(), target);

            assert!(
                !chrome.borrow().overlays.ws_settings,
                "workspace-settings overlay starts closed"
            );

            // INVOKE — run the side-effect. No scene is ever constructed.
            selected
                .invoke_nullary(&mut ui::widget::Edits::lend(&mut doc), &root, &chrome)
                .expect("a nullary action invoked with no arguments must run");

            assert!(
                chrome.borrow().overlays.ws_settings,
                "invoking the exported action mutated chrome state, entirely headless"
            );
        }

        // Phase `visibility_gate_hides_resources_from_enumerate_and_select`.
        // The permission seam gates enumeration and selection uniformly.
        {
            let mut reg = registry_with_actions();
            reg.register(Rc::new(ApplicationsProvider::new()));
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

        // Phase `active_app_scope_narrows_to_the_focused_provider`.
        // `ActiveApp` scope narrows the survey to the focused app's provider.
        {
            let mut reg = registry_with_actions();
            reg.register(Rc::new(ApplicationsProvider::new()));
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

        // Phase `no_match_yields_an_empty_catalog`.
        // Empty stays empty — no fabricated rows when nothing matches.
        {
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

    // ---- parameters -------------------------------------------------

    const PARAMS: &[ActionParam] = &[
        ActionParam::required("text", ParamType::Text, "what to write"),
        ActionParam::optional("at", ParamType::Integer, "where"),
    ];

    fn text(s: &str) -> ArgValue {
        ArgValue::Text(s.to_string())
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn argument_binding_follows_the_declared_parameters_exactly() {
        // Phase `binding_accepts_the_declared_parameters`.
        // The happy path, and the shape a caller reads back out.
        {
            let args = ActionArgs::bind(
                PARAMS,
                [
                    ("text".to_string(), text("hello")),
                    ("at".to_string(), ArgValue::Integer(4)),
                ],
            )
            .expect("well-formed arguments must bind");
            assert_eq!(args.text("text"), Some("hello"));
            assert_eq!(args.integer("at"), Some(4));
        }

        // Phase `an_optional_parameter_may_be_omitted_and_a_required_one_may_not`.
        // An optional parameter may be left out; a required one may not.
        {
            let args = ActionArgs::bind(PARAMS, [("text".to_string(), text("hi"))])
                .expect("omitting an optional parameter must bind");
            assert_eq!(args.integer("at"), None, "no default is invented");

            assert_eq!(
                ActionArgs::bind(PARAMS, []),
                Err(ArgError::Missing {
                    name: "text".to_string()
                }),
                "a required parameter must be supplied"
            );
        }

        // Phase `an_undeclared_argument_is_refused`.
        // A dropped argument would answer `ok` to a caller that asked for
        // something it did not get.
        {
            assert_eq!(
                ActionArgs::bind(
                    PARAMS,
                    [
                        ("text".to_string(), text("hi")),
                        ("colour".to_string(), text("red")),
                    ]
                ),
                Err(ArgError::Unknown {
                    name: "colour".to_string()
                })
            );
        }

        // Phase `a_wrong_typed_argument_is_refused`.
        // Types are checked, not coerced.
        {
            assert_eq!(
                ActionArgs::bind(PARAMS, [("text".to_string(), ArgValue::Integer(7))]),
                Err(ArgError::WrongType {
                    name: "text".to_string(),
                    expected: ParamType::Text,
                    got: ParamType::Integer,
                })
            );
        }

        // Phase `invoking_without_required_arguments_never_reaches_the_action`.
        // Refuses an empty invocation *before* running, so a bad call cannot
        // leave a half-applied edit. Asserted through `invoke`, not `bind` alone.
        {
            use std::cell::Cell;
            use std::rc::Rc as StdRc;

            let ran = StdRc::new(Cell::new(false));
            let flag = StdRc::clone(&ran);
            let mut reg = SearchRegistry::new();
            reg.register(StdRc::new(ParamProvider {
                ran: StdRc::clone(&flag),
            }));

            let chrome = chrome_rc();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let catalog = ActionCatalog::build(
                &reg,
                "",
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &root,
                &AllVisible,
            );

            // Enumeration is the only way a headless caller learns to pass it.
            let listed = catalog.enumerate();
            let entry = listed
                .iter()
                .find(|a| a.resource.id == "needs.text")
                .expect("the parameterised action is enumerable");
            assert_eq!(entry.params.len(), 1);
            assert_eq!(entry.params[0].name, "text");
            assert!(entry.params[0].required);

            let selected = catalog
                .select(&ResourceRef::new("params", "needs.text"))
                .expect("addressable");

            let err = selected
                .invoke_nullary(&mut ui::widget::Edits::lend(&mut doc), &root, &chrome)
                .expect_err("a required parameter must not be skippable");
            assert!(matches!(
                err,
                ActionError::Arguments(ArgError::Missing { .. })
            ));
            assert!(
                !ran.get(),
                "the action's body must not run when its arguments are refused"
            );

            // …and with the argument it declared, it runs.
            selected
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root,
                    &chrome,
                    [("text".to_string(), text("ok"))],
                )
                .expect("the declared argument must be accepted");
            assert!(ran.get(), "a satisfied invocation reaches the action");
        }

        // Phase `a_nullary_action_still_invokes_with_no_arguments`.
        // The parameter machinery must not make an existing nullary caller
        // pass anything new.
        {
            let reg = registry_with_actions();
            let chrome = chrome_rc();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
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
            let selected = catalog
                .select(&ResourceRef::new("actions", "settings.open"))
                .expect("addressable");
            assert!(
                selected.params().is_empty(),
                "a chrome toggle declares no parameters"
            );
            selected
                .invoke_nullary(&mut ui::widget::Edits::lend(&mut doc), &root, &chrome)
                .expect("a nullary action takes no arguments");
            assert!(chrome.borrow().overlays.ws_settings);
        }
    }

    /// A provider whose one row takes a required text argument.
    struct ParamProvider {
        ran: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl super::super::SearchProvider for ParamProvider {
        fn id(&self) -> &'static str {
            "params"
        }
        fn name(&self) -> &'static str {
            "Params"
        }
        fn resource_kinds(&self) -> &[ResourceKind] {
            &[ResourceKind::Action]
        }
        fn query(&self, _ctx: super::super::SearchContext<'_>) -> Vec<SearchResult> {
            let ran = std::rc::Rc::clone(&self.ran);
            vec![SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Document,
                provider_id: "params".to_string(),
                id: "needs.text".to_string(),
                title: "Needs text".to_string(),
                subtitle: None,
                badge: None,
                score: 0,
                params: vec![ActionParam::required(
                    "text",
                    ParamType::Text,
                    "what to write",
                )],
                contract: None,
                on_open: std::rc::Rc::new(move |_doc, _root, _chrome, args: &ActionArgs| {
                    assert_eq!(args.text("text"), Some("ok"));
                    ran.set(true);
                    Ok(())
                }),
                on_invoke: None,
                on_begin_invoke: None,
            }]
        }
    }

    struct ReceiptProvider;

    impl super::super::SearchProvider for ReceiptProvider {
        fn id(&self) -> &'static str {
            "receipts"
        }

        fn name(&self) -> &'static str {
            "Receipts"
        }

        fn resource_kinds(&self) -> &[ResourceKind] {
            &[ResourceKind::Action]
        }

        fn query(&self, _ctx: super::super::SearchContext<'_>) -> Vec<SearchResult> {
            vec![SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: "receipts".into(),
                id: "broker.action".into(),
                title: "Broker action".into(),
                subtitle: None,
                badge: None,
                score: 0,
                params: Vec::new(),
                contract: None,
                on_open: std::rc::Rc::new(|_, _, _, _| Ok(())),
                on_invoke: Some(std::rc::Rc::new(|_, _, _, _| {
                    Ok(ActionReceipt {
                        broker_invocation_id: Some(73),
                        update: tilo_actions::InvocationUpdate::Accepted {
                            invocation: tilo_actions::InvocationId::new(),
                        },
                    })
                })),
                on_begin_invoke: None,
            }]
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn invocations_return_the_brokers_receipt_and_admission_state() {
        // Phase `headless_invocation_returns_the_providers_broker_receipt`.
        {
            let mut registry = SearchRegistry::new();
            registry.register(std::rc::Rc::new(ReceiptProvider));
            let chrome = chrome_rc();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let catalog = ActionCatalog::build(
                &registry,
                "",
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &root,
                &AllVisible,
            );
            let disposition = catalog
                .select(&ResourceRef::new("receipts", "broker.action"))
                .expect("the broker action is addressable")
                .invoke_nullary(&mut ui::widget::Edits::lend(&mut doc), &root, &chrome)
                .expect("the provider admits the action");
            let InvocationDisposition::Accepted(receipt) = disposition else {
                panic!("the synchronous provider must return its receipt immediately");
            };

            assert_eq!(receipt.broker_invocation_id, Some(73));
            assert!(matches!(
                receipt.update,
                tilo_actions::InvocationUpdate::Accepted { .. }
            ));
        }

        // Phase `json_invocation_begins_once_and_retains_pending_admission_ticket`.
        {
            let begins = std::rc::Rc::new(std::cell::Cell::new(0));
            let polls = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut registry = SearchRegistry::new();
            registry.register(std::rc::Rc::new(AsyncJsonProvider {
                begins: std::rc::Rc::clone(&begins),
                polls: std::rc::Rc::clone(&polls),
                refuse: false,
            }));
            let chrome = chrome_rc();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let catalog = ActionCatalog::build(
                &registry,
                "",
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &root,
                &AllVisible,
            );
            let selected = catalog
                .select(&ResourceRef::new("async-json", "wait-for-admission"))
                .expect("async action is addressable");
            let InvocationDisposition::Pending(ticket) = selected
                .invoke_json(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root,
                    &chrome,
                    &serde_json::Map::new(),
                )
                .expect("begin returns a pending ticket")
            else {
                panic!("admission must remain pending")
            };

            assert_eq!(begins.get(), 1, "the operation starts exactly once");
            assert_eq!(ticket().expect("first poll succeeds"), None);
            assert_eq!(ticket().expect("second poll succeeds"), None);
            let receipt = ticket()
                .expect("third poll succeeds")
                .expect("third poll admits the invocation");
            assert_eq!(begins.get(), 1, "polling must not restart the operation");
            assert_eq!(receipt.broker_invocation_id, Some(91));
            assert!(matches!(
                receipt.update,
                tilo_actions::InvocationUpdate::Accepted { invocation }
                    if invocation == tilo_actions::InvocationId(uuid::Uuid::from_u128(0x91))
            ));
        }

        // Phase `json_invocation_exposes_rejected_admission`.
        {
            let begins = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut registry = SearchRegistry::new();
            registry.register(std::rc::Rc::new(AsyncJsonProvider {
                begins: std::rc::Rc::clone(&begins),
                polls: std::rc::Rc::new(std::cell::Cell::new(0)),
                refuse: true,
            }));
            let chrome = chrome_rc();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let catalog = ActionCatalog::build(
                &registry,
                "",
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &root,
                &AllVisible,
            );
            let error = catalog
                .select(&ResourceRef::new("async-json", "wait-for-admission"))
                .expect("async action is addressable")
                .invoke_json(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root,
                    &chrome,
                    &serde_json::Map::new(),
                )
                .expect_err("rejected admission must be returned");
            assert_eq!(begins.get(), 1);
            assert!(
                matches!(error, ActionError::Refused(message) if message.contains("authority"))
            );
        }
    }

    struct AsyncJsonProvider {
        begins: std::rc::Rc<std::cell::Cell<usize>>,
        polls: std::rc::Rc<std::cell::Cell<usize>>,
        refuse: bool,
    }

    impl super::super::SearchProvider for AsyncJsonProvider {
        fn id(&self) -> &'static str {
            "async-json"
        }

        fn name(&self) -> &'static str {
            "Async JSON"
        }

        fn resource_kinds(&self) -> &[ResourceKind] {
            &[ResourceKind::Action]
        }

        fn query(&self, _ctx: super::super::SearchContext<'_>) -> Vec<SearchResult> {
            let begins = std::rc::Rc::clone(&self.begins);
            let polls = std::rc::Rc::clone(&self.polls);
            let refuse = self.refuse;
            vec![SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: self.id().into(),
                id: "wait-for-admission".into(),
                title: "Wait for admission".into(),
                badge: None,
                subtitle: None,
                score: 0,
                params: Vec::new(),
                contract: None,
                on_open: std::rc::Rc::new(|_, _, _, _| {
                    panic!("legacy fallback must not run when async admission exists")
                }),
                on_invoke: Some(std::rc::Rc::new(|_, _, _, _| {
                    panic!("legacy invoke must not run when async admission exists")
                })),
                on_begin_invoke: Some(std::rc::Rc::new(move |_, _, _, _| {
                    begins.set(begins.get().saturating_add(1));
                    if refuse {
                        return Err(ActionError::Refused("authority rejected admission".into()));
                    }
                    let polls = std::rc::Rc::clone(&polls);
                    Ok(InvocationDisposition::Pending(std::rc::Rc::new(
                        move || {
                            let attempt = polls.get().saturating_add(1);
                            polls.set(attempt);
                            if attempt < 3 {
                                return Ok(None);
                            }
                            Ok(Some(ActionReceipt {
                                broker_invocation_id: Some(91),
                                update: tilo_actions::InvocationUpdate::Accepted {
                                    invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                                        0x91,
                                    )),
                                },
                            }))
                        },
                    )))
                })),
            }]
        }
    }
}
