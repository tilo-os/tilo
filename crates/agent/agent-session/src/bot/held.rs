//! The records a bot acts on where they are kept: on the host that governs
//! the workspace, which holds each record itself and needs no session.
//!
//! Each record is a confined replica opened as the bot for its installer.
//! The host brings a replica up to date before a run ([`HeldDocuments::hold`])
//! and publishes what the run changed afterwards
//! ([`HeldDocuments::take_changes`]) as the record's governor publishes any
//! change, so the changes keep the bot's entries. The parts of a chain an
//! app keeps beside its record (a chat's messages) are held in that
//! record's replica, reached through the chain the app publishes, and
//! their changes published on their own routes.
//!
//! Each record held keeps a copy of its whole document, as the workspace
//! has it with what the bot's runs changed since: what the bot's record reads
//! answer ([`ApiMember::record`]), for any record the bot follows,
//! whether or not it acts on it.
//!
//! A tilolang app's record is held beside them ([`HeldDocuments::hold_tilolang`]): opened
//! through the app's package over the workspace's resource store, its resources published as
//! every tilolang resource is (`tl.<app>.<Type>`), so the bot's steps find, type, invoke and
//! read its actions on the same path as a native record's, and a read answers what the core read
//! answers of it. What a step changes of it reaches the workspace as the step runs.

use std::collections::BTreeMap;

use collab_types::DocumentMutation;
use serde_json::{Map, Value};
use tilo_actions::api::{ActionAddress, ApiError};
use tilo_actions::remote::{CatalogueRow, ResourceCopy};
use tilo_actions::{ActionContext, ActionInvoker, ActionOutput, Actor, MemberId};
use tilo_peer_state::document_frame::DocumentRoute;

use super::api::{ApiMember, AutomationStep};
use super::item_json::{OutboxRow, outbox_entries};
use crate::apps::{AppCatalogue, ReplicaActor, Seed};
use crate::worker::ConfinedReplica;
use ui::permissions::policy;

/// A tilolang app's record of the workspace as a bot holds it: read and changed through the
/// app's package on the calling thread, each change reaching the workspace as the bot makes it.
/// A step on it is not found again in history ([`ApiMember::find_in_history`]): what the store
/// keeps of a change names its invoker and run, not the step of a program it was.
pub trait TilolangRecord: Send {
    /// Every action its resources publish, each in the row of the resource holding it.
    ///
    /// # Errors
    /// The record does not open through its package.
    fn rows(&self) -> Result<Vec<CatalogueRow>, String>;
    /// Run the published action `address` with `arguments` as `invoker`, its change recorded
    /// with `context` (the run and step it is, what caused it).
    ///
    /// # Errors
    /// As any call of a published action: unknown, refused, invalid arguments.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
    ) -> Result<ActionOutput, ApiError>;
    /// What the core read answers of `resource` (a published id; empty for the workspace's
    /// documents of the app) for `reader`.
    ///
    /// # Errors
    /// The record does not open, or the read is refused.
    fn read(&self, resource: &str, reader: &Actor) -> Result<Value, String>;
}

/// One installation's replicas of the workspace's records.
pub struct HeldDocuments {
    catalogue: AppCatalogue,
    workspace: String,
    member: MemberId,
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
    /// Each record's whole document, as read.
    copies: BTreeMap<DocumentRoute, Vec<u8>>,
    /// The tilolang apps' records, by app.
    tilolang: BTreeMap<String, Box<dyn TilolangRecord>>,
}

