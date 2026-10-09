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
use ::chat::doc::ChatDoc;
use ::chat::model::{
    AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
};

#[derive(Clone, Debug)]
pub(super) struct ChatRun {
    channel: String,
    reply: String,
    pub(super) subject: Option<String>,
    work: AgentWork,
    pub(super) uncertain: bool,
    pub(super) target_artifact: Option<collab_types::ArtifactId>,
    pub(super) observed_mutation: bool,
}

fn chat_reply_id(agent_id: &str, summons_id: &str) -> String {
    // This stable, length-delimited derivation mints a Chat message identifier.
    // The returned UUID payload is never used as a document artifact or route grant.
    let digest =
        collab_types::document_artifact_id("tilo-agent-chat-reply-v1", agent_id, summons_id);
    format!("agent-reply-{}", digest.0.simple())
}

fn conversation_context(
    messages: &[::chat::model::Message],
    summons: &crate::listen::Summons,
) -> Vec<crate::plan::Block> {
    const MAX_CONTEXT_MESSAGES: usize = 60;
    let prior = messages
        .iter()
        .filter(|message| message.channel == summons.channel)
        .take_while(|message| message.id != summons.message)
        .collect::<Vec<_>>();
    let keep_from = prior.len().saturating_sub(MAX_CONTEXT_MESSAGES);
    prior
        .into_iter()
        .skip(keep_from)
        .map(|message| crate::plan::Block {
            id: message.id.clone(),
            kind: "chat_message".into(),
            text: format!("{}: {}", message.author, message.text),
            attrs: BTreeMap::new(),
        })
        .collect()
}

