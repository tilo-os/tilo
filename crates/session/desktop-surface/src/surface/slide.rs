//! The slide a strip or workspace change is drawn with: the view shown
//! before leaves along the vertical axis while the one chosen arrives from
//! the side it lies on.
//!
//! One strip is on screen at a time and a strip is a full-height screen, so
//! going to another strip is a move up or down the deck (`motion::Snap`, the
//! paged-strip settle) and going to another workspace is a whole-view change
//! one level out (`motion::Drill::WORKSPACE`). Both are drawn the same way
//! here: the layout places the strip chosen, and this draws the strip shown
//! before over it, at the pose the motion gives, until the two come to rest.
//!
//! Only a hand moves the view. A pointer's click on the switcher, the
//! workspace menu or a banner follows `Pointer`; a finger follows `Gesture`,
//! and while fingers carry the deck the strip they landed on is drawn at
//! exactly how far they have travelled, so the deck is under the hand. A
//! keyboard chord or an action lands, as
//! `docs/architecture/window-management.md` says, so neither starts a slide.
//!
//! The change itself is presentation only: the strip chosen is laid out and
//! takes input from its first frame, and the strip left behind is drawn from
//! the place it held in the last frame, so nothing of it is rebuilt to draw
//! it going.

use std::collections::HashMap;

use tilo_desktop_model::StripId;
use ui::kurbo::{Affine, Rect};
use ui::motion::{Direction, Drill, MotionValue, Pose, Snap, unit_opacity};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::{GroupRow, Widget};

/// Which change a slide carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlideKind {
    /// Another strip of the same workspace: the deck moves by as many strips as were stepped, along
    /// its own axis.
    Strip,
    /// Another workspace: a whole-view change one level out.
    Workspace,
}

/// What started a slide, and which way it goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Started {
    pub(crate) kind: SlideKind,
    /// `1` toward the end of the list (a later workspace, the strip below),
    /// `-1` toward its start.
    pub(crate) toward: f64,
    /// How many steps were taken: one workspace is one step, and a step of several strips moves the
    /// deck that many screens.
    pub(crate) steps: f64,
    pub(crate) source: MotionSource,
}

impl Started {
    /// The timing this change moves with; `None` when it lands at once.
    fn timing(self, motion: EffectiveMotion) -> Option<ui::motion::Timing> {
        let timing = match self.kind {
            SlideKind::Workspace => {
                Drill::WORKSPACE.timing(Direction::of(self.toward > 0.0), motion, self.source)
            }
            SlideKind::Strip => {
                Snap::SPEC.timing_over(Direction::Enter, motion, self.source, self.steps)
            }
        };
        (!timing.is_instant()).then_some(timing)
    }

    /// The pose of the view shown before, `p` from `0` where it stood to `1`
    /// fully gone, over a view `width` by `height`.
    ///
    /// A workspace goes along the shared axis at `Drill::WORKSPACE`'s travel,
    /// a direction cue and not a whole screen, as the old shell drew it. A
    /// strip is a whole screen: the deck slides the view's own height.
    fn leaving_pose(self, p: f64, width: f64, height: f64, motion: EffectiveMotion) -> Pose {
        let p = p.clamp(0.0, 1.0);
        match self.kind {
            SlideKind::Workspace => Drill::pose(-self.toward * p, width, motion),
            SlideKind::Strip if motion.reduced() => Pose {
                opacity: unit_opacity(1.0 - p),
                transform: Affine::IDENTITY,
            },
            SlideKind::Strip => Pose {
                opacity: unit_opacity(1.0),
                transform: Affine::translate((0.0, -self.toward * p * height)),
            },
        }
    }
}

/// A slide running on its motion.
#[derive(Debug)]
struct Running {
    started: Started,
    /// The strip whose last drawn place is the view that leaves.
    from: StripId,
    /// `0` when the change was made, `1` once the view is in place.
    progress: MotionValue<f64>,
}

