//! What a person sees of a job: one row each, in a band across the window's
//! foot or in the Jobs surface, and the presses those rows report.
//!
//! A row says what is happening ([`Job::title`]), what the job is doing now
//! and how far ([`Job::live_line`]), how it ended ([`Job::outcome_line`]) and
//! offers the two things a person can do about it: **Stop** while it runs and
//! can be stopped, **Show me** to go to the thing being worked on. A finished
//! row is read with **Read** and then leaves.
//!
//! The rows carry no state of their own: they are built from the registry
//! ([`super::rows`]) whenever a surface draws, so the same job looks the same
//! in the band of one app and the pane of another.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::icons::Icon;
use crate::tokens::Rhythm;
use crate::widget::{
    Button, Column, ControlSize, Decorator, Element, EmptyState, ListRow, ProgressBar, Row,
    Scrollable, Section, SectionHeader, Widget,
};

use super::record::{Job, JobId, Progress, Subject, Unit};
use super::{dismiss, revision, rows, show, stop};

/// What the person pressed on a jobs surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// Stop this job.
    Stop(JobId),
    /// Go to what this job works on.
    Show(JobId),
    /// Read this finished job's row: it leaves.
    Read(JobId),
}

/// How many rows the band across a window's foot shows before it names the
/// rest; the pane shows them all.
const BAND_ROWS: usize = 3;

/// A jobs surface's state: the press sink its rows write to, and the
/// registry revision it last drew.
///
/// An app keeps one beside the surface it draws, takes what was pressed
/// ([`Self::take`]) in its update, and rebuilds when [`Self::changed`] says
/// the rows moved.
#[derive(Clone, Default)]
pub struct Presses {
    pressed: Rc<RefCell<Vec<Press>>>,
    drawn: Rc<Cell<Option<u64>>>,
}

impl std::fmt::Debug for Presses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presses")
            .field("pending", &self.pressed.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Presses {
    /// A sink with nothing pressed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the registry moved since the last surface was built with it:
    /// the app lays out again then.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.drawn.get() != Some(revision())
    }

    /// Note that the surface was built against the registry as it stands.
    pub fn drawn(&self) {
        self.drawn.set(Some(revision()));
    }

    /// What was pressed since the last call.
    #[must_use]
    pub fn take(&self) -> Vec<Press> {
        std::mem::take(&mut *self.pressed.borrow_mut())
    }

    /// Record `press` as the person's: what a row's control does, and what a
    /// host that stands for the row (a chord, a test) does.
    pub fn pressed(&self, press: Press) {
        self.pressed.borrow_mut().push(press);
    }
}

/// A row per job that runs, and per job that ended and has not been read.
///
/// The band is for [`NavShell::bottom_band`](crate::widget::NavShell::bottom_band):
/// it belongs to the window rather than to the page open, so a job stays
/// visible and stoppable while the person works somewhere else.
#[must_use]
pub fn band(presses: &Presses) -> Column {
    presses.drawn();
    let all = rows();
    let mut shown: Vec<Element> = all
        .iter()
        .take(BAND_ROWS)
        .map(|job| job_row(job, presses))
        .collect();
    if all.len() > BAND_ROWS {
        shown.push(more_row(all.len() - BAND_ROWS));
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related);
    for row in shown {
        column = column.push(row);
    }
    column
}

