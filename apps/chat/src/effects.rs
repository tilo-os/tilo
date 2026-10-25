//! What Chat does that changes no document: showing a conversation, a
//! message or a person, reading a message as a value, sharing a file from
//! this device, and the call controls of the shell's voice service.
//!
//! Each is a [`ChatEffect`], offered bound on the item it acts on (a
//! conversation, a category, a message, a call), so the palette, an agent
//! or a script reaches the same handler a click does. What an effect changes
//! in the document — the message a shared file becomes — it changes through
//! the record's verbs and field edits. Sharing a file and the call controls
//! reach this device's files and voice service: outside effects, decided by
//! the person's answers like any other.

use ui::framework::{EffectAnswer, EffectStatus, Effected, LiveRow};
use ui::native_media::Command;
use ui::tilo_actions::{ResourceViewRangeSpec, ResourceViewSpec};
use ui::tilo_resource::{self, Effect};

use crate::app::{ChatApp, Focus};
use crate::model::ChannelKind;
use crate::resource::Chat;

/// How many of a conversation's latest messages its row offers effects on;
/// any other message's row is resolved when it is addressed.
pub const RECENT_MESSAGES: usize = 20;

/// The id of the item resource the record publishes for `id` of `field`.
fn item_row_id(field: &str, id: &str) -> String {
    tilo_resource::items::item_resource_id::<Chat>(field, &tilo_resource::Id::from_raw(id))
}

/// The views an item of the chat opens in, each opened by the item's own
/// view effect: a category (its first conversation), and a message (to a
/// range of its text). A conversation's view depends on its kind and is
/// offered by its row.
pub(crate) fn item_views(field: &str, _item: &tilo_resource::Id) -> Vec<ResourceViewSpec> {
    match field {
        "categories" => vec![
            ResourceViewSpec::new("category", "Category", ChatEffect::OPEN_CATEGORY.id)
                .with_primary(true),
        ],
        "messages" => vec![
            ResourceViewSpec::new("message", "Message", ChatEffect::REVEAL_MESSAGE.id)
                .with_primary(true)
                .with_range(ResourceViewRangeSpec::new(
                    ChatEffect::REVEAL_MESSAGE_RANGE.id,
                )),
        ],
        _ => Vec::new(),
    }
}

