//! Adapters: what an agent worked out by hand in a legacy application, kept
//! so later runs of the same task need no model.
//!
//! A legacy application — a browser, an office suite, a host application Tilo
//! did not package — publishes no typed actions, so an agent does its work
//! through [`computer`](crate::computer): pointer moves, clicks, typed text
//! and keys, aimed at the app's window. That run is a recording here: one
//! [`Recorded`] step per `computer` call, with the screen before the first
//! step and after each step, so a failure can be told from success without a
//! model looking at it again.
//!
//! Two things follow from a recording. Replaying it ([`Recording::replay`])
//! runs the same steps with no model at all, checked by the steps the agent
//! marked: a person watching the agent mark a screen (`computer_check`) or
//! agree one it was shown (`computer_step`'s `check`), and the first step
//! whose marked screen does not appear stops the replay with a typed
//! [`Drift`]. A step carrying `expect` is only carried out once its mark
//! holds, so it never acts on a screen the app left. A run that drifts falls
//! back to the agent doing the task live, and the steps it takes are then
//! appended to the recording, so the recording becomes what the app does
//! now ([`Recording::append`]) and is offered for review ([`Recording::repair`]).
//!
//! A recording is one file in the profile's `adapters/` directory, named
//! after its app, and is only ever read back for that app.
//!
//! ## What decides sameness
//!
//! Two screens match when the finer of the two grids is reduced to the
//! coarser one, cell by cell, and the fractions of a cell's pixels a picture
//! fills differ by no more than [`CELL_GRID`]'s tolerance — the same
//! comparison at either stored size, so where a mark came from does not
//! matter. The colours are dropped: a theme, a focus ring or a font change
//! that leaves the dark shapes of a row where they were is the same screen,
//! which is what makes a moved button (a new mark with different dark shapes
//! at a new place) a real difference.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;

use crate::computer::{Action, Agent, Computer, Outcome};

/// Grid an agent's marks are kept and compared at, and the most a cell may
/// differ between two pictures of the same screen. Finer grids notice a
/// shifted label; coarser ones survive a repaint. The tests below fix both
/// ends.
const CELL_GRID: (u32, u32) = (32, 20);
const CELL_TOLERANCE: f32 = 0.2;

/// The most steps one recording keeps. A longer run is a task of its own;
/// a recording is meant to be a handful of steps a shortcut can stand for.
pub const MOST_STEPS: usize = 200;

/// The most adapters one profile keeps, oldest-run first to be forgotten.
pub const MOST_ADAPTERS: usize = 64;

/// The most tasks one app's adapter keeps. Beyond this the oldest task is
/// forgotten, so a long-lived app cannot grow its file without bound.
pub const MOST_TASKS_PER_APP: usize = 32;

/// Screens keep the profile small: a recording is an adapter, not a video.
const MOST_SCREEN_BYTES: usize = 8 * 1024 * 1024;
const MOST_NAME_CHARS: usize = 80;

/// The app whose window the steps act in: a provider-less name, as the
/// catalogue lists a host application (`app.host:<desktop-id>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppId(String);

