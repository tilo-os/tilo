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
//! [`prompted`](ActionSpec::prompted) projects to one required
//! [`Text`](ParamType::Text) parameter named [`PROMPTED_PARAM`] that calls the
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
    ActionContract, Availability, ConfirmationPolicy, DiscoveryScope, ExecutionMode,
    PermissionRequirement, ResultSpec, RetryPolicy, RiskLevel,
};
use crate::door::ActionAddress;
use crate::params::{ActionArgs, ActionParam, ArgError, ArgValue, ParamType};

/// Builds an action's encoded form from arguments already bound against its
/// declared [`params`](ActionSpec::params).
///
/// Infallible by design: every check a caller can fail — presence, type,
/// choice — happens in [`ActionArgs::bind`] *before* this runs, so a builder
/// that holds an `ActionArgs` is looking at values it declared and can use them
/// directly.
pub type ArgsBuilder = Arc<dyn Fn(&ActionArgs) -> EncodedAction + Send + Sync>;

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

/// Failure to construct or emit a control binding.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionBindingError {
    #[error("action `{action}` is not exported by resource `{resource}`")]
    ActionNotExported { resource: String, action: String },
    #[error(transparent)]
    InvalidArguments(#[from] ArgError),
}

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
    arguments: Arc<dyn Fn(I) -> serde_json::Map<String, serde_json::Value> + Send + Sync>,
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
        let arguments = (self.arguments)(input);
        if self.spec.deferred_invocation_type().is_some()
            || matches!(self.spec.input, ActionInput::Deferred { .. })
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
            || matches!(binding.spec.input, ActionInput::Deferred { .. })
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
            || matches!(binding.spec.input, ActionInput::Deferred { .. })
        {
            binding.spec.validate_json(&(binding.arguments)(()))?;
        } else {
            binding.spec.build_json(&(binding.arguments)(()))?;
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

/// The parameter name a [`prompted`](ActionSpec::prompted) spec projects its
/// single free-text argument under.
///
/// Fixed rather than derived from the human prompt: the prompt is localisable
/// display text, and an external caller addresses parameters by a stable name.
pub const PROMPTED_PARAM: &str = "value";

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

/// How an [`ActionSpec`] produces its replayable [`EncodedAction`] when chosen.
#[derive(Clone)]
pub enum ActionInput {
    /// Fires immediately with this pre-encoded action.
    Immediate(EncodedAction),
    /// A no-argument operation built only at final admitted owner execution.
    /// Enumeration and confirmation retain declared intent, not minted bytes.
    Deferred {
        type_id: String,
        build: Arc<dyn Fn() -> EncodedAction + Send + Sync>,
    },
    /// Collects one free-text argument first, then builds its encoded action.
    Prompted {
        prompt: String,
        placeholder: String,
        /// What that argument names. See [`Collects`].
        collects: Collects,
        build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>,
    },
}

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
}

impl InvocationBuilder {
    fn build(&self, args: &ActionArgs) -> EncodedAction {
        match self {
            Self::Fixed(encoded) => encoded.as_ref().clone(),
            Self::Dynamic { build, .. } => build(args),
        }
    }
}

/// One action a resource exports to the palette.
#[derive(Clone)]
pub struct ActionSpec {
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
    pub input: ActionInput,
    /// The typed argument surface an external caller invokes against. Private
    /// because it must stay in step with `build` — the two are always set
    /// together by a constructor, never independently.
    params: Vec<ActionParam>,
    /// Builds the encoded action from arguments bound against `params`. Shares
    /// the app's closure with `input` rather than duplicating it, which is what
    /// makes the palette path and the external path provably one function.
    build: InvocationBuilder,
    /// Classifies one invocation's bound arguments when the action's risk
    /// depends on them. See [`ActionSpec::with_risk`].
    risk_of: Option<RiskClassifier>,
    /// Names one invocation in history from the action it built. See
    /// [`ActionSpec::with_history_label`].
    history_label_of: Option<HistoryLabeler>,
    continuation_resolver: Option<ContinuationResolver>,
}

/// Classifies the risk of one invocation from its bound arguments.
pub type RiskClassifier = Arc<dyn Fn(&ActionArgs) -> RiskLevel + Send + Sync>;

