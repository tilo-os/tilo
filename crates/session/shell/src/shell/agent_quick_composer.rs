//! The agent composer: one private field beside what the person is looking
//! at. They say what they want and send it; the agent decides whether that
//! is a question it answers here or work it carries out as a task. While the
//! work runs, the same field steers it and a Stop control appears.
//!
//! The owner adapter (`overlay_host::agent_actions`), not this widget,
//! resolves and admits the published agent and Tasks actions.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use tilo_reference::{ProgramStep, ResourceAt, StepChange, StepPreview};
use ui::widget::{
    BlockEditor, Button, ButtonRole, Chip, Column, Container, ControlSize, Decorator,
    DisclosureGroup, Divider, Dropdown, ListRow, Popover, PopoverSide, ProgramStepView, Scrollable,
    Slot, SlotHandle,
};
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
    /// The model the person picked for the run; the agent's own without.
    pub model: Option<runtime_collab::TaskRunModel>,
    /// What the person decided the request is, when they decided it in the
    /// composer's mode picker rather than leaving it to the agent: work to
    /// run, or something answered in place. `None` while the agent decides,
    /// and always on a follow-up, which the run already shapes.
    pub as_task: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ComposerOutcome {
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
    /// What the run has shown so far while it works.
    pub progress: RunProgress,
}

/// What a working run shows of itself: the text its current model turn
/// streams, and the steps it reported so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RunProgress {
    /// The current turn's text as it streams; empty before the first words.
    pub streamed: String,
    /// What the run reported so far, oldest first: the last few steps.
    pub steps: Vec<String>,
    /// The programs the run ran so far, oldest first, with their answers.
    pub programs: Vec<tilo_reference::ProgramStep>,
    /// The view the run answered with, once it did: the resource keeping it.
    pub answer: Option<tilo_reference::ResourceAt>,
}

/// What the person did with the work the composer shows, until the adapter takes it: each
/// goes through the ordinary door as the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkPress {
    /// Open a resource the work names, where it lives.
    Open(ResourceAt),
    /// Undo what the followed task changed in one resource.
    Undo { task: String, change: StepChange },
    /// Confirm or decline what a step proposes.
    Resolve { preview: StepPreview, approve: bool },
    /// Open the view the run answered with.
    OpenAnswer(ResourceAt),
}

/// The parts of the composer that show the work, replaced as it moves.
struct Work {
    /// What the person pressed in them, oldest first.
    presses: Rc<RefCell<Vec<WorkPress>>>,
    /// The subject as a chip.
    subject: SlotHandle,
    /// The steps and the answer row.
    steps: SlotHandle,
    /// The agent's reply, rendered.
    reply: SlotHandle,
    /// Which long answers the person unfolded.
    folds: DisclosureGroup,
    /// What the slots show now, so a refresh that changes nothing rebuilds nothing.
    shown: RefCell<WorkShown>,
}

#[derive(Default, PartialEq)]
struct WorkShown {
    task: String,
    programs: Vec<ProgramStep>,
    answer: Option<ResourceAt>,
    reply: String,
}

/// The slots [`Work`] fills, as they stand in the composer's tree.
struct WorkSlots {
    subject: Slot,
    steps: Slot,
    reply: Slot,
}

impl Work {
    fn new() -> (Self, WorkSlots) {
        let (subject, subject_handle) = Slot::empty();
        let (steps, steps_handle) = Slot::empty();
        let (reply, reply_handle) = Slot::empty();
        (
            Self {
                presses: Rc::default(),
                subject: subject_handle,
                steps: steps_handle,
                reply: reply_handle,
                folds: DisclosureGroup::new(),
                shown: RefCell::default(),
            },
            WorkSlots {
                subject,
                steps,
                reply,
            },
        )
    }

    /// Show `progress` of `task` and `reply`, rebuilding only what changed.
    fn show(&self, task: &str, progress: Option<&RunProgress>, reply: &str) {
        let mut shown = self.shown.borrow_mut();
        let (programs, answer) = progress.map_or((&[][..], None), |progress| {
            (progress.programs.as_slice(), progress.answer.as_ref())
        });
        if shown.task != task || shown.programs != programs || shown.answer.as_ref() != answer {
            task.clone_into(&mut shown.task);
            shown.programs = programs.to_vec();
            shown.answer = answer.cloned();
            self.steps.set(work_list(
                &self.presses,
                &self.folds,
                task,
                programs,
                answer,
            ));
        }
        if shown.reply != reply {
            reply.clone_into(&mut shown.reply);
            self.reply.set(reply_view(reply));
        }
    }

    /// The presses since the last take, oldest first.
    fn take(&self) -> Vec<WorkPress> {
        std::mem::take(&mut *self.presses.borrow_mut())
    }
}

/// Note `press`, for the adapter's next pump.
fn pressed(presses: &Rc<RefCell<Vec<WorkPress>>>) -> impl Fn(WorkPress) + 'static {
    let presses = Rc::clone(presses);
    move |press| {
        presses.borrow_mut().push(press);
        crate::shell::wake_compositor();
    }
}

/// The steps a run took, newest last, then the view it answered with as one row.
fn work_list(
    presses: &Rc<RefCell<Vec<WorkPress>>>,
    folds: &DisclosureGroup,
    task: &str,
    programs: &[ProgramStep],
    answer: Option<&ResourceAt>,
) -> Box<dyn Widget> {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Section);
    for (index, step) in programs.iter().enumerate() {
        let (open, undo, confirm, decline) = (
            pressed(presses),
            pressed(presses),
            pressed(presses),
            pressed(presses),
        );
        let task = task.to_owned();
        column = column.push(
            ProgramStepView::new(format!("agent-quick-step-{index}"), step.clone())
                .with_disclosure(folds)
                .on_open(move |resource| open(WorkPress::Open(resource.clone())))
                .on_undo(move |change| {
                    undo(WorkPress::Undo {
                        task: task.clone(),
                        change: change.clone(),
                    });
                })
                .on_confirm(move |preview| {
                    confirm(WorkPress::Resolve {
                        preview: preview.clone(),
                        approve: true,
                    });
                })
                .on_decline(move |preview| {
                    decline(WorkPress::Resolve {
                        preview: preview.clone(),
                        approve: false,
                    });
                })
                .into_widget(),
        );
    }
    if let Some(answer) = answer {
        let open = pressed(presses);
        let resource = answer.clone();
        column = column.push(Box::new(
            ListRow::new("agent-quick-answer-view", answer.reference.label.clone())
                .secondary(t("agent-quick-answer-view"))
                .icon(ui::icons::Icon::LayoutGrid)
                .trailing(Box::new(
                    Button::new("agent-quick-answer-open", &t("agent-quick-answer-open"))
                        .with_icon(ui::icons::Icon::ExternalLink)
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Small)
                        .on_click(move || open(WorkPress::OpenAnswer(resource.clone()))),
                )),
        ));
    }
    Box::new(column)
}

