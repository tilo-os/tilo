//! Task review is a local view of authenticated owner evidence. Commands carry
//! the observation the person reviewed; refreshing never retargets a draft.
use super::{
    ActivityFocus, ActivityTree, ActivityView, Duration, ElementState, Key, LensSelection,
    Modifiers, NamedKey, PaintCtx, PresenceCtx, Rect, RoomActivityPane, SystemTime, TaskAction,
    TaskListOrigin, TextRole, Widget, WindowEvent, at, draw_task_button, format_relative_time, t,
    t_args, text_width,
};
use crate::activity::{TaskReviewCommand, TaskReviewRequest, TaskReviewSnapshot};
use tilo_sort::CompactSort as _;
use ui::icons::Icon;
use ui::text_render::TextWeight;
use ui::widget::{ButtonRole, ControlSize, ScrollMetrics, Scrollbar};
use vello::peniko::Color;

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
            let undo = std::mem::take(&mut self.review_pending_undo);
            match result {
                Ok(()) if undo => {
                    self.review_notice = Some(t("activity-undo-task-done"));
                    self.agent_undo_notice = None;
                }
                Ok(()) => {
                    self.direction = None;
                    self.review_notice = Some(t("activity-task-review-waiting"));
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
                // Storing the review is the system's job; what the agent
                // does with it is the news.
                self.review_notice = None;
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
        let summary = t("activity-task-direction-observation");
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

/// Where a reviewed task's actions sit in their row: what moves the task on
/// comes first, leaving it alone or ending it last.
fn review_action_rank(action: TaskAction) -> u8 {
    match action {
        TaskAction::ApproveRun => 0,
        TaskAction::StartAgent => 1,
        TaskAction::RetryRun => 2,
        TaskAction::Steer => 3,
        TaskAction::OpenResource => 4,
        TaskAction::ReviewHistory => 5,
        TaskAction::CancelRun => 7,
        TaskAction::Details => 8,
        _ => 6,
    }
}

impl RoomActivityPane {
    /// The reviewed task's actions as buttons at their natural width,
    /// flowing onto further rows when the sheet is narrow.
    fn draw_review_actions(
        &mut self,
        paint: &mut PaintCtx<'_>,
        bounds: Rect,
        y: f64,
        actions: &[TaskAction],
        task: &tilo_desktop_model::WorkspaceTask,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s2);
        let height = ui::widget::ButtonFace::height(ControlSize::Regular, paint.theme);
        let (mut x, mut y) = (bounds.x0, y);
        for (index, action) in actions.iter().enumerate() {
            let label = self.task_action_label(*action, task);
            let role = match action {
                TaskAction::ApproveRun | TaskAction::StartAgent | TaskAction::RetryRun
                    if index == 0 =>
                {
                    ButtonRole::Primary
                }
                TaskAction::Details => ButtonRole::Plain,
                _ => ButtonRole::Secondary,
            };
            let face = ui::widget::ButtonFace::new(Rect::ZERO, &label).role(role);
            let width = face
                .intrinsic_width(paint.text, paint.theme)
                .min(bounds.width());
            if x > bounds.x0 && x + width > bounds.x1 {
                x = bounds.x0;
                y += height + gap;
            }
            let rect = Rect::new(x, y, x + width, y + height);
            ui::widget::Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ui::widget::ButtonFace::new(rect, &label).role(role),
            );
            self.task_action_rects.push((*action, rect));
            x += width + gap;
        }
        y + height
    }

    /// "2 of 8" between Previous and Next. Each keeps its side whether or
    /// not the other is offered, so a button never moves under the pointer.
    fn draw_review_pager(&mut self, paint: &mut PaintCtx<'_>, bounds: Rect, y: f64) -> f64 {
        let height = ui::widget::ButtonFace::height(ControlSize::Small, paint.theme);
        let caption = paint.theme.text(TextRole::Caption);
        let position = t_args(
            "activity-task-position",
            &[
                ("index", self.review_task_offset.saturating_add(1).into()),
                ("count", self.tasks.len().into()),
            ],
        );
        let (w, h) = paint.text.measure_style(&position, None, &caption);
        paint.text.draw_style(
            paint.scene,
            &position,
            paint.theme.tokens.colors.text_dim,
            at(
                paint.transform,
                bounds.center().x - f64::from(w) / 2.0,
                y + (height - f64::from(h)) / 2.0,
            ),
            None,
            &caption,
        );
        let previous = self.review_task_offset > 0;
        let next = self.review_task_offset.saturating_add(1) < self.tasks.len();
        for (action, shown, icon, leading) in [
            (TaskAction::PreviousTasks, previous, Icon::ChevronLeft, true),
            (TaskAction::NextTasks, next, Icon::ChevronRight, false),
        ] {
            if !shown {
                continue;
            }
            let label = action.label();
            let face = ui::widget::ButtonFace::new(Rect::ZERO, &label)
                .icon(icon)
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain);
            let width = face.intrinsic_width(paint.text, paint.theme);
            let x0 = if leading {
                bounds.x0
            } else {
                bounds.x1 - width
            };
            let rect = Rect::new(x0, y, x0 + width, y + height);
            ui::widget::Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ui::widget::ButtonFace::new(rect, &label)
                    .icon(icon)
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Plain),
            );
            self.task_action_rects.push((action, rect));
        }
        y + height + f64::from(paint.theme.tokens.spacing.s3)
    }

    /// What happened to the run, the last instruction, and — behind
    /// Details — the owner's sample and the run counters.
    fn review_evidence(
        &self,
        snapshot: &TaskReviewSnapshot,
        control: &tilo_desktop_model::WorkspaceTaskControl,
    ) -> Vec<String> {
        let now = self.now();
        let local = self.local_user();
        let mut lines = task_run_evidence(control, now, |id| {
            (id != local).then(|| self.person_label(id))
        });
        if let Some(direction) = control.directions.last() {
            lines.push(t_args(
                "activity-task-review-direction",
                &[
                    ("author", self.person_label(&direction.author).into()),
                    ("text", direction.text.as_str().into()),
                ],
            ));
        }
        if self.review_details {
            lines.push(review_source(snapshot, now));
            lines.push(review_frontier(control));
        }
        lines
    }

    /// The reviewed task in a viewport of its own that scrolls when the
    /// task, its card and its buttons run past the sheet — at a large text
    /// scale, or with a long draft to confirm.
    pub(super) fn draw_scrolling_review(
        &mut self,
        paint: &mut PaintCtx<'_>,
        viewport: Rect,
        focused: Option<Rect>,
    ) {
        let mut scroll = self.review_scroll;
        scroll.viewport = Some(viewport);
        // Keep the control that has keyboard focus in view, from where it
        // was painted last frame.
        let in_review = matches!(
            self.keyboard_focus,
            Some(
                ActivityFocus::TaskAction(_)
                    | ActivityFocus::Task(_)
                    | ActivityFocus::ConfirmField(_)
                    | ActivityFocus::DeclineReason
            )
        );
        if let Some(focused) = focused.filter(|_| in_review) {
            if focused.y1 > viewport.y1 {
                scroll.offset += focused.y1 - viewport.y1;
            } else if focused.y0 < viewport.y0 {
                scroll.offset -= viewport.y0 - focused.y0;
            }
        }
        let max = ScrollMetrics::new(scroll.content, viewport.height(), 0.0).max_scroll();
        scroll.offset = scroll.offset.clamp(0.0, max);
        let gutter = f64::from(paint.theme.tokens.metrics.scrollbar_gutter);
        let content = Rect::new(
            viewport.x0,
            viewport.y0 - scroll.offset,
            viewport.x1 - gutter / 2.0,
            f64::MAX / 4.0,
        );
        paint.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            paint.transform,
            &viewport,
        );
        let bottom = self.draw_review_tasks(paint, content);
        paint.scene.pop_layer();
        scroll.content = bottom - content.y0;
        let metrics = ScrollMetrics::new(scroll.content, viewport.height(), scroll.offset);
        if metrics.overflows() {
            let bar = Scrollbar::vertical();
            let lane = Rect::new(viewport.x1 - gutter, viewport.y0, viewport.x1, viewport.y1);
            let track = bar.track_in(lane, paint.theme);
            bar.draw(
                paint.scene,
                paint.transform,
                track,
                metrics,
                paint.theme,
                false,
            );
        }
        self.review_scroll = scroll;
    }

    /// Scroll the reviewed task by a wheel turn over it; whether it moved.
    pub(super) fn scroll_review(
        &mut self,
        point: vello::kurbo::Point,
        delta: &winit::event::MouseScrollDelta,
    ) -> bool {
        let Some(viewport) = self.review_scroll.viewport else {
            return false;
        };
        if self.view != ActivityView::Tasks || !viewport.contains(point) {
            return false;
        }
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, lines) => f64::from(*lines) * 40.0,
            winit::event::MouseScrollDelta::PixelDelta(pixels) => pixels.y,
        };
        let max =
            ScrollMetrics::new(self.review_scroll.content, viewport.height(), 0.0).max_scroll();
        let offset = (self.review_scroll.offset - dy).clamp(0.0, max);
        let moved = (offset - self.review_scroll.offset).abs() > f64::EPSILON;
        self.review_scroll.offset = offset;
        moved
    }

    /// Paint the reviewed task from `bounds`' top; returns where it ends.
    pub(super) fn draw_review_tasks(&mut self, paint: &mut PaintCtx<'_>, bounds: Rect) -> f64 {
        self.review_approval = None;
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let body = paint.theme.text(TextRole::Body);
        let caption = paint.theme.text(TextRole::Caption);
        let inner = Rect::new(bounds.x0 + gap, bounds.y0, bounds.x1 - gap, bounds.y1 - gap);
        let width = text_width(inner.width());
        let x = inner.x0;
        let mut y = bounds.y0 + gap;
        let Some(Ok(snapshot)) = self.task_review.clone() else {
            return y;
        };
        let snapshot = &*snapshot;
        let primary = paint.theme.tokens.colors.text_primary;
        let dim = paint.theme.tokens.colors.text_dim;
        if self
            .requested_review_task
            .as_ref()
            .is_some_and(|id| !self.tasks.iter().any(|task| &task.id == id))
        {
            let missing = t("activity-task-review-missing");
            draw_review_line(paint, (&missing, primary), &body, width, x, &mut y);
            return y;
        }
        let selected = self
            .selected_task
            .as_ref()
            .and_then(|id| self.tasks.iter().position(|task| &task.id == id));
        self.review_task_offset = selected
            .unwrap_or(self.review_task_offset)
            .min(self.tasks.len().saturating_sub(1));
        let Some(task) = self.tasks.get(self.review_task_offset).cloned() else {
            let empty = t("activity-tasks-empty");
            draw_review_line(paint, (&empty, primary), &body, width, x, &mut y);
            return y;
        };
        self.selected_task = Some(task.id.clone());
        if self.tasks.len() > 1 {
            y = self.draw_review_pager(paint, inner, y);
        }
        let top = y;
        y = draw_review_title(paint, &task.title, (x, y), width);
        // Who is doing it and where it stands.
        let asking = self
            .reviewed_confirmation(&task.id)
            .is_some_and(|(_, confirmation)| confirmation.is_waiting());
        let state = review_state_line(
            &task,
            task.assigned_agent
                .as_deref()
                .map(|id| self.agent_label(id)),
            asking,
        );
        draw_review_line(paint, (&state, primary), &body, width, x, &mut y);
        if let Some(notice) = self.review_notice.clone() {
            draw_review_line(paint, (&notice, dim), &caption, width, x, &mut y);
        }
        // An action the run paused at is answered on its own card; the run
        // evidence returns once it is answered.
        let confirmation = self
            .reviewed_confirmation(&task.id)
            .map(|(_, confirmation)| confirmation);
        if confirmation.is_none()
            && let Some(control) = snapshot.controls.get(&task.id)
        {
            for line in self.review_evidence(snapshot, control) {
                draw_review_line(paint, (&line, dim), &caption, width, x, &mut y);
            }
        }
        self.task_rects
            .push((task.id.clone(), Rect::new(bounds.x0, top, bounds.x1, y)));
        if let Some(confirmation) = &confirmation {
            y = self.draw_confirmation_card(paint, inner, y + gap, confirmation);
        }
        let mut actions = self.task_actions_for(&task);
        actions.compact_sort_by_key(|action| review_action_rank(*action));
        if snapshot.controls.contains_key(&task.id) {
            actions.push(TaskAction::Details);
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
        let bottom = self.draw_review_actions(paint, inner, y + gap, &actions, &task);
        if self
            .task_action_rects
            .iter()
            .any(|(action, _)| *action == TaskAction::ApproveRun)
        {
            self.review_approval = approval;
        }
        bottom + gap
    }
}

