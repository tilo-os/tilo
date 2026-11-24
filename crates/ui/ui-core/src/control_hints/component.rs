//! The shell's control hints as a described component (`ui.ControlHints`): the shell's top layer,
//! over everything it shows. It reads where the windows and their edges are as they draw
//! ([`super::anchors`]) and what the person did with them ([`super::performed`]), schedules the
//! shell's hint set ([`super::shell::HINTS`]) and draws the hint on screen. The person's record
//! comes in as props, and what changes in it goes out as events, for the shell to keep through the
//! person's settings: each hint learned, each showing noted, each showing taken back.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};

use taffy::style_helpers::percent;
use taffy::{NodeId, Size as TaffySize, Style, TaffyTree};
use vello::kurbo::{Affine, Line, Point, Rect};
use winit::event::WindowEvent;

use super::anchors::{self, Window};
use super::coach::{MAX_SHOWINGS, Phase};
use super::performed::{self, Performed};
use super::shell::{HINTS, Signal};
use super::{Gesture, HintLayer, HintLook, HintPlace, HintSpec, Learned, Quiet};
use crate::component::{Component, Described, Event, Value};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::EffectiveMotion;
use crate::widget::floating_surface::PopoverSide;
use crate::widget::{TaffyTreeExt, Widget};

/// How long after the last typed key the person counts as typing.
const TYPING_PAUSE: Duration = Duration::from_secs(3);
/// How long after search closes a window that appears counts as opened from it.
const OPENED_FROM_SEARCH: Duration = Duration::from_secs(3);
/// How far below the top of the area the search hint stands: where search comes down.
const SEARCH_DROP: f64 = 24.0;
/// How far inside the area's edge the edge the view moves toward lights.
const EDGE_INSET: f64 = 3.0;
/// How wide a free stretch of a window's bar must be to be where a person takes the window: the
/// slivers between a bar's buttons take it too, but pointing there points at the buttons.
const GRAB_MIN_W: f64 = 32.0;
/// How wide the stretch the move hint points at is.
const GRAB_W: f64 = 40.0;
/// How near a resizing edge stands to a window's side to be that window's.
const EDGE_NEAR: f64 = 6.0;

/// The shell's control hints: a pill that teaches one of the shell's controls at the moment it is
/// useful, beside where it is done, and goes once the person does it. The shell's top layer, over
/// the whole area it is given; it takes no input.
#[derive(Component)]
#[component(name = "ControlHints", role = "Label")]
pub struct ControlHintsProps {
    /// Whether hints show at all: the person's "Show control hints".
    pub shows: bool,
    /// The hints the person has shown they know, by name.
    #[component(data)]
    pub known: Vec<String>,
    /// How often each hint showed, by name.
    #[component(data)]
    pub shown: BTreeMap<String, i64>,
    /// The keys that open search, as the keyboard spells them: what the search hint names.
    pub search_keys: Vec<String>,
    /// The person points by touch: hints name what a finger does.
    pub touch: bool,
    /// Search is open: the person found it, and a window opened from it is opened from search.
    pub searching: bool,
    /// Something has the person's attention (a menu, a sheet, a surface over the windows): no hint
    /// begins, and one on screen waits.
    pub busy: bool,
    /// The person is in a call or follows someone: no hint begins, and one on screen goes.
    pub engaged: bool,
    /// A hint was learned: carries its name.
    #[component(event(value = String))]
    pub learn: Event,
    /// A hint began to show: carries its name.
    #[component(event(value = String))]
    pub show: Event,
    /// A hint begun never reached the screen, so its showing does not count: carries its name.
    #[component(event(value = String))]
    pub withdraw: Event,
}

impl Default for ControlHintsProps {
    fn default() -> Self {
        Self {
            shows: true,
            known: Vec::new(),
            shown: BTreeMap::new(),
            search_keys: Vec::new(),
            touch: false,
            searching: false,
            busy: false,
            engaged: false,
            learn: Event::None,
            show: Event::None,
            withdraw: Event::None,
        }
    }
}

