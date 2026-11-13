//! The fixed desktop's activities for a capture and the app's own window:
//! the cards it shows and the screen they stand on, staged per scenario.
//!
//! Nothing here reads the session: a capture publishes this feed on the
//! layer's link, exactly as the session publishes its own, so the cards in a
//! picture are the cards the paint draws.

use ui::floating_layer::{Card, Corner, Placement};
use ui::live_activity::{Control, ControlKind, Kind, Summary};
use ui::vello::kurbo::Rect;

use crate::link::Feed;

/// Every screenshot scenario.
pub const SCENARIOS: &[&str] = &["resting", "banner", "narrow", "folded"];

/// The bar over the desk, where the pill shows the same activities.
const BAR: f64 = 32.0;

/// The window a scenario is drawn in.
#[must_use]
pub fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some("narrow") => (420, 800),
        _ => (1280, 800),
    }
}

/// The feed `scenario` shows: its cards on the screen [`viewport`] names.
#[must_use]
pub fn feed(scenario: Option<&str>) -> Feed {
    let (width, height) = viewport(scenario);
    let (width, height) = (f64::from(width), f64::from(height));
    let work = Rect::new(0.0, BAR, width, height);
    let placement = Placement {
        work,
        bar: Rect::new(0.0, 0.0, width, BAR),
        // The notification column takes the top right while a banner shows.
        banners: (scenario == Some("banner")).then(|| {
            Rect::new(
                width - 32.0 - 360.0,
                BAR + 8.0,
                width - 32.0,
                BAR + 8.0 + 96.0,
            )
        }),
        readout: None,
        caret: None,
    };
    let cards = match scenario {
        Some("narrow") => vec![timer()],
        Some("folded") => vec![timer(), music(), call(), transfer()],
        _ => vec![timer(), music()],
    };
    Feed {
        placement,
        // A banner takes the top right, which is where a person who docks
        // their cards there sees the column step down past it.
        corner: if scenario == Some("banner") {
            Corner::TopRight
        } else {
            Corner::BottomRight
        },
        cards,
        expanded: scenario == Some("narrow"),
    }
}

/// The timer the pill names first: the one due soonest.
fn timer() -> Card {
    Card::new(
        "so.tilo.clocks",
        Summary::new(Kind::Timer, "Tea", "4:32")
            .with_id("tea")
            .with_icon("timer")
            .with_progress(0.5)
            .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
            .with_control(Control::new(
                "add-minute",
                "A minute more",
                ControlKind::Secondary,
            ))
            .with_control(Control::new("close", "End", ControlKind::Close)),
    )
}

/// A song: its track, who plays it, and the three controls a deck offers.
fn music() -> Card {
    Card::new(
        "so.tilo.music",
        Summary::new(Kind::Music, "Goldberg Variations", "")
            .with_id("playing")
            .with_icon("music")
            .with_subtitle("Glenn Gould")
            .with_progress(0.25)
            .with_control(
                Control::new("previous", "Previous", ControlKind::Secondary).with_icon("skip-back"),
            )
            .with_control(Control::new("toggle", "Pause", ControlKind::Primary).with_icon("pause"))
            .with_control(
                Control::new("next", "Next", ControlKind::Secondary).with_icon("skip-forward"),
            ),
    )
}

fn call() -> Card {
    Card::new(
        "so.tilo.chat",
        Summary::new(Kind::Call, "Mira", "on the call")
            .with_id("mira")
            .with_icon("phone")
            .with_control(Control::new("toggle", "Join", ControlKind::Primary))
            .with_control(Control::new("close", "Leave", ControlKind::Close)),
    )
}

fn transfer() -> Card {
    Card::new(
        "so.tilo.files",
        Summary::new(Kind::Other, "Field notes", "62%")
            .with_id("field-notes")
            .with_icon("folder")
            .with_control(Control::new("close", "Stop", ControlKind::Close)),
    )
}

#[cfg(test)]
mod tests {
    use ui::floating_layer::place;

    use super::*;

    #[test]
    fn every_scenario_places_every_card_on_its_screen() {
        for scenario in SCENARIOS.iter().copied().map(Some).chain([None]) {
            let feed = feed(scenario);
            let layer = place(&feed.placement, feed.corner, &feed.cards, feed.expanded);
            assert_eq!(
                layer.cards.len() + layer.folded.len(),
                feed.cards.len(),
                "{scenario:?} accounts for every activity"
            );
            let work = feed.placement.work;
            for placed in &layer.cards {
                let card = placed.rect;
                assert!(
                    card.x0 >= work.x0
                        && card.y0 >= work.y0
                        && card.x1 <= work.x1
                        && card.y1 <= work.y1,
                    "{scenario:?} puts {} on its screen: {card:?} in {work:?}",
                    placed.app
                );
            }
        }
    }

    /// A card never shows under a banner, and a column that starts at the
    /// top steps down past it.
    #[test]
    fn a_banner_keeps_the_cards_out_of_its_way() {
        let banners = feed(Some("banner"))
            .placement
            .banners
            .expect("the scenario shows a banner");
        let over = feed(Some("banner"));
        let kept = place(&over.placement, Corner::TopRight, &over.cards, false);
        let resting = feed(Some("resting"));
        let below = place(&resting.placement, Corner::TopRight, &resting.cards, false);
        for card in &kept.cards {
            let rect = card.rect;
            assert!(
                rect.y1 <= banners.y0 || rect.y0 >= banners.y1 || rect.x1 <= banners.x0,
                "{} stays clear of the banner {banners:?}: {rect:?}",
                card.app
            );
        }
        assert!(
            kept.cards[0].rect.y0 > below.cards[0].rect.y0,
            "the banner moved the column down: {:?} then {:?}",
            below.cards[0].rect,
            kept.cards[0].rect
        );
    }
}
