//! Applying, replaying, and dispatching actions.
//!
//! Three entry points, one commit discipline: [`apply_local`] runs an
//! action on the local replica, minting a fresh envelope (the origin);
//! [`replay`] decodes an envelope's action and applies it, reproducing
//! the *identical* change on any replica at the same base state (peers);
//! [`dispatch`] is the locus-aware front API — `ClientLocal` applies
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

use crate::action::{Action, ActionError, ActionId, Doc, EncodedAction, ExecutionLocus};
use crate::context::{ActionContext, CONTINUATION_PREFIX, ChangeKind};
use crate::entry::{Described, Did};
use crate::envelope::ActionEnvelope;
use crate::registry::ActionRegistry;

thread_local! {
    static REPLAYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INVOKING: std::cell::RefCell<Option<ActionContext>> = const { std::cell::RefCell::new(None) };
    static OPEN_STEP: std::cell::Cell<Option<ActionId>> = const { std::cell::Cell::new(None) };
}

/// Whether the executor is reproducing an existing action rather than
/// admitting a new invocation. Replay must reproduce document writes without
/// issuing host-local approvals or other outside effects again.
pub fn is_replaying() -> bool {
    REPLAYING.with(std::cell::Cell::get)
}

struct ReplayScope(bool);

impl ReplayScope {
    fn enter() -> Self {
        Self(REPLAYING.with(|replaying| replaying.replace(true)))
    }
}

impl Drop for ReplayScope {
    fn drop(&mut self) {
        REPLAYING.with(|replaying| replaying.set(self.0));
    }
}

