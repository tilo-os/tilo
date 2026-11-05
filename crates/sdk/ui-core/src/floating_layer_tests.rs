//! The floating layer's placement as invariants: what a frame draws is
//! checked against the keep-clear rules for every shape of screen, banner,
//! readout and set of activities, and the pill's order is the documented one.

use super::*;
use crate::live_activity::{Control, ControlKind, Kind, Tone};
use vello::kurbo::Point;

fn work(width: f64, height: f64) -> Rect {
    Rect::new(0.0, 32.0, width, 32.0 + height)
}

fn screen(width: f64, height: f64) -> Placement {
    let work = work(width, height);
    Placement {
        work,
        bar: Rect::new(work.x0, work.y0 - 32.0, work.x1, work.y0),
        banners: None,
        readout: None,
    }
}

fn activity(kind: Kind, id: &str) -> Summary {
    Summary::new(kind, format!("{id} title"), "4:32").with_id(id)
}

fn tall(kind: Kind, id: &str) -> Summary {
    activity(kind, id)
        .with_progress(0.4)
        .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
        .with_control(Control::new("add", "+1 min", ControlKind::Secondary))
        .with_control(Control::new("close", "End", ControlKind::Close))
}

fn cards(shapes: &[Summary]) -> Vec<Card> {
    shapes
        .iter()
        .enumerate()
        .map(|(at, summary)| Card::new(format!("so.tilo.app{at}"), summary.clone()))
        .collect()
}

fn intersects(a: Rect, b: Rect) -> bool {
    let overlap = a.intersect(b);
    overlap.width() > 0.5 && overlap.height() > 0.5
}

/// Every rule the layer promises, over one staging of the screen.
fn assert_layer(placement: &Placement, corner: Corner, held: &[Card], layer: &Layer) {
    let work = placement.work;
    for (at, card) in layer.cards.iter().enumerate() {
        let rect = card.rect;
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "{corner:?} {at}: a card has a size"
        );
        if card.loose {
            // A card the person dragged keeps a strip to grab, and never
            // hides under the bar.
            let inside = rect.intersect(work);
            assert!(
                inside.width() >= REACHABLE - 0.5 && inside.height() >= REACHABLE - 0.5,
                "{corner:?} {at}: {rect:?} keeps {REACHABLE}px reachable inside {work:?}"
            );
            assert!(rect.y0 >= work.y0 - 0.5, "{corner:?} {at}: under the bar");
            continue;
        }
        assert!(
            rect.x0 >= work.x0 - 0.5
                && rect.y0 >= work.y0 - 0.5
                && rect.x1 <= work.x1 + 0.5
                && rect.y1 <= work.y1 + 0.5,
            "{corner:?} {at}: {rect:?} stays inside {work:?}"
        );
        assert!(
            !intersects(rect, placement.bar),
            "{corner:?} {at}: {rect:?} never under the bar {:?}",
            placement.bar
        );
        if let Some(banners) = placement.banners {
            assert!(
                !intersects(rect, banners),
                "{corner:?} {at}: {rect:?} never under a banner {banners:?}"
            );
        }
        if let Some(readout) = placement.readout {
            assert!(
                !intersects(rect, readout),
                "{corner:?} {at}: {rect:?} never in the readout's band {readout:?}"
            );
        }
        for (other_at, other) in layer.cards.iter().enumerate().skip(at + 1) {
            if other.loose {
                continue;
            }
            assert!(
                !intersects(rect, other.rect),
                "{corner:?}: docked cards {at} {rect:?} and {other_at} {:?} overlap",
                other.rect
            );
        }
    }
    // Nothing is lost: every activity is on screen or waiting in the pill.
    assert_eq!(
        layer.cards.len() + layer.folded.len(),
        held.len(),
        "{corner:?}: every activity is accounted for"
    );
}

const CORNERS: [Corner; 4] = [
    Corner::TopLeft,
    Corner::TopRight,
    Corner::BottomLeft,
    Corner::BottomRight,
];

