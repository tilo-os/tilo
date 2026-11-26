//! What Chat does that changes no document: showing a conversation, a
//! message or a person, reading a message as a value, sharing a file from
//! this device, and the call controls of the shell's voice service.
//!
//! The effects themselves are declared with the resource
//! ([`chat_model::effects`]), so a server that never draws Chat publishes the
//! same ones; this module answers them in the window. What an effect changes in
//! the document — the message a shared file becomes — it changes through the
//! resource's verbs and field edits. Sharing a file and the call controls reach
//! this device's files and voice service: outside effects, decided by the
//! person's answers like any other.

pub use chat_model::effects::{ChatEffect, RECENT_MESSAGES, item_row_id, item_views};

use ui::framework::{EffectAnswer, EffectStatus, Effected, LiveRow};
use ui::native_media::Command;
use ui::tilo_actions::ResourceViewSpec;
use ui::tilo_resource::{self, AbsolutePath};

use crate::app::{ChatApp, Focus};
use crate::model::ChannelKind;
use crate::resource::Chat;

impl ChatApp {
    /// Answer `effect` for this window.
    pub(crate) fn answer_effect(&mut self, effect: ChatEffect) -> EffectAnswer {
        // Showing a conversation or a message again ends showing one view alone.
        if matches!(
            effect,
            ChatEffect::OpenChannel { .. }
                | ChatEffect::OpenCategory { .. }
                | ChatEffect::RevealMessage { .. }
                | ChatEffect::RevealMessageRange { .. }
        ) {
            self.alone = None;
        }
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
            ChatEffect::ShowView { message } => self.show_view_alone(&message)?,
            ChatEffect::RevealMessageRange {
                message,
                start,
                end,
                unit,
            } => return self.reveal_message_range(&message, start, end, &unit),
            // A direct conversation is read and written in the record and the
            // parts, which the effect context holds: one invoked without it
            // cannot answer.
            ChatEffect::OpenDirectConversation { .. }
            | ChatEffect::OpenAgent { .. }
            | ChatEffect::SetUpPersonalAssistant
            | ChatEffect::SendDirectMessage { .. }
            | ChatEffect::Send { .. } => {
                return Err("a direct conversation answers where the chat is held".into());
            }
            ChatEffect::AttachFile { channel, path } => {
                self.attach_file_effect(&channel, path.as_ref())?;
            }
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
            ChatEffect::ExportContent { .. } => {
                return Err("the resource answers this read".into());
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
            self.expanded_agent.insert(message);
        } else {
            self.expanded_agent.remove(&message);
        }
        Ok(())
    }

