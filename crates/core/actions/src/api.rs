//! **The action API**: how a member that is not the app enumerates what
//! it has open and performs one of its exported operations.
//!
//! [`ResourceSpec`] trees already describe an app's surface and
//! [`dispatch`](crate::executor::dispatch) already performs one
//! operation on it; the missing step was naming a thing from outside the
//! process that built it — without which collaboration, agency and
//! scripting stay per-app features.
//!
//! [`survey`] and [`invoke`] stay separate so enumeration can never
//! cause an effect: a descriptor carries no callback, token or handle,
//! so read-only is the shape of the type, not a promise in a comment.
//!
//! Authority stays with the host, which resolves the caller against the
//! workspace's capability model before offering an address: an
//! operation the caller may not perform is *absent* from the survey,
//! never a flag to ignore.

use crate::ActionContract;
use crate::ActionParam;
use crate::action::{EncodedAction, ExecutionLocus, OperationKind};
use crate::context::ActionContext;
use crate::entry::Described;
use crate::executor::{ActionTransport, Dispatched, dispatch_by_with_context};
use crate::registry::ActionRegistry;
use crate::resource::{
    ActionInput, ActionSpec, Collects, KEY_SEP, ResourceSpec, ResourceViewMountSpec,
    ResourceViewRangeSpec,
};
use serde::{Deserialize, Serialize};

/// The code a resource answers a write with when what it changes is still on
/// its way from the workspace: nothing was made, and the same call under the
/// same invocation is asked again once that is here.
pub const NOT_READY: &str = "not_ready";

/// What such a write says in words, beside [`NOT_READY`]. A resource answers
/// it as the whole reason its operation failed, and the toolkit turns that
/// reason into [`ApiError::Unavailable`] with [`NOT_READY`]: a caller is told
/// to ask again, never that a write made nothing.
pub const NOT_READY_REASON: &str = "what this changes has not arrived from the workspace yet; ask \
     again";

/// The stable address of one exported operation: which resource, which
/// action. Both halves are the app's own ids — no screen position, no
/// list index — so the same address names the same operation across
/// layouts, themes, resolutions, and a caller with no display can work
/// from ids and labels alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActionAddress {
    /// The id the app publishes the one resource the operation acts on under:
    /// a control that stands for one row among many names its row, so the
    /// second task's Complete is not the first's.
    pub resource: String,
    /// The id the resource publishes the action under. A label, a declared
    /// id or a kind never stands in for it: the API resolves this pair and
    /// nothing else.
    pub action: String,
}

impl ActionAddress {
    pub fn new(resource: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            action: action.into(),
        }
    }

    /// The address of `action` as `resource` publishes it: what the API
    /// takes, what a window's control bound to it says it does, and what the
    /// survey lists — one address, which neither the window's composition
    /// moving the control nor the person's language renaming it changes.
    #[must_use]
    pub fn naming(resource: &ResourceSpec, action: &ActionSpec) -> Self {
        Self::new(resource.id.as_str(), action.id.as_str())
    }

    /// The address of `resource` and `action`, or `None` when either half
    /// cannot survive [`Self::encode`]: an empty id or one containing
    /// [`KEY_SEP`] would let two different pairs name one string.
    #[must_use]
    pub fn checked(resource: impl AsRef<str>, action: impl AsRef<str>) -> Option<Self> {
        let (resource, action) = (resource.as_ref(), action.as_ref());
        (!resource.is_empty()
            && !action.is_empty()
            && !resource.contains(KEY_SEP)
            && !action.contains(KEY_SEP))
        .then(|| Self::new(resource, action))
    }

    /// The single-string form, for transports that carry one id. [`KEY_SEP`]
    /// never appears in either half, so [`parse`](Self::parse) is unambiguous.
    pub fn encode(&self) -> String {
        format!("{}{KEY_SEP}{}", self.resource, self.action)
    }

    /// Recover an address from [`encode`](Self::encode). `None` when the string
    /// is not one, so a caller that made an address up is refused here rather
    /// than looked up against a half-parsed id.
    pub fn parse(s: &str) -> Option<Self> {
        let (resource, action) = s.split_once(KEY_SEP)?;
        Self::checked(resource, action)
    }
}

/// What an invocation of one action must carry, advertised on the
/// descriptor rather than discovered by trying — enumeration is the only
/// thing a caller that can't see the screen has to work from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InputSpec {
    /// Takes nothing. The majority.
    None,
    /// Takes one text argument. `prompt` and `placeholder` are the app's own
    /// words for it — the ones the palette would show a person — and `collects`
    /// says what kind of thing it names.
    Text {
        prompt: String,
        placeholder: String,
        collects: Collects,
    },
}

impl ActionDescriptor {
    /// What the action writes besides its own resource, as a reader is told it: `None` for an
    /// action that declares nothing else ([`ActionContract::effects`]).
    #[must_use]
    pub fn writes(&self) -> Option<String> {
        if self.contract.effects.is_empty() {
            return None;
        }
        let steps: Vec<String> = self
            .contract
            .effects
            .iter()
            .map(ToString::to_string)
            .collect();
        Some(format!("Writes: {}.", steps.join(", ")))
    }

    /// Whether this action changes only a document: a document operation, or
    /// one carried out on another document than the one its resource is kept
    /// in ([`crate::ROUTED_CHANGE_TAG`]). Such an action reads none of the
    /// content its resource refers to.
    #[must_use]
    pub fn changes_only_a_document(&self) -> bool {
        self.kind == OperationKind::Document
            || self
                .contract
                .tags
                .iter()
                .any(|tag| tag == crate::ROUTED_CHANGE_TAG)
    }

    /// The parameter this action's one line of typed text goes into, when it
    /// is a prompted action: what an agent or a host holding only the
    /// descriptor learns instead of assuming a name.
    #[must_use]
    pub fn prompt_param(&self) -> Option<&str> {
        match self.input {
            InputSpec::Text { .. } => self
                .params
                .iter()
                .find(|param| param.entry)
                .or_else(|| self.params.iter().find(|param| param.required))
                .or_else(|| self.params.first())
                .map(|param| param.name.as_ref()),
            InputSpec::None => None,
        }
    }

    /// The resource's own id, not the transport alias the window published
    /// it under (`ResourceSpec::domain_name`): the address a directory row
    /// carries, resolved back to its alias where the window published one.
    #[must_use]
    pub fn domain_resource(&self) -> &str {
        match &self.semantic_identity {
            Some(identity) if identity.is_valid() => identity.key.as_str(),
            _ => self.address.resource.as_str(),
        }
    }

    /// The parameter one line of typed text fills, when the action is a
    /// capture: it declares an entry (`title: Text (entry)`), what a person
    /// writes down from wherever they are and what a palette offers as "New
    /// task", run with the line they typed.
    #[must_use]
    pub fn entry_param(&self) -> Option<&str> {
        self.params
            .iter()
            .find(|param| param.entry)
            .map(|param| param.name.as_ref())
    }

    /// `arguments` as this action takes them. A text argument on the wire
    /// (`InvokeArgument::Text`) names no parameter and reaches a host as a lone
    /// [`PROMPTED_PARAM`](crate::PROMPTED_PARAM); it is moved under this
    /// action's own prompt parameter. Anything else is returned as given.
    #[must_use]
    pub fn bind_prompt_text(
        &self,
        mut arguments: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        if arguments.len() == 1
            && let Some(name) = self.prompt_param()
            && name != crate::PROMPTED_PARAM
            && let Some(text) = arguments.remove(crate::PROMPTED_PARAM)
        {
            arguments.insert(name.to_owned(), text);
        }
        arguments
    }
}

impl InputSpec {
    fn of(input: ActionInput<'_>) -> Self {
        match input {
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => InputSpec::None,
            ActionInput::Prompted {
                prompt,
                placeholder,
                collects,
            } => InputSpec::Text {
                prompt: prompt.to_owned(),
                placeholder: placeholder.to_owned(),
                collects,
            },
        }
    }

    /// Whether an invocation must supply text.
    pub fn wants_text(&self) -> bool {
        matches!(self, InputSpec::Text { .. })
    }

    /// Whether the text it wants is the address of content rather than
    /// words — words can be quoted out of the request, a content hash cannot.
    pub fn wants_content(&self) -> bool {
        matches!(
            self,
            InputSpec::Text {
                collects: Collects::Content,
                ..
            }
        )
    }
}

/// One exported operation, as inert data: everything a caller needs to
/// decide whether to perform it, nothing that could perform it — an
/// [`ActionSpec`]'s closure never appears here or crosses a boundary,
/// only the address does.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionDescriptor {
    pub address: ActionAddress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_identity: Option<crate::ResourceSemanticIdentity>,
    /// The publishing resource's stable kind tag (`word_processor.section`,
    /// `files.entry`), shared with the OS-wide type registry so the same kind
    /// means the same thing across apps.
    pub resource_kind: String,
    /// The resource's human label, as the app words it.
    pub resource_label: String,
    pub action_label: String,
    /// The other words people use for the action ([`ActionSpec::keywords`]),
    /// published with its label so every search ranks them as it ranks the
    /// label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// What the action did, said after who did it ([`ActionSpec::did`]):
    /// the past-tense form its app declares, when it declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    /// The action's own picture ([`ActionSpec::icon`]), by its Lucide name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub description: Option<String>,
    /// What the action's row shows of the state it acts on
    /// ([`ActionSpec::state`](crate::ActionType::state)).
    #[serde(default, skip_serializing_if = "crate::ActionState::is_empty")]
    pub state: crate::ActionState,
    pub locus: ExecutionLocus,
    pub kind: OperationKind,
    pub contract: ActionContract,
    pub input: InputSpec,
    /// Canonical typed invocation surface. `input` remains the palette-facing
    /// projection for compatibility; headless callers use these parameters.
    pub params: Vec<ActionParam>,
    /// The explicit resource view opened by this action. Mount actions and
    /// unrelated local view operations carry `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ResourceViewDescriptor>,
}

