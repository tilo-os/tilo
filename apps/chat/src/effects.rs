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
    #[effect(row, idempotent, device_only)]
    CallStatus { channel: String },
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
            }
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

    /// The effects this window cannot run right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        if self.external_services {
            return Vec::new();
        }
        let reason = "this host has no attachment store or voice service";
        vec![
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
        }))
    }
}
