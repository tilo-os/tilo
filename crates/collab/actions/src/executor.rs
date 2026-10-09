//! Applying, replaying, and dispatching actions.
//!
//! Three entry points, one commit discipline: [`apply_local`] runs an
//! action on the local replica, minting a fresh envelope (the origin);
//! [`replay`] decodes an envelope's action and applies it, reproducing
//! the *identical* change on any replica at the same base state (peers);
//! [`dispatch`] is the locus-aware front door — `ClientLocal` applies
//! and hands to the transport for replication; `ServerPreferred` submits to
//! an authoritative host and fails closed when none answers.
//!
//! The commit discipline is what makes replay deterministic: every
//! apply pins the Automerge actor id (from the envelope's [`ActionId`])
//! and the commit time, then restores the doc's previous actor. A fresh
//! actor id per action means its single change is `seq == 1` on every
//! replica, so the change hash is content-addressed identical everywhere.

use automerge::ActorId;
use automerge::transaction::{CommitOptions, Transactable};
use tilo_automerge::ApplyIncoming;

use crate::action::{Action, ActionError, ActionId, Doc, EncodedAction, ExecutionLocus};
use crate::context::{ActionContext, ChangeKind};
use crate::entry::{Described, Did};
use crate::envelope::ActionEnvelope;
use crate::registry::ActionRegistry;

thread_local! {
    static INVOKING: std::cell::RefCell<Option<ActionContext>> = const { std::cell::RefCell::new(None) };
    static OPEN_STEP: std::cell::Cell<Option<ActionId>> = const { std::cell::Cell::new(None) };
}

/// Open the undo step the changes committed on this thread join until the
/// next one opens: every action applied meanwhile records it as its
/// [`gesture`](ActionContext::gesture), so the document's history takes them
/// back as one step. The host's history opens one at every step boundary;
/// `None` closes it.
pub fn open_step(step: Option<ActionId>) {
    OPEN_STEP.with(|open| open.set(step));
}

/// The undo step open on this thread, if any (see [`open_step`]).
pub fn current_step() -> Option<ActionId> {
    OPEN_STEP.with(std::cell::Cell::get)
}

/// The context of the action whose [`Action::apply`] is running on this
/// thread: who invoked it and in which work group (an agent run). Pinned by
/// the envelope like the actor and the time, so a replica replaying the
/// action reads the same value. `None` outside an apply, or for an action
/// dispatched with no context.
///
/// An action whose meaning depends on its author — "undo my last step" —
/// reads it here; any other action must not.
pub fn invoking_context() -> Option<ActionContext> {
    INVOKING.with(|current| current.borrow().clone())
}

/// Sets [`invoking_context`] for one apply and restores the outer value.
struct InvokingScope(Option<ActionContext>);

impl InvokingScope {
    fn enter(context: Option<ActionContext>) -> Self {
        Self(INVOKING.with(|current| current.replace(context)))
    }
}

impl Drop for InvokingScope {
    fn drop(&mut self) {
        let outer = self.0.take();
        INVOKING.with(|current| *current.borrow_mut() = outer);
    }
}

/// Build an envelope for `action` at the doc's current heads, without
/// applying — used by [`dispatch`] so a `ServerPreferred` action can be
/// submitted before (or instead of) local apply.
fn envelope_for(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: Option<&[u8]>,
    context: Option<ActionContext>,
) -> Result<ActionEnvelope, ActionError> {
    let base = doc.get_heads();
    // The action declares what it is; a context may only say a content
    // action is nobody's step this time (a record's first-run seed).
    let change = action.change_kind();
    let id = ActionId::new();
    let mut context = context.unwrap_or_default();
    if change != ChangeKind::Content {
        context.change = change;
    }
    context.kind = Some(action.action_kind());
    if context.gesture.is_none() {
        context.gesture = current_step();
    }
    // What history records the step did: described by the value itself from
    // the document before it applies, else as the published action the door
    // resolved, else as the verb its encoded form names.
    let app = context.app.as_ref().map(|app| app.id.clone());
    // The value describes itself as it will apply: under its invoker.
    let own = {
        let _invoking = InvokingScope::enter(Some(context.clone()));
        action.describe(doc)
    };
    let described = own
        .or_else(|| context.described())
        .or_else(|| Described::from_encoded(&encoded, app.as_deref()));
    if let Some(mut described) = described {
        if let Some(app) = app {
            name_app(&mut described.what, &app);
        }
        context = context.with_described(described);
    }
    // Caller-supplied descriptive context cannot choose executable history.
    // Compute it from the actual decoded action at its canonical before state,
    // under the actor the change commits as: what the action mints (the
    // entry a compensation names) depends on it.
    let actor = actor.filter(|a| !a.is_empty()).map(<[u8]>::to_vec);
    let committing = actor
        .clone()
        .map_or_else(|| ActorId::from(id.actor_bytes().to_vec()), ActorId::from);
    let compensation =
        inverse_as(doc, action, committing).map(|undo| crate::context::ActionCompensation {
            undo,
            redo: encoded.clone(),
        });
    if let Some(compensation) = &compensation
        && serde_json::to_vec(compensation)
            .map_err(ActionError::Encode)?
            .len()
            > 128 * 1024
    {
        return Err(ActionError::Refused(
            "document compensation exceeds the history budget".into(),
        ));
    }
    context.compensation = compensation;
    context.executed_action = Some(id);
    let context = Some(context);
    Ok(ActionEnvelope {
        id,
        action: encoded,
        base_heads: base.iter().map(|h| h.0).collect(),
        timestamp: tilo_clock::unix_now_i64(),
        locus: action.locus(),
        actor,
        context,
    })
}

