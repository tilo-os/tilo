//! The nested **Resource** tree an app publishes, and the **`ActionSpec`**s
//! each resource exports.
//!
//! A [`ResourceSpec`] is something the app currently has *open/presented*
//! (a playing video, an open book); resources **nest** — a video owns
//! its comments, a book its annotations. Each exports [`ActionSpec`]s:
//! the operations Ctrl+K offers for it.
//!
//! The split that matters: [`ResourceSpec`]/[`ActionSpec`] is
//! **presentation** — local, per-frame, with human labels and (for a
//! prompted action) a closure. The [`EncodedAction`] an `ActionSpec`
//! yields is the **action** — serializable, replayable. Presentation
//! never crosses a boundary; only the action does.
//!
//! # The typed projection
//!
//! A spec also carries a **serializable projection of itself**: a typed
//! [`params`](ActionSpec::params) list and a builder that turns bound
//! [`ActionArgs`] into that same [`EncodedAction`]. That is what lets a caller
//! with no window enumerate the surface (see `crate::wire`) and invoke it by
//! name — the presentation stays local, but a faithful *description* of it can
//! now cross.
//!
//! The projection is derived from the same closure the palette uses:
//! [`immediate`](ActionSpec::immediate) projects to zero parameters returning
//! its pre-encoded action, and
//! [`prompted_param`](ActionSpec::prompted_param) projects to one required
//! [`Text`](ParamType::Text) parameter, named for what it is, that calls the
//! app's closure. An affordance, its palette row, and an external invocation
//! therefore run through one function and produce the same action.
//!
//! [`typed`](ActionSpec::typed) constructs an action whose argument has a
//! domain type (a number in `0..=1`, a fixed set of filter names) instead of
//! relying on a lenient prose parser.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionError, EncodedAction, ExecutionLocus, OperationKind};
use crate::api::ActionAddress;
use crate::contract::{
    ActionContract, ActionKind, ActionNeed, Availability, ConfirmationPolicy, ExecutionMode,
    ResultSpec, RiskLevel,
};
use crate::params::{ActionArgs, ActionParam, ArgError, ArgValue, ParamType};

/// Builds an action's encoded form from arguments already bound against its
/// declared [`params`](ActionSpec::params).
///
/// Infallible by design: every check a caller can fail — presence, type,
/// choice — happens in [`ActionArgs::bind`] *before* this runs, so a builder
/// that holds an `ActionArgs` is looking at values it declared and can use them
/// directly. The [`Target`] is that of the spec being invoked, for a type
/// shared by many; an action whose builder holds what it acts on ignores it.
pub type ArgsBuilder = Arc<dyn Fn(&Target, &ActionArgs) -> EncodedAction + Send + Sync>;

/// Failure to construct or emit a control binding.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionBindingError {
    #[error("action `{action}` is not exported by resource `{resource}`")]
    ActionNotExported { resource: String, action: String },
    #[error(transparent)]
    InvalidArguments(#[from] ArgError),
    /// The control's value does not make arguments of the action at all.
    #[error("{0}")]
    Unbindable(String),
}

type Arguments<I> =
    Arc<dyn Fn(I) -> Result<serde_json::Map<String, serde_json::Value>, String> + Send + Sync>;

/// What a control binds to: one published resource and the actions it exports. A row an app
/// publishes ([`ResourceSpec`] here, the framework's live row above it) is one, so a control
/// binds to the row it shows without the app building the published tree itself.
pub trait Exports {
    /// The resource as it is published: its identity, kind, sharing and exported actions.
    fn exported(&self) -> &ResourceSpec;
}

impl Exports for ResourceSpec {
    fn exported(&self) -> &ResourceSpec {
        self
    }
}

/// A resource kept behind a pointer exports what the resource does, as a reference to it did when
/// the binding took a `&ResourceSpec`.
impl<T: Exports + ?Sized> Exports for &T {
    fn exported(&self) -> &ResourceSpec {
        (**self).exported()
    }
}

impl<T: Exports + ?Sized> Exports for Box<T> {
    fn exported(&self) -> &ResourceSpec {
        (**self).exported()
    }
}

impl<T: Exports + ?Sized> Exports for std::rc::Rc<T> {
    fn exported(&self) -> &ResourceSpec {
        (**self).exported()
    }
}

impl<T: Exports + ?Sized> Exports for Arc<T> {
    fn exported(&self) -> &ResourceSpec {
        (**self).exported()
    }
}

impl<T: Exports> Exports for std::sync::LazyLock<T> {
    fn exported(&self) -> &ResourceSpec {
        std::sync::LazyLock::force(self).exported()
    }
}

/// Type-safe adapter from a control's input value to an exported action.
///
/// Construction requires the row that really exports the action ([`Exports`])
/// and its [`ActionSpec`], rather than parallel string constants. Construction and emission validate the
/// arguments against the parameters of a clone of that spec and never run its
/// encoder, which may plan the whole action: a binding is made for each control
/// a frame reads. The host re-resolves, validates and encodes at dispatch time
/// because this object is convenience, not authority.
#[derive(Clone)]
pub struct ActionBinding<I> {
    address: ActionAddress,
    /// The kind of record the action acts on, as the resource publishes it: what
    /// the control's identity names instead of the one resource, which the
    /// window's business is ([`Self::identity`]).
    kind: String,
    /// The app the resource is published by, when everyone in the workspace reads it rather than
    /// only its person ([`ResourceSpec::personal`]): what [`Self::shared_resource`] names.
    shared_by: Option<String>,
    spec: ActionSpec,
    arguments: Arguments<I>,
}

impl<I> ActionBinding<I> {
    /// Bind a typed control value to arguments for an action actually exported
    /// by `resource`.
    ///
    /// # Errors
    ///
    /// Returns [`ActionBindingError::ActionNotExported`] if `spec` is absent
    /// from `resource`.
    pub fn new(
        resource: &(impl Exports + ?Sized),
        spec: &ActionSpec,
        arguments: impl Fn(I) -> serde_json::Map<String, serde_json::Value> + Send + Sync + 'static,
    ) -> Result<Self, ActionBindingError> {
        Self::fallible(resource, spec, move |input| Ok(arguments(input)))
    }

    /// [`Self::new`] for an input that may not make arguments: a value that
    /// does not serialize, a form that does not fit the parameters. Such an
    /// input is refused when the control emits it, never sent empty.
    ///
    /// # Errors
    ///
    /// Returns [`ActionBindingError::ActionNotExported`] if `spec` is absent
    /// from `resource`.
    pub fn fallible(
        resource: &(impl Exports + ?Sized),
        spec: &ActionSpec,
        arguments: impl Fn(I) -> Result<serde_json::Map<String, serde_json::Value>, String>
        + Send
        + Sync
        + 'static,
    ) -> Result<Self, ActionBindingError> {
        let resource = resource.exported();
        let exported = resource
            .actions
            .iter()
            .find(|candidate| candidate.id == spec.id)
            .ok_or_else(|| ActionBindingError::ActionNotExported {
                resource: resource.id.0.clone(),
                action: spec.id.clone(),
            })?;
        Ok(Self {
            address: ActionAddress::new(resource.id.0.clone(), spec.id.clone()),
            kind: resource.kind.clone(),
            shared_by: (!resource.personal).then(|| resource.app_id.clone().unwrap_or_default()),
            // Always retain the spec from the resource, even if a caller
            // passed an independently-created spec with the same id.
            spec: exported.clone(),
            arguments: Arc::new(arguments),
        })
    }

    /// Produce a host-routable intent after strict parameter validation.
    ///
    /// # Errors
    ///
    /// Returns [`ActionBindingError::InvalidArguments`] if the mapped input
    /// violates the exported action's parameter contract.
    pub fn invoke(&self, input: I) -> Result<ControlInvocation, ActionBindingError> {
        let arguments = (self.arguments)(input).map_err(ActionBindingError::Unbindable)?;
        self.spec.validate_json(&arguments)?;
        Ok(ControlInvocation {
            address: self.address.clone(),
            arguments,
        })
    }

    pub fn address(&self) -> &ActionAddress {
        &self.address
    }

    /// The record a control bound here writes into for everyone in the workspace to read, as the
    /// app publishing it, the resource and its kind: a change to a shared document (a message
    /// sent into a conversation), never an effect, a view change or the person's own record.
    #[must_use]
    pub fn shared_resource(&self) -> Option<(&str, &str, &str)> {
        let app = self.shared_by.as_deref()?;
        (self.spec.kind == OperationKind::Document).then_some((
            app,
            self.address.resource.as_str(),
            self.kind.as_str(),
        ))
    }

    /// What a control bound to it does, as a window publishes it: the address
    /// the API runs it by ([`ResourceSpec::address_of`]) — so the row's Complete
    /// names that row, and what a person presses and what a caller names at
    /// the API are one thing.
    #[must_use]
    pub fn identity(&self) -> ActionAddress {
        self.address.clone()
    }

    /// The same binding for a control whose value `J` maps to this one's
    /// input: a field that reports each edit for a binding that takes the text.
    #[must_use]
    pub fn map_input<J>(self, map: impl Fn(J) -> I + Send + Sync + 'static) -> ActionBinding<J>
    where
        I: 'static,
    {
        let arguments = self.arguments;
        ActionBinding {
            address: self.address,
            kind: self.kind,
            shared_by: self.shared_by,
            spec: self.spec,
            arguments: Arc::new(move |input| arguments(map(input))),
        }
    }
}

impl ActionBinding<()> {
    /// Bind a nullary control to the action `spec` its declaration publishes
    /// on `resource`, which no row carries: an effect the resource offers.
    /// # Errors
    /// [`ActionBindingError::InvalidArguments`] when `spec` requires an argument.
    pub fn declared(
        resource: impl Into<String>,
        spec: ActionSpec,
    ) -> Result<Self, ActionBindingError> {
        let binding = Self {
            address: ActionAddress::new(resource, spec.id.clone()),
            kind: String::new(),
            shared_by: None,
            spec,
            arguments: std::sync::Arc::new(|()| Ok(serde_json::Map::new())),
        };
        binding.spec.validate_json(&serde_json::Map::new())?;
        Ok(binding)
    }

    /// Bind a nullary control to a nullary exported action.
    pub fn unit(
        resource: &(impl Exports + ?Sized),
        spec: &ActionSpec,
    ) -> Result<Self, ActionBindingError> {
        let binding = Self::new(resource, spec, |()| serde_json::Map::new())?;
        // Refuse a required-argument action at construction, not first click.
        binding.spec.validate_json(&serde_json::Map::new())?;
        Ok(binding)
    }

    /// Look up the real exported spec by stable id, then create a nullary
    /// binding. Useful when the resource is assembled inline or generated.
    pub fn unit_for(
        resource: &(impl Exports + ?Sized),
        action_id: &str,
    ) -> Result<Self, ActionBindingError> {
        let spec = resource
            .exported()
            .actions
            .iter()
            .find(|spec| spec.id == action_id)
            .ok_or_else(|| ActionBindingError::ActionNotExported {
                resource: resource.exported().id.0.clone(),
                action: action_id.to_string(),
            })?;
        Self::unit(resource, spec)
    }

