use super::*;
use crate::widget::{
    Actions, Button, Field, Form, FormSection, LabelPlacement, LayoutInfo, TextInput,
};

fn form_layout(app: &mut App, width: u32) {
    app.layout.layout_dirty = true;
    run_layout_pass(
        app,
        app.layout.root_node.unwrap(),
        width,
        1000,
        1.0,
        false,
        None,
    )
    .unwrap();
}
#[test]
fn form_host_preserves_entry_selection_focus_and_click_geometry_across_modes() {
    let mut input = TextInput::local().with_id("name");
    input.set_text("Alex Morgan");
    let form = Form::new()
        .section(
            FormSection::new("Profile")
                .field(Field::new("Name", input))
                .field(Field::new(
                    "Email address",
                    TextInput::local().with_id("email"),
                )),
        )
        .with_actions(Actions::new(
            Button::new("save", "Save changes").on_click(crate::testing::record_click("saved")),
        ));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(form));
    app.init_layout();
    form_layout(&mut app, 900);
    let first = app.focusable_order[0];
    app.set_focused_node(Some(first));
    app.mutate_node(first, |widget| crate::undo::set_state(widget, "8:b:2"))
        .unwrap();
    for (width, placement) in [(340, None), (900, Some(LabelPlacement::Above)), (900, None)] {
        let root = app
            .root_widget
            .as_any_mut()
            .unwrap()
            .downcast_mut::<Form>()
            .unwrap();
        *root = std::mem::take(root).with_label_placement(placement);
        // A normal layout rebuild observes the newly authored region value.
        app.layout.structure_dirty = true;
        form_layout(&mut app, width);
        let focused = app.focused_node.unwrap();
        assert_eq!(
            app.inspect_node(focused, |widget| (
                widget.test_id().map(str::to_owned),
                crate::undo::get_state(widget)
            ))
            .unwrap(),
            (Some("name".into()), Some("8:b:2".into()))
        );
        assert_eq!(
            app.inspect_node(focused, |widget| widget.text_review().unwrap().1)
                .unwrap(),
            "Alex Morgan"
        );
        let Some(LayoutInfo::Form(info)) = app.root_widget.layout_info() else {
            panic!("form info")
        };
        assert_eq!(
            info.label_placement.value,
            placement.unwrap_or(LabelPlacement::Automatic)
        );
    }
    let save = app.focusable_order[2];
    let bounds = app.inspect_node(save, |widget| widget.get_rect()).unwrap();
    assert!(app.dispatch_semantic_click(bounds.center().x, bounds.center().y));
    assert_eq!(crate::testing::clicks("saved"), 1);
}