/// The agent's reply as it reads: its Markdown rendered, never shown as source.
fn reply_view(reply: &str) -> Box<dyn Widget> {
    if reply.trim().is_empty() {
        return ui::widget::optional(None);
    }
    Box::new(
        Container::new()
            .with_no_padding()
            .with_id("agent-quick-answer")
            .with_child(Box::new(
                BlockEditor::reading_markdown(reply)
                    .with_wrap_lines(true)
                    .with_compact_paragraph_spacing(true)
                    .with_block_handles(false)
                    .with_read_only(true),
            )),
    )
}

/// What the request is about, as a chip: the resource it names, which opens where it lives,
/// else the app or agent it is about.
fn subject_chip(
    presses: &Rc<RefCell<Vec<WorkPress>>>,
    label: &str,
    resource: Option<ResourceAt>,
) -> Box<dyn Widget> {
    let Some(resource) = resource else {
        return Box::new(Chip::new(label).with_id("agent-quick-subject"));
    };
    let open = pressed(presses);
    Box::new(
        Chip::reference(label, "agent-quick-subject")
            .on_click(move || open(WorkPress::Open(resource.clone()))),
    )
}

/// What the person does with a text answer about their selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AnswerUse {
    /// Put the answer where the selection is.
    Insert(String),
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
    /// What the request is about, by name: the resource, else the app.
    subject: LocalState<String>,
    /// The agent the person picked, among [`AgentQuickComposer::agents`].
    agent: LocalState<String>,
    picks_agent: LocalState<bool>,
    /// The model the person picked, among [`AgentQuickComposer::models`].
    model: LocalState<String>,
    picks_model: LocalState<bool>,
    /// Whether the person decided the request themselves, and as what.
    /// Empty while the agent decides, which is what it says until they do.
    mode: LocalState<String>,
    picks_mode: LocalState<bool>,
    context: LocalState<String>,
    asked: LocalState<String>,
    answer: LocalState<String>,
    /// What the answer line shows: the agent's last answer, or what its run
    /// streams while it works.
    shown: LocalState<String>,
    status: LocalState<String>,
    has_context: LocalState<bool>,
    has_asked: LocalState<bool>,
    has_answer: LocalState<bool>,
    can_use_answer: LocalState<bool>,
    has_status: LocalState<bool>,
    /// Whether the composer follows a run, which opens in Tasks.
    has_run: LocalState<bool>,
    /// Whether there are steps or an answer view to show.
    has_work: LocalState<bool>,
    /// Whether the work area shows anything: steps, an answer view or a reply.
    has_work_area: LocalState<bool>,
    /// Whether the agent is still at it: thinking, sending or working.
    waiting: LocalState<bool>,
    running: LocalState<bool>,
    enabled: LocalState<bool>,
    /// What keeps the chosen agent from being asked, said in place.
    blocked: LocalState<bool>,
    blocked_text: LocalState<String>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            title: LocalState::new(t("agent-quick-accessible-name")),
            subject: LocalState::new(String::new()),
            agent: LocalState::new(String::new()),
            picks_agent: LocalState::new(false),
            model: LocalState::new(String::new()),
            picks_model: LocalState::new(false),
            mode: LocalState::new(String::new()),
            picks_mode: LocalState::new(true),
            context: LocalState::new(String::new()),
            asked: LocalState::new(String::new()),
            answer: LocalState::new(String::new()),
            shown: LocalState::new(String::new()),
            status: LocalState::new(String::new()),
            has_context: LocalState::new(false),
            has_asked: LocalState::new(false),
            has_answer: LocalState::new(false),
            can_use_answer: LocalState::new(false),
            has_status: LocalState::new(false),
            has_run: LocalState::new(false),
            has_work: LocalState::new(false),
            has_work_area: LocalState::new(false),
            waiting: LocalState::new(false),
            running: LocalState::new(false),
            enabled: LocalState::new(false),
            blocked: LocalState::new(false),
            blocked_text: LocalState::new(String::new()),
        }
    }
}

/// What keeps a new agent made for a task from being asked here: no agent
/// runtime can be launched, no model is configured, or the model is on the
/// internet and this device is offline. Each has its fix, offered in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AskBlocker {
    NoRuntime,
    NoModel,
    Offline,
}

impl AskBlocker {
    /// The message that says it, as the composer and the canvas say it.
    pub(crate) const fn message_key(self) -> &'static str {
        match self {
            Self::NoRuntime => "agent-quick-blocked-no-runtime",
            Self::NoModel => "agent-quick-blocked-no-model",
            Self::Offline => "agent-quick-blocked-offline",
        }
    }

    fn text(self) -> String {
        t(self.message_key())
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
    open_run: LocalState<bool>,
    /// The fix for what keeps a new agent from being asked.
    fix: LocalState<bool>,
}

