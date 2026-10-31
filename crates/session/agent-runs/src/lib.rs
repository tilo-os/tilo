//! The session's runs of what people ask its agents to do.
//!
//! A person asks an agent to do something — typed in the agent composer, or
//! over a window — and the session queues it as a run: an ask is an action of
//! [`wire::RESOURCE`], and the run it starts is a resource of its own
//! ([`wire::run`]). Whoever follows the run reads that one resource: what its
//! agent is doing, what it says as it says it, the steps it took, the answer
//! it gave, and where the run stands. Nothing here draws, and nothing here
//! reads an application's record.
//!
//! The run is fed two ways, and one run's facts come from both:
//!
//! - **The person**, through the door: [`RunsAction::Ask`] queues a run,
//!   [`RunAction::SendMessage`] steers it, [`RunAction::StopRun`] ends it.
//! - **The run channel**, through [`TrackerRuns`]: the broker delivers a run
//!   to its agent, hears what its agent reports (what it accepted, what it
//!   said, the messages it took in, how it ended), and shows what a model
//!   turn streams while it goes ([`TrackerRuns::presented`]). The session's
//!   broker calls [`AgentRuns::reported`] for every one of those reports.
//!
//! The agent records the parts of its work that are not a report — the
//! programs it ran ([`RunAction::RecordStep`]) and the view it answered with
//! ([`RunAction::RecordAnswer`]) — on the run's own resource.
//!
//! What the session feeds it and only the session knows: who sits here (the
//! asks are theirs), the room in front, which agents that room admits, and
//! the room's grants, which decide the run's child actions
//! ([`TrackerRuns::authorize_action`]). None of it is read from an action's
//! arguments.

mod effects;
mod service;
#[cfg(test)]
mod tests;
mod tracker;

use std::sync::Mutex;

use tilo_action_broker::services::{TrackerDirection, TrackerRun};
use tilo_actions::{DoorError, SelectionSubject};
use tilo_clock::unix_now_ms_i64;
use tilo_lock::MutexRecover as _;
use tilo_reference::{ProgramStep, ResourceAt};
use tilo_service::Wake;

pub use effects::{RunAction, RunsAction};
pub use service::Served;
pub use tilo_actions::remote::agent_runs as wire;

/// The kind of [`wire::RESOURCE`].
pub const RUNS_KIND: &str = "so.tilo.session.agent-runs";
/// The kind of one run's resource, [`wire::run`].
pub const RUN_KIND: &str = "so.tilo.session.agent-run";
/// What the runs are called wherever a person reads them.
pub const LABEL: &str = "Agent runs";

/// How many runs the session keeps: the last ones asked. What an older run
/// did stays in the records its steps changed.
const KEPT_RUNS: usize = 64;
/// How many of a run's own reports a follower is shown: the steps so far.
const SHOWN_STEPS: usize = 4;
/// How many of a run's programs a follower is shown: enough to follow a
/// task's work, bounded so a long task does not grow the follower without end.
const SHOWN_PROGRAMS: usize = 24;
/// How many of the messages sent to a run a follower is shown.
const SHOWN_DIRECTIONS: usize = 8;
/// How many messages a run keeps: its agent asks for the rest as it goes.
const KEPT_DIRECTIONS: usize = 64;
/// How many characters an ask's title keeps.
const TITLE_CHARS: usize = 120;
/// How many characters an ask's instruction keeps.
const INSTRUCTION_CHARS: usize = 32 * 1024;
/// How many characters a message to a run, or a report from it, keeps.
const MESSAGE_CHARS: usize = 16 * 1024;

/// One run, as the session keeps it.
struct Run {
    id: String,
    /// The room it was asked in: the room whose agents run it.
    room: String,
    title: String,
    instruction: String,
    /// The agent, by its bare id.
    agent: String,
    /// The person whose ask started it.
    starter: String,
    epoch: u64,
    phase: wire::Phase,
    at_ms: i64,
    quick: bool,
    model: Option<runtime_collab::TaskRunModel>,
    /// What it is about, when it was asked about a selection: where to act,
    /// granting nothing.
    subject: Option<SelectionSubject>,
    /// The messages sent to it, oldest first.
    directions: Vec<TrackerDirection>,
    /// What its agent said in it so far, oldest first, the last
    /// [`SHOWN_STEPS`] of them.
    said: Vec<String>,
    programs: Vec<ProgramStep>,
    answer: Option<ResourceAt>,
    /// The model turn it streams now, while it goes.
    live: Option<runtime_collab::TaskPresentation>,
}

