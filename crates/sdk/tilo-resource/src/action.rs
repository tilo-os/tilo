//! Typed actions over a record resource: a method on the record becomes a
//! replayable, attributable, undoable operation.
//!
//! `#[resource]` turns every `#[action]` method into a variant of a generated
//! operation enum implementing [`Operation`]. Applying it reads the record
//! from the document, runs the method, and writes back only what the method
//! changed — so the method is ordinary Rust over ordinary fields and the CRDT
//! stays invisible.

use std::any::Any;
use std::marker::PhantomData;

use automerge::ROOT;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, ExecutionLocus, RiskLevel};

use crate::collection::Id;
use crate::model::{Doc, Fields};
use crate::resource::Resource;

/// Deterministic services available while an action applies.
///
/// Everything here is a pure function of the action and the document it was
/// authored against, so the origin and every replaying replica compute the
/// same ids and see the same time.
#[derive(Debug)]
pub struct ActionCx {
    seed: [u8; 32],
    minted: u64,
    /// The ids [`Self::id`] minted, in order.
    ids: Vec<Id>,
    now_ms: i64,
    author: Option<String>,
    invocation: Option<tilo_actions::InvocationId>,
}

/// Who invoked the action applying now, as its envelope pinned it.
fn invoking_author() -> Option<String> {
    tilo_actions::invoking_context()
        .and_then(|context| context.invoker)
        .map(|invoker| invoker.actor)
}

fn invoking_invocation() -> Option<tilo_actions::InvocationId> {
    tilo_actions::invoking_context().and_then(|context| context.source_invocation)
}

impl ActionCx {
    /// A context seeded from the document's heads and actor (both pinned by
    /// the executor) and the action's own payload.
    ///
    /// Inside an [`ActionGroup`](crate::ActionGroup) the group's seed and the
    /// member's position stand in for the heads, which a member cannot read
    /// without committing the members before it as separate changes.
    #[must_use]
    pub fn for_apply(doc: &Doc, payload: &[u8], now_ms: i64) -> Self {
        Self {
            seed: seed(doc, b"tilo-resource/action-seed/v1", payload),
            minted: 0,
            ids: Vec::new(),
            now_ms,
            author: invoking_author(),
            invocation: invoking_invocation(),
        }
    }

    /// A context for ids minted part-way through an action that has already
    /// written: seeded like [`Self::for_apply`] but from the heads the action
    /// started from, so reading them does not commit its writes so far as a
    /// separate change.
    #[must_use]
    pub fn for_write(doc: &Doc, payload: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo-resource/write-seed/v1");
        if let Some((group, position)) = crate::group::current_member() {
            hasher.update(b"member");
            hasher.update(&group);
            hasher.update(&position.to_le_bytes());
        } else {
            hasher.update(doc.get_actor().to_bytes());
            let mut heads = automerge::transaction::Transactable::base_heads(doc);
            heads.sort();
            for head in heads {
                hasher.update(&head.0);
            }
        }
        hasher.update(payload);
        Self {
            seed: *hasher.finalize().as_bytes(),
            minted: 0,
            ids: Vec::new(),
            now_ms: 0,
            author: invoking_author(),
            invocation: invoking_invocation(),
        }
    }

    /// A fresh id, unique to this action and stable under replay.
    pub fn id(&mut self) -> Id {
        let id = minted(&self.seed, self.minted);
        self.minted = self.minted.saturating_add(1);
        self.ids.push(id.clone());
        note_minted(id.as_str());
        id
    }

    /// A fresh UUID, unique to this action and stable under replay.
    pub fn uuid(&mut self) -> uuid::Uuid {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"uuid");
        hasher.update(&self.seed);
        hasher.update(&self.minted.to_le_bytes());
        self.minted = self.minted.saturating_add(1);
        let uuid = uuid_from(hasher.finalize().as_bytes());
        note_minted(&uuid.to_string());
        uuid
    }

    /// The person the invoker acted for (an agent's or a Shortcut's person),
    /// as the envelope pinned it; `None` when someone acted for themselves.
    #[must_use]
    pub fn for_person(&self) -> Option<String> {
        tilo_actions::invoking_context()
            .and_then(|context| context.invoker)
            .and_then(|invoker| invoker.on_behalf_of)
    }

    /// What kind of actor invoked the action (a person, an agent, a
    /// Shortcut), as the envelope pinned it.
    #[must_use]
    pub fn actor_kind(&self) -> Option<tilo_actions::ActorKind> {
        tilo_actions::invoking_context()
            .and_then(|context| context.invoker)
            .map(|invoker| invoker.kind)
    }

    /// When the action was authored, in milliseconds since the Unix epoch, as
    /// read once on the origin.
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// Who the action is by: the person or agent that invoked it, the same
    /// id that participant's own views read as `me`. Pinned with the action
    /// like its time, so every replica reads the same author; the payload
    /// need not carry it. `None` for an action applied with no invoker
    /// (an edit written straight into a document outside the host).
    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    /// The invocation admitted by the action door, pinned in the action
    /// envelope so origin and replay observe the same correlation identity.
    /// This is not caller authority; use [`Self::author`] for that.
    pub const fn invocation(&self) -> Option<tilo_actions::InvocationId> {
        self.invocation
    }

    /// The content store of the window applying the action, where the bytes
    /// the record names by hash are kept: the one the host lent that window,
    /// which carries them on the record's route. Content is not replayed —
    /// a replica that applies the action reads the bytes through its own
    /// window's store. `None` where no window applies it (a tool writing a
    /// document directly).
    #[must_use]
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        crate::content::current()
    }
}

