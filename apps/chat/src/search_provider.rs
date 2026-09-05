//! Chat global search backed exclusively by canonical resource actions.

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{Channel, ChannelKind, Message, User, Workspace};

pub const CHANNEL_KIND: &str = "Conversations";
pub const MESSAGE_KIND: &str = "Messages";
pub const PERSON_KIND: &str = "People";

pub struct ChatSearchProvider {
    channels: Vec<Channel>,
    workspaces: Vec<Workspace>,
    messages: Vec<Message>,
    users: Vec<User>,
}

impl ChatSearchProvider {
    pub fn new(
        channels: &[Channel],
        workspaces: &[Workspace],
        messages: &[Message],
        users: &[User],
    ) -> Self {
        Self {
            channels: channels
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
            workspaces: workspaces
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
            messages: messages
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
            users: users
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
        }
    }

    fn channel_address(id: &str) -> ActionAddress {
        ActionAddress::new(format!("chat.channel:{id}"), "open")
    }
    fn message_address(id: &str) -> ActionAddress {
        ActionAddress::new(format!("chat.message:{id}"), "open")
    }
    fn person_address(id: &str) -> ActionAddress {
        ActionAddress::new(format!("chat.person:{id}"), "open")
    }

    fn workspace_name(&self, id: &str) -> &str {
        self.workspaces
            .iter()
            .find(|ws| ws.id == id)
            .map_or("Workspace", |ws| ws.name.as_str())
    }
    fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|channel| channel.id == id)
    }
    fn user_name(&self, id: &str) -> String {
        self.users
            .iter()
            .find(|user| user.id == id)
            .map_or_else(|| id.to_string(), |user| user.name.clone())
    }
}

