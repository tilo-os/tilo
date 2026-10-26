//! The desktop's control hints: how to open a window from search, move a
//! window, resize one and move the view, each taught once at the moment it
//! is useful, beside where it is done ([`ui::control_hints`]).
//!
//! The set is data ([`HINTS`]); both hosts drive the same
//! [`ControlHints`] with what their frame shows ([`HintFrame`]) and draw it
//! above the windows, as they do the strip navigator. What the person
//! learns is their own record ([`Learned`]), kept with their personal
//! settings; the host loads it, writes what [`ControlHints::take_changed`]
//! hands back, and folds in what arrives from their other devices.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::control_hints::performed::{self, Performed};
use ui::control_hints::{Cue, Gesture, HintLayer, HintLook, HintPlace, HintSpec, Learned, Quiet};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::EffectiveMotion;
use ui::widget::floating_surface::PopoverSide;
use ui::widget::{HintPlaces, WindowEdge};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// What the desktop's hints are cued and taught by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The person did this.
    Did(Performed),
    /// The shown strip holds two windows or more.
    TwoWindows,
    /// Windows of the shown strip lie out of view.
    WindowsOutOfView,
}

/// The desktop's control hints, in the order they are offered.
pub static HINTS: &[HintSpec<Signal>] = &[
    HintSpec {
        id: "open-from-search",
        when: &[Cue::After(Duration::from_secs(17))],
        only_while: &[],
        learned_by: &[
            Signal::Did(Performed::OpenedSearch),
            Signal::Did(Performed::OpenedWindow),
        ],
        gesture: Gesture::OpenFromSearch,
        words: "control-hint-open",
    },
    HintSpec {
        id: "move-window",
        when: &[
            Cue::Saw(Signal::Did(Performed::HoveredWindowBar)),
            Cue::Saw(Signal::Did(Performed::OpenedWindow)),
        ],
        // A lone window held by its bar only stretches and springs back:
        // moving one means carrying it past another.
        only_while: &[Signal::TwoWindows],
        learned_by: &[Signal::Did(Performed::MovedWindow)],
        gesture: Gesture::MoveWindow,
        words: "control-hint-move",
    },
    HintSpec {
        id: "resize-window",
        when: &[Cue::Holds(Signal::TwoWindows)],
        only_while: &[Signal::TwoWindows],
        learned_by: &[Signal::Did(Performed::ResizedWindow)],
        gesture: Gesture::ResizeEdge,
        words: "control-hint-resize",
    },
    HintSpec {
        id: "pan-view",
        when: &[Cue::Holds(Signal::WindowsOutOfView)],
        only_while: &[Signal::WindowsOutOfView],
        learned_by: &[Signal::Did(Performed::PannedView)],
        gesture: Gesture::PanView,
        words: "control-hint-pan",
    },
];

/// How long after the last typed key the person counts as typing.
const TYPING_PAUSE: Duration = Duration::from_secs(3);

/// How far below the bar the search hint stands.
const SEARCH_DROP: f64 = 24.0;

/// How far inside the desktop's edge the edge the view moves toward lights.
const EDGE_INSET: f64 = 3.0;

/// How often a host reads the record again for what the person's other
/// devices learned meanwhile.
const REREAD_EVERY: Duration = Duration::from_secs(60);

/// What a host's frame shows the hints, in the coordinates it draws in.
#[derive(Clone, Debug, Default)]
pub struct HintFrame {
    /// The shown strip's row ([`ui::widget::GroupRow::hint_places`]).
    pub row: HintPlaces,
    /// The desktop under the bar: where cards may stand.
    pub area: Rect,
    /// The keys that open search, as the keyboard spells them.
    pub search_keys: Vec<String>,
    /// The person's own avatar in the bar, where learned hints go.
    pub avatar: Option<Rect>,
    /// The person's colour.
    pub colour: Option<(u8, u8, u8)>,
    /// Why no hint may begin now, if anything.
    pub quiet: Option<Quiet>,
    /// The person can see the desktop: its first frame is on screen, past
    /// any setup. Hints that wait for a while in ([`Cue::After`]) count
    /// from the first frame that says so.
    pub presented: bool,
}