impl AppId {
    /// The app `value` names.
    ///
    /// # Errors
    ///
    /// Names longer than [`MOST_NAME_CHARS`], or that cannot be part of one
    /// file's name.
    pub fn parse(value: &str) -> Result<Self, AdapterError> {
        if value.is_empty() || value.chars().count() > MOST_NAME_CHARS {
            return Err(AdapterError::Invalid("app id length".into()));
        }
        if value
            .chars()
            .any(|character| character == '/' || character == '\0' || character.is_control())
        {
            return Err(AdapterError::Invalid("app id".into()));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A picture an agent marked as "the app looks like this".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    /// [`CELL_GRID`]'s cells, row by row, each the fraction of the cell's
    /// pixels the picture fills (`0`..=`255`).
    pub cells: Vec<u8>,
}

impl Screen {
    /// Reduce a picture to the grid marks are compared at.
    ///
    /// # Errors
    ///
    /// A picture with no pixels.
    pub fn mark(picture: &image::RgbaImage) -> Result<Self, AdapterError> {
        if picture.width() == 0 || picture.height() == 0 {
            return Err(AdapterError::Invalid("empty picture".into()));
        }
        let cells = reduce(picture, CELL_GRID);
        Ok(Self {
            width: CELL_GRID.0,
            height: CELL_GRID.1,
            cells,
        })
    }

    /// Read a screen back from the cells an agent was shown.
    fn from_cells(cells: Vec<u8>) -> Result<Self, AdapterError> {
        if cells.len()
            != usize::try_from(CELL_GRID.0)
                .unwrap_or(0)
                .saturating_mul(usize::try_from(CELL_GRID.1).unwrap_or(0))
        {
            return Err(AdapterError::Invalid("screen size".into()));
        }
        Ok(Self {
            width: CELL_GRID.0,
            height: CELL_GRID.1,
            cells,
        })
    }

    /// How far this screen is from `other`, `0.0` identical and `1.0` in
    /// nothing alike: the largest a cell differs after both are taken at the
    /// finer grid.
    #[must_use]
    pub fn distance(&self, other: &Self) -> f32 {
        let grid = (self.width.max(other.width), self.height.max(other.height));
        let mine = resample(&self.cells, (self.width, self.height), grid);
        let theirs = resample(&other.cells, (other.width, other.height), grid);
        mine.iter()
            .zip(theirs.iter())
            .map(|(mine, theirs)| f32::from(mine.abs_diff(*theirs)) / 255.0)
            .fold(0.0, f32::max)
    }

    /// Whether `other` is this screen: a repaint that leaves the shapes of
    /// the screen where they were.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.distance(other) <= CELL_TOLERANCE
    }
}

/// Which points of a fresh recording a person is offered to mark.
///
/// The screen before the first step, the screen after the last, and then
/// every [`interest`] step the run takes until [`MOST_MARKS`] marks exist: the
/// most a person is asked to look at, spread over the run rather than all at
/// its end.
pub const MOST_MARKS: usize = 8;

fn interest(context: &str) -> bool {
    // Steps that leave something behind: what a later step or the person's
    // task depends on having happened.
    matches!(
        context,
        "key"
            | "type"
            | "left_click"
            | "right_click"
            | "middle_click"
            | "double_click"
            | "triple_click"
            | "left_click_drag"
            | "scroll"
    )
}

/// One step of a recording: the `computer` call it replays, the screen an
/// agent marked for it (the screen the step left, or with `expect` the
/// screen it may only be taken from), and the screen it actually left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    pub action: Action,
    pub check: Option<Screen>,
    #[serde(default)]
    pub expect: Option<Screen>,
    pub after: Option<Screen>,
}

/// Everything an agent did in one app for one task, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub app: AppId,
    /// What the person asked, in their words: how a later run says the same
    /// thing and reaches this adapter again.
    pub task: String,
    pub steps: Vec<Recorded>,
    /// The screen the run started from, when it captured one.
    #[serde(default)]
    pub start: Option<Screen>,
    /// How many runs have been recorded for this task, and how many of them
    /// fell back to an agent because the app drifted.
    pub runs: u32,
    #[serde(default)]
    pub fallbacks: u32,
    pub created_unix: i64,
    pub updated_unix: i64,
    /// Whether the run now recorded repaired an earlier one, and the step a
    /// replay that fell back stopped at.
    #[serde(default)]
    pub repaired: bool,
    #[serde(default)]
    pub drifted_at: Option<usize>,
    /// The steps this recording had before the run that repaired it, so a
    /// person is shown a difference rather than a whole recording.
    #[serde(default)]
    pub baseline_steps: Vec<Repaired>,
}

/// Where a replay stopped and what the app showed instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    /// The step not carried out: the first one whose marked screen was not
    /// there, so the agent does the task live from here.
    pub step: usize,
    /// What a repair or a review is about, in words for the person.
    pub reason: String,
}