fn uuid_from(digest: &[u8; 32]) -> uuid::Uuid {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(digest.get(..16).unwrap_or(&[0; 16]));
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

thread_local! {
    /// The ids the action applying last on this thread minted, in order.
    static MINTED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn note_minted(id: &str) {
    MINTED.with(|minted| minted.borrow_mut().push(id.to_owned()));
}

/// The ids the record action applied last on this thread minted (items it
/// added, a group's members' together), taken: what the door answers its
/// caller with beside the action's own answer.
#[must_use]
pub fn take_minted() -> Vec<String> {
    MINTED.with(|minted| std::mem::take(&mut *minted.borrow_mut()))
}

thread_local! {
    /// The seed and count of the UUIDs [`fresh_uuid`] mints while a verb
    /// performs on this thread.
    static MINT: std::cell::Cell<Option<([u8; 32], u64)>> = const { std::cell::Cell::new(None) };
}

struct MintScope(Option<([u8; 32], u64)>);

impl MintScope {
    fn enter(cx: &ActionCx) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo-resource/fresh-uuid/v1");
        hasher.update(&cx.seed);
        let seed = *hasher.finalize().as_bytes();
        Self(MINT.with(|mint| mint.replace(Some((seed, 0)))))
    }
}

impl Drop for MintScope {
    fn drop(&mut self) {
        MINT.with(|mint| mint.set(self.0));
    }
}

/// A new UUID for domain code that makes things (a clip split in two, a
/// marker): inside a verb it is minted from the action, so every replica
/// makes the same one; anywhere else (a surface editing its local copy
/// before it submits the edits) it is random.
#[must_use]
pub fn fresh_uuid() -> uuid::Uuid {
    MINT.with(|mint| match mint.get() {
        Some((seed, counter)) => {
            mint.set(Some((seed, counter.saturating_add(1))));
            let mut hasher = blake3::Hasher::new();
            hasher.update(&seed);
            hasher.update(&counter.to_le_bytes());
            let uuid = uuid_from(hasher.finalize().as_bytes());
            note_minted(&uuid.to_string());
            uuid
        }
        None => uuid::Uuid::new_v4(),
    })
}

fn minted(seed: &[u8; 32], counter: u64) -> Id {
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed);
    hasher.update(&counter.to_le_bytes());
    let digest = hasher.finalize();
    let hex = digest.to_hex();
    Id::from_raw(hex.as_str().get(..20).unwrap_or_default())
}

/// The ids the action `envelope` committed minted with [`ActionCx::id`], in
/// the order it minted them: how the one who invoked it learns which item
/// its action created (the first id of a "new snippet"). For an action
/// that is not a group, applied with nothing else pending.
pub fn minted_ids(envelope: &tilo_actions::ActionEnvelope) -> impl Iterator<Item = Id> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-resource/action-seed/v1");
    hasher.update(envelope.actor().to_bytes());
    let mut heads = envelope.base_heads.clone();
    heads.sort_unstable();
    for head in &heads {
        hasher.update(head);
    }
    hasher.update(&envelope.action.payload);
    let seed = *hasher.finalize().as_bytes();
    (0_u64..).map(move |counter| minted(&seed, counter))
}

/// A deterministic seed for applying `payload` to `doc`: the document's
/// actor and heads (both pinned by the executor), or, inside a group member,
/// the group's seed and the member's position.
pub(crate) fn seed(doc: &Doc, domain: &[u8], payload: &[u8]) -> [u8; 32] {
    use automerge::transaction::Transactable as _;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    if let Some((group, position)) = crate::group::current_member() {
        hasher.update(b"member");
        hasher.update(&group);
        hasher.update(&position.to_le_bytes());
    } else {
        hasher.update(doc.get_actor().to_bytes());
        // The committed heads, and how far an open transaction has got past
        // them: reading the heads must not commit a transaction someone else
        // (a host stamping typing as one change) owns. With nothing pending
        // this is exactly the document's heads.
        let mut heads = doc.base_heads();
        heads.sort();
        for head in heads {
            hasher.update(&head.0);
        }
        let pending = doc.pending_ops();
        if pending > 0 {
            hasher.update(b"pending");
            hasher.update(&u64::try_from(pending).unwrap_or(u64::MAX).to_le_bytes());
        }
    }
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

/// Why an action declined to run, in the user's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl Refusal {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Refusal {
    fn from(reason: &str) -> Self {
        Self(reason.to_owned())
    }
}

impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Self(reason)
    }
}

/// What an action method may return: nothing, or a refusal.
pub trait Outcome {
    /// # Errors
    /// Returns the refusal.
    fn into_result(self) -> Result<(), Refusal>;
}

impl Outcome for () {
    fn into_result(self) -> Result<(), Refusal> {
        Ok(())
    }
}

impl<E: Into<Refusal>> Outcome for Result<(), E> {
    fn into_result(self) -> Result<(), Refusal> {
        self.map_err(Into::into)
    }
}

pub use tilo_action_policy::ActionKind;

pub use tilo_actions::ActionKey;

/// One named operation of a record resource, generated by `#[resource]`.
pub trait Operation: Serialize + DeserializeOwned + Clone + std::fmt::Debug + 'static {
    type Record: Fields + 'static;

    /// The resource kind the record is published as.
    const KIND: &'static str;

    /// The field edit this operation is, when it is one: it applies at its
    /// path instead of through [`Self::perform`].
    fn edit(&self) -> Option<&crate::Edit<Self::Record>> {
        None
    }

    /// What this operation can do. `#[resource]` derives it from each
    /// verb's signature.
    fn kind(&self) -> ActionKind {
        ActionKind::Undoable
    }

    /// Run the operation's method on the record.
    ///
    /// # Errors
    /// Returns the method's refusal.
    fn perform(&self, record: &mut Self::Record, cx: &mut ActionCx) -> Result<(), Refusal>;

    /// The units this operation may write beside the record unit (see
    /// [`crate::footprint`]). `#[resource]` generates it: an item verb its
    /// item, `#[action(writes = field)]` the fields named.
    fn writes(&self) -> crate::footprint::Writes {
        crate::footprint::Writes::record()
    }

    /// A copy of `record`, so the next action can start from it rather than
    /// read the whole record from the document again. `#[resource]`
    /// generates it for a `Clone` record; without one (the default) every
    /// action reads the record whole.
    fn snapshot(record: &Self::Record) -> Option<Self::Record> {
        let _ = record;
        None
    }

    /// The consequence class of this operation. `#[resource]` generates it
    /// from `#[action(destructive)]`, so the
    /// same method reached through the generic `apply` action is confirmed
    /// exactly as its named declaration is.
    fn risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    /// What the operation is to the people sharing the record: content,
    /// housekeeping about it, or content from outside that is nobody's undo
    /// step. `#[resource]` generates it from `#[action(housekeeping)]` and
    /// `#[action(external)]`.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        tilo_actions::ChangeKind::Content
    }

    /// Whether this operation declares the verb that undoes it
    /// (`#[action(undo = path)]`).
    fn compensates(&self) -> bool {
        false
    }

    /// Whether every change of the record may queue an outside consequence
    /// (`#[resource(queue = path)]`): [`Self::queue`] runs after each.
    const QUEUES: bool = false;

    /// Queue in the record's outboxes what the change from `before` to
    /// `record` must do outside (`#[resource(queue = path)]`).
    fn queue(record: &mut Self::Record, before: &Self::Record, cx: &mut ActionCx) {
        let _ = (record, before, cx);
    }

    /// Apply an item method (`fn star(contact: &mut Contact)`) to its item
    /// alone in `doc`: read the item, run the method, write what changed.
    /// `None` for any other operation, or where the record holds an
    /// invariant of its own.
    fn perform_item(&self, doc: &mut Doc, cx: &mut ActionCx) -> Option<Result<(), Refusal>> {
        let _ = (doc, cx);
        None
    }

    /// The field edit `edit` as an operation of the record.
    fn field_edit(edit: crate::Edit<Self::Record>) -> Option<Self> {
        let _ = edit;
        None
    }

    /// The verb that undoes this one, from the record before and after it
    /// ran and the ids it minted (`#[action(undo = path)]`). `None` when it
    /// declares none: undo then reverts the document by inverse patches.
    ///
    /// # Errors
    /// Returns why the declared undo does not build.
    fn compensation(
        &self,
        before: &Self::Record,
        after: &Self::Record,
        minted: &[Id],
    ) -> Option<Result<Self, String>> {
        let _ = (before, after, minted);
        None
    }
}