impl Run {
    /// What a follower reads of it.
    fn reading(&self) -> wire::Run {
        let incorporated = self
            .directions
            .iter()
            .take_while(|direction| direction.incorporated)
            .count();
        let shown = self.directions.len().saturating_sub(SHOWN_DIRECTIONS);
        wire::Run {
            id: self.id.clone(),
            title: self.title.clone(),
            instruction: self.instruction.clone(),
            agent: self.agent.clone(),
            starter: self.starter.clone(),
            epoch: self.epoch,
            phase: self.phase,
            at_ms: self.at_ms,
            quick: self.quick,
            model: self.model.as_ref().map(|model| wire::RunModel {
                profile: model.profile.clone(),
                model: model.model.clone(),
            }),
            directions: self
                .directions
                .get(shown..)
                .unwrap_or_default()
                .iter()
                .map(|direction| wire::Direction {
                    author: direction.author.clone(),
                    text: direction.text.clone(),
                    at_ms: direction.at_ms,
                    incorporated: direction.incorporated,
                })
                .collect(),
            messages: u64::try_from(self.directions.len()).unwrap_or(u64::MAX),
            incorporated: u64::try_from(incorporated).unwrap_or(u64::MAX),
            said: self.said.last().cloned(),
            steps: self.said.clone(),
            programs: self
                .programs
                .iter()
                .filter_map(|program| serde_json::to_value(program).ok())
                .collect(),
            answer: self
                .answer
                .as_ref()
                .and_then(|answer| serde_json::to_value(answer).ok()),
            live: self.live.as_ref().map(|live| wire::Live {
                run: live.run.clone(),
                sequence: live.sequence,
                text: live.text.clone(),
                detail: live.detail.clone(),
                reasoning: live.reasoning.clone(),
                end: live.finish.map(|finish| match finish {
                    runtime_collab::TaskPresentationFinish::Complete => wire::TurnEnd::Complete,
                    runtime_collab::TaskPresentationFinish::Truncated => wire::TurnEnd::Truncated,
                    runtime_collab::TaskPresentationFinish::Refused => wire::TurnEnd::Refused,
                    runtime_collab::TaskPresentationFinish::Cancelled => wire::TurnEnd::Cancelled,
                    runtime_collab::TaskPresentationFinish::Failed => wire::TurnEnd::Failed,
                }),
            }),
        }
    }

    /// Take in what its agent said, keeping the last few things it said.
    fn note_said(&mut self, said: &str) {
        let Some(said) = message_text(said) else {
            return;
        };
        self.said.push(said);
        let over = self.said.len().saturating_sub(SHOWN_STEPS);
        self.said.drain(..over);
    }

    /// Take in the model turn it streams now, or forget the one it stopped
    /// streaming. Whether what a follower reads changed.
    fn absorb(&mut self, presentation: Option<&runtime_collab::TaskPresentation>) -> bool {
        match presentation {
            None => self.live.take().is_some(),
            Some(presentation) => {
                let newer = self.live.as_ref().is_none_or(|known| {
                    known.run != presentation.run || presentation.sequence > known.sequence
                });
                if newer {
                    self.live = Some(presentation.clone());
                }
                newer
            }
        }
    }

    /// The run as the broker reads it.
    fn tracked(&self) -> TrackerRun {
        TrackerRun {
            issue: wire::issue(&self.id),
            title: self.title.clone(),
            instruction: self.instruction.clone(),
            agent: self.agent.clone(),
            epoch: self.epoch,
            starter: self.starter.clone(),
            active: self.phase.active(),
            finished_by_agent: matches!(self.phase, wire::Phase::Done | wire::Phase::Failed),
            // A run the session keeps is never held at a person's pause: what
            // stops it ends it.
            paused_through: None,
            budget: None,
            subject: self.subject.clone(),
            model: self.model.clone(),
            directions: self
                .directions
                .iter()
                .enumerate()
                .map(|(index, direction)| TrackerDirection {
                    id: format!("{}/{index}", self.id),
                    author: direction.author.clone(),
                    text: direction.text.clone(),
                    at_ms: direction.at_ms,
                    incorporated: direction.incorporated,
                })
                .collect(),
            question: None,
            // A run the session keeps is its own person's ask, never a task agent's: no
            // delegation certifies it.
            delegation: None,
        }
    }
}