impl Drift {
    /// The typed error a replay answers with: a check failed, so the agent
    /// does the task live and the recording is repaired. Every other way a
    /// replay can stop (an action refused, a call timed out) is not a check
    /// failing and stays the step's own error.
    #[must_use]
    pub fn error(&self) -> AdapterError {
        AdapterError::Drifted(self.clone())
    }

    /// Whether `error` is a failed check ([`AdapterError::Drifted`]).
    #[must_use]
    pub fn of(error: &AdapterError) -> Option<&Self> {
        match error {
            AdapterError::Drifted(drift) => Some(drift),
            _ => None,
        }
    }
}

/// Why an adapter could not be kept, replayed or read.
#[derive(Debug)]
pub enum AdapterError {
    /// What the caller said about an adapter does not make one.
    Invalid(String),
    /// An adapter's file could not be read or written.
    Io(PathBuf, std::io::Error),
    /// An adapter's file is there but is not one.
    Unreadable(PathBuf, String),
    /// The app has no adapter that answers this task.
    Missing,
    /// A marked screen was not there, so the agent does the task live.
    Drifted(Drift),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(what) => write!(formatter, "an adapter cannot be made: {what}"),
            Self::Io(path, error) => write!(formatter, "adapter {}: {error}", path.display()),
            Self::Unreadable(path, why) => {
                write!(formatter, "adapter {} is unreadable: {why}", path.display())
            }
            Self::Missing => write!(formatter, "no adapter for that app and task"),
            Self::Drifted(drift) => write!(
                formatter,
                "the app no longer looks like the adapter expects at step {}: {}",
                drift.step, drift.reason
            ),
        }
    }
}

impl std::error::Error for AdapterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(_, error) => Some(error),
            _ => None,
        }
    }
}

impl Recording {
    /// Start recording `app` for `task` at the screen `start` showed.
    #[must_use]
    pub fn new(app: AppId, task: impl Into<String>, start: Option<Screen>) -> Self {
        let now = now_unix();
        Self {
            app,
            task: task.into(),
            steps: Vec::new(),
            start,
            runs: 1,
            fallbacks: 0,
            created_unix: now,
            updated_unix: now,
            repaired: false,
            drifted_at: None,
            baseline_steps: Vec::new(),
        }
    }

    /// The screen the recording expects before `index` runs: the one this
    /// step may only be taken from, or the screen the step before it left.
    /// `None` when nothing marked the way there — a step's `check` is the
    /// screen it leaves, never the one it starts from.
    #[must_use]
    pub fn before(&self, index: usize) -> Option<&Screen> {
        let step = self.steps.get(index)?;
        step.expect
            .as_ref()
            .or_else(|| index.checked_sub(1).and_then(|prior| self.after(prior)))
    }

    /// The screen the step at `index` left.
    #[must_use]
    pub fn after(&self, index: usize) -> Option<&Screen> {
        self.steps.get(index).and_then(|step| step.after.as_ref())
    }

    /// Whether `step` may be carried out against `now`: the screen it needs
    /// is there, or nothing marked the screen it needs.
    #[must_use]
    pub fn admits(&self, index: usize, now: &Screen) -> bool {
        self.before(index)
            .is_none_or(|expected| expected.matches(now))
    }

