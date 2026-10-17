//! The action a run paused at, as a card the person answers.
//!
//! The card shows exactly what will run: the draft the owner recorded from
//! the action's descriptor and the agent's arguments, laid out as a message
//! when it is one. The person confirms it as it is, changes its editable
//! fields first, declines it with a reason the agent is told, or lets the
//! agent take that action without asking from now on. Every answer names
//! the run and the draft the person saw, so a draft that changed meanwhile
//! is never answered by mistake.

use super::{
    ActivityFocus, ElementState, Key, Modifiers, NamedKey, PaintCtx, PresenceCtx, Rect,
    RoomActivityPane, TaskAction, TextRole, Widget, WindowEvent, at, t, t_args, text_width,
};
use crate::activity::TaskReviewCommand;
use tilo_actions::{Draft, DraftEdit, DraftField, DraftLayout, DraftValue, MessageRole};
use tilo_desktop_model::{
    WorkspaceTaskConfirmation, WorkspaceTaskConfirmationAnswer, WorkspaceTaskConfirmationState,
};
use ui::widget::{
    Button, ButtonFace, ButtonRole, CardFace, ChipFace, ChipLead, ChipStyle, ControlSize, Tone,
};
use vello::Scene;

/// Most lines of a message body the card shows before it is edited.
const BODY_LINES: usize = 8;
/// Lines a multi-line field's editor shows.
const EDITOR_LINES: f64 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfirmMode {
    Review,
    Edit,
    Decline,
}

/// One editable field while the person changes the draft.
pub(super) struct ConfirmInput {
    pub path: Vec<String>,
    pub original: String,
    pub input: ui::widget::TextInput,
    pub rect: Rect,
    pub label: String,
}

/// The person's work on one paused action: which draft they are looking
/// at, and their changes and reason while they make them.
pub(super) struct ConfirmationReview {
    pub room: String,
    pub task_id: String,
    pub confirmation_id: String,
    pub mode: ConfirmMode,
    pub fields: Vec<ConfirmInput>,
    pub reason: ui::widget::TextInput,
    pub reason_rect: Rect,
}

/// A run newly paused for the person, for the notification that points them
/// at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmationNotice {
    pub room: String,
    pub task_id: String,
    pub task_title: String,
    pub agent: String,
    pub action: String,
    pub gist: Option<String>,
}

/// What an agent's run of a task is doing now, for a surface beside the
/// work (the agent composer) rather than in Task Center.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunGlance {
    /// The step the run is at, in the person's language ("Editing Notes…").
    pub step: Option<String>,
    /// The action it is paused at, waiting for the person.
    pub paused: Option<PausedDraft>,
}

/// The action a run is paused at, as the confirmation card presents it:
/// the same status, risk, heading, meta and fields, for a surface that shows
/// the card's content with its own widgets and answers it the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PausedDraft {
    pub room: String,
    pub task_id: String,
    pub run_epoch: u64,
    pub confirmation_id: String,
    /// "Waiting for your confirmation".
    pub status: String,
    /// How far the action reaches, as the card's chip says it.
    pub risk: String,
    /// The action, as the card's heading says it.
    pub action: String,
    /// Who asks, and on what.
    pub meta: String,
    /// Each field's label and value, in the card's order.
    pub fields: Vec<(String, String)>,
}

impl ConfirmationNotice {
    /// The banner's title: who asks, and what.
    #[must_use]
    pub fn title(&self) -> String {
        t_args(
            "activity-confirm-notice-title",
            &[
                ("agent", self.agent.as_str().into()),
                ("action", self.action.as_str().into()),
            ],
        )
    }

    /// The banner's body: the task, and what the draft is about.
    #[must_use]
    pub fn body(&self) -> String {
        match &self.gist {
            Some(gist) => t_args(
                "activity-confirm-notice-gist",
                &[
                    ("task", self.task_title.as_str().into()),
                    ("gist", gist.as_str().into()),
                ],
            ),
            None => t_args(
                "activity-confirm-notice-body",
                &[("task", self.task_title.as_str().into())],
            ),
        }
    }

    /// The banner's action, which opens the card.
    #[must_use]
    pub fn review_label(&self) -> String {
        t("activity-confirm-notice-review")
    }
}

impl ConfirmationReview {
    fn new(room: &str, task_id: &str, confirmation: &WorkspaceTaskConfirmation) -> Self {
        Self {
            room: room.to_owned(),
            task_id: task_id.to_owned(),
            confirmation_id: confirmation.id.clone(),
            mode: ConfirmMode::Review,
            fields: Vec::new(),
            reason: ui::widget::TextInput::local()
                .with_placeholder(t("activity-confirm-reason-placeholder"))
                .with_accessible_name(t("activity-confirm-reason-label"))
                .with_id("task-confirm-reason"),
            reason_rect: Rect::ZERO,
        }
    }

    fn start_editing(&mut self, draft: &Draft) {
        self.fields = draft
            .fields
            .iter()
            .filter(|field| field.editable)
            .map(|field| {
                let original = field.value.line();
                let multiline = field.multiline;
                ConfirmInput {
                    path: field.path.clone(),
                    input: ui::widget::TextInput::local()
                        .multiline(multiline)
                        .with_initial_value(&original)
                        .with_accessible_name(field_label(field))
                        .with_id(&format!("task-confirm-field-{}", field.path.join("."))),
                    original,
                    rect: Rect::ZERO,
                    label: field_label(field),
                }
            })
            .collect();
        self.mode = ConfirmMode::Edit;
    }