/// Who runs, where, and what the rooms' people may do there: what the
/// session feeds its runs each pass, and never an action's arguments.
#[derive(Clone, Debug, Default, PartialEq)]
struct Here {
    /// The person at this session: every ask and steering is theirs.
    person: String,
    /// The room in front: the one an ask is made in.
    front: String,
    /// Every workspace's room as the desk holds it, in its order. A run
    /// belongs to the room it was asked in, so a person moving to another
    /// workspace does not stall it.
    rooms: Vec<RoomFacts>,
}

/// One room as the desk holds it: what runs in it may do, and who may run.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomFacts {
    /// The room's id.
    pub room: String,
    /// The agents it admits now, by their bare id.
    pub agents: Vec<String>,
    /// Its grants: a run's child actions are admitted by these.
    pub policy: Option<tilo_action_policy::RoomPolicy>,
}

impl RoomFacts {
    /// `room`, admitting `agents`, under the grants its people gave.
    #[must_use]
    pub fn new(
        room: &str,
        agents: Vec<String>,
        policy: Option<tilo_action_policy::RoomPolicy>,
    ) -> Self {
        Self {
            room: room.to_owned(),
            agents,
            policy,
        }
    }
}

/// The session's runs, and what the person asking and the agent running one
/// see of them.
#[derive(Default)]
pub struct AgentRuns {
    held: Mutex<Held>,
    /// What wakes the session's loop when the run channel brings something:
    /// whoever follows a run sees it without the loop's next timer.
    wake: Option<Wake>,
}

#[derive(Default)]
struct Held {
    here: Here,
    /// The runs it keeps, oldest first.
    runs: Vec<Run>,
    /// Whether anything landed since the loop last took it in: what makes the
    /// readings of the runs be read again.
    landed: bool,
}

impl Held {
    /// `room` as the desk holds it, or why this session cannot speak for it.
    fn room(&self, room: &str) -> Result<&RoomFacts, String> {
        self.here
            .rooms
            .iter()
            .find(|facts| facts.room == room)
            .ok_or_else(|| "this session holds no workspace of that room".into())
    }
}

impl AgentRuns {
    /// The runs, waking `wake` when the run channel brings one something.
    #[must_use]
    pub fn new(wake: Option<Wake>) -> Self {
        Self {
            held: Mutex::new(Held::default()),
            wake,
        }
    }

    /// Who sits here, the room in front, and every workspace's room with the
    /// agents it admits and the grants its people gave. The session tells it
    /// every pass; it keeps what changed and nothing else.
    pub fn here(&self, person: &str, front: &str, rooms: Vec<RoomFacts>) {
        let moved = {
            let mut held = self.held.lock_recover();
            let here = Here {
                person: person.to_owned(),
                front: front.to_owned(),
                rooms,
            };
            let moved = held.here != here;
            if moved {
                held.here = here;
                held.landed = true;
            }
            moved
        };
        if moved {
            self.wake();
        }
    }

    /// What the runs read now, every one of them.
    #[must_use]
    pub fn readings(&self) -> Vec<wire::Run> {
        self.held
            .lock_recover()
            .runs
            .iter()
            .map(Run::reading)
            .collect()
    }

    /// What run `id` reads now.
    #[must_use]
    pub fn reading(&self, id: &str) -> Option<wire::Run> {
        self.held
            .lock_recover()
            .runs
            .iter()
            .find(|run| run.id == id)
            .map(Run::reading)
    }