/// One step an action takes on a record other than the resource it is declared on: the app
/// whose record it writes, the record's kind, and the verb it runs there. A step that is an
/// action a device's provider offers names that provider as its app and the resource it
/// offers the action on as its kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActionEffect {
    pub app: String,
    pub kind: String,
    pub verb: String,
}

impl ActionEffect {
    /// The verb of an undo declared as a step ([`Self::undo`]).
    pub const UNDO: &'static str = "undo";

    /// Taking back changes of the records `app` keeps, whichever kinds the changes wrote: an undo
    /// declared as an action's step, which a reader admits as it admits any declared step.
    #[must_use]
    pub fn undo(app: impl Into<String>) -> Self {
        Self::new(app, "", Self::UNDO)
    }

    /// Whether this is the undo of `app`'s records ([`Self::undo`]).
    #[must_use]
    pub fn undoes_in(&self, app: &str) -> bool {
        self.app == app && self.kind.is_empty() && self.verb == Self::UNDO
    }

    /// The step `verb` on records of `kind` that `app` keeps.
    #[must_use]
    pub fn new(app: impl Into<String>, kind: impl Into<String>, verb: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            kind: kind.into(),
            verb: verb.into(),
        }
    }
}

impl std::fmt::Display for ActionEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // An undo names no kind: it takes back changes of the app's records, whichever.
        if self.kind.is_empty() {
            return write!(formatter, "{}#{}", self.app, self.verb);
        }
        write!(formatter, "{} {}#{}", self.app, self.kind, self.verb)
    }
}

/// Serializable data describing one explicitly declared resource view.
///
/// This is projected only onto the descriptor for `open_action`, keeping the
/// view identity independent from the operation used to present it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceViewDescriptor {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub open_action: String,
    pub primary: bool,
    pub mount: Option<ResourceViewMountSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ResourceViewRangeSpec>,
    /// The view opens on the whole desktop ([`crate::ResourceViewSpec::desktop`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub desktop: bool,
}

impl ResourceViewDescriptor {
    /// The view `resource` declares for `action_id`, when that action is a
    /// view's open action.
    #[must_use]
    pub fn declared_for(resource: &ResourceSpec, action_id: &str) -> Option<Self> {
        resource
            .views
            .iter()
            .find(|view| view.open_action == action_id)
            .map(|view| Self {
                id: view.id.clone(),
                label: view.label.clone(),
                description: view.description.clone(),
                open_action: view.open_action.clone(),
                primary: view.primary,
                mount: view.mount.clone(),
                range: view.range.clone(),
                desktop: view.desktop,
            })
    }
}

/// Why answering a question was refused: it is the asked person's to answer, and the caller is
/// not them. A question a request raised is the person's decision, never its asker's, and no
/// agent answers another's on their behalf ([`ApiError::Refused`]'s code).
pub const NOT_THE_ASKED_PERSON: &str = "question-not-yours";

/// Why an address could not be performed. `Unknown` is "nothing publishes this address"; an action
/// published there that the caller's permissions keep from them is refused as `permission_denied`
/// ([`ApiError::Unavailable`]), saying why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// Nothing in the tree carries this address.
    Unknown(ActionAddress),
    /// The action takes text and none was supplied.
    MissingArgument(ActionAddress),
    /// The action takes nothing and an argument was supplied. Refused, not
    /// dropped — a silently ignored argument leaves the caller believing it
    /// asked for something it didn't get.
    UnexpectedArgument(ActionAddress),
    /// Typed arguments failed the action's declared schema.
    InvalidArguments(String),
    /// A typed argument is a number outside the range its parameter
    /// declares: [`InvalidArguments`](Self::InvalidArguments) whose bound
    /// reaches the caller as data ([`ApiError::failure_details`]).
    ArgumentOutOfRange {
        param: String,
        violation: crate::params::ArgViolation,
    },
    /// The address exists and is intentionally advertised, but its current
    /// precondition is not satisfied.
    Unavailable { code: String, reason: String },
    /// The action contract requires an explicit confirmation step that this
    /// invocation did not carry.
    ConfirmationRequired,
    /// The app never registered a decoder for the action's type, so the API
    /// can't prove what it would run. Fail-closed: a refusal, not a
    /// best-effort apply.
    UnregisteredType(String),
    /// The app declined for a reason it named by a stable code (the thing it
    /// would add is there already, say), in words beside it for whoever
    /// reads: a caller decides on the code without reading the words.
    Refused { code: String, reason: String },
    /// The action ran and failed.
    Failed(String),
}

impl ApiError {
    /// Why an encoded action could not be turned back into one: no decoder
    /// for its type, or a payload the registered decoder does not read (the
    /// code that encoded it and the code decoding it disagree). The second is
    /// a fault of its own and says so; reporting it as an unknown type sends
    /// whoever reads it looking for a registration that is there.
    pub fn decoding(error: crate::action::ActionError) -> Self {
        match error {
            crate::action::ActionError::UnknownType(type_id) => ApiError::UnregisteredType(type_id),
            other => ApiError::refused(other),
        }
    }

    /// `error` as the caller reads it: an app that named why keeps its code
    /// as data, so a caller decides on it without reading the words.
    pub fn refused(error: crate::action::ActionError) -> Self {
        match error {
            crate::action::ActionError::Unavailable { code, message } => ApiError::Refused {
                code,
                reason: message,
            },
            other => ApiError::Failed(other.to_string()),
        }
    }

    /// Stable machine-readable code, distinguishing "I addressed this
    /// wrongly" from "it ran and could not do its job".
    pub fn code(&self) -> &str {
        match self {
            ApiError::Unknown(_) | ApiError::UnregisteredType(_) => "unknown_action",
            ApiError::MissingArgument(_)
            | ApiError::UnexpectedArgument(_)
            | ApiError::InvalidArguments(_)
            | ApiError::ArgumentOutOfRange { .. } => "invalid_arguments",
            ApiError::Unavailable { .. } => "action_unavailable",
            // The app's own code, which is why it gave one.
            ApiError::Refused { code, .. } => code,
            ApiError::ConfirmationRequired => "action_confirmation_required",
            ApiError::Failed(_) => "action_failed",
        }
    }

    /// What an [`ActionFailure`](crate::ActionFailure) reporting this
    /// refusal carries beside its message, for a caller to act on without
    /// reading prose: `{"param": …, "violation": …}` for an argument out of
    /// its declared range, `{"code": …, "reason": …}` for an action that is unavailable, so a
    /// caller tells why (the person's own view, a policy) by its code.
    #[must_use]
    pub fn failure_details(&self) -> Option<serde_json::Value> {
        match self {
            ApiError::ArgumentOutOfRange { param, violation } => {
                Some(serde_json::json!({ "param": param, "violation": violation }))
            }
            ApiError::Unavailable { code, reason } => {
                Some(serde_json::json!({ "code": code, "reason": reason }))
            }
            _ => None,
        }
    }
}

/// An argument out of its declared range keeps its bound as data; every
/// other argument refusal is reported in words.
impl From<crate::params::ArgError> for ApiError {
    fn from(error: crate::params::ArgError) -> Self {
        match error {
            crate::params::ArgError::OutOfRange { name, violation } => {
                ApiError::ArgumentOutOfRange {
                    param: name,
                    violation,
                }
            }
            error => ApiError::InvalidArguments(error.to_string()),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unknown(a) => {
                write!(f, "no action addressed by ({}, {})", a.resource, a.action)
            }
            ApiError::MissingArgument(a) => write!(
                f,
                "action ({}, {}) needs a text argument and none was given",
                a.resource, a.action
            ),
            ApiError::UnexpectedArgument(a) => {
                write!(f, "action ({}, {}) takes no argument", a.resource, a.action)
            }
            ApiError::InvalidArguments(why) | ApiError::Failed(why) => write!(f, "{why}"),
            ApiError::Refused { reason, .. } => f.write_str(reason),
            ApiError::ArgumentOutOfRange { param, violation } => {
                write!(f, "argument `{param}` {violation}")
            }
            ApiError::Unavailable { code, reason } => write!(f, "{code}: {reason}"),
            ApiError::ConfirmationRequired => {
                f.write_str("this action requires explicit confirmation")
            }
            ApiError::UnregisteredType(t) => {
                write!(f, "no decoder registered for action type `{t}`")
            }
        }
    }
}

impl std::error::Error for ApiError {}