    /// Bind a control to a fixed, strictly validated JSON argument object.
    pub fn static_json(
        resource: &(impl Exports + ?Sized),
        spec: &ActionSpec,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, ActionBindingError> {
        let binding = Self::new(resource, spec, move |()| arguments.clone())?;
        binding
            .spec
            .validate_json(&(binding.arguments)(()).map_err(ActionBindingError::Unbindable)?)?;
        Ok(binding)
    }

    /// Look up the real exported spec by id, then bind fixed JSON arguments.
    pub fn static_json_for(
        resource: &(impl Exports + ?Sized),
        action_id: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, ActionBindingError> {
        let spec = resource
            .exported()
            .actions
            .iter()
            .find(|spec| spec.id == action_id)
            .ok_or_else(|| ActionBindingError::ActionNotExported {
                resource: resource.exported().id.0.clone(),
                action: action_id.to_string(),
            })?;
        Self::static_json(resource, spec, arguments)
    }
}

/// The generic parameter name of an argument that is a bare value: an
/// accessibility set-value, a content hash ([`ActionSpec::prompted_content`]),
/// a wire `InvokeArgument::Text` before a descriptor says where it goes.
/// An app's own prompted action names its parameter
/// ([`ActionSpec::prompted_param`]) and callers ask [`ActionSpec::prompt_param`].
pub const PROMPTED_PARAM: &str = "value";

/// The text of an argument object that holds exactly one string, whatever it
/// is called: what a continuation or an accessibility set-value reads of a
/// prompted action's arguments without knowing the action's own name for it.
/// The contract of a spec nothing has declared a kind for: its operation's
/// without the kind, which only the declaration names
/// ([`ActionSpec::of_kind`]).
fn unnamed_kind(contract: ActionContract) -> ActionContract {
    ActionContract {
        kind: ActionKind::Outside,
        ..contract
    }
}

pub fn sole_text_argument(arguments: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    let mut values = arguments.values();
    match (values.next(), values.next()) {
        (Some(serde_json::Value::String(text)), None) => Some(text),
        _ => None,
    }
}

/// Inert envelope produced when the legacy one-field palette cannot satisfy a
/// typed contract. Hosts recognize it as a refusal; it is never registered or
/// dispatched as an application operation.
pub const INVALID_ARGUMENTS_TYPE: &str = "tilo.invalid_arguments";

/// The operation type of the verbs an accessibility surface publishes for a
/// control: focusing it, pressing it, toggling it or setting its value. They
/// stand in for the person's own pointer and keyboard, which only the person
/// at the device invokes, so they need what reading needs; what the control
/// then does is decided as the control's own action declares it.
pub const SEMANTIC_OPERATION_TYPE: &str = "tilo.ui.semantic-action";

pub use tilo_action_types::ResourceId;

/// What an action's row shows of the state the action acts on, beside its
/// label: data, never drawing code. Every surface that lists the action (the
/// palette, a menu, an agent's listing) reads the same.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActionState {
    /// A short live status ("On", "Current").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// The action flips the state its badge shows, so choosing it keeps the
    /// list it was chosen from open on the new state.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub toggles: bool,
    /// The keys that do the same, spelled as the keyboard layout in use
    /// reads them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
}

impl ActionState {
    pub const MAX_BADGE_BYTES: usize = ResourcePresentation::MAX_BADGE_BYTES;

    /// Nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// A row showing `badge`.
    #[must_use]
    pub fn badge(badge: impl Into<String>) -> Self {
        Self {
            badge: Some(badge.into()),
            ..Self::default()
        }
    }
}

/// Bounded, declarative search presentation for a resource.
///
/// This is data, never app-provided drawing code: the shell chooses the
/// typography, colours, icons, and layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcePresentation {
    pub detail: Option<String>,
    pub badge: Option<String>,
    /// Bounded plain-text content for a shell/toolkit-owned preview. Apps
    /// provide data only; they never control preview pixels or interaction.
    pub preview: Option<String>,
}

impl ResourcePresentation {
    pub const MAX_DETAIL_BYTES: usize = 160;
    pub const MAX_BADGE_BYTES: usize = 32;
    pub const MAX_PREVIEW_BYTES: usize = 512;

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.detail
            .as_ref()
            .is_none_or(|value| value.len() <= Self::MAX_DETAIL_BYTES && !value.contains('\n'))
            && self
                .badge
                .as_ref()
                .is_none_or(|value| value.len() <= Self::MAX_BADGE_BYTES && !value.contains('\n'))
            && self.preview.as_ref().is_none_or(|value| {
                value.len() <= Self::MAX_PREVIEW_BYTES
                    && !value
                        .chars()
                        .any(|character| matches!(character, '\r' | '\0'))
            })
    }
}

/// How a live resource-view provider accepts a compositor-owned mount.
///
/// The named parameters belong to the referenced action's typed contract.
/// Keeping them explicit avoids inferring view support from conventional
/// parameter names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceViewMountSpec {
    pub action: String,
    pub capability_param: String,
    pub view_param: String,
}

impl ResourceViewMountSpec {
    pub fn new(
        action: impl Into<String>,
        capability_param: impl Into<String>,
        view_param: impl Into<String>,
    ) -> Self {
        Self {
            action: action.into(),
            capability_param: capability_param.into(),
            view_param: view_param.into(),
        }
    }
}

/// Explicit range-navigation binding. Offsets retain the reference's Chars or
/// Bytes unit; the app validates them against its current text before moving.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceViewRangeSpec {
    pub action: String,
    pub start_param: String,
    pub end_param: String,
    pub unit_param: String,
}

impl ResourceViewRangeSpec {
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            start_param: "start".into(),
            end_param: "end".into(),
            unit_param: "unit".into(),
        }
    }
}

/// An explicit presentation of a resource, separate from the actions used to
/// open or mount it.
///
/// `id` is the stable view identity. `open_action` and the optional mount
/// action are execution bindings and may change without turning every local
/// [`OperationKind::View`] action into an advertised resource view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceViewSpec {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub open_action: String,
    pub primary: bool,
    pub mount: Option<ResourceViewMountSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ResourceViewRangeSpec>,
    /// The view opens on the whole desktop, not in its app alone full tab:
    /// what it shows comes from the desktop's session (a workspace's
    /// history, say), which a page showing one app does not run. A link to
    /// it opens the desktop at this view.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub desktop: bool,
    /// What a place of the resource shows when it stands as content on its own (a place on the
    /// canvas, an answer's view) rather than as its app's window: the view the words naming the
    /// resource, not its app, place it through. At most one view of a resource is so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub place: bool,
}

impl ResourceViewSpec {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        open_action: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            open_action: open_action.into(),
            primary: false,
            mount: None,
            range: None,
            desktop: false,
            place: false,
        }
    }

    /// Open it on the whole desktop ([`Self::desktop`]).
    #[must_use]
    pub fn on_desktop(mut self) -> Self {
        self.desktop = true;
        self
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn with_primary(mut self, primary: bool) -> Self {
        self.primary = primary;
        self
    }

    /// Make it what a place of the resource shows on its own ([`Self::place`]).
    #[must_use]
    pub fn with_place(mut self, place: bool) -> Self {
        self.place = place;
        self
    }

    #[must_use]
    pub fn with_mount(mut self, mount: ResourceViewMountSpec) -> Self {
        self.mount = Some(mount);
        self
    }

    #[must_use]
    pub fn with_range(mut self, range: ResourceViewRangeSpec) -> Self {
        self.range = Some(range);
        self
    }

    /// Validate this declaration against the actions exported by `resource`.
    ///
    /// Cross-view invariants such as unique open actions and a single primary
    /// view are checked by [`crate::validate_surface_contracts`].
    pub fn validate(&self, resource: &ResourceSpec) -> Vec<String> {
        let mut errors = Vec::new();
        if self.id.is_empty() || self.id.contains(KEY_SEP) {
            errors.push("view id is empty or contains the separator".into());
        }
        if self.label.trim().is_empty() {
            errors.push("view label must be non-empty".into());
        }
        validate_view_action(resource, &self.open_action, "open", &mut errors);
        if let Some(mount) = &self.mount {
            validate_view_action(resource, &mount.action, "mount", &mut errors);
            if mount.capability_param == mount.view_param {
                errors.push("mount capability and view parameters must be distinct".into());
            }
            if let Some(action) = resource
                .actions
                .iter()
                .find(|action| action.id == mount.action)
            {
                for (role, name) in [
                    ("capability", &mount.capability_param),
                    ("view", &mount.view_param),
                ] {
                    if name.trim().is_empty() {
                        errors.push(format!("mount {role} parameter must be non-empty"));
                    } else if !action.params().iter().any(|parameter| {
                        parameter.name.as_ref() == name
                            && parameter.ty == ParamType::Text
                            && !parameter.required
                    }) {
                        errors.push(format!(
                            "mount {role} parameter `{name}` must name an optional text parameter"
                        ));
                    }
                }
            }
        }
        if let Some(range) = &self.range {
            let names = [&range.start_param, &range.end_param, &range.unit_param];
            if names.iter().any(|name| name.trim().is_empty())
                || names[0] == names[1]
                || names[0] == names[2]
                || names[1] == names[2]
            {
                errors.push("range parameter names must be non-empty and distinct".into());
            }
            if let Some(action) = resource.actions.iter().find(|a| a.id == range.action) {
                if action.kind != OperationKind::View
                    || action.contract.confirmation != ConfirmationPolicy::None
                {
                    errors.push("range action must be a view without confirmation".into());
                }
                for (name, ty) in [
                    (&range.start_param, ParamType::Integer),
                    (&range.end_param, ParamType::Integer),
                    (&range.unit_param, ParamType::Text),
                ] {
                    if !action
                        .params()
                        .iter()
                        .any(|p| p.name.as_ref() == name && p.ty == ty)
                    {
                        errors.push(format!(
                            "range parameter `{name}` has the wrong type or is missing"
                        ));
                    }
                }
                if action
                    .params()
                    .iter()
                    .any(|p| p.required && !names.iter().any(|n| p.name.as_ref() == *n))
                {
                    errors.push("range action requires unrelated input".into());
                }
            } else {
                errors.push("range action is not exported".into());
            }
        }
        errors
    }
}

fn validate_view_action(
    resource: &ResourceSpec,
    action_id: &str,
    role: &str,
    errors: &mut Vec<String>,
) {
    let Some(action) = resource
        .actions
        .iter()
        .find(|action| action.id == action_id)
    else {
        errors.push(format!("{role} action `{action_id}` is not exported"));
        return;
    };
    if action.kind != OperationKind::View {
        errors.push(format!(
            "{role} action `{action_id}` must be a view operation"
        ));
    }
    if action.contract.confirmation != ConfirmationPolicy::None {
        errors.push(format!(
            "{role} action `{action_id}` must not require confirmation"
        ));
    }
    if action.params().iter().any(|parameter| parameter.required) {
        errors.push(format!(
            "{role} action `{action_id}` must not require caller parameters"
        ));
    }
}

/// Descriptive source-domain identity, independent of the live dispatch alias.
/// This metadata grants no permission or authority.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceSemanticIdentity {
    pub kind: String,
    pub key: String,
}

impl ResourceSemanticIdentity {
    pub const MAX_KIND_BYTES: usize = 128;
    pub const MAX_KEY_BYTES: usize = 4096;

    #[must_use]
    pub fn new(kind: impl Into<String>, key: impl Into<String>) -> Option<Self> {
        let identity = Self {
            kind: kind.into(),
            key: key.into(),
        };
        identity.is_valid().then_some(identity)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.kind.is_empty()
            && self.kind.len() <= Self::MAX_KIND_BYTES
            && self
                .kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            && !self.key.is_empty()
            && self.key.len() <= Self::MAX_KEY_BYTES
            && !self.key.chars().any(char::is_control)
    }
}

pub use crate::noun::{Noun, ResourceType, action_label, app_label, in_words, plural, singular};

/// Whether `kind` names a control the window already draws, or an
/// operation of the host it runs on: nodes published so a caller can
/// address them, never something a person looks for in a list.
#[must_use]
pub fn kind_mirrors_a_control(kind: &str) -> bool {
    kind == "tilo.control"
        || kind == "tl.host"
        || ["tilo.control.", "tilo.ui.", "tilo.host."]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
}

/// Whether `kind` names a control the window draws: its press, drag, focus
/// or value stands in for the pointer and keyboard of the person at the
/// device. Only that person invokes an action on it; anyone else, an agent
/// above all, is refused it and never offered it. What a control does that
/// others need, the app offers as an action of its own.
#[must_use]
pub fn kind_is_a_drawn_control(kind: &str) -> bool {
    kind == "tilo.control"
        || ["tilo.control.", "tilo.ui."]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
}

