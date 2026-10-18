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
    /// on one ([`SelectionComposer::open_about`]).
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

/// What the person answered the action a run paused at, from the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftAnswer {
    Confirm(tilo_desktop::activity::PausedDraft),
    Decline(tilo_desktop::activity::PausedDraft),
    /// Change it first, on the card in Task Center.
    Edit(tilo_desktop::activity::PausedDraft),
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
    /// Whether the followed run waits for the person on an action, which
    /// the composer then shows as the card does, with its answers.
    paused: LocalState<bool>,
    paused_chips: LocalState<String>,
    paused_action: LocalState<String>,
    paused_meta: LocalState<String>,
    paused_fields: LocalState<String>,
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
            paused: LocalState::new(false),
            paused_chips: LocalState::new(String::new()),
            paused_action: LocalState::new(String::new()),
            paused_meta: LocalState::new(String::new()),
            paused_fields: LocalState::new(String::new()),
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
    confirm: LocalState<bool>,
    edit: LocalState<bool>,
    decline: LocalState<bool>,
}

impl Default for Presses {
    fn default() -> Self {
        Self {
            submit: LocalState::new(false),
            stop: LocalState::new(false),
            close: LocalState::new(false),
            insert: LocalState::new(false),
            save: LocalState::new(false),
            confirm: LocalState::new(false),
            edit: LocalState::new(false),
            decline: LocalState::new(false),
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

pub(crate) struct SelectionComposer {
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
    /// What each followed run is doing now: its step, and the action it
    /// waits for the person on.
    glances: HashMap<ComposerOwner, tilo_desktop::activity::RunGlance>,
    /// The person's answer to a paused action, until the adapter takes it.
    draft_answer: Option<DraftAnswer>,
}

impl Default for SelectionComposer {
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
            glances: HashMap::new(),
            draft_answer: None,
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

impl SelectionComposer {
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

    /// The action the followed run waits on, as the confirmation card shows
    /// it: its status and reach, what it does and to what, its fields, and
    /// Confirm, Edit and Decline.
    fn paused(&self) -> Column {
        let view = &self.view;
        let confirm = self.presses.confirm.clone();
        let edit = self.presses.edit.clone();
        let decline = self.presses.decline.clone();
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(wrapped(
                Text::from_state(&view.paused_chips, Clone::clone)
                    .with_id("agent-quick-draft-status")
                    .with_role(ui::TextRole::Caption)
                    .with_live(ui::a11y::Politeness::Assertive),
            ))
            .push(wrapped(
                Text::from_state(&view.paused_action, Clone::clone)
                    .with_id("agent-quick-draft-action")
                    .with_role(ui::TextRole::Subheading),
            ))
            .push(wrapped(
                Text::from_state(&view.paused_meta, Clone::clone)
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
            ))
            .push(wrapped(
                Text::from_state(&view.paused_fields, Clone::clone)
                    .with_id("agent-quick-draft-fields"),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("agent-quick-draft-confirm", &t("agent-quick-draft-confirm"))
                            .role(ButtonRole::Primary)
                            .on_click(move || confirm.set(true)),
                    ))
                    .push(Box::new(
                        Button::new("agent-quick-draft-edit", &t("agent-quick-draft-edit"))
                            .on_click(move || edit.set(true)),
                    ))
                    .push(Box::new(
                        Button::new("agent-quick-draft-decline", &t("agent-quick-draft-decline"))
                            .on_click(move || decline.set(true)),
                    )),
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
                        Box::new(Spinner::for_text(ui::TextRole::Caption)),
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
            .push(Box::new(When::from_state(
                &view.paused,
                |shown| *shown,
                Box::new(Divider::horizontal()),
            )))
            .push(Box::new(When::from_state(
                &view.paused,
                |shown| *shown,
                Box::new(self.paused()),
            )))
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
        origin: &AgentQuickComposerOrigin,
        viewport: Rect,
        about: super::selection_agent::SelectionSeed,
    ) {
        self.open(
            owner,
            label,
            AgentQuickComposerOrigin {
                marker: origin.marker,
                protected: origin.protected,
                context: about.context(),
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

    /// What `owner`'s followed run is doing now, as the room's review tells
    /// it: the step it is at, and the action it waits for the person on.
    pub(crate) fn glance(
        &mut self,
        owner: &ComposerOwner,
        glance: Option<tilo_desktop::activity::RunGlance>,
    ) {
        let changed = match glance {
            Some(glance) => self.glances.insert(owner.clone(), glance.clone()) != Some(glance),
            None => self.glances.remove(owner).is_some(),
        };
        if changed {
            self.refresh();
        }
    }

    /// The action `owner`'s followed run waits for the person on, if any.
    fn paused_draft(&self) -> Option<&tilo_desktop::activity::PausedDraft> {
        let owner = self.owner.as_ref()?;
        let run = self.runs.get(owner)?;
        self.glances
            .get(owner)?
            .paused
            .as_ref()
            .filter(|draft| draft.task_id == run.task.task_id)
    }

    /// Show the composer again where it was when it follows task
    /// `task_id`'s run, so the action that run waits on is answered there.
    /// Whether it does.
    pub(crate) fn reveal_run(&mut self, task_id: &str) -> bool {
        let Some(owner) = self.owner.as_ref() else {
            return false;
        };
        if self
            .runs
            .get(owner)
            .is_none_or(|run| run.task.task_id != task_id)
        {
            return false;
        }
        if !self.is_open() {
            self.popover.set_open(true);
            self.focus_reset = true;
        }
        self.refresh();
        true
    }

    /// Whether the open composer shows an action its run waits on.
    pub(crate) fn shows_paused_any(&self) -> bool {
        self.paused_draft().is_some()
    }

    /// The person's answer to the paused action, for the adapter to send.
    pub(crate) fn take_draft_answer(&mut self) -> Option<DraftAnswer> {
        self.tick();
        self.draft_answer.take()
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
        let paused = self.paused_draft().cloned();
        let step = run
            .filter(|run| run.phase == RunPhase::Working)
            .and_then(|_| self.glances.get(owner))
            .and_then(|glance| glance.step.clone());
        let status = self.notice.clone().or_else(|| {
            run.map(|run| {
                if paused.is_some() {
                    return t("agent-quick-run-waiting");
                }
                if let Some(step) = &step {
                    return step.clone();
                }
                let key = match run.phase {
                    RunPhase::Starting => "agent-quick-run-starting",
                    _ => "agent-quick-run-working",
                };
                t_args(key, &[("task", run.task.label.clone().into())])
            })
        });
        self.view.has_status.set(status.is_some());
        self.view.waiting.set(
            (busy && self.notice_waits) || (self.notice.is_none() && running && paused.is_none()),
        );
        self.view.status.set(status.unwrap_or_default());
        self.view.paused.set(paused.is_some() && !busy);
        if let Some(draft) = &paused {
            self.view
                .paused_chips
                .set(format!("{} · {}", draft.status, draft.risk));
            self.view.paused_action.set(draft.action.clone());
            self.view.paused_meta.set(draft.meta.clone());
            self.view.paused_fields.set(
                draft
                    .fields
                    .iter()
                    .map(|(label, value)| format!("{label}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
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
        let confirm = take(&self.presses.confirm);
        let edit = take(&self.presses.edit);
        let decline = take(&self.presses.decline);
        if let Some(draft) = self.paused_draft().cloned() {
            self.draft_answer = if confirm {
                Some(DraftAnswer::Confirm(draft))
            } else if decline {
                Some(DraftAnswer::Decline(draft))
            } else if edit {
                Some(DraftAnswer::Edit(draft))
            } else {
                self.draft_answer.take()
            };
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

impl Decorator for SelectionComposer {
    type Inner = Popover<Container>;
    fn inner(&self) -> &Self::Inner {
        &self.popover
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.popover
    }
    fn name(&self) -> &'static str {
        "SelectionComposer"
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

    fn open(composer: &mut SelectionComposer, owner: ComposerOwner) {
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

    fn edit(composer: &mut SelectionComposer, doc: &mut automerge::AutoCommit, text: &str) {
        Widget::sync(composer, doc, &automerge::ROOT);
        assert!(action(
            composer,
            "agent-quick-editor",
            &ui::a11y::Action::SetValue(text.into()),
            doc
        ));
        Widget::sync(composer, doc, &automerge::ROOT);
    }

    fn press(composer: &mut SelectionComposer, doc: &mut automerge::AutoCommit, id: &str) {
        Widget::sync(composer, doc, &automerge::ROOT);
        assert!(
            action(composer, id, &ui::a11y::Action::Activate, doc),
            "{id} is shown and enabled"
        );
    }

    fn submit(
        composer: &mut SelectionComposer,
        doc: &mut automerge::AutoCommit,
    ) -> ComposerRequest {
        press(composer, doc, "agent-quick-send");
        composer
            .take_request()
            .expect("the enabled ordinary button emits one intention")
    }

    fn shown(composer: &mut SelectionComposer, id: &str) -> bool {
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
        let mut composer = SelectionComposer::default();
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

    /// While the followed run waits on an action, the composer says so
    /// instead of "working", shows the action as the card does, and its
    /// Confirm, Edit and Decline answer that exact draft; while it works,
    /// it says the step the run is at.
    #[test]
    fn a_run_waiting_on_the_person_shows_its_draft_and_takes_the_answer() {
        let mut composer = SelectionComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        composer.project(&tide, Some(working("task:rename", RunPhase::Working)));
        composer.glance(
            &tide,
            Some(tilo_desktop::activity::RunGlance {
                step: Some("Reading Files…".into()),
                paused: None,
            }),
        );
        assert_eq!(composer.view.status.get(), "Reading Files…");
        assert!(
            composer.view.waiting.get(),
            "a working run shows its spinner"
        );
        assert!(!shown(&mut composer, "agent-quick-draft-confirm"));

        let draft = tilo_desktop::activity::PausedDraft {
            room: "room".into(),
            task_id: "task:rename".into(),
            run_epoch: 3,
            confirmation_id: "c1".into(),
            status: "Waiting for your confirmation".into(),
            risk: "Changes your work".into(),
            action: "Rename an entry here".into(),
            meta: "scribe asks first · Folder".into(),
            fields: vec![
                ("Entry".into(), "doc (3).txt".into()),
                ("Name".into(), "Meeting notes.txt".into()),
            ],
        };
        composer.glance(
            &tide,
            Some(tilo_desktop::activity::RunGlance {
                step: Some("Editing Files…".into()),
                paused: Some(draft.clone()),
            }),
        );
        assert_eq!(composer.view.status.get(), t("agent-quick-run-waiting"));
        assert!(
            !composer.view.waiting.get(),
            "waiting on the person, not working"
        );
        assert!(composer.shows_paused_any());
        assert_eq!(composer.view.paused_action.get(), "Rename an entry here");
        assert!(
            composer
                .view
                .paused_fields
                .get()
                .contains("Name: Meeting notes.txt")
        );
        assert!(
            composer
                .view
                .paused_chips
                .get()
                .contains("Changes your work")
        );

        press(&mut composer, &mut doc, "agent-quick-draft-confirm");
        assert_eq!(
            composer.take_draft_answer(),
            Some(DraftAnswer::Confirm(draft.clone()))
        );
        assert_eq!(
            composer.take_draft_answer(),
            None,
            "an answer is taken once"
        );
        press(&mut composer, &mut doc, "agent-quick-draft-edit");
        assert_eq!(
            composer.take_draft_answer(),
            Some(DraftAnswer::Edit(draft.clone()))
        );
        press(&mut composer, &mut doc, "agent-quick-draft-decline");
        assert_eq!(
            composer.take_draft_answer(),
            Some(DraftAnswer::Decline(draft))
        );
    }

    #[test]
    fn work_runs_as_a_task_the_same_field_steers_and_stop_appears_while_it_runs() {
        let mut composer = SelectionComposer::default();
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
        let mut composer = SelectionComposer::default();
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
        let mut composer = SelectionComposer::default();
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
        let mut composer = SelectionComposer::default();
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
        let mut composer = SelectionComposer::default();
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
        let mut composer = SelectionComposer::default();
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

    /// The app said where the selected paragraph is drawn, in a window as
    /// wide as the screen and reaching its bottom: no room beside it or
    /// below, so the composer opens above the paragraph and covers none of
    /// it.
    #[test]
    fn the_composer_opens_above_a_selected_paragraph_with_no_room_beside_or_below() {
        let mut composer = SelectionComposer::default();
        let paragraph = Rect::new(8.0, 470.0, 1272.0, 790.0);
        composer.open(
            owner("tide"),
            "Tide",
            AgentQuickComposerOrigin {
                marker: paragraph,
                protected: paragraph,
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
            rect.intersect(paragraph).area(),
            0.0,
            "{rect:?} covers {paragraph:?}"
        );
        assert!(rect.y1 <= paragraph.y0, "above the paragraph: {rect:?}");
    }

    #[test]
    fn a_composer_opened_on_a_selection_carries_it_with_the_request() {
        use super::super::selection_agent::SelectionSeed;
        let mut composer = SelectionComposer::default();
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
            &AgentQuickComposerOrigin {
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
            "agent-quick-run-waiting",
            "agent-quick-draft-confirm",
            "agent-quick-draft-edit",
            "agent-quick-draft-decline",
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

/// One chrome host for the two explicit entry points. A painted marker keeps
/// its exact task/run controls and private conversation handoff; asking about
/// a selection uses the answer-and-work conversation.
#[derive(Default)]
pub(crate) struct AgentQuickComposer {
    pub(crate) marker: marker::AgentQuickComposer,
    pub(crate) selection_composer: SelectionComposer,
    selection_mode: bool,
}

impl AgentQuickComposer {
    pub(crate) fn followed(&self) -> Option<&ComposerRun> {
        self.selection_mode
            .then(|| self.selection_composer.followed())
            .flatten()
    }

    pub(crate) fn glance(
        &mut self,
        owner: &ComposerOwner,
        glance: Option<tilo_desktop::activity::RunGlance>,
    ) {
        if self.selection_mode {
            self.selection_composer.glance(owner, glance);
        }
    }

    pub(crate) fn reveal_run(&mut self, task_id: &str) -> bool {
        self.selection_mode && self.selection_composer.reveal_run(task_id)
    }

    pub(crate) fn shows_paused_any(&self) -> bool {
        self.selection_mode && self.selection_composer.shows_paused_any()
    }

    pub(crate) fn take_draft_answer(&mut self) -> Option<DraftAnswer> {
        if self.selection_mode {
            self.selection_composer.take_draft_answer()
        } else {
            None
        }
    }

    pub(crate) fn open(
        &mut self,
        owner: ComposerOwner,
        label: &str,
        origin: AgentQuickComposerOrigin,
        viewport: Rect,
    ) {
        self.dismiss();
        self.selection_mode = false;
        self.selection_composer
            .open(owner.clone(), label, origin.clone(), viewport);
        self.selection_composer.dismiss();
        self.marker.open(owner, label, origin, viewport);
    }

    pub(crate) fn open_about(
        &mut self,
        owner: ComposerOwner,
        label: &str,
        origin: &AgentQuickComposerOrigin,
        viewport: Rect,
        about: super::selection_agent::SelectionSeed,
    ) {
        self.dismiss();
        self.selection_mode = true;
        self.selection_composer
            .open_about(owner, label, origin, viewport, about);
    }

    pub(crate) fn owner(&self) -> Option<&ComposerOwner> {
        if self.selection_mode {
            self.selection_composer.owner()
        } else {
            self.marker.owner()
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        if self.selection_mode {
            self.selection_composer.is_open()
        } else {
            self.marker.is_open()
        }
    }

    pub(crate) fn dismiss(&mut self) {
        self.marker.dismiss();
        self.selection_composer.dismiss();
    }

    pub(crate) fn about_selection(&self) -> bool {
        self.selection_mode && self.selection_composer.about_selection()
    }

    pub(crate) fn bind_marker_task(&mut self, task: ComposerTask) {
        self.marker.bind_marker_task(task);
    }

    pub(crate) fn bind_painted_task(
        &mut self,
        task: ComposerTask,
        app: String,
        interaction: tilo_actions::SemanticInteraction,
    ) {
        self.marker.bind_painted_task(task, app, interaction);
    }

    pub(crate) fn marker_task(&self) -> Option<&ComposerTask> {
        if self.selection_mode {
            None
        } else {
            self.marker.marker_task()
        }
    }

    pub(crate) fn marker_origin(&self) -> Option<&(String, tilo_actions::SemanticInteraction)> {
        if self.selection_mode {
            None
        } else {
            self.marker.marker_origin()
        }
    }

    pub(crate) fn follow_anchor(&mut self, marker: Rect, protected: Rect) {
        if !self.selection_mode {
            self.marker.follow_anchor(marker, protected);
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn set_about_selection(&mut self, selection: bool) {
        self.marker.dismiss();
        self.selection_mode = true;
        self.selection_composer.popover.set_open(true);
        self.selection_composer.set_about_selection(selection);
    }

    #[cfg(debug_assertions)]
    pub(crate) fn stage_for_shots(
        &mut self,
        draft: &str,
        conversation: Vec<ComposerTurn>,
        run: Option<ComposerRun>,
    ) {
        self.selection_composer
            .stage_for_shots(draft, conversation, run);
    }

    #[cfg(any(test, debug_assertions))]
    pub(crate) fn project(
        &mut self,
        owner: &ComposerOwner,
        available: [bool; 3],
        tasks: Vec<ComposerTask>,
        can_stop: bool,
    ) {
        self.marker.project(owner, available, tasks, can_stop);
    }
}

impl Decorator for AgentQuickComposer {
    type Inner = dyn Widget;
    fn inner(&self) -> &Self::Inner {
        if self.selection_mode {
            &self.selection_composer
        } else {
            &self.marker
        }
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        if self.selection_mode {
            &mut self.selection_composer
        } else {
            &mut self.marker
        }
    }
    fn name(&self) -> &'static str {
        "AgentQuickComposer"
    }
}

#[cfg(test)]
mod entry_point_tests {
    use super::*;

    #[test]
    fn a_selection_and_a_painted_task_never_share_a_recipient_or_visible_host() {
        let mut composer = AgentQuickComposer::default();
        let owner = ComposerOwner {
            room: "room".into(),
            agent: "scribe".into(),
            person: "person".into(),
            identity_revision: 1,
        };
        let origin = AgentQuickComposerOrigin {
            marker: Rect::new(10.0, 10.0, 40.0, 40.0),
            protected: Rect::ZERO,
            context: String::new(),
        };
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let task = ComposerTask {
            task_id: "exact-task".into(),
            run_epoch: 3,
            revision: 8,
            label: "Exact task".into(),
        };
        composer.open(owner.clone(), "Scribe", origin.clone(), viewport);
        composer.bind_marker_task(task.clone());
        assert_eq!(composer.marker_task(), Some(&task));
        assert!(composer.marker.is_open());
        assert!(!composer.selection_composer.is_open());

        composer.open_about(
            owner.clone(),
            "Scribe",
            &origin,
            viewport,
            super::super::selection_agent::SelectionSeed::new("so.tilo.notes", "Notes", None),
        );
        assert!(composer.about_selection());
        assert!(composer.marker_task().is_none());
        assert!(!composer.marker.is_open());
        assert!(composer.selection_composer.is_open());

        composer.open(owner, "Scribe", origin, viewport);
        composer.bind_marker_task(task.clone());
        assert!(!composer.about_selection());
        assert_eq!(composer.marker_task(), Some(&task));
        assert!(!composer.selection_composer.is_open());
    }
}

pub(crate) mod marker {
    //! Private, marker-adjacent controls. The owner adapter, not marker geometry,
    //! resolves and admits the ordinary published Chat and Task actions.

    use std::collections::HashMap;

    use ui::widget::{
        Actions, Button, ButtonRole, Column, Container, Decorator, Dropdown, Popover, PopoverSide,
        Row, Segment, Text, TextInput, ToggleGroup, When, Widget,
    };
    use ui::{LocalState, Theme};
    use vello::kurbo::Rect;

    pub(crate) use super::{AgentQuickComposerOrigin, ComposerOwner, ComposerTask};
    use crate::shell::i18n::{t, t_args};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub(crate) enum ComposerIntent {
        #[default]
        Message,
        Queue,
        Steer,
    }

    /// Descriptive custody of a selected certified action session. This is a
    /// draft partition, never a capability or a substitute for broker admission.
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
        StopRequested(ComposerTask),
        Refused,
        Unknown,
    }

    /// Only the adapter's owner-correlated observation may produce this receipt.
    pub(crate) struct ComposerReceipt {
        pub owner: ComposerOwner,
        pub submission: u64,
        pub outcome: ComposerOutcome,
    }

    /// Private UI state stays out of public action arguments and descriptions.
    #[derive(Clone)]
    pub(crate) struct ComposerExpansion {
        pub owner: ComposerOwner,
        pub task: (String, u64),
        pub editor: ui::private_draft::Editor,
        context: u64,
    }

    /// Revisions are admission guards, not conversation identities. A draft
    /// belongs to one task/run even if its public revision advances.
    type TaskRun = (String, u64);
    type DraftIdentity = (String, String, String);
    type DraftKey = (DraftIdentity, ComposerIntent, Option<TaskRun>);
    type SubmissionKey = (ComposerOwner, ComposerIntent, Option<TaskRun>);

    fn draft_identity(owner: &ComposerOwner) -> DraftIdentity {
        (
            owner.person.clone(),
            owner.room.clone(),
            owner.agent.clone(),
        )
    }

    fn private_key(key: &SubmissionKey) -> DraftKey {
        (draft_identity(&key.0), key.1, key.2.clone())
    }

    #[derive(Clone)]
    struct Draft {
        text: String,
        revision: u64,
        editor: Option<String>,
        private_editor: Option<ui::private_draft::Editor>,
    }

    struct RetainedEditor {
        revision: u64,
        caret: String,
        private: Option<ui::private_draft::Editor>,
    }

    struct Submission {
        key: SubmissionKey,
        revision: u64,
        task: Option<ComposerTask>,
        stop: bool,
        applied: bool,
        busy: bool,
    }

    /// Tracks only real input to the retained shared editor. Focus alone is not
    /// editing; selection, a context-menu press and IME preedit are.
    #[derive(Default)]
    pub(crate) struct ComposerAnchor {
        pub(crate) frozen: bool,
        pointer: Option<vello::kurbo::Point>,
    }

    impl ComposerAnchor {
        pub(crate) fn observe(
            &mut self,
            editor: &dyn Widget,
            event: &winit::event::WindowEvent,
            ctx: &ui::widget::EventCtx<'_>,
        ) {
            use winit::event::{ElementState, Ime, TouchPhase, WindowEvent};
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    self.pointer = Some(ctx.logical(position));
                }
                WindowEvent::CursorLeft { .. } => self.pointer = None,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => {
                    self.frozen |= self.pointer.is_some_and(|p| editor.get_rect().contains(p));
                }
                WindowEvent::Touch(touch) if touch.phase == TouchPhase::Started => {
                    self.frozen |= editor.get_rect().contains(ctx.logical(touch.location));
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    use winit::keyboard::{Key, NamedKey};
                    let editing = matches!(
                        event.logical_key,
                        Key::Character(_)
                            | Key::Named(
                                NamedKey::ArrowLeft
                                    | NamedKey::ArrowRight
                                    | NamedKey::ArrowUp
                                    | NamedKey::ArrowDown
                                    | NamedKey::Home
                                    | NamedKey::End
                                    | NamedKey::PageUp
                                    | NamedKey::PageDown
                                    | NamedKey::Backspace
                                    | NamedKey::Delete
                                    | NamedKey::Enter
                                    | NamedKey::Insert
                                    | NamedKey::ContextMenu
                            )
                    ) || event.text.as_ref().is_some_and(|text| !text.is_empty());
                    self.frozen |= editor.is_focused() && editing;
                }
                WindowEvent::Ime(Ime::Preedit(..) | Ime::Commit(..)) => {
                    self.frozen |= editor.is_focused();
                }
                _ => {}
            }
        }
    }

    pub(crate) fn composer_editor<'a>(widget: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if widget.test_id() == Some(id) {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| composer_editor(child, id))
    }

    pub(crate) struct AgentQuickComposer {
        anchor: ComposerAnchor,
        popover: Popover<Container>,
        owner: Option<ComposerOwner>,
        intent: LocalState<ComposerIntent>,
        shown: ComposerIntent,
        shown_task: Option<TaskRun>,
        draft: LocalState<String>,
        title: LocalState<String>,
        context: LocalState<String>,
        status: LocalState<String>,
        enabled: LocalState<bool>,
        conversation_enabled: LocalState<bool>,
        stop_enabled: LocalState<bool>,
        submit: LocalState<bool>,
        stop: LocalState<bool>,
        conversation: LocalState<bool>,
        close: LocalState<bool>,
        expanded: LocalState<bool>,
        task_visible: LocalState<bool>,
        task_options: LocalState<Vec<(String, String)>>,
        selected: LocalState<String>,
        tasks: Vec<ComposerTask>,
        marker_binding: Option<ComposerTask>,
        marker_origin: Option<(String, tilo_actions::SemanticInteraction)>,
        conversation_authorized: bool,
        expansion_editor: Option<ui::private_draft::Editor>,
        input_context: u64,
        remembered_selection: HashMap<DraftIdentity, ComposerTask>,
        queued_conversations: HashMap<DraftIdentity, String>,
        available: [bool; 3],
        can_stop: bool,
        drafts: HashMap<DraftKey, Draft>,
        submissions: HashMap<u64, Submission>,
        next_submission: u64,
        pending_editor: Option<RetainedEditor>,
        focus_reset: bool,
    }

    impl Default for AgentQuickComposer {
        fn default() -> Self {
            let mut composer = Self {
                anchor: ComposerAnchor::default(),
                popover: Popover::with_content(Container::new()),
                owner: None,
                intent: LocalState::new(ComposerIntent::Message),
                shown: ComposerIntent::Message,
                shown_task: None,
                draft: LocalState::new(String::new()),
                title: LocalState::new(t("agent-quick-accessible-name")),
                context: LocalState::new(String::new()),
                status: LocalState::new(t("task-cursor-checking")),
                enabled: LocalState::new(false),
                conversation_enabled: LocalState::new(false),
                stop_enabled: LocalState::new(false),
                submit: LocalState::new(false),
                stop: LocalState::new(false),
                conversation: LocalState::new(false),
                close: LocalState::new(false),
                expanded: LocalState::new(false),
                task_visible: LocalState::new(false),
                task_options: LocalState::new(Vec::new()),
                selected: LocalState::new(String::new()),
                tasks: Vec::new(),
                marker_binding: None,
                marker_origin: None,
                conversation_authorized: false,
                expansion_editor: None,
                input_context: 0,
                remembered_selection: HashMap::new(),
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
            let enter = self.submit.clone();
            let expanded = self.expanded.clone();
            let modes = ToggleGroup::from_state(
                vec![
                    (
                        ComposerIntent::Message,
                        Segment::new(t("agent-quick-message")),
                    ),
                    (ComposerIntent::Queue, Segment::new(t("task-cursor-queue"))),
                    (ComposerIntent::Steer, Segment::new(t("task-cursor-steer"))),
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
                Button::new("agent-quick-stop", &t("agent-quick-stop"))
                    .with_icon(ui::icons::Icon::Square)
                    .role(ButtonRole::Secondary)
                    .enabled_from(&self.stop_enabled)
                    .on_click(move || stop.set(true)),
            )
            .secondary(
                Button::new("agent-quick-close", &t("agent-quick-close"))
                    .with_icon(ui::icons::Icon::X)
                    .on_click(move || close.set(true)),
            );
            // Keep one editor mounted while auxiliary controls are disclosed.
            // The outer anchor host paints nothing: only the standard field and
            // buttons occupy the desktop, not a rectangular owner panel.
            let bar = Row::new()
                .no_wrap()
                .push(Box::new(
                    Text::from_state(&self.title, Clone::clone).with_role(ui::TextRole::Caption),
                ))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_min_width(0.0)
                        .with_child(Box::new(
                            TextInput::from_state(&self.draft)
                                .with_id("agent-quick-editor")
                                .with_placeholder(t("agent-quick-placeholder"))
                                .with_accessible_name(t("agent-quick-editor-name"))
                                .on_submit(move |_| enter.set(true)),
                        )),
                ))
                .push(Box::new(
                    Button::icon_only(
                        "agent-quick-send",
                        ui::icons::Icon::Send,
                        &t("agent-quick-send"),
                    )
                    .enabled_from(&self.enabled)
                    .on_click(move || submit.set(true)),
                ))
                .push(Box::new(
                    Button::icon_only(
                        "agent-quick-open-conversation",
                        ui::icons::Icon::MessageSquare,
                        &t("agent-quick-open-conversation"),
                    )
                    .enabled_from(&self.conversation_enabled)
                    .on_click(move || conversation.set(true)),
                ))
                .push(Box::new(
                    Button::icon_only(
                        "agent-quick-more",
                        ui::icons::Icon::ChevronDown,
                        &t("agent-quick-more"),
                    )
                    .on_click(move || expanded.set(!expanded.get())),
                ));
            let body = Column::new()
                .with_no_padding()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(bar))
                .push(Box::new(When::from_state(
                    &self.expanded,
                    |expanded| *expanded,
                    Box::new(self.details(modes, picker, actions)),
                )));
            Popover::with_content(
                Container::new()
                    .with_no_padding()
                    .with_child(Box::new(body)),
            )
            .transparent_content()
            .side(PopoverSide::Right)
            .fallback_side(PopoverSide::Below)
            .confined_to_viewport()
            .with_label(t("agent-quick-accessible-name"))
        }

        fn details(&self, modes: ToggleGroup, picker: Dropdown, actions: Actions) -> Column {
            Column::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(
                    Text::from_state(&self.context, Clone::clone)
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                        .dimmed(),
                ))
                .push(Box::new(modes))
                .push(Box::new(When::from_state(
                    &self.task_visible,
                    |visible| *visible,
                    Box::new(picker),
                )))
                .push(Box::new(
                    Text::from_state(&self.status, Clone::clone)
                        .with_id("agent-quick-status")
                        .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                        .dimmed(),
                ))
                .push(Box::new(actions))
        }

        pub(crate) fn open(
            &mut self,
            owner: ComposerOwner,
            label: &str,
            origin: AgentQuickComposerOrigin,
            viewport: Rect,
        ) {
            // Dismissal already saved the editor before the popover hid it.
            // A closed popover exposes no children to snapshot.
            if self.is_open() {
                self.save_draft();
            }
            let remembered = self
                .remembered_selection
                .get(&draft_identity(&owner))
                .cloned();
            self.owner = Some(owner);
            self.title
                .set(t_args("agent-quick-title", &[("agent", label.into())]));
            self.context.set(origin.context);
            self.anchor = ComposerAnchor::default();
            self.available = [false; 3];
            self.tasks.clear();
            self.marker_binding = None;
            self.marker_origin = None;
            self.conversation_authorized = true;
            self.input_context = self.input_context.saturating_add(1);
            self.expansion_editor = None;
            self.selected.set(
                remembered
                    .as_ref()
                    .map_or_else(String::new, ComposerTask::key),
            );
            self.shown_task = if self.shown == ComposerIntent::Queue {
                None
            } else {
                remembered.map(|task| (task.task_id, task.run_epoch))
            };
            self.expanded.set(false);
            self.status.set(t("task-cursor-checking"));
            self.load_draft();
            self.popover.set_viewport(viewport);
            self.popover.set_anchor(origin.marker);
            self.popover.set_avoid(Some(origin.protected));
            self.popover.set_open(true);
            self.refresh_enabled();
            self.focus_reset = true;
        }

        /// Follow geometry only: never reload the editor or change its recipient.
        pub(crate) fn follow_anchor(&mut self, marker: Rect, protected: Rect) {
            if self.is_open() && !self.anchor.frozen {
                self.popover.set_anchor(marker);
                self.popover.set_avoid(Some(protected));
            }
        }

        /// Called immediately after opening from an admitted painted marker.
        /// This exact authorized run overrides any remembered task for the agent.
        pub(crate) fn bind_marker_task(&mut self, task: ComposerTask) {
            self.save_draft();
            self.selected.set(task.key());
            self.tasks = vec![task.clone()];
            self.marker_binding = Some(task);
            self.intent.set(ComposerIntent::Message);
            self.tick();
            self.load_draft();
            self.refresh_enabled();
        }

        pub(crate) fn bind_painted_task(
            &mut self,
            task: ComposerTask,
            app: String,
            interaction: tilo_actions::SemanticInteraction,
        ) {
            self.bind_marker_task(task);
            self.marker_origin = Some((app, interaction));
        }

        pub(crate) fn marker_origin(&self) -> Option<&(String, tilo_actions::SemanticInteraction)> {
            self.marker_origin.as_ref()
        }

        pub(crate) fn marker_task(&self) -> Option<&ComposerTask> {
            self.marker_binding.as_ref()
        }

        pub(crate) fn owner(&self) -> Option<&ComposerOwner> {
            self.owner.as_ref()
        }
        pub(crate) fn is_open(&self) -> bool {
            self.popover.is_open()
        }

        pub(crate) fn dismiss(&mut self) {
            if !self.is_open() {
                return;
            }
            self.input_context = self.input_context.saturating_add(1);
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
            self.tick();
            let tasks: Vec<_> = tasks
                .into_iter()
                .filter(|task| {
                    self.marker_binding.as_ref().is_none_or(|bound| {
                        task.task_id == bound.task_id && task.run_epoch == bound.run_epoch
                    })
                })
                .collect();
            if let Some(bound) = &self.marker_binding {
                let selected = tasks.first().unwrap_or(bound).key();
                if self.selected.get() != selected {
                    self.selected.set(selected);
                }
            }
            let selected = self.selected.get();
            if self.marker_binding.is_none()
                && !selected.is_empty()
                && !tasks.iter().any(|task| task.key() == selected)
            {
                self.selected.set(String::new());
            }
            let options = tasks
                .iter()
                .map(|task| (task.key(), task.label.clone()))
                .collect::<Vec<_>>();
            if self.task_options.get() != options {
                self.task_options.set(options);
            }
            self.tasks = tasks;
            self.available = available;
            self.can_stop = can_stop;
            self.refresh_enabled();
        }

        pub(crate) fn observe_ended(&self) {
            if self.conversation_authorized {
                self.status.set(t("agent-quick-run-ended"));
            }
        }

        #[cfg(debug_assertions)]
        pub(crate) fn stage_for_shots(&self, text: &str) {
            self.draft.set(text.to_owned());
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
            if stop {
                self.stop.set(false);
            }
            if submit {
                self.submit.set(false);
            }
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
                    key: (owner.clone(), self.shown, self.shown_task.clone()),
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
            if !self.conversation_authorized {
                return None;
            }
            self.marker_binding
                .clone()
                .or_else(|| self.chosen_task())
                .map(|task| (task.task_id, task.run_epoch))
                .or_else(|| {
                    (self.shown == ComposerIntent::Queue)
                        .then(|| {
                            self.owner
                                .as_ref()
                                .and_then(|owner| {
                                    self.queued_conversations.get(&draft_identity(owner))
                                })
                                .map(|task| (task.clone(), 0))
                        })
                        .flatten()
                })
        }

        pub(crate) fn take_conversation_request(&self) -> bool {
            let requested = self.conversation.get();
            if requested {
                self.conversation.set(false);
            }
            requested && self.is_open() && self.conversation_task().is_some()
        }

        pub(crate) fn revoke_conversation(&mut self) {
            self.conversation_authorized = false;
            self.status.set(t("agent-quick-source-unavailable"));
            self.expanded.set(true);
            self.refresh_enabled();
        }

        pub(crate) fn expansion_request(&mut self) -> Option<ComposerExpansion> {
            let editor = self
                .expansion_editor
                .take()
                .or_else(|| self.editor_snapshot())?;
            // Restore preedit cleared by the expansion button's blur before any
            // fallible provider lookup/staging. Failure leaves a usable editor.
            restore_preedit(&mut self.popover, &editor);
            Some(ComposerExpansion {
                owner: self.owner.clone()?,
                task: self.conversation_task()?,
                editor,
                context: self.input_context,
            })
        }

        fn editor_snapshot(&self) -> Option<ui::private_draft::Editor> {
            Some(
                composer_editor(&self.popover, "agent-quick-editor")?
                    .as_any()?
                    .downcast_ref::<TextInput>()?
                    .private_draft_editor(),
            )
        }

        pub(crate) fn observe_expansion(&mut self, request: &ComposerExpansion, accepted: bool) {
            if self.input_context != request.context
                || self.owner.as_ref() != Some(&request.owner)
                || self.conversation_task().as_ref() != Some(&request.task)
            {
                return;
            }
            let editor = &request.editor;
            let unchanged = self.editor_snapshot().as_ref() == Some(editor);
            if accepted && unchanged {
                self.draft.set(String::new());
                self.save_draft();
                self.dismiss();
            } else if !accepted {
                if unchanged {
                    restore_preedit(&mut self.popover, editor);
                }
                self.status.set(t("agent-quick-source-unavailable"));
                self.expanded.set(true);
            }
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
                ComposerOutcome::StopRequested(task) => {
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
            let current = self.owner.as_ref() == Some(&key.0)
                && self.shown == key.1
                && self.shown_task == key.2;
            if success && !stop {
                if current && self.draft.revision() == revision {
                    self.draft.set(String::new());
                }
                if let Some(draft) = self.drafts.get_mut(&private_key(&key))
                    && draft.revision == revision
                {
                    draft.text.clear();
                    draft.editor = None;
                    draft.private_editor = None;
                    draft.revision = draft.revision.saturating_add(1);
                }
            }
            if success && let ComposerOutcome::Queued { task_id } = &receipt.outcome {
                self.queued_conversations
                    .insert(draft_identity(&receipt.owner), task_id.clone());
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
                    ComposerOutcome::StopRequested(task) if success => {
                        let _ = task;
                        t("agent-quick-stop-requested")
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
            let busy = self.submissions.values().any(|submission| {
                submission.busy && self.owner.as_ref() == Some(&submission.key.0)
            });
            set_flag(
                &self.enabled,
                self.conversation_authorized
                    && !busy
                    && self.available.get(index).copied().unwrap_or(false)
                    && !self.draft.get().trim().is_empty()
                    && (self.shown == ComposerIntent::Queue || self.chosen_task().is_some()),
            );
            set_flag(
                &self.stop_enabled,
                self.conversation_authorized
                    && !busy
                    && self.can_stop
                    && self.chosen_task().is_some(),
            );
            set_flag(
                &self.conversation_enabled,
                self.conversation_task().is_some(),
            );
            set_flag(&self.task_visible, self.shown != ComposerIntent::Queue);
        }

        fn tick(&mut self) {
            let next = self.intent.get();
            let task = if next == ComposerIntent::Queue {
                None
            } else {
                self.chosen_task()
                    .or_else(|| self.marker_binding.clone())
                    .or_else(|| {
                        self.owner
                            .as_ref()
                            .and_then(|owner| self.remembered_selection.get(&draft_identity(owner)))
                            .filter(|task| task.key() == self.selected.get())
                            .cloned()
                    })
                    .map(|task| (task.task_id, task.run_epoch))
            };
            if let Some(selected) = self.chosen_task()
                && self.title.get() != selected.label
            {
                self.title.set(selected.label);
            }
            if next != self.shown || task != self.shown_task {
                self.save_draft();
                self.shown = next;
                self.shown_task = task;
                self.load_draft();
            }
            if self.close.get() {
                self.close.set(false);
                self.dismiss();
            }
            self.refresh_enabled();
        }

        fn save_draft(&mut self) {
            if let (Some(owner), Some(task)) = (self.owner.as_ref(), self.chosen_task()) {
                self.remembered_selection
                    .insert(draft_identity(owner), task);
            }
            // Opening and binding a painted task happen before the next sync.
            // Until then the mounted editor still belongs to the previous draft.
            let retained = self
                .pending_editor
                .as_ref()
                .filter(|editor| editor.revision == self.draft.revision());
            let editor = retained
                .map(|editor| editor.caret.clone())
                .or_else(|| editor_state(&self.popover));
            let private_editor = retained
                .and_then(|editor| editor.private.clone())
                .or_else(|| self.editor_snapshot());

            if let Some(owner) = &self.owner {
                self.drafts.insert(
                    (draft_identity(owner), self.shown, self.shown_task.clone()),
                    Draft {
                        text: self.draft.get(),
                        revision: self.draft.revision(),
                        editor,
                        private_editor,
                    },
                );
            }
        }

        fn load_draft(&mut self) {
            let Some(owner) = &self.owner else {
                return;
            };
            let key = (draft_identity(owner), self.shown, self.shown_task.clone());
            let saved = self.drafts.get(&key).cloned();

            self.draft.set(
                saved
                    .as_ref()
                    .map_or_else(String::new, |draft| draft.text.clone()),
            );
            let revision = self.draft.revision();
            self.pending_editor = None;
            if let Some(saved) = saved {
                for pending in self.submissions.values_mut().filter(|pending| {
                    pending.key.0 == *owner
                        && private_key(&pending.key) == key
                        && pending.revision == saved.revision
                }) {
                    pending.revision = revision;
                }
                self.pending_editor = saved.editor.map(|caret| RetainedEditor {
                    revision,
                    caret,
                    private: saved.private_editor,
                });
            }
        }
    }

    fn set_flag(state: &LocalState<bool>, value: bool) {
        if state.get() != value {
            state.set(value);
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

    fn restore_preedit(widget: &mut dyn Widget, editor: &ui::private_draft::Editor) -> bool {
        if widget.test_id() == Some("agent-quick-editor") {
            if let Some(input) = widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<TextInput>())
            {
                input.restore_private_preedit(&editor.preedit, editor.preedit_cursor);
                return true;
            }
            return false;
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| restore_preedit(child, editor))
    }

    fn configure_editor(widget: &mut dyn Widget, multiline: bool) {
        if widget.test_id() == Some("agent-quick-editor")
            && let Some(input) = widget
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<TextInput>())
        {
            input.set_multiline(multiline);
            return;
        }
        for child in widget.children_mut() {
            configure_editor(child, multiline);
        }
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
            configure_editor(&mut self.popover, self.shown != ComposerIntent::Message);
            self.popover.sync(state, path);
            self.task_options
                .with(|options| refresh_task_picker(&mut self.popover, options));
            if let Some(editor) = self.pending_editor.take()
                && editor.revision == self.draft.revision()
            {
                restore_editor(&mut self.popover, &editor.caret);
                if let Some(private) = editor.private {
                    restore_preedit(&mut self.popover, &private);
                }
            }
        }
        fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
            if self.owner.is_none() {
                return false;
            }
            self.tick();
            configure_editor(&mut self.popover, self.shown != ComposerIntent::Message);
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
            let width = theme.tokens.metrics.panel_width_sheet * theme.text_scale.max(1.0);
            let capacity = self.popover.available_content_width(theme);
            self.popover.content_mut().set_width(width.min(capacity));
            self.popover.build_layout(taffy, theme, nodes)
        }
        fn ime_cursor_rect(&self) -> Option<Rect> {
            if !self.is_open() {
                return None;
            }
            composer_editor(&self.popover, "agent-quick-editor")?.ime_cursor_rect()
        }

        fn take_focus_reset(&mut self) -> bool {
            let reset = std::mem::take(&mut self.focus_reset) | self.popover.take_focus_reset();
            if self.is_open() && self.pending_editor.is_none() {
                // App can blur controls during any structure rebuild, including
                // one that preserves the focus ordinal rather than resetting it.
                // Restore private state in the post-focus sync in either case.
                self.pending_editor = editor_state(&self.popover).map(|caret| RetainedEditor {
                    revision: self.draft.revision(),
                    caret,
                    private: self.editor_snapshot(),
                });
            }
            reset
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut ui::widget::EventCtx<'_>,
        ) {
            let open = self.is_open();
            if open && let Some(editor) = composer_editor(&self.popover, "agent-quick-editor") {
                self.anchor.observe(editor, event, ctx);
            }
            // Expansion can blur the retained editor before the adapter runs.
            // Keep preedit/selection before dispatch, not the post-blur snapshot.
            if open
                && composer_editor(&self.popover, "agent-quick-editor")
                    .is_some_and(Widget::is_focused)
            {
                self.expansion_editor = self.editor_snapshot();
            }
            self.popover.handle_event(event, ctx);
            if !self.conversation.get()
                && composer_editor(&self.popover, "agent-quick-editor")
                    .is_some_and(Widget::is_focused)
            {
                self.expansion_editor = self.editor_snapshot();
            }
            if open && !self.is_open() {
                self.input_context = self.input_context.saturating_add(1);
                self.save_draft();
                self.focus_reset = true;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn painted_anchor_follows_until_real_editor_input_and_stays_frozen() {
            use ui::synthetic_input as input;
            use winit::event::{ElementState, Ime, MouseButton, WindowEvent};
            for event in [
                input::char_key("x"),
                input::named_key(winit::keyboard::NamedKey::ArrowLeft),
                WindowEvent::Ime(Ime::Commit("x".into())),
                WindowEvent::Ime(Ime::Preedit("composing".into(), Some((0, 9)))),
                input::mouse_press(),
                input::mouse_input(ElementState::Pressed, MouseButton::Right),
                input::mouse_input(ElementState::Pressed, MouseButton::Middle),
            ] {
                fn focus(widget: &mut dyn Widget) -> bool {
                    if widget.test_id() == Some("agent-quick-editor") {
                        widget.set_focus(true);
                        return true;
                    }
                    widget.children_mut().into_iter().any(focus)
                }
                let mut composer = AgentQuickComposer::default();

                open(&mut composer, owner("tide"));
                let mut doc = automerge::AutoCommit::new();
                edit(&mut composer, &mut doc, "private draft");
                let mut frame = ui::headless::FrameHarness::for_root(
                    ui::State::new(),
                    composer,
                    (800, 600),
                    Theme::dark(),
                );
                let initial = frame.root::<AgentQuickComposer>().popover.rect();
                let moved = Rect::new(300.0, 220.0, 332.0, 252.0);
                {
                    let composer = frame.root_mut::<AgentQuickComposer>();
                    composer.follow_anchor(moved, moved);
                }
                frame.recompose().unwrap();
                let followed = frame.root::<AgentQuickComposer>().popover.rect();
                assert_ne!(
                    initial, followed,
                    "untouched editor follows its painted cursor"
                );
                // Focus by itself must not freeze the anchor. The event below
                // reaches the real retained TextInput through the popover.
                assert!(focus(frame.root_mut::<AgentQuickComposer>()));
                frame.recompose().unwrap();
                assert!(!frame.root::<AgentQuickComposer>().anchor.frozen);
                let editor =
                    composer_editor(frame.root::<AgentQuickComposer>(), "agent-quick-editor")
                        .unwrap()
                        .get_rect()
                        .center();
                frame.dispatch_to_root(&input::cursor_moved(editor.x, editor.y));
                frame.dispatch_to_root(&event);
                assert!(
                    frame.root::<AgentQuickComposer>().anchor.frozen,
                    "{event:?}"
                );
                let draft = frame.root::<AgentQuickComposer>().draft.get();
                if matches!(&event, WindowEvent::Ime(Ime::Preedit(..))) {
                    assert_eq!(
                        draft, "private draft",
                        "preedit freezes before a committed text change"
                    );
                }
                if matches!(&event, WindowEvent::KeyboardInput { event, .. } if event.text.as_deref() == Some("x"))
                {
                    assert!(draft.contains('x'), "the shared editor received the typing");
                    assert_eq!(draft.len(), "private draft".len() + 1);
                }
                let later = Rect::new(500.0, 400.0, 532.0, 432.0);
                {
                    let composer = frame.root_mut::<AgentQuickComposer>();
                    composer.follow_anchor(later, later);
                }
                frame.recompose().unwrap();
                assert_eq!(frame.root::<AgentQuickComposer>().popover.rect(), followed);
                assert_eq!(frame.root::<AgentQuickComposer>().draft.get(), draft);
                frame.dispatch_to_root(&input::cursor_moved(790.0, 590.0));
                frame.dispatch_to_root(&input::mouse_press());
                let composer = frame.root_mut::<AgentQuickComposer>();
                assert!(!composer.is_open(), "outside click only dismisses");
                composer.follow_anchor(later, later);
                assert_eq!(
                    composer.popover.rect(),
                    followed,
                    "closed composer never retargets"
                );
                assert!(
                    Widget::children(composer).is_empty(),
                    "closed editor has no hit targets"
                );
                assert!(
                    Widget::ime_cursor_rect(composer).is_none(),
                    "closed editor has no IME caret"
                );
                assert_eq!(composer.draft.get(), draft, "dismissal preserves the draft");
            }
        }

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
        fn reopening_and_binding_retains_private_ime_through_focus_reset_and_expansion() {
            fn editor(widget: &mut dyn Widget) -> Option<&mut TextInput> {
                if widget.test_id() == Some("agent-quick-editor") {
                    return widget.as_any_mut()?.downcast_mut::<TextInput>();
                }
                widget.children_mut().into_iter().find_map(editor)
            }

            let mut composer = AgentQuickComposer::default();
            let owner = owner("agent");
            let task = ComposerTask {
                task_id: "task:original".into(),
                run_epoch: 4,
                revision: 0,
                label: "Original task".into(),
            };
            open(&mut composer, owner.clone());
            composer.bind_marker_task(task.clone());
            let mut doc = automerge::AutoCommit::new();
            edit(&mut composer, &mut doc, "Original private draft");
            let input = editor(&mut composer).unwrap();
            input.set_focus(true);
            input.set_selection(12, 5);
            input.restore_private_preedit("候補", Some((0, 3)));
            let expected = input.private_draft_editor();
            composer.dismiss();
            // The shell wrapper also dismisses before reopening a closed marker.
            composer.dismiss();

            // The real marker adapter opens first and binds the exact run
            // before either operation gets a widget sync.
            open(&mut composer, owner);
            composer.bind_marker_task(task);
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(composer.editor_snapshot() == Some(expected.clone()));

            // App's structure rebuild resets focus after the first sync and
            // then syncs again before painting. Blur clears TextInput's IME.
            assert!(Widget::take_focus_reset(&mut composer));
            let input = editor(&mut composer).unwrap();
            input.set_focus(false);
            input.set_focus(true);
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(composer.editor_snapshot() == Some(expected.clone()));

            // Later layout rebuilds can also blur the editor without requesting
            // a new focus target, for example while capturing the current frame.
            assert!(!Widget::take_focus_reset(&mut composer));
            let input = editor(&mut composer).unwrap();
            input.set_focus(false);
            input.set_focus(true);
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(composer.editor_snapshot() == Some(expected.clone()));
            assert!(composer.expansion_request().unwrap().editor == expected);
        }

        #[test]
        fn painted_marker_overrides_remembered_work_and_never_follows_a_replacement_run() {
            let mut composer = AgentQuickComposer::default();
            let owner = owner("agent");
            let previous = ComposerTask {
                task_id: "task:previous".into(),
                run_epoch: 4,
                revision: 0,
                label: "Previously selected task".into(),
            };
            composer
                .remembered_selection
                .insert(draft_identity(&owner), previous.clone());
            open(&mut composer, owner.clone());
            let exact = ComposerTask {
                task_id: "task:painted".into(),
                run_epoch: 9,
                revision: 2,
                label: "Authorized task title".into(),
            };
            composer.bind_marker_task(exact.clone());
            assert_eq!(
                composer.conversation_task(),
                Some((exact.task_id.clone(), 9))
            );
            assert_eq!(composer.title.get(), exact.label);
            composer.project(
                &owner,
                [true; 3],
                vec![previous.clone(), exact.clone()],
                true,
            );
            let mut doc = automerge::AutoCommit::new();
            edit(&mut composer, &mut doc, "Original A's unsent private draft");
            assert!(action(
                &mut composer,
                "agent-quick-editor",
                &ui::a11y::Action::SetSelection {
                    anchor: 12,
                    focus: 3
                },
                &mut doc
            ));
            let editor = composer.editor_snapshot().unwrap();
            assert_eq!((editor.anchor, editor.caret), (12, 3));
            assert_eq!(composer.selected_task(), Some(exact.clone()));
            assert_eq!(composer.tasks.len(), 1);
            let replacement = ComposerTask {
                run_epoch: 10,
                ..exact
            };
            composer.project(&owner, [true; 3], vec![previous, replacement], true);
            assert!(composer.selected_task().is_none());
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(composer.editor_snapshot().as_ref() == Some(&editor));
            assert_eq!(
                composer.conversation_task(),
                Some(("task:painted".into(), 9))
            );
            assert!(!composer.enabled.get());
            assert!(!composer.stop_enabled.get());
            assert!(composer.conversation_enabled.get());
            composer.revoke_conversation();
            assert!(composer.conversation_task().is_none());
            assert!(!composer.conversation_enabled.get());
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
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(
                composer.draft.get().is_empty(),
                "unaddressed text is not a task's draft"
            );
            edit(&mut composer, &mut doc, "Exact direction");
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
        fn task_and_run_switches_keep_separate_private_editors() {
            let mut composer = AgentQuickComposer::default();
            let mut doc = automerge::AutoCommit::new();
            let owner = owner("tide");
            open(&mut composer, owner.clone());
            let first = ComposerTask {
                task_id: "first".into(),
                run_epoch: 3,
                revision: 8,
                label: "First task".into(),
            };
            let second = ComposerTask {
                task_id: "second".into(),
                run_epoch: first.run_epoch,
                revision: first.revision,
                label: "Second task".into(),
            };
            let rerun = ComposerTask {
                task_id: first.task_id.clone(),
                run_epoch: 4,
                revision: first.revision,
                label: first.label.clone(),
            };
            composer.project(
                &owner,
                [true, false, false],
                vec![first.clone(), second.clone(), rerun.clone()],
                false,
            );
            composer.selected.set(first.key());
            edit(&mut composer, &mut doc, "First private draft");
            let first_editor = editor_state(&composer).expect("shared editor state");
            composer.selected.set(second.key());
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(composer.draft.get().is_empty());
            edit(&mut composer, &mut doc, "Second private draft");
            composer.selected.set(rerun.key());
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(
                composer.draft.get().is_empty(),
                "a restarted run is not the previous conversation"
            );
            composer.selected.set(first.key());
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert_eq!(composer.draft.get(), "First private draft");
            assert_eq!(editor_state(&composer), Some(first_editor));
            assert_eq!(composer.conversation_task(), Some(("first".into(), 3)));
            assert!(action(
                &mut composer,
                "agent-quick-more",
                &ui::a11y::Action::Activate,
                &mut doc
            ));
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert_eq!(composer.draft.get(), "First private draft");
            composer.project(&owner, [false; 3], Vec::new(), false);
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert!(!composer.enabled.get());
            assert!(composer.conversation_task().is_none());
            composer.conversation.set(true);
            assert!(
                !composer.take_conversation_request(),
                "never fall back to an agent-global chat"
            );
        }

        #[test]
        fn certificate_refresh_preserves_principal_scoped_private_draft_without_authorizing_old_calls()
         {
            let mut composer = AgentQuickComposer::default();
            let mut doc = automerge::AutoCommit::new();
            let original = owner("scribe");
            let task = ComposerTask {
                task_id: "task".into(),
                run_epoch: 3,
                revision: 0,
                label: "Task".into(),
            };
            open(&mut composer, original.clone());
            composer.bind_marker_task(task.clone());
            edit(&mut composer, &mut doc, "Keep this private text");
            let old = composer.expansion_request().unwrap();
            let renewed = ComposerOwner {
                identity_revision: original.identity_revision + 1,
                ..original
            };
            open(&mut composer, renewed);
            composer.bind_marker_task(task);
            Widget::sync(&mut composer, &doc, &automerge::ROOT);
            assert_eq!(composer.draft.get(), "Keep this private text");
            composer.observe_expansion(&old, true);
            assert_eq!(composer.draft.get(), "Keep this private text");
            assert!(composer.is_open());
        }

        #[test]
        fn private_expansion_is_bound_to_the_original_input_context_and_composition() {
            let mut composer = AgentQuickComposer::default();
            let mut doc = automerge::AutoCommit::new();
            let owner = owner("scribe");
            let task = ComposerTask {
                task_id: "original".into(),
                run_epoch: 3,
                revision: 0,
                label: "Original".into(),
            };
            open(&mut composer, owner.clone());
            composer.bind_marker_task(task.clone());
            edit(&mut composer, &mut doc, "Unsent original text");
            assert!(action(
                &mut composer,
                "agent-quick-editor",
                &ui::a11y::Action::SetSelection {
                    anchor: 11,
                    focus: 3
                },
                &mut doc
            ));
            let mut editor = composer.editor_snapshot().unwrap();
            editor.preedit = "候補".into();
            editor.preedit_cursor = Some((0, 3));
            restore_preedit(&mut composer.popover, &editor);
            let request = composer.expansion_request().unwrap();
            assert!(request.editor == editor);
            composer.observe_expansion(&request, false);
            assert!(composer.editor_snapshot().as_ref() == Some(&editor));
            assert!(composer.is_open());
            // A newer IME selection is an edit even when committed text is unchanged.
            let mut newer = editor.clone();
            newer.preedit_cursor = Some((3, 6));
            restore_preedit(&mut composer.popover, &newer);
            composer.observe_expansion(&request, true);
            assert!(composer.is_open());
            assert!(composer.editor_snapshot().as_ref() == Some(&newer));
            composer.dismiss();
            open(&mut composer, owner);
            composer.bind_marker_task(task);
            edit(&mut composer, &mut doc, &editor.text);
            assert!(action(
                &mut composer,
                "agent-quick-editor",
                &ui::a11y::Action::SetSelection {
                    anchor: 11,
                    focus: 3
                },
                &mut doc
            ));
            restore_preedit(&mut composer.popover, &editor);
            composer.observe_expansion(&request, true);
            assert!(
                composer.is_open(),
                "a late receipt cannot clear a reopened editor"
            );
            assert!(composer.editor_snapshot().as_ref() == Some(&editor));
            let current = composer.expansion_request().unwrap();
            composer.observe_expansion(&current, true);
            assert!(!composer.is_open());
            assert!(composer.draft.get().is_empty());
            assert!(
                doc.get_heads().is_empty(),
                "no collaborative document contains the draft"
            );
        }

        #[test]
        fn unchanged_projection_and_idle_poll_do_not_publish_updates() {
            let mut composer = AgentQuickComposer::default();
            let owner = owner("tide");
            open(&mut composer, owner.clone());
            composer.project(&owner, [false; 3], Vec::new(), false);
            let revisions = |composer: &AgentQuickComposer| {
                [
                    composer.title.revision(),
                    composer.enabled.revision(),
                    composer.stop_enabled.revision(),
                    composer.conversation_enabled.revision(),
                    composer.task_visible.revision(),
                    composer.task_options.revision(),
                    composer.selected.revision(),
                    composer.submit.revision(),
                    composer.stop.revision(),
                    composer.conversation.revision(),
                ]
            };
            let before = revisions(&composer);
            composer.project(&owner, [false; 3], Vec::new(), false);
            assert!(composer.take_request().is_none());
            assert!(!composer.take_conversation_request());
            assert_eq!(
                revisions(&composer),
                before,
                "idle bars must not request another frame"
            );
        }

        fn named_rect(widget: &dyn Widget, id: &str) -> Option<Rect> {
            if widget.test_id() == Some(id) {
                return Some(widget.get_rect());
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| named_rect(child, id))
        }

        #[test]
        fn compact_frames_leave_desktop_transparent_in_both_themes_and_at_edges() {
            for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
                for reduced in [false, true] {
                    for edge in [false, true] {
                        let mut theme = theme.clone();
                        theme.reduce_motion = reduced;
                        let mut composer = AgentQuickComposer::default();
                        composer.open(
                            owner("tide"),
                            "Release notes",
                            AgentQuickComposerOrigin {
                                marker: if edge {
                                    Rect::new(760.0, 560.0, 792.0, 592.0)
                                } else {
                                    Rect::new(100.0, 100.0, 132.0, 132.0)
                                },
                                protected: Rect::ZERO,
                                context: String::new(),
                            },
                            Rect::new(0.0, 0.0, 800.0, 600.0),
                        );
                        let frame = ui::headless::FrameHarness::for_root(
                            ui::State::new(),
                            composer,
                            (800, 600),
                            theme,
                        );
                        let composer = frame.root::<AgentQuickComposer>();
                        let bar = composer.popover.rect();
                        assert!(
                            Rect::new(0.0, 0.0, 800.0, 600.0).contains_rect(bar),
                            "{name}: {bar:?}"
                        );
                        let editor =
                            named_rect(composer, "agent-quick-editor").expect("shared editor");
                        let send =
                            named_rect(composer, "agent-quick-send").expect("shared send control");
                        assert!(
                            bar.height() <= editor.height() + 1.0,
                            "one control row, no popup chrome: {bar:?}"
                        );
                        assert!(
                            send.x0 > editor.x1,
                            "there is a genuine transparent control gap"
                        );
                        let rgba = frame.render_rgba_transparent();
                        let pixel = ui::offscreen::OffscreenRenderer::sample(
                            &rgba,
                            800,
                            ui::num::u32_of_f64(f64::midpoint(editor.x1, send.x0)),
                            ui::num::u32_of_f64(editor.center().y),
                        );
                        assert_eq!(
                            pixel.3, 0,
                            "{name}: outer host must not paint a panel between controls"
                        );
                        assert!(
                            rgba.chunks_exact(4)
                                .any(|pixel| pixel.get(3).is_some_and(|alpha| *alpha > 0)),
                            "the production bar actually painted"
                        );
                        if let Ok(dir) = std::env::var("TILO_UI_SCREENSHOT_DIR") {
                            let dir = std::path::Path::new(&dir);
                            std::fs::create_dir_all(dir).expect("capture directory");
                            ui::png::write_rgba8(
                                dir.join(format!(
                                    "cursor-bar-{name}-reduced-{reduced}-edge-{edge}.png"
                                )),
                                800,
                                600,
                                &rgba,
                            )
                            .expect("production widget capture");
                        }
                    }
                }
            }
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
}