/// What a verb's declared undo (`#[action(undo = path)]`) is computed from:
/// the record before and after the verb ran, the ids it minted (the outbox
/// entry it queued) and its parameters. The undo returns the verb that
/// compensates, which undo applies like any action, so its own outbox
/// entries are queued and drained the same way, and which reads at that
/// time how the original entry settled (where the trashed file went).
pub struct Undoing<'a, R, P> {
    pub before: &'a R,
    pub after: &'a R,
    pub minted: &'a [Id],
    pub params: &'a P,
}

impl<R, P> Clone for Undoing<'_, R, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R, P> Copy for Undoing<'_, R, P> {}

/// A model error as the action's refusal: a function, not a closure per
/// operation type.
#[allow(clippy::needless_pass_by_value, reason = "what `map_err` hands it")]
fn model_refused(error: crate::ModelError) -> ActionError {
    ActionError::Refused(error.to_string())
}

/// A document error as the action's refusal.
#[allow(clippy::needless_pass_by_value, reason = "what `map_err` hands it")]
fn automerge_refused(error: automerge::AutomergeError) -> ActionError {
    ActionError::Refused(error.to_string())
}

/// A verb's refusal as the action's.
fn refusal_refused(refusal: Refusal) -> ActionError {
    ActionError::Refused(refusal.0)
}

/// A record operation with the origin's time attached: the replayable action
/// of a `#[resource]` record.
#[derive(Serialize, Clone)]
pub struct Stamped<O> {
    #[serde(rename = "op")]
    pub operation: O,
    #[serde(rename = "at", default)]
    pub now_ms: i64,
}

impl<O: std::fmt::Debug> std::fmt::Debug for Stamped<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt_stamped(&self.operation, self.now_ms, f)
    }
}

fn fmt_stamped(
    operation: &dyn std::fmt::Debug,
    now_ms: i64,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    f.debug_struct("Stamped")
        .field("operation", operation)
        .field("now_ms", &now_ms)
        .finish()
}

// Decoded through a JSON value whatever the deserializer: the operation's
// decoder, one per record type, is compiled for that one deserializer.
impl<'de, O: DeserializeOwned> Deserialize<'de> for Stamped<O> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match Self::from_json(value) {
            Ok(stamped) => Ok(stamped),
            Err(error) => Err(serde::de::Error::custom(error)),
        }
    }
}

impl<O: DeserializeOwned> Stamped<O> {
    /// The stamped operation in `value`, its JSON form.
    ///
    /// # Errors
    /// Why `value` is not a stamped operation of `O`.
    pub fn from_json(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        let (operation, now_ms) = stamped_parts(value)?;
        Ok(Self {
            operation: O::deserialize(operation)?,
            now_ms,
        })
    }
}

/// The operation and time of a stamped operation's JSON form.
#[inline(never)]
fn stamped_parts(value: serde_json::Value) -> Result<(serde_json::Value, i64), serde_json::Error> {
    use serde::de::Error as _;
    let serde_json::Value::Object(mut fields) = value else {
        return Err(serde_json::Error::custom(
            "a stamped operation is an object",
        ));
    };
    let operation = fields
        .remove("op")
        .ok_or_else(|| serde_json::Error::missing_field("op"))?;
    let now_ms = match fields.remove("at") {
        None | Some(serde_json::Value::Null) => 0,
        Some(at) => i64::deserialize(at)?,
    };
    Ok((operation, now_ms))
}

/// Whether a field edit only records how an outbox entry settled or forgets
/// one: bookkeeping of the outbox that queues nothing and is nobody's step.
fn settles(operation: &impl Operation) -> bool {
    matches!(
        operation.edit().map(crate::Edit::op),
        Some(crate::EditOp::Settle { .. } | crate::EditOp::Forget { .. })
    )
}

impl<O: Operation> Stamped<O> {
    /// Whether undoing this goes through `apply`, so what it queued outside
    /// is compensated the same way: a verb that declares its undo, and every
    /// change of a record that queues (`#[resource(queue = path)]`) a person
    /// can take back.
    fn compensated(&self) -> bool {
        // Bookkeeping, content from outside and housekeeping are nobody's
        // undo step: nothing compensates them, however much they wrote.
        if settles(&self.operation)
            || matches!(
                self.operation.change_kind(),
                tilo_actions::ChangeKind::External | tilo_actions::ChangeKind::Housekeeping
            )
        {
            return false;
        }
        O::QUEUES || (self.operation.edit().is_none() && self.operation.compensates())
    }

    /// The record after this applies to `doc`, read apart from it, and the
    /// ids it minted: under the seed its application will have, so they are
    /// the ones it queues.
    fn applied_to_copy(&self, doc: &Doc) -> Option<(O::Record, Vec<Id>)> {
        let payload = serde_json::to_vec(self).ok()?;
        if let Some(edit) = self.operation.edit() {
            let mut copy = doc.clone();
            let mut cx = ActionCx::for_apply(doc, &payload, self.now_ms);
            edit.apply(&mut copy, &mut cx).ok()?;
            return Some((O::Record::read_fields(&copy, &ROOT).ok()?, cx.ids));
        }
        let mut after = O::Record::read_fields(doc, &ROOT).ok()?;
        let mut cx = ActionCx::for_apply(doc, &payload, self.now_ms);
        let minting = MintScope::enter(&cx);
        let performed = self.operation.perform(&mut after, &mut cx);
        drop(minting);
        performed.ok()?;
        Some((after, cx.ids))
    }

