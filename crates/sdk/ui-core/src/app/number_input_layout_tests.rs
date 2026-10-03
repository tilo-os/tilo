use super::*;
use crate::widget::NumberInput;

#[test]
fn number_input_value_change_resizes_retained_control_on_same_viewport_app_frame() {
    for scale in [1.0, 2.0] {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(
                NumberInput::new(15.0)
                    .with_id("interval")
                    .with_suffix("min"),
            ),
        );
        app.theme = Theme::dark().with_text_scale(scale);
        app.text_renderer.set_text_scale(scale);
        app.init_layout();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 900, 800, 1.0, false, None).unwrap();
        let before = app.root_widget.get_rect();
        let focused = app.focusable_order[0];
        app.set_focused_node(Some(focused));
        run_layout_pass(&mut app, root, 900, 800, 1.0, false, None).unwrap();
        draw_frame_content(&mut app, &mut Scene::new(), 900, 800, 1.0, None);
        app.root_widget.mark_drawn();
        assert!(!app.layout.layout_dirty);
        let target = app
            .build_a11y_tree_update(1.0)
            .nodes
            .into_iter()
            .find(|(_, node)| node.role() == accesskit::Role::SpinButton)
            .unwrap()
            .0;
        app.dispatch_a11y_action(
            target,
            &crate::a11y::Action::SetValue("123456789012345".into()),
        );
        assert!(
            app.layout.layout_dirty,
            "real action path must schedule the layout frame"
        );
        run_layout_pass(&mut app, root, 900, 800, 1.0, false, None).unwrap();
        let after = app.root_widget.get_rect();
        assert!(
            after.width() > before.width(),
            "longer committed value needs a wider automatic field: {before:?} -> {after:?}"
        );
        let value = app.root_widget.accessibility().unwrap().value.unwrap();
        assert_eq!(value, "123456789012345 min");
        let (text_width, _) = app.text_renderer.measure(
            &value,
            crate::tokens::typography::stock().size(crate::tokens::TextRole::Body),
            None,
        );
        assert!(
            after.width()
                >= f64::from(text_width)
                    + 2.0 * f64::from(app.theme.padding_standard)
                    + 2.0 * after.height()
                    + 8.0
        );
        assert_eq!(app.layout.root_node, Some(root));
        assert_eq!(app.focused_node, Some(focused));
        draw_frame_content(&mut app, &mut Scene::new(), 900, 800, 1.0, None);
    }
}
