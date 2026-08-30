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

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};

use super::{ResourceKind, SearchRegistry, SearchResult};

/// The parameter vocabulary, defined once in [`qos_actions::params`] and
/// re-exported here.
///
/// These types used to be declared in this module *and* again on the app
/// side, which meant the headless surface and an app's own actions agreed
/// only for as long as someone kept them in step by hand. Two spellings of
/// the same argument is precisely how an external caller ends up supplying
/// a value the app parses differently — a class of bug that no amount of
/// testing removes, because the two definitions are each internally
/// consistent. One definition removes it instead.
///
/// Nothing about this surface moves: `text` / `integer` / `boolean` are the
/// same wire spellings, and [`ActionArgs::bind`] keeps the same strictness —
/// an unknown argument is refused rather than dropped, the first failure
/// wins, and nothing partial reaches an action. The shared definition adds
/// [`ParamType::Number`] and [`ActionParam::choices`] on top; no action in
/// this crate declares either yet, so the compositor's exported descriptors
/// are byte-for-byte what they were.
pub use qos_actions::params::{ActionArgs, ActionParam, ArgError, ArgValue, ParamType};

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
        let results = registry.query(query, scope, active_provider_id, &[], chrome, doc, doc_root);
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
            .map(|&i| {
                let r = &self.results[i];
                ExportedAction {
                    resource: ResourceRef::new(&r.provider_id, &r.id),
                    kind: r.kind.clone(),
                    title: r.title.clone(),
                    subtitle: r.subtitle.clone(),
                    score: r.score,
                    params: r.params.clone(),
                }
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
        self.visible
            .iter()
            .map(|&i| &self.results[i])
            .find(|r| r.provider_id == resource.provider && r.id == resource.id)
            .map(|result| SelectedAction { result })
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

    /// Run the provider's side-effect, the same callback the palette fires
    /// on Enter. A failed bind is refused with the document untouched.
    pub fn invoke(
        &self,
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
        args: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<(), ActionError> {
        let bound = ActionArgs::bind(&self.result.params, args).map_err(ActionError::Arguments)?;
        (self.result.on_open)(doc, root, chrome, &bound)
    }

    /// Invoke the action with arguments that arrived as a JSON object —
    /// the shape a control socket delivers.
    ///
    /// The JSON→[`ArgValue`] conversion lives in the shared vocabulary
    /// ([`ActionArgs::bind_json`]) rather than in the transport, so the
    /// compositor's socket and an app's own out-of-process surface read a
    /// wire argument the same way. A transport that kept its own table
    /// would be a second definition of the thing this vocabulary exists to
    /// have exactly one of.
    ///
    /// Binding happens **before** the action runs, and every argument is
    /// converted before any is bound, so a request carrying one bad value
    /// leaves the document untouched rather than half-applied.
    pub fn invoke_json(
        &self,
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ActionError> {
        let bound =
            ActionArgs::bind_json(&self.result.params, args).map_err(ActionError::Arguments)?;
        (self.result.on_open)(doc, root, chrome, &bound)
    }

    /// Invoke an action that declares no parameters.
    ///
    /// Sugar over [`Self::invoke`] with an empty argument set, kept
    /// because most of the surface is nullary and spelling that out at
    /// every call site would bury the one case that is not. An action
    /// that *does* require a parameter still refuses here — this is a
    /// shorter way to pass nothing, not a way to skip the check.
    /// Sugar over [`Self::invoke`] with an empty argument set — a shorter
    /// way to pass nothing, not a way to skip the check.
    pub fn invoke_nullary(
        &self,
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
    ) -> Result<(), ActionError> {
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
                on_open: std::rc::Rc::new(move |_doc, _root, _chrome, args: &ActionArgs| {
                    assert_eq!(args.text("text"), Some("ok"));
                    ran.set(true);
                    Ok(())
                }),
            }]
        }
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

    /// The compositor's parameter vocabulary **is** the action model's, not
    /// a look-alike sitting beside it.
    ///
    /// This surface and an app's own actions are driven by the same external
    /// caller, and two structurally-identical declarations agree only for as
    /// long as someone keeps editing both. Re-declaring these types here
    /// would compile and would pass every other test in this file while
    /// silently reopening that gap, so the check is type *identity*: a
    /// parameter list built through the shared path is bound through this
    /// module's path. A separate declaration does not typecheck.
    #[test]
    fn the_parameter_vocabulary_is_shared_with_the_action_model_not_copied() {
        let shared: Vec<qos_actions::params::ActionParam> =
            vec![qos_actions::params::ActionParam::required(
                "text",
                qos_actions::params::ParamType::Text,
                "what to write",
            )];

        let bound = ActionArgs::bind(&shared, [("text".to_string(), text("hi"))])
            .expect("a parameter declared through the shared path binds through this one");
        assert_eq!(bound.text("text"), Some("hi"));

        // The wire spellings are the external contract: they are what an
        // enumeration descriptor carries, and the only thing a caller with no
        // screen has to build an invocation from.
        assert_eq!(ParamType::Text.label(), "text");
        assert_eq!(ParamType::Integer.label(), "integer");
        assert_eq!(ParamType::Boolean.label(), "boolean");
    }

    /// Declared `choices` are enforced by binding — so an action that
    /// restricts its values refuses anything else by name rather than
    /// quietly falling through to a default.
    ///
    /// Nothing in this crate declares choices yet. The check is here because
    /// the enforcement arrived with the shared vocabulary and applies to
    /// every action reachable through this catalogue the moment one does.
    #[test]
    fn a_value_outside_a_parameters_choices_is_refused() {
        let params = vec![
            qos_actions::params::ActionParam::required("filter", ParamType::Text, "which filter")
                .with_choices(["grayscale", "sepia"]),
        ];

        assert!(
            ActionArgs::bind(&params, [("filter".to_string(), text("sepia"))]).is_ok(),
            "a declared choice must be accepted"
        );

        let err = ActionArgs::bind(&params, [("filter".to_string(), text("invert"))])
            .expect_err("a value outside the declared choices must be refused");
        let ArgError::NotAllowed { name, value, .. } = &err else {
            panic!("expected a NotAllowed refusal, got {err:?}");
        };
        assert_eq!(name, "filter");
        assert_eq!(value, "invert");
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