    /// The action that undoes this one at the document `doc` it applies to:
    /// the verb it declares (`#[action(undo = path)]`), or on a record that
    /// queues, the field edits that restore what it changed, applied
    /// through `apply` so the record's `queue` compensates outside.
    fn compensation(&self, doc: &Doc) -> Option<tilo_actions::EncodedAction> {
        if !self.compensated() {
            return None;
        }
        let before = O::Record::read_fields(doc, &ROOT).ok()?;
        let (after, minted) = self.applied_to_copy(doc)?;
        if self.operation.edit().is_none() && self.operation.compensates() {
            // An undo whose arguments do not build is a bug in the app the
            // verb's tests catch; undo then reverts by inverse patches.
            let operation = self
                .operation
                .compensation(&before, &after, &minted)?
                .ok()?;
            let undo = Self {
                operation,
                now_ms: self.now_ms,
            };
            return tilo_actions::EncodedAction::encode(O::KIND, &undo).ok();
        }
        let mut restore: Vec<crate::Edit<()>> = Vec::new();
        crate::Model::diff(&after, &before, &[], &mut restore);
        let mut members = Vec::with_capacity(restore.len());
        for edit in restore {
            members.push(Self {
                operation: O::field_edit(edit.retyped())?,
                now_ms: self.now_ms,
            });
        }
        let restore = match members.len() {
            0 => return None,
            1 => tilo_actions::EncodedAction::encode(O::KIND, &members.pop()?).ok()?,
            _ => tilo_actions::EncodedAction::encode(
                format!("{}+group", O::KIND),
                &crate::ActionGroup { actions: members },
            )
            .ok()?,
        };
        if serde_json::to_vec(&restore).ok()?.len() <= tilo_actions::context::COMPENSATION_BUDGET {
            return Some(restore);
        }
        // Too large to record (an import of thousands of items): undo
        // derives the same restoring edits from the change itself.
        let reverting = Reverting::<O> {
            actor: doc.get_actor().to_hex_string(),
            deps: doc
                .committed_heads()
                .iter()
                .map(ToString::to_string)
                .collect(),
            now_ms: self.now_ms,
            operation: PhantomData,
        };
        tilo_actions::EncodedAction::encode(reverting_type(O::KIND), &reverting).ok()
    }
}

/// The type id [`Reverting`] actions of the record kind `kind` encode as.
pub fn reverting_type(kind: &str) -> String {
    format!("{kind}+revert")
}

/// The undo of one change of a record that queues, too large to record its
/// restoring field edits: it names the change (its writer and the heads it
/// was made on), derives the edits that restore what the change wrote from
/// the record before and after it, applies them, and runs the record's
/// `queue` so what the change queued outside is compensated.
#[derive(Serialize)]
#[serde(bound = "")]
pub struct Reverting<O> {
    actor: String,
    deps: Vec<String>,
    #[serde(rename = "at", default)]
    now_ms: i64,
    #[serde(skip)]
    operation: PhantomData<O>,
}

// Decoded through a JSON value by one function for every record type.
impl<'de, O> Deserialize<'de> for Reverting<O> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let (actor, deps, now_ms) = match reverted(value) {
            Ok(target) => target,
            Err(error) => return Err(serde::de::Error::custom(error)),
        };
        Ok(Self {
            actor,
            deps,
            now_ms,
            operation: PhantomData,
        })
    }
}

/// The writer, heads and time a reverting action's JSON form names.
#[inline(never)]
fn reverted(value: serde_json::Value) -> Result<(String, Vec<String>, i64), serde_json::Error> {
    #[derive(Deserialize)]
    struct Form {
        actor: String,
        deps: Vec<String>,
        #[serde(rename = "at", default)]
        now_ms: i64,
    }
    let form = Form::deserialize(value)?;
    Ok((form.actor, form.deps, form.now_ms))
}

impl<O> std::fmt::Debug for Reverting<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reverting")
            .field("actor", &self.actor)
            .field("deps", &self.deps)
            .finish_non_exhaustive()
    }
}

impl<O: Operation> Reverting<O> {
    /// The change this takes back, by its writer and the heads it was made
    /// on.
    fn change(&self, doc: &mut Doc) -> Result<automerge::ChangeHash, ActionError> {
        reverted_change(&self.actor, &self.deps, doc)
    }
}

/// The change by `actor` made on the heads `deps`.
#[inline(never)]
fn reverted_change(
    actor: &str,
    deps: &[String],
    doc: &mut Doc,
) -> Result<automerge::ChangeHash, ActionError> {
    let refused = |why: &str| ActionError::Refused(why.into());
    let mut deps = deps
        .iter()
        .map(|hash| hash.parse())
        .collect::<Result<Vec<automerge::ChangeHash>, _>>()
        .map_err(|_| refused("a reverted change names invalid heads"))?;
    deps.sort();
    doc.get_changes(&deps)
        .into_iter()
        .find(|change| {
            change.actor_id().to_hex_string() == actor && {
                let mut of = change.deps().to_vec();
                of.sort();
                of == deps
            }
        })
        .map(|change| change.hash())
        .ok_or_else(|| refused("the change to take back is not in this document"))
}

impl<O: Operation> tilo_actions::Action for Reverting<O> {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let change = self.change(doc)?;
        let payload = serde_json::to_vec(self).map_err(ActionError::Encode)?;
        revert(
            &RecordOf::<O::Record>::OPS,
            &|record, before, cx| {
                if let (Some(record), Some(before)) = (
                    record.downcast_mut::<O::Record>(),
                    before.downcast_ref::<O::Record>(),
                ) {
                    O::queue(record, before, cx);
                }
            },
            (change, &payload, self.now_ms),
            doc,
        )
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// [`Reverting::apply`] of the change `change`, encoded as `payload` at
/// `now_ms`, to a record of the type `record` whose `queue` is `queue`.
fn revert(
    record: &RecordOps,
    queue: &dyn Fn(&mut dyn Any, &dyn Any, &mut ActionCx),
    (change, payload, now_ms): (automerge::ChangeHash, &[u8], i64),
    doc: &mut Doc,
) -> Result<(), ActionError> {
    let deps: Vec<automerge::ChangeHash> = doc
        .get_change_meta_by_hash(&change)
        .map(|meta| meta.deps)
        .unwrap_or_default();
    let before = {
        let at = doc.committed_fork_at(&deps).map_err(automerge_refused)?;
        (record.read)(&at).map_err(model_refused)?
    };
    let after = {
        let at = doc
            .committed_fork_at(&[change])
            .map_err(automerge_refused)?;
        (record.read)(&at).map_err(model_refused)?
    };
    let restore = (record.diff)(after.as_ref(), before.as_ref());
    let mut cx = ActionCx::for_apply(doc, payload, now_ms);
    let current = (record.read)(doc).map_err(model_refused)?;
    for edit in &restore {
        (record.edit)(edit, doc, &mut cx).map_err(model_refused)?;
    }
    let mut written = (record.read)(doc).map_err(model_refused)?;
    let minting = MintScope::enter(&cx);
    queue(written.as_mut(), current.as_ref(), &mut cx);
    drop(minting);
    (record.write)(written.as_ref(), doc).map_err(model_refused)
}

/// Write the first record over the second, kept, with the changes `dirty`
/// others made since.
type WriteChanged =
    fn(&dyn Any, &dyn Any, &mut Doc, &crate::Delta) -> Result<(), crate::ModelError>;

/// What applying an operation needs of its record's type, with the type
/// erased: a few functions per record type, so the apply itself is
/// compiled once rather than for every record.
pub(crate) struct RecordOps {
    shape: crate::Shape,
    name: fn() -> &'static str,
    kind: fn() -> std::any::TypeId,
    read: fn(&Doc) -> Result<Box<dyn Any>, crate::ModelError>,
    write: fn(&dyn Any, &mut Doc) -> Result<(), crate::ModelError>,
    write_changed: WriteChanged,
    check: fn(&dyn Any) -> Result<(), Refusal>,
    refresh: fn(&mut dyn Any, &Doc, &crate::Delta) -> bool,
    /// The edits that turn the first record into the second.
    diff: fn(&dyn Any, &dyn Any) -> Vec<crate::Edit<()>>,
    edit: fn(&crate::Edit<()>, &mut Doc, &mut ActionCx) -> Result<(), crate::ModelError>,
}

/// The record of `R` a [`RecordOps`] function was handed.
fn record_of<R: 'static>(record: &dyn Any) -> Result<&R, crate::ModelError> {
    match record.downcast_ref::<R>() {
        Some(record) => Ok(record),
        None => Err(crate::ModelError::invalid("a record of another type")),
    }
}

