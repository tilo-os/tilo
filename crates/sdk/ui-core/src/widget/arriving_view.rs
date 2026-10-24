//! `ArrivingView`: what shows of a read ([`crate::arriving`]) while its
//! answer is on the way, and the answer once it is there.
//!
//! The app says what shows once the data is there ([`Arriving::view`]), and
//! optionally the content's shape ([`ArrivingView::shaped`]); this view owns
//! every timing and placeholder decision:
//!
//! - The space is reserved at the shape's geometry from the first frame, so
//!   nothing moves when the answer lands in a content of that shape.
//! - A local read inside its frame budget holds the frame
//!   ([`Widget::holds_frame_until`]) and shows its answer with no
//!   placeholder and no motion.
//! - Otherwise only the reserved ground shows until [`GRACE`](crate::arriving::GRACE),
//!   then a skeleton that stays at least [`MIN_SHOWN`](crate::arriving::MIN_SHOWN),
//!   naming where the data comes from after
//!   [`NAME_SOURCE_AFTER`](crate::arriving::NAME_SOURCE_AFTER).
//! - The answer cross-fades in ([`CrossFade::arrive`]); a failure shows the
//!   error with a retry when the read offers one.
//!
//! Each step is a deadline ([`Widget::next_wake_deadline`]), never a frame
//! loop: a pending view draws nothing between its steps.
//!
//! [`ArrivalFace`] paints the same placeholder steps for immediate-mode
//! painters that hold an [`ArrivalClock`] of their own.

use std::cell::Cell;
use std::rc::Rc;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::arriving::{Arrival, ArrivalClock, Arriving, Source, Step};
use crate::motion::CrossFade;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::widget::{
    AppModel, Banner, Button, Element, ModelPath, ModelView, SkeletonFace, SkeletonShape,
    TaffyTreeExt, Widget,
};

/// The box a widget fills when it is the answer of a read: what an
/// [`ArrivingView`] showing it must have reserved. A list row reports its
/// kind, a media card its aspect.
pub trait ReservedShape {
    fn reserved_shape(&self) -> SkeletonShape;
}

/// What an [`ArrivingView`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Showing {
    /// The reserved space, and the placeholder steps in it.
    Placeholder,
    /// The answer.
    Answer,
    /// Why it could not be read.
    Failure,
}

/// What the inner view is built for: what shows, and which answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reveal {
    showing: Showing,
    revision: u64,
    /// Whether a failure shows its face here: the first box in its region
    /// standing for its read does; the others keep their reserved space.
    speaks: bool,
}

/// The placeholder steps of a read, painted in a rect: the immediate-mode
/// counterpart of [`ArrivingView`], for a painter that draws its own rows.
#[derive(Clone, Debug)]
pub struct ArrivalFace<'a> {
    shape: SkeletonShape,
    step: Step,
    source: &'a Source,
    /// Whether this placeholder is the one in its region that says where
    /// its data comes from ([`NamedOnce`]).
    names: bool,
}

/// The sources a pending region already named: each source is named once
/// per region (a list's first row from it), not on every row.
#[derive(Clone, Debug, Default)]
pub struct NamedOnce(Vec<Source>);

impl NamedOnce {
    /// Whether `source` is named here: the first time it is asked.
    pub fn claim(&mut self, source: &Source) -> bool {
        if self.0.contains(source) {
            return false;
        }
        self.0.push(source.clone());
        true
    }
}

impl<'a> ArrivalFace<'a> {
    /// What `clock`'s read shows at `now`, in `shape`.
    #[must_use]
    pub fn new(shape: SkeletonShape, clock: &'a ArrivalClock, now: Instant) -> Self {
        Self {
            shape,
            step: clock.step(now),
            source: clock.source(),
            names: true,
        }
    }

    /// The face of `step` itself, for the placeholder an arrival fades out.
    #[must_use]
    pub fn at_step(shape: SkeletonShape, step: Step, source: &'a Source) -> Self {
        Self {
            shape,
            step,
            source,
            names: true,
        }
    }