impl HeldDocuments {
    /// No records yet, for `workspace`, held by `member` acting as `actor`.
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
            tilolang: BTreeMap::new(),
        }
    }

    /// Hold `record`, the record of the tilolang app `app`, in place of the one held before.
    pub fn hold_tilolang(&mut self, app: &str, record: Box<dyn TilolangRecord>) {
        self.tilolang.insert(app.to_owned(), record);
    }

    /// The route a tilolang app's record is offered and invoked on.
    fn tilolang_route(&self, app: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        }
    }

    /// Keep `bytes` (a whole document or changes to it) in the copy of the
    /// record at `route`.
    fn keep(&mut self, route: &DocumentRoute, bytes: &[u8]) -> Result<(), String> {
        let Some(copy) = self.copies.get_mut(route) else {
            self.copies.insert(route.clone(), bytes.to_vec());
            return Ok(());
        };
        // The record as it was held before, unchanged since.
        if copy.as_slice() == bytes {
            return Ok(());
        }
        let mut doc = automerge::Automerge::load(copy).map_err(|error| error.to_string())?;
        doc.load_incremental(bytes)
            .map_err(|error| error.to_string())?;
        *copy = doc.save();
        Ok(())
    }

    /// Whether the bot may read the record at `route` now: decided at the
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

    /// Hold the record at `route` as `saved` (the record's whole document)
    /// has it: open it the first time, merge what changed since after. A
    /// part of a chain an app keeps beside its record (a chat's messages)
    /// is held with that record, in its replica, so the app's actions reach
    /// it through the chain as in a window; one whose app's record is not
    /// held yet is held with it once it is.
    pub fn hold(&mut self, route: &DocumentRoute, saved: Vec<u8>) -> Result<(), String> {
        self.keep(route, &saved)?;
        if let Some(replica) = self.replicas.get(route) {
            return replica
                .apply(&DocumentMutation::Snapshot(saved))
                .map_err(|error| error.to_string());
        }
        // A record every app of the workspace opens beside its own (the
        // workspace's emoji) is kept under its owner's routes but is no
        // record of the owner's kind: a bot acts on the app's own records,
        // and reads what such a record says through the app's actions.
        if collab_types::workspace_document(&route.app_id, &route.document).is_some() {
            return Ok(());
        }
        let record = ui::resource::DocumentId::from_route(&route.document)
            .ok_or_else(|| format!("{} is not a record route", route.document))?;
        if record.chain_part().is_some() {
            return self.hold_beside(route, &record, saved);
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
                tracing::warn!(record = %part.document, %error, "a part of a chain could not be held");
            }
        }
        Ok(())
    }

    /// Hold `record`, a part of a chain, in the replica of its app's record
    /// that keeps it beside its own.
    fn hold_beside(
        &mut self,
        route: &DocumentRoute,
        record: &ui::resource::DocumentId,
        saved: Vec<u8>,
    ) -> Result<(), String> {
        let mutation = DocumentMutation::Snapshot(saved);
        if let Some(holder) = self.beside.get(route)
            && let Some(replica) = self.replicas.get(holder)
        {
            return replica
                .apply_document(record, &mutation)
                .map_err(|error| error.to_string());
        }
        let mut refusals = Vec::new();
        for (holder, replica) in &self.replicas {
            if holder.app_id != route.app_id || holder.cluster_id != route.cluster_id {
                continue;
            }
            match replica.apply_document(record, &mutation) {
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
    /// the one it is held beside.
    fn holder<'a>(&'a self, route: &'a DocumentRoute) -> &'a DocumentRoute {
        self.beside.get(route).unwrap_or(route)
    }

    /// Drop the replica of `route` (or the one it is held beside, with
    /// every record held there) and anything it changed that was not
    /// taken: the next [`Self::hold`] opens it afresh from the record as
    /// the workspace keeps it. For a change the workspace refused, which must not
    /// stay in the bot's copy under its later changes.
    pub fn forget(&mut self, route: &DocumentRoute) {
        self.tilolang.remove(&route.app_id);
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

impl ApiMember for HeldDocuments {
    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn offered(&self) -> Vec<(DocumentRoute, Vec<CatalogueRow>)> {
        // A record held beside another is offered through it: the chain
        // its app publishes, as in a window.
        let mut offered: Vec<_> = self
            .replicas
            .iter()
            .map(|(route, replica)| (route.clone(), replica.survey_rows()))
            .collect();
        for (app, record) in &self.tilolang {
            match record.rows() {
                Ok(rows) => offered.push((self.tilolang_route(app), rows)),
                Err(error) => {
                    tracing::warn!(%app, %error, "a tilolang record the bot holds offers nothing");
                }
            }
        }
        offered
    }

    fn holds(&self, route: &DocumentRoute) -> bool {
        self.replicas.contains_key(route)
            || (route.document.is_empty() && self.tilolang.contains_key(&route.app_id))
    }

    fn record(&self, route: &DocumentRoute) -> Result<ResourceCopy, String> {
        if let Some(record) = self.tilolang.get(&route.app_id) {
            if !self.may_read(&self.tilolang_route(&route.app_id)) {
                return Err(tilo_actions::remote::RESOURCE_READ_REFUSED.to_owned());
            }
            return record
                .read(&route.document, &Actor::Member(self.member.clone()))
                .map(ResourceCopy::Read);
        }
        if !self.may_read(route) {
            return Err(tilo_actions::remote::RESOURCE_READ_REFUSED.to_owned());
        }
        Ok(self
            .copies
            .get(route)
            .cloned()
            .map_or(ResourceCopy::Absent, ResourceCopy::Document))
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
        if route.document.is_empty()
            && let Some(record) = self.tilolang.get_mut(&route.app_id)
        {
            return record.invoke(address, arguments, invoker, context);
        }
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
        let commit = |error: crate::apps::AppError| ApiError::Unavailable {
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
        // What the step changed of the records held beside it (a post in
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
            // A step on a record held beside this one is recorded there,
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
        let mut held = HeldDocuments::new(
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
        assert_eq!(held.hold(&emoji, b"not chat's record".to_vec()), Ok(()));
        assert!(held.offered().is_empty(), "nothing is held for it");
    }

    /// A bot reads the copy of each record it follows, as far as its grant
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
        let mut held = HeldDocuments::new(
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
        held.hold(&route("so.tilo.chat"), b"emoji".to_vec())
            .unwrap();
        assert_eq!(
            held.record(&route("so.tilo.chat")),
            Ok(ResourceCopy::Document(b"emoji".to_vec()))
        );
        assert_eq!(held.record(&route("so.tilo.notes")).ok(), None);
        held.forget(&route("so.tilo.chat"));
        assert_eq!(
            held.record(&route("so.tilo.chat")),
            Ok(ResourceCopy::Absent)
        );
    }
}
