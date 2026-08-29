//! Applying, replaying, and dispatching actions.
//!
//! Three entry points, one commit discipline: [`apply_local`] runs an
//! action on the local replica, minting a fresh envelope (the origin);
//! [`replay`] decodes an envelope's action and applies it, reproducing
//! the *identical* change on any replica at the same base state (peers);
//! [`dispatch`] is the locus-aware front door — `ClientLocal` applies
//! and hands to the transport for replication, `ServerPreferred`
//! submits to a host or falls back to local apply when none answers.
//!
//! The commit discipline is what makes replay deterministic: every
//! apply pins the Automerge actor id (from the envelope's [`ActionId`])
//! and the commit time, then restores the doc's previous actor. A fresh
//! actor id per action means its single change is `seq == 1` on every
//! replica, so the change hash is content-addressed identical everywhere.

use automerge::transaction::{CommitOptions, Transactable};

use crate::action::{Action, ActionError, ActionId, Doc, EncodedAction, ExecutionLocus};
use crate::envelope::ActionEnvelope;
use crate::registry::ActionRegistry;

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Build an envelope for `action` at the doc's current heads, without
/// applying — used by [`dispatch`] so a `ServerPreferred` action can be
/// submitted before (or instead of) local apply.
fn envelope_for(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: Option<&[u8]>,
) -> ActionEnvelope {
    let base = doc.get_heads();
    ActionEnvelope {
        id: ActionId::new(),
        action: encoded,
        base_heads: base.iter().map(|h| h.0).collect(),
        timestamp: now_secs(),
        locus: action.locus(),
        actor: actor.filter(|a| !a.is_empty()).map(<[u8]>::to_vec),
    }
}

/// Apply `action` to `doc` under the envelope's pinned actor id +
/// timestamp, then restore the doc's previous actor. The single mutation
/// path shared by [`apply_local`], [`replay`], and [`dispatch`] so
/// origin and peers all commit the byte-identical change.
///
/// # Why a no-op action must not be committed
///
/// Each dispatch pins a *fresh* random actor id. That id only becomes a
/// permanent document actor if the action writes an op — automerge
/// introduces the actor at the transaction's first op and prunes it
/// again if the transaction is empty. But
/// [`AutoCommit::commit_with`](automerge::AutoCommit::commit_with)
/// *always* opens a transaction, so committing a no-op would introduce
/// the fresh actor into the document's actor table, mirror it into the
/// patch-log actor table, then immediately prune it from the document —
/// leaving the patch log referencing an actor the document no longer
/// has. Because pinned ids are random, each sorts into an arbitrary
/// slot, and automerge-0.8's `migrate_actors` (which reconciles the
/// patch log's actor table on the *next* transaction and only knows how
/// to *insert*, never remove) hits that stranded "ghost" and panics with
/// `PatchLogMismatch`. Stacking several dispatches on one document with
/// nothing draining the patch log between them is exactly where this surfaces.
///
/// A no-op action (a change-guarded write whose value already held, a
/// toggle whose precondition was already satisfied) is ordinary and must
/// be safe. We detect the empty result via
/// [`pending_ops`](Transactable::pending_ops) and abandon it without
/// committing — see [`abort_pinned_change`]. Only a change-producing
/// action is committed, under the pinned actor + timestamp, exactly as before.
fn apply_under_envelope(
    doc: &mut Doc,
    action: &dyn Action,
    env: &ActionEnvelope,
) -> Result<(), ActionError> {
    let prev_actor = doc.get_actor().clone();
    // `get_heads`/`set_actor` both close any open transaction, so we
    // start from a clean commit boundary and the action's ops land in
    // their own change under the pinned actor.
    doc.set_actor(env.actor());
    if let Err(e) = action.apply(doc) {
        abort_pinned_change(doc, prev_actor);
        return Err(e);
    }
    if doc.pending_ops() > 0 {
        // The action produced ops: commit as the pinned actor's single
        // change under the pinned timestamp, content-addressed identical on
        // origin and every replaying replica.
        doc.commit_with(CommitOptions::default().with_time(env.timestamp));
        doc.set_actor(prev_actor);
    } else {
        // No ops: a genuine no-op. Do not `commit_with` (it would pin then
        // prune this action's fresh actor and desync the patch log — see the
        // function docs). Leave the document exactly as the action found it.
        abort_pinned_change(doc, prev_actor);
    }
    Ok(())
}

