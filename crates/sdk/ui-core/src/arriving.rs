//! Data that is not here yet: the one model for every read whose answer
//! arrives later.
//!
//! An [`Arriving`] value is a read in flight. Its [`Arrival`] is pending
//! (with the [`Source`] the answer comes from), ready, or failed. App code
//! says only what it shows once the data is there; the framework decides
//! every timing and placeholder ([`crate::widget::ArrivingView`]):
//!
//! - **Local data** (this device's files, rooms and store) normally answers
//!   within a frame or two. A window or a view opens complete: the frame
//!   that would show it waits up to one frame budget for the answer
//!   ([`frame_budget`]), and an answer inside that budget shows with no
//!   placeholder and no motion. A local read that misses the budget (a cold
//!   disk) behaves like a remote one from then on.
//! - **Remote data** (another of the person's devices, a collaborator's, a
//!   host) gets its space reserved at the content's geometry from the
//!   start. Nothing but that ground shows for [`GRACE`]; then a skeleton in
//!   the content's shape, which stays at least [`MIN_SHOWN`] so it never
//!   flashes; after [`NAME_SOURCE_AFTER`] the placeholder says where the
//!   data comes from. The answer cross-fades in
//!   ([`crate::motion::CrossFade::arrive`]); a failure turns the reserved
//!   space into the error, with a retry.
//!
//! Every timed step is a deadline ([`ArrivalClock::next_step_at`]), served
//! by the window's wake scheduler: a pending read draws no frames between
//! its steps.
//!
//! The answer lives in a [`LocalState`], so a bound view updates and an idle
//! window wakes when it lands. [`Arriving::begin`] restarts the read and
//! makes every older [`ArrivalCompletion`] stale ([`LatestTask`]'s rule), so a
//! slow answer can never overwrite a newer one.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use crate::local_state::{LatestTask, LocalState, LocalStateWriter, TaskCompletion};
use crate::time::{Duration, Instant};

/// How long a remote read shows nothing but its reserved ground.
pub const GRACE: Duration = Duration::from_millis(150);
/// The shortest time a skeleton stays once it showed: it never flashes.
pub const MIN_SHOWN: Duration = Duration::from_millis(300);
/// When a placeholder starts saying where its data comes from.
pub const NAME_SOURCE_AFTER: Duration = Duration::from_secs(2);

/// Where data that is not here yet comes from. The read fills it in: a
/// read through this device's store says [`Self::Local`], one answered by a
/// workspace's host says [`Self::Host`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// This device: its files, its rooms, its store.
    Local,
    /// Another device, the person's own or a collaborator's, by the name the
    /// person knows it by ("Laptop").
    Peer(Arc<str>),
    /// A host that serves the data ("eu1.tilo.coffee").
    Host(Arc<str>),
}

impl Source {
    /// Another device, by its name.
    #[must_use]
    pub fn peer(name: impl Into<Arc<str>>) -> Self {
        Self::Peer(name.into())
    }

    /// A host, by its name.
    #[must_use]
    pub fn host(name: impl Into<Arc<str>>) -> Self {
        Self::Host(name.into())
    }

    /// Whether the data is on this device.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// The name a placeholder says the data comes from; `None` for this
    /// device, which is not worth saying.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Peer(name) | Self::Host(name) => Some(&**name).filter(|name| !name.is_empty()),
        }
    }
}

/// Where one read stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arrival<T, E = String> {
    /// Asked; the answer comes from `from`.
    Pending { from: Source },
    /// The answer.
    Ready(T),
    /// It could not be read.
    Failed(E),
}

/// A read of this device's data, not answered yet.
impl<T, E> Default for Arrival<T, E> {
    fn default() -> Self {
        Self::local()
    }
}

impl<T, E> Arrival<T, E> {
    /// Pending from this device.
    #[must_use]
    pub const fn local() -> Self {
        Self::Pending {
            from: Source::Local,
        }
    }

    /// Whether no answer has arrived yet.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    /// The answer, once there.
    #[must_use]
    pub const fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// Why it could not be read, once it failed.
    #[must_use]
    pub const fn failure(&self) -> Option<&E> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// Where a pending read's answer comes from.
    #[must_use]
    pub const fn source(&self) -> Option<&Source> {
        match self {
            Self::Pending { from } => Some(from),
            _ => None,
        }
    }