    /// Take one step of a run in `app`: carry it out with `computer`, mark
    /// what it left, and remember it. `capture` is what the agent said about
    /// the step, when it said anything.
    ///
    /// # Errors
    ///
    /// The step's own refusal, or a control socket that is not there.
    pub fn record(
        &mut self,
        computer: &Computer,
        agent: Option<&Agent>,
        action: Action,
        capture: &Capture,
    ) -> Result<Outcome, AdapterError> {
        // The screen the step is taken from, so a step whose mark is only a
        // precondition can be checked where it runs.
        let before = capture
            .mark
            .as_ref()
            .filter(|mark| mark.expect)
            .and_then(|_| self.captured_before(computer, agent));
        let action = Action {
            no_screenshot: match capture.screenshot {
                Screenshot::Default => action.no_screenshot,
                Screenshot::Always => Some(false),
                Screenshot::Never => Some(true),
            },
            ..action
        };
        let mut outcomes = computer
            .run_as(agent, std::slice::from_ref(&action), false)
            .map_err(AdapterError::Invalid)?;
        let outcome = outcomes
            .pop()
            .ok_or_else(|| AdapterError::Invalid("the step ran no action".into()))?;
        let after = mark_of_outcome(&outcome).or_else(|| before.clone());
        self.push(Recorded {
            action,
            check: capture.mark.as_ref().map(|mark| mark.screen.clone()),
            expect: capture.mark.as_ref().filter(|mark| mark.expect).and(before),
            after,
        })?;
        Ok(outcome)
    }

    /// Record a step that did not go through [`Self::record`]: the state an
    /// agent left, as it is replaying or working live.
    ///
    /// # Errors
    ///
    /// [`MOST_STEPS`] already recorded.
    pub fn push(&mut self, step: Recorded) -> Result<(), AdapterError> {
        if self.steps.len() >= MOST_STEPS {
            return Err(AdapterError::Invalid("too many steps".into()));
        }
        self.steps.push(step);
        self.updated_unix = now_unix();
        Ok(())
    }

    /// The screen a step may be taken from, when the app's window is there:
    /// a fresh screenshot of the app's window.
    fn captured_before(&self, computer: &Computer, agent: Option<&Agent>) -> Option<Screen> {
        let outcome = computer
            .run_as(
                agent,
                &[Action {
                    action: "screenshot".into(),
                    app_id: Some(self.app.as_str().to_owned()),
                    ..Action::default()
                }],
                false,
            )
            .ok()?;
        outcome.first().and_then(mark_of_outcome)
    }

    /// Run the recording instead of asking a model, checking each marked
    /// screen as it goes. A step whose screen an agent marked must leave it;
    /// a step only taken from a marked screen must find it first.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Drifted`] the moment a mark does not hold, saying
    /// where — the agent then does the task live and the recording is
    /// repaired — or the failing step's own refusal.
    pub fn replay(
        &self,
        computer: &Computer,
        agent: Option<&Agent>,
    ) -> Result<Vec<Outcome>, AdapterError> {
        let mut outcomes = Vec::with_capacity(self.steps.len());
        for (index, step) in self.steps.iter().enumerate() {
            // A step that may only be taken from a marked screen is checked
            // before it acts, so it never lands on a screen the app left.
            if let Some(expected) = self.before(index)
                && let Some(shown) = self.captured_before(computer, agent)
                && !expected.matches(&shown)
            {
                return Err(Drift {
                    step: index,
                    reason: "the app no longer shows what this step starts from".into(),
                }
                .error());
            }
            let stepped = computer
                .run_as(agent, std::slice::from_ref(&step.action), false)
                .map_err(AdapterError::Invalid)?;
            let outcome = stepped.into_iter().next();
            let after = outcome.as_ref().and_then(mark_of_outcome);
            // A step an agent marked as its result must leave that screen.
            if let Some(expected) = step.check.as_ref()
                && let Some(shown) = after.as_ref()
                && !expected.matches(shown)
            {
                return Err(Drift {
                    step: index,
                    reason: "the app no longer shows what this step should leave".into(),
                }
                .error());
            }
            outcomes.extend(outcome);
        }
        Ok(outcomes)
    }

    /// Append `steps`, the live run that did the task after a replay
    /// drifted: the recording becomes what the app does now. The run is not
    /// one of the recorded ones, so a repair never shows it beside itself.
    ///
    /// # Errors
    ///
    /// [`MOST_STEPS`] would be exceeded.
    pub fn append(&mut self, steps: Vec<Recorded>) -> Result<(), AdapterError> {
        if self.steps.len().saturating_add(steps.len()) > MOST_STEPS {
            return Err(AdapterError::Invalid("too many steps".into()));
        }
        self.steps.extend(steps);
        self.updated_unix = now_unix();
        Ok(())
    }

