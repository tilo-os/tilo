//! Task review is a local view of authenticated owner evidence. Commands carry
//! the observation the person reviewed; refreshing never retargets a draft.
use super::{
    ActivityFocus, ActivityTree, ActivityView, Duration, ElementState, Key, LensSelection,
    Modifiers, NamedKey, PaintCtx, PresenceCtx, Rect, RoomActivityPane, SystemTime, TaskAction,
    TaskListOrigin, TextRole, Widget, WindowEvent, at, draw_task_button, format_relative_time, t,
    t_args, task_status_label, text_width,
};
use crate::activity::{TaskReviewCommand, TaskReviewRequest, TaskReviewSnapshot};

#[derive(Clone)]
pub(super) struct ApprovalFrontier {
    room: String,
    task_id: String,
    agent_id: String,
    run_epoch: u64,
    revision: u64,
}

pub(super) struct DirectionDraft {
    pub room: String,
    pub task_id: String,
    pub run_epoch: u64,
    pub revision: u64,
    pub actor: String,
    pub input: ui::widget::TextInput,
    pub rect: Rect,
}
impl RoomActivityPane {
    /// Shared field semantics used by the overlay accessibility bridge.
    pub fn perform_task_direction_action(&mut self, action: &ui::a11y::Action) -> bool {
        let Some(draft) = &mut self.direction else {
            return false;
        };
        if self.review_pending.is_some() {
            return false;
        }
        match action {
            ui::a11y::Action::SetValue(value) => {
                draft.input.set_text(value);
                true
            }
            ui::a11y::Action::Focus | ui::a11y::Action::Activate => {
                self.keyboard_focus = Some(ActivityFocus::DirectionField);
                draft.input.set_focus(true);
                true
            }
            _ => false,
        }
    }
    /// Poll the room owner's review and say where the task list comes from.
    pub(super) fn refresh_task_review(&mut self) -> TaskListOrigin {
        if self
            .review_approval
            .as_ref()
            .is_some_and(|approval| approval.room != self.task_room)
        {
            self.review_approval = None;
        }
        if self
            .direction
            .as_ref()
            .is_some_and(|draft| draft.room != self.task_room)
        {
            self.direction = None;
            self.review_pending = None;
            self.review_notice = None;
            self.review_task_offset = 0;
        }
        self.task_review = self.host.task_review_snapshot(&self.task_room);
        let origin = if let Some(Ok(snapshot)) = &self.task_review
            && snapshot.room == self.task_room
        {
            TaskListOrigin::Review(std::sync::Arc::as_ptr(snapshot) as usize)
        } else if self.host.task_review_required(&self.task_room) {
            TaskListOrigin::Withheld
        } else {
            TaskListOrigin::Tree
        };
        if let Some(invocation) = self.review_pending
            && let Some(result) = self.host.poll_task_review(invocation)
        {
            self.review_pending = None;
            match result {
                Ok(()) => {
                    self.direction = None;
                    self.review_notice = Some(t("activity-task-review-saved"));
                    self.keyboard_focus = self.selected_task.clone().map(ActivityFocus::Task);
                }
                Err(error) => {
                    self.review_notice = Some(t_args(
                        "activity-task-review-refused",
                        &[("detail", error.into())],
                    ));
                }
            }
        }
        origin
    }
    pub(super) fn reviewed_control(
        &self,
        task_id: &str,
    ) -> Option<(
        &TaskReviewSnapshot,
        &tilo_desktop_model::WorkspaceTaskControl,
    )> {
        let snapshot = self.task_review.as_ref()?.as_ref().ok()?;
        if snapshot.room != self.task_room {
            return None;
        }
        Some((snapshot, snapshot.controls.get(task_id)?))
    }
    pub(super) fn apply_review_action(&mut self, action: TaskAction) -> bool {
        if action == TaskAction::CloseDirection {
            self.direction = None;
            return true;
        }
        if matches!(action, TaskAction::NextTasks | TaskAction::PreviousTasks) {
            self.requested_review_task = None;
            let offset = if action == TaskAction::NextTasks {
                self.review_task_offset.saturating_add(1)
            } else {
                self.review_task_offset.saturating_sub(1)
            };
            self.review_task_offset = offset.min(self.tasks.len().saturating_sub(1));
            self.selected_task = self
                .tasks
                .get(self.review_task_offset)
                .map(|task| task.id.clone());
            self.keyboard_focus = self.selected_task.clone().map(ActivityFocus::Task);
            return true;
        }
        let Some(id) = self.selected_task.clone() else {
            return true;
        };
        if action == TaskAction::ReviewHistory {
            self.lens = LensSelection::WorkGroup(id);
            self.view = ActivityView::Explore;
            self.reinstall_filter();
            return true;
        }
        if self.review_pending.is_some() {
            return true;
        }
        if action == TaskAction::SendDirection {
            let Some(draft) = &self.direction else {
                return true;
            };
            self.submit_review(TaskReviewCommand::Steer {
                task_id: draft.task_id.clone(),
                run_epoch: draft.run_epoch,
                after_revision: draft.revision,
                text: draft.input.value().to_owned(),
            });
            return true;
        }
        let Some((snapshot, control)) = self.reviewed_control(&id) else {
            return true;
        };
        if !snapshot.writable {
            return true;
        }
        let epoch = control.run_epoch;
        let revision = control
            .directions
            .last()
            .map_or(0, |direction| direction.revision);
        match action {
            TaskAction::ApproveRun => {
                let Some(approval) = self
                    .review_approval
                    .clone()
                    .filter(|approval| approval.room == self.task_room && approval.task_id == id)
                else {
                    return true;
                };
                self.submit_review(TaskReviewCommand::Approve {
                    task_id: approval.task_id,
                    agent_id: approval.agent_id,
                    run_epoch: approval.run_epoch,
                    after_revision: approval.revision,
                });
            }
            TaskAction::Steer => {
                self.direction = Some(DirectionDraft {
                    room: self.task_room.clone(),
                    task_id: id,
                    run_epoch: epoch,
                    revision,
                    actor: snapshot.actor.clone(),
                    input: ui::widget::TextInput::local()
                        .with_placeholder(t("activity-task-direction-placeholder"))
                        .with_accessible_name(t("activity-task-direction-label"))
                        .with_id("task-direction"),
                    rect: Rect::ZERO,
                });
                self.keyboard_focus = Some(ActivityFocus::DirectionField);
                self.review_notice = None;
            }
            TaskAction::CancelRun => self.submit_review(TaskReviewCommand::Cancel {
                task_id: id,
                run_epoch: epoch,
                after_revision: revision,
            }),
            TaskAction::RetryRun => self.submit_review(TaskReviewCommand::Retry {
                task_id: id,
                run_epoch: epoch,
            }),
            _ => return false,
        }
        true
    }
    pub(super) fn submit_review(&mut self, command: TaskReviewCommand) {
        let observed_actor = if matches!(&command, TaskReviewCommand::Steer { .. }) {
            self.direction.as_ref().map(|draft| draft.actor.clone())
        } else {
            self.task_review
                .as_ref()
                .and_then(|snapshot| snapshot.as_ref().ok())
                .map(|snapshot| snapshot.actor.clone())
        };
        let Some(observed_actor) = observed_actor else {
            return;
        };
        let invocation = uuid::Uuid::new_v4();
        match self.host.submit_task_review(TaskReviewRequest {
            room: self.task_room.clone(),
            observed_actor,
            invocation,
            command,
        }) {
            Ok(()) => {
                self.review_pending = Some(invocation);
                self.review_notice = Some(t("activity-task-review-saving"));
            }
            Err(error) => {
                self.review_notice = Some(t_args(
                    "activity-task-review-refused",
                    &[("detail", error.into())],
                ));
            }
        }
    }
    pub(super) fn draw_direction(&mut self, paint: &mut PaintCtx<'_>, bounds: Rect) {
        let Some(draft) = &mut self.direction else {
            return;
        };
        let style = paint.theme.text(TextRole::Body);
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let width = text_width(bounds.width() - 2.0 * gap);
        let mut y = bounds.y0 + gap;
        let summary = t_args(
            "activity-task-direction-observation",
            &[
                ("epoch", draft.run_epoch.into()),
                ("revision", draft.revision.into()),
            ],
        );
        let (_, h) = paint.text.measure_style(&summary, Some(width), &style);
        paint.text.draw_style(
            paint.scene,
            &summary,
            paint.theme.tokens.colors.text_primary,
            at(paint.transform, bounds.x0 + gap, y),
            Some(width),
            &style,
        );
        y += f64::from(h) + gap;
        let input_h = f64::from(style.line_height_px()).max(30.0) + 2.0 * gap;
        draft.rect = Rect::new(bounds.x0 + gap, y, bounds.x1 - gap, y + input_h);
        draft.input.set_disabled(self.review_pending.is_some());
        draft.input.apply_layout_external(draft.rect);
        draft
            .input
            .set_focus(self.keyboard_focus == Some(ActivityFocus::DirectionField));
        draft.input.draw(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            &PresenceCtx::empty(),
        );
        y += input_h + gap;
        if let Some(notice) = &self.review_notice {
            let (_, h) = paint.text.measure_style(notice, Some(width), &style);
            paint.text.draw_style(
                paint.scene,
                notice,
                paint.theme.tokens.colors.text_dim,
                at(paint.transform, bounds.x0 + gap, y),
                Some(width),
                &style,
            );
            y += f64::from(h) + gap;
        }
        let height = f64::from(style.line_height_px()).max(28.0) + 2.0 * gap;
        for action in [TaskAction::SendDirection, TaskAction::CloseDirection] {
            let rect = Rect::new(bounds.x0 + gap, y, bounds.x1 - gap, y + height);
            if rect.y1 > bounds.y1 - gap {
                break;
            }
            draw_task_button(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                rect,
                &action.label(),
                action == TaskAction::SendDirection,
            );
            self.task_action_rects.push((action, rect));
            y += height + gap;
        }
    }
    pub(super) fn direction_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        tree: &mut impl ActivityTree,
    ) -> bool {
        if self.direction.is_some()
            && matches!(event,WindowEvent::KeyboardInput {event,..} if event.state==ElementState::Pressed && event.logical_key==Key::Named(NamedKey::Escape))
        {
            self.apply_review_action(TaskAction::CloseDirection);
            return true;
        }
        if self.keyboard_focus != Some(ActivityFocus::DirectionField) || self.direction.is_none() {
            return false;
        }
        if matches!(event,WindowEvent::KeyboardInput {event,..} if matches!(event.logical_key,Key::Named(NamedKey::Tab|NamedKey::Escape)))
        {
            return false;
        }
        let mut scratch = taffy::TaffyTree::<()>::new();
        let Ok(node) = scratch.new_leaf(taffy::Style::default()) else {
            return false;
        };
        let mut handled = false;
        let mut focus = None;
        let Some(draft) = &mut self.direction else {
            return false;
        };
        draft.input.set_focus(true);
        draft.input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state: ui::widget::Edits::lend(tree.document_mut()),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &scratch,
            },
        );
        handled
    }
}

