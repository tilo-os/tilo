//! What Chat does that changes no document: showing a conversation, a
//! message or a person, reading a message as a value, sharing a file from
//! this device, and the call controls of the shell's voice service.
//!
//! Each is a [`ChatEffect`], offered bound on the item it acts on (a
//! workspace, a conversation, a message, a call), so the palette, an agent
//! or a script reaches the same handler a click does. What an effect changes
//! in the document — the message a shared file becomes — it changes through
//! ordinary [`ChatAction`](crate::actions::ChatAction)s. Sharing a file and
//! the call controls reach this device's files and voice service, so only
//! the person at the device runs them.

use ui::framework::{EffectAnswer, EffectStatus, Effected};
use ui::native_media::Command;
use ui::tilo_resource::{self, Effect};

use crate::app::{ChatApp, Focus};
use crate::model::ChannelKind;

/// Why `start-video-call` is unavailable and refused: calls carry
/// microphone audio only.
pub(crate) const VIDEO_CALL_UNAVAILABLE: &str = "voice_only";

pub(crate) fn video_call_unavailable_reason() -> String {
    crate::i18n::t("chat-start-video-call-unavailable")
}

/// Chat's effects. See the [module docs](self).
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "chat.effects")]
pub enum ChatEffect {
    /// Open workspace
    #[effect(view, row)]
    OpenWorkspace { workspace: String },
    /// Open conversation
    #[effect(view, row)]
    OpenChannel { channel: String },
    /// Open category
    #[effect(view, row)]
    OpenCategory { category: String },
    /// Fold or unfold category
    ///
    /// Hides or shows the category's channels in this person's sidebar.
    #[effect(view, row)]
    ToggleCategory { category: String },
    /// Reveal message
    #[effect(view, row)]
    RevealMessage { message: String },
    /// Open person
    #[effect(view, row)]
    OpenPerson { user: String },
    /// Read message
    ///
    /// The message as a value: who wrote it, whether that is an agent, its
    /// whole text and the agent's record of its work.
    #[effect(view, row, returns)]
    ReadMessage { message: String },
    /// Attach file
    ///
    /// Shares a file from this device in a conversation: its bytes go to
    /// this device's attachment store and the message carries them by
    /// reference.
    #[effect(device_only)]
    AttachFile {
        #[param("Id of the conversation")]
        channel: String,
        #[param("Path of the file on this device")]
        path: String,
    },
    /// Share live Chat context
    ///
    /// Opt in to publishing the open conversation, thread and scroll position.
    #[effect(device_only, idempotent)]
    ShareLiveContext,
    /// Stop sharing live Chat context
    #[effect(device_only, idempotent)]
    StopSharingLiveContext,
    /// Live context status
    #[effect(device_only, idempotent, returns)]
    LiveContextStatus,
    /// Open Slack channel in browser
    #[effect(device_only)]
    OpenSlackChannel { workspace: String, channel: String },
    /// Open Discord channel in browser
    #[effect(device_only)]
    OpenDiscordChannel { server: String, channel: String },
    /// Join call
    #[effect(row, device_only)]
    JoinCall { channel: String },
    /// Leave call
    #[effect(row, idempotent, device_only)]
    LeaveCall { channel: String },
    /// Mute
    #[effect(row, idempotent, device_only)]
    Mute { channel: String },
    /// Unmute
    #[effect(row, device_only)]
    Unmute { channel: String },
    /// Call status
    ///
    /// Requests a refresh and returns the last reported service state.
    /// The response is explicitly a cached report, never microphone consent.
    #[effect(row, idempotent, device_only, returns)]
    CallStatus { channel: String },
    /// Start a call
    ///
    /// Joins the open workspace's voice channel, making one when the
    /// workspace has none, or reveals the call already running.
    #[effect(row, idempotent, device_only)]
    StartCall,
    /// Start a video call
    ///
    /// Refused: calls carry microphone audio only.
    #[effect(row, idempotent, device_only)]
    StartVideoCall,
    /// Select microphone
    #[effect(device_only)]
    SelectInput {
        #[param("Id of the voice channel")]
        channel: String,
        #[param("Id of the audio input")]
        input: String,
    },
}

