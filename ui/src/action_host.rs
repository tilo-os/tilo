//! Framework-owned action dispatch every app gets by default.
//!
//! Adopting the qOS [action model](qos_actions) used to mean each app
//! copy-pasting the same undo-bracket, encode-and-dispatch,
//! last-dispatched, and no-op-transport plumbing. Hoisted here so an app
//! declares only its own domain and embeds one [`ActionHost`] field.
//!
//! ```ignore
//! struct MyApp { actions: ui::ActionHost, /* … */ }
//!
//! // At every affordance that mutates shared state:
//! self.actions.dispatch(doc, MY_ACTION_TYPE, &action);
//!
//! impl Widget for MyApp {
//!     fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
//!         self.actions.take_committed()
//!     }
//! }
//! ```
//!
//! # Driving an app from outside
//!
//! The same host is how an app is driven with **no window open**. A caller that
//! cannot see the screen enumerates the app's serializable projection
//! ([`qos_actions::project_flat`]), picks an address, and calls
//! [`invoke_projected`](ActionHost::invoke_projected) with typed arguments;
//! that resolves the address, binds the arguments strictly, runs the app's own
//! builder, and dispatches the result.
//!
//! Everything downstream of "an [`EncodedAction`] now exists" is shared with
//! the on-screen path — the same executor, the same undo bracket, the same
//! replication — so an externally-driven edit is undoable, replayable and
//! collaborative exactly as a clicked one is. The only thing the external
//! caller supplies that a click does not is the argument values, and those are
//! checked before an action exists at all.
//!
//! [`apply_remote`](ActionHost::apply_remote) is the other direction: an
//! envelope arriving *from* a peer or host. It deliberately does not touch the
//! local undo stack — see its docs for why that asymmetry is the design.

use automerge::{AutoCommit, ChangeHash};
use qos_actions::{
    Action, ActionEnvelope, ActionError, ActionRegistry, Dispatched, EncodedAction, InvokeError,
    Resource,
};

/// Framework-owned action dispatch an app embeds as a single field, in
/// place of hand-rolled `perform_action`/`DiscardTransport`/
/// `last_dispatched` boilerplate.
///
/// The undo boundary this brackets carries no author: what crosses this
/// seam is heads, not identity. It's still reachable by interactive Ctrl+Z
/// because an author-scoped undo claims unattributed entries too — see
/// `UndoLog::claimable_by`.
#[derive(Default)]
pub struct ActionHost {
    /// Drained via
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action)
    /// to record exactly one undo boundary bracketing the action(s).
    commit_before: Option<Vec<ChangeHash>>,
    /// Lets a test prove an affordance and the Ctrl+K
    /// [`ActionSpec`](qos_actions::ActionSpec) build the byte-identical
    /// action.
    last_dispatched: Option<EncodedAction>,
}

