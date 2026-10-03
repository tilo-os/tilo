//! Stacking several actions onto one document in a single turn.
//!
//! An adopter applying a handful of actions before the host next drains
//! the CRDT (renders, replicates, diffs) is doing something ordinary —
//! a multi-step edit, a paste fanning to several mutations, a batch
//! command. Every action pins a *fresh* actor id, sorting into
//! arbitrary positions in the document's actor table. These tests stack
//! many dispatches back-to-back on one `AutoCommit` with nothing
//! draining between, asserting the run is deterministic: all actions
//! apply, and a replaying peer lands on the byte-identical document.
//! The loop count smokes out the order-dependent flake — an unlucky
//! actor-sort must not panic or diverge.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionError, ActionRegistry, Doc, EncodedAction, LocalTransport, apply_local, dispatch,
    replay,
};

/// A real action: set a scalar under a key, or append to a list. Pure
/// function of `(self, doc)` — no wall-clock, no RNG — so replay is deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Counter {
    Put {
        key: String,
        value: i64,
    },
    Append {
        key: String,
        value: String,
    },
    /// A well-behaved action that legitimately performs no mutation this
    /// turn (a toggle whose precondition already holds). Its `apply` opens
    /// no transaction and writes no op.
    Noop,
    /// A mutating call with no effect (a zero-length splice): unlike
    /// [`Counter::Noop`] this *opens* a transaction but writes no op — the
    /// pathological shape of a no-op that still touched the document API.
    EmptySplice,
    /// An action that writes an op and *then* fails, exercising the
    /// mid-apply error/rollback path.
    WriteThenError,
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
            Counter::Noop => {}
            Counter::EmptySplice => {
                // Ensure a list exists, then splice nothing into it: a mutating API
                // call that opens a transaction yet produces zero ops.
                let list = match doc.get(&ROOT, "spliced")? {
                    Some((automerge::Value::Object(automerge::ObjType::List), id)) => id,
                    _ => doc.put_object(&ROOT, "spliced", automerge::ObjType::List)?,
                };
                let len = doc.length(&list);
                doc.splice(&list, len, 0, std::iter::empty::<ScalarValue>())?;
            }
            Counter::WriteThenError => {
                doc.put(&ROOT, "partial", ScalarValue::Int(1))?;
                let err = <serde_json::Error as serde::ser::Error>::custom(
                    "deliberate mid-apply failure",
                );
                return Err(ActionError::Encode(err));
            }
        }
        Ok(())
    }
}

fn registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();
    r.register::<Counter>(COUNTER_TYPE);
    r
}

/// Two replicas that share history, so a replayed change has identical deps.
fn two_replicas() -> (AutoCommit, AutoCommit) {
    let mut a = AutoCommit::new();
    a.put(&ROOT, "seed", ScalarValue::Str("s".into())).unwrap();
    a.commit();
    let bytes = a.save();
    let b = AutoCommit::load(&bytes).unwrap();
    (a, b)
}

/// Stack `n` `dispatch` calls of mixed loci onto one `AutoCommit` with
/// nothing draining between, then replay onto a fresh peer and require
/// byte-identical convergence. Repeated `iterations` times to exercise
/// the random actor-id sort order that used to make this flake.
fn stacked_dispatch_converges(n: usize, iterations: usize) {
    for iter in 0..iterations {
        let (mut a, mut b) = two_replicas();
        let mut tx = LocalTransport::new();
        let mut envelopes = Vec::new();

        for i in 0..n {
            // Alternate map/list mutation shapes on the same document. Host
            // locus behavior is covered separately and deliberately cannot
            // participate without an authoritative transport.
            let action = if i % 2 == 0 {
                Counter::Put {
                    key: format!("k{i}"),
                    value: i64::try_from(i).unwrap_or(i64::MAX),
                }
            } else {
                Counter::Append {
                    key: "log".into(),
                    value: format!("v{i}"),
                }
            };
            let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
            let dispatched = dispatch(&mut a, &action, encoded, &mut tx)
                .unwrap_or_else(|e| panic!("iter {iter}: dispatch {i} failed: {e}"));
            envelopes.push(dispatched.envelope().clone());
        }

        // Every stacked action landed a distinct scalar/append.
        for i in (0..n).step_by(2) {
            let got = a
                .get(&ROOT, format!("k{i}"))
                .unwrap()
                .unwrap_or_else(|| panic!("iter {iter}: key k{i} missing after stack"))
                .0;
            assert_eq!(
                got.as_i64(),
                Some(i64::try_from(i).unwrap_or(i64::MAX)),
                "iter {iter}: k{i} value"
            );
        }
        let appended = (0..n).filter(|i| i % 2 == 1).count();
        if appended > 0 {
            let (_, list) = a.get(&ROOT, "log").unwrap().unwrap();
            assert_eq!(a.length(&list), appended, "iter {iter}: append count");
        }

        // A peer that replays the stacked envelopes from the shared base
        // lands on the byte-identical document — same head hashes, not just values.
        for env in &envelopes {
            replay(&mut b, env, &registry())
                .unwrap_or_else(|e| panic!("iter {iter}: replay failed: {e}"));
        }
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "iter {iter}: stacked dispatch must converge to the identical document"
        );
    }
}

