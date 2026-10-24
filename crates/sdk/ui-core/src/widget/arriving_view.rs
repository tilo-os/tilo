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
use crate::tokens::TextRole;
use crate::widget::{
    AppModel, Banner, Button, Element, ModelPath, ModelView, SkeletonFace, SkeletonShape,
    TaffyTreeExt, Widget,
};

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
}

/// The placeholder steps of a read, painted in a rect: the immediate-mode
/// counterpart of [`ArrivingView`], for a painter that draws its own rows.
#[derive(Clone, Debug)]
pub struct ArrivalFace<'a> {
    shape: SkeletonShape,
    step: Step,
    source: &'a Source,
}

impl<'a> ArrivalFace<'a> {
    /// What `clock`'s read shows at `now`, in `shape`.
    #[must_use]
    pub fn new(shape: SkeletonShape, clock: &'a ArrivalClock, now: Instant) -> Self {
        Self {
            shape,
            step: clock.step(now),
            source: clock.source(),
        }
    }

    /// The face of `step` itself, for the placeholder an arrival fades out.
    #[must_use]
    pub fn at_step(shape: SkeletonShape, step: Step, source: &'a Source) -> Self {
        Self {
            shape,
            step,
            source,
        }
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
        if self.step != Step::Named {
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
    /// skeleton under a line saying where the data comes from. All of it
    /// inside `rect`, so a step never moves anything.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        match self.step {
            Step::Ground | Step::Shown => {}
            Step::Skeleton => SkeletonFace::new(self.shape).paint(ctx, rect),
            Step::Named => {
                let Some(caption) = self.caption() else {
                    SkeletonFace::new(self.shape).paint(ctx, rect);
                    return;
                };
                let line = ctx.line_box(TextRole::Caption);
                let gap = f64::from(ctx.theme.tokens.spacing.s1);
                let color = ctx.theme.tokens.colors.text_dim;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a laid-out width is far below f32::MAX"
                )]
                let width = rect.width() as f32;
                ctx.text_role(
                    &caption,
                    TextRole::Caption,
                    color,
                    rect.origin(),
                    Some(width),
                );
                let below = Rect::new(rect.x0, rect.y0 + line + gap, rect.x1, rect.y1);
                if below.height() > 0.0 {
                    SkeletonFace::new(self.shape).paint(ctx, below);
                }
            }
        }
    }
}

/// The space a pending read reserves: as tall as its content's shape, and
/// empty. Its [`ArrivingView`] paints the placeholder steps over it.
struct Reserve {
    shape: SkeletonShape,
    rect: Rect,
    theme: Option<Theme>,
}

