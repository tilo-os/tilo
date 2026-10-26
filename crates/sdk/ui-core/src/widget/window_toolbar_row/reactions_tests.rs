//! The window's reactions in its bar's row.

use super::*;
use crate::widget::window_toolbar_row::reactions::{REACTION_ADD_TARGET, REACTION_TARGET_PREFIX};
use crate::window_toolbar::{ToolbarReaction, ToolbarReactions, reaction_action};

fn pill(emoji: &str, count: u32, mine: bool) -> ToolbarReaction {
    ToolbarReaction {
        emoji: emoji.into(),
        count,
        mine,
        names: vec!["Ada".into()],
    }
}

fn reacted(pills: Vec<ToolbarReaction>, can_react: bool) -> WindowToolbar {
    WindowToolbar {
        reactions: Some(ToolbarReactions { pills, can_react }),
        ..browser()
    }
}

fn row_of(toolbar: &WindowToolbar, width: f64, compact: bool) -> ToolbarRow {
    let mut row = ToolbarRow::new();
    row.set_compact(compact);
    row.show(
        7,
        Some(toolbar),
        Theme::dark().motion(),
        crate::motion::frame_now(),
    );
    row.lay_out(Rect::new(40.0, 0.0, 40.0 + width, 40.0));
    row.measure_views(&Theme::dark());
    row
}

/// The reactions stand at the row's end, the add button last, after the
/// app's actions and clear of them; the app's slots share what is left,
/// and the location stays.
#[test]
fn the_reactions_stand_at_the_rows_end_after_the_apps_slots() {
    let toolbar = reacted(vec![pill("👍", 2, true), pill("🎉", 1, false)], true);
    let row = row_of(&toolbar, 900.0, false);
    let slots = row.slots();
    assert_eq!(slots.reactions.len(), 2, "both pills stand");
    assert!(slots.reaction_add.area() > 0.0, "the add button stands");
    assert!(
        (slots.reaction_add.x1 - 940.0).abs() < 0.5,
        "the add button ends the row"
    );
    assert!(slots.reactions[0].x1 <= slots.reactions[1].x0);
    assert!(slots.reactions[1].x1 <= slots.reaction_add.x0);
    let last_app = slots
        .actions
        .last()
        .copied()
        .expect("the reload action stands");
    assert!(
        last_app.x1 + SECTION_GAP <= slots.reactions[0].x0 + 0.5,
        "the app's actions stand clear before them: {last_app:?} {:?}",
        slots.reactions[0]
    );
    assert!(slots.location.width() > 0.0, "the location stays");
    for rect in slots.reactions.iter().chain([&slots.reaction_add]) {
        assert!(
            (rect.center().y - 20.0).abs() < 0.5,
            "on the row's centre line"
        );
    }

    let quiet = row_of(&browser(), 900.0, false);
    assert!(quiet.slots().reactions.is_empty());
    assert_eq!(quiet.slots().reaction_add, Rect::ZERO);
}

/// However many reactions there are, they take at most a third of the row,
/// the last giving way first, and the add button stays; a compact window
/// shows one pill at most.
#[test]
fn the_reactions_give_way_to_the_app_and_a_compact_window_shows_one() {
    let many: Vec<ToolbarReaction> = ["👍", "🎉", "❤️", "😂", "🚀", "👀", "🙏", "🔥", "✅", "💯"]
        .into_iter()
        .map(|emoji| pill(emoji, 3, false))
        .collect();
    let toolbar = reacted(many, true);
    let row = row_of(&toolbar, 600.0, false);
    let slots = row.slots();
    let start = slots
        .reactions
        .first()
        .map_or(slots.reaction_add.x0, |r| r.x0);
    assert!(
        slots.reaction_add.x1 - start <= 600.0 / 3.0 + 0.5,
        "a third of the row at most"
    );
    assert!(slots.reactions.len() < 10, "the last pills gave way");
    assert!(slots.reaction_add.area() > 0.0, "the add button stays");

    let compact = row_of(&toolbar, 600.0, true);
    assert!(compact.slots().reactions.len() <= 1);
    assert!(compact.slots().reaction_add.area() > 0.0);

    // Someone who may not react keeps the first pill, however narrow.
    let viewer = row_of(&reacted(vec![pill("👍", 2, false)], false), 200.0, true);
    assert_eq!(viewer.slots().reactions.len(), 1);
    assert_eq!(viewer.slots().reaction_add, Rect::ZERO);
}

/// A press on a pill is the window's reaction with its emoji; the add
/// button opens the picker; someone who may not react only sees them.
/// Assistive technology finds each pill and the add button and runs them.
#[test]
fn a_pill_reacts_through_the_window_and_the_add_button_opens_the_picker() {
    let toolbar = reacted(vec![pill("👍", 2, true), pill("🎉", 1, false)], true);
    let mut row = row_of(&toolbar, 900.0, false);
    let party = row.slots().reactions[1].center();
    assert!(row.press(party));
    assert!(row.release(party));
    assert_eq!(
        row.take_events(),
        vec![(
            7,
            ToolbarEvent::Action {
                id: reaction_action("🎉")
            }
        )]
    );
    assert!(!row.pressing_app_slot());

    let add = row.slots().reaction_add.center();
    assert!(row.press(add));
    assert!(row.release(add));
    assert!(row.popup_open(), "the emoji picker opens");
    assert!(
        row.take_events().is_empty(),
        "opening it reacts with nothing"
    );

    assert!(row.perform(&format!("{REACTION_TARGET_PREFIX}🚀")));
    assert_eq!(
        row.take_events(),
        vec![(
            7,
            ToolbarEvent::Action {
                id: reaction_action("🚀")
            }
        )]
    );

    let viewer_toolbar = reacted(vec![pill("👍", 2, false)], false);
    let mut viewer = row_of(&viewer_toolbar, 900.0, false);
    let thumbs = viewer.slots().reactions[0].center();
    viewer.press(thumbs);
    viewer.release(thumbs);
    assert!(
        viewer.take_events().is_empty(),
        "a viewer's press reacts with nothing"
    );
    assert!(!viewer.perform(REACTION_ADD_TARGET));
    assert!(!viewer.popup_open());
}
