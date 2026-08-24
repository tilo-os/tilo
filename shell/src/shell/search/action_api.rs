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
//!
//! ## Parameters
//!
//! An action may declare typed [`ActionParam`]s, and an invocation
//! carries [`ActionArgs`] satisfying them. This is the same shape
//! `docs/action-model-recipe.md` gives an app's own actions — a typed
//! payload that carries everything the mutation needs — rather than a
//! second mechanism beside it. Every action has parameters; most
//! declare none, and for those the vocabulary collapses back to the
//! nullary call it always was.
//!
//! Declaring them is what makes the surface drivable by a caller that
//! cannot see the screen: enumeration says *what to pass*, so an
//! argument is never guessed from a label. Binding is strict —
//! [`ActionArgs::bind`] refuses a missing, unknown or wrong-typed
//! argument **before** the action runs, so a bad call is a refusal and
//! never a partial mutation.

use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjId};

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};

use super::{ResourceKind, SearchRegistry, SearchResult};

/// The type of one action parameter. Deliberately small: these are
/// values a caller with no screen can produce from a descriptor alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Text,
    Integer,
    Boolean,
}

impl ParamType {
    /// Wire spelling, and what an enumeration descriptor carries.
    pub fn label(self) -> &'static str {
        match self {
            ParamType::Text => "text",
            ParamType::Integer => "integer",
            ParamType::Boolean => "boolean",
        }
    }
}

/// One typed parameter an action declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionParam {
    pub name: &'static str,
    pub ty: ParamType,
    /// Whether an invocation must supply it. An optional parameter's
    /// default belongs to the action, and is stated in `description` —
    /// the catalogue does not invent one.
    pub required: bool,
    pub description: &'static str,
}

impl ActionParam {
    pub const fn required(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self {
            name,
            ty,
            required: true,
            description,
        }
    }

    pub const fn optional(name: &'static str, ty: ParamType, description: &'static str) -> Self {
        Self {
            name,
            ty,
            required: false,
            description,
        }
    }
}

/// One bound argument value.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
}

impl ArgValue {
    fn ty(&self) -> ParamType {
        match self {
            ArgValue::Text(_) => ParamType::Text,
            ArgValue::Integer(_) => ParamType::Integer,
            ArgValue::Boolean(_) => ParamType::Boolean,
        }
    }
}

/// Why a set of arguments does not satisfy an action's parameters.
///
/// Each variant names the parameter, because a caller that cannot see
/// the screen has nothing else to correct against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    Missing {
        name: String,
    },
    Unknown {
        name: String,
    },
    WrongType {
        name: String,
        expected: ParamType,
        got: ParamType,
    },
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::Missing { name } => {
                write!(f, "required argument `{name}` was not supplied")
            }
            ArgError::Unknown { name } => {
                write!(f, "`{name}` is not a parameter of this action")
            }
            ArgError::WrongType {
                name,
                expected,
                got,
            } => write!(
                f,
                "argument `{name}` must be {}, got {}",
                expected.label(),
                got.label()
            ),
        }
    }
}

/// Why an invocation did not happen, or did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// The arguments did not satisfy the declared parameters. Nothing
    /// ran; the document is untouched.
    Arguments(ArgError),
    /// The address and the arguments were both good, but the action
    /// could not do its job — there was nothing to act on.
    Refused(String),
}

impl ActionError {
    /// Stable machine-readable code, so a caller can tell "I addressed
    /// this wrongly" from "there was nothing here to do".
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

/// Arguments for one invocation, already checked against the action's
/// declared parameters.
///
/// There is no way to build one except through [`ActionArgs::bind`] or
/// [`ActionArgs::none`], so an action's body never has to re-validate:
/// if it holds an `ActionArgs`, every required parameter is present and
/// every value has the declared type. That is what keeps a bad call
/// from becoming a half-applied edit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionArgs {
    values: BTreeMap<String, ArgValue>,
}

impl ActionArgs {
    /// The empty argument set — what a nullary action is invoked with.
    pub fn none() -> Self {
        Self::default()
    }

