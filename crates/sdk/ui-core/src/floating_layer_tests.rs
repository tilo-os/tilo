//! The floating layer's placement as invariants: what a frame draws is checked against the keep-clear
//! rules for every shape of screen, banner, readout and set of activities, and the pill's order is the
//! documented one.

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
        caret: None,
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

fn was_loose(card: &Placed, held: &[Card]) -> bool {
    held.iter().any(|source| {
        source.app == card.app
            && source.summary.id == card.summary.id
            && source.loose.is_some()
            && !source.rich
    })
}

fn assert_layer(placement: &Placement, corner: Corner, held: &[Card], layer: &Layer) {
    let work = placement.work;
    for (at, card) in layer.cards.iter().enumerate() {
        let rect = card.rect;
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "{corner:?} {at}: a card has a size"
        );
        if was_loose(card, held) {
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
        if let Some(caret) = placement.caret {
            assert!(
                !intersects(rect, caret.inflate(0.0, FIELD_REACH)),
                "{corner:?} {at}: {rect:?} never over the field typed in at {caret:?}"
            );
        }
        for (other_at, other) in layer.cards.iter().enumerate().skip(at + 1) {
            if was_loose(other, held) {
                continue;
            }
            assert!(
                !intersects(rect, other.rect),
                "{corner:?}: docked cards {at} {rect:?} and {other_at} {:?} overlap",
                other.rect
            );
        }
    }
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
        for y in [work.y1 - 60.0, work.y0 + 70.0] {
            screens.push(Placement {
                caret: Some(Rect::new(work.x1 - 200.0, y, work.x1 - 199.0, y + 18.0)),
                ..plain
            });
        }
        screens.push(Placement {
            banners: Some(banner),
            readout: Some(Rect::new(work.x0, work.y0, work.x1, work.y0 + 120.0)),
            ..plain
        });
    }
    screens
}

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
    assert_eq!(call.summary.id, "call", "the call stands first");
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
    let top_left = place(&placement, Corner::TopLeft, &held, false);
    let call = &top_left.cards[0];
    assert!((call.rect.x0 - (placement.work.x0 + EDGE_INSET)).abs() < 0.5);
    assert!((call.rect.y0 - (placement.work.y0 + EDGE_INSET)).abs() < 0.5);
}

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
    assert_eq!(card.summary.id, "call", "the most urgent activity");
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

#[test]
fn a_phone_stands_the_card_its_app_presents_before_its_window_is_up() {
    let placement = screen(420.0, 800.0);
    let mut held = cards(&[activity(Kind::Call, "call"), activity(Kind::Timer, "tea")]);
    held[0].size = Some(Size::new(376.0, 256.0));
    let layer = place(&placement, Corner::BottomRight, &held, false);
    let card = layer.cards.first().expect("the app's card stands");
    assert_eq!(card.summary.id, "call");
    assert!(
        (card.rect.width() - (placement.work.width() - 2.0 * EDGE_INSET)).abs() < 0.5,
        "the card is as wide as the work area"
    );
    assert_eq!(layer.folded.len(), 1, "the rest wait in the pill");
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
        assert!(was_loose(card, &[loose.clone()]));
        assert!(
            card.rect.x0 >= placement.work.x0 - 0.5 && card.rect.y0 >= placement.work.y0 - 0.5,
            "{at:?} left it under the bar or off the side: {:?}",
            card.rect
        );
    }
    let size = card_size(placement.work, &loose.summary);
    loose.loose = Some(Rect::from_origin_size((500.0, 300.0), (300.0, 120.0)));
    let layer = place(&placement, Corner::BottomRight, &[loose], false);
    assert_eq!(
        layer.cards.first().map(|card| card.rect),
        Some(Rect::from_origin_size((500.0, 300.0), size))
    );
}

