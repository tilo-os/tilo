//! Applying, replaying, and dispatching actions.
//!
//! Three entry points, one commit discipline:
//!
//! * [`apply_local`] — run an action on the local replica, minting a fresh
//!   envelope. Used by the origin.
//! * [`replay`] — decode an envelope's action and apply it, reproducing the
//!   *identical* change on any replica at the same base state. Used by peers.
//! * [`dispatch`] — the locus-aware front door: `ClientLocal` → apply + hand to
//!   the transport for replication; `ServerPreferred` → submit to a host, or
//!   fall back to local apply when no host answers.
//!
//! The commit discipline is what makes replay deterministic: every apply pins
//! the Automerge actor id (from the envelope's [`ActionId`]) and the commit time
//! (from the envelope), then restores the doc's previous actor. A fresh actor id
//! per action means its single change is `seq == 1` on every replica, so the
//! change hash is content-addressed identical everywhere.

use automerge::transaction::CommitOptions;

use crate::action::{Action, ActionError, ActionId, EncodedAction, Doc, ExecutionLocus};
use crate::envelope::ActionEnvelope;
use crate::registry::ActionRegistry;

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Build an envelope for `action` at the doc's current heads, without applying.
/// Used by [`dispatch`] so a `ServerPreferred` action can be *submitted* before
/// (or instead of) being applied locally.
fn envelope_for(doc: &mut Doc, action: &dyn Action, encoded: EncodedAction) -> ActionEnvelope {
    let base = doc.get_heads();
    ActionEnvelope {
        id: ActionId::new(),
        action: encoded,
        base_heads: base.iter().map(|h| h.0).collect(),
        timestamp: now_secs(),
        locus: action.locus(),
    }
}

/// Apply `action` to `doc` under the envelope's pinned actor id + timestamp,
/// then restore the doc's previous actor. This is the single mutation path
/// shared by [`apply_local`], [`replay`], and [`dispatch`] so origin and peers
/// all commit the byte-identical change.
fn apply_under_envelope(
    doc: &mut Doc,
    action: &dyn Action,
    env: &ActionEnvelope,
) -> Result<(), ActionError> {
    let prev_actor = doc.get_actor().clone();
    // `get_heads` / `set_actor` both close any open transaction, so we start
    // from a clean commit boundary and the action's ops land in their own
    // change under the pinned actor.
    doc.set_actor(env.actor());
    if let Err(e) = action.apply(doc) {
        doc.rollback();
        doc.set_actor(prev_actor);
        return Err(e);
    }
    doc.commit_with(CommitOptions::default().with_time(env.timestamp));
    doc.set_actor(prev_actor);
    Ok(())
}

/// Apply `action` to the local replica and return its replayable envelope.
/// `encoded` is the serialized form the envelope carries to peers — it must
/// encode the *same* action.
pub fn apply_local(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
) -> Result<ActionEnvelope, ActionError> {
    let env = envelope_for(doc, action, encoded);
    apply_under_envelope(doc, action, &env)?;
    Ok(env)
}

/// Replay an envelope received from a peer: decode its action via `registry`
/// and apply it under the envelope's pinned actor + timestamp. Applied to a
/// replica at the envelope's `base_heads`, this reproduces the identical
/// document.
pub fn replay(
    doc: &mut Doc,
    env: &ActionEnvelope,
    registry: &ActionRegistry,
) -> Result<(), ActionError> {
    let action = registry.decode(&env.action)?;
    apply_under_envelope(doc, action.as_ref(), env)
}

/// A host's answer to a `ServerPreferred` submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDecision {
    /// A host accepted the action; it will execute it authoritatively and
    /// broadcast the resulting change. The caller must NOT apply locally.
    AcceptedByHost,
    /// No host is reachable; the caller should apply the action locally and
    /// replicate it as if it were `ClientLocal`.
    NoHostFallbackLocal,
}

/// The transport an action is handed to after (or instead of) local execution.
///
/// This is the seam between the pure action model and whatever moves bytes: the
/// in-process broker, the compositor's Unix socket, libp2p. The model never
/// imports a transport — it only calls this trait.
pub trait ActionTransport {
    /// Replicate a locally-applied action to peers so they can [`replay`] it.
    fn replicate(&mut self, env: &ActionEnvelope);

    /// Submit a `ServerPreferred` action to a host for authoritative execution.
    /// The default declares "no host" so a transport that only does local
    /// replication is a one-method impl.
    fn submit_to_host(&mut self, _env: &ActionEnvelope) -> HostDecision {
        HostDecision::NoHostFallbackLocal
    }
}

/// What [`dispatch`] did with an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatched {
    /// `ClientLocal`: applied to the local replica and replicated.
    AppliedLocal(ActionEnvelope),
    /// `ServerPreferred` with no host reachable: applied locally as a fallback
    /// and replicated.
    AppliedLocalFallback(ActionEnvelope),
    /// `ServerPreferred` accepted by a host: NOT applied locally — the host
    /// executes and broadcasts the change.
    SubmittedToHost(ActionEnvelope),
}

impl Dispatched {
    /// The envelope produced, regardless of variant.
    pub fn envelope(&self) -> &ActionEnvelope {
        match self {
            Dispatched::AppliedLocal(e)
            | Dispatched::AppliedLocalFallback(e)
            | Dispatched::SubmittedToHost(e) => e,
        }
    }

    /// Whether the local `doc` was mutated by this dispatch (false only when a
    /// host accepted the action).
    pub fn mutated_local(&self) -> bool {
        !matches!(self, Dispatched::SubmittedToHost(_))
    }
}

/// The locus-aware front door. Routes `action` per its
/// [`ExecutionLocus`](crate::action::ExecutionLocus):
///
/// * `ClientLocal` → [`apply_local`] then `transport.replicate`.
/// * `ServerPreferred` → `transport.submit_to_host`; on
///   [`HostDecision::AcceptedByHost`] nothing is applied locally, on
///   [`HostDecision::NoHostFallbackLocal`] it applies locally + replicates.
///
/// The envelope's id/timestamp are fixed *before* submission, so a host and the
/// local fallback would execute the byte-identical change.
pub fn dispatch(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ActionError> {
    match action.locus() {
        ExecutionLocus::ClientLocal => {
            let env = apply_local(doc, action, encoded)?;
            transport.replicate(&env);
            Ok(Dispatched::AppliedLocal(env))
        }
        ExecutionLocus::ServerPreferred => {
            let env = envelope_for(doc, action, encoded);
            match transport.submit_to_host(&env) {
                HostDecision::AcceptedByHost => Ok(Dispatched::SubmittedToHost(env)),
                HostDecision::NoHostFallbackLocal => {
                    apply_under_envelope(doc, action, &env)?;
                    transport.replicate(&env);
                    Ok(Dispatched::AppliedLocalFallback(env))
                }
            }
        }
    }
}

/// A transport that only replicates locally (no host). Records every replicated
/// envelope so a caller (or a test) can inspect / forward them. This is the
/// default the toolkit installs when a session has no peer/host wiring yet.
#[derive(Default)]
pub struct LocalTransport {
    pub replicated: Vec<ActionEnvelope>,
}

impl LocalTransport {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ActionTransport for LocalTransport {
    fn replicate(&mut self, env: &ActionEnvelope) {
        self.replicated.push(env.clone());
    }
    // submit_to_host uses the trait default: NoHostFallbackLocal.
}