/// Fingers carrying the deck: the strip they landed on, and how far they have carried it since, in
/// the strip's height, positive toward the strips below.
#[derive(Clone, Copy, Debug)]
struct Lift {
    from: StripId,
    travel: f64,
}

/// What the surface draws of a change, and the place each strip stood in its last frame.
#[derive(Default)]
pub struct Slides {
    /// Where each strip was last laid out, by strip: what the view that leaves is drawn at.
    laid: HashMap<StripId, Rect>,
    running: Option<Running>,
    /// Fingers carrying the deck, while they are down.
    lift: Option<Lift>,
    /// The strip the surface showed at the last change.
    shown: Option<StripId>,
}

impl Slides {
    /// Where `strip` was laid out this frame.
    pub(crate) fn laid(&mut self, strip: StripId, rect: Rect) {
        self.laid.insert(strip, rect);
    }

    /// A change from `from` to `to` at `started` : starts or follows the slide the frames draw.
    /// `to` is the strip now shown; `None` says the frame asked for it before the change, which
    /// starts nothing.
    pub(crate) fn change(
        &mut self,
        started: Option<Started>,
        from: Option<StripId>,
        to: Option<StripId>,
        motion: EffectiveMotion,
        now: Instant,
    ) {
        let Some(started) = started else {
            return;
        };
        let was = self.shown;
        self.shown = to;
        let (Some(from), Some(to)) = (from, to) else {
            self.running = None;
            return;
        };
        if was == Some(from) && from == to {
            // The place already shown, asked for again: nothing was left.
            self.running = None;
            return;
        }
        // A slide that has come to rest is gone: the next change starts from what is on screen,
        // never from where the last one ended.
        if self.running.as_ref().is_some_and(|run| !run.is_moving(now)) {
            self.running = None;
        }
        if let Some(running) = self.running.as_mut()
            && running.follow(started, motion, now)
        {
            return;
        }
        self.running = Running::start(started, from, motion, now);
    }

    /// Fingers landed on `strip` , which the surface shows now, to carry the deck.
    pub fn begin_lift(&mut self, strip: Option<StripId>) {
        self.running = None;
        self.shown = strip;
        self.lift = strip.map(|from| Lift { from, travel: 0.0 });
    }

    /// The fingers have carried the deck `travel` (in the strip's height,
    /// positive toward the strips below).
    pub fn lift_to(&mut self, travel: f64) {
        if let Some(lift) = self.lift.as_mut() {
            lift.travel = travel;
        }
    }

    pub fn is_moving(&self, now: Instant) -> bool {
        self.lift.is_some() || self.running.as_ref().is_some_and(|run| run.is_moving(now))
    }

    /// Forget a slide already drawn: the strip left behind had no place to leave from.
    pub fn drop_running(&mut self) {
        self.running = None;
    }

    /// Where the leaving strip is drawn this frame, in pixels from where it stood: `None` when
    /// nothing is leaving. A slide across frames reads here as a value between two places, never as
    /// a jump.
    #[must_use]
    pub fn leaving_offset(&self, now: Instant) -> Option<f64> {
        let (_from, _rect, pose) = self.leaving_at(EffectiveMotion::default(), now)?;
        let drawn = pose.transform.translation();
        Some(if drawn.y == 0.0 { drawn.x } else { drawn.y })
    }

    /// The leaving view to draw this frame: the strip, the place it was laid out at and its pose.
    /// `None` once the change has come to rest, or while nothing is moving.
    fn leaving_at(&self, motion: EffectiveMotion, now: Instant) -> Option<(StripId, Rect, Pose)> {
        if let Some(lift) = self.lift {
            let rect = self.laid.get(&lift.from).copied()?;
            let pose = Pose {
                opacity: unit_opacity(1.0),
                transform: Affine::translate((0.0, lift.travel.clamp(-1.0, 1.0) * rect.height())),
            };
            return Some((lift.from, rect, pose));
        }
        let running = self.running.as_ref()?;
        if !running.is_moving(now) {
            return None;
        }
        let rect = self.laid.get(&running.from).copied()?;
        let p = running.progress.get(now);
        Some((
            running.from,
            rect,
            running
                .started
                .leaving_pose(p, rect.width(), rect.height(), motion),
        ))
    }

