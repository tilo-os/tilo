//! Marker-anchored, private Agent Message / Queue / Steer composer.

use std::collections::HashMap;

use tilo_actions::remote::Caller;
use ui::agent_message_action::{
    AgentMessageOffer, AgentMessageOfferPoll, AgentMessagePoll, AgentMessageSubmission,
};
use ui::agent_task_frontier::{
    AgentDirectionState, AgentQueueState, AgentTaskChoice, AgentTaskFrontiers,
};
use ui::widget::{
    Button, ButtonRole, Column, Container, Popover, PopoverSide, Row, Segment, Text, TextInput,
    ToggleGroup, Widget,
};
use ui::{LocalState, Theme};
use vello::kurbo::Rect;

use super::collab_broker::AppActionBroker;
use super::i18n::{t, t_args};
use super::remote_actions::BrokerActionHost;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum ComposerIntent {
    #[default]
    Message,
    Queue,
    Steer,
}

struct PendingMessage {
    state: AgentMessageSubmission,
    deadline: std::time::Instant,
    pages_remaining: u16,
    text: String,
    draft_revision: u64,
    outcome_uncertain: bool,
}

struct PendingMessageOffer {
    state: AgentMessageSubmission,
    deadline: std::time::Instant,
    pages_remaining: u16,
}

#[derive(Clone, Copy)]
struct PendingWork {
    intent: ComposerIntent,
    draft_revision: u64,
}

#[derive(Default)]
struct Drafts {
    values: HashMap<(String, String, ComposerIntent), DraftEntry>,
}

#[derive(Clone)]
struct DraftEntry {
    text: String,
    revision: u64,
}

struct RetainedSession {
    intent: ComposerIntent,
    actions: ui::remote_actions::RemoteActions,
    tasks: AgentTaskFrontiers,
    pending_message: Option<PendingMessage>,
    pending_work: Option<PendingWork>,
}

impl RetainedSession {
    fn is_pending(&self) -> bool {
        self.pending_message.is_some() || self.pending_work.is_some()
    }

    fn next_wake_deadline(&self) -> Option<std::time::Instant> {
        let message = self
            .pending_message
            .as_ref()
            .and_then(pending_discovery_deadline);
        let task = self
            .pending_work
            .is_some()
            .then(|| self.tasks.next_refresh_deadline())
            .flatten();
        earliest(message, task)
    }
}

fn pending_discovery_deadline(pending: &PendingMessage) -> Option<std::time::Instant> {
    match &pending.state {
        AgentMessageSubmission::Invoke { .. } | AgentMessageSubmission::LaunchInvoke { .. } => None,
        AgentMessageSubmission::AwaitPublisher {
            retry_at, until, ..
        } => Some((*retry_at).min(*until)),
        _ => Some(pending.deadline),
    }
}

fn earliest(
    left: Option<std::time::Instant>,
    right: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

/// Shell-owned state. Presence identifies the row; the pinned caller and
/// freshly resolved published actions remain the only authority.
pub(crate) struct AgentQuickComposer {
    popover: Popover<Container>,
    room: String,
    agent: String,
    agent_label: String,
    frozen_anchor: Rect,
    drafts: Drafts,
    draft: LocalState<String>,
    title: LocalState<String>,
    intent: LocalState<ComposerIntent>,
    shown_intent: ComposerIntent,
    status: LocalState<String>,
    send_enabled: LocalState<bool>,
    send_requested: LocalState<bool>,
    conversation_requested: LocalState<bool>,
    actions: Option<ui::remote_actions::RemoteActions>,
    tasks: Option<AgentTaskFrontiers>,
    pending_message: Option<PendingMessage>,
    pending_message_offer: Option<PendingMessageOffer>,
    message_offer: Option<AgentMessageOffer>,
    next_message_offer_refresh: Option<std::time::Instant>,
    pending_work: Option<PendingWork>,
    message_receipt_revision: Option<u64>,
    permission_refused: bool,
    focus_reset: bool,
    retained_sessions: HashMap<(String, String, ComposerIntent), RetainedSession>,
}

impl Default for AgentQuickComposer {
    fn default() -> Self {
        let draft = LocalState::new(String::new());
        let title = LocalState::new(t("agent-quick-accessible-name"));
        let intent = LocalState::new(ComposerIntent::Message);
        let status = LocalState::new(t("agent-quick-checking"));
        let send_enabled = LocalState::new(false);
        let send_requested = LocalState::new(false);
        let conversation_requested = LocalState::new(false);

        let send_signal = send_requested.clone();
        let conversation_signal = conversation_requested.clone();
        let mode = ToggleGroup::from_state(
            vec![
                (
                    ComposerIntent::Message,
                    Segment::new(t("agent-quick-message")),
                ),
                (ComposerIntent::Queue, Segment::new(t("agent-quick-queue"))),
                (ComposerIntent::Steer, Segment::new(t("agent-quick-steer"))),
            ],
            &intent,
        )
        .fill()
        .with_accessible_name(t("agent-quick-intent"));
        let editor = TextInput::from_state(&draft)
            .with_id("agent-quick-editor")
            .with_placeholder(&t("agent-quick-placeholder"))
            .with_accessible_name(t("agent-quick-editor-name"))
            .multiline(true)
            .with_min_lines(3)
            .with_enter_submits(true);
        let actions = Row::new()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(
                Button::new("agent-quick-send", &t("agent-quick-send"))
                    .with_icon(ui::icons::Icon::Send)
                    .role(ButtonRole::Primary)
                    .enabled_from(&send_enabled)
                    .on_click(move || send_signal.set(true)),
            ))
            .push(Box::new(
                Button::new(
                    "agent-quick-open-conversation",
                    &t("agent-quick-open-conversation"),
                )
                .with_icon(ui::icons::Icon::MessageSquare)
                .on_click(move || conversation_signal.set(true)),
            ));
        let body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::from_state(&title, Clone::clone).with_role(ui::tokens::TextRole::Heading),
            ))
            .push(Box::new(Text::from_state(&status, Clone::clone).dimmed()))
            .push(Box::new(mode))
            .push(Box::new(editor))
            .push(Box::new(actions));
        let content = Container::new()
            .with_no_padding()
            .with_width(ui::tokens::Metrics::BASE.panel_width_utility)
            .with_child(Box::new(body));
        let popover = Popover::with_content(content)
            .side(PopoverSide::Right)
            .with_label(t("agent-quick-accessible-name"));
        Self {
            popover,
            room: String::new(),
            agent: String::new(),
            agent_label: String::new(),
            frozen_anchor: Rect::ZERO,
            drafts: Drafts::default(),
            draft,
            title,
            intent,
            shown_intent: ComposerIntent::Message,
            status,
            send_enabled,
            send_requested,
            conversation_requested,
            actions: None,
            tasks: None,
            pending_message: None,
            pending_message_offer: None,
            message_offer: None,
            next_message_offer_refresh: None,
            pending_work: None,
            message_receipt_revision: None,
            permission_refused: false,
            focus_reset: false,
            retained_sessions: HashMap::new(),
        }
    }
}

