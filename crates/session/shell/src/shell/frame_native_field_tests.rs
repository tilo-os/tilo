//! The identity under which the focused native field is served to the input
//! method, and so what dictation consent is bound to.

use super::tests::axis_a11y_tree;
use super::*;

#[test]
fn native_field_identity_survives_a_layout_switch_and_back() {
    // The compositor serves a native field to dictation under the focused
    // node's identity; a changed identity retires the original consent.
    // A layout change around the same field must keep it.
    let (mut tree, strip, groups, tabs) = axis_a11y_tree();
    let workspace = tree.desktop.active_workspace_mut().unwrap();
    workspace.strips[0].window_groups.truncate(2);
    for group in &mut workspace.strips[0].window_groups {
        group.tabs.truncate(1);
        group.tabs[0].tile = crate::shell::state::TileContent::AppPlaceholder {
            kind: crate::shell::state::AppKind::Terminal,
            label: "Terminal".into(),
        };
    }
    let mut frame = FrameBuilder::with_tree(tree, (3200, 900));
    frame.relayout();
    assert!(
        frame
            .host_mut()
            .focus_window_on_strip(strip, groups[1], tabs[1][0])
    );
    let mut focus = None;
    frame.relayout();
    frame.reconcile_native_focus(&mut focus);
    let field = |frame: &mut FrameBuilder, focus: Option<NodeId>| {
        let id = focus.expect("a focused native field");
        assert_eq!(Some(id), frame.active_terminal_node());
        frame.native_field_identity(focus).unwrap()
    };
    let original = field(&mut frame, focus);
    for fullscreen in [Some(groups[1]), None] {
        frame.set_fullscreen(fullscreen);
        frame.relayout();
        frame.reconcile_native_focus(&mut focus);
        assert_eq!(
            field(&mut frame, focus),
            original,
            "a layout switch must keep the focused field's identity"
        );
    }
    // Another window's field is a different field, and returning starts
    // a fresh identity: consent never follows focus away and back.
    let mut seen = vec![original];
    for index in [0, 1] {
        assert!(
            frame
                .host_mut()
                .focus_window_on_strip(strip, groups[index], tabs[index][0])
        );
        frame.relayout();
        // Keyboard routing to the active window's Terminal has its own tests.
        focus = frame.active_terminal_node();
        let identity = field(&mut frame, focus);
        assert!(
            !seen.contains(&identity),
            "a field switch reused an identity"
        );
        seen.push(identity);
    }
}
