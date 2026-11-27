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
use tilo_sort::CompactSort as _;

use automerge::ROOT;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, ExecutionLocus, RiskLevel};
use tilo_member_types::MemberId;

pub use crate::apply_cache::forget_record;

use crate::collection::Id;
use crate::kind::ResourceKind;
use crate::model::{Doc, Fields};
use crate::resource::Resource;

/// Deterministic services available while an action applies.
///
/// Everything here is a pure function of the action and the document it was
/// authored against, so the origin and every replaying replica compute the
/// same ids and see the same time.
#[derive(Debug)]
pub struct ActionCx {
    preview: bool,
    seed: [u8; 32],
    minted: u64,
    /// The ids [`Self::id`] minted, in order.
    ids: Vec<Id>,
    now_ms: i64,
    author: Option<MemberId>,
    invocation: Option<tilo_actions::InvocationId>,
    /// An item method removes the item it acts on ([`Self::remove_item`]).
    removes_item: bool,
}

/// Who invoked the action applying now, as its envelope pinned it.
fn invoking_author() -> Option<MemberId> {
    tilo_actions::reading_invoking_context(|context| {
        context
            .and_then(|context| context.invoker.as_ref())
            .and_then(invoker_member)
    })
    .or_else(|| RESOURCE_ACTOR.with_borrow(Clone::clone))
}

/// The member an invoker acts as: the member itself, or for an automation the
/// member it runs for.
fn invoker_member(invoker: &tilo_actions::ActionInvoker) -> Option<MemberId> {
    invoker
        .acting
        .members()
        .first()
        .map(|member| (*member).clone())
}

/// Who writes now: the invoker the envelope of the action applying now
/// pinned; outside an action, the actor a record write is lent. Inside an
/// action only its envelope counts, so every replica replaying it reads the
/// same author.
pub(crate) fn writing_author() -> Option<MemberId> {
    tilo_actions::reading_invoking_context(|context| match context {
        Some(context) => context.invoker.as_ref().and_then(invoker_member),
        None => RESOURCE_ACTOR.with_borrow(Clone::clone),
    })
}

thread_local! {
    static RESOURCE_ACTOR: std::cell::RefCell<Option<MemberId>> = const { std::cell::RefCell::new(None) };
}

/// Attribution lent by the SDK's checked record callback. Compensation is
/// calculated before an action envelope is installed, but must still see the
/// actual admitted actor rather than an anonymous ambient host.
///
/// This supplies attribution only, never permission to access host resources.
pub struct ResourceActorScope(Option<MemberId>);

impl ResourceActorScope {
    /// Lend the actor admitted by the record host for this callback.
    #[must_use]
    pub fn enter(actor: MemberId) -> Self {
        Self(RESOURCE_ACTOR.replace(Some(actor)))
    }
}

impl Drop for ResourceActorScope {
    fn drop(&mut self) {
        RESOURCE_ACTOR.replace(self.0.take());
    }
}

fn invoking_invocation() -> Option<tilo_actions::InvocationId> {
    tilo_actions::reading_invoking_context(|context| context?.source_invocation)
}

impl ActionCx {
    /// Remove the item the item method running now acts on
    /// (`#[action(item = field)]` with the item as its first parameter): what
    /// it changed of the item is let go with it. The item is read and removed
    /// alone, never the collection holding it, however many items that holds.
    pub fn remove_item(&mut self) {
        self.removes_item = true;
    }

    /// Whether the item method that ran asked for its item to be removed
    /// ([`Self::remove_item`]), forgetting it.
    #[doc(hidden)]
    pub fn take_item_removal(&mut self) -> bool {
        std::mem::take(&mut self.removes_item)
    }

    /// A context seeded from the document's heads and actor (both pinned by
    /// the executor) and the action's own payload.
    ///
    /// Inside an [`ActionGroup`](crate::ActionGroup) the group's seed and the
    /// member's position stand in for the heads, which a member cannot read
    /// without committing the members before it as separate changes.
    #[must_use]
    pub fn for_apply(doc: &Doc, payload: &[u8], now_ms: i64) -> Self {
        Self {
            preview: tilo_actions::executor::is_replaying(),
            seed: seed(doc, b"tilo-resource/action-seed/v1", payload),
            minted: 0,
            ids: Vec::new(),
            now_ms,
            author: invoking_author(),
            invocation: invoking_invocation(),
            removes_item: false,
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
            heads.compact_sort();
            for head in heads {
                hasher.update(&head.0);
            }
        }
        hasher.update(payload);
        Self {
            preview: tilo_actions::executor::is_replaying(),
            seed: *hasher.finalize().as_bytes(),
            minted: 0,
            ids: Vec::new(),
            now_ms: 0,
            author: invoking_author(),
            invocation: invoking_invocation(),
            removes_item: false,
        }
    }

    /// Whether the runtime is evaluating compensation on a private record copy
    /// or replaying an existing change. Neither may issue host-local approvals
    /// or repeat outside effects; only a new invocation admits those.
    #[must_use]
    pub fn is_preview(&self) -> bool {
        self.preview
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
        let uuid = minted_uuid(&self.seed, self.minted);
        self.minted = self.minted.saturating_add(1);
        note_minted(&uuid.to_string());
        uuid
    }