    /// The fields the person changed, as edits.
    pub(super) fn edits(&self) -> Vec<DraftEdit> {
        if self.mode != ConfirmMode::Edit {
            return Vec::new();
        }
        self.fields
            .iter()
            .filter(|field| field.input.value() != field.original)
            .map(|field| DraftEdit {
                path: field.path.clone(),
                text: field.input.value().to_owned(),
            })
            .collect()
    }

    fn input_mut(&mut self, focus: &ActivityFocus) -> Option<&mut ui::widget::TextInput> {
        match focus {
            ActivityFocus::ConfirmField(index) => {
                self.fields.get_mut(*index).map(|field| &mut field.input)
            }
            ActivityFocus::DeclineReason if self.mode == ConfirmMode::Decline => {
                Some(&mut self.reason)
            }
            _ => None,
        }
    }

    fn input(&self, focus: &ActivityFocus) -> Option<&ui::widget::TextInput> {
        match focus {
            ActivityFocus::ConfirmField(index) => self.fields.get(*index).map(|field| &field.input),
            ActivityFocus::DeclineReason if self.mode == ConfirmMode::Decline => Some(&self.reason),
            _ => None,
        }
    }
}

/// How a paused action reaches beyond the document, in a word.
fn risk_label(confirmation: &WorkspaceTaskConfirmation) -> (String, Tone) {
    use tilo_actions::RiskLevel;
    let (key, tone) = match confirmation.risk {
        // A message leaves; any other consequential action (deleting for
        // good, overwriting, publishing) is one that cannot be taken back.
        RiskLevel::Destructive
            if confirmation.draft.layout == tilo_actions::DraftLayout::Message =>
        {
            ("activity-confirm-risk-sends", Tone::Warning)
        }
        RiskLevel::Destructive => ("activity-confirm-risk-deletes", Tone::Danger),
        RiskLevel::Privileged => ("activity-confirm-risk-privileged", Tone::Danger),
        RiskLevel::Sensitive => ("activity-confirm-risk-sensitive", Tone::Warning),
        RiskLevel::None | RiskLevel::Low => match confirmation.kind {
            // Queued for this device and undone by queueing its revert (a
            // file renamed or moved in the workspace): it changes the
            // person's work rather than sending anything away.
            tilo_action_policy::ActionKind::Outside if confirmation.draft.undoable => {
                ("activity-confirm-risk-changes", Tone::Info)
            }
            tilo_action_policy::ActionKind::Outside => {
                ("activity-confirm-risk-sends", Tone::Warning)
            }
            tilo_action_policy::ActionKind::Undoable => {
                ("activity-confirm-risk-changes", Tone::Info)
            }
            tilo_action_policy::ActionKind::Read => ("activity-confirm-risk-reads", Tone::Info),
        },
    };
    (t(key), tone)
}

/// A run's public note as the step it names, in the person's language; the
/// note as it is when it names none of the steps every run takes.
fn step_line(note: &str) -> Option<String> {
    use tilo_actions::task_step::TaskStep;
    let note = note.trim();
    if note.is_empty() {
        return None;
    }
    Some(match TaskStep::parse(note) {
        Some(TaskStep::Searching) => t("activity-task-step-searching"),
        Some(TaskStep::Reading(app)) => {
            t_args("activity-task-step-reading", &[("app", app.into())])
        }
        Some(TaskStep::Editing(app)) => {
            t_args("activity-task-step-editing", &[("app", app.into())])
        }
        Some(TaskStep::Exploring(app)) => {
            t_args("activity-task-step-exploring", &[("app", app.into())])
        }
        None => note.to_owned(),
    })
}

/// What a message's field is called on the card, in the session's language.
fn role_label(role: MessageRole) -> String {
    t(match role {
        MessageRole::To => "activity-confirm-to",
        MessageRole::Cc => "activity-confirm-cc",
        MessageRole::Bcc => "activity-confirm-bcc",
        MessageRole::Subject => "activity-confirm-subject",
        MessageRole::Body => "activity-confirm-body",
        MessageRole::Attachments => "activity-confirm-attachments",
    })
}

/// A field's label: its message role's name when it has one.
fn field_label(field: &DraftField) -> String {
    field.role.map_or_else(|| field.label.clone(), role_label)
}

/// What a list field's label column says: its name and how many entries.
fn list_heading(label: &str, count: usize) -> String {
    t_args(
        "activity-confirm-list-label",
        &[("label", label.into()), ("count", count.into())],
    )
}

/// The text a field's row is labelled with.
fn row_label(field: &DraftField) -> String {
    match &field.value {
        DraftValue::List { count, .. } => list_heading(&field_label(field), *count),
        _ => field_label(field),
    }
}

/// A value as the card says it, in one line.
fn value_text(value: &DraftValue) -> String {
    match value {
        DraftValue::Withheld => t("activity-confirm-withheld"),
        DraftValue::Flag { on } => t(if *on {
            "activity-confirm-yes"
        } else {
            "activity-confirm-no"
        }),
        DraftValue::List { count, labels } => t_args(
            "activity-confirm-list",
            &[
                ("count", (*count).into()),
                ("items", labels.join(", ").into()),
            ],
        ),
        other => other.line(),
    }
}