/// Screens people use, phone-width to wide, with and without the corners a
/// banner, the readout or a large text setting takes.
fn placements() -> Vec<Placement> {
    let mut screens = Vec::new();
    for (width, height) in [
        (1280.0, 800.0),
        (1920.0, 1040.0),
        (2560.0, 1400.0),
        (721.0, 480.0),
        (720.0, 800.0),
        (420.0, 800.0),
        (320.0, 560.0),
        (1024.0, 641.0),
    ] {
        let plain = screen(width, height);
        let work = plain.work;
        let banner = Rect::new(work.x1 - 380.0, work.y0, work.x1, work.y0 + 132.0);
        let readout = Rect::new(work.x0, work.y1 - 120.0, work.x1, work.y1);
        screens.push(plain);
        screens.push(Placement {
            banners: Some(banner),
            ..plain
        });
        screens.push(Placement {
            readout: Some(readout),
            ..plain
        });
        screens.push(Placement {
            banners: Some(banner),
            readout: Some(readout),
            ..plain
        });
        // A banner and the readout at the top, with the work area under it.
        screens.push(Placement {
            banners: Some(banner),
            readout: Some(Rect::new(work.x0, work.y0, work.x1, work.y0 + 120.0)),
            ..plain
        });
    }
    screens
}

/// Activity sets people run at once: one timer, a call beside a timer, a
/// full house, and more than the layer expands.
fn sets() -> Vec<Vec<Summary>> {
    vec![
        vec![],
        vec![activity(Kind::Timer, "tea")],
        vec![tall(Kind::Timer, "tea")],
        vec![activity(Kind::Call, "call"), activity(Kind::Timer, "tea")],
        vec![
            activity(Kind::Call, "call"),
            activity(Kind::Navigation, "route"),
            tall(Kind::Timer, "tea"),
            activity(Kind::Music, "song"),
            activity(Kind::Stopwatch, "watch"),
        ],
        vec![
            tall(Kind::Call, "call"),
            tall(Kind::Navigation, "route"),
            tall(Kind::Timer, "tea"),
            tall(Kind::Timer, "coffee"),
            tall(Kind::Stopwatch, "watch"),
            tall(Kind::Other, "job"),
        ],
    ]
}

/// The layer's answer holds every keep-clear rule on every staged screen.
#[test]
fn docked_cards_keep_clear_of_each_other_the_bar_banners_and_the_readout() {
    for placement in placements() {
        for corner in CORNERS {
            for set in sets() {
                let held = cards(&set);
                for expanded in [false, true] {
                    let layer = place(&placement, corner, &held, expanded);
                    assert_layer(&placement, corner, &held, &layer);
                }
            }
        }
    }
}

/// The same screen and the same activities always draw the same layer, in
/// whatever order the board was read in.
#[test]
fn the_layout_is_deterministic_and_order_independent() {
    for placement in placements() {
        for corner in CORNERS {
            for set in sets() {
                let held = cards(&set);
                let first = place(&placement, corner, &held, false);
                assert_eq!(
                    first,
                    place(&placement, corner, &held, false),
                    "{corner:?}: drawing twice draws the same"
                );
                let mut reversed = held.clone();
                reversed.reverse();
                assert_eq!(
                    first,
                    place(&placement, corner, &reversed, false),
                    "{corner:?}: the order the board was read in does not matter"
                );
            }
        }
    }
}

/// Cards rest against the work area's edges: 16 px in, 8 px apart, down the
/// corner the person chose, the most urgent one nearest the corner.
#[test]
fn docked_cards_stand_in_the_chosen_corner_column() {
    let placement = screen(1280.0, 800.0);
    let held = cards(&[activity(Kind::Timer, "tea"), activity(Kind::Call, "call")]);
    let layer = place(&placement, Corner::BottomRight, &held, false);
    assert_eq!(layer.cards.len(), 2);
    let call = &layer.cards[0];
    let tea = &layer.cards[1];
    assert_eq!(call.activity(), "call", "the call stands first");
    assert!(
        (call.rect.x1 - (placement.work.x1 - EDGE_INSET)).abs() < 0.5,
        "16 px from the right edge"
    );
    assert!(
        (call.rect.y1 - (placement.work.y1 - EDGE_INSET)).abs() < 0.5,
        "16 px from the bottom edge"
    );
    assert!(
        (tea.rect.y1 - (call.rect.y0 - CARD_GAP)).abs() < 0.5,
        "the next card stands 8 px above it"
    );
    // The chosen corner moves the column with it.
    let top_left = place(&placement, Corner::TopLeft, &held, false);
    let call = &top_left.cards[0];
    assert!((call.rect.x0 - (placement.work.x0 + EDGE_INSET)).abs() < 0.5);
    assert!((call.rect.y0 - (placement.work.y0 + EDGE_INSET)).abs() < 0.5);
}

