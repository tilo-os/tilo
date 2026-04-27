use quick_ui::overlay::{OverlayStack, Modal, Popover, FocusTrap};
use vello::kurbo::Point;

#[test]
fn overlay_stack_push_pop_order() {
    let mut s = OverlayStack::new();
    let a = s.push_modal(Modal::new("a"));
    let b = s.push_popover(Popover::new("b", Point::new(100.0, 100.0)));
    assert_eq!(s.len(), 2);
    assert_eq!(s.top_id(), Some(b));
    s.dismiss(a);
    assert_eq!(s.len(), 1);
    assert_eq!(s.top_id(), Some(b));
}

#[test]
fn focus_trap_cycles_only_within_members() {
    let trap = FocusTrap::new(vec![1, 2, 3]);
    assert_eq!(trap.next_from(1), 2);
    assert_eq!(trap.next_from(3), 1);
    assert_eq!(trap.prev_from(1), 3);
}
