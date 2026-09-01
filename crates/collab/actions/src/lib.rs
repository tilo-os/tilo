//! # qOS ACTION model
//!
//! The state layer behind Ctrl+K and cross-peer collaboration: a
//! serializable, **replayable** unit of CRDT mutation.
//!
//! ## Play locally, replay anywhere
//!
//! An [`Action`] is a *value* that deterministically mutates a CRDT
//! [`Doc`](action::Doc). Applied to the local replica, its
//! [`ActionEnvelope`] is replicated; a peer starting from the same state
//! [`replay`](executor::replay)s it and lands on the **identical**
//! document — the [executor](executor) pins the Automerge actor id (from
//! the action's [`ActionId`]) and commit timestamp on every apply, so
//! the resulting change is content-addressed identical everywhere.
//!
//! Actions are strictly **not presentation** — anything local-only
//! (rendering, scroll, selection) never becomes an action; only
//! shared-CRDT mutations do.
//!
//! ## Resources
//!
//! An app publishes the tree of [`Resource`]s it currently has open (a
//! video → its comments; a book → its annotations) via
//! [`ResourceProvider`]. Each resource exports [`ActionSpec`]s; the
//! palette lists them, and choosing one
//! [`dispatch`](executor::dispatch)es the underlying action.
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
//! ## Execution locus
//!
//! Each action declares an [`ExecutionLocus`]: `ClientLocal` (this
//! client applies + replicates; peers replay) or `ServerPreferred`
//! (route to a host). [`dispatch`](executor::dispatch) honours it
//! against an [`ActionTransport`](executor::ActionTransport), falling
//! back to local execution when no host answers.
//!
//! ## Sharing this vocabulary with the compositor
//!
//! [`params`] is deliberately the *same* vocabulary the compositor's headless
//! action API speaks — [`ParamType`], [`ActionParam`], [`ArgValue`], and the
//! strict [`ActionArgs::bind`] — lifted into one definition rather than left to
//! converge by accident. The wire spellings of `text` / `integer` / `boolean`
//! and the bind semantics (unknown argument refused, first failure wins,
//! nothing partial reaches an action) are unchanged from that surface, and the
//! `const` constructors keep a `const PARAMS: &[ActionParam] = &[…]` compiling
//! as written.
//!
//! Two deliberate supersets, both additive: [`ParamType::Number`], because the
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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod action;
pub mod context;
pub mod door;
pub mod envelope;
pub mod executor;
pub mod optimistic;
pub mod params;
pub mod registry;
pub mod resource;

pub use action::{
    Action, ActionError, ActionId, Doc, EncodedAction, ExecutionLocus, OperationKind,
};
pub use context::{
    ActionApp, ActionContext, ActionFacet, ActionGroup, ActionResource, CHANGE_MESSAGE_PREFIX,
};
pub use door::{ActionAddress, ActionDescriptor, DoorError, InputSpec};
pub use envelope::ActionEnvelope;
pub use executor::{
    ActionTransport, Dispatched, HostDecision, LocalTransport, apply_local, apply_local_by,
    apply_local_with_context, dispatch, dispatch_by, dispatch_by_with_context,
    dispatch_with_context, replay,
};
pub use optimistic::{Acknowledgement, OptimisticError, OptimisticQueue, RebaseBatch};
pub use params::{ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ArgError, ArgValue, ParamType};
pub use registry::ActionRegistry;
pub use resource::{
    ActionInput, ActionSpec, ArgsBuilder, Collects, FlatAction, KEY_SEP, PROMPTED_PARAM, Resource,
    ResourceId, ResourceProvider, flatten,
};

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};

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

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Counter>(COUNTER_TYPE);
        r
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

    /// THE HEADLINE TEST. An action applied on replica A, replicated, and
    /// replayed on replica B (from the same state) yields the **identical**
    /// document — same head hashes, not merely the same values.
    #[test]
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
        assert_eq!(av.to_i64(), Some(42));
        assert_eq!(bv.to_i64(), av.to_i64());
    }

    /// The replayed change is content-addressed *identical* to the origin's,
    /// so if the replicas later exchange native Automerge changes (ordinary
    /// CRDT sync) the duplicate dedupes instead of double-applying — why
    /// pinning actor + time on every apply matters.
    #[test]
    fn replayed_change_dedupes_against_native_automerge_merge() {
        let (mut a, mut b) = two_replicas();
        let action = Counter::Append {
            key: "queue".into(),
            value: "vid1".into(),
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
        // Append is ServerPreferred; with the local (no-host) transport it
        // falls back to a local apply. Grab that envelope.
        let mut tx = LocalTransport::new();
        let d = dispatch(&mut a, &action, encoded, &mut tx).unwrap();
        assert!(matches!(d, Dispatched::AppliedLocalFallback(_)));
        let env = d.envelope().clone();

        // B replays the envelope -> reproduces the identical change.
        replay(&mut b, &env, &registry()).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "replay == origin change");
        let heads_after_replay = b.get_heads();

        // Now B *also* receives A's native Automerge change via ordinary CRDT
        // sync. Since it's the same content-addressed change B already
        // replayed, the merge is a no-op: no second append.
        let a_bytes = a.save();
        let mut a_native = AutoCommit::load(&a_bytes).unwrap();
        b.merge(&mut a_native).unwrap();
        assert_eq!(
            b.get_heads(),
            heads_after_replay,
            "native merge of the identical change dedupes"
        );
        let (_, list) = b.get(&ROOT, "queue").unwrap().unwrap();
        assert_eq!(b.length(&list), 1, "no double-apply");
    }

    /// `ClientLocal` dispatch applies locally and replicates exactly one
    /// envelope through the transport.
    #[test]
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
        assert_eq!(a.get(&ROOT, "likes").unwrap().unwrap().0.to_i64(), Some(1));
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

    #[test]
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

    #[test]
    fn server_preferred_falls_back_local_without_host() {
        let (mut a, _) = two_replicas();
        let action = Counter::Append {
            key: "queue".into(),
            value: "vidY".into(),
        };
        let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
        let mut tx = LocalTransport::new();
        let d = dispatch(&mut a, &action, encoded, &mut tx).unwrap();
        assert!(matches!(d, Dispatched::AppliedLocalFallback(_)));
        assert!(d.mutated_local());
        assert_eq!(tx.replicated.len(), 1);
        // The append landed in the list.
        let (val, list) = a.get(&ROOT, "queue").unwrap().unwrap();
        assert!(matches!(
            val,
            automerge::Value::Object(automerge::ObjType::List)
        ));
        assert_eq!(a.length(&list), 1);
    }

    #[test]
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
    #[test]
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
