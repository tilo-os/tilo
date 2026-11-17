//! The conversation's running summary, kept by the host while a turn is answered.
//!
//! A turn whose conversation outgrew what it is told as written carries a fold ([`Fold`], asked
//! for by [`chat::recall::told`]): the summary so far and the oldest messages outside it. The host
//! asks a model once for what those messages add and keeps the summary through Chat's own
//! `keep-summary`, as the agent on its turn. It does so once the reply has its words, so the
//! person never waits for it.
//!
//! The question is one short answer, so it goes to the model the agent's keeper chose for such
//! work ([`Interaction::OneShot`]), with their own key and counted on the agent's day like every
//! call made for it. A keeper who chose none for it is asked through the model the turn itself
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

use super::{CHAT, HostAgents, Reply, Turn, invoke_named, later};
use crate::workspaces::person_model::{Interaction, ModelRefusal, Spender};

/// The chat action that keeps a conversation's summary.
const KEEP_SUMMARY: &str = "keep-summary";
/// How long the one question may take.
const ASK_TIME: Duration = Duration::from_secs(60);

impl HostAgents {
    /// Take what `turn` asks into its conversation's summary: `words`, the turn's answer, are
    /// given to the person first, then one question goes to a model and one `keep-summary` keeps
    /// what is kept of its answer. A turn that asks for nothing changes nothing.
    pub(super) async fn keep_summary(
        &self,
        (workspace, workspace_id): (&str, WorkspaceId),
        turn: &Turn,
        (reply, words): (&Reply, &str),
        day: &str,
    ) {
        let Some(fold) = turn.fold.as_ref() else {
            return;
        };
        reply.work(json!({"state": "doing"}), Some(words)).await;
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
            &reply.host,
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
        let theirs = tilo_action_policy::PersonId::parse(&turn.agent.keeper)
            .ok()
            .and_then(|keeper| keeper.root())
            .map(|keeper| {
                self.workspaces
                    .person_model(keeper, Interaction::OneShot)
                    .working_in(workspace_id)
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
            // As a turn's own model is chosen: what the keeper linked decides alone, and only
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