    /// Name the item this change is about, when the record holds no item for
    /// it: the file a device will write, the entry the action queued. History
    /// says the change by `label`, which is what a person reads, under `id`:
    /// the id the change carries, which need not be a resource the app can
    /// open. A change that makes or edits an item of the record itself says
    /// so already, and keeps that item.
    pub fn named(&mut self, id: impl Into<String>, label: impl Into<String>) {
        NAMED.with(|named| {
            named.borrow_mut().replace(tilo_actions::ActionItem {
                id: id.into(),
                label: label.into(),
            });
        });
    }

    /// The name software acting for someone (an agent, an app) acted under, as
    /// the host that admitted it named it; empty for a person.
    #[must_use]
    pub fn acting_name(&self) -> String {
        invoking_name()
    }

    /// The name and avatar a bot showed for this action instead of its own,
    /// as the envelope pinned it; `None` for anyone else's action.
    #[must_use]
    pub fn appearance(&self) -> Option<tilo_actions::Appearance> {
        tilo_actions::reading_invoking_context(|context| {
            context?
                .invoker
                .as_ref()
                .filter(|invoker| invoker.acting.is_bot())?
                .appearance
                .clone()
        })
    }

    /// Who invoked the action (a person, an agent, an automation), as the
    /// envelope pinned it.
    #[must_use]
    pub fn invoked_by(&self) -> Option<tilo_actions::Acting> {
        tilo_actions::reading_invoking_context(|context| {
            Some(context?.invoker.as_ref()?.acting.clone())
        })
    }

    /// When the action was authored, in milliseconds since the Unix epoch, as
    /// read once on the origin.
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// Who the action is by: the person or agent that invoked it, the same
    /// id that member's own views read as `me`. Pinned with the action
    /// like its time, so every replica reads the same author; the payload
    /// need not carry it. `None` for an action applied with no invoker
    /// (an edit written straight into a document outside the host).
    pub fn author(&self) -> Option<&MemberId> {
        self.author.as_ref()
    }