/// Walk a resource forest and describe every operation it exports.
/// Depth-first, parents before children, matching
/// [`flatten`](crate::resource::flatten)'s order. Borrows the tree,
/// returns owned data — nothing handed back can act on what it described.
pub fn survey(resources: &[ResourceSpec]) -> Vec<ActionDescriptor> {
    survey_placed(resources)
        .into_iter()
        .map(|row| row.action)
        .collect()
}

/// Whether `a` and `b` publish the same: every resource as a reader sees it and every action as
/// a survey describes it. A target bound anew to the same item publishes the same, so a surface
/// that described itself again after drawing is told from one whose offer moved.
#[must_use]
pub fn publishes_same(a: &[ResourceSpec], b: &[ResourceSpec]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.id == b.id
                && a.kind == b.kind
                && a.label == b.label
                && a.presentation == b.presentation
                && a.semantic_identity == b.semantic_identity
                && a.app_id == b.app_id
                && a.views == b.views
                && a.personal == b.personal
                && a.workspace == b.workspace
                && a.resource_type == b.resource_type
                && a.actions.len() == b.actions.len()
                && a.actions
                    .iter()
                    .zip(&b.actions)
                    .all(|(x, y)| x.bound() == y.bound() && describe(a, x) == describe(b, y))
                && publishes_same(&a.children, &b.children)
        })
}

/// One exported operation of a surveyed forest, in the row it stands in: the
/// resource of the level above it, and whether its own resource holds rows
/// below it, which a listing asked with that resource as its parent answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedAction {
    /// The resource this row hangs under; empty at the top level.
    pub parent: String,
    /// Whether a query with this row's resource as parent answers more rows.
    pub has_children: bool,
    pub action: ActionDescriptor,
}

/// Walk a resource forest and describe every operation it exports, each in
/// the row a listing publishes it in: [`survey`] flattened, with the resource
/// it stands under and whether its own resource holds rows below it, so a
/// caller reading a level at a time knows where a level continues.
#[must_use]
pub fn survey_placed(resources: &[ResourceSpec]) -> Vec<PlacedAction> {
    let mut out = Vec::new();
    let _ = survey_placed_each(resources, &mut |placed| {
        out.push(placed);
        true
    });
    out
}

/// [`survey_placed`], one row at a time and only as far as `visit` asks:
/// it answers whether to go on, so a caller listing a page of a record that
/// publishes thousands of actions builds the rows up to its page and no
/// more. `false` when it stopped, `true` when it saw every row.
pub fn survey_placed_each(
    resources: &[ResourceSpec],
    visit: &mut dyn FnMut(PlacedAction) -> bool,
) -> bool {
    resources
        .iter()
        .all(|resource| placed_each(resource, "", visit))
}

/// [`survey_placed_each`] of one level: the rows of `children`, the
/// resources `parent` holds (`""` for the top), and none further down, which
/// stand under those. What a listing of one level walks, however much its
/// resources hold. `false` when `visit` stopped it.
pub fn survey_placed_level(
    children: &[ResourceSpec],
    parent: &str,
    visit: &mut dyn FnMut(PlacedAction) -> bool,
) -> bool {
    survey_placed_level_items(children, parent, &mut |_, placed| visit(placed))
}

/// [`survey_placed_level`] with where each row stands: `visit` is told the
/// index in `children` of the resource the action is on, so a caller listing
/// a page counts what each resource admits and keeps its place from one page
/// to the next. `false` when `visit` stopped it.
pub fn survey_placed_level_items(
    children: &[ResourceSpec],
    parent: &str,
    visit: &mut dyn FnMut(usize, PlacedAction) -> bool,
) -> bool {
    children.iter().enumerate().all(|(at, child)| {
        let has_children = !child.children.is_empty();
        child.actions.iter().all(|action| {
            visit(
                at,
                PlacedAction {
                    parent: parent.to_owned(),
                    has_children,
                    action: describe(child, action),
                },
            )
        })
    })
}

/// [`survey_placed_each`] for one resource's subtree, as a child of
/// `parent`: the items a listing keeps past the published page, walked as
/// the tree would walk them.
pub fn survey_placed_under(
    resource: &ResourceSpec,
    parent: &str,
    visit: &mut dyn FnMut(PlacedAction) -> bool,
) -> bool {
    placed_each(resource, parent, visit)
}

fn placed_each(
    resource: &ResourceSpec,
    parent: &str,
    visit: &mut dyn FnMut(PlacedAction) -> bool,
) -> bool {
    let has_children = !resource.children.is_empty();
    for action in &resource.actions {
        let go_on = visit(PlacedAction {
            parent: parent.to_owned(),
            has_children,
            action: describe(resource, action),
        });
        if !go_on {
            return false;
        }
    }
    for child in &resource.children {
        if !placed_each(child, resource.id.as_str(), visit) {
            return false;
        }
    }
    true
}

/// `action` of `resource`, as inert data: what a survey hands back.
fn describe(resource: &ResourceSpec, action: &ActionSpec) -> ActionDescriptor {
    let view = ResourceViewDescriptor::declared_for(resource, &action.id);
    ActionDescriptor {
        address: ActionAddress::new(resource.id.as_str(), &action.id),
        semantic_identity: resource.semantic_identity.clone(),
        resource_kind: resource.kind.clone(),
        resource_label: resource.label.clone(),
        action_label: action.label.clone(),
        keywords: action.keywords.clone(),
        did: action.did.clone(),
        icon: action.icon.clone(),
        description: action.description.clone(),
        state: action.state.clone(),
        locus: action.locus,
        kind: action.kind,
        contract: action.contract.clone(),
        input: InputSpec::of(action.input()),
        params: action.params().to_vec(),
        view,
    }
}

/// Resolve `address` against `resources` without running anything, so a caller
/// can check what an address currently names before committing to it.
pub fn resolve<'a>(
    resources: &'a [ResourceSpec],
    address: &ActionAddress,
) -> Option<&'a ActionSpec> {
    locate(resources, address).map(|(_, spec)| spec)
}

/// [`resolve`], with the resource that publishes the action: what the API
/// needs to place the action in the tree it grants over.
pub fn locate<'a>(
    resources: &'a [ResourceSpec],
    address: &ActionAddress,
) -> Option<(&'a ResourceSpec, &'a ActionSpec)> {
    fn walk<'a>(
        r: &'a ResourceSpec,
        address: &ActionAddress,
    ) -> Option<(&'a ResourceSpec, &'a ActionSpec)> {
        if r.id.as_str() == address.resource
            && let Some(spec) = r.actions.iter().find(|a| a.id == address.action)
        {
            return Some((r, spec));
        }
        r.children.iter().find_map(|c| walk(c, address))
    }
    resources.iter().find_map(|r| walk(r, address))
}

/// The address of the action `action` that the resource `resource` in the
/// tree `resources` publishes, or `None` when the tree publishes no such
/// action: a caller holding a published tree takes its addresses from it
/// (see [`ResourceSpec::action_address`]).
pub fn published(
    resources: &[ResourceSpec],
    resource: &str,
    action: &str,
) -> Option<ActionAddress> {
    fn walk(r: &ResourceSpec, resource: &str, action: &str) -> Option<ActionAddress> {
        if r.id.as_str() == resource
            && let Some(address) = r.action_address(action)
        {
            return Some(address);
        }
        r.children.iter().find_map(|c| walk(c, resource, action))
    }
    resources.iter().find_map(|r| walk(r, resource, action))
}

/// The action `action` that the resource `resource` in the tree `resources`
/// publishes, or `None`: [`resolve`] of the address the tree publishes it at
/// ([`published`]).
pub fn spec_of<'a>(
    resources: &'a [ResourceSpec],
    resource: &str,
    action: &str,
) -> Option<&'a ActionSpec> {
    published(resources, resource, action).and_then(|address| resolve(resources, &address))
}

