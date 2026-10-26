//! The nested **Resource** tree an app publishes, and the **`ActionSpec`**s
//! each resource exports.
//!
//! A [`Resource`] is something the app currently has *open/presented*
//! (a playing video, an open book); resources **nest** — a video owns
//! its comments, a book its annotations. Each exports [`ActionSpec`]s:
//! the operations Ctrl+K offers for it.
//!
//! The split that matters: [`Resource`]/[`ActionSpec`] is
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
use crate::contract::{
    ActionContract, ActionKind, ActionNeed, Availability, ConfirmationPolicy, ExecutionMode,
    ResultSpec, RetryPolicy, RiskLevel,
};
use crate::door::ActionAddress;
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

/// A UI control's inert request to invoke one advertised operation.
///
/// This deliberately carries an address and typed JSON arguments, not an
/// [`EncodedAction`]. The application host resolves the address against its
/// *fresh* resource tree before opening the action door, so a control cannot
/// retain stale authority when availability or policy changes.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlInvocation {
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

impl ControlInvocation {
    /// An invocation of `action` on `resource`, with its arguments, or `None`
    /// when either half is empty or contains the address separator.
    ///
    /// This checks the format only. The host resolves the address against its
    /// fresh resource tree and revalidates the arguments; constructing a value
    /// grants no authority.
    #[must_use]
    pub fn from_parts(
        resource: impl AsRef<str>,
        action: impl AsRef<str>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        Some(Self {
            address: ActionAddress::checked(resource, action)?,
            arguments,
        })
    }
}

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

/// Type-safe adapter from a control's input value to an exported action.
///
/// Construction requires the real [`Resource`] and [`ActionSpec`], rather
/// than parallel string constants. Emission pre-validates arguments against a
/// clone of that spec; the host nevertheless re-resolves and validates again
/// at dispatch time because this object is convenience, not authority.
#[derive(Clone)]
pub struct ActionBinding<I> {
    address: ActionAddress,
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
        resource: &Resource,
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
        resource: &Resource,
        spec: &ActionSpec,
        arguments: impl Fn(I) -> Result<serde_json::Map<String, serde_json::Value>, String>
        + Send
        + Sync
        + 'static,
    ) -> Result<Self, ActionBindingError> {
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
        if self.spec.deferred_invocation_type().is_some()
            || matches!(self.spec.input(), ActionInput::Deferred { .. })
        {
            self.spec.validate_json(&arguments)?;
        } else {
            self.spec.build_json(&arguments)?;
        }
        Ok(ControlInvocation {
            address: self.address.clone(),
            arguments,
        })
    }

    pub fn address(&self) -> &ActionAddress {
        &self.address
    }
}

impl ActionBinding<()> {
    /// Bind a nullary control to a nullary exported action.
    ///
    /// # Errors
    ///
    /// Returns an error if the action is not exported or requires arguments.
    pub fn unit(resource: &Resource, spec: &ActionSpec) -> Result<Self, ActionBindingError> {
        let binding = Self::new(resource, spec, |()| serde_json::Map::new())?;
        // Refuse a required-argument action at construction, not first click.
        if binding.spec.deferred_invocation_type().is_some()
            || matches!(binding.spec.input(), ActionInput::Deferred { .. })
        {
            binding.spec.validate_json(&serde_json::Map::new())?;
        } else {
            binding.spec.build_json(&serde_json::Map::new())?;
        }
        Ok(binding)
    }

    /// Look up the real exported spec by stable id, then create a nullary
    /// binding. Useful when the resource is assembled inline or generated.
    ///
    /// # Errors
    ///
    /// Returns an error if `action_id` is not exported or requires arguments.
    pub fn unit_for(resource: &Resource, action_id: &str) -> Result<Self, ActionBindingError> {
        let spec = resource
            .actions
            .iter()
            .find(|spec| spec.id == action_id)
            .ok_or_else(|| ActionBindingError::ActionNotExported {
                resource: resource.id.0.clone(),
                action: action_id.to_string(),
            })?;
        Self::unit(resource, spec)
    }

