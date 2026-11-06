//! The workspace commits an endpoint has seen: one action, every unit it
//! wrote, delivered all together or not at all.
//!
//! A commit arrives inside the signed payload of each part's document op
//! ([`collab_types::WorkspaceCommit`]). The gate here holds a part's event
//! until every part of its action has arrived, then releases them in the
//! commit's own order, so an app never applies half an action and every peer
//! that can read all parts applies them in the same order. A peer that
//! receives only some parts keeps the ones it has, undelivered, until the
//! rest come.
//!
//! The log is also the action log history reads: [`Commits::history`] names
//! one action with its author, its time, the action it ran and every unit
//! change it made, in one query.

use std::collections::BTreeMap;

use collab_types::{CommitAction, CommitPart, WorkspaceCommit};

use crate::CollabEvent;
use crate::broker::BridgeState;

/// Commits whose every part has arrived, and commits this endpoint received
/// only parts of.
#[derive(Default)]
pub(crate) struct Commits {
    /// Complete actions, in arrival order of their last part.
    log: Vec<CommitHistory>,
    /// Actions some of whose parts have not arrived, oldest first.
    held: Vec<HeldCommit>,
}

/// One complete action, as history reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitHistory {
    /// The commit's content address.
    pub commit: [u8; 32],
    /// The workspace whose units the action wrote.
    pub workspace: String,
    /// The principal that invoked it, when one did.
    pub principal: Option<String>,
    /// The device that signed it.
    pub device: [u8; 32],
    /// When it was made, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The action's name, item and invocation.
    pub action: CommitAction,
    /// Every unit it wrote, with the hashes of the changes it made there.
    pub parts: Vec<CommitPart>,
}

struct HeldCommit {
    commit: WorkspaceCommit,
    /// The part events that arrived, by unit.
    parts: BTreeMap<(String, String), CollabEvent>,
}

/// What to do with the document event that arrived with a commit.
pub(crate) enum Delivery {
    /// Deliver it: it is no part of an action (or its action is complete).
    Now(CollabEvent),
    /// Keep it back until the rest of its action arrives.
    Waiting,
    /// Deliver every part of this action, in the commit's order.
    Complete(Vec<CollabEvent>),
    /// Drop it, and why: it is not the part of the commit it names.
    Refused(&'static str),
}

/// Hold back the event of one arriving part until all of its action is here,
/// then hand back all of its parts. `unit` is the (app, record) whose route
/// the part arrived on, and `claims` the change hashes its signed envelope
/// claims.
pub(crate) fn gate(
    state: &BridgeState,
    commit: Option<WorkspaceCommit>,
    claims: &[[u8; 32]],
    unit: (&str, &str),
    event: CollabEvent,
) -> Delivery {
    let Some(commit) = commit else {
        return Delivery::Now(event);
    };
    let Some(part) = commit.part(unit.0, unit.1) else {
        return Delivery::Refused("the commit names no part on the route it arrived on");
    };
    // The signed envelope claims each change it carries; a commit naming
    // changes the envelope does not claim would let an action smuggle units
    // past the receiver's claims decision. A cleartext envelope carries its
    // claims in the changes themselves, which the app decides.
    if !claims.is_empty() && !part.changes.iter().all(|change| claims.contains(change)) {
        return Delivery::Refused("the commit names changes its envelope does not claim");
    }
    state.commits().arrive(commit, unit, event)
}

impl Commits {
    /// Note the arrival of one part of `commit` on the unit `(app, record)`,
    /// keeping its `event` until the action's other parts are here.
    pub(crate) fn arrive(
        &mut self,
        commit: WorkspaceCommit,
        unit: (&str, &str),
        event: CollabEvent,
    ) -> Delivery {
        let id = commit.id();
        if self.log.iter().any(|entry| entry.commit == id) {
            return Delivery::Now(event);
        }
        if !commit.covers(unit.0, unit.1) {
            return Delivery::Refused("the commit names no part on the route it arrived on");
        }
        let key = (unit.0.to_owned(), unit.1.to_owned());
        let Some(held) = self.held.iter_mut().find(|held| held.commit.id() == id) else {
            if self.held.len() >= MAX_HELD_COMMITS {
                // A peer that never sends the rest of an action must not
                // keep this endpoint's memory: the oldest incomplete action
                // goes. Its parts are not applied, so nothing is half-done.
                let dropped = self.held.remove(0);
                tracing::warn!(
                    commit = %hex::encode(dropped.commit.id()),
                    parts = dropped.parts.len(),
                    "collab: incomplete action dropped; its parts are still missing"
                );
            }
            let mut parts = BTreeMap::new();
            parts.insert(key, event);
            self.held.push(HeldCommit { commit, parts });
            return self.complete_if_whole();
        };
        held.parts.insert(key, event);
        self.complete_if_whole()
    }