impl RoomActivityPane {
    fn draw_review_action_grid(
        &mut self,
        paint: &mut PaintCtx<'_>,
        bounds: Rect,
        mut y: f64,
        actions: &[TaskAction],
    ) {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let height =
            f64::from(paint.theme.text(TextRole::Body).line_height_px()).max(28.0) + 2.0 * gap;
        let width = (bounds.width() - 3.0 * gap) / 2.0;
        for pair in actions.chunks(2) {
            if y + gap + height > bounds.y1 - gap {
                break;
            }
            for (action, left) in pair.iter().zip([true, false]) {
                let x = if left {
                    bounds.x0 + gap
                } else {
                    bounds.x0 + 2.0 * gap + width
                };
                let rect = Rect::new(x, y + gap, x + width, y + gap + height);
                draw_task_button(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    rect,
                    &action.label(),
                    false,
                );
                self.task_action_rects.push((*action, rect));
            }
            y += height + gap;
        }
    }
    pub(super) fn draw_review_tasks(&mut self, paint: &mut PaintCtx<'_>, bounds: Rect) {
        self.review_approval = None;
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let style = paint.theme.text(TextRole::Body);
        let width = text_width(bounds.width() - 2.0 * gap);
        let mut y = bounds.y0 + gap;
        let Some(Ok(snapshot)) = self.task_review.clone() else {
            return;
        };
        let snapshot = &*snapshot;
        let source = review_source(snapshot, self.now());
        draw_review_line(paint, &source, &style, width, bounds.x0 + gap, &mut y);
        if let Some(notice) = &self.review_notice {
            draw_review_line(paint, notice, &style, width, bounds.x0 + gap, &mut y);
        }
        if self
            .requested_review_task
            .as_ref()
            .is_some_and(|id| !self.tasks.iter().any(|task| &task.id == id))
        {
            draw_review_line(
                paint,
                &t("activity-task-review-missing"),
                &style,
                width,
                bounds.x0 + gap,
                &mut y,
            );
            return;
        }
        let selected = self
            .selected_task
            .as_ref()
            .and_then(|id| self.tasks.iter().position(|task| &task.id == id));
        self.review_task_offset = selected
            .unwrap_or(self.review_task_offset)
            .min(self.tasks.len().saturating_sub(1));
        let Some(task) = self.tasks.get(self.review_task_offset).cloned() else {
            draw_review_line(
                paint,
                &t("activity-tasks-empty"),
                &style,
                width,
                bounds.x0 + gap,
                &mut y,
            );
            return;
        };
        self.selected_task = Some(task.id.clone());
        let top = y;
        draw_review_line(paint, &task.title, &style, width, bounds.x0 + gap, &mut y);
        let summary = t_args(
            "activity-task-review-status",
            &[
                ("status", task_status_label(task.status).into()),
                (
                    "agent",
                    task.assigned_agent
                        .clone()
                        .unwrap_or_else(|| t("activity-task-executor-none"))
                        .into(),
                ),
            ],
        );
        draw_review_line(paint, &summary, &style, width, bounds.x0 + gap, &mut y);
        y = self.draw_task_evidence(paint, (snapshot, &task), bounds, (top, y));
        let mut actions = self.task_actions_for(&task);
        if self.review_task_offset > 0 {
            actions.insert(0, TaskAction::PreviousTasks);
        }
        if self.review_task_offset.saturating_add(1) < self.tasks.len() {
            actions.insert(0, TaskAction::NextTasks);
        }
        let approval = snapshot.controls.get(&task.id).and_then(|control| {
            Some(ApprovalFrontier {
                room: snapshot.room.clone(),
                task_id: task.id.clone(),
                agent_id: task.assigned_agent.clone()?,
                run_epoch: control.run_epoch,
                revision: control
                    .directions
                    .last()
                    .map_or(0, |direction| direction.revision),
            })
        });
        self.draw_review_action_grid(paint, bounds, y, &actions);
        if self
            .task_action_rects
            .iter()
            .any(|(action, _)| *action == TaskAction::ApproveRun)
        {
            self.review_approval = approval;
        }
    }
}
impl RoomActivityPane {
    /// What the owner records of `task`'s run: the action it paused at, as
    /// a card the person answers, or its run evidence.
    fn draw_task_evidence(
        &mut self,
        paint: &mut PaintCtx<'_>,
        (snapshot, task): (&TaskReviewSnapshot, &tilo_desktop_model::WorkspaceTask),
        bounds: Rect,
        (top, mut y): (f64, f64),
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let width = text_width(bounds.width() - 2.0 * gap);
        let confirmation = self
            .reviewed_confirmation(&task.id)
            .map(|(_, confirmation)| confirmation);
        if confirmation.is_none()
            && let Some(control) = snapshot.controls.get(&task.id)
        {
            draw_control_evidence(paint, control, width, bounds.x0 + gap, &mut y, self.now());
        }
        self.task_rects
            .push((task.id.clone(), Rect::new(bounds.x0, top, bounds.x1, y)));
        if let Some(confirmation) = &confirmation {
            let inner = Rect::new(bounds.x0 + gap, bounds.y0, bounds.x1 - gap, bounds.y1);
            y = self.draw_confirmation_card(paint, inner, y + gap, confirmation);
        }
        y
    }
}

