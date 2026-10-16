//! Native constructor tests: no window, native input, GPU, or broker qualification.
use super::*;
use crate::widget::{Column, EditableLabel, TextInput};
use automerge::transaction::Transactable;

fn target(app: &App, id: &str) -> NodeId {
    app.layout
        .node_map
        .keys()
        .copied()
        .find(|node| {
            app.inspect_node(*node, |widget| widget.test_id() == Some(id))
                .unwrap_or(false)
        })
        .unwrap()
}

#[test]
fn native_focus_and_selection_follow_only_the_semantic_target() {
    // Case `selection_adopts_focus_only_when_the_semantic_target_acquires_it`.
    {
        let root = Column::new()
            .push(Box::new(TextInput::local().with_id("body")))
            .push(Box::new(EditableLabel::new("Title").with_id("title")));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        let body = target(&app, "body");
        let title = target(&app, "title");
        app.set_focused_node(Some(body));
        let selection = crate::a11y::Action::SetSelection {
            anchor: 0,
            focus: 0,
        };
        app.dispatch_a11y_action(accesskit::NodeId(u64::from(title) + 1), &selection);
        assert_eq!(app.focused_node, Some(title));
        assert!(
            app.inspect_node(title, |widget| widget.is_focused())
                .unwrap()
        );
        // Plain TextInput moves its selection without implicitly acquiring focus.
        app.dispatch_a11y_action(accesskit::NodeId(u64::from(body) + 1), &selection);
        assert_eq!(app.focused_node, Some(title));
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 900, 650, 1.0, true, None).unwrap();
        let title = target(&app, "title");
        assert_eq!(app.focused_node, Some(title));
        assert!(
            app.inspect_node(title, |widget| widget.is_focused())
                .unwrap()
        );
        assert_eq!(app.state.doc.pending_ops(), 0);
        assert!(app.state.doc.base_heads().is_empty());
    }

    // Case `anonymous_native_focus_is_rediscovered_without_stale_node_or_state_transfer`.
    {
        use crate::widget::layout::AnonymousFocusProbe;
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(AnonymousFocusProbe::new("own selection")),
        );
        app.init_layout();
        let old = app.layout.root_node.unwrap();
        app.set_focused_node(Some(old));
        app.layout.structure_dirty = true;
        app.layout.layout_dirty = true;
        run_layout_pass(&mut app, old, 900, 650, 1.0, true, None).unwrap();
        let current = app.layout.root_node.unwrap();
        assert_ne!(old, current);
        assert_eq!(app.focused_node, Some(current));
        assert_eq!(
            app.inspect_node(current, |widget| crate::undo::get_state(widget))
                .unwrap()
                .as_deref(),
            Some("own selection")
        );
    }

    // Case `anonymous_native_rediscovery_refuses_disabled_or_unrelated_focused_widgets`.
    {
        use crate::widget::layout::AnonymousFocusProbe;
        for disabled in [false, true] {
            let mut root = Column::new();
            let mut probe = AnonymousFocusProbe::new("one");
            probe.enabled = !disabled;
            root = root.push(Box::new(probe));
            if !disabled {
                root = root.push(Box::new(AnonymousFocusProbe::new("two")));
            }
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
            app.init_layout();
            app.focused_node = None;
            app.layout.structure_dirty = true;
            app.layout.layout_dirty = true;
            let old = app.layout.root_node.unwrap();
            run_layout_pass(&mut app, old, 900, 650, 1.0, true, None).unwrap();
            assert!(app.focused_node.is_none());
        }
    }
}
