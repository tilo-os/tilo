//! The live banner stack: title + body cards pinned top-right, sliding in,
//! fading out and auto-dismissing, each on the shared `ui::widget::toast::ToastCard`:
//! dual-line layout, a severity tint, a close affordance, a card for the
//! banners that do not fit, keyboard focus on request and the
//! locked-session redaction.
//!
//! A banner's time runs only while it is on screen and nobody holds it: the
//! stack tells its host which banners it laid out and whether the pointer
//! rests on it or keyboard focus is in it ([`BannerAsk::Present`]).
//!
//! The host owns the data and hands the stack a copy ([`BannerFeed`]); the
//! stack paints it, routes presses and keys, says when it next needs a
//! frame, and puts what the person did to the host as [`BannerAsk`]s.

use ui::a11y::AccessNode;
use ui::num::f32_of_f64;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::PaintedTargets;
use ui::widget::toast::ToastCard;
use ui::widget::top_edge_reveal::AIM_HOLD;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::NotificationHost;
use super::history::relative_time;
use super::model::{INLINE_REPLY, Notification, NotificationSeverity, lock_suppressed_label};
use crate::i18n::{t, t_args};
use ui::TextRole;
use ui::widget::toast::Dwell;

/// How a banner arrives and leaves: from its edge, on the panel slide.
const BANNER: ui::motion::MotionSpec = ui::motion::PanelSlide::SPEC;

/// A banner's arrival and exit for the motion the person allows: the panel slide, or under reduced
/// motion a fade in place at `fast` (a change of what is shown still reads; nothing travels).
fn banner_motion(reduce_motion: bool) -> (ui::motion::MotionSpec, ui::tokens::EffectiveMotion) {
    if reduce_motion {
        (
            ui::motion::CROSS_FADE,
            ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), true),
        )
    } else {
        (BANNER, ui::tokens::EffectiveMotion::default())
    }
}

pub(super) const TOAST_WIDTH: f64 = 360.0;
pub(super) const TOAST_HEIGHT: f64 = 64.0;
const TOAST_GAP: f64 = 8.0;
/// A banner's slot is in place within a quarter of a pixel.
const SLOT_TOLERANCE: f64 = 0.25;

/// How far the pointer travels sideways on a banner before it is a swipe rather than a press.
const SWIPE_SLOP: f64 = 8.0;

/// How far a banner must be carried (a fraction of its width), or how fast it must be thrown
/// (logical px per second), to be dismissed.
const SWIPE_DISMISS: f64 = 0.35;
const SWIPE_VELOCITY: f64 = 700.0;
/// The margin a panel pinned to a display's top-right corner keeps from its edge: one number for
/// the banner stack, the notification centre and quick settings, so the corner reads as one place
/// rather than three panels that nearly line up.
pub const CORNER_INSET: f64 = 24.0;
/// Horizontal text inset of the locked card and the "more" card. Deliberately the shared toast
/// card's own `CARD_PAD_X` (`ui::widget::toast`, 14) so their text starts on the cards' line; it is
/// off the 4 px grid for the same reason the card is.
const CARD_PADDING_X: f64 = 14.0;
/// Least height of the "more" card: a Regular control's target.
const MORE_HEIGHT: f64 = 40.0;
/// Banners standing in the corner at once. Further arrivals fold into the last one's caption as "+N
/// more", opening the centre.
const MOST_BANNERS: usize = 3;

/// Bundles the render handles the per-section painters need.
struct PaintCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text_renderer: &'a mut TextRenderer,
    theme: &'a Theme,
}

/// What the stack shows: a copy of the host's live banners and whether the session is locked. The
/// stack ages its copy as it reports to the host ([`BannerAsk::Present`]), so the two agree until
/// the host hands it the next one.
#[derive(Clone, Debug, Default)]
pub struct BannerFeed {
    /// Live banners, oldest first.
    pub banners: Vec<Banner>,
    /// While locked, banners collapse to a redacted count.
    pub locked: bool,
    /// Whether the host's banners were held at its last present.
    pub held: bool,
}

/// One live banner and the host's choices for it.
#[derive(Clone, Debug)]
pub struct Banner {
    pub notification: Notification,
    /// Host-owned choices, as `(key, label)` : the card draws at most two.
    pub actions: Vec<(String, String)>,
}

impl BannerFeed {
    /// The banners `host` shows now.
    pub fn of(host: &dyn NotificationHost) -> Self {
        let log = host.log();
        Self {
            banners: log
                .active()
                .iter()
                .map(|notification| Banner {
                    actions: host.actions_for(notification),
                    notification: notification.clone(),
                })
                .collect(),
            locked: host.session_locked(),
            held: log.held(),
        }
    }

    /// Live banner `id` .
    pub fn banner(&self, id: u64) -> Option<&Banner> {
        self.banners
            .iter()
            .find(|banner| banner.notification.id == id)
    }

    /// A call rings now: an incoming-call banner is up and the session is unlocked. Its card is on
    /// the display in use; the other displays show a quiet edge.
    pub fn call_ringing(&self) -> bool {
        !self.locked && self.banners.iter().any(|banner| banner.notification.rings)
    }

    /// Age the banners as the host does for the same report ([`super::NotificationLog::present`]).
    /// Whether that changed them.
    fn present(&mut self, now: Instant, on_screen: &[u64], held: bool) -> bool {
        let before: Vec<Dwell> = self.dwells().collect();
        let was_held = self.held;
        super::log::present_banners(
            self.banners
                .iter_mut()
                .map(|banner| &mut banner.notification),
            &mut self.held,
            (now, on_screen, held),
        );
        was_held != self.held || !self.dwells().eq(before)
    }

    fn dwells(&self) -> impl Iterator<Item = Dwell> + '_ {
        self.banners.iter().map(|banner| banner.notification.dwell)
    }
}

/// What the person did with the banners, for the host to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BannerAsk {
    /// The stack laid out `on_screen` (newest first) at `now` , and the person `held` it (the
    /// pointer on the stack, keyboard focus in a banner): forward to
    /// [`super::NotificationLog::present`].
    Present {
        now: Instant,
        on_screen: Vec<u64>,
        held: bool,
    },
    /// The card standing in for the banners that did not fit was chosen.
    OpenCenter,
    /// The person selected banner `id` (not its close button).
    Activate(u64),
    /// The person closed banner `id` .
    Dismiss(u64),
    /// The person chose one of banner `id` 's host-owned choices.
    Invoke(u64, String),
    /// The person answered banner `id` 's inline reply with `text` .
    Reply { id: u64, text: String },
}

impl BannerAsk {
    /// Carry the ask out on `host` .
    pub fn answer(self, host: &dyn NotificationHost) {
        match self {
            Self::Present {
                now,
                on_screen,
                held,
            } => host.present(now, &on_screen, held),
            Self::OpenCenter => host.open_center(),
            Self::Activate(id) => host.activate(id),
            Self::Dismiss(id) => host.dismiss(id),
            Self::Invoke(id, key) => host.invoke_action(id, &key),
            Self::Reply { id, text } => host.reply(id, &text),
        }
    }
}

/// The y-extent of everything painted, so the stack's rect is exactly what was drawn.
struct StackBounds {
    min_y: f64,
    max_y: f64,
}