impl ActionHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Brackets the undo boundary on the *first* action of the event only
    /// (`get_heads` flushes pending app writes, so the bracket covers only
    /// the action's own change), encodes + remembers it as
    /// [`last_dispatched`](Self::last_dispatched), then dispatches through
    /// the default [`LocalTransport`](qos_actions::LocalTransport) — applied
    /// via the executor with a pinned actor, so it replays byte-identically
    /// on peers. A dispatch error is logged at WARN and swallowed; an
    /// affordance never panics the app.
    pub fn dispatch<A>(&mut self, doc: &mut AutoCommit, type_id: &str, action: &A)
    where
        A: Action + serde::Serialize,
    {
        let mut transport = qos_actions::LocalTransport::new();
        self.dispatch_with(doc, type_id, action, &mut transport);
    }

    /// [`dispatch`](Self::dispatch), but replicating through a transport the
    /// caller owns — a socket to the compositor, a peer link — instead of the
    /// throwaway [`LocalTransport`](qos_actions::LocalTransport) that drops
    /// what it is handed.
    ///
    /// Same bracket, same encode, same executor: swapping the transport changes
    /// only where the envelope *goes*, never what the action does or how it is
    /// recorded. That is what makes it safe to move an app onto a real
    /// transport without re-proving its actions.
    pub fn dispatch_with<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
        transport: &mut dyn qos_actions::ActionTransport,
    ) where
        A: Action + serde::Serialize,
    {
        let encoded =
            EncodedAction::encode(type_id, action).expect("a domain action is always serializable");
        self.arm(doc);
        self.last_dispatched = Some(encoded.clone());
        if let Err(e) = qos_actions::dispatch(doc, action, encoded, transport) {
            tracing::warn!(error = %e, type_id = %type_id, "action dispatch failed");
        }
    }

    /// Dispatch an action that arrived **already encoded** — the path an
    /// external driver takes.
    ///
    /// A caller with no window builds its action from the app's serialized
    /// projection ([`qos_actions::project_flat`]) and sends back an
    /// [`EncodedAction`], not a typed Rust value. It is decoded here through
    /// the app's own [`ActionRegistry`] and then dispatched by exactly the code
    /// path [`dispatch`](Self::dispatch) uses.
    ///
    /// "Exactly" is the point, and it is why this lives on `ActionHost` rather
    /// than beside it: the undo bracket is armed the same way, so the boundary
    /// the host later drains is attributed to the local user and Ctrl+Z reverts
    /// an externally-driven edit precisely as it reverts a clicked one. An
    /// external driver is the user working without a window — not a second
    /// author, and not a peer.
    ///
    /// Returns what the executor did, so a caller can tell a local apply from a
    /// submission a host accepted.
    pub fn dispatch_encoded(
        &mut self,
        doc: &mut AutoCommit,
        encoded: EncodedAction,
        registry: &ActionRegistry,
    ) -> Result<Dispatched, ActionError> {
        let mut transport = qos_actions::LocalTransport::new();
        self.dispatch_encoded_with(doc, encoded, registry, &mut transport)
    }

    /// [`dispatch_encoded`](Self::dispatch_encoded) through a caller-owned
    /// transport.
    pub fn dispatch_encoded_with(
        &mut self,
        doc: &mut AutoCommit,
        encoded: EncodedAction,
        registry: &ActionRegistry,
        transport: &mut dyn qos_actions::ActionTransport,
    ) -> Result<Dispatched, ActionError> {
        // Decode before arming: an action this app cannot reconstruct is a
        // refusal, and a refusal must not leave an undo bracket open that a
        // later, unrelated edit would then be swept into.
        let action = registry.decode(&encoded)?;
        self.arm(doc);
        self.last_dispatched = Some(encoded.clone());
        qos_actions::dispatch(doc, action.as_ref(), encoded, transport)
    }

    /// Apply an envelope that arrived from a **peer or host** — the receiving
    /// half of a transport.
    ///
    /// Replays through [`qos_actions::replay`], so a replica at the envelope's
    /// base heads lands on the byte-identical document.
    ///
    /// Deliberately does **not** arm the undo bracket, and that asymmetry with
    /// [`dispatch_encoded`](Self::dispatch_encoded) is the design, not an
    /// omission. The same remote edit can reach this replica two ways: replayed
    /// from an envelope, or merged as an ordinary Automerge change during CRDT
    /// sync. The sync path never touches the undo stack. If this one did, then
    /// whether the local user could Ctrl+Z somebody else's edit would depend on
    /// which pipe happened to deliver it — a difference no user could predict
    /// and no app could rely on. A peer's edit is not the local user's to
    /// revert, by either route.
    pub fn apply_remote(
        &mut self,
        doc: &mut AutoCommit,
        env: &ActionEnvelope,
        registry: &ActionRegistry,
    ) -> Result<(), ActionError> {
        qos_actions::replay(doc, env, registry)
    }

    /// Capture the undo bracket, once per event.
    ///
    /// `get_heads` flushes any pending app writes, so the bracket covers only
    /// the action's own change; on later actions in the same event it is left
    /// alone, so one event is one boundary.
    fn arm(&mut self, doc: &mut AutoCommit) {
        if self.commit_before.is_none() {
            self.commit_before = Some(doc.get_heads());
        }
    }

    /// The whole external round trip in one call: resolve an address a caller
    /// read off the app's projection, bind its arguments, build the action, and
    /// dispatch it.
    ///
    /// This is what "drive the app with no window open" reduces to. `key` and
    /// `args` are exactly what [`qos_actions::project_flat`] advertised —
    /// `key` is the same composite address the Ctrl+K palette uses for its
    /// rows, so an external caller and the palette name the same thing the same
    /// way.
    ///
    /// Strict throughout, and in an order that matters: the address is resolved
    /// and the arguments bound **before** an [`EncodedAction`] exists at all,
    /// so a bad call cannot become a partial mutation — there was never
    /// anything to hand the executor. Only once an action has been built does
    /// it go through [`dispatch_encoded`](Self::dispatch_encoded), and from
    /// there through the ordinary executor and the ordinary undo bracket.
    pub fn invoke_projected(
        &mut self,
        doc: &mut AutoCommit,
        resources: &[Resource],
        key: &str,
        args: &serde_json::Map<String, serde_json::Value>,
        registry: &ActionRegistry,
    ) -> Result<Dispatched, InvokeError> {
        let encoded = qos_actions::build_from_json(resources, key, args)?;
        self.dispatch_encoded(doc, encoded, registry)
            .map_err(|e| InvokeError::Refused(e.to_string()))
    }

    /// Rising-edge: reading clears it, so it returns `Some` once per
    /// dispatched batch, then `None`. Wire this straight to
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action).
    pub fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.commit_before.take()
    }

    pub fn last_dispatched(&self) -> Option<&EncodedAction> {
        self.last_dispatched.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use qos_actions::{ActionError, Doc, ExecutionLocus};
    use serde::{Deserialize, Serialize};

    /// Deterministic, no wall-clock / RNG — the executor's contract.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Put {
        key: String,
        value: i64,
    }

    const PUT_TYPE: &str = "test.put";

    impl Action for Put {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            doc.put(&ROOT, &self.key, ScalarValue::Int(self.value))?;
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    #[test]
    fn dispatch_applies_the_action_to_the_doc() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        host.dispatch(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "score".into(),
                value: 42,
            },
        );
        assert_eq!(
            doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
            Some(42)
        );
    }

    #[test]
    fn brackets_one_undo_boundary_across_several_actions() {
        let mut doc = AutoCommit::new();
        // A pre-existing app write; the bracket must start *after* it.
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        let heads_before_actions = doc.get_heads();

        let mut host = ActionHost::new();
        host.dispatch(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "a".into(),
                value: 1,
            },
        );
        host.dispatch(
            &mut doc,
            PUT_TYPE,
            &Put {
                key: "b".into(),
                value: 2,
            },
        );

        let committed = host
            .take_committed()
            .expect("first dispatch armed the boundary");
        assert_eq!(
            committed, heads_before_actions,
            "bracket covers only the actions"
        );
        assert!(
            host.take_committed().is_none(),
            "reading clears the boundary"
        );
    }

    #[test]
    fn last_dispatched_records_the_encoded_action() {
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        assert!(host.last_dispatched().is_none());
        let action = Put {
            key: "k".into(),
            value: 7,
        };
        host.dispatch(&mut doc, PUT_TYPE, &action);
        let enc = host
            .last_dispatched()
            .expect("dispatch recorded the action");
        assert_eq!(enc, &EncodedAction::encode(PUT_TYPE, &action).unwrap());
    }

    // -----------------------------------------------------------------------
    // The external path: an action that arrived already encoded.
    // -----------------------------------------------------------------------

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Put>(PUT_TYPE);
        r
    }

    /// A resource forest exporting the same `Put` action twice: once nullary,
    /// once with a typed integer parameter. This is what a headless caller
    /// enumerates.
    fn resources() -> Vec<Resource> {
        use qos_actions::{ActionArgs, ActionParam, ActionSpec, ParamType};
        vec![
            Resource::new("test.doc:1", "test.doc", "A document")
                .with_action(
                    ActionSpec::immediate(
                        "reset",
                        "Reset score",
                        PUT_TYPE,
                        &Put {
                            key: "score".into(),
                            value: 0,
                        },
                    )
                    .unwrap(),
                )
                .with_action(ActionSpec::typed(
                    "set-score",
                    "Set score",
                    "42",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "value",
                        ParamType::Integer,
                        "The score to record.",
                    )],
                    |args: &ActionArgs| {
                        EncodedAction::encode(
                            PUT_TYPE,
                            &Put {
                                key: "score".into(),
                                value: args.integer("value").unwrap_or(0),
                            },
                        )
                        .unwrap()
                    },
                )),
        ]
    }

    fn key(action: &str) -> String {
        format!("test.doc:1{}{action}", qos_actions::KEY_SEP)
    }

    fn json(src: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(src).unwrap()
    }

    /// THE BOUNDARY TEST. An action driven in from outside arms the *same*
    /// undo bracket, from the same point, as the same action fired by an
    /// on-screen affordance.
    ///
    /// This is what makes an externally-driven edit undoable: the host drains
    /// the bracket through `take_committed_action` and records it attributed to
    /// the local user. If the external path skipped arming, the edit would land
    /// in the document with no boundary at all and Ctrl+Z would step past it to
    /// revert something older.
    #[test]
    fn an_externally_invoked_action_brackets_undo_exactly_as_a_local_one() {
        let action = Put {
            key: "score".into(),
            value: 42,
        };
        let encoded = EncodedAction::encode(PUT_TYPE, &action).unwrap();

        // Two replicas of the *same* document, so their heads are comparable.
        // (Two independently-built `AutoCommit`s would mint different actor ids
        // and hash the identical write differently, which would make a
        // cross-document comparison meaningless rather than strict.)
        let mut base = AutoCommit::new();
        base.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        base.commit();
        let seed_heads = base.get_heads();
        let mut local_doc = AutoCommit::load(&base.save()).unwrap();
        let mut ext_doc = AutoCommit::load(&base.save()).unwrap();
        assert_eq!(local_doc.get_heads(), ext_doc.get_heads());

        // Local: the affordance path.
        let mut local_host = ActionHost::new();
        local_host.dispatch(&mut local_doc, PUT_TYPE, &action);
        let local_bracket = local_host.take_committed().expect("local armed a boundary");

        // External: the same action, arriving already encoded.
        let mut ext_host = ActionHost::new();
        ext_host
            .dispatch_encoded(&mut ext_doc, encoded, &registry())
            .expect("the external dispatch ran");
        let ext_bracket = ext_host
            .take_committed()
            .expect("external armed a boundary");

        assert_eq!(
            local_bracket, seed_heads,
            "the local bracket starts after the app's own write"
        );
        assert_eq!(
            ext_bracket, seed_heads,
            "so does the external one — the same point, not merely some point"
        );
        assert_eq!(local_bracket, ext_bracket);

        // And rising-edge behaves the same: reading clears it.
        assert!(ext_host.take_committed().is_none());

        // Same document result, too.
        assert_eq!(
            ext_doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
            local_doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64()
        );
    }

    /// Several externally-driven actions in one event collapse to one boundary,
    /// exactly as several local ones do — so an external driver cannot make a
    /// single logical edit take several Ctrl+Zs to undo.
    #[test]
    fn several_external_actions_in_one_event_share_one_boundary() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        let before = doc.get_heads();
        let mut host = ActionHost::new();
        let reg = registry();

        for (k, v) in [("a", 1), ("b", 2), ("c", 3)] {
            let enc = EncodedAction::encode(
                PUT_TYPE,
                &Put {
                    key: k.into(),
                    value: v,
                },
            )
            .unwrap();
            host.dispatch_encoded(&mut doc, enc, &reg).unwrap();
        }

        assert_eq!(host.take_committed().unwrap(), before);
        assert!(host.take_committed().is_none(), "one event, one boundary");
    }

    /// An action whose type this app does not know is refused, and — the part
    /// that matters — refused without arming a bracket. A bracket left open by
    /// a failed call would be drained by the *next* unrelated edit, which would
    /// then undo further back than the user asked.
    #[test]
    fn an_unknown_action_type_is_refused_without_arming_a_bracket() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        let before = doc.get_heads();
        let mut host = ActionHost::new();

        let bogus = EncodedAction {
            type_id: "nope".into(),
            payload: b"{}".to_vec(),
        };
        let err = host
            .dispatch_encoded(&mut doc, bogus, &registry())
            .expect_err("no decoder is registered for `nope`");
        assert!(matches!(err, ActionError::UnknownType(_)));

        assert!(
            host.take_committed().is_none(),
            "a refused dispatch leaves no bracket behind"
        );
        assert_eq!(doc.get_heads(), before, "and touches nothing");
    }

    /// A peer's replayed envelope does **not** enter the local undo stack.
    ///
    /// The same edit can also arrive by ordinary Automerge sync, which bypasses
    /// the undo stack entirely; if replay armed a bracket, whether the local
    /// user could Ctrl+Z a peer's edit would depend on which pipe delivered it.
    #[test]
    fn a_replayed_peer_envelope_does_not_arm_the_local_undo_bracket() {
        // Two replicas at the same state.
        let mut origin = AutoCommit::new();
        origin
            .put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        origin.commit();
        let mut peer = AutoCommit::load(&origin.save()).unwrap();

        // The origin dispatches; its envelope crosses to the peer.
        let action = Put {
            key: "score".into(),
            value: 9,
        };
        let env = qos_actions::apply_local(
            &mut origin,
            &action,
            EncodedAction::encode(PUT_TYPE, &action).unwrap(),
        )
        .unwrap();

        let mut peer_host = ActionHost::new();
        peer_host
            .apply_remote(&mut peer, &env, &registry())
            .unwrap();

        assert_eq!(
            origin.get_heads(),
            peer.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            peer_host.take_committed().is_none(),
            "a peer's edit is not the local user's to undo"
        );
    }

    // -----------------------------------------------------------------------
    // The full round trip: enumerate, invoke, and get the same action.
    // -----------------------------------------------------------------------

    /// An action invoked through the host from a serialized address and typed
    /// arguments produces the byte-identical [`EncodedAction`] the on-screen
    /// affordance would — proven against `last_dispatched`, which is what the
    /// affordance path records.
    #[test]
    fn invoking_from_the_projection_dispatches_the_affordances_exact_action() {
        let resources = resources();
        let reg = registry();

        // What the affordance builds, via the spec's own palette closure.
        let spec = qos_actions::resolve(&resources, &key("set-score")).unwrap();
        let from_affordance = match &spec.input {
            qos_actions::ActionInput::Prompted { build, .. } => build("42"),
            qos_actions::ActionInput::Immediate(e) => e.clone(),
        };

        // What a caller with no window gets, going through the host.
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        host.invoke_projected(
            &mut doc,
            &resources,
            &key("set-score"),
            &json(r#"{"value": 42}"#),
            &reg,
        )
        .expect("the projected invocation ran");

        assert_eq!(
            host.last_dispatched().unwrap(),
            &from_affordance,
            "the headless call dispatched the affordance's exact action"
        );
        assert_eq!(
            doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
            Some(42)
        );
    }

    /// Bad arguments are refused before the document is touched *and* before a
    /// bracket is armed — the refusal happens while there is still nothing to
    /// apply.
    #[test]
    fn a_projected_invocation_with_bad_arguments_changes_nothing() {
        let resources = resources();
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        let before = doc.get_heads();
        let mut host = ActionHost::new();

        let err = host
            .invoke_projected(
                &mut doc,
                &resources,
                &key("set-score"),
                &json(r#"{"value": "forty-two"}"#),
                &registry(),
            )
            .expect_err("text is not an integer");

        assert_eq!(err.code(), "invalid_arguments");
        assert_eq!(doc.get_heads(), before, "the document is untouched");
        assert!(host.take_committed().is_none(), "no bracket was armed");
        assert!(host.last_dispatched().is_none(), "nothing was dispatched");
    }

    /// A nullary projected action invokes with no arguments at all.
    #[test]
    fn a_nullary_projected_action_invokes_with_no_arguments() {
        let resources = resources();
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "score", ScalarValue::Int(7)).unwrap();
        let mut host = ActionHost::new();

        host.invoke_projected(
            &mut doc,
            &resources,
            &key("reset"),
            &serde_json::Map::new(),
            &registry(),
        )
        .expect("reset takes nothing");

        assert_eq!(
            doc.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
            Some(0)
        );
    }

    /// A stale address is refused as an address problem, distinct from an
    /// argument problem — the two are fixed differently.
    #[test]
    fn a_projected_invocation_of_an_unknown_address_is_refused() {
        let resources = resources();
        let mut doc = AutoCommit::new();
        let mut host = ActionHost::new();
        let err = host
            .invoke_projected(
                &mut doc,
                &resources,
                &key("set-volume"),
                &serde_json::Map::new(),
                &registry(),
            )
            .expect_err("no such action");
        assert_eq!(err.code(), "unknown_action");
    }
}
