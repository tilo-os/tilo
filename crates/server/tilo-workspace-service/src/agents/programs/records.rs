//! The records a program acts on where they are kept: on the host that governs
//! the workspace, which keeps each record itself and needs no session.
//!
//! Each record is a confined replica opened as the program for its installer.
//! The host brings a replica up to date before a run ([`ProgramRecords::load`])
//! and publishes what the run changed afterwards
//! ([`ProgramRecords::take_changes`]) as the record's governor publishes any
//! change, so the changes keep the program's entries. The parts of a chain an
//! app keeps beside its record (a chat's messages) are loaded in that
//! record's replica, reached through the chain the app publishes, and
//! their changes published on their own routes.
//!
//! Each record loaded keeps a copy of its whole document, as the workspace
//! has it with what the program's runs changed since: what the program's record reads
//! answer ([`ApiMember::record`]), for any record the program follows,
//! whether or not it acts on it.

use std::collections::BTreeMap;

use collab_types::DocumentMutation;
use serde_json::{Map, Value};
use tilo_actions::api::{ActionAddress, ApiError};
use tilo_actions::remote::CatalogueRow;
use tilo_actions::{ActionContext, ActionInvoker, ActionOutput, Actor, MemberId};
use tilo_peer_state::document_frame::DocumentRoute;

use super::api::{ApiMember, AutomationStep};
use super::item_json::{OutboxRow, outbox_entries};
use tilo_agent_session::ConfinedReplica;
use tilo_agent_session::apps::{AppCatalogue, ReplicaActor, Seed};
use ui::permissions::policy;

/// One installation's replicas of the workspace's records.
pub struct ProgramRecords {
    catalogue: AppCatalogue,
    workspace: String,
    member: MemberId,
    actor: ReplicaActor,
    replicas: BTreeMap<DocumentRoute, ConfinedReplica>,
    /// The records loaded beside another (the parts of a chain its app
    /// keeps), each with the route of the replica that keeps it.
    beside: BTreeMap<DocumentRoute, DocumentRoute>,
    /// Records that are parts of a chain whose app's record is not loaded
    /// yet: loaded with it once it is.
    waiting: BTreeMap<DocumentRoute, Vec<u8>>,
    /// What runs changed, by record, not yet taken by the host.
    changed: Vec<(DocumentRoute, DocumentMutation)>,
    /// Each record's whole document, as read.
    copies: BTreeMap<DocumentRoute, Vec<u8>>,
}

impl ProgramRecords {
    /// No records yet, for `workspace`, kept by `member` acting as `actor`.
    #[must_use]
    pub fn new(
        catalogue: AppCatalogue,
        workspace: String,
        member: MemberId,
        actor: ReplicaActor,
    ) -> Self {
        Self {
            catalogue,
            workspace,
            member,
            actor,
            replicas: BTreeMap::new(),
            beside: BTreeMap::new(),
            waiting: BTreeMap::new(),
            changed: Vec::new(),
            copies: BTreeMap::new(),
        }
    }

    /// Keep `bytes` (a whole document or changes to it) in the copy of the
    /// record at `route`.
    fn keep(&mut self, route: &DocumentRoute, bytes: &[u8]) -> Result<(), String> {
        let Some(copy) = self.copies.get_mut(route) else {
            self.copies.insert(route.clone(), bytes.to_vec());
            return Ok(());
        };
        // The record as it was loaded before, unchanged since.
        if copy.as_slice() == bytes {
            return Ok(());
        }
        let mut doc = automerge::Automerge::load(copy).map_err(|error| error.to_string())?;
        doc.load_incremental(bytes)
            .map_err(|error| error.to_string())?;
        *copy = doc.save();
        Ok(())
    }

    /// Whether the program may read the record at `route` now: decided at the
    /// record's node as the workspace decides any reader's read of it.
    fn may_read(&self, route: &DocumentRoute) -> bool {
        use policy::{ActionKind, Answer, Context};
        let access = match &self.actor {
            ReplicaActor::Bot { access, .. } | ReplicaActor::Member { access, .. } => access,
            ReplicaActor::Owner => return true,
            ReplicaActor::Agent { .. } => return false,
        };
        let node = collab_types::document_node(&self.workspace, &route.app_id, &route.document);
        let now = tilo_clock::unix_now_ms();
        access.answer(&node, ActionKind::Read, &Context::at(now)) == Answer::Allow
    }

