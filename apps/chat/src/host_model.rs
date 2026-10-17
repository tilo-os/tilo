//! Explicit, mount-local host inference. No model output is dispatched as an action.
#[cfg(test)]
mod tests;
use crate::{
    agent_stream::{AgentFinish, AgentOutput, AgentStreamEvent},
    app::ChatApp,
};
use std::{
    cell::RefCell,
    rc::Rc,
    task::{Context, Poll, Waker},
};
use ui::models::{
    CompletionFuture, ModelRequest, Models, Role, StreamEvent, StreamState, StreamStatus,
    ToolChoice,
};

#[derive(Default)]
pub(crate) struct HostModel {
    pub models: Models,
    attempt: Option<Attempt>,
}
struct Attempt {
    channel: String,
    reply: String,
    request: ModelRequest,
    future: Option<CompletionFuture<'static>>,
    events: Rc<RefCell<Vec<StreamEvent>>>,
    state: StreamState,
    sequence: u64,
    published_text: String,
    published_reasoning: String,
    terminal_published: bool,
}
impl HostModel {
    pub fn running(&self) -> bool {
        self.attempt.as_ref().is_some_and(|a| a.future.is_some())
    }
    pub fn needs_frame(&self) -> bool {
        self.attempt.as_ref().is_some_and(|a| !a.terminal_published)
    }
    pub fn controls(&self, channel: &str) -> Vec<&'static str> {
        if !self.models.is_available() {
            return vec![];
        }
        // A stopped future still owns its reply until the terminal event is
        // persisted. Do not let another channel replace that pending publish.
        if self
            .attempt
            .as_ref()
            .is_some_and(|a| a.channel != channel && !a.terminal_published)
        {
            return vec![];
        }
        match self.attempt.as_ref().filter(|a| a.channel == channel) {
            Some(_) if self.running() => vec!["stop"],
            Some(a) if !a.terminal_published => vec![],
            Some(a) if a.state.status == StreamStatus::Truncated && !a.state.text.is_empty() => {
                vec!["ask", "retry", "continue"]
            }
            Some(a)
                if matches!(
                    a.state.status,
                    StreamStatus::Truncated
                        | StreamStatus::Failed
                        | StreamStatus::Cancelled
                        | StreamStatus::Refused
                ) =>
            {
                vec!["ask", "retry"]
            }
            _ if self.running() => vec![],
            _ => vec!["ask"],
        }
    }
}
impl ChatApp {
    pub(crate) fn model_message_label(&self, message: &crate::model::Message) -> String {
        let author = self.message_author_label(message);
        let mut label = format!("{author}: {}", message.text);
        if let Some(stream) = message.work.as_ref().and_then(|work| work.stream.as_ref()) {
            label.push('\n');
            label.push_str(&crate::i18n::t(stream.status_key()));
            if !stream.detail.is_empty() {
                label.push('\n');
                label.push_str(&stream.detail);
            }
            if self.expanded_reasoning.contains(&message.id) && !stream.reasoning.is_empty() {
                label.push('\n');
                label.push_str(&crate::i18n::t("chat-agent-provider-reasoning"));
                label.push_str(": ");
                label.push_str(&stream.reasoning);
            }
        }
        label
    }

    /// Stream output keeps the authenticated invoker for custody, but is not
    /// that person's speech. Appearance supplies only the model's display name.
    pub(crate) fn is_provider_output(&self, message: &crate::model::Message) -> bool {
        !message.author.starts_with("bot:")
            && !message.author.starts_with("agent:")
            && !message
                .author
                .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
            && !self.agent_authors.contains_key(&message.author)
            && message
                .work
                .as_ref()
                .and_then(|work| work.stream.as_ref())
                .is_some()
    }

    pub(crate) fn message_author_name(&self, message: &crate::model::Message) -> String {
        if self.is_provider_output(message) {
            message
                .appearance
                .as_ref()
                .map(|appearance| appearance.name.as_str())
                .filter(|name| !name.trim().is_empty())
                .map_or_else(|| crate::i18n::t("chat-model-author"), str::to_owned)
        } else {
            self.user_name(&message.author)
        }
    }

    pub(crate) fn message_author_label(&self, message: &crate::model::Message) -> String {
        let name = self.message_author_name(message);
        if self.is_provider_output(message) {
            crate::i18n::t_args(
                "chat-model-attribution",
                &[
                    ("model", name.into()),
                    ("person", self.user_name(&message.author).into()),
                ],
            )
        } else if message.author.starts_with("bot:") {
            let members: Vec<&str> = self.users.iter().map(|user| user.name.as_str()).collect();
            let shown = self
                .message_author(message, &name, "", None)
                .label(&members);
            crate::i18n::t_args(
                "chat-bot-attribution",
                &[("name", shown.into()), ("bot", name.into())],
            )
        } else {
            name
        }
    }