/// Chat's effects. See the [module docs](self).
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "chat.effects")]
pub enum ChatEffect {
    /// Ask the device's model about the latest message.
    #[effect(row, returns, item = channels)]
    ModelControl {
        channel: String,
        #[param("Model command", choices = ["ask", "stop", "retry", "continue"])]
        command: String,
    },
    /// Show or hide reasoning actually supplied by the provider.
    #[effect(view, row, item = messages, audience = "agents")]
    ShowAgentReasoning { message: String, expanded: bool },
    /// Open conversation
    #[effect(view, row, item = channels)]
    OpenChannel { channel: String },
    /// Open category
    #[effect(view, row, item = categories)]
    OpenCategory { category: String },
    /// Fold or unfold category
    ///
    /// Hides or shows the category's channels in this person's sidebar.
    #[effect(view, row, item = categories)]
    ToggleCategory { category: String },
    /// Reveal message
    #[effect(view, row, item = messages)]
    RevealMessage { message: String },
    /// Reveal message range
    #[effect(view, row, returns, item = messages, audience = "agents")]
    RevealMessageRange {
        message: String,
        #[param("Start offset in the declared unit")]
        start: usize,
        #[param("End offset in the declared unit")]
        end: usize,
        #[param("Offset unit", choices = ["Chars", "Bytes"])]
        unit: String,
    },
    /// Open person
    #[effect(view, row, item = people)]
    OpenPerson { user: String },
    /// Open a private conversation with a certified person public key.
    #[effect(view, audience = "agents")]
    OpenDirectConversation { public_key: String },
    /// Expand an unsent private editor using its ephemeral native handoff token.
    #[effect(view, returns, audience = "agents")]
    AdoptPrivateDraft { token: String },
    /// Send privately through the native person-only host door.
    #[effect(returns, audience = "agents")]
    SendDirectMessage { public_key: String, text: String },
    /// Read a private conversation on this device.
    #[effect(returns, audience = "agents")]
    ReadDirectConversation { public_key: String },
    /// Read message
    ///
    /// The message as a value: who wrote it, whether that is an agent, its
    /// whole text and the agent's record of its work.
    #[effect(view, row, returns, item = messages, audience = "agents")]
    ReadMessage { message: String },
    /// Read room
    ///
    /// The room's categories and channels and its people as the record
    /// stores them, and, when asked, one message with the message it
    /// answers or a channel's latest messages: what a bot mirroring the
    /// room elsewhere works from.
    #[effect(kind = "read", returns, audience = "agents")]
    ReadRoom {
        #[param("A message to include, with the message it answers")]
        message: Option<String>,
        #[param("A channel whose latest messages to include, each with the message it answers")]
        channel: Option<String>,
    },
    /// Read conversation messages
    ///
    /// A page of the channel's messages, oldest first, from every part of
    /// the chat's messages this window holds, each with its author's id
    /// (`author`) and name (`authorName`); `cursor` is the preceding page's
    /// `nextCursor`.
    #[effect(kind = "read", returns, audience = "agents")]
    ReadMessages {
        #[param("Id of the channel")]
        channel: String,
        #[param(
            "Maximum messages in this page, 1 to 32 (default 32); more reads 32",
            name = "pageSize"
        )]
        page_size: Option<u8>,
        #[param("The preceding page's nextCursor")]
        cursor: Option<String>,
    },
    /// Export conversation
    ///
    /// Keeps the channel's transcript, oldest first, each line by the name
    /// readers see and its time as RFC 3339, as content, from every part
    /// of the chat's messages this window holds; answers with its content
    /// address. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportContent {
        #[param("Id of the channel")]
        channel: String,
        #[param("md (the default) or json", choices = ["md", "json"])]
        format: Option<String>,
    },
    /// Attach file
    ///
    /// Shares a file from this device in a conversation: its bytes go to
    /// this device's attachment store and the message carries them by
    /// reference.
    AttachFile {
        #[param("Id of the conversation")]
        channel: String,
        #[param("Path of the file on this device")]
        path: String,
    },
    /// Share live Chat context
    ///
    /// Opt in to publishing the open conversation, thread and scroll position.
    #[effect(idempotent)]
    ShareLiveContext,
    /// Stop sharing live Chat context
    #[effect(idempotent)]
    StopSharingLiveContext,
    /// Live context status
    #[effect(idempotent, returns, audience = "agents")]
    LiveContextStatus,
    /// Open Slack channel in browser
    OpenSlackChannel { workspace: String, channel: String },
    /// Open Discord channel in browser
    OpenDiscordChannel { server: String, channel: String },
    /// Join call
    ///
    /// Joins the channel's call muted, listening: being in a room's call is
    /// for whoever may read the room, as the channel itself is.
    #[effect(row, kind = "read")]
    JoinCall { channel: String },
    /// Leave call
    #[effect(row, idempotent, kind = "read")]
    LeaveCall { channel: String },
    /// Mute
    #[effect(row, idempotent, kind = "read")]
    Mute { channel: String },
    /// Unmute
    ///
    /// Speaks into the call: for whoever may write in the room. The
    /// microphone itself is asked for through the device's own prompt.
    #[effect(row, kind = "undoable")]
    Unmute { channel: String },
    /// Deafen
    ///
    /// Hear nothing of the call, and be muted, until undeafened.
    #[effect(row, idempotent, kind = "read")]
    Deafen { channel: String },
    /// Undeafen
    ///
    /// Hear the call again; the microphone comes back as it was.
    #[effect(row, idempotent, kind = "read")]
    Undeafen { channel: String },
    /// Turn camera on
    ///
    /// Shows this person's camera to everyone in the call; asked for
    /// through the normal permission prompt.
    #[effect(row, kind = "undoable")]
    CameraOn { channel: String },
    /// Turn camera off
    #[effect(row, idempotent, kind = "read")]
    CameraOff { channel: String },
    /// Share screen
    ///
    /// Shows this person's screen to everyone in the call; asked for
    /// through the normal permission prompt.
    #[effect(row, kind = "undoable")]
    ShareScreen { channel: String },
    /// Stop sharing screen
    #[effect(row, idempotent, kind = "read")]
    StopSharingScreen { channel: String },
    /// Call status
    ///
    /// Requests a refresh and returns the last reported service state.
    /// The response is explicitly a cached report, never microphone consent.
    #[effect(row, idempotent, returns, kind = "read", audience = "agents")]
    CallStatus { channel: String },
    /// Start a call
    ///
    /// Joins the room's voice channel, making one when the room has none,
    /// or reveals the call already running.
    #[effect(idempotent, kind = "read")]
    StartCall,
    /// Start a video call
    ///
    /// Starts a call as Start a call does, with the camera on once it is
    /// live.
    #[effect(idempotent, kind = "undoable")]
    StartVideoCall,
    /// Select microphone
    #[effect(kind = "read")]
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
            ChatEffect::ModelControl { channel, command } => {
                return self.model_command(&channel, &command).map(Effected::value);
            }
            ChatEffect::ShowAgentReasoning { message, expanded } => {
                self.show_agent_reasoning(message, expanded)?;
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
            ChatEffect::RevealMessageRange {
                message,
                start,
                end,
                unit,
            } => return self.reveal_message_range(&message, start, end, &unit),
            ChatEffect::OpenPerson { user } => self.open_person(&user)?,
            ChatEffect::OpenDirectConversation { public_key } => {
                self.open_person_conversation(&public_key)?;
            }
            ChatEffect::AdoptPrivateDraft { .. } => {
                return Err("private draft adoption requires its original effect context".into());
            }
            ChatEffect::SendDirectMessage { public_key, text } => {
                return self.send_person_message(&public_key, &text);
            }
            ChatEffect::ReadDirectConversation { public_key } => {
                return self.read_person_conversation(&public_key);
            }
            ChatEffect::ReadMessage { message } => {
                return self.read_message(&message).map(Effected::value);
            }
            ChatEffect::AttachFile { channel, path } => self.attach_file_effect(&channel, &path)?,
            ChatEffect::ShareLiveContext => self.set_live_context_sharing(true)?,
            ChatEffect::StopSharingLiveContext => self.set_live_context_sharing(false)?,
            ChatEffect::LiveContextStatus => {
                return Ok(Effected::value(serde_json::json!({
                    "sharing": self.sharing_context,
                    "following": self.following_context,
                    "channel": self.active_channel,
                    "videoCapture": false,
                })));
            }
            ChatEffect::ReadRoom { .. }
            | ChatEffect::ReadMessages { .. }
            | ChatEffect::ExportContent { .. } => {
                return Err("the record answers this read".into());
            }
            ChatEffect::OpenSlackChannel { .. } | ChatEffect::OpenDiscordChannel { .. } => {
                return Err("Browser navigation requires the owning host".into());
            }
            effect @ (ChatEffect::JoinCall { .. }
            | ChatEffect::LeaveCall { .. }
            | ChatEffect::Mute { .. }
            | ChatEffect::Unmute { .. }
            | ChatEffect::Deafen { .. }
            | ChatEffect::Undeafen { .. }
            | ChatEffect::CameraOn { .. }
            | ChatEffect::CameraOff { .. }
            | ChatEffect::ShareScreen { .. }
            | ChatEffect::StopSharingScreen { .. }) => {
                if let Some((channel, command)) = voice_command_of(effect) {
                    self.native_voice_command(&channel, command)?;
                }
            }
            ChatEffect::CallStatus { channel } => return self.native_call_status(&channel),
            ChatEffect::StartCall => self.start_call(None)?,
            ChatEffect::StartVideoCall => self.start_video_call()?,
            ChatEffect::SelectInput { channel, input } => {
                self.select_voice_input(&channel, &input)?;
            }
        }
        Ok(Effected::done())
    }

    /// Start a call with the camera on: at once when the call is already
    /// live and audio only, or as soon as it goes live.
    fn start_video_call(&mut self) -> Result<(), String> {
        self.start_call(None)?;
        if self.native_voice.snapshot.state == ui::native_media::State::Live
            && let Some(channel) = self.native_voice.channel.clone()
        {
            if self.call.audio_only {
                self.native_voice_command(&channel, Command::SetCamera { on: true })?;
            }
        } else {
            self.camera_on_live = true;
        }
        Ok(())
    }

    fn set_live_context_sharing(&mut self, sharing: bool) -> Result<(), String> {
        if sharing
            && !self.channels.iter().any(|channel| {
                channel.id == self.active_channel
                    && channel.kind == ChannelKind::Text
                    && !channel.archived
            })
        {
            return Err("Open a conversation before sharing its context".into());
        }
        self.sharing_context = sharing;
        self.request_relayout();
        Ok(())
    }

    fn select_voice_input(&mut self, channel: &str, input: &str) -> Result<(), String> {
        self.native_voice_command(
            channel,
            Command::SelectInput {
                input_id: input.trim().into(),
            },
        )
    }

    fn show_agent_reasoning(&mut self, message: String, expanded: bool) -> Result<(), String> {
        if !self.search_messages.iter().any(|m| {
            m.id == message
                && m.work
                    .as_ref()
                    .and_then(|work| work.stream.as_ref())
                    .is_some_and(|stream| !stream.reasoning.is_empty())
        }) {
            return Err("provider reasoning is unavailable".into());
        }
        self.thread_follow = false;
        if expanded {
            self.expanded_reasoning.insert(message);
        } else {
            self.expanded_reasoning.remove(&message);
        }
        Ok(())
    }

    fn attach_file_effect(&mut self, channel: &str, path: &str) -> Result<(), String> {
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
        if !self.begin_attachment_import_into(channel, std::path::Path::new(path)) {
            return Err("attachment import was not accepted".into());
        }
        Ok(())
    }

    fn native_call_status(&mut self, channel: &str) -> EffectAnswer {
        self.native_voice_command(channel, Command::Status)?;
        Ok(Effected::value(serde_json::json!({
            "channel": self.native_voice.channel,
            "reported": self.native_voice.snapshot,
            "clientIssue": self.native_voice.client_issue,
            "refreshRequested": true,
        })))
    }

    /// Start a call in the room: join its voice channel, making one first
    /// when it has none so a text-only room can still huddle.
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
        let existing = self
            .ws_channels()
            .into_iter()
            .find(|channel| channel.kind == ChannelKind::Voice)
            .map(|channel| channel.id.clone());
        let channel = if let Some(existing) = existing {
            existing
        } else {
            if !self.keeps_layout() {
                return Err("The room has no voice channel; its moderators make channels".into());
            }
            let Some(doc) = doc else {
                if self.dispatch.is_none() {
                    return Err("Chat is open to view only".into());
                }
                self.submit(crate::resource::Chat::CREATE_CHANNEL.with(
                    crate::record::chat::CreateChannel {
                        name: "Voice".into(),
                        category: None,
                        kind: Some(ChannelKind::Voice),
                        topic: None,
                    },
                ));
                // The channel's id is the room's to mint: the call joins the
                // voice channel once it is in the document.
                self.pending_call = Some(String::new());
                return Ok(());
            };
            let made = self.create_channel_now(doc, "", "Voice", ChannelKind::Voice);
            if made.is_empty() {
                return Err("The voice channel could not be made".into());
            }
            made
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
        let Some(pending) = self.pending_call.clone() else {
            return;
        };
        let channel = if pending.is_empty() {
            let Some(voice) = self
                .ws_channels()
                .into_iter()
                .find(|channel| channel.kind == ChannelKind::Voice)
            else {
                return;
            };
            voice.id.clone()
        } else {
            pending
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
        let mut unavailable = Vec::new();
        if !self.external_services {
            unavailable.push(EffectStatus::unavailable(
                ChatEffect::ATTACH_FILE,
                "no_services",
                crate::i18n::t("chat-attachment-import-unavailable"),
            ));
        }
        if !self.voice_service_available() {
            let reason = crate::i18n::t("chat-voice-unavailable");
            let reason = reason.as_str();
            unavailable.extend([
                EffectStatus::unavailable(ChatEffect::START_CALL, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::START_VIDEO_CALL, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::CAMERA_ON, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::SHARE_SCREEN, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::JOIN_CALL, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::UNMUTE, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::SELECT_INPUT, "no_services", reason),
            ]);
        }
        if !self.call.active {
            // A call's own controls mean nothing outside one: search and
            // menus do not offer turning a camera on with no call to show
            // it in. Joining is what applies.
            let reason = crate::i18n::t("chat-call-not-joined");
            let reason = reason.as_str();
            unavailable.extend([
                EffectStatus::unavailable(ChatEffect::LEAVE_CALL, "not_in_call", reason),
                EffectStatus::unavailable(ChatEffect::MUTE, "not_in_call", reason),
                EffectStatus::unavailable(ChatEffect::CAMERA_OFF, "not_in_call", reason),
                EffectStatus::unavailable(ChatEffect::STOP_SHARING_SCREEN, "not_in_call", reason),
            ]);
            // Without a voice service these already say why.
            if self.voice_service_available() {
                unavailable.extend([
                    EffectStatus::unavailable(ChatEffect::UNMUTE, "not_in_call", reason),
                    EffectStatus::unavailable(ChatEffect::CAMERA_ON, "not_in_call", reason),
                    EffectStatus::unavailable(ChatEffect::SHARE_SCREEN, "not_in_call", reason),
                ]);
            }
        }
        unavailable
    }

    fn open_channel(&mut self, channel: &str) -> Result<(), String> {
        if !self.channels.iter().any(|item| item.id == channel) {
            return Err("conversation no longer exists".into());
        }
        self.close_person_conversation();
        channel.clone_into(&mut self.active_channel);
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
        self.reveal_category_resource(category)?;
        self.reselect();
        Ok(())
    }

    fn reveal_message_range(
        &mut self,
        message: &str,
        start: usize,
        end: usize,
        unit: &str,
    ) -> EffectAnswer {
        let unit = match unit {
            "Chars" => tilo_reference::SpanUnit::Chars,
            "Bytes" => tilo_reference::SpanUnit::Bytes,
            _ => return Err("unsupported range unit".into()),
        };
        if start > end {
            return Err("backwards message range".into());
        }
        let span = tilo_reference::Span { start, end, unit };
        let text = self
            .search_messages
            .iter()
            .find(|m| m.id == message)
            .map(|m| &m.text)
            .ok_or("message no longer exists")?;
        let selected = span
            .slice(text)
            .ok_or("message range no longer fits")?
            .to_owned();
        let start_byte = match unit {
            tilo_reference::SpanUnit::Bytes => start,
            tilo_reference::SpanUnit::Chars => text.chars().take(start).map(char::len_utf8).sum(),
        };
        let end_byte = start_byte + selected.len();
        let source = text.clone();
        self.reveal_message(message)?;
        self.revealed_range = Some((message.to_owned(), source, start_byte, end_byte));
        Ok(Effected::value(
            serde_json::json!({"selected": selected, "span": span}),
        ))
    }

    pub(crate) fn reveal_message(&mut self, message: &str) -> Result<(), String> {
        let found = self
            .search_messages
            .iter()
            .find(|item| item.id == message)
            .ok_or("message no longer exists")?;
        let (channel, parent) = (found.channel.clone(), found.parent.clone());
        if !self.channels.iter().any(|item| item.id == channel) {
            return Err("conversation no longer exists".into());
        }
        self.active_channel = channel;
        self.thread_open = parent;
        self.revealed_range = None;
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
        // Prefer the room's people when they already evidence this person;
        // otherwise open the first conversation where they wrote.
        if !self.users.iter().any(|item| item.id == user)
            && let Some(channel) = self
                .search_messages
                .iter()
                .find(|message| {
                    message.author == user
                        || message.mentions.iter().any(|mention| mention.user == user)
                        || message.reactions.iter().any(|reaction| {
                            reaction.reactors.iter().any(|reactor| reactor.by == user)
                        })
                })
                .and_then(|message| self.channel(&message.channel))
                .map(|channel| channel.id.clone())
        {
            self.active_channel = channel;
        }
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
            "sourceProvider": message.source_provider,
        }))
    }
}