    /// A view of this arrival borrowing its answer and error.
    #[must_use]
    pub fn as_ref(&self) -> Arrival<&T, &E> {
        match self {
            Self::Pending { from } => Arrival::Pending { from: from.clone() },
            Self::Ready(value) => Arrival::Ready(value),
            Self::Failed(error) => Arrival::Failed(error),
        }
    }

    /// The same arrival with its answer mapped.
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Arrival<U, E> {
        match self {
            Self::Pending { from } => Arrival::Pending { from },
            Self::Ready(value) => Arrival::Ready(map(value)),
            Self::Failed(error) => Arrival::Failed(error),
        }
    }

    /// The same arrival with its error mapped.
    pub fn map_err<F>(self, map: impl FnOnce(E) -> F) -> Arrival<T, F> {
        match self {
            Self::Pending { from } => Arrival::Pending { from },
            Self::Ready(value) => Arrival::Ready(value),
            Self::Failed(error) => Arrival::Failed(map(error)),
        }
    }
}

impl<T, E> From<Result<T, E>> for Arrival<T, E> {
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::Ready(value),
            Err(error) => Self::Failed(error),
        }
    }
}

thread_local! {
    /// One refresh of the display this thread's window is on.
    static FRAME_BUDGET: std::cell::Cell<Duration> =
        const { std::cell::Cell::new(DEFAULT_FRAME_BUDGET) };
}

/// The common desktop refresh, until the window's display says otherwise.
const DEFAULT_FRAME_BUDGET: Duration = Duration::from_nanos(16_666_667);

/// One frame at the display's rate: how long a frame waits for local data,
/// and how soon a local answer must land to show without motion.
#[must_use]
pub fn frame_budget() -> Duration {
    FRAME_BUDGET.with(std::cell::Cell::get)
}

/// The window's display refreshes every `interval`: the frame budget local
/// reads get. The native runner calls this when it learns the display's
/// refresh rate.
pub fn set_frame_budget(interval: Duration) {
    if !interval.is_zero() {
        FRAME_BUDGET.with(|budget| budget.set(interval));
    }
}

/// What a placeholder shows at a moment of its read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing but the reserved ground.
    Ground,
    /// A skeleton in the content's shape.
    Skeleton,
    /// The skeleton, saying where the data comes from.
    Named,
    /// The answer (or the failure) shows.
    Shown,
}

/// The timing of one read, from the moment it was asked: the one place the
/// placeholder steps are decided, for [`crate::widget::ArrivingView`] and for
/// immediate-mode painters alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrivalClock {
    since: Instant,
    from: Source,
    budget: Duration,
    arrived: Option<Instant>,
}

impl ArrivalClock {
    /// A read asked at `since`, from `from`, with this window's frame
    /// budget.
    #[must_use]
    pub fn new(since: Instant, from: Source) -> Self {
        Self::with_budget(since, from, frame_budget())
    }

    /// A read asked at `since`, from `from`, given `budget` for a local
    /// answer.
    #[must_use]
    pub fn with_budget(since: Instant, from: Source, budget: Duration) -> Self {
        Self {
            since,
            from,
            budget,
            arrived: None,
        }
    }

    /// When the read was asked.
    #[must_use]
    pub fn since(&self) -> Instant {
        self.since
    }

    /// Where the answer comes from.
    #[must_use]
    pub fn source(&self) -> &Source {
        &self.from
    }

    /// The answer (or a failure) landed at `at`. Only the first landing
    /// counts.
    pub fn arrive(&mut self, at: Instant) {
        if self.arrived.is_none() {
            self.arrived = Some(at.max(self.since));
        }
    }

    /// Whether the answer landed.
    #[must_use]
    pub fn has_arrived(&self) -> bool {
        self.arrived.is_some()
    }

    fn after(&self, delay: Duration) -> Instant {
        self.since.checked_add(delay).unwrap_or(self.since)
    }

    /// When the skeleton first shows, unless the answer is there first.
    fn skeleton_at(&self) -> Instant {
        self.after(GRACE)
    }

    /// Whether a skeleton showed before the answer landed (or is showing
    /// now, at `now`, with none landed).
    fn skeleton_showed(&self, now: Instant) -> bool {
        self.arrived.unwrap_or(now) >= self.skeleton_at()
    }