impl Default for Presses {
    fn default() -> Self {
        Self {
            submit: LocalState::new(false),
            stop: LocalState::new(false),
            close: LocalState::new(false),
            insert: LocalState::new(false),
            save: LocalState::new(false),
            open_run: LocalState::new(false),
            fix: LocalState::new(false),
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

/// Whether a picker shows the choices offered to it yet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerListing {
    Listed,
    Unlisted,
}

impl PickerListing {
    /// A picker is listed once shown, and only when there is a choice.
    fn for_choices(count: usize) -> Self {
        if count > 1 {
            Self::Unlisted
        } else {
            Self::Listed
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one composer: what it was opened about, its notice, its focus and its placement"
)]
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
    /// The agents the person may pick from, when opened about something.
    agents: Vec<String>,
    /// The agent the person picked instead, until the host takes it.
    agent_switch: Option<String>,
    /// Whether the picker lists [`Self::agents`] yet: it does once it is
    /// shown.
    agent_picker: PickerListing,
    /// The models a run may plan with, by how the picker names them; the
    /// first is the agent's own.
    models: Vec<(String, Option<runtime_collab::TaskRunModel>)>,
    /// Whether the model picker lists [`Self::models`] yet.
    model_picker: PickerListing,
    /// Whether the mode picker lists its two choices yet.
    mode_picker: PickerListing,
    /// The run the person asked to see in Tasks, until the host takes it.
    open_run: Option<String>,
    /// Whether it stands along the bottom of a narrow viewport.
    sheet: bool,
    /// The work it shows: steps, the answer view and the reply.
    work: Work,
    /// What each agent's last run showed, kept once it ended so its steps stay readable.
    worked: HashMap<ComposerOwner, (String, RunProgress)>,
    /// The answer views already opened, by resource id: each opens once.
    opened_answers: HashSet<String>,
    /// The answer view to open, until the host takes it.
    answer_to_open: Option<ResourceAt>,
    /// What the composer keeps clear of: where its subject is drawn.
    anchor: Option<Rect>,
    /// What keeps the chosen agent from being asked, while something does.
    blocker: Option<AskBlocker>,
    /// The fix the person pressed, until the host takes it.
    fix: Option<AskBlocker>,
}

impl Default for AgentQuickComposer {
    fn default() -> Self {
        let (work, slots) = Work::new();
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
            agents: Vec::new(),
            agent_switch: None,
            agent_picker: PickerListing::Listed,
            models: Vec::new(),
            model_picker: PickerListing::Listed,
            mode_picker: PickerListing::Unlisted,
            open_run: None,
            sheet: false,
            work,
            worked: HashMap::new(),
            opened_answers: HashSet::new(),
            answer_to_open: None,
            anchor: None,
            blocker: None,
            fix: None,
        };
        composer.popover = composer.controls(slots);
        composer
    }
}

/// How wide the composer is beside its subject: one width whatever it
/// shows, so an answer arriving grows it downwards only.
const COMPOSER_WIDTH: f32 = 360.0;
/// Below this viewport width the composer is a sheet along the bottom.
const SHEET_BELOW: f64 = 600.0;
/// The test id of the area showing the work: the steps, the answer view and the reply.
const WORK_AREA: &str = "agent-quick-work";
/// The tallest the work area starts, before the viewport it stands in is known.
const WORK_MAX: f32 = 360.0;
/// The share of the viewport's height the work area may take; past it the work scrolls,
/// following its end, so the field and its controls stay in view.
const WORK_SHARE: f64 = 0.5;

fn wrapped(text: Text) -> Box<dyn Widget> {
    Box::new(Container::new().with_no_padding().with_child(Box::new(
        text.with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph),
    )))
}

impl AgentQuickComposer {
    /// What the request is about, and Close.
    fn header(subject: Slot, close: LocalState<bool>) -> Row {
        Row::new()
            .no_wrap()
            .with_justify(taffy::JustifyContent::SpaceBetween)
            .push(Box::new(subject))
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

    /// Where the work stands: what the agent is doing, then the run in
    /// Tasks and Stop, on a line of their own away from Send.
    fn status(&self) -> (Row, Row) {
        let view = &self.view;
        let stop = self.presses.stop.clone();
        let open_run = self.presses.open_run.clone();
        let line = Row::new()
            .no_wrap()
            .push(Box::new(When::from_state(
                &view.waiting,
                |shown| *shown,
                Box::new(Spinner::for_text(ui::TextRole::Caption)),
            )))
            .push(Box::new(
                Text::from_state(&view.status, Clone::clone)
                    .with_id("agent-quick-status")
                    .with_role(ui::TextRole::Caption)
                    .with_live(ui::a11y::Politeness::Polite)
                    .dimmed(),
            ));
        let run = Row::new()
            .push(Box::new(When::from_state(
                &view.has_run,
                |shown| *shown,
                Box::new(
                    Button::new("agent-quick-open-run", &t("agent-quick-open-run"))
                        .with_icon(ui::icons::Icon::ExternalLink)
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Small)
                        .on_click(move || open_run.set(true)),
                ),
            )))
            .push(Box::new(When::from_state(
                &view.running,
                |shown| *shown,
                Box::new(
                    Button::new("agent-quick-stop", &t("agent-quick-stop"))
                        .with_icon(ui::icons::Icon::Square)
                        .control_size(ControlSize::Small)
                        .on_click(move || stop.set(true)),
                ),
            )));
        (line, run)
    }

