//! The shell's hints as a shell draws them: windows drawn (side by side, in a strip scrolled along
//! them, on a canvas, as places on it) and the hints over them, frame by frame, with the pointer's
//! events reaching what it points at as the platform delivers them, what the person does noted
//! where the windows note it, and the record's changes read from the events the shell binds to its
//! settings.

use std::collections::BTreeMap;

use vello::kurbo::Rect;

use super::performed::{self, Performed};
use super::{ControlHints, ControlHintsProps};
use crate::component::{Described, Event, Fired, Intent, Value};
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::arrange::{Fit, Length, Sizing, Stack};
use crate::widget::{
    CanvasPlace, Manual, Map, MapProps, MeasureConstraint, PLACE_BAND, PinPlace, PlaceProps,
    Scrollable, Spacer, Widget, WindowFrame, WindowFrameProps,
};

/// The display the shell draws on, and where its two windows stand: room above them, where a hint
/// about a window's bar stands clear of what the windows show.
const AREA: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
const LEFT: Rect = Rect::new(40.0, 160.0, 700.0, 880.0);
const RIGHT: Rect = Rect::new(740.0, 160.0, 1400.0, 880.0);

/// A window `width` wide showing nothing, carrying a resource so its bar moves it.
fn window(key: &str, width: f64, focused: bool) -> WindowFrame {
    WindowFrame::from_props(
        key,
        WindowFrameProps {
            key: key.to_owned(),
            name: Some(key.to_owned()),
            focused,
            carries: Some(format!("{key}.workspace/doc")),
            // A body as wide as the window, so the window takes the width the shell gives it.
            #[allow(clippy::cast_possible_truncation, reason = "a window's width fits f32")]
            body: Some(Box::new(Spacer::fixed(width as f32))),
            ..WindowFrameProps::default()
        },
    )
}

/// A place on a canvas showing nothing, carrying a resource so its handle moves it.
fn place(key: &str) -> CanvasPlace {
    CanvasPlace::from_props(
        key,
        PlaceProps {
            name: Some(key.to_owned()),
            carries: Some(format!("{key}.workspace/doc")),
            ..PlaceProps::default()
        },
    )
}

/// Where a pin stands on a canvas, at its size, with or without a place's band over it.
fn pin(x0: f64, y0: f64, x1: f64, y1: f64, band: f64) -> PinPlace {
    PinPlace {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: Some(y1 - y0),
        band,
        hug: false,
        grows: false,
    }
}

/// The measure a part at `rect` takes: exactly its own, as the shell gives each window its box.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the test's rects are a display's, far inside f32"
)]
fn measured(rect: Rect) -> taffy::Size<MeasureConstraint> {
    taffy::Size {
        width: MeasureConstraint::Exact(rect.width() as f32),
        height: MeasureConstraint::Exact(rect.height() as f32),
    }
}

/// The shell's window over what it shows: the parts each laid out where it stands, the hints over
/// the whole area, and the pointer's events reaching what it points at, as the platform delivers
/// them. Each frame draws at the instant it is asked for.
struct Shell {
    host: FrameHarness,
    queue: crate::Intents<Fired>,
    start: Instant,
}

impl Shell {
    /// Two windows side by side, room above them.
    fn new(props: ControlHintsProps) -> Self {
        Self::with(
            vec![
                (Box::new(window("notes", LEFT.width(), false)), LEFT),
                (Box::new(window("mail", RIGHT.width(), true)), RIGHT),
            ],
            props,
        )
    }

