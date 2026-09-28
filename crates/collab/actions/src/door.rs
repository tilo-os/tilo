//! **The door**: how a participant that is not the app enumerates what
//! it has open and performs one of its exported operations.
//!
//! [`Resource`] trees already describe an app's surface and
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
use crate::executor::{ActionTransport, Dispatched, dispatch_by_with_context};
use crate::registry::ActionRegistry;
use crate::resource::{
    ActionInput, ActionSpec, Collects, KEY_SEP, Resource, ResourceViewMountSpec,
};
use serde::{Deserialize, Serialize};

/// The stable address of one exported operation: which resource, which
/// action. Both halves are the app's own ids — no screen position, no
/// list index — so the same address names the same operation across
/// layouts, themes, resolutions, and a caller with no display can work
/// from ids and labels alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActionAddress {
    pub resource: String,
    pub action: String,
}

impl ActionAddress {
    pub fn new(resource: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            action: action.into(),
        }
    }

    /// The single-string form, for transports that carry one id. [`KEY_SEP`]
    /// never appears in either half, so [`parse`](Self::parse) is unambiguous.
    pub fn encode(&self) -> String {
        format!("{}{}{}", self.resource, KEY_SEP, self.action)
    }

    /// Recover an address from [`encode`](Self::encode). `None` when the string
    /// is not one, so a caller that made an address up is refused here rather
    /// than looked up against a half-parsed id.
    pub fn parse(s: &str) -> Option<Self> {
        let (resource, action) = s.split_once(KEY_SEP)?;
        if resource.is_empty() || action.is_empty() {
            return None;
        }
        Some(Self::new(resource, action))
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

impl InputSpec {
    fn of(input: &ActionInput) -> Self {
        match input {
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => InputSpec::None,
            ActionInput::Prompted {
                prompt,
                placeholder,
                collects,
                ..
            } => InputSpec::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                collects: *collects,
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
    /// The publishing resource's stable kind tag (`word_processor.section`,
    /// `files.entry`), shared with the OS-wide type registry so the same kind
    /// means the same thing across apps.
    pub resource_kind: String,
    /// The resource's human label, as the app words it.
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
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
}

impl ResourceViewDescriptor {
    /// The view `resource` declares for `action_id`, when that action is a
    /// view's open action.
    #[must_use]
    pub fn declared_for(resource: &Resource, action_id: &str) -> Option<Self> {
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
            })
    }
}

/// Why an address could not be performed. `Unknown` deliberately covers
/// both "no such address" and "not offered to you", so a caller can't
/// map what it may not reach by probing for a different error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoorError {
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
    /// The address exists and is intentionally advertised, but its current
    /// precondition is not satisfied.
    Unavailable { code: String, reason: String },
    /// The action contract requires an explicit confirmation step that this
    /// invocation did not carry.
    ConfirmationRequired,
    /// The app never registered a decoder for the action's type, so the door
    /// can't prove what it would run. Fail-closed: a refusal, not a
    /// best-effort apply.
    UnregisteredType(String),
    /// The action ran and failed.
    Failed(String),
}

impl DoorError {
    /// Stable machine-readable code, distinguishing "I addressed this
    /// wrongly" from "it ran and could not do its job".
    pub fn code(&self) -> &'static str {
        match self {
            DoorError::Unknown(_) | DoorError::UnregisteredType(_) => "unknown_action",
            DoorError::MissingArgument(_)
            | DoorError::UnexpectedArgument(_)
            | DoorError::InvalidArguments(_) => "invalid_arguments",
            DoorError::Unavailable { .. } => "action_unavailable",
            DoorError::ConfirmationRequired => "action_confirmation_required",
            DoorError::Failed(_) => "action_failed",
        }
    }
}

impl std::fmt::Display for DoorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DoorError::Unknown(a) => {
                write!(f, "no action addressed by ({}, {})", a.resource, a.action)
            }
            DoorError::MissingArgument(a) => write!(
                f,
                "action ({}, {}) needs a text argument and none was given",
                a.resource, a.action
            ),
            DoorError::UnexpectedArgument(a) => {
                write!(f, "action ({}, {}) takes no argument", a.resource, a.action)
            }
            DoorError::InvalidArguments(why) | DoorError::Failed(why) => write!(f, "{why}"),
            DoorError::Unavailable { code, reason } => write!(f, "{code}: {reason}"),
            DoorError::ConfirmationRequired => {
                f.write_str("this action requires explicit confirmation")
            }
            DoorError::UnregisteredType(t) => {
                write!(f, "no decoder registered for action type `{t}`")
            }
        }
    }
}