#[test]
fn a_card_its_app_draws_is_as_wide_as_the_column_and_stands_on_its_edge() {
    let placement = screen(1280.0, 800.0);
    let mut call = Card::new("so.tilo.chat", activity(Kind::Call, "call"));
    call.size = Some(Size::new(110.0, 256.0));
    call.rich = true;
    let mut call_again = Card::new("so.tilo.clocks", activity(Kind::Call, "another"));
    call_again.size = Some(Size::new(110.0, 200.0));
    call_again.rich = true;
    let mut route = Card::new("so.tilo.maps", activity(Kind::Navigation, "route"));
    route.size = Some(Size::new(360.0, 240.0));
    route.rich = true;
    let held = vec![call.clone(), call_again.clone(), route.clone()];
    let layer = place(&placement, Corner::BottomRight, &held, false);
    assert_layer(&placement, Corner::BottomRight, &held, &layer);
    let at = |activity: &str| {
        layer
            .cards
            .iter()
            .find(|card| card.summary.id == activity)
            .map(|card| card.rect)
            .expect("every card stands")
    };
    let column = card_size(placement.work, &call.summary).width;
    let call_rect = at("call");
    let route_rect = at("route");
    assert!(
        (call_rect.width() - column).abs() < 0.5 && (route_rect.width() - column).abs() < 0.5,
        "the cards share the layer's one column width: {call_rect:?} {route_rect:?}"
    );
    assert!(
        (call_rect.x1 - route_rect.x1).abs() < 0.5,
        "the cards share one right edge"
    );
    assert!(
        (call_rect.height() - 256.0).abs() < 0.5,
        "the height its app asked for stands"
    );
    assert!(
        (at("another").height() - 200.0).abs() < 0.5,
        "each card keeps its own height"
    );
}

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
                .map(|summary| ("so.tilo.app".to_owned(), (*summary).clone()))
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
            .map(|named| named.summary.id),
        Some("coffee".to_owned()),
        "the timer that is due within a minute"
    );
    assert_eq!(
        of(&[&song, &timer]).heading.map(|named| named.summary.id),
        Some("tea".to_owned()),
        "a timer before music"
    );
    let music = of(&[&song]);
    assert_eq!(
        music.heading.map(|named| named.summary.id),
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

#[test]
fn a_summary_draws_three_actions_and_its_close_beside_them() {
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
        .action_controls()
        .iter()
        .map(|control| control.id.as_str())
        .collect();
    assert_eq!(
        shown,
        ["mute", "video", "more"],
        "three actions, the close beside them"
    );
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

#[test]
fn a_cards_parts_fit_the_height_card_size_reserves() {
    for summary in sets().into_iter().flatten() {
        let rect = Rect::from_origin_size((100.0, 100.0), card_size(work(1280.0, 800.0), &summary));
        let inside = rect.inflate(-CARD_PADDING, -CARD_PADDING);
        let parts = card_parts(rect, &summary);
        for part in [
            ("icon", parts.icon),
            ("collapse", parts.collapse),
            ("close", parts.close),
            ("hero", parts.hero),
        ]
        .into_iter()
        .chain(parts.title.map(|part| ("title", part)))
        .chain(parts.subtitle.map(|part| ("subtitle", part)))
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
        if let Some(title) = parts.title {
            assert!(title.x1 <= parts.collapse.x0);
        }
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

/// A card that says a line under its reading stacks its controls under it:
/// the line and the controls never share pixels, and the rows end where
/// `card_size` put the card's edge.
#[test]
fn a_line_under_the_reading_stands_above_the_controls() {
    let summary = activity(Kind::Stopwatch, "watch")
        .with_subtitle("The whole desktop")
        .with_control(Control::new("stop", "Stop", ControlKind::Primary))
        .with_control(Control::new("pause", "Pause", ControlKind::Secondary));
    let rect = Rect::from_origin_size((100.0, 100.0), card_size(work(1280.0, 800.0), &summary));
    let inside = rect.inflate(-CARD_PADDING, -CARD_PADDING);
    let parts = card_parts(rect, &summary);
    let line = parts.subtitle.expect("the line under the reading");
    let row = parts.controls_row.expect("the controls' row");
    assert!(
        line.y1 <= row.y0 + 0.5,
        "the line {line:?} stands clear of the controls {row:?}"
    );
    assert!(
        (row.y1 - inside.y1).abs() <= 0.5,
        "the controls end at the card's edge: {row:?} in {inside:?}"
    );
    for (id, control) in &parts.controls {
        assert!(
            control.y0 >= line.y1 - 0.5,
            "control {id} {control:?} clears the line {line:?}"
        );
    }
}

#[test]
fn a_card_without_a_cover_holds_the_lines_it_draws() {
    let summary = Summary::new(Kind::Music, "Glasslight", "3:34")
        .with_id("song")
        .with_subtitle("Aurora Skies")
        .with_progress(0.37)
        .with_length(215)
        .with_control(Control::new("play-pause", "Pause", ControlKind::Primary));
    let size = card_size(work(1280.0, 800.0), &summary);
    let rect = Rect::from_origin_size((100.0, 100.0), size);
    let parts = card_parts(rect, &summary);
    let subtitle = parts.subtitle.expect("the song has a subtitle");
    assert!(
        subtitle.y1 <= rect.y1 - CARD_PADDING + 0.5,
        "the subtitle stands in the card: {subtitle:?} in {rect:?}"
    );
    assert_eq!(
        parts.height, size.height,
        "the card is as tall as the rows it draws"
    );
}

#[test]
fn a_work_area_shorter_than_a_card_still_places_it() {
    for (width, height) in [(1280.0, 800.0), (320.0, 90.0), (200.0, 60.0), (40.0, 30.0)] {
        let placement = screen(width, height);
        let held = cards(&[tall(Kind::Timer, "a"), tall(Kind::Music, "b")]);
        for corner in [Corner::BottomRight, Corner::TopLeft] {
            let layer = place(&placement, corner, &held, true);
            for card in &layer.cards {
                assert!(
                    card.rect.x0 >= placement.work.x0 - 0.5
                        && card.rect.y0 >= placement.work.y0 - 0.5,
                    "{width}x{height} {corner:?}: {:?} starts in {:?}",
                    card.rect,
                    placement.work
                );
            }
        }
    }
}

#[test]
fn a_shot_heads_its_card_with_the_picture_itself() {
    let tile = Summary::new(Kind::Other, "Screenshot", "")
        .with_id("shot-1")
        .with_artwork("data:image/jpeg;base64,AA");
    let hero = Summary::new(Kind::Other, "Screenshot", "")
        .with_id("shot-1")
        .with_hero_artwork("data:image/jpeg;base64,AA");
    let work = work(1280.0, 800.0);
    let size = card_size(work, &hero);
    let rect = Rect::from_origin_size((100.0, 100.0), size);
    let inside = rect.inflate(-CARD_PADDING, -CARD_PADDING);
    let parts = card_parts(rect, &hero);
    let picture = parts.artwork.expect("the picture heads the card");
    assert!(
        (picture.width() - inside.width()).abs() <= 0.5,
        "the picture is as wide as the card: {picture:?} in {inside:?}"
    );
    assert!(
        (picture.height() - picture.width() * 9.0 / 16.0).abs() <= 0.5,
        "the picture keeps a screenshot's shape: {picture:?}"
    );
    assert!(
        parts.hero.y0 >= picture.y1,
        "the title stands under the picture: {:?} under {picture:?}",
        parts.hero
    );
    assert!(
        size.height > card_size(work, &tile).height,
        "a card headed with a picture makes room for it"
    );
}

#[test]
fn a_screenshots_controls_are_labelled_in_their_slots() {
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use crate::tokens::TextRole;

    let summary = Summary::new(Kind::Other, "Screenshot", "")
        .with_id("shot-1")
        .with_subtitle("Screenshot 2026-07-30 12.00.05.png")
        .with_hero_artwork("data:image/jpeg;base64,AA")
        .with_control(Control::new("copy-shot", "Copy", ControlKind::Primary).with_icon("copy"))
        .with_control(Control::new("open-shot", "Open", ControlKind::Secondary).with_icon("image"))
        .with_control(
            Control::new("reveal-shot", "Show in folder", ControlKind::Secondary)
                .with_icon("folder"),
        );
    let rect = Rect::from_origin_size((100.0, 100.0), card_size(work(1280.0, 800.0), &summary));
    let style = Theme::dark().text(TextRole::Label);
    let mut text = TextRenderer::new();
    for (id, slot) in card_parts(rect, &summary).controls {
        let control = summary
            .action_controls()
            .into_iter()
            .find(|control| control.id == id)
            .expect("every slot stands for a control it offers");
        let measured = f64::from(text.measure_style(&control.label, None, &style).0);
        assert!(
            measured + 24.0 <= slot.width(),
            "{} ({measured} wide) fits its slot {slot:?}",
            control.label
        );
    }
}