    /// Queue an ask as a run: who asked, what of whom, and where it acts.
    ///
    /// # Errors
    /// The ask names no agent, a blank title or instruction, an id already
    /// taken, a subject that does not read, or it is asked from a session
    /// with no room in front to run in.
    pub fn ask(&self, ask: Ask) -> Result<wire::Asked, DoorError> {
        let title = cut(&ask.title);
        if title.is_empty() {
            return Err(DoorError::InvalidArguments(
                "an ask says what the agent is asked, in a line".into(),
            ));
        }
        let instruction = cut_long(&ask.body.unwrap_or_default(), INSTRUCTION_CHARS);
        let instruction = if instruction.trim().is_empty() {
            title.clone()
        } else {
            instruction
        };
        let agent = ask
            .agent
            .trim()
            .strip_prefix("agent:")
            .unwrap_or_else(|| ask.agent.trim())
            .to_owned();
        if agent.is_empty() {
            return Err(DoorError::InvalidArguments(
                "an ask names the agent to ask".into(),
            ));
        }
        let subject = ask.subject.map(read_subject).transpose()?;
        let (person, room) = {
            let held = self.held.lock_recover();
            (held.here.person.clone(), held.here.front.clone())
        };
        if room.is_empty() {
            return Err(DoorError::Unavailable {
                code: "no-room".into(),
                reason: "this session is in no workspace to run the ask in".into(),
            });
        }
        if person.is_empty() {
            return Err(DoorError::Unavailable {
                code: "no-person".into(),
                reason: "no person sits at this session to ask".into(),
            });
        }
        let id = match ask.id {
            Some(id) if !id.trim().is_empty() => read_id(id.trim())?,
            _ => uuid::Uuid::new_v4().to_string(),
        };
        let asked = wire::Asked {
            run: wire::run(&id),
            issue: wire::issue(&id),
        };
        {
            let mut held = self.held.lock_recover();
            if held.runs.iter().any(|run| run.id == id) {
                return Err(DoorError::InvalidArguments(format!(
                    "the ask {id} was made already"
                )));
            }
            held.runs.push(Run {
                id,
                room,
                title,
                instruction,
                agent,
                starter: person,
                epoch: 1,
                phase: wire::Phase::Approved,
                at_ms: unix_now_ms_i64(),
                quick: ask.quick.unwrap_or(false),
                model: ask.model.map(|model| runtime_collab::TaskRunModel {
                    profile: model.profile,
                    model: model.model,
                }),
                subject,
                directions: Vec::new(),
                said: Vec::new(),
                programs: Vec::new(),
                answer: None,
                live: None,
            });
            let over = held.runs.len().saturating_sub(KEPT_RUNS);
            held.runs.drain(..over);
            held.landed = true;
        }
        self.wake();
        Ok(asked)
    }

    /// Tell run `id` something its agent takes in as it goes.
    ///
    /// # Errors
    /// No such run here, or it ended.
    pub fn send_message(&self, id: &str, text: &str) -> Result<(), DoorError> {
        let text = cut_long(text.trim(), MESSAGE_CHARS);
        if text.is_empty() {
            return Err(DoorError::InvalidArguments(
                "a message to a run says something".into(),
            ));
        }
        {
            let mut held = self.held.lock_recover();
            let author = held.here.person.clone();
            let run = run_of(&mut held.runs, id)?;
            if !run.phase.active() {
                return Err(ended(run));
            }
            if run.directions.len() >= KEPT_DIRECTIONS {
                return Err(DoorError::Unavailable {
                    code: "run-hears-no-more".into(),
                    reason: "this run took in as many messages as it keeps; ask again in a new ask"
                        .into(),
                });
            }
            run.directions.push(TrackerDirection {
                id: format!("{id}/{}", run.directions.len()),
                author,
                text,
                at_ms: unix_now_ms_i64(),
                incorporated: false,
            });
            held.landed = true;
        }
        self.wake();
        Ok(())
    }

    /// Stop run `id`: its agent takes no further step, and what it already
    /// did stays.
    ///
    /// # Errors
    /// No such run here.
    pub fn stop(&self, id: &str) -> Result<(), DoorError> {
        let changed = {
            let mut held = self.held.lock_recover();
            let run = run_of(&mut held.runs, id)?;
            if run.phase.active() {
                run.phase = wire::Phase::Stopped;
                held.landed = true;
                true
            } else {
                false
            }
        };
        if changed {
            self.wake();
        }
        Ok(())
    }

