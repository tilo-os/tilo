//! The shell's hints as a shell draws them: windows drawn (side by side, in a strip scrolled along
//! them, on a canvas, as places on it) and the hints over them, frame by frame, with what the
//! person does noted where the windows note it and the record's changes read from the events the
//! shell binds to its settings.

use std::collections::BTreeMap;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::performed::{self, Performed};
use super::{ControlHints, ControlHintsProps};
use crate::component::{Described, Event, Fired, Intent, Value};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::arrange::{Fit, Length, Sizing, Stack};
use crate::widget::{
    CanvasPlace, Map, MapProps, PLACE_BAND, PinPlace, PlaceProps, Scrollable, Spacer, Widget,
    WindowFrame, WindowFrameProps,
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

struct Shell {
    /// What the shell shows under the hints, each laid out where it stands.
    parts: Vec<Box<dyn Widget>>,
    hints: ControlHints,
    queue: crate::Intents<Fired>,
    theme: Theme,
    text: crate::text_render::TextRenderer,
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
        let event = |name: &str| Event::View(Intent::new(&queue, "hints", name));
        let theme = Theme::dark();
        let mut text = crate::font_catalog::fixture_renderer();
        let parts = parts
            .into_iter()
            .map(|(mut part, rect)| {
                for _ in 0..2 {
                    crate::widget::layout::lay_out_detached(&mut *part, rect, &theme, &mut text)
                        .expect("the part lays out");
                }
                part
            })
            .collect();
        let mut hints = ControlHints::from_props(
            "hints",
            ControlHintsProps {
                learn: event("learn"),
                show: event("show"),
                withdraw: event("withdraw"),
                ..props
            },
        );
        crate::widget::layout::lay_out_detached(&mut hints, AREA, &theme, &mut text)
            .expect("the hints lay out");
        Self {
            parts,
            hints,
            queue,
            theme,
            text,
            start: crate::time::now(),
        }
    }

    /// The shell gives the hints new props: the record and readings change, the events the
    /// shell binds stay.
    fn give(&mut self, props: ControlHintsProps) {
        let queue = &self.queue;
        let event = |name: &str| Event::View(Intent::new(queue, "hints", name));
        self.hints.set_props(ControlHintsProps {
            learn: event("learn"),
            show: event("show"),
            withdraw: event("withdraw"),
            ..props
        });
    }

    /// Draw a frame `ms` milliseconds in: the windows, then the hints over them.
    fn frame(&mut self, ms: u64) {
        let _at = crate::motion::pin_frame_at(self.start + Duration::from_millis(ms));
        let mut scene = Scene::new();
        let presence = crate::presence::PresenceCtx::empty();
        let env = presence.draw_env();
        for part in &mut self.parts {
            Widget::draw(
                &mut **part,
                &mut crate::draw::DrawCtx::new(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut self.text,
                    &self.theme,
                    env.clone(),
                ),
            );
        }
        Widget::draw(
            &mut self.hints,
            &mut crate::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                env,
            ),
        );
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
    assert_eq!(shell.hints.current(), None, "no hint is due yet");
    // The pointer rests on a window's bar: the moment the move hint is for.
    performed::note(Performed::HoveredWindowBar);
    shell.frame(1_000);
    assert_eq!(shell.hints.current(), Some("move-window"));
    assert!(shell.hints.is_showing());
    let card = shell.hints.card_rect().expect("the pill is drawn");
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
        assert_ne!(shell.hints.current(), Some("move-window"));
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
            assert_eq!(shell.hints.current(), None, "no hint begins");
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
    assert_eq!(shell.hints.current(), Some("open-from-search"));
    assert_eq!(shell.told(), [told("show", "open-from-search")]);
}

/// The hint on screen, and where its pill stands, after the frames that bring it.
fn shown(shell: &mut Shell) -> (Option<&'static str>, Rect) {
    shell.frame(0);
    shell.frame(16);
    shell.frame(1_000);
    let card = shell.hints.card_rect().unwrap_or(Rect::ZERO);
    (shell.hints.current(), card)
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