    /// One quiet line of the defaults a request goes with (which agent,
    /// which model, and whether it decides, answers here or runs as a task),
    /// each changeable in place, and Send.
    fn footer(&self) -> Row {
        let submit = self.presses.submit.clone();
        let quiet = |picker: Dropdown| picker.control_size(ControlSize::Small);
        let defaults = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(When::from_state(
                &self.view.picks_agent,
                |picks| !*picks,
                Box::new(
                    Text::from_state(&self.view.title, Clone::clone)
                        .with_role(ui::TextRole::Caption)
                        .dimmed(),
                ),
            )))
            .push(Box::new(When::from_state(
                &self.view.picks_agent,
                |picks| *picks,
                Box::new(quiet(
                    Dropdown::from_state(Vec::new(), &self.view.agent)
                        .with_id(AGENT_PICKER)
                        .with_resolved_accessible_name(t("agent-quick-agent")),
                )),
            )))
            .push(Box::new(When::from_state(
                &self.view.picks_model,
                |picks| *picks,
                Box::new(quiet(
                    Dropdown::from_state(Vec::new(), &self.view.model)
                        .with_id(MODEL_PICKER)
                        .with_resolved_accessible_name(t("agent-quick-model")),
                )),
            )))
            .push(Box::new(When::from_state(
                &self.view.picks_mode,
                |picks| *picks,
                Box::new(quiet(
                    Dropdown::from_state(mode_choices(), &self.view.mode)
                        .with_id(MODE_PICKER)
                        .with_resolved_accessible_name(t("agent-quick-mode")),
                )),
            )));
        Row::new()
            .no_wrap()
            .with_justify(taffy::JustifyContent::SpaceBetween)
            .push(Box::new(defaults))
            .push(Box::new(
                Button::new("agent-quick-send", &t("agent-quick-send"))
                    .with_icon(ui::icons::Icon::Send)
                    .role(ButtonRole::Primary)
                    .enabled_from(&self.view.enabled)
                    .on_click(move || submit.set(true)),
            ))
    }

    /// What keeps the agent from being asked, and the button that fixes it.
    fn blocked_row(&self) -> Row {
        let fix = self.presses.fix.clone();
        Row::new()
            .no_wrap()
            .with_justify(taffy::JustifyContent::SpaceBetween)
            .push(wrapped(
                Text::from_state(&self.view.blocked_text, Clone::clone)
                    .with_id("agent-quick-blocked")
                    .with_role(ui::TextRole::Caption),
            ))
            .push(Box::new(
                Button::new("agent-quick-fix", &t("agent-quick-fix"))
                    .role(ButtonRole::Secondary)
                    .on_click(move || fix.set(true)),
            ))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the composer's one column of rows, top to bottom, reads as one layout"
    )]
    fn controls(&self, slots: WorkSlots) -> Popover<Container> {
        let view = &self.view;
        // The work: the steps, the answer view and the reply, following the end while the run
        // streams, and scrolled within the composer once it is taller than the room it has.
        let work = Container::new()
            .with_no_padding()
            .with_id(WORK_AREA)
            .with_child(Box::new(
                Scrollable::new_vertical()
                    .following_end()
                    .with_edge_fade()
                    .fit_content_height()
                    .with_max_height(WORK_MAX)
                    .with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_rhythm(ui::tokens::Rhythm::Section)
                            .push(Box::new(When::from_state(
                                &view.has_work,
                                |shown| *shown,
                                Box::new(slots.steps),
                            )))
                            .push(Box::new(When::from_state(
                                &view.has_answer,
                                |shown| *shown,
                                Box::new(slots.reply),
                            ))),
                    )),
            ));
        let (status_line, run_line) = self.status();
        let submit = self.presses.submit.clone();
        let editor = TextInput::from_state(&self.draft)
            .with_id("agent-quick-editor")
            .with_placeholder(t("agent-quick-placeholder-idle"))
            .with_accessible_name(t("agent-quick-editor-name"))
            .with_min_lines(2)
            .on_submit(move |_| submit.set(true));
        let body = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
            .push(Box::new(Self::header(
                slots.subject,
                self.presses.close.clone(),
            )))
            .push(Box::new(When::from_state(
                &view.has_context,
                |shown| *shown,
                wrapped(
                    Text::from_state(&view.context, Clone::clone)
                        .with_id("agent-quick-excerpt")
                        .with_role(ui::TextRole::Caption)
                        .dimmed(),
                ),
            )))
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
                &view.has_work_area,
                |shown| *shown,
                Box::new(work),
            )))
            .push(Box::new(When::from_state(
                &view.can_use_answer,
                |shown| *shown,
                Box::new(self.uses()),
            )))
            .push(Box::new(When::from_state(
                &view.has_status,
                |shown| *shown,
                Box::new(status_line),
            )))
            .push(Box::new(When::from_state(
                &view.has_run,
                |shown| *shown,
                Box::new(run_line),
            )))
            .push(Box::new(When::from_state(
                &view.blocked,
                |shown| *shown,
                Box::new(self.blocked_row()),
            )))
            .push(Box::new(editor))
            .push(Box::new(self.footer()));
        Popover::with_content(
            Container::new()
                .with_no_padding()
                .with_width(COMPOSER_WIDTH)
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
        self.agents.clear();
        self.agent_switch = None;
        self.view.picks_agent.set(false);
        self.view.agent.set(agent_label(&owner.agent));
        self.blocker = None;
        self.fix = None;
        self.view.mode.set(t("agent-quick-mode-decides"));
        self.offer_models(models_of());
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
        // Opened beside an agent's marker, it is about what the marker says.
        self.show_subject(None);
        self.place(
            viewport,
            origin.marker.union(origin.protected),
            origin.protected,
        );
        self.popover.set_open(true);
        self.refresh();
        self.focus_reset = true;
    }

    /// Stand beside `anchor` and never over `protected` in `viewport`; in a
    /// viewport too narrow for that, along its bottom edge as a sheet the
    /// width of the viewport, with even air either side.
    fn place(&mut self, viewport: Rect, anchor: Rect, protected: Rect) {
        self.popover.set_viewport(viewport);
        self.anchor = Some(protected);
        set_work_max(
            self.popover.content_mut(),
            ui::num::f32_of_f64(viewport.height() * WORK_SHARE),
        );
        if viewport.width() < SHEET_BELOW {
            // The popover keeps its edge margin and padding on both sides.
            let air = 2.0 * f64::from(ui::tokens::Spacing::BASE.s2)
                + 2.0 * f64::from(ui::tokens::Spacing::BASE.s4);
            self.popover
                .content_mut()
                .set_width(ui::num::f32_of_f64((viewport.width() - air).max(0.0)));
            self.popover.set_anchor(Rect::new(
                viewport.x0,
                viewport.y1,
                viewport.x1,
                viewport.y1,
            ));
            self.popover.set_avoid(None);
            self.popover.set_side(PopoverSide::Above);
            self.sheet = true;
        } else {
            self.popover.content_mut().set_width(COMPOSER_WIDTH);
            self.popover.set_side(PopoverSide::Right);
            // Beside everything the composer is about, never on top of it.
            self.popover.set_anchor(anchor);
            self.popover.set_avoid(Some(protected));
            self.sheet = false;
        }
    }

    /// Keep a sheet above the on-screen keyboard: `inset` is how far up from
    /// the viewport's bottom it reaches.
    pub(crate) fn rise_above_keyboard(&mut self, viewport: Rect, inset: f64) {
        if !self.sheet || !self.is_open() {
            return;
        }
        let bottom = viewport.y1 - inset.clamp(0.0, viewport.height());
        let floor = Rect::new(viewport.x0, bottom, viewport.x1, bottom);
        set_work_max(
            self.popover.content_mut(),
            ui::num::f32_of_f64((bottom - viewport.y0) * WORK_SHARE),
        );
        self.popover
            .set_viewport(Rect::new(viewport.x0, viewport.y0, viewport.x1, bottom));
        self.popover.set_anchor(floor);
    }

    /// Show what the request is about: `about`'s thing by name and its
    /// selected text as it reads, or with none, the agent and the line the
    /// composer was opened with. Never Markdown source.
    fn show_subject(&self, about: Option<&super::selection_agent::SelectionSeed>) {
        let (subject, excerpt, resource) = match about {
            Some(seed) => {
                let (label, excerpt) = seed.subject();
                (
                    label,
                    excerpt.map(|text| t_args("agent-quick-excerpt", &[("text", text.into())])),
                    seed.subject_resource(),
                )
            }
            None => (
                self.label.clone(),
                Some(super::selection_agent::plain_excerpt(&self.context))
                    .filter(|line| !line.trim().is_empty()),
                None,
            ),
        };
        self.work
            .subject
            .set(subject_chip(&self.work.presses, &subject, resource));
        self.view.subject.set(subject);
        self.view.has_context.set(excerpt.is_some());
        self.view.context.set(excerpt.unwrap_or_default());
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
        self.show_subject(Some(&about));
        self.about = Some(about);
        self.refresh();
    }

    /// Ask `text` at once, as if the person had typed it into the field
    /// and pressed Send: what they typed into a context menu that none of
    /// its entries answered. The adapter takes it with the next request.
    pub(crate) fn ask(&self, text: &str) {
        if !self.is_open() || text.trim().is_empty() {
            return;
        }
        self.draft.set(text.to_owned());
        self.presses.submit.set(true);
        self.refresh();
    }

    /// Start the field with `text`, what the person began typing over the
    /// selection, their typing going on after it.
    pub(crate) fn begin_typing(&mut self, text: &str) {
        if !self.is_open() || text.is_empty() {
            return;
        }
        self.draft.set(text.to_owned());
        self.pending_editor = None;
        self.refresh();
    }

    /// The run the person asked to see in Tasks, exactly once: its issue.
    pub(crate) fn take_open_run(&mut self) -> Option<String> {
        self.tick();
        self.open_run.take()
    }

    /// What the person did with the work shown since the last take, oldest first: opening a
    /// reference, undoing a change, deciding a proposal. Opening the answer view is not among
    /// them; [`Self::take_answer_to_open`] has it.
    pub(crate) fn take_work_presses(&mut self) -> Vec<WorkPress> {
        let mut presses = self.work.take();
        presses.retain(|press| match press {
            WorkPress::OpenAnswer(resource) => {
                self.answer_to_open = Some(resource.clone());
                false
            }
            _ => true,
        });
        presses
    }

    /// The answer view to open now, exactly once: the followed run's when it first answered
    /// with one, or the one the person asked to open again.
    pub(crate) fn take_answer_to_open(&mut self) -> Option<ResourceAt> {
        let presses = self.take_work_presses();
        self.work.presses.borrow_mut().splice(0..0, presses);
        self.answer_to_open.take()
    }

    /// Where the composer's subject is drawn, which an answer opens beside.
    pub(crate) fn anchor(&self) -> Option<Rect> {
        self.anchor
    }

    /// Say `notice` on the status line until something newer happens.
    pub(crate) fn tell(&mut self, notice: String) {
        self.notice = Some(notice);
        self.notice_waits = false;
        self.refresh();
    }

    /// The answer the composer shows now, as text: the agent's last answer,
    /// or what its run streams while it works.
    #[cfg(test)]
    pub(crate) fn shown_answer(&self) -> String {
        self.view.shown.get()
    }

    /// The selection the composer was opened about, if it was.
    pub(crate) fn about(&self) -> Option<&super::selection_agent::SelectionSeed> {
        self.about.as_ref()
    }

    /// Let the person pick which of `agents` to tell, in its header.
    pub(crate) fn offer_agents(&mut self, agents: Vec<String>) {
        self.view.picks_agent.set(agents.len() > 1);
        self.agent_picker = PickerListing::for_choices(agents.len());
        self.agents = agents;
        self.list_agents();
    }

    fn list_agents(&mut self) {
        if self.mode_picker == PickerListing::Unlisted
            && set_picker_options(&mut self.popover, MODE_PICKER, &mode_choices())
        {
            self.mode_picker = PickerListing::Listed;
        }
        if self.agent_picker == PickerListing::Unlisted
            && set_picker_options(
                &mut self.popover,
                AGENT_PICKER,
                &self
                    .agents
                    .iter()
                    .map(|agent| agent_label(agent))
                    .collect::<Vec<_>>(),
            )
        {
            self.agent_picker = PickerListing::Listed;
        }
        let models: Vec<String> = self.models.iter().map(|(label, _)| label.clone()).collect();
        if self.model_picker == PickerListing::Unlisted
            && set_picker_options(&mut self.popover, MODEL_PICKER, &models)
        {
            self.model_picker = PickerListing::Listed;
        }
    }

    /// Let the person pick the model the run plans with, among `models`;
    /// the model Settings chose for a short answer starts it when the picker
    /// offers that model, and the agent's own otherwise.
    fn offer_models(&mut self, models: Vec<(String, Option<runtime_collab::TaskRunModel>)>) {
        self.view.picks_model.set(models.len() > 1);
        self.model_picker = PickerListing::for_choices(models.len());
        // A composer that already has a model keeps it when no list arrives.
        if let Some(start) =
            short_answer_label().or_else(|| models.first().map(|(own, _)| own.clone()))
        {
            self.view.model.set(start);
        }
        self.models = models;
        self.list_agents();
    }

    /// The model the person picked: `None` for the agent's own.
    fn picked_model(&self) -> Option<runtime_collab::TaskRunModel> {
        let picked = self.view.model.get();
        self.models
            .iter()
            .find(|(label, _)| *label == picked)
            .and_then(|(_, model)| model.clone())
    }

    /// The agent the person picked instead of the composer's own. The host
    /// opens the composer again for it, about the same thing.
    pub(crate) fn take_agent_switch(&mut self) -> Option<String> {
        self.agent_switch.take()
    }

    /// Say in place what keeps the chosen agent from being asked, and offer
    /// its fix; `None` once nothing does. Sending waits until then.
    pub(crate) fn block(&mut self, blocker: Option<AskBlocker>) {
        if self.blocker != blocker {
            self.blocker = blocker;
            self.refresh();
        }
    }

    /// The fix the person pressed, exactly once.
    pub(crate) fn take_fix(&mut self) -> Option<AskBlocker> {
        self.tick();
        self.fix.take()
    }

    /// Whether the composer is open about a selection rather than beside an
    /// agent's marker, so it stays while that agent shows no marker.
    pub(crate) fn about_selection(&self) -> bool {
        self.is_open() && self.about.is_some()
    }

    /// Offer to insert a text answer at the selection the composer was
    /// opened about, or keep it as a note.
    #[cfg(test)]
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
        // What the run showed stays once it ended, and its answer view opens once.
        if run.phase != RunPhase::Starting {
            if let Some(answer) = &run.progress.answer
                && self.opened_answers.insert(answer.reference.id.clone())
                && self.owner.as_ref() == Some(owner)
            {
                self.answer_to_open = Some(answer.clone());
            }
            let kept = RunProgress {
                streamed: String::new(),
                ..run.progress.clone()
            };
            self.worked
                .insert(owner.clone(), (run.task.task_id.clone(), kept));
        }
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
        // The picker says what the person decided about a new request; a
        // follow-up to one they ruled on keeps that ruling, which the
        // adapter already carries with the run.
        let as_task = match intent {
            ComposerIntent::Request => mode_of(&self.view.mode.get()),
            ComposerIntent::Steer => None,
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
            conversation: self.conversation(&owner, as_task),
            about: self.about.clone().filter(|_| !stop),
            model: self.picked_model(),
            as_task,
        };
        // The ruling was for that request; the next one starts from the
        // agent's own decision until the person makes another.
        self.view.mode.set(t("agent-quick-mode-decides"));
        // A new request starts new work: the last run's steps make way for it.
        if intent == ComposerIntent::Request && !stop {
            self.worked.remove(&owner);
        }
        self.refresh();
        Some(request)
    }

    /// The turns before a request, with how the person decided it when they
    /// decided it themselves: a follow-up that overrules the last decision
    /// says so, in the place an agent reads its conversation.
    fn conversation(&self, owner: &ComposerOwner, as_task: Option<bool>) -> Vec<ComposerTurn> {
        let mut turns = self.conversations.get(owner).cloned().unwrap_or_default();
        let answered = turns.last().is_some_and(|turn| turn.agent);
        let decided = as_task.filter(|_| answered);
        if let Some(as_task) = decided {
            turns.push(ComposerTurn {
                agent: false,
                text: mode_line(as_task),
            });
        }
        turns
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
                    progress: RunProgress::default(),
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
        let run = self.runs.get(owner);
        let running = run.is_some_and(|run| run.phase.running());
        // While a run works, what it streams is its answer so far.
        let streamed = run
            .filter(|_| running)
            .map(|run| run.progress.streamed.as_str())
            .filter(|text| !text.trim().is_empty());
        let shown_answer = streamed
            .map(str::to_owned)
            .or_else(|| answer.map(|turn| turn.text.clone()))
            .unwrap_or_default();
        // The work the composer shows: the followed run's, else what the agent's last run
        // showed before it ended.
        let worked = run
            .map(|run| (run.task.task_id.as_str(), &run.progress))
            .or_else(|| {
                self.worked
                    .get(owner)
                    .map(|(task, progress)| (task.as_str(), progress))
            });
        let has_work = worked.is_some_and(|(_, progress)| {
            !progress.programs.is_empty() || progress.answer.is_some()
        });
        self.work.show(
            worked.map_or("", |(task, _)| task),
            worked.map(|(_, p)| p),
            &shown_answer,
        );
        self.view.has_work.set(has_work);
        self.view
            .has_work_area
            .set(has_work || !shown_answer.trim().is_empty());
        self.view.has_answer.set(!shown_answer.trim().is_empty());
        self.view.shown.set(shown_answer);
        // A run being started has no issue yet to open.
        self.view
            .has_run
            .set(run.is_some_and(|run| run.phase != RunPhase::Starting));
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
        self.view.enabled.set(
            !busy && !starting && self.blocker.is_none() && !self.draft.get().trim().is_empty(),
        );
        self.view.blocked.set(self.blocker.is_some());
        self.view
            .blocked_text
            .set(self.blocker.map(AskBlocker::text).unwrap_or_default());
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
        if take(&self.presses.open_run)
            && let Some(run) = self.followed()
        {
            self.open_run = Some(run.task.task_id.clone());
        }
        if take(&self.presses.fix) {
            self.fix = self.blocker;
        }
        let picked = self.view.agent.get();
        if let Some(agent) = self
            .agents
            .iter()
            .find(|agent| agent_label(agent) == picked)
            && self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.agent != *agent)
        {
            self.agent_switch = Some(agent.clone());
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

/// The test id of the composer's agent picker.
const AGENT_PICKER: &str = "agent-quick-agent";

/// How the agent picker names `agent`: a new agent made for the task, or a
/// collaborator by its id.
fn agent_label(agent: &str) -> String {
    if agent == crate::shell::task_agents::NEW_TASK_AGENT {
        t("agent-quick-new-agent")
    } else {
        agent.to_owned()
    }
}

/// The test id of the composer's model picker.
const MODEL_PICKER: &str = "agent-quick-model";

/// The test id of the composer's picker for what the request is.
const MODE_PICKER: &str = "agent-quick-mode";

/// What a request of the composer's is, as the person reads it there: the
/// agent's own decision (empty), an answer in place, or work to run.
fn mode_choices() -> Vec<String> {
    vec![
        t("agent-quick-mode-decides"),
        t("agent-quick-mode-answer"),
        t("agent-quick-mode-task"),
    ]
}

/// What the mode picker's value means: `None` while the agent decides.
fn mode_of(picked: &str) -> Option<bool> {
    let task = t("agent-quick-mode-task");
    let answer = t("agent-quick-mode-answer");
    if picked == task {
        Some(true)
    } else if picked == answer {
        Some(false)
    } else {
        None
    }
}

/// What a request the person ruled on says in its follow-ups, in their own
/// words in the place an agent reads its conversation.
fn mode_line(as_task: bool) -> String {
    if as_task {
        t("agent-quick-mode-task-line")
    } else {
        t("agent-quick-mode-answer-line")
    }
}

/// The models a run may plan with: the agent's own first, then those the
/// person's providers offer, as their record of them lists them.
fn models_of() -> Vec<(String, Option<runtime_collab::TaskRunModel>)> {
    let offered = crate::shell::settings_records::Holder::of(&crate::shell::state::local_user_id())
        .and_then(|records| {
            records
                .read::<agent_provider_config::OfferedModels, _>(|offered| offered.models.clone())
        })
        .unwrap_or_default();
    let mut models = vec![(t("agent-quick-model-own"), None)];
    models.extend(offered.into_iter().map(|offered| {
        (
            t_args(
                "agent-quick-model-of",
                &[
                    ("model", offered.label.into()),
                    ("provider", offered.provider.into()),
                ],
            ),
            Some(runtime_collab::TaskRunModel {
                profile: offered.profile,
                model: offered.model,
            }),
        )
    }));
    models
}

/// The label of the model Settings chose for a short answer, when that model
/// is one the picker offers: what the composer starts on, so asking for a
/// translation does not run on the model kept for tasks.
fn short_answer_label() -> Option<String> {
    let config = agent_provider_config::ProviderConfig::load();
    let (profile, model) =
        config.interaction_selection(agent_provider_config::Interaction::OneShot)?;
    let (profile, model) = (profile.id.as_str(), model.id.as_str());
    models_of()
        .into_iter()
        .find(|(_, chosen)| {
            chosen
                .as_ref()
                .is_some_and(|chosen| chosen.profile == profile && chosen.model == model)
        })
        .map(|(label, _)| label)
}

fn set_picker_options(widget: &mut dyn Widget, id: &str, options: &[String]) -> bool {
    if widget.test_id() == Some(id)
        && let Some(any) = widget.as_any_mut()
    {
        // A picker is shown only while there is a choice, so it is reached
        // through the `When` that shows it.
        let picker = match any.downcast_mut::<When>() {
            Some(when) => when
                .child_mut()
                .as_any_mut()
                .and_then(|child| child.downcast_mut::<Dropdown>()),
            None => any.downcast_mut::<Dropdown>(),
        };
        if let Some(picker) = picker {
            picker.set_options(options.to_vec());
            return true;
        }
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| set_picker_options(child, id, options))
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

/// Let the work area grow to `most` before it scrolls.
fn set_work_max(widget: &mut dyn Widget, most: f32) -> bool {
    fn scroller(widget: &mut dyn Widget, most: f32) -> bool {
        if let Some(scroll) = widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Scrollable>())
        {
            scroll.set_max_height(most);
            return true;
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| scroller(child, most))
    }
    // The area shows only while there is work, so it is reached through its `When`.
    if let Some(when) = widget
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<When>())
    {
        return set_work_max(when.child_mut(), most);
    }
    if widget.test_id() == Some(WORK_AREA) {
        return scroller(widget, most);
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| set_work_max(child, most))
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
        self.list_agents();
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
        self.list_agents();
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
            progress: RunProgress::default(),
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
    fn the_person_can_rule_that_an_instruction_is_work_or_an_answer() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        edit(
            &mut composer,
            &mut doc,
            "Rename every file to its invoice number",
        );
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        // Nothing is picked until the person picks it: the agent decides.
        let decided = submit(&mut composer, &mut doc);
        assert_eq!(decided.as_task, None);
        assert!(
            decided.conversation.is_empty(),
            "the agent's own decision says nothing extra"
        );
        composer.observe_receipt(ComposerReceipt {
            owner: tide.clone(),
            submission: decided.submission,
            outcome: ComposerOutcome::Answered("Which folder?".into()),
        });

        edit(&mut composer, &mut doc, "Sum up this section");
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert!(
            action(
                &mut composer,
                MODE_PICKER,
                &ui::a11y::Action::SetValue(t("agent-quick-mode-task")),
                &mut doc
            ),
            "the mode picker offers running it as a task"
        );
        let ruled = submit(&mut composer, &mut doc);
        assert_eq!(ruled.as_task, Some(true));
        assert_eq!(
            ruled.conversation.last(),
            Some(&ComposerTurn {
                agent: false,
                text: t("agent-quick-mode-task-line"),
            }),
            "a follow-up tells the agent what the person ruled"
        );
        // The composer waits for the agent's receipt of a request before it
        // sends another.
        composer.observe_receipt(ComposerReceipt {
            owner: tide,
            submission: ruled.submission,
            outcome: ComposerOutcome::Answered("Done.".into()),
        });

        // The ruling was for that request: the next one asks the agent again.
        edit(&mut composer, &mut doc, "And how long is the section?");
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        let next = submit(&mut composer, &mut doc);
        assert_eq!(next.as_task, None);
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

    /// The app said where the selected paragraph is drawn, in a window as
    /// wide as the screen and reaching its bottom: no room beside it or
    /// below, so the composer opens above the paragraph and covers none of
    /// it.
    #[test]
    fn the_composer_opens_above_a_selected_paragraph_with_no_room_beside_or_below() {
        let mut composer = AgentQuickComposer::default();
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
    fn the_person_picks_which_agent_to_tell_and_the_host_hears_it() {
        use super::super::selection_agent::SelectionSeed;
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let origin = AgentQuickComposerOrigin {
            marker: Rect::new(400.0, 200.0, 401.0, 201.0),
            protected: Rect::new(400.0, 200.0, 401.0, 201.0),
            context: String::new(),
        };
        let seed = SelectionSeed::new("so.tilo.notes", "Notes", None);
        composer.open_about(
            owner("tide"),
            "tide",
            &origin,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            seed.clone(),
        );
        composer.offer_agents(vec!["tide".into()]);
        assert!(
            !shown(&mut composer, AGENT_PICKER),
            "one agent: nothing to pick"
        );
        composer.offer_agents(vec!["tide".into(), "reed".into()]);
        assert!(shown(&mut composer, AGENT_PICKER));
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert!(action(
            &mut composer,
            AGENT_PICKER,
            &ui::a11y::Action::SetValue("reed".into()),
            &mut doc
        ));
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert_eq!(composer.take_agent_switch(), Some("reed".into()));
        assert_eq!(composer.take_agent_switch(), None, "heard once");
        composer.open_about(
            owner("reed"),
            "reed",
            &origin,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            seed.clone(),
        );
        composer.offer_agents(vec!["tide".into(), "reed".into()]);
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert_eq!(composer.take_agent_switch(), None);
        assert_eq!(composer.about(), Some(&seed));
    }

    #[test]
    #[serial_test::serial]
    fn the_model_picker_lists_what_the_persons_providers_offer() {
        use agent_provider_config::{OfferedModel, OfferedModels};
        let _home = tilo_test_env::TestHome::new().unwrap();
        let held =
            crate::shell::settings_records::Holder::of(&crate::shell::state::local_user_id())
                .unwrap();
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        open(&mut composer, owner("tide"));
        assert!(!shown(&mut composer, MODEL_PICKER), "only the agent's own");

        held.run(|records| {
            let record = records.shared::<OfferedModels>().unwrap();
            record
                .invoke(&OfferedModels::OFFER.with(vec![OfferedModel {
                    profile: "work".into(),
                    provider: "Work".into(),
                    model: "m-large".into(),
                    label: "Large".into(),
                }]))
                .unwrap();
        })
        .unwrap();
        open(&mut composer, owner("tide"));
        assert!(shown(&mut composer, MODEL_PICKER));
        let large = composer.models[1].0.clone();
        Widget::sync(&mut composer, &doc, &automerge::ROOT);
        assert!(action(
            &mut composer,
            MODEL_PICKER,
            &ui::a11y::Action::SetValue(large),
            &mut doc
        ));
        edit(&mut composer, &mut doc, "Tidy the notes");
        assert_eq!(
            submit(&mut composer, &mut doc).model,
            Some(runtime_collab::TaskRunModel {
                profile: "work".into(),
                model: "m-large".into(),
            })
        );
    }

    fn board_card() -> ResourceAt {
        ResourceAt {
            app: "so.tilo.lang.board".into(),
            record: None,
            reference: tilo_reference::ResourceRef::whole(
                "tl.so.tilo.lang.board.Card",
                "tl:so.tilo.lang.board:root/cards:c1",
                "Write the release notes",
            ),
        }
    }

    fn release_progress(answer: bool) -> RunProgress {
        RunProgress {
            streamed: "Three cards are **open**".into(),
            steps: Vec::new(),
            programs: vec![ProgramStep {
                program: "board.move(@board.Card:root/cards:c1, column: \"Review\")".into(),
                outcome: tilo_reference::StepOutcome::Asked,
                answer: vec![
                    "@board.Card:root/cards:c1 \"Write the release notes\" column: \"Review\""
                        .into(),
                ],
                refs: vec![board_card()],
                changes: vec![StepChange {
                    resource: board_card(),
                    action: "Moved to Review".into(),
                }],
                previews: vec![StepPreview {
                    invocation: ResourceAt {
                        app: "so.tilo.lang.chat".into(),
                        record: None,
                        reference: tilo_reference::ResourceRef::whole(
                            "tl.invocation",
                            "inv-7",
                            "Post",
                        ),
                    },
                    action: "Post in Release".into(),
                    summary: String::new(),
                }],
                planned: Vec::new(),
            }],
            answer: answer.then(|| ResourceAt {
                app: "so.tilo.lang.views".into(),
                record: None,
                reference: tilo_reference::ResourceRef::whole(
                    "tl.so.tilo.lang.views.Answer",
                    "tl:so.tilo.lang.views:a1",
                    "What's left for the release",
                ),
            }),
        }
    }

    /// A working run shows its steps, and what the person does with them reaches the host as
    /// presses: opening a reference, undoing a change for the run's task, deciding a proposal.
    /// The steps stay readable once the run ended, and its answer view opens exactly once.
    #[test]
    fn the_run_shows_its_steps_and_what_the_person_does_with_them_reaches_the_host() {
        let mut composer = AgentQuickComposer::default();
        let mut doc = automerge::AutoCommit::new();
        let tide = owner("tide");
        open(&mut composer, tide.clone());
        assert!(!composer.view.has_work.get(), "no run, no steps");
        let mut run = working("task:release", RunPhase::Working);
        run.progress = release_progress(false);
        composer.project(&tide, Some(run.clone()));
        assert!(composer.view.has_work.get());
        assert_eq!(composer.shown_answer(), "Three cards are **open**");
        assert!(
            shown(&mut composer, "agent-quick-answer"),
            "the reply is rendered"
        );
        assert!(
            composer.take_answer_to_open().is_none(),
            "no answer view yet"
        );

        press(
            &mut composer,
            &mut doc,
            "reference:tl:so.tilo.lang.board:root/cards:c1",
        );
        press(&mut composer, &mut doc, "agent-quick-step-0-change-0-undo");
        press(
            &mut composer,
            &mut doc,
            "agent-quick-step-0-preview-0-decline",
        );
        let presses = composer.take_work_presses();
        assert_eq!(
            presses,
            vec![
                WorkPress::Open(board_card()),
                WorkPress::Undo {
                    task: "task:release".into(),
                    change: run.progress.programs[0].changes[0].clone(),
                },
                WorkPress::Resolve {
                    preview: run.progress.programs[0].previews[0].clone(),
                    approve: false,
                },
            ]
        );
        assert!(
            composer.take_work_presses().is_empty(),
            "each is taken once"
        );

        // The run answers with a view and ends: the view opens once, the steps stay.
        run.progress = release_progress(true);
        composer.project(&tide, Some(run.clone()));
        let answer = run.progress.answer.clone().unwrap();
        assert_eq!(composer.take_answer_to_open(), Some(answer.clone()));
        run.phase = RunPhase::Finished(Some("Moved it.".into()));
        composer.project(&tide, Some(run.clone()));
        composer.project(&tide, Some(run));
        assert_eq!(composer.take_answer_to_open(), None, "opened once");
        assert!(composer.followed().is_none());
        assert!(composer.view.has_work.get(), "the ended run's steps stay");
        assert!(shown(&mut composer, "agent-quick-step-0-program"));
        // Opening it again is the person's to ask.
        press(&mut composer, &mut doc, "agent-quick-answer-open");
        assert_eq!(composer.take_answer_to_open(), Some(answer));

        // A new request makes way for new work.
        edit(&mut composer, &mut doc, "And what about the website?");
        let _ = submit(&mut composer, &mut doc);
        assert!(!composer.view.has_work.get());
    }

    #[test]
    fn every_composer_string_is_in_every_catalogue() {
        const KEYS: &[&str] = &[
            "agent-quick-title",
            "agent-quick-agent",
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
            "agent-quick-answer-view",
            "agent-quick-answer-open",
            "agent-quick-undo-refused",
            "agent-quick-resolve-refused",
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