impl RoomActivityPane {
    /// What the agent's run of task `task_id` in `room` is doing now: its
    /// latest step, and the action it waits for the person on, if any. Read
    /// from the room owner's review, which the host keeps current; `None`
    /// while there is none for the room.
    #[must_use]
    pub fn run_glance(&self, room: &str, task_id: &str) -> Option<RunGlance> {
        let Some(Ok(snapshot)) = self.host.task_review_snapshot(room) else {
            return None;
        };
        let task = snapshot.tasks.iter().find(|task| task.id == task_id)?;
        let control = snapshot.controls.get(task_id);
        let epoch = control
            .and_then(|control| control.active_run.as_ref())
            .map(|run| run.epoch);
        let step = epoch.and_then(|epoch| {
            task.audit
                .iter()
                .rev()
                .filter(|event| event.action() == "public-progress" && event.for_run(epoch))
                .find_map(|event| event.note.as_deref().and_then(step_line))
        });
        let paused = control
            .and_then(|control| control.confirmation.as_ref())
            .filter(|held| held.is_waiting() && Some(held.run_epoch) == epoch)
            .map(|held| PausedDraft {
                room: room.to_owned(),
                task_id: task_id.to_owned(),
                run_epoch: held.run_epoch,
                confirmation_id: held.id.clone(),
                status: t("activity-confirm-waiting"),
                risk: risk_label(held).0,
                action: held.draft.action_label.clone(),
                meta: t_args(
                    "activity-confirm-meta",
                    &[
                        ("agent", self.agent_label(&held.agent_id).into()),
                        ("resource", held.draft.resource_label.as_str().into()),
                    ],
                ),
                fields: held
                    .draft
                    .fields
                    .iter()
                    .map(|field| (row_label(field), value_text(&field.value)))
                    .collect(),
            });
        Some(RunGlance { step, paused })
    }

    /// Answer `draft` as the card does: confirm it as it stands, or decline
    /// it. Returns the request's invocation, which
    /// [`ActivityHost::poll_task_review`](crate::activity::ActivityHost::poll_task_review)
    /// answers.
    ///
    /// # Errors
    /// The room's review is unavailable or the host refused the request.
    pub fn answer_paused_draft(
        &mut self,
        draft: &PausedDraft,
        confirm: bool,
    ) -> Result<uuid::Uuid, String> {
        let Some(Ok(snapshot)) = self.host.task_review_snapshot(&draft.room) else {
            return Err("the task's review is unavailable".into());
        };
        let invocation = uuid::Uuid::new_v4();
        self.host
            .submit_task_review(crate::activity::TaskReviewRequest {
                room: draft.room.clone(),
                observed_actor: snapshot.actor.clone(),
                invocation,
                command: TaskReviewCommand::AnswerConfirmation {
                    task_id: draft.task_id.clone(),
                    run_epoch: draft.run_epoch,
                    confirmation_id: draft.confirmation_id.clone(),
                    answer: if confirm {
                        WorkspaceTaskConfirmationAnswer::Confirm { edits: Vec::new() }
                    } else {
                        WorkspaceTaskConfirmationAnswer::Decline { reason: None }
                    },
                },
            })?;
        Ok(invocation)
    }

    /// The outcome of an answer [`Self::answer_paused_draft`] sent: `None`
    /// while it is on its way.
    #[must_use]
    pub fn paused_answer_outcome(&self, invocation: uuid::Uuid) -> Option<Result<(), String>> {
        self.host.poll_task_review(invocation)
    }

    /// Open task `task_id`'s paused action for editing, as the card's Edit
    /// does, once Task Center shows that task.
    pub fn edit_paused_draft(&mut self, task_id: &str) {
        self.selected_task = Some(task_id.to_owned());
        self.view = super::ActivityView::Tasks;
        self.apply_confirmation_action(TaskAction::EditDraft);
    }

    /// The confirmation the selected task's run waits on, from the owner's
    /// review: still waiting, or answered and on its way to the run.
    pub(super) fn reviewed_confirmation(
        &self,
        task_id: &str,
    ) -> Option<(u64, WorkspaceTaskConfirmation)> {
        let (_, control) = self.reviewed_control(task_id)?;
        let confirmation = control.confirmation.clone()?;
        (control.active_run.as_ref()?.epoch == confirmation.run_epoch)
            .then_some((confirmation.run_epoch, confirmation))
    }

    /// Whether `task` waits for the person: a run paused at an action, or a
    /// task an agent handed back.
    pub(super) fn task_needs_person(&self, task: &tilo_desktop_model::WorkspaceTask) -> bool {
        task.status == tilo_desktop_model::WorkspaceTaskStatus::Waiting
            || self
                .reviewed_confirmation(&task.id)
                .is_some_and(|(_, confirmation)| confirmation.is_waiting())
    }

    /// Note runs newly paused at an action since the last review, once each.
    pub(super) fn observe_confirmations(&mut self) {
        let Some(Ok(snapshot)) = &self.task_review else {
            return;
        };
        if snapshot.room != self.task_room || !snapshot.writable {
            return;
        }
        let mut notices = Vec::new();
        for task in &snapshot.tasks {
            let Some(confirmation) = snapshot
                .controls
                .get(&task.id)
                .and_then(|control| control.confirmation.as_ref())
                .filter(|confirmation| confirmation.is_waiting())
            else {
                continue;
            };
            let agent = self.agent_label(&confirmation.agent_id);
            if self.noticed_confirmations.insert(confirmation.id.clone()) {
                notices.push(ConfirmationNotice {
                    room: snapshot.room.clone(),
                    task_id: task.id.clone(),
                    task_title: task.title.clone(),
                    agent,
                    action: confirmation.draft.action_label.clone(),
                    gist: confirmation.draft.gist(),
                });
            }
        }
        self.confirmation_notices.extend(notices);
        if let Some(review) = &self.confirmation
            && (review.room != self.task_room
                || self
                    .reviewed_confirmation(&review.task_id)
                    .is_none_or(|(_, held)| held.id != review.confirmation_id))
        {
            self.confirmation = None;
        }
    }

    /// Runs newly paused for the person, for the host to notify about.
    pub fn take_confirmation_notices(&mut self) -> Vec<ConfirmationNotice> {
        std::mem::take(&mut self.confirmation_notices)
    }