    /// Check `supplied` against `params` and bind it.
    ///
    /// Fails on the first problem rather than accumulating: the caller
    /// has to fix them one at a time anyway, and a partial bind must
    /// never reach an action.
    pub fn bind(
        params: &[ActionParam],
        supplied: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<Self, ArgError> {
        let mut values: BTreeMap<String, ArgValue> = BTreeMap::new();
        for (name, value) in supplied {
            let Some(param) = params.iter().find(|p| p.name == name) else {
                // An unknown argument is refused rather than dropped. A
                // silently ignored argument is the failure mode this
                // whole surface exists to remove: the caller believes it
                // asked for something it did not get.
                return Err(ArgError::Unknown { name });
            };
            if value.ty() != param.ty {
                return Err(ArgError::WrongType {
                    name,
                    expected: param.ty,
                    got: value.ty(),
                });
            }
            values.insert(name, value);
        }
        for param in params.iter().filter(|p| p.required) {
            if !values.contains_key(param.name) {
                return Err(ArgError::Missing {
                    name: param.name.to_string(),
                });
            }
        }
        Ok(Self { values })
    }

    /// The text bound to `name`, if any.
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.values.get(name) {
            Some(ArgValue::Text(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The integer bound to `name`, if any.
    pub fn integer(&self, name: &str) -> Option<i64> {
        match self.values.get(name) {
            Some(ArgValue::Integer(i)) => Some(*i),
            _ => None,
        }
    }

    /// The boolean bound to `name`, if any.
    pub fn boolean(&self, name: &str) -> Option<bool> {
        match self.values.get(name) {
            Some(ArgValue::Boolean(b)) => Some(*b),
            _ => None,
        }
    }

    /// Whether anything is bound.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

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
        Self {
            provider: provider.into(),
            id: id.into(),
        }
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
    /// What an invocation of this action must carry. Empty for the
    /// nullary majority.
    ///
    /// Part of the descriptor rather than something a caller discovers
    /// by trying: enumeration is the only thing a caller that cannot see
    /// the screen has to work from, so an action whose parameters were
    /// not advertised would be un-invokable except by guesswork. It
    /// stays inert data — a parameter list is not a handle, so this adds
    /// nothing fireable to an enumeration result.
    pub params: Vec<ActionParam>,
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

    /// What this action's invocation must carry.
    pub fn params(&self) -> &[ActionParam] {
        &self.result.params
    }

    /// Invoke the action headlessly: run the provider's side-effect
    /// against the live CRDT (`doc`) and `chrome`.
    ///
    /// This is the same callback the palette fires on Enter — the whole
    /// contract of the action API is that it reaches this side-effect
    /// **without** ever constructing a scene, a window, or a widget
    /// tree. Whatever the provider mutates (a chrome overlay flag, a
    /// CRDT splice, a tab activation) is observable immediately.
    ///
    /// `args` is bound against the declared parameters **before**
    /// anything runs, so a call that does not satisfy them is refused
    /// with the document untouched rather than half-applied.
    pub fn invoke(
        &self,
        doc: &mut AutoCommit,
        root: &ObjId,
        chrome: &Chrome,
        args: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<(), ActionError> {
        let bound =
            ActionArgs::bind(&self.result.params, args).map_err(ActionError::Arguments)?;
        (self.result.on_open)(doc, root, chrome, &bound)
    }

    /// Invoke an action that declares no parameters.
    ///
    /// Sugar over [`Self::invoke`] with an empty argument set, kept
    /// because most of the surface is nullary and spelling that out at
    /// every call site would bury the one case that is not. An action
    /// that *does* require a parameter still refuses here — this is a
    /// shorter way to pass nothing, not a way to skip the check.
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
        selected
            .invoke_nullary(&mut doc, &root, &chrome)
            .expect("a nullary action invoked with no arguments must run");

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

    /// An argument the action never declared is refused rather than
    /// dropped. A dropped argument answers `ok` to a caller that asked
    /// for something it did not get — which, for a caller that cannot
    /// see the screen, is indistinguishable from success.
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

    /// A parameterised action refuses an empty invocation *before*
    /// running, so a bad call cannot leave a half-applied edit behind.
    /// Asserted through `SelectedAction::invoke`, which is the path the
    /// socket takes, rather than against `bind` alone.
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

        // Enumeration advertises the parameter, which is the only way a
        // caller that cannot see the screen learns to pass it.
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
        fn id(&self) -> &str {
            "params"
        }
        fn name(&self) -> &str {
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
                fetch_source: None,
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

    /// Every existing action declares no parameters, so the nullary call
    /// stays exactly what it was. This is the compatibility claim: the
    /// parameter machinery must not make a caller pass anything new.
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
