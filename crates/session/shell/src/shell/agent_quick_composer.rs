//! Marker-anchored, private Agent Message / Queue / Steer composer.

use std::collections::{HashMap, HashSet};

use ui::agent_message_action::{
    AgentMessageOffer, AgentMessageOfferPoll, AgentMessagePoll, AgentMessageSubmission,
};
use ui::agent_task_frontier::{
    AgentDirectionState, AgentQueueState, AgentTaskChoice, AgentTaskFrontier, AgentTaskFrontiers,
};
use ui::widget::{
    Badge, Button, ButtonRole, Column, Container, Dropdown, Popover, PopoverSide, Row, Segment,
    Text, TextInput, ToggleGroup, When, Widget,
};
use ui::{LocalState, Theme};
use vello::kurbo::Rect;

use super::certified_action_session::{ActionSessionCustody, DesktopActionClient};
use super::i18n::{t, t_args};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum ComposerIntent {
    #[default]
    Message,
    Queue,
    Steer,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TaskCenterRequest {
    pub(crate) task_id: Option<String>,
}

struct ComposerControls<'a> {
    intent: &'a LocalState<ComposerIntent>,
    draft: &'a LocalState<String>,
    title: &'a LocalState<String>,
    origin_context: &'a LocalState<String>,
    status: &'a LocalState<String>,
    send_enabled: &'a LocalState<bool>,
    send_requested: &'a LocalState<bool>,
    conversation_requested: &'a LocalState<bool>,
    task_center_requested: &'a LocalState<bool>,
    close_requested: &'a LocalState<bool>,
    task_options: &'a LocalState<Vec<(String, String)>>,
    task_selected: &'a LocalState<String>,
    task_visible: &'a LocalState<bool>,
}

/// Validated descriptive origin for one marker-opened composer. Geometry can
/// guide placement and the text can preserve context when overlap is
/// unavoidable; neither is an execution capability.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentQuickComposerOrigin {
    pub marker: Rect,
    pub protected: Rect,
    pub context: String,
}

impl AgentQuickComposerOrigin {
    #[cfg(test)]
    pub(crate) fn marker_only(marker: Rect) -> Self {
        Self {
            marker,
            protected: marker,
            context: String::new(),
        }
    }
}

/// A presentation choice, never an execution capability. Revision fences a
/// stale painted option as well as a replacement launch of the same task.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskChoice {
    task_id: String,
    run_epoch: u64,
    revision: u64,
}

impl TaskChoice {
    fn from_frontier(frontier: &AgentTaskFrontier) -> Self {
        Self {
            task_id: frontier.task_id.clone(),
            run_epoch: frontier.run_epoch,
            revision: frontier.after_revision,
        }
    }

    fn value(&self) -> String {
        format!("{}:{}:{}", self.task_id, self.run_epoch, self.revision)
    }
}

fn task_option_label(frontier: &AgentTaskFrontier) -> String {
    use ui::agent_task_frontier::WorkspaceTaskExecutionPhase as Phase;
    let phase = frontier
        .execution
        .as_ref()
        .map_or(
            "agent-quick-task-phase-unknown",
            |execution| match execution.phase {
                Phase::Accepted => "agent-quick-task-phase-accepted",
                Phase::Planning => "agent-quick-task-phase-planning",
                Phase::Executing => "agent-quick-task-phase-executing",
            },
        );
    t_args(
        "agent-quick-task-option",
        &[
            ("task", frontier.title.clone().into()),
            ("phase", t(phase).into()),
        ],
    )
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
    follow_until: std::time::Instant,
}

const RECEIPT_FOLLOW_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
type DraftKey = (Option<ActionSessionCustody>, String, String, ComposerIntent);

#[derive(Default)]
struct Drafts {
    values: HashMap<DraftKey, DraftEntry>,
}

#[derive(Clone)]
struct DraftEntry {
    text: String,
    revision: u64,
    // Peer-local shared-editor caret/selection snapshot, never CRDT state.
    editor_state: Option<String>,
}

fn quick_editor_state(widget: &dyn Widget) -> Option<String> {
    if widget.test_id() == Some("agent-quick-editor") {
        return ui::undo::get_state(widget);
    }
    widget.children().into_iter().find_map(quick_editor_state)
}

fn restore_quick_editor_state(widget: &mut dyn Widget, state: &str) -> bool {
    if widget.test_id() == Some("agent-quick-editor") {
        ui::undo::set_state(widget, state);
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| restore_quick_editor_state(child, state))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ComposerReceiptKind {
    MessageAccepted,
    Queued {
        task_id: String,
    },
    DirectionSaved {
        task_id: String,
        run_epoch: u64,
        revision: u64,
    },
    DirectionApplied {
        task_id: String,
        run_epoch: u64,
        revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ComposerReceipt {
    submitted_revision: u64,
    kind: ComposerReceiptKind,
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
            .and_then(|pending| pending_wake_deadline(&self.actions, pending));
        let task = self
            .pending_work
            .is_some()
            .then(|| self.tasks.next_wake_deadline())
            .flatten();
        let follow = self.pending_work.and_then(|pending| {
            (pending.intent == ComposerIntent::Steer
                && matches!(
                    self.tasks.direction_state(),
                    AgentDirectionState::Saved { .. }
                ))
            .then_some(pending.follow_until)
        });
        earliest(earliest(message, task), follow)
    }
}

fn pending_wake_deadline(
    actions: &ui::remote_actions::RemoteActions,
    pending: &PendingMessage,
) -> Option<std::time::Instant> {
    ui::agent_message_action::next_wake_deadline(actions, &pending.state, pending.deadline)
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

fn work_outcome(
    tasks: &AgentTaskFrontiers,
    intent: ComposerIntent,
) -> Option<Result<ComposerReceiptKind, ()>> {
    match intent {
        ComposerIntent::Queue => match tasks.queue_state() {
            AgentQueueState::Queued { task_id } => Some(Ok(ComposerReceiptKind::Queued {
                task_id: task_id.clone(),
            })),
            AgentQueueState::Failed => Some(Err(())),
            AgentQueueState::Ready | AgentQueueState::Submitting | AgentQueueState::Uncertain => {
                None
            }
        },
        ComposerIntent::Steer => match tasks.direction_state() {
            AgentDirectionState::Saved {
                task_id,
                run_epoch,
                revision,
            } => Some(Ok(ComposerReceiptKind::DirectionSaved {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                revision: *revision,
            })),
            AgentDirectionState::Applied {
                task_id,
                run_epoch,
                revision,
            } => Some(Ok(ComposerReceiptKind::DirectionApplied {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                revision: *revision,
            })),
            AgentDirectionState::NeedsRefresh(_) => Some(Err(())),
            AgentDirectionState::Draft
            | AgentDirectionState::Submitting
            | AgentDirectionState::Uncertain { .. } => None,
        },
        ComposerIntent::Message => None,
    }
}

fn receipt_follow_deadline() -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(RECEIPT_FOLLOW_WINDOW).unwrap_or(now)
}

fn poll_retained_message(
    drafts: &mut Drafts,
    receipts: &mut HashMap<DraftKey, ComposerReceipt>,
    custody: Option<&ActionSessionCustody>,
    room: &str,
    agent: &str,
    intent: ComposerIntent,
    session: &mut RetainedSession,
) {
    let Some(pending) = session.pending_message.as_mut() else {
        return;
    };
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
            let revision = pending.draft_revision;
            receipts.insert(
                (custody.cloned(), room.to_owned(), agent.to_owned(), intent),
                ComposerReceipt {
                    submitted_revision: revision,
                    kind: ComposerReceiptKind::MessageAccepted,
                },
            );
            clear_retained_draft(drafts, custody, room, agent, intent, revision);
            session.pending_message = None;
        }
        AgentMessagePoll::Failed => session.pending_message = None,
    }
}

fn retained_work_outcome(
    session: &AgentTaskFrontiers,
    pending: PendingWork,
    now: std::time::Instant,
) -> Option<Result<(ComposerReceiptKind, bool), ()>> {
    if pending.intent != ComposerIntent::Steer {
        return work_outcome(session, pending.intent)
            .map(|outcome| outcome.map(|kind| (kind, true)));
    }
    match session.direction_state() {
        AgentDirectionState::Saved {
            task_id,
            run_epoch,
            revision,
        } => Some(Ok((
            ComposerReceiptKind::DirectionSaved {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                revision: *revision,
            },
            now >= pending.follow_until,
        ))),
        AgentDirectionState::Applied {
            task_id,
            run_epoch,
            revision,
        } => Some(Ok((
            ComposerReceiptKind::DirectionApplied {
                task_id: task_id.clone(),
                run_epoch: *run_epoch,
                revision: *revision,
            },
            true,
        ))),
        AgentDirectionState::NeedsRefresh(_) => Some(Err(())),
        AgentDirectionState::Draft
        | AgentDirectionState::Submitting
        | AgentDirectionState::Uncertain { .. } => None,
    }
}

fn poll_retained_work(
    drafts: &mut Drafts,
    receipts: &mut HashMap<DraftKey, ComposerReceipt>,
    custody: Option<&ActionSessionCustody>,
    room: &str,
    agent: &str,
    session: &mut RetainedSession,
    now: std::time::Instant,
) {
    let Some(pending) = session.pending_work else {
        return;
    };
    let key = (
        custody.cloned(),
        room.to_owned(),
        agent.to_owned(),
        pending.intent,
    );
    let Some(outcome) = retained_work_outcome(&session.tasks, pending, now) else {
        return;
    };
    let Ok((kind, terminal)) = outcome else {
        session.pending_work = None;
        return;
    };
    if terminal {
        session.pending_work = None;
    }
    let would_downgrade = matches!(
        (receipts.get(&key), &kind),
        (
            Some(ComposerReceipt {
                submitted_revision,
                kind: ComposerReceiptKind::DirectionApplied {
                    task_id: applied_task,
                    run_epoch: applied_run,
                    revision: applied_revision,
                },
            }),
            ComposerReceiptKind::DirectionSaved {
                task_id: saved_task,
                run_epoch: saved_run,
                revision: saved_revision,
            },
        ) if *submitted_revision == pending.draft_revision
            && applied_task == saved_task
            && applied_run == saved_run
            && applied_revision == saved_revision
    );
    if !would_downgrade {
        receipts.insert(
            key,
            ComposerReceipt {
                submitted_revision: pending.draft_revision,
                kind,
            },
        );
    }
    clear_retained_draft(
        drafts,
        custody,
        room,
        agent,
        pending.intent,
        pending.draft_revision,
    );
}

