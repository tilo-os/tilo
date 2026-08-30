//! Chat conversations as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`] / [`apps/mail`]: Chat
//! declares its searchable type by implementing [`SearchProvider`] over a
//! snapshot of its channels, so a conversation is findable from the unified
//! Ctrl+K palette by name or topic. Activating a row opens that channel
//! (switching workspace if needed).
//!
//! Only **conversations** are indexed here, not individual messages: the app
//! keeps only the *active* channel's messages in memory (others live in the
//! CRDT and would need reading on the query path), whereas every channel of
//! every workspace is already resident. Text channels only — a voice channel is
//! joined, not read. The query path touches no network.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{Channel, ChannelKind, Id, Workspace};

/// The kind label (and result-group header) Chat produces.
pub const CHANNEL_KIND: &str = "Conversations";

/// The `action_id` prefix. A chosen row's `action_id` is
/// `chat.channel:<id>`; [`parse_action_id`] turns it back into the channel id.
const ACTION_PREFIX: &str = "chat.channel:";

/// A [`SearchProvider`] over a snapshot of the text channels.
pub struct ChannelSearchProvider {
    channels: Vec<Channel>,
    workspaces: Vec<Workspace>,
}

impl ChannelSearchProvider {
    /// Snapshot the current channels + workspaces into a provider.
    pub fn new(channels: &[Channel], workspaces: &[Workspace]) -> Self {
        Self {
            channels: channels.to_vec(),
            workspaces: workspaces.to_vec(),
        }
    }

    /// The `action_id` for a channel — what a chosen row hands back.
    pub fn action_id(id: &str) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back into a channel id.
    pub fn parse_action_id(action_id: &str) -> Option<Id> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string)
    }

    /// The workspace name a channel belongs to, for its subtitle.
    fn workspace_name(&self, workspace_id: &str) -> Option<&str> {
        self.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.name.as_str())
    }
}

impl SearchProvider for ChannelSearchProvider {
    fn name(&self) -> &str {
        CHANNEL_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Text && !c.archived)
            .filter(|c| c.name.to_lowercase().contains(&q) || c.topic.to_lowercase().contains(&q))
            .map(|c| {
                // A "#name" title reads as a channel; the workspace name (and
                // topic, when present) disambiguates same-named channels.
                let subtitle = match (self.workspace_name(&c.workspace), c.topic.trim()) {
                    (Some(ws), "") => ws.to_string(),
                    (Some(ws), topic) => format!("{ws} · {topic}"),
                    (None, "") => String::new(),
                    (None, topic) => topic.to_string(),
                };
                let mut r =
                    SearchResult::new(CHANNEL_KIND, format!("#{}", c.name), Self::action_id(&c.id))
                        .with_icon(Icon::Mail);
                if !subtitle.is_empty() {
                    r = r.with_subtitle(subtitle);
                }
                r
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn seeded() -> (crate::app::ChatApp, automerge::AutoCommit) {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;
        let mut app = crate::app::ChatApp::new_ephemeral();
        let mut doc = AutoCommit::new();
        crate::doc::ChatDoc::seed_if_empty(&mut doc);
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    #[test]
    fn provider_finds_a_channel_by_name() {
        let (app, _doc) = seeded();
        let provider = ChannelSearchProvider::new(&app.channels, &app.workspaces);
        let hits = provider.search("frontend");
        assert!(
            hits.iter().any(|r| r.title == "#frontend"),
            "the provider surfaces the channel"
        );
        assert!(hits.iter().all(|r| r.kind == CHANNEL_KIND));
    }

    #[test]
    fn empty_query_browses_text_channels_only() {
        let (app, _doc) = seeded();
        let provider = ChannelSearchProvider::new(&app.channels, &app.workspaces);
        let hits = provider.search("");
        let expected = app
            .channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Text && !c.archived)
            .count();
        assert_eq!(hits.len(), expected);
        assert!(hits.iter().all(|r| r.title.starts_with('#')));
    }

    #[test]
    fn action_id_round_trips_to_a_channel() {
        let (app, _doc) = seeded();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(ChannelSearchProvider::new(
            &app.channels,
            &app.workspaces,
        )));
        let hit = registry
            .query("frontend")
            .into_iter()
            .find(|r| r.title == "#frontend")
            .expect("frontend is findable through the registry");
        let id = ChannelSearchProvider::parse_action_id(&hit.action_id).unwrap();
        assert!(
            app.channel(&id).is_some(),
            "the id resolves to a real stored channel"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(ChannelSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(ChannelSearchProvider::parse_action_id("chat.channel:").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_channel() {
        use automerge::ROOT;
        use ui::widget::Widget;

        let (mut app, mut doc) = seeded();

        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "chat publishes a search provider");
            let hit = providers[0]
                .search("frontend")
                .into_iter()
                .find(|r| r.title == "#frontend")
                .expect("the frontend channel is findable through the real hook");
            let id = ChannelSearchProvider::parse_action_id(&hit.action_id).unwrap();
            (hit.action_id, id)
        };
        assert_ne!(app.active_channel, id, "not already on the target channel");
        let ws = app.channel(&id).unwrap().workspace.clone();

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.active_channel, id,
            "activation opened the chosen channel"
        );
        assert_eq!(
            app.active_ws, ws,
            "activation switched to the channel's workspace"
        );
    }
}