/// The action's [`inverse_encoded`](Action::inverse_encoded) at `doc`, as
/// `actor` would apply it.
fn inverse_as(doc: &mut Doc, action: &dyn Action, actor: ActorId) -> Option<EncodedAction> {
    let previous = doc.get_actor().clone();
    doc.set_actor(actor);
    let inverse = action.inverse_encoded(doc);
    doc.set_actor(previous);
    inverse
}

/// Fill the app of every verb in `what` that does not name one.
fn name_app(what: &mut Did, app: &str) {
    match what {
        Did::Verb(verb) if verb.action.app.is_empty() => app.clone_into(&mut verb.action.app),
        Did::Group(group) => {
            for member in &mut group.members {
                name_app(member, app);
            }
        }
        _ => {}
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
    let applied = {
        let _invoking = InvokingScope::enter(env.context.clone());
        action.apply(doc)
    };
    if let Err(e) = applied {
        abort_pinned_change(doc, prev_actor);
        return Err(e);
    }
    if doc.pending_ops() > 0 {
        // The action produced ops: commit as the pinned actor's single
        // change under the pinned timestamp, content-addressed identical on
        // origin and every replaying replica.
        let mut options = CommitOptions::default().with_time(env.timestamp);
        if let Some(message) = env.context.as_ref().and_then(ActionContext::change_message) {
            options = options.with_message(message);
        }
        doc.commit_with(options);
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
/// optimization nothing in Tilo currently arms on an app document);
/// document content and history are untouched, so replay stays
/// byte-identical. Runs only on this abandon path — a change-producing
/// dispatch keeps its patch log intact.
fn abort_pinned_change(doc: &mut Doc, prev_actor: ActorId) {
    doc.rollback();
    doc.reset_diff_cursor();
    doc.set_actor(prev_actor);
}

/// Apply `action` to the local replica and return its replayable
/// envelope. `encoded` is the serialized form carried to peers — must
/// encode the *same* action.
///
/// # Errors
///
/// Returns whatever [`Action::apply`] returns
/// for `action` — typically [`ActionError::Automerge`] for a document
/// write failure, or [`ActionError::Refused`] if the app declined the action.
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
///
/// # Errors
///
/// See [`apply_local`].
pub fn apply_local_by(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: Option<&[u8]>,
) -> Result<ActionEnvelope, ActionError> {
    let env = envelope_for(doc, action, encoded, actor, None)?;
    apply_under_envelope(doc, action, &env)?;
    Ok(env)
}

/// [`apply_local`], with semantic provenance embedded in the resulting
/// durable change. Use this at framework/action-door boundaries where the
/// resource and human action label are already known.
///
/// # Errors
///
/// See [`apply_local`].
pub fn apply_local_with_context(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: Option<&[u8]>,
    context: ActionContext,
) -> Result<ActionEnvelope, ActionError> {
    let env = envelope_for(doc, action, encoded, actor, Some(context))?;
    apply_under_envelope(doc, action, &env)?;
    Ok(env)
}

/// Replay an envelope received from a peer: decode its action via
/// `registry` and apply under the envelope's pinned actor + timestamp.
/// Applied at the envelope's `base_heads`, this reproduces the identical document.
///
/// # Errors
///
/// Returns [`ActionError::UnknownType`] or [`ActionError::Decode`] if
/// `registry` can't decode `env.action`, or any error [`apply_local`]
/// documents once decoding succeeds.
pub fn replay(
    doc: &mut Doc,
    env: &ActionEnvelope,
    registry: &ActionRegistry,
) -> Result<(), ActionError> {
    let action = registry.decode(&env.action)?;
    let base_heads = env.base_change_hashes();
    if doc.get_heads() == base_heads {
        validate_compensation(doc, action.as_ref(), env)?;
        return apply_under_envelope(doc, action.as_ref(), env);
    }

    // The live replica may already contain concurrent work made after the
    // envelope's authoring point. Applying directly to that newer state makes
    // an action's reads (guards, list positions, expected values, and so on)
    // order-dependent and can produce a different change from the origin.
    // Recreate the action on the exact recorded base, under the envelope's
    // pinned actor/time, then merge that content-addressed change into live.
    // `fork_at` fails closed when the dependencies have not arrived yet; the
    // transport can retry once it has the missing base changes.
    let mut replay_base = doc.fork_at(&base_heads)?;
    validate_compensation(&mut replay_base, action.as_ref(), env)?;
    apply_under_envelope(&mut replay_base, action.as_ref(), env)?;
    doc.merge_incoming(&mut replay_base)?;
    Ok(())
}

/// What replaying a received change showed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verified {
    /// The action the change records reproduces exactly this change.
    Reproduced,
    /// The change records no action to replay (a framework change: a
    /// record's genesis, a checkpoint, a merge).
    NothingRecorded,
    /// The change records an action this replica cannot decode: the app is
    /// not installed here, so another device has to verify it.
    UnknownAction(String),
}

/// The action a change records, encoded as the executor dispatched it.
fn recorded_action(context: &ActionContext) -> Option<&EncodedAction> {
    context
        .compensation
        .as_ref()
        .map(|compensation| &compensation.redo)
}

/// Replay the action the change `hash` records on the document it was
/// authored against, and check that it produces exactly that change: same
/// operations, same actor, time and message, so the same hash. This is what
/// a device that has the app does with every change it receives, beside the
/// relay's grant check: an author granted an action who wrote something else
/// inside its storage units is caught here.
///
/// # Errors
/// Refuses a change that is not in `doc`, whose base is incomplete, or that
/// its recorded action does not reproduce.
pub fn verify_change(
    doc: &mut Doc,
    hash: automerge::ChangeHash,
    registry: &ActionRegistry,
) -> Result<Verified, ActionError> {
    let change = doc
        .get_change_meta_by_hash(&hash)
        .ok_or_else(|| ActionError::Refused("the change is not in the document".into()))?
        .into_owned();
    let Some(context) = change
        .message
        .as_deref()
        .and_then(ActionContext::from_change_message)
    else {
        return Ok(Verified::NothingRecorded);
    };
    let Some(action) = recorded_action(&context).cloned() else {
        return Ok(Verified::NothingRecorded);
    };
    if !registry.contains(&action.type_id) {
        return Ok(Verified::UnknownAction(action.type_id));
    }
    let env = ActionEnvelope {
        id: ActionId::new(),
        action,
        base_heads: change.deps.iter().map(|head| head.0).collect(),
        timestamp: change.timestamp,
        locus: ExecutionLocus::ClientLocal,
        actor: Some(change.actor.to_bytes().to_vec()),
        context: Some(context),
    };
    let mut original = doc.fork_at(&change.deps)?;
    replay(&mut original, &env, registry)?;
    if original.get_heads() != [hash] {
        return Err(ActionError::Refused(
            "the change is not what its recorded action produces".into(),
        ));
    }
    Ok(Verified::Reproduced)
}

/// Decode a recorded inverse only after reproducing its original change.
/// Descriptive CRDT metadata alone cannot install executable history.
///
/// # Errors
/// Refuses missing changes, forged inverses, and changes that their recorded
/// action cannot reproduce at the original dependency heads.
pub fn verified_compensation(
    doc: &mut Doc,
    hash: automerge::ChangeHash,
    registry: &ActionRegistry,
) -> Result<Option<Box<dyn Action>>, ActionError> {
    let inverse = doc
        .get_change_meta_by_hash(&hash)
        .ok_or_else(|| ActionError::Refused("history change is unavailable".into()))?
        .message
        .as_deref()
        .and_then(ActionContext::from_change_message)
        .and_then(|context| context.compensation)
        .map(|compensation| compensation.undo);
    let Some(inverse) = inverse else {
        return Ok(None);
    };
    match verify_change(doc, hash, registry)? {
        Verified::Reproduced => registry.decode(&inverse).map(Some),
        Verified::NothingRecorded | Verified::UnknownAction(_) => Err(ActionError::Refused(
            "recorded history action does not reproduce its change".into(),
        )),
    }
}

fn validate_compensation(
    doc: &mut Doc,
    action: &dyn Action,
    env: &ActionEnvelope,
) -> Result<(), ActionError> {
    let Some(compensation) = env
        .context
        .as_ref()
        .and_then(|context| context.compensation.as_ref())
    else {
        return Ok(());
    };
    if compensation.redo != env.action
        || inverse_as(doc, action, env.actor()).as_ref() != Some(&compensation.undo)
        || serde_json::to_vec(compensation)
            .map_err(ActionError::Encode)?
            .len()
            > 128 * 1024
    {
        return Err(ActionError::Refused(
            "recorded compensation differs from the action's canonical before state".into(),
        ));
    }
    Ok(())
}

/// A host's answer to a `ServerPreferred` submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDecision {
    /// A host accepted the action; it will execute authoritatively and
    /// broadcast the change. The caller must NOT apply locally.
    AcceptedByHost,
    /// No authoritative host is reachable. The caller must not apply locally.
    Unavailable,
}

/// The transport an action is handed to after (or instead of) local
/// execution — the seam between the pure action model and whatever
/// moves bytes (in-process broker, compositor socket, Iroh). The
/// model never imports a transport, only calls this trait.
pub trait ActionTransport {
    /// Replicate a locally-applied action to peers so they can [`replay`] it.
    fn replicate(&mut self, env: &ActionEnvelope);

    /// Submit a `ServerPreferred` action to a host for authoritative
    /// execution. Default is unavailable, so a local-only transport fails
    /// closed for host-authoritative actions.
    fn submit_to_host(&mut self, _env: &ActionEnvelope) -> HostDecision {
        HostDecision::Unavailable
    }
}

/// What [`dispatch`] did with an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dispatched {
    /// `ClientLocal`: applied to the local replica and replicated.
    AppliedLocal(ActionEnvelope),
    /// `ServerPreferred` accepted by a host: NOT applied locally — the host
    /// executes and broadcasts the change.
    SubmittedToHost(ActionEnvelope),
}