/// A banner takes the top of the corner column: the cards below it stand
/// under it, and whatever no longer fits waits in the pill.
#[test]
fn a_banner_pushes_the_docked_cards_out_of_its_column() {
    let mut placement = screen(1280.0, 800.0);
    let work = placement.work;
    let held = cards(&[
        tall(Kind::Call, "call"),
        tall(Kind::Navigation, "route"),
        tall(Kind::Timer, "tea"),
    ]);
    let plain = place(&placement, Corner::BottomRight, &held, false);
    assert_eq!(plain.cards.len(), 3, "three cards fit without a banner");
    placement.banners = Some(Rect::new(
        work.x1 - 360.0,
        work.y0,
        work.x1,
        work.y0 + 300.0,
    ));
    let pushed = place(&placement, Corner::BottomRight, &held, false);
    assert_layer(&placement, Corner::BottomRight, &held, &pushed);
    for card in &pushed.cards {
        assert!(
            card.rect.y0 >= work.y0 + 300.0,
            "{:?} stands below the banner",
            card.rect
        );
    }
    assert!(
        pushed.cards.len() < plain.cards.len()
            || pushed
                .cards
                .iter()
                .all(|card| card.rect.y0 >= work.y0 + 300.0),
        "the column made room"
    );
}

/// The readout's band is momentary: while it is up the cards stand above it,
/// and it never takes a card's place for good.
#[test]
fn the_readout_band_steps_the_cards_out_of_its_way() {
    let mut placement = screen(1280.0, 800.0);
    let work = placement.work;
    let held = cards(&[tall(Kind::Call, "call"), tall(Kind::Timer, "tea")]);
    let plain = place(&placement, Corner::BottomRight, &held, false);
    assert_eq!(plain.cards.len(), 2);
    placement.readout = Some(Rect::new(work.x0, work.y1 - 140.0, work.x1, work.y1));
    let stepped = place(&placement, Corner::BottomRight, &held, false);
    assert_layer(&placement, Corner::BottomRight, &held, &stepped);
    for card in &stepped.cards {
        assert!(
            card.rect.y1 <= work.y1 - 140.0 + 0.5,
            "{:?} stands above the readout",
            card.rect
        );
    }
}

/// A phone has no floating cards at rest: the pill is the only resting form.
/// Expanding drops one full-width card from the bar.
#[test]
fn a_phone_keeps_its_cards_in_the_pill_until_the_person_expands_one() {
    let placement = screen(420.0, 800.0);
    let held = cards(&[activity(Kind::Call, "call"), activity(Kind::Timer, "tea")]);
    let resting = place(&placement, Corner::BottomRight, &held, false);
    assert!(narrow(placement.work));
    assert!(resting.cards.is_empty(), "nothing floats over content");
    assert_eq!(resting.folded.len(), 2, "both wait in the pill");
    assert!(
        resting
            .folded
            .iter()
            .all(|folded| folded.reason == FoldReason::Phone)
    );
    let expanded = place(&placement, Corner::BottomRight, &held, true);
    assert_layer(&placement, Corner::BottomRight, &held, &expanded);
    let card = expanded.cards.first().expect("one card drops");
    assert_eq!(card.activity(), "call", "the most urgent activity");
    assert!(
        (card.rect.width() - (placement.work.width() - 2.0 * EDGE_INSET)).abs() < 0.5,
        "the card is as wide as the work area"
    );
    assert!(
        (card.rect.y0 - (placement.work.y0 + EDGE_INSET)).abs() < 0.5,
        "it drops from the bar"
    );
    assert_eq!(expanded.folded.len(), 1, "the rest wait in the pill");
}

/// A dragged card stays where the person left it, clamped inside the work
/// area with a strip they can grab, and never under the bar.
#[test]
fn a_card_dragged_loose_stays_where_it_is_left_but_reachable() {
    let placement = screen(1280.0, 800.0);
    let mut loose = Card::new("so.tilo.clocks", tall(Kind::Timer, "tea"));
    for at in [
        Point::new(-900.0, -400.0),
        Point::new(4000.0, 3000.0),
        Point::new(200.0, 100.0),
        Point::new(1270.0, 0.0),
    ] {
        loose.loose = Some(Rect::from_origin_size(at, (300.0, 120.0)));
        let layer = place(&placement, Corner::BottomRight, &[loose.clone()], false);
        assert_layer(&placement, Corner::BottomRight, &[loose.clone()], &layer);
        let card = layer.cards.first().expect("a loose card is drawn");
        assert!(card.loose);
    }
    // A card dragged nowhere near an edge lands exactly where it was put.
    let size = card_size(placement.work, &loose.summary);
    loose.loose = Some(Rect::from_origin_size((500.0, 300.0), (300.0, 120.0)));
    let layer = place(&placement, Corner::BottomRight, &[loose], false);
    assert_eq!(
        layer.cards.first().map(|card| card.rect),
        Some(Rect::from_origin_size((500.0, 300.0), size))
    );
}

