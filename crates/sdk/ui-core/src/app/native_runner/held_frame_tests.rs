//! A frame waits for the local reads it would show, inside its budget.

use super::frame_held_until;

#[test]
fn a_frame_waits_for_a_local_read_only_inside_its_budget() {
    let now = crate::time::now();
    let budget = now + crate::time::Duration::from_millis(16);
    assert_eq!(frame_held_until(Some(budget), now), Some(budget));
    assert_eq!(
        frame_held_until(Some(budget), budget),
        None,
        "the budget ran out"
    );
    assert_eq!(frame_held_until(None, now), None, "nothing to wait for");
}