    /// Record a program run `id` ran, with what it answered.
    ///
    /// # Errors
    /// No such run here, or it ended.
    pub fn record_step(&self, id: &str, step: ProgramStep) -> Result<(), DoorError> {
        if step.program.trim().is_empty() {
            return Err(DoorError::InvalidArguments(
                "a step names the program it ran".into(),
            ));
        }
        {
            let mut held = self.held.lock_recover();
            let run = run_of(&mut held.runs, id)?;
            if !run.phase.active() {
                return Err(ended(run));
            }
            run.programs.push(step.bounded());
            let over = run.programs.len().saturating_sub(SHOWN_PROGRAMS);
            run.programs.drain(..over);
            held.landed = true;
        }
        self.wake();
        Ok(())
    }

    /// Record that run `id` answered with the view `answer` keeps.
    ///
    /// # Errors
    /// No such run here, or it ended.
    pub fn record_answer(&self, id: &str, answer: ResourceAt) -> Result<(), DoorError> {
        if answer.reference.id.trim().is_empty() || answer.app.trim().is_empty() {
            return Err(DoorError::InvalidArguments(
                "an answer names the resource that keeps it".into(),
            ));
        }
        {
            let mut held = self.held.lock_recover();
            let run = run_of(&mut held.runs, id)?;
            if !run.phase.active() {
                return Err(ended(run));
            }
            run.answer = Some(answer);
            held.landed = true;
        }
        self.wake();
        Ok(())
    }

    /// What the run channel of `room` reports of the run `issue` names.
    ///
    /// The agent's own report: what it accepted, what it said, the messages
    /// it took in, and how it ended. A report for a run this session does
    /// not keep, or for another epoch of one it does, changes nothing.
    pub fn absorb_report(
        &self,
        room: &str,
        issue: &str,
        epoch: u64,
        update: &runtime_collab::TaskRunUpdate,
    ) {
        use runtime_collab::TaskRunUpdate;
        let Some(id) = wire::id_of_issue(issue) else {
            return;
        };
        let mut held = self.held.lock_recover();
        let found = held
            .runs
            .iter_mut()
            .find(|run| run.id == id && run.room == room && run.epoch == epoch);
        let Some(run) = found else {
            return;
        };
        match update {
            TaskRunUpdate::Accepted => {
                if run.phase == wire::Phase::Approved {
                    run.phase = wire::Phase::Accepted;
                }
            }
            TaskRunUpdate::DirectionsApplied { revision } => {
                let through = usize::try_from(*revision).unwrap_or(usize::MAX);
                for direction in run.directions.iter_mut().take(through) {
                    direction.incorporated = true;
                }
            }
            TaskRunUpdate::Progress { message, .. } => {
                if run.phase.active() {
                    run.phase = wire::Phase::Executing;
                }
                run.note_said(message);
            }
            TaskRunUpdate::Succeeded { message } => {
                run.phase = wire::Phase::Done;
                run.note_said(message);
                run.live = None;
            }
            TaskRunUpdate::Failed { message } => {
                run.phase = wire::Phase::Failed;
                run.note_said(message);
                run.live = None;
            }
            TaskRunUpdate::Cancelled { message } => {
                run.phase = wire::Phase::Stopped;
                run.note_said(message);
                run.live = None;
            }
        }
        held.landed = true;
        drop(held);
        self.wake();
    }

    /// What run `epoch` of `issue` in `room` streams of its model turn now,
    /// while it goes: the newest snapshot of it, never written anywhere.
    pub fn absorb_presentation(
        &self,
        room: &str,
        issue: &str,
        epoch: u64,
        presentation: Option<&runtime_collab::TaskPresentation>,
    ) {
        let Some(id) = wire::id_of_issue(issue) else {
            return;
        };
        let mut held = self.held.lock_recover();
        let found = held
            .runs
            .iter_mut()
            .find(|run| run.id == id && run.room == room && run.epoch == epoch);
        let changed = found.is_some_and(|run| run.absorb(presentation));
        if changed {
            held.landed = true;
        }
        drop(held);
        if changed {
            self.wake();
        }
    }