    /// Carry out a card button.
    pub(super) fn apply_confirmation_action(&mut self, action: TaskAction) -> bool {
        let Some(id) = self.selected_task.clone() else {
            return true;
        };
        let Some((run_epoch, confirmation)) = self.reviewed_confirmation(&id) else {
            return true;
        };
        if self.review_pending.is_some() || !confirmation.is_waiting() {
            return true;
        }
        let room = self.task_room.clone();
        let review = self
            .confirmation
            .get_or_insert_with(|| ConfirmationReview::new(&room, &id, &confirmation));
        match action {
            TaskAction::EditDraft => {
                review.start_editing(&confirmation.draft);
                self.keyboard_focus = Some(if review.fields.is_empty() {
                    ActivityFocus::TaskAction(TaskAction::ConfirmDraft)
                } else {
                    ActivityFocus::ConfirmField(0)
                });
            }
            TaskAction::DiscardEdits | TaskAction::KeepDraft => {
                review.mode = ConfirmMode::Review;
                review.fields.clear();
                self.keyboard_focus = Some(ActivityFocus::TaskAction(TaskAction::ConfirmDraft));
            }
            TaskAction::DeclineDraft => {
                review.mode = ConfirmMode::Decline;
                self.keyboard_focus = Some(ActivityFocus::DeclineReason);
            }
            TaskAction::ConfirmDraft => {
                let edits = review.edits();
                self.submit_review(TaskReviewCommand::AnswerConfirmation {
                    task_id: id,
                    run_epoch,
                    confirmation_id: confirmation.id,
                    answer: WorkspaceTaskConfirmationAnswer::Confirm { edits },
                });
            }
            TaskAction::SendDecline => {
                let reason = review.reason.value().trim().to_owned();
                self.submit_review(TaskReviewCommand::AnswerConfirmation {
                    task_id: id,
                    run_epoch,
                    confirmation_id: confirmation.id,
                    answer: WorkspaceTaskConfirmationAnswer::Decline {
                        reason: (!reason.is_empty()).then_some(reason),
                    },
                });
            }
            TaskAction::AlwaysAllow => {
                let edits = review.edits();
                self.submit_review(TaskReviewCommand::AlwaysAllow {
                    task_id: id,
                    run_epoch,
                    confirmation_id: confirmation.id,
                    edits,
                });
            }
            _ => return false,
        }
        true
    }

    /// The card's text fields, in focus order.
    pub(super) fn confirmation_focus_order(&self) -> Vec<ActivityFocus> {
        let Some(review) = &self.confirmation else {
            return Vec::new();
        };
        match review.mode {
            ConfirmMode::Edit => (0..review.fields.len())
                .map(ActivityFocus::ConfirmField)
                .collect(),
            ConfirmMode::Decline => vec![ActivityFocus::DeclineReason],
            ConfirmMode::Review => Vec::new(),
        }
    }

    pub(super) fn confirmation_field_rect(&self, focus: &ActivityFocus) -> Option<Rect> {
        let review = self.confirmation.as_ref()?;
        match focus {
            ActivityFocus::ConfirmField(index) => review.fields.get(*index).map(|field| field.rect),
            ActivityFocus::DeclineReason => Some(review.reason_rect),
            _ => None,
        }
    }

    pub(super) fn confirmation_field_label(&self, focus: &ActivityFocus) -> Option<String> {
        let review = self.confirmation.as_ref()?;
        match focus {
            ActivityFocus::ConfirmField(index) => {
                review.fields.get(*index).map(|field| field.label.clone())
            }
            ActivityFocus::DeclineReason => Some(t("activity-confirm-reason-label")),
            _ => None,
        }
    }

    pub(super) fn confirmation_field_accessibility(
        &self,
        focus: &ActivityFocus,
    ) -> Option<ui::a11y::AccessNode> {
        self.confirmation.as_ref()?.input(focus)?.accessibility()
    }

    /// Shared field semantics for the overlay accessibility bridge.
    pub fn perform_confirmation_field_action(
        &mut self,
        focus_index: Option<usize>,
        action: &ui::a11y::Action,
    ) -> bool {
        let focus = focus_index.map_or(ActivityFocus::DeclineReason, ActivityFocus::ConfirmField);
        let Some(input) = self
            .confirmation
            .as_mut()
            .and_then(|review| review.input_mut(&focus))
        else {
            return false;
        };
        match action {
            ui::a11y::Action::SetValue(value) => {
                input.set_text(value);
                true
            }
            ui::a11y::Action::Focus | ui::a11y::Action::Activate => {
                self.keyboard_focus = Some(focus);
                true
            }
            _ => false,
        }
    }