/// Three and ten stacked dispatches, a stack with an active patch log, and stacked `apply_local`
/// all converge deterministically.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn stacked_dispatches_are_deterministic() {
    three_stacked_dispatches_are_deterministic();
    ten_stacked_dispatches_are_deterministic();
    stacked_dispatch_with_active_patch_log_is_deterministic();
    stacked_apply_local_is_deterministic();
}

fn three_stacked_dispatches_are_deterministic() {
    stacked_dispatch_converges(3, 200);
}

fn ten_stacked_dispatches_are_deterministic() {
    stacked_dispatch_converges(10, 100);
}

/// A no-op action, an empty mutation and an erroring action (which rolls back) leave the
/// executor uncorrupted.
/// Runs each phase below in order; every phase builds its own fixture.
#[test]
fn odd_actions_in_a_stack_do_not_corrupt_the_executor() {
    noop_action_in_a_stack_does_not_corrupt_the_executor();
    empty_mutation_in_a_stack_does_not_corrupt_the_executor();
    erroring_action_rolls_back_without_corrupting_the_executor();
}

/// The same stack via `apply_local` directly (the `ClientLocal` origin
/// path), no transport in the loop. A zero-op action in the middle must
/// not corrupt the executor.
///
/// The sharp edge: `commit_with` always opens a transaction, so a
/// zero-op action still pins its fresh actor into the document's actor
/// table — then, being empty, that actor is pruned on commit. Any later
/// dispatch reconciling the automerge patch log used to panic
/// (`PatchLogMismatch`) on the pruned "ghost" actor. Stacking a
/// well-behaved no-op must stay deterministic.
fn noop_action_in_a_stack_does_not_corrupt_the_executor() {
    for iter in 0..200 {
        let (mut a, mut b) = two_replicas();
        let mut tx = LocalTransport::new();
        let mut envelopes = Vec::new();

        let actions = [
            Counter::Put {
                key: "a".into(),
                value: 1,
            },
            Counter::Noop,
            Counter::Put {
                key: "b".into(),
                value: 2,
            },
            Counter::Noop,
            Counter::Append {
                key: "log".into(),
                value: "x".into(),
            },
            Counter::Put {
                key: "c".into(),
                value: 3,
            },
        ];
        for (i, action) in actions.iter().enumerate() {
            let encoded = EncodedAction::encode(COUNTER_TYPE, action).unwrap();
            let d = dispatch(&mut a, action, encoded, &mut tx)
                .unwrap_or_else(|e| panic!("iter {iter}: dispatch {i} ({action:?}) failed: {e}"));
            // A no-op action legitimately mutates nothing, so it produces no new
            // change and nothing to replicate; the real ones do.
            if d.mutated_local() {
                envelopes.push(d.envelope().clone());
            }
        }

        for (k, v) in [("a", 1i64), ("b", 2), ("c", 3)] {
            let got = a.get(&ROOT, k).unwrap().unwrap().0;
            assert_eq!(got.as_i64(), Some(v), "iter {iter}: {k}");
        }
        for env in &envelopes {
            replay(&mut b, env, &registry())
                .unwrap_or_else(|e| panic!("iter {iter}: replay failed: {e}"));
        }
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "iter {iter}: converge with no-ops in the stack"
        );
    }
}

/// The same stacking on a document whose patch log is *active* and never
/// drained between dispatches — the state a rendering host leaves the
/// CRDT in (`update_diff_cursor` armed, no frame drawn in between).
fn stacked_dispatch_with_active_patch_log_is_deterministic() {
    for iter in 0..200 {
        let mut a = AutoCommit::new();
        a.put(&ROOT, "seed", ScalarValue::Str("s".into())).unwrap();
        a.commit();
        // A host arms diff-based rendering and then does NOT drain between edits.
        a.update_diff_cursor();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let mut tx = LocalTransport::new();
        let mut envelopes = Vec::new();
        let actions = [
            Counter::Put {
                key: "a".into(),
                value: 1,
            },
            Counter::Noop,
            Counter::Append {
                key: "log".into(),
                value: "x".into(),
            },
            Counter::Put {
                key: "b".into(),
                value: 2,
            },
            Counter::Noop,
        ];
        for (i, action) in actions.iter().enumerate() {
            let encoded = EncodedAction::encode(COUNTER_TYPE, action).unwrap();
            let d = dispatch(&mut a, action, encoded, &mut tx)
                .unwrap_or_else(|e| panic!("iter {iter}: dispatch {i} failed: {e}"));
            if d.mutated_local() {
                envelopes.push(d.envelope().clone());
            }
        }
        for env in &envelopes {
            replay(&mut b, env, &registry()).unwrap();
        }
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "iter {iter}: converge (active log)"
        );
    }
}