/// The most messages one [`read_messages`] page holds.
const PAGE_MOST: usize = 32;

/// The answer of [`ChatEffect::ReadMessages`]: `channel`'s messages in
/// `held` (every part of the chat's messages the window holds), oldest
/// first, a page of at most `page_size` after `cursor`, with the cursor of
/// the next page when there is more. Each names its author by id and by
/// the name `name` gives them, as the window shows it.
///
/// A page asked larger than [`PAGE_MOST`] reads a full page: a caller
/// asking for more wants as many as it can have, and reads on from the
/// cursor. A message delivered or bridged under another name is shown by
/// it ([`crate::transcript::shown_author`]).
///
/// # Errors
/// When `page_size` is zero or `cursor` names no place.
pub(crate) fn read_messages(
    held: &crate::messages::Messages,
    (channel, name): (&str, &dyn Fn(&str) -> String),
    page_size: Option<u8>,
    cursor: Option<&str>,
) -> Result<serde_json::Value, String> {
    let size = page_size.map_or(PAGE_MOST, |size| usize::from(size).min(PAGE_MOST));
    if size == 0 {
        return Err(format!("a page holds 1 to {PAGE_MOST} messages"));
    }
    let after = cursor
        .map(|cursor| {
            let (ts, id) = cursor
                .split_once(':')
                .ok_or("not a cursor of this channel")?;
            Ok::<_, String>((
                ts.parse::<i64>().map_err(|_| "not a cursor")?,
                id.to_owned(),
            ))
        })
        .transpose()?;
    // Oldest first by when written; posts of the same second keep the
    // order the chain holds them in (the stable sort), which is the order
    // they were posted in. Their ids are random, so ordering ties by id
    // would shuffle a burst of posts.
    let mut messages: Vec<(&tilo_resource::Id, &crate::record::Message)> = held
        .iter()
        .filter(|(_, found)| found.channel.as_str() == channel)
        .collect();
    messages.sort_by_key(|(_, found)| found.ts);
    if let Some((ts, last)) = &after {
        // After the cursor's message where it is still held; past its
        // second when it has gone since.
        let start = messages
            .iter()
            .position(|(id, _)| id.as_str() == last)
            .map_or_else(
                || messages.partition_point(|(_, found)| found.ts <= *ts),
                |at| at + 1,
            );
        messages.drain(..start);
    }
    let more = messages.len() > size;
    messages.truncate(size);
    let next = more
        .then(|| {
            messages
                .last()
                .map(|(id, found)| format!("{}:{}", found.ts, id.as_str()))
        })
        .flatten();
    Ok(serde_json::json!({
        "channel": channel,
        "messages": messages.iter().map(|(id, found)| serde_json::json!({
            "id": id.as_str(),
            "author": found.author.0.as_str(),
            "authorName": crate::transcript::shown_author(found, name),
            "ts": found.ts.to_string(),
            "parent": found.parent.as_ref().map_or("", tilo_resource::Id::as_str),
            "edited": found.edited,
            "text": found.text,
        })).collect::<Vec<_>>(),
        "nextCursor": next,
    }))
}