    /// `parts` laid out where each stands, and the hints over the whole area.
    fn with(parts: Vec<(Box<dyn Widget>, Rect)>, props: ControlHintsProps) -> Self {
        let queue = crate::Intents::default();
        let start = crate::time::now();
        let hints = ControlHints::from_props("hints", Self::props(&queue, props));
        let rects: Vec<Rect> = parts.iter().map(|(_, rect)| *rect).collect();
        let mut region = Manual::new(
            move |cx| -> Result<taffy::Size<f32>, crate::widget::WidgetLayoutError> {
                let available = cx.available();
                for (index, rect) in rects.iter().enumerate() {
                    let _ = cx.measure_child(index, measured(*rect))?;
                    cx.place(index, *rect)?;
                }
                Ok(taffy::Size {
                    width: available.width.max(0.0),
                    height: available.height.max(0.0),
                })
            },
        )
        .filling();
        for (part, _) in parts {
            region = region.child(part);
        }
        let root = crate::widget::Stack::new()
            .push(Box::new(region))
            .push(Box::new(hints));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the display's size, in whole pixels"
        )]
        let viewport = (AREA.width() as u32, AREA.height() as u32);
        let host = FrameHarness::for_root(crate::State::new(), root, viewport, Theme::dark());
        Self { host, queue, start }
    }

    /// The hints' props with the events this shell binds to its record.
    fn props(queue: &crate::Intents<Fired>, props: ControlHintsProps) -> ControlHintsProps {
        let event = |name: &str| Event::View(Intent::new(queue, "hints", name));
        ControlHintsProps {
            learn: event("learn"),
            show: event("show"),
            withdraw: event("withdraw"),
            ..props
        }
    }

    fn hints(&mut self) -> &mut ControlHints {
        self.host
            .widget_mut::<ControlHints>()
            .expect("the hints are drawn")
    }

    /// The shell gives the hints new props: the record and readings change, the events the
    /// shell binds stay.
    fn give(&mut self, props: ControlHintsProps) {
        let props = Self::props(&self.queue, props);
        self.hints().set_props(props);
    }

    /// Draw a frame `ms` milliseconds in: the windows, then the hints over them.
    fn frame(&mut self, ms: u64) {
        let _at = crate::motion::pin_frame_at(self.start + Duration::from_millis(ms));
        self.host.relayout();
    }

    /// The pointer moves to `(x, y)`: what a window's bar or a place's band hears as the pointer
    /// resting on it.
    fn hover(&mut self, x: f64, y: f64) {
        self.host
            .dispatch_to_root(&crate::synthetic_input::cursor_moved(x, y));
    }

    /// The pointer presses at `from`, travels to `to` and lets go there: a window carried by its
    /// bar, a place by its handle.
    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.host.drag_at(from, to);
    }

    /// The name of the hint on screen, in any phase.
    fn current(&mut self) -> Option<&'static str> {
        self.hints().current()
    }

    /// Whether a hint stands on screen, not on its way out.
    fn is_showing(&mut self) -> bool {
        self.hints().is_showing()
    }

    /// Where the hint's pill was last drawn.
    fn card_rect(&mut self) -> Option<Rect> {
        self.hints().card_rect()
    }

    /// The record's changes the hints told the shell since last asked: each event with the hint
    /// it names.
    fn told(&self) -> Vec<(String, String)> {
        self.queue
            .take()
            .into_iter()
            .map(|fired| {
                let hint = match fired.value {
                    Some(Value::Text(hint)) => hint,
                    other => panic!("an event names its hint: {other:?}"),
                };
                (fired.event, hint)
            })
            .collect()
    }
}

fn told(event: &str, hint: &str) -> (String, String) {
    (event.to_owned(), hint.to_owned())
}

#[test]
fn the_move_hint_shows_over_two_windows_and_is_learned_by_moving_one() {
    let mut shell = Shell::new(ControlHintsProps::default());
    // The first frame asks the windows to read where they show text and controls; the next brings
    // it, and nothing shows before that.
    shell.frame(0);
    shell.frame(16);
    assert_eq!(shell.current(), None, "no hint is due yet");
    // The pointer rests on a window's bar: the moment the move hint is for.
    performed::note(Performed::HoveredWindowBar);
    shell.frame(1_000);
    assert_eq!(shell.current(), Some("move-window"));
    assert!(shell.is_showing());
    let card = shell.card_rect().expect("the pill is drawn");
    let content = |window: Rect| Rect::new(window.x0, window.y0 + 80.0, window.x1, window.y1);
    assert!(
        AREA.contains_rect(card) && !card.overlaps(content(LEFT)) && !card.overlaps(content(RIGHT)),
        "the pill stands clear of what the windows show: {card:?}"
    );
    assert_eq!(shell.told(), [told("show", "move-window")]);
    // The person carries a window by its bar: the hint is learned, and the shell told so.
    performed::note(Performed::MovedWindow);
    shell.frame(2_000);
    assert_eq!(shell.told(), [told("learn", "move-window")]);
    // The record the shell keeps comes back as props: nothing more to tell, and the hint never
    // shows again.
    shell.give(ControlHintsProps {
        known: vec!["move-window".to_owned()],
        shown: BTreeMap::from([("move-window".to_owned(), 1)]),
        ..ControlHintsProps::default()
    });
    performed::note(Performed::HoveredWindowBar);
    for second in 70..80 {
        shell.frame(second * 1_000);
        assert_ne!(shell.current(), Some("move-window"));
    }
    assert!(
        shell.told().iter().all(|(_, hint)| hint != "move-window"),
        "a learned hint is never told again"
    );
}

