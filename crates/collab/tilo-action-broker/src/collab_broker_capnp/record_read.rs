//! Record reads outside their app: the door an agent task's host reads a room's records
//! through (`docs/designs/app-language/agents.md`, "Reading records outside their app").
//!
//! The caller is the agent acting for the person who admitted it ([`Caller::acting_for`]),
//! never one a person's device handed its session to, and holds no app's route. Each read is
//! decided at the record's node with that actor, the lesser of the agent's grants and the
//! person's ([`BrokerActor::answer_in`]), then captured by the room's document owner under the
//! person's device certificate, whose own check ([`NativeDocumentReads::authorize`]) is the
//! person's read of the record. The capture's revalidation decides the agent's read again
//! before every chunk it hands out. What leaves the broker is the record's committed content,
//! read-only: the record channel's keys stay here, and nothing in a read writes to the record.

use std::sync::{Arc, Weak};

use tilo_action_policy::{ActionKind, Answer};
use tilo_actions::remote::{
    ActionHostError, Caller, CallerPrincipal, RECORD_READS_FOR_TASKS, RecordAddress, RecordChange,
    RecordChanges, RecordCopy, RecordReads,
};

use runtime_collab::committed_document::MAX_DOCUMENT_BYTES;

use super::committed_document::{NativeDocumentRead, NativeDocumentReadContext, RecordRead};
use super::{CapnpBrokerState, CrdtRoute};

/// What a caller is told about a record it may not read.
pub const RECORD_READ_REFUSED: &str = "not allowed to read this record";

impl CapnpBrokerState {
    /// The record read door, when this host keeps a room's documents and knows its person's
    /// device.
    pub fn record_reads(self: &Arc<Self>) -> Option<Arc<dyn RecordReads>> {
        self.services.committed_documents.as_ref()?;
        self.services.local_person.as_ref()?;
        Some(Arc::new(BrokerRecordReads {
            state: Arc::downgrade(self),
        }))
    }

    /// Whether `caller` may read `address` in `room` now: decided at the record's node with
    /// the caller's actor, an agent's bounded by its person's authority.
    fn may_read_record(
        &self,
        caller: &Caller,
        room: &str,
        address: &RecordAddress,
    ) -> Result<(), ActionHostError> {
        super::action_host::check_execution(caller)?;
        let node = collab_types::document_node(room, &address.app, &address.record);
        match self.decide_for(caller, room, |workspace, actor, now| {
            actor.answer_in(workspace, &node, ActionKind::Read, now) == Answer::Allow
        }) {
            Some(true) => Ok(()),
            Some(false) => Err(ActionHostError::Refused {
                reason: RECORD_READ_REFUSED.into(),
            }),
            None => Err(ActionHostError::NotPermitted),
        }
    }
}

/// What a caller is told about a change the room does not allow it.
pub const RECORD_CHANGE_REFUSED: &str = "not allowed to make this change";

impl CapnpBrokerState {
    /// The record change door, when this host has an origin for its rooms' records.
    pub fn record_changes(self: &Arc<Self>) -> Option<Arc<dyn RecordChanges>> {
        self.services.record_changes.as_ref()?;
        Some(Arc::new(BrokerRecordChanges {
            state: Arc::downgrade(self),
        }))
    }
}

struct BrokerRecordChanges {
    state: Weak<CapnpBrokerState>,
}

impl RecordChanges for BrokerRecordChanges {
    fn apply(
        &self,
        caller: &Caller,
        change: &RecordChange,
    ) -> Result<serde_json::Value, ActionHostError> {
        let unavailable = |reason: String| ActionHostError::Unavailable { reason };
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| unavailable("the broker stopped".into()))?;
        let (Some((person, room)), CallerPrincipal::Agent(agent)) =
            (task_person(caller), caller.principal())
        else {
            return Err(ActionHostError::Refused {
                reason: RECORD_READS_FOR_TASKS.into(),
            });
        };
        super::action_host::check_execution(caller)?;
        let answer = state
            .decide_for(caller, room, |workspace, actor, now| {
                let node = tilo_actions::action_node(
                    &workspace.node(),
                    &change.app,
                    "",
                    &change.resource,
                    None,
                    &change.action,
                );
                actor.answer_in(workspace, &node, ActionKind::Undoable, now)
            })
            .ok_or(ActionHostError::NotPermitted)?;
        match answer {
            Answer::Allow => {}
            Answer::Ask => return Err(ActionHostError::ConfirmationRequired),
            Answer::Deny => {
                return Err(ActionHostError::Refused {
                    reason: RECORD_CHANGE_REFUSED.into(),
                });
            }
        }
        apply_record_change(state, room, change, (agent, person))
    }
}

/// Apply `change` in `room` at its record's origin as `agent` acting for
/// `person`, admitted already.
fn apply_record_change(
    state: Arc<CapnpBrokerState>,
    room: &str,
    change: &RecordChange,
    (agent, person): (&str, &str),
) -> Result<serde_json::Value, ActionHostError> {
    let unavailable = |reason: String| ActionHostError::Unavailable { reason };
    let origin = state
        .services
        .record_changes
        .clone()
        .ok_or_else(|| unavailable("this host has no origin for its records".into()))?;
    drop(state);
    let invoker = tilo_actions::ActionInvoker::actor(format!("agent:{agent}"))
        .with_kind(tilo_actions::ActorKind::Agent)
        .acting_for(person);
    origin.apply(room, change, &invoker).map_err(unavailable)
}

/// A task run's reads and changes of records outside their apps: admitted by
/// the run, as every other step of it is (its delegation, its issue's and its
/// starter's live grants decide, only while it goes), then read and applied
/// as the run's agent acting for its starter. The agent door's connection
/// and a launched task agent's programs reach records through it alike.
pub(super) struct RunRecords(pub(super) super::TaskRunHost);

