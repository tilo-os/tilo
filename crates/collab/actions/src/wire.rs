//! Getting the action surface — and the actions themselves — across a process
//! boundary.
//!
//! Two halves, and they are deliberately separate:
//!
//! * **The projection.** [`project`] turns the [`Resource`] forest an app
//!   publishes into [`SerializableResource`]s / [`FlatSpec`]s: pure data, no
//!   closures, no handles. It answers *"what can I do here, and what must I
//!   pass"* for a caller that has never seen the window. Read-only by
//!   construction — a projection carries nothing fireable, so surveying can
//!   never trip a side-effect.
//! * **The transport.** [`IoTransport`] is an [`ActionTransport`] that writes
//!   [`WireMessage`]s to any [`Write`] and reads host decisions from any
//!   [`BufRead`], so the same dispatch that used to drop envelopes into a `Vec`
//!   now moves them down a Unix socket, a pipe, or a TCP stream.
//!
//! ## Wire format
//!
//! Newline-delimited JSON, one [`WireMessage`] per line. That is what the
//! compositor's control socket already speaks, so a host that multiplexes both
//! needs no second framing, and a human debugging a session can read the
//! traffic. [`ActionEnvelope`] was already `Serialize`; nothing about the action
//! model had to change to make it shippable — what was missing was only ever
//! something to ship it *down*.
//!
//! ## Invoking from the projection
//!
//! [`build_from_flat`] closes the loop: given the live resource forest and a
//! `(key, args)` pair a caller read off a projection, it resolves the spec,
//! binds the arguments strictly, and returns the [`EncodedAction`] — the *same*
//! one the on-screen affordance builds, because it runs the same closure. The
//! caller then hands that to the ordinary executor, so an external invocation
//! replays, replicates, and undoes exactly like a local one.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::action::{ActionId, EncodedAction, ExecutionLocus};
use crate::envelope::ActionEnvelope;
use crate::executor::{ActionTransport, HostDecision};
use crate::params::{ActionParam, ArgError, ArgValue};
use crate::resource::{ActionSpec, KEY_SEP, Resource, flatten};

/// The serializable shadow of one [`ActionSpec`]: everything about it except
/// the closure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerializableSpec {
    /// Stable id within its resource (`"like"`, `"set-opacity"`).
    pub action_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub locus: ExecutionLocus,
    /// What an invocation must carry. Empty for the nullary majority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ActionParam>,
}

impl SerializableSpec {
    /// Project one spec.
    pub fn of(spec: &ActionSpec) -> Self {
        Self {
            action_id: spec.id.clone(),
            label: spec.label.clone(),
            description: spec.description.clone(),
            locus: spec.locus,
            params: spec.params().to_vec(),
        }
    }
}

/// The serializable shadow of one [`Resource`], nesting exactly as the live
/// tree does, so a caller can see *what owns what* rather than a flat list of
/// verbs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerializableResource {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<SerializableSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SerializableResource>,
}

impl SerializableResource {
    /// Project one resource and everything under it.
    pub fn of(resource: &Resource) -> Self {
        Self {
            id: resource.id.0.clone(),
            kind: resource.kind.clone(),
            label: resource.label.clone(),
            actions: resource.actions.iter().map(SerializableSpec::of).collect(),
            children: resource.children.iter().map(Self::of).collect(),
        }
    }
}

/// One `(resource, action)` pair, addressed by the same composite
/// [`key`](FlatSpec::key) the palette uses for its rows — so the address a
/// caller invokes is the address the palette shows, not a parallel naming
/// scheme.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatSpec {
    /// `"<resource_id>\u{1f}<action_id>"`.
    pub key: String,
    pub resource_id: String,
    pub resource_kind: String,
    pub resource_label: String,
    #[serde(flatten)]
    pub spec: SerializableSpec,
}