    /// Load the record at `route` as `saved` (the record's whole document)
    /// has it: open it the first time, merge what changed since after. A
    /// part of a chain an app keeps beside its record (a chat's messages)
    /// is loaded with that record, in its replica, so the app's actions reach
    /// it through the chain as in a window; one whose app's record is not
    /// loaded yet is loaded with it once it is.
    pub fn load(&mut self, route: &DocumentRoute, saved: Vec<u8>) -> Result<(), String> {
        self.keep(route, &saved)?;
        if let Some(replica) = self.replicas.get(route) {
            return replica
                .apply(&DocumentMutation::Snapshot(saved))
                .map_err(|error| error.to_string());
        }
        // A record every app of the workspace opens beside its own (the
        // workspace's emoji) is kept under its owner's routes but is no
        // record of the owner's kind: a program acts on the app's own records,
        // and reads what such a record says through the app's actions.
        if collab_types::workspace_document(&route.app_id, &route.document).is_some() {
            return Ok(());
        }
        let record = ui::resource::DocumentId::from_route(&route.document)
            .ok_or_else(|| format!("{} is not a record route", route.document))?;
        if record.chain_part().is_some() {
            return self.load_beside(route, &record, saved);
        }
        let replica = ConfinedReplica::open(
            &self.catalogue,
            &route.app_id,
            &self.workspace,
            &self.member,
            &self.actor,
            Seed::Existing {
                record,
                changes: vec![saved],
            },
            false,
        )
        .map_err(|error| error.to_string())?;
        self.replicas.insert(route.clone(), replica);
        // The parts that waited for this record are loaded with it now.
        let waiting: Vec<DocumentRoute> = self
            .waiting
            .keys()
            .filter(|part| part.app_id == route.app_id && part.cluster_id == route.cluster_id)
            .cloned()
            .collect();
        for part in waiting {
            if let Some(saved) = self.waiting.remove(&part)
                && let Err(error) = self.load(&part, saved)
            {
                tracing::warn!(record = %part.document, %error, "a part of a chain could not be loaded");
            }
        }
        Ok(())
    }