    /// When the answer shows: when it landed, or once a skeleton that
    /// showed has stood [`MIN_SHOWN`].
    #[must_use]
    pub fn shows_at(&self) -> Option<Instant> {
        let arrived = self.arrived?;
        if arrived >= self.skeleton_at() {
            let held = self.skeleton_at().checked_add(MIN_SHOWN).unwrap_or(arrived);
            Some(arrived.max(held))
        } else {
            Some(arrived)
        }
    }

    /// Whether a frame showing this read waits for it at `now`: a local
    /// read inside its frame budget, with no answer yet.
    #[must_use]
    pub fn holds_frame(&self, now: Instant) -> bool {
        self.from.is_local() && self.arrived.is_none() && now < self.after(self.budget)
    }

    /// Until when a frame showing this read waits for it, while it does.
    #[must_use]
    pub fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        self.holds_frame(now).then(|| self.after(self.budget))
    }

    /// Whether the answer moves in (cross-fades) when it shows. A local
    /// answer inside the frame budget lands at once: the person never saw
    /// the space without it.
    #[must_use]
    pub fn arrives_with_motion(&self) -> bool {
        match self.arrived {
            Some(arrived) => !(self.from.is_local() && arrived <= self.after(self.budget)),
            None => true,
        }
    }

    /// What shows at `now`.
    #[must_use]
    pub fn step(&self, now: Instant) -> Step {
        if self.shows_at().is_some_and(|at| now >= at) {
            return Step::Shown;
        }
        if !self.skeleton_showed(now) || now < self.skeleton_at() {
            return Step::Ground;
        }
        if self.from.name().is_some() && now >= self.after(NAME_SOURCE_AFTER) {
            Step::Named
        } else {
            Step::Skeleton
        }
    }

    /// The placeholder step the answer replaces when it shows: what stood
    /// there just before.
    #[must_use]
    pub fn replaced_step(&self) -> Step {
        match self.shows_at() {
            Some(at) => self.step(at.checked_sub(Duration::from_nanos(1)).unwrap_or(at)),
            None => Step::Ground,
        }
    }

    /// The next instant after `now` at which [`Self::step`] changes; `None`
    /// once the answer shows, or while nothing will change until it lands.
    #[must_use]
    pub fn next_step_at(&self, now: Instant) -> Option<Instant> {
        let shows_at = self.shows_at();
        let mut candidates = Vec::with_capacity(3);
        if let Some(at) = shows_at {
            candidates.push(at);
        }
        let before_answer = |at: Instant| shows_at.is_none_or(|shows| at < shows);
        let skeleton = self.skeleton_at();
        if before_answer(skeleton) {
            candidates.push(skeleton);
        }
        let named = self.after(NAME_SOURCE_AFTER);
        if self.from.name().is_some() && before_answer(named) {
            candidates.push(named);
        }
        candidates.into_iter().filter(|at| *at > now).min()
    }
}

type Retry<T, E> = Arc<dyn Fn(&Arriving<T, E>) + Send + Sync>;

/// A read whose answer arrives later. Clones share one read.
///
/// Build the view with [`Self::view`]: it says what shows once the answer
/// is there, and nothing about waiting.
pub struct Arriving<T, E = String> {
    task: LatestTask<Arrival<T, E>>,
    began: Arc<Mutex<Instant>>,
    retry: Arc<Mutex<Option<Retry<T, E>>>>,
}

impl<T, E> Clone for Arriving<T, E> {
    fn clone(&self) -> Self {
        Self {
            task: self.task.clone(),
            began: Arc::clone(&self.began),
            retry: Arc::clone(&self.retry),
        }
    }
}

impl<T, E> std::fmt::Debug for Arriving<T, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Arriving").finish_non_exhaustive()
    }
}

impl<T, E> Arriving<T, E> {
    fn with_state(initial: Arrival<T, E>) -> Self {
        Self {
            task: LatestTask::new(initial),
            began: Arc::new(Mutex::new(crate::time::now())),
            retry: Arc::new(Mutex::new(None)),
        }
    }

    /// An answer already here.
    #[must_use]
    pub fn ready(value: T) -> Self {
        Self::with_state(Arrival::Ready(value))
    }

    /// A read asked now, its answer coming from `from`. Publish the answer
    /// through [`Self::writer`] or [`Self::begin`].
    #[must_use]
    pub fn pending(from: Source) -> Self {
        Self::with_state(Arrival::Pending { from })
    }

    /// A read asked at `since`, its answer coming from `from`: for a model
    /// that keeps when its read began and builds its view again meanwhile.
    #[must_use]
    pub fn pending_since(from: Source, since: Instant) -> Self {
        let arriving = Self::pending(from);
        *arriving.began.lock_recover() = since;
        arriving
    }