    /// Route a pointer press onto a card field, then keys to the focused one.
    pub(super) fn confirmation_event(
        &mut self,
        event: &WindowEvent,
        (scale_factor, modifiers, cursor): (f64, Modifiers, Option<vello::kurbo::Point>),
        tree: &mut impl super::ActivityTree,
    ) -> bool {
        let Some(review) = &self.confirmation else {
            return false;
        };
        if let Some(point) = cursor
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            )
        {
            let hit = review
                .fields
                .iter()
                .position(|field| field.rect.contains(point))
                .map(ActivityFocus::ConfirmField)
                .or_else(|| {
                    (review.mode == ConfirmMode::Decline && review.reason_rect.contains(point))
                        .then_some(ActivityFocus::DeclineReason)
                });
            if let Some(hit) = hit {
                self.keyboard_focus = Some(hit);
            }
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.state == ElementState::Pressed
                && event.logical_key == Key::Named(NamedKey::Escape)
                && review.mode != ConfirmMode::Review)
        {
            return self.apply_confirmation_action(TaskAction::KeepDraft);
        }
        let Some(focus) = self.keyboard_focus.clone() else {
            return false;
        };
        if !matches!(
            focus,
            ActivityFocus::ConfirmField(_) | ActivityFocus::DeclineReason
        ) {
            return false;
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. }
            if matches!(event.logical_key, Key::Named(NamedKey::Tab | NamedKey::Escape)))
        {
            return false;
        }
        let mut scratch = taffy::TaffyTree::<()>::new();
        let Ok(node) = scratch.new_leaf(taffy::Style::default()) else {
            return false;
        };
        let Some(input) = self
            .confirmation
            .as_mut()
            .and_then(|review| review.input_mut(&focus))
        else {
            return false;
        };
        let mut handled = false;
        let mut focus_target = None;
        input.set_focus(true);
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(tree.document_mut()),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &scratch,
            },
        );
        handled
    }

    /// Paint the card for `confirmation` from `y` down; returns where the
    /// space below it starts.
    pub(super) fn draw_confirmation_card(
        &mut self,
        paint: &mut PaintCtx<'_>,
        bounds: Rect,
        y: f64,
        confirmation: &WorkspaceTaskConfirmation,
    ) -> f64 {
        let room = self.task_room.clone();
        let task_id = self.selected_task.clone().unwrap_or_default();
        if self
            .confirmation
            .as_ref()
            .is_none_or(|review| review.confirmation_id != confirmation.id)
        {
            self.confirmation = Some(ConfirmationReview::new(&room, &task_id, confirmation));
        }
        let card = CardFace::new().leading_stripe(if confirmation.is_waiting() {
            Tone::Warning
        } else {
            Tone::Success
        });
        let theme = CardFace::content_theme(paint.theme);
        let outer = Rect::new(bounds.x0, y, bounds.x1, bounds.y1);
        let body = card.body_rect(paint.text, paint.theme, outer);
        let mut content = Scene::new();
        let bottom = {
            let mut inner = PaintCtx::new(&mut content, paint.transform, paint.text, &theme);
            self.draw_confirmation_body(&mut inner, body, confirmation)
        };
        let height = card.height_for_body(paint.text, paint.theme, outer.width(), bottom - body.y0);
        let rect = Rect::new(
            outer.x0,
            outer.y0,
            outer.x1,
            (outer.y0 + height).min(bounds.y1),
        );
        card.paint(paint, rect);
        paint.scene.append(&content, None);
        rect.y1 + f64::from(paint.theme.tokens.spacing.s3)
    }

    fn draw_confirmation_body(
        &mut self,
        paint: &mut PaintCtx<'_>,
        body: Rect,
        confirmation: &WorkspaceTaskConfirmation,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let small = f64::from(paint.theme.tokens.spacing.s2);
        let mut y = body.y0;
        // The status, and how far the action reaches, as chips.
        let status = if confirmation.is_waiting() {
            t("activity-confirm-waiting")
        } else {
            match &confirmation.state {
                WorkspaceTaskConfirmationState::Declined { .. } => t("activity-confirm-declined"),
                _ => t("activity-confirm-confirmed"),
            }
        };
        let chip_h = ChipFace::height(paint.theme);
        let status_w =
            ChipFace::intrinsic_width(&status, ChipLead::None, false, paint.text, paint.theme);
        ChipFace::new(
            Rect::new(body.x0, y, body.x0 + status_w, y + chip_h),
            &status,
        )
        .style(ChipStyle::Tinted(if confirmation.is_waiting() {
            Tone::Warning
        } else {
            Tone::Success
        }))
        .paint(paint.scene, paint.transform, paint.text, paint.theme);
        let (risk, tone) = risk_label(confirmation);
        let risk_w =
            ChipFace::intrinsic_width(&risk, ChipLead::None, false, paint.text, paint.theme);
        if body.x0 + status_w + gap + risk_w <= body.x1 {
            ChipFace::new(Rect::new(body.x1 - risk_w, y, body.x1, y + chip_h), &risk)
                .style(ChipStyle::Tinted(tone))
                .paint(paint.scene, paint.transform, paint.text, paint.theme);
        }
        y += chip_h + gap;

        let draft = confirmation.reviewed_draft();
        let width = text_width(body.width());
        let heading = paint.theme.text(TextRole::Subheading);
        y = draw_text(
            paint,
            &draft.action_label,
            &heading,
            (body.x0, y),
            width,
            false,
        ) + small;
        let meta = t_args(
            "activity-confirm-meta",
            &[
                ("agent", self.agent_label(&confirmation.agent_id).into()),
                ("resource", draft.resource_label.as_str().into()),
            ],
        );
        let caption = paint.theme.text(TextRole::Caption);
        y = draw_text(paint, &meta, &caption, (body.x0, y), width, true) + gap;

        let mode = self
            .confirmation
            .as_ref()
            .map_or(ConfirmMode::Review, |review| review.mode);
        y = if mode == ConfirmMode::Edit {
            self.draw_editors(paint, body, y, &draft)
        } else if draft.layout == DraftLayout::Message {
            draw_message(paint, body, y, &draft)
        } else {
            let labels: Vec<String> = draft.fields.iter().map(row_label).collect();
            let column = label_column(paint, &labels, body.width());
            draw_fields(paint, body, y, (draft.fields.iter(), column))
        };
        if mode == ConfirmMode::Decline {
            y = self.draw_reason(paint, body, y + small);
        }
        if confirmation.is_waiting() {
            y = self.draw_confirmation_buttons(paint, body, y + gap, mode);
        } else {
            y = draw_text(
                paint,
                &t("activity-confirm-on-its-way"),
                &caption,
                (body.x0, y + small),
                width,
                true,
            );
        }
        y
    }

    fn draw_editors(
        &mut self,
        paint: &mut PaintCtx<'_>,
        body: Rect,
        mut y: f64,
        draft: &Draft,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let small = f64::from(paint.theme.tokens.spacing.s2);
        let label_style = paint.theme.text(TextRole::Label);
        let line = f64::from(paint.theme.text(TextRole::Body).line_height_px());
        let control = ButtonFace::height(ControlSize::Regular, paint.theme);
        let focus = self.keyboard_focus.clone();
        let disabled = self.review_pending.is_some();
        let width = text_width(body.width());
        // What cannot be changed here is still shown, above the editors.
        let fixed: Vec<&DraftField> = draft
            .fields
            .iter()
            .filter(|field| !field.editable)
            .collect();
        let Some(review) = self.confirmation.as_mut() else {
            return y;
        };
        for (index, field) in review.fields.iter_mut().enumerate() {
            y = draw_text(paint, &field.label, &label_style, (body.x0, y), width, true) + small;
            let multiline = draft
                .fields
                .iter()
                .any(|draft_field| draft_field.path == field.path && draft_field.multiline);
            let height = if multiline {
                line.mul_add(EDITOR_LINES, gap)
            } else {
                control
            };
            field.rect = Rect::new(body.x0, y, body.x1, y + height);
            field.input.set_disabled(disabled);
            field.input.apply_layout_external(field.rect);
            field
                .input
                .set_focus(focus == Some(ActivityFocus::ConfirmField(index)));
            field.input.draw(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &PresenceCtx::empty(),
            );
            y += height + gap;
        }
        if !fixed.is_empty() {
            let labels: Vec<String> = fixed.iter().map(|field| row_label(field)).collect();
            let column = label_column(paint, &labels, body.width());
            y = draw_fields(paint, body, y, (fixed.into_iter(), column));
        }
        y
    }

    fn draw_reason(&mut self, paint: &mut PaintCtx<'_>, body: Rect, mut y: f64) -> f64 {
        let small = f64::from(paint.theme.tokens.spacing.s2);
        let label_style = paint.theme.text(TextRole::Label);
        let control = ButtonFace::height(ControlSize::Regular, paint.theme);
        let focused = self.keyboard_focus == Some(ActivityFocus::DeclineReason);
        let disabled = self.review_pending.is_some();
        let width = text_width(body.width());
        y = draw_text(
            paint,
            &t("activity-confirm-reason-label"),
            &label_style,
            (body.x0, y),
            width,
            true,
        ) + small;
        let Some(review) = self.confirmation.as_mut() else {
            return y;
        };
        review.reason_rect = Rect::new(body.x0, y, body.x1, y + control);
        review.reason.set_disabled(disabled);
        review.reason.apply_layout_external(review.reason_rect);
        review.reason.set_focus(focused);
        review.reason.draw(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &PresenceCtx::empty(),
        );
        y + control
    }

    fn draw_confirmation_buttons(
        &mut self,
        paint: &mut PaintCtx<'_>,
        body: Rect,
        y: f64,
        mode: ConfirmMode,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s2);
        let height = ButtonFace::height(ControlSize::Regular, paint.theme);
        let (leading, trailing): (&[(TaskAction, ButtonRole)], Option<TaskAction>) = match mode {
            ConfirmMode::Review => (
                &[
                    (TaskAction::ConfirmDraft, ButtonRole::Primary),
                    (TaskAction::EditDraft, ButtonRole::Secondary),
                    (TaskAction::DeclineDraft, ButtonRole::Secondary),
                ],
                self.may_grant.then_some(TaskAction::AlwaysAllow),
            ),
            ConfirmMode::Edit => (
                &[
                    (TaskAction::ConfirmDraft, ButtonRole::Primary),
                    (TaskAction::DiscardEdits, ButtonRole::Secondary),
                ],
                None,
            ),
            ConfirmMode::Decline => (
                &[
                    (TaskAction::SendDecline, ButtonRole::Destructive),
                    (TaskAction::KeepDraft, ButtonRole::Secondary),
                ],
                None,
            ),
        };
        let disabled = self.review_pending.is_some();
        let mut x = body.x0;
        let mut row = y;
        let mut place = |label: &str, role: ButtonRole, paint: &mut PaintCtx<'_>| {
            let width = ButtonFace::new(Rect::ZERO, label)
                .role(role)
                .intrinsic_width(paint.text, paint.theme)
                .min(body.width());
            if x > body.x0 && x + width > body.x1 {
                x = body.x0;
                row += height + gap;
            }
            let rect = Rect::new(x, row, x + width, row + height);
            x += width + gap;
            rect
        };
        let mut placed = Vec::new();
        for (action, role) in leading {
            let label = action.label();
            let rect = place(&label, *role, paint);
            placed.push((*action, *role, label, rect));
        }
        if let Some(action) = trailing {
            let label = action.label();
            let width = ButtonFace::new(Rect::ZERO, &label)
                .role(ButtonRole::Plain)
                .intrinsic_width(paint.text, paint.theme);
            // Away from the answers, at the end of the row when it fits.
            let rect = if x + gap + width <= body.x1 {
                Rect::new(body.x1 - width, row, body.x1, row + height)
            } else {
                row += height + gap;
                Rect::new(body.x0, row, body.x0 + width, row + height)
            };
            placed.push((action, ButtonRole::Plain, label, rect));
        }
        for (action, role, label, rect) in placed {
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(rect, &label)
                    .role(role)
                    .disabled(disabled)
                    .focused(self.keyboard_focus == Some(ActivityFocus::TaskAction(action))),
            );
            self.task_action_rects.push((action, rect));
        }
        let mut bottom = row + height;
        if mode == ConfirmMode::Review && self.may_grant {
            // Always allow writes the workspace's agent access, which only
            // the device that manages the workspace can do; say so before
            // it is pressed rather than after it is refused.
            let caption = paint.theme.text(TextRole::Caption);
            bottom = draw_text(
                paint,
                &t("activity-confirm-always-note"),
                &caption,
                (body.x0, bottom + gap),
                text_width(body.width()),
                true,
            );
        }
        bottom
    }
}