impl AgentQuickComposer {
    /// Deterministic presentation fixture for `shots`. It carries no action
    /// host, so it renders the real controls without granting a sample
    /// identity permission to submit anything.
    #[doc(hidden)]
    pub fn open_for_shots(
        &mut self,
        room: &str,
        agent: &str,
        label: &str,
        draft: &str,
        anchor: Rect,
        viewport: Rect,
    ) {
        room.clone_into(&mut self.room);
        agent.clone_into(&mut self.agent);
        label.clone_into(&mut self.agent_label);
        self.title.set(t_args(
            "agent-quick-title",
            &[("agent", label.to_owned().into())],
        ));
        self.frozen_anchor = anchor;
        self.shown_intent = ComposerIntent::Message;
        self.intent.set(ComposerIntent::Message);
        self.draft.set(draft.to_owned());
        self.status.set(t("agent-quick-ready"));
        self.actions = None;
        self.tasks = None;
        self.pending_message = None;
        self.pending_message_offer = None;
        self.message_offer = None;
        self.next_message_offer_refresh = None;
        self.pending_work = None;
        self.permission_refused = false;
        self.popover.set_anchor(anchor);
        self.popover.set_viewport(viewport);
        self.popover.set_open(true);
        self.focus_reset = true;
        self.refresh_enabled();
    }

    pub(crate) fn open(
        &mut self,
        broker: AppActionBroker,
        caller: Caller,
        room: String,
        agent: String,
        agent_label: String,
        anchor: Rect,
        viewport: Rect,
    ) {
        self.save_draft();
        if self.room == room && self.agent == agent && self.actions.is_some() {
            self.agent_label = agent_label;
            self.title.set(t_args(
                "agent-quick-title",
                &[("agent", self.agent_label.clone().into())],
            ));
            self.popover.set_viewport(viewport);
            self.popover.set_open(true);
            self.focus_reset = true;
            self.refresh_status();
            self.refresh_enabled();
            return;
        }
        self.park_pending_session();
        self.room = room;
        self.agent = agent;
        self.agent_label = agent_label;
        self.title.set(t_args(
            "agent-quick-title",
            &[("agent", self.agent_label.clone().into())],
        ));
        self.frozen_anchor = anchor;
        self.permission_refused = false;
        if let Some(session) = self.retained_sessions.remove(&(
            self.room.clone(),
            self.agent.clone(),
            ComposerIntent::Message,
        )) {
            self.shown_intent = session.intent;
            self.actions = Some(session.actions);
            self.tasks = Some(session.tasks);
            self.pending_message = session.pending_message;
            self.pending_work = session.pending_work;
        } else {
            self.shown_intent = ComposerIntent::Message;
            let actions = ui::remote_actions::RemoteActions::with_host(
                std::sync::Arc::new(BrokerActionHost::new(broker)),
                caller,
            );
            self.tasks = Some(AgentTaskFrontiers::with_actions(actions.clone()));
            self.actions = Some(actions);
            self.pending_message = None;
            self.pending_work = None;
        }
        self.intent.set(self.shown_intent);
        self.load_draft_and_rebind_pending_revision();
        if let Some(tasks) = self.tasks.as_mut() {
            tasks.observe(&self.room, &self.agent);
        }
        self.pending_message_offer = None;
        self.message_offer = None;
        self.next_message_offer_refresh = None;
        self.message_receipt_revision = None;
        self.status.set(t("agent-quick-checking"));
        if self.pending_message.is_none() {
            self.start_message_offer_refresh();
        }
        self.popover.set_anchor(anchor);
        self.popover.set_viewport(viewport);
        self.popover.set_open(true);
        self.focus_reset = true;
        self.refresh_enabled();
    }