    /// Move an action into the log once every part of it is here, handing
    /// back its events in the commit's own part order.
    fn complete_if_whole(&mut self) -> Delivery {
        let Some(index) = self.held.iter().position(|held| {
            held.commit
                .parts
                .iter()
                .all(|part| held.parts.contains_key(&key(part)))
        }) else {
            return Delivery::Waiting;
        };
        let mut held = self.held.remove(index);
        let mut parts = std::mem::take(&mut held.parts);
        let events = held
            .commit
            .parts
            .iter()
            .filter_map(|part| parts.remove(&key(part)))
            .collect();
        self.log.push(CommitHistory::of(&held.commit));
        Delivery::Complete(events)
    }

    /// Note an action this endpoint itself made: every part of it is here.
    pub(crate) fn record(&mut self, commit: WorkspaceCommit) {
        let id = commit.id();
        if self.log.iter().any(|entry| entry.commit == id) {
            return;
        }
        self.log.push(CommitHistory::of(&commit));
    }

    /// Every action this endpoint has seen in `workspace`, oldest first: one
    /// action with its author, its time and every unit change it made.
    #[must_use]
    pub fn history(&self, workspace: &str) -> Vec<CommitHistory> {
        let mut actions: Vec<CommitHistory> = self
            .log
            .iter()
            .filter(|entry| entry.workspace == workspace)
            .cloned()
            .collect();
        actions.sort_by(|left, right| (left.at_ms, left.commit).cmp(&(right.at_ms, right.commit)));
        actions
    }

