//! The conversation's running summary, kept by the host while a turn is answered.
//!
//! A turn whose conversation outgrew what it is told as written carries a fold ([`Fold`], asked
//! for by [`chat::recall::told`]): the summary so far and the oldest messages outside it. The host
//! asks a model once for what those messages add and keeps the summary through Chat's own
//! `keep-summary`, as the agent on its turn. It does so once the answer is finished for its
//! person and the turn gave its place back ([`Owed`]): the person never waits for it, and what
//! they write meanwhile gets a turn of its own. One summary of a conversation is written at a
//! time, and a turn that began before one was kept asks for none of what that one holds.
//!
//! The question is one short answer, so it goes to the model the agent's person chose for such
//! work ([`Interaction::OneShot`]), with their own key and counted on the agent's day like every
//! call made for it. A person who chose none for it is asked through the model the turn itself
//! was worked with. Nothing here can fail a turn: no model, a refusal, a call that ran out of
//! time or a summary Chat did not take all leave the summary as it stood, and the next turn that
//! still sees the messages outside it asks again.

use std::time::Duration;

use runtime_collab::recall::Fold;
use serde_json::{Map, json};
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelRequest, Role, ToolChoice,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Resource as _;
use tilo_server_protocol::WorkspaceId;

use super::{CHAT, Reply, ServerAgents, Turn, invoke_named, later};
use crate::workspaces::person_model::{Interaction, ModelRefusal, Spender};

/// The chat action that keeps a conversation's summary.
const KEEP_SUMMARY: &str = "keep-summary";
/// How long the one question may take.
const ASK_TIME: Duration = Duration::from_secs(60);

/// The summary a turn that answered still owes its conversation. Its answer is finished for
/// its person already; this is kept once the turn gave its place back.
pub(super) struct Owed {
    workspace: String,
    workspace_id: WorkspaceId,
    turn: Turn,
    reply: Reply,
    day: String,
}

impl Owed {
    /// What `turn`, answered through `reply` on `day`, asks into its conversation's summary:
    /// none for a turn that asks for nothing.
    pub(super) fn of(
        (workspace, workspace_id): (&str, WorkspaceId),
        turn: Turn,
        reply: Reply,
        day: String,
    ) -> Option<Self> {
        turn.fold.is_some().then(|| Self {
            workspace: workspace.to_owned(),
            workspace_id,
            turn,
            reply,
            day,
        })
    }
}