    /// Where the read stands, without exposing its lock.
    pub fn with<R>(&self, read: impl FnOnce(&Arrival<T, E>) -> R) -> R {
        self.task.state().with(read)
    }

    /// Where the read stands, cloned.
    #[must_use]
    pub fn get(&self) -> Arrival<T, E>
    where
        T: Clone,
        E: Clone,
    {
        self.with(Clone::clone)
    }

    /// The observable state, for [`crate::widget::ModelView`], `Text` or a
    /// callback that needs the whole arrival.
    #[must_use]
    pub fn state(&self) -> LocalState<Arrival<T, E>> {
        self.task.state()
    }

    /// Advances with every change of the read, for a widget that skips work
    /// while nothing changed.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.task.state().revision()
    }

    /// When the read now pending was asked (or when the last one was).
    #[must_use]
    pub fn began(&self) -> Instant {
        *self.began.lock_recover()
    }

    /// Which read this is: the same for every clone of it, and for no other
    /// read while it lives.
    pub(crate) fn identity(&self) -> usize {
        Arc::as_ptr(&self.began).addr()
    }

    /// The timing of the read now pending; `None` once it settled.
    #[must_use]
    pub fn clock(&self) -> Option<ArrivalClock> {
        let since = self.began();
        self.with(|arrival| {
            arrival
                .source()
                .map(|from| ArrivalClock::new(since, from.clone()))
        })
    }

    /// Until when a frame showing this read waits for it at `now`: while it
    /// is a local read inside its frame budget.
    #[must_use]
    pub fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        self.clock()?.holds_frame_until(now)
    }

    /// Whether a retry is offered when the read fails.
    #[must_use]
    pub fn can_retry(&self) -> bool {
        self.retry.lock_recover().is_some()
    }

    /// Ask again after a failure, the way the read was first asked. A read
    /// that set no way to retry ([`Self::on_retry`]) ignores this.
    pub fn retry(&self) {
        let retry = self.retry.lock_recover().clone();
        if let Some(retry) = retry {
            retry(self);
        }
    }

    /// How to ask again after a failure: `retry` restarts the read on this
    /// handle (usually through [`Self::begin`]).
    #[must_use]
    pub fn on_retry(self, retry: impl Fn(&Self) + Send + Sync + 'static) -> Self {
        *self.retry.lock_recover() = Some(Arc::new(retry));
        self
    }

    /// Say where the read stands, from the thread that holds its data (a
    /// record the window follows), with no worker to publish through. A
    /// read that settled and is pending again was asked now; one still
    /// pending keeps when it was asked, now waiting on `from`.
    pub fn stand(&self, arrival: Arrival<T, E>) {
        if arrival.is_pending() && !self.with(Arrival::is_pending) {
            self.mark_began();
        }
        self.task.state().set(arrival);
    }

    fn mark_began(&self) {
        *self.began.lock_recover() = crate::time::now();
    }
}

impl<T, E> Arriving<T, E>
where
    T: Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    /// A handle for a producer that publishes more than one answer (an
    /// invocation's updates, pages gathering). It does not restart the
    /// read: [`Self::begin`] does.
    #[must_use]
    pub fn writer(&self) -> ArrivalWriter<T, E> {
        ArrivalWriter {
            state: self.task.state().writer(),
            began: Arc::clone(&self.began),
        }
    }

    /// Restart the read, its answer now coming from `from`: every older
    /// completion becomes stale before the pending state is published.
    pub fn begin(&self, from: Source) -> ArrivalCompletion<T, E> {
        self.mark_began();
        ArrivalCompletion {
            completion: self.task.begin(Arrival::Pending { from }),
        }
    }

    /// Restart the read with `load`'s answer, run on the app runtime
    /// ([`tilo_rt::app`]), never on the calling thread. A later restart
    /// aborts it at its next await point.
    pub fn load<F>(&self, from: Source, load: F) -> bool
    where
        F: Future<Output = Result<T, E>> + Send + 'static,
    {
        self.mark_began();
        self.task
            .spawn(Arrival::Pending { from }, async move { load.await.into() })
    }

    /// A read of this device's data: `load` runs on a background worker
    /// ([`crate::background::Background`]) and a failure offers a retry that
    /// runs it again. A host that runs no background work fails it, saying
    /// so.
    #[must_use]
    pub fn local(
        name: &'static str,
        load: impl Fn() -> Result<T, E> + Send + Sync + 'static,
    ) -> Self
    where
        E: From<String>,
    {
        let load: Arc<dyn Fn() -> Result<T, E> + Send + Sync> = Arc::new(load);
        let arriving = Self::pending(Source::Local).on_retry(move |arriving| {
            arriving.read_in_background(name, Source::Local, Arc::clone(&load));
        });
        arriving.retry();
        arriving
    }

    /// Restart the read with `load`, run on a background worker, its answer
    /// coming from `from`.
    pub fn read_in_background(
        &self,
        name: &'static str,
        from: Source,
        load: Arc<dyn Fn() -> Result<T, E> + Send + Sync>,
    ) where
        E: From<String>,
    {
        let completion = self.begin(from);
        let refused = self.writer();
        let spawned = crate::background::Background::current().spawn(name, move || {
            completion.finish(load());
        });
        if let Err(unsupported) = spawned {
            refused.fail(E::from(unsupported.to_string()));
        }
    }
}

