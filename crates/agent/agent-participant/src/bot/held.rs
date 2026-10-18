//! The records a bot acts on where they are kept: on the host that governs
//! the room, which holds each record itself and needs no session.
//!
//! Each record is a confined replica opened as the bot for its installer.
//! The host brings a replica up to date before a run ([`HeldRecords::hold`])
//! and publishes what the run changed afterwards
//! ([`HeldRecords::take_changes`]) as the record's governor publishes any
//! change, so the changes keep the bot's entries. The parts of a chain an
//! app keeps beside its record (a chat's messages) are held in that
//! record's replica, reached through the chain the app publishes, and
//! their changes published on their own routes.

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
    /// The records held beside another (the parts of a chain its app
    /// keeps), each with the route of the replica that holds it.
    beside: BTreeMap<DocumentRoute, DocumentRoute>,
    /// Records that are parts of a chain whose app's record is not held
    /// yet: held with it once it is.
    waiting: BTreeMap<DocumentRoute, Vec<u8>>,
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
            beside: BTreeMap::new(),
            waiting: BTreeMap::new(),
            changed: Vec::new(),
        }
    }

    /// Who the replicas name as participant: `bot:<id>` for a bot.
    fn participant(&self) -> String {
        match &self.actor {
            ReplicaActor::Bot { installation, .. } => format!("bot:{installation}"),
            ReplicaActor::Owner | ReplicaActor::Agent | ReplicaActor::Member { .. } => {
                "host".to_owned()
            }
        }
    }

    /// Hold the record at `route` as `saved` (the record's whole document)
    /// has it: open it the first time, merge what changed since after. A
    /// part of a chain an app keeps beside its record (a chat's messages)
    /// is held with that record, in its replica, so the app's actions reach
    /// it through the chain as in a window; one whose app's record is not
    /// held yet is held with it once it is.
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
        if record.chain_part().is_some() {
            return self.hold_beside(route, &record, saved);
        }
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
        // The parts that waited for this record are held with it now.
        let waiting: Vec<DocumentRoute> = self
            .waiting
            .keys()
            .filter(|part| part.app_id == route.app_id && part.cluster_id == route.cluster_id)
            .cloned()
            .collect();
        for part in waiting {
            if let Some(saved) = self.waiting.remove(&part)
                && let Err(error) = self.hold(&part, saved)
            {
                tracing::warn!(record = %part.record, %error, "a part of a chain could not be held");
            }
        }
        Ok(())
    }

    /// Hold `record`, a part of a chain, in the replica of its app's record
    /// that keeps it beside its own.
    fn hold_beside(
        &mut self,
        route: &DocumentRoute,
        record: &ui::record::RecordId,
        saved: Vec<u8>,
    ) -> Result<(), String> {
        let mutation = DocumentMutation::Snapshot(saved);
        if let Some(holder) = self.beside.get(route)
            && let Some(replica) = self.replicas.get(holder)
        {
            return replica
                .apply_record(record, &mutation)
                .map_err(|error| error.to_string());
        }
        let mut refusals = Vec::new();
        for (holder, replica) in &self.replicas {
            if holder.app_id != route.app_id || holder.cluster_id != route.cluster_id {
                continue;
            }
            match replica.apply_record(record, &mutation) {
                Ok(()) => {
                    self.beside.insert(route.clone(), holder.clone());
                    return Ok(());
                }
                Err(error) => refusals.push(error.to_string()),
            }
        }
        if !refusals.is_empty() {
            return Err(format!(
                "no record of {} keeps {} beside it: {}",
                route.app_id,
                route.record,
                refusals.join("; ")
            ));
        }
        let DocumentMutation::Snapshot(saved) = mutation else {
            return Ok(());
        };
        self.waiting.insert(route.clone(), saved);
        Ok(())
    }

    /// The route of the replica that holds `route`'s record: its own, or
    /// the one it is held beside.
    fn holder<'a>(&'a self, route: &'a DocumentRoute) -> &'a DocumentRoute {
        self.beside.get(route).unwrap_or(route)
    }

    /// Drop the replica of `route` (or the one it is held beside, with
    /// every record held there) and anything it changed that was not
    /// taken: the next [`Self::hold`] opens it afresh from the record as
    /// the room keeps it. For a change the room refused, which must not
    /// stay in the bot's copy under its later changes.
    pub fn forget(&mut self, route: &DocumentRoute) {
        let holder = self.holder(route).clone();
        self.replicas.remove(&holder);
        let mut dropped = vec![holder.clone()];
        self.beside.retain(|part, held_by| {
            let kept = *held_by != holder;
            if !kept {
                dropped.push(part.clone());
            }
            kept
        });
        self.waiting.remove(route);
        self.changed
            .retain(|(changed, _)| !dropped.contains(changed));
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
        // A record held beside another is offered through it: the chain
        // its app publishes, as in a window.
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
        let commit = |error: crate::apps::AppError| DoorError::Unavailable {
            code: "commit".into(),
            reason: error.to_string(),
        };
        // Taken at once, so the replica's own reads (history, the outbox)
        // see the step before the next one runs.
        let taken = replica.take_mutation().map_err(commit)?;
        let beside = replica.take_record_mutations().map_err(commit)?;
        if let Some(mutation) = taken {
            self.changed.push((route.clone(), mutation));
        }
        // What the step changed of the records held beside it (a post in
        // a chat's part, a part it started) goes on their own routes.
        for (record, mutation) in beside {
            let part = DocumentRoute {
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                record: record.as_str().to_owned(),
            };
            self.beside.insert(part.clone(), route.clone());
            self.changed.push((part, mutation));
        }
        Ok(output)
    }

    fn find_in_history(
        &self,
        step: &RunStep,
        actor: &str,
    ) -> Option<(DocumentRoute, ActionContext)> {
        let recorded = |doc: &automerge::Automerge| {
            doc.get_changes(&[]).into_iter().rev().find_map(|change| {
                let context = ActionContext::from_change_message(change.message()?)?;
                let by = context
                    .invoker
                    .as_ref()
                    .map(|invoker| invoker.actor.as_str());
                (RunStep::of(&context).as_ref() == Some(step) && by == Some(actor))
                    .then_some(context)
            })
        };
        self.replicas.iter().find_map(|(route, replica)| {
            // A step on a record held beside this one is recorded there,
            // and answered for the route it was invoked on.
            replica
                .read_history(recorded)
                .or_else(|| {
                    replica
                        .records_beside()
                        .iter()
                        .find_map(|record| replica.read_record_history(record, recorded).flatten())
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