    /// The app changed under a replay: keep what the recording was, and start
    /// the run the agent is about to do live, so the recording ends up
    /// describing what the app does now. `drift` says where the replay
    /// stopped; `start` is the screen the live run begins from.
    pub fn fall_back(&mut self, drift: &Drift, start: Option<Screen>) {
        self.repaired = true;
        self.baseline_steps = self.steps.iter().map(Repaired::of).collect();
        self.drifted_at = Some(drift.step);
        self.steps.clear();
        self.start = start;
        self.fallbacks = self.fallbacks.saturating_add(1);
        self.runs = self.runs.saturating_add(1);
        self.updated_unix = now_unix();
    }

    /// The recording as it now stands, for review, beside what it was before
    /// the run that repaired it.
    #[must_use]
    pub fn repair(&self) -> (Repair, Repair) {
        (self.baseline(), self.current())
    }

    /// What this recording was before the repair, or itself when nothing
    /// repaired it.
    #[must_use]
    pub fn baseline(&self) -> Repair {
        Repair {
            app: self.app.clone(),
            task: self.task.clone(),
            steps: self.baseline_steps.clone(),
            runs: self.runs,
            fallbacks: self.fallbacks,
            drifted_at: self.drifted_at,
        }
    }

    /// The recording as it now stands.
    #[must_use]
    pub fn current(&self) -> Repair {
        Repair {
            app: self.app.clone(),
            task: self.task.clone(),
            steps: self.steps.iter().map(Repaired::of).collect(),
            runs: self.runs,
            fallbacks: self.fallbacks,
            drifted_at: self.drifted_at,
        }
    }
}

/// What one step of a repaired recording is now, for review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repaired {
    pub action: String,
    /// Where a click lands, or what a step has typed: what a person
    /// recognises the step by.
    pub detail: Option<String>,
    /// Whether this step is the one an agent now marks as the point of it.
    pub marked: bool,
}

impl Repaired {
    /// What one step of `step` now is, for review.
    #[must_use]
    pub fn of(step: &Recorded) -> Self {
        // What a person recognises the step by: where a click lands, the
        // chord a key takes, the words a step types.
        let detail = step
            .action
            .coordinate
            .map(|[x, y]| format!("at {}, {}", x.round(), y.round()))
            .or_else(|| step.action.key.clone())
            .or_else(|| step.action.text.clone());
        Self {
            action: step.action.action.clone(),
            detail,
            marked: step.check.is_some(),
        }
    }
}

/// A recording at one moment, for review: its steps, and how many runs and
/// fallbacks made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repair {
    pub app: AppId,
    pub task: String,
    pub steps: Vec<Repaired>,
    pub runs: u32,
    pub fallbacks: u32,
    /// The step a replay stopped at before this run, when one did.
    pub drifted_at: Option<usize>,
}

impl Repair {
    /// Whether nothing meaningful changed since `prior`: no step was added,
    /// dropped, moved or newly marked, so there is nothing for a person to
    /// review and the repair is applied and logged. A screen re-marked at the
    /// same place is still the same step, which is what makes a repaint at a
    /// new theme or scale trivial.
    #[must_use]
    pub fn is_trivial(&self, prior: &Self) -> bool {
        prior.steps == self.steps
    }

    /// One line saying what changed, for the history and the review.
    #[must_use]
    pub fn describe(&self, prior: &Self) -> String {
        let moved = self
            .steps
            .iter()
            .zip(prior.steps.iter())
            .filter(|(after, before)| {
                after.action == before.action && after.detail != before.detail
            })
            .count();
        json!({
            "app": self.app.as_str(),
            "task": self.task,
            "steps": self.steps.len(),
            "was": prior.steps.len(),
            "moved": moved,
            "runs": self.runs,
            "fallbacks": self.fallbacks,
            "drifted_at": self.drifted_at,
        })
        .to_string()
    }
}

