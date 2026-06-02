//! Local-mode TextInput: a `TextInput` with no CRDT `on_change_path`
//! must still accept user edits — typed characters land in
//! `self.text`, the dirty flag flips, and `take_dirty` returns the
//! new value exactly once.

use std::collections::HashMap;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::Widget;
use ui::widget::TextInput;
use winit::event::{Ime, Modifiers, WindowEvent};

#[test]
fn local_textinput_accepts_typed_characters_and_reports_via_take_dirty() {
    let mut input = TextInput::local().with_initial_value("hi");
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = (&mut input as &mut dyn Widget).build_layout(&mut taffy, &ui::theme::Theme::dark(), &mut node_map);
    taffy.compute_layout(
        root_node,
        Size {
            width: AvailableSpace::Definite(200.0),
            height: AvailableSpace::Definite(40.0),
        },
    ).unwrap();
    (&mut input as &mut dyn Widget).apply_layout(&taffy, root_node, vello::kurbo::Point::ZERO);

    let mut doc = AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    let modifiers = Modifiers::default();
    // A real click would set `focus_target`, but it also re-computes
    // `cursor_pos` from `char_x_positions` (only populated during a
    // real draw pass). To keep this test display-free, simulate the
    // click's only relevant effect: focus the widget. The cursor
    // stays at the tail (from `with_initial_value`), so the "!"
    // appended below produces "hi!".
    (&mut input as &mut dyn Widget).set_focus(true);

    let typing = WindowEvent::Ime(Ime::Commit("!".to_string()));
    (&mut input as &mut dyn Widget).handle_event(
        &typing, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    let dirty = input.take_dirty();
    assert_eq!(dirty.as_deref(), Some("hi!"));
    assert!(input.take_dirty().is_none());
    assert_eq!(input.value(), "hi!");
}