impl Dispatched {
    /// The envelope produced, regardless of variant.
    pub fn envelope(&self) -> &ActionEnvelope {
        match self {
            Dispatched::AppliedLocal(e) | Dispatched::SubmittedToHost(e) => e,
        }
    }

    /// Whether the local `doc` was mutated by this dispatch (false only when
    /// a host accepted the action).
    pub fn mutated_local(&self) -> bool {
        !matches!(self, Dispatched::SubmittedToHost(_))
    }
}

/// The locus-aware front door, routing `action` per its
/// [`ExecutionLocus`]: `ClientLocal` →
/// [`apply_local`] then `transport.replicate`; `ServerPreferred` →
/// `transport.submit_to_host`, refusing when no authoritative host is
/// reachable. The envelope's id/timestamp are fixed before submission.
///
/// # Errors
///
/// See [`apply_local`].
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
///
/// # Errors
///
/// See [`apply_local`].
pub fn dispatch_by(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
    actor: Option<&[u8]>,
) -> Result<Dispatched, ActionError> {
    dispatch_by_maybe_context(doc, action, encoded, transport, actor, None)
}

/// [`dispatch`], carrying semantic provenance into the replicated envelope
/// and the committed CRDT change.
///
/// # Errors
///
/// See [`dispatch`].
pub fn dispatch_with_context(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
    context: ActionContext,
) -> Result<Dispatched, ActionError> {
    dispatch_by_maybe_context(doc, action, encoded, transport, None, Some(context))
}