/// One app's adapters: a recording per task, in the order they were kept.
///
/// A legacy app is one window, and a person asks it several things, so the
/// file is the app's and carries each of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdaptersOf {
    pub app: AppId,
    pub recordings: Vec<Recording>,
}

/// Where adapters are kept: one file per app in the profile, each holding
/// that app's recordings one per task.
#[derive(Clone, Debug)]
pub struct Adapters {
    directory: PathBuf,
    /// Serialises writes, so two agents of one profile never leave half of
    /// two adapters in one file.
    writing: std::sync::Arc<std::sync::Mutex<()>>,
}

impl Adapters {
    /// The adapters of `directory`, which is created when first written.
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            writing: std::sync::Arc::default(),
        }
    }

    /// The profile's own `adapters/` directory.
    #[must_use]
    pub fn in_profile(profile: &Path) -> Self {
        Self::new(profile.join("adapters"))
    }

    /// `app`'s file, named after the app and nothing else, so a task can
    /// never be reached through a path.
    fn path(&self, app: &AppId) -> PathBuf {
        let name: String = app
            .as_str()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        self.directory.join(format!("{name}.json"))
    }

    /// The adapter for `app` whose task best answers `task`: the same words
    /// after trimming, ignoring case, spacing and a full stop.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Missing`] when the app has none that says the same
    /// thing, or a file that cannot be read.
    pub fn find(&self, app: &AppId, task: &str) -> Result<Recording, AdapterError> {
        let asked = words(task);
        self.load(app)?
            .recordings
            .into_iter()
            .find(|recording| words(&recording.task) == asked)
            .ok_or(AdapterError::Missing)
    }

    /// Everything `app` has learned to do.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Missing`] when the app has none, or a file that cannot
    /// be read.
    pub fn load(&self, app: &AppId) -> Result<AdaptersOf, AdapterError> {
        let path = self.path(app);
        let bytes = std::fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AdapterError::Missing
            } else {
                AdapterError::Io(path.clone(), error)
            }
        })?;
        let adapters: AdaptersOf = serde_json::from_slice(&bytes)
            .map_err(|error| AdapterError::Unreadable(path.clone(), error.to_string()))?;
        if adapters.app != *app {
            return Err(AdapterError::Unreadable(
                path,
                "the file belongs to another app".into(),
            ));
        }
        Ok(adapters)
    }

    /// Keep `recording`, replacing the adapter `app` already has for the same
    /// task and leaving its other tasks alone.
    ///
    /// # Errors
    ///
    /// A directory or file that cannot be written.
    pub fn save(&self, recording: &Recording) -> Result<(), AdapterError> {
        let _writing = self.writing.lock_recover();
        crate::unix::private_directory(&self.directory)
            .map_err(|error| AdapterError::Io(self.directory.clone(), error))?;
        let app = &recording.app;
        let mut adapters = match self.load(app) {
            Ok(adapters) => adapters,
            Err(AdapterError::Missing) => AdaptersOf {
                app: app.clone(),
                recordings: Vec::new(),
            },
            Err(error) => return Err(error),
        };
        let kept = recording.task.trim().to_lowercase();
        adapters
            .recordings
            .retain(|kept_recording| kept_recording.task.trim().to_lowercase() != kept);
        adapters.recordings.push(recording.clone());
        if adapters.recordings.len() > MOST_TASKS_PER_APP {
            adapters.recordings.remove(0);
        }
        let path = self.path(app);
        let bytes = serde_json::to_vec_pretty(&adapters)
            .map_err(|error| AdapterError::Invalid(error.to_string()))?;
        let temporary = path.with_extension("json.new");
        std::fs::write(&temporary, bytes)
            .map_err(|error| AdapterError::Io(temporary.clone(), error))?;
        std::fs::rename(&temporary, &path)
            .map_err(|error| AdapterError::Io(path.clone(), error))?;
        self.evict();
        Ok(())
    }

    /// Forget the least recently run apps once there are more than
    /// [`MOST_ADAPTERS`]. A folder that cannot be read is left alone: a
    /// person's adapters are not this run's to lose.
    fn evict(&self) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return;
        };
        let mut files: Vec<(i64, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "json") {
                    return None;
                }
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |age| i64::try_from(age.as_secs()).unwrap_or(i64::MAX));
                Some((modified, path))
            })
            .collect();
        if files.len() <= MOST_ADAPTERS {
            return;
        }
        files.sort();
        let most = files.len().saturating_sub(MOST_ADAPTERS);
        for (_, path) in files.into_iter().take(most) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The words of a task, so two ways of saying the same thing meet: case,