impl SearchProvider for ChatSearchProvider {
    fn name(&self) -> &str {
        "Chat"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        let query = query.trim().to_lowercase();
        let mut results = Vec::with_capacity(limit.min(32));
        for channel in &self.channels {
            if channel.kind == ChannelKind::Text
                && !channel.archived
                && (channel.name.to_lowercase().contains(&query)
                    || channel.topic.to_lowercase().contains(&query))
            {
                let workspace = self.workspace_name(&channel.workspace);
                let subtitle = if channel.topic.trim().is_empty() {
                    workspace.to_string()
                } else {
                    format!("{workspace} · {}", channel.topic)
                };
                results.push(
                    SearchResult::for_action(
                        CHANNEL_KIND,
                        format!("#{}", channel.name),
                        Self::channel_address(&channel.id),
                    )
                    .with_subtitle(subtitle)
                    .with_icon(Icon::Mail),
                );
                if results.len() == limit {
                    return results;
                }
            }
        }

        // Keep an empty query useful as a compact conversation browser; the
        // much larger full-text and directory sets join only for a real query.
        if query.is_empty() {
            return results;
        }

        for message in &self.messages {
            let author = self.user_name(&message.author);
            let Some(channel) = self.channel(&message.channel) else {
                continue;
            };
            let attachment = message
                .attachment
                .as_ref()
                .map(|attachment| match attachment {
                    crate::model::Attachment::File { name, .. }
                    | crate::model::Attachment::Image { name, .. }
                    | crate::model::Attachment::Video { name, .. } => name.as_str(),
                });
            let searchable = format!("{author} {} {}", message.text, attachment.unwrap_or(""));
            if !searchable.to_lowercase().contains(&query) {
                continue;
            }
            let title = if message.text.trim().is_empty() {
                attachment.unwrap_or("Attachment").to_string()
            } else {
                message.text.trim().chars().take(96).collect()
            };
            results.push(
                SearchResult::for_action(MESSAGE_KIND, title, Self::message_address(&message.id))
                    .with_subtitle(format!(
                        "{author} · #{} · {}",
                        channel.name,
                        self.workspace_name(&channel.workspace)
                    ))
                    .with_icon(Icon::Mail),
            );
            if results.len() == limit {
                return results;
            }
        }
        for user in &self.users {
            if format!("{} {}", user.name, user.id)
                .to_lowercase()
                .contains(&query)
            {
                results.push(
                    SearchResult::for_action(
                        PERSON_KIND,
                        user.name.clone(),
                        Self::person_address(&user.id),
                    )
                    .with_subtitle(if user.is_agent {
                        "Agent"
                    } else {
                        "Person"
                    }),
                );
                if results.len() == limit {
                    break;
                }
            }
        }
        results
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<qos_actions::Resource> {
        let message_id = address.resource.strip_prefix("chat.message:")?;
        let message = self
            .messages
            .iter()
            .find(|message| message.id == message_id)?;
        let channel = self.channel(&message.channel)?;
        let workspace = self
            .workspaces
            .iter()
            .find(|ws| ws.id == channel.workspace)?;
        Some(
            qos_actions::Resource::new(
                address.resource.clone(),
                "chat.message",
                message.text.clone(),
            )
            .with_action(
                qos_actions::ActionSpec::operation(
                    "open",
                    "Reveal message",
                    qos_actions::OperationKind::View,
                    crate::actions::CHAT_OPERATION_TYPE,
                    &crate::actions::ChatOperation::RevealMessage {
                        workspace: workspace.id.clone(),
                        channel: channel.id.clone(),
                        message: message.id.clone(),
                        parent: message.parent.clone(),
                    },
                )
                .ok()?
                .retry_policy(qos_actions::RetryPolicy::Idempotent),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::widget::Widget;

    fn seeded() -> (crate::app::ChatApp, AutoCommit) {
        let mut app = crate::app::ChatApp::new_ephemeral();
        let mut doc = AutoCommit::new();
        crate::doc::ChatDoc::seed_if_empty(&mut doc);
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    #[test]
    fn every_kind_uses_a_canonical_address() {
        let (app, _) = seeded();
        let providers = app.search_providers();
        let provider = &providers[0];
        assert!(
            provider
                .search("")
                .iter()
                .all(|hit| hit.action_address().is_some())
        );
        let message = app
            .search_messages
            .iter()
            .find(|message| !message.text.is_empty())
            .unwrap();
        assert!(
            provider
                .search(&message.text)
                .iter()
                .any(|hit| hit.kind == MESSAGE_KIND && hit.action_address().is_some())
        );
        let person = app.search_users.first().unwrap();
        assert!(
            provider
                .search(&person.name)
                .iter()
                .any(|hit| hit.kind == PERSON_KIND && hit.action_address().is_some())
        );
    }

    #[test]
    fn limited_search_honours_the_materialisation_budget() {
        let (app, _) = seeded();
        let providers = app.search_providers();
        let provider = &providers[0];
        assert_eq!(provider.search_limited("", 1).len(), 1);
        assert!(provider.search_limited("", 0).is_empty());
    }

    #[test]
    fn search_addresses_resolve_and_navigate_exactly() {
        let (mut app, doc) = seeded();
        let resources = app.resources(&doc);
        let providers = app.search_providers();
        let provider = &providers[0];
        let channel_hit = provider
            .search("")
            .into_iter()
            .find(|hit| hit.title == "#frontend")
            .expect("frontend");
        let address = channel_hit.action_address().unwrap();
        let encoded =
            qos_actions::door::encode(&resources, &address, None).expect("exported channel action");
        app.invoke_resource_operation(
            qos_actions::OperationKind::View,
            &encoded.type_id,
            &encoded.payload,
            &doc,
            &ROOT,
        )
        .unwrap();
        assert_eq!(
            app.active_channel,
            address.resource.trim_start_matches("chat.channel:")
        );

        let message = app
            .search_messages
            .iter()
            .find(|message| !message.text.is_empty())
            .cloned()
            .unwrap();
        let message_hit = provider
            .search(&message.text)
            .into_iter()
            .find(|hit| {
                hit.action_address()
                    .is_some_and(|a| a.resource == format!("chat.message:{}", message.id))
            })
            .unwrap();
        let message_address = message_hit.action_address().unwrap();
        let message_resource = provider
            .resolve_action_resource(&message_address)
            .expect("message resolves lazily");
        let encoded = qos_actions::door::encode(&[message_resource], &message_address, None)
            .expect("exported message action");
        app.invoke_resource_operation(
            qos_actions::OperationKind::View,
            &encoded.type_id,
            &encoded.payload,
            &doc,
            &ROOT,
        )
        .unwrap();
        assert_eq!(app.active_channel, message.channel);
        assert_eq!(app.revealed_message.as_deref(), Some(message.id.as_str()));
        assert_eq!(app.thread_open, message.parent);

        let person = app.search_users.first().cloned().unwrap();
        let person_hit = provider
            .search(&person.name)
            .into_iter()
            .find(|hit| hit.kind == PERSON_KIND)
            .unwrap();
        let encoded =
            qos_actions::door::encode(&resources, &person_hit.action_address().unwrap(), None)
                .expect("exported person action");
        app.invoke_resource_operation(
            qos_actions::OperationKind::View,
            &encoded.type_id,
            &encoded.payload,
            &doc,
            &ROOT,
        )
        .unwrap();
        assert_eq!(app.revealed_person.as_deref(), Some(person.id.as_str()));
        assert!(app.right_flap_open());
    }
}