    /// The runs in `room` the broker delivers: every one of them, going or
    /// ended, as the tracker it reads them from.
    ///
    /// # Errors
    /// The room is not one this session holds a workspace of.
    pub fn tracked(&self, room: &str) -> Result<Vec<TrackerRun>, String> {
        let held = self.held.lock_recover();
        held.room(room)?;
        Ok(held
            .runs
            .iter()
            .filter(|run| run.room == room)
            .map(Run::tracked)
            .collect())
    }

    /// Whether `agent` is one this room admits now.
    ///
    /// # Errors
    /// Another room, or the agent is not admitted there.
    pub fn admits(&self, room: &str, agent: &str) -> Result<(), String> {
        let held = self.held.lock_recover();
        if held
            .room(room)?
            .agents
            .iter()
            .any(|admitted| admitted == agent)
        {
            Ok(())
        } else {
            Err(format!("the agent {agent} is no longer admitted here"))
        }
    }

    /// `room`'s grants, as the desk holds them: what a run's child actions are
    /// decided by, and nothing when its workspace is not open.
    #[must_use]
    pub fn room_policy(&self, room: &str) -> Option<tilo_action_policy::RoomPolicy> {
        self.held
            .lock_recover()
            .room(room)
            .ok()
            .and_then(|facts| facts.policy.clone())
    }

    /// Whether this session holds a workspace of `room`.
    #[must_use]
    pub fn knows(&self, room: &str) -> bool {
        self.held.lock_recover().room(room).is_ok()
    }

    /// The room in front, and nothing while there is none.
    #[must_use]
    pub fn room(&self) -> String {
        self.held.lock_recover().here.front.clone()
    }

    /// Tell the loop something landed: whoever follows a run reads it at
    /// once rather than at the next timer.
    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    /// Take in what landed since the last pass. Whether any reading moved.
    fn took_landed(&self) -> bool {
        std::mem::take(&mut self.held.lock_recover().landed)
    }
}

/// What a person asks of an agent, as [`RunsAction::Ask`] carries it.
#[derive(Clone, Debug, Default)]
pub struct Ask {
    pub id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub agent: String,
    pub model: Option<wire::RunModel>,
    pub subject: Option<serde_json::Value>,
    pub quick: Option<bool>,
}

/// The run `id` of `runs`, where it is here.
fn run_of<'a>(runs: &'a mut [Run], id: &str) -> Result<&'a mut Run, DoorError> {
    runs.iter_mut().find(|run| run.id == id).ok_or_else(no_run)
}

/// What an action on a run this session does not keep is refused with.
fn no_run() -> DoorError {
    DoorError::Unavailable {
        code: "no-run".into(),
        reason: "this session keeps no run by that name".into(),
    }
}

/// What a run that ended refuses with.
fn ended(run: &Run) -> DoorError {
    DoorError::Unavailable {
        code: "run-ended".into(),
        reason: format!("the run {} ended", run.title),
    }
}

/// The id an ask names itself, when it names one: a word the run's resource
/// address and its run channel address can both carry.
///
/// # Errors
/// The id is not one word of letters, digits, dots, dashes or underscores.
fn read_id(id: &str) -> Result<String, DoorError> {
    let one_word = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|one| one.is_ascii_alphanumeric() || matches!(one, '.' | '-' | '_'));
    if one_word {
        Ok(id.to_owned())
    } else {
        Err(DoorError::InvalidArguments(
            "an ask's id is one word of letters, digits, dots, dashes or underscores".into(),
        ))
    }
}

/// A title or a message cut to its bound on a character boundary.
fn cut(text: &str) -> String {
    cut_long(text.trim(), TITLE_CHARS)
}

fn cut_long(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// A report's message, when it says something.
fn message_text(text: &str) -> Option<String> {
    let text = cut_long(text.trim(), MESSAGE_CHARS);
    (!text.is_empty()).then_some(text)
}

/// The selection an ask is about, when it reads.
///
/// # Errors
/// A subject this session cannot read, or one out of bounds.
fn read_subject(subject: serde_json::Value) -> Result<SelectionSubject, DoorError> {
    let subject: SelectionSubject = serde_json::from_value(subject).map_err(|error| {
        DoorError::InvalidArguments(format!("the subject does not read: {error}"))
    })?;
    subject.validate().map_err(|reason| {
        DoorError::InvalidArguments(format!("the subject is out of bounds: {reason}"))
    })?;
    Ok(subject)
}