/// A mutating-but-empty action stacked between real ones must not
/// strand a ghost actor either.
fn empty_mutation_in_a_stack_does_not_corrupt_the_executor() {
    for iter in 0..200 {
        let (mut a, mut b) = two_replicas();
        a.update_diff_cursor(); // a rendering host with a live, undrained diff index
        let mut tx = LocalTransport::new();
        let mut envelopes = Vec::new();
        let actions = [
            Counter::Put {
                key: "a".into(),
                value: 1,
            },
            Counter::EmptySplice,
            Counter::Put {
                key: "b".into(),
                value: 2,
            },
            Counter::EmptySplice,
            Counter::Put {
                key: "c".into(),
                value: 3,
            },
        ];
        for (i, action) in actions.iter().enumerate() {
            let encoded = EncodedAction::encode(COUNTER_TYPE, action).unwrap();
            let d = dispatch(&mut a, action, encoded, &mut tx)
                .unwrap_or_else(|e| panic!("iter {iter}: dispatch {i} failed: {e}"));
            envelopes.push(d.envelope().clone());
        }
        for (k, v) in [("a", 1i64), ("b", 2), ("c", 3)] {
            assert_eq!(
                a.get(&ROOT, k).unwrap().unwrap().0.as_i64(),
                Some(v),
                "iter {iter}: {k}"
            );
        }
        for env in &envelopes {
            replay(&mut b, env, &registry()).unwrap();
        }
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "iter {iter}: converge with empty mutations"
        );
    }
}

/// An action that writes an op and then errors must roll back cleanly:
/// later dispatches must not panic, and the failed action must leave no
/// trace in the document.
fn erroring_action_rolls_back_without_corrupting_the_executor() {
    for iter in 0..200 {
        let (mut a, mut b) = two_replicas();
        a.update_diff_cursor();
        let mut tx = LocalTransport::new();

        let ok1 = Counter::Put {
            key: "a".into(),
            value: 1,
        };
        let e1 = EncodedAction::encode(COUNTER_TYPE, &ok1).unwrap();
        let env1 = dispatch(&mut a, &ok1, e1, &mut tx)
            .unwrap()
            .envelope()
            .clone();

        // The erroring action returns Err from dispatch and must not have written.
        let bad = Counter::WriteThenError;
        let eb = EncodedAction::encode(COUNTER_TYPE, &bad).unwrap();
        assert!(
            dispatch(&mut a, &bad, eb, &mut tx).is_err(),
            "iter {iter}: erroring action reports Err"
        );
        assert!(
            a.get(&ROOT, "partial").unwrap().is_none(),
            "iter {iter}: the failed action left no partial write"
        );

        // A later dispatch on the same document must still work (no ghost actor).
        let ok2 = Counter::Put {
            key: "b".into(),
            value: 2,
        };
        let e2 = EncodedAction::encode(COUNTER_TYPE, &ok2).unwrap();
        let env2 = dispatch(&mut a, &ok2, e2, &mut tx)
            .unwrap()
            .envelope()
            .clone();

        assert_eq!(
            a.get(&ROOT, "a").unwrap().unwrap().0.as_i64(),
            Some(1),
            "iter {iter}: a"
        );
        assert_eq!(
            a.get(&ROOT, "b").unwrap().unwrap().0.as_i64(),
            Some(2),
            "iter {iter}: b"
        );

        replay(&mut b, &env1, &registry()).unwrap();
        replay(&mut b, &env2, &registry()).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "iter {iter}: converge after an error"
        );
    }
}

fn stacked_apply_local_is_deterministic() {
    for iter in 0..200 {
        let (mut a, mut b) = two_replicas();
        let mut envs = Vec::new();
        for i in 0..6 {
            let action = Counter::Put {
                key: format!("k{i}"),
                value: i64::from(i),
            };
            let encoded = EncodedAction::encode(COUNTER_TYPE, &action).unwrap();
            let env = apply_local(&mut a, &action, encoded)
                .unwrap_or_else(|e| panic!("iter {iter}: apply_local {i} failed: {e}"));
            envs.push(env);
        }
        for env in &envs {
            replay(&mut b, env, &registry()).unwrap();
        }
        assert_eq!(a.get_heads(), b.get_heads(), "iter {iter}: converge");
    }
}