/// Whether a resource of `kind` titled `label` is something a person looks
/// for: titled, and the app's own rather than a mirror of one of its
/// controls or an operation of the host it runs on. Those are published to
/// be addressed, never listed.
#[must_use]
pub fn listed_for_people(kind: &str, label: &str) -> bool {
    !label.trim().is_empty() && !kind_mirrors_a_control(kind)
}

/// What kind of thing an action's one text argument names. Words are
/// what the asker says outright; a content hash addresses bytes already
/// in this machine's blob store (nobody types one, it names something
/// an earlier operation produced). Declared on the spec rather than
/// inferred, so a caller with no screen can tell "give me a new name"
/// from "give me the image to place".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Collects {
    /// Free text, in the words of whoever is asking.
    #[default]
    Words,
    /// The content hash of bytes already in this machine's blob store.
    Content,
}

/// How the palette collects an [`ActionSpec`]'s input when it is chosen.
///
/// A view of the action's type: the encoding itself comes from the spec
/// ([`ActionSpec::encode_immediate`], [`ActionSpec::build_deferred`],
/// [`ActionSpec::build_text`]), which binds the type to the spec's target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionInput<'a> {
    /// Fires at once, with no argument. Holds the encoding made when the
    /// action was declared, the same for every invocation; `None` for one
    /// encoded per invocation from its target
    /// ([`ActionSpec::encode_immediate`]), which a caller holding the
    /// resource tree resolves by address when it fires, as a deferred one.
    Immediate(Option<&'a Arc<EncodedAction>>),
    /// A no-argument operation built only at final admitted owner execution.
    /// Enumeration and confirmation retain declared intent, not minted bytes.
    Deferred { type_id: &'a str },
    /// Collects one free-text argument first, then builds its encoded action.
    Prompted {
        prompt: &'a str,
        placeholder: &'a str,
        /// What that argument names. See [`Collects`].
        collects: Collects,
    },
}

/// What an action type keeps of how the palette collects its input.
#[derive(Clone)]
enum InputMode {
    /// Fires at once. `Some` holds the encoding made when the action was
    /// declared (an action with no target, so one encoding for every
    /// invocation); `None` builds it from the target per invocation.
    Immediate(Option<Arc<EncodedAction>>),
    Deferred {
        type_id: String,
    },
    Prompted {
        prompt: String,
        placeholder: String,
        collects: Collects,
    },
}

/// What one [`ActionSpec`] acts on: the item its shared [`ActionType`] is
/// bound to (a note's path, a file's id). An action type's builders receive
/// it, so the type is held once for every item that offers it. An action
/// declared with its state captured in its builder has none.
#[derive(Clone, Default)]
pub struct Target(Option<Arc<dyn std::any::Any + Send + Sync>>);

impl Target {
    /// No target: the action's builder holds what it acts on.
    #[must_use]
    pub const fn none() -> Self {
        Self(None)
    }

    /// `value` as a target. Clone the target to bind several actions of one
    /// item to the same allocation.
    #[must_use]
    pub fn new<T: std::any::Any + Send + Sync>(value: T) -> Self {
        Self(Some(Arc::new(value)))
    }

    /// The target, when it is a `T`.
    #[must_use]
    pub fn get<T: std::any::Any>(&self) -> Option<&T> {
        self.0.as_deref()?.downcast_ref()
    }

    /// Whether `self` and `other` are one target: the same allocation, as
    /// the specs of one item bound from one target are. Two targets made
    /// apart are different even when their values are equal, so a resource
    /// bound anew is told from one kept.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

/// Builds a prompted action's encoded form for one target from its text.
type TargetTextBuilder = Arc<dyn Fn(&Target, &str) -> EncodedAction + Send + Sync>;

/// Collision-free, provider-defined identity for one continuation target or
/// handler. Tuple components remain separate; callers never join or hash them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationKey {
    namespace: String,
    parts: Vec<Vec<u8>>,
}

impl ContinuationKey {
    pub fn new(
        namespace: impl Into<String>,
        parts: impl IntoIterator<Item = impl Into<Vec<u8>>>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            parts: parts.into_iter().map(Into::into).collect(),
        }
    }
}

pub enum ContinuationArguments<'a> {
    Immediate,
    Text(&'a str),
    Json(&'a serde_json::Map<String, serde_json::Value>),
}

pub enum ContinuationQuery<'a> {
    Exact(ContinuationArguments<'a>),
    Draft,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuationEvidence {
    Exact {
        target: ContinuationKey,
        handler: ContinuationKey,
    },
    Draft {
        resolution_domain: ContinuationKey,
        handler: ContinuationKey,
    },
}

impl ContinuationEvidence {
    /// Stream unambiguous comparison data without allocating a serialized copy.
    /// The format is private to comparison consumers, not a wire protocol.
    pub fn write_canonical_identity(&self, sink: &mut impl std::io::Write) -> std::io::Result<()> {
        fn length(sink: &mut impl std::io::Write, value: usize) -> std::io::Result<()> {
            let value = u64::try_from(value).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "identity length overflow")
            })?;
            sink.write_all(&value.to_le_bytes())
        }
        fn bytes(sink: &mut impl std::io::Write, value: &[u8]) -> std::io::Result<()> {
            length(sink, value.len())?;
            sink.write_all(value)
        }
        fn key(sink: &mut impl std::io::Write, value: &ContinuationKey) -> std::io::Result<()> {
            bytes(sink, value.namespace.as_bytes())?;
            length(sink, value.parts.len())?;
            for part in &value.parts {
                bytes(sink, part)?;
            }
            Ok(())
        }
        sink.write_all(b"tilo.continuation-identity\0v1")?;
        match self {
            Self::Exact { target, handler } => {
                sink.write_all(&[0])?;
                key(sink, target)?;
                key(sink, handler)
            }
            Self::Draft {
                resolution_domain,
                handler,
            } => {
                sink.write_all(&[1])?;
                key(sink, resolution_domain)?;
                key(sink, handler)
            }
        }
    }
}

type ContinuationResolver =
    Arc<dyn for<'a> Fn(ContinuationQuery<'a>) -> Option<ContinuationEvidence> + Send + Sync>;

#[derive(Clone)]
enum InvocationBuilder {
    Fixed(Arc<EncodedAction>),
    Dynamic {
        build: ArgsBuilder,
        deferred_type: Option<String>,
    },
    /// A prompted action's builder of its one text, which the typed input mode
    /// reads from the action's [`prompt parameter`](ActionSpec::prompt_param).
    Text(TargetTextBuilder),
}

/// The facet naming the nested resource an action was addressed through,
/// when history names the enclosing resource whose action it is
/// ([`ActionSpec::history_context`]).
pub const NESTED_RESOURCE_FACET: &str = "part";

/// What an action is, whichever resource offers it: its id, label, contract,
/// parameters and builders. Held once per (resource type, action) and shared
/// by every [`ActionSpec`] that binds it to a [`Target`].
///
/// Read through the spec (`spec.label`, `spec.contract`); change one spec's
/// with [`ActionSpec::type_mut`], which copies a shared type first.
#[derive(Clone)]
pub struct ActionType {
    /// Stable id within the resource (e.g. `"like"`, `"reply"`).
    pub id: String,
    /// The id a change this action makes says it by, where that is not `id`: an app that offers
    /// its actions under a name (a tilolang app's `new_note`) records the declaration's id
    /// (`qymo5lg2`) in its changes, and history reads this action's label for them. Empty where
    /// the two are the same, as for a native verb.
    pub declared: String,
    pub label: String,
    /// The other words people use for this action, in plain language
    /// ("mkdir" and "make directory" for New folder): search finds the
    /// action by them as it does by its label. Declared with the label, so
    /// every surface that lists the action knows them; never a list kept
    /// inside a search provider.
    pub keywords: Vec<String>,
    /// What the action did, in the app's own words, said after who did it
    /// ("showed this on the canvas", "added a subtask"): the past-tense form a
    /// record of what was done reads, declared beside the label rather than
    /// made from it. `None` where the app declares none; a record then names
    /// the action by its label.
    pub did: Option<String>,
    /// The action's own picture beside its label, by its Lucide name.
    pub icon: Option<String>,
    pub description: Option<String>,
    /// What the action's row shows of the state it acts on: its badge,
    /// whether it flips that state in place, its shortcut. Published with
    /// the action and published anew whenever that state changes.
    pub state: ActionState,
    /// Declared execution locus (mirrors the concrete action's
    /// [`Action::locus`]), so the palette can show/route it before the
    /// action is built.
    pub locus: ExecutionLocus,
    /// Whether this mutates the shared document, performs an external effect,
    /// or changes only the invoking application's local view.
    pub kind: OperationKind,
    /// The single protocol type routed for an effect/view operation. Keeping
    /// this beside the descriptor lets the host prove that publication and
    /// the widget's handler registry agree, including prompted operations
    /// whose encoded value does not exist until arguments are bound.
    pub operation_type: Option<String>,
    /// Machine-readable safety, availability, lifecycle, and result contract.
    pub contract: ActionContract,
    /// How the palette collects the input. See [`ActionSpec::input`].
    input: InputMode,
    /// The typed argument surface an external caller invokes against. Private
    /// because it must stay in step with `build` — the two are always set
    /// together by a constructor, never independently.
    pub(crate) params: Vec<ActionParam>,
    /// Builds the encoded action from arguments bound against `params`. The
    /// palette's input mode uses the same closure, which is what makes the palette
    /// path and the external path provably one function.
    build: InvocationBuilder,
    /// Classifies one invocation's bound arguments when the action's risk
    /// depends on them. See [`ActionSpec::with_risk`].
    risk_of: Option<RiskClassifier>,
    /// Decides from one invocation's bound arguments whether Undo takes it
    /// back, when that depends on them. See [`ActionSpec::with_undo_of`].
    undo_of: Option<UndoClassifier>,
    /// Classifies what one invocation's bound arguments are, for the grants
    /// deciding it. See [`ActionSpec::with_need_of`].
    need_of: Option<NeedClassifier>,
    continuation_resolver: Option<ContinuationResolver>,
    /// Whether the action carries any of its resource's other actions (a
    /// generic edit, a group) rather than being a verb of its own. See
    /// [`ActionSpec::carrying`].
    carrier: bool,
    /// The kind and label of the enclosing resource whose action this one
    /// applies as, for a nested resource's action. See
    /// [`ActionSpec::applied_as`].
    applied_as: Option<(String, String)>,
    /// The provider that publishes and runs the action, when it is not the
    /// resource's publisher, and the resource it publishes it on there when
    /// another than the one it is offered on here: a view binds its
    /// controls to another provider's action ([`ActionSpec::run_by`]).
    provider: Option<Arc<RunBy>>,
}

/// Where another provider runs an action a view offers
/// ([`ActionSpec::run_by`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RunBy {
    /// The provider that publishes and runs it.
    pub provider: String,
    /// The resource it publishes it on, when another than the one it is
    /// offered on here (a row's operation on a record the provider keeps).
    pub resource: Option<String>,
    /// The action it publishes and the arguments this one fixes of it,
    /// when it is offered here as another (a bound edit of its field edit).
    pub invokes: Option<(String, serde_json::Map<String, serde_json::Value>)>,
}

impl ActionType {
    /// This type bound to `target`: the action one item offers.
    #[must_use]
    pub fn bind(self: &Arc<Self>, target: Target) -> ActionSpec {
        ActionSpec {
            ty: Arc::clone(self),
            target,
            bound: None,
        }
    }

    fn encode(&self, target: &Target, args: &ActionArgs) -> EncodedAction {
        match &self.build {
            InvocationBuilder::Fixed(encoded) => encoded.as_ref().clone(),
            InvocationBuilder::Dynamic { build, .. } => build(target, args),
            InvocationBuilder::Text(build) => {
                let text = primary_param(&self.params)
                    .and_then(|param| args.text(&param.name))
                    .unwrap_or("");
                build(target, text)
            }
        }
    }
}

