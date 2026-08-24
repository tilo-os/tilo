//! **The door**: how a participant that is not the app enumerates what the app
//! has open, and performs one of the operations it exports.
//!
//! [`Resource`] and [`ActionSpec`] already describe an app's surface, and
//! [`dispatch`](crate::executor::dispatch) already performs one operation on
//! it. What was missing is the step between them — a way for *someone other
//! than the app* to name a thing and ask for it. Without that the surface is
//! reachable only from inside the process that built it, which makes
//! collaboration, agency and scripting per-app features again.
//!
//! This module is that step, and it is deliberately small: it turns a live
//! resource tree into **inert descriptors** with stable addresses, and turns an
//! address plus an argument back into the [`EncodedAction`] the executor
//! already knows how to run. It owns no socket and no policy. The transport is
//! whatever the host already has, and the capability check belongs to the host
//! too — see "What this module does not do" below.
//!
//! ## Two halves, and why they are separate
//!
//! * [`survey`] answers *what can be done here*. It is a pure function of the
//!   resource tree: it borrows, copies labels and ids, and returns data. There
//!   is no callback, no token and no handle anywhere in an [`ActionDescriptor`],
//!   so surveying cannot cause an effect even by mistake. Read-only is a
//!   property of the returned type's shape, not a promise in a comment.
//! * [`invoke`] performs one. It re-resolves the address against the tree it is
//!   given, binds the argument, decodes through the app's own
//!   [`ActionRegistry`] and dispatches — attributing the change to the caller.
//!
//! Collapsing them would make every enumeration a potential side effect, so a
//! caller exploring an unfamiliar app could not explore it safely.
//!
//! ## Addresses are stable and machine-readable
//!
//! An [`ActionAddress`] is `(resource id, action id)` — both minted by the app,
//! both stable for as long as the app keeps publishing that resource. No screen
//! position, no coordinate, no index into a rendered list. A caller with no
//! display can work entirely from ids and labels, and the same address keeps
//! naming the same operation across window layouts, themes and resolutions.
//!
//! ## Attribution
//!
//! [`invoke`] takes the caller's actor bytes and hands them to
//! [`dispatch_by`](crate::executor::dispatch_by), so the Automerge change
//! commits under *that participant*. An edit made through the door is
//! attributed in history exactly like an edit someone typed — which is the
//! point: a participant reaching an app from outside is still a participant,
//! not an anonymous mutation.
//!
//! ## What this module does not do
//!
//! It does **not** decide whether the caller may act. Authority is the host's:
//! in qOS the shell resolves the caller against the workspace's capability
//! model before an address is ever offered, so an operation the caller may not
//! perform is *absent* from the survey and its address resolves to nothing.
//! Keeping the check out of here is what stops it becoming advisory — there is
//! no "allowed" flag on a descriptor for a caller to ignore, because a denied
//! operation never becomes a descriptor.
//!
//! See `docs/action-model-recipe.md` for what an app must implement to be
//! reachable through it.

use crate::action::{EncodedAction, ExecutionLocus};
use crate::executor::{dispatch_by, ActionTransport, Dispatched};
use crate::registry::ActionRegistry;
use crate::resource::{ActionInput, ActionSpec, Resource, KEY_SEP};

/// The stable address of one exported operation: which resource, and which of
/// that resource's actions.
///
/// Both halves are the app's own ids. The string form joins them with
/// [`KEY_SEP`], a unit separator that never appears in either, so
/// [`ActionAddress::parse`] is unambiguous.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

    /// The single-string form, for transports that carry one id.
    pub fn encode(&self) -> String {
        format!("{}{}{}", self.resource, KEY_SEP, self.action)
    }

    /// Recover an address from [`encode`](Self::encode). `None` when the string
    /// is not one — a caller that made an address up gets a refusal here rather
    /// than a lookup against a half-parsed id.
    pub fn parse(s: &str) -> Option<Self> {
        let (resource, action) = s.split_once(KEY_SEP)?;
        if resource.is_empty() || action.is_empty() {
            return None;
        }
        Some(Self::new(resource, action))
    }
}

/// What an invocation of one action must carry.
///
/// Advertised on the descriptor rather than discovered by trying, because
/// enumeration is the only thing a caller that cannot see the screen has to
/// work from: an argument that was not described could only be guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputSpec {
    /// Takes nothing. The majority.
    None,
    /// Takes one free-text argument. `prompt` and `placeholder` are the app's
    /// own words for it — the same ones the palette would show a person.
    Text { prompt: String, placeholder: String },
}

impl InputSpec {
    fn of(input: &ActionInput) -> Self {
        match input {
            ActionInput::Immediate(_) => InputSpec::None,
            ActionInput::Prompted {
                prompt,
                placeholder,
                ..
            } => InputSpec::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
            },
        }
    }

    /// Whether an invocation must supply text.
    pub fn wants_text(&self) -> bool {
        matches!(self, InputSpec::Text { .. })
    }
}