/// How many of a channel's latest messages [`read_room`] includes: a
/// post's change names its channel, not the message it made, so a bot finds
/// that message among the channel's newest.
const READ_LATEST: usize = 20;

/// The answer of [`ChatEffect::ReadRoom`]: the record's own JSON form of
/// the room's categories and channels, every person, the channels holding
/// any message (`busy`), and `message` and the latest messages of
/// `channel`, each with the message it answers.
pub(crate) fn read_room(
    chat: &Chat,
    held: &crate::messages::Messages,
    message: Option<&str>,
    channel: Option<&str>,
) -> serde_json::Value {
    use ui::tilo_resource::Model as _;
    let mut wanted: Vec<tilo_resource::Id> = message
        .map(tilo_resource::Id::from_raw)
        .into_iter()
        .collect();
    if let Some(channel) = channel {
        let mut latest: Vec<(&tilo_resource::Id, &crate::record::Message)> = held
            .iter()
            .filter(|(_, found)| found.channel.as_str() == channel)
            .collect();
        latest.sort_by_key(|(_, found)| found.ts);
        let skip = latest.len().saturating_sub(READ_LATEST);
        wanted.extend(latest.into_iter().skip(skip).map(|(id, _)| id.clone()));
    }
    // Each with the message it answers, once.
    let answered: Vec<tilo_resource::Id> = wanted
        .iter()
        .filter_map(|id| held.get(id)?.parent.clone())
        .collect();
    wanted.extend(answered);
    let mut messages = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in wanted {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(found) = held.get(&id) {
            messages.push(serde_json::json!({ "id": id.as_str(), "value": found.to_json() }));
        }
    }
    let busy: std::collections::BTreeSet<&str> = held
        .iter()
        .map(|(_, found)| found.channel.as_str())
        .collect();
    serde_json::json!({
        "categories": chat.categories.to_json(),
        "channels": chat.channels.to_json(),
        "people": chat.people.to_json(),
        "busy": busy,
        "messages": messages,
    })
}

