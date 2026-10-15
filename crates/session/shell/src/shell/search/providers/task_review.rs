//! Owner-observed task review. Run/frontier are bound at discovery, never inferred
//! again after the person has reviewed them. Commits return durable owner receipts.
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{
    ActionReceipt, InvocationDisposition, ResourceKind, SearchBudget, SearchContext, SearchCursor,
    SearchPage, SearchProvider, SearchResult,
};
use crate::shell::task_review::NativeTaskReview;
use std::rc::Rc;
use tilo_desktop::activity::{TaskReviewCommand, TaskReviewRequest, TaskReviewSnapshot};

pub(crate) struct TaskReviewProvider {
    review: NativeTaskReview,
}
impl TaskReviewProvider {
    pub(crate) fn new(review: NativeTaskReview) -> Self {
        Self { review }
    }
    fn snapshot(&self, ctx: &SearchContext<'_>) -> Option<std::sync::Arc<TaskReviewSnapshot>> {
        let room = ctx.chrome.tree().active_workspace()?.room.to_string();
        self.review
            .snapshot(&room)?
            .ok()
            .filter(|snapshot| snapshot.room == room)
    }
    fn rows(&self, snapshot: &TaskReviewSnapshot) -> Vec<SearchResult> {
        snapshot
            .tasks
            .iter()
            .flat_map(|task| self.task_rows(snapshot, task))
            .collect()
    }
    fn task_rows(
        &self,
        snapshot: &TaskReviewSnapshot,
        task: &crate::shell::state::WorkspaceTask,
    ) -> Vec<SearchResult> {
        let mut rows = vec![self.inspect(snapshot, task)];
        if snapshot.writable
            && let Some(control) = snapshot.controls.get(&task.id)
        {
            let revision = control
                .directions
                .last()
                .map_or(0, |direction| direction.revision);
            if task.status == crate::shell::state::WorkspaceTaskStatus::Queued
                && control.active_run.is_none()
                && task.route != crate::shell::state::WorkspaceTaskRoute::Manual
                && task
                    .resource
                    .as_ref()
                    .is_none_or(|resource| resource.agent_request.is_none())
                && let Some(agent_id) = &task.assigned_agent
            {
                rows.push(self.edit(
                    snapshot,
                    &task.id,
                    &task.title,
                    TaskReviewCommand::Approve {
                        task_id: task.id.clone(),
                        agent_id: agent_id.clone(),
                        run_epoch: control.run_epoch,
                        after_revision: revision,
                    },
                ));
            }
            if !task.status.is_finished() && control.active_run.is_some() {
                rows.push(self.edit(
                    snapshot,
                    &task.id,
                    &task.title,
                    TaskReviewCommand::Steer {
                        task_id: task.id.clone(),
                        run_epoch: control.run_epoch,
                        after_revision: revision,
                        text: String::new(),
                    },
                ));
            }
            if !task.status.is_finished() {
                rows.push(self.edit(
                    snapshot,
                    &task.id,
                    &task.title,
                    TaskReviewCommand::Cancel {
                        task_id: task.id.clone(),
                        run_epoch: control.run_epoch,
                        after_revision: revision,
                    },
                ));
            }
            if task.status == crate::shell::state::WorkspaceTaskStatus::Waiting
                && control.active_run.is_none()
            {
                rows.push(self.edit(
                    snapshot,
                    &task.id,
                    &task.title,
                    TaskReviewCommand::Retry {
                        task_id: task.id.clone(),
                        run_epoch: control.run_epoch,
                    },
                ));
            }
        }
        rows
    }
    fn status(&self, room: String) -> SearchResult {
        let review = self.review.clone();
        SearchResult::view_with_output(
            ResourceKind::Action,
            self.id(),
            format!("{room}/status"),
            t("task-review-status"),
            Some(room.clone()),
            80,
            move |chrome| {
                check_room(chrome, &room)?;
                match review.snapshot(&room) {
                    None => Err(ActionError::Refused(
                        "authenticated task evidence is still loading".into(),
                    )),
                    Some(Err(error)) => Err(ActionError::Refused(error)),
                    Some(Ok(snapshot)) => Ok(
                        serde_json::json!({"room":snapshot.room,"generation":snapshot.generation,"sampled_at":snapshot.sampled_at_secs,"tasks":snapshot.tasks.len()}),
                    ),
                }
            },
        )
    }
    fn inspect(
        &self,
        snapshot: &TaskReviewSnapshot,
        task: &crate::shell::state::WorkspaceTask,
    ) -> SearchResult {
        let room = snapshot.room.clone();
        let actor = snapshot.actor.clone();
        let task_id = task.id.clone();
        let review = self.review.clone();
        let mut row = SearchResult::nullary_fallible(
            ResourceKind::Action,
            self.id(),
            format!("{room}/task/{task_id}/inspect"),
            format!("{}: {}", t("task-review-inspect"), task.title),
            Some(room.clone()),
            80,
            |_, _, _| {
                Err(ActionError::Refused(
                    "task inspection requires current owner admission".into(),
                ))
            },
        );
        row.operation_kind = tilo_actions::OperationKind::View;
        row.on_begin_invoke = Some(Rc::new(move |_, _, chrome, _| {
            check_room(chrome, &room)?;
            let invocation = uuid::Uuid::new_v4();
            review
                .submit(TaskReviewRequest {
                    room: room.clone(),
                    observed_actor: actor.clone(),
                    invocation,
                    command: TaskReviewCommand::Inspect {
                        task_id: task_id.clone(),
                    },
                })
                .map_err(ActionError::Refused)?;
            let review = review.clone();
            let room = room.clone();
            let task_id = task_id.clone();
            let chrome = Rc::clone(chrome);
            Ok(InvocationDisposition::Pending(Rc::new(move || {
                check_room(&chrome, &room)?;
                match review.poll(invocation) {
                    None => return Ok(None),
                    Some(Err(error)) => return Err(ActionError::Refused(error)),
                    Some(Ok(())) => {}
                }
                let snapshot = review
                    .snapshot(&room)
                    .ok_or_else(|| ActionError::Refused("owner evidence unavailable".into()))?
                    .map_err(ActionError::Refused)?;
                let task = snapshot
                    .tasks
                    .iter()
                    .find(|task| task.id == task_id)
                    .ok_or_else(|| ActionError::Refused("task no longer exists".into()))?;
                let evidence = serde_json::json!({"room":room,"generation":snapshot.generation,"sampled_at":snapshot.sampled_at_secs,"task":task,"control":snapshot.controls.get(&task_id)});
                chrome.borrow_mut().open_room_activity(Some(
                    crate::shell::chrome_state::ActivityFocus {
                        task_id: Some(task_id.clone()),
                        ..Default::default()
                    },
                ));
                Ok(Some(ActionReceipt::completed_with(evidence)))
            })))
        }));
        row
    }