/// One exported operation, as inert data.
///
/// Everything a caller needs to decide whether to perform it, and nothing that
/// could perform it. The presentation half of an [`ActionSpec`] — its closure —
/// never appears here and never crosses a boundary; only the address does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub address: ActionAddress,
    /// The publishing resource's stable kind tag (`word_processor.section`,
    /// `files.entry`). Shared with the OS-wide type registry, so the same kind
    /// means the same thing across apps.
    pub resource_kind: String,
    /// The resource's human label, as the app words it.
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub locus: ExecutionLocus,
    pub input: InputSpec,
}

/// Why an address could not be performed.
///
/// `Unknown` deliberately covers both "no such address" and "not offered to
/// you": a caller must not be able to map what it may not reach by probing for
/// a different error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoorError {
    /// Nothing in the surveyed tree carries this address.
    Unknown(ActionAddress),
    /// The action takes text and none was supplied.
    MissingArgument(ActionAddress),
    /// The action takes nothing and an argument was supplied. Refused rather
    /// than dropped — a silently ignored argument means the caller believes it
    /// asked for something it did not get.
    UnexpectedArgument(ActionAddress),
    /// The app never registered a decoder for the action's type, so the door
    /// cannot prove what it would run. Fail-closed: an unregistered type is a
    /// refusal, not a best-effort apply.
    UnregisteredType(String),
    /// The action ran and failed.
    Failed(String),
}

impl DoorError {
    /// Stable machine-readable code, so a caller can tell "I addressed this
    /// wrongly" from "it ran and could not do its job".
    pub fn code(&self) -> &'static str {
        match self {
            DoorError::Unknown(_) => "unknown_action",
            DoorError::MissingArgument(_) | DoorError::UnexpectedArgument(_) => "invalid_arguments",
            DoorError::UnregisteredType(_) => "unknown_action",
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
            DoorError::UnregisteredType(t) => {
                write!(f, "no decoder registered for action type `{t}`")
            }
            DoorError::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// Walk a resource forest and describe every operation it exports.
///
/// Depth-first, parents before their children — the order
/// [`flatten`](crate::resource::flatten) already establishes, so a caller sees
/// the document before its sections and the folder before its files.
///
/// Takes `&[Resource]` and returns owned data: it cannot mutate the tree, and
/// nothing it returns can act on it.
pub fn survey(resources: &[Resource]) -> Vec<ActionDescriptor> {
    let mut out = Vec::new();
    for r in resources {
        survey_into(r, &mut out);
    }
    out
}

fn survey_into(resource: &Resource, out: &mut Vec<ActionDescriptor>) {
    for action in &resource.actions {
        out.push(ActionDescriptor {
            address: ActionAddress::new(resource.id.as_str(), &action.id),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action.label.clone(),
            description: action.description.clone(),
            locus: action.locus,
            input: InputSpec::of(&action.input),
        });
    }
    for child in &resource.children {
        survey_into(child, out);
    }
}

/// Resolve `address` against `resources`, without running anything.
///
/// The inspection step: a caller can check what an address currently names
/// before committing to it. `None` when nothing carries the address.
pub fn resolve<'a>(resources: &'a [Resource], address: &ActionAddress) -> Option<&'a ActionSpec> {
    fn walk<'a>(r: &'a Resource, address: &ActionAddress) -> Option<&'a ActionSpec> {
        if r.id.as_str() == address.resource {
            if let Some(spec) = r.actions.iter().find(|a| a.id == address.action) {
                return Some(spec);
            }
        }
        r.children.iter().find_map(|c| walk(c, address))
    }
    resources.iter().find_map(|r| walk(r, address))
}

/// Turn an address plus its argument into the replayable action it names.
///
/// Split out from [`invoke`] so a host can build the encoded action, inspect or
/// route it, and dispatch it itself — and so the argument rules are checked in
/// exactly one place. Binding happens **before** anything runs, so a call that
/// does not fit is a refusal with the document untouched, never a half-applied
/// edit.
pub fn encode(
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<EncodedAction, DoorError> {
    let spec = resolve(resources, address).ok_or_else(|| DoorError::Unknown(address.clone()))?;
    match (&spec.input, argument) {
        (ActionInput::Immediate(encoded), None) => Ok(encoded.clone()),
        (ActionInput::Immediate(_), Some(_)) => Err(DoorError::UnexpectedArgument(address.clone())),
        (ActionInput::Prompted { build, .. }, Some(text)) => Ok(build(text)),
        (ActionInput::Prompted { .. }, None) => Err(DoorError::MissingArgument(address.clone())),
    }
}

/// Perform the operation `address` names, attributed to `actor`.
///
/// The whole door in one call: resolve, bind, decode through the app's own
/// registry, then [`dispatch_by`] honouring the action's locus. `actor` is the
/// caller's Automerge actor bytes; the resulting change commits under it, so
/// the room's history names who asked.
///
/// Decoding through `registry` rather than trusting the tree is what makes this
/// fail-closed: an action whose type the app never registered cannot be run,
/// and the same decoder a replaying peer would use is the one that reconstructs
/// it here.
pub fn invoke(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
    actor: &[u8],
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    let encoded = encode(resources, address, argument)?;
    let action = registry
        .decode(&encoded)
        .map_err(|_| DoorError::UnregisteredType(encoded.type_id.clone()))?;
    dispatch_by(doc, action.as_ref(), encoded, transport, Some(actor))
        .map_err(|e| DoorError::Failed(e.to_string()))
}