    /// Whether this placeholder says where its data comes from, once it
    /// would: a region names each source once.
    #[must_use]
    pub fn naming(mut self, names: bool) -> Self {
        self.names = names;
        self
    }

    /// Name the source here only if no placeholder before it in `region`
    /// named it: one line per source per region.
    #[must_use]
    pub fn named_once(self, region: &mut NamedOnce) -> Self {
        let names = self.step == Step::Named && region.claim(self.source);
        self.naming(names)
    }

    /// The step it paints.
    #[must_use]
    pub fn step(&self) -> Step {
        self.step
    }

    /// The height the content will take at `width`: the space a painter
    /// reserves for it.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        SkeletonFace::new(self.shape).height(text, theme, width)
    }

    /// What it says about where the data comes from, once it says it.
    #[must_use]
    pub fn caption(&self) -> Option<String> {
        if self.step != Step::Named || !self.names {
            return None;
        }
        let name = self.source.name()?;
        let mut args = fluent::FluentArgs::new();
        args.set("source", name.to_owned());
        Some(crate::i18n::I18nManager::lookup(
            "arriving-from",
            Some(&args),
        ))
    }

    /// Paint the step in `rect`: nothing on the ground, the skeleton, or the
    /// skeleton with a line saying where the data comes from in a place the
    /// shape keeps for it. All of it inside `rect`, so a step never moves
    /// anything.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        match self.step {
            Step::Ground | Step::Shown => {}
            Step::Skeleton | Step::Named => {
                let caption = self.caption();
                SkeletonFace::new(self.shape).paint_naming(ctx, rect, caption.as_deref());
            }
        }
    }
}

/// Where a view stands, which read it stands for and what it waits on, for
/// the views around it to tell whether a source is named already
/// ([`names_source`]) and whether its read's failure shows already
/// ([`speaks_failure`]).
#[derive(Debug, Default)]
struct NamingEntry {
    rect: Cell<Rect>,
    /// The read the view shows ([`Arriving::identity`]).
    read: Cell<usize>,
    /// The source while the view shows its placeholder.
    pending: std::cell::RefCell<Option<Source>>,
    /// Whether the view shows its read's failure.
    failed: Cell<bool>,
}

impl NamingEntry {
    /// Whether the view stands in a region: its placeholder or its failure
    /// shows, in a box with room.
    fn stands(&self) -> bool {
        (self.pending.borrow().is_some() || self.failed.get()) && self.rect.get().height() > 0.0
    }
}

thread_local! {
    /// Every pending view on this thread, weakly.
    static PENDING_VIEWS: std::cell::RefCell<Vec<std::rc::Weak<NamingEntry>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn register_naming(entry: &Rc<NamingEntry>) {
    PENDING_VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        views.retain(|view| view.strong_count() > 0);
        views.push(Rc::downgrade(entry));
    });
}

/// How far apart two pending boxes stand and still be one region: no
/// farther than the gap between groups (a list's rows, a grid's cards),
/// not the space between two parts of a page.
fn region_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.group)
}

/// Whether two pending boxes stand in one region: stacked or side by side
/// with no more than `gap` between them.
fn adjoin(a: Rect, b: Rect, gap: f64) -> bool {
    let overlap_x = a.x0 < b.x1 && b.x0 < a.x1;
    let overlap_y = a.y0 < b.y1 && b.y0 < a.y1;
    let gap_y = (b.y0 - a.y1).max(a.y0 - b.y1);
    let gap_x = (b.x0 - a.x1).max(a.x0 - b.x1);
    (overlap_x && gap_y <= gap) || (overlap_y && gap_x <= gap)
}

