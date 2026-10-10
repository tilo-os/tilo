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
pub(super) struct ReviewButton {
    action: TaskAction,
    button: ui::widget::Embedded<ui::widget::Button>,
    pressed: std::rc::Rc<std::cell::Cell<bool>>,
}
#[derive(Clone, PartialEq)]
pub(super) struct ReviewObservation {
    room: String,
    actor: String,
    task: String,
    epoch: u64,
    direction: u64,
    pending: tilo_desktop_model::WorkspaceTaskPendingFrontier,
}
impl RoomActivityPane {
    fn review_observation(&self) -> Option<ReviewObservation> {
        let id = self.selected_task.as_ref()?;
        let (snapshot, control) = self.reviewed_control(id)?;
        let task = snapshot.tasks.iter().find(|task| &task.id == id)?;
        Some(ReviewObservation {
            room: snapshot.room.clone(),
            actor: snapshot.actor.clone(),
            task: id.clone(),
            epoch: control.run_epoch,
            direction: control
                .directions
                .last()
                .map_or(0, |direction| direction.revision),
            pending: tilo_desktop_model::WorkspaceTaskPendingFrontier {
                status: task.status,
                run_epoch: control.run_epoch,
                revision: control.pending_revision,
            },
        })
    }
    pub(super) fn review_pointer_is_clipped(
        &self,
        event: &WindowEvent,
        scale: f64,
        cursor: Option<vello::kurbo::Point>,
    ) -> bool {
        if self.view != ActivityView::Tasks
            || self.task_instruction_detail.is_some()
            || self.direction.is_some()
            || self
                .reviewed_control(self.selected_task.as_deref().unwrap_or(""))
                .is_none()
        {
            return false;
        }
        let point = match event {
            WindowEvent::CursorMoved { position, .. } => Some(vello::kurbo::Point::new(
                position.x / scale,
                position.y / scale,
            )),
            WindowEvent::MouseInput { .. } => cursor,
            _ => return false,
        };
        point.is_none_or(|point| !self.task_list_scroll.content_rect().contains(point))
    }
    pub(super) fn review_instruction(
        &self,
        task: &tilo_desktop_model::WorkspaceTask,
    ) -> Option<String> {
        let original = super::task_instruction(task);
        let current = self
            .reviewed_control(&task.id)
            .and_then(|(_, control)| control.instruction.as_deref());
        match (current, original) {
            (Some(current), Some(original)) if current != original => Some(format!(
                "{}\n\n{current}\n\n{}\n\n{original}",
                t("activity-task-current-instruction"),
                t("activity-task-original-instruction")
            )),
            _ => current.or(original).map(str::to_owned),
        }
    }
    pub(super) fn route_review_button_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        cursor: Option<vello::kurbo::Point>,
        tree: &mut impl ActivityTree,
    ) -> bool {
        if self.view != ActivityView::Tasks
            || self.task_instruction_detail.is_some()
            || self.direction.is_some()
            || self
                .reviewed_control(self.selected_task.as_deref().unwrap_or(""))
                .is_none()
        {
            return false;
        }
        let viewport = self.task_list_scroll.content_rect();
        let clipped = self.review_pointer_is_clipped(event, scale, cursor);
        let mut handled = false;
        let mut activated = None;
        for button in &mut self.review_buttons {
            if clipped || button.button.rect().intersect(viewport).area() <= 0.0 {
                button.button.widget_mut().cancel_pointer_interaction();
                button.pressed.set(false);
                continue;
            }
            button
                .button
                .widget_mut()
                .set_focus(self.keyboard_focus == Some(ActivityFocus::TaskAction(button.action)));
            button.button.handle_event(
                event,
                scale,
                &modifiers,
                &mut ui::widget::Edits::lend(tree.document_mut()),
                &automerge::ROOT,
                &mut handled,
            );
            if button.button.take_focus_request() {
                self.keyboard_focus = Some(ActivityFocus::TaskAction(button.action));
            }
            if button.pressed.replace(false) {
                activated = Some(button.action);
            }
        }
        if let Some(action) = activated {
            self.apply_task_action(action, tree);
            return true;
        }
        handled
    }
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
        if self.review_action_is_stale(action) {
            self.review_notice = Some(t("activity-task-review-changed"));
            return true;
        }
        if matches!(action, TaskAction::EditPending | TaskAction::CancelPending) {
            self.apply_review_pending(action, &id);
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
            TaskAction::ApproveRun => self.submit_reviewed_approval(&id),
            TaskAction::Steer => {
                self.direction = Some(DirectionDraft {
                    room: self.task_room.clone(),
                    task_id: id,
                    run_epoch: epoch,
                    revision,
                    actor: snapshot.actor.clone(),
                    input: ui::widget::TextInput::local()
                        .with_placeholder(&t("activity-task-direction-placeholder"))
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
    fn submit_reviewed_approval(&mut self, id: &str) {
        let Some(approval) = self
            .review_approval
            .clone()
            .filter(|approval| approval.room == self.task_room && approval.task_id == id)
        else {
            return;
        };
        self.submit_review(TaskReviewCommand::Approve {
            task_id: approval.task_id,
            agent_id: approval.agent_id,
            run_epoch: approval.run_epoch,
            after_revision: approval.revision,
        });
    }
    fn review_action_is_stale(&self, action: TaskAction) -> bool {
        let reopening_private_edit = action == TaskAction::EditPending
            && self.pending_task_editing
            && self
                .pending_task_detail
                .as_ref()
                .is_some_and(|detail| self.selected_task.as_ref() == Some(&detail.task_id));
        !reopening_private_edit
            && matches!(
                action,
                TaskAction::Steer
                    | TaskAction::CancelRun
                    | TaskAction::RetryRun
                    | TaskAction::EditPending
                    | TaskAction::CancelPending
            )
            && (self.review_painted.is_none() || self.review_painted != self.review_observation())
    }
    fn apply_review_pending(&mut self, action: TaskAction, id: &str) {
        if action == TaskAction::EditPending
            && self.pending_task_editing
            && self
                .pending_task_detail
                .as_ref()
                .is_some_and(|detail| detail.task_id == id)
        {
            self.task_instruction_detail = Some(id.into());
            self.keyboard_focus = Some(ActivityFocus::PendingTaskEditor);
            return;
        }
        let Some((snapshot, control)) = self.reviewed_control(id) else {
            return;
        };
        let Some(task) = snapshot.tasks.iter().find(|task| task.id == id) else {
            return;
        };
        if !snapshot.writable
            || control.active_run.is_some()
            || !matches!(
                task.status,
                tilo_desktop_model::WorkspaceTaskStatus::Queued
                    | tilo_desktop_model::WorkspaceTaskStatus::Waiting
            )
        {
            return;
        }
        let frontier = tilo_desktop_model::WorkspaceTaskPendingFrontier {
            status: task.status,
            run_epoch: control.run_epoch,
            revision: control.pending_revision,
        };
        if action == TaskAction::CancelPending {
            self.submit_review(TaskReviewCommand::CancelPending {
                task_id: id.into(),
                frontier,
            });
            return;
        }
        let Some(instruction) = control
            .instruction
            .clone()
            .or_else(|| super::task_instruction(task).map(str::to_owned))
        else {
            return;
        };
        self.pending_task_detail = Some(crate::activity::PendingTaskDetail {
            generation: snapshot.generation,
            task_id: id.into(),
            title: task.title.clone(),
            instruction: instruction.clone(),
            frontier,
            queue_ordinal: control.queue_ordinal,
        });
        self.pending_task_editor.widget_mut().set_text(&instruction);
        self.pending_task_editing = true;
        self.task_instruction_detail = Some(id.into());
        self.keyboard_focus = Some(ActivityFocus::PendingTaskEditor);
    }
    fn submit_review(&mut self, command: TaskReviewCommand) {
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

impl RoomActivityPane {
    fn draw_review_action_grid(
        &mut self,
        paint: &mut PaintCtx<'_>,
        bounds: Rect,
        mut y: f64,
        actions: &[TaskAction],
        presence: &PresenceCtx,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        self.review_buttons
            .retain(|button| actions.contains(&button.action));
        let width = (bounds.width() - 2.0 * gap)
            .min(22.0 * f64::from(paint.theme.text(TextRole::Label).line_height_px()));
        for action in actions {
            if !self
                .review_buttons
                .iter()
                .any(|button| button.action == *action)
            {
                let pressed = std::rc::Rc::new(std::cell::Cell::new(false));
                let signal = pressed.clone();
                self.review_buttons.push(ReviewButton {
                    action: *action,
                    button: ui::widget::Embedded::new(
                        ui::widget::Button::new("task-review-action", &action.label())
                            .on_click(move || signal.set(true)),
                    ),
                    pressed,
                });
            }
            let Some(button) = self
                .review_buttons
                .iter_mut()
                .find(|button| button.action == *action)
            else {
                continue;
            };
            button
                .button
                .widget_mut()
                .set_focus(self.keyboard_focus == Some(ActivityFocus::TaskAction(*action)));
            let height = button.button.layout(
                Rect::new(bounds.x0 + gap, y + gap, bounds.x0 + gap + width, y + gap),
                paint.theme,
                paint.text,
            );
            button.button.draw(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                presence,
            );
            self.task_action_rects
                .push((*action, button.button.widget().get_rect()));
            y += height + gap;
        }
        y + gap
    }
    pub(super) fn draw_review_tasks(
        &mut self,
        paint: &mut PaintCtx<'_>,
        bounds: Rect,
        presence: &PresenceCtx,
    ) {
        self.review_approval = None;
        let mut content = vello::Scene::new();
        // Reserve the shared scrollbar lane before measuring, so overflow on
        // the first frame cannot clip controls laid out at the wider width.
        let content_bounds = Rect::new(
            bounds.x0,
            bounds.y0,
            (bounds.x1 - f64::from(paint.theme.tokens.metrics.scrollbar_gutter)).max(bounds.x0),
            bounds.y1,
        );
        let height = self.draw_review_content(
            &mut PaintCtx::new(
                &mut content,
                vello::kurbo::Affine::IDENTITY,
                paint.text,
                paint.theme,
            ),
            content_bounds,
            presence,
        ) - bounds.y0;
        let observation = self.review_observation();
        if self.review_painted != observation {
            for button in &mut self.review_buttons {
                button.button.widget_mut().cancel_pointer_interaction();
                button.pressed.set(false);
            }
        }
        self.review_painted = observation;
        self.task_list_scroll.set_viewport(bounds, paint.theme);
        self.task_list_scroll
            .set_content_size(content_bounds.width(), height.max(bounds.height()));
        self.task_list_theme = Some(paint.theme.clone());
        if self.keyboard_focus != self.review_scroll_focus {
            let focused = match self.keyboard_focus.as_ref() {
                Some(ActivityFocus::TaskAction(action)) => self
                    .task_action_rects
                    .iter()
                    .find(|(value, _)| value == action)
                    .map(|(_, rect)| *rect),
                Some(ActivityFocus::TaskInstruction(id))
                    if self.task_instruction_target.as_ref() == Some(id) =>
                {
                    Some(self.task_instruction_open_button.rect())
                }
                _ => None,
            };
            if let Some(rect) = focused {
                let rect = rect.with_origin((rect.x0 - bounds.x0, rect.y0 - bounds.y0));
                let gap = f64::from(paint.theme.tokens.spacing.s3);
                self.task_list_scroll.scroll_into_view(Rect::new(
                    rect.x0,
                    rect.y0 - gap,
                    rect.x1,
                    rect.y1 + gap,
                ));
            }
            self.review_scroll_focus = self.keyboard_focus.clone();
        }
        let shift = vello::kurbo::Vec2::new(0.0, -self.task_list_scroll.offset().y);
        for (_, rect) in &mut self.task_rects {
            *rect = vello::kurbo::Affine::translate(shift)
                .transform_rect_bbox(*rect)
                .intersect(bounds);
        }
        for (_, rect) in &mut self.task_action_rects {
            *rect = vello::kurbo::Affine::translate(shift)
                .transform_rect_bbox(*rect)
                .intersect(bounds);
        }
        for button in &mut self.review_buttons {
            let rect =
                vello::kurbo::Affine::translate(shift).transform_rect_bbox(button.button.rect());
            button.button.layout(rect, paint.theme, paint.text);
        }
        if self.task_instruction_target.is_some() {
            let rect = vello::kurbo::Affine::translate(shift)
                .transform_rect_bbox(self.task_instruction_open_button.rect());
            self.task_instruction_open_button
                .layout(rect, paint.theme, paint.text);
        }
        self.task_list_scroll
            .paint(paint.scene, paint.transform, paint.theme, |scene, _| {
                scene.append(&content, Some(paint.transform.pre_translate(shift)));
            });
    }
    fn draw_review_content(
        &mut self,
        paint: &mut PaintCtx<'_>,
        bounds: Rect,
        presence: &PresenceCtx,
    ) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let style = paint.theme.text(TextRole::Body);
        let width = text_width(bounds.width() - 2.0 * gap);
        let mut y = self.draw_review_heading(paint, bounds);
        let Some(Ok(snapshot)) = &self.task_review else {
            return y;
        };
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
            return y;
        }
        let selected = self
            .selected_task
            .as_ref()
            .and_then(|id| self.tasks.iter().position(|task| &task.id == id));
        self.review_task_offset = selected
            .unwrap_or(self.review_task_offset)
            .min(self.tasks.len().saturating_sub(1));
        let Some(task) = self.tasks.get(self.review_task_offset) else {
            draw_review_line(
                paint,
                &t("activity-tasks-empty"),
                &style,
                width,
                bounds.x0 + gap,
                &mut y,
            );
            return y;
        };
        self.selected_task = Some(task.id.clone());
        let top = y;
        draw_review_line(paint, &task.title, &style, width, bounds.x0 + gap, &mut y);
        let summary = review_task_summary(task);
        draw_review_line(paint, &summary, &style, width, bounds.x0 + gap, &mut y);
        if let Some(control) = snapshot.controls.get(&task.id) {
            draw_control_evidence(
                paint,
                control,
                task.assigned_agent.as_deref(),
                width,
                bounds.x0 + gap,
                &mut y,
                self.now(),
            );
        }
        let instruction = snapshot
            .controls
            .get(&task.id)
            .and_then(|control| control.instruction.as_deref())
            .or_else(|| super::task_instruction(task));
        if let Some(instruction) = instruction {
            draw_instruction_preview(paint, instruction, bounds, &mut y);
            self.task_instruction_target = Some(task.id.clone());
            draw_instruction_control(
                paint,
                bounds,
                &mut y,
                &mut self.task_instruction_open_button,
                presence,
            );
        }
        self.task_rects
            .push((task.id.clone(), Rect::new(bounds.x0, top, bounds.x1, y)));
        let mut actions = self.task_actions_for(task);
        if self.review_task_offset > 0 {
            actions.insert(0, TaskAction::PreviousTasks);
        }
        if self.review_task_offset.saturating_add(1) < self.tasks.len() {
            actions.insert(0, TaskAction::NextTasks);
        }
        let approval = approval_frontier(snapshot, task);
        let bottom = self.draw_review_action_grid(paint, bounds, y, &actions, presence);
        if self
            .task_action_rects
            .iter()
            .any(|(action, _)| *action == TaskAction::ApproveRun)
        {
            self.review_approval = approval;
        }
        bottom
    }
    fn draw_review_heading(&self, paint: &mut PaintCtx<'_>, bounds: Rect) -> f64 {
        let gap = f64::from(paint.theme.tokens.spacing.s3);
        let style = paint.theme.text(TextRole::Body);
        let width = text_width(bounds.width() - 2.0 * gap);
        let mut y = bounds.y0 + gap;
        if let Some(Ok(snapshot)) = &self.task_review {
            let source = review_source(snapshot, self.now());
            for line in paint
                .text
                .clamp_to_lines(&source, &style, width, usize::MAX)
            {
                draw_review_line(paint, &line, &style, width, bounds.x0 + gap, &mut y);
            }
            if let Some(notice) = &self.review_notice {
                draw_review_line(paint, notice, &style, width, bounds.x0 + gap, &mut y);
            }
        }
        y
    }
}
fn approval_frontier(
    snapshot: &TaskReviewSnapshot,
    task: &tilo_desktop_model::WorkspaceTask,
) -> Option<ApprovalFrontier> {
    let control = snapshot.controls.get(&task.id)?;
    if !native_run_approval_available(task, control) {
        return None;
    }
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
}
pub(super) fn native_run_approval_available(
    task: &tilo_desktop_model::WorkspaceTask,
    control: &tilo_desktop_model::WorkspaceTaskControl,
) -> bool {
    task.status == tilo_desktop_model::WorkspaceTaskStatus::Queued
        && control.active_run.is_none()
        && !control.instruction_required
        && control.instruction.is_none()
        && task.assigned_agent.is_some()
        && task.route != tilo_desktop_model::WorkspaceTaskRoute::Manual
        && task
            .resource
            .as_ref()
            .is_none_or(|resource| resource.agent_request.is_none())
}
fn review_task_summary(task: &tilo_desktop_model::WorkspaceTask) -> String {
    t_args(
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
    )
}
fn draw_instruction_preview(
    paint: &mut PaintCtx<'_>,
    instruction: &str,
    bounds: Rect,
    y: &mut f64,
) {
    let gap = f64::from(paint.theme.tokens.spacing.s3);
    let style = paint.theme.text(TextRole::Body);
    let width = text_width(bounds.width() - 2.0 * gap);
    draw_review_line(
        paint,
        &t("activity-task-full-instruction"),
        &style,
        width,
        bounds.x0 + gap,
        y,
    );
    for line in paint.text.clamp_to_lines(instruction, &style, width, 3) {
        draw_review_line(paint, &line, &style, width, bounds.x0 + gap, y);
    }
}
fn draw_instruction_control(
    paint: &mut PaintCtx<'_>,
    bounds: Rect,
    y: &mut f64,
    button: &mut ui::widget::Embedded<ui::widget::Button>,
    presence: &PresenceCtx,
) {
    let gap = f64::from(paint.theme.tokens.spacing.s3);
    let width = 22.0 * f64::from(paint.theme.text(TextRole::Label).line_height_px());
    let height = button.layout(
        Rect::new(
            bounds.x0 + gap,
            *y,
            (bounds.x0 + gap + width).min(bounds.x1 - gap),
            *y,
        ),
        paint.theme,
        paint.text,
    );
    button.draw(
        paint.scene,
        paint.transform,
        paint.text,
        paint.theme,
        presence,
    );
    *y += height + gap;
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
    assigned_agent: Option<&str>,
    width: f32,
    x: f64,
    y: &mut f64,
    now: SystemTime,
) {
    let style = paint.theme.text(TextRole::Body);
    if let Some(status) = direction_application_status(control, assigned_agent) {
        draw_review_line(paint, &t(status), &style, width, x, y);
    }
    if control.cancellation.as_ref().is_some_and(|request| {
        control
            .active_run
            .as_ref()
            .is_some_and(|run| run.epoch == request.run_epoch)
    }) {
        draw_review_line(
            paint,
            &t("activity-task-review-stopping"),
            &style,
            width,
            x,
            y,
        );
    }
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

pub(super) fn direction_application_status(
    control: &tilo_desktop_model::WorkspaceTaskControl,
    assigned_agent: Option<&str>,
) -> Option<&'static str> {
    let direction = control.directions.last()?;
    let run = control.active_run.as_ref()?;
    if run.epoch != control.run_epoch || assigned_agent != Some(run.agent_id.as_str()) {
        return None;
    }
    Some(if run.applied_direction_revision >= direction.revision {
        "activity-task-review-direction-applied"
    } else {
        "activity-task-review-direction-waiting"
    })
}

fn review_source(snapshot: &TaskReviewSnapshot, now: SystemTime) -> String {
    t_args(
        "activity-task-review-source",
        &[(
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
        )],
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
