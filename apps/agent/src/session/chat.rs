//! Canonical package-primary Chat orchestration. Work is a threaded Chat
//! record; it is never a shell `TaskRun` or a grant of authority.
use std::collections::BTreeMap;
use ui::time::Duration;

use anyhow::Result;
use ui::collaboration::{CollabEvent, CollabRequest, DocumentMutation, PresencePayload, RouteSpec};
use ui::presence::PresenceKey;

use super::{ActiveRun, Session, arguments_for_step, is_chat_app_id};
use crate::apply::Editor;
use crate::model::Route;
use crate::plan::Instruction;
use crate::surface::{self, AppContract};
use ::chat::model::{
    AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
};

#[derive(Clone, Debug)]
pub(super) struct ChatRun {
    /// The message that summoned the agent: its work is recorded on the
    /// agent's reply to it.
    summons: String,
    pub(super) subject: Option<String>,
    work: AgentWork,
    pub(super) uncertain: bool,
    pub(super) target_artifact: Option<collab_types::ArtifactId>,
    pub(super) observed_mutation: bool,
}

impl Session {
    /// The artifact id is part of the authenticated envelope, unlike a
    /// schema discovered by inspecting untrusted document contents.
    pub(super) fn absorb_document_event(&mut self, event: &CollabEvent) -> Result<bool> {
        let observing = self.chat_run.is_some();
        let before = observing.then(|| self.document_heads());
        let changed = self.absorb_routed_document(event)?;
        if let CollabEvent::Document { envelope, .. } = event {
            let moved = before.is_some_and(|heads| self.agent_authored_since(&heads));
            if let Some(run) = &mut self.chat_run
                && run.target_artifact == Some(envelope.artifact)
                && moved
            {
                run.observed_mutation = true;
            }
        }
        Ok(changed)
    }