fn read_record<R: Fields + 'static>(doc: &Doc) -> Result<Box<dyn Any>, crate::ModelError> {
    match R::read_fields(doc, &ROOT) {
        Ok(record) => Ok(Box::new(record)),
        Err(error) => Err(error),
    }
}

fn write_record<R: Fields + 'static>(
    record: &dyn Any,
    doc: &mut Doc,
) -> Result<(), crate::ModelError> {
    record_of::<R>(record)?.write_fields(doc, &ROOT)
}

fn write_changed_record<R: Fields + 'static>(
    record: &dyn Any,
    kept: &dyn Any,
    doc: &mut Doc,
    dirty: &crate::Delta,
) -> Result<(), crate::ModelError> {
    record_of::<R>(record)?.write_changed_fields(record_of::<R>(kept)?, doc, &ROOT, dirty)
}

fn check_record<R: Fields + 'static>(record: &dyn Any) -> Result<(), Refusal> {
    match record.downcast_ref::<R>() {
        Some(record) => crate::Model::check(record),
        None => Err(Refusal::new("a record of another type")),
    }
}

fn refresh_record<R: Fields + 'static>(
    record: &mut dyn Any,
    doc: &Doc,
    delta: &crate::Delta,
) -> bool {
    match record.downcast_mut::<R>() {
        Some(record) => record.refresh_fields(doc, &ROOT, delta).is_ok(),
        None => false,
    }
}

fn diff_records<R: Fields + 'static>(from: &dyn Any, to: &dyn Any) -> Vec<crate::Edit<()>> {
    let mut edits = Vec::new();
    if let (Some(from), Some(to)) = (from.downcast_ref::<R>(), to.downcast_ref::<R>()) {
        crate::Model::diff(from, to, &[], &mut edits);
    }
    edits
}

fn edit_record<R: Fields + 'static>(
    edit: &crate::Edit<()>,
    doc: &mut Doc,
    cx: &mut ActionCx,
) -> Result<(), crate::ModelError> {
    R::edit_fields(doc, &ROOT, edit.path(), edit.op(), cx)
}

pub(crate) struct RecordOf<R>(PhantomData<R>);

impl<R: Fields + 'static> RecordOf<R> {
    pub(crate) const OPS: RecordOps = RecordOps {
        shape: <R as crate::Model>::SHAPE,
        name: std::any::type_name::<R>,
        kind: std::any::TypeId::of::<R>,
        read: read_record::<R>,
        write: write_record::<R>,
        write_changed: write_changed_record::<R>,
        check: check_record::<R>,
        refresh: refresh_record::<R>,
        diff: diff_records::<R>,
        edit: edit_record::<R>,
    };
}

/// Apply `operation`, a field edit, at its path.
fn apply_edit(
    operation: &dyn Performs,
    doc: &mut Doc,
    cx: &mut ActionCx,
) -> Result<(), ActionError> {
    match operation.apply_edit(doc, cx) {
        Some(applied) => applied.map_err(model_refused),
        None => Ok(()),
    }
}

/// One operation of a record, as [`apply_operation`] performs it.
trait Performs {
    fn is_edit(&self) -> bool;
    /// Apply it as the field edit it is; `None` when it is a verb.
    fn apply_edit(&self, doc: &mut Doc, cx: &mut ActionCx)
    -> Option<Result<(), crate::ModelError>>;
    fn queues(&self) -> bool;
    fn perform(&self, record: &mut dyn Any, cx: &mut ActionCx) -> Result<(), Refusal>;
    fn queue(&self, record: &mut dyn Any, before: &dyn Any, cx: &mut ActionCx);
    fn perform_item(&self, doc: &mut Doc, cx: &mut ActionCx) -> Option<Result<(), Refusal>>;
    fn writes(&self) -> crate::footprint::Writes;
    fn snapshot(&self, record: &dyn Any) -> Option<Box<dyn Any>>;
}

impl<O: Operation> Performs for O {
    fn is_edit(&self) -> bool {
        self.edit().is_some()
    }

    fn apply_edit(
        &self,
        doc: &mut Doc,
        cx: &mut ActionCx,
    ) -> Option<Result<(), crate::ModelError>> {
        let edit = self.edit()?;
        Some(O::Record::edit_fields(
            doc,
            &ROOT,
            edit.path(),
            edit.op(),
            cx,
        ))
    }

    fn queues(&self) -> bool {
        O::QUEUES && !settles(self)
    }

    fn perform(&self, record: &mut dyn Any, cx: &mut ActionCx) -> Result<(), Refusal> {
        match record.downcast_mut::<O::Record>() {
            Some(record) => Operation::perform(self, record, cx),
            None => Err(Refusal::new("a record of another type")),
        }
    }

    fn queue(&self, record: &mut dyn Any, before: &dyn Any, cx: &mut ActionCx) {
        if let (Some(record), Some(before)) = (
            record.downcast_mut::<O::Record>(),
            before.downcast_ref::<O::Record>(),
        ) {
            O::queue(record, before, cx);
        }
    }