    /// The invocation admitted by the action API, pinned in the action
    /// envelope so origin and replay observe the same correlation identity.
    /// This is not caller authority; use [`Self::author`] for that.
    pub const fn invocation(&self) -> Option<tilo_actions::InvocationId> {
        self.invocation
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
    /// The entries the action applying last on this thread made in a
    /// collection: the collection map each item's id was made a key of.
    static ENTRIES: std::cell::RefCell<std::collections::HashMap<String, automerge::ObjId>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// The item the action applying last on this thread said its change is
    /// about, when the record holds no item for it (a file on a device): what
    /// history names the change by.
    static NAMED: std::cell::RefCell<Option<tilo_actions::ActionItem>> =
        const { std::cell::RefCell::new(None) };
}

/// An action starts applying on this thread: nothing minted or made before
/// it is its own.
pub(crate) fn start_minting() {
    drop(take_minted());
    ENTRIES.with(|entries| entries.borrow_mut().clear());
    NAMED.with(|named| named.borrow_mut().take());
}

/// Note that the action applying on this thread made the entry of the item
/// `id` in the collection map `map`.
pub(crate) fn note_entry(map: &automerge::ObjId, id: &str) {
    ENTRIES.with(|entries| {
        entries.borrow_mut().insert(id.to_owned(), map.clone());
    });
}

/// The collection map the action applied last on this thread made the
/// entry of the item `id` in, if it made one: where whoever answers what it
/// made finds the item, without reading the change it wrote.
#[must_use]
pub fn minted_entry(id: &str) -> Option<automerge::ObjId> {
    ENTRIES.with(|entries| entries.borrow().get(id).cloned())
}

/// The item the action applying last on this thread named as its change's
/// ([`ActionCx::named`]), if it named one.
pub(crate) fn named_item() -> Option<tilo_actions::ActionItem> {
    NAMED.with_borrow(Clone::clone)
}

/// The ids of the entries the action applying last on this thread made in a
/// collection, minted or handed to it, in no order.
pub(crate) fn entries_so_far() -> Vec<String> {
    ENTRIES.with_borrow(|entries| entries.keys().cloned().collect())
}

/// The ids the action applying last on this thread minted, as it minted
/// them: for reading what it made, without taking them from whoever answers
/// its caller ([`take_minted`]).
pub(crate) fn minted_so_far() -> Vec<String> {
    MINTED.with_borrow(Clone::clone)
}

/// Note `id` among the ids the action applying on this thread minted, for
/// whichever caller answers what it made.
pub(crate) fn note_minted(id: &str) {
    MINTED.with(|minted| minted.borrow_mut().push(id.to_owned()));
}

/// The ids the record action applied last on this thread minted (items it
/// added, a group's members' together), taken: what the API answers its
/// caller with beside the action's own answer.
#[must_use]
pub fn take_minted() -> Vec<String> {
    MINTED.with(|minted| std::mem::take(&mut *minted.borrow_mut()))
}

/// Note `ids` again, which [`take_minted`] took: what a layer that took them to answer its own
/// caller puts back, so the caller that invoked the action learns what it made too.
pub fn restore_minted(ids: impl IntoIterator<Item = String>) {
    MINTED.with(|minted| minted.borrow_mut().extend(ids));
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

/// Who invoked the action applying now, as its envelope pinned it: what
/// [`ActionCx::author`] reads, for domain code a verb calls without its
/// context.
#[must_use]
pub fn invoking_member() -> Option<MemberId> {
    invoking_author()
}

/// The name software acting for someone (an agent, an app) acted under in
/// the action applying now: what [`ActionCx::acting_name`] reads, for domain
/// code a verb calls without its context; empty for a person.
#[must_use]
pub fn invoking_name() -> String {
    tilo_actions::reading_invoking_context(|context| {
        context
            .and_then(|context| context.invoker.as_ref())
            .filter(|invoker| !invoker.acting.is_person())
            .and_then(|invoker| invoker.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_default()
    })
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

fn minted_uuid(seed: &[u8; 32], counter: u64) -> uuid::Uuid {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"uuid");
    hasher.update(seed);
    hasher.update(&counter.to_le_bytes());
    uuid_from(hasher.finalize().as_bytes())
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
    let seed = envelope_seed(envelope);
    (0_u64..).map(move |counter| minted(&seed, counter))
}

/// What the action `envelope` committed may have minted at each count, in
/// order: the id [`ActionCx::id`] mints there and the UUID [`ActionCx::uuid`]
/// mints there instead, as the action minted one or the other.
pub fn minted_keys(envelope: &tilo_actions::ActionEnvelope) -> impl Iterator<Item = [String; 2]> {
    let seed = envelope_seed(envelope);
    (0_u64..).map(move |counter| {
        [
            minted(&seed, counter).as_str().to_owned(),
            minted_uuid(&seed, counter).to_string(),
        ]
    })
}

/// The seed the action `envelope` committed minted its ids from.
fn envelope_seed(envelope: &tilo_actions::ActionEnvelope) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-resource/action-seed/v1");
    hasher.update(envelope.actor().to_bytes());
    let mut heads = envelope.base_heads.clone();
    heads.compact_sort();
    for head in &heads {
        hasher.update(head);
    }
    hasher.update(&envelope.action.payload);
    *hasher.finalize().as_bytes()
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
        heads.compact_sort();
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

/// What an action method may return: nothing, or a refusal.
pub trait Outcome {
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

pub use tilo_action_types::ActionKind;

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

    /// Whether the operation records a message someone received.
    /// `#[resource]` generates it from `#[action(incoming_message)]`.
    fn incoming_message(&self) -> bool {
        false
    }

    /// Whether the operation loses what Undo cannot bring back, beside its
    /// document change. `#[resource]` generates it from
    /// `#[action(irreversible)]`.
    fn irreversible(&self) -> bool {
        false
    }

    /// Whether the operation leaves the system for good. `#[resource]`
    /// generates it from `#[action(leaves)]`.
    fn leaves(&self) -> bool {
        false
    }

    /// Whether only the app's own code applies this operation
    /// (`#[action(by_app)]`): it is declared to no caller, and an action API that
    /// decodes one refuses it. `#[resource]` generates it.
    fn by_app(&self) -> bool {
        false
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
#[allow(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands it the error by value"
)]
fn model_refused(error: crate::ModelError) -> ActionError {
    ActionError::Refused(error.to_string())
}

/// A document error as the action's refusal.
#[allow(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands it the error by value"
)]
fn automerge_refused(error: automerge::AutomergeError) -> ActionError {
    ActionError::Refused(error.to_string())
}

pub use tilo_actions::Refusal;

/// A verb's refusal as the action's: one a caller decides on keeps its code.
fn refusal_refused(refusal: Refusal) -> ActionError {
    refusal.into()
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

/// `apply` to a copy of `doc`, dropped here: not generic, so a document's
/// teardown is compiled once rather than for every resource's actions.
#[inline(never)]
fn on_copy(doc: &Doc, apply: &mut dyn FnMut(&mut Doc)) {
    let mut copy = doc.clone();
    apply(&mut copy);
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
            let mut cx = ActionCx::for_apply(doc, &payload, self.now_ms);
            cx.preview = true;
            let mut after = None;
            on_copy(doc, &mut |copy| {
                after = edit
                    .apply(copy, &mut cx)
                    .ok()
                    .and_then(|()| O::Record::read_fields(copy, &ROOT).ok());
            });
            return Some((after?, cx.ids));
        }
        let mut after = O::Record::read_fields(doc, &ROOT).ok()?;
        let mut cx = ActionCx::for_apply(doc, &payload, self.now_ms);
        cx.preview = true;
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
                crate::group::group_type_of(O::KIND),
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
    deps.compact_sort();
    doc.get_changes(&deps)
        .into_iter()
        .find(|change| {
            change.actor_id().to_hex_string() == actor && {
                let mut of = change.deps().to_vec();
                of.compact_sort();
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
            &ResourceOf::<O::Record>::OPS,
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
    record: &ResourceOps,
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
pub(crate) struct ResourceOps {
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

/// The record of `R` a [`ResourceOps`] function was handed.
fn resource_of<R: 'static>(record: &dyn Any) -> Result<&R, crate::ModelError> {
    match record.downcast_ref::<R>() {
        Some(record) => Ok(record),
        None => Err(crate::ModelError::invalid("a record of another type")),
    }
}

fn read_resource<R: Fields + 'static>(doc: &Doc) -> Result<Box<dyn Any>, crate::ModelError> {
    match R::read_fields(doc, &ROOT) {
        Ok(record) => Ok(Box::new(record)),
        Err(error) => Err(error),
    }
}

fn write_resource<R: Fields + 'static>(
    record: &dyn Any,
    doc: &mut Doc,
) -> Result<(), crate::ModelError> {
    resource_of::<R>(record)?.write_fields(doc, &ROOT)
}

fn write_changed_resource<R: Fields + 'static>(
    record: &dyn Any,
    kept: &dyn Any,
    doc: &mut Doc,
    dirty: &crate::Delta,
) -> Result<(), crate::ModelError> {
    resource_of::<R>(record)?.write_changed_fields(resource_of::<R>(kept)?, doc, &ROOT, dirty)
}

fn check_resource<R: Fields + 'static>(record: &dyn Any) -> Result<(), Refusal> {
    match record.downcast_ref::<R>() {
        Some(record) => crate::Model::check(record),
        None => Err(Refusal::new("a record of another type")),
    }
}

fn refresh_resource<R: Fields + 'static>(
    record: &mut dyn Any,
    doc: &Doc,
    delta: &crate::Delta,
) -> bool {
    match record.downcast_mut::<R>() {
        Some(record) => record.refresh_fields(doc, &ROOT, delta).is_ok(),
        None => false,
    }
}