/// Project a resource forest to its nested serializable form.
pub fn project(resources: &[Resource]) -> Vec<SerializableResource> {
    resources.iter().map(SerializableResource::of).collect()
}

/// Project a resource forest to the flat, addressable action list — the
/// enumeration a headless caller works from.
pub fn project_flat(resources: &[Resource]) -> Vec<FlatSpec> {
    flatten(resources)
        .into_iter()
        .map(|fa| FlatSpec {
            key: fa.key,
            resource_id: fa.resource_id.0,
            resource_kind: fa.resource_kind,
            resource_label: fa.resource_label,
            spec: SerializableSpec::of(fa.action),
        })
        .collect()
}

/// Resolve a composite `key` back to the live spec in `resources`.
///
/// `None` when the key is not in this snapshot — an app's resource tree is
/// rebuilt per frame, so a stale key must be refused rather than matched to
/// whatever now sits at that position.
pub fn resolve<'a>(resources: &'a [Resource], key: &str) -> Option<&'a ActionSpec> {
    let (resource_id, action_id) = key.split_once(KEY_SEP)?;
    fn walk<'a>(r: &'a Resource, rid: &str, aid: &str) -> Option<&'a ActionSpec> {
        if r.id.0 == rid {
            if let Some(spec) = r.actions.iter().find(|a| a.id == aid) {
                return Some(spec);
            }
        }
        r.children.iter().find_map(|c| walk(c, rid, aid))
    }
    resources
        .iter()
        .find_map(|r| walk(r, resource_id, action_id))
}

/// Why an invocation addressed from a projection did not produce an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvokeError {
    /// No action lives at that key in the current resource forest.
    UnknownAction { key: String },
    /// The key was good; the arguments were not. Nothing was built, so nothing
    /// could have been applied.
    Arguments(ArgError),
    /// The address and the arguments were both good, but the action could not
    /// be carried out — its type is not registered here, or applying it failed.
    Refused(String),
}

impl InvokeError {
    /// Stable machine-readable code. A caller fixes "I addressed something that
    /// isn't there", "I addressed the right thing wrongly", and "it was right
    /// but it did not work" differently, so they must not arrive as one
    /// failure.
    pub fn code(&self) -> &'static str {
        match self {
            InvokeError::UnknownAction { .. } => "unknown_action",
            InvokeError::Arguments(_) => "invalid_arguments",
            InvokeError::Refused(_) => "action_failed",
        }
    }
}

impl std::fmt::Display for InvokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvokeError::UnknownAction { key } => {
                write!(f, "no action addressed by `{key}` is currently exported")
            }
            InvokeError::Arguments(e) => write!(f, "{e}"),
            InvokeError::Refused(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for InvokeError {}

/// Build the action a caller addressed from a projection.
///
/// The whole round trip in one call: resolve `key` in the live forest, bind
/// `args` strictly against the parameters the projection advertised, and run
/// the app's own builder. The result is byte-identical to what the on-screen
/// affordance produces, because it *is* the affordance's closure.
pub fn build_from_flat(
    resources: &[Resource],
    key: &str,
    args: impl IntoIterator<Item = (String, ArgValue)>,
) -> Result<EncodedAction, InvokeError> {
    let spec = resolve(resources, key).ok_or_else(|| InvokeError::UnknownAction {
        key: key.to_string(),
    })?;
    spec.build_args(args).map_err(InvokeError::Arguments)
}

/// [`build_from_flat`] for arguments that arrived as a JSON object.
pub fn build_from_json(
    resources: &[Resource],
    key: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<EncodedAction, InvokeError> {
    let spec = resolve(resources, key).ok_or_else(|| InvokeError::UnknownAction {
        key: key.to_string(),
    })?;
    spec.build_json(args).map_err(InvokeError::Arguments)
}

/// One framed message on the action wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireMessage {
    /// A locally-applied action, sent so peers can [`replay`](crate::replay) it.
    Replicate(ActionEnvelope),
    /// A `ServerPreferred` action offered to a host for authoritative
    /// execution.
    Submit(ActionEnvelope),
    /// A host's answer to a [`Submit`](Self::Submit), naming the envelope it
    /// answers so a decision can never be mistaken for the reply to a different
    /// submission.
    Decision { id: ActionId, accepted: bool },
}

impl WireMessage {
    /// Encode as one newline-terminated JSON line.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).expect("a wire message is always serializable");
        s.push('\n');
        s
    }

    /// Decode one JSON line.
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Read the next [`WireMessage`] from a framed stream.
///
/// `Ok(None)` is a clean end of stream. Blank lines are skipped so a writer may
/// flush padding without desynchronising the reader.
pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<WireMessage>> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            continue;
        }
        return WireMessage::from_line(line.trim())
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
    }
}