/// Whether the view `me` names its source: of the boxes in its region (the
/// boxes it adjoins, the boxes those adjoin, and so on, whatever they wait
/// on), it is the first in reading order that waits on its source. A list
/// or a grid names each source once, wherever its rows or cards come from.
fn names_source(me: &Rc<NamingEntry>, theme: &Theme) -> bool {
    let Some(source) = me.pending.borrow().clone() else {
        return false;
    };
    if source.name().is_none() {
        return false;
    }
    first_in_region(me, theme, |other| {
        other.pending.borrow().as_ref() == Some(&source)
    })
}

/// Whether the view `me`, its read failed, shows the failure: of the boxes
/// in its region, it is the first in reading order that stands for its
/// read. Rows sharing one read show one failure, with one way to ask
/// again; rows with reads of their own each show theirs.
fn speaks_failure(me: &Rc<NamingEntry>, theme: &Theme) -> bool {
    let read = me.read.get();
    first_in_region(me, theme, |other| other.read.get() == read)
}

/// Whether no box in `me`'s region that is `alike` stands before it in
/// reading order: `(top, left)`.
fn first_in_region(
    me: &Rc<NamingEntry>,
    theme: &Theme,
    alike: impl Fn(&NamingEntry) -> bool,
) -> bool {
    let mine = me.rect.get();
    let others: Vec<(Rect, bool)> = PENDING_VIEWS.with(|views| {
        views
            .borrow()
            .iter()
            .filter_map(std::rc::Weak::upgrade)
            .filter(|view| !Rc::ptr_eq(view, me) && view.stands())
            .map(|view| (view.rect.get(), alike(&view)))
            .collect()
    });
    let gap = region_gap(theme);
    let before = |rect: Rect| (rect.y0, rect.x0) < (mine.y0, mine.x0);
    let mut reached = vec![false; others.len()];
    let mut frontier = vec![mine];
    while let Some(edge) = frontier.pop() {
        for ((rect, like), seen) in others.iter().zip(reached.iter_mut()) {
            if *seen || !adjoin(edge, *rect, gap) {
                continue;
            }
            if *like && before(*rect) {
                return false;
            }
            *seen = true;
            frontier.push(*rect);
        }
    }
    true
}

/// The space a pending read reserves: as tall as its content's shape, and
/// empty. Its [`ArrivingView`] paints the placeholder steps over it.
struct Reserve {
    shape: SkeletonShape,
    rect: Rect,
    theme: Option<Theme>,
    laid_out: bool,
}

impl Reserve {
    /// Nothing, in no space: what a placeholder's read that never answers
    /// would show.
    fn empty() -> Self {
        Self::of(SkeletonShape::Text { lines: 0 })
    }

    fn of(shape: SkeletonShape) -> Self {
        Self {
            shape,
            rect: Rect::ZERO,
            theme: None,
            laid_out: false,
        }
    }
}

impl Widget for Reserve {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let fills = self.shape.fills();
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_grow: if fills { 1.0 } else { 0.0 },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = &self.theme else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let height = SkeletonFace::new(self.shape).height(text_renderer, theme, f64::from(width));
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.laid_out = true;
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn is_layout_dirty(&self) -> bool {
        !self.laid_out
    }

    fn name(&self) -> &'static str {
        "Reserve"
    }
}

/// What a view reads of its read, whatever its answer's type: the view
/// itself is not generic, so the toolkit builds it once for every app.
trait ArrivalRead {
    fn revision(&self) -> u64;
    fn began(&self) -> Instant;
    /// Where a pending read's answer comes from; `None` once it landed.
    fn source(&self) -> Option<Source>;
    /// What its answer shows once it landed; `None` while pending.
    fn landed(&self) -> Option<Showing>;
    fn holds_frame_until(&self, now: Instant) -> Option<Instant>;
}

impl<T, E> ArrivalRead for Arriving<T, E> {
    fn revision(&self) -> u64 {
        Arriving::revision(self)
    }

    fn began(&self) -> Instant {
        Arriving::began(self)
    }

    fn source(&self) -> Option<Source> {
        self.with(|arrival| arrival.source().cloned())
    }

