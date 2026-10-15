//! The agent composer: one private field beside what the person is looking
//! at. They say what they want and send it; the agent decides whether that
//! is a question it answers here or work it carries out as a task. While the
//! work runs, the same field steers it and a Stop control appears.
//!
//! The owner adapter (`overlay_host::agent_actions`), not this widget,
//! resolves and admits the published agent and Task Center actions.

use std::collections::{HashMap, HashSet};

use ui::widget::{Button, ButtonRole, Column, Container, Decorator, Divider, Popover, PopoverSide};
use ui::widget::{Row, Spinner, Text, TextInput, When, Widget};
use ui::{LocalState, Theme};
use vello::kurbo::Rect;

use super::i18n::{t, t_args};

/// What a submission asks of the agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum ComposerIntent {
    /// A new request: the agent answers it or carries it out as a task.
    #[default]
    Request,
    /// A direction for the run the composer follows.
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

/// One turn of the composer's private conversation with its agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerTurn {
    /// Whether the agent said it, rather than the person.
    pub agent: bool,
    pub text: String,
}

/// A user's intention, not an execution authority. The adapter re-resolves
/// the actions and rechecks this exact owner/run/revision before submitting.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ComposerRequest {
    pub owner: ComposerOwner,
    pub intent: ComposerIntent,
    pub submission: u64,
    pub text: String,
    /// The followed run a direction or Stop is for.
    pub task: Option<ComposerTask>,
    pub stop: bool,
    /// What the request is about, as the composer shows it.
    pub context: String,
    /// The conversation before this request, oldest first.
    pub conversation: Vec<ComposerTurn>,
    /// The selection the request is about, when the composer was opened
    /// on one ([`AgentQuickComposer::open_about`]).
    pub about: Option<super::selection_agent::SelectionSeed>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ComposerOutcome {
    /// The agent answered the request itself.
    Answered(String),
    /// The request became a task, which the adapter starts and follows.
    Started {
        task_id: String,
        title: String,
    },
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

/// Where the followed run is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunPhase {
    /// Queued and being started.
    Starting,
    /// The agent is working on it.
    Working,
    /// Done, with the agent's closing words.
    Finished(Option<String>),
    /// Stopped short and waiting on the person, with the agent's reason.
    NeedsAttention(Option<String>),
    Stopped,
}

impl RunPhase {
    pub(crate) fn running(&self) -> bool {
        matches!(self, Self::Starting | Self::Working)
    }
}

/// The run the composer follows: the one it started, or the one its agent
/// was working on where the person opened it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerRun {
    pub task: ComposerTask,
    pub phase: RunPhase,
}

/// What the person does with a text answer about their selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AnswerUse {
    /// Put the answer where the selection is.
    Insert(String),
    /// Keep the answer as a new note.
    SaveAsNote(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentQuickComposerOrigin {
    /// What the person pointed at: an agent's marker, or a selection.
    pub marker: Rect,
    /// What the composer must not cover.
    pub protected: Rect,
    pub context: String,
}

/// Most turns the conversation keeps, and so sends with a follow-up.
const MAX_TURNS: usize = 16;

#[derive(Clone)]
struct Draft {
    text: String,
    revision: u64,
    editor: Option<String>,
}

struct Submission {
    owner: ComposerOwner,
    intent: ComposerIntent,
    text: String,
    revision: u64,
    task: Option<ComposerTask>,
    stop: bool,
    applied: bool,
    busy: bool,
}

/// The view's live values, each read by one widget.
#[derive(Clone)]
struct View {
    title: LocalState<String>,
    context: LocalState<String>,
    asked: LocalState<String>,
    answer: LocalState<String>,
    status: LocalState<String>,
    has_context: LocalState<bool>,
    has_asked: LocalState<bool>,
    has_answer: LocalState<bool>,
    can_use_answer: LocalState<bool>,
    has_status: LocalState<bool>,
    /// Whether the agent is still at it: thinking, sending or working.
    waiting: LocalState<bool>,
    running: LocalState<bool>,
    enabled: LocalState<bool>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            title: LocalState::new(t("agent-quick-accessible-name")),
            context: LocalState::new(String::new()),
            asked: LocalState::new(String::new()),
            answer: LocalState::new(String::new()),
            status: LocalState::new(String::new()),
            has_context: LocalState::new(false),
            has_asked: LocalState::new(false),
            has_answer: LocalState::new(false),
            can_use_answer: LocalState::new(false),
            has_status: LocalState::new(false),
            waiting: LocalState::new(false),
            running: LocalState::new(false),
            enabled: LocalState::new(false),
        }
    }
}

/// Buttons the person pressed since the last poll.
#[derive(Clone)]
struct Presses {
    submit: LocalState<bool>,
    stop: LocalState<bool>,
    close: LocalState<bool>,
    insert: LocalState<bool>,
    save: LocalState<bool>,
}

impl Default for Presses {
    fn default() -> Self {
        Self {
            submit: LocalState::new(false),
            stop: LocalState::new(false),
            close: LocalState::new(false),
            insert: LocalState::new(false),
            save: LocalState::new(false),
        }
    }
}