/// An [`ActionTransport`] that carries envelopes across a real boundary.
///
/// Generic over [`Write`] + [`BufRead`] rather than owning a socket, so the
/// same transport serves a `UnixStream`, a pipe pair, a TCP stream, or an
/// in-memory buffer — the model still imports no transport, it only writes
/// bytes someone else routes.
///
/// [`replicate`](ActionTransport::replicate) writes and returns.
/// [`submit_to_host`](ActionTransport::submit_to_host) writes and then blocks
/// for the matching [`Decision`](WireMessage::Decision); anything else — no
/// host at the far end, a closed stream, an I/O error, a decision for a
/// different envelope — is
/// [`NoHostFallbackLocal`](HostDecision::NoHostFallbackLocal), so a broken
/// wire degrades to the behaviour that never blocks the user rather than
/// losing the action.
pub struct IoTransport<W: Write, R: BufRead> {
    out: W,
    inp: R,
    /// Every envelope written, in order. Lets a caller (or a test) audit what
    /// actually crossed, the way [`LocalTransport`](crate::LocalTransport)
    /// does.
    pub sent: Vec<ActionEnvelope>,
    /// The last I/O error, if the wire broke. A failed write is not silently
    /// successful: dispatch still falls back to a local apply, and this says
    /// why.
    pub last_error: Option<String>,
}

impl<W: Write, R: BufRead> IoTransport<W, R> {
    pub fn new(out: W, inp: R) -> Self {
        Self {
            out,
            inp,
            sent: Vec::new(),
            last_error: None,
        }
    }

    fn write(&mut self, msg: &WireMessage) -> bool {
        match self
            .out
            .write_all(msg.to_line().as_bytes())
            .and_then(|_| self.out.flush())
        {
            Ok(()) => true,
            Err(e) => {
                self.last_error = Some(e.to_string());
                false
            }
        }
    }
}

impl<W: Write, R: BufRead> ActionTransport for IoTransport<W, R> {
    fn replicate(&mut self, env: &ActionEnvelope) {
        if self.write(&WireMessage::Replicate(env.clone())) {
            self.sent.push(env.clone());
        }
    }

    fn submit_to_host(&mut self, env: &ActionEnvelope) -> HostDecision {
        if !self.write(&WireMessage::Submit(env.clone())) {
            return HostDecision::NoHostFallbackLocal;
        }
        self.sent.push(env.clone());
        match read_message(&mut self.inp) {
            Ok(Some(WireMessage::Decision { id, accepted })) if id == env.id && accepted => {
                HostDecision::AcceptedByHost
            }
            Ok(_) => HostDecision::NoHostFallbackLocal,
            Err(e) => {
                self.last_error = Some(e.to_string());
                HostDecision::NoHostFallbackLocal
            }
        }
    }
}

/// The parameters a projected spec advertised, as a caller reads them back off
/// the wire. Convenience for a client that deserialized a [`FlatSpec`] and now
/// wants to check its own arguments before sending them.
pub fn params_of(spec: &SerializableSpec) -> &[ActionParam] {
    &spec.params
}