/// Attributed variant of [`dispatch_with_context`].
///
/// # Errors
///
/// See [`dispatch`].
pub fn dispatch_by_with_context(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
    actor: Option<&[u8]>,
    context: ActionContext,
) -> Result<Dispatched, ActionError> {
    dispatch_by_maybe_context(doc, action, encoded, transport, actor, Some(context))
}

fn dispatch_by_maybe_context(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    transport: &mut dyn ActionTransport,
    actor: Option<&[u8]>,
    context: Option<ActionContext>,
) -> Result<Dispatched, ActionError> {
    let context = context.map(|context| context.with_inferred_app(&encoded.type_id));
    match action.locus() {
        ExecutionLocus::ClientLocal => {
            let env = envelope_for(doc, action, encoded, actor, context)?;
            apply_under_envelope(doc, action, &env)?;
            transport.replicate(&env);
            Ok(Dispatched::AppliedLocal(env))
        }
        ExecutionLocus::ServerPreferred => {
            let env = envelope_for(doc, action, encoded, actor, context)?;
            match transport.submit_to_host(&env) {
                HostDecision::AcceptedByHost => Ok(Dispatched::SubmittedToHost(env)),
                HostDecision::Unavailable => Err(ActionError::Refused(
                    "this action requires an authoritative host, but none is reachable".into(),
                )),
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
    // submit_to_host uses the trait default: Unavailable.
}