impl std::error::Error for DoorError {}

/// Walk a resource forest and describe every operation it exports.
/// Depth-first, parents before children, matching
/// [`flatten`](crate::resource::flatten)'s order. Borrows the tree,
/// returns owned data — nothing handed back can act on what it described.
pub fn survey(resources: &[Resource]) -> Vec<ActionDescriptor> {
    let mut out = Vec::new();
    for r in resources {
        survey_into(r, &mut out);
    }
    out
}

fn survey_into(resource: &Resource, out: &mut Vec<ActionDescriptor>) {
    for action in &resource.actions {
        let view = ResourceViewDescriptor::declared_for(resource, &action.id);
        out.push(ActionDescriptor {
            address: ActionAddress::new(resource.id.as_str(), &action.id),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action.label.clone(),
            description: action.description.clone(),
            locus: action.locus,
            kind: action.kind,
            contract: action.contract.clone(),
            input: InputSpec::of(&action.input),
            params: action.params().to_vec(),
            view,
        });
    }
    for child in &resource.children {
        survey_into(child, out);
    }
}

/// Resolve `address` against `resources` without running anything, so a caller
/// can check what an address currently names before committing to it.
pub fn resolve<'a>(resources: &'a [Resource], address: &ActionAddress) -> Option<&'a ActionSpec> {
    fn walk<'a>(r: &'a Resource, address: &ActionAddress) -> Option<&'a ActionSpec> {
        if r.id.as_str() == address.resource
            && let Some(spec) = r.actions.iter().find(|a| a.id == address.action)
        {
            return Some(spec);
        }
        r.children.iter().find_map(|c| walk(c, address))
    }
    resources.iter().find_map(|r| walk(r, address))
}

