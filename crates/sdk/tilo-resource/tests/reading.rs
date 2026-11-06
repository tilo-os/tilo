//! A record's state rebuilt from the reading its document host publishes
//! (`docs/architecture/apps.md` §2–§3): a view whose app holds no document
//! reads exactly the state the host read from the one it keeps.
//!
//! The host answers a reading ([`Resource::state_json`] /
//! [`tilo_resource::ResourceKind::to_json`]); the app's process turns it back
//! into its own model type with [`tilo_resource::state_from_reading`], so the
//! fields a view reads are the fields the host holds — never a second reading
//! of the record made by the app.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions of a known shape"
)]

use tilo_resource::{Replica, Resource, ResourceKind, state_from_reading};

use crate::golden_path::Board;

#[test]
fn the_reading_a_host_publishes_rebuilds_the_state_its_view_reads() {
    // The host's record: a board two actions moved.
    let mut replica = Replica::<Board>::new();
    replica.apply(Board::RENAME.with("Launch")).unwrap();
    replica.apply(Board::ADD_CARD.with("Ship it")).unwrap();
    let host_state = replica.state().unwrap();

    // The host answers the reading: what it offers the app's process.
    let reading = Board::state_json(&host_state).expect("a board has a JSON form");
    let kind = ResourceKind::of::<Board>();
    let by_kind = kind
        .to_json(&host_state)
        .expect("the kind answers the same reading");
    assert_eq!(reading, by_kind, "a record's reading is its model's JSON");

    // The app's process reads the state from that reading alone: no document,
    // no store, and every field the view reads is the host's.
    let rebuilt: Board = state_from_reading::<Board>(&reading).expect("the reading is the board");
    assert_eq!(rebuilt, host_state, "the reading names the host's state");
    assert_eq!(rebuilt.cards.len(), 1, "the card the host's action made");
    let card = rebuilt.cards.values().next().expect("the card");
    assert_eq!(card.text, "Ship it");
}

#[test]
fn a_reading_that_is_not_the_record_is_refused() {
    let refused = state_from_reading::<Board>(&serde_json::json!("not a board"));
    assert!(refused.is_err(), "a reading of another shape is refused");
}
