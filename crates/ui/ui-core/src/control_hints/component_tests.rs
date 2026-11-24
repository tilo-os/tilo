//! The shell's hints as a shell draws them: two windows drawn and the hints over them, frame by
//! frame, with what the person does noted where the windows note it and the record's changes read
//! from the events the shell binds to its settings.

use std::collections::BTreeMap;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::performed::{self, Performed};
use super::{ControlHints, ControlHintsProps};
use crate::component::{Described, Event, Fired, Intent, Value};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::{Spacer, Widget, WindowFrame, WindowFrameProps};

/// The display the shell draws on, and where its two windows stand: room above them, where a hint
/// about a window's bar stands clear of what the windows show.
const AREA: Rect = Rect::new(0.0, 0.0, 1440.0, 900.0);
const LEFT: Rect = Rect::new(40.0, 160.0, 700.0, 880.0);
const RIGHT: Rect = Rect::new(740.0, 160.0, 1400.0, 880.0);

struct Shell {
    frames: Vec<WindowFrame>,
    hints: ControlHints,
    queue: crate::Intents<Fired>,
    theme: Theme,
    text: crate::text_render::TextRenderer,
    start: Instant,
}

impl Shell {
    fn new(props: ControlHintsProps) -> Self {
        let queue = crate::Intents::default();
        let event = |name: &str| Event::View(Intent::new(&queue, "hints", name));
        let theme = Theme::dark();
        let mut text = crate::font_catalog::fixture_renderer();
        let frames = [("notes", LEFT, false), ("mail", RIGHT, true)]
            .into_iter()
            .map(|(key, rect, focused)| {
                let mut frame = WindowFrame::from_props(
                    key,
                    WindowFrameProps {
                        key: key.to_owned(),
                        name: Some(key.to_owned()),
                        focused,
                        carries: Some(format!("{key}.workspace/doc")),
                        // A body as wide as the window, so the window takes the width the shell gives it.
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "a window's width fits f32"
                        )]
                        body: Some(Box::new(Spacer::fixed(rect.width() as f32))),
                        ..WindowFrameProps::default()
                    },
                );
                for _ in 0..2 {
                    crate::widget::layout::lay_out_detached(&mut frame, rect, &theme, &mut text)
                        .expect("the window lays out");
                }
                frame
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
            frames,
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
        for frame in &mut self.frames {
            Widget::draw(
                frame,
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
fn nothing_begins_while_the_person_is_busy_or_with_someone() {
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