/// Draw `text` at `(x, y)` wrapped to `width`, dim when `dim`; returns the
/// line below it.
fn draw_text(
    paint: &mut PaintCtx<'_>,
    text: &str,
    style: &ui::text_render::TextStyle,
    (x, y): (f64, f64),
    width: f32,
    dim: bool,
) -> f64 {
    let (_, height) = paint.text.measure_style(text, Some(width), style);
    let ink = if dim {
        paint.theme.tokens.colors.text_dim
    } else {
        paint.theme.tokens.colors.text_primary
    };
    paint.text.draw_style(
        paint.scene,
        text,
        ink,
        at(paint.transform, x, y),
        Some(width),
        style,
    );
    y + f64::from(height)
}

/// The width of the label column for `labels` in a body `width` wide.
fn label_column(paint: &mut PaintCtx<'_>, labels: &[String], width: f64) -> f64 {
    let style = paint.theme.text(TextRole::Caption);
    let widest = labels
        .iter()
        .map(|label| f64::from(paint.text.measure_style(label, None, &style).0))
        .fold(0.0, f64::max);
    widest.min(width * 0.32).ceil()
}

/// One `label  value` row; returns the line below it.
fn draw_row(
    paint: &mut PaintCtx<'_>,
    body: Rect,
    y: f64,
    (label, column): (&str, f64),
    value: &str,
    emphasis: bool,
) -> f64 {
    let gap = f64::from(paint.theme.tokens.spacing.s3);
    let caption = paint.theme.text(TextRole::Caption);
    let mut style = paint.theme.text(TextRole::Body);
    if emphasis {
        style = style.with_weight(ui::text_render::TextWeight::Medium);
    }
    // Captions sit on the value's first baseline.
    let lift = f64::from(style.line_height_px() - caption.line_height_px()).max(0.0) / 2.0;
    let label = paint
        .text
        .elide_to_width_style(label, text_width(column), &caption);
    draw_text(
        paint,
        &label,
        &caption,
        (body.x0, y + lift),
        text_width(column),
        true,
    );
    let x = body.x0 + column + gap;
    draw_text(paint, value, &style, (x, y), text_width(body.x1 - x), false)
}