    fn landed(&self) -> Option<Showing> {
        self.with(|arrival| match arrival {
            Arrival::Pending { .. } => None,
            Arrival::Ready(_) => Some(Showing::Answer),
            Arrival::Failed(_) => Some(Showing::Failure),
        })
    }

    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        Arriving::holds_frame_until(self, now)
    }
}

/// A read's view. See the module docs; build one with [`Arriving::view`].
pub struct ArrivingView {
    arriving: Box<dyn ArrivalRead>,
    shape: Rc<Cell<SkeletonShape>>,
    reveal: Rc<Cell<Reveal>>,
    inner: ModelView<Reveal>,
    /// The box the read reserves: the content's geometry from the first
    /// frame. The placeholder, the answer and a failure all stand in it.
    reserve: Reserve,
    /// This view's place among the pending views naming their sources.
    naming: Rc<NamingEntry>,
    /// The revision of the read last looked at.
    observed: Option<u64>,
    /// The timing of the read pending now, or of the last one.
    clock: Option<ArrivalClock>,
    fade: CrossFade<Showing>,
    /// The placeholder step the shown content replaced: what fades out.
    replaced: Step,
    /// The placeholder step the last frame painted.
    drawn_step: Option<Step>,
    /// Whether the placeholder last drawn named its source: the face its
    /// answer fades out keeps saying it.
    drawn_naming: bool,
    /// Whether layout moved which box shows a shared read's failure, for
    /// the next frame to build it there.
    respoken: bool,
    theme: Option<Theme>,
    rect: Rect,
    /// Whether a read can still answer this view: a still capture waits
    /// for it. A [`placeholder`](ArrivingView::placeholder) stands for data
    /// whose answer comes with a new view, not into this one.
    answerable: bool,
}

impl ArrivingView {
    /// The view of `arriving`, showing `build`'s content once the answer is
    /// there.
    pub fn new<T: 'static, E: std::fmt::Display + 'static>(
        arriving: &Arriving<T, E>,
        mut build: impl FnMut(&T) -> Element + 'static,
    ) -> Self {
        let shape = Rc::new(Cell::new(SkeletonShape::Page));
        let reveal = Rc::new(Cell::new(Reveal {
            showing: Showing::Placeholder,
            revision: 0,
            speaks: true,
        }));
        let projected = Rc::clone(&reveal);
        let built_shape = Rc::clone(&shape);
        let source = arriving.clone();
        let inner = ModelView::from_document(
            move |_, _| projected.get(),
            move |reveal: &Reveal| -> Element {
                let empty = || -> Element { Box::new(Reserve::empty()) };
                match reveal.showing {
                    Showing::Placeholder => empty(),
                    Showing::Answer => source
                        .with(|arrival| arrival.ready().map(&mut build))
                        .unwrap_or_else(empty),
                    Showing::Failure if !reveal.speaks => empty(),
                    Showing::Failure => source
                        .with(|arrival| arrival.failure().map(ToString::to_string))
                        .map_or_else(empty, |message| {
                            failure(&source, message, built_shape.get())
                        }),
                }
            },
        );
        let naming = Rc::new(NamingEntry::default());
        naming.read.set(arriving.identity());
        register_naming(&naming);
        Self {
            arriving: Box::new(arriving.clone()),
            shape,
            reveal,
            inner,
            reserve: Reserve::of(SkeletonShape::Page),
            naming,
            observed: None,
            clock: None,
            fade: CrossFade::new(Showing::Placeholder),
            replaced: Step::Ground,
            drawn_step: None,
            drawn_naming: false,
            respoken: false,
            theme: None,
            rect: Rect::ZERO,
            answerable: true,
        }
    }
}