/// What the task is, in its own words: wrapped, at most two lines.
/// Returns the line below it.
fn draw_review_title(paint: &mut PaintCtx<'_>, title: &str, (x, y): (f64, f64), width: f32) -> f64 {
    let title_style = paint
        .theme
        .text(TextRole::Body)
        .with_weight(TextWeight::Medium);
    let (_, line_h) = paint.text.measure_style(title, None, &title_style);
    let (_, title_h) = paint.text.measure_style(title, Some(width), &title_style);
    let title = if title_h > 2.0 * line_h + 1.0 {
        // Elided to a little under two lines' run, so the wrap lands on the
        // second line whatever the word breaks.
        paint
            .text
            .elide_to_width_style(title, 1.8 * width, &title_style)
    } else {
        title.to_owned()
    };
    let (_, title_h) = paint.text.measure_style(&title, Some(width), &title_style);
    paint.text.draw_style(
        paint.scene,
        &title,
        paint.theme.tokens.colors.text_primary,
        at(paint.transform, x, y),
        Some(width),
        &title_style,
    );
    y + f64::from(title_h) + f64::from(paint.theme.tokens.spacing.s2)
}

/// One line of the reviewed task, elided rather than run past the sheet.
fn draw_review_line(
    paint: &mut PaintCtx<'_>,
    (text, color): (&str, Color),
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
        color,
        at(paint.transform, x, *y),
        Some(width),
        style,
    );
    *y += f64::from(height) + f64::from(paint.theme.tokens.spacing.s2);
}