/// Publishes a read's answers without restarting it. See
/// [`Arriving::writer`].
pub struct ArrivalWriter<T, E = String> {
    state: LocalStateWriter<Arrival<T, E>>,
    began: Arc<Mutex<Instant>>,
}

impl<T, E> Clone for ArrivalWriter<T, E> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            began: Arc::clone(&self.began),
        }
    }
}

impl<T, E> ArrivalWriter<T, E> {
    /// The answer.
    pub fn ready(&self, value: T) {
        self.state.set(Arrival::Ready(value));
    }

    /// It could not be read.
    pub fn fail(&self, error: E) {
        self.state.set(Arrival::Failed(error));
    }

    /// Asked again, now: the answer comes from `from`.
    pub fn pending(&self, from: Source) {
        *self.began.lock_recover() = crate::time::now();
        self.state.set(Arrival::Pending { from });
    }

    /// The answer or the failure in `result`.
    pub fn finish(&self, result: Result<T, E>) {
        self.state.set(result.into());
    }

    /// Change the answer in place (a page joining those gathered).
    pub fn update(&self, update: impl FnOnce(&mut Arrival<T, E>)) {
        self.state.update(update);
    }
}

/// The single-use capability of one restart of a read
/// ([`Arriving::begin`]). Only the newest restart may publish.
pub struct ArrivalCompletion<T, E = String> {
    completion: TaskCompletion<Arrival<T, E>>,
}