    fn edit(
        &self,
        snapshot: &TaskReviewSnapshot,
        id: &str,
        title: &str,
        command: TaskReviewCommand,
    ) -> SearchResult {
        let (verb, epoch, revision) = match &command {
            TaskReviewCommand::Approve {
                run_epoch,
                after_revision,
                ..
            } => ("approve", *run_epoch, *after_revision),
            TaskReviewCommand::Steer {
                run_epoch,
                after_revision,
                ..
            } => ("steer", *run_epoch, *after_revision),
            TaskReviewCommand::Cancel {
                run_epoch,
                after_revision,
                ..
            } => ("cancel", *run_epoch, *after_revision),
            TaskReviewCommand::Retry { run_epoch, .. } => ("retry", *run_epoch, 0),
            // A paused action is answered on its card, where its draft is
            // shown; search offers no answer to a draft it cannot show.
            TaskReviewCommand::Inspect { .. }
            | TaskReviewCommand::AnswerConfirmation { .. }
            | TaskReviewCommand::AlwaysAllow { .. } => return self.status(snapshot.room.clone()),
        };
        let room = snapshot.room.clone();
        let actor = snapshot.actor.clone();
        let review = self.review.clone();
        let mut row = SearchResult::nullary_fallible(
            ResourceKind::Action,
            self.id(),
            format!("{room}/task/{id}/{verb}/{epoch}/{revision}"),
            format!(
                "{}: {title}",
                t(match verb {
                    "approve" => "task-review-approve",
                    "steer" => "task-review-steer",
                    "cancel" => "task-review-cancel",
                    _ => "task-review-retry",
                })
            ),
            Some(format!("{} · {}", snapshot.actor, snapshot.sampled_at_secs)),
            80,
            |_, _, _| {
                Err(ActionError::Refused(
                    "task review requires retained asynchronous admission".into(),
                ))
            },
        );
        row.operation_kind = tilo_actions::OperationKind::Document;
        if verb == "steer" {
            row.params = vec![ActionParam::required(
                "text",
                ParamType::Text,
                "Public instructions for this observed task run.",
            )];
        }
        row.on_begin_invoke = Some(Rc::new(move |_, _, chrome, args| {
            check_room(chrome, &room)?;
            let mut command = command.clone();
            if let TaskReviewCommand::Steer { text, .. } = &mut command {
                args.text("text")
                    .ok_or_else(|| ActionError::Refused("task direction requires text".into()))?
                    .clone_into(text);
            }
            let invocation = uuid::Uuid::new_v4();
            review
                .submit(TaskReviewRequest {
                    room: room.clone(),
                    observed_actor: actor.clone(),
                    invocation,
                    command,
                })
                .map_err(ActionError::Refused)?;
            let review = review.clone();
            Ok(InvocationDisposition::Pending(Rc::new(
                move || match review.poll(invocation) {
                    None => Ok(None),
                    Some(Ok(())) => Ok(Some(ActionReceipt::completed())),
                    Some(Err(error)) => Err(ActionError::Refused(error)),
                },
            )))
        }));
        row
    }
}
fn check_room(chrome: &crate::shell::chrome_state::Chrome, room: &str) -> Result<(), ActionError> {
    if chrome
        .borrow()
        .tree()
        .active_workspace()
        .is_some_and(|workspace| workspace.room.to_string() == room)
    {
        Ok(())
    } else {
        Err(ActionError::Refused(
            "task belongs to a different workspace".into(),
        ))
    }
}
impl SearchProvider for TaskReviewProvider {
    fn id(&self) -> &'static str {
        "task-review"
    }
    fn name(&self) -> &'static str {
        "Task review"
    }
    fn display_name(&self) -> String {
        t("task-review-provider")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let room = ctx
            .chrome
            .tree()
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        let Some(room) = room else { return Vec::new() };
        let snapshot = self.review.snapshot(&room);
        let mut rows = match snapshot {
            Some(Ok(snapshot)) if snapshot.room == room => self.rows(&snapshot),
            _ => vec![self.status(room)],
        };
        rows.retain(|row| {
            row.title
                .to_lowercase()
                .contains(&ctx.needle.to_lowercase())
        });
        rows
    }
    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        self.snapshot(ctx).map_or_else(
            || "loading".into(),
            |snapshot| {
                format!(
                    "{}:{}:{}",
                    snapshot.room, snapshot.generation, snapshot.writable
                )
            },
        )
    }
    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let Some(snapshot) = self.snapshot(&ctx) else {
            return SearchPage::exhausted(self.search_revision(&ctx), self.query(ctx));
        };
        let revision = format!(
            "{}:{}:{}",
            snapshot.room, snapshot.generation, snapshot.writable
        );
        if cursor.is_some_and(|cursor| cursor.revision != revision) {
            return SearchPage::stale(revision);
        }
        let (mut task_index, mut row_index) = if let Some(cursor) = cursor {
            let Some(position) = cursor.token.split_once(':').and_then(|(task, row)| {
                Some((task.parse::<usize>().ok()?, row.parse::<usize>().ok()?))
            }) else {
                return SearchPage::stale(revision);
            };
            position
        } else {
            (0, 0)
        };
        let needle = ctx.needle.to_lowercase();
        let mut results = Vec::new();
        let mut bytes = 0usize;
        let mut work = 0usize;
        'tasks: while let Some(task) = snapshot.tasks.get(task_index) {
            let rows = self.task_rows(&snapshot, task);
            while let Some(row) = rows.get(row_index) {
                if work >= budget.max_work || results.len() >= budget.max_results {
                    break 'tasks;
                }
                work = work.saturating_add(1);
                if row.title.to_lowercase().contains(&needle) {
                    let row_bytes = row
                        .title
                        .len()
                        .saturating_add(row.subtitle.as_ref().map_or(0, String::len))
                        .saturating_add(row.id.len());
                    if bytes.saturating_add(row_bytes) > budget.max_bytes {
                        break 'tasks;
                    }
                    bytes = bytes.saturating_add(row_bytes);
                    results.push(row.clone());
                }
                row_index = row_index.saturating_add(1);
            }
            task_index = task_index.saturating_add(1);
            row_index = 0;
        }
        let exhausted = task_index >= snapshot.tasks.len();
        SearchPage::continuing(
            revision,
            results,
            (!exhausted).then(|| format!("{task_index}:{row_index}")),
        )
    }
}