    pub(crate) fn attach_file_effect(
        &mut self,
        channel: &str,
        path: Option<&AbsolutePath>,
    ) -> Result<(), String> {
        if !self.channels.iter().any(|item| item.id == channel) {
            return Err("conversation no longer exists".into());
        }
        // No path: the person chooses the files, from their bytes, in the
        // conversation they go to; a browser page has no paths but this.
        let Some(path) = path else {
            if !self.can_participate() {
                return Err("you can't post in this conversation".into());
            }
            self.open_channel(channel)?;
            self.choose_device_files();
            return Ok(());
        };
        if !self.external_services {
            return Err(
                "this device has no file paths; leave out the path to choose the files".into(),
            );
        }
        if !self.begin_attachment_import_into(channel, path) {
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

    /// Start a call in the workspace: join its voice channel, making one first
    /// when it has none so a text-only workspace can still huddle.
    /// A call already running is revealed, never closed. The channel is
    /// made through `doc` at once when the caller holds the document (the
    /// header button), else through the dispatcher, and joined once it is
    /// there.
    pub(crate) fn start_call(
        &mut self,
        doc: Option<&mut ui::widget::Edits<'_>>,
    ) -> Result<(), String> {
        if self.refused_call.is_none()
            && let Some(reason) = self.call_refusal()
        {
            // Said on the call's card, on the workspace's voice channel if it has
            // one: no channel is made for a call that cannot start.
            let channel = self
                .ws_channels()
                .into_iter()
                .find(|channel| channel.kind == ChannelKind::Voice)
                .map(|channel| channel.id.clone())
                .unwrap_or_default();
            self.call_refused(&channel, reason);
            return Ok(());
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
                return Err(
                    "The workspace has no voice channel; its moderators make channels".into(),
                );
            }
            let name = ui::tilo_resource::NonBlank::parse("Voice")
                .ok_or_else(|| crate::i18n::t("chat-voice-channel-unnamed"))?;
            let Some(doc) = doc else {
                if self.dispatch.is_none() {
                    return Err("Chat is open to view only".into());
                }
                self.submit(crate::resource::Chat::CREATE_CHANNEL.with(
                    crate::chat::chat::CreateChannel {
                        name,
                        category: None,
                        kind: Some(ChannelKind::Voice),
                        topic: None,
                    },
                ));
                // The channel's id is the workspace's to mint: the call joins the
                // voice channel once it is in the document.
                self.pending_call = Some(String::new());
                return Ok(());
            };
            let made = self.create_channel_now(doc, "", name, ChannelKind::Voice);
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

    /// What a row says its state does not allow: a call's own controls are
    /// offered only inside the call they belong to, so outside it each says
    /// why, and the palette and menus leave them out while an agent is told
    /// to join first.
    pub(crate) fn row_status(&self, id: &str) -> Vec<EffectStatus> {
        let Some(conversation) = id.strip_suffix("/call") else {
            return Vec::new();
        };
        let Some(("channels", channel)) =
            tilo_resource::items::item_of_resource::<Chat>(conversation)
        else {
            return Vec::new();
        };
        if self.call.active && self.call.channel == channel.as_str() {
            return Vec::new();
        }
        let reason = crate::i18n::t("chat-call-not-joined");
        let reason = reason.as_str();
        [
            ChatEffect::LEAVE_CALL,
            ChatEffect::MUTE,
            ChatEffect::UNMUTE,
            ChatEffect::DEAFEN,
            ChatEffect::UNDEAFEN,
            ChatEffect::CAMERA_ON,
            ChatEffect::CAMERA_OFF,
            ChatEffect::SHARE_SCREEN,
            ChatEffect::STOP_SHARING_SCREEN,
        ]
        .map(|effect| EffectStatus::unavailable(effect, "not_in_call", reason).on(id))
        .to_vec()
    }

    /// The effects this window cannot run right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut unavailable = Vec::new();
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

    /// Show the conversation `channel`: a channel the sidebar lists, or a direct conversation
    /// (with a person or an agent) that is one by its id, as the window a send floats opens on.
    fn open_channel(&mut self, channel: &str) -> Result<(), String> {
        if !self.is_conversation(channel) {
            return Err("conversation no longer exists".into());
        }
        channel.clone_into(&mut self.active_channel);
        self.thread_open = None;
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        self.reset_timeline_to_latest();
        self.focus = Focus::Main;
        // The conversation is what this window shows now: a sidebar floating over it as a drawer
        // (a narrow window, a card a send floated) goes away rather than covering it.
        if let Some(bar) = &self.bar {
            bar.put_drawer_away();
        }
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
        // A message no held part has may be in an earlier one: revealed
        // once the parts before are opened and one has it.
        if !self.search_messages.iter().any(|item| item.id == message) && self.seek_earlier(message)
        {
            return Ok(());
        }
        self.reveal_held_message(message)
    }

    /// Reveal `message`, which a held part has.
    pub(crate) fn reveal_held_message(&mut self, message: &str) -> Result<(), String> {
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
}

impl ChatApp {
    /// The effects offered on what the window shows: each category and
    /// channel (a voice channel with its call), the latest
    /// messages of every conversation and the reviewed reports.
    /// They are rows merged into the item resources the record publishes.
    pub(crate) fn published_rows(&self) -> Vec<LiveRow> {
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
        let reports = self
            .reports
            .iter()
            .filter_map(|(report, _)| self.report_row(report));
        channels
            .chain(messages)
            .chain(reports)
            .chain(self.direct_conversation_rows())
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
            "channels" => self.channel_row(item),
            "messages" => self.message_row(item),
            "feedback" => self.report_row(item),
            _ => None,
        }
    }

    fn channel_row(&self, id: &str) -> Option<LiveRow> {
        // Only a conversation the resource holds is an item with effects on it:
        // a conversation with a person neither has written in yet is derived
        // from the workspace's people, and its row is
        // [`Self::direct_conversation_rows`]'.
        if !self.recorded_channels.contains(id) {
            return None;
        }
        let channel = self.channels.iter().find(|item| item.id == id)?;
        let mut bindings = ui::tilo_resource::RowBindings::new();
        // A channel with no model behind it offers no command to it.
        if !self.model_controls(id).is_empty() {
            bindings = bindings.bind(
                ChatEffect::MODEL_CONTROL,
                [("channel", id.to_owned().into())],
            );
        }
        let row = LiveRow::typed_with::<chat_model::rows::Channel>(
            item_row_id("channels", id),
            &channel.name,
            &bindings,
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
        Some(if channel.kind == ChannelKind::Voice {
            // Opening the conversation is the channel's: a view here would be
            // the call row's disclosure, a private one that keeps joining and
            // every call control from an agent. A control outside the call
            // says so through the window's row status.
            let channel_id = id.to_owned();
            let mut bindings = ui::tilo_resource::RowBindings::new();
            for effect in [
                ChatEffect::JOIN_CALL,
                ChatEffect::LEAVE_CALL,
                ChatEffect::MUTE,
                ChatEffect::UNMUTE,
                ChatEffect::DEAFEN,
                ChatEffect::UNDEAFEN,
                ChatEffect::CAMERA_ON,
                ChatEffect::CAMERA_OFF,
                ChatEffect::SHARE_SCREEN,
                ChatEffect::STOP_SHARING_SCREEN,
                ChatEffect::CALL_STATUS,
            ] {
                bindings = bindings.bind(effect, [("channel", channel_id.clone().into())]);
            }
            row.row(LiveRow::typed_with::<chat_model::rows::Call>(
                format!("{}/call", item_row_id("channels", id)),
                crate::i18n::t_args(
                    "chat-call-title",
                    &[("channel", channel.name.clone().into())],
                ),
                &bindings,
            ))
        } else {
            row
        })
    }

    fn message_row(&self, id: &str) -> Option<LiveRow> {
        let message = self.search_messages.iter().find(|item| item.id == id)?;
        // The badge beside the detail says the author is software; its name is its own.
        let author = self.message_author_label(message);
        let agent = crate::model_stream::is_provider_output(message) || !message.author.is_person();
        // The chat keeps its messages in its parts, so the window publishes
        // each as a row of the item type, offering the message's own effects
        // bound to it: reveal it, show a range of it, open what it carries.
        let message_id = || id.to_owned();
        let bindings = ui::tilo_resource::RowBindings::new()
            .bind(
                ChatEffect::REVEAL_MESSAGE,
                [("message", message_id().into())],
            )
            .bind(
                ChatEffect::REVEAL_MESSAGE_RANGE,
                [("message", message_id().into())],
            )
            .bind(ChatEffect::SHOW_VIEW, [("message", message_id().into())])
            .bind(
                ChatEffect::SHOW_AGENT_REASONING,
                [
                    (
                        "message",
                        ui::tilo_resource::__private::serde_json::Value::from(message_id()),
                    ),
                    (
                        "expanded",
                        ui::tilo_resource::__private::serde_json::Value::Bool(
                            !self.expanded_agent.contains(id),
                        ),
                    ),
                ],
            );
        let mut row = LiveRow::typed_with::<chat_model::rows::Message>(
            item_row_id("messages", id),
            self.message_label(message),
            &bindings,
        )
        // Who wrote it, beside the message's own words.
        .presentation(ui::tilo_actions::ResourcePresentation {
            detail: Some(author.chars().take(80).collect()),
            badge: agent.then(|| "agent".to_owned()),
            preview: None,
        });
        let item = tilo_resource::Id::from_raw(id);
        for view in item_views("messages", &item) {
            row = row.view(view);
        }
        // A message that carries a view opens in it too, alone.
        if message.view.is_some() {
            row = row.view(ResourceViewSpec::new(
                "view",
                "View",
                ChatEffect::SHOW_VIEW.id,
            ));
        }
        Some(row)
    }

    fn report_row(&self, id: &str) -> Option<LiveRow> {
        let (_, text) = self.reports.iter().find(|(report, _)| report == id)?;
        let message = crate::feedback::message_id(id);
        let mut bindings = ui::tilo_resource::RowBindings::new().bind(
            ChatEffect::REVEAL_MESSAGE,
            [("message", message.clone().into())],
        );
        // The launch replaying what a report recorded, an action its own
        // resource declares and the report offers bound to this recording.
        if let Some(recording) = self.report_recordings.get(message.as_str()) {
            bindings = bindings.spec(crate::feedback::replay_spec(recording));
        }
        Some(LiveRow::typed_with::<chat_model::rows::Report>(
            item_row_id("feedback", id),
            text,
            &bindings,
        ))
    }

    /// The reads the window answers for one of its items, from the chat's
    /// committed state: a conversation's destination, a report's receipt and
    /// status. They are the app's registered readers, published on the item
    /// the window knows.
    pub(crate) fn item_reads(field: &str, id: &str) -> Vec<ui::semantic_document_read::Declared> {
        match field {
            "channels" => vec![crate::feedback::destination_read::spec(id)],
            "feedback" => vec![
                crate::feedback::receipt_read::spec(id),
                crate::feedback::status_read::spec(id),
            ],
            _ => Vec::new(),
        }
    }

    /// What a message's row is called: who wrote it and how it begins.
    pub(crate) fn message_label(&self, message: &crate::model::Message) -> String {
        let author = self.message_author_label(message);
        let said = if message.text.is_empty() {
            message
                .files()
                .next()
                .map_or_else(String::new, |file| file.label().to_owned())
        } else {
            message.text.chars().take(80).collect()
        };
        format!("{author}: {said}")
    }
}

impl ChatApp {
    /// What runs when a search hit or an agent's `open` names an item of
    /// the chat: it is shown on the next sync, once the resource is read.
    pub(crate) fn opener(&self) -> impl FnMut(&str) + 'static {
        let opened = std::rc::Rc::clone(&self.opened);
        move |anchor| opened.borrow_mut().push(anchor.to_owned())
    }

    /// Show the items asked for since the last sync, on the chat `doc` holds: the conversation
    /// with an agent is read from it, as the `open-agent` action reads it.
    ///
    /// An item the chat does not hold yet is asked for again at the next sync, as an item a
    /// launch names is: a notice of a message is read from what the workspace holds, which is no
    /// promise that this window has taken that part in, and an item dropped here would never show.
    pub(crate) fn open_requested(&mut self, doc: &automerge::AutoCommit) {
        let asked = std::mem::take(&mut *self.opened.borrow_mut());
        let mut waiting = Vec::new();
        for anchor in asked {
            if !self.show_opened(&anchor, doc) {
                waiting.push(anchor);
            }
        }
        if waiting.is_empty() {
            return;
        }
        // What was asked for meanwhile stands after them: items show in the order they were asked
        // for, however many syncs the first needs.
        let mut opened = self.opened.borrow_mut();
        waiting.append(&mut opened);
        *opened = waiting;
    }

    /// Show the item `anchor` names, answering whether the window has it: `false` while the chat
    /// does not hold it yet — a message whose part has not arrived, a conversation it has not
    /// listed — and it is to be asked for again.
    fn show_opened(&mut self, anchor: &str, doc: &automerge::AutoCommit) -> bool {
        match opening(anchor) {
            Some(ChatEffect::OpenAgent { agent }) => {
                if let Ok(chat) = <crate::chat::Chat as tilo_resource::Resource>::read(doc) {
                    self.open_agent(&agent, &chat);
                }
                true
            }
            Some(effect) => self.answer_effect(effect).is_ok(),
            // An item no view of the chat shows.
            None => true,
        }
    }
}

/// The view effect that shows the item `anchor` names.
fn opening(anchor: &str) -> Option<ChatEffect> {
    // An agent is an item of the workspace's own document of agents, not of
    // the chat's: search finds it there, and it opens the conversation it is
    // in here.
    if let Some((_, agent)) =
        tilo_resource::items::item_of_resource::<ui::tilo_workspace_agent::WorkspaceAgents>(anchor)
    {
        return Some(ChatEffect::OpenAgent {
            agent: agent.as_str().to_owned(),
        });
    }
    let (field, item) = tilo_resource::items::item_of_resource::<Chat>(anchor)?;
    let id = item.as_str().to_owned();
    Some(match field {
        "channels" => ChatEffect::OpenChannel { channel: id },
        "categories" => ChatEffect::OpenCategory { category: id },
        "messages" => ChatEffect::RevealMessage { message: id },
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
mod opening_tests {
    use super::*;
    use ui::tilo_resource::Id;
    use ui::tilo_resource::items::item_resource_id;

    /// What a hit of search opens: an agent its conversation with it, a
    /// conversation itself, a message the place it was said, and a person
    /// their conversation.
    #[test]
    fn a_hit_opens_the_place_it_names() {
        let agent = item_resource_id::<ui::tilo_workspace_agent::WorkspaceAgents>(
            "agents",
            &Id::from_raw("a_ada"),
        );
        assert_eq!(
            opening(&agent),
            Some(ChatEffect::OpenAgent {
                agent: "a_ada".to_owned()
            })
        );
        let conversation = item_resource_id::<Chat>("channels", &Id::from_raw("c_crit"));
        assert_eq!(
            opening(&conversation),
            Some(ChatEffect::OpenChannel {
                channel: "c_crit".to_owned()
            })
        );
        let message = item_resource_id::<Chat>("messages", &Id::from_raw("m_wheel"));
        assert_eq!(
            opening(&message),
            Some(ChatEffect::RevealMessage {
                message: "m_wheel".to_owned()
            })
        );
        assert_eq!(opening("chat.workspace"), None);
    }
}

#[cfg(test)]
mod audience_tests {
    use super::ChatEffect;
    use ui::tilo_resource::Effect;

    /// Where an agent reveals a message, or the reasoning it folds, is
    /// not something a person looks for in search: those effects stay offered
    /// to agents and out of people's listings, while revealing a found
    /// message reaches both.
    #[test]
    fn agent_only_message_effects_stay_out_of_peoples_listings() {
        let declarations = ChatEffect::declarations();
        let for_people: Vec<&str> = declarations
            .iter()
            .filter(|decl| decl.spec_of::<ChatEffect>().reaches_people())
            .map(|decl| decl.id)
            .collect();
        for agent_only in [
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
                .all(|decl| decl.spec_of::<ChatEffect>().reaches_agents()),
            "every Chat effect stays offered to agents"
        );
    }
}