/// spacing and a trailing full stop do not make them different.
fn words(task: &str) -> Vec<String> {
    task.to_lowercase()
        .split_whitespace()
        .map(|word| word.trim_end_matches(['.', '!', '?']).to_owned())
        .filter(|word| !word.is_empty())
        .collect()
}

/// What an agent says a step must leave, or only start from.
///
/// A tool passes the cells it was shown (`screen`) — the frame its own last
/// `computer` call returned, or a `computer_check` screenshot — either as an
/// array or as a comma-separated string of numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    /// [`CELL_GRID`]'s cells.
    pub screen: Screen,
    /// The screen may only be taken from: the step leaves something else.
    #[serde(default)]
    pub expect: bool,
}

impl Mark {
    /// A mark reading `cells` from what a tool call carried.
    ///
    /// # Errors
    ///
    /// Cells that are not [`CELL_GRID`]'s, or not numbers `0..=255`.
    pub fn parse(cells: &Value) -> Result<Self, AdapterError> {
        let cells = match cells {
            Value::Array(values) => values
                .iter()
                .map(Value::as_u64)
                .collect::<Option<Vec<u64>>>(),
            Value::String(text) => text
                .split(',')
                .map(|part| part.trim().parse::<u64>().ok())
                .collect::<Option<Vec<u64>>>(),
            _ => None,
        }
        .ok_or_else(|| AdapterError::Invalid("a mark is the cells of a screen".into()))?;
        let cells = cells
            .into_iter()
            .map(|cell| u8::try_from(cell).map_err(|_| AdapterError::Invalid("cell".into())))
            .collect::<Result<Vec<u8>, AdapterError>>()?;
        Ok(Self {
            screen: Screen::from_cells(cells)?,
            expect: false,
        })
    }

    /// A screenshot an agent took as the mark: its image, reduced.
    ///
    /// # Errors
    ///
    /// Bytes that are not a picture.
    pub fn of_image(bytes: &[u8]) -> Result<Self, AdapterError> {
        let picture = image::load_from_memory(bytes)
            .map_err(|error| AdapterError::Invalid(error.to_string()))?
            .to_rgba8();
        Ok(Self {
            screen: Screen::mark(&picture)?,
            expect: false,
        })
    }
}

/// Whether a recorded step asks for the app's picture after it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screenshot {
    /// Whatever the tool's own default is: a screenshot after the last step.
    #[default]
    Default,
    /// Always, so a step that is not last still leaves a mark.
    Always,
    /// Never, so a step that would return a frame does not.
    Never,
}

/// What an agent said about one recorded step: the screen it marks, and
/// whether the step needs a picture of its own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capture {
    pub mark: Option<Mark>,
    pub screenshot: Screenshot,
}

/// The cells of `picture`, taken at `grid`: each cell the fraction of its own
/// pixels the picture fills, scaled to `0..=255`.
fn reduce(picture: &image::RgbaImage, (columns, rows): (u32, u32)) -> Vec<u8> {
    let mut cells = Vec::with_capacity(
        usize::try_from(columns)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(rows).unwrap_or(0)),
    );
    let (width, height) = picture.dimensions();
    for row in 0..rows {
        for column in 0..columns {
            let Some(pixels) = cell_pixels((width, height), (columns, rows), (column, row)) else {
                cells.push(0);
                continue;
            };
            cells.push(darkness(picture, pixels));
        }
    }
    cells
}

