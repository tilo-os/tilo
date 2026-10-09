//! Typed actions over a record resource: a method on the record becomes a
//! replayable, attributable, undoable operation.
//!
//! `#[resource]` turns every `#[action]` method into a variant of a generated
//! operation enum implementing [`Operation`]. Applying it reads the record
//! from the document, runs the method, and writes back only what the method
//! changed — so the method is ordinary Rust over ordinary fields and the CRDT
//! stays invisible.

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
    pub fn for_apply(doc: &mut Doc, payload: &[u8], now_ms: i64) -> Self {
        Self {
            seed: seed(doc, b"tilo-resource/action-seed/v1", payload),
            minted: 0,
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
            now_ms: 0,
            author: invoking_author(),
            invocation: invoking_invocation(),
        }
    }

    /// A fresh id, unique to this action and stable under replay.
    pub fn id(&mut self) -> Id {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.seed);
        hasher.update(&self.minted.to_le_bytes());
        self.minted = self.minted.saturating_add(1);
        let digest = hasher.finalize();
        let hex = digest.to_hex();
        Id::from_raw(hex.as_str().get(..20).unwrap_or_default())
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
    type Record: Fields;

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
}

/// A record operation with the origin's time attached: the replayable action
/// of a `#[resource]` record.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Stamped<O> {
    #[serde(rename = "op")]
    pub operation: O,
    #[serde(rename = "at", default)]
    pub now_ms: i64,
}

impl<O: Operation> tilo_actions::Action for Stamped<O> {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload = serde_json::to_vec(self).map_err(ActionError::Encode)?;
        let mut cx = ActionCx::for_apply(doc, &payload, self.now_ms);
        if let Some(edit) = self.operation.edit() {
            return edit
                .apply(doc, &mut cx)
                .map_err(|error| ActionError::Refused(error.to_string()));
        }
        let mut record = O::Record::read_fields(doc, &ROOT)
            .map_err(|error| ActionError::Refused(error.to_string()))?;
        self.operation
            .perform(&mut record, &mut cx)
            .map_err(|refusal| ActionError::Refused(refusal.0))?;
        crate::Model::check(&record).map_err(|refusal| ActionError::Refused(refusal.0))?;
        record
            .write_fields(doc, &ROOT)
            .map_err(|error| ActionError::Refused(error.to_string()))
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    fn risk(&self) -> RiskLevel {
        self.operation.risk()
    }

    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.operation.change_kind()
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
    pub fn try_arguments(
        &self,
        input: &A,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let id = self.id;
        let value = serde_json::to_value(input).map_err(|error| format!("`{id}`: {error}"))?;
        match (self.params, value) {
            ([], serde_json::Value::Object(fields)) => Ok(fields),
            ([], serde_json::Value::Null) => Ok(serde_json::Map::new()),
            ([], other) => Err(format!("`{id}` takes named arguments, not `{other}`")),
            ([single], value) => Ok(serde_json::Map::from_iter([((*single).to_owned(), value)])),
            (names, serde_json::Value::Array(values)) if values.len() == names.len() => Ok(names
                .iter()
                .map(|name| (*name).to_owned())
                .zip(values)
                .collect()),
            (names, other) => Err(format!(
                "`{id}` takes {} arguments ({}), not `{other}`",
                names.len(),
                names.join(", ")
            )),
        }
    }

    /// [`Self::try_arguments`] for a control binding: an input that does
    /// not bind gives no arguments, which the binding's validation against
    /// the published spec then refuses.
    pub fn arguments(&self, input: &A) -> serde_json::Map<String, serde_json::Value> {
        self.try_arguments(input).unwrap_or_default()
    }

    /// Fully apply the input: an invocation with nothing left to supply.
    /// A verb's parameter struct converts from its parameters in order: one
    /// value, or a tuple of several. An input that does not bind makes an
    /// invocation that refuses with why ([`Invoke::refusal`]), never one
    /// with empty arguments.
    pub fn with(&self, input: impl Into<A>) -> Invoke<R> {
        let (arguments, refusal) = match self.try_arguments(&input.into()) {
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
    fn arguments(&self, input: I) -> serde_json::Map<String, serde_json::Value>;
}

/// A control binds an action whose input its value converts to: a text
/// field binds a verb taking one `String`.
impl<R: 'static, A: Serialize + 'static, I: Into<A>> Binds<R, I> for ActionRef<R, A> {
    fn action_id(&self) -> &'static str {
        self.id
    }
    fn arguments(&self, input: I) -> serde_json::Map<String, serde_json::Value> {
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
    fn arguments(&self, input: I) -> serde_json::Map<String, serde_json::Value> {
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
    /// The verb.
    const ACTION: ActionRef<Self::Resource, Self>;
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

    /// Select the audited low-risk door when this exact value is opted in.
    /// The target independently revalidates the decoded value.
    ///
    /// # Errors
    /// Returns the serialization error of `action`.
    pub fn checked_apply(action: &R::Action) -> Result<Self, serde_json::Error> {
        let mut invocation = Self::apply(action)?;
        if R::AUDITED_LOW_MUTATIONS
            && R::allows_low_mutation(action)
            && tilo_actions::Action::risk(action) == tilo_actions::RiskLevel::Low
        {
            invocation.id = crate::APPLY_LOW;
        }
        Ok(invocation)
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
        let low = !actions.is_empty()
            && actions.iter().all(|member| {
                member.get("action").and_then(serde_json::Value::as_str) == Some(crate::APPLY_LOW)
            });
        Self {
            id: if low { crate::GROUP_LOW } else { crate::GROUP },
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
        self.id == crate::GROUP || self.id == crate::GROUP_LOW
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
