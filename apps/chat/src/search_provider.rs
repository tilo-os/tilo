//! Chat global search backed exclusively by canonical resource actions.

use tilo_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use crate::model::{Category, Channel, Message, User, Workspace};

pub const CHANNEL_KIND: &str = "Conversations";
pub const MESSAGE_KIND: &str = "Messages";
pub const PERSON_KIND: &str = "People";
pub const WORKSPACE_KIND: &str = "Workspaces";
pub const CATEGORY_KIND: &str = "Categories";

pub struct ChatSearchProvider {
    /// Complete, paged index over the durable chat namespace.  This is never a
    /// capped snapshot: a late message remains discoverable even when a room
    /// has far more rows than a single palette frame can inspect or render.
    catalog: SearchCatalog,
    categories: Vec<Category>,
    channels: Vec<Channel>,
    workspaces: Vec<Workspace>,
    messages: Vec<Message>,
    users: Vec<User>,
}

impl ChatSearchProvider {
    pub fn new(
        categories: &[Category],
        channels: &[Channel],
        workspaces: &[Workspace],
        messages: &[Message],
        users: &[User],
    ) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new("Chat"),
            categories: categories.to_vec(),
            channels: channels.to_vec(),
            workspaces: workspaces.to_vec(),
            messages: messages.to_vec(),
            users: users.to_vec(),
        };
        provider.index_complete_snapshot();
        provider
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
    fn workspace_address(id: &str) -> ActionAddress {
        ActionAddress::new(format!("chat.workspace:{id}"), "open")
    }
    fn category_address(id: &str) -> ActionAddress {
        ActionAddress::new(format!("chat.category:{id}"), "open")
    }

    /// Index each row exactly once.  The catalog validates that every result
    /// names an action on the same canonical resource before it accepts it;
    /// search cannot accidentally grow a private navigation backdoor.
    fn index_complete_snapshot(&self) {
        for workspace in &self.workspaces {
            let result = SearchResult::for_action(
                WORKSPACE_KIND,
                workspace.name.clone(),
                Self::workspace_address(&workspace.id),
            )
            .with_subtitle("Workspace");
            self.upsert(
                format!("chat.workspace:{}", workspace.id),
                &workspace.name,
                result,
            );
        }
        for channel in &self.channels {
            let workspace = self.workspace_name(&channel.workspace);
            let subtitle = if channel.topic.trim().is_empty() {
                workspace.to_string()
            } else {
                channel.topic.clone()
            };
            let result = SearchResult::for_action(
                CHANNEL_KIND,
                channel.kind.label(&channel.name),
                Self::channel_address(&channel.id),
            )
            .with_subtitle(subtitle)
            .with_icon(match channel.kind {
                crate::model::ChannelKind::Voice => Icon::Volume2,
                crate::model::ChannelKind::Text => Icon::List,
            });
            self.upsert(
                format!("chat.channel:{}", channel.id),
                format!("{} {} {}", channel.name, channel.topic, workspace),
                result,
            );
        }
        for category in &self.categories {
            let workspace = self.workspace_name(&category.workspace);
            let result = SearchResult::for_action(
                CATEGORY_KIND,
                category.name.clone(),
                Self::category_address(&category.id),
            )
            .with_subtitle(workspace.to_string());
            self.upsert(
                format!("chat.category:{}", category.id),
                format!("{} {workspace}", category.name),
                result,
            );
        }
        for message in &self.messages {
            let Some(channel) = self.channel(&message.channel) else {
                // An orphaned row is not a navigable resource.  Refusing to
                // index it is preferable to advertising an activation that a
                // fresh resource tree must reject.
                continue;
            };
            let author = self.user_name(&message.author);
            let attachment = message
                .attachment
                .as_ref()
                .map(super::model::Attachment::name);
            let title = if message.text.trim().is_empty() {
                attachment.unwrap_or("Attachment").to_string()
            } else {
                message.text.trim().chars().take(96).collect()
            };
            let result = SearchResult::for_action(
                MESSAGE_KIND,
                title.clone(),
                Self::message_address(&message.id),
            )
            .with_subtitle(format!("{author} in {}", channel.name))
            .with_icon(Icon::Mail)
            .with_row(
                ui::search::SearchRow::message(author.clone(), channel.name.clone()).snippet(title),
            );
            self.upsert(
                format!("chat.message:{}", message.id),
                format!("{author} {} {}", message.text, attachment.unwrap_or("")),
                result,
            );
        }
        for user in &self.users {
            let result = SearchResult::for_action(
                PERSON_KIND,
                user.name.clone(),
                Self::person_address(&user.id),
            )
            .with_subtitle(if user.is_agent { "Agent" } else { "Person" });
            self.upsert(
                format!("chat.person:{}", user.id),
                format!("{} {}", user.name, user.id),
                result,
            );
        }
    }

    fn upsert(&self, resource_id: String, searchable_text: impl AsRef<str>, result: SearchResult) {
        // All inputs above are created from their matching stable resource id,
        // so validation failure would be a programming error rather than user
        // input.  Do not expose a partially indexed namespace if that contract
        // is ever broken: one row being absent is safer than a non-canonical hit.
        let _ = self.catalog.upsert(resource_id, searchable_text, result);
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

impl CatalogSearchProvider for ChatSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Chat"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive resource-kind dispatcher keeps canonical search addresses and their matching open operations adjacent"
    )]
    fn resolve_action_address(&self, address: &ActionAddress) -> Option<tilo_actions::Resource> {
        if let Some(id) = address.resource.strip_prefix("chat.workspace:") {
            let workspace = self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == id)?;
            return (address == &Self::workspace_address(id)).then(|| {
                tilo_actions::Resource::new(
                    address.resource.clone(),
                    "chat.workspace",
                    workspace.name.clone(),
                )
                .with_action(crate::actions::view_operation(
                    "open",
                    "Open workspace",
                    &crate::actions::ChatOperation::OpenWorkspace {
                        workspace: id.to_string(),
                    },
                ))
            });
        }
        if let Some(id) = address.resource.strip_prefix("chat.channel:") {
            let channel = self.channels.iter().find(|channel| channel.id == id)?;
            return (address == &Self::channel_address(id)).then(|| {
                tilo_actions::Resource::new(
                    address.resource.clone(),
                    "chat.channel",
                    channel.kind.label(&channel.name),
                )
                .with_action(crate::actions::view_operation(
                    "open",
                    "Open conversation",
                    &crate::actions::ChatOperation::OpenChannel {
                        workspace: channel.workspace.clone(),
                        channel: id.to_string(),
                    },
                ))
            });
        }
        if let Some(id) = address.resource.strip_prefix("chat.category:") {
            let category = self.categories.iter().find(|category| category.id == id)?;
            return (address == &Self::category_address(id)).then(|| {
                tilo_actions::Resource::new(
                    address.resource.clone(),
                    "chat.category",
                    category.name.clone(),
                )
                .with_action(crate::actions::view_operation(
                    "open",
                    "Open category",
                    &crate::actions::ChatOperation::OpenCategory {
                        workspace: category.workspace.clone(),
                        category: id.to_string(),
                    },
                ))
            });
        }
        if let Some(id) = address.resource.strip_prefix("chat.person:") {
            let user = self.users.iter().find(|user| user.id == id)?;
            return (address == &Self::person_address(id)).then(|| {
                tilo_actions::Resource::new(
                    address.resource.clone(),
                    "chat.person",
                    user.name.clone(),
                )
                .with_action(crate::actions::view_operation(
                    "open",
                    "Open person",
                    &crate::actions::ChatOperation::OpenPerson {
                        user: id.to_string(),
                    },
                ))
            });
        }
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
        if address != &Self::message_address(message_id) {
            return None;
        }
        let open = tilo_actions::ActionSpec::operation(
            "open",
            "Reveal message",
            tilo_actions::OperationKind::View,
            crate::actions::CHAT_OPERATION_TYPE,
            &crate::actions::ChatOperation::RevealMessage {
                workspace: workspace.id.clone(),
                channel: channel.id.clone(),
                message: message.id.clone(),
                parent: message.parent.clone(),
            },
        )
        .ok()?
        .retry_policy(tilo_actions::RetryPolicy::Idempotent);
        Some(
            tilo_actions::Resource::new(
                address.resource.clone(),
                "chat.message",
                message.text.clone(),
            )
            .with_action(open),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::search::SearchProvider;
    use ui::search::testing::assert_late_hit_reachable;
    use ui::widget::Widget;

    fn workspace() -> Workspace {
        Workspace {
            id: "ws".into(),
            name: "Team Ångström".into(),
            glyph: "TA".into(),
            color_idx: 0,
            order: 0,
        }
    }

    fn channel() -> Channel {
        Channel {
            id: "c".into(),
            workspace: "ws".into(),
            category: String::new(),
            name: "general".into(),
            topic: String::new(),
            kind: crate::model::ChannelKind::Text,
            order: 0,
            unread: 0,
            archived: false,
        }
    }

    fn user() -> User {
        User {
            id: "u".into(),
            name: "Élodie".into(),
            initials: "É".into(),
            color_idx: 0,
            status: crate::model::Presence::Online,
            is_agent: false,
        }
    }

    fn message(id: usize, text: impl Into<String>) -> Message {
        Message {
            id: format!("m_{id:06}"),
            channel: "c".into(),
            author: "u".into(),
            ts: id as i64,
            text: text.into(),
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            reactions: Vec::new(),
            mentions: Vec::new(),
            reference: None,
            attachment: None,
            pinned: false,
            work: None,
        }
    }

    fn seeded() -> (crate::app::ChatApp, AutoCommit) {
        let mut app = crate::app::ChatApp::new_ephemeral();
        let mut doc = AutoCommit::new();
        crate::doc::ChatDoc::seed_if_empty(&mut doc);
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn search_hits_carry_canonical_addresses_that_resolve_navigate_and_refuse_stale() {
        // Phase: every_kind_uses_a_canonical_address.
        {
            let (app, _) = seeded();
            let providers = ui::search::capability::search_providers(&app);
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
        // Phase: every_search_kind_resolves_its_canonical_open_resource.
        {
            let (app, _) = seeded();
            let providers = ui::search::capability::search_providers(&app);
            let provider = &providers[0];
            for kind in [
                WORKSPACE_KIND,
                CHANNEL_KIND,
                CATEGORY_KIND,
                PERSON_KIND,
                MESSAGE_KIND,
            ] {
                let hit = provider
                    .search("")
                    .into_iter()
                    .find(|hit| hit.kind == kind)
                    .unwrap_or_else(|| panic!("missing search result kind {kind}"));
                let address = hit.action_address().expect("canonical action address");
                let resource = provider
                    .resolve_action_resource(&address)
                    .unwrap_or_else(|| panic!("{kind} did not resolve lazily"));
                assert_eq!(resource.id.as_str(), address.resource);
                assert!(resource.actions.iter().any(|action| action.id == "open"));
            }
        }
        // Phase: limited_search_honours_the_materialisation_budget.
        {
            let (app, _) = seeded();
            let providers = ui::search::capability::search_providers(&app);
            let provider = &providers[0];
            assert_eq!(provider.search_limited("", 1).len(), 1);
            assert!(provider.search_limited("", 0).is_empty());
        }
        // Phase: search_addresses_resolve_and_navigate_exactly.
        {
            let (mut app, mut doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            let providers = ui::search::capability::search_providers(&app);
            let provider = &providers[0];
            let channel_hit = provider
                .search("")
                .into_iter()
                .find(|hit| hit.title == "#frontend")
                .expect("frontend");
            let address = channel_hit.action_address().unwrap();
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .expect("exported channel action");
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &ROOT,
            )
            .map(|started| started.is_some())
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
            let encoded = tilo_actions::door::encode(&[message_resource], &message_address, None)
                .expect("exported message action");
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &ROOT,
            )
            .map(|started| started.is_some())
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
                tilo_actions::door::encode(&resources, &person_hit.action_address().unwrap(), None)
                    .expect("exported person action");
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &ROOT,
            )
            .map(|started| started.is_some())
            .unwrap();
            assert_eq!(app.revealed_person.as_deref(), Some(person.id.as_str()));
            assert!(app.right_flap_open());
        }
        // Phase: stale_search_resource_is_refused_at_activation.
        {
            let (mut app, mut doc) = seeded();
            let message = app
                .search_messages
                .iter()
                .find(|message| !message.text.is_empty())
                .cloned()
                .unwrap();
            let mut providers = ui::search::capability::search_providers(&app);
            let provider = providers.pop().unwrap();
            let hit = provider
                .search(&message.text)
                .into_iter()
                .find(|hit| {
                    hit.action_address().is_some_and(|address| {
                        address.resource == format!("chat.message:{}", message.id)
                    })
                })
                .unwrap();
            let address = hit.action_address().unwrap();
            let stale = provider.resolve_action_resource(&address).unwrap();
            let encoded = tilo_actions::door::encode(&[stale], &address, None).unwrap();
            crate::doc::ChatDoc::delete_message(&mut doc, &message.channel, &message.id);
            app.reload(&doc);

            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .map(|started| started.is_some())
                .is_err()
            );
        }
    }

    #[test]
    fn message_search_pages_bound_work_in_large_histories() {
        // Phase: unicode_late_message_remains_findable_in_a_large_history.
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let messages = corpus
                        .map(|row| {
                            let text = if row.is_target {
                                format!("{} late ÄTHER-needle 🧪", row.text)
                            } else {
                                row.text
                            };
                            message(row.index, text)
                        })
                        .collect::<Vec<_>>();
                    ChatSearchProvider::new(&[], &[channel()], &[workspace()], &messages, &[user()])
                },
                3_000,
            );
            assert_eq!(
                hit.action_address(),
                Some(ActionAddress::new("chat.message:m_002999", "open"))
            );

            let page =
                provider.search_page(&ui::search::SearchQuery::new("äther-needle 🧪"), None, 8, 8);
            assert_eq!(page.inspected, 1, "the rare gram must avoid a corpus scan");
            assert_eq!(page.results, vec![hit]);
        }
        // Phase: adversarial_message_search_pages_bound_work_and_materialization.
        {
            let mut messages = (0..1_000)
                .map(|index| message(index, "abcXbcd"))
                .collect::<Vec<_>>();
            messages.push(message(1_000, "the exact abcd message"));
            let provider =
                ChatSearchProvider::new(&[], &[channel()], &[workspace()], &messages, &[user()]);

            let mut cursor = None;
            let mut results = Vec::new();
            loop {
                let page = provider.search_page(
                    &ui::search::SearchQuery::new("abcd"),
                    cursor.as_ref(),
                    31,
                    4,
                );
                assert!(page.inspected <= 31);
                assert!(page.results.len() <= 4);
                assert!(
                    page.results
                        .iter()
                        .all(|result| result.action_address().is_some())
                );
                results.extend(page.results);
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].title, "the exact abcd message");
        }
    }
}