impl<T, E> ArrivalCompletion<T, E> {
    /// Whether this is still the newest restart: a long read may stop early
    /// once it is not.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.completion.is_current()
    }

    /// Publish the answer or the failure, unless a newer restart won.
    /// Returns whether it was published.
    pub fn finish(self, result: Result<T, E>) -> bool {
        self.completion.complete(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_millis(16);

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_local_answer_inside_the_budget_shows_at_once_without_motion() {
        let t0 = crate::time::now();
        let mut clock = ArrivalClock::with_budget(t0, Source::Local, BUDGET);
        assert!(clock.holds_frame(t0), "the first frame waits for it");
        assert_eq!(clock.holds_frame_until(t0), Some(t0 + BUDGET));
        assert_eq!(clock.step(t0), Step::Ground);
        clock.arrive(at(t0, 8));
        assert!(!clock.holds_frame(at(t0, 8)));
        assert_eq!(clock.step(at(t0, 8)), Step::Shown);
        assert!(!clock.arrives_with_motion());
        assert_eq!(clock.next_step_at(at(t0, 8)), None);
    }

    #[test]
    fn a_local_read_past_its_budget_behaves_like_a_remote_one() {
        let t0 = crate::time::now();
        let mut clock = ArrivalClock::with_budget(t0, Source::Local, BUDGET);
        assert!(!clock.holds_frame(at(t0, 17)), "the frame stops waiting");
        assert_eq!(clock.step(at(t0, 100)), Step::Ground);
        assert_eq!(clock.step(at(t0, 150)), Step::Skeleton);
        // This device is not worth naming.
        assert_eq!(clock.step(at(t0, 5_000)), Step::Skeleton);
        clock.arrive(at(t0, 60));
        assert!(clock.arrives_with_motion());
    }

    #[test]
    fn a_remote_read_shows_ground_then_a_skeleton_then_its_source() {
        let t0 = crate::time::now();
        let clock = ArrivalClock::with_budget(t0, Source::host("eu1.tilo.coffee"), BUDGET);
        assert!(!clock.holds_frame(t0), "a remote read never holds a frame");
        assert_eq!(clock.step(t0), Step::Ground);
        assert_eq!(clock.step(at(t0, 149)), Step::Ground);
        assert_eq!(clock.next_step_at(t0), Some(t0 + GRACE));
        assert_eq!(clock.step(at(t0, 150)), Step::Skeleton);
        assert_eq!(
            clock.next_step_at(at(t0, 150)),
            Some(t0 + NAME_SOURCE_AFTER)
        );
        assert_eq!(clock.step(at(t0, 2_000)), Step::Named);
        assert_eq!(
            clock.next_step_at(at(t0, 2_000)),
            None,
            "nothing changes until the answer lands"
        );
    }

    #[test]
    fn a_skeleton_never_shows_for_less_than_its_minimum() {
        let t0 = crate::time::now();
        let mut clock = ArrivalClock::with_budget(t0, Source::peer("Laptop"), BUDGET);
        clock.arrive(at(t0, 200));
        assert_eq!(clock.shows_at(), Some(t0 + GRACE + MIN_SHOWN));
        assert_eq!(clock.step(at(t0, 200)), Step::Skeleton);
        assert_eq!(clock.step(at(t0, 449)), Step::Skeleton);
        assert_eq!(
            clock.next_step_at(at(t0, 200)),
            Some(t0 + GRACE + MIN_SHOWN)
        );
        assert_eq!(clock.step(at(t0, 450)), Step::Shown);
        assert_eq!(clock.replaced_step(), Step::Skeleton);
        assert!(clock.arrives_with_motion());
    }

    #[test]
    fn a_remote_answer_before_the_grace_period_shows_without_a_skeleton() {
        let t0 = crate::time::now();
        let mut clock = ArrivalClock::with_budget(t0, Source::peer("Laptop"), BUDGET);
        clock.arrive(at(t0, 90));
        assert_eq!(clock.shows_at(), Some(at(t0, 90)));
        assert_eq!(clock.step(at(t0, 90)), Step::Shown);
        assert_eq!(clock.replaced_step(), Step::Ground);
        assert!(clock.arrives_with_motion(), "it fades onto its ground");
    }

    #[test]
    fn a_late_answer_shows_when_it_lands() {
        let t0 = crate::time::now();
        let mut clock = ArrivalClock::with_budget(t0, Source::host("host"), BUDGET);
        clock.arrive(at(t0, 3_000));
        assert_eq!(clock.shows_at(), Some(at(t0, 3_000)));
        assert_eq!(clock.replaced_step(), Step::Named);
    }

    #[test]
    fn a_restart_makes_an_older_completion_stale() {
        let arriving: Arriving<u32> = Arriving::pending(Source::Local);
        let first = arriving.begin(Source::Local);
        let second = arriving.begin(Source::host("host"));
        assert!(!first.is_current());
        assert!(
            !first.finish(Ok(1)),
            "a slow answer cannot overwrite a newer read"
        );
        assert_eq!(
            arriving.with(Clone::clone),
            Arrival::Pending {
                from: Source::host("host")
            }
        );
        assert!(second.finish(Ok(2)));
        assert_eq!(arriving.with(Clone::clone), Arrival::Ready(2));
    }

    #[test]
    fn a_retry_asks_again_the_way_the_read_was_asked() {
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let arriving: Arriving<u32> =
            Arriving::pending(Source::Local).on_retry(move |arriving: &Arriving<u32>| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = arriving.begin(Source::Local);
            });
        arriving.writer().fail(String::from("disk unplugged"));
        assert!(arriving.can_retry());
        arriving.retry();
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(arriving.with(Arrival::is_pending));
    }

    #[test]
    fn a_settled_read_holds_no_frame_and_has_no_clock() {
        let arriving: Arriving<u32> = Arriving::ready(3);
        assert_eq!(arriving.clock(), None);
        assert_eq!(arriving.holds_frame_until(crate::time::now()), None);
    }

    #[test]
    fn this_device_is_never_named_as_a_source() {
        assert_eq!(Source::Local.name(), None);
        assert_eq!(Source::peer("Laptop").name(), Some("Laptop"));
        assert_eq!(Source::host("").name(), None);
    }
}