/// Names one invocation in history from the encoded action it built, or
/// declines with `None`.
pub type HistoryLabeler = Arc<dyn Fn(&EncodedAction) -> Option<String> + Send + Sync>;

struct TypedActionSeed {
    id: String,
    label: String,
    placeholder: String,
    locus: ExecutionLocus,
    kind: OperationKind,
    operation_type: Option<String>,
    params: Vec<ActionParam>,
}

impl ActionSpec {
    /// Borrow the actual fixed encoding used by the typed invocation builder.
    /// `None` means the builder is opaque, even if its palette input is Immediate.
    /// This is comparison data, never permission to invoke an action.
    pub fn fixed_invocation_encoding(&self) -> Option<&EncodedAction> {
        match &self.build {
            InvocationBuilder::Fixed(encoded) => Some(encoded),
            InvocationBuilder::Dynamic { .. } => None,
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
            InvocationBuilder::Fixed(_) => None,
        }
    }

    pub(crate) fn validate_deferred_shape(&self) -> Result<(), ArgError> {
        match (&self.input, self.deferred_invocation_type()) {
            (ActionInput::Deferred { type_id, .. }, Some(expected))
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
            (ActionInput::Deferred { .. }, _) | (_, Some(_)) => Err(ArgError::Constraint {
                name: "deferred".into(),
                reason: "deferred input and retained declaration disagree".into(),
            }),
            (_, None) => Ok(()),
        }
    }