/// The reserved box as the error, with a retry when the read offers one:
/// a banner where the box holds one, a row of the box's own kind where it
/// is a row's (or a line's).
fn failure<T: 'static, E: 'static>(
    arriving: &Arriving<T, E>,
    message: String,
    shape: SkeletonShape,
) -> Element {
    let failed = crate::i18n::I18nManager::lookup_plain("arriving-failed");
    let retry = arriving.can_retry().then(|| {
        let again = arriving.clone();
        Button::new("arriving.retry", "arriving-retry").on_view(move || again.retry())
    });
    let compact = match shape {
        SkeletonShape::Row(_) | SkeletonShape::Chip => true,
        SkeletonShape::Text { lines } => lines <= 2,
        SkeletonShape::Media(_) | SkeletonShape::Page => false,
    };
    if compact {
        let mut row = crate::widget::ListRow::new("arriving.failed", failed);
        if matches!(shape, SkeletonShape::Row(row) if row.secondary_lines > 0) {
            row = row.secondary(message);
        } else {
            row = row.tooltip(message);
        }
        if let Some(retry) = retry {
            row = row.trailing(Box::new(
                retry.control_size(crate::widget::ControlSize::Small),
            ));
        }
        return Box::new(row);
    }
    let mut banner = Banner::error(failed).with_message(message);
    if let Some(retry) = retry {
        banner = banner.with_action(retry);
    }
    Box::new(banner)
}

impl ArrivingView {
    /// The framework's placeholder for data on its way from `from`, timed
    /// from when it is built: for a view rebuilt from a snapshot that knows
    /// the data is pending but holds no read of its own. It reserves
    /// `shape`'s geometry and steps through the same placeholder as any
    /// [`ArrivingView`].
    #[must_use]
    pub fn placeholder(from: Source, shape: SkeletonShape) -> Element {
        Self::placeholder_since(from, crate::time::now(), shape)
    }

    /// [`Self::placeholder`] for data asked for at `since`: its steps keep
    /// their timing however often the view around it is built again.
    #[must_use]
    pub fn placeholder_since(from: Source, since: Instant, shape: SkeletonShape) -> Element {
        let pending = Arriving::<(), String>::pending_since(from, since);
        let mut view = Self::new(&pending, |()| -> Element { Box::new(Reserve::empty()) });
        view.answerable = false;
        Box::new(view.shaped(shape))
    }
}

impl ArrivingView {
    /// Say what the content looks like, so the reserved space and the
    /// skeleton take its geometry and nothing moves when it lands.
    #[must_use]
    pub fn shaped(self, shape: SkeletonShape) -> Self {
        self.shape.set(shape);
        self
    }

    /// The answer's view, while it shows: what a host that stands for the
    /// view (a window's root) reaches through it. A traversal meets it as
    /// the view's child; the view itself answers for nothing of it.
    #[must_use]
    pub fn answer(&self) -> Option<&dyn Widget> {
        (self.showing() == Showing::Answer).then(|| self.inner.child())
    }

    /// Whether the answer shows at `now`, its cross-fade over: nothing of
    /// the placeholder is left to paint.
    #[must_use]
    pub fn settled(&self, now: Instant) -> bool {
        self.showing() == Showing::Answer && !self.fade.is_moving(now)
    }

    /// Take the answer's view out of a settled view ([`Self::settled`]),
    /// for a host that shows it in the view's place from now on (a window's
    /// root, once its record arrived). Nothing once taken, or before.
    pub fn take_answer(&mut self, now: Instant) -> Option<Element> {
        self.settled(now)
            .then(|| self.inner.replace_child(Box::new(Reserve::empty())))
    }

    /// [`Self::answer`], to change.
    pub fn answer_mut(&mut self) -> Option<&mut dyn Widget> {
        if self.showing() == Showing::Answer {
            Some(self.inner.child_mut())
        } else {
            None
        }
    }

    /// What shows now.
    #[must_use]
    pub fn showing(&self) -> Showing {
        self.reveal.get().showing
    }