/// The Jobs surface: every row, under the two headings that say which is
/// which.
#[must_use]
pub fn pane(presses: &Presses) -> Column {
    presses.drawn();
    let all = rows();
    if all.is_empty() {
        return Column::new().with_no_padding().push(Box::new(
            EmptyState::new(crate::i18n::widget_format("jobs-none", &[]))
                .with_message(crate::i18n::widget_format("jobs-none-message", &[]))
                .with_icon(Icon::Timer)
                .with_id("jobs-empty"),
        ));
    }
    let mut running: Vec<Element> = Vec::new();
    let mut ended: Vec<Element> = Vec::new();
    for job in &all {
        if job.is_running() {
            running.push(job_row(job, presses));
            running.push(bar(job));
        } else {
            ended.push(job_row(job, presses));
        }
    }
    let mut parts: Vec<Element> = Vec::new();
    if !running.is_empty() {
        parts.push(Box::new(Section::new(running).with_header(
            SectionHeader::group(crate::i18n::widget_format("jobs-running-heading", &[])),
        )));
    }
    if !ended.is_empty() {
        parts.push(Box::new(Section::new(ended).with_header(
            SectionHeader::group(crate::i18n::widget_format("jobs-finished-heading", &[])),
        )));
    }
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group);
    for part in parts {
        column = column.push(part);
    }
    Column::new().with_no_padding().push(Box::new(
        Scrollable::new_vertical().with_child(Box::new(column)),
    ))
}

/// One job's row: what it is, what it is doing, and the two things that can
/// be done about it.
fn job_row(job: &Job, presses: &Presses) -> Element {
    let id = job.id();
    let subject = job.subject();
    let mut controls: Vec<Element> = Vec::new();
    if job.is_running() {
        if job.can_stop() {
            let presses = Rc::clone(&presses.pressed);
            controls.push(Box::new(
                Button::new(&format!("jobs-stop-{}", id.number()), "jobs-stop")
                    .control_size(ControlSize::Small)
                    .on_view(move || presses.borrow_mut().push(Press::Stop(id))),
            ));
        } else if job.is_stopping() {
            controls.push(Box::new(
                crate::widget::Text::new(&crate::i18n::widget_format("jobs-stopping", &[]))
                    .with_role(crate::tokens::TextRole::Caption),
            ));
        }
    } else {
        if subject.target.is_some() {
            let presses = Rc::clone(&presses.pressed);
            controls.push(Box::new(
                Button::icon_only(
                    &format!("jobs-show-{}", id.number()),
                    Icon::ArrowRight,
                    &crate::i18n::widget_format("jobs-show-me", &[]),
                )
                .control_size(ControlSize::Small)
                .on_view(move || presses.borrow_mut().push(Press::Show(id))),
            ));
        }
        let presses = Rc::clone(&presses.pressed);
        controls.push(Box::new(
            Button::icon_only(
                &format!("jobs-read-{}", id.number()),
                Icon::X,
                &crate::i18n::widget_format("jobs-read", &[]),
            )
            .control_size(ControlSize::Small)
            .on_view(move || presses.borrow_mut().push(Press::Read(id))),
        ));
    }
    let mut row = ListRow::new(format!("jobs-row-{}", id.number()), job.title())
        .icon(subject.owner.icon)
        .secondary(job.end_line())
        .tooltip(format!("{}: {}", subject.owner.name, subject.label));
    if let Some(percent) = job.percent() {
        row = row.meta(percent);
    }
    if matches!(job.outcome(), Some(super::Outcome::Failed(_))) {
        row = row.error();
    }
    if !controls.is_empty() {
        let mut row_controls = Row::new().no_wrap().with_rhythm(Rhythm::Related);
        for control in controls {
            row_controls = row_controls.push(control);
        }
        row = row.trailing(Box::new(row_controls));
    }
    Box::new(row)
}

/// A job's progress under its row: determinate when the total is known, a
/// bar with no claim when it is not.
fn bar(job: &Job) -> Element {
    let bar = match job.progress() {
        Progress::Measured { done, total } if *total > 0 => {
            ProgressBar::new(*done as f32 / *total as f32)
        }
        Progress::Measured { .. } | Progress::Starting | Progress::Seen { .. } => {
            ProgressBar::indeterminate()
        }
    };
    let line = match (job.progress(), job.unit()) {
        (Progress::Measured { done, total }, unit) => crate::i18n::widget_format(
            "jobs-bar-of",
            &[
                ("done", crate::i18n::ArgValue::from(unit.format(*done))),
                ("total", crate::i18n::ArgValue::from(unit.format(*total))),
            ],
        ),
        (Progress::Seen { seen: _ }, Unit::Steps) | (Progress::Starting, _) => String::new(),
        (Progress::Seen { seen }, unit) => unit.format(*seen),
    };
    let bar = if line.is_empty() {
        bar
    } else {
        bar.with_label(line)
    };
    Box::new(bar)
}

