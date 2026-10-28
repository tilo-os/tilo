//! The instance's path to its room, without the toolkit: what the runtime planned is committed
//! to the room record and handed to the room through the collab client (`collaboration`), and
//! what the room's other replicas committed comes back into the record and the instance.
//!
//! An instance queues each planned change as an invocation of the host's `commit` action
//! ([`Instance::take_invocation`]). [`RoomRecord::commit`] applies each to the room record as
//! the recorded action it is ([`DynAction`]), attributed to the person at the window, and sends
//! the change on the record's route. [`RoomRecord::receive`] takes in the changes the route
//! brings, checked against the route they came on, and brings the instance up to date with the
//! record.

use automerge::ChangeHash;
use collaboration::document_routes::{decode_routed_mutation, validated_route_payload_with};
use collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, DocumentMutation, RouteSpec,
};

use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::Doc;

use super::Instance;
use crate::publish::{COMMIT, HOST_RESOURCE};

/// Why a change did not reach the room.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    /// The instance has no host over a record yet.
    #[error("the instance holds no room record yet")]
    NoHost,
    /// No committed action is queued under the ticket.
    #[error("no committed action {0}")]
    NoAction(u64),
    /// The action could not be applied to the room record.
    #[error("the action was refused: {0}")]
    Refused(String),
    /// The collab client did not take the change.
    #[error("the room did not take the change")]
    Unsent,
}

/// The room record an instance commits to, and the route it travels on.
pub struct RoomRecord {
    doc: Doc,
    manager: CollaborationManager,
    route: RouteSpec,
    person: String,
    /// The heads of what the room was handed: a commit sends what follows them.
    sent: Vec<ChangeHash>,
}

impl RoomRecord {
    /// The record `doc`, shared on `route` through `manager`, changed by `person`.
    #[must_use]
    pub fn new(
        doc: Doc,
        manager: CollaborationManager,
        route: RouteSpec,
        person: impl Into<String>,
    ) -> Self {
        let mut doc = doc;
        let sent = doc.get_heads();
        Self {
            doc,
            manager,
            route,
            person: person.into(),
            sent,
        }
    }

    /// The record as this replica holds it.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The collab client the record travels through.
    #[must_use]
    pub fn manager(&self) -> &CollaborationManager {
        &self.manager
    }

    /// Commit every change `instance` queued for the room, in order, and send each on the
    /// record's route; the instance hears each answered. Returns the tickets committed.
    ///
    /// An invocation of anything but the host's `commit` is answered refused: this path runs
    /// only what the instance planned itself.
    ///
    /// # Errors
    /// The first change that could not be committed or sent; it is answered refused, and what
    /// is still queued stays queued.
    pub fn commit(&mut self, instance: &mut Instance) -> Result<Vec<u64>, RoomError> {
        let mut committed = Vec::new();
        while let Some((invocation, _origin)) = instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource != HOST_RESOURCE || address.action != COMMIT {
                instance.answered(
                    &address,
                    Some("the room path runs only the instance's own commits".to_owned()),
                );
                continue;
            }
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            match self.commit_ticket(instance, ticket) {
                Ok(()) => {
                    instance.answered(&address, None);
                    committed.push(ticket);
                }
                Err(error) => {
                    instance.answered(&address, Some(error.to_string()));
                    return Err(error);
                }
            }
        }
        instance.sync(Some(&self.doc));
        Ok(committed)
    }

    fn commit_ticket(&mut self, instance: &Instance, ticket: u64) -> Result<(), RoomError> {
        let host = instance.host().ok_or(RoomError::NoHost)?;
        let action = host
            .store()
            .committed(ticket)
            .ok_or(RoomError::NoAction(ticket))?;
        let encoded = action
            .encode()
            .map_err(|error| RoomError::Refused(error.to_string()))?;
        let context = ActionContext::for_resource(
            action.resources().first().copied().unwrap_or("room"),
            &action.kind,
            &action.kind,
        )
        .with_invoker(ActionInvoker::actor(self.person.clone()));
        tilo_actions::apply_local_with_context(
            &mut self.doc,
            &action,
            encoded,
            Some(self.person.as_bytes()),
            context,
        )
        .map_err(|error| RoomError::Refused(error.to_string()))?;
        self.doc.commit();
        let change = self.doc.save_after(&self.sent);
        if !self
            .manager
            .send(CollabRequest::document_incremental(change))
        {
            return Err(RoomError::Unsent);
        }
        self.sent = self.doc.get_heads();
        Ok(())
    }

    /// Take in what the record's route brought and bring `instance` up to date with the
    /// record. Whether the record changed.
    pub fn receive(&mut self, instance: &mut Instance) -> bool {
        let mut changed = false;
        for event in self.manager.poll() {
            let CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } = event
            else {
                continue;
            };
            let Some(payload) =
                validated_route_payload_with(&self.route, &envelope, decrypted_payload.as_deref())
            else {
                tracing::warn!("room: a change for another route was not taken in");
                continue;
            };
            let Ok(mutation) =
                decode_routed_mutation(&self.route, payload, decrypted_payload.as_deref())
            else {
                tracing::warn!("room: a change that does not decode was not taken in");
                continue;
            };
            let taken = match &mutation {
                DocumentMutation::Incremental(bytes) => self.doc.load_incoming(bytes),
                DocumentMutation::Snapshot(bytes) => self.doc.load_incoming_whole(bytes),
            };
            match taken {
                Ok(_) => changed = true,
                Err(error) => tracing::warn!(%error, "room: a change was not taken in"),
            }
        }
        if changed {
            // What arrived is the room's already: nothing of it is sent back.
            self.sent = self.doc.get_heads();
            instance.sync(Some(&self.doc));
        }
        changed
    }
}