#[test]
fn nothing_begins_while_the_person_is_busy_typing_or_with_someone() {
    for props in [
        ControlHintsProps {
            busy: true,
            ..ControlHintsProps::default()
        },
        ControlHintsProps {
            engaged: true,
            ..ControlHintsProps::default()
        },
        ControlHintsProps {
            typing: true,
            ..ControlHintsProps::default()
        },
        ControlHintsProps {
            searching: true,
            known: vec!["open-from-search".to_owned()],
            ..ControlHintsProps::default()
        },
    ] {
        let mut shell = Shell::new(props);
        shell.frame(0);
        for second in 1..40 {
            performed::note(Performed::HoveredWindowBar);
            shell.frame(second * 1_000);
            assert_eq!(shell.current(), None, "no hint begins");
        }
        assert!(
            shell.told().iter().all(|(event, _)| event != "show"),
            "nothing showed"
        );
    }
}

#[test]
fn opening_search_learns_the_search_hint_and_clearing_the_record_brings_it_back() {
    let mut shell = Shell::new(ControlHintsProps::default());
    shell.frame(0);
    shell.give(ControlHintsProps {
        searching: true,
        ..ControlHintsProps::default()
    });
    assert_eq!(shell.told(), [told("learn", "open-from-search")]);
    // The shell kept it, then the person asked to see every hint again.
    shell.give(ControlHintsProps {
        known: vec!["open-from-search".to_owned()],
        ..ControlHintsProps::default()
    });
    shell.give(ControlHintsProps::default());
    shell.frame(1_000);
    shell.frame(18_000);
    assert_eq!(shell.current(), Some("open-from-search"));
    assert_eq!(shell.told(), [told("show", "open-from-search")]);
}

/// The hint on screen, and where its pill stands, after the frames that bring it.
fn shown(shell: &mut Shell) -> (Option<&'static str>, Rect) {
    shell.frame(0);
    shell.frame(16);
    shell.frame(1_000);
    let card = shell.card_rect().unwrap_or(Rect::ZERO);
    (shell.current(), card)
}

#[test]
fn a_strip_scrolled_short_of_its_windows_teaches_panning_until_the_person_pans() {
    // Three windows in a strip wider than the display: the third lies past its trailing edge.
    // Each window keeps its width, as a strip's windows do, rather than squeeze to the display.
    let width = Sizing {
        fill: None,
        min: Some(Length::Dp(640.0)),
        max: Some(Length::Dp(640.0)),
    };
    let row = ["notes", "mail", "files"]
        .into_iter()
        .fold(Stack::row(), |row, key| {
            row.push_fit(
                Box::new(window(key, 640.0, key == "mail")),
                Fit::hug().width(width),
            )
        });
    let strip = Scrollable::new_horizontal().with_child(Box::new(row));
    let mut shell = Shell::with(vec![(Box::new(strip), AREA)], ControlHintsProps::default());
    let (current, card) = shown(&mut shell);
    assert_eq!(current, Some("pan-view"));
    assert!(
        AREA.contains_rect(card) && card.center().x > AREA.center().x,
        "the pill stands at the side the windows lie past: {card:?}"
    );
    assert_eq!(shell.told(), [told("show", "pan-view")]);
    // The person moves along the strip: learned.
    performed::note(Performed::PannedView);
    shell.frame(2_000);
    assert_eq!(shell.told(), [told("learn", "pan-view")]);
}