/// The pill says the most urgent activity and counts the rest: a call before
/// directions, directions before a timer due within a minute, that timer
/// before another timer, timers before music, music before anything else.
#[test]
fn the_pill_puts_a_call_before_directions_and_a_nearly_due_timer_before_the_rest() {
    let song = activity(Kind::Music, "song");
    let timer = activity(Kind::Timer, "tea").with_countdown(300);
    let soon = activity(Kind::Timer, "coffee").with_countdown(30);
    let route = activity(Kind::Navigation, "route");
    let call = activity(Kind::Call, "call");
    let of = |held: &[&Summary]| {
        pill(
            &held
                .iter()
                .map(|summary| (*summary).clone())
                .collect::<Vec<_>>(),
        )
    };

    assert_eq!(
        of(&[&song, &timer, &route, &soon, &call]).reading(),
        call.reading(),
        "a call stands first"
    );
    assert_eq!(
        of(&[&song, &timer, &route, &soon]).reading(),
        route.reading(),
        "directions before a timer"
    );
    assert_eq!(
        of(&[&song, &timer, &soon])
            .heading
            .map(|summary| summary.id),
        Some("coffee".to_owned()),
        "the timer that is due within a minute"
    );
    assert_eq!(
        of(&[&song, &timer]).heading.map(|summary| summary.id),
        Some("tea".to_owned()),
        "a timer before music"
    );
    let music = of(&[&song]);
    assert_eq!(
        music.heading.map(|summary| summary.id),
        Some("song".to_owned())
    );
    assert_eq!(music.more, 0);
    assert_eq!(
        of(&[&song, &timer, &route, &soon, &call]).more,
        4,
        "one dot each"
    );
    assert!(pill(&[]).is_empty());
}

/// An activity with controls sorts its close last and draws at most three.
#[test]
fn a_summary_draws_at_most_three_controls_with_the_close_last() {
    let mut summary = activity(Kind::Call, "call");
    for (id, kind) in [
        ("close", ControlKind::Close),
        ("mute", ControlKind::Primary),
        ("video", ControlKind::Secondary),
        ("more", ControlKind::Secondary),
    ] {
        summary = summary.with_control(Control::new(id, id, kind));
    }
    let shown: Vec<&str> = summary
        .shown_controls()
        .iter()
        .map(|control| control.id.as_str())
        .collect();
    assert_eq!(shown, ["mute", "video", "close"]);
    assert_eq!(
        summary.first_control().map(|control| control.id.as_str()),
        Some("mute")
    );
    assert_eq!(
        summary.close_control().map(|control| control.id.as_str()),
        Some("close")
    );
    assert!(matches!(summary.tone, Tone::Neutral));
}

/// Whatever an activity says, its parts stand inside the card the layer
/// sizes for it: the header with its two controls, then its reading, its
/// progress and the controls it offers.
#[test]
fn a_cards_parts_fit_the_height_card_size_reserves() {
    for summary in sets().into_iter().flatten() {
        let rect = Rect::from_origin_size((100.0, 100.0), card_size(work(1280.0, 800.0), &summary));
        let inside = rect.inflate(-CARD_PADDING, -CARD_PADDING);
        let parts = card_parts(rect, &summary);
        for part in [
            ("mark", parts.mark),
            ("title", parts.title),
            ("collapse", parts.collapse),
            ("close", parts.close),
        ]
        .into_iter()
        .chain(parts.value.map(|part| ("value", part)))
        .chain(parts.progress.map(|part| ("progress", part)))
        {
            assert!(
                part.1.y0 >= inside.y0 - 0.5 && part.1.y1 <= inside.y1 + 0.5,
                "{} stands in {rect:?}: {:?}",
                part.0,
                part.1
            );
        }
        assert!(parts.collapse.x1 <= parts.close.x0 - 3.5);
        assert!(parts.title.x1 <= parts.collapse.x0);
        assert_eq!(parts.controls.len(), summary.action_controls().len());
        for (id, control) in &parts.controls {
            assert!(
                control.x0 >= inside.x0 - 0.5
                    && control.x1 <= inside.x1 + 0.5
                    && control.y1 <= inside.y1 + 0.5,
                "control {id} stands in {rect:?}: {control:?}"
            );
        }
    }
}