/// Turn an address plus its argument into the replayable action it
/// names. Split out from [`invoke`] so a host can build/inspect/route
/// the encoded action itself, and so argument rules are checked in one
/// place. Binding happens *before* anything runs, so a bad call is a
/// refusal with the document untouched, never a half-applied edit.
///
/// # Errors
///
/// Returns [`ApiError::Unknown`] if `address` names no resource/action
/// in `resources`; [`ApiError::UnexpectedArgument`] if `argument` is
/// `Some` for an [`ActionInput::Immediate`] action; or
/// [`ApiError::MissingArgument`] if it's `None` for an
/// [`ActionInput::Prompted`] one whose text is required.
pub fn encode(
    resources: &[ResourceSpec],
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<EncodedAction, ApiError> {
    let spec = resolve(resources, address).ok_or_else(|| ApiError::Unknown(address.clone()))?;
    ensure_available(spec)?;
    spec.validate_deferred_shape()
        .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
    match (spec.input(), argument) {
        (ActionInput::Deferred { type_id }, None) => {
            let Some(encoded) = spec.build_deferred() else {
                return Err(ApiError::Unknown(address.clone()));
            };
            if encoded.type_id == crate::resource::INVALID_ARGUMENTS_TYPE {
                return Err(ApiError::InvalidArguments(
                    String::from_utf8_lossy(&encoded.payload).into_owned(),
                ));
            }
            if encoded.type_id != *type_id {
                return Err(ApiError::InvalidArguments(format!(
                    "builder returned `{}` but declared `{type_id}`",
                    encoded.type_id
                )));
            }
            Ok(encoded)
        }
        (ActionInput::Deferred { .. }, Some(_)) => {
            Err(ApiError::UnexpectedArgument(address.clone()))
        }
        (ActionInput::Immediate(_), None) => spec
            .encode_immediate()
            .map(std::sync::Arc::unwrap_or_clone)
            .ok_or_else(|| ApiError::Unknown(address.clone())),
        (ActionInput::Immediate(_), Some(_)) => Err(ApiError::UnexpectedArgument(address.clone())),
        (ActionInput::Prompted { .. }, Some(text)) => {
            let Some(encoded) = spec.build_text(text) else {
                return Err(ApiError::Unknown(address.clone()));
            };
            if encoded.type_id == crate::resource::INVALID_ARGUMENTS_TYPE {
                return Err(ApiError::InvalidArguments(
                    String::from_utf8_lossy(&encoded.payload).into_owned(),
                ));
            }
            Ok(encoded)
        }
        // A prompt whose text is optional (a choice with a default) is
        // invoked without it through the typed parameters, as any caller
        // omitting an optional argument is.
        (ActionInput::Prompted { .. }, None)
            if !spec.params().is_empty() && spec.params().iter().all(|param| !param.required) =>
        {
            let encoded = spec
                .build_json(&serde_json::Map::new())
                .map_err(ApiError::from)?;
            if encoded.type_id == crate::resource::INVALID_ARGUMENTS_TYPE {
                return Err(ApiError::InvalidArguments(
                    String::from_utf8_lossy(&encoded.payload).into_owned(),
                ));
            }
            Ok(encoded)
        }
        (ActionInput::Prompted { .. }, None) => Err(ApiError::MissingArgument(address.clone())),
    }
}

/// Build an action from the canonical typed JSON argument object advertised
/// by [`ActionDescriptor::params`]. Binding is strict and completes before any
/// document mutation or application effect is attempted.
///
/// # Errors
///
/// Returns [`ApiError::Unknown`] when the address is not published,
/// [`ApiError::Unavailable`] when policy currently disables it, or
/// [`ApiError::InvalidArguments`] (or [`ApiError::ArgumentOutOfRange`])
/// when the object violates its parameter contract.
pub fn encode_json(
    resources: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<EncodedAction, ApiError> {
    let spec = resolve(resources, address).ok_or_else(|| ApiError::Unknown(address.clone()))?;
    ensure_available(spec)?;
    let encoded = spec.build_json(arguments).map_err(ApiError::from)?;
    if encoded.type_id == crate::resource::INVALID_ARGUMENTS_TYPE {
        return Err(ApiError::InvalidArguments(
            String::from_utf8_lossy(&encoded.payload).into_owned(),
        ));
    }
    Ok(encoded)
}

/// Admit one invocation of `address` with `arguments` against the tree `resources` publishes, as
/// a provider serving that tree answers a caller: resolved, available, confirmed as its contract
/// asks of these arguments by `confirmation` (the caller's own word or the phrase it names; a
/// token naming a person's answer is not this provider's to spend), and bound strictly into the
/// action's own encoded form.
///
/// # Errors
///
/// [`ApiError::Unknown`] for an address the tree does not offer, [`ApiError::Unavailable`] for an
/// action that cannot run now, [`ApiError::ConfirmationRequired`] when the confirmation is missing
/// or wrong, and any error [`encode_json`] documents for the arguments.
pub fn admit(
    resources: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    confirmation: Option<&crate::remote::Confirmation>,
) -> Result<EncodedAction, ApiError> {
    use crate::remote::Confirmation;
    let spec = resolve(resources, address).ok_or_else(|| ApiError::Unknown(address.clone()))?;
    ensure_available(spec)?;
    let confirmed = match spec.confirmation_for_json(arguments) {
        crate::ConfirmationPolicy::None => true,
        crate::ConfirmationPolicy::Phrase { phrase } => {
            matches!(confirmation, Some(Confirmation::Phrase(given)) if *given == phrase)
        }
        crate::ConfirmationPolicy::Confirm => matches!(confirmation, Some(Confirmation::Confirmed)),
    };
    if !confirmed {
        return Err(ApiError::ConfirmationRequired);
    }
    encode_json(resources, address, arguments)
}

fn ensure_available(spec: &ActionSpec) -> Result<(), ApiError> {
    match &spec.contract.availability {
        crate::Availability::Available => Ok(()),
        crate::Availability::Unavailable { code, reason } => Err(ApiError::Unavailable {
            code: code.clone(),
            reason: reason.clone(),
        }),
    }
}

/// Caller identity and optional semantic context for an invoked action.
///
/// Every API entry point takes one, so an invocation made inside a task or
/// agent run — confirmed or not, text or typed arguments — is recorded with
/// that run's context rather than only the resource's own.
#[derive(Clone, Copy, Debug)]
pub struct InvocationProvenance<'a> {
    /// Automerge actor bytes attributed to the resulting change.
    pub actor: &'a [u8],
    /// Task or session context used to enrich the resource's own context.
    pub ambient: Option<&'a ActionContext>,
}

impl<'a> InvocationProvenance<'a> {
    /// Provenance naming only the caller, for an invocation made outside any
    /// task or run.
    #[must_use]
    pub const fn caller(actor: &'a [u8]) -> Self {
        Self {
            actor,
            ambient: None,
        }
    }

    /// The same caller acting within `ambient`, e.g. an agent run's intent
    /// group.
    #[must_use]
    pub const fn within(self, ambient: &'a ActionContext) -> Self {
        Self {
            actor: self.actor,
            ambient: Some(ambient),
        }
    }
}

/// Perform the operation `address` names, attributed to
/// `provenance.actor`. Resolve, bind, decode through the app's own registry,
/// then [`dispatch_by`](crate::executor::dispatch_by) honouring the action's locus. The actor is the
/// caller's Automerge actor bytes, so the workspace's history names who asked;
/// `provenance.ambient` carries the task or run the call belongs to. The
/// resource's concrete action label and address remain authoritative; app,
/// purpose, group, and facets of the ambient context fill only fields the
/// resource did not supply.
///
/// `resources` must be a tree built *now*, not the one an earlier
/// [`survey`] walked — re-resolving turns an address that has since
/// stopped naming anything into a refusal rather than an edit to
/// whatever took its place. Decoding through `registry` (rather than
/// trusting the tree) makes this fail-closed: an unregistered action
/// type can't run, and the decoder used here is the one a replaying peer
/// would use too.
///
/// # Errors
///
/// Returns any error [`encode`] documents; [`ApiError::ConfirmationRequired`]
/// when the action's effective confirmation for this argument is not
/// [`None`](crate::ConfirmationPolicy::None) (see [`invoke_confirmed`]);
/// [`ApiError::UnregisteredType`] if `registry` has no decoder for the
/// encoded action's type; or [`ApiError::Failed`] if [`dispatch_by`](crate::executor::dispatch_by) fails
/// once decoded.
pub fn invoke(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    argument: Option<&str>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ApiError> {
    invoke_with_consent(
        doc, resources, address, argument, provenance, registry, transport, false,
    )
}

/// [`invoke`] after an outer policy boundary has completed the action
/// contract's confirmation step. This is intentionally separate from
/// [`invoke`], whose default is fail-closed for consequential actions.
pub fn invoke_confirmed(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    argument: Option<&str>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ApiError> {
    invoke_with_consent(
        doc, resources, address, argument, provenance, registry, transport, true,
    )
}

fn invoke_with_consent(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    argument: Option<&str>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
    confirmed: bool,
) -> Result<Dispatched, ApiError> {
    ensure_confirmation(resources, address, confirmed, |spec| {
        spec.confirmation_for_argument(argument)
    })?;
    ensure_deferred_local_owner(resources, address, registry)?;
    let encoded = encode(resources, address, argument)?;
    dispatch_encoded_with_context(
        doc, resources, address, encoded, provenance, registry, transport,
    )
}

/// Typed-argument counterpart to [`invoke`].
pub fn invoke_json(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ApiError> {
    invoke_json_with_consent(
        doc, resources, address, arguments, provenance, registry, transport, false,
    )
}

/// Typed-argument invocation after an outer policy boundary has completed the
/// action contract's confirmation step.
pub fn invoke_json_confirmed(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ApiError> {
    invoke_json_with_consent(
        doc, resources, address, arguments, provenance, registry, transport, true,
    )
}

fn invoke_json_with_consent(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
    confirmed: bool,
) -> Result<Dispatched, ApiError> {
    ensure_confirmation(resources, address, confirmed, |spec| {
        spec.confirmation_for_json(arguments)
    })?;
    ensure_deferred_local_owner(resources, address, registry)?;
    let encoded = encode_json(resources, address, arguments)?;
    dispatch_encoded_with_context(
        doc, resources, address, encoded, provenance, registry, transport,
    )
}

/// Refuse an unconfirmed invocation whose effective confirmation — for these
/// exact arguments — is not [`None`](crate::ConfirmationPolicy::None).
fn ensure_confirmation(
    resources: &[ResourceSpec],
    address: &ActionAddress,
    confirmed: bool,
    policy: impl FnOnce(&ActionSpec) -> crate::ConfirmationPolicy,
) -> Result<(), ApiError> {
    let spec = resolve(resources, address).ok_or_else(|| ApiError::Unknown(address.clone()))?;
    if !matches!(policy(spec), crate::ConfirmationPolicy::None) && !confirmed {
        return Err(ApiError::ConfirmationRequired);
    }
    Ok(())
}

// This low-level transport accepts already encoded actions. A Deferred server
// operation must instead reach the SDK's address-based broker API before mint.
fn ensure_deferred_local_owner(
    resources: &[ResourceSpec],
    address: &ActionAddress,
    registry: &ActionRegistry,
) -> Result<(), ApiError> {
    let spec = resolve(resources, address).ok_or_else(|| ApiError::Unknown(address.clone()))?;
    if spec.deferred_invocation_type().is_some()
        || matches!(spec.input(), ActionInput::Deferred { .. })
    {
        spec.validate_deferred_shape()
            .map_err(|error| ApiError::InvalidArguments(error.to_string()))?;
        if spec.kind != OperationKind::Document || spec.locus != ExecutionLocus::ClientLocal {
            return Err(ApiError::Unavailable {
                code: "deferred_owner_required".into(),
                reason: "This creation must use the owning application's address-based action API"
                    .into(),
            });
        }
        if let Some(type_id) = spec.deferred_invocation_type()
            && !registry.contains(type_id)
        {
            return Err(ApiError::UnregisteredType(type_id.into()));
        }
    }
    Ok(())
}

fn dispatch_encoded_with_context(
    doc: &mut crate::action::Doc,
    resources: &[ResourceSpec],
    address: &ActionAddress,
    encoded: EncodedAction,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ApiError> {
    let resource = find_resource(resources, &address.resource)
        .ok_or_else(|| ApiError::Unknown(address.clone()))?;
    let spec = resource
        .actions
        .iter()
        .find(|candidate| candidate.id == address.action)
        .ok_or_else(|| ApiError::Unknown(address.clone()))?;
    if spec.kind != OperationKind::Document {
        return Err(ApiError::Failed(format!(
            "{} operation must be handled by the owning application",
            spec.kind.as_str()
        )));
    }
    let mut context = spec.history_context(resource.id.as_str(), &resource.kind, &resource.label);
    if let Some(ambient) = provenance.ambient {
        context = context.enriched_by(ambient);
    }
    // The published action is the verb history names, unless the value
    // describes itself or the action only carries others.
    if !spec.carries_actions()
        && let Some(described) = Described::of_published(
            &encoded,
            crate::ActionKey::new("", &resource.kind, &spec.id),
        )
    {
        context = context.with_described(described);
    }
    let action = registry.decode(&encoded).map_err(ApiError::decoding)?;
    if spec.deferred_invocation_type().is_some() && action.locus() != spec.locus {
        return Err(ApiError::Failed(
            "Deferred action locus differs from its declaration".into(),
        ));
    }
    dispatch_by_with_context(
        doc,
        action.as_ref(),
        encoded,
        transport,
        Some(provenance.actor),
        context,
    )
    .map_err(ApiError::refused)
}

/// The resource `id` in `resources` or among their children.
#[must_use]
pub fn find_resource<'a>(resources: &'a [ResourceSpec], id: &str) -> Option<&'a ResourceSpec> {
    fn walk<'a>(resource: &'a ResourceSpec, id: &str) -> Option<&'a ResourceSpec> {
        if resource.id.as_str() == id {
            return Some(resource);
        }
        resource.children.iter().find_map(|child| walk(child, id))
    }
    resources.iter().find_map(|resource| walk(resource, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, ActionError, Doc};
    use crate::executor::{LocalTransport, replay};
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};

    const NOTE_TYPE: &str = "test.note";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum Note {
        Star,
        Retitle(String),
    }

    impl Action for Note {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                Note::Star => {
                    doc.put(&ROOT, "starred", ScalarValue::Boolean(true))?;
                }
                Note::Retitle(title) => {
                    doc.put(&ROOT, "title", ScalarValue::Str(title.as_str().into()))?;
                }
            }
            Ok(())
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Note>(NOTE_TYPE);
        r
    }

    fn encoded(action: &Note) -> EncodedAction {
        EncodedAction::encode(NOTE_TYPE, action).unwrap()
    }

    /// One note publishing an immediate action and a prompted one, as an app
    /// would. `star_does` / `retitle_does` decide what each *performs* while
    /// leaving every id and label identical.
    fn note(star_does: &Note, retitle_does: fn(&str) -> Note) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new("notes.note:1", "notes.note", "Harbour")
                .with_action(ActionSpec::immediate("star", "Star", NOTE_TYPE, star_does).unwrap())
                .with_action(ActionSpec::prompted_param(
                    "retitle",
                    "Rename",
                    ActionParam::required("title", crate::ParamType::Text, "New title"),
                    "title",
                    ExecutionLocus::ClientLocal,
                    move |text| encoded(&retitle_does(text)),
                )),
        ]
    }

    fn open() -> Vec<ResourceSpec> {
        note(&Note::Star, |text| Note::Retitle(text.to_string()))
    }

    fn star() -> ActionAddress {
        ActionAddress::new("notes.note:1", "star")
    }

    fn retitle() -> ActionAddress {
        ActionAddress::new("notes.note:1", "retitle")
    }

    fn perform(
        doc: &mut Doc,
        tree: &[ResourceSpec],
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<Dispatched, ApiError> {
        invoke(
            doc,
            tree,
            address,
            argument,
            InvocationProvenance::caller(b"caller-key"),
            &registry(),
            &mut LocalTransport::new(),
        )
    }

    /// A descriptor carries no live handle: two trees whose actions *do
    /// different things* survey to equal descriptors — only the address
    /// reaches the closure, and only against a tree handed in again.
    fn a_descriptor_carries_nothing_that_could_act() {
        let honest = open();
        let hijacked = note(&Note::Retitle("hijacked".into()), |_| Note::Star);
        assert_eq!(
            survey(&honest),
            survey(&hijacked),
            "descriptors that differ only in what they would run must not differ"
        );

        let mut a = AutoCommit::new();
        perform(&mut a, &honest, &star(), None).unwrap();
        let mut b = AutoCommit::new();
        perform(&mut b, &hijacked, &star(), None).unwrap();

        assert_eq!(
            a.get(&ROOT, "starred").unwrap().unwrap().0.as_bool(),
            Some(true)
        );
        assert!(b.get(&ROOT, "starred").unwrap().is_none());
        assert_eq!(
            b.get(&ROOT, "title")
                .unwrap()
                .unwrap()
                .0
                .into_string()
                .unwrap(),
            "hijacked",
            "the same descriptor performed a different operation — so it holds none of it"
        );

        // Owned data: the descriptions outlive the tree they came from.
        let described = survey(&honest);
        drop(honest);
        assert_eq!(described.len(), 2);
    }

    tilo_test_env::scenario! {
        fn a_survey_describes_without_acting;

        phase surveying_through_the_api_describes_without_acting;
        // A descriptor carries nothing that could act, survey annotates only the declared view-open
        // action, enumeration leaves the document untouched, and an address survives its string form.
        // Runs each phase below in order; every phase builds its own fixture.
        {
            a_descriptor_carries_nothing_that_could_act();
            survey_annotates_only_the_declared_view_open_action();
            enumeration_leaves_the_document_untouched();
            an_address_survives_its_string_form();
        }

        phase a_survey_names_the_row_each_action_stands_in;
        // A survey names the row each action stands in: the resource of the level above it, and
        // whether its own resource holds rows below it, so a reader asking a level at a time knows
        // where the listing continues and where it ends.
        {
            let leaf = ResourceSpec::new("notes.note:1", "notes.note", "Harbour")
                .with_action(ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap());
            let folder = ResourceSpec::new("notes.folder:a", "notes.folder", "Trips")
                .with_action(
                    ActionSpec::immediate("archive", "Archive", NOTE_TYPE, &Note::Star).unwrap(),
                )
                .with_child(leaf);
            let placed = survey_placed(std::slice::from_ref(&folder));
            let at = |action: &str| {
                placed
                    .iter()
                    .find(|placed| placed.action.address.action == action)
                    .expect("the action was surveyed")
            };
            let archived = at("archive");
            assert_eq!(archived.parent, "", "the folder stands at the top level");
            assert!(archived.has_children, "the folder holds the note below it");
            let starred = at("star");
            assert_eq!(starred.parent, "notes.folder:a", "the note is under it");
            assert!(
                !starred.has_children,
                "nothing stands below the note: the listing ends here"
            );
            // The flat survey is the same actions, in the same order, without the rows.
            assert_eq!(
                survey(std::slice::from_ref(&folder))
                    .iter()
                    .map(|descriptor| descriptor.address.action.clone())
                    .collect::<Vec<_>>(),
                placed
                    .iter()
                    .map(|placed| placed.action.address.action.clone())
                    .collect::<Vec<_>>(),
            );
        }
    }

    fn survey_annotates_only_the_declared_view_open_action() {
        let open =
            ActionSpec::operation("open", "Open", OperationKind::View, "notes.open", &()).unwrap();
        let select =
            ActionSpec::operation("select", "Select", OperationKind::View, "notes.select", &())
                .unwrap();
        let resource = ResourceSpec::new("note:1", "notes.note", "Harbour")
            .with_action(open)
            .with_action(select)
            .with_view(
                crate::ResourceViewSpec::new("editor", "Editor", "open")
                    .with_description("Edit the note")
                    .with_primary(true),
            );

        let descriptors = survey(&[resource]);
        let open = descriptors
            .iter()
            .find(|descriptor| descriptor.address.action == "open")
            .unwrap();
        assert_eq!(
            open.view,
            Some(ResourceViewDescriptor {
                id: "editor".into(),
                label: "Editor".into(),
                description: Some("Edit the note".into()),
                open_action: "open".into(),
                primary: true,
                range: None,
                mount: None,
                desktop: false,
            })
        );
        assert!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.address.action == "select")
                .unwrap()
                .view
                .is_none()
        );
    }

    /// A tree is per-frame presentation, so an address is re-resolved against a
    /// freshly built one. An address whose resource has gone is refused, not
    /// applied to whatever is open in its place.
    fn an_address_that_no_longer_resolves_is_refused_not_redirected() {
        let address = survey(&open())[0].address.clone();
        assert_eq!(address, star());

        // The note closed; a different one is open, exporting the same action id.
        let now = vec![
            ResourceSpec::new("notes.note:2", "notes.note", "Log").with_action(
                ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap(),
            ),
        ];
        assert!(resolve(&now, &address).is_none());

        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        let err = perform(&mut doc, &now, &address, None).unwrap_err();

        assert_eq!(err, ApiError::Unknown(address));
        assert_eq!(err.code(), "unknown_action");
        assert_eq!(doc.get_heads(), before, "a refused address left no change");
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// Argument rules are checked both directions: a missing declared
    /// argument, and one supplied where none was declared. Neither is
    /// quietly tolerated.
    fn an_argument_that_does_not_match_the_declaration_is_refused() {
        let tree = open();
        let described = survey(&tree);
        let spec = described.iter().find(|d| d.address == retitle()).unwrap();
        assert!(spec.input.wants_text());
        assert!(!spec.input.wants_content());

        let mut doc = AutoCommit::new();
        let before = doc.get_heads();

        let err = perform(&mut doc, &tree, &retitle(), None).unwrap_err();
        assert_eq!(err, ApiError::MissingArgument(retitle()));
        assert_eq!(err.code(), "invalid_arguments");

        let err = perform(&mut doc, &tree, &star(), Some("Log")).unwrap_err();
        assert_eq!(err, ApiError::UnexpectedArgument(star()));
        assert_eq!(err.code(), "invalid_arguments");

        assert_eq!(doc.get_heads(), before, "a refused call left no change");
        assert!(doc.get(&ROOT, "title").unwrap().is_none());
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// An action the app never registered a decoder for cannot be run, even
    /// though its own tree offers it: the API will not dispatch what it cannot
    /// reconstruct the way a replaying peer would.
    fn an_unregistered_action_type_is_refused_rather_than_applied() {
        let tree = open();
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();

        let err = invoke(
            &mut doc,
            &tree,
            &star(),
            None,
            InvocationProvenance::caller(b"caller-key"),
            &ActionRegistry::new(),
            &mut LocalTransport::new(),
        )
        .unwrap_err();

        assert_eq!(err, ApiError::UnregisteredType(NOTE_TYPE.into()));
        assert_eq!(doc.get_heads(), before);
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// The type is registered, but by code that reads another shape (as after
    /// the code that encodes it changed): the refusal names the payload, not a
    /// missing registration, and nothing is applied.
    fn a_payload_its_decoder_cannot_read_is_not_called_unregistered() {
        #[derive(Debug, Serialize, Deserialize)]
        struct OtherShape {
            // Only its shape is decoded.
            #[serde(rename = "pinned")]
            _pinned: bool,
        }
        impl Action for OtherShape {
            fn apply(&self, _doc: &mut Doc) -> Result<(), ActionError> {
                Ok(())
            }
        }
        let tree = open();
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        let mut other = ActionRegistry::new();
        other.register::<OtherShape>(NOTE_TYPE);

        let err = invoke(
            &mut doc,
            &tree,
            &star(),
            None,
            InvocationProvenance::caller(b"caller-key"),
            &other,
            &mut LocalTransport::new(),
        )
        .unwrap_err();

        let ApiError::Failed(why) = &err else {
            panic!("a payload the decoder cannot read is its own failure: {err:?}");
        };
        assert!(why.contains(NOTE_TYPE), "{why}");
        assert_eq!(doc.get_heads(), before);
    }

    /// Build the tree by reading the document, so enumeration exercises the
    /// path an app actually takes.
    fn notes(doc: &AutoCommit) -> Vec<ResourceSpec> {
        let label = doc
            .get(&ROOT, "title")
            .ok()
            .flatten()
            .and_then(|(v, _)| v.into_string().ok())
            .unwrap_or_else(|| "Untitled".into());
        vec![
            ResourceSpec::new("notes.note:1", "notes.note", label).with_action(
                ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap(),
            ),
        ]
    }

    /// Asking what can be done here is not doing it: building the tree and
    /// surveying it twice leaves the document byte-identical, so a caller may
    /// explore an unfamiliar app without changing it.
    fn enumeration_leaves_the_document_untouched() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "title", ScalarValue::Str("Harbour".into()))
            .unwrap();
        doc.commit();
        let heads = doc.get_heads();
        let saved = doc.save();

        let first = survey(&notes(&doc));
        let second = survey(&notes(&doc));

        assert_eq!(first, second);
        assert_eq!(
            first.len(),
            1,
            "the survey found something to be side-effect free about"
        );
        assert_eq!(
            first[0].resource_label, "Harbour",
            "it really read the document"
        );
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(doc.save(), saved, "surveying rewrote the document");
    }

    /// A change made through the API commits under the *caller's* actor, so
    /// history names who asked rather than the app — and since the actor
    /// rides the envelope, a peer still replays to the identical document.
    fn a_change_through_the_api_is_attributed_to_the_caller() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        doc.commit();
        let app_actor = doc.get_actor().clone();
        let mut peer = AutoCommit::load(&doc.save()).unwrap();

        let caller: &[u8] = b"maren-device-key";
        // A caller names the app its change belongs to, as a record's host does.
        let notes = ActionContext::default().with_app("org.tilo.Notes", "Notes");
        let mut transport = LocalTransport::new();
        let dispatched = invoke(
            &mut doc,
            &open(),
            &star(),
            None,
            InvocationProvenance::caller(caller).within(&notes),
            &registry(),
            &mut transport,
        )
        .unwrap();

        assert!(dispatched.mutated_local());
        let env = dispatched.envelope().clone();
        assert_eq!(env.actor.as_deref(), Some(caller));
        let context = env.context.as_ref().expect("the API supplies provenance");
        assert_eq!(published_verb(context), "star");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("notes.note:1")
        );

        let heads = doc.get_heads();
        let change = doc.get_change_by_hash(&heads[0]).unwrap();
        assert_eq!(change.actor_id().to_bytes(), caller);
        assert_eq!(
            change
                .message()
                .and_then(ActionContext::from_change_message),
            env.context,
            "semantic provenance survives as part of the durable change"
        );
        assert_ne!(change.actor_id(), &app_actor, "not the app's own actor");
        assert_ne!(
            change.actor_id().to_bytes(),
            &env.id.actor_bytes()[..],
            "not the action id's derived actor"
        );

        replay(&mut peer, &env, &registry()).unwrap();
        assert_eq!(
            peer.get_heads(),
            heads,
            "attribution must not cost the identical replay"
        );
        assert_eq!(transport.replicated.len(), 1);
    }

    fn ambient_task_context_reaches_api_actions_without_overwriting_resource() {
        let mut doc = AutoCommit::new();
        let mut transport = LocalTransport::new();
        let ambient = ActionContext::default()
            .with_app("org.tilo.Notes", "Notes")
            .with_purpose("Prepare launch")
            .with_group("issue:42", "Launch", "issue");
        let dispatched = invoke(
            &mut doc,
            &open(),
            &star(),
            None,
            InvocationProvenance {
                actor: b"agent:ada",
                ambient: Some(&ambient),
            },
            &registry(),
            &mut transport,
        )
        .expect("the API answered");

        let context = dispatched.envelope().context.as_ref().expect("context");
        assert_eq!(published_verb(context), "star");
        assert_eq!(context.purpose.as_deref(), Some("Prepare launch"));
        assert_eq!(
            context.group.as_ref().map(|group| group.id.as_str()),
            Some("issue:42")
        );
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("notes.note:1")
        );
    }

    fn consequential_actions_cannot_cross_the_api_without_consent() {
        let resources = vec![
            ResourceSpec::new("notes.note:1", "notes.note", "Harbour").with_action(
                ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star)
                    .unwrap()
                    .with_contract(ActionContract {
                        confirmation: crate::ConfirmationPolicy::Confirm,
                        ..ActionContract::default()
                    }),
            ),
        ];
        let mut doc = AutoCommit::new();
        let mut transport = LocalTransport::new();

        let refusal = invoke(
            &mut doc,
            &resources,
            &star(),
            None,
            InvocationProvenance::caller(b"caller-key"),
            &registry(),
            &mut transport,
        )
        .unwrap_err();
        assert_eq!(refusal, ApiError::ConfirmationRequired);
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());

        invoke_confirmed(
            &mut doc,
            &resources,
            &star(),
            None,
            InvocationProvenance::caller(b"caller-key"),
            &registry(),
            &mut transport,
        )
        .expect("a policy check may explicitly attest confirmation");
        assert_eq!(
            doc.get(&ROOT, "starred").unwrap().unwrap().0.as_bool(),
            Some(true)
        );
    }

    /// A generic edit whose value decides its risk: clearing the title is
    /// destructive, renaming is not.
    fn edit_by_value() -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new("notes.note:1", "notes.note", "Harbour").with_action(
                ActionSpec::typed(
                    "edit",
                    "Edit",
                    "title",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "title",
                        crate::ParamType::Text,
                        "Title",
                    )],
                    |args| encoded(&Note::Retitle(args.text("title").unwrap_or("").to_owned())),
                )
                .for_audience(crate::Audience::Agents)
                .with_risk(|args| {
                    if args.text("title").is_some_and(str::is_empty) {
                        crate::RiskLevel::Destructive
                    } else {
                        crate::RiskLevel::Low
                    }
                }),
            ),
        ]
    }

    fn edit() -> ActionAddress {
        ActionAddress::new("notes.note:1", "edit")
    }

    fn title(value: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::from_iter([("title".to_owned(), serde_json::json!(value))])
    }

    fn stored_title(doc: &AutoCommit) -> Option<String> {
        doc.get(&ROOT, "title")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
    }

    fn a_value_classified_as_destructive_needs_the_same_consent() {
        let resources = edit_by_value();
        let mut doc = AutoCommit::new();
        let mut transport = LocalTransport::new();
        let registry = registry();
        let mut run = |doc: &mut AutoCommit, value: &str, confirmed: bool| {
            let api = if confirmed {
                invoke_json_confirmed
            } else {
                invoke_json
            };
            api(
                doc,
                &resources,
                &edit(),
                &title(value),
                InvocationProvenance::caller(b"caller-key"),
                &registry,
                &mut transport,
            )
            .map(|_| ())
        };

        run(&mut doc, "Harbour", false).expect("an ordinary value runs");
        assert_eq!(
            run(&mut doc, "", false).unwrap_err(),
            ApiError::ConfirmationRequired
        );
        assert_eq!(
            stored_title(&doc).as_deref(),
            Some("Harbour"),
            "the refused value changed nothing"
        );
        run(&mut doc, "", true).expect("a confirmed destructive value runs");
        assert_eq!(stored_title(&doc).as_deref(), Some(""));

        let mut transport = LocalTransport::new();
        assert_eq!(
            invoke(
                &mut doc,
                &resources,
                &edit(),
                Some(""),
                InvocationProvenance::caller(b"caller-key"),
                &registry,
                &mut transport,
            )
            .unwrap_err(),
            ApiError::ConfirmationRequired
        );
    }

    /// The context an agent run hands every invocation it makes.
    fn agent_session() -> ActionContext {
        ActionContext::default()
            .with_app("org.tilo.Agent", "Agent")
            .with_purpose("Remove the draft marks")
            .with_group("agent-session:7", "Remove the draft marks", "agent-session")
    }

    /// Read the durable change the API just committed back from its message,
    /// and require it to carry both the resource's own context and the run's.
    /// The published action an entry names, with the resource kind it was
    /// published on.
    fn published_verb(context: &ActionContext) -> &str {
        let Some(crate::Did::Verb(verb)) = &context.what else {
            panic!("a verb: {context:?}");
        };
        assert_eq!(verb.action.kind, "notes.note");
        assert!(!verb.action.app.is_empty(), "the app is filled in");
        &verb.action.action
    }

    fn assert_recorded_within_run(doc: &mut AutoCommit, action: &str) {
        let heads = doc.get_heads();
        let change = doc
            .get_change_by_hash(&heads[0])
            .expect("the API committed");
        assert_eq!(change.actor_id().to_bytes(), b"agent:ada");
        let context = change
            .message()
            .and_then(ActionContext::from_change_message)
            .expect("the change carries its semantic provenance");
        assert_eq!(published_verb(&context), action);
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("notes.note:1"),
            "the resource stays authoritative"
        );
        let group = context.group.as_ref().expect("the run's group is recorded");
        assert_eq!(group.id, "agent-session:7");
        assert_eq!(group.kind, "agent-session");
        assert_eq!(context.purpose.as_deref(), Some("Remove the draft marks"));
    }

    tilo_test_env::scenario! {
        fn an_invocation_through_the_api_is_checked_and_recorded;

        phase invoking_through_the_api_refuses_mismatches_and_attributes_the_caller;
        // Stale addresses, mismatched arguments and unregistered action types are refused rather than
        // redirected or applied; an accepted change is attributed to the caller and ambient task
        // context reaches it without overwriting the resource.
        // Runs each phase below in order; every phase builds its own fixture.
        {
            an_address_that_no_longer_resolves_is_refused_not_redirected();
            an_argument_that_does_not_match_the_declaration_is_refused();
            an_unregistered_action_type_is_refused_rather_than_applied();
            a_payload_its_decoder_cannot_read_is_not_called_unregistered();
            a_change_through_the_api_is_attributed_to_the_caller();
            ambient_task_context_reaches_api_actions_without_overwriting_resource();
        }

        phase an_optional_only_prompt_runs_without_its_argument;
        // A prompt whose only parameter is optional runs when the caller omits
        // it, bound through the typed parameters exactly as an omitted optional
        // argument is; a required one is still refused.
        {
            let tree = vec![
                ResourceSpec::new("notes.note:1", "notes.note", "Harbour").with_action(
                    ActionSpec::prompted_param(
                        "retitle",
                        "Rename",
                        ActionParam::optional("title", crate::ParamType::Text, "New title"),
                        "title",
                        ExecutionLocus::ClientLocal,
                        |text| encoded(&Note::Retitle(format!("[{text}]"))),
                    ),
                ),
            ];
            let mut doc = AutoCommit::new();
            perform(&mut doc, &tree, &retitle(), None).unwrap();
            assert_eq!(
                doc.get(&ROOT, "title")
                    .unwrap()
                    .unwrap()
                    .0
                    .into_string()
                    .unwrap(),
                "[]",
                "the omitted optional text binds as nothing typed"
            );

            let mut doc = AutoCommit::new();
            let err = perform(&mut doc, &open(), &retitle(), None).unwrap_err();
            assert_eq!(err, ApiError::MissingArgument(retitle()));
        }

        phase a_wire_text_argument_goes_under_the_actions_own_prompt_parameter;
        // A text argument on the wire names no parameter; the descriptor says
        // where it goes.
        {
            let tree = open();
            let described = survey(&tree);
            let retitle_spec = described.iter().find(|d| d.address == retitle()).unwrap();
            assert_eq!(retitle_spec.prompt_param(), Some("title"));
            let lone = serde_json::json!({ crate::PROMPTED_PARAM: "Log" })
                .as_object()
                .unwrap()
                .clone();
            assert_eq!(
                retitle_spec.bind_prompt_text(lone.clone()),
                serde_json::json!({"title": "Log"})
                    .as_object()
                    .unwrap()
                    .clone()
            );
            // Already named, or more than one argument: left as given.
            let named = serde_json::json!({"title": "Log"})
                .as_object()
                .unwrap()
                .clone();
            assert_eq!(retitle_spec.bind_prompt_text(named.clone()), named);
            // An action that collects no text has no prompt parameter.
            let star_spec = described.iter().find(|d| d.address == star()).unwrap();
            assert_eq!(star_spec.prompt_param(), None);
            assert_eq!(star_spec.bind_prompt_text(lone.clone()), lone);
        }

        phase consequential_api_actions_need_consent_whatever_form_the_argument_takes;
        // Consequential actions and values classified destructive need consent, and effective
        // confirmation reads every argument form.
        // Runs each phase below in order; every phase builds its own fixture.
        {
            consequential_actions_cannot_cross_the_api_without_consent();
            a_value_classified_as_destructive_needs_the_same_consent();
            effective_confirmation_reads_every_argument_form();
        }

        phase a_confirmed_invocation_inside_a_run_is_recorded_with_its_group;
        // A destructive operation an agent run performs after its policy check
        // confirmed it is recorded within the run, exactly like an unconfirmed
        // one — consent is not a reason to lose whose task the change belongs to.
        {
            let resources = vec![
                ResourceSpec::new("notes.note:1", "notes.note", "Harbour").with_action(
                    ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star)
                        .unwrap()
                        .destructive(),
                ),
            ];
            let run = agent_session();
            let provenance = InvocationProvenance::caller(b"agent:ada").within(&run);
            let mut doc = AutoCommit::new();
            let mut transport = LocalTransport::new();

            assert_eq!(
                invoke(
                    &mut doc,
                    &resources,
                    &star(),
                    None,
                    provenance,
                    &registry(),
                    &mut transport,
                )
                .unwrap_err(),
                ApiError::ConfirmationRequired
            );
            let dispatched = invoke_confirmed(
                &mut doc,
                &resources,
                &star(),
                None,
                provenance,
                &registry(),
                &mut transport,
            )
            .expect("a confirmed destructive action runs");

            assert_eq!(
                dispatched
                    .envelope()
                    .context
                    .as_ref()
                    .and_then(|context| context.group.as_ref())
                    .map(|group| group.id.as_str()),
                Some("agent-session:7"),
                "the replicated envelope carries the run too"
            );
            assert_recorded_within_run(&mut doc, "star");
        }

        phase typed_invocations_inside_a_run_are_recorded_with_its_group;
        // Both typed-argument entry points record the run's context: the
        // ordinary value through [`invoke_json`] and the destructive one through
        // [`invoke_json_confirmed`].
        {
            let resources = edit_by_value();
            let run = agent_session();
            let provenance = InvocationProvenance::caller(b"agent:ada").within(&run);
            let mut doc = AutoCommit::new();
            let mut transport = LocalTransport::new();

            invoke_json(
                &mut doc,
                &resources,
                &edit(),
                &title("Harbour"),
                provenance,
                &registry(),
                &mut transport,
            )
            .expect("an ordinary value runs");
            assert_recorded_within_run(&mut doc, "edit");

            invoke_json_confirmed(
                &mut doc,
                &resources,
                &edit(),
                &title(""),
                provenance,
                &registry(),
                &mut transport,
            )
            .expect("a confirmed destructive value runs");
            assert_eq!(stored_title(&doc).as_deref(), Some(""));
            assert_recorded_within_run(&mut doc, "edit");
        }
    }

    fn effective_confirmation_reads_every_argument_form() {
        let resources = edit_by_value();
        let spec = resolve(&resources, &edit()).unwrap();
        let none = crate::ConfirmationPolicy::None;
        let confirm = crate::ConfirmationPolicy::Confirm;
        assert_eq!(spec.confirmation_for_json(&title("x")), none);
        assert_eq!(spec.confirmation_for_json(&title("")), confirm);
        assert_eq!(spec.confirmation_for_argument(Some("x")), none);
        assert_eq!(spec.confirmation_for_argument(Some("")), confirm);
        let typed = format!("{}{{\"title\":\"\"}}", crate::ARGUMENTS_JSON_PREFIX);
        assert_eq!(spec.confirmation_for_argument(Some(&typed)), confirm);
        // Arguments that cannot bind build nothing, so they get the declared policy.
        assert_eq!(spec.confirmation_for_json(&serde_json::Map::new()), none);

        assert!(!spec.reaches_people() && !spec.contract.reaches_people());
        assert!(spec.contract.confirmation_depends_on_arguments());
        assert_eq!(spec.contract.confirmation, none);
        assert_eq!(spec.contract.advertised_confirmation(), confirm);
        assert!(spec.contract.validate_declaration().is_empty());

        // A declared policy is never weakened by a classifier.
        let declared = spec.clone().destructive();
        assert_eq!(declared.confirmation_for_json(&title("x")), confirm);
    }

    /// Resource ids carry colons, so the address separator has to be something
    /// they cannot contain; and a string that is not an address is refused
    /// rather than half-parsed into a lookup. An address is a resource and an
    /// action, and nothing else: a third part is no address.
    fn an_address_survives_its_string_form() {
        let address = star();
        assert_eq!(ActionAddress::parse(&address.encode()), Some(address));
        let one = ActionAddress::new("notes.note:1", "edit");
        assert_eq!(ActionAddress::parse(&one.encode()), Some(one));
        assert_eq!(ActionAddress::parse("notes.note:1"), None);
        assert_eq!(ActionAddress::parse(&format!("{KEY_SEP}star")), None);
        assert_eq!(
            ActionAddress::parse(&format!("notes{KEY_SEP}{KEY_SEP}edit")),
            None
        );
        assert_eq!(
            ActionAddress::parse(&format!("notes.note{KEY_SEP}notes.note:1{KEY_SEP}edit")),
            None
        );
        assert_eq!(
            ActionAddress::checked("notes.note:1", "edit"),
            Some(ActionAddress::new("notes.note:1", "edit"))
        );
        assert_eq!(ActionAddress::checked("", "edit"), None);
        assert_eq!(ActionAddress::checked("notes.note:1", ""), None);
        assert_eq!(
            ActionAddress::checked(format!("notes{KEY_SEP}note"), "edit"),
            None
        );
        assert_eq!(
            ActionAddress::checked("notes.note:1", format!("edit{KEY_SEP}title")),
            None
        );
    }

    const OP: &str = "test.operation";

    /// A session with a renamable name, a step it cannot take now, a disk erased only by its
    /// typed name and a power switch that asks to be confirmed.
    fn published_tree() -> Vec<ResourceSpec> {
        use crate::{ActionContract, ConfirmationPolicy, ParamType, RiskLevel};
        let encode =
            |value: serde_json::Value| EncodedAction::encode_operation(OP, &value).unwrap();
        vec![
            ResourceSpec::new("session", "test.session", "Session")
                .with_action(ActionSpec::typed_operation(
                    "rename",
                    "Rename",
                    "name",
                    OperationKind::Effect,
                    OP,
                    vec![crate::ActionParam::required(
                        "name",
                        ParamType::Text,
                        "New name",
                    )],
                    move |args| encode(serde_json::json!({"rename": args.text("name")})),
                ))
                .with_action(
                    ActionSpec::operation("reset", "Reset", OperationKind::Effect, OP, &"reset")
                        .unwrap()
                        .unavailable("not_now", "Finish the current step first"),
                )
                .with_child(
                    ResourceSpec::new("disk:/dev/vdb", "test.disk", "vdb").with_action(
                        ActionSpec::operation(
                            "erase",
                            "Erase",
                            OperationKind::Effect,
                            OP,
                            &"erase",
                        )
                        .unwrap()
                        .destructive()
                        .with_contract(ActionContract {
                            risk: RiskLevel::Destructive,
                            confirmation: ConfirmationPolicy::Phrase {
                                phrase: "/dev/vdb".into(),
                            },
                            ..ActionContract::default()
                        }),
                    ),
                )
                .with_child(
                    ResourceSpec::new("power", "test.power", "Power").with_action(
                        ActionSpec::operation(
                            "off",
                            "Power off",
                            OperationKind::Effect,
                            OP,
                            &"off",
                        )
                        .unwrap()
                        .privileged(),
                    ),
                ),
        ]
    }

    #[test]
    fn a_published_tree_is_listed_and_admits_only_exact_confirmed_bindings() {
        use crate::remote::{CataloguePage, CatalogueQuery, Confirmation};
        let tree = published_tree();
        let page = CataloguePage::published("test", &tree, &CatalogueQuery::default());
        // A listing is one level: the session's own actions, then what it holds.
        assert_eq!(page.rows.len(), 2);
        assert!(page.rows.iter().all(|row| row.provider == "test"));
        let held = CatalogueQuery {
            parent_resource: Some("session".into()),
            ..CatalogueQuery::default()
        };
        let held = CataloguePage::published("test", &tree, &held);
        assert_eq!(held.rows.len(), 2);
        let erase = held
            .rows
            .iter()
            .find_map(|row| row.action.as_ref().filter(|a| a.address.action == "erase"))
            .unwrap();
        assert_eq!(erase.address.resource, "disk:/dev/vdb");
        let narrowed = CatalogueQuery {
            needle: "power".into(),
            parent_resource: Some("session".into()),
            ..CatalogueQuery::default()
        };
        assert_eq!(
            CataloguePage::published("test", &tree, &narrowed)
                .rows
                .len(),
            1
        );

        let args = |value: serde_json::Value| value.as_object().unwrap().clone();
        let admitted = |resource: &str, action: &str, value, confirmation: Option<Confirmation>| {
            admit(
                &tree,
                &ActionAddress::new(resource, action),
                &args(value),
                confirmation.as_ref(),
            )
        };
        assert_eq!(
            admitted("session", "rename", serde_json::json!({"name": "x"}), None)
                .unwrap()
                .type_id,
            OP
        );
        for (resource, action, value, refusal) in [
            ("session", "nope", serde_json::json!({}), "unknown"),
            ("session", "rename", serde_json::json!({}), "argument"),
            (
                "session",
                "rename",
                serde_json::json!({"name": 3}),
                "argument",
            ),
            (
                "session",
                "rename",
                serde_json::json!({"name": "x", "extra": 1}),
                "argument",
            ),
            ("session", "reset", serde_json::json!({}), "unavailable"),
        ] {
            let error = admitted(resource, action, value, None).unwrap_err();
            let kind = match error {
                ApiError::Unknown(_) => "unknown",
                ApiError::Unavailable { .. } => "unavailable",
                ApiError::MissingArgument(_)
                | ApiError::UnexpectedArgument(_)
                | ApiError::InvalidArguments(_) => "argument",
                other => panic!("{other:?}"),
            };
            assert_eq!(kind, refusal, "{action}");
        }
        let erase = |confirmation| {
            admitted(
                "disk:/dev/vdb",
                "erase",
                serde_json::json!({}),
                confirmation,
            )
        };
        for wrong in [
            None,
            Some(Confirmation::Phrase("/dev/vda".into())),
            Some(Confirmation::Confirmed),
        ] {
            assert!(matches!(erase(wrong), Err(ApiError::ConfirmationRequired)));
        }
        assert!(erase(Some(Confirmation::Phrase("/dev/vdb".into()))).is_ok());
        let off = |confirmation| admitted("power", "off", serde_json::json!({}), confirmation);
        assert!(matches!(off(None), Err(ApiError::ConfirmationRequired)));
        assert!(off(Some(Confirmation::Confirmed)).is_ok());
    }
}
