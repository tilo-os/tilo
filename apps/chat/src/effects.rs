//! What Chat does that changes no document: showing a conversation, a
//! message or a person, reading a message as a value, sharing a file from
//! this device, and the call controls of the shell's voice service.
//!
//! Each is a [`ChatEffect`], offered bound on the item it acts on (a
//! conversation, a category, a message, a call), so the palette, an agent
//! or a script reaches the same handler a click does. What an effect changes
//! in the document — the message a shared file becomes — it changes through
//! the record's verbs and field edits. Sharing a file and
//! the call controls reach this device's files and voice service, so only
//! the person at the device runs them.

use ui::framework::{EffectAnswer, EffectStatus, Effected, LiveRow};
use ui::native_media::Command;
use ui::tilo_actions::{ResourceSemanticIdentity, ResourceViewRangeSpec, ResourceViewSpec};
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

/// Stable semantic key of one authenticated Agent collaborator in Chat.
pub(crate) fn agent_row_id(agent: &str) -> String {
    format!("chat.agent:{agent}")
}

fn agent_semantic_identity(agent: &str) -> Option<ResourceSemanticIdentity> {
    ResourceSemanticIdentity::new("chat.agent", agent_row_id(agent))
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
    /// Ask the device's model about the latest message.
    #[effect(device_only, row, returns, item = channels)]
    ModelControl {
        channel: String,
        #[param("Model command", choices = ["ask", "stop", "retry", "continue"])]
        command: String,
    },
    /// Show or hide reasoning actually supplied by the provider.
    #[effect(view, row, item = messages)]
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
    #[effect(view, row, returns, item = messages)]
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
    #[effect(view, device_only)]
    OpenDirectConversation { public_key: String },
    /// Expand an unsent private editor using its ephemeral native handoff token.
    #[effect(view, device_only, returns)]
    AdoptPrivateDraft { token: String },
    /// Send privately through the native person-only host door.
    #[effect(device_only, returns)]
    SendDirectMessage { public_key: String, text: String },
    /// Read a private conversation on this device.
    #[effect(device_only, returns)]
    ReadDirectConversation { public_key: String },
    /// Open a direct conversation with an Agent authenticated by this room.
    #[effect(view, row)]
    OpenAgentConversation { agent: String },
    /// Open the conversation for one exact task and run.
    ///
    /// The Agent row binds its authenticated agent, asking only for task and
    /// run epoch; the root also supports exact navigation.
    #[effect(view)]
    OpenTaskConversation {
        agent: String,
        task: String,
        run_epoch: u64,
    },
    /// Start task
    ///
    /// Saving a message never invokes this action: the caller must consciously
    /// approve starting the selected task in the open Agent conversation.
    #[effect(device_only)]
    StartAgentTask,
    /// Stop task
    ///
    /// Requests a stop for the selected exact run; completed changes are not undone.
    #[effect(device_only)]
    StopAgentTask,
    /// New task
    ///
    /// Opens a private goal composer without starting execution.
    #[effect(device_only)]
    NewTask,
    /// Add subtask
    ///
    /// Opens a private goal under an existing parent; creates nothing yet.
    #[effect(device_only)]
    NewSubtaskGoal { task: String },
    /// Edit private goal
    ///
    /// Replaces only this device's pending private task goal.
    #[effect(device_only)]
    SetTaskGoal { text: String },
    /// Save private goal
    ///
    /// Saves the pending goal as queued work. Never starts execution.
    #[effect(device_only)]
    SaveTaskGoal,
    /// Open output
    ///
    /// Opens only a retained receipt's exact registered resource view.
    #[effect(device_only)]
    OpenTaskOutput {
        task: String,
        run_epoch: u64,
        invocation: String,
    },
    /// Open task settings
    ///
    /// Opens model or worker configuration; never grants task authority.
    #[effect(device_only)]
    SetupTaskModels,
    /// Filter tasks
    ///
    /// Changes only this device's task list.
    #[effect(device_only)]
    FilterTasks { filter: String },
    /// Find task
    ///
    /// Narrows tasks by public titles and labels, never private goals.
    #[effect(device_only)]
    QueryTasks { query: String, label: String },
    /// Expand or collapse subtasks
    ///
    /// Changes only this device's hierarchy navigation.
    #[effect(device_only)]
    ToggleTaskChildren { task: String },
    /// Add subtask with public title
    ///
    /// Adds a queued child. Its title is public, not a private goal.
    #[effect(device_only)]
    CreateSubtask { task: String, title: String },
    /// Read message
    ///
    /// The message as a value: who wrote it, whether that is an agent, its
    /// whole text and the agent's record of its work.
    #[effect(view, row, returns, item = messages)]
    ReadMessage { message: String },
    /// Read room
    ///
    /// The room's categories and channels and its people as the record
    /// stores them, and, when asked, one message with the message it
    /// answers or a channel's latest messages: what a bot mirroring the
    /// room elsewhere works from.
    #[effect(kind = "read", returns, programmatic)]
    ReadRoom {
        #[param("A message to include, with the message it answers")]
        message: Option<String>,
        #[param("A channel whose latest messages to include, each with the message it answers")]
        channel: Option<String>,
    },
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
    /// Joins the room's voice channel, making one when the room has none,
    /// or reveals the call already running.
    #[effect(idempotent, device_only)]
    StartCall,
    /// Start a video call
    ///
    /// Refused: calls carry microphone audio only.
    #[effect(idempotent, device_only)]
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

/// Typed dispatch for private task goal editing.
enum TaskGoalEffect {
    NewTask,
    NewSubtaskGoal { task: String },
    SetTaskGoal { text: String },
    SaveTaskGoal,
}

/// Typed dispatch for task navigation and execution.
enum TaskEffect {
    OpenTaskConversation {
        agent: String,
        task: String,
        run_epoch: u64,
    },
    OpenAgentConversation {
        agent: String,
    },
    OpenTaskOutput {
        task: String,
        run_epoch: u64,
        invocation: String,
    },
    SetupTaskModels,
    QueryTasks {
        query: String,
        label: String,
    },
    FilterTasks {
        filter: String,
    },
    ToggleTaskChildren {
        task: String,
    },
    CreateSubtask {
        task: String,
        title: String,
    },
    StartAgentTask,
}

/// Typed dispatch for native call controls.
enum CallEffect {
    JoinCall { channel: String },
    LeaveCall { channel: String },
    Mute { channel: String },
    Unmute { channel: String },
    CallStatus { channel: String },
    StartCall,
    StartVideoCall,
    SelectInput { channel: String, input: String },
}

impl ChatApp {
    pub(crate) fn open_task_conversation_effect(
        &mut self,
        agent: &str,
        task: &str,
        run_epoch: u64,
    ) -> Result<(), String> {
        // Reading a historical run must not depend on its executor still being
        // registered. The owner authenticates this exact task/run read; steering
        // and Stop separately require its live assignment and revision.
        if !self.open_agent_conversation(agent) {
            return Err("the task's agent is not in this room".into());
        }
        if !self
            .agent_tasks
            .follow(crate::app::ROOM_SCOPE, agent, task, run_epoch)
        {
            return Err("the exact task identity is invalid".into());
        }
        self.agent_intents.update(|intents| {
            _ = intents.insert(
                (crate::app::ROOM_SCOPE.to_owned(), agent.to_owned()),
                crate::agent_tasks::AgentComposerIntent::Steer,
            );
        });
        self.sync_task_channel();
        self.sync_agent_route();
        let channel = self.active_channel.clone();
        self.restore_attached_agent_surface(&channel);
        Ok(())
    }

    /// Answer `effect` for this window.
    pub(crate) fn answer_effect(&mut self, effect: ChatEffect) -> EffectAnswer {
        match effect {
            ChatEffect::ModelControl { channel, command } => {
                self.model_command(&channel, &command).map(Effected::value)
            }
            ChatEffect::ShowAgentReasoning { message, expanded } => {
                self.show_agent_reasoning(message, expanded)?;
                Ok(Effected::done())
            }
            ChatEffect::OpenChannel { channel } => {
                self.open_channel(&channel).map(|()| Effected::done())
            }
            ChatEffect::OpenCategory { category } => {
                self.reveal_category(&category).map(|()| Effected::done())
            }
            ChatEffect::RevealMessage { message } => {
                self.reveal_message(&message).map(|()| Effected::done())
            }
            ChatEffect::ToggleCategory { category } => self.toggle_category_effect(&category),
            ChatEffect::RevealMessageRange {
                message,
                start,
                end,
                unit,
            } => self.reveal_message_range(&message, start, end, &unit),
            ChatEffect::OpenDirectConversation { public_key } => self
                .open_person_conversation(&public_key)
                .map(|()| Effected::done()),
            ChatEffect::AdoptPrivateDraft { .. } => {
                Err("private draft adoption requires its original effect context".into())
            }
            ChatEffect::SendDirectMessage { public_key, text } => {
                self.send_person_message(&public_key, &text)
            }
            ChatEffect::ReadDirectConversation { public_key } => {
                self.read_person_conversation(&public_key)
            }
            ChatEffect::OpenPerson { user } => self.open_person(&user).map(|()| Effected::done()),
            ChatEffect::NewTask => self.answer_task_goal_effect(TaskGoalEffect::NewTask),
            ChatEffect::NewSubtaskGoal { task } => {
                self.answer_task_goal_effect(TaskGoalEffect::NewSubtaskGoal { task })
            }
            ChatEffect::SetTaskGoal { text } => {
                self.answer_task_goal_effect(TaskGoalEffect::SetTaskGoal { text })
            }
            ChatEffect::SaveTaskGoal => self.answer_task_goal_effect(TaskGoalEffect::SaveTaskGoal),
            ChatEffect::OpenTaskConversation {
                agent,
                task,
                run_epoch,
            } => self.answer_task_conversation(agent, task, run_epoch),
            ChatEffect::OpenAgentConversation { agent } => {
                self.answer_task_effect(TaskEffect::OpenAgentConversation { agent })
            }
            ChatEffect::OpenTaskOutput {
                task,
                run_epoch,
                invocation,
            } => self.answer_task_output(task, run_epoch, invocation),
            ChatEffect::SetupTaskModels => self.answer_task_effect(TaskEffect::SetupTaskModels),
            ChatEffect::QueryTasks { query, label } => {
                self.answer_task_effect(TaskEffect::QueryTasks { query, label })
            }
            ChatEffect::FilterTasks { filter } => {
                self.answer_task_effect(TaskEffect::FilterTasks { filter })
            }
            ChatEffect::ToggleTaskChildren { task } => {
                self.answer_task_effect(TaskEffect::ToggleTaskChildren { task })
            }
            ChatEffect::CreateSubtask { task, title } => {
                self.answer_task_effect(TaskEffect::CreateSubtask { task, title })
            }
            ChatEffect::StartAgentTask => self.answer_task_effect(TaskEffect::StartAgentTask),
            ChatEffect::StopAgentTask => {
                if !self.stop_agent_task() {
                    return Err(crate::i18n::t("chat-agent-stop-unavailable"));
                }
                self.request_relayout();
                Ok(Effected::done())
            }
            ChatEffect::ReadMessage { message } => self.read_message(&message).map(Effected::value),
            ChatEffect::AttachFile { channel, path } => self
                .attach_file_effect(&channel, &path)
                .map(|()| Effected::done()),
            ChatEffect::ShareLiveContext => self.share_live_context(),
            ChatEffect::StopSharingLiveContext => Ok(self.set_context_sharing(false)),
            ChatEffect::LiveContextStatus => Ok(self.live_context_status()),
            ChatEffect::ReadRoom { .. } => Err("the record answers this read".into()),
            ChatEffect::OpenSlackChannel { .. } | ChatEffect::OpenDiscordChannel { .. } => {
                Err("Browser navigation requires the owning host".into())
            }
            ChatEffect::JoinCall { channel } => {
                self.answer_call_effect(CallEffect::JoinCall { channel })
            }
            ChatEffect::LeaveCall { channel } => {
                self.answer_call_effect(CallEffect::LeaveCall { channel })
            }
            ChatEffect::Mute { channel } => self.answer_call_effect(CallEffect::Mute { channel }),
            ChatEffect::Unmute { channel } => {
                self.answer_call_effect(CallEffect::Unmute { channel })
            }
            ChatEffect::CallStatus { channel } => {
                self.answer_call_effect(CallEffect::CallStatus { channel })
            }
            ChatEffect::StartCall => self.answer_call_effect(CallEffect::StartCall),
            ChatEffect::StartVideoCall => self.answer_call_effect(CallEffect::StartVideoCall),
            ChatEffect::SelectInput { channel, input } => {
                self.answer_call_effect(CallEffect::SelectInput { channel, input })
            }
        }
    }

    fn toggle_category_effect(&mut self, category: &str) -> EffectAnswer {
        if !self.categories.iter().any(|item| item.id == category) {
            return Err("category no longer exists".into());
        }
        self.toggle_category(category);
        Ok(Effected::done())
    }

    fn answer_task_conversation(
        &mut self,
        agent: String,
        task: String,
        run_epoch: u64,
    ) -> EffectAnswer {
        self.answer_task_effect(TaskEffect::OpenTaskConversation {
            agent,
            task,
            run_epoch,
        })
    }

    fn answer_task_output(
        &mut self,
        task: String,
        run_epoch: u64,
        invocation: String,
    ) -> EffectAnswer {
        self.answer_task_effect(TaskEffect::OpenTaskOutput {
            task,
            run_epoch,
            invocation,
        })
    }

    fn live_context_status(&self) -> Effected {
        Effected::value(serde_json::json!({
            "sharing": self.sharing_context,
            "following": self.following_context,
            "channel": self.active_channel,
            "videoCapture": false,
        }))
    }

    fn share_live_context(&mut self) -> EffectAnswer {
        self.set_live_context_sharing(true)?;
        Ok(Effected::done())
    }

    fn set_context_sharing(&mut self, sharing: bool) -> Effected {
        self.sharing_context = sharing;
        self.request_relayout();
        Effected::done()
    }

    fn answer_task_goal_effect(&mut self, effect: TaskGoalEffect) -> EffectAnswer {
        match effect {
            TaskGoalEffect::NewTask => self.begin_task_goal(None),
            TaskGoalEffect::NewSubtaskGoal { task } => {
                if !self
                    .agent_tasks
                    .all_tasks()
                    .iter()
                    .any(|known| known.task_id == task)
                {
                    return Err(crate::i18n::t("chat-task-save-refused"));
                }
                self.begin_task_goal(Some(task));
            }
            TaskGoalEffect::SetTaskGoal { text } => {
                let goal = self
                    .task_goal
                    .as_ref()
                    .filter(|goal| !goal.saving)
                    .ok_or_else(|| crate::i18n::t("chat-task-save-refused"))?;
                self.composer_main.set_conversation_text(&goal.key(), &text);
                self.draft_main = text;
                self.request_relayout();
            }
            TaskGoalEffect::SaveTaskGoal => {
                if !self.save_task_goal() {
                    return Err(crate::i18n::t("chat-task-save-refused"));
                }
            }
        }
        Ok(Effected::done())
    }

    fn answer_task_effect(&mut self, effect: TaskEffect) -> EffectAnswer {
        match effect {
            TaskEffect::OpenTaskConversation {
                agent,
                task,
                run_epoch,
            } => {
                self.open_task_conversation_effect(&agent, &task, run_epoch)?;
            }
            TaskEffect::OpenAgentConversation { agent } => {
                if !self.open_agent_conversation(&agent) {
                    return Err("agent is not admitted to this room".into());
                }
            }
            TaskEffect::OpenTaskOutput {
                task,
                run_epoch,
                invocation,
            } => {
                let output = self
                    .agent_tasks
                    .outputs()
                    .into_iter()
                    .find(|output| {
                        output.task_run.task_id == task
                            && output.task_run.run_epoch == run_epoch
                            && output.invocation == invocation
                    })
                    .cloned()
                    .ok_or_else(|| crate::i18n::t("chat-task-output-unavailable"))?;
                if !self.agent_tasks.open_output(&output) {
                    return Err(crate::i18n::t("chat-task-output-unavailable"));
                }
            }
            TaskEffect::SetupTaskModels => {
                if !self.agent_tasks.needs_setup() {
                    return Err(crate::i18n::t("chat-agent-start-unavailable"));
                }
                ui::widget::try_open_external_url("settings://agent_models")
                    .map_err(|error| error.to_string())?;
            }
            TaskEffect::QueryTasks { query, label } => {
                self.task_search.set_query(&query);
                self.task_label = label;
                self.request_relayout();
            }
            TaskEffect::FilterTasks { filter } => {
                if !["all", "active", "completed", "mine"].contains(&filter.as_str()) {
                    return Err("Unknown task filter".into());
                }
                self.task_filter = filter;
                self.request_relayout();
            }
            TaskEffect::ToggleTaskChildren { task } => {
                if !self
                    .agent_tasks
                    .all_tasks()
                    .iter()
                    .any(|known| known.task_id == task)
                {
                    return Err("Task is no longer available".into());
                }
                self.toggle_task_children(&task);
            }
            TaskEffect::CreateSubtask { task, title } => {
                let frontier = self
                    .agent_tasks
                    .all_tasks()
                    .into_iter()
                    .find(|known| known.task_id == task)
                    .cloned()
                    .ok_or("Task is no longer available")?;
                if !self.select_agent_task(&frontier)
                    || !self
                        .agent_tasks
                        .create_subtask(&frontier.assigned_agent, title)
                {
                    return Err("Subtask could not be created".into());
                }
            }
            TaskEffect::StartAgentTask => {
                if !self.start_agent_task() {
                    return Err(crate::i18n::t("chat-agent-start-unavailable"));
                }
                self.request_relayout();
            }
        }
        Ok(Effected::done())
    }

    fn answer_call_effect(&mut self, effect: CallEffect) -> EffectAnswer {
        match effect {
            CallEffect::JoinCall { channel } => {
                let channel_id = channel.clone();
                self.native_voice_command(&channel, Command::Join { channel_id })?;
            }
            CallEffect::LeaveCall { channel } => {
                self.native_voice_command(&channel, Command::Leave)?;
            }
            CallEffect::Mute { channel } => {
                self.native_voice_command(&channel, Command::SetMuted { muted: true })?;
            }
            CallEffect::Unmute { channel } => {
                self.native_voice_command(&channel, Command::SetMuted { muted: false })?;
            }
            CallEffect::CallStatus { channel } => return self.native_call_status(&channel),
            CallEffect::StartCall => self.start_call(None)?,
            CallEffect::StartVideoCall => return Err(video_call_unavailable_reason()),
            CallEffect::SelectInput { channel, input } => {
                self.select_voice_input(&channel, &input)?;
            }
        }
        Ok(Effected::done())
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
            if !self.can_participate() {
                return Err("A room writer must create a voice channel".into());
            }
            let Some(doc) = doc else {
                if self.dispatch.is_none() {
                    return Err("Chat is open to view only".into());
                }
                let id = crate::events::fresh_id("c");
                let channel = crate::record::Channel {
                    name: "Voice".into(),
                    kind: ChannelKind::Voice,
                    ..crate::record::Channel::default()
                };
                self.submit(crate::resource::Chat::paths().channels.insert_as(
                    &tilo_resource::Id::from_raw(id.clone()),
                    self.channels.len(),
                    &channel,
                ));
                self.pending_call = Some(id);
                return Ok(());
            };
            self.create_channel_now(doc, "", "Voice", ChannelKind::Voice)
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
        let mut unavailable = vec![video];
        if !self
            .direct_agent_tasks()
            .is_some_and(crate::agent_tasks::AgentTaskView::can_stop)
        {
            unavailable.push(EffectStatus::unavailable(
                ChatEffect::STOP_AGENT_TASK,
                "task_run_not_current",
                crate::i18n::t("chat-agent-stop-unavailable"),
            ));
        }
        if !self.agent_tasks.needs_setup() {
            unavailable.push(EffectStatus::unavailable(
                ChatEffect::SETUP_TASK_MODELS,
                "model_setup_not_required",
                crate::i18n::t("chat-agent-start-unavailable"),
            ));
        }
        if self.task_goal.as_ref().is_none_or(|goal| goal.saving) {
            unavailable.push(EffectStatus::unavailable(
                ChatEffect::SAVE_TASK_GOAL,
                "no_private_goal_draft",
                crate::i18n::t("chat-task-save-refused"),
            ));
        }
        if self.task_goal.as_ref().is_none_or(|goal| goal.saving) {
            unavailable.push(EffectStatus::unavailable(
                ChatEffect::SET_TASK_GOAL,
                "no_private_goal_draft",
                crate::i18n::t("chat-task-save-refused"),
            ));
        }
        if !self.direct_agent_is_available()
            || !self
                .direct_agent_tasks()
                .is_some_and(super::agent_tasks::AgentTaskView::can_start)
        {
            unavailable.push(EffectStatus::unavailable(
                ChatEffect::START_AGENT_TASK,
                "task_not_ready",
                crate::i18n::t("chat-agent-start-unavailable"),
            ));
        }
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
                EffectStatus::unavailable(ChatEffect::JOIN_CALL, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::UNMUTE, "no_services", reason),
                EffectStatus::unavailable(ChatEffect::SELECT_INPUT, "no_services", reason),
            ]);
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
                        || message
                            .reactions
                            .iter()
                            .any(|reaction| reaction.users.iter().any(|id| id == user))
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

/// How many of a channel's latest messages [`read_room`] includes: a
/// post's change names its channel, not the message it made, so a bot finds
/// that message among the channel's newest.
const READ_LATEST: usize = 20;

/// The answer of [`ChatEffect::ReadRoom`]: the record's own JSON form of
/// the room's categories and channels, every person, and `message` and the
/// latest messages of `channel`, each with the message it answers.
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
    serde_json::json!({
        "categories": chat.categories.to_json(),
        "channels": chat.channels.to_json(),
        "people": chat.people.to_json(),
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
        let agents = self.agent_rows();
        let tasks = self.task_rows();
        let task_list = Self::task_list_row();
        let outputs = self.task_output_rows();
        outputs
            .chain(std::iter::once(task_list))
            .chain(categories)
            .chain(channels)
            .chain(messages)
            .chain(people)
            .chain(reports)
            .chain(agents)
            .chain(Self::person_conversation_rows())
            .chain(tasks)
            .collect()
    }

    fn agent_rows(&self) -> impl Iterator<Item = LiveRow> + '_ {
        self.room_agents.iter().filter_map(|agent| {
            let identity = agent_semantic_identity(agent)?;
            Some(
                LiveRow::new(
                    agent_row_id(agent),
                    "chat.agent",
                    self.agent_display_name(agent),
                )
                .semantic_identity(identity)
                .effect(&ChatEffect::OpenAgentConversation {
                    agent: agent.clone(),
                })
                .operation(
                    ChatEffect::OpenTaskConversation {
                        agent: agent.clone(),
                        task: String::new(),
                        run_epoch: 0,
                    }
                    .bound_asking(&["task", "run_epoch"]),
                ),
            )
        })
    }

    fn task_rows(&self) -> impl Iterator<Item = LiveRow> + '_ {
        self.agent_tasks.all_tasks().into_iter().filter_map(|task| {
            let agent = &task.assigned_agent;
            let epoch = task.run_epoch?;
            let id = format!("chat.task:{}:{epoch}", task.task_id);
            let identity = ui::tilo_actions::ResourceSemanticIdentity::new("chat.task", &id)?;
            Some(
                LiveRow::new(id, "chat.task", &task.title)
                    .semantic_identity(identity)
                    .effect(&ChatEffect::OpenTaskConversation {
                        agent: agent.to_owned(),
                        task: task.task_id.clone(),
                        run_epoch: epoch,
                    })
                    .effect(&ChatEffect::NewSubtaskGoal {
                        task: task.task_id.clone(),
                    })
                    .operation(
                        ChatEffect::CreateSubtask {
                            task: task.task_id.clone(),
                            title: String::new(),
                        }
                        .bound_asking(&["title"]),
                    )
                    .effect(&ChatEffect::ToggleTaskChildren {
                        task: task.task_id.clone(),
                    }),
            )
        })
    }

    fn task_list_row() -> LiveRow {
        LiveRow::new(
            "chat.tasks",
            "chat.task-list",
            crate::i18n::t("chat-agents-section"),
        )
        .effect(&ChatEffect::NewTask)
        .operation(
            ChatEffect::SetTaskGoal {
                text: String::new(),
            }
            .bound_asking(&["text"]),
        )
        .effect(&ChatEffect::SaveTaskGoal)
        .effect(&ChatEffect::StartAgentTask)
        .effect(&ChatEffect::StopAgentTask)
        .effect(&ChatEffect::SetupTaskModels)
        .operation(
            ChatEffect::QueryTasks {
                query: String::new(),
                label: String::new(),
            }
            .bound_asking(&["query", "label"]),
        )
        .operation(
            ChatEffect::FilterTasks {
                filter: String::new(),
            }
            .bound_asking(&["filter"]),
        )
    }

    fn task_output_rows(&self) -> impl Iterator<Item = LiveRow> + '_ {
        self.agent_tasks.outputs().into_iter().map(|output| {
            LiveRow::new(
                format!(
                    "chat.task-output:{}:{}:{}",
                    output.task_run.task_id, output.task_run.run_epoch, output.invocation
                ),
                "chat.task-output",
                crate::agent_tasks::output_label(output),
            )
            .effect(&ChatEffect::OpenTaskOutput {
                task: output.task_run.task_id.clone(),
                run_epoch: output.task_run.run_epoch,
                invocation: output.invocation.clone(),
            })
        })
    }

    /// The row `id`, when [`Self::published_rows`] leaves it out.
    pub(crate) fn published_row(&self, id: &str) -> Option<LiveRow> {
        if id.starts_with("chat.person:")
            || id == "chat.tasks"
            || id.starts_with("chat.task:")
            || id.starts_with("chat.task-output:")
        {
            return self.published_rows().into_iter().find(|row| row.id() == id);
        }
        if let Some(agent) = id.strip_prefix("chat.agent:") {
            let identity = agent_semantic_identity(agent)?;
            return self
                .room_agents
                .iter()
                .any(|known| known == agent)
                .then(|| {
                    LiveRow::new(id, "chat.agent", self.agent_display_name(agent))
                        .semantic_identity(identity)
                        .effect(&ChatEffect::OpenAgentConversation {
                            agent: agent.to_owned(),
                        })
                        .operation(
                            ChatEffect::OpenTaskConversation {
                                agent: agent.to_owned(),
                                task: String::new(),
                                run_epoch: 0,
                            }
                            .bound_asking(&["task", "run_epoch"]),
                        )
                });
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
                .effect(&ChatEffect::LeaveCall {
                    channel: channel_id(),
                })
                .effect(&ChatEffect::Mute {
                    channel: channel_id(),
                })
                .effect(&ChatEffect::Unmute {
                    channel: channel_id(),
                })
                .effect(&ChatEffect::CallStatus {
                    channel: channel_id(),
                }),
            )
        } else {
            row.operation(crate::semantic_read::action::spec(id))
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