impl Reserve {
    /// Nothing, in no space: what a placeholder's read that never answers
    /// would show.
    fn empty() -> Self {
        Self {
            shape: SkeletonShape::Text { lines: 0 },
            rect: Rect::ZERO,
            theme: None,
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
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
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

    crate::rect_geometry!();

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
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Reserve"
    }
}

/// A read's view. See the module docs; build one with [`Arriving::view`].
pub struct ArrivingView<T: 'static, E: 'static = String> {
    arriving: Arriving<T, E>,
    shape: Rc<Cell<SkeletonShape>>,
    reveal: Rc<Cell<Reveal>>,
    inner: ModelView<Reveal>,
    /// The revision of the read last looked at.
    observed: Option<u64>,
    /// The timing of the read pending now, or of the last one.
    clock: Option<ArrivalClock>,
    fade: CrossFade<Showing>,
    /// The placeholder step the shown content replaced: what fades out.
    replaced: Step,
    /// The placeholder step the last frame painted.
    drawn_step: Option<Step>,
    theme: Option<Theme>,
    rect: Rect,
    /// Whether a read can still answer this view: a still capture waits
    /// for it. A [`placeholder`](ArrivingView::placeholder) stands for data
    /// whose answer comes with a new view, not into this one.
    answerable: bool,
}

impl<T: 'static, E: std::fmt::Display + 'static> ArrivingView<T, E> {
    /// The view of `arriving`, showing `build`'s content once the answer is
    /// there.
    pub fn new(arriving: &Arriving<T, E>, mut build: impl FnMut(&T) -> Element + 'static) -> Self {
        let shape = Rc::new(Cell::new(SkeletonShape::Text { lines: 3 }));
        let reveal = Rc::new(Cell::new(Reveal {
            showing: Showing::Placeholder,
            revision: 0,
        }));
        let projected = Rc::clone(&reveal);
        let built_shape = Rc::clone(&shape);
        let source = arriving.clone();
        let inner = ModelView::from_document(
            move |_, _| projected.get(),
            move |reveal: &Reveal| -> Element {
                let reserve = || -> Element {
                    Box::new(Reserve {
                        shape: built_shape.get(),
                        rect: Rect::ZERO,
                        theme: None,
                    })
                };
                match reveal.showing {
                    Showing::Placeholder => reserve(),
                    Showing::Answer => source
                        .with(|arrival| arrival.ready().map(&mut build))
                        .unwrap_or_else(reserve),
                    Showing::Failure => source
                        .with(|arrival| arrival.failure().map(ToString::to_string))
                        .map_or_else(reserve, |message| failure(&source, message)),
                }
            },
        );
        Self {
            arriving: arriving.clone(),
            shape,
            reveal,
            inner,
            observed: None,
            clock: None,
            fade: CrossFade::new(Showing::Placeholder),
            replaced: Step::Ground,
            drawn_step: None,
            theme: None,
            rect: Rect::ZERO,
            answerable: true,
        }
    }
}

/// The reserved space as the error, with a retry when the read offers one.
fn failure<T: 'static, E: 'static>(arriving: &Arriving<T, E>, message: String) -> Element {
    let mut banner = Banner::error(crate::i18n::I18nManager::lookup_plain("arriving-failed"))
        .with_message(message);
    if arriving.can_retry() {
        let again = arriving.clone();
        banner = banner.with_action(
            Button::new("arriving.retry", "arriving-retry").on_view(move || again.retry()),
        );
    }
    Box::new(banner)
}

impl ArrivingView<()> {
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

impl<T: 'static, E: 'static> ArrivingView<T, E> {
    /// Say what the content looks like, so the reserved space and the
    /// skeleton take its geometry and nothing moves when it lands.
    #[must_use]
    pub fn shaped(self, shape: SkeletonShape) -> Self {
        self.shape.set(shape);
        self
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

    /// Look at the read, and show its answer once it is due.
    fn reconcile(&mut self, now: Instant) {
        let revision = self.arriving.revision();
        if self.observed != Some(revision) {
            self.observed = Some(revision);
            let pending = self.arriving.with(|arrival| arrival.source().cloned());
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
        let Some(showing) = self.arriving.with(|arrival| match arrival {
            Arrival::Pending { .. } => None,
            Arrival::Ready(_) => Some(Showing::Answer),
            Arrival::Failed(_) => Some(Showing::Failure),
        }) else {
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
        self.reveal.set(Reveal { showing, revision });
    }

    fn paint_step(&self, ctx: &mut PaintCtx<'_>, step: Step) {
        let local = Source::Local;
        let source = self.clock.as_ref().map_or(&local, ArrivalClock::source);
        ArrivalFace::at_step(self.shape.get(), step, source).paint(ctx, self.rect);
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

impl<T: 'static, E: 'static> Widget for ArrivingView<T, E> {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.reconcile(crate::motion::frame_now());
        self.inner.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.reconcile(crate::motion::frame_now());
        self.inner.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let child = self.inner.build_layout(taffy, theme, node_map);
        taffy.widget_node(
            super::document_view::boundary_style(taffy.style(child).ok()),
            &[child],
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Ok(child) = taffy.child_at_index(node, 0) {
            self.inner.apply_layout(taffy, child, self.rect.origin());
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
            self.paint_step(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                step,
            );
            return;
        }
        if let Some((_, opacity)) = self.fade.outgoing(now) {
            let replaced = self.replaced;
            let rect = self.rect;
            layered(scene, transform, rect, opacity, |scene| {
                self.paint_step(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    replaced,
                );
            });
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
        if let Ok(child) = ctx.taffy.child_at_index(ctx.node_id, 0) {
            self.inner
                .handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.inner]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.inner);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.inner]
    }

    fn name(&self) -> &'static str {
        "ArrivingView"
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }

    fn take_focus_reset(&mut self) -> bool {
        self.inner.take_focus_reset()
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.fade.is_moving(now)
            || self.drawn_step.is_some_and(|drawn| drawn != self.step(now))
            || self.inner.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        let now = crate::motion::frame_now();
        self.drawn_step != Some(self.step(now))
            || self.fade.is_moving(now)
            || self.inner.needs_redraw()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = crate::time::now();
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
        let inner = self.inner.serve_deadline(now);
        let revision = self.arriving.revision();
        self.reveal_when_due(now, revision);
        inner
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
    pub fn view(&self, build: impl FnMut(&T) -> Element + 'static) -> ArrivingView<T, E> {
        ArrivingView::new(self, build)
    }
}

#[cfg(test)]
#[path = "arriving_view_tests.rs"]
mod tests;