fn receipt_status(kind: &ComposerReceiptKind) -> String {
    match kind {
        ComposerReceiptKind::MessageAccepted => t("agent-quick-message-sent"),
        ComposerReceiptKind::Queued { task_id } => t_args(
            "agent-quick-queue-accepted",
            &[("task", task_id.clone().into())],
        ),
        ComposerReceiptKind::DirectionSaved {
            task_id,
            run_epoch,
            revision,
        } => t_args(
            "agent-quick-guide-saved",
            &[
                ("task", task_id.clone().into()),
                ("run", i64::try_from(*run_epoch).unwrap_or(i64::MAX).into()),
                (
                    "revision",
                    i64::try_from(*revision).unwrap_or(i64::MAX).into(),
                ),
            ],
        ),
        ComposerReceiptKind::DirectionApplied {
            task_id,
            run_epoch,
            revision,
        } => t_args(
            "agent-quick-guide-applied",
            &[
                ("task", task_id.clone().into()),
                ("run", i64::try_from(*run_epoch).unwrap_or(i64::MAX).into()),
                (
                    "revision",
                    i64::try_from(*revision).unwrap_or(i64::MAX).into(),
                ),
            ],
        ),
    }
}

fn message_status(
    permission_refused: bool,
    checking_offer: bool,
    receipt: Option<&ComposerReceipt>,
) -> String {
    if permission_refused {
        return t("agent-quick-message-refused");
    }
    if let Some(receipt) = receipt
        && matches!(&receipt.kind, ComposerReceiptKind::MessageAccepted)
    {
        return receipt_status(&receipt.kind);
    }
    if checking_offer {
        t("agent-quick-checking")
    } else {
        t("agent-quick-ready")
    }
}

fn queue_status(tasks: &AgentTaskFrontiers, receipt: Option<&ComposerReceipt>) -> String {
    match tasks.queue_state() {
        AgentQueueState::Submitting => t("agent-quick-queue-submitting"),
        AgentQueueState::Uncertain => t("agent-quick-queue-outcome-unknown"),
        AgentQueueState::Queued { task_id } => receipt_status(&ComposerReceiptKind::Queued {
            task_id: task_id.clone(),
        }),
        AgentQueueState::Failed => t("agent-quick-work-refused"),
        AgentQueueState::Ready => receipt
            .filter(|receipt| matches!(&receipt.kind, ComposerReceiptKind::Queued { .. }))
            .map_or_else(
                || t("agent-quick-queue-help"),
                |receipt| receipt_status(&receipt.kind),
            ),
    }
}

fn direction_status(tasks: &AgentTaskFrontiers, receipt: Option<&ComposerReceipt>) -> String {
    match tasks.direction_state() {
        AgentDirectionState::Submitting => t("agent-quick-guide-submitting"),
        AgentDirectionState::Uncertain { .. } => t("agent-quick-guide-outcome-unknown"),
        AgentDirectionState::Saved {
            task_id,
            run_epoch,
            revision,
        } => receipt_status(&ComposerReceiptKind::DirectionSaved {
            task_id: task_id.clone(),
            run_epoch: *run_epoch,
            revision: *revision,
        }),
        AgentDirectionState::Applied {
            task_id,
            run_epoch,
            revision,
        } => receipt_status(&ComposerReceiptKind::DirectionApplied {
            task_id: task_id.clone(),
            run_epoch: *run_epoch,
            revision: *revision,
        }),
        AgentDirectionState::NeedsRefresh(_) => t("agent-quick-work-refused"),
        AgentDirectionState::Draft => receipt
            .filter(|receipt| {
                matches!(
                    &receipt.kind,
                    ComposerReceiptKind::DirectionApplied { .. }
                        | ComposerReceiptKind::DirectionSaved { .. }
                )
            })
            .map_or_else(
                || match tasks.choice() {
                    AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => {
                        t_args("agent-quick-guide-task", &[("task", task.title.into())])
                    }
                    AgentTaskChoice::Choose(_) => t("agent-quick-guide-choose"),
                    AgentTaskChoice::None => t("agent-quick-guide-none"),
                },
                |receipt| receipt_status(&receipt.kind),
            ),
    }
}

/// Shell-owned state. Presence identifies the row; the pinned caller and
/// freshly resolved published actions remain the only authority.
pub(crate) struct AgentQuickComposer {
    popover: Popover<Container>,
    room: String,
    agent: String,
    custody: Option<ActionSessionCustody>,
    agent_label: String,
    frozen_anchor: Rect,
    drafts: Drafts,
    pending_editor_state: Option<(u64, String)>,
    receipts: HashMap<DraftKey, ComposerReceipt>,
    draft: LocalState<String>,
    title: LocalState<String>,
    origin_context: LocalState<String>,
    intent: LocalState<ComposerIntent>,
    shown_intent: ComposerIntent,
    status: LocalState<String>,
    send_enabled: LocalState<bool>,
    send_requested: LocalState<bool>,
    conversation_requested: LocalState<bool>,
    task_center_requested: LocalState<bool>,
    close_requested: LocalState<bool>,
    actions: Option<ui::remote_actions::RemoteActions>,
    tasks: Option<AgentTaskFrontiers>,
    task_options: LocalState<Vec<(String, String)>>,
    task_selected: LocalState<String>,
    task_selected_observed: u64,
    task_visible: LocalState<bool>,
    displayed_tasks: Vec<TaskChoice>,
    chosen_tasks: HashMap<DraftKey, TaskChoice>,
    reconfirm_tasks: HashSet<DraftKey>,
    pending_message: Option<PendingMessage>,
    pending_message_offer: Option<PendingMessageOffer>,
    message_offer: Option<AgentMessageOffer>,
    pending_work: Option<PendingWork>,
    permission_refused: bool,
    focus_reset: bool,
    retained_sessions: HashMap<DraftKey, RetainedSession>,
}

impl Default for AgentQuickComposer {
    fn default() -> Self {
        let draft = LocalState::new(String::new());
        let title = LocalState::new(t("agent-quick-accessible-name"));
        let origin_context = LocalState::new(String::new());
        let intent = LocalState::new(ComposerIntent::Message);
        let status = LocalState::new(t("agent-quick-checking"));
        let send_enabled = LocalState::new(false);
        let send_requested = LocalState::new(false);
        let conversation_requested = LocalState::new(false);
        let task_center_requested = LocalState::new(false);
        let close_requested = LocalState::new(false);
        let task_options = LocalState::new(Vec::new());
        let task_selected = LocalState::new(String::new());
        let task_visible = LocalState::new(false);

        let popover = quick_composer_popover(&ComposerControls {
            intent: &intent,
            draft: &draft,
            title: &title,
            origin_context: &origin_context,
            status: &status,
            send_enabled: &send_enabled,
            send_requested: &send_requested,
            conversation_requested: &conversation_requested,
            task_center_requested: &task_center_requested,
            close_requested: &close_requested,
            task_options: &task_options,
            task_selected: &task_selected,
            task_visible: &task_visible,
        });
        Self {
            popover,
            room: String::new(),
            agent: String::new(),
            custody: None,
            agent_label: String::new(),
            frozen_anchor: Rect::ZERO,
            drafts: Drafts::default(),
            pending_editor_state: None,
            receipts: HashMap::new(),
            draft,
            title,
            origin_context,
            intent,
            shown_intent: ComposerIntent::Message,
            status,
            send_enabled,
            send_requested,
            conversation_requested,
            task_center_requested,
            close_requested,
            actions: None,
            tasks: None,
            task_options,
            task_selected,
            task_selected_observed: 0,
            task_visible,
            displayed_tasks: Vec::new(),
            chosen_tasks: HashMap::new(),
            reconfirm_tasks: HashSet::new(),
            pending_message: None,
            pending_message_offer: None,
            message_offer: None,
            pending_work: None,
            permission_refused: false,
            focus_reset: false,
            retained_sessions: HashMap::new(),
        }
    }
}

fn origin_context_text(context: &LocalState<String>) -> Container {
    Container::new().with_no_padding().with_child(Box::new(
        Text::from_state(context, Clone::clone)
            .with_id("agent-quick-origin-context")
            .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
            .dimmed(),
    ))
}