    /// The placeholder step on screen at `now` while the placeholder shows;
    /// [`Step::Shown`] once the answer or the failure does.
    #[must_use]
    pub fn step(&self, now: Instant) -> Step {
        if self.showing() != Showing::Placeholder {
            return Step::Shown;
        }
        match &self.clock {
            Some(clock) => match clock.step(now) {
                // Due, but not revealed until the next sync.
                Step::Shown => clock.replaced_step(),
                step => step,
            },
            None => Step::Ground,
        }
    }

    /// Whether the read moved since the view last looked at it.
    fn unobserved(&self) -> bool {
        self.observed != Some(self.arriving.revision())
    }

    /// Look at the read, and show its answer once it is due.
    fn reconcile(&mut self, now: Instant) {
        let revision = self.arriving.revision();
        if self.observed != Some(revision) {
            self.observed = Some(revision);
            let pending = self.arriving.source();
            match pending {
                // An answer on screen stays until the next one lands, and
                // that one replaces it as it lands: no placeholder stood
                // between them.
                Some(_) if self.showing() == Showing::Answer => self.clock = None,
                Some(from) => {
                    let since = self.arriving.began();
                    let fresh = self
                        .clock
                        .as_ref()
                        .is_none_or(|clock| clock.since() != since || clock.has_arrived());
                    if fresh {
                        self.clock = Some(ArrivalClock::new(since, from));
                    }
                    // A failure gives way to the read asked again.
                    if self.showing() == Showing::Failure {
                        self.set_reveal(Showing::Placeholder, revision);
                        self.fade = CrossFade::new(Showing::Placeholder);
                    }
                }
                None => {
                    if let Some(clock) = self.clock.as_mut() {
                        clock.arrive(now);
                    }
                }
            }
        }
        self.reveal_when_due(now, revision);
    }

    fn reveal_when_due(&mut self, now: Instant, revision: u64) {
        let Some(showing) = self.arriving.landed() else {
            return;
        };
        let current = self.reveal.get();
        if current.showing == showing && current.revision == revision {
            return;
        }
        let due = self
            .clock
            .as_ref()
            .and_then(ArrivalClock::shows_at)
            .is_none_or(|at| now >= at);
        if !due {
            return;
        }
        let from_placeholder = current.showing == Showing::Placeholder;
        let moves = from_placeholder
            && self.theme.is_some()
            && self
                .clock
                .as_ref()
                .is_some_and(ArrivalClock::arrives_with_motion);
        self.replaced = self
            .clock
            .as_ref()
            .map_or(Step::Ground, ArrivalClock::replaced_step);
        self.set_reveal(showing, revision);
        match (&self.theme, moves) {
            (Some(theme), true) => self.fade.arrive(showing, theme.motion(), now),
            _ => self.fade = CrossFade::new(showing),
        }
    }

    fn set_reveal(&self, showing: Showing, revision: u64) {
        let speaks = showing != Showing::Failure || self.speaks();
        self.reveal.set(Reveal {
            showing,
            revision,
            speaks,
        });
    }

    /// Whether this view shows its read's failure ([`speaks_failure`]);
    /// before its first layout, it does.
    fn speaks(&self) -> bool {
        self.theme
            .as_ref()
            .is_none_or(|theme| speaks_failure(&self.naming, theme))
    }

    /// Whether this frame's layout moved a shared read's failure to another
    /// box while this one still holds the face it built: it neither paints
    /// it nor offers it, so the first frame already shows one failure, and
    /// the next frame builds nothing here.
    fn failure_moved_away(&self) -> bool {
        let reveal = self.reveal.get();
        self.respoken && reveal.showing == Showing::Failure && !reveal.speaks
    }

    /// Move a shared read's failure to the box that shows it now. True when
    /// it moved.
    fn respeak(&self) -> bool {
        let current = self.reveal.get();
        if current.showing != Showing::Failure {
            return false;
        }
        let speaks = self.speaks();
        if speaks == current.speaks {
            return false;
        }
        self.reveal.set(Reveal { speaks, ..current });
        true
    }