fn draw_review_line(
    paint: &mut PaintCtx<'_>,
    text: &str,
    style: &ui::text_render::TextStyle,
    width: f32,
    x: f64,
    y: &mut f64,
) {
    let text = paint.text.elide_to_width_style(text, width, style);
    let (_, height) = paint.text.measure_style(&text, Some(width), style);
    paint.text.draw_style(
        paint.scene,
        &text,
        paint.theme.tokens.colors.text_primary,
        at(paint.transform, x, *y),
        Some(width),
        style,
    );
    *y += f64::from(height) + f64::from(paint.theme.tokens.spacing.s2);
}

fn draw_control_evidence(
    paint: &mut PaintCtx<'_>,
    control: &tilo_desktop_model::WorkspaceTaskControl,
    width: f32,
    x: f64,
    y: &mut f64,
    now: SystemTime,
) {
    let style = paint.theme.text(TextRole::Body);
    let revision = control
        .directions
        .last()
        .map_or(0, |direction| direction.revision);
    let applied = control.active_run.as_ref().map_or_else(
        || {
            control
                .directions
                .iter()
                .filter(|direction| direction.applied.is_some())
                .map(|direction| direction.revision)
                .max()
                .unwrap_or(0)
        },
        |run| run.applied_direction_revision,
    );
    let status = t_args(
        "activity-task-review-frontier",
        &[
            ("epoch", control.run_epoch.into()),
            ("revision", revision.into()),
            ("applied", applied.into()),
        ],
    );
    draw_review_line(paint, &status, &style, width, x, y);
    for line in task_run_evidence(control, now) {
        let line = paint.text.elide_to_width_style(&line, width, &style);
        draw_review_line(paint, &line, &style, width, x, y);
    }
    if let Some(direction) = control.directions.last() {
        let text = t_args(
            "activity-task-review-direction",
            &[
                ("author", direction.author.as_str().into()),
                ("text", direction.text.as_str().into()),
            ],
        );
        let text = paint.text.elide_to_width_style(&text, width, &style);
        draw_review_line(paint, &text, &style, width, x, y);
    }
}