    fn validate_built(&self, encoded: EncodedAction) -> Result<EncodedAction, ArgError> {
        if let Some(expected) = self.deferred_invocation_type()
            && encoded.type_id != expected
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

    /// A document operation whose no-argument builder runs only at execution.
    /// Both invocation lanes share this one closure; construction never runs it.
    pub fn deferred(
        id: impl Into<String>,
        label: impl Into<String>,
        locus: ExecutionLocus,
        type_id: impl Into<String>,
        build: impl Fn() -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self::deferred_with_kind(
            id.into(),
            label.into(),
            locus,
            OperationKind::Document,
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
            id.into(),
            label.into(),
            ExecutionLocus::ClientLocal,
            kind,
            type_id.into(),
            build,
        )
    }

    fn deferred_with_kind(
        id: String,
        label: String,
        locus: ExecutionLocus,
        kind: OperationKind,
        type_id: String,
        build: impl Fn() -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let build: Arc<dyn Fn() -> EncodedAction + Send + Sync> = Arc::new(build);
        let shared = build.clone();
        Self {
            id,
            label,
            description: None,
            locus,
            kind,
            operation_type: (kind != OperationKind::Document).then(|| type_id.clone()),
            contract: if kind == OperationKind::View {
                ActionContract::view()
            } else {
                ActionContract::default()
            },
            input: ActionInput::Deferred {
                type_id: type_id.clone(),
                build,
            },
            params: vec![],
            build: InvocationBuilder::Dynamic {
                build: Arc::new(move |_| shared()),
                deferred_type: Some(type_id),
            },
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        }
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
        let for_build = encoded.clone();
        Ok(Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            kind: OperationKind::Document,
            operation_type: None,
            contract: ActionContract::default(),
            input: ActionInput::Immediate(encoded),
            params: Vec::new(),
            build: InvocationBuilder::Fixed(Arc::new(for_build)),
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        })
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
        let for_build = encoded.clone();
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            kind: OperationKind::Document,
            operation_type: None,
            contract: ActionContract::default(),
            input: ActionInput::Immediate(encoded),
            params: Vec::new(),
            build: InvocationBuilder::Fixed(Arc::new(for_build)),
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        }
    }

    /// An action that first collects a free-text argument, then builds its
    /// encoded action from it.
    ///
    /// Projects to one required [`Text`](ParamType::Text) parameter named
    /// [`PROMPTED_PARAM`], described by `prompt`. The projection and the
    /// palette share the *same* `build` closure, so an external invocation and
    /// the on-screen affordance produce the byte-identical [`EncodedAction`].
    pub fn prompted(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let prompt = prompt.into();
        // One `Arc`, shared by the palette's `ActionInput` and the typed
        // projection. Not two closures that happen to agree.
        let build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync> = Arc::new(build);
        let for_args = build.clone();
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            kind: OperationKind::Document,
            operation_type: None,
            contract: ActionContract::default(),
            params: vec![ActionParam::required_owned(
                PROMPTED_PARAM,
                ParamType::Text,
                prompt.clone(),
            )],
            build: InvocationBuilder::Dynamic {
                build: Arc::new(move |args| for_args(args.text(PROMPTED_PARAM).unwrap_or(""))),
                deferred_type: None,
            },
            input: ActionInput::Prompted {
                prompt,
                placeholder: placeholder.into(),
                collects: Collects::Words,
                build,
            },
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        }
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
        let for_build = encoded.clone();
        Ok(Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind,
            operation_type: Some(type_id),
            contract: if kind == OperationKind::View {
                ActionContract::view()
            } else {
                ActionContract::default()
            },
            input: ActionInput::Immediate(encoded),
            params: Vec::new(),
            build: InvocationBuilder::Fixed(Arc::new(for_build)),
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        })
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
        let build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync> =
            Arc::new(move |value| enforce_operation_type(build(value), &expected));
        let for_args = build.clone();
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind,
            operation_type: Some(type_id),
            contract: if kind == OperationKind::View {
                ActionContract::view()
            } else {
                ActionContract::default()
            },
            params: vec![ActionParam::required_owned(
                PROMPTED_PARAM,
                ParamType::Text,
                prompt.clone(),
            )],
            build: InvocationBuilder::Dynamic {
                build: Arc::new(move |args| for_args(args.text(PROMPTED_PARAM).unwrap_or(""))),
                deferred_type: None,
            },
            input: ActionInput::Prompted {
                prompt,
                placeholder: placeholder.into(),
                collects: Collects::Words,
                build,
            },
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        }
    }

    /// An action with a real typed argument surface.
    ///
    /// This is the constructor for an argument that has a *type* — a number in
    /// `0..=1`, a rate, one of a fixed set of filter names — rather than prose
    /// a lenient parser guesses at. [`ActionArgs::bind`] enforces the
    /// declaration, so a caller that sends `"hlaf"` for a choice parameter gets
    /// a named refusal instead of a silent default.
    ///
    /// The palette can still offer it. `input` is derived from `params`:
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
            build,
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
            build,
        )
    }

    fn typed_with_kind(
        seed: TypedActionSeed,
        build: impl Fn(&ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let build: ArgsBuilder = if let Some(expected) = seed.operation_type.clone() {
            Arc::new(move |args| enforce_operation_type(build(args), &expected))
        } else {
            Arc::new(build)
        };
        let input = derive_input(&seed.params, &build, seed.placeholder);
        Self {
            id: seed.id,
            label: seed.label,
            description: None,
            locus: seed.locus,
            kind: seed.kind,
            operation_type: seed.operation_type,
            contract: if seed.kind == OperationKind::View {
                ActionContract::view()
            } else {
                ActionContract::default()
            },
            input,
            params: seed.params,
            build: InvocationBuilder::Dynamic {
                build,
                deferred_type: None,
            },
            risk_of: None,
            history_label_of: None,
            continuation_resolver: None,
        }
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
        self.continuation_resolver = Some(Arc::new(resolver));
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
    /// constructor (not a parameter on [`Self::prompted`]) so every
    /// existing action keeps saying exactly what it said.
    pub fn prompted_content(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let mut spec = Self::prompted(id, label, prompt, placeholder, locus, build);
        if let ActionInput::Prompted { collects, .. } = &mut spec.input {
            *collects = Collects::Content;
        }
        spec
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn with_contract(mut self, contract: ActionContract) -> Self {
        self.contract = contract;
        self
    }

    #[must_use]
    pub fn unavailable(mut self, code: impl Into<String>, reason: impl Into<String>) -> Self {
        self.contract.availability = Availability::Unavailable {
            code: code.into(),
            reason: reason.into(),
        };
        self
    }

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.contract.risk = RiskLevel::Destructive;
        self.contract.confirmation = ConfirmationPolicy::Confirm;
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
        push_tag(&mut self.contract, crate::contract::PROGRAMMATIC_TAG);
        self
    }

    /// Declare this read-only action as the one that answers, with no
    /// arguments, the first page of the app's open document. Recorded as
    /// [`DOCUMENT_READ_TAG`](crate::DOCUMENT_READ_TAG), so a caller that just
    /// opened a file finds it in the directory. Call after
    /// [`with_contract`](Self::with_contract), which replaces the tags.
    #[must_use]
    pub fn reads_document(mut self) -> Self {
        push_tag(&mut self.contract, crate::contract::DOCUMENT_READ_TAG);
        self
    }

    /// Whether this action is offered only to programmatic callers. See
    /// [`programmatic`](Self::programmatic).
    pub fn is_programmatic(&self) -> bool {
        self.contract.is_programmatic()
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
        mut self,
        classify: impl Fn(&ActionArgs) -> RiskLevel + Send + Sync + 'static,
    ) -> Self {
        push_tag(&mut self.contract, crate::contract::ARGUMENT_RISK_TAG);
        self.risk_of = Some(Arc::new(classify));
        self
    }

    /// The risk of one invocation: the classifier's verdict for these
    /// arguments, or the declared [`ActionContract::risk`].
    pub fn risk_for(&self, args: &ActionArgs) -> RiskLevel {
        self.risk_of
            .as_ref()
            .map_or(self.contract.risk, |classify| classify(args))
    }

    /// Name each invocation in history after the action it built rather
    /// than after this spec, for an action that carries different edits (a
    /// generic typed edit is "Remove marks" once and "Edit marks" the next
    /// time). `None` falls back to [`label`](Self::label).
    #[must_use]
    pub fn with_history_label(
        mut self,
        label: impl Fn(&EncodedAction) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.history_label_of = Some(Arc::new(label));
        self
    }

    /// What history calls one invocation that built `encoded`: the
    /// [`with_history_label`](Self::with_history_label) name when there is
    /// one, else the spec's [`label`](Self::label).
    pub fn history_label(&self, encoded: &EncodedAction) -> String {
        self.history_label_of
            .as_ref()
            .and_then(|label| label(encoded))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| self.label.clone())
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
        self.contract.risk = RiskLevel::Privileged;
        self.contract.confirmation = ConfirmationPolicy::Confirm;
        self
    }

    #[must_use]
    pub fn requiring(mut self, permission: PermissionRequirement) -> Self {
        self.contract.permission = permission;
        self
    }

    #[must_use]
    pub fn reversible(mut self) -> Self {
        self.contract.reversible = true;
        self
    }

    #[must_use]
    pub fn retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.contract.retry = retry;
        self
    }

    #[must_use]
    pub fn execution(mut self, execution: ExecutionMode) -> Self {
        self.contract.execution = execution;
        self
    }

    #[must_use]
    pub fn returns(mut self, result: ResultSpec) -> Self {
        self.contract.result = result;
        self
    }

    #[must_use]
    pub fn launchable_when_closed(mut self) -> Self {
        self.contract.discovery = DiscoveryScope::Launchable;
        self
    }

    #[must_use]
    pub fn durably_discoverable(mut self) -> Self {
        self.contract.discovery = DiscoveryScope::Durable;
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
        self.build = match self.build {
            InvocationBuilder::Fixed(encoded) => {
                InvocationBuilder::Fixed(Arc::new(wrap(encoded.as_ref().clone())))
            }
            InvocationBuilder::Dynamic {
                build,
                deferred_type,
            } => {
                let wrap = Arc::clone(&wrap);
                InvocationBuilder::Dynamic {
                    build: Arc::new(move |args| wrap(build(args))),
                    deferred_type: deferred_type.map(|_| wrapped_type.to_owned()),
                }
            }
        };
        self.input = match self.input {
            ActionInput::Immediate(encoded) => ActionInput::Immediate(wrap(encoded)),
            ActionInput::Deferred { build, .. } => ActionInput::Deferred {
                type_id: wrapped_type.to_owned(),
                build: Arc::new(move || wrap(build())),
            },
            ActionInput::Prompted {
                prompt,
                placeholder,
                collects,
                build,
            } => ActionInput::Prompted {
                prompt,
                placeholder,
                collects,
                build: Arc::new(move |text| wrap(build(text))),
            },
        };
        self
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
        self.validate_built(self.build.build(&bound))
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
        self.validate_built(self.build.build(&bound))
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
    params
        .iter()
        .find(|p| p.required)
        .or_else(|| params.first())
        .map_or_else(Vec::new, |primary| {
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

/// Derive the palette-facing [`ActionInput`] for a [`typed`](ActionSpec::typed)
/// spec from its declared parameters. See that constructor's docs.
fn derive_input(params: &[ActionParam], build: &ArgsBuilder, placeholder: String) -> ActionInput {
    let Some(primary) = params
        .iter()
        .find(|p| p.required)
        .or_else(|| params.first())
    else {
        return ActionInput::Immediate(build(&ActionArgs::none()));
    };
    let name = primary.name.to_string();
    let ty = primary.ty;
    let params = params.to_vec();
    let build = build.clone();
    ActionInput::Prompted {
        prompt: primary.description.to_string(),
        placeholder,
        collects: Collects::Words,
        build: Arc::new(move |text: &str| {
            let supplied = parse_argument(&name, ty, text);
            match ActionArgs::bind(&params, supplied) {
                Ok(bound) => build(&bound),
                Err(error) => EncodedAction {
                    type_id: INVALID_ARGUMENTS_TYPE.into(),
                    payload: error.to_string().into_bytes(),
                },
            }
        }),
    }
}

/// An app instance implements this to publish the resource tree it
/// currently has open. The host collects the focused instance's tree
/// each frame; `doc` lets a provider enumerate resources living *in* the
/// document (e.g. a book's annotations).
///
/// Deprecated: apps must be a `tilo_app::ResourceApp` whose tree comes only
/// from `tilo_resource::publish`. Do not hand-build a second
/// `tilo_actions::Resource` tree for the same document state.
#[deprecated(note = "use ResourceApp + tilo_resource::publish; do not hand-build Resource trees")]
pub trait ResourceProvider {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource>;

    /// Resolve one stable resource id without requiring it to be present in
    /// the eagerly published open-resource snapshot.
    ///
    /// Providers with large child collections should keep `resources`
    /// contextual and override this hook with an indexed lookup. The default
    /// preserves the original behaviour for small providers.
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<Resource> {
        fn find(resources: Vec<Resource>, id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource);
                }
                if let Some(found) = find(resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(self.resources(doc), id)
    }

    /// The resource tree as seen by one participant (a window or an agent
    /// task), built from that participant's own view state.
    ///
    /// `None` is the provider's default view (the local window). Single-view
    /// providers keep this default, which ignores the participant.
    /// Multi-participant providers (Writer) override it and must never let
    /// one participant's view leak into another's tree.
    fn resources_for(
        &self,
        participant: Option<&crate::ParticipantId>,
        doc: &automerge::AutoCommit,
    ) -> Vec<Resource> {
        let _ = participant;
        self.resources(doc)
    }

    /// Resolve one resource id in `participant`'s tree. The default searches
    /// [`Self::resources_for`], and keeps [`Self::resource`]'s indexed lookup
    /// for the default view.
    fn resource_for(
        &self,
        participant: Option<&crate::ParticipantId>,
        doc: &automerge::AutoCommit,
        id: &str,
    ) -> Option<Resource> {
        fn find(resources: Vec<Resource>, id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource);
                }
                if let Some(found) = find(resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        let Some(participant) = participant else {
            return self.resource(doc, id);
        };
        find(self.resources_for(Some(participant), doc), id)
    }

    /// The participant-independent catalogue this provider publishes to
    /// the broker and grants are checked against (see [`crate::schema`]).
    ///
    /// It must not depend on any participant's caret, selection or Find
    /// state: stable addresses only, with context preconditions declared as
    /// [`ContextRequirement`](crate::ContextRequirement) flags. Every
    /// operation reachable in any participant's
    /// [`resources_for`](Self::resources_for) tree must have an entry here
    /// with the same contract. Single-view providers keep this default.
    fn catalogue_schema(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        self.resources(doc)
    }
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
        let prompted = ActionSpec::prompted(
            "rename",
            "Rename",
            "Name",
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

    #[test]
    fn continuation_keys_preserve_tuple_boundaries_and_clone_by_value() {
        let first = ContinuationKey::new("scope", [b"ab".as_slice(), b"c".as_slice()]);
        let second = ContinuationKey::new("scope", [b"a".as_slice(), b"bc".as_slice()]);
        assert_ne!(first, second);
        assert_eq!(first, first.clone());
    }
}

#[cfg(test)]
#[path = "resource/effective_invocation_tests.rs"]
mod effective_invocation_tests;

#[cfg(test)]
#[path = "resource/deferred_tests.rs"]
mod deferred_tests;