/// One action a resource exports to the palette: an [`ActionType`] bound to
/// the [`Target`] it acts on. Small: the type is shared, so a resource of
/// ten thousand items holds each of its types once.
///
/// It reads as its type (`spec.id`, `spec.contract`).
#[derive(Clone)]
pub struct ActionSpec {
    ty: Arc<ActionType>,
    target: Target,
    /// The arguments this action fixes, when it is a declaration offered on
    /// the item it acts on, and so not among `params`: the item's, as its
    /// target is, so the type stays shared. See [`ActionSpec::with_bound`].
    bound: Option<Arc<[(String, serde_json::Value)]>>,
}

impl std::ops::Deref for ActionSpec {
    type Target = ActionType;

    fn deref(&self) -> &ActionType {
        &self.ty
    }
}

/// Classifies the risk of one invocation from its bound arguments.
/// The [`Target`] is that of the spec being invoked.
pub type RiskClassifier = Arc<dyn Fn(&Target, &ActionArgs) -> RiskLevel + Send + Sync>;

/// Decides from one invocation's bound arguments whether Undo takes it back.
pub type UndoClassifier = Arc<dyn Fn(&ActionArgs) -> bool + Send + Sync>;

/// Classifies what one invocation is from its bound arguments.
/// The [`Target`] is that of the spec being invoked.
pub type NeedClassifier = Arc<dyn Fn(&Target, &ActionArgs) -> ActionNeed + Send + Sync>;

struct TypedActionSeed {
    id: String,
    label: String,
    placeholder: String,
    locus: ExecutionLocus,
    kind: OperationKind,
    operation_type: Option<String>,
    params: Vec<ActionParam>,
}

/// What a constructor declares of a new action type before its input mode and
/// builder.
struct Declared {
    id: String,
    label: String,
    locus: ExecutionLocus,
    kind: OperationKind,
    operation_type: Option<String>,
    params: Vec<ActionParam>,
}

impl Declared {
    fn new(id: String, label: String, locus: ExecutionLocus, kind: OperationKind) -> Self {
        Self {
            id,
            label,
            locus,
            kind,
            operation_type: None,
            params: Vec::new(),
        }
    }
}

impl ActionSpec {
    fn of(ty: ActionType) -> Self {
        Self {
            ty: Arc::new(ty),
            target: Target::none(),
            bound: None,
        }
    }

    /// The type this spec binds: what every resource offering the action
    /// shares. Bind it to other targets with [`ActionType::bind`].
    #[must_use]
    pub fn action_type(&self) -> &Arc<ActionType> {
        &self.ty
    }

    /// What this spec acts on.
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// This spec's type, to change: copied first when other specs share it,
    /// so a change to one item's action never reaches another's.
    pub fn type_mut(&mut self) -> &mut ActionType {
        Arc::make_mut(&mut self.ty)
    }