fn review_source(snapshot: &TaskReviewSnapshot, now: SystemTime) -> String {
    t_args(
        "activity-task-review-source",
        &[
            ("generation", snapshot.generation.into()),
            (
                "time",
                format_relative_time(
                    SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_secs(
                            snapshot.sampled_at_secs.try_into().unwrap_or(0),
                        ))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    now,
                )
                .into(),
            ),
        ],
    )
}

pub(super) fn task_run_evidence(
    control: &tilo_desktop_model::WorkspaceTaskControl,
    now: SystemTime,
) -> Vec<String> {
    let Some(approval) = control
        .active_run
        .as_ref()
        .and_then(|run| run.approval.as_ref())
    else {
        return Vec::new();
    };
    let time = |seconds: i64| {
        format_relative_time(
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(seconds.try_into().unwrap_or(0)))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            now,
        )
    };
    let approved = t_args(
        "activity-task-run-approved",
        &[
            ("person", approval.person.as_str().into()),
            ("time", time(approval.approved_at).into()),
        ],
    );
    let phase = control.execution.as_ref().map_or_else(
        || t("activity-task-awaiting-acceptance"),
        |execution| {
            let key = match execution.phase {
                tilo_desktop_model::WorkspaceTaskExecutionPhase::Accepted => {
                    "activity-task-run-accepted"
                }
                tilo_desktop_model::WorkspaceTaskExecutionPhase::Planning => {
                    "activity-task-run-planning"
                }
                tilo_desktop_model::WorkspaceTaskExecutionPhase::Executing => {
                    "activity-task-run-executing"
                }
            };
            t_args(key, &[("time", time(execution.updated_at).into())])
        },
    );
    vec![approved, phase]
}