    pub(crate) fn model_controls(&self, channel: &str) -> Vec<&'static str> {
        self.eligible_model_controls(channel, self.search_messages.iter())
    }

    fn eligible_model_controls<'a>(
        &self,
        channel: &str,
        mut messages: impl Iterator<Item = &'a crate::model::Message>,
    ) -> Vec<&'static str> {
        if !self.can_participate()
            || !self.channels.iter().any(|ch| {
                ch.id == channel
                    && ch.workspace == self.active_ws
                    && ch.kind == crate::model::ChannelKind::Text
                    && !ch.archived
            })
        {
            return vec![];
        }
        let mut controls = self.host_model.controls(channel);
        if !messages.any(|message| {
            message.channel == channel
                && message.author == self.me.id
                && message
                    .work
                    .as_ref()
                    .and_then(|work| work.stream.as_ref())
                    .is_none()
                && !message.text.trim().is_empty()
        }) {
            controls.retain(|command| *command != "ask");
        }
        controls
    }

    /// Pointer and assistive activation share both acceptance and visible errors.
    pub(crate) fn activate_model_control(&mut self, command: &str) -> bool {
        match self.model_command(&self.active_channel.clone(), command) {
            Ok(_) => true,
            Err(error) => {
                self.attachment_status = Some(error);
                self.canonical_dirty = true;
                false
            }
        }
    }

    /// Acknowledgement means the host future has been installed (or dropped for
    /// stop), not that a provider accepted or completed the request.
    pub(crate) fn model_command(
        &mut self,
        channel: &str,
        command: &str,
    ) -> Result<serde_json::Value, String> {
        let messages = self.search_messages.clone();
        if !self
            .eligible_model_controls(channel, messages.iter())
            .contains(&command)
        {
            return Err(
                "Model control unavailable. Send a message first, wait for any active reply to finish, and check your channel access."
                    .into(),
            );
        }
        if command == "stop" {
            let attempt = self
                .host_model
                .attempt
                .as_mut()
                .ok_or("no active inference")?;
            attempt.future = None;
            attempt.state.cancel();
            self.canonical_dirty = true;
            return Ok(serde_json::json!({"stopped": true, "reply": attempt.reply}));
        }
        let request = if command == "ask" {
            let messages = self.search_messages.clone();
            let source = messages
                .iter()
                .rev()
                .find(|m| {
                    m.channel == channel
                        && m.author == self.me.id
                        && m.work
                            .as_ref()
                            .and_then(|work| work.stream.as_ref())
                            .is_none()
                        && !m.text.trim().is_empty()
                })
                .ok_or("send a message before asking the assistant")?;
            ModelRequest {
                messages: vec![ui::models::Message::text(Role::User, &source.text)],
                tools: vec![],
                tool_choice: ToolChoice::None,
                max_output_tokens: 2048,
            }
        } else {
            let previous = self
                .host_model
                .attempt
                .as_ref()
                .ok_or("no previous inference")?;
            let mut request = previous.request.clone();
            if command == "continue" {
                request.messages.push(ui::models::Message::text(
                    Role::Assistant,
                    &previous.state.text,
                ));
                request.messages.push(ui::models::Message::text(
                    Role::User,
                    "Continue the interrupted answer. Do not repeat or perform actions.",
                ));
            }
            request.tools.clear();
            request.tool_choice = ToolChoice::None;
            request
        };
        let reply = uuid::Uuid::new_v4().to_string();
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let models = self.host_model.models.clone();
        let input = request.clone();
        let future = Box::pin(async move {
            models
                .stream(&input, Box::new(move |event| sink.borrow_mut().push(event)))
                .await
        });
        self.host_model.attempt = Some(Attempt {
            channel: channel.into(),
            reply: reply.clone(),
            request,
            future: Some(future),
            events,
            state: StreamState::default(),
            sequence: 0,
            published_text: String::new(),
            published_reasoning: String::new(),
            terminal_published: false,
        });
        self.canonical_dirty = true;
        Ok(serde_json::json!({"started": true, "reply": reply}))
    }

    pub(crate) fn pump_model(&mut self) -> bool {
        let Some(mut attempt) = self.host_model.attempt.take() else {
            return false;
        };
        if attempt.terminal_published {
            self.host_model.attempt = Some(attempt);
            return false;
        }
        if let Some(future) = attempt.future.as_mut() {
            // The widget requests frames only while this local future is live.
            let mut context = Context::from_waker(Waker::noop());
            if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                attempt.future = None;
                let event = match result {
                    Ok(response) => StreamEvent::Finished { response },
                    Err(error) => StreamEvent::Failed { error },
                };
                attempt.events.borrow_mut().push(event);
            }
        }
        let pending = std::mem::take(&mut *attempt.events.borrow_mut());
        for event in pending {
            attempt.state.apply(&event);
            // StreamState retains absent final prose for interrupted responses.
            // A successful, empty final must not certify stale partial prose.
            if let StreamEvent::Finished { response } = &event
                && attempt.state.status == StreamStatus::Completed
                && !response.message.content.iter().any(
                    |block| matches!(block, ui::models::Content::Text { text } if !text.is_empty()),
                )
            {
                attempt.state.text.clear();
            }
            if !attempt.bound_output() {
                attempt.future = None;
                break;
            }
        }
        let (changed, events) = attempt.publish();
        for event in events {
            self.submit_part(crate::parts::ChatPart::STREAM_OUTPUT.with(
                crate::messages::chat_part::StreamOutput {
                    channel: ui::tilo_resource::Id::from_raw(&attempt.channel),
                    reply: attempt.reply.clone(),
                    name: Some(crate::i18n::t("chat-model-author")),
                    event: serde_json::to_value(event).unwrap_or_default(),
                },
            ));
        }
        self.host_model.attempt = Some(attempt);
        if changed {
            self.canonical_dirty = true;
        }
        changed
    }
}