    fn park_pending_session(&mut self) {
        if self.room.is_empty() || self.agent.is_empty() {
            return;
        }
        let (Some(actions), Some(tasks)) = (self.actions.take(), self.tasks.take()) else {
            return;
        };
        let session = RetainedSession {
            intent: self.shown_intent,
            actions,
            tasks,
            pending_message: self.pending_message.take(),
            pending_work: self.pending_work.take(),
        };
        if session.is_pending() {
            self.retained_sessions.insert(
                (self.room.clone(), self.agent.clone(), self.shown_intent),
                session,
            );
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.popover.is_open()
    }

    pub(crate) fn dismiss(&mut self) -> bool {
        let open = self.is_open();
        self.save_draft();
        self.popover.close();
        self.focus_reset |= open;
        open
    }

    pub(crate) fn take_conversation_request(&self) -> bool {
        let requested = self.conversation_requested.get();
        if requested {
            self.conversation_requested.set(false);
        }
        requested
    }

    pub(crate) fn target(&self) -> Option<(&str, &str)> {
        self.is_open().then_some((&self.room, &self.agent))
    }

    pub(crate) fn tick(&mut self) -> bool {
        let before = (
            self.draft.revision(),
            self.intent.revision(),
            self.status.revision(),
            self.send_enabled.revision(),
            self.send_requested.revision(),
        );
        self.poll_retained_sessions();
        if !self.is_open() {
            if self.pending_message.is_some() || self.pending_work.is_some() {
                if let Some(actions) = &self.actions {
                    actions.poll();
                }
                if self.pending_work.is_some()
                    && let Some(tasks) = self.tasks.as_mut()
                {
                    tasks.observe(&self.room, &self.agent);
                }
                self.poll_submission();
                self.refresh_status();
                self.refresh_enabled();
            }
            return before
                != (
                    self.draft.revision(),
                    self.intent.revision(),
                    self.status.revision(),
                    self.send_enabled.revision(),
                    self.send_requested.revision(),
                );
        }
        let selected = self.intent.get();
        if selected != self.shown_intent {
            self.switch_intent(selected);
        }
        if let Some(actions) = &self.actions {
            actions.poll();
        }
        if let Some(tasks) = self.tasks.as_mut() {
            tasks.observe(&self.room, &self.agent);
        }
        if self.send_requested.get() {
            self.send_requested.set(false);
            self.submit();
        }
        self.poll_message_offer();
        self.poll_submission();
        self.refresh_status();
        self.refresh_enabled();
        before
            != (
                self.draft.revision(),
                self.intent.revision(),
                self.status.revision(),
                self.send_enabled.revision(),
                self.send_requested.revision(),
            )
    }

    fn switch_intent(&mut self, selected: ComposerIntent) {
        self.save_draft();
        let shared_actions = self.actions.clone();
        self.park_pending_session();
        self.shown_intent = selected;
        self.intent.set(selected);
        self.message_receipt_revision = None;
        self.pending_message_offer = None;
        self.message_offer = None;
        self.next_message_offer_refresh = None;
        self.permission_refused = false;
        if let Some(session) =
            self.retained_sessions
                .remove(&(self.room.clone(), self.agent.clone(), selected))
        {
            self.actions = Some(session.actions);
            self.tasks = Some(session.tasks);
            self.pending_message = session.pending_message;
            self.pending_work = session.pending_work;
        } else if let Some(actions) = shared_actions {
            self.tasks = Some(AgentTaskFrontiers::with_actions(actions.clone()));
            self.actions = Some(actions);
            self.pending_message = None;
            self.pending_work = None;
        }
        self.load_draft_and_rebind_pending_revision();
        if let Some(tasks) = self.tasks.as_mut() {
            tasks.observe(&self.room, &self.agent);
        }
        if selected == ComposerIntent::Message && self.pending_message.is_none() {
            self.start_message_offer_refresh();
        }
    }

    fn poll_retained_sessions(&mut self) {
        let drafts = &mut self.drafts;
        self.retained_sessions
            .retain(|(room, agent, intent), session| {
                session.actions.poll();
                if session.pending_work.is_some() {
                    session.tasks.observe(room, agent);
                }
                if let Some(pending) = session.pending_message.as_mut() {
                    match ui::agent_message_action::poll(
                        &session.actions,
                        room,
                        agent,
                        &pending.text,
                        &pending.state,
                        pending.deadline,
                        pending.pages_remaining,
                    ) {
                        AgentMessagePoll::Waiting => {}
                        AgentMessagePoll::Advance {
                            submission,
                            pages_remaining,
                        } => {
                            pending.state = submission;
                            pending.pages_remaining = pages_remaining;
                        }
                        AgentMessagePoll::Succeeded => {
                            clear_retained_draft(
                                drafts,
                                room,
                                agent,
                                *intent,
                                pending.draft_revision,
                            );
                            session.pending_message = None;
                        }
                        AgentMessagePoll::Failed => session.pending_message = None,
                    }
                }
                if let Some(pending) = session.pending_work {
                    let settled = match pending.intent {
                        ComposerIntent::Queue => match session.tasks.queue_state() {
                            AgentQueueState::Queued { .. } => Some(true),
                            AgentQueueState::Failed => Some(false),
                            AgentQueueState::Ready | AgentQueueState::Submitting => None,
                        },
                        ComposerIntent::Steer => match session.tasks.direction_state() {
                            AgentDirectionState::Saved { .. }
                            | AgentDirectionState::Applied { .. } => Some(true),
                            AgentDirectionState::NeedsRefresh(_) => Some(false),
                            AgentDirectionState::Draft | AgentDirectionState::Submitting => None,
                        },
                        ComposerIntent::Message => None,
                    };
                    if let Some(success) = settled {
                        session.pending_work = None;
                        if success {
                            clear_retained_draft(
                                drafts,
                                room,
                                agent,
                                pending.intent,
                                pending.draft_revision,
                            );
                        }
                    }
                }
                session.is_pending()
            });
    }

    fn submit(&mut self) {
        let text = self.draft.get();
        if text.trim().is_empty() {
            return;
        }
        let revision = self.draft.revision();
        self.message_receipt_revision = None;
        match self.shown_intent {
            ComposerIntent::Message => {
                let Some(actions) = self.actions.as_ref() else {
                    return;
                };
                let Some(offer) = self.message_offer.as_ref() else {
                    return;
                };
                self.pending_message = Some(PendingMessage {
                    state: ui::agent_message_action::begin_from_offer(actions, offer),
                    deadline: std::time::Instant::now()
                        .checked_add(ui::agent_message_action::RESOLVE_WINDOW)
                        .unwrap_or_else(std::time::Instant::now),
                    pages_remaining: ui::agent_message_action::MAX_PAGES,
                    text,
                    draft_revision: revision,
                    outcome_uncertain: false,
                });
            }
            ComposerIntent::Queue => {
                if self
                    .tasks
                    .as_mut()
                    .is_some_and(|tasks| tasks.queue(&text, None).is_ok())
                {
                    self.pending_work = Some(PendingWork {
                        intent: ComposerIntent::Queue,
                        draft_revision: revision,
                    });
                }
            }
            ComposerIntent::Steer => {
                if self
                    .tasks
                    .as_mut()
                    .is_some_and(|tasks| tasks.steer(&text).is_ok())
                {
                    self.pending_work = Some(PendingWork {
                        intent: ComposerIntent::Steer,
                        draft_revision: revision,
                    });
                }
            }
        }
    }

    fn poll_submission(&mut self) {
        if let (Some(actions), Some(pending)) = (&self.actions, self.pending_message.as_mut()) {
            pending.outcome_uncertain =
                ui::agent_message_action::outcome_uncertain(actions, &pending.state);
            match ui::agent_message_action::poll(
                actions,
                &self.room,
                &self.agent,
                &pending.text,
                &pending.state,
                pending.deadline,
                pending.pages_remaining,
            ) {
                AgentMessagePoll::Waiting => {}
                AgentMessagePoll::Advance {
                    submission,
                    pages_remaining,
                } => {
                    pending.state = submission;
                    pending.pages_remaining = pages_remaining;
                }
                AgentMessagePoll::Succeeded => {
                    let revision = pending.draft_revision;
                    self.pending_message = None;
                    self.clear_if_unchanged(revision);
                    self.message_receipt_revision = Some(self.draft.revision());
                    self.start_message_offer_refresh();
                }
                AgentMessagePoll::Failed => {
                    self.pending_message = None;
                    self.permission_refused = true;
                    self.message_offer = None;
                    self.schedule_message_offer_refresh();
                    self.status.set(t("agent-quick-message-refused"));
                }
            }
        }
        let Some(tasks) = self.tasks.as_ref() else {
            return;
        };
        let Some(pending_work) = self.pending_work else {
            return;
        };
        let settled = match pending_work.intent {
            ComposerIntent::Queue => match tasks.queue_state() {
                AgentQueueState::Queued { .. } => Some(true),
                AgentQueueState::Failed => Some(false),
                AgentQueueState::Ready | AgentQueueState::Submitting => None,
            },
            ComposerIntent::Steer => match tasks.direction_state() {
                AgentDirectionState::Saved { .. } | AgentDirectionState::Applied { .. } => {
                    Some(true)
                }
                AgentDirectionState::NeedsRefresh(_) => Some(false),
                AgentDirectionState::Draft | AgentDirectionState::Submitting => None,
            },
            ComposerIntent::Message => None,
        };
        if let Some(success) = settled {
            self.pending_work = None;
            if success {
                self.clear_if_unchanged(pending_work.draft_revision);
            } else {
                self.status.set(t("agent-quick-work-refused"));
            }
        }
    }

    fn start_message_offer_refresh(&mut self) {
        let Some(actions) = self.actions.as_ref() else {
            return;
        };
        self.message_offer = None;
        self.next_message_offer_refresh = None;
        self.pending_message_offer = Some(PendingMessageOffer {
            state: ui::agent_message_action::begin(actions, &self.room),
            deadline: std::time::Instant::now()
                .checked_add(ui::agent_message_action::RESOLVE_WINDOW)
                .unwrap_or_else(std::time::Instant::now),
            pages_remaining: ui::agent_message_action::MAX_PAGES,
        });
    }

    fn schedule_message_offer_refresh(&mut self) {
        self.next_message_offer_refresh =
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(5));
    }