fn diff_resources<R: Fields + 'static>(from: &dyn Any, to: &dyn Any) -> Vec<crate::Edit<()>> {
    let mut edits = Vec::new();
    if let (Some(from), Some(to)) = (from.downcast_ref::<R>(), to.downcast_ref::<R>()) {
        crate::Model::diff(from, to, &[], &mut edits);
    }
    edits
}

fn edit_resource<R: Fields + 'static>(
    edit: &crate::Edit<()>,
    doc: &mut Doc,
    cx: &mut ActionCx,
) -> Result<(), crate::ModelError> {
    R::edit_fields(doc, &ROOT, edit.path(), edit.op(), cx)
}

pub(crate) struct ResourceOf<R>(PhantomData<R>);

impl<R: Fields + 'static> ResourceOf<R> {
    pub(crate) const OPS: ResourceOps = ResourceOps {
        shape: <R as crate::Model>::SHAPE,
        name: std::any::type_name::<R>,
        kind: std::any::TypeId::of::<R>,
        read: read_resource::<R>,
        write: write_resource::<R>,
        write_changed: write_changed_resource::<R>,
        check: check_resource::<R>,
        refresh: refresh_resource::<R>,
        diff: diff_resources::<R>,
        edit: edit_resource::<R>,
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

/// Apply the field edit `operation` to the document; a record that queues
/// on its changes has its `queue` see the change the edit made.
fn apply_edit_queued(
    record: &'static ResourceOps,
    operation: &dyn Performs,
    doc: &mut Doc,
    cx: &mut ActionCx,
) -> Result<(), ActionError> {
    if !operation.queues() {
        return apply_edit(operation, doc, cx);
    }
    let before = (record.read)(doc).map_err(model_refused)?;
    apply_edit(operation, doc, cx)?;
    let mut after = (record.read)(doc).map_err(model_refused)?;
    let minting = MintScope::enter(cx);
    operation.queue(after.as_mut(), before.as_ref(), cx);
    drop(minting);
    (record.write)(after.as_ref(), doc).map_err(model_refused)
}

/// Everything `doc` holds, read whole for one action: what an item method
/// never costs, as an observation of it says.
fn read_whole(ops: &'static ResourceOps, doc: &Doc) -> Result<Box<dyn Any>, ActionError> {
    let _whole = crate::profiling::Phase::start("resource.apply_whole_read");
    #[cfg(any(test, feature = "test-support"))]
    if let Some(record) = crate::apply_cache::current_record() {
        let mut counts = WHOLE_READS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = counts.entry(record.to_string()).or_default();
        *count = count.saturating_add(1);
        drop(counts);
    }
    (ops.read)(doc).map_err(model_refused)
}

/// How many actions have read the whole record at `record` (its address as its caller names it)
/// since this process started: what a test asserts a run of writes costs once per record, not
/// once per write.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn whole_reads(record: &str) -> usize {
    WHOLE_READS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(record)
        .copied()
        .unwrap_or_default()
}

/// [`read_whole`]'s counts, by the address of the record read: a test's, and no cost of a write.
#[cfg(any(test, feature = "test-support"))]
static WHOLE_READS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));