    fn perform_item(&self, doc: &mut Doc, cx: &mut ActionCx) -> Option<Result<(), Refusal>> {
        Operation::perform_item(self, doc, cx)
    }

    fn writes(&self) -> crate::footprint::Writes {
        Operation::writes(self)
    }

    fn snapshot(&self, record: &dyn Any) -> Option<Box<dyn Any>> {
        let record = record.downcast_ref::<O::Record>()?;
        O::snapshot(record).map(|record| Box::new(record) as Box<dyn Any>)
    }
}

/// [`Stamped::apply`] of `operation`, of a record of the type `record`,
/// encoded as `payload` at `now_ms`.
///
/// The record the previous action of this type left behind, brought up to
/// date with only the changes since, stands in for reading the whole
/// record, and the write compares with it instead of reading every field
/// back. Either way the action writes exactly the operations a whole read
/// and write would, so a replica replaying it, warm or cold, makes the
/// identical change.
#[allow(
    clippy::too_many_lines,
    reason = "one apply: an edit, an item verb or a record verb, each written and checked"
)]
fn apply_operation(
    record: &'static RecordOps,
    operation: &dyn Performs,
    (payload, now_ms): (&[u8], i64),
    doc: &mut Doc,
) -> Result<(), ActionError> {
    // A group's members mint into one list; anything else starts afresh.
    if crate::group::current_member().is_none() {
        drop(take_minted());
    }
    let mut cx = ActionCx::for_apply(doc, payload, now_ms);
    let shape = record.shape;
    let units = crate::footprint::has_units(shape);
    // The unit index, kept once something applied: a refused action
    // writes nothing.
    let index = |doc: &mut Doc| {
        if units {
            crate::footprint::index(doc, shape).map_err(automerge_refused)?;
        }
        Ok::<(), ActionError>(())
    };
    let queues = operation.queues();
    if operation.is_edit() {
        if !queues {
            apply_edit(operation, doc, &mut cx)?;
            return index(doc);
        }
        // The record's `queue` sees the change the edit made.
        let before = (record.read)(doc).map_err(model_refused)?;
        apply_edit(operation, doc, &mut cx)?;
        let mut after = (record.read)(doc).map_err(model_refused)?;
        let minting = MintScope::enter(&cx);
        operation.queue(after.as_mut(), before.as_ref(), &mut cx);
        drop(minting);
        (record.write)(after.as_ref(), doc).map_err(model_refused)?;
        return index(doc);
    }
    // An item method reads and writes its item alone; what it wrote is
    // its item's, within its footprint.
    if !queues {
        let minting = MintScope::enter(&cx);
        let performed = operation.perform_item(doc, &mut cx);
        drop(minting);
        if let Some(performed) = performed {
            performed.map_err(refusal_refused)?;
            return index(doc);
        }
    }
    // The record the previous action of this type left behind, brought
    // up to date with only the changes since, stands in for a whole read.
    let committed = automerge::transaction::Transactable::pending_ops(doc) == 0;
    let started = automerge::transaction::Transactable::base_heads(doc);
    let (mut current, dirty) =
        match crate::apply_cache::take_any((record.kind)(), doc, &record.refresh) {
            Some((warm, dirty)) => (warm, Some(dirty)),
            None => ((record.read)(doc).map_err(model_refused)?, None),
        };
    // What it was: for the record's `queue`, to hold what the verb
    // produced against its footprint, and, when it starts from a
    // committed version, for the next action to start from.
    let kept = if committed || queues || units {
        operation.snapshot(current.as_ref())
    } else {
        None
    };
    let read_before;
    let before: Option<&dyn Any> = match (&kept, queues || units) {
        (_, false) => None,
        (Some(kept), true) => Some(kept.as_ref()),
        (None, true) => {
            read_before = (record.read)(doc).map_err(model_refused)?;
            Some(read_before.as_ref())
        }
    };
    let minting = MintScope::enter(&cx);
    let performed = operation.perform(current.as_mut(), &mut cx);
    if let (Ok(()), Some(before), true) = (&performed, before, queues) {
        operation.queue(current.as_mut(), before, &mut cx);
    }
    drop(minting);
    performed.map_err(refusal_refused)?;
    if crate::group::grouping() {
        // The group checks the record once, at its end.
        crate::group::check_or_defer(format!("{}@root", (record.name)()), doc, move |doc| {
            match (record.read)(doc) {
                Ok(read) => (record.check)(read.as_ref()),
                Err(error) => Err(Refusal::new(error.to_string())),
            }
        })
        .map_err(refusal_refused)?;
    } else {
        (record.check)(current.as_ref()).map_err(refusal_refused)?;
    }
    if let (Some(before), true) = (before, units) {
        let edits = (record.diff)(before, current.as_ref());
        crate::footprint::check_edits(shape, &edits, &operation.writes())
            .map_err(refusal_refused)?;
    }
    match (&kept, &dirty) {
        (Some(kept), Some(dirty)) => {
            (record.write_changed)(current.as_ref(), kept.as_ref(), doc, dirty)
        }
        _ => (record.write)(current.as_ref(), doc),
    }
    .map_err(model_refused)?;
    index(doc)?;
    // Kept once all it wrote is written: the next action finds this
    // one's change by its size.
    if let (Some(kept), true) = (kept, committed) {
        crate::apply_cache::keep_any((record.kind)(), kept, doc, &started);
    }
    Ok(())
}

impl<O: Operation> tilo_actions::Action for Stamped<O> {
    /// Read the record, run the method, write back what changed
    /// ([`apply_operation`]).
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload = serde_json::to_vec(self).map_err(ActionError::Encode)?;
        apply_operation(
            &RecordOf::<O::Record>::OPS,
            &self.operation,
            (&payload, self.now_ms),
            doc,
        )
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    fn risk(&self) -> RiskLevel {
        self.operation.risk()
    }

    /// An outbox settlement is content from outside and forgetting a
    /// settled entry is housekeeping: neither is anyone's undo step, so an
    /// undo never sends a mail twice.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self.operation.edit().map(crate::Edit::op) {
            Some(crate::EditOp::Settle { .. }) => tilo_actions::ChangeKind::External,
            Some(crate::EditOp::Forget { .. }) => tilo_actions::ChangeKind::Housekeeping,
            _ => self.operation.change_kind(),
        }
    }

    fn action_kind(&self) -> ActionKind {
        self.operation.kind()
    }

    /// A verb that declares its undo is undone by applying that verb, and a
    /// change of a record that queues by applying the edits that restore it:
    /// either queues the compensating outside consequence.
    fn inverse_encoded(&self, doc: &Doc) -> Option<tilo_actions::EncodedAction> {
        self.compensation(doc)
    }

    /// A field edit with the value before and after, or a verb with its
    /// plain arguments, and the item either addressed.
    fn describe(&self, doc: &Doc) -> Option<tilo_actions::Described> {
        let shape = <O::Record as crate::Model>::SHAPE;
        if let Some(edit) = self.operation.edit() {
            return crate::describe::edit(doc, shape, edit);
        }
        let value = crate::__private::json_via_bytes(&self.operation).ok()?;
        crate::describe::verb(doc, shape, O::KIND, &value)
    }
}