/// The desktop's control hints on one host.
pub struct ControlHints {
    layer: HintLayer<Signal>,
    /// What the hint on screen says to assistive technology, as last
    /// drawn, for the host's [`HintAnnouncer`].
    said: Rc<RefCell<Option<ui::a11y::AccessNode>>>,
    /// The motion it was last drawn with, for the frames between draws.
    motion: EffectiveMotion,
    /// The person typed until then: no hint begins meanwhile.
    typing_until: Option<Instant>,
    /// When the host next reads the record again for what the person's
    /// other devices learned.
    reread_at: Option<Instant>,
}

impl ControlHints {
    /// Hints over the person's record as last kept.
    #[must_use]
    pub fn new(learned: Learned) -> Self {
        Self {
            layer: HintLayer::new(HINTS, learned),
            said: Rc::default(),
            motion: EffectiveMotion::default(),
            typing_until: None,
            reread_at: None,
        }
    }

    /// Whether hints show at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.layer.coach().learned().enabled
    }

    /// Turn hints on or off (the person's "Show control hints").
    pub fn set_enabled(&mut self, enabled: bool, now: Instant) {
        self.layer.coach_mut().set_enabled(enabled, now);
    }

    /// Fold in the record as another of the person's devices left it.
    pub fn merge(&mut self, other: &Learned, now: Instant) {
        self.layer.coach_mut().merge(other, now);
    }

    /// Whether the host should read the person's kept record again now and
    /// [`Self::merge`] it, for what their other devices learned: once a
    /// minute.
    pub fn reread_due(&mut self, now: Instant) -> bool {
        if self.reread_at.is_some_and(|at| now < at) {
            return false;
        }
        self.reread_at = now.checked_add(REREAD_EVERY);
        true
    }

    /// The record, when it changed: the host keeps it.
    pub fn take_changed(&mut self) -> Option<Learned> {
        self.layer.coach_mut().take_changed()
    }

    /// The person did `what` somewhere the toolkit's controls do not see
    /// (opened search, opened a window from it).
    pub fn observe(&mut self, what: Performed, now: Instant) {
        self.layer.coach_mut().observe(Signal::Did(what), now);
    }

    /// An event the desktop received: a key that types text, or text an
    /// input method committed, holds new hints back until the person
    /// pauses. Shortcuts do not count.
    pub fn heard(&mut self, event: &winit::event::WindowEvent, now: Instant) {
        use winit::event::{ElementState, Ime, WindowEvent};
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
            self.typing_until = now.checked_add(TYPING_PAUSE);
        }
    }

    /// Advance to `now` with what `frame` shows; what the toolkit's
    /// controls saw the person do since the last frame is read here.
    pub fn update(&mut self, frame: &HintFrame, now: Instant) {
        for what in performed::take() {
            self.observe(what, now);
        }
        // Not yet seen, no hint's time runs: what the person does is
        // still heard above.
        if !frame.presented {
            return;
        }
        let row = &frame.row;
        let holds = |signal: Signal| match signal {
            Signal::TwoWindows => row.windows >= 2,
            Signal::WindowsOutOfView => row.beyond.0 || row.beyond.1,
            Signal::Did(_) => false,
        };
        self.layer.update(
            now,
            &holds,
            frame.quiet.or_else(|| {
                self.typing_until
                    .filter(|until| now < *until)
                    .map(|_| Quiet::Typing)
            }),
            &|spec| place(spec, frame),
            self.motion,
        );
    }

    /// Draw the hint on screen.
    pub fn draw(
        &mut self,
        frame: &HintFrame,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        let look = HintLook {
            colour: frame.colour,
            keep_at: frame.avatar.map(|avatar| avatar.center()),
            area: frame.area,
        };
        self.motion = theme.motion();
        self.layer.draw(scene, transform, text, theme, now, &look);
        *self.said.borrow_mut() = self.layer.access_node();
    }

    /// Whether the hint on screen moves at `now`.
    #[must_use]
    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.layer.needs_frame(now, self.motion)
    }

    /// When the hints next need a frame.
    #[must_use]
    pub fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.layer.next_wake(now, self.motion)
    }

    /// The hint on screen, for assistive technology.
    #[must_use]
    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.layer.access_node()
    }

    /// Whether a hint is on screen.
    #[must_use]
    pub fn is_showing(&self) -> bool {
        self.layer.coach().current().is_some()
    }

    /// Everything the hint's last draw painted ([`HintLayer::painted_rect`]).
    #[must_use]
    pub fn painted_rect(&self) -> Option<Rect> {
        self.layer.painted_rect()
    }

    /// Where the hint's card was last drawn.
    #[must_use]
    pub fn card_rect(&self) -> Option<Rect> {
        self.layer.card_rect()
    }
}