impl ChatApp {
    /// The effects offered on what the window shows: each category and
    /// channel (a voice channel with its call), the latest
    /// messages of every conversation, the reviewed reports and the people.
    /// They are rows merged into the item resources the record publishes.
    pub(crate) fn published_rows(&self) -> Vec<LiveRow> {
        let categories = self
            .categories
            .iter()
            .filter_map(|category| self.category_row(&category.id));
        let channels = self
            .channels
            .iter()
            .filter_map(|channel| self.channel_row(&channel.id));
        let messages = self.channels.iter().flat_map(|channel| {
            let messages: Vec<&crate::model::Message> = self
                .search_messages
                .iter()
                .filter(|message| message.channel == channel.id)
                .collect();
            let recent = messages.len().saturating_sub(RECENT_MESSAGES);
            messages
                .into_iter()
                .skip(recent)
                .filter_map(|message| self.message_row(&message.id))
                .collect::<Vec<_>>()
        });
        let people = self
            .search_users
            .iter()
            .filter_map(|user| self.person_row(&user.id));
        let reports = self
            .reports
            .iter()
            .filter_map(|(report, _)| self.report_row(report));
        categories
            .chain(channels)
            .chain(messages)
            .chain(people)
            .chain(reports)
            .chain(Self::person_conversation_rows())
            .collect()
    }