/// The draft as label and value rows, lists as chips, labels in `column`.
fn draw_fields<'a>(
    paint: &mut PaintCtx<'_>,
    body: Rect,
    mut y: f64,
    (fields, column): (impl Iterator<Item = &'a DraftField>, f64),
) -> f64 {
    let small = f64::from(paint.theme.tokens.spacing.s2);
    for field in fields {
        let label = row_label(field);
        if let DraftValue::List { count, labels } = &field.value {
            let lead = if field.role == Some(MessageRole::Attachments) {
                ChipLead::Glyph(ui::icons::Icon::Paperclip)
            } else {
                ChipLead::None
            };
            y = draw_list(paint, body, y, (&label, column), (*count, labels), lead) + small;
        } else {
            let emphasis = field.role == Some(MessageRole::Subject);
            y = draw_row(
                paint,
                body,
                y,
                (&label, column),
                &value_text(&field.value),
                emphasis,
            ) + small;
        }
    }
    y
}

/// A list field: its heading in the label column, each entry as a chip.
fn draw_list(
    paint: &mut PaintCtx<'_>,
    body: Rect,
    y: f64,
    (heading, column): (&str, f64),
    (count, labels): (usize, &[String]),
    lead: ChipLead,
) -> f64 {
    let gap = f64::from(paint.theme.tokens.spacing.s3);
    let small = f64::from(paint.theme.tokens.spacing.s2);
    let caption = paint.theme.text(TextRole::Caption);
    let chip_h = ChipFace::height(paint.theme);
    let baseline = (chip_h - f64::from(caption.line_height_px())).max(0.0) / 2.0;
    let elided = paint
        .text
        .elide_to_width_style(heading, text_width(column), &caption);
    draw_text(
        paint,
        &elided,
        &caption,
        (body.x0, y + baseline),
        text_width(column),
        true,
    );
    let start = body.x0 + column + gap;
    let (mut x, mut row) = (start, y);
    let more = count.saturating_sub(labels.len());
    let more_label = (more > 0).then(|| t_args("activity-confirm-more", &[("count", more.into())]));
    let entries = labels
        .iter()
        .map(|label| (label.as_str(), lead))
        .chain(more_label.as_deref().map(|label| (label, ChipLead::None)));
    for (entry, lead) in entries {
        let width = ChipFace::intrinsic_width(entry, lead, false, paint.text, paint.theme)
            .min(body.x1 - start);
        if x > start && x + width > body.x1 {
            x = start;
            row += chip_h + small;
        }
        ChipFace::new(Rect::new(x, row, x + width, row + chip_h), entry)
            .lead(lead)
            .paint(paint.scene, paint.transform, paint.text, paint.theme);
        x += width + small;
    }
    row + chip_h
}