    /// Bind a control to a fixed, strictly validated JSON argument object.
    ///
    /// # Errors
    ///
    /// Returns an error if the action is not exported or `arguments` violate
    /// its parameter contract.
    pub fn static_json(
        resource: &Resource,
        spec: &ActionSpec,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, ActionBindingError> {
        let binding = Self::new(resource, spec, move |()| arguments.clone())?;
        if binding.spec.deferred_invocation_type().is_some()
            || matches!(binding.spec.input(), ActionInput::Deferred { .. })
        {
            binding
                .spec
                .validate_json(&(binding.arguments)(()).map_err(ActionBindingError::Unbindable)?)?;
        } else {
            binding
                .spec
                .build_json(&(binding.arguments)(()).map_err(ActionBindingError::Unbindable)?)?;
        }
        Ok(binding)
    }

    /// Look up the real exported spec by id, then bind fixed JSON arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if `action_id` is not exported or `arguments` violate
    /// its parameter contract.
    pub fn static_json_for(
        resource: &Resource,
        action_id: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, ActionBindingError> {
        let spec = resource
            .actions
            .iter()
            .find(|spec| spec.id == action_id)
            .ok_or_else(|| ActionBindingError::ActionNotExported {
                resource: resource.id.0.clone(),
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

/// Stable identity of a resource within the publishing app instance
/// (e.g. `tube.video:dQw4w9WgXcQ`, `ebook.annotation:<uuid>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(pub String);

impl ResourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
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
        }
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
    pub fn validate(&self, resource: &Resource) -> Vec<String> {
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
    resource: &Resource,
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

/// One open/presented resource and the actions it exports. Resources
/// nest via [`children`](Resource::children).
#[derive(Clone)]
pub struct Resource {
    pub id: ResourceId,
    /// A stable kind tag (e.g. `"tube.video"`, `"ebook.annotation"`),
    /// shared with the global-search type registry for OS-wide enumeration.
    pub kind: String,
    /// Human, localisable label shown in the palette.
    pub label: String,
    /// Optional shell-rendered metadata used by system search and resource
    /// browsers. Invalid or oversized values are omitted at the boundary.
    pub presentation: Option<ResourcePresentation>,
    /// Exact original typed identity retained before host namespacing.
    pub semantic_identity: Option<ResourceSemanticIdentity>,
    pub children: Vec<Resource>,
    pub actions: Vec<ActionSpec>,
    /// Explicit presentations of this resource. Actions remain invocation
    /// mechanisms; only entries in this collection are resource views.
    pub views: Vec<ResourceViewSpec>,
}

impl Resource {
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: ResourceId::new(id),
            kind: kind.into(),
            label: label.into(),
            presentation: None,
            semantic_identity: None,
            children: Vec::new(),
            actions: Vec::new(),
            views: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_presentation(mut self, presentation: ResourcePresentation) -> Self {
        self.presentation = presentation.is_valid().then_some(presentation);
        self
    }

    #[must_use]
    pub fn with_action(mut self, action: ActionSpec) -> Self {
        self.actions.push(action);
        self
    }

    #[must_use]
    pub fn with_view(mut self, view: ResourceViewSpec) -> Self {
        self.views.push(view);
        self
    }

    #[must_use]
    pub fn with_child(mut self, child: Resource) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub fn with_children(mut self, children: impl IntoIterator<Item = Resource>) -> Self {
        self.children.extend(children);
        self
    }

    /// The address of the action `action` this resource publishes, or `None`
    /// when it publishes none by that id. A caller holding a published
    /// resource (a survey's, a search hit's, a bound tree's) addresses its
    /// actions from it rather than writing an address of its own, so an
    /// action the resource stopped publishing fails here, at the caller.
    #[must_use]
    pub fn action_address(&self, action: &str) -> Option<ActionAddress> {
        self.actions
            .iter()
            .any(|spec| spec.id == action)
            .then(|| ActionAddress::new(self.id.as_str(), action))
    }
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
enum Lane {
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
    ///
    /// # Errors
    /// Propagates a sink failure or an unrepresentable component length.
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
    /// A prompted action's builder of its one text, which the typed lane
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
    pub label: String,
    pub description: Option<String>,
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
    lane: Lane,
    /// The typed argument surface an external caller invokes against. Private
    /// because it must stay in step with `build` — the two are always set
    /// together by a constructor, never independently.
    params: Vec<ActionParam>,
    /// Builds the encoded action from arguments bound against `params`. The
    /// palette's lane uses the same closure, which is what makes the palette
    /// path and the external path provably one function.
    build: InvocationBuilder,
    /// Classifies one invocation's bound arguments when the action's risk
    /// depends on them. See [`ActionSpec::with_risk`].
    risk_of: Option<RiskClassifier>,
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
}

impl ActionType {
    /// This type bound to `target`: the action one item offers.
    #[must_use]
    pub fn bind(self: &Arc<Self>, target: Target) -> ActionSpec {
        ActionSpec {
            ty: Arc::clone(self),
            target,
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

/// What a constructor declares of a new action type before its lane and
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
        match &self.lane {
            Lane::Immediate(encoded) => ActionInput::Immediate(encoded.as_ref()),
            Lane::Deferred { type_id } => ActionInput::Deferred { type_id },
            Lane::Prompted {
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
        match &self.lane {
            Lane::Immediate(Some(encoded)) => Some(Arc::clone(encoded)),
            Lane::Immediate(None) => {
                Some(Arc::new(self.ty.encode(&self.target, &ActionArgs::none())))
            }
            Lane::Deferred { .. } | Lane::Prompted { .. } => None,
        }
    }

    /// Run a [`Deferred`](ActionInput::Deferred) spec's builder, unchecked:
    /// `None` for any other. Callers check the result's type against the
    /// declared one, as the door does.
    #[must_use]
    pub fn build_deferred(&self) -> Option<EncodedAction> {
        match &self.lane {
            Lane::Deferred { .. } => Some(self.ty.encode(&self.target, &ActionArgs::none())),
            Lane::Immediate(_) | Lane::Prompted { .. } => None,
        }
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
        match (&self.lane, self.deferred_invocation_type()) {
            (Lane::Deferred { type_id }, Some(expected))
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
            (Lane::Deferred { .. }, _) | (_, Some(_)) => Err(ArgError::Constraint {
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
    ///
    /// # Errors
    ///
    /// Returns an [`ArgError`] when the deferred declaration is malformed or
    /// `supplied` does not bind to the declared parameters.
    pub fn validate_json(
        &self,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ArgError> {
        self.validate_deferred_shape()?;
        ActionArgs::bind_json(&self.params, supplied).map(|_| ())
    }

    /// The spec of a newly declared type, bound to no target.
    fn declared(seed: Declared, lane: Lane, build: InvocationBuilder) -> Self {
        Self::of(ActionType {
            id: seed.id,
            label: seed.label,
            description: None,
            locus: seed.locus,
            kind: seed.kind,
            operation_type: seed.operation_type,
            contract: ActionContract::for_operation(seed.kind),
            lane,
            params: seed.params,
            build,
            risk_of: None,
            need_of: None,
            continuation_resolver: None,
            carrier: false,
            applied_as: None,
        })
    }

    /// A document operation whose no-argument builder runs only at execution.
    /// Both invocation lanes share this one closure; construction never runs it.
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
        Self::declared(
            seed,
            Lane::Deferred {
                type_id: type_id.clone(),
            },
            InvocationBuilder::Dynamic {
                build: Arc::new(move |target, _| build(target)),
                deferred_type: Some(type_id),
            },
        )
    }

    /// An action that fires immediately with `encoded`, declared as `seed`.
    fn fixed(seed: Declared, encoded: EncodedAction) -> Self {
        let encoded = Arc::new(encoded);
        Self::declared(
            seed,
            Lane::Immediate(Some(Arc::clone(&encoded))),
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
            encoded,
        ))
    }

    /// An action that fires immediately, from an [`EncodedAction`] the caller
    /// already holds.
    ///
    /// [`immediate`](Self::immediate) is the usual door — it takes a typed
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
            Lane::Prompted {
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
        match self.lane {
            Lane::Prompted { .. } => primary_param(self.params()).map(|param| param.name.as_ref()),
            _ => None,
        }
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
        Ok(Self::fixed(seed, encoded))
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
            Lane::Prompted {
                prompt,
                placeholder: placeholder.into(),
                collects: Collects::Words,
            },
            InvocationBuilder::Text(Arc::new(move |_, value| {
                enforce_operation_type(build(value), &expected)
            })),
        )
    }

    /// An action with a real typed argument surface.
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
    pub fn typed_operation_on(
        id: impl Into<String>,
        label: impl Into<String>,
        placeholder: impl Into<String>,
        kind: OperationKind,
        type_id: impl Into<String>,
        params: Vec<ActionParam>,
        build: impl Fn(&Target, &ActionArgs) -> EncodedAction + Send + Sync + 'static,
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
            Arc::new(build),
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
        let lane = match primary_param(&seed.params) {
            None => Lane::Immediate(
                eager.then(|| Arc::new(build(&Target::none(), &ActionArgs::none()))),
            ),
            Some(primary) => Lane::Prompted {
                prompt: primary.description.to_string(),
                placeholder: seed.placeholder,
                collects: Collects::Words,
            },
        };
        Self::declared(
            Declared {
                id: seed.id,
                label: seed.label,
                locus: seed.locus,
                kind: seed.kind,
                operation_type: seed.operation_type,
                params: seed.params,
            },
            lane,
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
        self.type_mut().lane = Lane::Immediate(Some(Arc::new(encoded)));
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
        if let Lane::Prompted { collects, .. } = &mut spec.type_mut().lane {
            *collects = Collects::Content;
        }
        spec
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.type_mut().description = Some(description.into());
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

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.type_mut().contract.risk = RiskLevel::Destructive;
        self.type_mut().contract.confirmation = ConfirmationPolicy::Confirm;
        self
    }

    /// Offer this action only to programmatic callers — agents, scripts, and
    /// the controls or surfaces that submit it — never in a listing a person
    /// picks from (the action palette, menus). It stays published, resolvable
    /// and invocable by address, under the same permission, confirmation and
    /// history rules as every other action. Recorded as
    /// [`PROGRAMMATIC_TAG`](crate::PROGRAMMATIC_TAG) so the property crosses
    /// every boundary the inert contract does.
    #[must_use]
    pub fn programmatic(mut self) -> Self {
        push_tag(
            &mut self.type_mut().contract,
            crate::contract::PROGRAMMATIC_TAG,
        );
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

    /// Whether this action is offered only to programmatic callers. See
    /// [`programmatic`](Self::programmatic).
    pub fn is_programmatic(&self) -> bool {
        self.contract.is_programmatic()
    }

    /// Declare that this action carries the resource's other actions (a
    /// generic typed edit, a group of edits): history records the actions
    /// it carried, never this one.
    #[must_use]
    pub fn carrying(mut self) -> Self {
        self.type_mut().carrier = true;
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
    /// under; every gate that admits an invocation asks
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
            |classify| classify(&self.target, args),
        );
        need.privileged |= self.risk_for(args) == RiskLevel::Privileged;
        need
    }

    /// What one invocation is, by the value it carries: `json`, or the
    /// palette's and legacy text door's `argument` (typed JSON behind
    /// [`ARGUMENTS_JSON_PREFIX`](crate::ARGUMENTS_JSON_PREFIX), one line of
    /// text for the primary parameter, or nothing). Arguments that do not
    /// bind build no action, so they are what the declaration says.
    #[must_use]
    pub fn need_for(
        &self,
        json: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> ActionNeed {
        if self.need_of.is_none() {
            return self.contract.need();
        }
        if let Some(json) = json {
            return ActionArgs::bind_json(&self.params, json)
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
    /// [`risk_for`](Self::risk_for) requires confirmation. Every gate that
    /// admits an invocation asks this, never the declared policy alone.
    pub fn confirmation_for_args(&self, args: &ActionArgs) -> ConfirmationPolicy {
        if self.contract.confirmation != ConfirmationPolicy::None || self.risk_of.is_none() {
            return self.contract.confirmation.clone();
        }
        if self.risk_for(args).requires_confirmation() {
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
        ActionArgs::bind_json(&self.params, supplied).map_or_else(
            |_| self.contract.confirmation.clone(),
            |args| self.confirmation_for_args(&args),
        )
    }

    /// [`confirmation_for_args`](Self::confirmation_for_args) for the palette
    /// and legacy text door: typed JSON behind
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

    /// Set what the action is able to do. See [`ActionContract::kind`].
    #[must_use]
    pub fn of_kind(mut self, kind: ActionKind) -> Self {
        self.type_mut().contract.kind = kind;
        self
    }

    /// Set what the action is: its kind and the field it writes.
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
    pub fn reversible(mut self) -> Self {
        self.type_mut().contract.reversible = true;
        self
    }

    #[must_use]
    pub fn retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.type_mut().contract.retry = retry;
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
    /// permission, risk and confirmation stay this action's. A builder's
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
        match &mut ty.lane {
            Lane::Immediate(Some(encoded)) => {
                *encoded = Arc::new(wrap(encoded.as_ref().clone()));
            }
            Lane::Deferred { type_id } => wrapped_type.clone_into(type_id),
            Lane::Immediate(None) | Lane::Prompted { .. } => {}
        }
        self
    }

    /// This document action, carried out on another document than the one
    /// its resource's host holds (the person's own record, a settings
    /// section): published as an effect operation of `operation_type` whose
    /// value is `wrap` of the encoded action, which the host answering
    /// `operation_type` applies to that document. Parameters, kind,
    /// permission, risk and confirmation stay this action's. An effect or
    /// view operation is returned as it is.
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

    /// Bind `supplied` against [`params`](Self::params) and build the action.
    ///
    /// Strict: a missing, unknown, wrong-typed or out-of-choice argument is
    /// refused here, and the caller never receives an [`EncodedAction`] to
    /// dispatch — so a bad call cannot become a partial mutation, because
    /// nothing was ever handed to the executor.
    ///
    /// # Errors
    ///
    /// Returns [`ArgError`] when the supplied arguments do not satisfy this
    /// action's declared parameters.
    pub fn build_args(
        &self,
        supplied: impl IntoIterator<Item = (String, ArgValue)>,
    ) -> Result<EncodedAction, ArgError> {
        self.validate_deferred_shape()?;
        let bound = ActionArgs::bind(&self.params, supplied)?;
        self.validate_built(self.ty.encode(&self.target, &bound))
    }

    /// [`build_args`](Self::build_args) for arguments that arrived as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ArgError`] when a value is unrepresentable or the supplied
    /// arguments do not satisfy this action's declared parameters.
    pub fn build_json(
        &self,
        supplied: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EncodedAction, ArgError> {
        self.validate_deferred_shape()?;
        let bound = ActionArgs::bind_json(&self.params, supplied)?;
        self.validate_built(self.ty.encode(&self.target, &bound))
    }

    /// The palette lane's encoding for one typed text argument, exactly as a
    /// prompted affordance builds it. `None` when this action collects no text.
    pub fn build_text(&self, text: &str) -> Option<EncodedAction> {
        if !matches!(self.lane, Lane::Prompted { .. }) {
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

fn enforce_operation_type(encoded: EncodedAction, expected: &str) -> EncodedAction {
    if encoded.type_id == expected {
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
        ParamType::Integer => text.trim().parse::<i64>().ok().map(ArgValue::Integer),
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

/// The parameter one line of palette text binds to: the first required one,
/// else the first.
fn primary_param(params: &[ActionParam]) -> Option<&ActionParam> {
    params
        .iter()
        .find(|p| p.required)
        .or_else(|| params.first())
}

/// Field separator inside a flattened composite key — a unit-separator
/// control char never appears in a resource or action id.
pub const KEY_SEP: char = '\u{1f}';

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
}

/// Depth-first flatten of a resource forest into every exported action,
/// parents before children — what the palette lists.
pub fn flatten(resources: &[Resource]) -> Vec<FlatAction<'_>> {
    let mut out = Vec::new();
    for r in resources {
        flatten_into(r, &mut out);
    }
    out
}

fn flatten_into<'a>(resource: &'a Resource, out: &mut Vec<FlatAction<'a>>) {
    for action in &resource.actions {
        out.push(FlatAction {
            key: format!("{}{}{}", resource.id.0, KEY_SEP, action.id),
            resource_id: resource.id.clone(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            resource_presentation: resource.presentation.clone(),
            action,
        });
    }
    for child in &resource.children {
        flatten_into(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    struct OpenPath {
        path: String,
    }

    /// A non-document operation needs no fake action implementation, an operation builder cannot
    /// switch its declared protocol type, and presentation is bounded before publication.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn resource_operations_keep_their_declared_shape_and_bounds() {
        non_document_operation_needs_no_fake_action_implementation();
        operation_builder_cannot_switch_its_declared_protocol_type();
        resource_presentation_is_bounded_before_publication();
    }

    fn non_document_operation_needs_no_fake_action_implementation() {
        let spec = ActionSpec::operation(
            "open",
            "Open",
            OperationKind::Effect,
            "files.open",
            &OpenPath {
                path: "/tmp/report".into(),
            },
        )
        .expect("effect payload encodes");
        assert_eq!(spec.kind, OperationKind::Effect);
        assert!(spec.params().is_empty());
    }

    /// Explicit view builders validate open and mount bindings, implicit or unsafe bindings are
    /// rejected, a typed view binds the schema it advertises, and a binding looks up the real spec and
    /// validates static arguments.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn resource_views_bind_exactly_the_schema_they_advertise() {
        explicit_resource_view_builders_validate_open_and_mount_bindings();
        resource_view_validation_rejects_implicit_or_unsafe_bindings();
        typed_view_binds_the_same_schema_it_advertises();
        binding_looks_up_real_spec_and_validates_static_arguments();
    }

    fn explicit_resource_view_builders_validate_open_and_mount_bindings() {
        let open = ActionSpec::typed_operation(
            "open",
            "Open",
            "",
            OperationKind::View,
            "notes.open",
            vec![
                ActionParam::optional("slot_capability", ParamType::Text, "Capability"),
                ActionParam::optional("slot_view", ParamType::Text, "Qualified view"),
            ],
            |_| EncodedAction::encode_operation("notes.open", &()).unwrap(),
        );
        let view = ResourceViewSpec::new("editor", "Editor", "open")
            .with_description("Edit this note")
            .with_primary(true)
            .with_mount(ResourceViewMountSpec::new(
                "open",
                "slot_capability",
                "slot_view",
            ));
        let resource = Resource::new("note:1", "notes.note", "Note")
            .with_action(open)
            .with_view(view.clone());

        assert!(view.validate(&resource).is_empty());
        assert_eq!(resource.views, vec![view]);
    }

    fn resource_view_validation_rejects_implicit_or_unsafe_bindings() {
        let effect = ActionSpec::typed_operation(
            "open",
            "Open",
            "value",
            OperationKind::Effect,
            "notes.open",
            vec![ActionParam::required("value", ParamType::Text, "Value")],
            |_| EncodedAction::encode_operation("notes.open", &()).unwrap(),
        )
        .with_contract(ActionContract {
            confirmation: ConfirmationPolicy::Confirm,
            ..ActionContract::default()
        });
        let resource = Resource::new("note:1", "notes.note", "Note").with_action(effect);
        let view = ResourceViewSpec::new("editor", "Editor", "open")
            .with_mount(ResourceViewMountSpec::new("open", "missing", "missing"));
        let errors = view.validate(&resource).join("\n");

        assert!(errors.contains("must be a view operation"));
        assert!(errors.contains("must not require confirmation"));
        assert!(errors.contains("must not require caller parameters"));
        assert!(errors.contains("must be distinct"));
        assert!(errors.contains("must name an optional text parameter"));
    }

    fn typed_view_binds_the_same_schema_it_advertises() {
        let spec = ActionSpec::typed_operation(
            "zoom",
            "Zoom",
            "factor",
            OperationKind::View,
            "view.zoom",
            vec![ActionParam::required_owned(
                "factor",
                ParamType::Number,
                "Zoom factor",
            )],
            |args| {
                EncodedAction::encode_operation("view.zoom", &args.number("factor"))
                    .expect("bound number encodes")
            },
        );
        let supplied = serde_json::json!({"factor": 1.25});
        let encoded = spec
            .build_json(supplied.as_object().expect("object"))
            .expect("declared number binds");
        assert_eq!(spec.kind, OperationKind::View);
        assert_eq!(encoded.type_id, "view.zoom");
        assert!(
            spec.build_json(
                serde_json::json!({"factor": "large"})
                    .as_object()
                    .expect("object")
            )
            .is_err()
        );
    }

    fn operation_builder_cannot_switch_its_declared_protocol_type() {
        let spec = ActionSpec::typed_operation(
            "run",
            "Run",
            "value",
            OperationKind::Effect,
            "safe.operation",
            vec![ActionParam::required("value", ParamType::Text, "Value")],
            |_| EncodedAction::encode_operation("other.operation", &()).unwrap(),
        );
        let encoded = spec
            .build_json(serde_json::json!({"value":"x"}).as_object().unwrap())
            .unwrap();
        assert_eq!(encoded.type_id, INVALID_ARGUMENTS_TYPE);
    }

    fn binding_looks_up_real_spec_and_validates_static_arguments() {
        let resource =
            Resource::new("view:1", "view", "View").with_action(ActionSpec::typed_operation(
                "zoom",
                "Zoom",
                "factor",
                OperationKind::View,
                "view.zoom",
                vec![ActionParam::required("factor", ParamType::Number, "Factor")],
                |args| {
                    EncodedAction::encode_operation("view.zoom", &args.number("factor")).unwrap()
                },
            ));
        assert!(ActionBinding::unit_for(&resource, "zoom").is_err());
        let binding = ActionBinding::static_json_for(
            &resource,
            "zoom",
            serde_json::json!({"factor": 1.5})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        let invocation = binding.invoke(()).unwrap();
        assert_eq!(invocation.address, ActionAddress::new("view:1", "zoom"));
        assert_eq!(invocation.arguments["factor"], 1.5);
        assert!(ActionBinding::unit_for(&resource, "missing").is_err());
    }

    fn resource_presentation_is_bounded_before_publication() {
        let valid = ResourcePresentation {
            detail: Some("19 °C".into()),
            badge: Some("LIVE".into()),
            preview: Some("Sunny with a light breeze".into()),
        };
        assert!(valid.is_valid());
        assert!(
            Resource::new("weather:home", "weather", "Home")
                .with_presentation(valid)
                .presentation
                .is_some()
        );

        let oversized = ResourcePresentation {
            detail: Some("x".repeat(ResourcePresentation::MAX_DETAIL_BYTES + 1)),
            badge: None,
            preview: None,
        };
        assert!(!oversized.is_valid());
        assert!(
            Resource::new("weather:home", "weather", "Home")
                .with_presentation(oversized)
                .presentation
                .is_none()
        );
        assert!(
            !ResourcePresentation {
                detail: None,
                badge: Some("unsafe\nsecond line".into()),
                preview: None,
            }
            .is_valid()
        );
        for preview in [
            "x".repeat(ResourcePresentation::MAX_PREVIEW_BYTES + 1),
            "unsafe\rpreview".into(),
            "unsafe\0preview".into(),
        ] {
            assert!(
                !ResourcePresentation {
                    detail: None,
                    badge: None,
                    preview: Some(preview),
                }
                .is_valid()
            );
        }
        assert!(
            ResourcePresentation {
                detail: None,
                badge: None,
                preview: Some("x".repeat(ResourcePresentation::MAX_PREVIEW_BYTES)),
            }
            .is_valid()
        );
    }

    #[test]
    fn continuation_identity_is_explicit_argument_aware_and_variant_checked() {
        let plain =
            ActionSpec::operation("run", "Run", OperationKind::Effect, "test.run", &()).unwrap();
        assert!(
            plain
                .continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
                .is_none()
        );
        let immediate = ActionSpec::immediate_encoded(
            "open",
            "Open",
            ExecutionLocus::ClientLocal,
            EncodedAction::encode_operation("test.open", &()).unwrap(),
        );
        let prompted = ActionSpec::prompted_param(
            "rename",
            "Rename",
            ActionParam::required("value", ParamType::Text, "Name"),
            "name",
            ExecutionLocus::ClientLocal,
            |value| EncodedAction::encode_operation("test.rename", &value).unwrap(),
        );
        let typed = ActionSpec::typed(
            "count",
            "Count",
            "count",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required("count", ParamType::Integer, "Count")],
            |args| EncodedAction::encode_operation("test.count", &args.integer("count")).unwrap(),
        );
        for spec in [&immediate, &prompted, &typed] {
            assert!(
                spec.continuation_identity(ContinuationQuery::Draft)
                    .is_none(),
                "every constructor is unsupported unless its provider opts in"
            );
        }

        let spec = plain.with_continuation_identity(|query| match query {
            ContinuationQuery::Exact(ContinuationArguments::Text(value)) => {
                Some(ContinuationEvidence::Exact {
                    target: ContinuationKey::new("target", [value.as_bytes()]),
                    handler: ContinuationKey::new("handler", [b"v1".as_slice()]),
                })
            }
            ContinuationQuery::Draft => Some(ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new("domain", [b"aliases-v4".as_slice()]),
                handler: ContinuationKey::new("handler", [b"v1".as_slice()]),
            }),
            ContinuationQuery::Exact(_) => None,
        });
        assert!(matches!(
            spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Text("a"))),
            Some(ContinuationEvidence::Exact { target, .. })
                if target == ContinuationKey::new("target", [b"a".as_slice()])
        ));
        assert!(matches!(
            spec.continuation_identity(ContinuationQuery::Draft),
            Some(ContinuationEvidence::Draft { .. })
        ));
        assert!(
            spec.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                serde_json::json!({PROMPTED_PARAM:"a"}).as_object().unwrap()
            )))
            .is_none(),
            "exact Text and exact JSON remain distinct provider inputs"
        );

        let mismatched =
            ActionSpec::operation("run", "Run", OperationKind::Effect, "test.run", &())
                .unwrap()
                .with_continuation_identity(|_| {
                    Some(ContinuationEvidence::Draft {
                        resolution_domain: ContinuationKey::new("domain", [b"v1".as_slice()]),
                        handler: ContinuationKey::new("handler", [b"v1".as_slice()]),
                    })
                });
        assert!(
            mismatched
                .continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Immediate))
                .is_none()
        );
    }

    /// A prompted action names its one argument for what it is; every caller
    /// holding the typed text finds the name through the action, so renaming
    /// it never needs a caller changed.
    #[test]
    fn a_prompted_action_names_its_argument_and_callers_ask_it_for_the_name() {
        let encode = |title: &str| EncodedAction::encode_operation("test.rename", &title).unwrap();
        let named = ActionSpec::prompted_param(
            "rename",
            "Rename",
            ActionParam::required("title", ParamType::Text, "The note's new title"),
            "note title…",
            ExecutionLocus::ClientLocal,
            encode,
        );
        assert_eq!(named.prompt_param(), Some("title"));
        assert_eq!(named.params()[0].name.as_ref(), "title");
        assert_eq!(
            named.params()[0].description.as_ref(),
            "The note's new title"
        );
        let typed = named.prompt_arguments("Plan").unwrap();
        assert_eq!(
            typed,
            serde_json::json!({"title": "Plan"})
                .as_object()
                .unwrap()
                .clone()
        );
        // The typed projection and the palette's one-field input build the same bytes.
        assert_eq!(named.build_json(&typed).unwrap(), encode("Plan"));
        let ActionInput::Prompted { prompt, .. } = named.input() else {
            panic!("a prompted action prompts");
        };
        assert_eq!(prompt, "The note's new title");
        assert_eq!(named.build_text("Plan"), Some(encode("Plan")));
        // An action that collects no text has no prompt parameter.
        let immediate =
            ActionSpec::operation("run", "Run", OperationKind::Effect, "test.run", &()).unwrap();
        assert_eq!(immediate.prompt_param(), None);
        assert_eq!(immediate.prompt_arguments("x"), None);
    }

    /// One action type serves every item that offers it: each spec is the
    /// type bound to its item, builds for that item on every lane, and a
    /// change to one item's action copies the type rather than reaching the
    /// others.
    #[test]
    fn one_action_type_bound_to_many_targets_builds_for_each() {
        fn encode(item: &str, title: &str) -> EncodedAction {
            EncodedAction::encode_operation("notes.retitle", &(item, title)).unwrap()
        }
        let item = |target: &Target| target.get::<String>().cloned().unwrap_or_default();
        let retitle = ActionSpec::typed_operation_on(
            "retitle",
            "Retitle",
            "Title",
            OperationKind::Effect,
            "notes.retitle",
            vec![ActionParam::required("title", ParamType::Text, "The title")],
            move |target, args| encode(&item(target), args.text("title").unwrap_or("")),
        )
        .with_description("Give the note a new title");
        let ty = Arc::clone(retitle.action_type());
        let a = ty.bind(Target::new("a".to_owned()));
        let b = ty.bind(Target::new("b".to_owned()));
        assert!(Arc::ptr_eq(a.action_type(), b.action_type()));
        assert_eq!(a.description.as_deref(), Some("Give the note a new title"));
        assert_eq!(a.prompt_param(), b.prompt_param());
        let typed = a.prompt_arguments("Plan").unwrap();
        assert_eq!(a.build_json(&typed).unwrap(), encode("a", "Plan"));
        assert_eq!(b.build_json(&typed).unwrap(), encode("b", "Plan"));
        assert_eq!(b.build_text("Plan"), Some(encode("b", "Plan")));

        // A nullary type is encoded per target when it fires.
        let open = ActionSpec::typed_operation_on(
            "open",
            "Open",
            "",
            OperationKind::View,
            "notes.open",
            Vec::new(),
            move |target, _| EncodedAction::encode_operation("notes.open", &item(target)).unwrap(),
        );
        let open = Arc::clone(open.action_type());
        assert_eq!(
            open.bind(Target::none()).input(),
            ActionInput::Immediate(None)
        );
        assert_eq!(
            open.bind(Target::new("b".to_owned()))
                .encode_immediate()
                .as_deref(),
            Some(&EncodedAction::encode_operation("notes.open", &"b").unwrap())
        );

        // A deferred type builds for the target it is invoked on.
        let finish = ActionSpec::deferred_on(
            "finish",
            "Finish",
            ExecutionLocus::ClientLocal,
            "notes.finish",
            move |target| EncodedAction::encode_operation("notes.finish", &item(target)).unwrap(),
        );
        let finish = Arc::clone(finish.action_type()).bind(Target::new("a".to_owned()));
        assert_eq!(
            finish.build_deferred(),
            Some(EncodedAction::encode_operation("notes.finish", &"a").unwrap())
        );

        // Changing one item's action leaves the shared type alone.
        let unavailable = b.clone().unavailable("locked", "The note is locked");
        assert!(!Arc::ptr_eq(unavailable.action_type(), &ty));
        assert!(!unavailable.contract.availability.is_available());
        assert!(a.contract.availability.is_available());
        assert_eq!(unavailable.build_json(&typed).unwrap(), encode("b", "Plan"));
    }

    #[test]
    fn the_sole_text_of_an_argument_object_is_found_whatever_it_is_called() {
        let sole = |value: serde_json::Value| {
            sole_text_argument(value.as_object().unwrap()).map(str::to_owned)
        };
        assert_eq!(sole(serde_json::json!({"title": "a"})), Some("a".into()));
        assert_eq!(sole(serde_json::json!({"value": "b"})), Some("b".into()));
        assert_eq!(sole(serde_json::json!({})), None);
        assert_eq!(sole(serde_json::json!({"a": "x", "b": "y"})), None);
        assert_eq!(sole(serde_json::json!({"n": 1})), None);
    }

    /// The public parts constructor only names an address and carries the
    /// caller's arguments. It does not resolve the resource, run the action
    /// or mark the request as authorized; the host does all three.
    #[test]
    fn control_invocation_from_parts_is_an_inert_address_and_arguments() {
        let arguments = serde_json::json!({"title": "Plan"})
            .as_object()
            .unwrap()
            .clone();
        let invocation =
            ControlInvocation::from_parts("notes.note:1", "edit-title", arguments.clone())
                .expect("a stable address");
        assert_eq!(
            invocation.address,
            ActionAddress::new("notes.note:1", "edit-title")
        );
        assert_eq!(invocation.arguments, arguments);
        assert_eq!(
            ActionAddress::parse(&invocation.address.encode()),
            Some(invocation.address)
        );
        assert!(
            ControlInvocation::from_parts("notes\u{1f}note", "edit-title", arguments.clone())
                .is_none()
        );
        assert!(
            ControlInvocation::from_parts("notes.note:1", "edit\u{1f}title", arguments).is_none()
        );
    }

    #[test]
    fn continuation_keys_preserve_tuple_boundaries_and_clone_by_value() {
        let first = ContinuationKey::new("scope", [b"ab".as_slice(), b"c".as_slice()]);
        let second = ContinuationKey::new("scope", [b"a".as_slice(), b"bc".as_slice()]);
        assert_ne!(first, second);
        assert_eq!(first, first.clone());
    }

    /// An action declares that it opens the file its `path` names, for any
    /// file or for the extensions it names; one that only takes a `path`
    /// declares nothing.
    #[test]
    fn a_path_opener_is_declared_not_named() {
        let action = |id: &str| {
            ActionSpec::typed_operation(
                id,
                "Import",
                "",
                OperationKind::Effect,
                "sheets.import",
                vec![ActionParam::optional("path", ParamType::Text, "File")],
                |_| EncodedAction::encode_operation("sheets.import", &()).unwrap(),
            )
        };
        let csv = action("import-csv").opens_path(&["CSV"]);
        assert!(csv.contract.opens_path());
        assert_eq!(
            csv.contract.opened_extensions().collect::<Vec<_>>(),
            ["csv"]
        );
        let any = action("open").opens_path(&[]);
        assert!(any.contract.opens_path());
        assert_eq!(any.contract.opened_extensions().count(), 0);
        let iso = action("import-iso");
        assert!(!iso.contract.opens_path());
    }
}

#[cfg(test)]
mod history_context_tests {
    use super::*;

    fn spec() -> ActionSpec {
        ActionSpec::immediate_encoded(
            "set-block-text",
            "Set block text",
            ExecutionLocus::default(),
            EncodedAction {
                type_id: "notes.notebook".into(),
                payload: Vec::new(),
            },
        )
    }

    /// A nested resource's action is the enclosing record's field edit, so
    /// its history names the record's kind, which the change's claim is
    /// decided by, and keeps the body it was addressed through as a facet.
    #[test]
    fn a_nested_action_is_named_as_the_resource_whose_action_it_is() {
        let id = "notes.notebook/pages/p1/body";
        let own = spec().history_context(id, "tilo.rich-text", "Body");
        let own_resource = own.resource.expect("a resource");
        assert_eq!(own_resource.kind, "tilo.rich-text");
        assert!(own.facets.is_empty());

        let nested = spec()
            .applied_as("notes.notebook", "Notebook")
            .history_context(id, "tilo.rich-text", "Body");
        let resource = nested.resource.expect("a resource");
        assert_eq!(
            resource.id, id,
            "still the resource it was addressed through"
        );
        assert_eq!(resource.kind, "notes.notebook");
        assert_eq!(resource.label, "Notebook");
        assert_eq!(
            nested.facets,
            [crate::ActionFacet::new(NESTED_RESOURCE_FACET, "Body")]
        );
    }
}

#[cfg(test)]
#[path = "resource/effective_invocation_tests.rs"]
mod effective_invocation_tests;

#[cfg(test)]
#[path = "resource/deferred_tests.rs"]
mod deferred_tests;