    fn document_heads(&mut self) -> BTreeMap<&'static str, Vec<automerge::ChangeHash>> {
        let mut heads = BTreeMap::new();
        if let Some(editor) = &mut self.editor {
            heads.insert(editor.contract().namespace, editor.heads());
        }
        heads.extend(
            self.peers
                .iter_mut()
                .map(|(namespace, editor)| (*namespace, editor.heads())),
        );
        heads
    }

    fn agent_authored_since(
        &mut self,
        before: &BTreeMap<&'static str, Vec<automerge::ChangeHash>>,
    ) -> bool {
        // What the owner applied for the agent's invocation, under the
        // broker's actor for it.
        let actor = format!("agent:{}", self.cfg.agent_id);
        self.peers.iter_mut().any(|(namespace, editor)| {
            before.get(namespace).is_some_and(|heads| {
                editor
                    .changes_since(heads)
                    .iter()
                    .any(|change| change.actor_id().to_bytes() == actor.as_bytes())
            })
        })
    }

    /// The app a document belongs to is read off its authenticated route.
    /// In Chat mode every document is a route-owned replica; in document
    /// mode, so is every app's except the page the agent was invited to.
    fn absorb_routed_document(&mut self, event: &CollabEvent) -> Result<bool> {
        let CollabEvent::Document {
            envelope,
            from_user_id,
            ..
        } = event
        else {
            return Ok(false);
        };
        let mutation = event
            .document_mutation()
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("document event lost its mutation"))?;
        let chat_mode = is_chat_app_id(&self.cfg.app_id);
        let routed = surface::for_artifact(&self.cfg.cluster.to_string(), envelope.artifact)
            .filter(|contract| chat_mode || contract.app_id() != self.cfg.app_id);
        if let Some(contract) = routed {
            let changed = self.absorb_route_owned(contract, mutation);
            if changed
                && contract.namespace == "chat"
                && let Some(peer) = self.peers.get("chat")
            {
                let chat = ::chat::projection::read(peer.doc());
                for channel in chat.channel_rows() {
                    for message in chat.message_rows(&channel.id) {
                        self.chat_sources
                            .entry(message.id)
                            .or_insert_with(|| (from_user_id.clone(), envelope.author));
                    }
                }
            }
            return Ok(changed);
        }
        // Chat mode holds route-owned replicas only. In particular, Writer
        // must never become `editor`, whose presence reply writes to the
        // session's primary (there, Chat) route.
        if chat_mode {
            return Ok(false);
        }
        match mutation {
            DocumentMutation::Snapshot(data) => self.absorb_snapshot(envelope.artifact, &data),
            DocumentMutation::Incremental(data) => {
                // An unrouted change never touches a route-owned replica.
                Ok(self
                    .editor
                    .as_mut()
                    .is_some_and(|editor| editor.merge_incremental(&data)))
            }
        }
    }

    /// Merge what `contract`'s package-primary route sent into the replica
    /// held for it, starting one from a snapshot of the app's own kind.
    fn absorb_route_owned(
        &mut self,
        contract: &'static AppContract,
        mutation: DocumentMutation,
    ) -> bool {
        let route_app_id = contract.app_id();
        match mutation {
            DocumentMutation::Snapshot(data) => match self.peers.get_mut(contract.namespace) {
                Some(peer) => peer.route_app_id() == route_app_id && peer.merge_full(&data),
                None => match Editor::peer_from_snapshot_on_route(
                    contract,
                    route_app_id,
                    &self.cfg.cluster.to_string(),
                    &data,
                    &self.cfg.agent_id,
                    self.cfg.permissions.clone(),
                ) {
                    Ok(peer) => {
                        self.peers.insert(contract.namespace, peer);
                        true
                    }
                    Err(error) => {
                        tracing::debug!(app = contract.namespace, %error, "ignored a snapshot");
                        false
                    }
                },
            },
            DocumentMutation::Incremental(data) => {
                self.peers.get_mut(contract.namespace).is_some_and(|peer| {
                    peer.route_app_id() == route_app_id && peer.merge_incremental(&data)
                })
            }
        }
    }

    /// Write to the conversation through Chat's own door: the agent's
    /// reply and the state of its work are Chat's verbs, permission-checked
    /// and attributed to the agent like any participant's.
    fn publish_chat(&mut self, invoke: &ui::tilo_resource::Invoke<::chat::Chat>) -> Result<()> {
        let editor = self
            .peers
            .get_mut("chat")
            .ok_or_else(|| anyhow::anyhow!("Chat state unavailable"))?;
        let route_app_id = editor.route_app_id().to_string();
        if let Some(data) = editor.invoke(invoke)? {
            self.link.send_to_route(
                RouteSpec::app(route_app_id.as_str(), self.cfg.cluster),
                &CollabRequest::document_incremental(data),
            )?;
        }
        Ok(())
    }

    fn publish_chat_work(&mut self, say: Option<&str>) -> Result<()> {
        let Some(run) = &self.chat_run else {
            return Ok(());
        };
        let invoke = ::chat::Chat::RECORD_WORK.with(::chat::record::chat::RecordWork {
            message: ui::tilo_resource::Id::from_raw(run.summons.clone()),
            work: run.work.to_update(),
            say: say.map(str::to_owned),
        });
        self.publish_chat(&invoke)
    }

    pub(super) fn chat_dispatched(&mut self, resource: &str, label: &str) -> Result<()> {
        let Some(run) = self.chat_run.as_mut() else {
            return Ok(());
        };
        run.uncertain = true;
        run.work.trace.push(AgentStep::authored(
            resource,
            label,
            AgentStepOutcome::Asked,
            run.work.trace.len() as u64,
        ));
        self.publish_chat_work(None)
    }

    fn chat_refused(&mut self, why: &str) -> Result<bool> {
        if let Some(run) = &mut self.chat_run {
            run.work.state = AgentWorkState::Refused;
            run.work.since = ui::time::unix_now_i64();
        }
        self.publish_chat_work(Some(why))?;
        self.active_run = None;
        self.chat_run = None;
        Ok(true)
    }

    fn chat_waiting(&mut self, namespace: &str) -> Result<bool> {
        if let Some(run) = &mut self.chat_run {
            run.work.state = AgentWorkState::Waiting {
                app: namespace.into(),
            };
        }
        self.publish_chat_work(None)?;
        if let Some(contract) = surface::CONTRACTS
            .iter()
            .find(|contract| contract.namespace == namespace)
            && let Some(route_app_id) = self
                .broker_surfaces
                .iter()
                .filter(|candidate| {
                    candidate.live
                        && candidate.cluster_id == self.cfg.cluster.to_string()
                        && candidate.record_id.is_empty()
                        && candidate.app_id == contract.app_id()
                })
                .map(|candidate| candidate.app_id.as_str())
                .next()
        {
            self.link.send_to_route(
                RouteSpec::app(route_app_id, self.cfg.cluster),
                &CollabRequest::Presence {
                    key: PresenceKey::Workspace {
                        id: route_app_id.into(),
                    },
                    payload: PresencePayload::Arrived,
                },
            )?;
        }
        self.active_run = None;
        self.chat_run = None;
        Ok(false)
    }

    /// Document frames and invocation receipts travel on independent streams.
    /// A terminal receipt may arrive first even when the owner sent its change
    /// before completing. Observe the real signed frame for a bounded interval.
    fn await_chat_mutation(&mut self) -> Result<()> {
        let deadline = ui::time::now() + Duration::from_millis(500);
        let mut deferred = Vec::new();
        while self
            .chat_run
            .as_ref()
            .is_some_and(|run| !run.observed_mutation)
            && ui::time::now() < deadline
        {
            match self
                .link
                .recv(deadline.saturating_duration_since(ui::time::now()))?
            {
                Some(event @ CollabEvent::Document { .. }) => {
                    let _ = self.absorb_document_event(&event)?;
                }
                Some(CollabEvent::ActionCatalogue { surfaces }) => {
                    self.set_broker_surfaces(surfaces);
                }
                Some(event) => deferred.push(event),
                None => {}
            }
        }
        self.link.defer(deferred);
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one serial summons owns its canonical reply and dispatch lifecycle through completion"
    )]
    pub(super) fn act_chat(&mut self) -> Result<bool> {
        // The document and catalogue use independent transport streams.
        // Decide only after the broker's first permission-filtered survey.
        if !self.chat_catalogue_received {
            return Ok(false);
        }
        let Some(editor) = self.peers.get("chat") else {
            return Ok(false);
        };
        let chat = ::chat::projection::read(editor.doc());
        let messages: Vec<_> = chat
            .channel_rows()
            .iter()
            .flat_map(|channel| chat.message_rows(&channel.id))
            .collect();
        let handled = crate::listen::answered(&messages, &self.cfg.agent_id);
        let Some(summons) = crate::listen::next_summons(
            &messages,
            &self.cfg.agent_id,
            &self.cfg.name,
            self.last_chat_action.as_ref(),
            ui::time::unix_now_i64(),
            &handled,
        ) else {
            return Ok(false);
        };
        // Recording the work makes the agent's reply to the summons the
        // first time, and goes on writing into it after.
        self.chat_run = Some(ChatRun {
            summons: summons.message.clone(),
            subject: summons.subject.resource().map(str::to_string),
            work: AgentWork::new(AgentWorkState::Taken, ui::time::unix_now_i64()),
            uncertain: false,
            target_artifact: None,
            observed_mutation: false,
        });
        self.publish_chat_work(None)?;
        // Wait only when an explicit carried subject names an absent owning
        // app. Waiting is not pause, and nothing has been dispatched.
        if let Some(resource) = summons.subject.resource() {
            let namespace = surface::namespace_of(summons.subject.kind(), resource);
            if !self
                .held()
                .iter()
                .any(|(contract, _)| contract.namespace == namespace)
            {
                return self.chat_waiting(namespace);
            }
        }
        let plan = self.decide(&summons.message, &summons.asked, &[]);
        let route = self.last_route.as_ref();
        if let Some(run) = &mut self.chat_run {
            run.work.reasoning = route.map(|route| AgentReasoning {
                route: if route.is_live() {
                    AgentRoute::Model
                } else {
                    AgentRoute::Floor
                },
                why: route.detail(),
            });
        }
        self.active_run = Some(ActiveRun {
            instruction: summons.asked.clone(),
            issued_by: Some(summons.author.clone()),
            decision: route.map_or_else(String::new, Route::detail),
            group_id: format!("chat:{}", summons.message),
            steps: vec![],
        });
        tracing::info!(message = %summons.message, authored_by = %summons.author, forwarding_frame = ?self.chat_sources.get(&summons.message),
            "took canonical Chat summons; authored identity and transport sender remain distinct");
        if plan.steps.is_empty()
            || plan
                .steps
                .iter()
                .any(|step| !matches!(step.instruction, Instruction::Perform { .. }))
        {
            return self.chat_refused("I cannot resolve this request to a permitted application action. Document-only recipes require a document instruction.");
        }
        // Preflight the whole plan: no partial work before a known refusal.
        let mut selected = Vec::new();
        for step in &plan.steps {
            let Instruction::Perform {
                id, confirmation, ..
            } = &step.instruction
            else {
                continue;
            };
            let offers = self.offers(&self.held());
            let Some(offer) = surface::find(&offers, id).cloned() else {
                return self.chat_refused("That action is no longer offered.");
            };
            // A document action, or an edit at the agent's own caret in the
            // owner's view: both change only the record, as the agent.
            if offer.descriptor.kind != tilo_actions::OperationKind::Document
                && !super::edits_only(&offer.descriptor)
            {
                return self.chat_refused("External effects and view operations are unavailable in this Chat runner until durable delegated admission is connected.");
            }
            let namespace = surface::for_descriptor(&offer.descriptor).map_or_else(
                || surface::app_of(&offer.descriptor.address.resource),
                |contract| contract.namespace,
            );
            if !self
                .held()
                .iter()
                .any(|(contract, _)| contract.namespace == namespace)
            {
                return self.chat_waiting(namespace);
            }
            if !confirmation.satisfies(&offer.descriptor.contract.confirmation) {
                return self.chat_refused("That action requires explicit confirmation.");
            }
            selected.push(offer);
        }
        self.dispatched_this_turn.clear();
        let mut previous = None;
        for (step, offer) in plan.steps.iter().zip(selected) {
            let Instruction::Perform {
                id,
                arguments,
                confirmation,
            } = &step.instruction
            else {
                continue;
            };
            if !self.dispatched_this_turn.insert(id.clone()) {
                continue;
            }
            let arguments = match arguments_for_step(step.carries, arguments, previous.as_ref()) {
                Ok(arguments) => arguments,
                Err(why) => return self.chat_refused(&why),
            };
            if let Some(run) = &mut self.chat_run {
                run.work.state = AgentWorkState::Doing {
                    did: offer.descriptor.action_label.clone(),
                    about: offer.descriptor.resource_label.clone(),
                };
                run.work.since = ui::time::unix_now_i64();
            }
            self.publish_chat_work(None)?;
            let trace_len = self.chat_run.as_ref().map_or(0, |run| run.work.trace.len());
            match self.perform(id, &arguments, confirmation)? {
                Ok(outcome) => {
                    self.await_chat_mutation()?;
                    if !self
                        .chat_run
                        .as_ref()
                        .is_some_and(|run| run.observed_mutation)
                    {
                        self.publish_chat_work(Some("The owner reported completion, but its resulting document change has not been observed. This request will not be repeated automatically."))?;
                        self.active_run = None;
                        self.chat_run = None;
                        return Ok(true);
                    }
                    if let Some(run) = &mut self.chat_run
                        && run.work.trace.len() > trace_len
                        && let Some(last) = run.work.trace.last_mut()
                    {
                        last.outcome = AgentStepOutcome::Applied;
                    }
                    self.publish_chat_work(None)?;
                    self.last_chat_action = Some(crate::listen::LastAction::new(
                        &offer.descriptor.address.resource,
                        ui::time::unix_now_i64(),
                    ));
                    previous = Some(outcome);
                }
                Err(why) => {
                    // After dispatch an observation error says nothing about
                    // whether an effect occurred. Preserve Asked, never retry.
                    let uncertain = self.chat_run.as_ref().is_some_and(|run| run.uncertain);
                    if uncertain {
                        self.publish_chat_work(Some(&format!("Outcome unconfirmed: {why}. This request will not be repeated automatically.")))?;
                        self.active_run = None;
                        self.chat_run = None;
                        return Ok(true);
                    }
                    if let Some(run) = &mut self.chat_run
                        && run.work.trace.len() > trace_len
                        && let Some(last) = run.work.trace.last_mut()
                    {
                        last.outcome = AgentStepOutcome::Refused { why: why.clone() };
                    }
                    return self.chat_refused(&why);
                }
            }
        }
        let did = self
            .chat_run
            .as_ref()
            .map(|run| {
                run.work
                    .trace
                    .iter()
                    .map(|step| step.label.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if let Some(run) = &mut self.chat_run {
            run.work.state = AgentWorkState::Done { did: did.clone() };
            run.work.since = ui::time::unix_now_i64();
        }
        self.publish_chat_work(Some(&format!("Completed: {did}.")))?;
        self.active_run = None;
        self.chat_run = None;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routed_document_lookup_matches_only_the_contract_app_id() {
        let cluster = "6a462c9b-9d6f-41f0-a0d4-3fe31b451102";
        let artifact = collab_types::document_artifact_id("so.tilo.photos", cluster, "");
        assert_eq!(
            surface::for_artifact(cluster, artifact).map(|contract| contract.namespace),
            Some("photos")
        );
        assert!(
            surface::for_artifact(
                cluster,
                collab_types::document_artifact_id("so.tilo.notes", cluster, "")
            )
            .is_none()
        );
        assert!(
            surface::for_artifact(
                cluster,
                collab_types::document_artifact_id("so.tilo.photos", cluster, "file:/a.png")
            )
            .is_none()
        );
    }

    #[test]
    fn the_reply_a_summons_gets_is_bounded_canonical_and_tuple_stable() {
        let agent_id = format!("Reader:/? #{}", "読り手🧪".repeat(80));
        let summons_id = "s".repeat(128);
        let id = |summons: &str, agent: &str| {
            ::chat::record::reply_id(&ui::tilo_resource::Id::from_raw(summons), agent).to_string()
        };
        let reply = id(&summons_id, &agent_id);
        assert_eq!(reply, id(&summons_id, &agent_id));
        assert_ne!(id("ab", "c"), id("a", "bc"));
        assert!(reply.len() <= 64);
        assert!(
            reply
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        );

        let mut document = automerge::AutoCommit::new();
        ::chat::demo::start(&mut document, "human-author", "Human", "HU").unwrap();
        let summons =
            ::chat::demo::say(&mut document, "human-author", "c_general", "@Reader help").unwrap();
        let work = AgentWork::new(AgentWorkState::Taken, 0);
        ::chat::demo::apply_as(
            &mut document,
            &agent_id,
            ::chat::Chat::RECORD_WORK.with(::chat::record::chat::RecordWork {
                message: ui::tilo_resource::Id::from_raw(summons.clone()),
                work: work.to_update(),
                say: None,
            }),
        )
        .unwrap();
        let reply = ::chat::projection::read(&document)
            .message_row(&id(&summons, &agent_id))
            .expect("recording the work makes the reply");
        assert_eq!(reply.author, agent_id);
        assert_eq!(reply.parent.as_deref(), Some(summons.as_str()));
    }
}