/// A message draft: who sends it and its other settings, recipients and
/// subject as rows, the body as a block with its paragraphs, attachments as
/// chips.
fn draw_message(paint: &mut PaintCtx<'_>, body: Rect, mut y: f64, draft: &Draft) -> f64 {
    let gap = f64::from(paint.theme.tokens.spacing.s3);
    let rest: Vec<&DraftField> = draft
        .fields
        .iter()
        .filter(|field| field.role.is_none())
        .collect();
    let heads: Vec<&DraftField> = [
        MessageRole::To,
        MessageRole::Cc,
        MessageRole::Bcc,
        MessageRole::Subject,
    ]
    .into_iter()
    .filter_map(|role| draft.role(role))
    .collect();
    let attachments = draft.role(MessageRole::Attachments);
    let labels: Vec<String> = rest
        .iter()
        .chain(&heads)
        .chain(attachments.as_ref())
        .map(|field| row_label(field))
        .collect();
    let column = label_column(paint, &labels, body.width());
    y = draw_fields(paint, body, y, (rest.into_iter().chain(heads), column));
    if let Some(field) = draft.role(MessageRole::Body) {
        y = draw_body(paint, body, y, &field.value.line()) + gap;
    }
    if let Some(field) = attachments {
        y = draw_fields(paint, body, y, (std::iter::once(field), column));
    }
    y
}

/// A message body as a quiet block, paragraph breaks kept, cut to
/// [`BODY_LINES`] lines.
fn draw_body(paint: &mut PaintCtx<'_>, body: Rect, y: f64, text: &str) -> f64 {
    let gap = f64::from(paint.theme.tokens.spacing.s3);
    let small = f64::from(paint.theme.tokens.spacing.s2);
    let style = paint.theme.text(TextRole::Body);
    let width = text_width(body.width() - 2.0 * gap);
    let line = f64::from(style.line_height_px());
    let mut lines: Vec<Option<String>> = Vec::new();
    for paragraph in text.lines() {
        if paragraph.trim().is_empty() {
            lines.push(None);
            continue;
        }
        let left = BODY_LINES.saturating_sub(lines.len());
        if left == 0 {
            break;
        }
        lines.extend(
            paint
                .text
                .clamp_to_lines(paragraph, &style, width, left)
                .into_iter()
                .map(Some),
        );
    }
    lines.truncate(BODY_LINES);
    while lines.last().is_some_and(Option::is_none) {
        lines.pop();
    }
    let height: f64 = lines
        .iter()
        .map(|entry| if entry.is_some() { line } else { line / 2.0 })
        .sum::<f64>()
        .max(line)
        + 2.0 * gap;
    let block = Rect::new(body.x0, y + small, body.x1, y + small + height);
    paint.scene.fill(
        vello::peniko::Fill::NonZero,
        paint.transform,
        paint.theme.tokens.colors.lift(0.35),
        None,
        &vello::kurbo::RoundedRect::from_rect(block, f64::from(paint.theme.tokens.radii.control)),
    );
    let mut line_y = block.y0 + gap;
    for entry in &lines {
        match entry {
            Some(text) => {
                draw_text(paint, text, &style, (block.x0 + gap, line_y), width, false);
                line_y += line;
            }
            None => line_y += line / 2.0,
        }
    }
    block.y1
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::ActionKind;
    use tilo_actions::{ActionContract, RiskLevel};

    fn paused(
        label: &str,
        kind: ActionKind,
        risk: RiskLevel,
        tags: &[&str],
    ) -> WorkspaceTaskConfirmation {
        let contract = ActionContract {
            kind,
            risk,
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            ..ActionContract::default()
        };
        let descriptor = tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new("folder", "do"),
            resource_kind: "files.folder".into(),
            resource_label: "Folder".into(),
            action_label: label.into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Document,
            contract,
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        };
        let arguments = serde_json::Map::new();
        let digest = tilo_actions::draft::arguments_digest(&arguments);
        WorkspaceTaskConfirmation {
            id: WorkspaceTaskConfirmation::id_for("folder", "do", &digest),
            asking: uuid::Uuid::from_u128(1),
            run_epoch: 1,
            agent_id: "scribe".into(),
            app: "so.tilo.file-explorer".into(),
            resource_kind: "files.folder".into(),
            resource: "folder".into(),
            action: "do".into(),
            kind,
            risk,
            draft: tilo_actions::Draft::render(&descriptor, &arguments),
            arguments_digest: digest,
            requested_at: 1,
            state: tilo_desktop_model::WorkspaceTaskConfirmationState::Waiting,
        }
    }

    fn chip(confirmation: &WorkspaceTaskConfirmation) -> String {
        risk_label(confirmation).0
    }

    /// A rename or move in the workspace is queued for the device and
    /// undone by queueing its revert: it changes the person's work, and
    /// only an action that cannot be taken back is said to send outside.
    #[test]
    fn an_undoable_queued_change_is_not_said_to_send_outside() {
        let rename = paused(
            "Rename an entry here",
            ActionKind::Outside,
            RiskLevel::Low,
            &[tilo_actions::COMPENSATED_UNDO_TAG],
        );
        assert_eq!(chip(&rename), t("activity-confirm-risk-changes"));
        let send = paused("Send", ActionKind::Outside, RiskLevel::Low, &[]);
        assert_eq!(chip(&send), t("activity-confirm-risk-sends"));
        let edit = paused("Edit", ActionKind::Undoable, RiskLevel::Low, &[]);
        assert_eq!(chip(&edit), t("activity-confirm-risk-changes"));
        let read = paused("Read", ActionKind::Read, RiskLevel::None, &[]);
        assert_eq!(chip(&read), t("activity-confirm-risk-reads"));
        let delete = paused(
            "Delete permanently",
            ActionKind::Outside,
            RiskLevel::Destructive,
            &[],
        );
        assert_eq!(chip(&delete), t("activity-confirm-risk-deletes"));
    }
}
