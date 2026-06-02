//! Editing the embedded hex field changes the picker's colour.
//! Typing a valid `#rrggbb` updates `picker.color()`; invalid
//! input is ignored without throwing the picker into a bad state.

use std::collections::HashMap;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::Widget;
use ui::ColorCompat;
use ui::widget::ColorPicker;
use vello::peniko::Color;
use winit::event::{Ime, Modifiers, WindowEvent};

fn layout_picker(picker: &mut ColorPicker) -> (TaffyTree<()>, NodeId) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = (picker as &mut dyn Widget).build_layout(&mut taffy, &ui::theme::Theme::dark(), &mut node_map);
    taffy.compute_layout(
        root_node,
        Size {
            width: AvailableSpace::Definite(500.0),
            height: AvailableSpace::Definite(400.0),
        },
    ).unwrap();
    (picker as &mut dyn Widget).apply_layout(&taffy, root_node, vello::kurbo::Point::ZERO);
    (taffy, root_node)
}

#[test]
fn typing_hex_into_picker_updates_picker_color() {
    let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
    let (taffy, root_node) = layout_picker(&mut picker);

    let mut doc = AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    let modifiers = Modifiers::default();

    picker.set_hex_text_for_test("#10C0F0");
    let typing = WindowEvent::Ime(Ime::Commit(String::new()));
    (&mut picker as &mut dyn Widget).handle_event(
        &typing, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    let c = picker.color();
    assert_eq!((c.r(), c.g(), c.b()), (0x10, 0xC0, 0xF0));
}

#[test]
fn typing_invalid_hex_leaves_picker_color_unchanged() {
    let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
    let (taffy, root_node) = layout_picker(&mut picker);

    let mut doc = AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    let modifiers = Modifiers::default();

    picker.set_hex_text_for_test("not a colour");
    let typing = WindowEvent::Ime(Ime::Commit(String::new()));
    (&mut picker as &mut dyn Widget).handle_event(
        &typing, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    let c = picker.color();
    assert_eq!((c.r(), c.g(), c.b()), (99, 102, 241));
}
