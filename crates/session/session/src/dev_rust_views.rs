//! A Rust app's resources and views in a development session, so a shell draws them live.
//!
//! Built only with `dev-rust-views` (a shot of a Rust app's view on the canvas,
//! `dist/dev/shell-run.sh --dev-rust-views`): the sample catalogue of Tube's channels and videos,
//! each a resource of the app `tube`, and the demo workspace's `#design-crit` channel of Chat,
//! each a resource of its app, published as the session publishes any app's resources, and the
//! views `#[view]` declared in the crates registered with the shell's runtime, so a place that
//! shows a channel draws that app's own `page` (one-app-model.md §2). Chat's channel posts: its
//! `post` action writes into the demo workspace the same service keeps, so the composer a
//! person types in adds a message the page draws. A person's session, and every production
//! image, carries none of it.

use serde_json::Value as Json;
use tilo_actions::{DoorError, ResourceSpec};
use tilo_service::{Service, Started};

/// The chat channel the demo workspace opens: what a place showing it draws Chat's page over.
const CHAT_CHANNEL: &str = "c_crit";

/// A small channel of the demo workspace, holding one message: a page whose whole transcript
/// stands within its view, so a shot of an attachment's press has the row in reach.
pub const CHAT_SHORT_CHANNEL: &str = "c_general";

/// Tube's sample channels and videos and Chat's demo conversation, as the session publishes
/// them: each a resource of its app, a channel through its `page` view.
pub struct RustApps {
    model: tube::model::TubeModel,
    /// Chat's demo workspace, read and written as its window does: the channel's messages and
    /// what its composer posts.
    chat: chat::demo::Workspace,
}

impl RustApps {
    /// The apps' sample catalogue, and the views this crate links registered with the shell.
    #[must_use]
    pub fn new() -> Self {
        let mut apps = Self {
            model: tube::sample::model(),
            chat: chat::demo::Workspace::demo().expect("the demo chat"),
        };
        apps.share_a_record();
        apps
    }

    /// The demo conversation's page, as its place draws it.
    fn chat_channel(&self) -> chat::views::Channel {
        chat::views::Channel::read(
            &self.chat.read(),
            &self.chat.message_rows(CHAT_CHANNEL),
            CHAT_CHANNEL,
            chat::model::ME,
        )
    }

    /// The page of the small demo channel `CHAT_SHORT_CHANNEL`, whose whole transcript stands
    /// within a place: what a shot of an attachment's press draws.
    fn chat_short_channel(&self) -> chat::views::Channel {
        chat::views::Channel::read(
            &self.chat.read(),
            &self.chat.message_rows(CHAT_SHORT_CHANNEL),
            CHAT_SHORT_CHANNEL,
            chat::model::ME,
        )
    }

    /// Share the sample video `vid_rust_async` in the small demo channel, as a person sharing a
    /// record does: the message the channel then shows carries a row that places the video,
    /// which the session publishes (Tube's own). A development fixture, so a shot of an
    /// attachment's press has one to press, in a transcript short enough to reach it.
    fn share_a_record(&mut self) {
        let video = self
            .model
            .videos
            .iter()
            .find(|video| video.id == "vid_rust_async")
            .expect("the sample video");
        let at = tilo_reference::ResourceAt {
            app: "so.tilo.tube".to_owned(),
            record: None,
            reference: tilo_reference::ResourceRef::whole(
                "tube.video",
                format!("tube.video:{}", video.id),
                video.title.clone(),
            ),
        };
        let invoke =
            chat::parts::ChatPart::SHARE_RESOURCE.with(chat::messages::chat_part::ShareResource {
                channel: tilo_resource::Id::from_raw(CHAT_SHORT_CHANNEL),
                attached: at,
                text: Some("Worth a watch".to_owned()),
                mentions: None,
            });
        let _ = self.chat.apply_part_as(chat::model::ME, invoke);
    }
}