// AgentStream's aggregate reasoning and terminal-detail limits. Its text limit
// is shared with record; reasoning currently has no exported limit constant.
const MAX_REASONING_BYTES: usize = 256 * 1024;
const MAX_DETAIL_BYTES: usize = 16 * 1024;

fn utf8_prefix(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.get(..end).unwrap_or_default()
}

impl Attempt {
    /// Validate before emitting any ordered record actions. A rejected delta
    /// would otherwise leave a sequence gap that also rejects the terminal.
    fn bound_output(&mut self) -> bool {
        if !self.state.text.starts_with(&self.published_text)
            || !self.state.reasoning.starts_with(&self.published_reasoning)
        {
            self.state.text.clone_from(&self.published_text);
            self.state.reasoning.clone_from(&self.published_reasoning);
            self.state.status = StreamStatus::Failed;
            self.state.error = Some(ui::models::ModelError {
                kind: ui::models::ErrorKind::InvalidResponse,
                message:
                    "Provider final changed previously published output; partial output retained"
                        .into(),
            });
            return false;
        }
        if self.state.text.len() > crate::record::MAX_MESSAGE_TEXT_BYTES
            || self.state.reasoning.len() > MAX_REASONING_BYTES
        {
            self.state.text =
                utf8_prefix(&self.state.text, crate::record::MAX_MESSAGE_TEXT_BYTES).into();
            self.state.reasoning = utf8_prefix(&self.state.reasoning, MAX_REASONING_BYTES).into();
            self.state.status = StreamStatus::Truncated;
            self.state.error = Some(ui::models::ModelError {
                kind: ui::models::ErrorKind::Truncated,
                message: "Output exceeded the reply presentation limit; partial output retained"
                    .into(),
            });
            return false;
        }
        true
    }

    fn publish(&mut self) -> (bool, Vec<AgentStreamEvent>) {
        let mut events = Vec::new();
        for (value, published, reasoning) in [
            (&self.state.text, &mut self.published_text, false),
            (&self.state.reasoning, &mut self.published_reasoning, true),
        ] {
            while let Some(suffix) = value
                .strip_prefix(published.as_str())
                .filter(|delta| !delta.is_empty())
            {
                let delta = utf8_prefix(suffix, crate::record::MAX_MESSAGE_TEXT_BYTES);
                self.sequence += 1;
                let event = if reasoning {
                    AgentOutput::ReasoningDelta { text: delta.into() }
                } else {
                    AgentOutput::TextDelta { text: delta.into() }
                };
                events.push(AgentStreamEvent {
                    run: self.reply.clone(),
                    sequence: self.sequence,
                    event,
                });
                published.push_str(delta);
            }
        }
        if self.state.status != StreamStatus::Streaming {
            self.future = None;
            let outcome = match self.state.status {
                StreamStatus::Completed => AgentFinish::Complete,
                StreamStatus::Truncated => AgentFinish::Truncated,
                StreamStatus::Refused => AgentFinish::Refused,
                StreamStatus::Cancelled => AgentFinish::Cancelled,
                _ => AgentFinish::Failed,
            };
            self.sequence += 1;
            let detail = self
                .state
                .error
                .as_ref()
                .map(|e| format!("{} {}", e.message, e.recovery().guidance()))
                .unwrap_or_default();
            events.push(AgentStreamEvent {
                run: self.reply.clone(),
                sequence: self.sequence,
                event: AgentOutput::Finished {
                    outcome,
                    detail: utf8_prefix(&detail, MAX_DETAIL_BYTES).into(),
                },
            });
            self.terminal_published = true;
        }
        (!events.is_empty(), events)
    }
}