/// Abandon a dispatch that produced no committed change, leaving the
/// document — and the `AutoCommit` patch log — as consistent as a
/// dispatch that never ran. Used for both the no-op result and the
/// mid-apply error path.
///
/// The hazard: if the action opened a transaction under its
/// freshly-pinned actor but that transaction is discarded (an errored
/// write, or a mutation producing no op), automerge prunes the now-unused
/// actor from the document while the patch log still names it — the
/// stranded "ghost" that makes the next apply's `migrate_actors` panic
/// with `PatchLogMismatch`.
/// [`rollback`](automerge::AutoCommit::rollback) discards the open
/// transaction; [`reset_diff_cursor`](automerge::AutoCommit::reset_diff_cursor)
/// then reinstalls a fresh patch log seeded from the document's current
/// actor table, so no ghost can survive either way.
///
/// The reset only discards an *incremental diff index* (a rendering
/// optimization nothing in qOS currently arms on an app document);
/// document content and history are untouched, so replay stays
/// byte-identical. Runs only on this abandon path — a change-producing
/// dispatch keeps its patch log intact.
fn abort_pinned_change(doc: &mut Doc, prev_actor: automerge::ActorId) {
    doc.rollback();
    doc.reset_diff_cursor();
    doc.set_actor(prev_actor);
}

/// Apply `action` to the local replica and return its replayable
/// envelope. `encoded` is the serialized form carried to peers — must
/// encode the *same* action.
pub fn apply_local(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
) -> Result<ActionEnvelope, ActionError> {
    apply_local_by(doc, action, encoded, None)
}

/// [`apply_local`], attributing the resulting change to `actor`: the
/// Automerge change commits under *their* actor so the room's history
/// names them the way it names a typing person. `None` keeps the
/// [`ActionId`]-derived actor, as before.
pub fn apply_local_by(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: Option<&[u8]>,
) -> Result<ActionEnvelope, ActionError> {
    let env = envelope_for(doc, action, encoded, actor);
    apply_under_envelope(doc, action, &env)?;
    Ok(env)
}

/// Replay an envelope received from a peer: decode its action via
/// `registry` and apply under the envelope's pinned actor + timestamp.
/// Applied at the envelope's `base_heads`, this reproduces the identical document.
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
    /// A host accepted the action; it will execute authoritatively and
    /// broadcast the change. The caller must NOT apply locally.
    AcceptedByHost,
    /// No host reachable; the caller applies locally and replicates as if
    /// `ClientLocal`.
    NoHostFallbackLocal,
}

/// The transport an action is handed to after (or instead of) local
/// execution — the seam between the pure action model and whatever
/// moves bytes (in-process broker, compositor socket, Iroh). The
/// model never imports a transport, only calls this trait.
pub trait ActionTransport {
    /// Replicate a locally-applied action to peers so they can [`replay`] it.
    fn replicate(&mut self, env: &ActionEnvelope);

    /// Submit a `ServerPreferred` action to a host for authoritative
    /// execution. Default is "no host", so a local-only transport is a
    /// one-method impl.
    fn submit_to_host(&mut self, _env: &ActionEnvelope) -> HostDecision {
        HostDecision::NoHostFallbackLocal
    }
}

/// What [`dispatch`] did with an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatched {
    /// `ClientLocal`: applied to the local replica and replicated.
    AppliedLocal(ActionEnvelope),
    /// `ServerPreferred` with no host reachable: applied locally as a
    /// fallback and replicated.
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

    /// Whether the local `doc` was mutated by this dispatch (false only when
    /// a host accepted the action).
    pub fn mutated_local(&self) -> bool {
        !matches!(self, Dispatched::SubmittedToHost(_))
    }
}

/// The locus-aware front door, routing `action` per its
/// [`ExecutionLocus`](crate::action::ExecutionLocus): `ClientLocal` →
/// [`apply_local`] then `transport.replicate`; `ServerPreferred` →
/// `transport.submit_to_host`, applying locally + replicating only on
/// [`HostDecision::NoHostFallbackLocal`]. The envelope's id/timestamp are
/// fixed *before* submission, so a host and the local fallback execute
/// the byte-identical change.
pub fn dispatch(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, ActionError> {
    dispatch_by(doc, action, encoded, transport, None)
}

/// [`dispatch`], attributing the resulting change to `actor` — the
/// executor owns the commit boundary, so it's the only place that
/// chooses which actor a change commits under, rather than each app remembering.
pub fn dispatch_by(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
    actor: Option<&[u8]>,
) -> Result<Dispatched, ActionError> {
    match action.locus() {
        ExecutionLocus::ClientLocal => {
            let env = apply_local_by(doc, action, encoded, actor)?;
            transport.replicate(&env);
            Ok(Dispatched::AppliedLocal(env))
        }
        ExecutionLocus::ServerPreferred => {
            let env = envelope_for(doc, action, encoded, actor);
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

/// A transport that only replicates locally (no host); records every
/// replicated envelope for a caller/test to inspect. The default the
/// toolkit installs when a session has no peer/host wiring yet.
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