    /// The row `id`, when [`Self::published_rows`] leaves it out.
    pub(crate) fn published_row(&self, id: &str) -> Option<LiveRow> {
        if id.starts_with("chat.person:") {
            return self.published_rows().into_iter().find(|row| row.id() == id);
        }
        let (field, item) = tilo_resource::items::item_of_resource::<Chat>(id)?;
        let item = item.as_str();
        match field {
            "categories" => self.category_row(item),
            "channels" => self.channel_row(item),
            "messages" => self.message_row(item),
            "people" => self.person_row(item),
            "feedback" => self.report_row(item),
            _ => None,
        }
    }

    fn category_row(&self, id: &str) -> Option<LiveRow> {
        let category = self.categories.iter().find(|item| item.id == id)?;
        Some(LiveRow::new(
            item_row_id("categories", id),
            "chat.workspace.categories",
            &category.name,
        ))
    }

    fn channel_row(&self, id: &str) -> Option<LiveRow> {
        let channel = self.channels.iter().find(|item| item.id == id)?;
        let channel_id = || id.to_owned();
        let row = LiveRow::new(
            item_row_id("channels", id),
            "chat.workspace.channels",
            &channel.name,
        );
        // A voice channel is a call, not a conversation to read.
        let row = if channel.kind == ChannelKind::Voice {
            row
        } else {
            row.view(
                ResourceViewSpec::new("conversation", "Conversation", ChatEffect::OPEN_CHANNEL.id)
                    .with_primary(true),
            )
        };
        let mut row = row;
        if !self.model_controls(id).is_empty() {
            row = row.operation(
                ChatEffect::ModelControl {
                    channel: id.to_owned(),
                    command: String::new(),
                }
                .bound_asking(&["command"]),
            );
        }
        // A call's own controls apply only inside it: outside, they are
        // published unavailable with why, so the palette and menus leave
        // them out and an agent is told to join first.
        let in_call = self.call.active && self.call.channel == id;
        let not_joined = crate::i18n::t("chat-call-not-joined");
        let in_call_only = |effect: ChatEffect| {
            let mut spec = effect.bound();
            if !in_call {
                spec.contract.availability = ui::tilo_actions::Availability::Unavailable {
                    code: "not_in_call".into(),
                    reason: not_joined.clone(),
                };
            }
            spec
        };
        Some(if channel.kind == ChannelKind::Voice {
            row.row(
                LiveRow::new(
                    format!("{}/call", item_row_id("channels", id)),
                    "chat.call",
                    crate::i18n::t_args(
                        "chat-call-title",
                        &[("channel", channel.name.clone().into())],
                    ),
                )
                .effect(&ChatEffect::OpenChannel {
                    channel: channel_id(),
                })
                .effect(&ChatEffect::JoinCall {
                    channel: channel_id(),
                })
                .operation(in_call_only(ChatEffect::LeaveCall {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::Mute {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::Unmute {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::Deafen {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::Undeafen {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::CameraOn {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::CameraOff {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::ShareScreen {
                    channel: channel_id(),
                }))
                .operation(in_call_only(ChatEffect::StopSharingScreen {
                    channel: channel_id(),
                }))
                .effect(&ChatEffect::CallStatus {
                    channel: channel_id(),
                }),
            )
        } else {
            row.operation(
                ChatEffect::ReadMessages {
                    channel: id.to_owned(),
                    page_size: None,
                    cursor: None,
                }
                .bound_asking(&["pageSize", "cursor"]),
            )
            .operation(crate::feedback::destination_read::spec(id))
        })
    }

    fn message_row(&self, id: &str) -> Option<LiveRow> {
        let message = self.search_messages.iter().find(|item| item.id == id)?;
        let author = if self.is_provider_output(message) {
            self.message_author_label(message)
        } else {
            self.author_name(&message.author)
        };
        let agent = self.is_provider_output(message)
            || self.agent_authors.contains_key(&message.author)
            || message
                .author
                .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX);
        let mut row = LiveRow::new(
            item_row_id("messages", id),
            "chat.workspace.messages",
            self.message_label(message),
        )
        // Who wrote it, beside the message's own words.
        .presentation(ui::tilo_actions::ResourcePresentation {
            detail: Some(author.chars().take(80).collect()),
            badge: agent.then(|| "agent".to_owned()),
            preview: None,
        });
        // The record keeps no message (they are in its parts), so the row
        // carries what the record offers on each: its reads and views.
        let item = tilo_resource::Id::from_raw(id);
        let effects = <Chat as tilo_resource::Resource>::item_effects("messages", &item);
        let offers_reasoning = effects.iter().any(|spec| spec.id == "show-agent-reasoning");
        for spec in effects {
            row = row.operation(spec);
        }
        for view in item_views("messages", &item) {
            row = row.view(view);
        }
        if !offers_reasoning
            && message
                .work
                .as_ref()
                .and_then(|work| work.stream.as_ref())
                .is_some_and(|stream| !stream.reasoning.is_empty())
        {
            row = row.effect(&ChatEffect::ShowAgentReasoning {
                message: id.to_owned(),
                expanded: !self.expanded_reasoning.contains(id),
            });
        }
        Some(row)
    }

    fn person_row(&self, id: &str) -> Option<LiveRow> {
        let user = self.search_users.iter().find(|item| item.id == id)?;
        Some(LiveRow::new(
            item_row_id("people", id),
            "chat.workspace.people",
            crate::identity::shared_name_label(&user.id, &user.name),
        ))
    }

    fn report_row(&self, id: &str) -> Option<LiveRow> {
        let (_, text) = self.reports.iter().find(|(report, _)| report == id)?;
        Some(
            LiveRow::new(item_row_id("feedback", id), "chat.workspace.feedback", text)
                .effect(&ChatEffect::RevealMessage {
                    message: crate::feedback::message_id(id),
                })
                .operation(crate::feedback::receipt_read::spec(id))
                .operation(crate::feedback::status_read::spec(id)),
        )
    }

    /// Who wrote a message, as the room names them.
    fn author_name(&self, author: &str) -> String {
        self.agent_authors
            .get(author)
            .cloned()
            .unwrap_or_else(|| self.display_name(author))
    }

    /// What a message's row is called: who wrote it and how it begins.
    fn message_label(&self, message: &crate::model::Message) -> String {
        let author = if self.is_provider_output(message) {
            self.message_author_label(message)
        } else {
            self.author_name(&message.author)
        };
        let said = if message.text.is_empty() {
            message
                .attachment
                .as_ref()
                .map_or_else(String::new, |attachment| attachment.name().to_owned())
        } else {
            message.text.chars().take(80).collect()
        };
        format!("{author}: {said}")
    }
}

impl ChatApp {
    /// What runs when a search hit or an agent's `open` names an item of
    /// the chat: it is shown on the next sync, once the record is read.
    pub(crate) fn opener(&self) -> impl FnMut(&str) + 'static {
        let opened = std::rc::Rc::clone(&self.opened);
        move |anchor| opened.borrow_mut().push(anchor.to_owned())
    }

    /// Show the items asked for since the last sync.
    pub(crate) fn open_requested(&mut self) {
        let anchors = std::mem::take(&mut *self.opened.borrow_mut());
        for anchor in anchors {
            // An item gone since it was found shows nothing.
            if let Some(effect) = opening(&anchor) {
                let _ = self.answer_effect(effect);
            }
        }
    }
}

/// The view effect that shows the item `anchor` names.
fn opening(anchor: &str) -> Option<ChatEffect> {
    let (field, item) = tilo_resource::items::item_of_resource::<Chat>(anchor)?;
    let id = item.as_str().to_owned();
    Some(match field {
        "channels" => ChatEffect::OpenChannel { channel: id },
        "categories" => ChatEffect::OpenCategory { category: id },
        "messages" => ChatEffect::RevealMessage { message: id },
        "people" => ChatEffect::OpenPerson { user: id },
        "feedback" => ChatEffect::RevealMessage {
            message: crate::feedback::message_id(&id),
        },
        _ => return None,
    })
}

/// The call command a call-control effect asks of the channel it names;
/// none for any other effect.
fn voice_command_of(effect: ChatEffect) -> Option<(String, Command)> {
    Some(match effect {
        ChatEffect::JoinCall { channel } => {
            let channel_id = channel.clone();
            (channel, Command::Join { channel_id })
        }
        ChatEffect::LeaveCall { channel } => (channel, Command::Leave),
        ChatEffect::Mute { channel } => (channel, Command::SetMuted { muted: true }),
        ChatEffect::Unmute { channel } => (channel, Command::SetMuted { muted: false }),
        ChatEffect::Deafen { channel } => (channel, Command::SetDeafened { deafened: true }),
        ChatEffect::Undeafen { channel } => (channel, Command::SetDeafened { deafened: false }),
        ChatEffect::CameraOn { channel } => (channel, Command::SetCamera { on: true }),
        ChatEffect::CameraOff { channel } => (channel, Command::SetCamera { on: false }),
        ChatEffect::ShareScreen { channel } => (channel, Command::SetScreen { on: true }),
        ChatEffect::StopSharingScreen { channel } => (channel, Command::SetScreen { on: false }),
        _ => return None,
    })
}

#[cfg(test)]
mod audience_tests {
    use super::ChatEffect;
    use ui::tilo_resource::Effect;

    /// What an agent reads of a conversation, or the reasoning it folds, is
    /// not something a person looks for in search: those effects stay offered
    /// to agents and out of people's listings, while revealing a found
    /// message reaches both.
    #[test]
    fn agent_only_message_effects_stay_out_of_peoples_listings() {
        let declarations = ChatEffect::declarations();
        let for_people: Vec<&str> = declarations
            .iter()
            .filter(|decl| decl.spec(ChatEffect::OPERATION).reaches_people())
            .map(|decl| decl.id)
            .collect();
        for agent_only in [
            ChatEffect::READ_MESSAGE.id,
            ChatEffect::REVEAL_MESSAGE_RANGE.id,
            ChatEffect::SHOW_AGENT_REASONING.id,
        ] {
            assert!(
                !for_people.contains(&agent_only),
                "{agent_only} is offered to people: {for_people:?}"
            );
        }
        assert!(for_people.contains(&ChatEffect::REVEAL_MESSAGE.id));
        assert!(
            declarations
                .iter()
                .all(|decl| decl.spec(ChatEffect::OPERATION).reaches_agents()),
            "every Chat effect stays offered to agents"
        );
    }
}