/// [`Stamped::apply`] of `operation`, of a record of the type `record`,
/// encoded as `payload` at `now_ms`.
///
/// The record the previous action of this type left behind, brought up to
/// date with only the changes since, stands in for reading the whole
/// record, and the write compares with it instead of reading every field
/// back. Either way the action writes exactly the operations a whole read
/// and write would, so a replica replaying it, warm or cold, makes the
/// identical change.
fn apply_operation(
    record: &'static ResourceOps,
    operation: &dyn Performs,
    (payload, now_ms): (&[u8], i64),
    doc: &mut Doc,
) -> Result<(), ActionError> {
    // A group's members mint into one list; anything else starts afresh.
    if crate::group::current_member().is_none() {
        start_minting();
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
    // A member of a group whose record the group holds in memory runs on it there; a member
    // that writes the document itself (a field edit, a change an outbox queues on) first
    // writes what the group holds, so it starts from the document as the group left it.
    let grouping = crate::group::grouping();
    if grouping {
        if !operation.is_edit() && !queues && held::holds(record) {
            return held::perform(record, operation, &mut cx);
        }
        held::flush(record, doc)?;
    }
    if operation.is_edit() {
        apply_edit_queued(record, operation, doc, &mut cx)?;
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
            None => (read_whole(record, doc)?, None),
        };
    // A group's first member of a record that keeps no outbox starts the record the group
    // holds: every member after it runs on it in memory, and the group writes it once.
    if grouping && !queues {
        // The group checks the record it holds once, as it writes it.
        let base = operation.snapshot(current.as_ref());
        held::start(record, current, base, dirty);
        return held::perform(record, operation, &mut cx);
    }
    // What it was: for the record's `queue`, to hold what the verb
    // produced against its footprint, and for the next action to start from.
    let kept = if committed || queues || units || dirty.is_some() {
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
    if grouping {
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
    crate::apply_cache::begin_storing();
    match (&kept, &dirty) {
        (Some(kept), Some(dirty)) => {
            (record.write_changed)(current.as_ref(), kept.as_ref(), doc, dirty)
        }
        _ => (record.write)(current.as_ref(), doc),
    }
    .map_err(model_refused)?;
    index(doc)?;
    crate::apply_cache::seal_stored(doc);
    // Kept once all it wrote is written: inside a transaction still open (a caller applying
    // several actions before it commits), the record exactly as the action wrote it, which the
    // next action starts from; else the record it read, which the next action that starts from
    // it brings up to date by this one's change.
    if automerge::transaction::Transactable::pending_ops(doc) > 0 {
        crate::apply_cache::keep_open((record.kind)(), current, doc);
    } else if let Some(kept) = kept {
        crate::apply_cache::keep_any((record.kind)(), kept, doc, &started);
    }
    Ok(())
}

/// The records a group holds in memory while its members run, one per record type: read
/// once by the first member, changed by each member's verb in turn (each with its own
/// arguments, ids and attribution), and written once when the group ends or a member needs
/// the document itself. A thousand members of one record cost one read and one write of it,
/// not a thousand.
mod held {
    use std::any::{Any, TypeId};
    use std::cell::RefCell;

    use tilo_actions::ActionError;

    use super::{ActionCx, MintScope, Performs, ResourceOps, model_refused, refusal_refused};
    use crate::footprint::Footprints;
    use crate::model::Doc;

    struct Held {
        record: &'static ResourceOps,
        /// The record as the members left it.
        current: Box<dyn Any>,
        /// The record as the document held it when the group took it; none when the record
        /// keeps no copy, and the document is read again for it.
        base: Option<Box<dyn Any>>,
        /// What others wrote since the cached record was last written: written again whole.
        /// `None` when the record was read whole, and is written whole.
        dirty: Option<crate::Delta>,
        /// Every unit the members declare.
        footprints: Footprints,
    }

    thread_local! {
        /// `None` outside a group.
        static HELD: RefCell<Option<Vec<Held>>> = const { RefCell::new(None) };
    }

    /// Begin holding records, for the outermost group on this thread.
    pub(crate) fn begin() {
        HELD.with(|held| *held.borrow_mut() = Some(Vec::new()));
    }

    /// Stop holding records without writing them: the group refused.
    pub(crate) fn end() {
        HELD.with(|held| held.borrow_mut().take());
    }

    fn kind(record: &ResourceOps) -> TypeId {
        (record.kind)()
    }

    /// Whether the group holds the record `record`.
    pub(super) fn holds(record: &ResourceOps) -> bool {
        HELD.with(|held| {
            held.borrow()
                .as_ref()
                .is_some_and(|held| held.iter().any(|entry| kind(entry.record) == kind(record)))
        })
    }

    /// Hold `current`, read from the document as `base` (with `dirty` written by others).
    pub(super) fn start(
        record: &'static ResourceOps,
        current: Box<dyn Any>,
        base: Option<Box<dyn Any>>,
        dirty: Option<crate::Delta>,
    ) {
        HELD.with(|held| {
            if let Some(held) = held.borrow_mut().as_mut() {
                held.push(Held {
                    record,
                    current,
                    base,
                    dirty,
                    footprints: Footprints::default(),
                });
            }
        });
    }

    /// Run `operation`'s verb on the record the group holds.
    pub(super) fn perform(
        record: &'static ResourceOps,
        operation: &dyn Performs,
        cx: &mut ActionCx,
    ) -> Result<(), ActionError> {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            let entry = held
                .as_mut()
                .and_then(|held| {
                    held.iter_mut()
                        .find(|entry| kind(entry.record) == kind(record))
                })
                .ok_or_else(|| ActionError::Refused("the group does not hold the record".into()))?;
            let minting = MintScope::enter(cx);
            let performed = operation.perform(entry.current.as_mut(), cx);
            drop(minting);
            performed.map_err(refusal_refused)?;
            entry.footprints.add(&operation.writes());
            Ok(())
        })
    }

    /// Write the record of `record`'s type the group holds, if it holds one, and stop holding
    /// it: what its members did, checked against every unit they declare.
    pub(super) fn flush(record: &ResourceOps, doc: &mut Doc) -> Result<(), ActionError> {
        let taken = HELD.with(|held| {
            let mut held = held.borrow_mut();
            let held = held.as_mut()?;
            let at = held
                .iter()
                .position(|entry| kind(entry.record) == kind(record))?;
            Some(held.remove(at))
        });
        match taken {
            Some(entry) => write(entry, doc, Checked::AtTheEnd),
            None => Ok(()),
        }
    }

    /// Write every record the group holds: the group's end.
    pub(crate) fn flush_all(doc: &mut Doc) -> Result<(), ActionError> {
        let taken = HELD.with(|held| held.borrow_mut().as_mut().map(std::mem::take));
        for entry in taken.unwrap_or_default() {
            write(entry, doc, Checked::Now)?;
        }
        Ok(())
    }

    /// When a held record written is checked.
    #[derive(Clone, Copy)]
    enum Checked {
        /// As it is held: it is written at the group's end.
        Now,
        /// As the document holds it at the group's end: a later member may still mend it.
        AtTheEnd,
    }

    fn write(entry: Held, doc: &mut Doc, checked: Checked) -> Result<(), ActionError> {
        let Held {
            record,
            current,
            base,
            dirty,
            footprints,
        } = entry;
        let units = crate::footprint::has_units(record.shape);
        let read_base;
        let before: Option<&dyn Any> = match (&base, units) {
            (_, false) => None,
            (Some(base), true) => Some(base.as_ref()),
            (None, true) => {
                read_base = (record.read)(doc).map_err(model_refused)?;
                Some(read_base.as_ref())
            }
        };
        // What the members made is a record that holds, whatever it passed through on the way.
        match checked {
            Checked::Now => (record.check)(current.as_ref()).map_err(refusal_refused)?,
            Checked::AtTheEnd => {
                crate::group::check_or_defer(
                    format!("{}@root", (record.name)()),
                    doc,
                    move |doc| match (record.read)(doc) {
                        Ok(read) => (record.check)(read.as_ref()),
                        Err(error) => Err(crate::Refusal::new(error.to_string())),
                    },
                )
                .map_err(refusal_refused)?;
            }
        }
        if let Some(before) = before {
            let edits = (record.diff)(before, current.as_ref());
            crate::footprint::check_edits_of(record.shape, &edits, &footprints)
                .map_err(refusal_refused)?;
        }
        crate::apply_cache::begin_storing();
        match (&base, &dirty) {
            (Some(base), Some(dirty)) => {
                (record.write_changed)(current.as_ref(), base.as_ref(), doc, dirty)
            }
            _ => (record.write)(current.as_ref(), doc),
        }
        .map_err(model_refused)?;
        if units {
            crate::footprint::index(doc, record.shape).map_err(super::automerge_refused)?;
        }
        crate::apply_cache::seal_stored(doc);
        // The next action of its type in the same transaction starts from it as written.
        crate::apply_cache::keep_open((record.kind)(), current, doc);
        Ok(())
    }
}

pub(crate) use held::{begin as hold_records, end as drop_held, flush_all as write_held};

impl<O: Operation> tilo_actions::Action for Stamped<O> {
    /// Read the record, run the method, write back what changed
    /// (`apply_operation`).
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload = serde_json::to_vec(self).map_err(ActionError::Encode)?;
        apply_operation(
            &ResourceOf::<O::Record>::OPS,
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

    fn incoming_message(&self) -> bool {
        self.operation.incoming_message()
    }

    fn irreversible(&self) -> bool {
        self.operation.irreversible()
    }

    fn leaves(&self) -> bool {
        self.operation.leaves()
    }

    /// A verb that declares its undo is undone by applying that verb, and a
    /// change of a record that queues by applying the edits that restore it:
    /// either queues the compensating outside consequence.
    fn inverse_encoded(&self, doc: &Doc) -> Option<tilo_actions::EncodedAction> {
        self.compensation(doc)
    }

    fn compensates(&self) -> bool {
        self.compensated()
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

    /// The item this verb made, read once it wrote: the record's item it
    /// added (a note, an issue, a message), or the item the app named as it
    /// ran (a file a device will write). The document before the verb applied
    /// held neither, so [`Self::describe`] could not name it.
    fn made(&self, doc: &Doc) -> Option<tilo_actions::ActionItem> {
        crate::describe::made::<O::Record>(doc)
    }
}

/// A typed reference to one declared action of `R` taking input `A` — the
/// thing views bind controls to and tests apply. Generated by `#[resource]`
/// and `#[derive(Effect)]` as an associated constant per action
/// (`Note::EDIT`).
///
/// The key [`Self::key`] reads from it is how the framework binds a control:
/// it names the app, the resource kind and the action, so a control cannot
/// bind to an action another resource publishes, and an action a resource
/// stops publishing fails where it is bound rather than greying a button.
///
/// `A` is the action's argument, or a tuple of its arguments in order.
pub struct ActionRef<R, A> {
    pub id: &'static str,
    pub params: &'static [&'static str],
    /// The kind of resource that publishes the action, and so the app that
    /// publishes it: what [`Self::key`] names the action by.
    kind: ResourceKind,
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
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

impl<R, A> ActionRef<R, A> {
    /// The action's public id.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// The names of the action's parameters, in order.
    #[must_use]
    pub const fn params(&self) -> &'static [&'static str] {
        self.params
    }

    /// The resource kind that publishes the action.
    #[must_use]
    pub const fn kind(&self) -> &ResourceKind {
        &self.kind
    }

    /// The stored form of this reference — the app, resource kind and public
    /// id a control is bound to an action by, and the shape a Shortcut step,
    /// an agent's plan or a palette entry keeps.
    #[must_use]
    pub fn key(&self) -> ActionKey {
        ActionKey::new(self.kind.declaring_app(), self.kind.kind(), self.id)
    }
}

impl<E: crate::Effect, A> ActionRef<E, A> {
    /// The action this effect's declaration publishes, as the resource that
    /// offers the vocabulary publishes it: what a control binds to without a
    /// row carrying it.
    #[must_use]
    pub fn spec(&self) -> Option<tilo_actions::ActionSpec> {
        E::spec(self.id)
    }

    /// A control's binding to this effect on `resource`, from its
    /// declaration: the resource's own address and the declared action, with
    /// no carrier row. A nullary effect binds as a button; one taking
    /// arguments is offered with them asked.
    ///
    /// # Errors
    /// The declaration publishes no such action, or it requires an argument
    /// this binding gives none.
    pub fn binding(
        &self,
        resource: impl Into<String>,
    ) -> Result<tilo_actions::ActionBinding<()>, tilo_actions::ActionBindingError> {
        let resource = resource.into();
        let spec = E::spec(self.id).ok_or_else(|| {
            tilo_actions::ActionBindingError::ActionNotExported {
                resource: resource.clone(),
                action: self.id.to_owned(),
            }
        })?;
        tilo_actions::ActionBinding::declared(resource, spec)
    }
}

impl<R, A: Serialize> ActionRef<R, A> {
    /// The reference `#[resource]` and `#[derive(Effect)]` generate: action
    /// `id` taking the parameters `params`, of the kind `kind` declares. The
    /// kind is built by a `const fn` of a type's name alone, so it is the same
    /// value at every use and costs no initialisation.
    #[must_use]
    pub const fn new(
        id: &'static str,
        params: &'static [&'static str],
        kind: ResourceKind,
    ) -> Self {
        Self {
            id,
            params,
            kind,
            marker: PhantomData,
        }
    }

    /// This action as the published resource `resource` offers it (the
    /// record, one of its items, or a resource a view or search hit derives
    /// from it, by its published id): what a search hit invokes or a test
    /// addresses through the host's API. The action's id comes from the
    /// typed reference, so it cannot name one the app does not declare.
    #[must_use]
    pub fn on(&self, resource: impl Into<String>) -> tilo_actions::ActionAddress {
        tilo_actions::ActionAddress::new(resource, self.id)
    }

    /// The JSON arguments this input binds to: a verb's parameter struct
    /// as its fields, or positional inputs under the declared names.
    pub fn arguments(
        &self,
        input: &A,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        // Through bytes: the serializer writing recorded actions serves
        // here too, rather than one more per parameter type.
        bind_serialized(
            self.id,
            self.params,
            crate::__private::json_via_bytes(input),
        )
    }

    /// Fully apply the input: an invocation with nothing left to supply.
    /// A verb's parameter struct converts from its parameters in order: one
    /// value, or a tuple of several. An input that does not bind makes an
    /// invocation that refuses with why ([`Invoke::refusal`]), never one
    /// with empty arguments.
    pub fn with(&self, input: impl Into<A>) -> Invoke<R> {
        Invoke::bound(self.id, self.arguments(&input.into()))
    }

    /// [`Self::with`], reporting an input that does not bind at once.
    pub fn try_with(&self, input: impl Into<A>) -> Result<Invoke<R>, String> {
        let invoke = self.with(input);
        match invoke.refusal {
            Some(refusal) => Err(refusal),
            None => Ok(invoke),
        }
    }
}

/// [`bind_arguments`] of an action input as it serialized, or why it did
/// not: compiled once rather than for every input type.
#[inline(never)]
fn bind_serialized(
    id: &'static str,
    params: &'static [&'static str],
    value: Result<serde_json::Value, serde_json::Error>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match value {
        Ok(value) => bind_arguments(id, params, value),
        Err(error) => Err(format!("`{id}`: {error}")),
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

    /// Adapt a control's input to this action's arguments where the input
    /// may not make any: `parse` reads the input (a draft's text) and makes
    /// the arguments only when it parses, through the types that refuse what
    /// the action would refuse ([`NonBlank`](crate::NonBlank)):
    /// `Tracker::CREATE.try_map(|title: String| Some(Create { title: NonBlank::parse(&title)?, .. }))`.
    ///
    /// An input `parse` makes nothing of invokes nothing; a control bound
    /// to a draft through it is disabled meanwhile. `parse` only reads: it
    /// also runs, as the draft changes, to tell whether the control can act.
    pub fn try_map<I, T, F>(self, parse: F) -> TryMapped<R, A, F>
    where
        F: Fn(I) -> Option<T>,
        T: Into<A>,
    {
        TryMapped {
            action: self,
            parse,
        }
    }
}

/// An [`ActionRef`] behind an input adapter that may make no arguments. See
/// [`ActionRef::try_map`].
pub struct TryMapped<R, A, F> {
    action: ActionRef<R, A>,
    parse: F,
}

impl<R, A, F: Clone> Clone for TryMapped<R, A, F> {
    fn clone(&self) -> Self {
        Self {
            action: self.action,
            parse: self.parse.clone(),
        }
    }
}

/// An [`ActionRef`] behind an input adapter. See [`ActionRef::map`].
pub struct Mapped<R, A, F> {
    action: ActionRef<R, A>,
    map: F,
}

impl<R, A, F: Clone> Clone for Mapped<R, A, F> {
    fn clone(&self) -> Self {
        Self {
            action: self.action,
            map: self.map.clone(),
        }
    }
}

/// Something a control with input `I` can be bound to: an action and how the
/// control's value becomes that action's arguments.
pub trait Binds<R, I>: Send + Sync + 'static {
    fn action_id(&self) -> &'static str;

    /// The key the action is published under — the app, resource kind and
    /// action id — which a control is bound against. A reference carries its
    /// own ([`ActionRef::key`]); anything else is bound as `R`'s own action
    /// under [`R`'s app](Resource::APP_ID) and kind.
    fn action_key(&self) -> ActionKey
    where
        R: Resource,
    {
        ActionKey::new(R::APP_ID, R::KIND, self.action_id())
    }

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
    fn action_key(&self) -> ActionKey {
        ActionRef::key(self)
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
    fn action_key(&self) -> ActionKey {
        ActionRef::key(&self.action)
    }
    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        self.action.arguments(&(self.map)(input).into())
    }
}