impl ChatApp {
    /// Answer `effect` for this window.
    pub(crate) fn answer_effect(&mut self, effect: ChatEffect) -> EffectAnswer {
        match effect {
            ChatEffect::OpenWorkspace { workspace } => {
                self.reveal_workspace_resource(workspace)?;
                self.reselect();
            }
            ChatEffect::OpenChannel { channel } => self.open_channel(&channel)?,
            ChatEffect::OpenCategory { category } => self.reveal_category(&category)?,
            ChatEffect::ToggleCategory { category } => {
                if !self.categories.iter().any(|item| item.id == category) {
                    return Err("category no longer exists".into());
                }
                self.toggle_category(&category);
            }
            ChatEffect::RevealMessage { message } => self.reveal_message(&message)?,
            ChatEffect::OpenPerson { user } => self.open_person(&user)?,
            ChatEffect::ReadMessage { message } => {
                return self.read_message(&message).map(Effected::value);
            }
            ChatEffect::AttachFile { channel, path } => {
                if !self.external_services {
                    return Err("attachment import unsupported: no host asset transport".into());
                }
                let path = path.trim();
                if path.is_empty() {
                    return Err("attachment path is empty".into());
                }
                if !self.channels.iter().any(|item| item.id == channel) {
                    return Err("conversation no longer exists".into());
                }
                if !self.begin_attachment_import_into(&channel, std::path::Path::new(path)) {
                    return Err("attachment import was not accepted".into());
                }
            }
            ChatEffect::ShareLiveContext => {
                if !self.channels.iter().any(|channel| {
                    channel.id == self.active_channel
                        && channel.kind == ChannelKind::Text
                        && !channel.archived
                }) {
                    return Err("Open a conversation before sharing its context".into());
                }
                self.sharing_context = true;
                self.request_relayout();
            }
            ChatEffect::StopSharingLiveContext => {
                self.sharing_context = false;
                self.request_relayout();
            }
            ChatEffect::LiveContextStatus => {
                return Ok(Effected::value(serde_json::json!({
                    "sharing": self.sharing_context,
                    "following": self.following_context,
                    "channel": self.active_channel,
                    "videoCapture": false,
                })));
            }
            ChatEffect::OpenSlackChannel { .. } | ChatEffect::OpenDiscordChannel { .. } => {
                return Err("Browser handoff requires the owning host".into());
            }
            ChatEffect::JoinCall { channel } => {
                let channel_id = channel.clone();
                self.native_voice_command(&channel, Command::Join { channel_id })?;
            }
            ChatEffect::LeaveCall { channel } => {
                self.native_voice_command(&channel, Command::Leave)?;
            }
            ChatEffect::Mute { channel } => {
                self.native_voice_command(&channel, Command::SetMuted { muted: true })?;
            }
            ChatEffect::Unmute { channel } => {
                self.native_voice_command(&channel, Command::SetMuted { muted: false })?;
            }
            ChatEffect::CallStatus { channel } => {
                self.native_voice_command(&channel, Command::Status)?;
                return Ok(Effected::value(serde_json::json!({
                    "channel": self.native_voice.channel,
                    "reported": self.native_voice.snapshot,
                    "refreshRequested": true,
                })));
            }
            ChatEffect::StartCall => self.start_call(None)?,
            ChatEffect::StartVideoCall => return Err(video_call_unavailable_reason()),
            ChatEffect::SelectInput { channel, input } => {
                self.native_voice_command(
                    &channel,
                    Command::SelectInput {
                        input_id: input.trim().into(),
                    },
                )?;
            }
        }
        Ok(Effected::done())
    }

    /// Start a call in the open workspace: join its voice channel, making
    /// one first when it has none so a text-only workspace can still huddle.
    /// A call already running is revealed, never closed. The channel is
    /// made through `doc` at once when the caller holds the document (the
    /// header button), else through the dispatcher, and joined once it is
    /// there.
    pub(crate) fn start_call(
        &mut self,
        doc: Option<&mut ui::widget::Edits<'_>>,
    ) -> Result<(), String> {
        if !self.call_allows(crate::call::CallAction::Join) {
            return Err("Call action is not permitted".into());
        }
        if self.call.active {
            self.call.expanded = true;
            self.request_relayout();
            return Ok(());
        }
        if self.active_ws.is_empty() {
            return Err("No chat workspace is open".into());
        }
        let existing = self
            .ws_channels()
            .into_iter()
            .find(|channel| channel.kind == ChannelKind::Voice)
            .map(|channel| channel.id.clone());
        let channel = if let Some(existing) = existing {
            existing
        } else {
            if !self.can_participate() {
                return Err("A room writer must create a voice channel".into());
            }
            let id = crate::events::new_channel_id();
            let create = crate::actions::create_channel_action(
                &self.active_ws,
                "",
                &id,
                "Voice",
                ChannelKind::Voice,
            );
            let Some(doc) = doc else {
                let dispatch = self.dispatch.as_ref().ok_or("Chat is open to view only")?;
                dispatch.apply(&create);
                self.pending_call = Some(id);
                return Ok(());
            };
            self.apply_action(doc, &create);
            self.reload(doc);
            id
        };
        self.native_voice_command(
            &channel,
            Command::Join {
                channel_id: channel.clone(),
            },
        )
    }

    /// Join the call a started call is waiting on, once its voice channel
    /// is in the document.
    pub(crate) fn join_pending_call(&mut self) {
        let Some(channel) = self.pending_call.clone() else {
            return;
        };
        if self.channel(&channel).is_none() {
            return;
        }
        self.pending_call = None;
        if let Err(error) = self.native_voice_command(
            &channel,
            Command::Join {
                channel_id: channel.clone(),
            },
        ) {
            self.media_error = Some(error);
        }
    }