/// The record the props describe.
fn record(props: &ControlHintsProps) -> Learned {
    let mut known = props.known.clone();
    known.sort();
    known.dedup();
    Learned {
        enabled: props.shows,
        known,
        shown: props
            .shown
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(id, count)| (id.clone(), u8::try_from(*count).unwrap_or(u8::MAX)))
            .collect(),
    }
}

/// What the shell shows the hints this frame, in the hints' own coordinates.
#[derive(Default)]
struct Seen {
    windows: Vec<Window>,
    edges: Vec<Line>,
    beyond: (bool, bool),
}

/// [`ControlHintsProps`] built.
pub struct ControlHints {
    rect: Rect,
    layer: HintLayer<Signal>,
    /// The record as the props last gave it.
    record: Learned,
    /// The record as the shell knows it: the props, and what this told it since.
    reported: Learned,
    search_keys: Vec<String>,
    touch: bool,
    searching: bool,
    busy: bool,
    engaged: bool,
    learn: Event,
    show: Event,
    withdraw: Event,
    /// The person typed until then: no hint begins meanwhile.
    typing_until: Option<Instant>,
    /// Search closed then, with this many windows on screen: one more soon after is opened from it.
    search_closed: Option<(Instant, usize)>,
    /// How many windows the last frame showed.
    windows: usize,
    motion: EffectiveMotion,
    /// What the last two draws painted: the hint repaints where it was and where it is.
    painted: [Option<Rect>; 2],
    /// The windows were asked to read where they show text and controls before the last draw.
    reading_ink: bool,
    /// They were just asked: one more frame brings what they read.
    ink_frame: Cell<bool>,
    said: Option<crate::a11y::AccessNode>,
}

impl Described for ControlHints {
    type Props = ControlHintsProps;

    fn from_props(_key: &str, props: ControlHintsProps) -> Self {
        let record = record(&props);
        Self {
            rect: Rect::ZERO,
            layer: HintLayer::new(HINTS, record.clone()),
            reported: record.clone(),
            record,
            search_keys: props.search_keys,
            touch: props.touch,
            searching: props.searching,
            busy: props.busy,
            engaged: props.engaged,
            learn: props.learn,
            show: props.show,
            withdraw: props.withdraw,
            typing_until: None,
            search_closed: None,
            windows: 0,
            motion: EffectiveMotion::default(),
            painted: [None, None],
            reading_ink: false,
            ink_frame: Cell::new(false),
            said: None,
        }
    }

    fn set_props(&mut self, props: ControlHintsProps) -> bool {
        let now = crate::motion::frame_now();
        let record = record(&props);
        if record != self.record {
            self.take_record(&record, now);
        }
        if self.searching && !props.searching {
            self.search_closed = Some((now, self.windows));
        }
        if props.searching && !self.searching {
            self.layer
                .coach_mut()
                .observe(Signal::Did(Performed::OpenedSearch), now);
        }
        self.search_keys = props.search_keys;
        self.touch = props.touch;
        self.searching = props.searching;
        self.busy = props.busy;
        self.engaged = props.engaged;
        self.learn = props.learn;
        self.show = props.show;
        self.withdraw = props.withdraw;
        self.report();
        true
    }
}

impl ControlHints {
    /// Fold in the record as the person's settings now hold it. What their other devices learned
    /// joins what this one knows; a record that dropped what it held before and this still holds
    /// was cleared ("show them again"), and the hints start over from it.
    fn take_record(&mut self, record: &Learned, now: Instant) {
        let held = self.layer.coach().learned();
        let dropped = self
            .record
            .known
            .iter()
            .any(|id| !record.knows(id) && held.knows(id))
            || self.record.shown.iter().any(|(id, count)| {
                record.showings(id) < *count && held.showings(id) > record.showings(id)
            });
        if dropped {
            self.layer = HintLayer::new(HINTS, record.clone());
        } else {
            self.layer.coach_mut().merge(record, now);
        }
        self.record = record.clone();
        self.reported = self.layer.coach().learned().clone();
        let _ = self.layer.coach_mut().take_changed();
    }

