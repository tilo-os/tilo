//! The records a bot acts on where they are kept: on the host that governs
//! the room, which holds each record itself and needs no session.
//!
//! Each record is a confined replica opened as the bot for its installer.
//! The host brings a replica up to date before a run ([`HeldRecords::hold`])
//! and publishes what the run changed afterwards
//! ([`HeldRecords::take_changes`]) as the record's governor publishes any
//! change, so the changes keep the bot's entries.

use std::collections::BTreeMap;

use collab_types::DocumentMutation;
use serde_json::{Map, Value};
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_actions::{ActionContext, ActionInvoker, ActionOutput};
use tilo_browser_peer::DocumentRoute;

use super::door::{DoorParticipant, RunStep};
use super::item_json::{OutboxRow, outbox_entries};
use crate::apps::{AppCatalogue, ReplicaActor, Seed};
use crate::worker::ConfinedReplica;

/// One installation's replicas of the room's records.
pub struct HeldRecords {
    catalogue: AppCatalogue,
    room: String,
    actor: ReplicaActor,
    replicas: BTreeMap<DocumentRoute, ConfinedReplica>,
    /// What runs changed, by record, not yet taken by the host.
    changed: Vec<(DocumentRoute, DocumentMutation)>,
}

impl HeldRecords {
    /// No records yet, for `room`, acting as `actor`.
    #[must_use]
    pub fn new(catalogue: AppCatalogue, room: String, actor: ReplicaActor) -> Self {
        Self {
            catalogue,
            room,
            actor,
            replicas: BTreeMap::new(),
            changed: Vec::new(),
        }
    }

    /// Who the replicas name as participant: `bot:<id>` for a bot.
    fn participant(&self) -> String {
        match &self.actor {
            ReplicaActor::Bot { installation, .. } => format!("bot:{installation}"),
            ReplicaActor::Owner | ReplicaActor::Agent => "host".to_owned(),
        }
    }

    /// Hold the record at `route` as `saved` (the record's whole document)
    /// has it: open it the first time, merge what changed since after.
    ///
    /// # Errors
    /// The record's app is not on this host, its id is not a record id, or
    /// the replica refused the document.
    pub fn hold(&mut self, route: &DocumentRoute, saved: Vec<u8>) -> Result<(), String> {
        if let Some(replica) = self.replicas.get(route) {
            return replica
                .apply(&DocumentMutation::Snapshot(saved))
                .map_err(|error| error.to_string());
        }
        let record = if route.record.is_empty() {
            ui::record::RecordId::package_primary()
        } else {
            ui::record::RecordId::parse(&route.record)
                .ok_or_else(|| format!("{} is not a record id", route.record))?
        };
        let replica = ConfinedReplica::open(
            &self.catalogue,
            &route.app_id,
            &self.room,
            &self.participant(),
            &self.actor,
            Seed::Existing {
                record,
                changes: vec![saved],
            },
        )
        .map_err(|error| error.to_string())?;
        self.replicas.insert(route.clone(), replica);
        Ok(())
    }

    /// Drop the replica of `route` and anything it changed that was not
    /// taken: the next [`Self::hold`] opens it afresh from the record as
    /// the room keeps it. For a change the room refused, which must not
    /// stay in the bot's copy under its later changes.
    pub fn forget(&mut self, route: &DocumentRoute) {
        self.replicas.remove(route);
        self.changed.retain(|(changed, _)| changed != route);
    }

    /// What runs changed since the last call, by record, in the order the
    /// steps made the changes.
    pub fn take_changes(&mut self) -> Vec<(DocumentRoute, DocumentMutation)> {
        std::mem::take(&mut self.changed)
    }
}

impl DoorParticipant for HeldRecords {
    fn room(&self) -> &str {
        &self.room
    }

    fn offered(&self) -> Vec<(DocumentRoute, Vec<ActionDescriptor>)> {
        self.replicas
            .iter()
            .map(|(route, replica)| (route.clone(), replica.survey()))
            .collect()
    }

    fn holds(&self, route: &DocumentRoute) -> bool {
        self.replicas.contains_key(route)
    }

    fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError> {
        let replica = self
            .replicas
            .get(route)
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        let output = replica.invoke_within(
            Some(invoker),
            Some(context),
            invoker.appearance.as_ref(),
            address,
            arguments,
            confirmed,
        )?;
        // Taken at once, so the replica's own reads (history, the outbox)
        // see the step before the next one runs.
        let taken = replica
            .take_mutation()
            .map_err(|error| DoorError::Unavailable {
                code: "commit".into(),
                reason: error.to_string(),
            })?;
        if let Some(mutation) = taken {
            self.changed.push((route.clone(), mutation));
        }
        Ok(output)
    }

    fn find_in_history(
        &self,
        step: &RunStep,
        actor: &str,
    ) -> Option<(DocumentRoute, ActionContext)> {
        self.replicas.iter().find_map(|(route, replica)| {
            replica
                .read_history(|doc| {
                    doc.get_changes(&[]).into_iter().rev().find_map(|change| {
                        let context = ActionContext::from_change_message(change.message()?)?;
                        let by = context
                            .invoker
                            .as_ref()
                            .map(|invoker| invoker.actor.as_str());
                        (RunStep::of(&context).as_ref() == Some(step) && by == Some(actor))
                            .then_some(context)
                    })
                })
                .map(|context| (route.clone(), context))
        })
    }

    fn outbox(&self, route: &DocumentRoute, field: &str) -> Vec<OutboxRow> {
        self.replicas.get(route).map_or_else(Vec::new, |replica| {
            replica.read_history(|doc| outbox_entries(doc, &doc.get_heads(), field))
        })
    }
}