/// A typed reference to one declared action of `R` taking input `A` — the
/// thing views bind controls to and tests apply. Generated by `#[resource]`
/// as an associated constant per action (`Note::EDIT`).
///
/// `A` is the action's argument, or a tuple of its arguments in order.
pub struct ActionRef<R, A> {
    pub id: &'static str,
    pub params: &'static [&'static str],
    marker: PhantomData<fn(A) -> R>,
}

impl<R, A> Clone for ActionRef<R, A> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R, A> Copy for ActionRef<R, A> {}

impl<R, A> std::fmt::Debug for ActionRef<R, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionRef")
            .field("id", &self.id)
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl<R, A: Serialize> ActionRef<R, A> {
    #[must_use]
    pub const fn new(id: &'static str, params: &'static [&'static str]) -> Self {
        Self {
            id,
            params,
            marker: PhantomData,
        }
    }

    /// The stored form of this reference within the app `app`.
    #[must_use]
    pub fn key(&self, app: &str) -> ActionKey
    where
        R: Resource,
    {
        ActionKey::new(app, R::KIND, self.id)
    }

    /// The JSON arguments this input binds to: a verb's parameter struct
    /// as its fields, or positional inputs under the declared names.
    ///
    /// # Errors
    /// Returns why the input does not serialize, or does not have the shape
    /// the declared names ask for.
    pub fn arguments(
        &self,
        input: &A,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let id = self.id;
        // Through bytes: the serializer writing recorded actions serves
        // here too, rather than one more per parameter type.
        let value = match crate::__private::json_via_bytes(input) {
            Ok(value) => value,
            Err(error) => return Err(format!("`{id}`: {error}")),
        };
        bind_arguments(id, self.params, value)
    }

    /// Fully apply the input: an invocation with nothing left to supply.
    /// A verb's parameter struct converts from its parameters in order: one
    /// value, or a tuple of several. An input that does not bind makes an
    /// invocation that refuses with why ([`Invoke::refusal`]), never one
    /// with empty arguments.
    pub fn with(&self, input: impl Into<A>) -> Invoke<R> {
        let (arguments, refusal) = match self.arguments(&input.into()) {
            Ok(arguments) => (arguments, None),
            Err(refusal) => (serde_json::Map::new(), Some(refusal)),
        };
        Invoke {
            id: self.id,
            arguments,
            refusal,
            effect: false,
            marker: PhantomData,
        }
    }

    /// [`Self::with`], reporting an input that does not bind at once.
    ///
    /// # Errors
    /// Returns why the input does not bind.
    pub fn try_with(&self, input: impl Into<A>) -> Result<Invoke<R>, String> {
        let invoke = self.with(input);
        match invoke.refusal {
            Some(refusal) => Err(refusal),
            None => Ok(invoke),
        }
    }
}

/// The arguments `value` (an input in JSON form) binds to under the
/// declared parameter names: one compiled copy for every action reference.
fn bind_arguments(
    id: &str,
    params: &[&str],
    value: serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    // An absent option is left out, as a parameter not given.
    let given = |fields: Vec<(String, serde_json::Value)>| {
        fields
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .collect()
    };
    match (params, value) {
        ([], serde_json::Value::Object(fields)) => Ok(given(fields.into_iter().collect())),
        ([], serde_json::Value::Null) => Ok(serde_json::Map::new()),
        ([], other) => Err(format!("`{id}` takes named arguments, not `{other}`")),
        ([single], value) => Ok(given(vec![((*single).to_owned(), value)])),
        (names, serde_json::Value::Array(values)) if values.len() == names.len() => Ok(given(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .zip(values)
                .collect(),
        )),
        (names, other) => Err(format!(
            "`{id}` takes {} arguments ({}), not `{other}`",
            names.len(),
            names.join(", ")
        )),
    }
}

impl<R, A: Serialize> ActionRef<R, A> {
    /// Adapt a control's input to this action's arguments — typically to bind
    /// the item a row stands for: `Todo::SET_DONE.map(move |done| (id.clone(), done))`.
    pub fn map<I, T, F>(self, map: F) -> Mapped<R, A, F>
    where
        F: Fn(I) -> T,
        T: Into<A>,
    {
        Mapped { action: self, map }
    }
}

/// An [`ActionRef`] behind an input adapter. See [`ActionRef::map`].
pub struct Mapped<R, A, F> {
    action: ActionRef<R, A>,
    map: F,
}

/// Something a control with input `I` can be bound to: an action and how the
/// control's value becomes that action's arguments.
pub trait Binds<R, I>: Send + Sync + 'static {
    fn action_id(&self) -> &'static str;

    /// The arguments `input` binds to.
    ///
    /// # Errors
    /// Returns why `input` does not bind; the control invokes nothing.
    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String>;
}

/// A control binds an action whose input its value converts to: a text
/// field binds a verb taking one `String`.
impl<R: 'static, A: Serialize + 'static, I: Into<A>> Binds<R, I> for ActionRef<R, A> {
    fn action_id(&self) -> &'static str {
        self.id
    }
    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        ActionRef::arguments(self, &input.into())
    }
}

impl<R, A, I, T, F> Binds<R, I> for Mapped<R, A, F>
where
    R: 'static,
    A: Serialize + 'static,
    T: Into<A>,
    F: Fn(I) -> T + Send + Sync + 'static,
{
    fn action_id(&self) -> &'static str {
        self.action.id
    }
    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        self.action.arguments(&(self.map)(input).into())
    }
}

/// The input of an action that takes nothing: `()`, or a verb's empty
/// parameter struct.
pub trait NoParams {}

impl NoParams for () {}

/// A verb's generated parameter struct, linked to the verb it is the input
/// of. A form is a local value of it; submitting the form invokes
/// [`Self::ACTION`] with it.
pub trait Params: Serialize + DeserializeOwned + crate::JsonSchema + Clone + 'static {
    /// The resource the verb belongs to.
    type Resource;
    /// The typed paths into the parameters, one per parameter.
    type Paths: Clone;
    /// The verb.
    const ACTION: ActionRef<Self::Resource, Self>;

    /// The typed paths into the parameters.
    fn paths() -> Self::Paths;
}

