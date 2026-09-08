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
pub use qos_actions::{ActionArgs, ActionParam, ArgError, ArgValue, ParamType};

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

/// A typed, render-free descriptor of one exported thing. Carries **no**
/// invoke callback — invoking is a separate step through
/// [`ActionCatalog::select`], so a survey is side-effect free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: ResourceRef,
    pub kind: ResourceKind,
    pub title: String,
    pub subtitle: Option<String>,
    /// Provider-supplied rank within its kind, higher first.
    pub score: i32,
    /// What an invocation must carry, empty for the nullary majority.
    pub params: Vec<ActionParam>,
    pub contract: Option<qos_actions::ActionContract>,
}

/// Permission hook: decides whether a caller may *see* a given resource.
/// The permission subsystem ([`ui::permissions`]) owns the real policy; a
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
}

impl ActionCatalog {
    /// Fan `query` out to `registry` under `scope`, then apply `visibility`.
    /// An empty `query` returns every provider's default set (score 0), so
    /// a caller can list the full surface without guessing a needle.
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
        // Empty `wanted_kinds` ⇒ "any kind" — surveys the whole exported surface.
        let results = registry.query(&QueryArgs {
            needle: query,
            scope,
            active_provider_id,
            wanted_kinds: &[],
            chrome,
            doc,
            doc_root,
        });
        let visible = results
            .iter()
            .enumerate()
            .filter(|(_, r)| visibility.can_see(&ResourceRef::new(&r.provider_id, &r.id), &r.kind))
            .map(|(i, _)| i)
            .collect();
        Self { results, visible }
    }

    /// Enumerate the typed descriptors the caller may see. Render-free —
    /// no scene is built, no widget is touched.
    pub fn enumerate(&self) -> Vec<ExportedAction> {
        self.visible
            .iter()
            .filter_map(|&i| self.results.get(i))
            .map(|r| ExportedAction {
                resource: ResourceRef::new(&r.provider_id, &r.id),
                kind: r.kind.clone(),
                title: r.title.clone(),
                subtitle: r.subtitle.clone(),
                score: r.score,
                params: r.params.clone(),
                contract: r.contract.clone(),
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

    pub fn contract(&self) -> Option<&qos_actions::ActionContract> {
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
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
        args: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<ActionReceipt, ActionError> {
        match self.invoke_disposition(doc, root, chrome, args)? {
            InvocationDisposition::Accepted(receipt) => Ok(receipt),
            InvocationDisposition::Pending(_) => Err(ActionError::Refused(
                "action admission is pending; use invoke_disposition and poll its ticket".into(),
            )),
        }
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
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
        args: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<InvocationDisposition, ActionError> {
        let bound = ActionArgs::bind(&self.result.params, args).map_err(ActionError::Arguments)?;
        if let Some(begin) = &self.result.on_begin_invoke {
            return begin(doc, root, chrome, &bound);
        }
        if let Some(invoke) = &self.result.on_invoke {
            invoke(doc, root, chrome, &bound).map(InvocationDisposition::Accepted)
        } else {
            (self.result.on_open)(doc, root, chrome, &bound)?;
            Ok(InvocationDisposition::Accepted(ActionReceipt::completed()))
        }
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
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionReceipt, ActionError> {
        let bound =
            ActionArgs::bind_json(&self.result.params, args).map_err(ActionError::Arguments)?;
        if let Some(invoke) = &self.result.on_invoke {
            invoke(doc, root, chrome, &bound)
        } else {
            (self.result.on_open)(doc, root, chrome, &bound)?;
            Ok(ActionReceipt::completed())
        }
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
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
    ) -> Result<ActionReceipt, ActionError> {
        self.invoke(doc, root, chrome, Vec::new())
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
    /// headless, mutating real chrome state through the production path.
    #[test]
    fn enumerate_select_invoke_mutates_state_with_no_rendering() {
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
            .invoke_nullary(&mut doc, &root, &chrome)
            .expect("a nullary action invoked with no arguments must run");

        assert!(
            chrome.borrow().overlays.ws_settings,
            "invoking the exported action mutated chrome state, entirely headless"
        );
    }

    /// The permission seam gates enumeration and selection uniformly.
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

    /// `ActiveApp` scope narrows the survey to the focused app's provider.
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

    // ---- parameters -------------------------------------------------

    const PARAMS: &[ActionParam] = &[
        ActionParam::required("text", ParamType::Text, "what to write"),
        ActionParam::optional("at", ParamType::Integer, "where"),
    ];

    fn text(s: &str) -> ArgValue {
        ArgValue::Text(s.to_string())
    }

    /// The happy path, and the shape a caller reads back out.
    #[test]
    fn binding_accepts_the_declared_parameters() {
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

    /// An optional parameter may be left out; a required one may not.
    #[test]
    fn an_optional_parameter_may_be_omitted_and_a_required_one_may_not() {
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

    /// A dropped argument would answer `ok` to a caller that asked for
    /// something it did not get.
    #[test]
    fn an_undeclared_argument_is_refused() {
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

    /// Types are checked, not coerced.
    #[test]
    fn a_wrong_typed_argument_is_refused() {
        assert_eq!(
            ActionArgs::bind(PARAMS, [("text".to_string(), ArgValue::Integer(7))]),
            Err(ArgError::WrongType {
                name: "text".to_string(),
                expected: ParamType::Text,
                got: ParamType::Integer,
            })
        );
    }

    /// Refuses an empty invocation *before* running, so a bad call cannot
    /// leave a half-applied edit. Asserted through `invoke`, not `bind` alone.
    #[test]
    fn invoking_without_required_arguments_never_reaches_the_action() {
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
            .invoke_nullary(&mut doc, &root, &chrome)
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
            .invoke(&mut doc, &root, &chrome, [("text".to_string(), text("ok"))])
            .expect("the declared argument must be accepted");
        assert!(ran.get(), "a satisfied invocation reaches the action");
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
                provider_id: "params".to_string(),
                id: "needs.text".to_string(),
                title: "Needs text".to_string(),
                subtitle: None,
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
                provider_id: "receipts".into(),
                id: "broker.action".into(),
                title: "Broker action".into(),
                subtitle: None,
                score: 0,
                params: Vec::new(),
                contract: None,
                on_open: std::rc::Rc::new(|_, _, _, _| Ok(())),
                on_invoke: Some(std::rc::Rc::new(|_, _, _, _| {
                    Ok(ActionReceipt {
                        broker_invocation_id: Some(73),
                        update: qos_actions::InvocationUpdate::Accepted {
                            invocation: qos_actions::InvocationId::new(),
                        },
                    })
                })),
                on_begin_invoke: None,
            }]
        }
    }

    #[test]
    fn headless_invocation_returns_the_providers_broker_receipt() {
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
        let receipt = catalog
            .select(&ResourceRef::new("receipts", "broker.action"))
            .expect("the broker action is addressable")
            .invoke_nullary(&mut doc, &root, &chrome)
            .expect("the provider admits the action");

        assert_eq!(receipt.broker_invocation_id, Some(73));
        assert!(matches!(
            receipt.update,
            qos_actions::InvocationUpdate::Accepted { .. }
        ));
    }

    /// The parameter machinery must not make an existing nullary caller
    /// pass anything new.
    #[test]
    fn a_nullary_action_still_invokes_with_no_arguments() {
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
            .invoke_nullary(&mut doc, &root, &chrome)
            .expect("a nullary action takes no arguments");
        assert!(chrome.borrow().overlays.ws_settings);
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