/// Call `work` with [`is_replaying`] saying `replaying`: an app's model module
/// applies its verb as the device that called it does, reproducing or
/// admitting.
pub fn replaying_as<T>(replaying: bool, work: impl FnOnce() -> T) -> T {
    let outer = REPLAYING.with(|current| current.replace(replaying));
    let _restore = ReplayScope(outer);
    work()
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

/// The document key prefix of a step's undo marker, followed by the step's
/// change hash: `true` once taken back, a negative attempt count while its
/// undo waits for an outside consequence to settle, a positive one while its
/// redo does.
pub const UNDONE_KEY: &str = "tilo:undone:";

thread_local! {
    /// The steps the action applying now undoes or redoes, and whether it
    /// queued an outside consequence that settles them.
    static COMPENSATING: std::cell::RefCell<Option<(Vec<String>, bool)>> =
        const { std::cell::RefCell::new(None) };
}

/// The undo or redo of the steps `marks` (their change hashes) applying its
/// compensating action on this thread: an outside consequence the action
/// queues reads them ([`await_compensation`]), and the step counts as taken
/// back (or brought back) only once that consequence settles
/// ([`settle_compensation`]).
pub struct CompensatingScope(Option<(Vec<String>, bool)>);

impl CompensatingScope {
    #[must_use]
    pub fn enter(marks: Vec<String>) -> Self {
        Self(COMPENSATING.with(|current| current.replace(Some((marks, false)))))
    }

    /// Whether the action queued an outside consequence that will settle
    /// the steps.
    #[must_use]
    pub fn awaited(&self) -> bool {
        COMPENSATING.with(|current| {
            current
                .borrow()
                .as_ref()
                .is_some_and(|(_, awaited)| *awaited)
        })
    }
}

impl Drop for CompensatingScope {
    fn drop(&mut self) {
        COMPENSATING.with(|current| *current.borrow_mut() = self.0.take());
    }
}

/// The steps an outside consequence queued now settles, recording that
/// their undo or redo waits for it; empty outside a compensation.
#[must_use]
pub fn await_compensation() -> Vec<String> {
    COMPENSATING.with(|current| {
        current
            .borrow_mut()
            .as_mut()
            .map(|(marks, awaited)| {
                *awaited = true;
                marks.clone()
            })
            .unwrap_or_default()
    })
}

/// Mark the step `hash` as waiting for the outside consequence its undo (or,
/// with `redo`, its redo) queued: the next attempt's marker, negative for an
/// undo, positive for a redo. The step stays as it was until
/// [`settle_compensation`] completes it.
pub fn await_settlement(doc: &mut Doc, hash: &str, redo: bool) -> Result<(), ActionError> {
    use automerge::ReadDoc as _;
    let key = format!("{UNDONE_KEY}{hash}");
    let before = doc
        .get(automerge::ROOT, &key)?
        .and_then(|(value, _)| value.as_i64())
        .filter(|attempt| (*attempt > 0) == redo && *attempt != 0)
        .unwrap_or(0);
    let attempt = if redo {
        before.checked_add(1)
    } else {
        before.checked_sub(1)
    }
    .ok_or_else(|| ActionError::Refused("settlement attempts exceed the budget".into()))?;
    doc.put(automerge::ROOT, key, attempt)?;
    Ok(())
}

/// Mark the undo (or redo) of the step `hash` complete: its outside
/// consequence settled.
pub fn settle_compensation(doc: &mut Doc, hash: &str) -> Result<(), ActionError> {
    use automerge::ReadDoc as _;
    let key = format!("{UNDONE_KEY}{hash}");
    let pending = doc
        .get(automerge::ROOT, &key)?
        .and_then(|(value, _)| value.as_i64())
        .filter(|attempt| *attempt != 0)
        .ok_or_else(|| ActionError::Refused("history compensation is not pending".into()))?;
    if pending < 0 {
        doc.put(automerge::ROOT, key, true)?;
    } else {
        doc.delete(automerge::ROOT, key)?;
    }
    Ok(())
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

/// What `read` makes of the [`invoking_context`], without copying it: it
/// carries the action's arguments, which may be large.
pub fn reading_invoking_context<T>(read: impl FnOnce(Option<&ActionContext>) -> T) -> T {
    INVOKING.with(|current| read(current.borrow().as_ref()))
}

/// Call `work` with `context` as the [`invoking_context`]: an operation the
/// app carries out itself (a write into a part of a chain) keeps the agent
/// session and purpose its invocation came with.
pub fn with_invoking_context<T>(context: Option<ActionContext>, work: impl FnOnce() -> T) -> T {
    let _invoking = InvokingScope::enter(context);
    work()
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

/// Where an envelope reads what the action did from.
enum Before {
    /// The document before the action applies: the description and the
    /// compensation are read from it.
    Doc,
    /// The document with the action's writes as its open transaction: the
    /// description is the one read before its first write, and it records no
    /// compensation, which only the state before it could compute.
    Written(Option<Box<Described>>),
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
    before: Before,
) -> Result<ActionEnvelope, ActionError> {
    let pinned = (ActionId::new(), tilo_clock::unix_now_i64());
    envelope_pinned(doc, action, encoded, actor, context, before, pinned)
}

/// The envelope [`apply_local_with_context`] writes for `action` at the
/// doc's current heads, under the invocation id and origin time (seconds
/// since the Unix epoch) the caller pins rather than fresh ones: a device
/// applying an app's verb from its package mints both from the request, so
/// every holder given the same request writes the same change.
///
/// # Errors
///
/// See [`apply_local`].
pub fn envelope_at(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    (actor, context): (&[u8], ActionContext),
    pinned: (ActionId, i64),
) -> Result<ActionEnvelope, ActionError> {
    envelope_pinned(
        doc,
        action,
        encoded,
        Some(actor),
        Some(context),
        Before::Doc,
        pinned,
    )
}

fn envelope_pinned(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: Option<&[u8]>,
    context: Option<ActionContext>,
    before: Before,
    (id, timestamp): (ActionId, i64),
) -> Result<ActionEnvelope, ActionError> {
    let base = match before {
        Before::Doc => doc.get_heads(),
        Before::Written(_) => doc.committed_heads(),
    };
    // The action declares what it is; a context may only say a content
    // action is nobody's step this time (a record's first-run seed).
    let change = action.change_kind();
    let mut context = context.unwrap_or_default();
    if change != ChangeKind::Content {
        context.change = change;
    }
    context.kind = Some(action.action_kind());
    if action.incoming_message()
        && !context
            .facets
            .iter()
            .any(|facet| facet.key == crate::INCOMING_MESSAGE_FACET)
    {
        context.facets.push(crate::ActionFacet::new(
            crate::INCOMING_MESSAGE_FACET,
            "true",
        ));
    }
    if context.gesture.is_none() {
        context.gesture = current_step();
    }
    // What history records the step did: described by the value itself from
    // the document before it applies, else as the published action the API
    // resolved, else as the verb its encoded form names.
    let app = context.app.as_ref().map(|app| app.id.clone());
    // The value describes itself as it will apply: under its invoker.
    let (own, written) = match before {
        Before::Doc => {
            let _invoking = InvokingScope::enter(Some(context.clone()));
            (action.describe(doc), false)
        }
        Before::Written(described) => (described.map(|described| *described), true),
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
    let compensation = if written {
        None
    } else {
        inverse_as(doc, action, committing)
    };
    // A person setting the same item again with the same verb, right after
    // they last did, continues that run: undo takes it back and History
    // lists it as one ([`crate::run`]).
    if compensation.is_none()
        && let Some(actor) = actor.as_deref()
        && let Some(gesture) = crate::run::continued_gesture(doc, actor, &context, timestamp)
    {
        context.gesture = Some(gesture);
    }
    // Only what undo adds is bounded hard: the action itself is recorded
    // when it fits the budget, and travels in the envelope anyway.
    if let Some(compensation) = &compensation
        && serde_json::to_vec(compensation)
            .map_err(ActionError::Encode)?
            .len()
            > crate::context::COMPENSATION_BUDGET
    {
        return Err(ActionError::Refused(
            "document compensation exceeds the history budget".into(),
        ));
    }
    context.action = Some(recorded_form(&encoded)?);
    context.compensation = compensation;
    let context = Some(context);
    Ok(ActionEnvelope {
        id,
        action: encoded,
        base_heads: base.iter().map(|h| h.0).collect(),
        timestamp,
        locus: action.locus(),
        actor,
        context,
    })
}

/// The action's [`inverse_encoded`](Action::inverse_encoded) at `doc`, as
/// the change written by `actor` records it: what the action mints depends
/// on its writer.
pub fn inverse_as(doc: &mut Doc, action: &dyn Action, actor: ActorId) -> Option<EncodedAction> {
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
    content: Option<&tilo_content::ContentStore>,
) -> Result<Option<crate::entry::ActionItem>, ActionError> {
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
    let mut made = None;
    if doc.pending_ops() > 0 {
        // The action produced ops: commit as the pinned actor's single
        // change under the pinned timestamp, content-addressed identical on
        // origin and every replaying replica. A step that said what it did
        // but named no item made one the document before it lacked: the item
        // it made is what the change is about, and is read now that it is
        // written. Only the origin reads it: a replica replays the change
        // under the entry it was recorded with, so a change older than this
        // rule keeps the words it was written with.
        made = env
            .context
            .as_ref()
            .filter(|context| !is_replaying() && context.what.is_some() && context.item.is_none())
            .and_then(|_| action.made(doc));
        commit_pinned(doc, env, made.as_ref());
        doc.set_actor(prev_actor);
        crate::content::keep(env, content);
    } else {
        // No ops: a genuine no-op. Do not `commit_with` (it would pin then
        // prune this action's fresh actor and desync the patch log — see the
        // function docs). Leave the document exactly as the action found it.
        abort_pinned_change(doc, prev_actor);
    }
    Ok(made)
}

/// Commit the doc's open transaction as `env`'s change: under its pinned
/// time, with the message its context writes. `made` is the item the action
/// made that its description could not name, which the message names.
fn commit_pinned(doc: &mut Doc, env: &ActionEnvelope, made: Option<&crate::entry::ActionItem>) {
    let named = made.and_then(|item| {
        let context = env.context.as_ref()?;
        Some(
            context
                .clone()
                .with_described(context.described()?.on(item.id.clone(), item.label.clone())),
        )
    });
    let context = named.as_ref().or(env.context.as_ref());
    let mut options = CommitOptions::default().with_time(env.timestamp);
    if let Some(message) = context.and_then(|context| entry_message(doc, &env.actor(), context)) {
        options = options.with_message(message);
    }
    doc.commit_with(options);
}

/// `env` with the item `made` its change names, when it described one it
/// could not name before it applied: what a caller that replicates or
/// answers the change reads, as the change itself records it.
fn named(mut env: ActionEnvelope, made: Option<crate::entry::ActionItem>) -> ActionEnvelope {
    if let Some(item) = made
        && let Some(context) = env.context.as_ref()
        && let Some(described) = context.described()
    {
        env.context = Some(
            context
                .clone()
                .with_described(described.on(item.id, item.label)),
        );
    }
    env
}

/// The message `context` is written with by `actor` on `doc`: as the
/// continuation of the actor's last whole entry when the change differs from
/// it only in its gesture and action payload, else whole. Changes of others
/// in between do not matter. A function of the document, so a replaying device writes the
/// same message, hence the same change.
#[must_use]
pub fn entry_message(doc: &Doc, actor: &ActorId, context: &ActionContext) -> Option<String> {
    let context = context.holding(doc);
    if context.gesture.is_some() && context.compensation.is_none() {
        let document = doc.committed_document();
        let continuation = document
            .hash_of_actor_seq(actor, document.applied_seq(actor))
            .and_then(|last| context.continuation_of(&opening_entry(document, last)?));
        if continuation.is_some() {
            return continuation;
        }
    }
    context.entry_in(doc)
}

/// How deep [`entry_of`] follows its writer's continuations back to the
/// entry they continue. A writer writes its entry whole again past it, so a
/// change read on its own costs at most this many steps back.
const MAX_CONTINUATIONS: usize = 256;

/// How many continuations [`entry_of`] remembers the first entry of.
const CONTINUATIONS_KEPT: usize = 8;

thread_local! {
    /// The continuations [`entry_of`] read last, each with the whole entry
    /// it continues and how many changes back that entry is. A keystroke's
    /// change continues the one before it, so its entry is found one step
    /// back instead of by walking the whole gesture again. A change's hash
    /// names its content and history, so what is kept for it holds on any
    /// document.
    static CONTINUATIONS: std::cell::RefCell<std::collections::VecDeque<(automerge::ChangeHash, ActionContext, usize)>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(test)]
thread_local! {
    /// Changes [`entry_of`] stepped back over, for the test that pins it.
    static STEPS_BACK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The entry of the change `hash`: its message's, or for a continuation its
/// writer's last whole entry with the continuation's gesture and action
/// (found by following its writer's earlier changes).
#[must_use]
pub fn entry_of(doc: &automerge::Automerge, hash: automerge::ChangeHash) -> Option<ActionContext> {
    let entry = opening_entry(doc, hash)?;
    match doc.get_change_meta_by_hash(&hash)?.message.as_deref() {
        Some(message) if message.starts_with(CONTINUATION_PREFIX) => {
            ActionContext::continued(message, &entry)
        }
        _ => Some(entry),
    }
}

/// The whole entry of the change `hash`: its own, or for a continuation its
/// writer's last whole entry, which a continuation is written and read
/// against.
fn opening_entry(doc: &automerge::Automerge, hash: automerge::ChangeHash) -> Option<ActionContext> {
    let meta = doc.get_change_meta_by_hash(&hash)?;
    let message = meta.message.as_deref()?;
    if let Some(entry) = ActionContext::from_change_message(message) {
        return Some(entry);
    }
    if !message.starts_with(CONTINUATION_PREFIX) {
        return None;
    }
    let (first, depth) = first_entry(doc, &meta)?;
    CONTINUATIONS.with_borrow_mut(|kept| {
        if !kept.iter().any(|(kept, ..)| *kept == hash) {
            if kept.len() >= CONTINUATIONS_KEPT {
                kept.pop_front();
            }
            kept.push_back((hash, first.clone(), depth));
        }
    });
    Some(first)
}

/// The whole entry the continuation `meta` continues, and how many of its
/// writer's changes back it is: none when it is more than
/// [`MAX_CONTINUATIONS`] back.
fn first_entry(
    doc: &automerge::Automerge,
    meta: &automerge::ChangeMetadata<'_>,
) -> Option<(ActionContext, usize)> {
    for depth in 1..=MAX_CONTINUATIONS {
        #[cfg(test)]
        STEPS_BACK.with(|steps| steps.set(steps.get().saturating_add(1)));
        let seq = meta.seq.checked_sub(u64::try_from(depth).ok()?)?;
        let earlier = doc.get_change_meta_by_hash(&doc.hash_of_actor_seq(&meta.actor, seq)?)?;
        let text = earlier.message.as_deref()?;
        if let Some(first) = ActionContext::from_change_message(text) {
            return Some((first, depth));
        }
        if !text.starts_with(CONTINUATION_PREFIX) {
            return None;
        }
        let kept = CONTINUATIONS.with_borrow(|kept| {
            kept.iter()
                .find(|(hash, ..)| *hash == earlier.hash)
                .map(|(_, first, back)| (first.clone(), *back))
        });
        if let Some((first, back)) = kept {
            let depth = depth.checked_add(back)?;
            return (depth <= MAX_CONTINUATIONS).then_some((first, depth));
        }
    }
    None
}

#[cfg(test)]
#[path = "executor_entry_tests.rs"]
mod entry_tests;

/// The entries of a document's changes read in causal order, each
/// continuation folded into its writer's last whole entry as it is read.
#[derive(Default)]
pub struct EntryReader {
    firsts: std::collections::HashMap<ActorId, ActionContext>,
}

impl EntryReader {
    /// The entry of a change by `actor` with `message`, read after every
    /// change before it.
    pub fn read(&mut self, actor: &ActorId, message: Option<&str>) -> Option<ActionContext> {
        let message = message?;
        if let Some(entry) = ActionContext::from_change_message(message) {
            self.firsts.insert(actor.clone(), entry.clone());
            return Some(entry);
        }
        ActionContext::continued(message, self.firsts.get(actor)?)
    }
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
/// Automerge change commits under *their* actor so the workspace's history
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
    apply_steps(doc, action, encoded, (actor, None), &mut |_| {})
}

/// Apply `action` as the change of `actor` described by `context`, or, for a value that stands
/// for several steps ([`Action::steps`]), each of its steps as a change of its own, newest first:
/// the one way every action is applied here. `each` is handed every envelope as it is applied;
/// the last is returned. A step that fails leaves the steps before it applied.
fn apply_steps(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    (actor, context): (Option<&[u8]>, Option<ActionContext>),
    each: &mut dyn FnMut(&ActionEnvelope),
) -> Result<ActionEnvelope, ActionError> {
    // Read under the invocation that asks, as its steps then apply: whose steps they are
    // depends on who invokes.
    let steps = {
        let _invoking = InvokingScope::enter(context.clone());
        action.steps(doc).filter(|steps| !steps.is_empty())
    };
    let Some(steps) = steps else {
        let env = envelope_for(doc, action, encoded, actor, context, Before::Doc)?;
        let made = apply_under_envelope(doc, action, &env, None)?;
        let env = named(env, made);
        each(&env);
        return Ok(env);
    };
    // The steps are one move of the person's: one gesture, which undo and redo take as one
    // step, and one group, which History lists as one.
    let mut context = context.unwrap_or_default();
    let asked = {
        let _invoking = InvokingScope::enter(Some(context.clone()));
        action.describe(doc)
    };
    let op = match asked.map(|asked| asked.what) {
        Some(Did::History(step)) => step.op,
        _ => crate::HistoryOp::Undo,
    };
    let gesture = *context.gesture.get_or_insert_with(ActionId::new);
    context.group.get_or_insert_with(|| crate::WorkGroup {
        id: gesture.0.to_string(),
        label: None,
        kind: crate::context::history_group(&op),
        declared_id: None,
        session: None,
    });
    let context = Some(context);
    let mut last = None;
    for (step, payload) in steps {
        let encoded = EncodedAction {
            type_id: encoded.type_id.clone(),
            payload,
        };
        let env = envelope_for(
            doc,
            step.as_ref(),
            encoded,
            actor,
            context.clone(),
            Before::Doc,
        )?;
        let made = apply_under_envelope(doc, step.as_ref(), &env, None)?;
        let env = named(env, made);
        each(&env);
        last = Some(env);
    }
    last.ok_or_else(|| ActionError::Refused("no step to apply".into()))
}

/// [`apply_local`], with semantic provenance embedded in the resulting
/// durable change. Use this at framework/action-API boundaries where the
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
    apply_steps(doc, action, encoded, (actor, Some(context)), &mut |_| {})
}

/// Commit what `action` already wrote, the doc's open transaction under
/// `actor`, as its change recorded with `context`, and return its replayable
/// envelope: the change [`apply_local_with_context`] would have made, without
/// applying it again. `described` is what the action said it does, read from
/// the document before its first write.
///
/// The open transaction must hold the action's writes and nothing else,
/// written as [`Action::apply`] writes them, and the action must record no
/// compensation: a peer replays the action and checks that it makes exactly
/// this change ([`verify_change`]).
///
/// # Errors
///
/// [`ActionError::Refused`] when the doc writes as another actor or holds no
/// open writes, or the action is too large to record.
pub fn commit_written(
    doc: &mut Doc,
    action: &dyn Action,
    encoded: EncodedAction,
    actor: &[u8],
    context: ActionContext,
    described: Option<Described>,
) -> Result<ActionEnvelope, ActionError> {
    if doc.get_actor().to_bytes() != actor || doc.pending_ops() == 0 {
        return Err(ActionError::Refused(
            "the document holds no writes of this actor to commit".into(),
        ));
    }
    let env = envelope_for(
        doc,
        action,
        encoded,
        Some(actor),
        Some(context),
        Before::Written(described.map(Box::new)),
    )?;
    commit_pinned(doc, &env, None);
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
    replay_decoded(doc, env, action.as_ref(), registry.content())
}

/// [`replay`] of `action`, the envelope's action already decoded: what a
/// device replays an app's verb with when the app's own code is the model
/// module its package carries rather than a type in this process.
///
/// # Errors
///
/// See [`replay`].
pub fn replay_decoded(
    doc: &mut Doc,
    env: &ActionEnvelope,
    action: &dyn Action,
    content: Option<&tilo_content::ContentStore>,
) -> Result<(), ActionError> {
    let _replaying = ReplayScope::enter();
    let base_heads = env.base_change_hashes();
    if doc.get_heads() == base_heads {
        validate_compensation(doc, action, env)?;
        return apply_under_envelope(doc, action, env, content).map(drop);
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
    validate_compensation(&mut replay_base, action, env)?;
    apply_under_envelope(&mut replay_base, action, env, content).map(drop)?;
    merge_replayed(doc, &mut replay_base)?;
    Ok(())
}

/// Merge `replayed`, made at the envelope's base, into `doc` as it stands, dropping the patch
/// events the merge logs: an [`automerge::AutoCommit`] logs one per applied operation and never
/// drops them, and nothing reads them unless the document's owner armed a diff cursor.
#[expect(
    clippy::disallowed_methods,
    reason = "the action layer's one merge, dropping what it logs"
)]
fn merge_replayed(
    doc: &mut Doc,
    replayed: &mut Doc,
) -> Result<Vec<automerge::ChangeHash>, automerge::AutomergeError> {
    let merged = doc.merge(replayed);
    if doc.diff_cursor().is_empty() {
        doc.reset_diff_cursor();
    }
    merged
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
    /// The recorded action names content (its large parts) that is not in
    /// this device's content store yet: the change is held until it is.
    ContentMissing(String),
}

/// The action as its change records it: itself when the change documents it
/// whole, else with its large parts kept, sealed, in the content store of
/// the window that records it ([`crate::content::register`]). Built once, on
/// the device that invokes the action. An action too large even then is
/// refused, and so is one whose parts no store would keep: a change never
/// commits without the action it ran.
pub fn recorded_form(action: &EncodedAction) -> Result<EncodedAction, ActionError> {
    if recorded_size(action) <= crate::context::WHOLE_ACTION_BUDGET {
        return Ok(action.clone());
    }
    let recorded = action.with_content_parts(crate::content::CONTENT_PART_THRESHOLD)?;
    if recorded_size(&recorded) > crate::context::HISTORY_BUDGET {
        return Err(ActionError::Refused(
            "the action is too large to record, even with its large parts kept as content".into(),
        ));
    }
    Ok(recorded)
}

/// What an entry's action takes in the change that keeps it: its type and its
/// payload, inline.
fn recorded_size(action: &EncodedAction) -> usize {
    action.type_id.len().saturating_add(action.payload.len())
}

/// Replay the action the change `hash` records on the document it was
/// authored against, and check that it produces exactly that change: same
/// operations, same actor, time and message, so the same hash. This is what
/// a device that has the app does with every change it receives, beside the
/// server's grant check: an author granted an action who wrote something else
/// inside its storage units is caught here.
pub fn verify_change(
    doc: &mut Doc,
    hash: automerge::ChangeHash,
    registry: &ActionRegistry,
) -> Result<Verified, ActionError> {
    let change = doc
        .get_change_meta_by_hash(&hash)
        .ok_or_else(|| ActionError::Refused("the change is not in the document".into()))?
        .into_owned();
    let Some(context) = entry_of(doc.committed_document(), hash) else {
        return Ok(Verified::NothingRecorded);
    };
    let Some(recorded) = context.action.as_ref() else {
        return Ok(Verified::NothingRecorded);
    };
    if !registry.contains(&recorded.type_id) {
        return Ok(Verified::UnknownAction(recorded.type_id.clone()));
    }
    let action = match registry.resolve(recorded)? {
        Ok(action) => action,
        Err(missing) => return Ok(Verified::ContentMissing(missing.0)),
    };
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
pub fn verified_compensation(
    doc: &mut Doc,
    hash: automerge::ChangeHash,
    registry: &ActionRegistry,
) -> Result<Option<Box<dyn Action>>, ActionError> {
    doc.get_change_meta_by_hash(&hash)
        .ok_or_else(|| ActionError::Refused("history change is unavailable".into()))?;
    let inverse = entry_of(doc.committed_document(), hash).and_then(|context| context.compensation);
    let Some(inverse) = inverse else {
        return Ok(None);
    };
    match verify_change(doc, hash, registry)? {
        Verified::Reproduced => registry.decode(&inverse).map(Some),
        Verified::NothingRecorded | Verified::UnknownAction(_) | Verified::ContentMissing(_) => {
            Err(ActionError::Refused(
                "recorded history action does not reproduce its change".into(),
            ))
        }
    }
}

fn validate_compensation(
    doc: &mut Doc,
    action: &dyn Action,
    env: &ActionEnvelope,
) -> Result<(), ActionError> {
    let Some(context) = env.context.as_ref() else {
        return Ok(());
    };
    let Some(recorded) = context.action.as_ref() else {
        return Ok(());
    };
    if recorded.recorded_parts_of(&env.action).is_none()
        || inverse_as(doc, action, env.actor()) != context.compensation
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
/// execution — the boundary between the pure action model and whatever
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

/// The locus-aware front API, routing `action` per
/// its [`ExecutionLocus`]: `ClientLocal` →
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
    // A change names the app its record belongs to, and a caller that names none is a caller
    // that would be decided against an app guessed from a type id: never a decision. The typed
    // replica a system write goes through names one before it may apply at all
    // (`tilo_resource::Replica::in_app`); an app that writes its own record does too.
    match action.locus() {
        ExecutionLocus::ClientLocal => {
            apply_steps(doc, action, encoded, (actor, context), &mut |env| {
                transport.replicate(env);
            })
            .map(Dispatched::AppliedLocal)
        }
        ExecutionLocus::ServerPreferred => {
            let env = envelope_for(doc, action, encoded, actor, context, Before::Doc)?;
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