fn take(flag: &LocalState<bool>) -> bool {
    let pressed = flag.get();
    if pressed {
        flag.set(false);
    }
    pressed
}

pub(crate) struct AgentQuickComposer {
    popover: Popover<Container>,
    owner: Option<ComposerOwner>,
    label: String,
    context: String,
    draft: LocalState<String>,
    view: View,
    presses: Presses,
    /// Whether the composer was opened about a selection, so a text answer
    /// can be inserted there or kept as a note.
    selection: bool,
    conversations: HashMap<ComposerOwner, Vec<ComposerTurn>>,
    /// Agents whose last turn answers a request, rather than closing a run.
    answered: HashSet<ComposerOwner>,
    runs: HashMap<ComposerOwner, ComposerRun>,
    /// Ended runs already told, so a repeated projection is told once.
    ended: HashSet<(String, u64)>,
    /// What the status line says until something newer happens.
    notice: Option<String>,
    /// Whether the notice says the composer is waiting on the agent.
    notice_waits: bool,
    drafts: HashMap<ComposerOwner, Draft>,
    submissions: HashMap<u64, Submission>,
    next_submission: u64,
    answer_use: Option<AnswerUse>,
    placeholder: String,
    pending_editor: Option<(u64, String)>,
    focus_reset: bool,
    /// What the composer was opened about, which a queued task carries.
    about: Option<super::selection_agent::SelectionSeed>,
}

impl Default for AgentQuickComposer {
    fn default() -> Self {
        let mut composer = Self {
            popover: Popover::with_content(Container::new()),
            owner: None,
            label: String::new(),
            context: String::new(),
            draft: LocalState::new(String::new()),
            view: View::default(),
            presses: Presses::default(),
            selection: false,
            conversations: HashMap::new(),
            answered: HashSet::new(),
            runs: HashMap::new(),
            ended: HashSet::new(),
            notice: None,
            notice_waits: false,
            drafts: HashMap::new(),
            submissions: HashMap::new(),
            next_submission: 0,
            answer_use: None,
            placeholder: String::new(),
            pending_editor: None,
            focus_reset: false,
            about: None,
        };
        composer.popover = composer.controls();
        composer
    }
}

fn wrapped(text: Text) -> Box<dyn Widget> {
    Box::new(Container::new().with_no_padding().with_child(Box::new(
        text.with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
    )))
}

impl AgentQuickComposer {
    fn header(&self) -> Row {
        let close = self.presses.close.clone();
        Row::new()
            .with_justify(taffy::JustifyContent::SpaceBetween)
            .push(Box::new(
                Text::from_state(&self.view.title, Clone::clone).with_role(ui::TextRole::Heading),
            ))
            .push(Box::new(
                Button::icon_only(
                    "agent-quick-close",
                    ui::icons::Icon::X,
                    &t("agent-quick-close"),
                )
                .on_click(move || close.set(true)),
            ))
    }

    /// What to do with an answer about a selection.
    fn uses(&self) -> Row {
        let insert = self.presses.insert.clone();
        let save = self.presses.save.clone();
        Row::new()
            .push(Box::new(
                Button::new("agent-quick-insert", &t("agent-quick-insert"))
                    .on_click(move || insert.set(true)),
            ))
            .push(Box::new(
                Button::new("agent-quick-save-note", &t("agent-quick-save-note"))
                    .on_click(move || save.set(true)),
            ))
    }

    /// Stop while a run goes on, and Send.
    fn footer(&self) -> Row {
        let stop = self.presses.stop.clone();
        let submit = self.presses.submit.clone();
        Row::new()
            .with_justify(taffy::JustifyContent::End)
            .push(Box::new(When::from_state(
                &self.view.running,
                |shown| *shown,
                Box::new(
                    Button::new("agent-quick-stop", &t("agent-quick-stop"))
                        .on_click(move || stop.set(true)),
                ),
            )))
            .push(Box::new(
                Button::new("agent-quick-send", &t("agent-quick-send"))
                    .with_icon(ui::icons::Icon::Send)
                    .role(ButtonRole::Primary)
                    .enabled_from(&self.view.enabled)
                    .on_click(move || submit.set(true)),
            ))
    }