impl ControlHints {
    /// Where the host's widget tree tells assistive technology about the
    /// hint on screen.
    #[must_use]
    pub fn announcer(&self) -> HintAnnouncer {
        HintAnnouncer {
            said: Rc::clone(&self.said),
            rect: Rect::ZERO,
        }
    }
}

/// The hint on screen in a host's widget tree, for assistive technology: a
/// polite label with the hint's words and keys while one shows. It draws
/// nothing and takes no input; the hint itself is drawn above the windows.
pub struct HintAnnouncer {
    said: Rc<RefCell<Option<ui::a11y::AccessNode>>>,
    rect: Rect,
}

impl ui::widget::Widget for HintAnnouncer {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn ui::widget::Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        let style = taffy::Style {
            position: taffy::Position::Absolute,
            size: taffy::Size {
                width: taffy::Dimension::length(0.0),
                height: taffy::Dimension::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.said.borrow().clone()
    }

    fn name(&self) -> &'static str {
        "ControlHintAnnouncer"
    }
}

/// How hint `spec` shows over `frame`, if its anchor is on screen.
fn place(spec: &HintSpec<Signal>, frame: &HintFrame) -> Option<HintPlace> {
    if frame.area.is_zero_area() {
        return None;
    }
    let words = crate::i18n::t(spec.words);
    let row = &frame.row;
    let (anchor, side, edge, keys) = match spec.gesture {
        // Search comes down at the top of the desktop, at its middle: the
        // card stands where it will, pointing at nothing, since no control
        // on screen opens it.
        Gesture::OpenFromSearch => (
            Rect::from_points(
                Point::new(frame.area.center().x, frame.area.y0 + SEARCH_DROP),
                Point::new(frame.area.center().x, frame.area.y0 + SEARCH_DROP),
            ),
            PopoverSide::Below,
            None,
            frame.search_keys.clone(),
        ),
        Gesture::MoveWindow => (
            row.hovered_bar.or(row.focused_bar)?,
            PopoverSide::Below,
            None,
            Vec::new(),
        ),
        Gesture::ResizeEdge => {
            let (line, edge) = row.focused_edge?;
            let side = match edge {
                WindowEdge::Leading => PopoverSide::Left,
                WindowEdge::Trailing => PopoverSide::Right,
            };
            (
                Rect::from_points(line.p0, line.p1),
                side,
                Some(line),
                Vec::new(),
            )
        }
        Gesture::PanView => {
            let y = frame.area.center().y;
            let (x, side, inward) = match row.beyond {
                (_, true) => (frame.area.x1, PopoverSide::Left, -EDGE_INSET),
                (true, false) => (frame.area.x0, PopoverSide::Right, EDGE_INSET),
                (false, false) => return None,
            };
            // The side the windows hide past lights along its length.
            let edge = vello::kurbo::Line::new(
                (x + inward, frame.area.y0 + EDGE_INSET),
                (x + inward, frame.area.y1 - EDGE_INSET),
            );
            (
                Rect::from_points(Point::new(x, y), Point::new(x, y)),
                side,
                Some(edge),
                Vec::new(),
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Line;

    fn frame() -> HintFrame {
        HintFrame {
            row: HintPlaces {
                viewport: Rect::new(0.0, 40.0, 1440.0, 900.0),
                windows: 2,
                beyond: (false, true),
                hovered_bar: None,
                focused_bar: Some(Rect::new(100.0, 48.0, 700.0, 84.0)),
                focused_edge: Some((
                    Line::new((700.0, 96.0), (700.0, 880.0)),
                    WindowEdge::Trailing,
                )),
            },
            area: Rect::new(0.0, 40.0, 1440.0, 900.0),
            search_keys: vec!["Ctrl".into(), "K".into()],
            presented: true,
            ..HintFrame::default()
        }
    }

    #[test]
    fn every_hint_has_words_in_every_language() {
        for spec in HINTS {
            let words = crate::i18n::t(spec.words);
            assert_ne!(words, spec.words, "{} has no words", spec.id);
            assert!(words.split_whitespace().count() <= 4, "{words}");
        }
    }

    #[test]
    fn each_hint_stands_where_its_gesture_is_done() {
        let frame = frame();
        let at = |gesture| {
            let spec = HINTS.iter().find(|s| s.gesture == gesture).unwrap();
            place(spec, &frame).unwrap()
        };
        let open = at(Gesture::OpenFromSearch);
        assert_eq!(open.side, PopoverSide::Below);
        assert_eq!(open.keys, vec!["Ctrl".to_owned(), "K".to_owned()]);
        assert_eq!(
            at(Gesture::MoveWindow).anchor,
            Rect::new(100.0, 48.0, 700.0, 84.0)
        );
        let resize = at(Gesture::ResizeEdge);
        assert_eq!(resize.side, PopoverSide::Right);
        assert!(resize.edge.is_some(), "the edge it takes is lit");
        let pan = at(Gesture::PanView);
        assert_eq!(
            pan.side,
            PopoverSide::Left,
            "toward the windows out of view"
        );
    }

    #[test]
    fn a_hint_whose_anchor_is_not_on_screen_waits() {
        let mut frame = frame();
        frame.row.focused_bar = None;
        frame.row.focused_edge = None;
        frame.row.beyond = (false, false);
        for gesture in [Gesture::MoveWindow, Gesture::ResizeEdge, Gesture::PanView] {
            let spec = HINTS.iter().find(|s| s.gesture == gesture).unwrap();
            assert!(place(spec, &frame).is_none(), "{gesture:?}");
        }
    }

    #[test]
    fn what_the_row_saw_teaches_and_the_expert_sees_nothing() {
        let mut hints = ControlHints::new(Learned::default());
        let start = ui::time::now();
        let frame = frame();
        // An expert resizes and moves the view before any hint comes.
        performed::note(Performed::ResizedWindow);
        performed::note(Performed::PannedView);
        hints.update(&frame, start);
        hints.update(&frame, start + Duration::from_secs(1));
        assert!(!hints.is_showing());
        let learned = hints.take_changed().expect("learned");
        assert!(learned.knows("resize-window") && learned.knows("pan-view"));
        assert_eq!(learned.showings("resize-window"), 0);
    }

    #[test]
    fn the_wait_for_search_counts_from_when_the_desktop_is_seen() {
        let mut hints = ControlHints::new(Learned::default());
        let start = ui::time::now();
        let mut frame = frame();
        frame.row.windows = 0;
        frame.row.beyond = (false, false);
        frame.row.focused_bar = None;
        frame.row.focused_edge = None;
        frame.presented = false;
        // Loading for half a minute: nothing on screen yet.
        hints.update(&frame, start);
        hints.update(&frame, start + Duration::from_secs(30));
        assert!(!hints.is_showing());
        frame.presented = true;
        let seen = start + Duration::from_secs(31);
        hints.update(&frame, seen);
        hints.update(&frame, seen + Duration::from_secs(10));
        assert!(!hints.is_showing(), "too soon after it was seen");
        hints.update(&frame, seen + Duration::from_secs(18));
        assert!(hints.is_showing());
    }

    #[test]
    fn moving_is_taught_once_there_is_a_window_to_move_past() {
        let mut hints = ControlHints::new(Learned::default());
        let start = ui::time::now();
        let mut frame = frame();
        frame.row.windows = 1;
        frame.row.beyond = (false, false);
        performed::note(Performed::HoveredWindowBar);
        hints.update(&frame, start);
        assert!(!hints.is_showing(), "a lone window does not move");
        frame.row.windows = 2;
        performed::note(Performed::OpenedWindow);
        hints.update(&frame, start + Duration::from_secs(1));
        assert!(hints.is_showing());
        let learned = hints.take_changed().expect("shown");
        assert_eq!(learned.showings("move-window"), 1);
    }
}