/// Who is doing the task and where it stands, as one sentence.
/// `asking` when the run waits for the person to answer an action.
fn review_state_line(
    task: &tilo_desktop_model::WorkspaceTask,
    agent: Option<String>,
    asking: bool,
) -> String {
    let status = ("status", task.status.key().into());
    match agent {
        Some(agent) if asking => t_args(
            "activity-task-review-status-asking",
            &[("agent", agent.into())],
        ),
        Some(agent) => t_args(
            "activity-task-review-status",
            &[status, ("agent", agent.into())],
        ),
        None => t_args("activity-task-review-status-unassigned", &[status]),
    }
}

/// The run counters the owner keeps, for someone checking why a run
/// waits: which run, how many instructions, how many the agent applied.
fn review_frontier(control: &tilo_desktop_model::WorkspaceTaskControl) -> String {
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
    t_args(
        "activity-task-review-frontier",
        &[
            ("epoch", control.run_epoch.into()),
            ("revision", revision.into()),
            ("applied", applied.into()),
        ],
    )
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

/// What happened to the active run in plain words. `person` names an
/// approver, or gives `None` for the person looking.
pub(super) fn task_run_evidence(
    control: &tilo_desktop_model::WorkspaceTaskControl,
    now: SystemTime,
    person: impl Fn(&str) -> Option<String>,
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
    let when = ("time", time(approval.approved_at).into());
    let approved = match person(&approval.person) {
        Some(name) => t_args(
            "activity-task-run-approved",
            &[("person", name.into()), when],
        ),
        None => t_args("activity-task-run-approved-by-you", &[when]),
    };
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