impl Default for RustApps {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for RustApps {
    fn resources(&self) -> Vec<ResourceSpec> {
        let channels = self.model.channels.iter().map(|channel| {
            let mut spec = ResourceSpec::new(
                format!("tube.channel:{}", channel.id),
                "tube.channel",
                channel.name.clone(),
            )
            .with_view(tilo_actions::ResourceViewSpec::new(
                "page",
                channel.name.clone(),
                "open",
            ))
            .with_type(tilo_actions::ResourceType {
                noun: tilo_actions::Noun::new("channel", "channels"),
                app: tilo_actions::Noun::new("Tube", "Tube"),
                root: false,
            });
            spec.app_id = Some("tube".to_owned());
            spec
        });
        let videos = self.model.videos.iter().map(|video| {
            let mut spec = ResourceSpec::new(
                format!("tube.video:{}", video.id),
                "tube.video",
                video.title.clone(),
            )
            .with_type(tilo_actions::ResourceType {
                noun: tilo_actions::Noun::new("video", "videos"),
                app: tilo_actions::Noun::new("Tube", "Tube"),
                root: false,
            });
            spec.app_id = Some("tube".to_owned());
            spec
        });
        let conversations = [self.chat_channel(), self.chat_short_channel()];
        let chat: Vec<ResourceSpec> = conversations
            .into_iter()
            .map(|conversation| {
                let mut chat = ResourceSpec::new(
                    conversation.resource().id,
                    "chat.workspace.channels",
                    format!("#{}", conversation.name),
                )
                .with_view(tilo_actions::ResourceViewSpec::new(
                    "page",
                    conversation.name.clone(),
                    "open",
                ))
                .with_type(tilo_actions::ResourceType {
                    noun: tilo_actions::Noun::new("channel", "channels"),
                    app: tilo_actions::Noun::new("Chat", "Chat"),
                    root: false,
                });
                // What the page's composer runs: the chat's own post, the one verb the session's
                // `ChatViewService` publishes on a channel, so the fixture and a person's
                // session offer the same composer.
                chat = chat.with_action(crate::conversations::post_action());
                chat.app_id = Some("chat".to_owned());
                chat
            })
            .collect();
        channels.chain(videos).chain(chat).collect()
    }

    fn read(&self, resource: &str) -> Option<Json> {
        if let Some(id) = resource.strip_prefix("tube.channel:") {
            return tube::views::Channel::from_model(&self.model, id)
                .and_then(|channel| serde_json::to_value(channel).ok());
        }
        if let Some(id) = resource.strip_prefix("tube.video:") {
            return self
                .model
                .videos
                .iter()
                .find(|video| video.id == id)
                .and_then(|video| serde_json::to_value(video).ok());
        }
        if let Some(rest) = resource
            .strip_prefix("chat.workspace/channels/")
            .or_else(|| resource.strip_prefix("chat.workspace.channels:"))
        {
            let id = rest.to_owned();
            let conversation = [self.chat_channel(), self.chat_short_channel()]
                .into_iter()
                .find(|channel| channel.id == id)?;
            return serde_json::to_value(&conversation).ok();
        }
        None
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<Started, DoorError> {
        // The composer's send, as the chat's own verb writes it: a message from the person
        // typing, in the channel it names.
        if action == "post"
            && (resource.starts_with("chat.workspace/channels/")
                || resource.starts_with("chat.workspace.channels:"))
        {
            let channel = arguments
                .get("channel")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let text = arguments
                .get("text")
                .and_then(Json::as_str)
                .unwrap_or_default();
            if !text.trim().is_empty() {
                self.chat
                    .say(chat::model::ME, channel, text)
                    .map_err(|error| DoorError::Failed(format!("{error:?}")))?;
            }
            return Ok(Started::Done(tilo_actions::ActionOutput::default()));
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chat_channel_service_publishes_its_messages_and_their_people() {
        let apps = RustApps::new();
        let published: Vec<String> = apps
            .resources()
            .into_iter()
            .filter(|spec| spec.kind == "chat.workspace.channels")
            .map(|spec| spec.id.as_str().to_owned())
            .collect();
        assert_eq!(
            published,
            [
                "chat.workspace.channels:c_crit",
                "chat.workspace.channels:c_general"
            ],
            "the demo's conversation and its short channel are published"
        );
        let reading = apps
            .read("chat.workspace.channels:c_crit")
            .expect("the channel's reading");
        let channel: chat::views::Channel =
            serde_json::from_value(reading).expect("the reading is a channel");
        assert_eq!(channel.name, "design-crit");
        assert!(
            !channel.messages.is_empty(),
            "the demo channel holds messages"
        );
        let first = &channel.messages[0];
        assert!(!first.text.trim().is_empty(), "a message says something");
        let author = channel
            .person(&first.author)
            .unwrap_or_else(|| panic!("the page names {} in {:?}", first.author, channel.people));
        assert_ne!(author.name, first.author, "a person is named, not their id");
    }
}