impl<R, A, I, T, F> Binds<R, I> for TryMapped<R, A, F>
where
    R: 'static,
    A: Serialize + 'static,
    T: Into<A>,
    F: Fn(I) -> Option<T> + Send + Sync + 'static,
{
    fn action_id(&self) -> &'static str {
        self.action.id
    }
    fn action_key(&self) -> ActionKey {
        ActionRef::key(&self.action)
    }
    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let Some(parsed) = (self.parse)(input) else {
            return Err(no_arguments(self.action.id));
        };
        self.action.arguments(&parsed.into())
    }
}

/// Why a [`TryMapped`] control invoked nothing: written once, not once per
/// adapter.
#[inline(never)]
fn no_arguments(id: &str) -> String {
    format!("the input makes no arguments for `{id}`")
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
            on: None,
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
    /// The published row it acts on ([`Self::on`]), when not the resource.
    on: Option<String>,
    marker: PhantomData<fn() -> R>,
}

impl<R> Clone for Invoke<R> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            arguments: self.arguments.clone(),
            refusal: self.refusal.clone(),
            effect: self.effect,
            on: self.on.clone(),
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
            .field("on", &self.on)
            .finish()
    }
}

impl<R> Invoke<R> {
    /// An invocation of one of the app's effects — show an item, refresh a
    /// list — through the same API as its actions: what a launch asks of
    /// an app beside editing its resource (`tilo-store:app/<id>` showing that
    /// app). It changes no document, so it never builds an action.
    ///
    /// An effect offered only on rows (`#[effect(row, on = Page)]`) acts on
    /// the row it is invoked on: name it with [`Self::on`]. Until then it
    /// refuses, saying so, since the resource does not offer it.
    pub fn effect<E: crate::Effect>(effect: &E) -> Self {
        let id = effect.id();
        let refusal = E::declaration(id)
            .is_some_and(|declaration| declaration.row)
            .then(|| format!("`{id}` is offered only on rows: invoke it on one"));
        Self {
            id,
            arguments: effect.arguments(),
            refusal,
            effect: true,
            on: None,
            marker: PhantomData,
        }
    }