impl ServerAgents {
    /// Take what a turn that answered asks into its conversation's summary: one question goes
    /// to a model and one `keep-summary` keeps what is kept of its answer. Nothing waits for
    /// it: the turn's answer is finished and its place given back before this is called.
    ///
    /// One summary of a conversation is written at a time: a turn that ends while one is
    /// being written asks for none. And a turn that began before a summary was kept still
    /// carries the messages that summary holds: it asks for none either.
    pub(super) async fn keep_summary(&self, owed: Owed) {
        let Owed {
            workspace,
            workspace_id,
            turn,
            reply,
            day,
        } = owed;
        let (workspace, turn, day) = (workspace.as_str(), &turn, day.as_str());
        let Some(fold) = turn.fold.as_ref() else {
            return;
        };
        let Some(_written) = self.going.summarise(workspace, &fold.conversation) else {
            return;
        };
        if !self.still_asked(workspace, turn, fold).await {
            return;
        }
        let Some((said, cost)) = self
            .ask_once((workspace, workspace_id), turn, fold, day)
            .await
        else {
            return;
        };
        if cost > 0 {
            self.charge(workspace, &turn.agent, day, cost).await;
        }
        let mut arguments = Map::new();
        arguments.insert("conversation".into(), json!(fold.conversation));
        arguments.insert("through".into(), json!(fold.through));
        arguments.insert("through_ts".into(), json!(fold.through_ts));
        arguments.insert("text".into(), json!(fold.kept(&said)));
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: workspace.to_owned(),
            document: String::new(),
        };
        match invoke_named(
            &reply.provider,
            &reply.caller,
            &route,
            chat::Chat::KIND,
            KEEP_SUMMARY,
            arguments,
        )
        .await
        {
            Ok(_) => tracing::info!(
                %workspace,
                conversation = %fold.conversation,
                through = %fold.through,
                rewritten = fold.condenses(),
                "kept a conversation's summary"
            ),
            Err(error) => tracing::warn!(
                %workspace,
                conversation = %fold.conversation,
                %error,
                "a conversation did not keep its summary"
            ),
        }
    }

    /// Whether `fold` is still what `turn`'s conversation asks its agent to take in, as the
    /// workspace gives the turn now. A summary kept since the turn began took those messages
    /// in, and the conversation asks for something else or for nothing. Where it cannot be
    /// read the summary stays as it stands.
    async fn still_asked(&self, workspace: &str, turn: &Turn, fold: &Fold) -> bool {
        let super::Ask::Message { part, message, .. } = &turn.ask else {
            return false;
        };
        let (Ok(Some(chat)), Ok(Some(held))) = (
            self.read::<chat::chat::Chat>(workspace, CHAT, "").await,
            self.read::<chat::parts::ChatMessages>(workspace, CHAT, part)
                .await,
        ) else {
            return false;
        };
        let now = chat::recall::told(
            &chat,
            &held,
            None,
            &tilo_resource::Id::from_raw(fold.conversation.clone()),
            &tilo_resource::Id::from_raw(message.clone()),
            &collaboration::permissions::MemberId::agent(&turn.agent.id.0),
            String::new(),
        );
        now.fold.as_ref() == Some(fold)
    }

    /// The one answer to what `fold` asks and what it cost, or none where no answer came.
    async fn ask_once(
        &self,
        (workspace, workspace_id): (&str, WorkspaceId),
        turn: &Turn,
        fold: &Fold,
        day: &str,
    ) -> Option<(String, u64)> {
        let (instructions, input) = fold.asks();
        let tokens = fold.output_tokens();
        let theirs = tilo_failure::ignored!(
            "an agent kept by nobody's person pays for no summary",
            tilo_action_policy::PersonId::parse(&turn.agent.acts_for).ok()
        )
        .and_then(|acts_for| acts_for.root())
        .map(|_| {
            self.workspaces
                .workspace_model(workspace_id, Interaction::OneShot)
                .spending_as(Spender {
                    workspace: workspace.to_owned(),
                    agent: super::sub_agents::payer(turn).id.0.clone(),
                    name: super::sub_agents::payer(turn).name.clone(),
                    day: day.to_owned(),
                    limit_micros: super::sub_agents::payer(turn).spend_limit_micros,
                })
        });
        if let Some(theirs) = theirs {
            let asked = (instructions.clone(), input.clone());
            // As a turn's own model is chosen: what the person linked decides alone, and only
            // one who chose nothing for a short answer is asked as the turn was.
            let said = tokio::task::spawn_blocking(move || match theirs.linked() {
                Ok(_) => Some(theirs.say_blocking(
                    (&asked.0, &asked.1),
                    tokens,
                    later(ASK_TIME),
                    &|| false,
                )),
                Err(ModelRefusal::NoModelLinked { .. }) => None,
                Err(refused) => Some(Err(refused)),
            })
            .await;
            match said {
                Ok(Some(Ok(answer))) => {
                    return Some((answer.text(), answer.response.cost_micros));
                }
                Ok(None) => {}
                Ok(Some(Err(refused))) => {
                    tracing::info!(
                        %workspace,
                        agent = %turn.agent.id.0,
                        why = %refused.words(None),
                        "a summary's question was refused: the summary stays as it is"
                    );
                    return None;
                }
                Err(error) => {
                    tracing::warn!(%workspace, %error, "a summary's question could not be asked");
                    return None;
                }
            }
        }
        let pays = super::sub_agents::payer(turn);
        if pays
            .spend_limit_micros
            .is_some_and(|limit| pays.spent_on(day) >= limit)
        {
            return None;
        }
        let model = self
            .model_for((workspace, workspace_id), turn, day)
            .await
            .ok()?;
        let key = format!("{}:{}:summary", turn.agent.id.0, turn.cause);
        let request = ModelRequest {
            messages: vec![
                Message::text(Role::System, instructions),
                Message::text(Role::User, input),
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            max_output_tokens: tokens,
        };
        let response = tokio::task::spawn_blocking(move || {
            let mut turns = model()?;
            turns.turn(&request, &key, later(ASK_TIME), false).ok()
        })
        .await
        .ok()
        .flatten()?;
        let said = response
            .message
            .content
            .iter()
            .filter_map(|content| match content {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some((said, response.cost_micros))
    }
}