    /// The effects this window cannot run right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let video = EffectStatus::unavailable(
            ChatEffect::START_VIDEO_CALL,
            VIDEO_CALL_UNAVAILABLE,
            video_call_unavailable_reason(),
        );
        if self.external_services {
            return vec![video];
        }
        let reason = "this host has no attachment store or voice service";
        vec![
            video,
            EffectStatus::unavailable(ChatEffect::START_CALL, "no_services", reason),
            EffectStatus::unavailable(ChatEffect::ATTACH_FILE, "no_services", reason),
            EffectStatus::unavailable(ChatEffect::JOIN_CALL, "no_services", reason),
            EffectStatus::unavailable(ChatEffect::UNMUTE, "no_services", reason),
            EffectStatus::unavailable(ChatEffect::SELECT_INPUT, "no_services", reason),
        ]
    }

    fn open_channel(&mut self, channel: &str) -> Result<(), String> {
        let workspace = self
            .channels
            .iter()
            .find(|item| item.id == channel)
            .map(|item| item.workspace.clone())
            .ok_or("conversation no longer exists")?;
        self.active_ws = workspace;
        channel.clone_into(&mut self.active_channel);
        self.inbox_open = false;
        self.thread_open = None;
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        self.reset_timeline_to_latest();
        self.focus = Focus::Main;
        self.reselect();
        Ok(())
    }

    fn reveal_category(&mut self, category: &str) -> Result<(), String> {
        let workspace = self
            .categories
            .iter()
            .find(|item| item.id == category)
            .map(|item| item.workspace.clone())
            .ok_or("category no longer exists")?;
        self.reveal_category_resource(workspace, category)?;
        self.reselect();
        Ok(())
    }

    fn reveal_message(&mut self, message: &str) -> Result<(), String> {
        let found = self
            .search_messages
            .iter()
            .find(|item| item.id == message)
            .ok_or("message no longer exists")?;
        let (channel, parent) = (found.channel.clone(), found.parent.clone());
        let workspace = self
            .channels
            .iter()
            .find(|item| item.id == channel)
            .map(|item| item.workspace.clone())
            .ok_or("conversation no longer exists")?;
        self.active_ws = workspace;
        self.active_channel = channel;
        self.inbox_open = false;
        self.thread_open = parent;
        self.revealed_message = Some(message.to_owned());
        self.reveal_scroll_pending = true;
        self.revealed_person = None;
        self.timeline_total_height = 0.0;
        self.timeline_at_end = false;
        self.focus = if self.thread_open.is_some() {
            Focus::Thread
        } else {
            Focus::Main
        };
        if self.thread_open.is_some() {
            self.right_flap.open();
        }
        self.reselect();
        Ok(())
    }

    fn open_person(&mut self, user: &str) -> Result<(), String> {
        if !self.search_users.iter().any(|item| item.id == user) {
            return Err("person no longer exists".into());
        }
        // Prefer the current workspace when it already evidences this
        // person; otherwise open the first conversation where they wrote.
        if !self.users.iter().any(|item| item.id == user)
            && let Some(channel) = self
                .search_messages
                .iter()
                .find(|message| {
                    message.author == user
                        || message.mentions.iter().any(|mention| mention.user == user)
                        || message
                            .reactions
                            .iter()
                            .any(|reaction| reaction.users.iter().any(|id| id == user))
                })
                .and_then(|message| self.channel(&message.channel))
                .map(|channel| (channel.workspace.clone(), channel.id.clone()))
        {
            (self.active_ws, self.active_channel) = channel;
        }
        self.inbox_open = false;
        self.thread_open = None;
        self.revealed_message = None;
        self.revealed_person = Some(user.to_owned());
        self.focus = Focus::Main;
        self.right_flap.open();
        self.reselect();
        Ok(())
    }

    /// One message as a value: who wrote it, whether that is an agent, its
    /// whole text and the agent's record of its work.
    fn read_message(&self, id: &str) -> Result<serde_json::Value, String> {
        let message = self
            .search_messages
            .iter()
            .find(|message| message.id == id)
            .ok_or("message no longer exists")?;
        let agent_name = self.agent_authors.get(&message.author).cloned();
        let agent = agent_name.is_some()
            || self
                .search_users
                .iter()
                .any(|user| user.id == message.author && user.is_agent);
        Ok(serde_json::json!({
            "id": message.id,
            "channel": message.channel,
            "parent": message.parent,
            "author": message.author,
            "authorName": agent_name.unwrap_or_else(|| self.display_name(&message.author)),
            "agent": agent,
            "mine": self.is_me(&message.author),
            "ts": message.ts,
            "edited": message.edited,
            "text": message.text,
            "work": message.work,
            "reference": message.reference,
        }))
    }
}
