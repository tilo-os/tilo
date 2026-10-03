#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::dnd::{DragDropManager, DragPayload};
use vello::kurbo::Point;

#[test]
fn drag_lifecycle_tracks_payload_and_distance() {
    // Case `drag_lifecycle_transitions`.
    {
        let mut dnd = DragDropManager::new();
        assert!(!dnd.is_active());
        dnd.start(
            DragPayload::Tab { group: 7, tab: 3 },
            Point::new(10.0, 10.0),
        );
        assert!(dnd.is_active());
        dnd.update_position(Point::new(30.0, 40.0));
        assert!(dnd.is_active());
        let ended = dnd.end().expect("ended drag returns state");
        match ended.payload {
            DragPayload::Tab { group, tab } => {
                assert_eq!(group, 7);
                assert_eq!(tab, 3);
            }
            _ => panic!("wrong payload"),
        }
        assert!(!dnd.is_active());
    }

    // Case `drag_distance_exceeds_threshold`.
    {
        let mut dnd = DragDropManager::new();
        dnd.start(
            DragPayload::Text { value: "x".into() },
            Point::new(0.0, 0.0),
        );
        dnd.update_position(Point::new(10.0, 0.0));
        assert!(dnd.distance_moved() >= 10.0);
    }
}
