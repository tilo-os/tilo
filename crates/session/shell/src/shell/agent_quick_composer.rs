//! Private, marker-adjacent controls. The owner adapter, not marker geometry,
//! resolves and admits the ordinary published Chat and Task actions.

use std::collections::HashMap;

use ui::widget::{
    Actions, Button, ButtonRole, Column, Container, Decorator, Dropdown, Popover, PopoverSide,
    Segment, Text, TextInput, ToggleGroup, When, Widget,
};
use ui::{LocalState, Theme};
use vello::kurbo::Rect;

use super::i18n::{t, t_args};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum ComposerIntent {
    #[default]
    Message,
    Queue,
    Steer,
}

/// Descriptive custody of a selected certified action session. This is a
/// draft partition, never a capability or a substitute for broker admission.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ComposerOwner {
    pub room: String,
    pub agent: String,
    pub person: String,
    pub identity_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerTask {
    pub task_id: String,
    pub run_epoch: u64,
    pub revision: u64,
    pub label: String,
}

impl ComposerTask {
    fn key(&self) -> String {
        format!("{}:{}:{}", self.task_id, self.run_epoch, self.revision)
    }
}

/// A user's intention, not an execution authority. The adapter re-resolves
/// offers and rechecks this exact owner/run/revision before submitting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerRequest {
    pub owner: ComposerOwner,
    pub intent: ComposerIntent,
    pub submission: u64,
    pub text: String,
    pub task: Option<ComposerTask>,
    pub stop: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ComposerOutcome {
    Queued { task_id: String },
    DirectionSaved(ComposerTask),
    DirectionApplied(ComposerTask),
    Stopped(ComposerTask),
    Refused,
    Unknown,
}