    /// Tell the shell what changed in the record since it last heard: each hint learned, each
    /// showing begun or taken back.
    fn report(&mut self) {
        let now = self.layer.coach().learned().clone();
        let _ = self.layer.coach_mut().take_changed();
        let fire = |event: &Event, id: &str| {
            event.fire(Some(Value::Text(id.to_owned())));
        };
        for id in &now.known {
            if !self.reported.knows(id) {
                fire(&self.learn, id);
            }
        }
        for (id, count) in &now.shown {
            for _ in self.reported.showings(id)..*count {
                fire(&self.show, id);
            }
        }
        for (id, count) in &self.reported.shown {
            for _ in now.showings(id)..*count {
                fire(&self.withdraw, id);
            }
        }
        self.reported = now;
    }

    /// Why no hint may begin at `now`, if anything.
    fn quiet(&self, now: Instant) -> Option<Quiet> {
        if self.engaged {
            Some(Quiet::Following)
        } else if self.busy || self.searching {
            Some(Quiet::Busy)
        } else if self.typing_until.is_some_and(|until| now < until) {
            Some(Quiet::Typing)
        } else {
            None
        }
    }

    /// Whether a hint may show soon, so the windows should read where they show text and controls
    /// as they draw: hints are on, one is on screen or still to learn and unseen, and the person
    /// is not typing. Reading that walks the windows' content, so it stops once every hint is
    /// learned or has had its showing.
    fn wants_ink(&self, now: Instant) -> bool {
        let coach = self.layer.coach();
        let learned = coach.learned();
        let typing = self.typing_until.is_some_and(|until| now < until);
        learned.enabled
            && (coach.current().is_some()
                || (!typing
                    && HINTS.iter().any(|spec| {
                        !learned.knows(spec.id) && learned.showings(spec.id) < MAX_SHOWINGS
                    })))
    }

    /// What the windows and the surfaces holding them drew this frame, brought into this
    /// layer's coordinates through `transform`, the one it draws with.
    fn seen(transform: Affine) -> Seen {
        let back = transform.inverse();
        let windows = anchors::windows()
            .into_iter()
            .map(|mut window| {
                if let Some(drawn) = window.drawn.as_mut() {
                    drawn.whole = back.transform_rect_bbox(drawn.whole);
                    drawn.bar = back.transform_rect_bbox(drawn.bar);
                    for ink in &mut drawn.ink {
                        *ink = back.transform_rect_bbox(*ink);
                    }
                }
                window
            })
            .collect();
        let edges = anchors::edges()
            .into_iter()
            .map(|line| back * line)
            .collect();
        let beyond = anchors::beyond();
        anchors::next_pass();
        Seen {
            windows,
            edges,
            beyond,
        }
    }

    /// Advance to `now` with what this frame shows.
    fn update(&mut self, seen: &Seen, now: Instant, ink: bool) {
        for what in performed::take() {
            self.layer.coach_mut().observe(Signal::Did(what), now);
        }
        let count = seen.windows.len();
        if let Some((closed, before)) = self.search_closed {
            if now.saturating_duration_since(closed) > OPENED_FROM_SEARCH {
                self.search_closed = None;
            } else if count > before {
                self.search_closed = None;
                self.layer
                    .coach_mut()
                    .observe(Signal::Did(Performed::OpenedWindow), now);
            }
        }
        self.windows = count;
        let beyond = seen.beyond;
        let holds = |signal: Signal| match signal {
            Signal::TwoWindows => count >= 2,
            Signal::WindowsOutOfView => beyond.0 || beyond.1,
            Signal::Did(_) => false,
        };
        let area = self.rect;
        let words = |spec: &HintSpec<Signal>| {
            crate::i18n::widget_format(
                spec.touch_words
                    .filter(|_| self.touch)
                    .unwrap_or(spec.words),
                &[],
            )
        };
        let keys = self.search_keys.clone();
        let quiet = self.quiet(now);
        self.layer.update(
            now,
            &holds,
            quiet,
            &|spec| place(spec, seen, area, words(spec), &keys, ink),
            self.motion,
        );
    }
}