impl StackBounds {
    fn expand(&mut self, y0: f64, y1: f64) {
        self.min_y = self.min_y.min(y0);
        self.max_y = self.max_y.max(y1);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BannerTarget {
    Open(u64),
    Dismiss(u64),
    Action(u64, String),
    /// The inline reply field of banner `id` .
    Reply(u64),
    /// The card standing in for the banners that did not fit: opens the notification centre.
    More,
}

impl BannerTarget {
    fn key(&self) -> String {
        match self {
            Self::Open(id) => format!("notification:{id}:open"),
            Self::Dismiss(id) => format!("notification:{id}:dismiss"),
            Self::Action(id, key) => format!("notification:{id}:action:{key}"),
            Self::Reply(id) => format!("notification:{id}:reply"),
            Self::More => "notification:more".to_owned(),
        }
    }

    fn banner(&self) -> Option<u64> {
        match self {
            Self::Open(id) | Self::Dismiss(id) | Self::Action(id, _) | Self::Reply(id) => Some(*id),
            Self::More => None,
        }
    }
}

/// One row of keyboard focus: a banner's controls left to right (the card itself, its actions, its
/// close button), or the "more" card alone.
#[derive(Clone, Debug)]
struct FocusRow {
    controls: Vec<(BannerTarget, Rect, f64)>,
}

/// Top-right banner stack over a host's [`BannerFeed`].
#[derive(Debug, Default)]
pub struct NotificationStack {
    /// What it shows, as the host last handed it and as it aged since.
    feed: BannerFeed,
    /// What the person did, for the host to take ([`Self::take_asks`]).
    asks: Vec<BannerAsk>,
    /// The area the stack pins itself into, set by the host each frame.
    viewport: Rect,
    /// Per-banner `(id, card, close)` hit rects; cleared every paint so a stale hit cannot fire
    /// after expiry.
    hit_rects: Vec<(u64, Rect, Rect)>,
    action_rects: Vec<(u64, String, Rect)>,
    /// What a press on a painted banner does, as buttons for assistive
    /// technology (and anything else that reads the semantics): each card
    /// (opening it, labelled by its title) and its action buttons.
    /// Each painted control as assistive technology reads it: its rect,
    /// its label and the target activating it (a [`BannerTarget::key`]).
    action_buttons: Vec<(Rect, String, String)>,
    rect: Rect,
    /// Mirrored from the last paint so the frame and deadline hooks obey reduced motion without a
    /// theme argument.
    reduce_motion: bool,
    targets: PaintedTargets,
    target_actions: Vec<(String, BannerTarget)>,
    /// Live banners the last paint laid out on screen, newest first. Only these age.
    on_screen: Vec<u64>,
    /// The "more" card's rect; zero when every banner fit.
    more_rect: Rect,
    /// The banner whose caption carries the further arrivals, if any: its body opens the centre
    /// rather than the notification alone.
    folded: Option<u64>,
    /// The keyboard order of the last paint, top to bottom.
    focus_rows: Vec<FocusRow>,
    /// The control keyboard focus is on, while the person moved focus into the stack
    /// ([`Self::focus_newest`]).
    focus: Option<BannerTarget>,
    /// Where the pointer is (logical), followed also while a window under it takes the motion.
    pointer: Option<Point>,
    /// The pointer's trajectory, read for aim: heading for the stack and coming to rest on it, or
    /// passing over it on the way elsewhere.
    aim: ui::widget::EdgeAim,
    /// The last sample that confirmed the pointer was aiming at the stack.
    aimed_at: Option<Instant>,
    /// Where each banner on screen is drawn, by id: when the banners above it change it springs to
    /// its new slot (`motion::ListChange`) instead of jumping, and while the person holds the stack
    /// a slot that would close upward waits, so the banner under the pointer stays put.
    slots: Vec<(u64, ui::motion::MotionValue<f64>)>,
    /// Where the windows' content starts: banners begin below it, so the window bars' controls
    /// (maximise, close) under the corner stay reachable. Zero when the host has no windows to keep
    /// clear.
    clear_top: f64,
    /// The windows' bars on screen: a banner never covers one, the stack shifts below it instead
    /// ([`transient_slot`]).
    window_bars: Vec<Rect>,
    /// The banner under the pointer, while a sideways drag on it is in progress.
    swipe: Option<SwipeDrag>,
    /// Banners sliding under their own motion: out of the corner after a swipe dismissed them, or
    /// back to rest after one fell short.
    slides: Vec<Slide>,
    /// Whether this stack's host carries an inline reply to its sender. The banners app does; a
    /// stack painted by its own host offers none, so the field never sends an answer nowhere.
    replies: bool,
    /// The reply the person is typing, on one banner at a time.
    reply: Option<ReplyDraft>,
    /// The reply field of each painted banner, by id.
    reply_rects: Vec<(u64, Rect)>,
}

/// What the person typed so far in one banner's inline reply.
#[derive(Clone, Debug)]
struct ReplyDraft {
    id: u64,
    text: String,
}

/// A sideways drag on one banner: where it started, how far it has come,
/// how fast, and whether it has passed the slop (a swipe, not a press).
#[derive(Clone, Copy, Debug)]
struct SwipeDrag {
    id: u64,
    start_x: f64,
    dx: f64,
    last_dx: f64,
    last_at: Instant,
    velocity: f64,
    dragging: bool,
}

/// A banner following a swipe's throw: its horizontal offset from rest,
/// and whether it is leaving (the host is asked to dismiss it once it is
/// gone) or returning.
#[derive(Debug)]
struct Slide {
    id: u64,
    x: ui::motion::MotionValue<f64>,
    leaving: bool,
}

/// The least a banner is tall, for where the first one may start.
const MIN_BANNER_H: f64 = 56.0;

/// Where a transient goes (a banner, the follow bar): the reserved zone on the trailing side of
/// `viewport` , the area under the top bar, inset from its corner, starting no higher than `top` ,
/// `size` wide and tall. A window's bar always wins: where the slot would cover one of
/// `window_bars` , it shifts down below that bar, until it covers none.
#[must_use]
pub fn transient_slot(viewport: Rect, top: f64, size: (f64, f64), window_bars: &[Rect]) -> Rect {
    let (width, height) = size;
    let x1 = viewport.x1 - CORNER_INSET;
    let mut y0 = top.max(viewport.y0 + CORNER_INSET);
    // Each shift clears at least one bar, so this ends.
    for _ in 0..=window_bars.len() {
        let slot = Rect::new(x1 - width, y0, x1, y0 + height);
        let below = window_bars
            .iter()
            .filter(|bar| bar.intersect(slot).area() > 0.0)
            .map(|bar| bar.y1 + TOAST_GAP)
            .fold(None, |most: Option<f64>, y| {
                Some(most.map_or(y, |m| m.max(y)))
            });
        match below {
            Some(y) if y > y0 => y0 = y,
            _ => break,
        }
    }
    Rect::new(x1 - width, y0, x1, y0 + height)
}

impl NotificationStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Carry inline replies to the host ([`BannerAsk::Reply`]). Set by a stack whose host can
    /// deliver one; a stack that cannot offers no reply field.
    pub fn set_replies(&mut self, replies: bool) {
        self.replies = replies;
    }

    /// Show `feed` : the host's live banners now.
    pub fn set_feed(&mut self, feed: BannerFeed) {
        if feed.locked {
            self.targets.cancel_pointer_press();
            self.swipe = None;
            self.slides.clear();
            self.reply = None;
        }
        if let Some(draft) = &self.reply
            && !feed
                .banners
                .iter()
                .any(|banner| banner.notification.id == draft.id)
        {
            self.reply = None;
        }
        self.feed = feed;
    }

    /// What the stack shows.
    pub fn feed(&self) -> &BannerFeed {
        &self.feed
    }

    /// What the person did since last asked, in order, for the host to carry out
    /// ([`BannerAsk::answer`]).
    pub fn take_asks(&mut self) -> Vec<BannerAsk> {
        std::mem::take(&mut self.asks)
    }

    /// Pin the stack into `viewport` (top-right corner, inset).
    pub fn set_viewport(&mut self, viewport: Rect) {
        if self.viewport != viewport {
            self.targets.cancel_pointer_press();
        }
        self.viewport = viewport;
    }

    /// Keep everything above `y` clear: the window bars, whose controls a banner pinned under the
    /// top bar would cover and take clicks from.
    pub fn set_clear_top(&mut self, y: f64) {
        if (self.clear_top - y).abs() > f64::EPSILON {
            self.targets.cancel_pointer_press();
        }
        self.clear_top = y;
    }

    /// Keep the windows' bars on screen clear: where the first banner would cover one, the stack
    /// starts below it.
    pub fn set_window_bars(&mut self, bars: Vec<Rect>) {
        if self.window_bars != bars {
            self.targets.cancel_pointer_press();
        }
        self.window_bars = bars;
    }

    /// Where the first banner's top sits: inset from the viewport's corner,
    /// and below the window bars the host keeps clear.
    fn first_top(&self) -> f64 {
        let top = (self.viewport.y0 + CORNER_INSET).max(self.clear_top + TOAST_GAP);
        transient_slot(
            self.viewport,
            top,
            (self.toast_width(), MIN_BANNER_H),
            &self.window_bars,
        )
        .y0
    }

    /// Exactly what the last paint covered; zero when nothing was painted.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Whether the person holds the banners: keyboard focus is in the stack, or the pointer reaches
    /// for it (`Self::pointer_holds`). Held banners do not age.
    pub fn held(&self) -> bool {
        self.held_at(ui::time::now())
    }

    fn held_at(&self, now: Instant) -> bool {
        // A reply being typed holds its banner: it must not expire under the person's hands.
        self.focus.is_some() || self.reply.is_some() || self.pointer_holds(now)
    }

    /// Whether the pointer holds the banners at `now`, by the toolkit's
    /// hover-intent rules ([`ui::widget::EdgeAim`]): it rests on the stack,
    /// or it is heading for it and coming to rest there (held for
    /// [`AIM_HOLD`] past the last sample that said so, so a hand that
    /// stops short lets go). A pointer passing over the stack on its way
    /// elsewhere is in transit and holds nothing.
    fn pointer_holds(&self, now: Instant) -> bool {
        let resting = self
            .pointer
            .is_some_and(|p| self.rect.contains(p) && self.aim.settled(now));
        resting
            || self
                .aimed_at
                .is_some_and(|at| now.saturating_duration_since(at) < AIM_HOLD)
    }

    fn register_target(&mut self, target: BannerTarget, rect: Rect, label: &str) {
        let key = target.key();
        self.targets.target(&key, rect, label);
        self.target_actions.push((key, target));
    }

    /// Press the control assistive technology names by `target` (one of the action targets
    /// [`Self::accessibility`] publishes), as a click on it does. Whether it was one of the stack's
    /// controls.
    pub fn perform_semantic(&mut self, target: &str, action: &ui::a11y::Action) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some((_, banner)) = self.target_actions.iter().find(|(id, _)| id == target) else {
            return false;
        };
        let banner = banner.clone();
        self.dispatch_target(&banner);
        true
    }

