//! What a check's answer cost, from the provider receipt Chat keeps on each reply.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use chat::parts::ChatMessages;
use tilo_actions::policy::MemberId;

use super::{Asked, Initiative, Kept, Look, LookEnded};
use crate::agents::CHAT;
use crate::workspaces::ReadFor;

impl Initiative {
    /// Chat keeps a reply and all retries in the same chain part as the original ask.
    /// Walk newest parts first, just until every retained look's ask has been found.
    pub(super) async fn recent_looks(
        &self,
        asked: &Asked<'_>,
        kept: &Kept,
    ) -> Result<Vec<Look>, String> {
        let mut looks: Vec<_> = kept
            .looks
            .iter()
            .rev()
            .map(|row| row.look.clone())
            .collect();
        let person = MemberId::person(hex::encode(asked.owner.person));
        let mut waiting = BTreeMap::new();
        for (index, look) in looks.iter_mut().enumerate() {
            // Older versions stored an estimate from a whole day's spending between checks.
            look.cost_micros = 0;
            if look.ended == LookEnded::Told {
                waiting.insert(chat::cause::message_id(&person, &look.id), index);
            }
        }
        if waiting.is_empty() {
            return Ok(looks);
        }
        let workspace = &asked.owner.workspace;
        let ws = &self.inner.workspaces;
        let reader = (&asked.owner.person, ReadFor::Caller(asked.caller));
        let mut parts: Vec<_> = ws
            .workspace_document_reads(workspace, reader)
            .await?
            .into_iter()
            .filter_map(|((app, document), allowed)| {
                let id = collab_types::document::DocumentId::parse(&document)?;
                let index = chat::parts::part_index(&id)?;
                (allowed && app == CHAT).then_some((Reverse(index), (app, document)))
            })
            .collect();
        parts.sort_unstable();
        let account = kept.spending_event(&asked.owner);
        let mut matched = Vec::new();
        for (_, route) in parts {
            let Some(bytes) = ws
                .resource_document(workspace, &route.0, &route.1)
                .await
                .map_err(|e| e.to_string())?
            else {
                continue;
            };
            let mut doc = automerge::AutoCommit::load(&bytes).map_err(|e| e.to_string())?;
            let part = crate::agents::opened::<ChatMessages>(&mut doc)?;
            let before = waiting.len();
            waiting.retain(|id, index| {
                let Some(message) = part.messages.get(id) else {
                    return true;
                };
                if let Some(agent) = respondent(message, &person, kept, &account) {
                    let n = u32::try_from(part.messages.len()).unwrap_or(u32::MAX);
                    // Each retry follows an existing reply in this part.
                    looks[*index].cost_micros = (0..=message.attempts.min(n))
                        .filter_map(|attempt| {
                            let reply = part
                                .messages
                                .get(&chat::chat::reply_id_at(id, &agent, attempt))?;
                            let usage = reply.work.as_ref()?.answered.as_ref()?;
                            (reply.author == agent && reply.channel == message.channel)
                                .then_some(usage.cost_micros)
                        })
                        .fold(0, u64::saturating_add);
                }
                false
            });
            if waiting.len() < before {
                matched.push(route);
            }
            if waiting.is_empty() {
                break;
            }
        }
        if !matched.is_empty() {
            let current = ws.workspace_document_reads(workspace, reader).await?;
            if matched
                .iter()
                .any(|route| current.get(route) != Some(&true))
            {
                return Err("access to Chat changed while reading its receipts".into());
            }
        }
        Ok(looks)
    }
}

/// The historical assistant in the authenticated event's original private conversation.
fn respondent(
    message: &chat::chat::Message,
    person: &MemberId,
    kept: &Kept,
    account: &str,
) -> Option<MemberId> {
    let cause = message.cause.as_ref()?;
    let program = message.author.agent_id()?;
    if cause.by_person() || &cause.person != person || !kept.authored_by(account, program) {
        return None;
    }
    let members = chat::Chat::direct_members(&message.channel)?;
    if members.len() != 2 || !members.contains(person) {
        return None;
    }
    members.into_iter().find(MemberId::is_agent)
}