/// Only the adapter's owner-correlated observation may produce this receipt.
pub(crate) struct ComposerReceipt {
    pub owner: ComposerOwner,
    pub submission: u64,
    pub outcome: ComposerOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentQuickComposerOrigin {
    pub marker: Rect,
    pub protected: Rect,
    pub context: String,
}

type DraftKey = (ComposerOwner, ComposerIntent);

#[derive(Clone)]
struct Draft {
    text: String,
    revision: u64,
    editor: Option<String>,
}

struct Submission {
    key: DraftKey,
    revision: u64,
    task: Option<ComposerTask>,
    stop: bool,
    applied: bool,
    busy: bool,
}

pub(crate) struct AgentQuickComposer {
    popover: Popover<Container>,
    owner: Option<ComposerOwner>,
    intent: LocalState<ComposerIntent>,
    shown: ComposerIntent,
    draft: LocalState<String>,
    title: LocalState<String>,
    context: LocalState<String>,
    status: LocalState<String>,
    enabled: LocalState<bool>,
    stop_enabled: LocalState<bool>,
    submit: LocalState<bool>,
    stop: LocalState<bool>,
    conversation: LocalState<bool>,
    close: LocalState<bool>,
    task_visible: LocalState<bool>,
    task_options: LocalState<Vec<(String, String)>>,
    selected: LocalState<String>,
    tasks: Vec<ComposerTask>,
    queued_conversations: HashMap<ComposerOwner, String>,
    available: [bool; 3],
    can_stop: bool,
    drafts: HashMap<DraftKey, Draft>,
    submissions: HashMap<u64, Submission>,
    next_submission: u64,
    pending_editor: Option<(u64, String)>,
    focus_reset: bool,
}

impl Default for AgentQuickComposer {
    fn default() -> Self {
        let mut composer = Self {
            popover: Popover::with_content(Container::new()),
            owner: None,
            intent: LocalState::new(ComposerIntent::Message),
            shown: ComposerIntent::Message,
            draft: LocalState::new(String::new()),
            title: LocalState::new(t("agent-quick-accessible-name")),
            context: LocalState::new(String::new()),
            status: LocalState::new(t("agent-quick-checking")),
            enabled: LocalState::new(false),
            stop_enabled: LocalState::new(false),
            submit: LocalState::new(false),
            stop: LocalState::new(false),
            conversation: LocalState::new(false),
            close: LocalState::new(false),
            task_visible: LocalState::new(false),
            task_options: LocalState::new(Vec::new()),
            selected: LocalState::new(String::new()),
            tasks: Vec::new(),
            queued_conversations: HashMap::new(),
            available: [false; 3],
            can_stop: false,
            drafts: HashMap::new(),
            submissions: HashMap::new(),
            next_submission: 0,
            pending_editor: None,
            focus_reset: false,
        };
        composer.popover = composer.controls();
        composer
    }
}

impl AgentQuickComposer {
    fn controls(&self) -> Popover<Container> {
        let submit = self.submit.clone();
        let stop = self.stop.clone();
        let conversation = self.conversation.clone();
        let close = self.close.clone();
        let modes = ToggleGroup::from_state(
            vec![
                (
                    ComposerIntent::Message,
                    Segment::new(t("agent-quick-message")),
                ),
                (ComposerIntent::Queue, Segment::new(t("agent-quick-queue"))),
                (ComposerIntent::Steer, Segment::new(t("agent-quick-steer"))),
            ],
            &self.intent,
        )
        .with_id("agent-quick-intent")
        .with_accessible_name(t("agent-quick-intent"))
        .fill();
        let picker = Dropdown::from_state(Vec::new(), &self.selected)
            .with_id("agent-quick-task")
            .with_resolved_accessible_name(t("agent-quick-task-label"))
            .caption(t("agent-quick-task-label"))
            .fill();
        let actions = Actions::new(
            Button::new("agent-quick-send", &t("agent-quick-send"))
                .with_icon(ui::icons::Icon::Send)
                .role(ButtonRole::Primary)
                .enabled_from(&self.enabled)
                .on_click(move || submit.set(true)),
        )
        .secondary(
            Button::new("agent-quick-stop", &t("agent-quick-stop"))
                .with_icon(ui::icons::Icon::Square)
                .enabled_from(&self.stop_enabled)
                .on_click(move || stop.set(true)),
        )
        .secondary(
            Button::new(
                "agent-quick-open-conversation",
                &t("agent-quick-open-conversation"),
            )
            .with_icon(ui::icons::Icon::MessageSquare)
            .on_click(move || conversation.set(true)),
        )
        .secondary(
            Button::new("agent-quick-close", &t("agent-quick-close"))
                .with_icon(ui::icons::Icon::X)
                .on_click(move || close.set(true)),
        );
        let body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::from_state(&self.title, Clone::clone).with_role(ui::TextRole::Heading),
            ))
            .push(Box::new(
                Container::new().with_no_padding().with_child(Box::new(
                    Text::from_state(&self.context, Clone::clone)
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                        .dimmed(),
                )),
            ))
            .push(Box::new(modes))
            .push(Box::new(When::from_state(
                &self.task_visible,
                |visible| *visible,
                Box::new(picker),
            )))
            .push(Box::new(
                TextInput::from_state(&self.draft)
                    .with_id("agent-quick-editor")
                    .with_placeholder(t("agent-quick-placeholder"))
                    .with_accessible_name(t("agent-quick-editor-name"))
                    .multiline(true)
                    .with_min_lines(3),
            ))
            .push(Box::new(
                Container::new().with_no_padding().with_child(Box::new(
                    Text::from_state(&self.status, Clone::clone)
                        .with_id("agent-quick-status")
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                        .dimmed(),
                )),
            ))
            .push(Box::new(actions));
        Popover::with_content(
            Container::new()
                .with_no_padding()
                .with_child(Box::new(body)),
        )
        .side(PopoverSide::Right)
        .fallback_side(PopoverSide::Below)
        .confined_to_viewport()
        .with_label(t("agent-quick-accessible-name"))
    }

    pub(crate) fn open(
        &mut self,
        owner: ComposerOwner,
        label: &str,
        origin: AgentQuickComposerOrigin,
        viewport: Rect,
    ) {
        self.save_draft();
        self.owner = Some(owner);
        self.title
            .set(t_args("agent-quick-title", &[("agent", label.into())]));
        self.context.set(origin.context);
        self.available = [false; 3];
        self.tasks.clear();
        self.selected.set(String::new());
        self.status.set(t("agent-quick-checking"));
        self.load_draft();
        self.popover.set_viewport(viewport);
        self.popover.set_anchor(origin.marker);
        self.popover.set_avoid(Some(origin.protected));
        self.popover.set_open(true);
        self.refresh_enabled();
        self.focus_reset = true;
    }

    pub(crate) fn owner(&self) -> Option<&ComposerOwner> {
        self.owner.as_ref()
    }
    pub(crate) fn is_open(&self) -> bool {
        self.popover.is_open()
    }

    pub(crate) fn dismiss(&mut self) {
        self.save_draft();
        self.popover.close();
        self.focus_reset = true;
    }

    /// Replace current offers atomically. Selection never silently retargets
    /// a replaced run or revision; the person chooses the new exact option.
    pub(crate) fn project(
        &mut self,
        owner: &ComposerOwner,
        available: [bool; 3],
        tasks: Vec<ComposerTask>,
        can_stop: bool,
    ) {
        if self.owner.as_ref() != Some(owner) {
            return;
        }
        let selected = self.selected.get();
        if !tasks.iter().any(|task| task.key() == selected) {
            self.selected.set(String::new());
        }
        self.task_options.set(
            tasks
                .iter()
                .map(|task| (task.key(), task.label.clone()))
                .collect(),
        );
        self.tasks = tasks;
        self.available = available;
        self.can_stop = can_stop;
        self.refresh_enabled();
    }

    fn chosen_task(&self) -> Option<ComposerTask> {
        self.tasks
            .iter()
            .find(|task| task.key() == self.selected.get())
            .cloned()
    }

    pub(crate) fn selected_task(&self) -> Option<ComposerTask> {
        self.chosen_task()
    }

    pub(crate) fn take_request(&mut self) -> Option<ComposerRequest> {
        self.tick();
        let stop = self.stop.get();
        let submit = self.submit.get();
        self.stop.set(false);
        self.submit.set(false);
        if !self.is_open()
            || (!stop && !submit)
            || (stop && !self.stop_enabled.get())
            || (!stop && !self.enabled.get())
        {
            return None;
        }
        let owner = self.owner.clone()?;
        let task = self.chosen_task();
        if (stop || self.shown != ComposerIntent::Queue) && task.is_none() {
            return None;
        }
        self.next_submission = self.next_submission.checked_add(1)?;
        self.save_draft();
        self.submissions.insert(
            self.next_submission,
            Submission {
                key: (owner.clone(), self.shown),
                revision: self.draft.revision(),
                task: task.clone(),
                stop,
                applied: false,
                busy: true,
            },
        );
        self.enabled.set(false);
        self.stop_enabled.set(false);
        self.status.set(t(if stop {
            "agent-quick-stop-submitting"
        } else {
            match self.shown {
                ComposerIntent::Message => "agent-quick-message-submitting",
                ComposerIntent::Queue => "agent-quick-queue-submitting",
                ComposerIntent::Steer => "agent-quick-guide-submitting",
            }
        }));
        Some(ComposerRequest {
            owner,
            intent: self.shown,
            submission: self.next_submission,
            text: self.draft.get(),
            task,
            stop,
        })
    }

    pub(crate) fn conversation_task(&self) -> Option<(String, u64)> {
        self.chosen_task()
            .map(|task| (task.task_id, task.run_epoch))
            .or_else(|| {
                self.owner
                    .as_ref()
                    .and_then(|owner| self.queued_conversations.get(owner))
                    .map(|task| (task.clone(), 0))
            })
    }

    pub(crate) fn take_conversation_request(&self) -> bool {
        let requested = self.conversation.get();
        self.conversation.set(false);
        requested && self.is_open()
    }

    pub(crate) fn observe_receipt(&mut self, receipt: ComposerReceipt) {
        let Some(pending) = self.submissions.get_mut(&receipt.submission) else {
            return;
        };
        if pending.key.0 != receipt.owner {
            return;
        }
        let success = match &receipt.outcome {
            ComposerOutcome::Queued { task_id } => {
                pending.key.1 == ComposerIntent::Queue && !pending.stop && !task_id.is_empty()
            }
            ComposerOutcome::DirectionSaved(task) | ComposerOutcome::DirectionApplied(task) => {
                pending.key.1 != ComposerIntent::Queue
                    && !pending.stop
                    && matches_next_revision(pending.task.as_ref(), task)
            }
            ComposerOutcome::Stopped(task) => {
                pending.stop
                    && pending.task.as_ref().is_some_and(|before| {
                        before.task_id == task.task_id
                            && before.run_epoch == task.run_epoch
                            && before.revision == task.revision
                    })
            }
            ComposerOutcome::Refused | ComposerOutcome::Unknown => false,
        };
        if !success
            && !matches!(
                receipt.outcome,
                ComposerOutcome::Refused | ComposerOutcome::Unknown
            )
        {
            return;
        }
        if matches!(receipt.outcome, ComposerOutcome::DirectionSaved(_)) && pending.applied {
            return;
        }
        pending.applied |= matches!(receipt.outcome, ComposerOutcome::DirectionApplied(_));
        if success || matches!(receipt.outcome, ComposerOutcome::Refused) {
            pending.busy = false;
        }
        let key = pending.key.clone();
        let revision = pending.revision;
        let stop = pending.stop;
        let current = self.owner.as_ref() == Some(&key.0) && self.shown == key.1;
        if success && !stop {
            if current && self.draft.revision() == revision {
                self.draft.set(String::new());
            }
            if let Some(draft) = self.drafts.get_mut(&key)
                && draft.revision == revision
            {
                draft.text.clear();
                draft.editor = None;
                draft.revision = draft.revision.saturating_add(1);
            }
        }
        if success && let ComposerOutcome::Queued { task_id } = &receipt.outcome {
            self.queued_conversations
                .insert(receipt.owner.clone(), task_id.clone());
            if self.owner.as_ref() == Some(&receipt.owner) {
                self.selected.set(String::new());
            }
        }
        if current {
            let status = match receipt.outcome {
                ComposerOutcome::Queued { task_id } if success => {
                    t_args("agent-quick-queue-accepted", &[("task", task_id.into())])
                }
                ComposerOutcome::DirectionSaved(task) if success => {
                    task_receipt("agent-quick-guide-saved", &task)
                }
                ComposerOutcome::DirectionApplied(task) if success => {
                    task_receipt("agent-quick-guide-applied", &task)
                }
                ComposerOutcome::Stopped(task) if success => {
                    task_receipt("agent-quick-stopped", &task)
                }
                ComposerOutcome::Unknown => t(if stop {
                    "agent-quick-stop-outcome-unknown"
                } else {
                    match key.1 {
                        ComposerIntent::Message => "agent-quick-message-outcome-unknown",
                        ComposerIntent::Queue => "agent-quick-queue-outcome-unknown",
                        ComposerIntent::Steer => "agent-quick-guide-outcome-unknown",
                    }
                }),
                _ => t("agent-quick-work-refused"),
            };
            self.status.set(status);
        }
    }

    fn refresh_enabled(&self) {
        let index = match self.shown {
            ComposerIntent::Message => 0,
            ComposerIntent::Queue => 1,
            ComposerIntent::Steer => 2,
        };
        let busy = self
            .submissions
            .values()
            .any(|submission| submission.busy && self.owner.as_ref() == Some(&submission.key.0));
        self.enabled.set(
            !busy
                && self.available.get(index).copied().unwrap_or(false)
                && !self.draft.get().trim().is_empty()
                && (self.shown == ComposerIntent::Queue || self.chosen_task().is_some()),
        );
        self.stop_enabled
            .set(!busy && self.can_stop && self.chosen_task().is_some());
        self.task_visible.set(self.shown != ComposerIntent::Queue);
    }

    fn tick(&mut self) {
        let next = self.intent.get();
        if next != self.shown {
            self.save_draft();
            self.shown = next;
            self.load_draft();
        }
        if self.close.get() {
            self.close.set(false);
            self.dismiss();
        }
        self.refresh_enabled();
    }

    fn save_draft(&mut self) {
        if let Some(owner) = &self.owner {
            self.drafts.insert(
                (owner.clone(), self.shown),
                Draft {
                    text: self.draft.get(),
                    revision: self.draft.revision(),
                    editor: editor_state(&self.popover),
                },
            );
        }
    }

    fn load_draft(&mut self) {
        let Some(owner) = &self.owner else {
            return;
        };
        let key = (owner.clone(), self.shown);
        let saved = self.drafts.get(&key).cloned();
        self.draft.set(
            saved
                .as_ref()
                .map_or_else(String::new, |draft| draft.text.clone()),
        );
        let revision = self.draft.revision();
        if let Some(saved) = saved {
            for pending in self
                .submissions
                .values_mut()
                .filter(|pending| pending.key == key && pending.revision == saved.revision)
            {
                pending.revision = revision;
            }
            self.pending_editor = saved.editor.map(|editor| (revision, editor));
        }
    }
}

