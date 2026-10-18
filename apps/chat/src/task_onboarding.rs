//! Task goals exist before compute setup and never enter the shared Chat record.
use crate::app::{ChatApp, Focus};
use crate::composer::ConversationKey;
use serde::Deserialize;
use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ControlSize};
use vello::kurbo::{Affine, Rect};

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TaskCompute {
    pub(crate) agent_id: String,
    pub(crate) workspace: String,
    pub(crate) eligible: bool,
    pub(crate) configured: bool,
    pub(crate) registered: bool,
    pub(crate) profile: String,
}

pub(crate) fn eligible_compute<'a>(
    workers: &'a [TaskCompute],
    workspace: &str,
) -> Option<&'a TaskCompute> {
    let mut eligible = workers.iter().filter(|worker| {
        worker.workspace == workspace
            && worker.eligible
            && worker.configured
            && worker.registered
            && !worker.agent_id.is_empty()
            && !worker.profile.trim().is_empty()
    });
    let worker = eligible.next()?;
    // Choosing among different configurations requires an explicit selection.
    eligible.next().is_none().then_some(worker)
}

#[derive(Clone, Debug)]
pub(crate) struct TaskGoalDraft {
    pub(crate) workspace: String,
    pub(crate) parent: Option<String>,
    pub(crate) saving: bool,
    pub(crate) error: Option<String>,
}

impl TaskGoalDraft {
    pub(crate) fn key(&self) -> ConversationKey {
        ConversationKey::TaskDraft {
            workspace: self.workspace.clone(),
            parent: self.parent.clone(),
        }
    }

    fn caption(&self, compute: Option<&TaskCompute>) -> String {
        if self.saving {
            crate::i18n::t("chat-task-saving-goal")
        } else if let Some(error) = &self.error {
            error.clone()
        } else if let Some(compute) = compute {
            crate::i18n::t_args(
                "chat-task-compute-profile",
                &[("profile", compute.profile.clone().into())],
            )
        } else {
            // This is still an editor draft, not an acknowledged saved goal.
            // Compute discovery can also be pending or unavailable here.
            crate::i18n::t("chat-agent-empty-queue-message")
        }
    }
}

pub(crate) struct SavedGoal {
    pub(crate) workspace: String,
    pub(crate) parent: Option<String>,
    pub(crate) task: String,
    pub(crate) agent: String,
    pub(crate) revision: u64,
}

impl ChatApp {
    pub(crate) fn begin_task_goal(&mut self, parent: Option<String>) {
        self.leave_floated_agent_route();
        self.close_person_conversation();
        self.direct_agent = None;
        self.task_goal = Some(TaskGoalDraft {
            workspace: self.active_ws.clone(),
            parent,
            saving: false,
            error: None,
        });
        self.inbox_open = false;
        self.thread_open = None;
        self.editing = None;
        self.focus = Focus::Main;
        self.scroll = 0.0;
        self.sync_task_goal();
        self.agent_tasks.observe_tasks(&self.active_ws);
        self.agent_tasks.refresh_compute(&self.active_ws);
        self.request_relayout();
    }

    pub(crate) fn sync_task_goal(&mut self) {
        let Some(goal) = &self.task_goal else {
            return;
        };
        let key = goal.key();
        self.active_channel = key.draft_key();
        self.composer_main.set_conversation_draft(&key);
        self.composer_main
            .set_placeholder(&crate::i18n::t("chat-agent-task-placeholder"));
        self.draft_main = self.composer_main.private_plain().unwrap_or_default();
    }

    pub(crate) fn save_task_goal(&mut self) -> bool {
        let Some(goal) = &self.task_goal else {
            return false;
        };
        if goal.saving || goal.workspace != self.active_ws {
            return false;
        }
        let key = goal.key();
        let parent = goal.parent.clone();
        let text = self.composer_main.private_plain().unwrap_or_default();
        let Some(revision) = self.composer_main.conversation_revision(&key) else {
            return false;
        };
        if text.trim().is_empty() {
            return false;
        }
        // Saving is durable even before a model is configured. Only Start
        // prepares an executor; the editor never invents one.
        let accepted = self.agent_tasks.queue_goal("", text, revision, parent);
        if accepted && let Some(goal) = &mut self.task_goal {
            goal.saving = true;
            goal.error = None;
        }
        self.request_relayout();
        accepted
    }