    fn paint_step(&self, ctx: &mut PaintCtx<'_>, step: Step, names: bool) {
        let local = Source::Local;
        let source = self.clock.as_ref().map_or(&local, ArrivalClock::source);
        let rect = SkeletonFace::new(self.shape.get()).painted_rect(self.rect, ctx.theme);
        ArrivalFace::at_step(self.shape.get(), step, source)
            .naming(names)
            .paint(ctx, rect);
    }

    /// Tell the views around this one what it waits on, while its
    /// placeholder shows.
    fn note_pending(&self) {
        let pending = (self.showing() == Showing::Placeholder)
            .then(|| self.clock.as_ref().map(|clock| clock.source().clone()))
            .flatten();
        *self.naming.pending.borrow_mut() = pending;
        self.naming.failed.set(self.showing() == Showing::Failure);
    }

    /// Check that the answer is of the shape its box reserved: a row of
    /// another kind, a card of another aspect, is a mistake the view would
    /// otherwise show as a box that does not fit.
    fn check_answer_shape(&self) {
        if self.showing() != Showing::Answer {
            return;
        }
        if let Some(own) = super::capability::capability::<dyn ReservedShape>(self.inner.child()) {
            let reserved = self.shape.get();
            debug_assert!(
                own.reserved_shape() == reserved,
                "an answer of shape {:?} landed in a box reserved as {reserved:?}",
                own.reserved_shape()
            );
        }
    }
}

/// Paint `paint` at `opacity`, in a layer only while it is part-way.
fn layered(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    opacity: f32,
    paint: impl FnOnce(&mut Scene),
) {
    if opacity <= 0.0 {
        return;
    }
    let part = opacity < 1.0;
    if part {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &clip.inflate(2.0, 2.0),
        );
    }
    paint(scene);
    if part {
        scene.pop_layer();
    }
}

impl Widget for ArrivingView {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.reconcile(crate::motion::frame_now());
        self.respeak();
        self.respoken = false;
        self.inner.sync(state, path);
        self.note_pending();
        self.check_answer_shape();
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.reconcile(crate::motion::frame_now());
        self.respeak();
        self.respoken = false;
        let changed = self.inner.update(state, path);
        self.note_pending();
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let shape = self.shape.get();
        self.reserve.shape = shape;
        let reserve = self.reserve.build_layout(taffy, theme, node_map);
        let content = self.inner.build_layout(taffy, theme, node_map);
        // The content lies in the reserved box whatever size it would take
        // of its own: the placeholder, the answer and a failure share one
        // geometry, so nothing moves when the read lands.
        if let Ok(own) = taffy.style(content) {
            let mut boxed = own.clone();
            boxed.position = taffy::Position::Absolute;
            boxed.inset = taffy::Rect {
                left: length(0.0_f32),
                right: length(0.0_f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            };
            boxed.size = Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            };
            boxed.margin = taffy::Rect::zero();
            let _ = taffy.set_style(content, boxed);
        }
        let style = Style {
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_grow: if shape.fills() { 1.0 } else { 0.0 },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_node(style, &[reserve, content], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.naming.rect.set(self.rect);
        // A view built after its read failed learns where it stands only
        // now: the next frame builds the failure in the box that shows it.
        self.respoken |= self.respeak();
        if let Ok(reserve) = taffy.child_at_index(node, 0) {
            self.reserve
                .apply_layout(taffy, reserve, self.rect.origin());
        }
        if let Ok(content) = taffy.child_at_index(node, 1) {
            self.inner.apply_layout(taffy, content, self.rect.origin());
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = crate::motion::frame_now();
        let step = self.step(now);
        self.drawn_step = Some(step);
        if presence.should_cull(self.rect) {
            return;
        }
        if self.showing() == Showing::Placeholder {
            self.drawn_naming = names_source(&self.naming, theme);
            self.paint_step(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                step,
                self.drawn_naming,
            );
            return;
        }
        if let Some((_, opacity)) = self.fade.outgoing(now) {
            let replaced = self.replaced;
            let names = self.drawn_naming;
            let rect = self.rect;
            layered(scene, transform, rect, opacity, |scene| {
                self.paint_step(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    replaced,
                    names,
                );
            });
        }
        if self.failure_moved_away() {
            return;
        }
        let incoming = self.fade.incoming(now);
        let rect = self.rect;
        let inner = &mut self.inner;
        layered(scene, transform, rect, incoming, |scene| {
            inner.draw(scene, transform, text_renderer, theme, presence);
        });
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if self.failure_moved_away() {
            return;
        }
        if let Ok(child) = ctx.taffy.child_at_index(ctx.node_id, 1) {
            self.inner
                .handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.failure_moved_away() {
            return Vec::new();
        }
        vec![&self.inner]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if !self.failure_moved_away() {
            visitor(&self.inner);
        }
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.reserve);
        visitor(&self.inner);
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.inner]
    }

    fn name(&self) -> &'static str {
        "ArrivingView"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }

    fn take_focus_reset(&mut self) -> bool {
        self.inner.take_focus_reset()
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.respoken
            || self.fade.is_moving(now)
            || self.drawn_step.is_some_and(|drawn| drawn != self.step(now))
            || self.inner.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        let now = crate::motion::frame_now();
        self.respoken
            || self.unobserved()
            || self.drawn_step != Some(self.step(now))
            || self.fade.is_moving(now)
            || self.inner.needs_redraw()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = crate::time::now();
        // A read that moved since the view last looked (said on this thread,
        // with no worker to wake the window) is due now.
        if self.unobserved() {
            return Some(now);
        }
        let own = self.clock.as_ref().and_then(|clock| {
            // A reveal that waits only for the next sync is due now.
            match clock.shows_at() {
                Some(at) if self.showing() == Showing::Placeholder => Some(at.max(now)),
                Some(_) => None,
                None => clock.next_step_at(now),
            }
        });
        // A local read holding the frame wakes the window when its budget
        // runs out, so the frame it held is drawn then.
        let held = self.arriving.holds_frame_until(now);
        own.into_iter()
            .chain(held)
            .chain(self.inner.next_wake_deadline())
            .min()
    }

    fn serve_deadline(&mut self, now: Instant) -> bool {
        let before = (self.step(now), self.showing());
        let unobserved = self.unobserved();
        let inner = self.inner.serve_deadline(now);
        self.reconcile(now);
        inner
            || unobserved
            || before != (self.step(now), self.showing())
            || self.drawn_step != Some(self.step(now))
    }

    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        self.arriving
            .holds_frame_until(now)
            .into_iter()
            .chain(self.inner.holds_frame_until(now))
            .max()
    }

    fn has_pending_visual_work(&self) -> bool {
        (self.answerable && self.showing() == Showing::Placeholder)
            || self.fade.is_moving(crate::time::now())
            || self.inner.has_pending_visual_work()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if self.showing() != Showing::Placeholder {
            return None;
        }
        let now = crate::time::now();
        let local = Source::Local;
        let source = self.clock.as_ref().map_or(&local, ArrivalClock::source);
        let face = ArrivalFace::at_step(self.shape.get(), self.step(now), source);
        let name = face
            .caption()
            .unwrap_or_else(|| crate::i18n::I18nManager::lookup_plain("arriving-loading"));
        Some(
            AccessNode::new(Role::Generic)
                .with_name(name)
                .with_state(AccessState::new().busy(true)),
        )
    }
}

impl<T: 'static, E: std::fmt::Display + 'static> Arriving<T, E> {
    /// The view of this read: `build`'s content once the answer is there.
    /// Every placeholder and timing on the way is the framework's
    /// ([`ArrivingView`]).
    pub fn view(&self, build: impl FnMut(&T) -> Element + 'static) -> ArrivingView {
        ArrivingView::new(self, build)
    }
}

#[cfg(test)]
#[path = "arriving_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "arrival_geometry_tests.rs"]
mod geometry_tests;