fn matches_next_revision(before: Option<&ComposerTask>, after: &ComposerTask) -> bool {
    before.is_some_and(|before| {
        before.task_id == after.task_id
            && before.run_epoch == after.run_epoch
            && before.revision.checked_add(1) == Some(after.revision)
    })
}

fn task_receipt(key: &str, task: &ComposerTask) -> String {
    t_args(
        key,
        &[
            ("task", task.task_id.clone().into()),
            ("run", task.run_epoch.to_string().into()),
            ("revision", task.revision.to_string().into()),
        ],
    )
}

fn editor_state(widget: &dyn Widget) -> Option<String> {
    if widget.test_id() == Some("agent-quick-editor") {
        return ui::undo::get_state(widget);
    }
    widget.children().into_iter().find_map(editor_state)
}

fn refresh_task_picker(widget: &mut dyn Widget, options: &[(String, String)]) {
    if widget.test_id() == Some("agent-quick-task")
        && let Some(picker) = widget
            .as_any_mut()
            .and_then(|widget| widget.downcast_mut::<Dropdown>())
    {
        picker.set_value_options(options.to_vec());
        return;
    }
    for child in widget.children_mut() {
        refresh_task_picker(child, options);
    }
}

fn restore_editor(widget: &mut dyn Widget, state: &str) -> bool {
    if widget.test_id() == Some("agent-quick-editor") {
        ui::undo::set_state(widget, state);
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| restore_editor(child, state))
}