/// "…and 2 more", which opens the Jobs surface: a row that cannot act is not
/// a row, so this one goes somewhere.
fn more_row(more: usize) -> Element {
    Box::new(
        ListRow::new(
            "jobs-more",
            crate::i18n::widget_format(
                "jobs-more",
                &[("count", crate::i18n::ArgValue::from(more as f64))],
            ),
        )
        .icon(Icon::Timer)
        .on_view(super::open),
    )
}

/// What pressing one row's control asks of the registry: stop, dismiss, or
/// the subject "Show me" goes to.
///
/// An app applies what it took: `stop` and `dismiss` act on the registry,
/// and a [`Press::Show`] answer opens the app on that subject's target.
pub fn apply(press: Press) -> Option<Subject> {
    match press {
        Press::Stop(id) => {
            stop(id);
            None
        }
        Press::Show(id) => show(id),
        Press::Read(id) => {
            dismiss(id);
            None
        }
    }
}

/// A jobs surface as a widget: it keeps its rows, rebuilds them when the
/// registry moves, and reports what was pressed through its [`Presses`].
///
/// An app declares one in its window once — as the band across the foot
/// ([`NavShell::bottom_band`](crate::widget::NavShell::bottom_band)) or as
/// the Jobs page — and reads [`Band::presses`] in its update. The widget
/// owns no state of its own beyond its rows: what it shows is the registry.
pub struct Band {
    inner: Column,
    presses: Presses,
    shape: Shape,
    restack: bool,
}

/// Which surface a [`Band`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The band across the window's foot: the first rows, and how many more.
    Band,
    /// The Jobs surface: every row, under both headings.
    Page,
}

impl Band {
    /// The band across the window's foot.
    #[must_use]
    pub fn new() -> Self {
        Self::of(Shape::Band)
    }

    /// The Jobs page: every row.
    #[must_use]
    pub fn page() -> Self {
        Self::of(Shape::Page)
    }

    /// A band sharing `presses` with the app that reads them.
    #[must_use]
    pub fn with_presses(presses: Presses) -> Self {
        Self::of_with(Shape::Band, presses)
    }

    /// The Jobs page sharing `presses` with the app that reads them.
    #[must_use]
    pub fn page_with(presses: Presses) -> Self {
        Self::of_with(Shape::Page, presses)
    }

    fn of(shape: Shape) -> Self {
        Self::of_with(shape, Presses::new())
    }

    fn of_with(shape: Shape, presses: Presses) -> Self {
        let inner = bare(presses.clone(), shape);
        Self {
            inner,
            presses,
            shape,
            restack: false,
        }
    }

    /// What was pressed, and whether the registry moved: the app answers
    /// both in its update.
    #[must_use]
    pub fn presses(&self) -> &Presses {
        &self.presses
    }

    /// Whether there is anything at all to show: an app keeps the band out
    /// of the window while nothing runs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        super::rows().is_empty()
    }

    fn restack(&mut self) {
        self.inner = bare(self.presses.clone(), self.shape);
        self.restack = true;
    }
}

impl Default for Band {
    fn default() -> Self {
        Self::new()
    }
}

/// The rows `shape` shows, built into a column with no state of its own.
fn bare(presses: Presses, shape: Shape) -> Column {
    match shape {
        Shape::Band => band(&presses),
        Shape::Page => pane(&presses),
    }
}

impl Decorator for Band {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.inner
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let inner = Widget::update(&mut self.inner, state, path);
        if self.presses.changed() {
            self.restack();
            return true;
        }
        inner
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.restack) || Widget::take_structure_dirty(&mut self.inner)
    }

    fn name(&self) -> &'static str {
        "JobsBand"
    }
}
