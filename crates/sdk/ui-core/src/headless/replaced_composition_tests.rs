//! No stale pointer is followed: once a widget replaced children, the
//! harness admits the new composition before it looks a node up again.

use super::*;
use crate::widget::replaced_tripwire::{ReplaceAt, Replacing};

fn harness(at: ReplaceAt) -> (FrameHarness, std::rc::Rc<std::cell::Cell<bool>>) {
    let (root, armed) = Replacing::new(at);
    let mut frame = FrameHarness::new(State::new(), Box::new(root), (200, 100));
    frame.relayout();
    let child = frame.focus_order()[0];
    frame.set_focused(Some(child));
    (frame, armed)
}

/// An unhandled Alt+Arrow that replaced the focused control on its way
/// through: host navigation looks focus up in the new composition.
#[test]
fn an_event_that_replaces_the_focused_control_is_followed_by_the_new_tree() {
    let (mut frame, armed) = harness(ReplaceAt::Event);
    frame.set_modifiers(&["alt".to_owned()]);
    armed.set(true);
    frame.key("ArrowDown");
    assert!(!armed.get(), "the event replaced the control");
    assert_eq!(frame.focus_order().len(), 1, "the new control is laid out");
}

/// Remembering focus after a replacement reads the live tree.
#[test]
fn remembering_focus_after_a_replacement_reads_the_live_tree() {
    let (mut frame, armed) = harness(ReplaceAt::Sync);
    armed.set(true);
    frame.root.sync(&frame.state.doc, &frame.state.root);
    assert!(!armed.get(), "the sync replaced the control");
    assert!(
        frame.capture_focus_identity().is_none(),
        "the replacement was never focused"
    );
    frame.relayout();
    assert_eq!(frame.focus_order().len(), 1);
}

/// An accessibility action that arrives after a replacement, before any
/// layout: the node it names is looked up in the live composition.
#[test]
fn an_accessibility_action_after_a_replacement_reaches_no_replaced_control() {
    let (mut frame, armed) = harness(ReplaceAt::Sync);
    let target = frame
        .a11y_node_named("Tripwire")
        .expect("the control is exported");
    armed.set(true);
    frame.root.sync(&frame.state.doc, &frame.state.root);
    assert!(!armed.get(), "the sync replaced the control");
    let _ = frame.perform_a11y_action(target, &crate::a11y::Action::Focus);
    assert_eq!(frame.focus_order().len(), 1);
}