/// Turn an address plus its argument into the replayable action it
/// names. Split out from [`invoke`] so a host can build/inspect/route
/// the encoded action itself, and so argument rules are checked in one
/// place. Binding happens *before* anything runs, so a bad call is a
/// refusal with the document untouched, never a half-applied edit.
///
/// # Errors
///
/// Returns [`DoorError::Unknown`] if `address` names no resource/action
/// in `resources`; [`DoorError::UnexpectedArgument`] if `argument` is
/// `Some` for an [`ActionInput::Immediate`] action; or
/// [`DoorError::MissingArgument`] if it's `None` for an
/// [`ActionInput::Prompted`] one.
pub fn encode(
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<EncodedAction, DoorError> {
    let spec = resolve(resources, address).ok_or_else(|| DoorError::Unknown(address.clone()))?;
    ensure_available(spec)?;
    spec.validate_deferred_shape()
        .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    match (&spec.input, argument) {
        (ActionInput::Deferred { type_id, build }, None) => {
            let encoded = build();
            if encoded.type_id != *type_id {
                return Err(DoorError::InvalidArguments(format!(
                    "builder returned `{}` but declared `{type_id}`",
                    encoded.type_id
                )));
            }
            Ok(encoded)
        }
        (ActionInput::Deferred { .. }, Some(_)) => {
            Err(DoorError::UnexpectedArgument(address.clone()))
        }
        (ActionInput::Immediate(encoded), None) => Ok(encoded.clone()),
        (ActionInput::Immediate(_), Some(_)) => Err(DoorError::UnexpectedArgument(address.clone())),
        (ActionInput::Prompted { build, .. }, Some(text)) => {
            let encoded = build(text);
            if encoded.type_id == crate::resource::INVALID_ARGUMENTS_TYPE {
                return Err(DoorError::InvalidArguments(
                    String::from_utf8_lossy(&encoded.payload).into_owned(),
                ));
            }
            Ok(encoded)
        }
        (ActionInput::Prompted { .. }, None) => Err(DoorError::MissingArgument(address.clone())),
    }
}

/// Build an action from the canonical typed JSON argument object advertised
/// by [`ActionDescriptor::params`]. Binding is strict and completes before any
/// document mutation or application effect is attempted.
///
/// # Errors
///
/// Returns [`DoorError::Unknown`] when the address is not published,
/// [`DoorError::Unavailable`] when policy currently disables it, or
/// [`DoorError::InvalidArguments`] when the object violates its parameter
/// contract.
pub fn encode_json(
    resources: &[Resource],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<EncodedAction, DoorError> {
    let spec = resolve(resources, address).ok_or_else(|| DoorError::Unknown(address.clone()))?;
    ensure_available(spec)?;
    let encoded = spec
        .build_json(arguments)
        .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
    if encoded.type_id == crate::resource::INVALID_ARGUMENTS_TYPE {
        return Err(DoorError::InvalidArguments(
            String::from_utf8_lossy(&encoded.payload).into_owned(),
        ));
    }
    Ok(encoded)
}

fn ensure_available(spec: &ActionSpec) -> Result<(), DoorError> {
    match &spec.contract.availability {
        crate::Availability::Available => Ok(()),
        crate::Availability::Unavailable { code, reason } => Err(DoorError::Unavailable {
            code: code.clone(),
            reason: reason.clone(),
        }),
    }
}

/// Caller identity and optional semantic context for an invoked action.
///
/// Every door entry point takes one, so an invocation made inside a task or
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
/// caller's Automerge actor bytes, so the room's history names who asked;
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
/// Returns any error [`encode`] documents; [`DoorError::ConfirmationRequired`]
/// when the action's effective confirmation for this argument is not
/// [`None`](crate::ConfirmationPolicy::None) (see [`invoke_confirmed`]);
/// [`DoorError::UnregisteredType`] if `registry` has no decoder for the
/// encoded action's type; or [`DoorError::Failed`] if [`dispatch_by`](crate::executor::dispatch_by) fails
/// once decoded.
pub fn invoke(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    invoke_with_consent(
        doc, resources, address, argument, provenance, registry, transport, false,
    )
}

/// [`invoke`] after an outer policy boundary has completed the action
/// contract's confirmation step. This is intentionally separate from
/// [`invoke`], whose default is fail-closed for consequential actions.
///
/// # Errors
///
/// Returns the same errors as [`invoke`], except confirmation has already
/// been granted by the caller.
pub fn invoke_confirmed(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    invoke_with_consent(
        doc, resources, address, argument, provenance, registry, transport, true,
    )
}

fn invoke_with_consent(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
    confirmed: bool,
) -> Result<Dispatched, DoorError> {
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
///
/// # Errors
///
/// Returns address, availability, confirmation, argument, registry, dispatch,
/// and result-contract errors encountered while invoking the action.
pub fn invoke_json(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    invoke_json_with_consent(
        doc, resources, address, arguments, provenance, registry, transport, false,
    )
}

/// Typed-argument invocation after an outer policy boundary has completed the
/// action contract's confirmation step.
///
/// # Errors
///
/// Returns the same errors as [`invoke_json`], except confirmation has already
/// been granted by the caller.
pub fn invoke_json_confirmed(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    invoke_json_with_consent(
        doc, resources, address, arguments, provenance, registry, transport, true,
    )
}

fn invoke_json_with_consent(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
    confirmed: bool,
) -> Result<Dispatched, DoorError> {
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
    resources: &[Resource],
    address: &ActionAddress,
    confirmed: bool,
    policy: impl FnOnce(&ActionSpec) -> crate::ConfirmationPolicy,
) -> Result<(), DoorError> {
    let spec = resolve(resources, address).ok_or_else(|| DoorError::Unknown(address.clone()))?;
    if !matches!(policy(spec), crate::ConfirmationPolicy::None) && !confirmed {
        return Err(DoorError::ConfirmationRequired);
    }
    Ok(())
}

// This low-level transport accepts already encoded actions. A Deferred server
// operation must instead reach the SDK's address-based broker door before mint.
fn ensure_deferred_local_owner(
    resources: &[Resource],
    address: &ActionAddress,
    registry: &ActionRegistry,
) -> Result<(), DoorError> {
    let spec = resolve(resources, address).ok_or_else(|| DoorError::Unknown(address.clone()))?;
    if spec.deferred_invocation_type().is_some()
        || matches!(spec.input, ActionInput::Deferred { .. })
    {
        spec.validate_deferred_shape()
            .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
        if spec.kind != OperationKind::Document || spec.locus != ExecutionLocus::ClientLocal {
            return Err(DoorError::Unavailable {
                code: "deferred_owner_required".into(),
                reason: "This creation must use the owning application's address-based action door"
                    .into(),
            });
        }
        if let Some(type_id) = spec.deferred_invocation_type()
            && !registry.contains(type_id)
        {
            return Err(DoorError::UnregisteredType(type_id.into()));
        }
    }
    Ok(())
}

fn dispatch_encoded_with_context(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    encoded: EncodedAction,
    provenance: InvocationProvenance<'_>,
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    let resource = find_resource(resources, &address.resource)
        .ok_or_else(|| DoorError::Unknown(address.clone()))?;
    let spec = resource
        .actions
        .iter()
        .find(|candidate| candidate.id == address.action)
        .ok_or_else(|| DoorError::Unknown(address.clone()))?;
    if spec.kind != OperationKind::Document {
        return Err(DoorError::Failed(format!(
            "{} operation must be handled by the owning application",
            spec.kind.as_str()
        )));
    }
    let mut context = ActionContext::for_resource(
        &spec.label,
        resource.id.as_str(),
        &resource.kind,
        &resource.label,
    );
    if let Some(ambient) = provenance.ambient {
        context = context.enriched_by(ambient);
    }
    let action = registry
        .decode(&encoded)
        .map_err(|_| DoorError::UnregisteredType(encoded.type_id.clone()))?;
    if spec.deferred_invocation_type().is_some() && action.locus() != spec.locus {
        return Err(DoorError::Failed(
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
    .map_err(|e| DoorError::Failed(e.to_string()))
}

fn find_resource<'a>(resources: &'a [Resource], id: &str) -> Option<&'a Resource> {
    fn walk<'a>(resource: &'a Resource, id: &str) -> Option<&'a Resource> {
        if resource.id.as_str() == id {
            return Some(resource);
        }
        resource.children.iter().find_map(|child| walk(child, id))
    }
    resources.iter().find_map(|resource| walk(resource, id))
}

#[cfg(test)]
#[allow(
    deprecated,
    reason = "tests pin ResourceProvider behaviour while the trait stays re-exported"
)]
mod tests {
    use super::*;
    use crate::action::{Action, ActionError, Doc};
    use crate::executor::{LocalTransport, replay};
    use crate::resource::ResourceProvider;
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
    fn note(star_does: &Note, retitle_does: fn(&str) -> Note) -> Vec<Resource> {
        vec![
            Resource::new("notes.note:1", "notes.note", "Harbour")
                .with_action(ActionSpec::immediate("star", "Star", NOTE_TYPE, star_does).unwrap())
                .with_action(ActionSpec::prompted(
                    "retitle",
                    "Rename",
                    "New title",
                    "title",
                    ExecutionLocus::ClientLocal,
                    move |text| encoded(&retitle_does(text)),
                )),
        ]
    }