impl<R, A: NoParams> ActionRef<R, A> {
    /// The invocation of a nullary action.
    pub fn invoke(&self) -> Invoke<R> {
        Invoke {
            id: self.id,
            arguments: serde_json::Map::new(),
            refusal: None,
            effect: false,
            marker: PhantomData,
        }
    }
}

/// One fully specified invocation of a declared action, or of one of the
/// app's effects ([`Invoke::effect`]).
pub struct Invoke<R> {
    pub id: &'static str,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Why the input it was built from does not bind, if it does not.
    refusal: Option<String>,
    effect: bool,
    marker: PhantomData<fn() -> R>,
}

impl<R> Clone for Invoke<R> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            arguments: self.arguments.clone(),
            refusal: self.refusal.clone(),
            effect: self.effect,
            marker: PhantomData,
        }
    }
}

impl<R> std::fmt::Debug for Invoke<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invoke")
            .field("id", &self.id)
            .field("arguments", &self.arguments)
            .field("refusal", &self.refusal)
            .field("effect", &self.effect)
            .finish()
    }
}

impl<R> Invoke<R> {
    /// An invocation of one of the app's effects — show an item, refresh a
    /// list — through the same door as its actions: what a launch asks of
    /// an app beside editing its resource (`tilo-store:app/<id>` showing that
    /// app). It changes no document, so it never builds an action.
    pub fn effect<E: crate::Effect>(effect: &E) -> Self {
        Self {
            id: effect.id(),
            arguments: effect.arguments(),
            refusal: None,
            effect: true,
            marker: PhantomData,
        }
    }

    /// Why the input this invocation was built from does not bind: such an
    /// invocation refuses wherever it is submitted.
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    /// Whether this invokes an effect ([`Self::effect`]) rather than one of
    /// the resource's actions.
    pub fn is_effect(&self) -> bool {
        self.effect
    }
}

impl<R: Resource> Invoke<R> {
    /// An invocation of the generic [`APPLY`](crate::APPLY) action for one
    /// typed action value — the path for edits without a named declaration.
    ///
    /// # Errors
    /// Returns the serialization error of `action`.
    pub fn apply(action: &R::Action) -> Result<Self, serde_json::Error> {
        // A unit variant serializes as its name; written as a map it reads
        // back the same, and is the object the action's parameter takes.
        let value = match serde_json::to_value(action)? {
            serde_json::Value::String(variant) => serde_json::Value::Object(
                serde_json::Map::from_iter([(variant, serde_json::Value::Null)]),
            ),
            value => value,
        };
        Ok(Self {
            id: crate::APPLY,
            arguments: serde_json::Map::from_iter([(crate::APPLY_PARAM.to_owned(), value)]),
            refusal: None,
            effect: false,
            marker: PhantomData,
        })
    }

    /// An invocation of the field edit `edit` ([`FIELD_EDIT`](crate::FIELD_EDIT)).
    ///
    /// # Errors
    /// Returns the serialization error of `edit`.
    pub fn edit(edit: &crate::Edit<R>) -> Result<Self, serde_json::Error> {
        Ok(Self {
            id: crate::FIELD_EDIT,
            arguments: serde_json::Map::from_iter([(
                crate::EDIT_PARAM.to_owned(),
                serde_json::to_value(edit)?,
            )]),
            refusal: None,
            effect: false,
            marker: PhantomData,
        })
    }

    /// The field edit `edit`, or an invocation that refuses when it does
    /// not encode: [`Self::edit`] where an `Invoke` is expected
    /// (`replica.apply(paths.count.set(42))`).
    fn from_edit(edit: &crate::Edit<R>) -> Self {
        Self::edit(edit).unwrap_or_else(|error| Self {
            id: crate::FIELD_EDIT,
            arguments: serde_json::Map::new(),
            refusal: Some(error.to_string()),
            effect: false,
            marker: PhantomData,
        })
    }

    /// Several invocations applied as one action: one change, one version,
    /// one undo step. Members that are groups themselves are flattened.
    pub fn group(members: impl IntoIterator<Item = Self>) -> Self {
        let mut actions = Vec::new();
        let mut refusal = None;
        for member in members {
            if refusal.is_none() {
                refusal.clone_from(&member.refusal);
            }
            if member.is_group() {
                if let Some(serde_json::Value::Array(inner)) =
                    member.arguments.get(crate::GROUP_PARAM)
                {
                    actions.extend(inner.iter().cloned());
                }
            } else {
                actions.push(serde_json::json!({
                    "action": member.id,
                    "args": member.arguments,
                }));
            }
        }
        Self {
            id: crate::GROUP,
            arguments: serde_json::Map::from_iter([(
                crate::GROUP_PARAM.to_owned(),
                serde_json::Value::Array(actions),
            )]),
            refusal,
            effect: false,
            marker: PhantomData,
        }
    }

    /// Whether this is a [`group`](Self::group) of invocations.
    pub fn is_group(&self) -> bool {
        self.id == crate::GROUP
    }

    /// Build the replayable action on this (origin) machine. A group builds
    /// with [`build_group`](crate::group::build_group).
    ///
    /// # Errors
    /// Returns why the arguments do not bind.
    pub fn build(&self, now_ms: i64) -> Result<R::Action, String> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        crate::resource::build_action::<R>(self.id, &self.arguments, now_ms)
    }
}

impl<R: Resource> From<crate::Edit<R>> for Invoke<R> {
    fn from(edit: crate::Edit<R>) -> Self {
        Self::from_edit(&edit)
    }
}

impl<R: Resource> From<&crate::Edit<R>> for Invoke<R> {
    fn from(edit: &crate::Edit<R>) -> Self {
        Self::from_edit(edit)
    }
}

#[cfg(test)]
mod seed_tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    #[test]
    fn a_seed_read_mid_transaction_leaves_it_open_and_moves_with_it() {
        let mut doc = Doc::new();
        doc.put(ROOT, "a", 1).unwrap();
        doc.commit();
        let committed = seed(&doc, b"test", b"payload");
        doc.put(ROOT, "b", 2).unwrap();
        let base = doc.base_heads();
        let open = seed(&doc, b"test", b"payload");
        assert!(doc.pending_ops() > 0, "reading the seed commits nothing");
        assert_eq!(doc.base_heads(), base);
        assert_ne!(open, committed, "an open transaction moves the seed");
        doc.put(ROOT, "c", 3).unwrap();
        assert_ne!(seed(&doc, b"test", b"payload"), open);
        doc.commit();
        assert_ne!(seed(&doc, b"test", b"payload"), committed);
    }
}