    fn task_goal_caption(&self, goal: &TaskGoalDraft) -> String {
        if goal.saving
            || goal.error.is_some()
            || self.agent_tasks.eligible_compute(&self.active_ws).is_some()
        {
            goal.caption(self.agent_tasks.eligible_compute(&self.active_ws))
        } else if self.agent_tasks.needs_model_setup() {
            crate::i18n::t("chat-task-model-needed")
        } else if self.agent_tasks.can_prepare_model_start() {
            crate::i18n::t("chat-task-ready-to-start")
        } else if !self.agent_tasks.compute_ready() {
            crate::i18n::t("chat-task-checking-setup")
        } else {
            crate::i18n::t("chat-task-service-unavailable")
        }
    }

    pub(crate) fn task_goal_controls_height(
        &self,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        width: f64,
    ) -> f64 {
        let Some(goal) = &self.task_goal else {
            return 0.0;
        };
        let (_, height) = text.measure_style(
            &self.task_goal_caption(goal),
            Some(width.max(1.0) as f32),
            &theme.text(TextRole::Caption),
        );
        let button = if self.agent_tasks.needs_compute_controls() {
            ButtonFace::height(ControlSize::Regular, theme) + f64::from(theme.tokens.spacing.s2)
        } else {
            0.0
        };
        f64::from(height) + button + f64::from(theme.tokens.spacing.s2)
    }

    pub(crate) fn draw_task_goal_controls(
        &self,
        paint: PaintCtx<'_>,
        rect: Rect,
        hits: &mut Vec<(Rect, crate::app::Hit)>,
    ) {
        let Some(goal) = &self.task_goal else {
            return;
        };
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = paint;
        let row_h = ButtonFace::height(ControlSize::Regular, theme);
        let caption = self.task_goal_caption(goal);
        text.draw_style(
            scene,
            &caption,
            theme.text_dim,
            transform * Affine::translate((rect.x0, rect.y0)),
            Some(rect.width().max(1.0) as f32),
            &theme.text(TextRole::Caption),
        );
        if !self.agent_tasks.needs_compute_controls() {
            return;
        }
        let button = Rect::new(rect.x0, (rect.y1 - row_h).max(rect.y0), rect.x1, rect.y1);
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(button, &self.agent_tasks.setup_label()).hovered(self.hovered(button)),
        );
        ChatApp::push_hit(hits, button, crate::app::Hit::AgentSetup);
    }

    pub(crate) fn settle_task_goal(&mut self) {
        let Some(result) = self.agent_tasks.take_goal_receipt() else {
            return;
        };
        let Some(goal) = &mut self.task_goal else {
            return;
        };
        match result {
            Err(error) => {
                goal.saving = false;
                goal.error = Some(error);
            }
            Ok(saved) if saved.workspace == goal.workspace && saved.parent == goal.parent => {
                let key = goal.key();
                self.composer_main
                    .clear_conversation_revision(&key, saved.revision);
                self.task_goal = None;
                // Only a canonical owner receipt makes a child appear. Queueing
                // never grants authority or starts a model; Start stays explicit.
                let opened = if saved.agent.is_empty() {
                    // An unbound task has a task/run route, not a persona.
                    self.direct_agent = Some(String::new());
                    self.inbox_open = false;
                    self.thread_open = None;
                    self.focus = Focus::Main;
                    self.scroll = 0.0;
                    true
                } else {
                    self.open_agent_conversation(&saved.agent)
                };
                if opened {
                    self.agent_tasks
                        .follow(&saved.workspace, &saved.agent, &saved.task, 0);
                    self.agent_intents.update(|intents| {
                        intents.insert(
                            (saved.workspace.clone(), saved.agent.clone()),
                            crate::agent_tasks::AgentComposerIntent::Steer,
                        );
                    });
                    self.sync_task_channel();
                    self.sync_agent_route();
                }
            }
            Ok(_) => {}
        }
        self.request_relayout();
    }
}

#[cfg(test)]
mod tests {
    use super::TaskGoalDraft;

    #[test]
    fn unsaved_goal_without_compute_describes_saving_not_a_saved_goal() {
        let goal = TaskGoalDraft {
            workspace: "workspace".into(),
            parent: None,
            saving: false,
            error: None,
        };
        assert_eq!(
            goal.caption(None),
            crate::i18n::t("chat-agent-empty-queue-message")
        );
    }

    #[test]
    fn pending_save_and_refused_save_keep_their_distinct_statuses() {
        let mut goal = TaskGoalDraft {
            workspace: "workspace".into(),
            parent: None,
            saving: true,
            error: None,
        };
        assert_eq!(goal.caption(None), crate::i18n::t("chat-task-saving-goal"));
        goal.saving = false;
        goal.error = Some(crate::i18n::t("chat-task-save-refused"));
        assert_eq!(goal.caption(None), crate::i18n::t("chat-task-save-refused"));
    }
}
