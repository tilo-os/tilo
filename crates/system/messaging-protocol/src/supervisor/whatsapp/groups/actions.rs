use super::{Arc, Client, GroupMetadata, Host, SendError};
use crate::whatsapp_store::groups::{Incoming, Operation, Review};
use tilo_messaging::{
    GroupActionFuture, GroupChange, GroupChangeRequest, GroupHistory, GroupHistoryFuture,
    GroupMessage, GroupReceipt, GroupSendRequest,
};
use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
use whatsapp_rust::wacore::types::{
    events::InboundMessage,
    message::{EditAttribute, MessageCategory},
};
fn now() -> Result<u64, SendError> {
    tilo_clock::try_unix_now().ok_or(SendError::Unavailable)
}
impl Host {
    pub(super) fn snapshot(
        &self,
        meta: &GroupMetadata,
        identity: &(String, Option<String>),
    ) -> Result<Review, SendError> {
        self.membership(meta.clone(), identity)?;
        let mut members: Vec<_> = meta
            .participants
            .iter()
            .map(|p| {
                (
                    p.jid.to_non_ad_string(),
                    if p.is_super_admin() {
                        "super-admin"
                    } else if p.is_admin() {
                        "admin"
                    } else {
                        "member"
                    }
                    .to_owned(),
                )
            })
            .collect();
        members.sort();
        if members
            .iter()
            .map(|(id, _)| id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != members.len()
        {
            return Err(SendError::Revoked);
        }
        Ok(Review {
            group: meta.id.to_non_ad_string(),
            identity: identity.clone(),
            title: meta.subject.clone(),
            members,
            announcement: meta.is_announcement,
            restricted: meta.is_parent_group
                || meta.parent_group_jid.is_some()
                || meta.is_default_sub_group
                || meta.is_general_chat
                || meta.is_hidden_group
                || meta.is_limit_sharing_enabled
                || meta.is_support_group
                || meta.is_suspended
                || meta.is_incognito
                || meta
                    .ephemeral
                    .as_ref()
                    .is_some_and(|e| e.expiration.unwrap_or(0) > 0),
            expires: now()?.saturating_add(90),
        })
    }
    async fn fresh(&self, group: &str) -> Result<Review, SendError> {
        let identity = self.check()?;
        if group.len() > 256
            || !group.strip_suffix("@g.us").is_some_and(|id| {
                id.split('-').count() <= 2
                    && id
                        .split('-')
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            return Err(SendError::InvalidRequest);
        }
        let jid = group.parse().map_err(|_| SendError::InvalidRequest)?;
        let meta = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.source.groups().get_metadata(&jid),
        )
        .await
        .map_err(|_| SendError::Unavailable)?
        .map_err(|_| SendError::Unavailable)?;
        if self.check()? != identity || meta.id.to_non_ad_string() != group {
            return Err(SendError::Revoked);
        }
        self.snapshot(&meta, &identity)
    }
    async fn reviewed(&self, group: &str, token: &str) -> Result<Review, SendError> {
        self.check()?;
        let old = self
            .backend
            .group_review(token)
            .await
            .map_err(|_| SendError::Unavailable)?
            .ok_or(SendError::Revoked)?;
        if old.expires <= now()? || old.group != group || old.identity != self.identity {
            return Err(SendError::Revoked);
        }
        let current = self.fresh(group).await?;
        if !old.same_roster(&current) || current.restricted {
            return Err(SendError::Revoked);
        }
        Ok(current)
    }
    fn is_self(&self, address: &str) -> bool {
        address == self.identity.0 || self.identity.1.as_deref() == Some(address)
    }
    fn admin(&self, review: &Review) -> bool {
        review
            .members
            .iter()
            .any(|(id, role)| self.is_self(id) && ["admin", "super-admin"].contains(&role.as_str()))
    }
    async fn reserve(&self, id: &str, review: &str, op: Operation) -> Result<(), SendError> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(SendError::InvalidRequest);
        }
        self.check()?;
        if !self
            .backend
            .reserve_group_operation(id, review, op)
            .await
            .map_err(|_| SendError::Unavailable)?
        {
            return Err(SendError::AlreadyReserved);
        }
        self.check().map_err(|_| SendError::Uncertain)?;
        Ok(())
    }
    async fn acknowledged(
        &self,
        group: String,
        operation: String,
    ) -> Result<GroupReceipt, SendError> {
        self.check().map_err(|_| SendError::Uncertain)?;
        self.backend
            .acknowledge_group_operation(&operation)
            .await
            .map_err(|_| SendError::Uncertain)?;
        self.check().map_err(|_| SendError::Uncertain)?;
        Ok(GroupReceipt {
            account: self.account.clone(),
            group,
            operation,
        })
    }
    pub(crate) async fn receive(
        &self,
        source: Arc<Client>,
        batch: &[InboundMessage],
    ) -> Result<(), SendError> {
        if !Arc::ptr_eq(&source, &self.source) || batch.is_empty() || batch.len() > 64 {
            return Err(SendError::Revoked);
        }
        let mut reviews = std::collections::BTreeMap::new();
        let mut rows = Vec::new();
        for event in batch.iter().filter(|event| event.info.source.is_group) {
            let group = event.info.source.chat.to_non_ad_string();
            if !reviews.contains_key(&group) {
                reviews.insert(group.clone(), self.fresh(&group).await?);
            }
            rows.push(incoming(
                reviews.get(&group).ok_or(SendError::Revoked)?,
                event,
            )?);
        }
        self.check()?;
        self.broker
            .as_ref()
            .ok_or(SendError::Unavailable)?
            .observe_mixed(source, batch.to_vec(), rows, Some(self.authority.clone()))
            .await?;
        self.check()?;
        Ok(())
    }
}
fn incoming(review: &Review, event: &InboundMessage) -> Result<Incoming, SendError> {
    let info = &event.info;
    let author = info.source.sender.to_non_ad_string();
    if review.restricted
        || !info.source.is_group
        || info.source.chat.to_non_ad_string() != review.group
        || !review.members.iter().any(|(id, _)| id == &author)
        || info.source.broadcast_list_owner.is_some()
        || (info.source.is_from_me
            && author != review.identity.0
            && review.identity.1.as_deref() != Some(&author))
    {
        return Err(SendError::Revoked);
    }
    if info.ephemeral_expiration.is_some_and(|e| e > 0)
        || info.edit != EditAttribute::Empty
        || info.category != MessageCategory::Empty
        || info.multicast
        || info.comment_target.is_some()
        || info.device_sent_meta.is_some()
        || info.unavailable_request_id.is_some()
    {
        return Err(SendError::InvalidRequest);
    }
    let text = crate::whatsapp_runtime::group_text_body(&event.message)
        .map_err(|_| SendError::InvalidRequest)?;
    let timestamp_ms = u64::try_from(info.timestamp.timestamp_millis())
        .ok()
        .filter(|t| *t > 0)
        .ok_or(SendError::InvalidRequest)?;
    if !crate::identifier(&info.id)
        || serde_json::to_string(&(&author, &info.id))
            .map_err(|_| SendError::InvalidRequest)?
            .len()
            > 512
    {
        return Err(SendError::InvalidRequest);
    }
    Ok(Incoming {
        group: review.group.clone(),
        author: if info.source.is_from_me {
            review.identity.0.clone()
        } else {
            author
        },
        id: info.id.clone(),
        text: text.to_string(),
        timestamp_ms,
        outgoing: info.source.is_from_me,
    })
}
pub(super) fn change(host: Host, request: GroupChangeRequest) -> GroupActionFuture {
    Box::pin(async move {
        let review = host.reviewed(&request.group, &request.review).await?;
        let (kind, target) = match &request.change {
            GroupChange::Add(route) => {
                if !host.admin(&review)
                    || route.service != host.account.service
                    || route.sender.as_str() != host.account.account
                    || !host
                        .contact
                        .as_ref()
                        .is_some_and(|client| client.supports(route))
                    || review.members.iter().any(|(id, _)| id == &route.recipient)
                {
                    return Err(SendError::Revoked);
                }
                ("add", route.recipient.clone())
            }
            GroupChange::Remove(member) => {
                if !host.admin(&review)
                    || host.is_self(member)
                    || !review.members.iter().any(|(id, _)| id == member)
                {
                    return Err(SendError::Revoked);
                }
                ("remove", member.clone())
            }
            GroupChange::Leave => ("leave", host.identity.0.clone()),
        };
        let group: whatsapp_rust::Jid = request
            .group
            .parse()
            .map_err(|_| SendError::InvalidRequest)?;
        let participant: whatsapp_rust::Jid =
            target.parse().map_err(|_| SendError::InvalidRequest)?;
        host.reserve(
            &request.operation,
            &request.review,
            Operation {
                group: request.group.clone(),
                author: host.identity.0.clone(),
                kind: kind.into(),
                target: target.clone(),
                text: String::new(),
                timestamp_ms: now()?.saturating_mul(1000),
                acknowledged: false,
            },
        )
        .await?;
        if let GroupChange::Add(route) = &request.change
            && !host
                .contact
                .as_ref()
                .is_some_and(|client| client.supports(route))
        {
            return Err(SendError::Uncertain);
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let groups = host.source.groups();
            let responses = match kind {
                "add" => groups.add_participants(group, &[participant]).await?,
                "remove" => groups.remove_participants(group, &[participant]).await?,
                _ => {
                    groups.leave(group).await?;
                    return Ok(true);
                }
            };
            Ok::<bool, whatsapp_rust::features::GroupError>(
                matches!(responses.as_slice(),[response] if response.jid.to_non_ad_string()==target && response.is_ok() && response.add_request.is_none() && response.status.as_deref().is_none_or(|s|s=="200")),
            )
        })
        .await;
        if !matches!(result, Ok(Ok(true))) {
            return Err(SendError::Uncertain);
        }
        host.acknowledged(request.group, request.operation).await
    })
}
pub(super) fn send(host: Host, request: GroupSendRequest) -> GroupActionFuture {
    Box::pin(async move {
        let review = host.reviewed(&request.group, &request.review).await?;
        if (review.announcement && !host.admin(&review))
            || request.text.trim().is_empty()
            || request.text.len() > crate::signal::MAX_TEXT_BYTES
        {
            return Err(SendError::Revoked);
        }
        let group: whatsapp_rust::Jid = request
            .group
            .parse()
            .map_err(|_| SendError::InvalidRequest)?;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            host.source
                .groups()
                .query_info_with_freshness(&group, whatsapp_rust::cache::Freshness::Refresh),
        )
        .await
        .map_err(|_| SendError::Unavailable)?
        .map_err(|_| SendError::Unavailable)?;
        let fresh = host.fresh(&request.group).await?;
        if !review.same_roster(&fresh) {
            return Err(SendError::Revoked);
        }
        host.reserve(
            &request.operation,
            &request.review,
            Operation {
                group: request.group.clone(),
                author: host.identity.0.clone(),
                kind: "message".into(),
                target: String::new(),
                text: request.text.to_string(),
                timestamp_ms: now()?.saturating_mul(1000),
                acknowledged: false,
            },
        )
        .await?;
        let message = whatsapp_rust::waproto::whatsapp::Message::text(request.text.as_str());
        let options = whatsapp_rust::send::SendOptions::default()
            .with_message_id(request.operation.clone())
            .with_group_metadata_freshness(whatsapp_rust::Freshness::Refresh);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            host.source
                .send_message_with_options(group, message, options),
        )
        .await;
        if !matches!(result,Ok(Ok(ref sent)) if sent.message_id==request.operation && sent.to.to_non_ad_string()==request.group)
        {
            return Err(SendError::Uncertain);
        }
        host.acknowledged(request.group, request.operation).await
    })
}
pub(super) fn history(host: Host, group: String) -> GroupHistoryFuture {
    Box::pin(async move {
        host.check()?;
        if host.fresh(&group).await?.restricted {
            return Err(SendError::Revoked);
        }
        let sent = host
            .backend
            .group_operations(&group)
            .await
            .map_err(|_| SendError::Unavailable)?;
        let incoming = host
            .broker
            .as_ref()
            .ok_or(SendError::Unavailable)?
            .group_history(group.clone(), host.authority.clone())
            .await?;
        host.check()?;
        let mut messages = std::collections::BTreeMap::new();
        for row in incoming {
            let id = serde_json::to_string(&(&row.author, &row.id))
                .map_err(|_| SendError::Unavailable)?;
            messages.insert(
                (row.author.clone(), row.id.clone()),
                GroupMessage {
                    author: row.author.clone(),
                    message: tilo_messaging::Message {
                        id,
                        text: zeroize::Zeroizing::new(row.text.clone()),
                        direction: if row.outgoing {
                            tilo_messaging::Direction::Outgoing
                        } else {
                            tilo_messaging::Direction::Incoming
                        },
                        timestamp_ms: row.timestamp_ms,
                        delivery: None,
                    },
                },
            );
        }
        for (id, op) in sent {
            if let Some(row) = messages.get_mut(&(op.author.clone(), id.clone())) {
                if row.author != op.author || row.message.text.as_str() != op.text {
                    return Err(SendError::Revoked);
                }
                row.message.delivery = Some(if op.acknowledged {
                    tilo_messaging::Delivery::Submitted
                } else {
                    tilo_messaging::Delivery::Uncertain
                });
            } else {
                let public_id = serde_json::to_string(&(&op.author, &id))
                    .map_err(|_| SendError::Unavailable)?;
                messages.insert(
                    (op.author.clone(), id.clone()),
                    GroupMessage {
                        author: op.author.clone(),
                        message: tilo_messaging::Message {
                            id: public_id,
                            text: zeroize::Zeroizing::new(op.text.clone()),
                            direction: tilo_messaging::Direction::Outgoing,
                            timestamp_ms: op.timestamp_ms,
                            delivery: Some(if op.acknowledged {
                                tilo_messaging::Delivery::Submitted
                            } else {
                                tilo_messaging::Delivery::Uncertain
                            }),
                        },
                    },
                );
            }
        }
        let mut messages: Vec<_> = messages.into_values().collect();
        messages.sort_by_key(|row| row.message.timestamp_ms);
        let mut bytes = 0usize;
        let mut page = Vec::new();
        for row in messages
            .into_iter()
            .rev()
            .take(tilo_messaging::MAX_HISTORY_MESSAGES)
        {
            bytes = bytes
                .saturating_add(row.message.text.len())
                .saturating_add(row.author.len())
                .saturating_add(row.message.id.len());
            if bytes > 240 * 1024 {
                break;
            }
            page.push(row);
        }
        page.reverse();
        Ok(GroupHistory {
            account: host.account,
            group,
            messages: page,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::wacore::{
        proto_helpers::MessageBuilderExt,
        types::message::{MessageInfo, MessageSource},
    };
    fn event() -> InboundMessage {
        let Ok(chat) = "123@g.us".parse() else {
            panic!("fixture group")
        };
        let Ok(sender) = "222@lid".parse() else {
            panic!("fixture member")
        };
        let Ok(timestamp) = "2026-10-11T00:00:00Z".parse() else {
            panic!("fixture timestamp")
        };
        InboundMessage::builder()
            .message(Arc::new(whatsapp_rust::waproto::whatsapp::Message::text(
                "private group text",
            )))
            .info(Arc::new(MessageInfo {
                source: MessageSource {
                    chat,
                    sender,
                    is_group: true,
                    ..Default::default()
                },
                id: "stanza-a".into(),
                timestamp,
                ..Default::default()
            }))
            .build()
    }
    #[test]
    fn production_group_codec_requires_current_membership_and_preserves_privacy_modes() {
        let review = Review {
            group: "123@g.us".into(),
            identity: ("123@s.whatsapp.net".into(), Some("456@lid".into())),
            title: "Group".into(),
            members: vec![
                ("456@lid".into(), "admin".into()),
                ("222@lid".into(), "member".into()),
            ],
            announcement: false,
            restricted: false,
            expires: 100,
        };
        let Ok(row) = incoming(&review, &event()) else {
            panic!("valid fixture text refused")
        };
        assert_eq!(row.author, "222@lid");
        assert_eq!(row.text, "private group text");
        let mut changed = review.clone();
        changed.members.retain(|(id, _)| id != "222@lid");
        assert!(incoming(&changed, &event()).is_err());
        changed = review.clone();
        changed.restricted = true;
        assert!(incoming(&changed, &event()).is_err());
        let mut expired = event();
        Arc::make_mut(&mut expired.info).ephemeral_expiration = Some(60);
        assert!(incoming(&review, &expired).is_err());
        let mut false_self = event();
        Arc::make_mut(&mut false_self.info).source.is_from_me = true;
        assert!(incoming(&review, &false_self).is_err());
        let mut foreign = event();
        let Ok(chat) = "789@g.us".parse() else {
            panic!("fixture group")
        };
        Arc::make_mut(&mut foreign.info).source.chat = chat;
        assert!(incoming(&review, &foreign).is_err());
        let mut wrapped = event();
        Arc::make_mut(&mut wrapped.message)
            .ephemeral_message
            .get_or_insert_default();
        assert!(wrapped.message.ephemeral_message.as_option().is_some());
        assert!(incoming(&review, &wrapped).is_err());
    }
}