    /// This invocation on the published row `row` (the Browser's page, one
    /// download), which offers its action, rather than on the resource.
    #[must_use]
    pub fn on(mut self, row: impl Into<String>) -> Self {
        if self.effect {
            self.refusal = None;
        }
        self.on = Some(row.into());
        self
    }

    /// The published row it acts on, when [`Self::on`] named one.
    pub fn row(&self) -> Option<&str> {
        self.on.as_deref()
    }

    /// The invocation of action `id` with what its input bound to, or one
    /// that refuses with why it did not bind. Kept out of line: its body is
    /// the same for every resource, so the copies merge into one.
    #[inline(never)]
    pub(crate) fn bound(
        id: &'static str,
        bound: Result<serde_json::Map<String, serde_json::Value>, String>,
    ) -> Self {
        let (arguments, refusal) = match bound {
            Ok(arguments) => (arguments, None),
            Err(refusal) => (serde_json::Map::new(), Some(refusal)),
        };
        Self {
            id,
            arguments,
            refusal,
            effect: false,
            on: None,
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

    /// This invocation's action as the published resource `resource` (an
    /// item of the record, a child, a live row) offers it.
    #[must_use]
    pub fn address_on(&self, resource: impl Into<String>) -> tilo_actions::ActionAddress {
        tilo_actions::ActionAddress::new(resource, self.id)
    }
}

impl<R: Resource> Invoke<R> {
    /// This invocation's action as the record's own resource publishes it.
    #[must_use]
    pub fn address(&self) -> tilo_actions::ActionAddress {
        self.address_on(R::KIND)
    }

    /// An invocation of the generic [`APPLY`](crate::APPLY) action for one
    /// typed action value — the path for edits without a named declaration.
    ///
    /// # Errors
    /// Returns the serialization error of `action`.
    pub fn apply(action: &R::Action) -> Result<Self, serde_json::Error> {
        // A unit variant serializes as its name; written as a map it reads
        // back the same, and is the object the action's parameter takes.
        let value = match crate::__private::json_via_bytes(action)? {
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
            on: None,
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
                crate::__private::json_via_bytes(edit)?,
            )]),
            refusal: None,
            effect: false,
            on: None,
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
            on: None,
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
            on: None,
            marker: PhantomData,
        }
    }

    /// Whether this is a [`group`](Self::group) of invocations.
    pub fn is_group(&self) -> bool {
        self.id == crate::GROUP
    }

    /// Build the replayable action on this (origin) machine. A group builds
    /// with [`build_group`](crate::group::build_group).
    pub fn build(&self, now_ms: i64) -> Result<R::Action, String> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        // What only the app's own code applies has no declaration a caller
        // could name: it is built here, where the app invokes it.
        if let Some(built) = R::build_by_app(self.id, &self.arguments, now_ms) {
            return built;
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