impl RecordChanges for RunRecords {
    fn apply(
        &self,
        caller: &Caller,
        change: &RecordChange,
    ) -> Result<serde_json::Value, ActionHostError> {
        let host = &self.0;
        host.admit_record_step(
            caller,
            (&change.app, &change.resource, &change.action),
            ActionKind::Undoable,
        )?;
        apply_record_change(
            host.state(),
            host.room(),
            change,
            (host.agent(), host.person()),
        )
    }
}

impl RecordReads for RunRecords {
    fn read(
        &self,
        caller: &Caller,
        address: &RecordAddress,
    ) -> Result<RecordCopy, ActionHostError> {
        let host = &self.0;
        let read = (address.app.clone(), address.record.clone());
        host.admit_record_step(
            caller,
            (&read.0, &read.1, RECORD_READ_ACTION),
            ActionKind::Read,
        )?;
        let checked = (host.clone(), caller.clone());
        let revalidate = Arc::new(move || {
            let (host, caller) = &checked;
            host.admit_record_step(
                caller,
                (&read.0, &read.1, RECORD_READ_ACTION),
                ActionKind::Read,
            )
            .map_err(|_| "record read no longer allowed".to_owned())
        });
        read_record(
            host.state(),
            (host.person(), host.room()),
            address,
            revalidate,
        )
    }
}

/// The action a record read is decided as, on the record's node.
const RECORD_READ_ACTION: &str = "read";

/// The person an agent task acts for, when `caller` is one: an agent the host admitted as a
/// member acting for its person, confined to a room.
fn task_person(caller: &Caller) -> Option<(&str, &str)> {
    if !matches!(caller.principal(), CallerPrincipal::Agent(_)) || caller.is_session_delegated() {
        return None;
    }
    Some((caller.acting_for_person()?, caller.room()?))
}

struct BrokerRecordReads {
    state: Weak<CapnpBrokerState>,
}

impl RecordReads for BrokerRecordReads {
    fn read(
        &self,
        caller: &Caller,
        address: &RecordAddress,
    ) -> Result<RecordCopy, ActionHostError> {
        let unavailable = |reason: String| ActionHostError::Unavailable { reason };
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| unavailable("the broker stopped".into()))?;
        let Some((person, room)) = task_person(caller) else {
            return Err(ActionHostError::Refused {
                reason: RECORD_READS_FOR_TASKS.into(),
            });
        };
        state.may_read_record(caller, room, address)?;
        let checked = (Arc::downgrade(&state), caller.clone(), address.clone());
        let revalidate = Arc::new(move || {
            let (state, caller, address) = &checked;
            let state = state.upgrade().ok_or("record read broker ended")?;
            let room = caller.room().ok_or("record read lost its room")?;
            state
                .may_read_record(caller, room, address)
                .map_err(|_| "record read no longer allowed".to_owned())
        });
        read_record(state, (person, room), address, revalidate)
    }
}

/// Read `address` in `room` as `person`'s device captures it, admitted
/// already; `revalidate` decides the read again before every chunk.
fn read_record(
    state: Arc<CapnpBrokerState>,
    (person, room): (&str, &str),
    address: &RecordAddress,
    revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<RecordCopy, ActionHostError> {
    let unavailable = |reason: String| ActionHostError::Unavailable { reason };
    {
        let (Some(owner), Some(people)) = (
            state.services.committed_documents.clone(),
            state.services.local_person.clone(),
        ) else {
            return Err(unavailable(
                tilo_actions::remote::RECORD_READS_UNAVAILABLE.into(),
            ));
        };
        let certificate = people.certificate(room, person).map_err(unavailable)?;
        drop(state);
        let context = NativeDocumentReadContext::for_task(
            CrdtRoute::record(address.app.clone(), room, address.record.clone()),
            certificate,
            revalidate,
        );
        context
            .revalidate()
            .map_err(|reason| ActionHostError::Refused { reason })?;
        if owner.record_read(room, &address.app, &address.record) == RecordRead::Own {
            // A record a sealed room holds nothing of may still arrive in its host's replay;
            // once that landed, one this device holds nothing of is one the room has none of.
            // This read runs on a thread of its own, which the wait holds.
            if let Some(history) = owner.await_sealed_history(&context) {
                futures::executor::block_on(history);
            }
            if owner.record_read(room, &address.app, &address.record) == RecordRead::Own {
                if !owner.seals_records(room) {
                    return Err(unavailable(
                        "this device holds none of the room's records outside their apps".into(),
                    ));
                }
                if owner.holds_record(room, &address.app, &address.record) {
                    return Err(unavailable(
                        "the writes this device holds of the record build on changes it does \
                         not hold"
                            .into(),
                    ));
                }
                return Ok(RecordCopy::Absent);
            }
        }
        match owner
            .capture(&context, None, MAX_DOCUMENT_BYTES)
            .map_err(|reason| ActionHostError::Refused { reason })?
        {
            NativeDocumentRead::Captured { body, .. } => {
                Ok(RecordCopy::Document(body.bytes().to_vec()))
            }
            NativeDocumentRead::Absent => Ok(RecordCopy::Absent),
            NativeDocumentRead::Pending => Err(unavailable(
                "the record is still arriving from the room".into(),
            )),
            NativeDocumentRead::Unchanged(_) | NativeDocumentRead::Changes { .. } => Err(
                unavailable("the room's owner answered a whole read with a part".into()),
            ),
        }
    }
}

#[cfg(test)]
#[path = "record_read_tests.rs"]
mod tests;