    /// Load `record`, a part of a chain, in the replica of its app's record
    /// that keeps it beside its own.
    fn load_beside(
        &mut self,
        route: &DocumentRoute,
        record: &ui::resource::DocumentId,
        saved: Vec<u8>,
    ) -> Result<(), String> {
        let mutation = DocumentMutation::Snapshot(saved);
        if let Some(owner) = self.beside.get(route)
            && let Some(replica) = self.replicas.get(owner)
        {
            return replica
                .apply_document(record, &mutation)
                .map_err(|error| error.to_string());
        }
        let mut refusals = Vec::new();
        for (owner, replica) in &self.replicas {
            if owner.app_id != route.app_id || owner.cluster_id != route.cluster_id {
                continue;
            }
            match replica.apply_document(record, &mutation) {
                Ok(()) => {
                    self.beside.insert(route.clone(), owner.clone());
                    return Ok(());
                }
                Err(error) => refusals.push(error.to_string()),
            }
        }
        if !refusals.is_empty() {
            return Err(format!(
                "no record of {} keeps {} beside it: {}",
                route.app_id,
                route.document,
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
    /// the one it is loaded beside.
    fn owner_of<'a>(&'a self, route: &'a DocumentRoute) -> &'a DocumentRoute {
        self.beside.get(route).unwrap_or(route)
    }

    /// Drop the replica of `route` (or the one it is loaded beside, with
    /// every record loaded there) and anything it changed that was not
    /// taken: the next [`Self::load`] opens it afresh from the record as
    /// the workspace keeps it. For a change the workspace refused, which must not
    /// stay in the program's copy under its later changes.
    pub fn forget(&mut self, route: &DocumentRoute) {
        let owner = self.owner_of(route).clone();
        self.replicas.remove(&owner);
        let mut dropped = vec![owner.clone()];
        self.beside.retain(|part, beside| {
            let kept = *beside != owner;
            if !kept {
                dropped.push(part.clone());
            }
            kept
        });
        self.waiting.remove(route);
        dropped.push(route.clone());
        self.changed
            .retain(|(changed, _)| !dropped.contains(changed));
        self.copies.retain(|copied, _| !dropped.contains(copied));
    }

    /// What runs changed since the last call, by record, in the order the
    /// steps made the changes.
    pub fn take_changes(&mut self) -> Vec<(DocumentRoute, DocumentMutation)> {
        std::mem::take(&mut self.changed)
    }
}

impl ApiMember for ProgramRecords {
    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn offered(&self) -> Vec<(DocumentRoute, Vec<CatalogueRow>)> {
        // A record loaded beside another is offered through it: the chain
        // its app publishes, as in a window.
        self.replicas
            .iter()
            .map(|(route, replica)| (route.clone(), replica.survey_rows()))
            .collect()
    }

    fn has_record(&self, route: &DocumentRoute) -> bool {
        self.replicas.contains_key(route)
    }

    fn record(&self, route: &DocumentRoute) -> Result<Option<Vec<u8>>, String> {
        if !self.may_read(route) {
            return Err(tilo_actions::remote::RESOURCE_READ_REFUSED.to_owned());
        }
        Ok(self.copies.get(route).cloned())
    }

    fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError> {
        let replica = self
            .replicas
            .get(route)
            .ok_or_else(|| ApiError::Unknown(address.clone()))?;
        let output = replica.invoke_within(
            Some(invoker),
            Some(context),
            invoker.appearance.as_ref(),
            address,
            arguments,
            confirmed,
        )?;
        let commit = |error: tilo_agent_session::apps::AppError| ApiError::Unavailable {
            code: "commit".into(),
            reason: error.to_string(),
        };
        // Taken at once, so the replica's own reads (history, the outbox)
        // see the step before the next one runs.
        let taken = replica.take_mutation().map_err(commit)?;
        let beside = replica.take_document_mutations().map_err(commit)?;
        let mut kept = Vec::new();
        if let Some(mutation) = taken {
            kept.push((route.clone(), mutation));
        }
        // What the step changed of the records loaded beside it (a post in
        // a chat's part, a part it started) goes on their own routes.
        for (record, mutation) in beside {
            let part = DocumentRoute {
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                document: record.route().to_owned(),
            };
            self.beside.insert(part.clone(), route.clone());
            kept.push((part, mutation));
        }
        for (changed, mutation) in kept {
            let (DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes)) =
                &mutation;
            self.keep(&changed, bytes)
                .map_err(|reason| ApiError::Unavailable {
                    code: "commit".into(),
                    reason,
                })?;
            self.changed.push((changed, mutation));
        }
        Ok(output)
    }

    fn find_in_history(
        &self,
        step: &AutomationStep,
        actor: &Actor,
    ) -> Option<(DocumentRoute, ActionContext)> {
        let recorded = |doc: &automerge::Automerge| {
            doc.get_changes(&[]).into_iter().rev().find_map(|change| {
                let context = ActionContext::from_change_message(change.message()?)?;
                let by = context.invoker.as_ref().map(|invoker| &invoker.acting.by);
                (AutomationStep::of(&context).as_ref() == Some(step) && by == Some(actor))
                    .then_some(context)
            })
        };
        self.replicas.iter().find_map(|(route, replica)| {
            // A step on a record loaded beside this one is recorded there,
            // and answered for the route it was invoked on.
            replica
                .read_history(recorded)
                .or_else(|| {
                    replica.resources_beside().iter().find_map(|record| {
                        replica.read_document_history(record, recorded).flatten()
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

    fn queued_in_history(
        &self,
        route: &DocumentRoute,
        step: &AutomationStep,
        actor: &Actor,
        field: &str,
    ) -> Vec<String> {
        self.replicas.get(route).map_or_else(Vec::new, |replica| {
            replica.read_history(|doc| super::api::queued_by_step(doc, step, actor, field))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_record_under_a_watched_apps_routes_is_left_alone() {
        let mut records = ProgramRecords::new(
            AppCatalogue::native(),
            "workspace-1".into(),
            MemberId::person("ada"),
            ReplicaActor::Owner,
        );
        let emoji = DocumentRoute {
            app_id: "so.tilo.chat".into(),
            cluster_id: "workspace-1".into(),
            document: collab_types::workspace_document_id("tilo.workspace.emoji"),
        };
        assert_eq!(records.load(&emoji, b"not chat's record".to_vec()), Ok(()));
        assert!(records.offered().is_empty(), "nothing is loaded for it");
    }

    /// A program reads the copy of each record it follows, as far as its grant
    /// and its installer's access reach.
    #[test]
    fn a_bot_reads_what_it_holds_inside_its_grant() {
        use policy::{Answer, Grant, Grantee, KindSet, Level, MemberAccess, Rule, TreePath};
        let workspace = "workspace-1";
        let bot = Grantee::bot("b1");
        let chat = Rule::new(
            TreePath::root()
                .any(Level::Workspace)
                .app("so.tilo.chat")
                .any(Level::Resource),
            KindSet::READ,
            Answer::Allow,
        );
        let grants = vec![Grant::new(
            "bot:b1:0",
            bot,
            chat.node,
            chat.kinds,
            chat.answer,
            Grantee::person("ada"),
        )];
        let access = MemberAccess::person(policy::WorkspacePolicy::new(workspace, "ada"), "ada")
            .acting_as(policy::Actor::Member(policy::MemberId::bot("b1")), grants);
        let mut records = ProgramRecords::new(
            AppCatalogue::native(),
            workspace.into(),
            MemberId::bot("b1"),
            ReplicaActor::Bot {
                installation: "b1".into(),
                installer: "ada".into(),
                access,
            },
        );
        let route = |app: &str| DocumentRoute {
            app_id: app.into(),
            cluster_id: workspace.into(),
            document: collab_types::workspace_document_id("tilo.workspace.emoji"),
        };
        records
            .load(&route("so.tilo.chat"), b"emoji".to_vec())
            .unwrap();
        assert_eq!(
            records.record(&route("so.tilo.chat")),
            Ok(Some(b"emoji".to_vec()))
        );
        assert_eq!(records.record(&route("so.tilo.notes")).ok(), None);
        records.forget(&route("so.tilo.chat"));
        assert_eq!(records.record(&route("so.tilo.chat")), Ok(None));
    }
}