/// The pixels a cell's box covers: `(x0, y0, x1, y1)` half-open.
fn cell_pixels(
    (width, height): (u32, u32),
    (columns, rows): (u32, u32),
    (column, row): (u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let left = column.saturating_mul(width).checked_div(columns)?;
    let top = row.saturating_mul(height).checked_div(rows)?;
    let right = (column.saturating_add(1))
        .saturating_mul(width)
        .checked_div(columns)?;
    let bottom = (row.saturating_add(1))
        .saturating_mul(height)
        .checked_div(rows)?;
    (right > left && bottom > top).then_some((left, top, right, bottom))
}

/// How much of `box`'s pixels are dark: what a screen's shapes are made of,
/// whatever it is painted on.
fn darkness(picture: &image::RgbaImage, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> u8 {
    let mut dark = 0u64;
    let mut total = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let pixel = picture.get_pixel(x, y);
            // The picture is flattened over white: a transparent pixel is
            // paper, not ink.
            let alpha = u32::from(pixel[3]);
            let level = u32::from(pixel[0])
                .saturating_mul(alpha)
                .saturating_add(255u32.saturating_mul(255u32.saturating_sub(alpha)))
                .checked_div(255)
                .unwrap_or(0);
            total = total.saturating_add(1);
            if level < 128 {
                dark = dark.saturating_add(1);
            }
        }
    }
    if total == 0 {
        return 0;
    }
    u8::try_from(dark.saturating_mul(255).checked_div(total).unwrap_or(0)).unwrap_or(u8::MAX)
}

/// `cells` of one grid, reduced to `grid`: a finer picture keeps the average
/// of the cells it covers, a coarser one repeats its cells.
fn resample(cells: &[u8], (columns, rows): (u32, u32), (width, height): (u32, u32)) -> Vec<u8> {
    let mut sampled = Vec::with_capacity(
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0)),
    );
    for y in 0..height {
        for x in 0..width {
            let left = x.saturating_mul(columns).checked_div(width).unwrap_or(0);
            let right = (x.saturating_add(1))
                .saturating_mul(columns)
                .checked_div(width)
                .unwrap_or(0)
                .max(left.saturating_add(1))
                .min(columns);
            let top = y.saturating_mul(rows).checked_div(height).unwrap_or(0);
            let bottom = (y.saturating_add(1))
                .saturating_mul(rows)
                .checked_div(height)
                .unwrap_or(0)
                .max(top.saturating_add(1))
                .min(rows);
            let mut sum = 0u32;
            let mut count = 0u32;
            for row in top..bottom {
                for column in left..right {
                    let index = usize::try_from(row.saturating_mul(columns).saturating_add(column))
                        .unwrap_or(0);
                    sum = sum.saturating_add(u32::from(cells.get(index).copied().unwrap_or(0)));
                    count = count.saturating_add(1);
                }
            }
            sampled.push(u8::try_from(sum.checked_div(count.max(1)).unwrap_or(0)).unwrap_or(0));
        }
    }
    sampled
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |age| i64::try_from(age.as_secs()).unwrap_or(i64::MAX))
}

/// The screen a `computer` outcome carried, reduced, for a tool answering an
/// agent who has to mark it.
#[must_use]
pub fn mark_of_outcome(outcome: &Outcome) -> Option<Screen> {
    let frame = outcome.image.as_ref()?;
    let picture = image::load_from_memory(&frame.data).ok()?.to_rgba8();
    Screen::mark(&picture).ok()
}

/// Keep a screen no larger than a recording can hold.
#[must_use]
pub fn screen_fits(screen: &Screen) -> bool {
    screen.cells.len() <= MOST_SCREEN_BYTES
}

/// The `computer` actions a live run may take that leave something behind:
/// what a recording keeps as a step, and what [`interest`] marks.
#[must_use]
pub fn is_step(action: &str) -> bool {
    interest(action)
}

#[cfg(test)]
#[path = "adapters_tests.rs"]
mod tests;