    fn poll_message_offer(&mut self) {
        if self.pending_message.is_some() {
            return;
        }
        if self.pending_message_offer.is_none()
            && self
                .next_message_offer_refresh
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.start_message_offer_refresh();
        }
        let (Some(actions), Some(pending)) = (&self.actions, self.pending_message_offer.as_mut())
        else {
            return;
        };
        match ui::agent_message_action::poll_offer(
            actions,
            &self.room,
            &self.agent,
            &pending.state,
            pending.deadline,
            pending.pages_remaining,
        ) {
            AgentMessageOfferPoll::Waiting => {}
            AgentMessageOfferPoll::Advance {
                submission,
                pages_remaining,
            } => {
                pending.state = submission;
                pending.pages_remaining = pages_remaining;
            }
            AgentMessageOfferPoll::Available(offer) => {
                self.pending_message_offer = None;
                self.message_offer = Some(offer);
                self.permission_refused = false;
                self.schedule_message_offer_refresh();
            }
            AgentMessageOfferPoll::Failed => {
                self.pending_message_offer = None;
                self.message_offer = None;
                self.permission_refused = true;
                self.schedule_message_offer_refresh();
            }
        }
    }

    fn refresh_status(&self) {
        if self.pending_message.is_some() {
            let pending = if self
                .pending_message
                .as_ref()
                .is_some_and(|pending| pending.outcome_uncertain)
            {
                t("agent-quick-message-outcome-unknown")
            } else {
                t("agent-quick-message-submitting")
            };
            if self.status.get() != pending {
                self.status.set(pending);
            }
            return;
        }
        let Some(tasks) = self.tasks.as_ref() else {
            return;
        };
        let value = match self.shown_intent {
            ComposerIntent::Message if self.permission_refused => t("agent-quick-message-refused"),
            ComposerIntent::Message
                if self.message_receipt_revision == Some(self.draft.revision()) =>
            {
                t("agent-quick-message-sent")
            }
            ComposerIntent::Message if self.pending_message_offer.is_some() => {
                t("agent-quick-checking")
            }
            ComposerIntent::Message => t("agent-quick-ready"),
            ComposerIntent::Queue => match tasks.queue_state() {
                AgentQueueState::Submitting => t("agent-quick-queue-submitting"),
                AgentQueueState::Queued { .. } => t("agent-quick-queue-accepted"),
                AgentQueueState::Failed => t("agent-quick-work-refused"),
                AgentQueueState::Ready => t("agent-quick-queue-help"),
            },
            ComposerIntent::Steer => match tasks.direction_state() {
                AgentDirectionState::Submitting => t("agent-quick-guide-submitting"),
                AgentDirectionState::Saved { .. } => t("agent-quick-guide-saved"),
                AgentDirectionState::Applied { .. } => t("agent-quick-guide-applied"),
                AgentDirectionState::NeedsRefresh(_) => t("agent-quick-work-refused"),
                AgentDirectionState::Draft => match tasks.choice() {
                    AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => {
                        t_args("agent-quick-guide-task", &[("task", task.title.into())])
                    }
                    AgentTaskChoice::Choose(_) => t("agent-quick-guide-choose"),
                    AgentTaskChoice::None => t("agent-quick-guide-none"),
                },
            },
        };
        if self.status.get() != value {
            self.status.set(value);
        }
    }

    fn refresh_enabled(&self) {
        let has_text = !self.draft.get().trim().is_empty();
        let available = Self::intent_available(
            self.shown_intent,
            self.pending_message.is_none() && self.message_offer.is_some(),
            self.tasks
                .as_ref()
                .is_some_and(AgentTaskFrontiers::can_queue),
            self.tasks
                .as_ref()
                .is_some_and(AgentTaskFrontiers::can_steer),
        );
        let enabled = has_text && available;
        if self.send_enabled.get() != enabled {
            self.send_enabled.set(enabled);
        }
    }

    const fn intent_available(
        intent: ComposerIntent,
        can_message: bool,
        can_queue: bool,
        can_steer: bool,
    ) -> bool {
        match intent {
            ComposerIntent::Message => can_message,
            ComposerIntent::Queue => can_queue,
            ComposerIntent::Steer => can_steer,
        }
    }

    fn draft_key(&self) -> (String, String, ComposerIntent) {
        (self.room.clone(), self.agent.clone(), self.shown_intent)
    }

    fn save_draft(&mut self) {
        if !self.room.is_empty() && !self.agent.is_empty() {
            self.drafts.values.insert(
                self.draft_key(),
                DraftEntry {
                    text: self.draft.get(),
                    revision: self.draft.revision(),
                },
            );
        }
    }

    fn load_draft_and_rebind_pending_revision(&mut self) {
        let key = self.draft_key();
        let saved = self.drafts.values.get(&key).cloned();
        self.draft.set(
            saved
                .as_ref()
                .map_or_else(String::new, |entry| entry.text.clone()),
        );
        let loaded_revision = self.draft.revision();
        if let Some(saved) = saved {
            if let Some(pending) = self.pending_message.as_mut()
                && pending.draft_revision == saved.revision
            {
                pending.draft_revision = loaded_revision;
            }
            if let Some(pending) = self.pending_work.as_mut()
                && pending.draft_revision == saved.revision
            {
                pending.draft_revision = loaded_revision;
            }
        }
        self.drafts.values.insert(
            key,
            DraftEntry {
                text: self.draft.get(),
                revision: loaded_revision,
            },
        );
    }

    fn clear_if_unchanged(&mut self, revision: u64) {
        if self.draft.revision() == revision {
            self.draft.set(String::new());
            self.drafts.values.insert(
                self.draft_key(),
                DraftEntry {
                    text: String::new(),
                    revision: self.draft.revision(),
                },
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn draft(&self) -> String {
        self.draft.get()
    }

    #[cfg(test)]
    pub(crate) fn set_draft(&self, value: &str) {
        self.draft.set(value.to_owned());
    }

    #[cfg(test)]
    pub(crate) fn panel_rect(&self) -> Rect {
        self.popover.rect()
    }
}

fn clear_retained_draft(
    drafts: &mut Drafts,
    room: &str,
    agent: &str,
    intent: ComposerIntent,
    revision: u64,
) {
    let key = (room.to_owned(), agent.to_owned(), intent);
    let Some(entry) = drafts.values.get_mut(&key) else {
        return;
    };
    if entry.revision == revision {
        entry.text.clear();
        entry.revision = entry.revision.saturating_add(1);
    }
}

impl Widget for AgentQuickComposer {
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.popover.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let width = theme.tokens.metrics.panel_width_utility
            * (0.5 + 0.5 * theme.text_scale.clamp(1.0, 2.0));
        self.popover.content_mut().set_width(width);
        self.popover.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.popover.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut ui::text_render::TextRenderer,
        _theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.popover
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let was_open = self.popover.is_open();
        self.popover.handle_event(event, ctx);
        if was_open && !self.popover.is_open() {
            self.save_draft();
            self.focus_reset = true;
        }
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.tick() | self.popover.update(state, path)
    }

    fn name(&self) -> &'static str {
        "AgentQuickComposer"
    }

    fn get_rect(&self) -> Rect {
        self.popover.get_rect()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.popover.modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.popover.traps_focus()
    }

    fn next_wake_deadline(&self) -> Option<std::time::Instant> {
        let mut deadline = self.popover.next_wake_deadline();
        deadline = earliest(
            deadline,
            self.pending_message
                .as_ref()
                .and_then(pending_discovery_deadline),
        );
        if self.is_open() {
            deadline = earliest(
                deadline,
                self.pending_message_offer
                    .as_ref()
                    .map(|pending| pending.deadline),
            );
            deadline = earliest(deadline, self.next_message_offer_refresh);
        }
        if self.is_open() || self.pending_work.is_some() {
            deadline = earliest(
                deadline,
                self.tasks
                    .as_ref()
                    .and_then(AgentTaskFrontiers::next_refresh_deadline),
            );
        }
        for session in self.retained_sessions.values() {
            deadline = earliest(deadline, session.next_wake_deadline());
        }
        deadline
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.popover.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.popover.children_mut()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.popover.take_structure_dirty()
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset)
    }

    fn is_layout_dirty(&self) -> bool {
        self.popover.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.popover.layout_needs_measure()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::{AvailableSpace, Size};
    use tilo_actions::remote::{
        InvocationReceipt, InvokeArgument, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    };
    use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
    use ui::remote_actions::RemoteCall;
    use vello::{Scene, kurbo::Affine};

    fn layout_panel(
        composer: &mut AgentQuickComposer,
        anchor: Rect,
        viewport: Rect,
        theme: &Theme,
    ) -> Rect {
        composer.popover.set_anchor(anchor);
        composer.popover.set_viewport(viewport);
        composer.popover.set_open(true);
        let mut taffy = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = composer.build_layout(&mut taffy, theme, &mut nodes);
        let mut text = ui::text_render::TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: AvailableSpace::Definite(logical_dimension(viewport.width())),
                    height: AvailableSpace::Definite(logical_dimension(viewport.height())),
                },
                |known, available, node, _, _| {
                    nodes.get(&node).map_or(taffy::Size::ZERO, |widget| unsafe {
                        (**widget).measure(known, available, &mut text)
                    })
                },
            )
            .expect("quick composer layout");
        composer.apply_layout(&taffy, node, viewport.origin());
        let presence = ui::presence::PresenceCtx::default();
        let previous = presence.push_viewport(viewport);
        composer.draw_overlay(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            theme,
            &presence,
        );
        presence.pop_viewport(previous);
        composer.panel_rect()
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "shot-test viewports are bounded logical-pixel dimensions"
    )]
    fn logical_dimension(value: f64) -> f32 {
        assert!(value.is_finite() && value >= 0.0 && value <= f64::from(f32::MAX));
        value as f32
    }

    #[test]
    fn draft_is_private_per_room_agent_and_intent() {
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("message");
        composer.save_draft();
        composer.shown_intent = ComposerIntent::Queue;
        composer.load_draft_and_rebind_pending_revision();
        assert_eq!(composer.draft(), "");
        composer.set_draft("queued work");
        composer.save_draft();
        composer.shown_intent = ComposerIntent::Message;
        composer.load_draft_and_rebind_pending_revision();
        assert_eq!(composer.draft(), "message");
    }

    #[test]
    fn terminal_success_clears_only_the_submitted_revision() {
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("submitted");
        let submitted = composer.draft.revision();
        composer.set_draft("edited later");
        composer.clear_if_unchanged(submitted);
        assert_eq!(composer.draft(), "edited later");
        let current = composer.draft.revision();
        composer.clear_if_unchanged(current);
        assert_eq!(composer.draft(), "");
    }

    #[test]
    fn an_old_receipt_cannot_clear_a_draft_edited_away_and_back() {
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("submitted");
        let submitted = composer.draft.revision();
        composer.set_draft("different");
        composer.set_draft("submitted");

        composer.clear_if_unchanged(submitted);

        assert_eq!(composer.draft(), "submitted");
        assert_ne!(composer.draft.revision(), submitted);
    }

    #[test]
    fn a_hidden_session_keeps_polling_and_clears_only_its_unchanged_draft() {
        use ui::collaboration::{CollabEvent, CollabRequest};
        use ui::widget::Collaborative;

        let invocation = InvocationId(uuid::Uuid::from_bytes([0x4d; 16]));
        let mut actions = ui::remote_actions::RemoteActions::session();
        let call = actions.invoke_with_id(
            invocation,
            RemoteActionTarget {
                provider: "chat-provider".into(),
                address: ActionAddress::new("chat.person:scribe", "message"),
            },
            InvokeArgument::Text("Review the release notes".into()),
            None,
        );
        assert!(matches!(
            actions.take_collab_requests().as_slice(),
            [CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            }] if request.invocation == invocation
        ));
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            actions: Some(actions.clone()),
            tasks: Some(AgentTaskFrontiers::with_actions(actions.clone())),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("Review the release notes");
        composer.pending_message = Some(PendingMessage {
            state: AgentMessageSubmission::Invoke { invocation, call },
            deadline: std::time::Instant::now(),
            pages_remaining: 0,
            text: "Review the release notes".into(),
            draft_revision: composer.draft.revision(),
            outcome_uncertain: false,
        });
        composer.save_draft();
        assert!(!composer.is_open());

        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }),
        });
        composer.tick();

        assert!(composer.pending_message.is_none());
        assert_eq!(composer.draft(), "");
    }

    #[test]
    fn a_switched_away_session_keeps_custody_until_its_terminal_receipt() {
        use ui::collaboration::{CollabEvent, CollabRequest};
        use ui::widget::Collaborative;

        let invocation = InvocationId(uuid::Uuid::from_bytes([0x4e; 16]));
        let mut actions = ui::remote_actions::RemoteActions::session();
        let call = actions.invoke_with_id(
            invocation,
            RemoteActionTarget {
                provider: "chat-provider".into(),
                address: ActionAddress::new("chat.person:scribe", "message"),
            },
            InvokeArgument::Text("Review the release notes".into()),
            None,
        );
        let _invoke = actions.take_collab_requests();
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            actions: Some(actions.clone()),
            tasks: Some(AgentTaskFrontiers::with_actions(actions.clone())),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("Review the release notes");
        composer.pending_message = Some(PendingMessage {
            state: AgentMessageSubmission::Invoke { invocation, call },
            deadline: std::time::Instant::now(),
            pages_remaining: 0,
            text: "Review the release notes".into(),
            draft_revision: composer.draft.revision(),
            outcome_uncertain: false,
        });
        composer.save_draft();
        composer.park_pending_session();
        composer.room = "room-b".into();
        composer.agent = "quill".into();

        assert_eq!(composer.retained_sessions.len(), 1);
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }),
        });
        composer.tick();

        assert!(composer.retained_sessions.is_empty());
        let saved = composer
            .drafts
            .values
            .get(&("room-a".into(), "scribe".into(), ComposerIntent::Message))
            .expect("the submitted agent draft remains privately keyed");
        assert_eq!(saved.text, "");
        let follow_up = actions.take_collab_requests();
        assert!(
            follow_up.iter().all(|request| !matches!(
                request,
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(request),
                }
                if request.invocation == invocation
                    || request.target.address.action == "message"
            )),
            "background custody never emits a duplicate invocation: {follow_up:?}"
        );
    }

    #[test]
    fn offer_expiry_is_a_real_widget_wake_deadline() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut composer = AgentQuickComposer::default();
        composer.open_for_shots(
            "room-a",
            "scribe",
            "Scribe",
            "",
            Rect::new(10.0, 10.0, 34.0, 34.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        composer.pending_message_offer = Some(PendingMessageOffer {
            state: ui::agent_message_action::begin(&ui::remote_actions::RemoteActions::session()),
            deadline,
            pages_remaining: 1,
        });
        composer.next_message_offer_refresh = None;

        assert_eq!(composer.next_wake_deadline(), Some(deadline));
    }

    #[test]
    fn an_unresolved_dispatched_message_does_not_spin_the_desktop() {
        let invocation = InvocationId(uuid::Uuid::from_bytes([0x4f; 16]));
        let composer = AgentQuickComposer {
            pending_message: Some(PendingMessage {
                state: AgentMessageSubmission::Invoke {
                    invocation,
                    call: ui::LocalState::new(RemoteCall::Pending),
                },
                deadline: std::time::Instant::now(),
                pages_remaining: 0,
                text: "Review the release notes".into(),
                draft_revision: 1,
                outcome_uncertain: true,
            }),
            ..AgentQuickComposer::default()
        };

        assert_eq!(composer.next_wake_deadline(), None);
    }

    #[test]
    fn switching_intents_restores_the_exact_pending_operation() {
        let invocation = InvocationId(uuid::Uuid::from_bytes([0x50; 16]));
        let actions = ui::remote_actions::RemoteActions::session();
        let call = actions.invoke_with_id(
            invocation,
            RemoteActionTarget {
                provider: "chat-provider".into(),
                address: ActionAddress::new("chat.person:scribe", "message"),
            },
            InvokeArgument::Text("Review the release notes".into()),
            None,
        );
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            actions: Some(actions.clone()),
            tasks: Some(AgentTaskFrontiers::with_actions(actions)),
            pending_message: Some(PendingMessage {
                state: AgentMessageSubmission::Invoke { invocation, call },
                deadline: std::time::Instant::now(),
                pages_remaining: 0,
                text: "Review the release notes".into(),
                draft_revision: 1,
                outcome_uncertain: false,
            }),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("Review the release notes");

        composer.switch_intent(ComposerIntent::Queue);
        assert!(composer.pending_message.is_none());
        assert!(composer.retained_sessions.contains_key(&(
            "room-a".into(),
            "scribe".into(),
            ComposerIntent::Message,
        )));

        composer.switch_intent(ComposerIntent::Message);
        assert!(composer.pending_message.is_some());
        assert!(composer.retained_sessions.is_empty());
        assert_eq!(composer.draft(), "Review the release notes");
    }

    #[test]
    fn an_unknown_admitted_message_keeps_its_draft_and_reports_unresolved_outcome() {
        use ui::collaboration::{CollabEvent, CollabRequest};
        use ui::widget::Collaborative;

        let invocation = InvocationId(uuid::Uuid::from_bytes([0x3c; 16]));
        let mut actions = ui::remote_actions::RemoteActions::session();
        let call = actions.invoke_with_id(
            invocation,
            RemoteActionTarget {
                provider: "chat-provider".into(),
                address: ActionAddress::new("chat.person:scribe", "message"),
            },
            InvokeArgument::Text("Review the release notes".into()),
            None,
        );
        let dispatched = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            },
        ] = dispatched.as_slice()
        else {
            panic!("one message invocation is dispatched")
        };
        assert_eq!(request.invocation, invocation);
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            }),
        });
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: None,
            },
        });
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            actions: Some(actions.clone()),
            pending_message: Some(PendingMessage {
                state: AgentMessageSubmission::Invoke { invocation, call },
                deadline: std::time::Instant::now(),
                pages_remaining: 0,
                text: "Review the release notes".into(),
                draft_revision: 1,
                outcome_uncertain: false,
            }),
            ..AgentQuickComposer::default()
        };
        composer.set_draft("Review the release notes");

        composer.poll_submission();
        composer.refresh_status();

        assert_eq!(composer.draft(), "Review the release notes");
        assert!(composer.pending_message.is_some());
        assert_eq!(
            composer.status.get(),
            "Sent. Waiting for Chat to confirm the outcome…"
        );
        assert!(actions.take_collab_requests().is_empty());
        assert!(matches!(
            composer
                .pending_message
                .as_ref()
                .map(|pending| pending.state.clone()),
            Some(AgentMessageSubmission::Invoke { call, .. })
                if matches!(call.get(), RemoteCall::Done(InvocationUpdate::Accepted { .. }))
        ));
    }

    #[test]
    fn each_intent_uses_only_its_own_current_permission() {
        assert!(AgentQuickComposer::intent_available(
            ComposerIntent::Message,
            true,
            false,
            false,
        ));
        assert!(AgentQuickComposer::intent_available(
            ComposerIntent::Queue,
            false,
            true,
            false,
        ));
        assert!(AgentQuickComposer::intent_available(
            ComposerIntent::Steer,
            false,
            false,
            true,
        ));
        assert!(!AgentQuickComposer::intent_available(
            ComposerIntent::Message,
            false,
            true,
            true,
        ));
    }

    #[test]
    fn presentation_names_the_agent_without_inventing_a_readiness_state() {
        let mut composer = AgentQuickComposer::default();
        composer.open_for_shots(
            "room-a",
            "scribe",
            "Release Notes Agent",
            "draft",
            Rect::new(10.0, 10.0, 34.0, 34.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        assert_eq!(composer.title.get(), "Release Notes Agent");
        assert_eq!(composer.status.get(), "Send a message through Chat");
    }

    #[test]
    fn panel_prefers_the_markers_right_then_flips_and_clamps_in_compact_hc200() {
        let normal = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let anchor = Rect::new(420.0, 180.0, 444.0, 204.0);
        let panel = layout_panel(
            &mut AgentQuickComposer::default(),
            anchor,
            normal,
            &Theme::light(),
        );
        assert!(panel.x0 > anchor.x1, "normal placement stays marker-right");
        assert!(normal.contains(panel.origin()));
        assert!(normal.contains(vello::kurbo::Point::new(panel.x1, panel.y1)));

        let compact = Rect::new(0.0, 0.0, 720.0, 640.0);
        let edge = Rect::new(682.0, 110.0, 706.0, 134.0);
        let panel = layout_panel(
            &mut AgentQuickComposer::default(),
            edge,
            compact,
            &Theme::high_contrast().with_text_scale(2.0),
        );
        assert!(panel.x1 < edge.x0, "right-edge marker flips the panel left");
        assert!(panel.x0 >= compact.x0 && panel.x1 <= compact.x1);
        assert!(panel.y0 >= compact.y0 && panel.y1 <= compact.y1);
    }
}