    fn open() -> Vec<Resource> {
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
        tree: &[Resource],
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<Dispatched, DoorError> {
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

    /// A descriptor carries nothing that could act, survey annotates only the declared view-open
    /// action, enumeration leaves the document untouched, and an address survives its string form.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn surveying_through_the_door_describes_without_acting() {
        a_descriptor_carries_nothing_that_could_act();
        survey_annotates_only_the_declared_view_open_action();
        enumeration_leaves_the_document_untouched();
        an_address_survives_its_string_form();
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

    fn survey_annotates_only_the_declared_view_open_action() {
        let open =
            ActionSpec::operation("open", "Open", OperationKind::View, "notes.open", &()).unwrap();
        let select =
            ActionSpec::operation("select", "Select", OperationKind::View, "notes.select", &())
                .unwrap();
        let resource = Resource::new("note:1", "notes.note", "Harbour")
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
                mount: None,
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

    /// Stale addresses, mismatched arguments and unregistered action types are refused rather than
    /// redirected or applied; an accepted change is attributed to the caller and ambient task
    /// context reaches it without overwriting the resource.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn invoking_through_the_door_refuses_mismatches_and_attributes_the_caller() {
        an_address_that_no_longer_resolves_is_refused_not_redirected();
        an_argument_that_does_not_match_the_declaration_is_refused();
        an_unregistered_action_type_is_refused_rather_than_applied();
        a_change_through_the_door_is_attributed_to_the_caller();
        ambient_task_context_reaches_door_actions_without_overwriting_resource();
    }

    /// A tree is per-frame presentation, so an address is re-resolved against a
    /// freshly built one. An address whose resource has gone is refused, not
    /// applied to whatever is open in its place.
    fn an_address_that_no_longer_resolves_is_refused_not_redirected() {
        let address = survey(&open())[0].address.clone();
        assert_eq!(address, star());

        // The note closed; a different one is open, exporting the same action id.
        let now = vec![
            Resource::new("notes.note:2", "notes.note", "Ledger").with_action(
                ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap(),
            ),
        ];
        assert!(resolve(&now, &address).is_none());

        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        let err = perform(&mut doc, &now, &address, None).unwrap_err();

        assert_eq!(err, DoorError::Unknown(address));
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
        assert_eq!(err, DoorError::MissingArgument(retitle()));
        assert_eq!(err.code(), "invalid_arguments");

        let err = perform(&mut doc, &tree, &star(), Some("Ledger")).unwrap_err();
        assert_eq!(err, DoorError::UnexpectedArgument(star()));
        assert_eq!(err.code(), "invalid_arguments");

        assert_eq!(doc.get_heads(), before, "a refused call left no change");
        assert!(doc.get(&ROOT, "title").unwrap().is_none());
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// An action the app never registered a decoder for cannot be run, even
    /// though its own tree offers it: the door will not dispatch what it cannot
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

        assert_eq!(err, DoorError::UnregisteredType(NOTE_TYPE.into()));
        assert_eq!(doc.get_heads(), before);
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// A provider that reads the document to build its tree, so enumeration
    /// exercises the path an app actually takes.
    struct Notes;

    impl ResourceProvider for Notes {
        fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
            let label = doc
                .get(&ROOT, "title")
                .ok()
                .flatten()
                .and_then(|(v, _)| v.into_string().ok())
                .unwrap_or_else(|| "Untitled".into());
            vec![
                Resource::new("notes.note:1", "notes.note", label).with_action(
                    ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap(),
                ),
            ]
        }
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

        let first = survey(&Notes.resources(&doc));
        let second = survey(&Notes.resources(&doc));

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

    /// A change made through the door commits under the *caller's* actor, so
    /// history names who asked rather than the app — and since the actor
    /// rides the envelope, a peer still replays to the identical document.
    fn a_change_through_the_door_is_attributed_to_the_caller() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        doc.commit();
        let app_actor = doc.get_actor().clone();
        let mut peer = AutoCommit::load(&doc.save()).unwrap();

        let caller: &[u8] = b"maren-device-key";
        let mut transport = LocalTransport::new();
        let dispatched = invoke(
            &mut doc,
            &open(),
            &star(),
            None,
            InvocationProvenance::caller(caller),
            &registry(),
            &mut transport,
        )
        .unwrap();

        assert!(dispatched.mutated_local());
        let env = dispatched.envelope().clone();
        assert_eq!(env.actor.as_deref(), Some(caller));
        let context = env.context.as_ref().expect("door supplies provenance");
        assert_eq!(context.action_label, "Star");
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

    fn ambient_task_context_reaches_door_actions_without_overwriting_resource() {
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
        .expect("door action");

        let context = dispatched.envelope().context.as_ref().expect("context");
        assert_eq!(context.action_label, "Star");
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

    /// Consequential actions and values classified destructive need consent, and effective
    /// confirmation reads every argument form.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn consequential_door_actions_need_consent_whatever_form_the_argument_takes() {
        consequential_actions_cannot_cross_the_door_without_consent();
        a_value_classified_as_destructive_needs_the_same_consent();
        effective_confirmation_reads_every_argument_form();
    }

    fn consequential_actions_cannot_cross_the_door_without_consent() {
        let resources = vec![
            Resource::new("notes.note:1", "notes.note", "Harbour").with_action(
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
        assert_eq!(refusal, DoorError::ConfirmationRequired);
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
        .expect("a policy gate may explicitly attest confirmation");
        assert_eq!(
            doc.get(&ROOT, "starred").unwrap().unwrap().0.as_bool(),
            Some(true)
        );
    }

    /// A generic edit whose value decides its risk: clearing the title is
    /// destructive, renaming is not.
    fn edit_by_value() -> Vec<Resource> {
        vec![
            Resource::new("notes.note:1", "notes.note", "Harbour").with_action(
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
                .programmatic()
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
            let door = if confirmed {
                invoke_json_confirmed
            } else {
                invoke_json
            };
            door(
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
            DoorError::ConfirmationRequired
        );
        assert_eq!(
            stored_title(&doc).as_deref(),
            Some("Harbour"),
            "the refused value changed nothing"
        );
        run(&mut doc, "", true).expect("a confirmed destructive value runs");
        assert_eq!(stored_title(&doc).as_deref(), Some(""));

        // The legacy text door classifies the same value the same way.
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
            DoorError::ConfirmationRequired
        );
    }

    /// The context an agent run hands every invocation it makes.
    fn agent_run() -> ActionContext {
        ActionContext::default()
            .with_app("org.tilo.Agent", "Agent")
            .with_purpose("Remove the draft marks")
            .with_group("agent-run:7", "Remove the draft marks", "agent-run")
    }

    /// Read the durable change the door just committed back from its message,
    /// and require it to carry both the resource's own context and the run's.
    fn assert_recorded_within_run(doc: &mut AutoCommit, label: &str) {
        let heads = doc.get_heads();
        let change = doc
            .get_change_by_hash(&heads[0])
            .expect("the door committed");
        assert_eq!(change.actor_id().to_bytes(), b"agent:ada");
        let context = change
            .message()
            .and_then(ActionContext::from_change_message)
            .expect("the change carries its semantic provenance");
        assert_eq!(context.action_label, label);
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("notes.note:1"),
            "the resource stays authoritative"
        );
        let group = context.group.as_ref().expect("the run's group is recorded");
        assert_eq!(group.id, "agent-run:7");
        assert_eq!(group.kind, "agent-run");
        assert_eq!(context.purpose.as_deref(), Some("Remove the draft marks"));
    }

    /// A destructive operation an agent run performs after its policy gate
    /// confirmed it is recorded within the run, exactly like an unconfirmed
    /// one — consent is not a reason to lose whose task the change belongs to.
    #[test]
    fn a_confirmed_invocation_inside_a_run_is_recorded_with_its_group() {
        let resources = vec![
            Resource::new("notes.note:1", "notes.note", "Harbour").with_action(
                ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star)
                    .unwrap()
                    .destructive(),
            ),
        ];
        let run = agent_run();
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
            DoorError::ConfirmationRequired
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
            Some("agent-run:7"),
            "the replicated envelope carries the run too"
        );
        assert_recorded_within_run(&mut doc, "Star");
    }

    /// Both typed-argument entry points record the run's context: the
    /// ordinary value through [`invoke_json`] and the destructive one through
    /// [`invoke_json_confirmed`].
    #[test]
    fn typed_invocations_inside_a_run_are_recorded_with_its_group() {
        let resources = edit_by_value();
        let run = agent_run();
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
        assert_recorded_within_run(&mut doc, "Edit");

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
        assert_recorded_within_run(&mut doc, "Edit");
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

        // The inert contract carries both properties across every boundary.
        assert!(spec.is_programmatic() && spec.contract.is_programmatic());
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
    /// rather than half-parsed into a lookup.
    fn an_address_survives_its_string_form() {
        let address = star();
        assert_eq!(ActionAddress::parse(&address.encode()), Some(address));
        assert_eq!(ActionAddress::parse("notes.note:1"), None);
        assert_eq!(ActionAddress::parse(&format!("{KEY_SEP}star")), None);
    }
}