/// Where on `bar` a person takes the window: the first stretch of it free of the bar's text and
/// controls (`ink`) wide enough to be meant, rather than a sliver between two buttons, pointing
/// at its start. `None` when the bar's title and buttons fill it: the hint then points at the
/// bar's start, where the title lifts the window.
fn grab(bar: Rect, ink: &[Rect]) -> Option<Rect> {
    let mut taken: Vec<(f64, f64)> = ink
        .iter()
        .filter(|rect| rect.y0 < bar.y1 && rect.y1 > bar.y0 && rect.x1 > bar.x0 && rect.x0 < bar.x1)
        .map(|rect| (rect.x0.max(bar.x0), rect.x1.min(bar.x1)))
        .collect();
    taken.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut from = bar.x0;
    for (x0, x1) in taken.into_iter().chain([(bar.x1, bar.x1)]) {
        if x0 - from >= GRAB_MIN_W {
            return Some(Rect::new(from, bar.y0, (from + GRAB_W).min(x0), bar.y1));
        }
        from = from.max(x1);
    }
    None
}

/// How hint `spec` shows over what the shell shows (`seen`) in `area`, if its anchor is on screen.
/// Until the windows have read where they show text and controls (`ink`), no hint stands
/// anywhere: it could stand over what the person reads.
fn place(
    spec: &HintSpec<Signal>,
    seen: &Seen,
    area: Rect,
    words: String,
    search_keys: &[String],
    ink: bool,
) -> Option<HintPlace> {
    if area.is_zero_area() || !ink {
        return None;
    }
    let drawn = || {
        seen.windows
            .iter()
            .filter_map(|w| Some((w, w.drawn.as_ref()?)))
    };
    // The window a hint about one is about: the one under the pointer, else the one in front.
    let about = |movable: bool| {
        drawn()
            .filter(|(w, _)| w.movable || !movable)
            .find(|(w, _)| w.hovered)
            .or_else(|| {
                drawn()
                    .filter(|(w, _)| w.movable || !movable)
                    .find(|(w, _)| w.focused)
            })
    };
    let (anchor, side, edge, keys, along) = match spec.gesture {
        // Search comes down at the top of the area, at its middle: the pill is where search will
        // open, pointing at nothing, since no control on screen opens it.
        Gesture::OpenFromSearch => {
            let at = Point::new(area.center().x, area.y0 + SEARCH_DROP);
            let anchor = Rect::from_points(at, at);
            (
                anchor,
                PopoverSide::Below,
                None,
                search_keys.to_vec(),
                Rect::new(area.x0, anchor.y0, area.x1, area.center().y),
            )
        }
        Gesture::MoveWindow => {
            let (_, window) = about(true)?;
            (
                grab(window.bar, &window.ink).unwrap_or_else(|| {
                    Rect::new(
                        window.bar.x0,
                        window.bar.y0,
                        window.bar.x0 + GRAB_W,
                        window.bar.y1,
                    )
                }),
                PopoverSide::Below,
                None,
                Vec::new(),
                window.bar,
            )
        }
        Gesture::ResizeEdge => {
            let near = |line: &Line, x: f64, whole: Rect| {
                (line.p0.x - x).abs() <= EDGE_NEAR
                    && line.p0.y.min(line.p1.y) < whole.y1
                    && line.p0.y.max(line.p1.y) > whole.y0
            };
            let window = about(false).map(|(_, drawn)| drawn.whole);
            let (line, side) = window
                .and_then(|whole| {
                    seen.edges.iter().find_map(|line| {
                        if near(line, whole.x1, whole) {
                            Some((*line, PopoverSide::Right))
                        } else if near(line, whole.x0, whole) {
                            Some((*line, PopoverSide::Left))
                        } else {
                            None
                        }
                    })
                })
                .or_else(|| seen.edges.first().map(|line| (*line, PopoverSide::Right)))?;
            let anchor = Rect::from_points(line.p0, line.p1);
            if area.intersect(anchor.inflate(1.0, 1.0)).is_zero_area() {
                return None;
            }
            (anchor, side, Some(line), Vec::new(), anchor)
        }
        Gesture::PanView => {
            let y = area.center().y;
            let (x, side, inward) = match seen.beyond {
                (_, true) => (area.x1, PopoverSide::Left, -EDGE_INSET),
                (true, false) => (area.x0, PopoverSide::Right, EDGE_INSET),
                (false, false) => return None,
            };
            // The side the windows hide past lights along its length.
            let line = Line::new(
                (x + inward, area.y0 + EDGE_INSET),
                (x + inward, area.y1 - EDGE_INSET),
            );
            (
                Rect::from_points(Point::new(x, y), Point::new(x, y)),
                side,
                Some(line),
                Vec::new(),
                Rect::from_points(line.p0, line.p1),
            )
        }
    };
    Some(HintPlace {
        anchor,
        side,
        edge,
        words,
        keys,
        points: spec.gesture != Gesture::OpenFromSearch,
        along,
        windows: drawn().map(|(_, drawn)| drawn.whole).collect(),
        bars: drawn()
            .map(|(_, drawn)| drawn.bar)
            .filter(|bar| !bar.is_zero_area())
            .collect(),
        ink: drawn().flat_map(|(_, drawn)| drawn.ink.clone()).collect(),
    })
}