    fn controls(&self) -> Popover<Container> {
        let view = &self.view;
        let context = When::from_state(
            &view.has_context,
            |shown| *shown,
            wrapped(
                Text::from_state(&view.context, Clone::clone)
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
            ),
        );
        let submit = self.presses.submit.clone();
        let editor = TextInput::from_state(&self.draft)
            .with_id("agent-quick-editor")
            .with_placeholder(t("agent-quick-placeholder-idle"))
            .with_accessible_name(t("agent-quick-editor-name"))
            .with_min_lines(2)
            .on_submit(move |_| submit.set(true));
        let status = When::from_state(
            &view.has_status,
            |shown| *shown,
            Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(When::from_state(
                        &view.waiting,
                        |shown| *shown,
                        Box::new(Spinner::new()),
                    )))
                    .push(wrapped(
                        Text::from_state(&view.status, Clone::clone)
                            .with_id("agent-quick-status")
                            .with_role(ui::TextRole::Caption)
                            .with_live(ui::a11y::Politeness::Polite)
                            .dimmed(),
                    )),
            ),
        );
        let body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .push(Box::new(self.header()))
            .push(Box::new(context))
            .push(Box::new(When::from_state(
                &view.has_asked,
                |shown| *shown,
                Box::new(Divider::horizontal()),
            )))
            .push(Box::new(When::from_state(
                &view.has_asked,
                |shown| *shown,
                wrapped(
                    Text::from_state(&view.asked, Clone::clone)
                        .with_role(ui::TextRole::Caption)
                        .dimmed(),
                ),
            )))
            .push(Box::new(When::from_state(
                &view.has_answer,
                |shown| *shown,
                wrapped(
                    Text::from_state(&view.answer, Clone::clone)
                        .with_id("agent-quick-answer")
                        .with_live(ui::a11y::Politeness::Polite),
                ),
            )))
            .push(Box::new(When::from_state(
                &view.can_use_answer,
                |shown| *shown,
                Box::new(self.uses()),
            )))
            .push(Box::new(status))
            .push(Box::new(editor))
            .push(Box::new(self.footer()));
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
        self.about = None;
        label.clone_into(&mut self.label);
        self.view
            .title
            .set(t_args("agent-quick-title", &[("agent", label.into())]));
        self.context.clone_from(&origin.context);
        self.view.has_context.set(!origin.context.trim().is_empty());
        self.view.context.set(origin.context);
        self.selection = false;
        self.notice = None;
        self.answer_use = None;
        self.owner = Some(owner);
        self.load_draft();
        self.popover.set_viewport(viewport);
        // Beside everything the composer is about, never on top of it.
        self.popover
            .set_anchor(origin.marker.union(origin.protected));
        self.popover.set_avoid(Some(origin.protected));
        self.popover.set_open(true);
        self.refresh();
        self.focus_reset = true;
    }

    /// Open on what the person selected: the composer shows what it is
    /// about, offers to put a text answer at the selection, and work it
    /// queues carries the selection so the agent acts at that place.
    pub(crate) fn open_about(
        &mut self,
        owner: ComposerOwner,
        label: &str,
        origin: AgentQuickComposerOrigin,
        viewport: Rect,
        about: super::selection_agent::SelectionSeed,
    ) {
        self.open(
            owner,
            label,
            AgentQuickComposerOrigin {
                context: about.context(),
                ..origin
            },
            viewport,
        );
        self.selection = about.selection.is_some();
        self.about = Some(about);
        self.refresh();
    }

    /// The selection the composer was opened about, if it was.
    pub(crate) fn about(&self) -> Option<&super::selection_agent::SelectionSeed> {
        self.about.as_ref()
    }

    /// Whether the composer is open about a selection rather than beside an
    /// agent's marker, so it stays while that agent shows no marker.
    pub(crate) fn about_selection(&self) -> bool {
        self.is_open() && self.about.is_some()
    }

    /// Offer to insert a text answer at the selection the composer was
    /// opened about, or keep it as a note.
    #[cfg_attr(
        not(any(test, debug_assertions)),
        expect(
            dead_code,
            reason = "opening the composer about a selection is what sets it"
        )
    )]
    pub(crate) fn set_about_selection(&mut self, selection: bool) {
        self.selection = selection;
        self.refresh();
    }

    /// Show a draft, an earlier conversation and a run without any agent,
    /// for the offscreen shots renderer.
    #[cfg(debug_assertions)]
    pub(crate) fn stage_for_shots(
        &mut self,
        draft: &str,
        conversation: Vec<ComposerTurn>,
        run: Option<ComposerRun>,
    ) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        self.draft.set(draft.to_owned());
        if conversation.last().is_some_and(|turn| turn.agent) {
            self.answered.insert(owner.clone());
        }
        self.conversations.insert(owner.clone(), conversation);
        self.project(&owner, run);
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

    /// The run `owner`'s composer follows now, if any. A run that ended is
    /// told once, as the agent's turn in the conversation.
    pub(crate) fn project(&mut self, owner: &ComposerOwner, run: Option<ComposerRun>) {
        let Some(run) = run else {
            self.runs.remove(owner);
            self.refresh();
            return;
        };
        if run.phase.running() {
            // A new phase says more than an earlier notice; a new revision
            // of the same run does not.
            let moved = self.runs.get(owner).map(|known| &known.phase) != Some(&run.phase);
            if moved && self.owner.as_ref() == Some(owner) && !self.busy() {
                self.notice = None;
            }
            self.runs.insert(owner.clone(), run);
        } else {
            self.runs.remove(owner);
            if self
                .ended
                .insert((run.task.task_id.clone(), run.task.run_epoch))
            {
                self.end_run(owner, &run);
            }
        }
        self.refresh();
    }

    fn end_run(&mut self, owner: &ComposerOwner, run: &ComposerRun) {
        let agent = self.label.clone();
        let (said, notice) = match &run.phase {
            RunPhase::Finished(words) => (
                words.clone(),
                t_args("agent-quick-run-done", &[("agent", agent.into())]),
            ),
            RunPhase::NeedsAttention(words) => (
                words.clone(),
                t_args("agent-quick-run-attention", &[("agent", agent.into())]),
            ),
            _ => (None, t("agent-quick-run-stopped")),
        };
        if let Some(said) = said.filter(|said| !said.trim().is_empty()) {
            self.remember(owner, true, said);
        }
        if self.owner.as_ref() == Some(owner) {
            self.notice = Some(notice);
            self.notice_waits = false;
        }
    }

    fn remember(&mut self, owner: &ComposerOwner, agent: bool, text: String) {
        self.answered.remove(owner);
        let turns = self.conversations.entry(owner.clone()).or_default();
        turns.push(ComposerTurn { agent, text });
        let excess = turns.len().saturating_sub(MAX_TURNS);
        turns.drain(..excess);
    }

    /// The run the open composer follows.
    pub(crate) fn followed(&self) -> Option<&ComposerRun> {
        self.owner.as_ref().and_then(|owner| self.runs.get(owner))
    }

    fn busy(&self) -> bool {
        self.submissions
            .values()
            .any(|pending| pending.busy && self.owner.as_ref() == Some(&pending.owner))
    }

    pub(crate) fn take_request(&mut self) -> Option<ComposerRequest> {
        self.tick();
        let stop = take(&self.presses.stop);
        let submit = take(&self.presses.submit);
        if !self.is_open() || (!stop && !submit) || self.busy() {
            return None;
        }
        let owner = self.owner.clone()?;
        let task = self.followed().map(|run| run.task.clone());
        let text = self.draft.get();
        if (stop && task.is_none()) || (!stop && text.trim().is_empty()) {
            return None;
        }
        let intent = if task.is_some() {
            ComposerIntent::Steer
        } else {
            ComposerIntent::Request
        };
        self.next_submission = self.next_submission.checked_add(1)?;
        self.save_draft();
        self.submissions.insert(
            self.next_submission,
            Submission {
                owner: owner.clone(),
                intent,
                text: text.clone(),
                revision: self.draft.revision(),
                task: task.clone(),
                stop,
                applied: false,
                busy: true,
            },
        );
        self.notice_waits = true;
        self.notice = Some(t(match (stop, intent) {
            (true, _) => "agent-quick-stopping",
            (false, ComposerIntent::Request) => "agent-quick-thinking",
            (false, ComposerIntent::Steer) => "agent-quick-steering",
        }));
        let request = ComposerRequest {
            owner: owner.clone(),
            intent,
            submission: self.next_submission,
            text: if stop { String::new() } else { text },
            task,
            stop,
            context: self.context.clone(),
            conversation: self.conversations.get(&owner).cloned().unwrap_or_default(),
            about: self.about.clone().filter(|_| !stop),
        };
        self.refresh();
        Some(request)
    }

    /// What the person chose to do with the last answer.
    pub(crate) fn take_answer_use(&mut self) -> Option<AnswerUse> {
        self.tick();
        self.answer_use.take()
    }

    pub(crate) fn observe_receipt(&mut self, receipt: ComposerReceipt) {
        let Some(pending) = self.submissions.get_mut(&receipt.submission) else {
            return;
        };
        if pending.owner != receipt.owner {
            return;
        }
        let success = match &receipt.outcome {
            ComposerOutcome::Answered(_) | ComposerOutcome::Started { .. } => {
                pending.intent == ComposerIntent::Request && !pending.stop
            }
            ComposerOutcome::DirectionSaved(task) | ComposerOutcome::DirectionApplied(task) => {
                pending.intent == ComposerIntent::Steer
                    && !pending.stop
                    && matches_next_revision(pending.task.as_ref(), task)
            }
            ComposerOutcome::Stopped(task) => {
                pending.stop
                    && pending.task.as_ref().is_some_and(|before| {
                        before.task_id == task.task_id && before.run_epoch == task.run_epoch
                    })
            }
            ComposerOutcome::Refused | ComposerOutcome::Unknown => false,
        };
        let failed = matches!(
            receipt.outcome,
            ComposerOutcome::Refused | ComposerOutcome::Unknown
        );
        if !success && !failed {
            return;
        }
        if matches!(receipt.outcome, ComposerOutcome::DirectionSaved(_)) && pending.applied {
            return;
        }
        pending.applied |= matches!(receipt.outcome, ComposerOutcome::DirectionApplied(_));
        // Unknown keeps the composer waiting: the work may still land, and
        // sending it again could do it twice.
        if success || matches!(receipt.outcome, ComposerOutcome::Refused) {
            pending.busy = false;
        }
        let owner = pending.owner.clone();
        let revision = pending.revision;
        let asked = pending.text.clone();
        let stop = pending.stop;
        if success && !stop && !matches!(receipt.outcome, ComposerOutcome::DirectionApplied(_)) {
            self.clear_sent(&owner, revision);
        }
        let notice = self.record(&owner, asked, stop, receipt.outcome);
        if self.owner.as_ref() == Some(&owner) {
            self.notice = notice;
            self.notice_waits = false;
        }
        self.refresh();
    }

    /// The person's sent text leaves the field, unless they wrote on since.
    fn clear_sent(&mut self, owner: &ComposerOwner, revision: u64) {
        if self.owner.as_ref() == Some(owner) && self.draft.revision() == revision {
            self.draft.set(String::new());
        }
        if let Some(draft) = self.drafts.get_mut(owner)
            && draft.revision == revision
        {
            draft.text.clear();
            draft.editor = None;
            draft.revision = draft.revision.saturating_add(1);
        }
    }

    /// Keep what an accepted outcome changed, and say what the status line
    /// should.
    fn record(
        &mut self,
        owner: &ComposerOwner,
        asked: String,
        stop: bool,
        outcome: ComposerOutcome,
    ) -> Option<String> {
        let agent = self.label.clone();
        match outcome {
            ComposerOutcome::Answered(answer) => {
                self.remember(owner, false, asked);
                self.remember(owner, true, answer);
                self.answered.insert(owner.clone());
                None
            }
            ComposerOutcome::Started { task_id, title } => {
                self.remember(owner, false, asked);
                let run = ComposerRun {
                    task: ComposerTask {
                        task_id,
                        run_epoch: 0,
                        revision: 0,
                        label: title,
                    },
                    phase: RunPhase::Starting,
                };
                self.runs.insert(owner.clone(), run);
                None
            }
            ComposerOutcome::DirectionSaved(_) => Some(t_args(
                "agent-quick-steer-saved",
                &[("agent", agent.into())],
            )),
            ComposerOutcome::DirectionApplied(_) => Some(t_args(
                "agent-quick-steer-applied",
                &[("agent", agent.into())],
            )),
            ComposerOutcome::Stopped(task) => {
                self.runs.remove(owner);
                self.ended.insert((task.task_id, task.run_epoch));
                Some(t("agent-quick-run-stopped"))
            }
            ComposerOutcome::Unknown => Some(t("agent-quick-outcome-unknown")),
            ComposerOutcome::Refused => Some(t_args(
                if stop {
                    "agent-quick-stop-refused"
                } else {
                    "agent-quick-refused"
                },
                &[("agent", agent.into())],
            )),
        }
    }

    /// Bring every view value up to date with the composer's state.
    fn refresh(&self) {
        let Some(owner) = &self.owner else {
            return;
        };
        let turns = self.conversations.get(owner);
        let answer = turns
            .and_then(|turns| turns.last())
            .filter(|turn| turn.agent);
        let asked = turns.and_then(|turns| {
            let before = turns
                .len()
                .checked_sub(if answer.is_some() { 2 } else { 1 })?;
            turns.get(before).filter(|turn| !turn.agent)
        });
        self.view.asked.set(asked.map_or_else(String::new, |turn| {
            t_args(
                "agent-quick-you-asked",
                &[("text", turn.text.clone().into())],
            )
        }));
        self.view
            .answer
            .set(answer.map(|turn| turn.text.clone()).unwrap_or_default());
        self.view.has_asked.set(asked.is_some());
        self.view.has_answer.set(answer.is_some());
        let run = self.runs.get(owner);
        let running = run.is_some_and(|run| run.phase.running());
        // A run being started has no run yet to direct or stop.
        let starting = run.is_some_and(|run| run.phase == RunPhase::Starting);
        let busy = self.busy();
        self.view.can_use_answer.set(
            self.selection
                && answer.is_some()
                && self.answered.contains(owner)
                && !running
                && !busy,
        );
        self.view.running.set(running && !starting && !busy);
        self.view
            .enabled
            .set(!busy && !starting && !self.draft.get().trim().is_empty());
        let status = self.notice.clone().or_else(|| {
            run.map(|run| {
                let key = match run.phase {
                    RunPhase::Starting => "agent-quick-run-starting",
                    _ => "agent-quick-run-working",
                };
                t_args(key, &[("task", run.task.label.clone().into())])
            })
        });
        self.view.has_status.set(status.is_some());
        self.view
            .waiting
            .set((busy && self.notice_waits) || (self.notice.is_none() && running));
        self.view.status.set(status.unwrap_or_default());
    }

    fn placeholder_text(&self) -> String {
        let agent = self.label.clone();
        if self.followed().is_some() {
            t_args(
                "agent-quick-placeholder-running",
                &[("agent", agent.into())],
            )
        } else {
            t_args("agent-quick-placeholder-idle", &[("agent", agent.into())])
        }
    }

    fn tick(&mut self) {
        if take(&self.presses.close) {
            self.dismiss();
        }
        let answer = self.view.answer.get();
        if take(&self.presses.insert) && self.view.can_use_answer.get() {
            self.answer_use = Some(AnswerUse::Insert(answer.clone()));
        }
        if take(&self.presses.save) && self.view.can_use_answer.get() {
            self.answer_use = Some(AnswerUse::SaveAsNote(answer));
        }
        self.refresh();
    }

    fn save_draft(&mut self) {
        if let Some(owner) = &self.owner {
            self.drafts.insert(
                owner.clone(),
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
        let saved = self.drafts.get(owner).cloned();
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
                .filter(|pending| &pending.owner == owner && pending.revision == saved.revision)
            {
                pending.revision = revision;
            }
            self.pending_editor = saved.editor.map(|editor| (revision, editor));
        }
    }

    /// Bring the editor's placeholder in line with what Send will do.
    fn refresh_placeholder(&mut self) {
        let placeholder = self.placeholder_text();
        if placeholder != self.placeholder {
            set_placeholder(&mut self.popover, &placeholder);
            self.placeholder = placeholder;
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

fn editor_state(widget: &dyn Widget) -> Option<String> {
    if widget.test_id() == Some("agent-quick-editor") {
        return ui::undo::get_state(widget);
    }
    widget.children().into_iter().find_map(editor_state)
}

fn set_placeholder(widget: &mut dyn Widget, placeholder: &str) -> bool {
    if widget.test_id() == Some("agent-quick-editor")
        && let Some(editor) = widget
            .as_any_mut()
            .and_then(|widget| widget.downcast_mut::<TextInput>())
    {
        editor.set_placeholder(placeholder);
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| set_placeholder(child, placeholder))
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
        self.refresh_placeholder();
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
        self.refresh_placeholder();
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
            "Tide",
            AgentQuickComposerOrigin {
                marker: Rect::new(100.0, 100.0, 132.0, 132.0),
                protected: Rect::new(100.0, 100.0, 220.0, 164.0),
                context: "Tide was renaming the release notes".into(),
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

    fn press(composer: &mut AgentQuickComposer, doc: &mut automerge::AutoCommit, id: &str) {
        Widget::sync(composer, doc, &automerge::ROOT);
        assert!(
            action(composer, id, &ui::a11y::Action::Activate, doc),
            "{id} is shown and enabled"
        );
    }

    fn submit(
        composer: &mut AgentQuickComposer,
        doc: &mut automerge::AutoCommit,
    ) -> ComposerRequest {
        press(composer, doc, "agent-quick-send");
        composer
            .take_request()
            .expect("the enabled ordinary button emits one intention")
    }

    fn shown(composer: &mut AgentQuickComposer, id: &str) -> bool {
        fn find(widget: &dyn Widget, id: &str) -> bool {
            widget.test_id() == Some(id) || widget.children().into_iter().any(|c| find(c, id))
        }
        let doc = automerge::AutoCommit::new();
        Widget::sync(composer, &doc, &automerge::ROOT);
        find(composer, id)
    }

    fn working(task: &str, phase: RunPhase) -> ComposerRun {
        ComposerRun {
            task: ComposerTask {
                task_id: task.into(),
                run_epoch: 3,
                revision: 8,
                label: "Fix the link".into(),
            },
            phase,
        }
    }

    #[test]
    fn one_field_sends_a_request_the_agent_answers_and_a_follow_up_carries_it() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let heads = doc.get_heads();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        assert!(!composer.view.enabled.get(), "nothing to send yet");
        assert!(!composer.view.has_status.get(), "no checking notice");
        assert!(!shown(&mut composer, "agent-quick-stop"));
        edit(&mut composer, &mut doc, "What does idempotent mean?");
        let request = submit(&mut composer, &mut doc);
        assert_eq!(request.intent, ComposerIntent::Request);
        assert_eq!(request.task, None);
        assert_eq!(request.context, "Tide was renaming the release notes");
        assert!(request.conversation.is_empty());
        assert!(
            composer.take_request().is_none(),
            "one request at a time per agent"
        );
        assert_eq!(composer.view.status.get(), t("agent-quick-thinking"));
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: request.submission,
            outcome: ComposerOutcome::Answered("Safe to repeat.".into()),
        });
        assert!(composer.draft.get().is_empty());
        assert_eq!(composer.view.answer.get(), "Safe to repeat.");
        assert!(composer.view.asked.get().contains("idempotent"));
        assert!(!composer.view.has_status.get());
        assert!(
            !composer.view.can_use_answer.get(),
            "nothing selected, nothing to insert into"
        );

        edit(&mut composer, &mut doc, "Give an example");
        let follow_up = submit(&mut composer, &mut doc);
        assert_eq!(follow_up.intent, ComposerIntent::Request);
        assert_eq!(
            follow_up.conversation,
            vec![
                ComposerTurn {
                    agent: false,
                    text: "What does idempotent mean?".into()
                },
                ComposerTurn {
                    agent: true,
                    text: "Safe to repeat.".into()
                },
            ]
        );
        assert_eq!(
            doc.get_heads(),
            heads,
            "private editor input never writes collaborative content"
        );
    }

    #[test]
    fn work_runs_as_a_task_the_same_field_steers_and_stop_appears_while_it_runs() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        edit(
            &mut composer,
            &mut doc,
            "Replace the link with the real one",
        );
        let request = submit(&mut composer, &mut doc);
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::Started {
                task_id: "task:new".into(),
                title: "Fix the link".into(),
            },
        });
        assert!(composer.draft.get().is_empty());
        assert!(composer.view.status.get().contains("Fix the link"));
        assert!(
            !shown(&mut composer, "agent-quick-stop"),
            "nothing to stop before the run starts"
        );

        let run = working("task:new", RunPhase::Working);
        composer.project(&tide, Some(run.clone()));
        assert!(shown(&mut composer, "agent-quick-stop"));
        edit(&mut composer, &mut doc, "Use the https address");
        let steer = submit(&mut composer, &mut doc);
        assert_eq!(steer.intent, ComposerIntent::Steer);
        assert_eq!(steer.task, Some(run.task.clone()));
        assert!(!steer.stop);
        let saved = ComposerTask {
            revision: 9,
            ..run.task.clone()
        };
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: steer.submission,
            outcome: ComposerOutcome::DirectionSaved(ComposerTask {
                run_epoch: 4,
                ..saved.clone()
            }),
        });
        assert_eq!(
            composer.draft.get(),
            "Use the https address",
            "another run's receipt never clears the draft"
        );
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: steer.submission,
            outcome: ComposerOutcome::DirectionSaved(saved.clone()),
        });
        assert!(composer.draft.get().is_empty());
        let saved_notice = composer.view.status.get();
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: steer.submission,
            outcome: ComposerOutcome::DirectionApplied(saved.clone()),
        });
        let applied_notice = composer.view.status.get();
        assert_ne!(applied_notice, saved_notice);
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: steer.submission,
            outcome: ComposerOutcome::DirectionSaved(saved),
        });
        assert_eq!(
            composer.view.status.get(),
            applied_notice,
            "applied never regresses to saved"
        );

        press(&mut composer, &mut doc, "agent-quick-stop");
        let stop = composer.take_request().expect("Stop is a request");
        assert!(stop.stop);
        assert_eq!(stop.task, Some(run.task.clone()));
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: stop.submission,
            outcome: ComposerOutcome::Stopped(run.task),
        });
        assert!(composer.followed().is_none());
        assert!(!shown(&mut composer, "agent-quick-stop"));
        edit(&mut composer, &mut doc, "Something else");
        assert_eq!(
            submit(&mut composer, &mut doc).intent,
            ComposerIntent::Request,
            "with nothing running, the field asks anew"
        );
    }

    #[test]
    fn a_finished_run_is_told_once_as_the_agents_turn() {
        let mut composer = AgentQuickComposer::default();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        composer.set_about_selection(true);
        composer.project(&tide, Some(working("task:one", RunPhase::Working)));
        assert!(composer.followed().is_some());
        let done = working("task:one", RunPhase::Finished(Some("Linked it.".into())));
        composer.project(&tide, Some(done.clone()));
        composer.project(&tide, Some(done));
        assert!(composer.followed().is_none());
        assert_eq!(composer.view.answer.get(), "Linked it.");
        assert!(
            !composer.view.can_use_answer.get(),
            "a run's closing words are not text to insert"
        );
        assert_eq!(
            composer.conversations[&tide].len(),
            1,
            "a repeated projection is told once"
        );
        assert!(composer.view.status.get().contains("Tide"));
    }

    #[test]
    fn a_refused_request_keeps_the_draft_and_an_unknown_one_is_never_resent() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        edit(&mut composer, &mut doc, "Tidy this");
        let request = submit(&mut composer, &mut doc);
        composer.observe_receipt(ComposerReceipt {
            owner: owner("reed"),
            submission: request.submission,
            outcome: ComposerOutcome::Refused,
        });
        assert!(composer.busy(), "another agent's receipt is ignored");
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: request.submission,
            outcome: ComposerOutcome::Unknown,
        });
        assert_eq!(composer.draft.get(), "Tidy this");
        assert!(composer.busy(), "uncertain work is not replayed");
        assert!(composer.take_request().is_none());
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: request.submission,
            outcome: ComposerOutcome::Refused,
        });
        assert_eq!(composer.draft.get(), "Tidy this");
        assert!(composer.view.enabled.get());
    }

    #[test]
    fn drafts_and_conversations_stay_with_their_agent() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        edit(&mut composer, &mut doc, "Tide's private draft");
        open(&mut composer, owner("reed"));
        assert!(composer.draft.get().is_empty());
        edit(&mut composer, &mut doc, "Reed's private draft");
        open(&mut composer, tide);
        assert_eq!(composer.draft.get(), "Tide's private draft");
    }

    #[test]
    fn a_text_answer_about_a_selection_can_be_inserted_or_kept_as_a_note() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        composer.set_about_selection(true);
        assert!(!shown(&mut composer, "agent-quick-insert"), "no answer yet");
        edit(&mut composer, &mut doc, "Translate this");
        let request = submit(&mut composer, &mut doc);
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: request.submission,
            outcome: ComposerOutcome::Answered("Hallo".into()),
        });
        assert!(shown(&mut composer, "agent-quick-insert"));
        press(&mut composer, &mut doc, "agent-quick-insert");
        assert_eq!(
            composer.take_answer_use(),
            Some(AnswerUse::Insert("Hallo".into()))
        );
        press(&mut composer, &mut doc, "agent-quick-save-note");
        assert_eq!(
            composer.take_answer_use(),
            Some(AnswerUse::SaveAsNote("Hallo".into()))
        );
        assert_eq!(composer.take_answer_use(), None);
    }

    #[test]
    fn the_composer_is_anchored_beside_what_it_is_about() {
        let mut composer = AgentQuickComposer::default();
        let selection = Rect::new(560.0, 180.0, 700.0, 240.0);
        composer.open(
            owner("tide"),
            "Tide",
            AgentQuickComposerOrigin {
                marker: Rect::new(560.0, 180.0, 561.0, 181.0),
                protected: selection,
                context: String::new(),
            },
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        );
        let theme = Theme::dark();
        let mut text = ui::text_render::TextRenderer::new();
        let mut taffy = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        Widget::build_layout(&mut composer, &mut taffy, &theme, &mut nodes);
        composer.popover.place(&mut text, &theme);
        let rect = composer.popover.rect();
        assert!(rect.area() > 0.0);
        assert_eq!(
            rect.intersect(selection).area(),
            0.0,
            "{rect:?} covers {selection:?}"
        );
        assert!(rect.x0 >= selection.x1, "beside the selection: {rect:?}");

        // No room to its right: below it, still clear of it.
        let right = Rect::new(1100.0, 180.0, 1270.0, 240.0);
        composer.open(
            owner("tide"),
            "Tide",
            AgentQuickComposerOrigin {
                marker: Rect::new(1100.0, 180.0, 1101.0, 181.0),
                protected: right,
                context: String::new(),
            },
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        );
        Widget::build_layout(&mut composer, &mut taffy, &theme, &mut nodes);
        composer.popover.place(&mut text, &theme);
        let rect = composer.popover.rect();
        assert_eq!(
            rect.intersect(right).area(),
            0.0,
            "{rect:?} covers {right:?}"
        );
    }

    #[test]
    fn a_composer_opened_on_a_selection_carries_it_with_the_request() {
        use super::super::selection_agent::SelectionSeed;
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        let seed = SelectionSeed::new(
            "so.tilo.word-processor",
            "Writer",
            Some(
                ui::shared_selection::SharedSelection::of_text("which were late")
                    .at(tilo_actions::SelectionPlace::range("block-4", 12, 27)),
            ),
        );
        composer.open_about(
            tide.clone(),
            "Tide",
            AgentQuickComposerOrigin {
                marker: Rect::new(400.0, 200.0, 401.0, 201.0),
                protected: Rect::new(400.0, 200.0, 401.0, 201.0),
                context: String::new(),
            },
            Rect::new(0.0, 0.0, 800.0, 600.0),
            seed.clone(),
        );
        assert!(composer.about_selection());
        assert!(composer.selection, "a text answer may go at the selection");
        assert!(
            composer.view.context.get().contains("which were late"),
            "the composer shows what it is about: {}",
            composer.view.context.get()
        );
        edit(&mut composer, &mut doc, "Tighten this");
        let request = submit(&mut composer, &mut doc);
        assert_eq!(request.intent, ComposerIntent::Request);
        assert_eq!(request.text, "Tighten this");
        assert!(request.context.contains("which were late"));
        assert_eq!(request.about, Some(seed));

        open(&mut composer, tide);
        assert!(
            !composer.about_selection() && !composer.selection,
            "a composer opened at a marker is about nothing selected"
        );
    }

    #[test]
    fn every_composer_string_is_in_every_catalogue() {
        const KEYS: &[&str] = &[
            "agent-quick-title",
            "agent-quick-about-selection",
            "agent-quick-about-window",
            "agent-quick-accessible-name",
            "agent-quick-close",
            "agent-quick-insert",
            "agent-quick-save-note",
            "agent-quick-placeholder-idle",
            "agent-quick-placeholder-running",
            "agent-quick-editor-name",
            "agent-quick-send",
            "agent-quick-stop",
            "agent-quick-you-asked",
            "agent-quick-thinking",
            "agent-quick-steering",
            "agent-quick-stopping",
            "agent-quick-steer-saved",
            "agent-quick-steer-applied",
            "agent-quick-run-starting",
            "agent-quick-run-working",
            "agent-quick-run-done",
            "agent-quick-run-attention",
            "agent-quick-run-stopped",
            "agent-quick-outcome-unknown",
            "agent-quick-refused",
            "agent-quick-stop-refused",
        ];
        for (locale, source) in [
            ("en-US", include_str!("../../locales/en-US.ftl")),
            ("de-DE", include_str!("../../locales/de-DE.ftl")),
        ] {
            for key in KEYS {
                assert!(
                    source
                        .lines()
                        .any(|line| line.starts_with(&format!("{key} = "))),
                    "{key} is missing from {locale}"
                );
            }
            for retired in [
                "agent-quick-checking",
                "agent-quick-queue",
                "agent-quick-steer",
            ] {
                assert!(
                    !source
                        .lines()
                        .any(|line| line.starts_with(&format!("{retired} = "))),
                    "{retired} is retired from {locale}"
                );
            }
        }
    }
}
