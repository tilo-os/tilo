//! # Tilo ACTION model
//!
//! The state layer behind Ctrl+K and cross-peer collaboration: a
//! serializable, **replayable** unit of CRDT mutation.
//!
//! ## Play locally, replay anywhere
//!
//! An [`Action`] is a *value* that deterministically mutates a CRDT
//! [`Doc`]. Applied to the local replica, its
//! [`ActionEnvelope`] is replicated; a peer starting from the same state
//! [`replay`]s it and lands on the **identical**
//! document — the [executor] pins the Automerge actor id (from
//! the action's [`ActionId`]) and commit timestamp on every apply, so
//! the resulting change is content-addressed identical everywhere.
//!
//! Replayable [`Action`] values are strictly **not presentation** and mutate
//! only shared CRDT state. The wider resource/action protocol also describes
//! client-local [`OperationKind::View`] and external
//! [`OperationKind::Effect`] operations. Those use the same typed descriptors,
//! permission checks and invocation receipts, but are dispatched through the
//! toolkit's operation handler instead of being replayed as document history.
//!
//! ## Resources
//!
//! An app publishes the tree of [`Resource`]s it currently has open (a
//! video → its comments; a book → its annotations) through the toolkit's
//! `PublishesResources`. Each resource exports [`ActionSpec`]s; the
//! palette lists them, and choosing one
//! [`dispatch`]es the underlying action.
//!
//! ## The door
//!
//! A resource tree is only useful to the app that built it until someone
//! else can reach it. [`door`] is that reach: [`door::survey`] turns a
//! live tree into inert, stably-addressed descriptors, and
//! [`door::invoke`] performs one attributed to the caller — so another
//! session acts on an app without the app growing a remote-control
//! surface of its own.
//!
//! ## Remote use
//!
//! [`remote`] carries the door across machines: a versioned, transport-neutral
//! vocabulary for listing, invoking, observing and cancelling another peer's
//! actions, and the [`remote::ActionHost`] trait a peer implements to answer
//! it. The caller is always the principal the transport authenticated, and
//! invocations are idempotent per [`InvocationId`].
//!
//! ## Execution locus
//!
//! Each action declares an [`ExecutionLocus`]: `ClientLocal` (this
//! client applies + replicates; peers replay) or `ServerPreferred`
//! (route to an authoritative host). [`dispatch`] honours
//! it against an [`ActionTransport`] and refuses
//! the operation when no host answers.
//!
//! ## Sharing this vocabulary with the compositor
//!
//! [`params`] is the *same* vocabulary the compositor's headless
//! action API speaks — [`ParamType`], [`ActionParam`], [`ArgValue`], and the
//! strict [`ActionArgs::bind`] — lifted into one definition rather than left to
//! converge by accident. The wire spellings of `text` / `integer` / `boolean`
//! and the bind semantics (unknown argument refused, first failure wins,
//! nothing partial reaches an action) are unchanged from that surface, and the
//! `const` constructors keep a `const PARAMS: &[ActionParam] = &[…]` compiling
//! as written.
//!
//! Two additive extensions: [`ParamType::Number`], because the
//! app-side arguments this replaces are overwhelmingly fractional and rounding
//! them would discard the values they exist to carry; and
//! [`ActionParam::choices`], which turns a prose list into something a caller
//! can read and binding can enforce. The string fields are
//! [`Cow<'static, str>`](std::borrow::Cow) rather than `&'static str` because an
//! app builds its parameters per frame out of live document state — a `const`
//! declaration is `Cow::Borrowed` and costs nothing.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// Serde for a JSON-only type whose reading goes through one
/// `serde_json::Value`.
///
/// The type derives with `#[serde(remote = "Self")]`, which makes its
/// derived code inherent functions. Writing runs them as derived, so the
/// bytes stay what the derive writes. Reading runs them once, against a
/// `Value`, in this crate: every format and every crate that reads the type
/// then shares that one copy instead of compiling the type's whole
/// deserializer (and, for an internally tagged enum, its buffered copy)
/// again for each deserializer it meets. Only for types every encoding of
/// which is JSON: a `Value` needs a self-describing format. `custom` leaves
/// `from_json_value` to the type.
macro_rules! read_through_json_value {
    ($ty:ty) => {
        read_through_json_value!($ty, custom);

        impl $ty {
            #[inline(never)]
            fn from_json_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
                Self::deserialize(value)
            }
        }
    };
    ($ty:ty, custom) => {
        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                Self::serialize(self, serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
                Self::from_json_value(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

pub mod action;
pub mod content;
pub mod context;
pub mod contract;
pub mod door;
pub mod entry;
pub mod envelope;
pub mod executor;
pub mod framework;
pub mod interaction;
pub mod key;
pub mod optimistic;
pub mod outcome;
pub mod params;
pub mod protocol;
pub mod registry;
pub mod remote;
pub mod resource;
pub mod schema;
pub mod surface;

pub use action::{
    Action, ActionError, ActionId, Doc, EncodedAction, ExecutionLocus, OperationKind,
};
pub use content::{
    CONTENT_PART_THRESHOLD, CONTENT_REFERENCE_KEY, ContentParts, MissingContent, keep_content_in,
};
pub use context::{
    AGENT_TASK_PARTICIPANT_PREFIX, ActionApp, ActionContext, ActionFacet, ActionGroup,
    ActionInvoker, ActionResource, CHANGE_MESSAGE_PREFIX, CONTINUATION_PREFIX, ChangeKind,
    MAX_PARTICIPANT_ID_BYTES, ParticipantId, RISK_FACET,
};
pub use contract::{
    ARGUMENT_RISK_TAG, ActionContract, ActionKind, ActionNeed, Availability, ConfirmationPolicy,
    DOCUMENT_READ_TAG, ExecutionMode, LOCAL_DEVICE_ONLY, PROGRAMMATIC_TAG, ResultSpec, RetryPolicy,
    RiskLevel, UNTRUSTED_CONTENT_TAG, action_node,
};
pub use door::{ActionAddress, ActionDescriptor, DoorError, InputSpec, ResourceViewDescriptor};
pub use entry::{
    ActionItem, Described, Did, EditOp, FieldEdit, GroupOf, HistoryOp, HistoryStep, PathStep,
    Scalar, VerbCall,
};
pub use envelope::ActionEnvelope;
pub use executor::{
    ActionTransport, CompensatingScope, Dispatched, EntryReader, HostDecision, LocalTransport,
    UNDONE_KEY, Verified, apply_local, apply_local_by, apply_local_with_context,
    await_compensation, await_settlement, current_step, dispatch, dispatch_by,
    dispatch_by_with_context, dispatch_with_context, entry_message, entry_of, invoking_context,
    open_step, replay, settle_compensation, verify_change,
};
pub use framework::FrameworkChange;
pub use interaction::{
    ActorKind, INTERACTION_VERSION, InteractionAction, InteractionActor, InteractionAnchor,
    InteractionGridRange, InteractionPhase, InteractionTextRange, InteractionUpdate,
    InteractionViolation, MAX_ANCHOR_ITEMS, MAX_DETAIL_BYTES, MAX_ID_BYTES, MAX_INTERACTION_BYTES,
    MAX_LABEL_BYTES, SemanticInteraction,
};
pub use key::ActionKey;
pub use optimistic::{Acknowledgement, OptimisticError, OptimisticQueue, RebaseBatch};
pub use outcome::{
    ActionFailure, ActionOutput, CONTENT_RESOURCE_KIND, EffectReceiptStatus, InvocationId,
    InvocationUpdate, content_hash, content_resource,
};
pub use params::{
    ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ArgError, ArgValue, CONTENT_ANNOTATION,
    PLACE_ANNOTATION, ParamType, place_schema, validate_json_schema,
};
pub use protocol::{ContractViolation, validate_surface, validate_surface_contracts};
pub use registry::{ActionRegistry, decode_json};
#[allow(
    deprecated,
    reason = "existing app impls still name this trait until they publish through tilo_resource"
)]
pub use resource::{
    ActionBinding, ActionBindingError, ActionInput, ActionSpec, ArgsBuilder, Collects,
    ContinuationArguments, ContinuationEvidence, ContinuationKey, ContinuationQuery,
    ControlInvocation, FlatAction, KEY_SEP, PROMPTED_PARAM, Resource, ResourceId,
    ResourcePresentation, ResourceSemanticIdentity, ResourceViewMountSpec, ResourceViewRangeSpec,
    ResourceViewSpec, flatten,
};
pub use schema::{
    CatalogueSchemaEntry, ContextRequirement, PARTICIPANT_VIEW_TAG, REQUIRES_TAG_PREFIX,
    catalogue_schema, catalogue_schema_bytes,
};
/// Grants and the decisions over them, as apps read and show them.
pub use tilo_action_policy as policy;
/// The field a resource's comments live in: an action that only comments
/// is placed below it ([`ActionContract::field`]), where someone who may only
/// comment is granted changes.
pub use tilo_action_policy::roles::COMMENTS_FIELD;

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};
    use tilo_automerge::ApplyIncoming;

    /// A real action: set a scalar under a key, and (for lists) append.
    /// Mutates only via the passed doc, no wall-clock/RNG — the determinism contract.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum Counter {
        Put { key: String, value: i64 },
        Append { key: String, value: String },
    }

    const COUNTER_TYPE: &str = "test.counter";

    impl Action for Counter {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                Counter::Put { key, value } => {
                    doc.put(&ROOT, key, ScalarValue::Int(*value))?;
                }
                Counter::Append { key, value } => {
                    let list = match doc.get(&ROOT, key)? {
                        Some((automerge::Value::Object(automerge::ObjType::List), id)) => id,
                        _ => doc.put_object(&ROOT, key, automerge::ObjType::List)?,
                    };
                    let len = doc.length(&list);
                    doc.insert(&list, len, ScalarValue::Str(value.as_str().into()))?;
                }
            }
            Ok(())
        }

        fn locus(&self) -> ExecutionLocus {
            match self {
                // Appends prefer a host so a shared list is serialized.
                Counter::Append { .. } => ExecutionLocus::ServerPreferred,
                Counter::Put { .. } => ExecutionLocus::ClientLocal,
            }
        }
    }

    /// A read marker: shared state about the document, not its content.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Seen(i64);

    const SEEN_TYPE: &str = "test.seen";

    impl Action for Seen {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, "seen", ScalarValue::Int(self.0))?;
            Ok(())
        }

        fn change_kind(&self) -> ChangeKind {
            ChangeKind::Housekeeping
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Counter>(COUNTER_TYPE);
        r.register::<Seen>(SEEN_TYPE);
        r
    }

    fn last_message(doc: &mut AutoCommit) -> Option<String> {
        let heads = doc.get_heads();
        doc.get_change_by_hash(heads.first()?)?
            .message()
            .map(ToString::to_string)
    }

    fn last_context(doc: &mut AutoCommit) -> Option<ActionContext> {
        ActionContext::from_change_message(&last_message(doc)?)
    }

    /// The executor writes an action's declared kind into its change, with or
    /// without a caller's context, and replay reproduces the same change.
    #[test]
    fn declared_kinds_travel_in_the_change_and_every_change_names_what_it_did() {
        let (mut a, mut b) = two_replicas();
        let env = apply_local(
            &mut a,
            &Seen(3),
            EncodedAction::encode(SEEN_TYPE, &Seen(3)).unwrap(),
        )
        .unwrap();
        assert!(
            env.context
                .as_ref()
                .is_some_and(ActionContext::is_housekeeping)
        );
        assert!(last_context(&mut a).is_some_and(|context| context.is_housekeeping()));
        replay(&mut b, &env, &registry()).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "replay makes the same change");

        let labelled = ActionContext::for_resource("c:1", "chat.channel", "general");
        apply_local_with_context(
            &mut a,
            &Seen(4),
            EncodedAction::encode(SEEN_TYPE, &Seen(4)).unwrap(),
            None,
            labelled,
        )
        .unwrap();
        let context = last_context(&mut a).unwrap();
        assert!(context.is_housekeeping());
        assert_eq!(
            context.resource.as_ref().map(|r| r.label.as_str()),
            Some("general"),
            "the caller's context is kept"
        );

        let put = Counter::Put {
            key: "score".into(),
            value: 1,
        };
        let env = apply_local(
            &mut a,
            &put,
            EncodedAction::encode(COUNTER_TYPE, &put).unwrap(),
        )
        .unwrap();
        let entry = env
            .context
            .expect("every committed action records an entry");
        assert!(entry.change.is_step());
        assert_eq!(
            entry.what,
            Some(Did::verb(
                ActionKey::new("test", COUNTER_TYPE, "put"),
                [
                    ("key".to_string(), Scalar::from("score")),
                    ("value".to_string(), Scalar::from(1)),
                ],
            )),
            "an action that describes nothing is named by its encoded verb"
        );
        assert_eq!(last_context(&mut a), Some(entry));

        // A context may say a content action is nobody's step this time; an
        // action's own declaration is never overridden by a context.
        let seeded = Counter::Put {
            key: "score".into(),
            value: 2,
        };
        let env = apply_local_with_context(
            &mut a,
            &seeded,
            EncodedAction::encode(COUNTER_TYPE, &seeded).unwrap(),
            None,
            ActionContext::default().with_change(ChangeKind::External),
        )
        .unwrap();
        assert_eq!(last_context(&mut a).unwrap().change, ChangeKind::External);
        assert!(!env.context.unwrap().change.is_step());
        apply_local_with_context(
            &mut a,
            &Seen(5),
            EncodedAction::encode(SEEN_TYPE, &Seen(5)).unwrap(),
            None,
            ActionContext::default().with_change(ChangeKind::External),
        )
        .unwrap();
        assert!(last_context(&mut a).unwrap().is_housekeeping());
    }

    /// Seed two independent replicas that share history (so a replayed
    /// change has identical deps), each with its OWN actor for normal edits.
    fn two_replicas() -> (AutoCommit, AutoCommit) {
        let mut a = AutoCommit::new();
        a.put(&ROOT, "seed", ScalarValue::Str("s".into())).unwrap();
        a.commit();
        let bytes = a.save();
        let b = AutoCommit::load(&bytes).unwrap();
        (a, b)
    }

    #[test]
    fn the_executor_keeps_the_admitted_invocation_and_replays_the_identical_change() {
        let (mut origin, mut peer) = two_replicas();
        let action = Counter::Put {
            key: "score".into(),
            value: 42,
        };
        let invocation = InvocationId::new();
        let context = ActionContext {
            source_invocation: Some(invocation),
            ..Default::default()
        };
        let envelope = apply_local_with_context(
            &mut origin,
            &action,
            EncodedAction::encode(COUNTER_TYPE, &action).unwrap(),
            Some(b"actual-person-device-writer"),
            context,
        )
        .unwrap();
        let committed = last_context(&mut origin).unwrap();
        assert_eq!(committed.source_invocation, Some(invocation));
        assert_eq!(envelope.context, Some(committed));
        replay(&mut peer, &envelope, &registry()).unwrap();
        assert_eq!(origin.get_heads(), peer.get_heads());
        assert_eq!(
            origin.get_changes(&envelope.base_change_hashes())[0].raw_bytes(),
            peer.get_changes(&envelope.base_change_hashes())[0].raw_bytes(),
        );
    }

    #[test]
    fn a_callers_entry_replays_to_the_identical_change() {
        let (mut origin, mut peer) = two_replicas();
        let action = Counter::Put {
            key: "score".into(),
            value: 42,
        };
        let context = ActionContext::for_resource("score", "counter", "Score");
        let envelope = ActionEnvelope {
            id: ActionId::new(),
            action: EncodedAction::encode(COUNTER_TYPE, &action).unwrap(),
            base_heads: origin.get_heads().iter().map(|hash| hash.0).collect(),
            timestamp: 17,
            locus: ExecutionLocus::ClientLocal,
            actor: Some(b"legacy-person-device-writer".to_vec()),
            context: Some(context.clone()),
        };
        let old_message = context.change_message().unwrap();
        let wire = serde_json::to_vec(&envelope).unwrap();
        let decoded: ActionEnvelope = serde_json::from_slice(&wire).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), wire);
        origin.set_actor(envelope.actor());
        action.apply(&mut origin).unwrap();
        origin.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_time(envelope.timestamp)
                .with_message(old_message.clone()),
        );
        replay(&mut peer, &decoded, &registry()).unwrap();
        assert_eq!(last_message(&mut peer), Some(old_message));
        assert_eq!(origin.get_heads(), peer.get_heads());
        assert_eq!(
            origin.get_changes(&envelope.base_change_hashes())[0].raw_bytes(),
            peer.get_changes(&envelope.base_change_hashes())[0].raw_bytes(),
        );
    }

    /// An action replays to an identical doc on a peer, concurrent envelopes replay from their
    /// recorded base, and a replayed change dedupes against a native Automerge merge.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn envelopes_replay_deterministically_on_peers() {
        action_replays_to_identical_doc_on_peer();
        concurrent_envelopes_replay_from_their_recorded_base();
        replayed_change_dedupes_against_native_automerge_merge();
    }

    /// THE HEADLINE TEST. An action applied on replica A, replicated, and
    /// replayed on replica B (from the same state) yields the **identical**
    /// document — same head hashes, not merely the same values.
    fn action_replays_to_identical_doc_on_peer() {
        let (mut a, mut b) = two_replicas();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = Counter::Put {
            key: "score".into(),
            value: 42,
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();

        // A plays locally, producing an envelope to replicate.
        let env = apply_local(&mut a, &action, encoded).unwrap();

        // B replays the envelope from the same base state.
        replay(&mut b, &env, &registry()).unwrap();

        // Identical *history*, not just identical materialized values.
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduces the identical change on the peer"
        );
        let av = a.get(&ROOT, "score").unwrap().unwrap().0;
        let bv = b.get(&ROOT, "score").unwrap().unwrap().0;
        assert_eq!(av.as_i64(), Some(42));
        assert_eq!(bv.as_i64(), av.as_i64());
    }

    /// Two peers can author from the same base and receive each other's
    /// envelopes after their own action has already advanced the live doc.
    /// Replay must still reproduce each action at its recorded base and merge
    /// the resulting CRDT change; applying either payload directly to the
    /// newer live state would make document-reading actions order-dependent.
    fn concurrent_envelopes_replay_from_their_recorded_base() {
        let (mut a, mut b) = two_replicas();
        let left = Counter::Put {
            key: "left".into(),
            value: 1,
        };
        let right = Counter::Put {
            key: "right".into(),
            value: 2,
        };

        let left_env = apply_local(
            &mut a,
            &left,
            EncodedAction::encode(COUNTER_TYPE, &left).unwrap(),
        )
        .unwrap();
        let right_env = apply_local(
            &mut b,
            &right,
            EncodedAction::encode(COUNTER_TYPE, &right).unwrap(),
        )
        .unwrap();

        replay(&mut a, &right_env, &registry()).unwrap();
        replay(&mut b, &left_env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads());
        for doc in [&a, &b] {
            assert_eq!(doc.get(&ROOT, "left").unwrap().unwrap().0.as_i64(), Some(1));
            assert_eq!(
                doc.get(&ROOT, "right").unwrap().unwrap().0.as_i64(),
                Some(2)
            );
        }
    }

    /// The replayed change is content-addressed *identical* to the origin's,
    /// so if the replicas later exchange native Automerge changes (ordinary
    /// CRDT sync) the duplicate dedupes instead of double-applying — why
    /// pinning actor + time on every apply matters.
    fn replayed_change_dedupes_against_native_automerge_merge() {
        let (mut a, mut b) = two_replicas();
        let action = Counter::Append {
            key: "queue".into(),
            value: "vid1".into(),
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
        // This test is about deterministic replay, not locus routing; apply it
        // explicitly at the origin and retain the resulting envelope.
        let env = apply_local(&mut a, &action, encoded).unwrap();

        // B replays the envelope -> reproduces the identical change.
        replay(&mut b, &env, &registry()).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "replay == origin change");
        let heads_after_replay = b.get_heads();

        // Now B *also* receives A's native Automerge change via ordinary CRDT
        // sync. Since it's the same content-addressed change B already
        // replayed, the merge is a no-op: no second append.
        let a_bytes = a.save();
        let mut a_native = AutoCommit::load(&a_bytes).unwrap();
        b.merge_incoming(&mut a_native).unwrap();
        assert_eq!(
            b.get_heads(),
            heads_after_replay,
            "native merge of the identical change dedupes"
        );
        let (_, list) = b.get(&ROOT, "queue").unwrap().unwrap();
        assert_eq!(b.length(&list), 1, "no double-apply");
    }

    /// Client-local dispatch applies and replicates; server-preferred routes to the host without a
    /// local apply and refuses without a host; the registry rejects unknown types and flattens nested
    /// resources.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn dispatch_routes_by_placement_and_the_registry_knows_its_types() {
        client_local_dispatch_applies_and_replicates();
        server_preferred_routes_to_host_without_local_apply();
        server_preferred_refuses_without_host();
        registry_rejects_unknown_type();
        flatten_walks_nested_resources();
    }

    /// `ClientLocal` dispatch applies locally and replicates exactly one
    /// envelope through the transport.
    fn client_local_dispatch_applies_and_replicates() {
        let (mut a, _) = two_replicas();
        let action = Counter::Put {
            key: "likes".into(),
            value: 1,
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
        let mut tx = LocalTransport::new();
        let d = dispatch(&mut a, &action, encoded, &mut tx).unwrap();
        assert!(matches!(d, Dispatched::AppliedLocal(_)));
        assert!(d.mutated_local());
        assert_eq!(tx.replicated.len(), 1, "replicated exactly once");
        assert_eq!(a.get(&ROOT, "likes").unwrap().unwrap().0.as_i64(), Some(1));
    }

    /// A transport that speaks for a reachable host, proving the
    /// `ServerPreferred` plumbing is real: when a host accepts, the action is
    /// NOT applied to the local doc (the host will execute + broadcast).
    struct HostTransport {
        submitted: Vec<ActionEnvelope>,
    }
    impl ActionTransport for HostTransport {
        fn replicate(&mut self, _env: &ActionEnvelope) {}
        fn submit_to_host(&mut self, env: &ActionEnvelope) -> HostDecision {
            self.submitted.push(env.clone());
            HostDecision::AcceptedByHost
        }
    }

    fn server_preferred_routes_to_host_without_local_apply() {
        let (mut a, _) = two_replicas();
        let before = a.get_heads();
        let action = Counter::Append {
            key: "queue".into(),
            value: "vidX".into(),
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
        let mut tx = HostTransport { submitted: vec![] };
        let d = dispatch(&mut a, &action, encoded, &mut tx).unwrap();
        assert!(matches!(d, Dispatched::SubmittedToHost(_)));
        assert!(
            !d.mutated_local(),
            "host-accepted action is not applied locally"
        );
        assert_eq!(tx.submitted.len(), 1, "submitted to the host once");
        assert_eq!(a.get_heads(), before, "local doc untouched");
        assert_eq!(d.envelope().locus, ExecutionLocus::ServerPreferred);
    }

    fn server_preferred_refuses_without_host() {
        let (mut a, _) = two_replicas();
        let before = a.get_heads();
        let action = Counter::Append {
            key: "queue".into(),
            value: "vidY".into(),
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
        let mut tx = LocalTransport::new();
        let error = dispatch(&mut a, &action, encoded, &mut tx).unwrap_err();
        assert!(matches!(error, ActionError::Refused(_)));
        assert_eq!(
            a.get_heads(),
            before,
            "an unavailable host grants no authority"
        );
        assert!(tx.replicated.is_empty());
        assert!(a.get(&ROOT, "queue").unwrap().is_none());
    }

    fn registry_rejects_unknown_type() {
        let reg = registry();
        let bogus = EncodedAction {
            type_id: "nope".into(),
            payload: b"{}".to_vec(),
        };
        let mut doc = AutoCommit::new();
        let env = ActionEnvelope {
            id: ActionId::new(),
            action: bogus,
            base_heads: vec![],
            timestamp: 0,
            locus: ExecutionLocus::ClientLocal,
            actor: None,
            context: None,
        };
        assert!(matches!(
            replay(&mut doc, &env, &reg),
            Err(ActionError::UnknownType(_))
        ));
    }

    /// Resource forests flatten depth-first, parents before children, with
    /// composite keys.
    fn flatten_walks_nested_resources() {
        let put = Counter::Put {
            key: "k".into(),
            value: 1,
        };
        let video = Resource::new("tube.video:v1", "tube.video", "A Video")
            .with_action(ActionSpec::immediate("like", "Like", COUNTER_TYPE, &put).unwrap())
            .with_child(
                Resource::new("tube.comment:v1:0", "tube.comment", "nice!").with_action(
                    ActionSpec::immediate("like", "Like comment", COUNTER_TYPE, &put).unwrap(),
                ),
            );
        let flat = flatten(std::slice::from_ref(&video));
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].key, format!("tube.video:v1{KEY_SEP}like"));
        assert_eq!(flat[0].resource_kind, "tube.video");
        assert_eq!(flat[1].key, format!("tube.comment:v1:0{KEY_SEP}like"));
        assert_eq!(flat[1].resource_kind, "tube.comment");
    }
}