    /// How the palette collects this action's input.
    #[must_use]
    pub fn input(&self) -> ActionInput<'_> {
        match &self.input {
            InputMode::Immediate(encoded) => ActionInput::Immediate(encoded.as_ref()),
            InputMode::Deferred { type_id } => ActionInput::Deferred { type_id },
            InputMode::Prompted {
                prompt,
                placeholder,
                collects,
            } => ActionInput::Prompted {
                prompt,
                placeholder,
                collects: *collects,
            },
        }
    }

    /// The action an [`Immediate`](ActionInput::Immediate) spec fires, or
    /// `None` for one that takes input or is deferred. An action declared
    /// with what it acts on in its builder was encoded once, when declared;
    /// one bound to a target is encoded for it now.
    #[must_use]
    pub fn encode_immediate(&self) -> Option<Arc<EncodedAction>> {
        match &self.input {
            InputMode::Immediate(Some(encoded)) => Some(Arc::clone(encoded)),
            InputMode::Immediate(None) => {
                Some(Arc::new(self.ty.encode(&self.target, &ActionArgs::none())))
            }
            InputMode::Deferred { .. } | InputMode::Prompted { .. } => None,
        }
    }

    /// The encoding an [`Immediate`](ActionInput::Immediate) spec already holds, when it was
    /// encoded once and shared by every resource offering it; `None` for one built from its
    /// target when it is invoked. Reads, never builds: validating a surface of ten thousand
    /// items runs no action.
    #[must_use]
    pub fn published_encoding(&self) -> Option<&Arc<EncodedAction>> {
        match &self.input {
            InputMode::Immediate(encoded) => encoded.as_ref(),
            InputMode::Deferred { .. } | InputMode::Prompted { .. } => None,
        }
    }

    /// Run a [`Deferred`](ActionInput::Deferred) spec's builder, unchecked:
    /// `None` for any other. Callers check the result's type against the
    /// declared one, as the API does.
    #[must_use]
    pub fn build_deferred(&self) -> Option<EncodedAction> {
        match &self.input {
            InputMode::Deferred { .. } => Some(self.ty.encode(&self.target, &ActionArgs::none())),
            InputMode::Immediate(_) | InputMode::Prompted { .. } => None,
        }
    }

    /// This action as `provider` publishes and runs it: a view shows another
    /// provider's resource and binds its controls to that provider's
    /// actions. The view's publisher never runs nor publishes it; its host
    /// invokes it at `provider`, which admits and runs it as any caller's.
    #[must_use]
    pub fn run_by(self, provider: impl Into<String>) -> Self {
        self.run_as(RunBy {
            provider: provider.into(),
            resource: None,
            invokes: None,
        })
    }

    /// [`Self::run_by`] as `run` says where the provider publishes it.
    #[must_use]
    pub fn run_as(mut self, run: RunBy) -> Self {
        self.type_mut().provider = Some(Arc::new(run));
        self
    }

    /// Where another provider than the resource's publisher runs this
    /// action ([`Self::run_by`]).
    #[must_use]
    pub fn run_by_provider(&self) -> Option<&RunBy> {
        self.provider.as_deref()
    }

    /// The provider that runs this action, when it is another than the
    /// resource's publisher ([`Self::run_by`]).
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref().map(|run| run.provider.as_str())
    }

    /// Mark this action as applied as the enclosing resource of kind `kind`
    /// (labelled `label`): a nested resource (a note's rich-text body)
    /// publishes its document actions re-encoded as the record's field
    /// edit, so the record's surface, permissions and history decide them.
    #[must_use]
    pub fn applied_as(mut self, kind: impl Into<String>, label: impl Into<String>) -> Self {
        if self.kind == OperationKind::Document {
            self.type_mut().applied_as = Some((kind.into(), label.into()));
        }
        self
    }

    /// The history entry of invoking this action on the resource `id`, of
    /// kind `kind` and labelled `label`. It names that resource, of the kind
    /// whose action the value is: for a nested resource's action, the
    /// enclosing resource's kind and label, with the nested resource kept as
    /// a [`NESTED_RESOURCE_FACET`] facet. Naming the nested kind made the
    /// change claim a kind its app's surface does not describe, so every
    /// receiver held it and the signer refused it.
    #[must_use]
    pub fn history_context(&self, id: &str, kind: &str, label: &str) -> crate::ActionContext {
        match &self.applied_as {
            Some((owner_kind, owner_label)) => {
                let mut context = crate::ActionContext::for_resource(id, owner_kind, owner_label);
                context
                    .facets
                    .push(crate::ActionFacet::new(NESTED_RESOURCE_FACET, label));
                context
            }
            None => crate::ActionContext::for_resource(id, kind, label),
        }
    }

    /// Borrow the actual fixed encoding used by the typed invocation builder.
    /// `None` means the builder is opaque, even if its palette input is Immediate.
    /// This is comparison data, never permission to invoke an action.
    pub fn fixed_invocation_encoding(&self) -> Option<&EncodedAction> {
        match &self.build {
            InvocationBuilder::Fixed(encoded) => Some(encoded),
            InvocationBuilder::Dynamic { .. } | InvocationBuilder::Text(_) => None,
        }
    }

    /// Whether a provider installed continuation evidence. An installed provider
    /// returning unknown is distinct from having no provider.
    pub fn has_continuation_identity(&self) -> bool {
        self.continuation_resolver.is_some()
    }

    /// Declared type of an explicit deferred invocation. This is inert metadata,
    /// never an encoding or authorization to execute the retained builder.
    pub fn deferred_invocation_type(&self) -> Option<&str> {
        match &self.build {
            InvocationBuilder::Dynamic { deferred_type, .. } => deferred_type.as_deref(),
            InvocationBuilder::Fixed(_) | InvocationBuilder::Text(_) => None,
        }
    }

    pub(crate) fn validate_deferred_shape(&self) -> Result<(), ArgError> {
        match (&self.input, self.deferred_invocation_type()) {
            (InputMode::Deferred { type_id }, Some(expected))
                if !type_id.is_empty()
                    && type_id == expected
                    && self.params.is_empty()
                    && match self.kind {
                        OperationKind::Document => self.operation_type.is_none(),
                        OperationKind::Effect | OperationKind::View => {
                            self.operation_type.as_deref() == Some(expected)
                        }
                    } =>
            {
                Ok(())
            }
            (InputMode::Deferred { .. }, _) | (_, Some(_)) => Err(ArgError::Constraint {
                name: "deferred".into(),
                reason: "deferred input and retained declaration disagree".into(),
            }),
            (_, None) => Ok(()),
        }
    }

    fn validate_built(&self, encoded: EncodedAction) -> Result<EncodedAction, ArgError> {
        // A refusal carries the builder's reason to the caller, as a typed
        // action's does.
        if let Some(expected) = self.deferred_invocation_type()
            && encoded.type_id != expected
            && encoded.type_id != INVALID_ARGUMENTS_TYPE
        {
            return Err(ArgError::Constraint {
                name: "deferred".into(),
                reason: format!(
                    "builder returned `{}` but declared `{expected}`",
                    encoded.type_id
                ),
            });
        }
        Ok(encoded)
    }

    /// Strict argument validation without invoking an encoder.
    pub fn validate_json(
        &self,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ArgError> {
        self.validate_deferred_shape()?;
        ActionArgs::bind_json(&self.params, self.without_bound(supplied)?.as_ref()).map(|_| ())
    }

    /// The arguments this action fixes ([`Self::with_bound`]), by name.
    #[must_use]
    pub fn bound(&self) -> &[(String, serde_json::Value)] {
        self.bound.as_deref().unwrap_or_default()
    }

    /// This action fixing `values`: a declaration offered on the item it
    /// acts on ("rename *this* album"), its item parameter bound and left
    /// out of [`Self::params`]. A caller that passes a bound argument anyway
    /// is heard as leaving it out when it names the bound value (an item's
    /// id also in a longer form of the item's address, `sheets/<id>`), and
    /// refused, naming the bound value, when it names another: the action
    /// acts on its item only.
    #[must_use]
    pub fn with_bound(mut self, values: Vec<(String, serde_json::Value)>) -> Self {
        self.bound = (!values.is_empty()).then(|| values.into());
        self
    }

    /// The bound argument `name`, stated in what this action publishes: an
    /// optional parameter whose only accepted value is the one this action
    /// is bound to, described as `description`. A row of a person's own
    /// window invokes it without naming anything — it is bound here — while
    /// a caller reading the published action learns which item this one is
    /// bound to, and answers that one among several a page or a list
    /// publishes side by side. Calling it again for another name states that
    /// one beside the first, so a row can state every value it is bound to.
    ///
    /// Nothing an invocation carries changes: the value invocations run is
    /// still the bound one ([`Self::with_bound`]), and naming it is how a
    /// caller says which of several bound rows it means.
    #[must_use]
    pub fn stating_bound(mut self, name: &str, description: &str) -> Self {
        let Some((_, value)) = self.bound().iter().find(|(bound, _)| bound == name) else {
            return self;
        };
        let value = match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let ty = Arc::make_mut(&mut self.ty);
        let stated = crate::ActionParam {
            name: name.to_owned().into(),
            ty: crate::ParamType::Text,
            required: false,
            entry: false,
            description: description.to_owned().into(),
            choices: vec![value.into()],
            schema: None,
            nullable: false,
        };
        match ty.params.iter_mut().find(|param| param.name == name) {
            Some(param) => *param = stated,
            None => ty.params.push(stated),
        }
        self
    }

    /// `supplied` without the bound arguments it repeats
    /// ([`Self::with_bound`]).
    fn without_bound<'a>(
        &self,
        supplied: &'a serde_json::Map<String, serde_json::Value>,
    ) -> Result<std::borrow::Cow<'a, serde_json::Map<String, serde_json::Value>>, ArgError> {
        let repeated: Vec<&(String, serde_json::Value)> = self
            .bound()
            .iter()
            .filter(|(name, _)| supplied.contains_key(name))
            .collect();
        if repeated.is_empty() {
            return Ok(std::borrow::Cow::Borrowed(supplied));
        }
        let mut kept = supplied.clone();
        for (name, value) in repeated {
            let given = kept.remove(name).unwrap_or_default();
            check_bound(name, value, &given)?;
        }
        Ok(std::borrow::Cow::Owned(kept))
    }

    /// `supplied` pairs without the bound arguments they repeat.
    fn pairs_without_bound(
        &self,
        supplied: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<Vec<(String, ArgValue)>, ArgError> {
        let mut kept = Vec::new();
        for (name, given) in supplied {
            match self.bound().iter().find(|(bound, _)| *bound == name) {
                Some((_, value)) => check_bound(&name, value, &given.to_json())?,
                None => kept.push((name, given)),
            }
        }
        Ok(kept)
    }

    /// The spec of a newly declared type, bound to no target.
    fn declared(
        seed: Declared,
        contract: ActionContract,
        input: InputMode,
        build: InvocationBuilder,
    ) -> Self {
        Self::of(ActionType {
            id: seed.id,
            declared: String::new(),
            label: seed.label,
            keywords: Vec::new(),
            did: None,
            icon: None,
            description: None,
            state: ActionState::default(),
            locus: seed.locus,
            kind: seed.kind,
            operation_type: seed.operation_type,
            contract,
            input,
            params: seed.params,
            build,
            risk_of: None,
            undo_of: None,
            need_of: None,
            continuation_resolver: None,
            carrier: false,
            applied_as: None,
            provider: None,
        })
    }

    /// A document operation whose no-argument builder runs only at execution.
    /// Both invocation input modes share this one closure; construction never runs it.
    pub fn deferred(
        id: impl Into<String>,
        label: impl Into<String>,
        locus: ExecutionLocus,
        type_id: impl Into<String>,
        build: impl Fn() -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::deferred_on(id, label, locus, type_id, move |_| build())
    }

    /// [`deferred`](Self::deferred) for a type shared by many targets: the
    /// builder is given the target of the spec being invoked.
    pub fn deferred_on(
        id: impl Into<String>,
        label: impl Into<String>,
        locus: ExecutionLocus,
        type_id: impl Into<String>,
        build: impl Fn(&Target) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::deferred_with_kind(
            Declared::new(id.into(), label.into(), locus, OperationKind::Document),
            type_id.into(),
            build,
        )
    }

    /// An effect/view operation with deferred no-argument encoding.
    pub fn deferred_operation(
        id: impl Into<String>,
        label: impl Into<String>,
        kind: OperationKind,
        type_id: impl Into<String>,
        build: impl Fn() -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::deferred_with_kind(
            Declared::new(id.into(), label.into(), ExecutionLocus::ClientLocal, kind),
            type_id.into(),
            move |_| build(),
        )
    }

    fn deferred_with_kind(
        mut seed: Declared,
        type_id: String,
        build: impl Fn(&Target) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        seed.operation_type = (seed.kind != OperationKind::Document).then(|| type_id.clone());
        let contract = ActionContract::for_operation(seed.kind);
        Self::declared(
            seed,
            contract,
            InputMode::Deferred {
                type_id: type_id.clone(),
            },
            InvocationBuilder::Dynamic {
                build: Arc::new(move |target, _| build(target)),
                deferred_type: Some(type_id),
            },
        )
    }

    /// An action that fires immediately with `encoded`, declared as `seed`
    /// under `contract`.
    fn fixed(seed: Declared, contract: ActionContract, encoded: EncodedAction) -> Self {
        let encoded = Arc::new(encoded);
        Self::declared(
            seed,
            contract,
            InputMode::Immediate(Some(Arc::clone(&encoded))),
            InvocationBuilder::Fixed(encoded),
        )
    }

    /// An action that fires immediately. `type_id` must match the app's
    /// registry registration for `A`.
    ///
    /// Projects to zero parameters: invoking it externally with an empty
    /// argument set yields exactly the pre-encoded action the palette fires.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError`] when `action` cannot be encoded.
    pub fn immediate<A>(
        id: impl Into<String>,
        label: impl Into<String>,
        type_id: impl Into<String>,
        action: &A,
    ) -> Result<Self, ActionError>
    where
        A: Action + serde::Serialize,
    {
        let locus = action.locus();
        let encoded = EncodedAction::encode(type_id, action)?;
        Ok(Self::fixed(
            Declared::new(id.into(), label.into(), locus, OperationKind::Document),
            unnamed_kind(ActionContract::default()),
            encoded,
        ))
    }

    /// An action that fires immediately, from an [`EncodedAction`] the caller
    /// already holds.
    ///
    /// [`immediate`](Self::immediate) is the usual API — it takes a typed
    /// action and reads its [`locus`](Action::locus) — but a caller that has
    /// only the encoded form (a stored action, a fixture, one that arrived over
    /// a wire) has no typed value to hand it. Projects to zero parameters, the
    /// same as `immediate`.
    pub fn immediate_encoded(
        id: impl Into<String>,
        label: impl Into<String>,
        locus: ExecutionLocus,
        encoded: EncodedAction,
    ) -> Self {
        Self::fixed(
            Declared::new(id.into(), label.into(), locus, OperationKind::Document),
            unnamed_kind(ActionContract::default()),
            encoded,
        )
    }

    /// An action that first collects a free-text argument, then builds its
    /// encoded action from it, the argument being the parameter `param`:
    /// `ActionParam::required("title", ParamType::Text, "The note's new
    /// title")`. The name and description are what an agent reads, and the
    /// description is also the palette's prompt.
    ///
    /// The projection and the palette share the *same* `build` closure, so an
    /// external invocation and the on-screen affordance produce the
    /// byte-identical [`EncodedAction`]. Callers that hold the typed text
    /// find where it goes with [`Self::prompt_param`], never by name. Name the
    /// parameter for what it is (`title`, `name`, `comment`): an agent reads
    /// it, and the generic [`PROMPTED_PARAM`] tells it nothing.
    pub fn prompted_param(
        id: impl Into<String>,
        label: impl Into<String>,
        param: ActionParam,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::prompted_param_on(id, label, param, placeholder, locus, move |_, text| {
            build(text)
        })
    }

    /// [`prompted_param`](Self::prompted_param) for a type shared by many
    /// targets: the builder is given the target of the spec being invoked.
    pub fn prompted_param_on(
        id: impl Into<String>,
        label: impl Into<String>,
        param: ActionParam,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&Target, &str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let mut seed = Declared::new(id.into(), label.into(), locus, OperationKind::Document);
        let prompt = param.description.to_string();
        seed.params = vec![param];
        Self::declared(
            seed,
            unnamed_kind(ActionContract::default()),
            InputMode::Prompted {
                prompt,
                placeholder: placeholder.into(),
                collects: Collects::Words,
            },
            InvocationBuilder::Text(Arc::new(build)),
        )
    }

    /// The name of the parameter the typed text of a prompted action goes
    /// into, or `None` for an action that collects no text. The one place a
    /// caller holding the text of a prompt (the palette, a broker request, a
    /// continuation) learns where it belongs.
    pub fn prompt_param(&self) -> Option<&str> {
        match self.input {
            InputMode::Prompted { .. } => {
                primary_param(self.params()).map(|param| param.name.as_ref())
            }
            _ => None,
        }
    }

    /// The parameter one line of typed text fills, when the action is a
    /// capture: it declares one ([`ActionParam::entry`]).
    #[must_use]
    pub fn entry_param(&self) -> Option<&str> {
        self.params()
            .iter()
            .find(|param| param.entry)
            .map(|param| param.name.as_ref())
    }

    /// `text` as this action's typed arguments, when it is a prompted action.
    pub fn prompt_arguments(
        &self,
        text: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let name = self.prompt_param()?;
        let mut arguments = serde_json::Map::new();
        arguments.insert(name.to_owned(), serde_json::Value::String(text.to_owned()));
        Some(arguments)
    }

    /// An immediate effect or view operation owned by the application.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Encode`] if `payload` fails to serialize.
    pub fn operation<T>(
        id: impl Into<String>,
        label: impl Into<String>,
        kind: OperationKind,
        type_id: impl Into<String>,
        payload: &T,
    ) -> Result<Self, ActionError>
    where
        T: serde::Serialize,
    {
        debug_assert_ne!(kind, OperationKind::Document);
        let type_id = type_id.into();
        let encoded = EncodedAction::encode_operation(type_id.clone(), payload)?;
        let mut seed = Declared::new(id.into(), label.into(), ExecutionLocus::ClientLocal, kind);
        seed.operation_type = Some(type_id);
        Ok(Self::fixed(
            seed,
            ActionContract::for_operation(kind),
            encoded,
        ))
    }

    /// A prompted effect or view operation owned by the application.
    pub fn prompted_operation(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        kind: OperationKind,
        type_id: impl Into<String>,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        debug_assert_ne!(kind, OperationKind::Document);
        let prompt = prompt.into();
        let type_id = type_id.into();
        let expected = type_id.clone();
        let mut seed = Declared::new(id.into(), label.into(), ExecutionLocus::ClientLocal, kind);
        seed.operation_type = Some(type_id);
        seed.params = vec![ActionParam::required_owned(
            PROMPTED_PARAM,
            ParamType::Text,
            prompt.clone(),
        )];
        Self::declared(
            seed,
            ActionContract::for_operation(kind),
            InputMode::Prompted {
                prompt,
                placeholder: placeholder.into(),
                collects: Collects::Words,
            },
            InvocationBuilder::Text(Arc::new(move |_, value| {
                enforce_operation_type(build(value), &expected)
            })),
        )
    }

    /// An action with a real typed argument surface. Framework-internal: the
    /// declaration macros and the tilolang publisher build specs, never an app.
    ///
    /// This is the constructor for an argument that has a *type* — a number in
    /// `0..=1`, a rate, one of a fixed set of filter names — rather than prose
    /// a lenient parser guesses at. [`ActionArgs::bind`] enforces the
    /// declaration, so a caller that sends `"hlaf"` for a choice parameter gets
    /// a named refusal instead of a silent default.
    ///
    /// The palette can still offer it. Its [`input`](Self::input) is derived
    /// from `params`:
    ///
    /// * no parameters → [`Immediate`](ActionInput::Immediate) of the action
    ///   `build` returns for an empty argument set;
    /// * otherwise → [`Prompted`](ActionInput::Prompted) collecting the
    ///   *primary* parameter (the first required one, else the first), parsed
    ///   into its declared type.
    ///
    /// A palette entry is one text field, so for a multi-parameter action it
    /// can only carry the primary argument and the rest are left unbound —
    /// which is why `build` sees `ActionArgs` and decides what an absent
    /// optional means. Text the palette cannot parse into the primary
    /// parameter's type is likewise left unbound rather than coerced; only the
    /// keyboard path can be that lossy, and the external path stays strict.
    #[doc(hidden)]
    pub fn typed(
        id: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        params: Vec<ActionParam>,
        build: impl Fn(&ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::typed_with_kind(
            TypedActionSeed {
                id: id.into(),
                label: label.into(),
                placeholder: placeholder.into(),
                locus,
                kind: OperationKind::Document,
                operation_type: None,
                params,
            },
            Arc::new(move |_, args| build(args)),
            true,
        )
    }

    /// [`typed`](Self::typed) for a type shared by many targets: the builder
    /// is given the target of the spec being invoked. A nullary one is
    /// encoded per invocation, as its target says what it does.
    #[doc(hidden)]
    pub fn typed_on(
        id: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        params: Vec<ActionParam>,
        build: impl Fn(&Target, &ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::typed_with_kind(
            TypedActionSeed {
                id: id.into(),
                label: label.into(),
                placeholder: placeholder.into(),
                locus,
                kind: OperationKind::Document,
                operation_type: None,
                params,
            },
            Arc::new(build),
            false,
        )
    }

    /// A typed effect or view operation. This is the non-document counterpart
    /// to [`typed`](Self::typed), with the same strict multi-parameter binding.
    #[doc(hidden)]
    pub fn typed_operation(
        id: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        kind: OperationKind,
        type_id: impl Into<String>,
        params: Vec<ActionParam>,
        build: impl Fn(&ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        debug_assert_ne!(kind, OperationKind::Document);
        Self::typed_with_kind(
            TypedActionSeed {
                id: id.into(),
                label: label.into(),
                placeholder: placeholder.into(),
                locus: ExecutionLocus::ClientLocal,
                kind,
                operation_type: Some(type_id.into()),
                params,
            },
            Arc::new(move |_, args| build(args)),
            true,
        )
    }

    /// [`typed_operation`](Self::typed_operation) for a type shared by many
    /// targets, as [`typed_on`](Self::typed_on) is for [`typed`](Self::typed).
    ///
    /// The builder is boxed where it is made, so the toolkit compiles this
    /// once rather than every app once per vocabulary.
    #[doc(hidden)]
    pub fn typed_operation_on(
        id: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        kind: OperationKind,
        type_id: impl Into<String>,
        params: Vec<ActionParam>,
        build: ArgsBuilder,
    ) -> Self {
        debug_assert_ne!(kind, OperationKind::Document);
        Self::typed_with_kind(
            TypedActionSeed {
                id: id.into(),
                label: label.into(),
                placeholder: placeholder.into(),
                locus: ExecutionLocus::ClientLocal,
                kind,
                operation_type: Some(type_id.into()),
                params,
            },
            build,
            false,
        )
    }

    /// `eager`: encode a nullary action once, now, as an action that holds
    /// what it acts on in its builder always has.
    fn typed_with_kind(seed: TypedActionSeed, build: ArgsBuilder, eager: bool) -> Self {
        let build: ArgsBuilder = if let Some(expected) = seed.operation_type.clone() {
            Arc::new(move |target, args| enforce_operation_type(build(target, args), &expected))
        } else {
            build
        };
        let input = match primary_param(&seed.params) {
            None => InputMode::Immediate(
                eager.then(|| Arc::new(build(&Target::none(), &ActionArgs::none()))),
            ),
            Some(primary) => InputMode::Prompted {
                prompt: primary.description.to_string(),
                placeholder: seed.placeholder,
                collects: Collects::Words,
            },
        };
        let contract = unnamed_kind(ActionContract::for_operation(seed.kind));
        Self::declared(
            Declared {
                id: seed.id,
                label: seed.label,
                locus: seed.locus,
                kind: seed.kind,
                operation_type: seed.operation_type,
                params: seed.params,
            },
            contract,
            input,
            InvocationBuilder::Dynamic {
                build,
                deferred_type: None,
            },
        )
    }

    /// Fire `encoded` at once when a person chooses the action (the palette,
    /// a menu, the shortcut), asking nothing; a caller that passes arguments
    /// still binds them against its parameters. For a typed action whose
    /// arguments all have a meaning when left out (open search, unnarrowed).
    #[must_use]
    pub fn with_immediate(mut self, encoded: EncodedAction) -> Self {
        self.type_mut().input = InputMode::Immediate(Some(Arc::new(encoded)));
        self
    }

    /// Fire at once when a person chooses the action, asking nothing, encoded per invocation
    /// from its target: [`with_immediate`](Self::with_immediate) for a type shared by many
    /// targets. For a typed action whose parameters are all optional, each with a meaning when
    /// left out; a caller that passes arguments still binds them against its parameters. An
    /// action with a required parameter keeps asking for it.
    #[must_use]
    pub fn with_immediate_per_target(mut self) -> Self {
        if self.params().iter().all(|param| !param.required) {
            self.type_mut().input = InputMode::Immediate(None);
        }
        self
    }

    /// Opt one action into inert, provider-defined continuation evidence.
    #[must_use]
    pub fn with_continuation_identity(
        mut self,
        resolver: impl for<'a> Fn(ContinuationQuery<'a>) -> Option<ContinuationEvidence>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.type_mut().continuation_resolver = Some(Arc::new(resolver));
        self
    }

    /// Resolve comparison-only evidence for the exact input or draft domain.
    /// A mismatched response variant is rejected as unsupported.
    pub fn continuation_identity(
        &self,
        query: ContinuationQuery<'_>,
    ) -> Option<ContinuationEvidence> {
        let exact = matches!(query, ContinuationQuery::Exact(_));
        let evidence = self.continuation_resolver.as_ref()?(query)?;
        match (exact, &evidence) {
            (true, ContinuationEvidence::Exact { .. })
            | (false, ContinuationEvidence::Draft { .. }) => Some(evidence),
            _ => None,
        }
    }

    /// An action whose one argument is the content hash of bytes this
    /// machine already holds, rather than typed words — a separate
    /// constructor (not a parameter on [`Self::prompted_param`]) so every
    /// existing action keeps saying exactly what it said.
    pub fn prompted_content(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::prompted_content_on(id, label, prompt, placeholder, locus, move |_, hash| {
            build(hash)
        })
    }

    /// [`prompted_content`](Self::prompted_content) for a type shared by many
    /// targets: the builder is given the target of the spec being invoked.
    pub fn prompted_content_on(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&Target, &str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let mut spec = Self::prompted_param_on(
            id,
            label,
            ActionParam::required_owned(PROMPTED_PARAM, ParamType::Text, prompt.into()),
            placeholder,
            locus,
            build,
        );
        if let InputMode::Prompted { collects, .. } = &mut spec.type_mut().input {
            *collects = Collects::Content;
        }
        spec
    }

    /// Declare the other words people use for this action
    /// ([`Self::keywords`]): plain words, one phrase each, no repetition of
    /// the label.
    #[must_use]
    pub fn with_keywords<S: Into<String>>(mut self, keywords: impl IntoIterator<Item = S>) -> Self {
        self.type_mut().keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    /// Declare what the action did, said after who did it ([`Self::did`]):
    /// "showed this on the canvas", "added a subtask".
    #[must_use]
    pub fn with_did(mut self, did: impl Into<String>) -> Self {
        let did = did.into();
        self.type_mut().did = Some(did).filter(|did| !did.trim().is_empty());
        self
    }

    /// Give the action its own picture ([`Self::icon`]), by its Lucide name.
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.type_mut().icon = Some(icon.into());
        self
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.type_mut().description = Some(description.into());
        self
    }

    /// Show `state` on the action's row ([`ActionState`]): publish the
    /// action anew when it changes.
    #[must_use]
    pub fn with_state(mut self, state: ActionState) -> Self {
        self.type_mut().state = state;
        self
    }

    #[must_use]
    pub fn with_contract(mut self, contract: ActionContract) -> Self {
        self.type_mut().contract = contract;
        self
    }

    #[must_use]
    pub fn unavailable(mut self, code: impl Into<String>, reason: impl Into<String>) -> Self {
        self.type_mut().contract.availability = Availability::Unavailable {
            code: code.into(),
            reason: reason.into(),
        };
        self
    }

    /// A deletion: its kind is
    /// [`deletion_kind`](ActionContract::deletion_kind), and the tags the spec
    /// already carries settle its confirmation — asked with a click unless a
    /// compensated undo says Undo takes it back.
    #[must_use]
    pub fn destructive(mut self) -> Self {
        let contract = &mut self.type_mut().contract;
        contract.risk = RiskLevel::Destructive;
        contract.kind = contract.deletion_kind();
        contract.settle_confirmation();
        self
    }

    /// List this action for `audience` only ([`Audience`](crate::Audience)).
    /// Recorded in the contract's tags, so the audience crosses every
    /// boundary the inert contract does. Call after
    /// [`with_contract`](Self::with_contract), which replaces the tags.
    #[must_use]
    pub fn for_audience(mut self, audience: crate::Audience) -> Self {
        self.type_mut().contract.set_audience(audience);
        self
    }

    /// Declare this read-only action as the one that answers, with no
    /// arguments, the first page of the app's open document. Recorded as
    /// [`DOCUMENT_READ_TAG`](crate::DOCUMENT_READ_TAG), so a caller that just
    /// opened a file finds it in the directory. Call after
    /// [`with_contract`](Self::with_contract), which replaces the tags.
    #[must_use]
    pub fn reads_document(mut self) -> Self {
        push_tag(
            &mut self.type_mut().contract,
            crate::contract::DOCUMENT_READ_TAG,
        );
        self
    }

    /// Declare this view as a read of what its record holds that takes
    /// arguments (a version, a review of one): recorded as
    /// [`RESOURCE_READ_TAG`](crate::RESOURCE_READ_TAG), it moves nothing, anyone
    /// who may read the record may run it, and it discloses the record to
    /// them. Call after [`with_contract`](Self::with_contract), which replaces
    /// the tags.
    #[must_use]
    pub fn reads_resource(mut self) -> Self {
        push_tag(
            &mut self.type_mut().contract,
            crate::contract::RESOURCE_READ_TAG,
        );
        self
    }

    /// Declare this action as the one that opens the file its `path`
    /// parameter names in this app instance, of any type when `extensions`
    /// is empty, else of those (lowercase, no dot). Recorded as
    /// [`OPENS_PATH_TAG`](crate::OPENS_PATH_TAG), so a caller that launches
    /// the app with a file opens it here, and never through an action
    /// that only happens to take a `path`. Call after
    /// [`with_contract`](Self::with_contract), which replaces the tags.
    #[must_use]
    pub fn opens_path(mut self, extensions: &[&str]) -> Self {
        push_tag(
            &mut self.type_mut().contract,
            crate::contract::OPENS_PATH_TAG,
        );
        for extension in extensions {
            push_tag(
                &mut self.type_mut().contract,
                &format!(
                    "{}{}",
                    crate::contract::OPENS_PATH_EXTENSION_PREFIX,
                    extension.to_ascii_lowercase()
                ),
            );
        }
        self
    }

    /// Who this action is listed for. See [`for_audience`](Self::for_audience).
    pub fn audience(&self) -> crate::Audience {
        self.contract.audience()
    }

    /// Whether people's listings (the palette, menus) offer it.
    pub fn reaches_people(&self) -> bool {
        self.contract.reaches_people()
    }

    /// Whether agents' listings offer it.
    pub fn reaches_agents(&self) -> bool {
        self.contract.reaches_agents()
    }

    /// Whether a row a person picks can supply what it asks for: nothing, or
    /// the one line its prompt collects (a name, a number, a date). One that
    /// needs several arguments, or a list, an object or another resource's
    /// address, is taken by callers that name them (agents, scripts, the
    /// controls bound to where the person is), never from a row.
    pub fn takes_a_row_answer(&self) -> bool {
        let mut required = self.params.iter().filter(|param| param.required);
        match (required.next(), required.next()) {
            (None, _) => true,
            (Some(param), None) => {
                matches!(self.input(), ActionInput::Prompted { .. })
                    && matches!(
                        param.ty,
                        ParamType::Text
                            | ParamType::Integer
                            | ParamType::Number
                            | ParamType::DateTime
                            | ParamType::Secret
                    )
            }
            (Some(_), Some(_)) => false,
        }
    }

    /// Declare that this action carries the resource's other actions (a
    /// generic typed edit, a group of edits): history records the actions
    /// it carried, never this one. The contract advertises
    /// [`CARRIER_TAG`](crate::CARRIER_TAG), so whoever decides for it or words
    /// it for a person reads what it carries. Call after
    /// [`with_contract`](Self::with_contract), which replaces the tags.
    #[must_use]
    pub fn carrying(mut self) -> Self {
        self.type_mut().carrier = true;
        push_tag(&mut self.type_mut().contract, crate::contract::CARRIER_TAG);
        self
    }

    /// Whether this action carries the resource's other actions. See
    /// [`Self::carrying`].
    pub fn carries_actions(&self) -> bool {
        self.carrier
    }

    /// Classify each invocation's risk from its bound arguments, for an action
    /// whose consequence depends on them (a generic edit whose value may
    /// delete). Whenever the classified risk
    /// [requires confirmation](RiskLevel::requires_confirmation), the host
    /// asks for exactly the confirmation a declared
    /// [`destructive`](Self::destructive) action gets; otherwise the action
    /// runs directly. The contract advertises
    /// [`ARGUMENT_RISK_TAG`](crate::ARGUMENT_RISK_TAG) so callers that cannot
    /// classify know to expect it.
    #[must_use]
    pub fn with_risk(
        self,
        classify: impl Fn(&ActionArgs) -> RiskLevel + Send + Sync + 'static,
    ) -> Self {
        self.with_risk_on(move |_, args| classify(args))
    }

    /// [`with_risk`](Self::with_risk) for a type shared by many targets: the
    /// classifier is given the target of the spec being invoked.
    #[must_use]
    pub fn with_risk_on(
        mut self,
        classify: impl Fn(&Target, &ActionArgs) -> RiskLevel + Send + Sync + 'static,
    ) -> Self {
        push_tag(
            &mut self.type_mut().contract,
            crate::contract::ARGUMENT_RISK_TAG,
        );
        self.type_mut().risk_of = Some(Arc::new(classify));
        self
    }

    /// Classify what each invocation is from its bound arguments, for an
    /// action whose consequence depends on them (a generic edit: a reply
    /// writes only the comments, a deletion changes the document). The
    /// declared [`ActionContract::kind`] and [`ActionContract::field`] stay
    /// the least any invocation is, which is what a host offers the action
    /// under; every check that admits an invocation asks
    /// [`need_for`](Self::need_for) too.
    #[must_use]
    pub fn with_need_of(
        mut self,
        classify: impl Fn(&ActionArgs) -> ActionNeed + Send + Sync + 'static,
    ) -> Self {
        self.type_mut().need_of = Some(Arc::new(move |_, args| classify(args)));
        self
    }

    /// What one invocation with these bound arguments is: the classifier's
    /// verdict, else the declared contract's.
    pub fn need_for_args(&self, args: &ActionArgs) -> ActionNeed {
        let mut need = self.need_of.as_ref().map_or_else(
            || self.contract.need(),
            |classify| self.contract.scoped(classify(&self.target, args)),
        );
        need.privileged |= self.risk_for(args) == RiskLevel::Privileged;
        need
    }

    /// What one invocation is, by the value it carries: `json`, or the
    /// palette's and legacy text API's `argument` (typed JSON behind
    /// [`ARGUMENTS_JSON_PREFIX`](crate::ARGUMENTS_JSON_PREFIX), one line of
    /// text for the primary parameter, or nothing). Arguments that do not
    /// bind build no action, so they are what the declaration says.
    #[must_use]
    pub fn need_for(
        &self,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> ActionNeed {
        // An invocation whose risk depends on its arguments may be
        // privileged ([`Self::need_for_args`]) even with no classifier.
        if self.need_of.is_none() && self.risk_of.is_none() {
            return self.contract.need();
        }
        if let Some(json) = json {
            return self
                .without_bound(json)
                .and_then(|json| ActionArgs::bind_json(&self.params, &json))
                .map_or_else(|_| self.contract.need(), |args| self.need_for_args(&args));
        }
        if let Some(json) =
            argument.and_then(|text| text.strip_prefix(crate::ARGUMENTS_JSON_PREFIX))
        {
            return serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .map_or_else(
                    |_| self.contract.need(),
                    |arguments| self.need_for(Some(&arguments), None),
                );
        }
        let supplied = argument.map_or_else(Vec::new, |text| primary_argument(&self.params, text));
        ActionArgs::bind(&self.params, supplied)
            .map_or_else(|_| self.contract.need(), |args| self.need_for_args(&args))
    }

    /// What a person's own press of one invocation waits for, by the value it carries (`json`, or
    /// the palette's `argument`, as [`Self::need_for`] reads them): the contract's
    /// [`confirms`](ActionContract::confirms) of that invocation's risk and of whether Undo takes
    /// it back. Arguments that do not bind are what the declaration says.
    #[must_use]
    pub fn confirms_for(
        &self,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> crate::Confirms {
        if self.risk_of.is_none() && self.undo_of.is_none() {
            return self.contract.confirms();
        }
        let bound = match (json, argument) {
            (Some(json), _) => self
                .without_bound(json)
                .and_then(|json| ActionArgs::bind_json(&self.params, &json))
                .ok(),
            (None, Some(text)) => match text.strip_prefix(crate::ARGUMENTS_JSON_PREFIX) {
                Some(json) => {
                    return serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                        json,
                    )
                    .map_or_else(
                        |_| self.contract.confirms(),
                        |arguments| self.confirms_for(Some(&arguments), None),
                    );
                }
                None => ActionArgs::bind(&self.params, primary_argument(&self.params, text)).ok(),
            },
            (None, None) => ActionArgs::bind(&self.params, Vec::new()).ok(),
        };
        bound.map_or_else(
            || self.contract.confirms(),
            |args| {
                self.contract
                    .confirms_as(self.risk_for(&args), self.undoable_for(&args))
            },
        )
    }

    /// Decide per invocation whether Undo takes it back, for an action that
    /// carries others (a group: Undo takes it back only when it takes back
    /// every member). Without it, the contract's
    /// [`can_be_undone`](ActionContract::can_be_undone) decides.
    #[must_use]
    pub fn with_undo_of(
        mut self,
        classify: impl Fn(&ActionArgs) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.type_mut().undo_of = Some(Arc::new(classify));
        self
    }

    /// Whether Undo takes back one invocation with these bound arguments.
    pub fn undoable_for(&self, args: &ActionArgs) -> bool {
        self.undo_of
            .as_ref()
            .map_or_else(|| self.contract.can_be_undone(), |classify| classify(args))
    }

    /// The risk of one invocation: the classifier's verdict for these
    /// arguments, or the declared [`ActionContract::risk`].
    pub fn risk_for(&self, args: &ActionArgs) -> RiskLevel {
        self.risk_of
            .as_ref()
            .map_or(self.contract.risk, |classify| classify(&self.target, args))
    }

    /// The confirmation one invocation with these bound arguments needs: the
    /// declared policy when there is one, else
    /// [`Confirm`](ConfirmationPolicy::Confirm) when
    /// [`risk_for`](Self::risk_for) requires confirmation. Every check that
    /// admits an invocation asks this, never the declared policy alone.
    pub fn confirmation_for_args(&self, args: &ActionArgs) -> ConfirmationPolicy {
        if self.contract.confirmation != ConfirmationPolicy::None || self.risk_of.is_none() {
            return self.contract.confirmation.clone();
        }
        if self
            .risk_for(args)
            .requires_confirmation_when(self.undoable_for(args))
        {
            ConfirmationPolicy::Confirm
        } else {
            ConfirmationPolicy::None
        }
    }

    /// [`confirmation_for_args`](Self::confirmation_for_args) for arguments
    /// that arrived as JSON. Arguments that do not bind cannot build an
    /// action at all, so they get the declared policy.
    pub fn confirmation_for_json(
        &self,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> ConfirmationPolicy {
        if self.risk_of.is_none() {
            return self.contract.confirmation.clone();
        }
        self.without_bound(supplied)
            .and_then(|supplied| ActionArgs::bind_json(&self.params, &supplied))
            .map_or_else(
                |_| self.contract.confirmation.clone(),
                |args| self.confirmation_for_args(&args),
            )
    }

    /// [`confirmation_for_args`](Self::confirmation_for_args) for the palette
    /// and legacy text API: typed JSON behind
    /// [`ARGUMENTS_JSON_PREFIX`](crate::ARGUMENTS_JSON_PREFIX), one line of
    /// text for the primary parameter, or nothing.
    pub fn confirmation_for_argument(&self, argument: Option<&str>) -> ConfirmationPolicy {
        if self.risk_of.is_none() {
            return self.contract.confirmation.clone();
        }
        if let Some(json) =
            argument.and_then(|text| text.strip_prefix(crate::ARGUMENTS_JSON_PREFIX))
        {
            return serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .map_or_else(
                    |_| self.contract.confirmation.clone(),
                    |arguments| self.confirmation_for_json(&arguments),
                );
        }
        let supplied = argument.map_or_else(Vec::new, |text| primary_argument(&self.params, text));
        ActionArgs::bind(&self.params, supplied).map_or_else(
            |_| self.contract.confirmation.clone(),
            |args| self.confirmation_for_args(&args),
        )
    }

    #[must_use]
    pub fn privileged(mut self) -> Self {
        self.type_mut().contract.risk = RiskLevel::Privileged;
        self.type_mut().contract.confirmation = ConfirmationPolicy::Confirm;
        self
    }

    /// Set what the action is able to do, as the declaration sees it. See
    /// [`ActionContract::kind`].
    #[must_use]
    pub fn of_kind(mut self, kind: ActionKind) -> Self {
        self.type_mut().contract.kind = kind;
        self
    }

    /// Set the field the action writes, and its kind unless the declaration
    /// named one ([`of_kind`](Self::of_kind)), which a need does not overrule.
    #[must_use]
    pub fn with_need(mut self, need: ActionNeed) -> Self {
        self.type_mut().contract.kind = need.kind;
        self.type_mut().contract.field = need.field;
        self
    }

    /// Place the action below the one field it writes. See
    /// [`ActionContract::field`].
    #[must_use]
    pub fn below_field(mut self, field: impl Into<String>) -> Self {
        self.type_mut().contract.field = Some(field.into());
        self
    }

    #[must_use]
    pub fn execution(mut self, execution: ExecutionMode) -> Self {
        self.type_mut().contract.execution = execution;
        self
    }

    #[must_use]
    pub fn returns(mut self, result: ResultSpec) -> Self {
        self.type_mut().contract.result = result;
        self
    }

    /// This document action as another resource carries it: every action it
    /// builds passes through `wrap`, which re-encodes it as the action of the
    /// resource that contains this one (a rich-text edit becomes the note's
    /// edit of its body, registered under `wrapped_type`). Parameters,
    /// kind, field, risk and confirmation stay this action's. A builder's
    /// refusal passes through unchanged. An effect or view operation is
    /// returned as it is: it writes nothing to wrap.
    #[must_use]
    pub fn rewrapped(
        mut self,
        wrapped_type: &str,
        wrap: impl Fn(EncodedAction) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        if self.kind != OperationKind::Document {
            return self;
        }
        let wrap: Arc<dyn Fn(EncodedAction) -> EncodedAction + Send + Sync> =
            Arc::new(move |encoded: EncodedAction| {
                if encoded.type_id == INVALID_ARGUMENTS_TYPE {
                    encoded
                } else {
                    wrap(encoded)
                }
            });
        let ty = self.type_mut();
        ty.build = match ty.build.clone() {
            InvocationBuilder::Fixed(encoded) => {
                InvocationBuilder::Fixed(Arc::new(wrap(encoded.as_ref().clone())))
            }
            InvocationBuilder::Dynamic {
                build,
                deferred_type,
            } => {
                let wrap = Arc::clone(&wrap);
                InvocationBuilder::Dynamic {
                    build: Arc::new(move |target, args| wrap(build(target, args))),
                    deferred_type: deferred_type.map(|_| wrapped_type.to_owned()),
                }
            }
            InvocationBuilder::Text(build) => {
                let wrap = Arc::clone(&wrap);
                InvocationBuilder::Text(Arc::new(move |target, text| wrap(build(target, text))))
            }
        };
        match &mut ty.input {
            InputMode::Immediate(Some(encoded)) => {
                *encoded = Arc::new(wrap(encoded.as_ref().clone()));
            }
            InputMode::Deferred { type_id } => wrapped_type.clone_into(type_id),
            InputMode::Immediate(None) | InputMode::Prompted { .. } => {}
        }
        self
    }

    /// This document action, carried out on another document than the one
    /// its resource's host holds (the person's own record, a settings
    /// section): published as an effect operation of `operation_type` whose
    /// value is `wrap` of the encoded action, which the host answering
    /// `operation_type` applies to that document. Parameters, kind,
    /// permission, risk and confirmation stay this action's, and it is
    /// tagged [`crate::ROUTED_CHANGE_TAG`]: a change of a document still. An
    /// effect or view operation is returned as it is.
    #[must_use]
    pub fn routed(
        self,
        operation_type: &str,
        wrap: impl Fn(EncodedAction) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        if self.kind != OperationKind::Document {
            return self;
        }
        let mut spec = self.rewrapped(operation_type, wrap);
        let ty = spec.type_mut();
        ty.kind = OperationKind::Effect;
        ty.operation_type = Some(operation_type.to_owned());
        push_tag(&mut ty.contract, crate::ROUTED_CHANGE_TAG);
        spec
    }

    /// What an invocation of this action must carry. Empty for the nullary
    /// majority.
    ///
    /// Part of the descriptor rather than something a caller discovers by
    /// trying: enumeration is all a caller that cannot see the screen has to
    /// work from, so an action whose parameters were not advertised would be
    /// un-invokable except by guesswork.
    pub fn params(&self) -> &[ActionParam] {
        &self.params
    }

    /// Narrow the parameter `name` this action declares for the entity it is
    /// offered on: the values it takes there now, what it means there, the
    /// bounds its state sets. A parameter the action does not declare is left
    /// alone, so narrowing a spec an entity does not have the argument for is
    /// no change rather than a panic.
    pub fn narrow_param(&mut self, name: &str, narrow: impl FnOnce(&mut ActionParam)) {
        if let Some(param) = Arc::make_mut(&mut self.ty)
            .params
            .iter_mut()
            .find(|param| param.name == name)
        {
            narrow(param);
        }
    }

    /// Bind `supplied` against [`params`](Self::params) and build the action.
    ///
    /// Strict: a missing, unknown, wrong-typed or out-of-choice argument is
    /// refused here, and the caller never receives an [`EncodedAction`] to
    /// dispatch — so a bad call cannot become a partial mutation, because
    /// nothing was ever handed to the executor.
    pub fn build_args(
        &self,
        supplied: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<EncodedAction, ArgError> {
        self.validate_deferred_shape()?;
        let bound = ActionArgs::bind(&self.params, self.pairs_without_bound(supplied)?)?;
        self.validate_built(self.ty.encode(&self.target, &bound))
    }

    /// [`build_args`](Self::build_args) for arguments that arrived as JSON.
    pub fn build_json(
        &self,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EncodedAction, ArgError> {
        self.validate_deferred_shape()?;
        let bound = ActionArgs::bind_json(&self.params, self.without_bound(supplied)?.as_ref())?;
        self.validate_built(self.ty.encode(&self.target, &bound))
    }

    /// The palette input mode's encoding for one typed text argument, exactly as a
    /// prompted affordance builds it. `None` when this action collects no text.
    pub fn build_text(&self, text: &str) -> Option<EncodedAction> {
        if !matches!(self.input, InputMode::Prompted { .. }) {
            return None;
        }
        Some(match &self.build {
            InvocationBuilder::Text(build) => build(&self.target, text),
            InvocationBuilder::Fixed(_) | InvocationBuilder::Dynamic { .. } => {
                match ActionArgs::bind(&self.params, primary_argument(&self.params, text)) {
                    Ok(bound) => self.ty.encode(&self.target, &bound),
                    Err(error) => EncodedAction {
                        type_id: INVALID_ARGUMENTS_TYPE.into(),
                        payload: error.to_string().into_bytes(),
                    },
                }
            }
        })
    }
}

/// Whether `given`, passed for `name`, which an action binds to `value`
/// ([`ActionSpec::with_bound`]), names that value: as it is, or, for an
/// item's id, as the end of a longer form of its address (`sheets/<id>`,
/// `sheets.workbook/sheets/<id>`, `sheet:<id>`).
fn check_bound(
    name: &str,
    value: &serde_json::Value,
    given: &serde_json::Value,
) -> Result<(), ArgError> {
    let same = given == value
        || match (value, given) {
            (serde_json::Value::String(id), serde_json::Value::String(given)) => {
                given.trim() == id
                    || given
                        .rsplit(['/', ':'])
                        .next()
                        .is_some_and(|last| last == id && !id.is_empty())
            }
            _ => false,
        };
    if same {
        return Ok(());
    }
    let shown = match value {
        serde_json::Value::String(id) => id.clone(),
        other => other.to_string(),
    };
    Err(ArgError::Constraint {
        name: name.to_owned(),
        reason: format!(
            "is `{shown}` here: this action acts on that item only. Leave `{name}` out, or \
             invoke the action on the item you mean"
        ),
    })
}

fn enforce_operation_type(encoded: EncodedAction, expected: &str) -> EncodedAction {
    // A refusal carries the builder's own reason to the caller; the API reads
    // it out of the payload. Only an encoding of another operation, which the
    // spec cannot invoke, is a defect worth naming here.
    if encoded.type_id == expected || encoded.type_id == INVALID_ARGUMENTS_TYPE {
        encoded
    } else {
        EncodedAction {
            type_id: INVALID_ARGUMENTS_TYPE.into(),
            payload: format!(
                "operation builder returned `{}` but declared `{expected}`",
                encoded.type_id
            )
            .into_bytes(),
        }
    }
}

fn push_tag(contract: &mut ActionContract, tag: &str) {
    if !contract.tags.iter().any(|existing| existing == tag) {
        contract.tags.push(tag.to_owned());
    }
}

/// Bind one line of text to the primary parameter (the first required one,
/// else the first), as the palette collects it.
fn primary_argument(params: &[ActionParam], text: &str) -> Vec<(String, ArgValue)> {
    primary_param(params).map_or_else(Vec::new, |primary| {
        parse_argument(&primary.name, primary.ty, text)
    })
}

/// Parse the one line the palette can collect into a parameter's declared
/// type. Unparseable text binds nothing rather than coercing to a wrong value;
/// the builder decides what absence means, exactly as it does for an
/// unsupplied optional.
fn parse_argument(name: &str, ty: ParamType, text: &str) -> Vec<(String, ArgValue)> {
    let parsed = match ty {
        ParamType::Text => Some(ArgValue::Text(text.to_string())),
        ParamType::Integer => text.trim().parse::<i128>().ok().map(ArgValue::Integer),
        ParamType::Number => text.trim().parse::<f64>().ok().map(ArgValue::Number),
        ParamType::Boolean => match text.trim() {
            "true" | "yes" | "on" => Some(ArgValue::Boolean(true)),
            "false" | "no" | "off" => Some(ArgValue::Boolean(false)),
            _ => None,
        },
        ParamType::Object => {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text)
                .ok()
                .map(ArgValue::Object)
        }
        ParamType::Array => serde_json::from_str::<Vec<serde_json::Value>>(text)
            .ok()
            .map(ArgValue::Array),
        ParamType::Resource => Some(ArgValue::Resource(text.to_string())),
        ParamType::DateTime => Some(ArgValue::DateTime(text.to_string())),
        ParamType::Secret => Some(ArgValue::Secret(text.to_string())),
    };
    parsed
        .map(|v| vec![(name.to_owned(), v)])
        .unwrap_or_default()
}

/// The parameter one line of palette text binds to: the one that takes an
/// entry, else the first required one, else the first.
fn primary_param(params: &[ActionParam]) -> Option<&ActionParam> {
    params
        .iter()
        .find(|p| p.entry)
        .or_else(|| params.iter().find(|p| p.required))
        .or_else(|| params.first())
}

pub use tilo_action_types::KEY_SEP;

/// One (resource, action) pair produced by [`flatten`], addressable by a
/// composite [`key`](FlatAction::key) the palette uses as its row id.
pub struct FlatAction<'a> {
    /// `"<resource_id>\u{1f}<action_id>"` — the palette row id.
    pub key: String,
    pub resource_id: ResourceId,
    pub resource_kind: String,
    pub resource_label: String,
    pub resource_presentation: Option<ResourcePresentation>,
    pub action: &'a ActionSpec,
    /// How deep the resource sits in its forest: 0 for a root (the app or
    /// the open document itself), more for an item inside it.
    pub depth: usize,
}

impl FlatAction<'_> {
    /// Whether a person searching what can be done (the palette, a window's
    /// search) is offered this action: one they can take now
    /// ([`ActionContract::a_person_can_take_now`]) on a resource listed for
    /// people ([`listed_for_people`]), and not the opening of a search hit,
    /// which the hit's own row stands for ([`OPENS_HIT_TAG`]).
    #[must_use]
    pub fn offered_to_a_person(&self) -> bool {
        self.action.contract.a_person_can_take_now()
            && listed_for_people(&self.resource_kind, &self.resource_label)
            && !self
                .action
                .contract
                .tags
                .iter()
                .any(|tag| tag == crate::OPENS_HIT_TAG)
    }

    /// Whether it is listed as a row of its own: [`Self::offered_to_a_person`]
    /// and asking no more than a row can collect
    /// ([`ActionSpec::takes_a_row_answer`]). What a row cannot collect (a
    /// record, a person, several arguments) is picked where it lives.
    #[must_use]
    pub fn lists_a_row(&self) -> bool {
        self.offered_to_a_person() && self.action.takes_a_row_answer()
    }

    /// The row a person reads for this action ([`listed_row`]).
    #[must_use]
    pub fn row(&self) -> ListedRow {
        listed_row(
            &self.action.label,
            (self.depth > 0).then_some(self.resource_label.as_str()),
        )
    }
}

#[path = "resource/listed_row.rs"]
mod listed_row;
pub use listed_row::{ListedRow, listed_row, listed_row_problems};

mod reading_row;
pub use reading_row::ReadingRow;

/// Depth-first flatten of a resource forest into every exported action,
/// parents before children — what the palette lists.
pub fn flatten(resources: &[ResourceSpec]) -> Vec<FlatAction<'_>> {
    let mut out = Vec::new();
    for r in resources {
        flatten_into(r, 0, &mut out);
    }
    out
}

fn flatten_into<'a>(resource: &'a ResourceSpec, depth: usize, out: &mut Vec<FlatAction<'a>>) {
    for action in &resource.actions {
        out.push(FlatAction {
            key: format!("{}{}{}", resource.id.0, KEY_SEP, action.id),
            resource_id: resource.id.clone(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            resource_presentation: resource.presentation.clone(),
            action,
            depth,
        });
    }
    for child in &resource.children {
        flatten_into(child, depth.saturating_add(1), out);
    }
}

#[path = "resource/control.rs"]
mod control;
pub use control::ControlInvocation;

#[path = "resource/spec.rs"]
mod spec;
pub use spec::ResourceSpec;

#[path = "resource/choices.rs"]
mod choices;

#[path = "resource/carried.rs"]
mod carried;

#[cfg(test)]
#[path = "resource/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "resource/history_tests.rs"]
mod history_tests;

#[cfg(test)]
#[path = "resource/effective_invocation_tests.rs"]
mod effective_invocation_tests;

#[cfg(test)]
#[path = "resource/deferred_tests.rs"]
mod deferred_tests;