#[test]
fn places_on_a_canvas_panned_away_from_one_teach_panning() {
    // Two places on screen, a third past the display's trailing edge: places are windows to the
    // hints as a window frame is.
    let canvas = Map::new()
        .pin(
            "notes",
            pin(40.0, 200.0, 600.0, 800.0, PLACE_BAND),
            Box::new(place("notes")),
        )
        .pin(
            "mail",
            pin(800.0, 200.0, 1300.0, 800.0, PLACE_BAND),
            Box::new(place("mail")),
        )
        .pin(
            "files",
            pin(1600.0, 200.0, 2100.0, 800.0, PLACE_BAND),
            Box::new(place("files")),
        );
    let mut shell = Shell::with(vec![(Box::new(canvas), AREA)], ControlHintsProps::default());
    let (current, card) = shown(&mut shell);
    assert_eq!(current, Some("pan-view"));
    assert!(
        AREA.contains_rect(card) && card.center().x > AREA.center().x,
        "the pill stands at the side the place lies past: {card:?}"
    );
    assert_eq!(shell.told(), [told("show", "pan-view")]);
    performed::note(Performed::PannedView);
    shell.frame(2_000);
    assert_eq!(shell.told(), [told("learn", "pan-view")]);
}

#[test]
fn a_place_taken_by_its_band_teaches_moving_a_window() {
    // Two places on a canvas: a place is a window to the hints, and the band its handle shows in
    // is its bar.
    let canvas = Map::new()
        .pin(
            "notes",
            pin(40.0, 200.0, 600.0, 800.0, PLACE_BAND),
            Box::new(place("notes")),
        )
        .pin(
            "mail",
            pin(800.0, 200.0, 1300.0, 800.0, PLACE_BAND),
            Box::new(place("mail")),
        );
    let mut shell = Shell::with(vec![(Box::new(canvas), AREA)], ControlHintsProps::default());
    shell.frame(0);
    shell.frame(16);
    assert_eq!(shell.current(), None, "no hint is due yet");
    // The pointer comes to rest on the band over the first place's view: where the place is
    // taken, as a window is taken by its bar.
    shell.hover(60.0, 180.0);
    assert!(
        performed::pending(),
        "the place notes the pointer resting on its band, as a window's bar does"
    );
    shell.frame(1_000);
    assert_eq!(shell.current(), Some("move-window"));
    assert!(shell.is_showing());
    let card = shell.card_rect().expect("the pill is drawn");
    assert!(
        AREA.contains_rect(card) && !card.overlaps(Rect::new(40.0, 400.0, 600.0, 900.0)),
        "the pill stands clear of what the place shows: {card:?}"
    );
    assert_eq!(shell.told(), [told("show", "move-window")]);
    // The person carries the place by its handle and lets it go further along the canvas: moved,
    // which is the hint learned.
    shell.drag((60.0, 180.0), (300.0, 500.0));
    shell.frame(2_000);
    assert_eq!(shell.told(), [told("learn", "move-window")]);
}

#[test]
fn a_canvas_that_resizes_by_an_edge_teaches_it_beside_the_window_in_front() {
    let resized = crate::Intents::<Fired>::default();
    let canvas = Map::from_props(
        "canvas",
        MapProps {
            resize: Event::View(Intent::new(&resized, "canvas", "resize")),
            ..MapProps::default()
        },
    )
    .pin(
        "notes",
        pin(40.0, 200.0, 600.0, 800.0, 0.0),
        Box::new(window("notes", 560.0, true)),
    )
    .pin(
        "mail",
        pin(900.0, 200.0, 1400.0, 800.0, 0.0),
        Box::new(window("mail", 500.0, false)),
    );
    let mut shell = Shell::with(vec![(Box::new(canvas), AREA)], ControlHintsProps::default());
    let (current, card) = shown(&mut shell);
    assert_eq!(current, Some("resize-window"));
    assert!(
        card.center().x > 600.0 && card.center().x < 900.0,
        "the pill stands beside the trailing edge of the window in front: {card:?}"
    );
    assert_eq!(shell.told(), [told("show", "resize-window")]);
    // The person drags the edge: learned.
    performed::note(Performed::ResizedWindow);
    shell.frame(2_000);
    assert_eq!(shell.told(), [told("learn", "resize-window")]);
}
