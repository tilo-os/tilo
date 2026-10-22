//! A window whose app's code is still on its way holds the app's place
//! with the opening card, and the app built there takes the card over.

use super::tests::{paint, starting_host};
use super::*;
use vello::kurbo::Rect;

#[test]
fn an_app_whose_code_arrives_takes_over_the_card_its_window_showed() {
    let theme = Theme::default();
    let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
    let mut waiting = TileHost::new(TileContent::Arriving);
    waiting.rect = rect;
    waiting.set_opening_name("Chat");
    assert!(
        Widget::opening(&waiting),
        "the window holds the app's place with the opening card at once"
    );
    assert!(!waiting.uses_host_record());
    let _ = paint(&mut waiting, &theme);

    // The code is in: the window's host is built again, around the app.
    let (mut arrived, drawn) = starting_host(true, false);
    arrived.continue_opening_card(&waiting);
    assert_eq!(arrived.opening_name_for_test(), "Chat");
    let _ = paint(&mut arrived, &theme);
    assert!(drawn.get(), "the arrived app draws at once");
    assert!(
        arrived.handover_in_flight(),
        "the card dissolves over the app instead of cutting away"
    );

    // A card nobody saw has nothing to dissolve.
    let unseen = TileHost::new(TileContent::Arriving);
    let (mut quick, _) = starting_host(true, false);
    quick.continue_opening_card(&unseen);
    let _ = paint(&mut quick, &theme);
    assert!(!quick.handover_in_flight());
}