impl Decorator for AgentQuickComposer {
    type Inner = Popover<Container>;
    fn inner(&self) -> &Self::Inner {
        &self.popover
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.popover
    }
    fn name(&self) -> &'static str {
        "AgentQuickComposer"
    }
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if self.owner.is_none() {
            return;
        }
        self.tick();
        self.popover.sync(state, path);
        self.task_options
            .with(|options| refresh_task_picker(&mut self.popover, options));
        if let Some((revision, editor)) = self.pending_editor.take()
            && revision == self.draft.revision()
        {
            restore_editor(&mut self.popover, &editor);
        }
    }
    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        if self.owner.is_none() {
            return false;
        }
        self.tick();
        let changed = self.popover.update(state, path);
        self.task_options
            .with(|options| refresh_task_picker(&mut self.popover, options));
        changed
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let width = theme.tokens.metrics.panel_width_utility * theme.text_scale.max(1.0);
        let capacity = self.popover.available_content_width(theme);
        self.popover.content_mut().set_width(width.min(capacity));
        self.popover.build_layout(taffy, theme, nodes)
    }
    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset) | self.popover.take_focus_reset()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let open = self.is_open();
        self.popover.handle_event(event, ctx);
        if open && !self.is_open() {
            self.save_draft();
            self.focus_reset = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(agent: &str) -> ComposerOwner {
        ComposerOwner {
            room: "room".into(),
            agent: agent.into(),
            person: "certified-person".into(),
            identity_revision: 7,
        }
    }

    fn open(composer: &mut AgentQuickComposer, owner: ComposerOwner) {
        composer.open(
            owner,
            "Agent",
            AgentQuickComposerOrigin {
                marker: Rect::new(100.0, 100.0, 132.0, 132.0),
                protected: Rect::new(100.0, 100.0, 220.0, 164.0),
                context: String::new(),
            },
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
    }

    fn action(
        widget: &mut dyn Widget,
        id: &str,
        action: &ui::a11y::Action,
        doc: &mut automerge::AutoCommit,
    ) -> bool {
        if widget.test_id() == Some(id) {
            return widget.perform_action(
                action,
                &[],
                &mut ui::widget::Edits::lend(doc),
                &automerge::ROOT,
            );
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| self::action(child, id, action, doc))
    }

    fn edit(composer: &mut AgentQuickComposer, doc: &mut automerge::AutoCommit, text: &str) {
        Widget::sync(composer, doc, &automerge::ROOT);
        assert!(action(
            composer,
            "agent-quick-editor",
            &ui::a11y::Action::SetValue(text.into()),
            doc
        ));
        Widget::sync(composer, doc, &automerge::ROOT);
    }

    fn submit(
        composer: &mut AgentQuickComposer,
        doc: &mut automerge::AutoCommit,
    ) -> ComposerRequest {
        Widget::sync(composer, doc, &automerge::ROOT);
        assert!(action(
            composer,
            "agent-quick-send",
            &ui::a11y::Action::Activate,
            doc
        ));
        composer
            .take_request()
            .expect("the enabled ordinary button emits one intention")
    }

    #[test]
    fn ordinary_editor_and_submit_keep_unknown_outcomes_private_and_partition_agents() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let heads = doc.get_heads();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        let task = ComposerTask {
            task_id: "task".into(),
            run_epoch: 3,
            revision: 8,
            label: "Exact task".into(),
        };
        composer.project(&tide, [true, false, false], vec![task.clone()], false);
        composer.selected.set(task.key());
        edit(&mut composer, &mut doc, "Tide's private draft");
        let request = submit(&mut composer, &mut doc);
        assert_eq!(request.owner, tide);
        assert_eq!(request.text, "Tide's private draft");
        composer.observe_receipt(ComposerReceipt {
            owner: request.owner.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::Unknown,
        });
        assert_eq!(composer.draft.get(), request.text);
        assert!(
            composer.take_request().is_none(),
            "uncertain work is not replayed"
        );
        open(&mut composer, owner("reed"));
        assert!(composer.draft.get().is_empty());
        edit(&mut composer, &mut doc, "Reed's private draft");
        open(&mut composer, tide);
        assert_eq!(composer.draft.get(), "Tide's private draft");
        assert_eq!(
            doc.get_heads(),
            heads,
            "private editor input never writes collaborative content"
        );
    }

    #[test]
    fn actual_submit_receipt_does_not_clear_a_newer_edit_or_another_custody() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        let task = ComposerTask {
            task_id: "task".into(),
            run_epoch: 3,
            revision: 8,
            label: "Exact task".into(),
        };
        composer.project(&tide, [true, false, false], vec![task.clone()], false);
        composer.selected.set(task.key());
        edit(&mut composer, &mut doc, "Submitted");
        let request = submit(&mut composer, &mut doc);
        edit(&mut composer, &mut doc, "Newer draft");
        composer.observe_receipt(ComposerReceipt {
            owner: owner("reed"),
            submission: request.submission,
            outcome: ComposerOutcome::DirectionSaved(ComposerTask {
                revision: 9,
                ..task.clone()
            }),
        });
        assert_eq!(composer.draft.get(), "Newer draft");
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: request.submission,
            outcome: ComposerOutcome::DirectionSaved(ComposerTask {
                revision: 9,
                ..task
            }),
        });
        assert_eq!(composer.draft.get(), "Newer draft");
    }

    #[test]
    fn message_requires_explicit_task_and_saved_receipt_clears_only_its_private_draft() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let heads = doc.get_heads();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        let task = ComposerTask {
            task_id: "task".into(),
            run_epoch: 3,
            revision: 8,
            label: "Exact task".into(),
        };
        composer.project(&tide, [true, false, true], vec![task.clone()], false);
        edit(&mut composer, &mut doc, "Exact direction");
        assert!(!composer.enabled.get());
        composer.selected.set(task.key());
        let request = submit(&mut composer, &mut doc);
        assert_eq!(request.intent, ComposerIntent::Message);
        assert_eq!(request.task, Some(task.clone()));
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::DirectionSaved(ComposerTask {
                run_epoch: 4,
                revision: 9,
                ..task.clone()
            }),
        });
        assert_eq!(composer.draft.get(), "Exact direction");
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: request.submission,
            outcome: ComposerOutcome::DirectionSaved(ComposerTask {
                revision: 9,
                ..task
            }),
        });
        assert!(composer.draft.get().is_empty());
        assert_eq!(doc.get_heads(), heads);
    }

    #[test]
    fn genuine_queue_receipt_preserves_exact_conversation_target_through_refresh() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        composer.intent.set(ComposerIntent::Queue);
        composer.project(&tide, [false, true, false], Vec::new(), false);
        edit(
            &mut composer,
            &mut doc,
            "Full instruction\nincluding its second line",
        );
        let request = submit(&mut composer, &mut doc);
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::Queued {
                task_id: "task:new".into(),
            },
        });
        composer.project(&tide, [false, true, false], Vec::new(), false);
        assert_eq!(composer.conversation_task(), Some(("task:new".into(), 0)));
        assert!(composer.draft.get().is_empty());
        open(&mut composer, owner("reed"));
        assert!(composer.conversation_task().is_none());
        open(&mut composer, tide);
        assert_eq!(composer.conversation_task(), Some(("task:new".into(), 0)));
    }

    #[test]
    fn direction_receipts_require_exact_run_successor_and_never_regress_applied_to_saved() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        composer.intent.set(ComposerIntent::Steer);
        let before = ComposerTask {
            task_id: "task".into(),
            run_epoch: 3,
            revision: 8,
            label: "Exact task".into(),
        };
        composer.project(&tide, [false, false, true], vec![before.clone()], true);
        composer.selected.set(before.key());
        edit(&mut composer, &mut doc, "Direction");
        let request = submit(&mut composer, &mut doc);
        let after = ComposerTask {
            revision: 9,
            ..before
        };
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::DirectionApplied(ComposerTask {
                run_epoch: 4,
                ..after.clone()
            }),
        });
        assert_eq!(composer.draft.get(), "Direction");
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::DirectionApplied(after.clone()),
        });
        assert!(composer.draft.get().is_empty());
        let applied = composer.status.get();
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: request.submission,
            outcome: ComposerOutcome::DirectionSaved(after),
        });
        assert_eq!(composer.status.get(), applied);
    }
}