    /// Retain the matching release when foreground content interrupts a click.
    pub fn cancel_pointer_press(&mut self) {
        self.targets.cancel_pointer_press();
    }

    /// Whether an armed or cancelled banner gesture owns the next release: an armed click, or a
    /// swipe past its slop.
    pub fn wants_pointer_capture(&self) -> bool {
        self.targets.wants_pointer_capture() || self.swipe.is_some_and(|drag| drag.dragging)
    }

    /// Follow a sideways drag on a banner: a press inside a card starts one, motion past
    /// [`SWIPE_SLOP`] makes it a swipe (the press no longer activates the card), and the release
    /// either throws the card out of the corner or springs it back. All of it under the toolkit's
    /// motion, interruptible by the next press.
    fn track_swipe(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.begin_swipe();
                false
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.swipe.is_none() {
                    return false;
                }
                self.drag_to(
                    Point::new(position.x / scale_factor, position.y / scale_factor),
                    ui::time::now(),
                )
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.end_swipe(ui::time::now()),
            WindowEvent::CursorLeft { .. } => {
                let dragging = self.swipe.is_some_and(|drag| drag.dragging);
                if dragging {
                    self.spring_back(ui::time::now());
                } else {
                    self.swipe = None;
                }
                dragging
            }
            WindowEvent::Focused(false) => {
                let dragging = self.swipe.is_some_and(|drag| drag.dragging);
                if dragging {
                    self.spring_back(ui::time::now());
                } else {
                    self.swipe = None;
                }
                dragging
            }
            _ => false,
        }
    }

    /// Start following a press inside a banner's card, unless it landed on one of its controls:
    /// those keep the toolkit's completed-click routing.
    fn begin_swipe(&mut self) {
        self.swipe = None;
        if self.feed.locked {
            return;
        }
        let Some(p) = self.pointer else {
            return;
        };
        if self
            .action_rects
            .iter()
            .any(|(_, _, rect)| rect.contains(p))
            || self.reply_rects.iter().any(|(_, rect)| rect.contains(p))
        {
            return;
        }
        let Some(&(id, _, close)) = self.hit_rects.iter().find(|(_, card, _)| card.contains(p))
        else {
            return;
        };
        if close.contains(p) {
            return;
        }
        self.swipe = Some(SwipeDrag {
            id,
            start_x: p.x,
            dx: 0.0,
            last_dx: 0.0,
            last_at: ui::time::now(),
            velocity: 0.0,
            dragging: false,
        });
    }

    /// Carry the dragged banner by the pointer. Past the slop the press stops being a click, so a
    /// card carried aside does not also open.
    fn drag_to(&mut self, p: Point, now: Instant) -> bool {
        let Some(mut drag) = self.swipe else {
            return false;
        };
        let dx = p.x - drag.start_x;
        if !drag.dragging && dx.abs() <= SWIPE_SLOP {
            return false;
        }
        if !drag.dragging {
            drag.dragging = true;
            self.targets.cancel_pointer_press();
        }
        let elapsed = now.saturating_duration_since(drag.last_at).as_secs_f64();
        if elapsed > 0.0 {
            drag.velocity = (dx - drag.last_dx) / elapsed;
        }
        drag.last_dx = dx;
        drag.last_at = now;
        drag.dx = dx;
        self.swipe = Some(drag);
        true
    }

    /// Let go: past [`SWIPE_DISMISS`] of the card's width, or thrown faster than
    /// [`SWIPE_VELOCITY`], the card leaves and the host is asked to dismiss it once it is out of
    /// sight; otherwise it springs back.
    fn end_swipe(&mut self, now: Instant) -> bool {
        let Some(drag) = self.swipe.take() else {
            return false;
        };
        if !drag.dragging {
            return false;
        }
        let width = self.toast_width();
        let thrown = drag.dx.abs() > width * SWIPE_DISMISS
            || (drag.velocity * drag.dx.signum() > SWIPE_VELOCITY && drag.dx.abs() > SWIPE_SLOP);
        if !thrown {
            self.spring_back_to(drag, now);
            return true;
        }
        if self.reduce_motion {
            // Nothing travels: the card is simply gone.
            self.asks.push(BannerAsk::Dismiss(drag.id));
            return true;
        }
        let direction = if drag.dx < 0.0 { -1.0 } else { 1.0 };
        let (_, motion) = banner_motion(self.reduce_motion);
        let timing = BANNER.timing(
            ui::motion::Direction::Exit,
            motion,
            ui::tokens::MotionSource::Pointer,
        );
        let mut x = ui::motion::MotionValue::new(drag.dx);
        x.release(
            direction * (width + CORNER_INSET),
            drag.velocity,
            timing,
            now,
        );
        self.slides.retain(|slide| slide.id != drag.id);
        self.slides.push(Slide {
            id: drag.id,
            x,
            leaving: true,
        });
        true
    }

    /// A drag that fell short returns to rest on the list-change spring.
    fn spring_back(&mut self, now: Instant) {
        if let Some(drag) = self.swipe.take()
            && drag.dragging
        {
            self.spring_back_to(drag, now);
        }
    }

    fn spring_back_to(&mut self, drag: SwipeDrag, now: Instant) {
        if self.reduce_motion {
            return;
        }
        let (_, motion) = banner_motion(self.reduce_motion);
        let timing = ui::motion::ListChange::MOVE.timing(
            ui::motion::Direction::Enter,
            motion,
            ui::tokens::MotionSource::Pointer,
        );
        let mut x = ui::motion::MotionValue::new(drag.dx);
        x.release(0.0, drag.velocity, timing, now);
        self.slides.retain(|slide| slide.id != drag.id);
        self.slides.push(Slide {
            id: drag.id,
            x,
            leaving: false,
        });
    }

    /// How far banner `id` is drawn from its rest slot at `now` , and how transparent its swipe has
    /// left it.
    fn swipe_offset(&self, id: u64, now: Instant) -> f64 {
        if let Some(drag) = self.swipe
            && drag.id == id
            && drag.dragging
        {
            return drag.dx;
        }
        self.slides
            .iter()
            .find(|slide| slide.id == id)
            .map_or(0.0, |slide| slide.x.get(now))
    }

    /// Retire finished slides: a card thrown out is dismissed once it is gone; one that came back
    /// is simply at rest again.
    fn advance_slides(&mut self, now: Instant) {
        let mut done = Vec::new();
        for slide in &self.slides {
            if slide.x.is_moving(now) {
                continue;
            }
            done.push(slide.id);
            if slide.leaving {
                self.asks.push(BannerAsk::Dismiss(slide.id));
            }
        }
        self.slides.retain(|slide| !done.contains(&slide.id));
        let shown: Vec<u64> = self
            .feed
            .banners
            .iter()
            .map(|banner| banner.notification.id)
            .collect();
        self.slides.retain(|slide| shown.contains(&slide.id));
        if let Some(drag) = self.swipe
            && !shown.contains(&drag.id)
        {
            self.swipe = None;
        }
    }

    fn dispatch_target(&mut self, target: &BannerTarget) {
        if self.feed.locked {
            return;
        }
        let Some(id) = target.banner() else {
            self.asks.push(BannerAsk::OpenCenter);
            return;
        };
        let available = self.feed.banner(id).is_some_and(|banner| match target {
            BannerTarget::Action(_, key) => {
                banner.actions.iter().any(|(current, _)| current == key)
            }
            _ => true,
        });
        if !available {
            return;
        }
        self.asks.push(match target {
            BannerTarget::Open(id) => BannerAsk::Activate(*id),
            BannerTarget::Dismiss(id) => BannerAsk::Dismiss(*id),
            BannerTarget::Action(id, key) => BannerAsk::Invoke(*id, key.clone()),
            BannerTarget::Reply(id) => {
                self.begin_reply(*id);
                return;
            }
            BannerTarget::More => return,
        });
    }

    /// Tell the host what is on screen and whether it is held, now.
    fn present(&mut self) {
        self.present_at(ui::time::now(), self.held());
    }

    /// Age the copy shown for what is on screen at `now` , `held` or not, and tell the host when
    /// that changed anything.
    fn present_at(&mut self, now: Instant, held: bool) {
        if self.feed.present(now, &self.on_screen, held) {
            self.asks.push(BannerAsk::Present {
                now,
                on_screen: self.on_screen.clone(),
                held,
            });
        }
    }

    /// Route the real pointer stream through the toolkit's completed-click controller. Banners do
    /// not take keyboard focus from the application on a click. The pointer resting on the stack
    /// holds the banners. Returns whether hover/press feedback changed.
    pub fn handle_pointer_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        if self.feed.locked {
            self.targets.cancel_pointer_press();
        }
        let mut changed = match event {
            WindowEvent::CursorMoved { position, .. } => self.note_pointer(Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ))),
            WindowEvent::CursorLeft { .. } => self.note_pointer(None),
            _ => false,
        };
        changed |= self.track_swipe(event, scale_factor);
        changed |= self
            .targets
            .handle_pointer_event(event, scale_factor, handled);
        let activated = self.targets.take_activated();
        for key in activated {
            if let Some((_, target)) = self.target_actions.iter().find(|(id, _)| *id == key) {
                let target = target.clone();
                self.dispatch_target(&target);
            }
        }
        changed
    }

    /// Follow the pointer at `point` (logical; `None` once it left the display), also while a
    /// window under it takes the motion: reaching for the stack holds the banners. Returns whether
    /// that changed.
    pub fn note_pointer(&mut self, point: Option<Point>) -> bool {
        self.note_pointer_at(point, ui::time::now())
    }

    fn note_pointer_at(&mut self, point: Option<Point>, now: Instant) -> bool {
        let before = self.pointer_holds(now);
        self.pointer = point;
        if let Some(p) = point {
            if !self.rect.is_zero_area() && self.aim.on_motion(p, now, &[self.rect]).is_some() {
                self.aimed_at = Some(now);
            }
        } else {
            self.aim.clear();
            self.aimed_at = None;
        }
        if self.pointer_holds(now) == before {
            return false;
        }
        self.present_at(now, self.held_at(now));
        true
    }

    /// Move keyboard focus into the newest banner on screen, onto its first action (or the card
    /// itself when it has none). Returns whether there was one to focus.
    pub fn focus_newest(&mut self) -> bool {
        if self.feed.locked {
            return false;
        }
        let Some(row) = self.focus_rows.first() else {
            return false;
        };
        let first = row
            .controls
            .iter()
            .find(|(target, _, _)| matches!(target, BannerTarget::Action(..)))
            .or_else(|| row.controls.first())
            .map(|(target, _, _)| target.clone());
        self.focus = first;
        self.present();
        self.focus.is_some()
    }

    pub fn has_keyboard_focus(&self) -> bool {
        self.focus.is_some()
    }

    /// Whether `key` is one the focused stack acts on: the arrows, Tab,
    /// Return, Space and Escape. Any other key goes back to the person's
    /// window, and takes focus with it ([`Self::release_keyboard`]).
    pub fn owns_key(&self, key: &Key) -> bool {
        if self.focus.is_none() {
            return false;
        }
        if self.reply.is_some() {
            // Typing in a reply takes every key: letters are the answer.
            return true;
        }
        matches!(
            key,
            Key::Named(
                NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::Tab
                    | NamedKey::Enter
                    | NamedKey::Space
                    | NamedKey::Escape
            )
        )
    }

    /// Whether the person is typing a reply, so a host that hands keys back on any other key waits
    /// instead ([`Self::owns_key`]).
    #[must_use]
    pub fn wants_typing(&self) -> bool {
        self.reply.is_some()
    }

    /// Start a reply on banner `id` : focus its field and hold the banner while the person types.
    fn begin_reply(&mut self, id: u64) {
        if !self.replies || self.feed.locked {
            return;
        }
        self.reply = Some(ReplyDraft {
            id,
            text: String::new(),
        });
        self.focus = Some(BannerTarget::Reply(id));
        self.present();
    }

    /// Type into the open reply: Backspace deletes, Return sends it (when there is something to
    /// send), Escape gives up, and every other key is text or space. Returns whether it was taken.
    fn type_reply(&mut self, key: &Key) -> bool {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.reply = None;
                self.focus = None;
                self.present();
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(draft) = self.reply.take() {
                    let text = draft.text.trim().to_owned();
                    self.focus = None;
                    if !text.is_empty() {
                        self.asks.push(BannerAsk::Reply { id: draft.id, text });
                    }
                }
                self.present();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(draft) = self.reply.as_mut() {
                    draft.text.pop();
                }
            }
            Key::Named(NamedKey::Space) => {
                if let Some(draft) = self.reply.as_mut() {
                    draft.text.push(' ');
                }
            }
            Key::Character(text) => {
                if let Some(draft) = self.reply.as_mut() {
                    draft.text.extend(text.chars().filter(|c| !c.is_control()));
                }
            }
            _ => {}
        }
        true
    }

    /// Give keyboard focus back to the person's window. Returns whether the stack had it.
    pub fn release_keyboard(&mut self) -> bool {
        if self.focus.take().is_none() {
            return false;
        }
        self.present();
        true
    }

    /// Act on a key while keyboard focus is in the stack: Up and Down move
    /// between banners, Left, Right and Tab between a banner's controls,
    /// Return or Space runs the focused one, Escape dismisses the focused
    /// banner. Running or dismissing gives focus back to the window.
    /// Returns whether the key was taken.
    pub fn handle_key(&mut self, key: &Key, shift: bool) -> bool {
        if !self.owns_key(key) {
            return false;
        }
        if self.reply.is_some() {
            return self.type_reply(key);
        }
        let Some((row, column)) = self.focus_position() else {
            // The focused banner left (it was answered elsewhere): focus goes back to the window.
            self.release_keyboard();
            return true;
        };
        match key {
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let up = matches!(key, Key::Named(NamedKey::ArrowUp));
                let next = if up {
                    row.checked_sub(1)
                } else {
                    Some(row.saturating_add(1))
                };
                if let Some(target) = next
                    .and_then(|index| self.focus_rows.get(index))
                    .and_then(|row| row.controls.first())
                {
                    self.focus = Some(target.0.clone());
                }
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Tab) => {
                let back = matches!(key, Key::Named(NamedKey::ArrowLeft))
                    || (shift && matches!(key, Key::Named(NamedKey::Tab)));
                let next = if back {
                    column.checked_sub(1)
                } else {
                    Some(column.saturating_add(1))
                };
                if let Some(target) = next.and_then(|index| {
                    self.focus_rows
                        .get(row)
                        .and_then(|row| row.controls.get(index))
                }) {
                    self.focus = Some(target.0.clone());
                }
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(target) = self.focus.take() {
                    self.dispatch_target(&target);
                }
                self.present();
            }
            Key::Named(NamedKey::Escape) => {
                if let Some(id) = self.focus.take().and_then(|target| target.banner())
                    && !self.feed.locked
                {
                    self.asks.push(BannerAsk::Dismiss(id));
                }
                self.present();
            }
            _ => {}
        }
        true
    }

    /// Where the focused control sits in the last paint's keyboard order.
    fn focus_position(&self) -> Option<(usize, usize)> {
        let focus = self.focus.as_ref()?;
        self.focus_rows.iter().enumerate().find_map(|(row, line)| {
            line.controls
                .iter()
                .position(|(target, _, _)| target == focus)
                .map(|column| (row, column))
        })
    }

    /// The moment the banner first came on screen, for its entry.
    fn shown_at(n: &Notification, now: Instant) -> Instant {
        n.dwell.first_shown().unwrap_or(now)
    }

    /// Enter/exit alpha + horizontal slide: a banner comes in from its edge
    /// (`ui::motion::PanelSlide::sheet_pose`) when it first comes on screen, and fades out in place
    /// so that it is gone at expiry. A held banner does not fade; under reduced motion a banner
    /// simply stands.
    fn animation_state(
        n: &Notification,
        now: Instant,
        (reduce_motion, held): (bool, bool),
        travel_width: f64,
    ) -> (f32, f64) {
        let (spec, motion) = banner_motion(reduce_motion);
        let source = ui::tokens::MotionSource::Pointer;
        let age = now.saturating_duration_since(Self::shown_at(n, now));
        let enter = spec.progress_after(ui::motion::Direction::Enter, motion, source, age);
        // Under reduced motion it fades in where it rests; otherwise it slides in from its edge.
        let (arrive, dx) = if reduce_motion {
            (ui::motion::unit_opacity(enter), 0.0)
        } else {
            let pose = ui::motion::PanelSlide::sheet_pose(enter, travel_width, true, motion);
            (pose.opacity, pose.to_drawn(Point::ZERO).x)
        };
        // Exit fades only: the card drops in place rather than retracting.
        let remaining = n.time_left(now);
        let leaving = spec.settle_time(ui::motion::Direction::Exit, motion, source);
        let exit = if !held && remaining < leaving {
            1.0 - spec.progress_after(
                ui::motion::Direction::Exit,
                motion,
                source,
                leaving.saturating_sub(remaining),
            )
        } else {
            1.0
        };
        let alpha = arrive * ui::motion::unit_opacity(exit);
        (alpha.clamp(0.0, 1.0), dx)
    }

    /// Where banner `id` , laid out at `top` , is drawn at `now` : it springs to a new slot
    /// (`motion::ListChange`); a new banner starts in its slot (its arrival is the slide). While
    /// the stack is `held` , a slot closing upward waits, so nothing moves under the pointer.
    fn slot_top(&mut self, id: u64, top: f64, now: Instant, held: bool) -> f64 {
        let motion = banner_motion(self.reduce_motion).1;
        let timing = ui::motion::ListChange::MOVE.timing(
            ui::motion::Direction::Enter,
            motion,
            ui::tokens::MotionSource::Pointer,
        );
        if let Some((_, slot)) = self.slots.iter_mut().find(|(slot_id, _)| *slot_id == id) {
            let target = slot.target();
            let closes_up = top < target;
            if (top - target).abs() > SLOT_TOLERANCE && !(held && closes_up) {
                slot.set(top, timing, now);
            }
            return slot.get(now);
        }
        self.slots.push((
            id,
            ui::motion::MotionValue::new(top).with_tolerance(SLOT_TOLERANCE),
        ));
        top
    }

    /// Cards keep their designed width on ordinary displays and shrink on compact viewports instead
    /// of painting beyond the left edge.
    fn toast_width(&self) -> f64 {
        (self.viewport.width() - CORNER_INSET * 2.0).clamp(0.0, TOAST_WIDTH)
    }

    /// Locked-session collapse: a single redacted card showing only `count`,
    /// no hit rects.
    fn draw_locked_card(ctx: &mut PaintCtx, x0: f64, y: f64, width: f64, count: usize) -> Rect {
        let title_style = ctx.theme.text(TextRole::Caption);
        let card_rect = Rect::new(x0, y, x0 + width, y + TOAST_HEIGHT);
        // The floating card the banners it stands in for are made of.
        ui::widget::toast::paint_card_surface(ctx.scene, ctx.transform, card_rect, 1.0, ctx.theme);
        let label = lock_suppressed_label(count);
        // Centre on the measured line, not the font size (HIG §7a): the shared text-line painter
        // starts at the card's pad and centres on the card's middle line.
        let inner = Rect::new(
            card_rect.x0 + CARD_PADDING_X,
            card_rect.y0,
            card_rect.x1 - CARD_PADDING_X,
            card_rect.y1,
        );
        let ink = ctx.theme.tokens.colors.text_dim;
        let mut ui_ctx =
            ui::paint::PaintCtx::new(ctx.scene, ctx.transform, ctx.text_renderer, ctx.theme);
        ui::widget::TextLineFace::styled(&label, &title_style, TextRole::Caption)
            .start()
            .paint(&mut ui_ctx, inner, ink);
        card_rect
    }

    /// The "more" card's label and height for `count` banners.
    fn more_card(ctx: &mut PaintCtx, width: f64, count: usize) -> (String, f64, f64) {
        let style = ctx.theme.text(TextRole::Label);
        let label = t_args("notification-more", &[("count", count.into())]);
        let max_width = f32_of_f64((width - CARD_PADDING_X * 2.0).max(1.0));
        let (_, label_h) = ctx
            .text_renderer
            .measure_style(&label, Some(max_width), &style);
        let height =
            MORE_HEIGHT.max(f64::from(label_h) + 2.0 * f64::from(ctx.theme.tokens.spacing.s2));
        (label, height, f64::from(label_h))
    }

    /// The caption of the last standing banner for the banners beyond the corner's three: "+2 more
    /// from Notes" when they share an app, else just "2 more".
    fn fold_label(folded: &[&Banner]) -> Option<String> {
        let count = folded.len();
        if count == 0 {
            return None;
        }
        let app = folded
            .first()
            .and_then(|banner| banner.notification.app_name.as_deref());
        let shared = folded
            .iter()
            .all(|banner| banner.notification.app_name.as_deref() == app);
        Some(match app {
            Some(app) if shared => t_args(
                "notification-more-from",
                &[("count", count.into()), ("app", app.into())],
            ),
            _ => t_args("notification-more", &[("count", count.into())]),
        })
    }

    /// The card standing in for `count` banners that did not fit: one line,
    /// the same floating card, opening the notification centre.
    fn draw_more_card(&mut self, ctx: &mut PaintCtx, rect: Rect, label: &str, label_h: f64) {
        let style = ctx.theme.text(TextRole::Label);
        // Like a banner's own body, the card is pressable as a whole and shows no hover film:
        // passing over it is not aiming at it.
        ui::widget::toast::paint_card_surface(ctx.scene, ctx.transform, rect, 1.0, ctx.theme);
        // The label stands across the card's middle, a padding in from either side.
        let middle = rect.y0.midpoint(rect.y1);
        let inner = Rect::new(
            rect.x0 + CARD_PADDING_X,
            middle - label_h / 2.0,
            rect.x1 - CARD_PADDING_X,
            middle + label_h / 2.0,
        );
        let ink = ctx.theme.tokens.colors.text_primary;
        let mut ui_ctx =
            ui::paint::PaintCtx::new(ctx.scene, ctx.transform, ctx.text_renderer, ctx.theme);
        ui::widget::TextLineFace::styled(label, &style, TextRole::Label)
            .start()
            .paint(&mut ui_ctx, inner, ink);
        self.register_target(BannerTarget::More, rect, label);
        self.action_buttons
            .push((rect, label.to_owned(), BannerTarget::More.key()));
        self.focus_rows.push(FocusRow {
            controls: vec![(
                BannerTarget::More,
                rect,
                ui::widget::toast::card_radius(ctx.theme),
            )],
        });
        self.more_rect = rect;
    }

    /// The card every banner of `n` starts from, on the stack and on the machine-wide displays: the
    /// sender's mark, the body as previewed and the app's caption.
    fn banner_face(n: &Notification) -> ToastCard<'_> {
        let mark = n.mark();
        ToastCard::new(&n.title, n.severity.toast_kind())
            .icon(mark.icon)
            .app_image(mark.image)
            .body(if n.show_preview { &n.body } else { "" })
            .caption(n.app_name.as_deref().unwrap_or(""))
    }

    /// The shared card for banner `n` , with its interaction state.
    fn banner_card<'a>(
        &self,
        n: &'a Notification,
        actions: &'a [(String, String)],
        time: &'a str,
        reply: &'a str,
        reply_focused: bool,
    ) -> ToastCard<'a> {
        // The stack owns what to show and click routing; the toast
        // widget owns the card's look and hands back the close rect.
        // Plain information leads with what sent it: the person when one is
        // named, else the app's own mark. A severity glyph wins over both,
        // so an alert never loses its state colour.
        let mut card = Self::banner_face(n)
            .time(time)
            // A card that offers its own way out ("Not now", "Dismiss") needs no second one in its
            // corner.
            .closable(
                !actions
                    .iter()
                    .any(|(key, _)| key == super::model::NOT_NOW_ACTION || key == "dismiss"),
            )
            .actionable(!n.actions.is_empty());
        // A sender that takes an inline reply gets the field, when this stack's host can carry the
        // answer back. The one being typed in holds what the person has written so far.
        if self.replies && !self.feed.locked && n.actions.iter().any(|(key, _)| key == INLINE_REPLY)
        {
            card = card.reply(reply, reply_focused);
        }
        // A person's notification leads with them; the app stays the small source line above the
        // title.
        if let Some(person) = &n.from {
            card = card.person(&person.name, &person.identity);
        }
        let close = self.targets.interaction(&BannerTarget::Dismiss(n.id).key());
        card.close_hovered = close.hovered;
        card.close_pressed = close.pressed;
        if let Some((key, label)) = actions.first() {
            card.primary_action = label;
            let state = self
                .targets
                .interaction(&BannerTarget::Action(n.id, key.clone()).key());
            card.primary_action_hovered = state.hovered;
            card.primary_action_pressed = state.pressed;
        }
        if let Some((key, label)) = actions.get(1) {
            card.secondary_action = card.primary_action;
            card.secondary_action_hovered = card.primary_action_hovered;
            card.secondary_action_pressed = card.primary_action_pressed;
            card.primary_action = label;
            let state = self
                .targets
                .interaction(&BannerTarget::Action(n.id, key.clone()).key());
            card.primary_action_hovered = state.hovered;
            card.primary_action_pressed = state.pressed;
        }
        card
    }

    /// Paint each live card top to bottom from `y` , newest first, recording its hit rects and
    /// folding its bounds into `bounds` . Banners that do not fit fold into one "more" card under
    /// the last that does.
    fn draw_toast_cards(
        &mut self,
        ctx: &mut PaintCtx,
        banners: &[&Banner],
        now: Instant,
        (x0_rest, width): (f64, f64),
        mut y: f64,
        bounds: &mut StackBounds,
    ) {
        let bottom = self.viewport.y1 - CORNER_INSET;
        let card_radius = ui::widget::toast::card_radius(ctx.theme);
        let control_radius = f64::from(ctx.theme.tokens.radii.control);
        let held = self.held();
        // What the reply field shows, kept across the loop so a frame of none allocates nothing:
        // one buffer, filled only while typing.
        let mut reply_text = String::new();
        // The corner stands at most three banners; the rest fold into the last one's caption, which
        // opens the centre.
        let total = banners.len();
        let standing = total.min(MOST_BANNERS);
        let fold = Self::fold_label(banners.get(standing..).unwrap_or_default());
        for (index, banner) in banners.iter().take(standing).enumerate() {
            let n = &banner.notification;
            let actions = &banner.actions;
            let time = relative_time(n, now);
            let draft = self.reply.as_ref().filter(|draft| draft.id == n.id);
            reply_text.clear();
            if let Some(draft) = draft {
                reply_text.clone_from(&draft.text);
            }
            let mut card = self.banner_card(n, actions, &time, &reply_text, draft.is_some());
            let folded = index.saturating_add(1) == standing && fold.is_some();
            if folded && let Some(label) = fold.as_deref() {
                card.caption = label;
            }
            let height = ui::widget::toast::card_height(ctx.text_renderer, ctx.theme, &card, width)
                .max(TOAST_HEIGHT);
            // Banners that do not fit height-wise fold into the "more" card;
            // a banner that is not the last must leave room for it.
            let below = total.saturating_sub(index.saturating_add(1));
            let reserve = if below > 0 && !folded {
                Self::more_card(ctx, width, below).1 + TOAST_GAP
            } else {
                0.0
            };
            if y + height + reserve > bottom {
                let hidden = total.saturating_sub(index);
                let (label, more_h, label_h) = Self::more_card(ctx, width, hidden);
                if y + more_h <= bottom {
                    let rect = Rect::new(x0_rest, y, x0_rest + width, y + more_h);
                    bounds.expand(rect.y0, rect.y1);
                    self.draw_more_card(ctx, rect, &label, label_h);
                }
                return;
            }
            self.on_screen.push(n.id);
            let (alpha, slide_dx) =
                Self::animation_state(n, now, (self.reduce_motion, held), width);
            // A card the person is carrying follows the pointer, fading as it leaves the corner;
            // one thrown leaves under its own motion.
            let drag = self.swipe_offset(n.id, now);
            let alpha = alpha * f32_of_f64((1.0 - drag.abs() / width.max(1.0)).clamp(0.0, 1.0));
            let card_x0 = x0_rest + slide_dx + drag;
            let top = self.slot_top(n.id, y, now, held);
            let card_rect = Rect::new(card_x0, top, card_x0 + width, top + height);
            bounds.expand(card_rect.y0, card_rect.y1);
            if alpha <= 0.0 {
                // A banner's fade starts the frame it first comes on screen,
                // at no opacity. That frame still records its card, unseen,
                // so it differs from the last one where the banner arrives:
                // drawn as nothing, the frame matched the last and repainted
                // nothing until a later frame happened to come.
                ui::widget::toast::paint_card_surface(
                    ctx.scene,
                    ctx.transform,
                    card_rect,
                    0.0,
                    ctx.theme,
                );
                y += height + TOAST_GAP;
                continue;
            }
            card.alpha = alpha;
            let hit = ui::widget::toast::paint_card(
                ctx.scene,
                ctx.transform,
                ctx.text_renderer,
                ctx.theme,
                card_rect,
                &card,
            );
            let radii = (card_radius, control_radius);
            let fold = folded.then_some(fold.as_deref()).flatten();
            let row =
                self.record_card_hits(n, actions, &hit, card_rect, radii, card.closable, fold);
            self.focus_rows.push(row);
            self.hit_rects.push((n.id, card_rect, hit.close_rect));
            y += height + TOAST_GAP;
        }
    }

    /// Record a painted card's targets, so the pointer, the keyboard and tests reach its choices,
    /// its reply field and its close box. Returns the row of controls keyboard focus walks.
    fn record_card_hits(
        &mut self,
        n: &Notification,
        actions: &[(String, String)],
        hit: &ui::widget::toast::ToastCardHit,
        card_rect: Rect,
        (card_radius, control_radius): (f64, f64),
        closable: bool,
        fold: Option<&str>,
    ) -> FocusRow {
        // A card whose caption carries further arrivals stands for the corner, not for its own
        // notification: pressing it opens the list where they all are. Its choices and close still
        // reach it.
        let opened = fold.map_or(BannerTarget::Open(n.id), |_| BannerTarget::More);
        let name = fold.unwrap_or(&n.title);
        let mut row = FocusRow {
            controls: vec![(opened.clone(), card_rect, card_radius)],
        };
        self.register_target(opened.clone(), card_rect, name);
        self.action_buttons
            .push((card_rect, name.to_owned(), opened.key()));
        if fold.is_some() {
            self.folded = Some(n.id);
        }
        // Painted left to right: the secondary choice, then the primary.
        let mut choices = Vec::new();
        if let Some((key, label)) = actions.first() {
            let rect = if actions.len() >= 2 {
                hit.secondary_action_rect
            } else {
                hit.primary_action_rect
            };
            choices.push((key, label, rect));
        }
        if let Some((key, label)) = actions.get(1) {
            choices.push((key, label, hit.primary_action_rect));
        }
        for (key, label, rect) in choices {
            let target = BannerTarget::Action(n.id, key.clone());
            self.register_target(target.clone(), rect, label);
            self.action_rects.push((n.id, key.clone(), rect));
            self.action_buttons
                .push((rect, label.clone(), target.key()));
            row.controls.push((target, rect, control_radius));
        }
        if !hit.reply_rect.is_zero_area() {
            let target = BannerTarget::Reply(n.id);
            let label = t("notification-reply");
            self.register_target(target.clone(), hit.reply_rect, &label);
            self.reply_rects.push((n.id, hit.reply_rect));
            row.controls.push((target, hit.reply_rect, control_radius));
        }
        if closable {
            let target = BannerTarget::Dismiss(n.id);
            let label = t("notification-center-dismiss");
            self.register_target(target.clone(), hit.close_rect, &label);
            self.action_buttons
                .push((hit.close_rect, label, target.key()));
            row.controls.push((target, hit.close_rect, control_radius));
        }
        row
    }

    /// The keyboard focus ring, on the control focus is on.
    fn paint_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(focus) = self.focus.as_ref() else {
            return;
        };
        if let Some((_, rect, radius)) = self
            .focus_rows
            .iter()
            .flat_map(|row| row.controls.iter())
            .find(|(target, _, _)| target == focus)
        {
            ui::widget::paint_focus_ring(scene, transform, *rect, *radius, theme);
        }
    }

    /// Paint the stack: the locked card, or the live cards. Then tell the host which banners are on
    /// screen, so only those age.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.hit_rects.clear();
        self.action_rects.clear();
        self.reply_rects.clear();
        self.action_buttons.clear();
        self.targets.begin_frame();
        self.target_actions.clear();
        self.on_screen.clear();
        self.focus_rows.clear();
        self.more_rect = Rect::ZERO;
        self.folded = None;
        self.reduce_motion = theme.reduce_motion;
        self.rect = Rect::ZERO;
        let feed = std::mem::take(&mut self.feed);
        self.paint(&feed, scene, transform, text_renderer, theme);
        self.feed = feed;
        if self.focus_position().is_none() {
            // The focused banner left: focus goes back to the window.
            self.focus = None;
        }
        if self.rect.is_zero_area() {
            self.aimed_at = None;
        }
        self.paint_focus(scene, transform, theme);
        self.present();
    }

    fn paint(
        &mut self,
        feed: &BannerFeed,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.viewport.area() == 0.0 {
            return;
        }
        let locked = feed.locked;
        let shown = |banner: &&Banner| !locked || banner.notification.show_on_lock_screen;
        let banners: Vec<&Banner> = feed.banners.iter().rev().filter(shown).collect();
        if banners.is_empty() {
            return;
        }
        let width = self.toast_width();
        if width <= 0.0 {
            return;
        }
        // Theme type tokens already carry the user's text scale, and the stack (and the toast card
        // it paints) lays out with those resolved sizes; the renderer must not scale them again
        // (HIG §7a). Reset for this pass and restore it below.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let x0_rest = self.viewport.x1 - CORNER_INSET - width;
        let y = self.first_top();
        let mut ctx = PaintCtx {
            scene,
            transform,
            text_renderer,
            theme,
        };
        if locked {
            self.rect = Self::draw_locked_card(&mut ctx, x0_rest, y, width, banners.len());
            ctx.text_renderer.set_text_scale(renderer_scale);
            return;
        }
        let mut bounds = StackBounds {
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        let now = ui::time::now();
        self.draw_toast_cards(&mut ctx, &banners, now, (x0_rest, width), y, &mut bounds);
        self.advance_slides(now);
        // Slots of banners no longer on screen are forgotten: one that comes back starts in its
        // slot.
        let on_screen = &self.on_screen;
        self.slots.retain(|(id, _)| on_screen.contains(id));
        ctx.text_renderer.set_text_scale(renderer_scale);
        if bounds.min_y.is_finite() {
            self.rect = Rect::new(x0_rest, bounds.min_y, x0_rest + width, bounds.max_y);
        }
    }

    /// The banners about the whole machine (critical battery) on a display other than the one the
    /// stack lives on, pinned top-right into `viewport` as the stack pins its own: the same cards
    /// at rest, newest first, without controls, hover or focus, which stay with the stack on the
    /// display in use. Nothing while the session is locked. Like every transient they never cover
    /// one of `window_bars` . Returns what the cards cover, zero when nothing was painted.
    pub fn draw_machine_wide(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (viewport, window_bars): (Rect, &[Rect]),
    ) -> Rect {
        if viewport.area() == 0.0 || self.feed.locked {
            return Rect::ZERO;
        }
        let banners: Vec<&Notification> = self
            .feed
            .banners
            .iter()
            .rev()
            .map(|banner| &banner.notification)
            .filter(|n| n.machine_wide)
            .collect();
        let width = (viewport.width() - CORNER_INSET * 2.0).clamp(0.0, TOAST_WIDTH);
        if banners.is_empty() || width <= 0.0 {
            return Rect::ZERO;
        }
        // Resolved type sizes, as the stack paints them (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let x0 = viewport.x1 - CORNER_INSET - width;
        let bottom = viewport.y1 - CORNER_INSET;
        let mut y = transient_slot(viewport, viewport.y0, (width, MIN_BANNER_H), window_bars).y0;
        let mut covered = Rect::ZERO;
        for n in banners {
            let card = Self::banner_face(n).closable(false);
            let height = ui::widget::toast::card_height(text_renderer, theme, &card, width)
                .max(TOAST_HEIGHT);
            if y + height > bottom {
                break;
            }
            let rect = Rect::new(x0, y, x0 + width, y + height);
            let _ =
                ui::widget::toast::paint_card(scene, transform, text_renderer, theme, rect, &card);
            covered = if covered.is_zero_area() {
                rect
            } else {
                covered.union(rect)
            };
            y += height + TOAST_GAP;
        }
        text_renderer.set_text_scale(renderer_scale);
        covered
    }

    /// A call rings now ([`BannerFeed::call_ringing`]).
    pub fn call_ringing(&self) -> bool {
        self.feed.call_ringing()
    }

    /// Each banner the last paint drew and where, newest first.
    pub fn banner_rects(&self) -> Vec<(u64, Rect)> {
        self.hit_rects
            .iter()
            .map(|(id, card, _)| (*id, *card))
            .collect()
    }

    /// Whether there is a banner keyboard focus can move into ([`Self::focus_newest`]).
    pub fn can_focus(&self) -> bool {
        !self.feed.locked && !self.focus_rows.is_empty()
    }

    /// Activate a completed click at p. Interactive hosts forward the full pointer stream through
    /// `handle_pointer_event` so gestures remain cancellable. Returns whether the completed click
    /// landed on a banner.
    pub fn click_at(&mut self, p: Point) -> bool {
        let ask = if let Some((id, key, _)) = self
            .action_rects
            .iter()
            .find(|(_, _, rect)| rect.contains(p))
        {
            BannerAsk::Invoke(*id, key.clone())
        } else if let Some(&(id, full, close)) = self
            .hit_rects
            .iter()
            .find(|(_, full, close)| close.contains(p) || full.contains(p))
        {
            if close.contains(p) {
                BannerAsk::Dismiss(id)
            } else {
                debug_assert!(full.contains(p));
                if self.folded == Some(id) {
                    BannerAsk::OpenCenter
                } else {
                    BannerAsk::Activate(id)
                }
            }
        } else if self.more_rect.contains(p) {
            BannerAsk::OpenCenter
        } else {
            return false;
        };
        self.asks.push(ask);
        true
    }

    /// Whether a banner on screen is mid-entry or mid-exit.
    pub fn needs_animation_frame(&self) -> bool {
        let now = ui::time::now();
        if self.slots.iter().any(|(_, slot)| slot.is_moving(now))
            || self.slides.iter().any(|slide| slide.x.is_moving(now))
            || self.swipe.is_some_and(|drag| drag.dragging)
        {
            return true;
        }
        let (spec, motion) = banner_motion(self.reduce_motion);
        let source = ui::tokens::MotionSource::Pointer;
        let entry = spec.settle_time(ui::motion::Direction::Enter, motion, source);
        let window = spec.settle_time(ui::motion::Direction::Exit, motion, source);
        let held = self.held();
        self.feed
            .banners
            .iter()
            .map(|banner| &banner.notification)
            .filter(|n| self.on_screen.contains(&n.id))
            .any(|n| {
                let entering = n
                    .dwell
                    .first_shown()
                    .is_none_or(|shown| now.saturating_duration_since(shown) < entry);
                let remaining = n.time_left(now);
                let leaving =
                    !held && n.dwell.is_running() && !remaining.is_zero() && remaining <= window;
                entering || leaving
            })
    }

    /// The next instant a banner on screen starts leaving or expires. Nothing wakes for a held
    /// banner or one off screen: their time stands still.
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        let (spec, motion) = banner_motion(self.reduce_motion);
        let window = spec.settle_time(
            ui::motion::Direction::Exit,
            motion,
            ui::tokens::MotionSource::Pointer,
        );
        let now = ui::time::now();
        // The pointer's hold changes with no further input: a pointer that stops on the stack comes
        // to rest, an aim lapses.
        let resting = self
            .pointer
            .filter(|p| self.rect.contains(*p))
            .and_then(|_| self.aim.deadline(now));
        let aim_lapses = self
            .aimed_at
            .and_then(|at| at.checked_add(AIM_HOLD))
            .filter(|at| *at > now);
        let slides = self
            .slides
            .iter()
            .filter_map(|slide| slide.x.settles_at())
            .filter(|at| *at > now)
            .min();
        let banners = self
            .feed
            .banners
            .iter()
            .map(|banner| &banner.notification)
            .filter_map(|notification| {
                let expiry = notification.dwell.ends_at(notification.dismiss_after)?;
                let exit_start = expiry.checked_sub(window).unwrap_or(expiry);
                // While an animation runs the frame hook drives it; the next useful sleeping
                // deadline is the next phase edge.
                Some(if exit_start > now { exit_start } else { expiry })
            })
            .min();
        [banners, resting, aim_lapses, slides]
            .into_iter()
            .flatten()
            .min()
    }

    /// A live region, so a screen reader announces a freshly-posted banner. While locked only the
    /// count of what the person allows there is exposed, and nothing when there is nothing to
    /// count.
    pub fn accessibility(&self) -> Option<AccessNode> {
        use ui::a11y::{AccessState, Politeness, Role};
        if self.feed.locked {
            let n = self
                .feed
                .banners
                .iter()
                .filter(|banner| banner.notification.show_on_lock_screen)
                .count();
            if n == 0 {
                return None;
            }
            return Some(
                AccessNode::new(Role::Group)
                    .with_name(t("notification-stack-a11y"))
                    .with_live(Politeness::Polite)
                    .with_value(lock_suppressed_label(n)),
            );
        }
        let latest = self.feed.banners.last().map(|banner| &banner.notification);
        let text = latest.map_or_else(String::new, |t| {
            let body = if t.show_preview { t.body.as_str() } else { "" };
            if body.is_empty() {
                t.title.clone()
            } else {
                format!("{}. {body}", t.title)
            }
        });
        // Ordinary arrivals wait their turn; only an error cuts in. An empty stack publishes an
        // empty value so its clearing is silent.
        let politeness = if latest.is_some_and(|t| t.severity == NotificationSeverity::Error) {
            Politeness::Assertive
        } else {
            Politeness::Polite
        };
        let focused = self.focus.as_ref().and_then(|focus| {
            self.focus_rows
                .iter()
                .flat_map(|row| row.controls.iter())
                .find(|(target, _, _)| target == focus)
                .map(|(_, rect, _)| *rect)
        });
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("notification-stack-a11y"))
                .with_live(politeness)
                .with_value(text)
                .with_children(
                    self.action_buttons
                        .iter()
                        .map(|(rect, label, target)| {
                            AccessNode::new(Role::Button)
                                .with_name(label.clone())
                                .with_bounds(*rect)
                                // Pressed by assistive technology as by a click:
                                // [`Self::perform_semantic`].
                                .with_action_target(target.clone())
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(focused == Some(*rect)),
                                )
                        })
                        .collect(),
                ),
        )
    }
}

#[cfg(test)]
#[path = "stack_tests.rs"]
pub(super) mod tests;