impl Widget for ControlHints {
    crate::described!();

    fn name(&self) -> &'static str {
        "ControlHints"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: TaffySize {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    /// A key that types text, or text an input method committed, holds new hints back until the
    /// person pauses. Shortcuts do not count, and nothing here takes the event.
    fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        use winit::event::{ElementState, Ime};
        let typed = match event {
            WindowEvent::KeyboardInput { event, .. } => {
                event.state == ElementState::Pressed
                    && event
                        .text
                        .as_deref()
                        .is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control))
            }
            WindowEvent::Ime(Ime::Commit(text)) => !text.is_empty(),
            _ => false,
        };
        if typed {
            self.typing_until = crate::motion::frame_now().checked_add(TYPING_PAUSE);
        }
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let now = crate::motion::frame_now();
        self.motion = theme.motion();
        let seen = Self::seen(transform);
        let ink = self.reading_ink;
        self.update(&seen, now, ink);
        let look = HintLook {
            colour: None,
            keep_at: None,
            area: self.rect,
        };
        self.layer.draw(scene, transform, text, theme, now, &look);
        self.painted = [self.layer.painted_rect(), self.painted[0]];
        self.said = self.layer.access_node();
        self.report();
        let reads = self.wants_ink(now);
        if reads && !self.reading_ink {
            self.ink_frame.set(true);
        }
        self.reading_ink = reads;
        anchors::want_ink(reads);
    }

    fn needs_redraw(&self) -> bool {
        self.ink_frame.get()
            || performed::pending()
            || self
                .layer
                .needs_frame(crate::motion::frame_now(), self.motion)
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.ink_frame.replace(false)
            || performed::pending()
            || self.layer.needs_frame(now, self.motion)
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = crate::motion::frame_now();
        let pause = self.typing_until.filter(|until| *until > now);
        match (self.layer.next_wake(now, self.motion), pause) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        regions.extend(self.painted.iter().flatten().copied());
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.said.clone()
    }
}

impl ControlHints {
    /// Whether a hint stands on screen, not on its way out.
    #[must_use]
    pub fn is_showing(&self) -> bool {
        self.layer
            .coach()
            .current()
            .is_some_and(|current| matches!(current.phase, Phase::Shown(_)))
    }

    /// The name of the hint on screen, in any phase.
    #[must_use]
    pub fn current(&self) -> Option<&'static str> {
        let current = self.layer.coach().current()?;
        HINTS.get(current.index).map(|spec| spec.id)
    }

    /// Where the hint's pill was last drawn.
    #[must_use]
    pub fn card_rect(&self) -> Option<Rect> {
        self.layer.card_rect()
    }
}