    /// How many parts of actions are held back waiting for the rest.
    #[cfg(test)]
    pub(crate) fn held_parts(&self) -> usize {
        self.held.iter().map(|held| held.parts.len()).sum()
    }
}

impl CommitHistory {
    fn of(commit: &WorkspaceCommit) -> Self {
        Self {
            commit: commit.id(),
            workspace: commit.workspace.clone(),
            principal: commit.author.principal.clone(),
            device: commit.author.device,
            at_ms: commit.at_ms,
            action: commit.action.clone(),
            parts: commit.parts.clone(),
        }
    }
}

fn key(part: &CommitPart) -> (String, String) {
    (part.app.clone(), part.record.clone())
}

/// Actions this endpoint holds parts of, waiting for the rest. Bounded so a
/// peer that never completes one cannot grow this endpoint.
const MAX_HELD_COMMITS: usize = 32;

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ReadDoc as _};
    use collab_types::{
        AttestationId, DocumentMutation, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
        mutation_claims, record_unit,
    };
    use tilo_actions::{ActionContext, Did, EditOp, FieldEdit, InvocationId, PathStep};

    const DEVICE: [u8; 32] = [7; 32];
    const WORKSPACE: &str = "workspace";

    /// A record holding `field` = `value`, with the history entry an app's
    /// executor writes: the action `field`, invoked by alice under
    /// `invocation`.
    fn record(field: &str, value: &str, invocation: InvocationId) -> (AutoCommit, [u8; 32]) {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, field, value).unwrap();
        let context = ActionContext {
            what: Some(Did::Edit(FieldEdit {
                ty: "tasks".to_owned(),
                field: field.to_owned(),
                path: vec![PathStep::Field(field.to_owned())],
                op: EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            })),
            invoker: Some(tilo_actions::ActionInvoker::actor("alice")),
            source_invocation: Some(invocation),
            ..ActionContext::for_resource("tasks", "tasks", field)
        };
        let mut options = automerge::transaction::CommitOptions::default();
        options.set_message(context.change_message().unwrap());
        let hash = doc.commit_with(options).unwrap();
        (doc, hash.0)
    }

    fn mutation(doc: &mut AutoCommit) -> DocumentMutation {
        DocumentMutation::Incremental(doc.save_after(&[]))
    }

    /// One unit's signed publication of `doc` under `commit`.
    fn publication(
        commit: &WorkspaceCommit,
        app: &str,
        record_id: &str,
        doc: &AutoCommit,
        seq: u64,
    ) -> SignedOp {
        let mutation = mutation(&mut doc.clone());
        let frame = mutation.encode_frames().unwrap().remove(0);
        let framed = commit.frame(&frame).unwrap();
        let claims = mutation_claims(&mutation, record_unit).unwrap();
        SignedOp::new_unsigned(
            framed,
            document_artifact_id(app, WORKSPACE, record_id),
            Ed25519PublicKey::from_bytes(DEVICE),
            AttestationId::new(),
            OpKind::Change,
            std::time::UNIX_EPOCH,
            seq,
        )
        .with_claims(claims)
    }

    fn event(op: &SignedOp) -> CollabEvent {
        CollabEvent::Document {
            envelope: Box::new(op.clone()),
            from_user_id: "peer".to_owned(),
            decrypted_payload: None,
        }
    }

    /// What an app does with a delivered event: apply it to its replica.
    fn apply(peer: &mut AutoCommit, event: &CollabEvent) {
        let CollabEvent::Document {
            envelope,
            decrypted_payload,
            ..
        } = event
        else {
            unreachable!("a document event")
        };
        let bytes = decrypted_payload.as_deref().unwrap_or(&envelope.payload);
        match DocumentMutation::decode(bytes).unwrap() {
            DocumentMutation::Incremental(changes) => {
                peer.load_incremental(&changes).unwrap();
            }
            DocumentMutation::Snapshot(_) => unreachable!("an incremental change set"),
        }
    }

    fn deliver(
        state: &BridgeState,
        op: &SignedOp,
        app: &str,
        record_id: &str,
        peer: &mut AutoCommit,
    ) -> bool {
        let commit = WorkspaceCommit::of_envelope(op).unwrap();
        let claims: Vec<[u8; 32]> = op.claims.iter().map(|claim| claim.change).collect();
        match gate(state, commit, &claims, (app, record_id), event(op)) {
            Delivery::Now(event) => {
                apply(peer, &event);
                true
            }
            Delivery::Waiting => false,
            Delivery::Complete(events) => {
                assert_eq!(events.len(), 2, "both parts of the action arrive together");
                for event in &events {
                    apply(peer, event);
                }
                true
            }
            Delivery::Refused(reason) => panic!("refused: {reason}"),
        }
    }

    fn two_unit_action() -> (WorkspaceCommit, AutoCommit, AutoCommit) {
        let invocation = InvocationId(uuid::Uuid::from_u128(11));
        let (first, first_change) = record("title", "child", invocation);
        let (second, second_change) = record("body", "text", invocation);
        let commit = WorkspaceCommit::new(
            WORKSPACE,
            collab_types::CommitAuthor {
                principal: Some("alice".to_owned()),
                device: DEVICE,
            },
            1_700_000_000_000,
            collab_types::CommitAction {
                name: "title".to_owned(),
                item: None,
                invocation: Some(invocation.0),
            },
            vec![
                CommitPart {
                    app: "tasks".to_owned(),
                    record: "a".to_owned(),
                    changes: vec![first_change],
                },
                CommitPart {
                    app: "tasks".to_owned(),
                    record: "b".to_owned(),
                    changes: vec![second_change],
                },
            ],
        );
        (commit, first, second)
    }

    #[test]
    fn a_peer_shows_nothing_of_an_action_until_every_part_of_it_arrives() {
        let (commit, first, second) = two_unit_action();
        let state = BridgeState::new();
        let mut peer = AutoCommit::new();

        let part_a = publication(&commit, "tasks", "a", &first, 1);
        assert!(
            !deliver(&state, &part_a, "tasks", "a", &mut peer),
            "the first part alone is not applied"
        );
        assert_eq!(peer.get(automerge::ROOT, "title").unwrap(), None);
        assert_eq!(peer.get(automerge::ROOT, "body").unwrap(), None);
        assert_eq!(state.commits().held_parts(), 1);

        let part_b = publication(&commit, "tasks", "b", &second, 2);
        assert!(
            deliver(&state, &part_b, "tasks", "b", &mut peer),
            "the second part completes the action"
        );
        assert!(peer.get(automerge::ROOT, "title").unwrap().is_some());
        assert!(peer.get(automerge::ROOT, "body").unwrap().is_some());
        assert_eq!(state.commits().held_parts(), 0);

        // The same part again is a re-delivery of an action already applied:
        // it goes through on its own and changes nothing new.
        assert!(deliver(&state, &part_a, "tasks", "a", &mut peer));
    }

    #[test]
    fn restart_keeps_the_state_of_a_committed_action() {
        let (commit, first, second) = two_unit_action();
        let state = BridgeState::new();
        let mut peer = AutoCommit::new();
        deliver(
            &state,
            &publication(&commit, "tasks", "a", &first, 1),
            "tasks",
            "a",
            &mut peer,
        );
        deliver(
            &state,
            &publication(&commit, "tasks", "b", &second, 2),
            "tasks",
            "b",
            &mut peer,
        );

        let reloaded = AutoCommit::load(&peer.save()).unwrap();
        assert!(reloaded.get(automerge::ROOT, "title").unwrap().is_some());
        assert!(reloaded.get(automerge::ROOT, "body").unwrap().is_some());

        // The commit itself is a durable record, and the same action reads
        // the same after a restart.
        let stored = commit.encode();
        let reread = WorkspaceCommit::decode(&stored).unwrap();
        assert_eq!(reread.id(), commit.id());
        assert_eq!(reread.parts, commit.parts);
    }

    #[test]
    fn one_history_query_names_the_action_with_its_author_time_and_both_units() {
        let (commit, first, second) = two_unit_action();
        let state = BridgeState::new();
        let mut peer = AutoCommit::new();
        deliver(
            &state,
            &publication(&commit, "tasks", "a", &first, 1),
            "tasks",
            "a",
            &mut peer,
        );
        deliver(
            &state,
            &publication(&commit, "tasks", "b", &second, 2),
            "tasks",
            "b",
            &mut peer,
        );
        state.commits().record(commit.clone());

        let history = state.commits().history(WORKSPACE);
        assert_eq!(history.len(), 1, "one action, one entry: {history:?}");
        let action = &history[0];
        assert_eq!(action.commit, commit.id());
        assert_eq!(action.principal.as_deref(), Some("alice"));
        assert_eq!(action.device, DEVICE);
        assert_eq!(action.at_ms, commit.at_ms);
        assert_eq!(action.action.name, "title");
        assert_eq!(action.action.invocation, Some(uuid::Uuid::from_u128(11)));
        assert_eq!(action.parts, commit.parts);
        assert_eq!(action.parts.len(), 2);
        assert_eq!(action.parts[0].changes, vec![commit.parts[0].changes[0]]);
        assert!(state.commits().history("elsewhere").is_empty());
    }

    #[test]
    fn a_part_the_action_does_not_name_is_refused() {
        let (commit, first, _) = two_unit_action();
        let state = BridgeState::new();
        let op = publication(&commit, "tasks", "a", &first, 1);
        let claims: Vec<[u8; 32]> = op.claims.iter().map(|claim| claim.change).collect();
        assert!(matches!(
            gate(
                &state,
                WorkspaceCommit::of_envelope(&op).unwrap(),
                &claims,
                ("tasks", "elsewhere"),
                event(&op)
            ),
            Delivery::Refused(_)
        ));
        assert!(matches!(
            gate(
                &state,
                WorkspaceCommit::of_envelope(&op).unwrap(),
                &[[0; 32]],
                ("tasks", "a"),
                event(&op)
            ),
            Delivery::Refused(_)
        ));
    }

    #[test]
    fn a_part_signed_by_another_device_is_refused() {
        let (commit, first, _) = two_unit_action();
        let frame = mutation(&mut first.clone())
            .encode_frames()
            .unwrap()
            .remove(0);
        let framed = commit.frame(&frame).unwrap();
        let op = SignedOp::new_unsigned(
            framed,
            document_artifact_id("tasks", WORKSPACE, "a"),
            Ed25519PublicKey::from_bytes([8; 32]),
            AttestationId::new(),
            OpKind::Change,
            std::time::UNIX_EPOCH,
            1,
        );
        assert_eq!(
            WorkspaceCommit::of_envelope(&op),
            Err(collab_types::CommitError::ForeignDevice)
        );
    }

    #[test]
    fn an_incomplete_action_does_not_grow_without_bound() {
        let invocation = InvocationId::new();
        let state = BridgeState::new();
        let mut peer = AutoCommit::new();
        let (first, first_change) = record("title", "child", invocation);
        for index in 0..(MAX_HELD_COMMITS + 3) {
            let commit = WorkspaceCommit::new(
                WORKSPACE,
                collab_types::CommitAuthor {
                    principal: None,
                    device: DEVICE,
                },
                u64::try_from(index).unwrap(),
                collab_types::CommitAction {
                    name: "title".to_owned(),
                    item: None,
                    invocation: None,
                },
                vec![
                    CommitPart {
                        app: "tasks".to_owned(),
                        record: "a".to_owned(),
                        changes: vec![first_change],
                    },
                    CommitPart {
                        app: "tasks".to_owned(),
                        record: "b".to_owned(),
                        changes: vec![[u8::try_from(index).unwrap(); 32]],
                    },
                ],
            );
            deliver(
                &state,
                &publication(&commit, "tasks", "a", &first, u64::try_from(index).unwrap()),
                "tasks",
                "a",
                &mut peer,
            );
        }
        assert!(state.commits().held_parts() <= MAX_HELD_COMMITS);
    }
}