fn quick_composer_popover(controls: &ComposerControls<'_>) -> Popover<Container> {
    let send_signal = controls.send_requested.clone();
    let conversation_signal = controls.conversation_requested.clone();
    let task_center_signal = controls.task_center_requested.clone();
    let close_signal = controls.close_requested.clone();
    let picker = Dropdown::from_state(Vec::new(), controls.task_selected)
        .value_options_from(controls.task_options)
        .with_id("agent-quick-task")
        .with_resolved_accessible_name(t("agent-quick-task-label"))
        .caption(t("agent-quick-task-label"))
        .with_placeholder(&t("agent-quick-task-placeholder"))
        .fill();
    let mode = ToggleGroup::from_state(
        vec![
            (
                ComposerIntent::Message,
                Segment::new(t("agent-quick-message")),
            ),
            (ComposerIntent::Queue, Segment::new(t("agent-quick-queue"))),
            (ComposerIntent::Steer, Segment::new(t("agent-quick-steer"))),
        ],
        controls.intent,
    )
    .fill()
    .with_id("agent-quick-intent")
    .with_accessible_name(t("agent-quick-intent"));
    let editor = TextInput::from_state(controls.draft)
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
                .enabled_from(controls.send_enabled)
                .on_click(move || send_signal.set(true)),
        ))
        .push(Box::new(
            Button::new(
                "agent-quick-open-conversation",
                &t("agent-quick-open-conversation"),
            )
            .with_icon(ui::icons::Icon::MessageSquare)
            .on_click(move || conversation_signal.set(true)),
        ))
        .push(Box::new(
            Button::new("agent-quick-open-tasks", &t("agent-quick-open-tasks"))
                .with_icon(ui::icons::Icon::List)
                .on_click(move || task_center_signal.set(true)),
        ))
        .push(Box::new(
            Button::new("agent-quick-close", &t("agent-quick-close"))
                .with_icon(ui::icons::Icon::X)
                .on_click(move || close_signal.set(true)),
        ));
    let body = Column::new()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
        .push(Box::new(
            Row::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(
                    Text::from_state(controls.title, Clone::clone)
                        .with_role(ui::tokens::TextRole::Heading),
                ))
                .push(Box::new(Badge::text(t("agent-quick-kind")))),
        ))
        .push(Box::new(origin_context_text(controls.origin_context)))
        .push(Box::new(
            Container::new().with_no_padding().with_child(Box::new(
                Text::from_state(controls.status, Clone::clone)
                    .with_id("agent-quick-status")
                    .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                    .dimmed(),
            )),
        ))
        .push(Box::new(mode))
        .push(Box::new(When::from_state(
            controls.task_visible,
            |visible| *visible,
            Box::new(picker),
        )))
        .push(Box::new(editor))
        .push(Box::new(actions));
    let content = Container::new()
        .with_no_padding()
        .with_width(ui::tokens::Metrics::BASE.panel_width_utility)
        .with_child(Box::new(body));
    Popover::with_content(content)
        .side(PopoverSide::Right)
        .fallback_side(PopoverSide::Below)
        .confined_to_viewport()
        .with_label(t("agent-quick-accessible-name"))
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
        self.origin_context.set(t_args(
            "agent-quick-origin-context",
            &[("context", t("agent-quick-shots-context").into())],
        ));
        self.frozen_anchor = anchor;
        self.shown_intent = ComposerIntent::Message;
        self.intent.set(ComposerIntent::Message);
        self.draft.set(draft.to_owned());
        self.status.set(t("agent-quick-ready"));
        self.actions = None;
        self.tasks = None;
        self.custody = None;
        self.pending_message = None;
        self.pending_message_offer = None;
        self.message_offer = None;
        self.pending_work = None;
        self.permission_refused = false;
        self.popover.set_anchor(anchor);
        self.popover.set_avoid(Some(anchor));
        self.popover.set_viewport(viewport);
        self.popover.set_open(true);
        self.focus_reset = true;
        self.refresh_enabled();
    }

    pub(crate) fn open(
        &mut self,
        client: DesktopActionClient,
        room: String,
        agent: String,
        agent_label: String,
        origin: AgentQuickComposerOrigin,
        viewport: Rect,
    ) {
        let DesktopActionClient { actions, custody } = client;
        self.save_draft();
        if self.room == room
            && self.agent == agent
            && self.custody.as_ref() == Some(&custody)
            && self.actions.is_some()
        {
            self.agent_label = agent_label;
            self.title.set(t_args(
                "agent-quick-title",
                &[("agent", self.agent_label.clone().into())],
            ));
            self.origin_context.set(origin.context);
            self.frozen_anchor = origin.marker;
            self.popover.set_anchor(origin.marker);
            self.popover.set_avoid(Some(origin.protected));
            self.popover.set_viewport(viewport);
            self.popover.set_open(true);
            self.focus_reset = true;
            self.refresh_message_offer_for_user_navigation();
            self.refresh_status();
            self.refresh_enabled();
            return;
        }
        self.park_pending_session();
        // A user event from the old presentation cannot choose a task in
        // another room/Agent even if the new snapshot reuses its task id.
        self.task_selected_observed = self.task_selected.revision();
        self.displayed_tasks.clear();
        self.room = room;
        self.agent = agent;
        self.custody = Some(custody);
        self.agent_label = agent_label;
        self.title.set(t_args(
            "agent-quick-title",
            &[("agent", self.agent_label.clone().into())],
        ));
        self.frozen_anchor = origin.marker;
        self.origin_context.set(origin.context);
        self.permission_refused = false;
        if let Some(session) = self.retained_sessions.remove(&(
            self.custody.clone(),
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
        self.status.set(t("agent-quick-checking"));
        self.refresh_message_offer_for_user_navigation();
        self.popover.set_anchor(origin.marker);
        self.popover.set_avoid(Some(origin.protected));
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
                (
                    self.custody.clone(),
                    self.room.clone(),
                    self.agent.clone(),
                    self.shown_intent,
                ),
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

    pub(crate) fn take_task_center_request(&self) -> Option<TaskCenterRequest> {
        if !self.task_center_requested.get() {
            return None;
        }
        self.task_center_requested.set(false);
        let task = self.tasks.as_ref().and_then(|tasks| match tasks.choice() {
            AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => Some(task.task_id),
            AgentTaskChoice::None | AgentTaskChoice::Choose(_) => None,
        });
        Some(TaskCenterRequest { task_id: task })
    }

    pub(crate) fn target(&self) -> Option<(&str, &str)> {
        self.is_open().then_some((&self.room, &self.agent))
    }

    pub(crate) fn tick(&mut self) -> bool {
        if self.close_requested.get() {
            self.close_requested.set(false);
            return self.dismiss();
        }
        let before = (
            self.draft.revision(),
            self.intent.revision(),
            self.status.revision(),
            self.send_enabled.revision(),
            self.send_requested.revision(),
            self.task_visible.revision(),
            self.task_options.revision(),
            self.task_selected.revision(),
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
                    self.task_visible.revision(),
                    self.task_options.revision(),
                    self.task_selected.revision(),
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
        self.refresh_task_picker();
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
                self.task_visible.revision(),
                self.task_options.revision(),
                self.task_selected.revision(),
            )
    }

    fn switch_intent(&mut self, selected: ComposerIntent) {
        self.task_selected_observed = self.task_selected.revision();
        self.save_draft();
        let shared_actions = self.actions.clone();
        self.park_pending_session();
        self.shown_intent = selected;
        self.intent.set(selected);
        self.pending_message_offer = None;
        self.message_offer = None;
        self.permission_refused = false;
        if let Some(session) = self.retained_sessions.remove(&(
            self.custody.clone(),
            self.room.clone(),
            self.agent.clone(),
            selected,
        )) {
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
        self.refresh_message_offer_for_user_navigation();
    }

    fn refresh_task_picker(&mut self) {
        let key = self.draft_key();
        let available = self
            .tasks
            .as_ref()
            .map_or_else(Vec::new, AgentTaskFrontiers::available_tasks);
        self.apply_task_pick(&key, &available);
        self.restore_task_choice(&key, &available);
        self.project_task_picker(&key, &available);
    }

    fn apply_task_pick(&mut self, key: &DraftKey, available: &[AgentTaskFrontier]) {
        if self.task_selected.revision() != self.task_selected_observed {
            let requested = self.task_selected.get();
            self.task_selected_observed = self.task_selected.revision();
            let picked = self
                .displayed_tasks
                .iter()
                .find(|choice| choice.value() == requested);
            if self.shown_intent == ComposerIntent::Steer
                && self.pending_work.is_none()
                && let Some(picked) = picked
                && let Some(frontier) = available
                    .iter()
                    .find(|frontier| TaskChoice::from_frontier(frontier) == *picked)
                && let Some(tasks) = self.tasks.as_mut()
                && tasks
                    .select_frontier_for(&self.room, &self.agent, frontier)
                    .is_ok()
            {
                self.chosen_tasks.insert(key.clone(), picked.clone());
                self.reconfirm_tasks.remove(key);
            }
        }
    }

    fn restore_task_choice(&mut self, key: &DraftKey, available: &[AgentTaskFrontier]) {
        if let Some(chosen) = self.chosen_tasks.get(key) {
            if let Some(frontier) = available.iter().find(|frontier| {
                frontier.task_id == chosen.task_id && frontier.run_epoch == chosen.run_epoch
            }) {
                if let Some(tasks) = self.tasks.as_mut()
                    && self.pending_work.is_none()
                    && matches!(
                        tasks.choice(),
                        AgentTaskChoice::Choose(_) | AgentTaskChoice::None
                    )
                {
                    let _selected = tasks.select_frontier_for(&self.room, &self.agent, frontier);
                }
            } else if !available.is_empty() {
                self.chosen_tasks.remove(key);
                self.reconfirm_tasks.insert(key.clone());
            }
        }
        if self.shown_intent == ComposerIntent::Steer
            && !self.chosen_tasks.contains_key(key)
            && !self.reconfirm_tasks.contains(key)
            && let Some(AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task)) =
                self.tasks.as_ref().map(AgentTaskFrontiers::choice)
        {
            self.chosen_tasks
                .insert(key.clone(), TaskChoice::from_frontier(&task));
        }
    }

    fn project_task_picker(&mut self, key: &DraftKey, available: &[AgentTaskFrontier]) {
        let visible = self.shown_intent == ComposerIntent::Steer
            && self.pending_work.is_none()
            && !available.is_empty()
            && (available.len() > 1
                || self.reconfirm_tasks.contains(key)
                || self
                    .tasks
                    .as_ref()
                    .is_some_and(AgentTaskFrontiers::needs_frontier_confirmation));
        if self.task_visible.get() != visible {
            self.task_visible.set(visible);
        }
        self.displayed_tasks = available.iter().map(TaskChoice::from_frontier).collect();
        let options = available
            .iter()
            .zip(&self.displayed_tasks)
            .map(|(frontier, choice)| (choice.value(), task_option_label(frontier)))
            .collect();
        if self.task_options.get() != options {
            self.task_options.set(options);
        }
        let selected = if self.reconfirm_tasks.contains(key) {
            String::new()
        } else {
            self.tasks
                .as_ref()
                .map(AgentTaskFrontiers::choice)
                .and_then(|choice| match choice {
                    AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => {
                        Some(TaskChoice::from_frontier(&task).value())
                    }
                    AgentTaskChoice::None | AgentTaskChoice::Choose(_) => None,
                })
                .unwrap_or_default()
        };
        if self.task_selected.get() != selected {
            self.task_selected.set(selected);
        }
        self.task_selected_observed = self.task_selected.revision();
    }

    fn poll_retained_sessions(&mut self) {
        let drafts = &mut self.drafts;
        let receipts = &mut self.receipts;
        let now = std::time::Instant::now();
        self.retained_sessions
            .retain(|(custody, room, agent, intent), session| {
                session.actions.poll();
                if session.pending_work.is_some() {
                    session.tasks.observe(room, agent);
                }
                poll_retained_message(
                    drafts,
                    receipts,
                    custody.as_ref(),
                    room,
                    agent,
                    *intent,
                    session,
                );
                poll_retained_work(
                    drafts,
                    receipts,
                    custody.as_ref(),
                    room,
                    agent,
                    session,
                    now,
                );
                session.is_pending()
            });
    }

    fn submit(&mut self) {
        let text = self.draft.get();
        if text.trim().is_empty() {
            return;
        }
        let revision = self.draft.revision();
        self.receipts.remove(&self.draft_key());
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
                        follow_until: receipt_follow_deadline(),
                    });
                }
            }
            ComposerIntent::Steer => {
                if !self.reconfirm_tasks.contains(&self.draft_key())
                    && self
                        .tasks
                        .as_mut()
                        .is_some_and(|tasks| tasks.steer(&text).is_ok())
                {
                    self.pending_work = Some(PendingWork {
                        intent: ComposerIntent::Steer,
                        draft_revision: revision,
                        follow_until: receipt_follow_deadline(),
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
                    self.receipts.insert(
                        self.draft_key(),
                        ComposerReceipt {
                            submitted_revision: revision,
                            kind: ComposerReceiptKind::MessageAccepted,
                        },
                    );
                    self.start_message_offer_refresh();
                }
                AgentMessagePoll::Failed => {
                    self.pending_message = None;
                    self.permission_refused = true;
                    self.message_offer = None;
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
        if let Some(settled) = work_outcome(tasks, pending_work.intent) {
            self.pending_work = None;
            if let Ok(kind) = settled {
                self.clear_if_unchanged(pending_work.draft_revision);
                self.receipts.insert(
                    self.draft_key(),
                    ComposerReceipt {
                        submitted_revision: pending_work.draft_revision,
                        kind,
                    },
                );
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
        self.pending_message_offer = Some(PendingMessageOffer {
            state: ui::agent_message_action::begin(actions, &self.room),
            deadline: std::time::Instant::now()
                .checked_add(ui::agent_message_action::RESOLVE_WINDOW)
                .unwrap_or_else(std::time::Instant::now),
            pages_remaining: ui::agent_message_action::MAX_PAGES,
        });
    }

    fn refresh_message_offer_for_user_navigation(&mut self) {
        if self.shown_intent == ComposerIntent::Message
            && self.pending_message.is_none()
            && self.pending_message_offer.is_none()
        {
            self.start_message_offer_refresh();
        }
    }

    fn poll_message_offer(&mut self) {
        if self.pending_message.is_some() {
            return;
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
            }
            AgentMessageOfferPoll::Failed => {
                self.pending_message_offer = None;
                self.message_offer = None;
                self.permission_refused = true;
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
        let receipt_key = self.draft_key();
        let receipt = self
            .receipts
            .get(&receipt_key)
            .filter(|receipt| receipt.submitted_revision != self.draft.revision());
        let value = match self.shown_intent {
            ComposerIntent::Message => message_status(
                self.permission_refused,
                self.pending_message_offer.is_some(),
                receipt,
            ),
            ComposerIntent::Queue => queue_status(tasks, receipt),
            ComposerIntent::Steer
                if receipt.is_none() && self.reconfirm_tasks.contains(&receipt_key) =>
            {
                t("agent-quick-task-changed")
            }
            ComposerIntent::Steer => direction_status(tasks, receipt),
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
                .is_some_and(AgentTaskFrontiers::can_steer)
                && !self.reconfirm_tasks.contains(&self.draft_key()),
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

    fn draft_key(&self) -> DraftKey {
        (
            self.custody.clone(),
            self.room.clone(),
            self.agent.clone(),
            self.shown_intent,
        )
    }

    fn save_draft(&mut self) {
        if !self.room.is_empty() && !self.agent.is_empty() {
            let editor_state = self
                .pending_editor_state
                .as_ref()
                .filter(|(revision, _)| *revision == self.draft.revision())
                .map(|(_, state)| state.clone())
                .or_else(|| quick_editor_state(&self.popover));
            self.drafts.values.insert(
                self.draft_key(),
                DraftEntry {
                    text: self.draft.get(),
                    revision: self.draft.revision(),
                    editor_state,
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
        let editor_state = saved.as_ref().and_then(|entry| entry.editor_state.clone());
        // TextInput's LocalState sync installs the draft first. Restore the
        // shared selection snapshot only afterwards, and only for this text
        // revision: a late receipt or new edit must not restore stale offsets.
        self.pending_editor_state = Some((
            loaded_revision,
            editor_state
                .clone()
                .unwrap_or_else(|| self.draft.get().len().to_string()),
        ));
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
                editor_state,
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
                    editor_state: None,
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

    fn restore_pending_editor_state(&mut self) {
        if let Some((revision, state)) = self.pending_editor_state.take()
            && revision == self.draft.revision()
        {
            restore_quick_editor_state(&mut self.popover, &state);
        }
    }
}

fn clear_retained_draft(
    drafts: &mut Drafts,
    custody: Option<&ActionSessionCustody>,
    room: &str,
    agent: &str,
    intent: ComposerIntent,
    revision: u64,
) {
    let key = (custody.cloned(), room.to_owned(), agent.to_owned(), intent);
    let Some(entry) = drafts.values.get_mut(&key) else {
        return;
    };
    if entry.revision == revision {
        entry.text.clear();
        entry.revision = entry.revision.saturating_add(1);
        entry.editor_state = None;
    }
}

impl Widget for AgentQuickComposer {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.popover.sync(state, path);
        self.restore_pending_editor_state();
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let width = theme.tokens.metrics.panel_width_utility
            * (0.5 + 0.5 * theme.text_scale.clamp(1.0, 2.0));
        let width = width.min(self.popover.available_content_width(theme));
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
        let changed = self.tick();
        if changed {
            // `tick` projects asynchronous offer/receipt state into bound
            // LocalState after the frame's ordinary sync pass. Refresh the
            // retained children now so layout measures the new status in
            // this frame rather than painting it into the previous label's
            // one-line rect with no later wake guaranteed.
            self.popover.sync(state, path);
            self.restore_pending_editor_state();
        }
        changed | self.popover.update(state, path)
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
            self.actions.as_ref().and_then(|actions| {
                self.pending_message
                    .as_ref()
                    .and_then(|pending| pending_wake_deadline(actions, pending))
            }),
        );
        if self.is_open() {
            deadline = earliest(
                deadline,
                self.actions.as_ref().and_then(|actions| {
                    self.pending_message_offer.as_ref().and_then(|pending| {
                        ui::agent_message_action::next_wake_deadline(
                            actions,
                            &pending.state,
                            pending.deadline,
                        )
                    })
                }),
            );
        }
        if self.is_open() || self.pending_work.is_some() {
            deadline = earliest(
                deadline,
                self.tasks
                    .as_ref()
                    .and_then(AgentTaskFrontiers::next_wake_deadline),
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.popover
            .accessibility()
            .map(|node| node.with_semantic_key("agent-quick-composer"))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use taffy::{AvailableSpace, Size};
    use tilo_actions::remote::{
        ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
        RemoteActionMessage, RemoteActionTarget,
    };
    use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
    use ui::remote_actions::RemoteCall;
    use vello::{Scene, kurbo::Affine};

    fn perform_accessible_descendant_action(
        widget: &mut dyn Widget,
        target: &str,
        action: &ui::a11y::Action,
        edits: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if widget.test_id() == Some(target) {
            return widget.perform_action(action, &[], edits, &automerge::ROOT);
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| perform_accessible_descendant_action(child, target, action, edits))
    }

    fn editor_state(widget: &mut dyn Widget) -> Option<String> {
        if widget.test_id() == Some("agent-quick-editor") {
            return ui::undo::get_state(widget);
        }
        widget.children_mut().into_iter().find_map(editor_state)
    }

    fn has_accessible_value(widget: &dyn Widget, expected: &str) -> bool {
        widget
            .accessibility()
            .is_some_and(|node| node.value.as_deref() == Some(expected))
            || widget
                .children()
                .into_iter()
                .any(|child| has_accessible_value(child, expected))
    }

    struct MessageOfferHost {
        authorized: AtomicBool,
        queries: AtomicUsize,
        invokes: AtomicUsize,
    }

    impl MessageOfferHost {
        fn row() -> tilo_actions::remote::CatalogueRow {
            let resource = "chat.person:scribe".to_owned();
            let action = tilo_actions::ActionDescriptor {
                address: ActionAddress::new(&resource, "message"),
                semantic_identity: Some(tilo_actions::ResourceSemanticIdentity {
                    kind: "chat.agent".into(),
                    key: resource.clone(),
                }),
                resource_kind: "chat.agent".into(),
                resource_label: "Agent".into(),
                action_label: "Message".into(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract::default(),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            };
            tilo_actions::remote::CatalogueRow {
                provider: "chat-provider".into(),
                app_id: "so.tilo.chat".into(),
                parent_resource: resource.clone(),
                resource: tilo_actions::remote::CatalogueResource {
                    id: resource,
                    kind: "chat.agent".into(),
                    label: "Agent".into(),
                    presentation: None,
                },
                action: Some(action.clone()),
                has_children: false,
                fresh_binding: Some(tilo_actions::remote::FreshActionBinding {
                    descriptor: action,
                    operation_type: Some("chat.action".into()),
                }),
                visibility: None,
            }
        }
    }

    impl ActionHost for MessageOfferHost {
        fn query(
            &self,
            _caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            self.queries.fetch_add(1, Ordering::Relaxed);
            let mut page = CataloguePage {
                revision: "chat-r1".into(),
                exhausted: true,
                ..CataloguePage::default()
            };
            if query.canonical_route.is_some() {
                page.providers
                    .push(tilo_actions::remote::CatalogueProvider {
                        provider: "chat-provider".into(),
                        app_id: "so.tilo.chat".into(),
                        room: "room-a".into(),
                        record_id: String::new(),
                        browsable: true,
                    });
            } else if query.provider.as_deref() == Some("chat-provider")
                && query.target.is_none()
                && query.needle == "message"
                && self.authorized.load(Ordering::Relaxed)
            {
                page.rows.push(Self::row());
            }
            Pending::ready(Ok(page))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            if !self.authorized.load(Ordering::Relaxed) {
                return Pending::ready(Err(ActionHostError::NotPermitted));
            }
            let row = Self::row();
            if request.revision.is_some()
                || request.target != row.target().expect("Message fixture target")
                || request.fresh_binding != row.fresh_binding
            {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "fixture requires the current Message binding".into(),
                }));
            }
            self.invokes.fetch_add(1, Ordering::Relaxed);
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
            Err(ActionHostError::NotCancellable {
                reason: "fixture actions settle immediately".into(),
            })
        }
    }

    fn message_offer_composer(host: Arc<MessageOfferHost>) -> AgentQuickComposer {
        let actions = ui::remote_actions::RemoteActions::with_host(
            host,
            Caller::local_person("owner").in_room("room-a"),
        );
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            actions: Some(actions),
            shown_intent: ComposerIntent::Message,
            ..AgentQuickComposer::default()
        };
        composer.popover.set_open(true);
        composer.refresh_message_offer_for_user_navigation();
        composer
    }

    fn settle_message_offer(composer: &mut AgentQuickComposer) {
        for _ in 0..16 {
            composer.tick();
            if composer.pending_message_offer.is_none() {
                return;
            }
        }
        panic!("bounded Message offer did not settle");
    }

    struct CompletedTaskHost {
        applied_revision: Arc<AtomicU64>,
    }

    impl ActionHost for CompletedTaskHost {
        fn query(
            &self,
            _caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            let provider = query.provider.unwrap_or_default();
            let target = query.target.expect("exact task control query");
            let resource = target.resource.clone();
            Pending::ready(Ok(CataloguePage {
                revision: "task-controls-1".into(),
                rows: vec![tilo_actions::remote::CatalogueRow {
                    provider,
                    app_id: "task-center".into(),
                    parent_resource: String::new(),
                    resource: tilo_actions::remote::CatalogueResource {
                        id: resource,
                        kind: "desktop.task".into(),
                        label: "Task".into(),
                        presentation: None,
                    },
                    action: Some(tilo_actions::ActionDescriptor {
                        address: target,
                        semantic_identity: None,
                        resource_kind: "desktop.task".into(),
                        resource_label: "Task".into(),
                        action_label: "Control".into(),
                        description: None,
                        locus: tilo_actions::ExecutionLocus::ClientLocal,
                        kind: tilo_actions::OperationKind::Document,
                        contract: tilo_actions::ActionContract::default(),
                        input: tilo_actions::InputSpec::None,
                        params: Vec::new(),
                        view: None,
                    }),
                    has_children: false,
                    fresh_binding: None,
                    visibility: None,
                }],
                exhausted: true,
                ..CataloguePage::default()
            }))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            let action = request.target.address.action.as_str();
            let value = match action {
                "read-current-tasks" => serde_json::json!({
                    "generation": 1,
                    "complete": true,
                    "total": 1,
                    "tasks": [{
                        "task_id": "task-a",
                        "title": "Release notes",
                        "assigned_agent": "scribe",
                        "status": "in_progress",
                        "run_epoch": 7,
                        "active_agent": "scribe",
                        "after_revision": 4,
                        "applied_revision": self.applied_revision.load(Ordering::Relaxed),
                        "route": "model",
                        "cancellation_requested": false
                    }],
                    "pending_complete": true,
                    "pending_total": 0,
                    "pending": []
                }),
                "queue" => serde_json::json!({
                    "result": {"queued": {"task_id": "queued-a"}},
                    "delivery": "queued"
                }),
                "direct" => serde_json::json!({
                    "result": {"direction_added": {"task_id": "task-a", "revision": 5}}
                }),
                "read-task" => serde_json::json!({
                    "generation": 2,
                    "task_id": "task-a",
                    "title": "Release notes",
                    "route": "model",
                    "cancellation_requested": false,
                    "status": "in_progress",
                    "assigned_agent": "scribe",
                    "active_agent": "scribe",
                    "run_epoch": 7,
                    "after_revision": 5,
                    "applied_revision": self.applied_revision.load(Ordering::Relaxed)
                }),
                _ => {
                    return Pending::ready(Err(ActionHostError::InvalidRequest {
                        reason: "unexpected task action".into(),
                    }));
                }
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
            Err(ActionHostError::NotCancellable {
                reason: "fixture actions settle immediately".into(),
            })
        }
    }

    fn task_composer(applied_revision: u64) -> AgentQuickComposer {
        task_composer_with_applied(applied_revision).0
    }

    struct ChoiceTaskHost {
        tasks: std::sync::Mutex<Vec<serde_json::Value>>,
        requests: std::sync::Mutex<Vec<InvokeRequest>>,
    }

    impl ActionHost for ChoiceTaskHost {
        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            CompletedTaskHost {
                applied_revision: Arc::new(AtomicU64::new(4)),
            }
            .query(caller, query)
        }

        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            self.requests.lock().unwrap().push(request.clone());
            let tasks = self.tasks.lock().unwrap().clone();
            let value = match request.target.address.action.as_str() {
                "read-current-tasks" => serde_json::json!({
                    "generation": 1, "complete": true, "total": tasks.len(), "tasks": tasks,
                    "pending_complete": true, "pending_total": 0, "pending": []
                }),
                "direct" => {
                    let id = request
                        .target
                        .address
                        .resource
                        .strip_prefix("desktop:task:")
                        .unwrap();
                    serde_json::json!({"result": {"direction_added": {"task_id": id, "revision": 5}}})
                }
                "read-task" => {
                    let id = request
                        .target
                        .address
                        .resource
                        .strip_prefix("desktop:task:")
                        .unwrap();
                    let mut task = tasks
                        .iter()
                        .find(|task| task["task_id"] == id)
                        .unwrap()
                        .clone();
                    task["generation"] = serde_json::json!(1);
                    task
                }
                _ => panic!("unexpected task action"),
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }
        fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
            Err(ActionHostError::NotCancellable {
                reason: "immediate fixture action".into(),
            })
        }
    }

    fn choice_task(id: &str, title: &str, epoch: u64) -> serde_json::Value {
        serde_json::json!({
            "task_id": id, "title": title, "assigned_agent": "scribe",
            "status": "in_progress", "run_epoch": epoch, "active_agent": "scribe",
            "after_revision": 4, "applied_revision": 4, "route": "model", "cancellation_requested": false,
            "execution": {"run_epoch": epoch, "sequence": 1, "phase": "planning", "updated_at": 12}
        })
    }

    fn choice_composer() -> (AgentQuickComposer, Arc<ChoiceTaskHost>) {
        let host = Arc::new(ChoiceTaskHost {
            tasks: std::sync::Mutex::new(vec![
                choice_task("task-a", "Release notes", 7),
                choice_task("task-b", "Review changes", 11),
            ]),
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let actions = ui::remote_actions::RemoteActions::with_host(
            host.clone(),
            Caller::local_app("so.tilo.chat").in_room("room-a"),
        );
        let mut tasks = AgentTaskFrontiers::with_actions(actions.clone());
        tasks.observe("room-a", "scribe");
        let mut composer = AgentQuickComposer {
            room: "room-a".into(),
            agent: "scribe".into(),
            actions: Some(actions),
            tasks: Some(tasks),
            shown_intent: ComposerIntent::Steer,
            ..AgentQuickComposer::default()
        };
        composer.intent.set(ComposerIntent::Steer);
        composer.set_draft("Keep the compatibility notes");
        composer
            .popover
            .set_anchor(Rect::new(100.0, 140.0, 120.0, 160.0));
        composer
            .popover
            .set_viewport(Rect::new(0.0, 0.0, 900.0, 700.0));
        composer.popover.set_open(true);
        composer.tick();
        (composer, host)
    }

    #[test]
    fn task_picker_keyboard_selects_exact_current_run_and_preserves_editor() {
        let (composer, host) = choice_composer();
        let mut frame =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(composer), (900, 700));
        assert!(
            !frame
                .widget_mut::<AgentQuickComposer>()
                .unwrap()
                .send_enabled
                .get()
        );
        frame.click_named("agent-quick-editor").unwrap();
        frame.key("End");
        frame.set_modifiers(&["shift".to_owned()]);
        frame.key("ArrowLeft");
        frame.set_modifiers(&[]);
        let selected = editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap());
        assert!(
            selected.is_some(),
            "the mounted editor exposes caret/selection state"
        );
        frame.click_named("agent-quick-task").unwrap();
        frame.key("ArrowDown");
        frame.key("Enter");
        let composer = frame.widget_mut::<AgentQuickComposer>().unwrap();
        assert!(
            matches!(composer.tasks.as_ref().unwrap().choice(), AgentTaskChoice::Selected(task) if task.task_id == "task-b" && task.run_epoch == 11)
        );
        assert_eq!(composer.draft(), "Keep the compatibility notes");
        assert_eq!(
            editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()),
            selected
        );
        frame.click_named("agent-quick-send").unwrap();
        let directs: Vec<_> = host
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.target.address.action == "direct")
            .cloned()
            .collect();
        assert_eq!(directs.len(), 1);
        assert_eq!(directs[0].target.address.resource, "desktop:task:task-b");
        let InvokeArgument::Json(argument) = &directs[0].argument else {
            panic!("typed direction")
        };
        assert_eq!(argument["run_epoch"], 11);
        assert_eq!(argument["after_revision"], 4);
    }

    #[test]
    fn task_picker_public_set_value_uses_the_shared_control_without_submitting_work() {
        let (composer, host) = choice_composer();
        let value = composer.displayed_tasks[1].value();
        let mut frame =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(composer), (900, 700));
        let resources = frame.resources();
        let picker = resources
            .iter()
            .find(|resource| {
                resource.label == t("agent-quick-task-label")
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "set-value")
            })
            .expect("published shared task picker");
        frame
            .invoke_action(
                &ActionAddress::new(picker.id.0.clone(), "set-value"),
                &serde_json::Map::from_iter([("value".into(), serde_json::json!(value))]),
                false,
                None,
            )
            .unwrap();
        frame.with_widget_mut::<AgentQuickComposer, _>(|composer| {
            composer.tick();
            assert!(matches!(composer.tasks.as_ref().unwrap().choice(), AgentTaskChoice::Selected(task) if task.task_id == "task-b" && task.run_epoch == 11));
            assert_eq!(composer.draft(), "Keep the compatibility notes");
        }).unwrap();
        assert!(
            !host
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.target.address.action == "direct")
        );
    }

    #[test]
    fn task_picker_accessibility_refuses_stale_displayed_run_and_preserves_intent_draft() {
        let (mut composer, host) = choice_composer();
        let old_value = composer.displayed_tasks[1].value();
        layout_panel(
            &mut composer,
            Rect::new(100.0, 140.0, 120.0, 160.0),
            Rect::new(0.0, 0.0, 900.0, 700.0),
            &Theme::dark(),
        );
        let mut doc = automerge::AutoCommit::new();
        composer.sync(&doc, &automerge::ROOT);
        assert!(perform_accessible_descendant_action(
            &mut composer,
            "agent-quick-task",
            &ui::a11y::Action::SetValue(old_value.clone()),
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        composer.tick();
        assert_eq!(
            composer
                .tasks
                .as_ref()
                .unwrap()
                .context_task()
                .unwrap()
                .task_id,
            "task-b"
        );
        composer.switch_intent(ComposerIntent::Queue);
        composer.set_draft("Separate queue draft");
        composer.switch_intent(ComposerIntent::Steer);
        composer.tick();
        assert_eq!(composer.draft(), "Keep the compatibility notes");
        assert_eq!(
            composer
                .tasks
                .as_ref()
                .unwrap()
                .context_task()
                .unwrap()
                .run_epoch,
            11
        );
        {
            let mut tasks = host.tasks.lock().unwrap();
            tasks[1]["after_revision"] = serde_json::json!(6);
            tasks[1]["execution"]["phase"] = serde_json::json!("executing");
        }
        composer.tasks.as_mut().unwrap().refresh();
        composer.tick();
        assert!(
            composer.send_enabled.get(),
            "same-run progress retains the explicit task choice"
        );
        *host.tasks.lock().unwrap() = vec![choice_task("task-b", "Review changes", 12)];
        composer.tasks.as_mut().unwrap().refresh();
        composer.tick();
        assert!(!composer.send_enabled.get());
        assert!(composer.task_selected.get().is_empty());
        assert!(composer.task_visible.get());
        composer.task_selected.set(old_value);
        composer.tick();
        composer.submit();
        assert!(
            !host
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.target.address.action == "direct")
        );
        assert_eq!(composer.draft(), "Keep the compatibility notes");
        composer.sync(&doc, &automerge::ROOT);
        let replacement = composer.displayed_tasks[0].value();
        assert!(perform_accessible_descendant_action(
            &mut composer,
            "agent-quick-task",
            &ui::a11y::Action::SetValue(replacement),
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        composer.tick();
        assert!(composer.send_enabled.get());
        composer.submit();
        let directs: Vec<_> = host
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.target.address.action == "direct")
            .cloned()
            .collect();
        assert_eq!(directs.len(), 1);
        let InvokeArgument::Json(argument) = &directs[0].argument else {
            panic!("typed direction")
        };
        assert_eq!(argument["run_epoch"], 12);
    }

    #[test]
    fn task_picker_render_component_matrix() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/agent-task-choice-renders");
        std::fs::create_dir_all(&out).unwrap();
        for (name, theme) in [
            ("light", Theme::light()),
            ("dark", Theme::dark()),
            ("hc200", Theme::high_contrast().with_text_scale(2.0)),
        ] {
            for (size, viewport) in [("normal", (1600, 900)), ("compact", (560, 560))] {
                let (mut composer, host) = choice_composer();
                composer
                    .title
                    .set(t_args("agent-quick-title", &[("agent", "Scribe".into())]));
                composer
                    .popover
                    .set_anchor(Rect::new(80.0, 90.0, 100.0, 110.0));
                composer.popover.set_viewport(Rect::new(
                    0.0,
                    0.0,
                    f64::from(viewport.0),
                    f64::from(viewport.1),
                ));
                let mut frame = ui::headless::FrameHarness::new_with_theme(
                    ui::State::new(),
                    Box::new(composer),
                    viewport,
                    theme.clone(),
                );
                frame.click_named("agent-quick-editor").unwrap();
                frame.key("End");
                frame.set_modifiers(&["shift".to_owned()]);
                frame.key("ArrowLeft");
                frame.set_modifiers(&[]);
                frame.render_png(&out.join(format!("{name}-{size}-choose.png")));
                let selection = editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap());
                assert!(
                    selection.is_some(),
                    "the mounted editor exposes caret/selection state"
                );
                frame.click_named("agent-quick-task").unwrap();
                frame.render_png(&out.join(format!("{name}-{size}-open.png")));
                frame.key("ArrowDown");
                frame.key("Enter");
                frame
                    .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                        composer
                            .tasks
                            .as_mut()
                            .unwrap()
                            .select_for("room-a", "scribe", "task-b")
                            .unwrap();
                        composer.tick();
                    })
                    .unwrap();
                frame.render_png(&out.join(format!("{name}-{size}-selected.png")));
                host.tasks.lock().unwrap()[1] = choice_task("task-b", "Review changes", 12);
                frame
                    .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                        let tasks = composer.tasks.as_mut().unwrap();
                        tasks.set_observation_active(false);
                        tasks.set_observation_active(true);
                        tasks.observe("room-a", "scribe");
                        composer.tick();
                        assert!(!composer.send_enabled.get());
                        assert_eq!(composer.draft(), "Keep the compatibility notes");
                    })
                    .unwrap();
                assert_eq!(
                    editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()),
                    selection
                );
                frame.render_png(&out.join(format!("{name}-{size}-stale.png")));
            }
        }
    }

    fn task_composer_with_applied(applied_revision: u64) -> (AgentQuickComposer, Arc<AtomicU64>) {
        let applied_revision = Arc::new(AtomicU64::new(applied_revision));
        let actions = ui::remote_actions::RemoteActions::with_host(
            Arc::new(CompletedTaskHost {
                applied_revision: applied_revision.clone(),
            }),
            Caller::local_app("so.tilo.chat").in_room("room-a"),
        );
        let mut tasks = AgentTaskFrontiers::with_actions(actions.clone());
        tasks.observe("room-a", "scribe");
        (
            AgentQuickComposer {
                room: "room-a".into(),
                agent: "scribe".into(),
                actions: Some(actions),
                tasks: Some(tasks),
                ..AgentQuickComposer::default()
            },
            applied_revision,
        )
    }

    fn layout_panel(
        composer: &mut AgentQuickComposer,
        anchor: Rect,
        viewport: Rect,
        theme: &Theme,
    ) -> Rect {
        composer.popover.set_anchor(anchor);
        composer.popover.set_viewport(viewport);
        composer.popover.set_open(true);
        let state = ui::State::new();
        composer.sync(&state.doc, &state.root);
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

    fn assert_opened_context_layout(origin: &dyn Widget, intent: &dyn Widget, theme: &Theme) {
        assert!(
            origin.get_rect().width() + 0.5 >= intent.get_rect().width(),
            "context {:?} must use the body width established by intent {:?}",
            origin.get_rect(),
            intent.get_rect()
        );
        let context = t_args(
            "agent-quick-origin-context",
            &[("context", t("agent-quick-shots-context").into())],
        );
        let style = theme
            .text(ui::tokens::TextRole::Body)
            .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph);
        let (_, measured_height) = ui::text_render::TextRenderer::new().measure_style(
            &context,
            Some(logical_dimension(origin.get_rect().width() + 1.0)),
            &style,
        );
        // Taffy pixel-rounds the absolute top and bottom independently, so
        // their difference can vary from fractional text height by < 1 px.
        let height_delta = origin.get_rect().height() - f64::from(measured_height);
        assert!(
            height_delta.abs() < 1.0 && origin.get_rect().y1 <= intent.get_rect().y0,
            "context {:?} must match its measured {measured_height}px within Taffy's edge rounding before intent {:?}",
            origin.get_rect(),
            intent.get_rect()
        );
        assert_eq!(
            origin
                .accessibility()
                .and_then(|node| node.name.or(node.value)),
            Some(context),
            "measured wrapping preserves the complete localized context"
        );
    }

    #[test]
    fn task_center_button_uses_shared_accessibility_activation() {
        let mut composer = task_composer(4);
        let viewport = Rect::new(0.0, 0.0, 900.0, 700.0);
        layout_panel(
            &mut composer,
            Rect::new(300.0, 200.0, 320.0, 220.0),
            viewport,
            &Theme::dark(),
        );
        let mut doc = automerge::AutoCommit::new();
        assert!(perform_accessible_descendant_action(
            &mut composer,
            "agent-quick-open-tasks",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
        ));
        assert_eq!(
            composer.take_task_center_request(),
            Some(TaskCenterRequest {
                task_id: Some("task-a".into())
            })
        );
        assert_eq!(composer.take_task_center_request(), None);
    }

    #[test]
    fn accessible_close_retains_the_private_draft_for_reopen() {
        let mut composer = task_composer(4);
        composer.set_draft("Keep the release note wording");
        let viewport = Rect::new(0.0, 0.0, 900.0, 700.0);
        layout_panel(
            &mut composer,
            Rect::new(300.0, 200.0, 320.0, 220.0),
            viewport,
            &Theme::high_contrast(),
        );
        let mut doc = automerge::AutoCommit::new();
        assert!(perform_accessible_descendant_action(
            &mut composer,
            "agent-quick-close",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
        ));
        assert!(
            composer.tick(),
            "the shared close action changes presentation"
        );
        assert!(!composer.is_open());

        composer.popover.set_open(true);
        composer.load_draft_and_rebind_pending_revision();
        assert_eq!(composer.draft(), "Keep the release note wording");
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

    fn private_editor_session() -> (
        super::super::certified_action_session::DesktopActionSession,
        String,
    ) {
        use super::super::certified_action_session::ActionSessionSource;
        use super::super::state::{Workspace, WorkspaceId, WorkspaceTree};

        let mut workspace = Workspace::new(WorkspaceId(191), "Private editor");
        workspace.set_owner("person-a");
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let source = ActionSessionSource::fixture(
            super::super::broker_services::test_broker_with_membership(tree.shared()),
        );
        (source.open(&room, Some("person-a")).unwrap(), room)
    }

    fn select_private_suffix(frame: &mut ui::headless::FrameHarness, count: usize) -> String {
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.key("End");
        frame.set_modifiers(&["shift".to_owned()]);
        for _ in 0..count {
            frame.key("ArrowLeft");
        }
        frame.set_modifiers(&[]);
        editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()).unwrap()
    }

    #[test]
    fn private_quick_editor_restores_selection_after_intent_switch_and_continues_typing() {
        let (composer, _) = choice_composer();
        let mut frame =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(composer), (900, 700));
        let steer = select_private_suffix(&mut frame, 5);
        frame
            .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                composer.intent.set(ComposerIntent::Queue);
            })
            .unwrap();
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("Queued note");
        let queue = select_private_suffix(&mut frame, 4);
        assert_ne!(queue, steer);
        frame
            .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                composer.intent.set(ComposerIntent::Steer);
            })
            .unwrap();
        assert_eq!(
            editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()),
            Some(steer)
        );
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("text");
        assert_eq!(
            frame.widget_mut::<AgentQuickComposer>().unwrap().draft(),
            "Keep the compatibility text"
        );
        frame
            .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                composer.intent.set(ComposerIntent::Queue);
            })
            .unwrap();
        assert_eq!(
            editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()),
            Some(queue)
        );
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("work");
        assert_eq!(
            frame.widget_mut::<AgentQuickComposer>().unwrap().draft(),
            "Queued work"
        );
    }

    #[test]
    fn private_quick_editor_restores_each_agents_selection_through_production_open() {
        let (session, room) = private_editor_session();
        let anchor = Rect::new(80.0, 80.0, 100.0, 100.0);
        let viewport = Rect::new(0.0, 0.0, 900.0, 700.0);
        let mut composer = AgentQuickComposer::default();
        composer.open(
            session.action_client(),
            room.clone(),
            "scribe".into(),
            "Scribe".into(),
            AgentQuickComposerOrigin::marker_only(anchor),
            viewport,
        );
        let mut frame =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(composer), (900, 700));
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("Scribe draft");
        let scribe = select_private_suffix(&mut frame, 5);
        frame
            .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                composer.open(
                    session.action_client(),
                    room.clone(),
                    "reed".into(),
                    "Reed".into(),
                    AgentQuickComposerOrigin::marker_only(anchor),
                    viewport,
                );
            })
            .unwrap();
        assert_eq!(
            frame.widget_mut::<AgentQuickComposer>().unwrap().draft(),
            ""
        );
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("Reed direction");
        let reed = select_private_suffix(&mut frame, 9);
        frame
            .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                composer.open(
                    session.action_client(),
                    room.clone(),
                    "scribe".into(),
                    "Scribe".into(),
                    AgentQuickComposerOrigin::marker_only(anchor),
                    viewport,
                );
            })
            .unwrap();
        assert_eq!(
            frame.widget_mut::<AgentQuickComposer>().unwrap().draft(),
            "Scribe draft"
        );
        assert_eq!(
            editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()),
            Some(scribe)
        );
        frame
            .with_widget_mut::<AgentQuickComposer, _>(|composer| {
                composer.open(
                    session.action_client(),
                    room.clone(),
                    "reed".into(),
                    "Reed".into(),
                    AgentQuickComposerOrigin::marker_only(anchor),
                    viewport,
                );
            })
            .unwrap();
        assert_eq!(
            frame.widget_mut::<AgentQuickComposer>().unwrap().draft(),
            "Reed direction"
        );
        assert_eq!(
            editor_state(frame.widget_mut::<AgentQuickComposer>().unwrap()),
            Some(reed)
        );
        frame
            .perform_named("agent-quick-editor", &ui::a11y::Action::Focus)
            .unwrap();
        frame.type_text("message");
        assert_eq!(
            frame.widget_mut::<AgentQuickComposer>().unwrap().draft(),
            "Reed message"
        );
    }

    #[test]
    fn same_room_agent_keeps_drafts_and_pending_submission_private_per_identity_custody() {
        use crate::shell::certified_action_session::ActionSessionSource;
        use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

        let mut workspace = Workspace::new(WorkspaceId(190), "Custody room");
        workspace.set_owner("person-a");
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let source = ActionSessionSource::fixture(
            crate::shell::broker_services::test_broker_with_membership(tree.shared()),
        );
        let session_a = source
            .open(&room, Some("person-a"))
            .expect("first fixture identity");
        let session_b = source
            .open(&room, Some("person-b"))
            .expect("second fixture identity");
        drop(source);
        let custody_a = session_a.custody();
        let custody_b = session_b.custody();
        assert!(custody_a != custody_b);

        let mut composer = AgentQuickComposer::default();
        let anchor = Rect::new(80.0, 80.0, 100.0, 100.0);
        let viewport = Rect::new(0.0, 0.0, 900.0, 700.0);
        composer.open(
            session_a.action_client(),
            room.clone(),
            "scribe".into(),
            "Scribe".into(),
            AgentQuickComposerOrigin::marker_only(anchor),
            viewport,
        );
        composer.set_draft("person A private direction");
        let invocation = InvocationId(uuid::Uuid::from_bytes([0x6d; 16]));
        let actions = composer.actions.as_ref().expect("first actions");
        let call = actions.invoke_with_id(
            invocation,
            RemoteActionTarget {
                provider: "chat-provider".into(),
                address: ActionAddress::new("chat.person:scribe", "message"),
            },
            InvokeArgument::Text("person A private direction".into()),
            None,
        );
        composer.pending_message = Some(PendingMessage {
            state: AgentMessageSubmission::Invoke { invocation, call },
            deadline: receipt_follow_deadline(),
            pages_remaining: 1,
            text: "person A private direction".into(),
            draft_revision: composer.draft.revision(),
            outcome_uncertain: false,
        });

        composer.open(
            session_b.action_client(),
            room.clone(),
            "scribe".into(),
            "Scribe".into(),
            AgentQuickComposerOrigin::marker_only(anchor),
            viewport,
        );
        assert!(composer.custody.as_ref() == Some(&custody_b));
        assert_eq!(composer.draft(), "");
        assert!(composer.pending_message.is_none());
        composer.set_draft("person B private direction");

        composer.open(
            session_a.action_client(),
            room.clone(),
            "scribe".into(),
            "Scribe".into(),
            AgentQuickComposerOrigin::marker_only(anchor),
            viewport,
        );
        drop(session_a);
        assert!(composer.custody.as_ref() == Some(&custody_a));
        assert_eq!(composer.draft(), "person A private direction");
        assert!(composer.pending_message.is_some());

        composer.open(
            session_b.action_client(),
            room.clone(),
            "scribe".into(),
            "Scribe".into(),
            AgentQuickComposerOrigin::marker_only(anchor),
            viewport,
        );
        assert!(composer.custody.as_ref() == Some(&custody_b));
        assert_eq!(composer.draft(), "person B private direction");
        assert!(composer.pending_message.is_none());
        assert!(composer.dismiss());
        composer.open(
            session_b.action_client(),
            room,
            "scribe".into(),
            "Scribe".into(),
            AgentQuickComposerOrigin::marker_only(anchor),
            viewport,
        );
        drop(session_b);
        assert_eq!(composer.draft(), "person B private direction");
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
        let submitted_revision = composer.draft.revision();
        composer.pending_message = Some(PendingMessage {
            state: AgentMessageSubmission::Invoke { invocation, call },
            deadline: std::time::Instant::now(),
            pages_remaining: 0,
            text: "Review the release notes".into(),
            draft_revision: submitted_revision,
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
        let submitted_revision = composer.draft.revision();
        composer.pending_message = Some(PendingMessage {
            state: AgentMessageSubmission::Invoke { invocation, call },
            deadline: std::time::Instant::now(),
            pages_remaining: 0,
            text: "Review the release notes".into(),
            draft_revision: submitted_revision,
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
        assert_eq!(
            composer.receipts.get(&(
                None,
                "room-a".into(),
                "scribe".into(),
                ComposerIntent::Message,
            )),
            Some(&ComposerReceipt {
                submitted_revision,
                kind: ComposerReceiptKind::MessageAccepted,
            })
        );
        let saved = composer
            .drafts
            .values
            .get(&(
                None,
                "room-a".into(),
                "scribe".into(),
                ComposerIntent::Message,
            ))
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
    fn hidden_queue_receipt_survives_return_and_does_not_claim_execution() {
        let mut composer = task_composer(4);
        composer.shown_intent = ComposerIntent::Queue;
        composer.set_draft("Prepare the launch checklist");
        let submitted_revision = composer.draft.revision();
        let actions = composer.actions.clone().expect("task actions");
        composer.submit();
        assert!(composer.pending_work.is_some());
        composer.save_draft();
        composer.park_pending_session();
        composer.poll_retained_sessions();

        let key = (
            None,
            "room-a".into(),
            "scribe".into(),
            ComposerIntent::Queue,
        );
        assert!(composer.retained_sessions.is_empty());
        assert_eq!(
            composer.receipts.get(&key),
            Some(&ComposerReceipt {
                submitted_revision,
                kind: ComposerReceiptKind::Queued {
                    task_id: "queued-a".into()
                }
            })
        );
        assert_eq!(
            composer
                .drafts
                .values
                .get(&key)
                .map(|draft| draft.text.as_str()),
            Some("")
        );

        composer.shown_intent = ComposerIntent::Queue;
        composer.tasks = Some(AgentTaskFrontiers::with_actions(actions));
        composer.load_draft_and_rebind_pending_revision();
        composer.refresh_status();
        assert_eq!(
            composer.status.get(),
            "Earlier work was queued in Task Center."
        );
    }

    #[test]
    fn hidden_saved_direction_receipt_keeps_a_later_edit_and_original_run_binding() {
        let mut composer = task_composer(4);
        composer.shown_intent = ComposerIntent::Steer;
        composer.set_draft("Use the reviewed wording");
        let submitted_revision = composer.draft.revision();
        let actions = composer.actions.clone().expect("task actions");
        composer.submit();
        assert!(composer.pending_work.is_some());
        composer
            .pending_work
            .as_mut()
            .expect("saved direction follow window")
            .follow_until = std::time::Instant::now();
        composer.save_draft();
        composer.park_pending_session();
        let key = (
            None,
            "room-a".into(),
            "scribe".into(),
            ComposerIntent::Steer,
        );
        composer.drafts.values.insert(
            key.clone(),
            DraftEntry {
                text: "Keep the legal note too".into(),
                revision: submitted_revision.saturating_add(100),
                editor_state: None,
            },
        );

        composer.poll_retained_sessions();

        assert!(composer.retained_sessions.is_empty());
        assert_eq!(
            composer.receipts.get(&key),
            Some(&ComposerReceipt {
                submitted_revision,
                kind: ComposerReceiptKind::DirectionSaved {
                    task_id: "task-a".into(),
                    run_epoch: 7,
                    revision: 5,
                }
            })
        );
        assert_eq!(
            composer
                .drafts
                .values
                .get(&key)
                .map(|draft| draft.text.as_str()),
            Some("Keep the legal note too")
        );

        composer.shown_intent = ComposerIntent::Steer;
        composer.tasks = Some(AgentTaskFrontiers::with_actions(actions));
        composer.load_draft_and_rebind_pending_revision();
        composer.refresh_status();
        assert_eq!(
            composer.status.get(),
            "Earlier direction was saved in Task Center."
        );
    }

    #[test]
    fn hidden_applied_direction_receipt_is_not_downgraded_to_saved() {
        let (mut composer, applied_revision) = task_composer_with_applied(4);
        composer.shown_intent = ComposerIntent::Steer;
        composer.set_draft("Use the reviewed wording");
        let actions = composer.actions.clone().expect("task actions");
        composer.submit();
        composer.save_draft();
        composer.park_pending_session();
        composer.poll_retained_sessions();

        let key = (
            None,
            "room-a".into(),
            "scribe".into(),
            ComposerIntent::Steer,
        );
        assert_eq!(composer.retained_sessions.len(), 1);
        assert!(matches!(
            composer.receipts.get(&key).map(|receipt| &receipt.kind),
            Some(ComposerReceiptKind::DirectionSaved { .. })
        ));
        applied_revision.store(5, Ordering::Relaxed);
        composer
            .retained_sessions
            .values_mut()
            .next()
            .expect("saved direction remains bounded for owner confirmation")
            .tasks
            .refresh_direction_status()
            .expect("re-read the exact saved direction");
        composer.poll_retained_sessions();

        assert!(composer.retained_sessions.is_empty());
        assert!(matches!(
            composer.receipts.get(&key).map(|receipt| &receipt.kind),
            Some(ComposerReceiptKind::DirectionApplied {
                task_id,
                run_epoch: 7,
                revision: 5,
            }) if task_id == "task-a"
        ));

        composer.shown_intent = ComposerIntent::Steer;
        composer.tasks = Some(AgentTaskFrontiers::with_actions(actions));
        composer.load_draft_and_rebind_pending_revision();
        composer.refresh_status();

        assert_eq!(
            composer.status.get(),
            "The agent applied earlier direction."
        );
    }

    #[test]
    fn applied_receipt_from_another_run_cannot_hide_the_new_saved_receipt() {
        let (mut composer, _) = task_composer_with_applied(4);
        composer.shown_intent = ComposerIntent::Steer;
        composer.set_draft("Use the new run wording");
        composer.submit();
        let submitted_revision = composer
            .pending_work
            .expect("new direction submission")
            .draft_revision;
        composer.save_draft();
        composer.park_pending_session();
        let key = (
            None,
            "room-a".into(),
            "scribe".into(),
            ComposerIntent::Steer,
        );
        composer.receipts.insert(
            key.clone(),
            ComposerReceipt {
                submitted_revision,
                kind: ComposerReceiptKind::DirectionApplied {
                    task_id: "retired-task".into(),
                    run_epoch: 2,
                    revision: 5,
                },
            },
        );

        composer.poll_retained_sessions();

        assert_eq!(
            composer.receipts.get(&key),
            Some(&ComposerReceipt {
                submitted_revision,
                kind: ComposerReceiptKind::DirectionSaved {
                    task_id: "task-a".into(),
                    run_epoch: 7,
                    revision: 5,
                },
            }),
            "only the exact original receipt may suppress its Saved predecessor"
        );
    }

    #[test]
    fn historical_direction_receipt_never_rebinds_to_a_newer_frontier() {
        let mut composer = task_composer(4);
        composer.shown_intent = ComposerIntent::Steer;
        composer.set_draft("New draft for the replacement task");
        let current_revision = composer.draft.revision();
        composer.receipts.insert(
            composer.draft_key(),
            ComposerReceipt {
                submitted_revision: current_revision.saturating_sub(1),
                kind: ComposerReceiptKind::DirectionSaved {
                    task_id: "retired-task".into(),
                    run_epoch: 2,
                    revision: 9,
                },
            },
        );

        composer.refresh_status();

        assert_eq!(
            composer.status.get(),
            "Earlier direction was saved in Task Center."
        );
        assert!(matches!(
            composer.tasks.as_ref().map(AgentTaskFrontiers::choice),
            Some(AgentTaskChoice::One(task)) if task.task_id == "task-a" && task.run_epoch == 7
        ));
    }

    #[test]
    fn pending_offer_callbacks_get_a_bounded_native_wake_before_expiry() {
        let now = ui::time::now();
        let deadline = now + std::time::Duration::from_secs(5);
        let mut composer = AgentQuickComposer::default();
        composer.open_for_shots(
            "room-a",
            "scribe",
            "Scribe",
            "",
            Rect::new(10.0, 10.0, 34.0, 34.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        let actions = ui::remote_actions::RemoteActions::session();
        composer.pending_message_offer = Some(PendingMessageOffer {
            state: ui::agent_message_action::begin(&actions, "room-a"),
            deadline,
            pages_remaining: 1,
        });
        composer.actions = Some(actions);

        let sampled_at = ui::time::now();
        let wake = composer
            .next_wake_deadline()
            .expect("pending catalogue reply schedules a native pump");
        assert!(wake > sampled_at);
        assert!(wake <= ui::time::now() + std::time::Duration::from_millis(50));
        assert!(wake < deadline);
    }

    #[test]
    fn available_message_offer_stays_idle_until_a_user_or_submission_refresh() {
        let host = Arc::new(MessageOfferHost {
            authorized: AtomicBool::new(true),
            queries: AtomicUsize::new(0),
            invokes: AtomicUsize::new(0),
        });
        let mut composer = message_offer_composer(host.clone());
        settle_message_offer(&mut composer);
        assert!(composer.message_offer.is_some());
        composer.popover.set_open(false);
        assert_eq!(composer.next_wake_deadline(), None);
        let settled_queries = host.queries.load(Ordering::Relaxed);

        for _ in 0..64 {
            assert!(!composer.tick(), "a stable offer must not repaint at idle");
        }

        assert_eq!(host.queries.load(Ordering::Relaxed), settled_queries);
        assert!(composer.pending_message_offer.is_none());
        assert_eq!(composer.next_wake_deadline(), None);
    }

    #[test]
    fn stale_message_offer_is_refused_on_submit_and_waits_for_user_refresh() {
        let host = Arc::new(MessageOfferHost {
            authorized: AtomicBool::new(true),
            queries: AtomicUsize::new(0),
            invokes: AtomicUsize::new(0),
        });
        let mut composer = message_offer_composer(host.clone());
        settle_message_offer(&mut composer);
        composer.set_draft("Review the release notes");
        host.authorized.store(false, Ordering::Relaxed);

        composer.submit();
        assert!(composer.pending_message.is_some());
        for _ in 0..16 {
            composer.tick();
            if composer.pending_message.is_none() {
                break;
            }
        }
        assert!(composer.pending_message.is_none());
        assert!(composer.permission_refused);
        assert!(composer.message_offer.is_none());
        assert!(!composer.send_enabled.get());
        assert_eq!(composer.draft(), "Review the release notes");
        assert_eq!(host.invokes.load(Ordering::Relaxed), 0);
        composer.popover.set_open(false);
        assert_eq!(composer.next_wake_deadline(), None);
        let refused_queries = host.queries.load(Ordering::Relaxed);

        host.authorized.store(true, Ordering::Relaxed);
        for _ in 0..64 {
            composer.tick();
        }
        assert_eq!(
            host.queries.load(Ordering::Relaxed),
            refused_queries,
            "permission failure stays disabled until another user navigation"
        );
        assert!(composer.permission_refused);
        assert_eq!(composer.next_wake_deadline(), None);

        composer.popover.set_open(true);
        composer.refresh_message_offer_for_user_navigation();
        settle_message_offer(&mut composer);
        assert!(composer.message_offer.is_some());
        assert!(!composer.permission_refused);
        assert!(composer.send_enabled.get());
        assert!(host.queries.load(Ordering::Relaxed) > refused_queries);
    }

    #[test]
    fn an_unresolved_dispatched_message_does_not_spin_the_desktop() {
        use ui::collaboration::CollabEvent;
        use ui::widget::Collaborative;

        let invocation = InvocationId(uuid::Uuid::from_bytes([0x4f; 16]));
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
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: None,
            },
        });
        let composer = AgentQuickComposer {
            actions: Some(actions),
            pending_message: Some(PendingMessage {
                state: AgentMessageSubmission::Invoke { invocation, call },
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
            None,
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
        let observing = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message:
                    RemoteActionMessage::Observe {
                        invocation: observed,
                        ..
                    },
            },
        ] = observing.as_slice()
        else {
            panic!("one accepted invocation observation is dispatched")
        };
        assert_eq!(*observed, invocation);
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
        fn origin_node(widget: &dyn Widget) -> Option<ui::a11y::AccessNode> {
            if widget.test_id() == Some("agent-quick-origin-context") {
                return widget.accessibility();
            }
            widget.children().into_iter().find_map(origin_node)
        }
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
        assert!(
            has_accessible_value(&composer, "Agent"),
            "the visible collaborator kind is also published to accessibility"
        );
        let opened_context = t_args(
            "agent-quick-origin-context",
            &[("context", t("agent-quick-shots-context").into())],
        );
        let state = ui::State::new();
        composer.sync(&state.doc, &state.root);
        let context_node = origin_node(&composer).expect("opened context is accessible");
        assert_eq!(context_node.role, ui::a11y::Role::StaticText);
        assert_eq!(context_node.name.as_deref(), Some(opened_context.as_str()));
        composer.status.set(t("agent-quick-checking"));
        composer.sync(&state.doc, &state.root);
        assert_eq!(composer.origin_context.get(), opened_context);
        assert_eq!(
            origin_node(&composer).unwrap().name.as_deref(),
            Some(opened_context.as_str()),
            "live status changes do not rewrite the validated context captured at open"
        );
    }

    #[test]
    fn unavailable_status_uses_measured_height_without_overlapping_intent_tabs() {
        fn widget_with_id<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if widget.test_id() == Some(id) {
                return Some(widget);
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| widget_with_id(child, id))
        }

        let unavailable = t("agent-quick-message-refused");
        for (compact, viewport, anchor, theme) in [
            (
                false,
                Rect::new(0.0, 0.0, 1280.0, 800.0),
                Rect::new(420.0, 180.0, 444.0, 204.0),
                Theme::dark(),
            ),
            (
                true,
                Rect::new(0.0, 0.0, 560.0, 560.0),
                Rect::new(264.0, 188.0, 288.0, 212.0),
                Theme::high_contrast().with_text_scale(2.0),
            ),
        ] {
            let (mut composer, _applied_revision) = task_composer_with_applied(4);
            composer.status.set(t("agent-quick-checking"));
            composer.popover.set_anchor(anchor);
            composer.popover.set_viewport(viewport);
            composer.popover.set_open(true);
            let document = automerge::AutoCommit::new();
            composer.sync(&document, &automerge::ROOT);
            composer.permission_refused = true;
            assert!(
                composer.update(&document, &automerge::ROOT),
                "the refused offer changes the visible status"
            );
            assert_eq!(composer.status.get(), unavailable);
            assert_eq!(
                widget_with_id(&composer, "agent-quick-status")
                    .and_then(Widget::accessibility)
                    .and_then(|node| node.name.or(node.value))
                    .as_deref(),
                Some(unavailable.as_str()),
                "the retained sync must expose the actual unavailable status before measurement"
            );
            layout_panel(&mut composer, anchor, viewport, &theme);
            let status = widget_with_id(&composer, "agent-quick-status")
                .expect("the visible status uses the shared Text widget");
            let intent = widget_with_id(&composer, "agent-quick-intent")
                .expect("the visible intent tabs use the shared ToggleGroup");
            let status_rect = status.get_rect();
            let intent_rect = intent.get_rect();
            let content_rect = composer.popover.content().get_rect();
            let status_style = theme
                .text(ui::tokens::TextRole::Body)
                .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph);
            // `Text::measure` and `Text::draw` share one logical pixel of
            // wrap slack so rounding cannot make one wrap while the other
            // does not. Reproduce that exact public layout contract here.
            let (_, expected_height) = ui::text_render::TextRenderer::new().measure_style(
                &unavailable,
                Some(logical_dimension(status_rect.width() + 1.0)),
                &status_style,
            );
            assert!(
                status_rect.height() + 0.5 >= f64::from(expected_height),
                "status {status_rect:?} must reserve its renderer's full {expected_height}px text height"
            );
            assert!(
                status_rect.x0 >= content_rect.x0 && status_rect.x1 <= content_rect.x1,
                "status {status_rect:?} must stay inside composer content {content_rect:?}"
            );
            assert!(
                status_rect.y1 <= intent_rect.y0,
                "wrapped status {status_rect:?} must not paint under intent tabs {intent_rect:?}"
            );
            if compact {
                assert!(
                    expected_height > status_style.line_height_px() * 1.5,
                    "compact HC200 must exercise a genuinely wrapped localized status"
                );
            }
            assert_eq!(
                status
                    .accessibility()
                    .and_then(|node| node.name.or(node.value))
                    .as_deref(),
                Some(unavailable.as_str()),
                "wrapping must preserve the complete localized status for assistive technology"
            );
        }
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

    #[test]
    fn hc200_panel_context_and_controls_fit_normal_and_compact_viewports() {
        fn widget_with_id<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if widget.test_id() == Some(id) {
                return Some(widget);
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| widget_with_id(child, id))
        }

        fn assert_inside(widget: &dyn Widget, panel: Rect) {
            let rect = widget.get_rect();
            if rect.width() > 0.0 && rect.height() > 0.0 {
                assert!(
                    rect.x0 >= panel.x0
                        && rect.x1 <= panel.x1
                        && rect.y0 >= panel.y0
                        && rect.y1 <= panel.y1,
                    "{} bounds {rect:?} escape panel {panel:?}",
                    widget.name()
                );
            }
            for child in widget.children() {
                assert_inside(child, panel);
            }
        }
        let theme = Theme::high_contrast().with_text_scale(2.0);
        for (viewport, anchor) in [
            (
                Rect::new(0.0, 0.0, 1280.0, 800.0),
                Rect::new(420.0, 180.0, 444.0, 204.0),
            ),
            (
                Rect::new(0.0, 0.0, 560.0, 560.0),
                Rect::new(264.0, 188.0, 288.0, 212.0),
            ),
            (
                Rect::new(0.0, 0.0, 560.0, 560.0),
                Rect::new(510.0, 110.0, 534.0, 134.0),
            ),
        ] {
            let mut composer = AgentQuickComposer::default();
            composer.open_for_shots(
                "room-a",
                "scribe",
                "Release Notes Agent",
                "Review changes and preserve decisions.",
                anchor,
                viewport,
            );
            let panel = layout_panel(&mut composer, anchor, viewport, &theme);
            assert!(
                panel.x0 >= viewport.x0 && panel.x1 <= viewport.x1,
                "panel {panel:?}, viewport {viewport:?}, anchor {anchor:?}, body {:?}",
                composer.popover.content().get_rect()
            );
            assert!(panel.y0 >= viewport.y0 && panel.y1 <= viewport.y1);
            assert_inside(composer.popover.content(), panel);
            let origin = widget_with_id(&composer, "agent-quick-origin-context")
                .expect("the immutable opened context uses the shared Text widget");
            let intent = widget_with_id(&composer, "agent-quick-intent")
                .expect("the intent control establishes the usable body width");
            assert_opened_context_layout(origin, intent, &theme);
        }
    }
}