    /// The leaving view to draw this frame, dropping a slide that has come to rest.
    pub(crate) fn leaving(
        &mut self,
        motion: EffectiveMotion,
        now: Instant,
    ) -> Option<(StripId, Rect, Pose)> {
        let leaving = self.leaving_at(motion, now);
        if leaving.is_none() {
            // A run that has come to rest is gone: the next change starts from what is on screen.
            self.running = None;
        }
        leaving
    }
}

impl Running {
    /// The slide from `from` toward `started` 's direction, as its source moves it; `None` when the
    /// change lands at once.
    fn start(
        started: Started,
        from: StripId,
        motion: EffectiveMotion,
        now: Instant,
    ) -> Option<Self> {
        let timing = started.timing(motion)?;
        let mut progress = MotionValue::new(0.0);
        progress.set(1.0, timing, now);
        Some(Self {
            started,
            from,
            progress,
        })
    }

    fn is_moving(&self, now: Instant) -> bool {
        self.progress.is_moving(now)
    }

    /// A change asked for mid-flight: the view shown before keeps its place and the slide starts
    /// over from where it is drawn, so the deck reads as one motion instead of two.
    fn follow(&mut self, started: Started, motion: EffectiveMotion, now: Instant) -> bool {
        let Some(timing) = started.timing(motion) else {
            return false;
        };
        let at = self.progress.get(now);
        self.started = started;
        self.progress.snap(at);
        self.progress.set(1.0, timing, now);
        true
    }
}

impl super::DesktopSurface {
    /// The strip shown changed from `before` to `after` : start the slide the change is drawn with.
    pub(crate) fn show_strip_changed(
        &mut self,
        before: Option<StripId>,
        after: Option<StripId>,
        tree: &tilo_desktop_model::DesktopTree,
    ) {
        let (kind, toward, steps) = Self::change_of(before, after, tree);
        let motion = self.motion;
        // Across several displays a strip change happens on the display the person works on; the
        // strip left behind stays where it was on the displays that keep showing it, so the change
        // lands there instead of sliding the whole desk.
        let started = self.navigation_source().and_then(|source| {
            let started = Started {
                kind,
                toward,
                steps,
                source,
            };
            (!(kind == SlideKind::Strip && tree.is_partitioned())).then_some(started)
        });
        self.shown = after;
        self.structure_dirty = true;
        // The change is this frame's: the next change starts from what the person sees, not from
        // the input that made this one.
        self.navigation_source = None;
        self.slides
            .get_mut()
            .change(started, before, after, motion, ui::motion::frame_now());
    }

    /// What asked for the change the frame shows: a chord or an action names itself before it acts,
    /// and a press or a finger was just handled. Nothing is the person's own, so the change lands,
    /// as an agent's or a script's does.
    pub(crate) fn navigation_source(&self) -> Option<ui::tokens::MotionSource> {
        self.navigation_source
            .or_else(|| ui::motion::recent_input(ui::motion::frame_now()))
    }