fn textual_conversation_reply(answer: crate::tools::Answer) -> String {
    if !answer.actions.is_empty() {
        return crate::i18n::t("agent-message-actions-not-run");
    }
    let reply = answer
        .reply
        .filter(|reply| !reply.trim().is_empty())
        .unwrap_or_else(|| crate::i18n::t("agent-message-no-reply"));
    if reply.len() > ::chat::doc::MAX_MESSAGE_TEXT_BYTES {
        // The Chat action refuses oversized text. Record a truthful bounded
        // outcome instead of silently leaving this request unanswered, which
        // would ask the model again after every restart.
        return crate::i18n::t("agent-message-reply-too-long");
    }
    reply
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

    fn answer_chat_conversation(
        &mut self,
        summons: &crate::listen::Summons,
        messages: &[::chat::model::Message],
        reply_id: &str,
    ) -> Result<bool> {
        // Message has no operation surface. Even a malicious model action
        // proposal becomes text explaining that nothing ran.
        let context = conversation_context(messages, summons);
        let reply = match self.chooser.choose_conversation(&summons.asked, &context) {
            Ok(answer) => textual_conversation_reply(answer),
            Err(route) => {
                self.last_route = Some(route);
                crate::i18n::t("agent-message-model-unavailable")
            }
        };
        self.publish_chat(&::chat::actions::reply_agent_message_action(
            &summons.channel,
            &summons.message,
            reply_id,
            ui::time::unix_now_i64(),
            &reply,
        ))?;
        Ok(true)
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
                for channel in ChatDoc::list_channels(peer.doc()) {
                    for message in ChatDoc::list_messages(peer.doc(), &channel.id) {
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
            DocumentMutation::Snapshot(data) => {
                self.absorb_route_snapshot(contract, route_app_id, &data)
            }
            DocumentMutation::Incremental(data) => {
                self.peers.get_mut(contract.namespace).is_some_and(|peer| {
                    peer.route_app_id() == route_app_id && peer.merge_incremental(&data)
                })
            }
        }
    }

    fn absorb_route_snapshot(
        &mut self,
        contract: &'static AppContract,
        route_app_id: &str,
        data: &[u8],
    ) -> bool {
        if let Some(peer) = self.peers.get_mut(contract.namespace) {
            return peer.route_app_id() == route_app_id && peer.merge_full(data);
        }
        let chat_actor;
        let participant = if contract.namespace == "chat" {
            chat_actor = format!("agent:{}", self.cfg.agent_id);
            &chat_actor
        } else {
            &self.cfg.agent_id
        };
        match Editor::peer_from_snapshot_on_route(
            contract,
            route_app_id,
            data,
            participant,
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
        }
    }

    /// Write to the conversation through Chat's own door: the agent's
    /// reply and the state of its work are Chat actions, permission-checked
    /// and attributed to the agent like any participant's.
    fn publish_chat(&mut self, action: &::chat::actions::ChatAction) -> Result<()> {
        let editor = self
            .peers
            .get_mut("chat")
            .ok_or_else(|| anyhow::anyhow!("Chat state unavailable"))?;
        let route_app_id = editor.route_app_id().to_string();
        if let Some(data) = editor.apply::<::chat::Chat>(std::slice::from_ref(action))? {
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
        self.publish_chat(&::chat::actions::set_agent_work_action(
            &run.channel,
            &run.reply,
            run.work.clone(),
            say,
        ))
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
        let Some(editor) = self.peers.get("chat") else {
            return Ok(false);
        };
        let channels = ChatDoc::list_channels(editor.doc());
        let messages: Vec<_> = channels
            .iter()
            .flat_map(|channel| ChatDoc::list_messages(editor.doc(), &channel.id))
            .collect();
        let participant = format!("agent:{}", self.cfg.agent_id);
        let handled = crate::listen::answered(&messages, &participant);
        let Some(summons) = crate::listen::next_summons(
            &messages,
            &channels,
            &participant,
            &self.cfg.agent_id,
            &self.cfg.name,
            self.last_chat_action.as_ref(),
            ui::time::unix_now_i64(),
            &handled,
        ) else {
            return Ok(false);
        };
        let reply = crate::listen::record_of(&messages, &participant, &summons.message)
            .map_or_else(
                || chat_reply_id(&self.cfg.agent_id, &summons.message),
                |record| record.id.clone(),
            );
        if summons.conversation {
            return self.answer_chat_conversation(&summons, &messages, &reply);
        }
        // Catalogue and document arrive independently. Message needs no
        // action service; legacy action requests do.
        if !self.chat_catalogue_received {
            return Ok(false);
        }
        if crate::listen::record_of(&messages, &participant, &summons.message).is_none() {
            self.publish_chat(&::chat::actions::send_message_action(
                &summons.channel,
                &reply,
                ui::time::unix_now_i64(),
                "",
                Some(summons.message.clone()),
                None,
                vec![],
            ))?;
        }
        self.chat_run = Some(ChatRun {
            channel: summons.channel.clone(),
            reply,
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
            if offer.descriptor.kind != tilo_actions::OperationKind::Document {
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

    fn message(id: &str, author: &str, text: &str) -> ::chat::model::Message {
        ::chat::model::Message {
            id: id.into(),
            channel: "c_agent".into(),
            author: author.into(),
            ts: 1,
            text: text.into(),
            body: vec![],
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![],
            mentions: vec![],
            reference: None,
            source_provider: None,
            attachment: None,
            pinned: false,
            work: None,
            agent_intent: None,
        }
    }

    #[test]
    fn direct_message_context_preserves_causal_channel_order_and_is_bounded() {
        let mut messages = (0..65)
            .map(|index| message(&format!("m{index}"), "person", &format!("prior {index}")))
            .collect::<Vec<_>>();
        messages.push(message("request", "person", "question"));
        messages.push(message("later", "person", "must stay out"));
        messages.push({
            let mut other = message("other", "person", "other channel");
            other.channel = "c_other".into();
            other
        });
        let summons = crate::listen::Summons {
            message: "request".into(),
            channel: "c_agent".into(),
            author: "person".into(),
            ts: 1,
            asked: "question".into(),
            conversation: true,
            address: crate::listen::Address::Direct,
            subject: crate::listen::Subject::Nothing,
        };

        let context = conversation_context(&messages, &summons);

        assert_eq!(context.len(), 60);
        assert_eq!(context.first().map(|block| block.id.as_str()), Some("m5"));
        assert_eq!(context.last().map(|block| block.id.as_str()), Some("m64"));
        assert!(
            context
                .iter()
                .all(|block| block.id != "later" && block.id != "other")
        );
    }

    #[test]
    fn direct_message_never_executes_a_model_action_proposal() {
        let reply = textual_conversation_reply(crate::tools::Answer {
            actions: vec!["delete-everything".into()],
            reply: Some("Done".into()),
            ..Default::default()
        });

        assert!(reply.contains("did not run any actions"), "{reply}");
        assert!(!reply.contains("Done"), "{reply}");
    }

    #[test]
    fn oversized_model_reply_becomes_a_bounded_explicit_outcome() {
        let reply = textual_conversation_reply(crate::tools::Answer {
            reply: Some("x".repeat(::chat::doc::MAX_MESSAGE_TEXT_BYTES + 1)),
            ..Default::default()
        });
        assert_eq!(reply, crate::i18n::t("agent-message-reply-too-long"));
        assert!(reply.len() <= ::chat::doc::MAX_MESSAGE_TEXT_BYTES);
        assert!(!reply.trim().is_empty());
    }

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
    fn reply_id_is_bounded_canonical_and_tuple_stable() {
        let agent_id = format!("Reader:/? #{}", "読り手🧪".repeat(80));
        let summons_id = "s".repeat(128);
        let reply = chat_reply_id(&agent_id, &summons_id);

        assert_eq!(reply, chat_reply_id(&agent_id, &summons_id));
        assert_ne!(chat_reply_id("ab", "c"), chat_reply_id("a", "bc"));
        assert_eq!(reply.len(), 44);
        assert!(
            reply
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        );

        let mut document = automerge::AutoCommit::new();
        ChatDoc::seed_starter(&mut document, "human-author", "Human", "HU");
        assert!(ChatDoc::accepts_message_action(
            &document,
            "c_general",
            &reply,
            "",
            None,
            None,
        ));
    }
}