    /// Which change a move from `before` to `after` is, which way it goes and how many steps it
    /// takes.
    pub(crate) fn change_of(
        before: Option<StripId>,
        after: Option<StripId>,
        tree: &tilo_desktop_model::DesktopTree,
    ) -> (SlideKind, f64, f64) {
        let (Some(from), Some(to)) = (before, after) else {
            // A workspace with no strip at all, or one appearing: a whole-view change either way.
            return (SlideKind::Workspace, 1.0, 1.0);
        };
        let Some(workspace) = tree.active_workspace() else {
            return (SlideKind::Workspace, 1.0, 1.0);
        };
        let index = |id: StripId| workspace.strips.iter().position(|s| s.id == id);
        if let (Some(from), Some(to)) = (index(from), index(to)) {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a workspace has a handful of strips"
            )]
            let steps = to.abs_diff(from).max(1) as f64;
            return (SlideKind::Strip, if to >= from { 1.0 } else { -1.0 }, steps);
        }
        // The strips belong to different workspaces: what is drawn is a whole-view change, as a
        // workspace switch is.
        let order = |id: StripId| {
            tree.workspaces
                .iter()
                .position(|w| w.strips.iter().any(|strip| strip.id == id))
        };
        let toward = match (order(from), order(to)) {
            (Some(from), Some(to)) if to < from => -1.0,
            _ => 1.0,
        };
        (SlideKind::Workspace, toward, 1.0)
    }

    /// What asked for the next strip or workspace change: the host notes the input it was before
    /// showing the change, so the frame draws it moving (a press, a finger) or landed (a chord, an
    /// action).
    pub fn note_source(&mut self, source: ui::tokens::MotionSource) {
        self.navigation_source = Some(source);
    }

    /// The strip the surface shows now.
    #[must_use]
    pub fn shown_strip_id(&self) -> Option<StripId> {
        self.shown
    }

    /// The slides the surface draws its strip and workspace changes with: a host carrying the deck
    /// under the person's fingers moves them here.
    pub fn slides_mut(&mut self) -> &mut Slides {
        self.slides.get_mut()
    }

    /// Three fingers landed on the strip shown to carry the deck: it follows them from here.
    pub fn begin_strip_slide(&self) {
        let shown = self.shown;
        self.slides.borrow_mut().begin_lift(shown);
    }

    /// The fingers have carried the deck `travel` pixels, positive toward the strips below, since
    /// they landed. The strip they landed on is drawn exactly that far the other way, so the deck
    /// is under the hand.
    pub fn drag_strip_slide(&self, travel: f64) {
        let height = self
            .shown_row_ref()
            .map(GroupRow::get_rect)
            .map_or(0.0, |rect| rect.height())
            .max(1.0);
        self.slides.borrow_mut().lift_to(travel / height);
    }

    /// Draw the strip the person left behind, at its pose, under the strip that arrived: a strip or
    /// workspace change moves as one view instead of cutting from one to the other. The bar, the
    /// notifications and the menus stay where they are.
    pub(crate) fn draw_leaving_strip(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let motion = ui::tokens::EffectiveMotion::new(theme.tokens.motion, theme.reduce_motion);
        self.motion = motion;
        let now = ui::motion::frame_now();
        let Some((from, rect, pose)) = self.slides.get_mut().leaving(motion, now) else {
            return;
        };
        let Some(row) = self.rows.get_mut(&from) else {
            self.slides.get_mut().drop_running();
            return;
        };
        draw_leaving(row, rect, pose, scene, transform, text, theme, presence);
    }

    /// Note where the strip shown was laid out, so a change away from it is drawn from where the
    /// person left it.
    pub(crate) fn note_laid_strip(&mut self) {
        if let Some(strip) = self.shown
            && let Some(rect) = self.shown_row_ref().map(GroupRow::get_rect)
            && rect.width() > 0.0
            && rect.height() > 0.0
        {
            self.slides.get_mut().laid(strip, rect);
        }
    }
}

/// Draw the strip `row` leaving at `pose` , under the strip that arrived.
pub(super) fn draw_leaving(
    row: &mut GroupRow,
    rect: Rect,
    pose: Pose,
    scene: &mut vello::Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    if !pose.is_visible() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    // The strip keeps the deck's edge: it never draws over the bar or past the display.
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        transform,
        &rect,
    );
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        pose.opacity,
        transform,
        &rect,
    );
    let moved = transform.pre_translate(pose.transform.translation());
    ui::widget::draw_child(row, scene, moved, text, theme, presence);
    scene.pop_layer();
    scene.pop_layer();
}
