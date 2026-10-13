//! Local-mode `TextInput`: a `TextInput` with no CRDT `on_change_path`
//! must still accept user edits — typed characters land in
//! `self.text`, the dirty flag flips, and `take_dirty` returns the
//! new value exactly once.

use std::collections::HashMap;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::Widget;
use tilo_ui::widget::TextInput;
use winit::event::{Ime, Modifiers, WindowEvent};

#[test]
fn local_textinput_reports_typing_but_not_programmatic_text() {
    // Case `local_textinput_accepts_typed_characters_and_reports_via_take_dirty`.
    {
        let mut input = TextInput::local().with_initial_value("hi");
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root_node = (&mut input as &mut dyn Widget).build_layout(
            &mut taffy,
            &tilo_ui::theme::Theme::dark(),
            &mut node_map,
        );
        taffy
            .compute_layout(
                root_node,
                Size {
                    width: AvailableSpace::Definite(200.0),
                    height: AvailableSpace::Definite(40.0),
                },
            )
            .unwrap();
        (&mut input as &mut dyn Widget).apply_layout(&taffy, root_node, vello::kurbo::Point::ZERO);

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let modifiers = Modifiers::default();
        // A real click would also re-compute `cursor_pos` from
        // `char_x_positions`, populated only during a real draw pass. To stay
        // display-free, simulate its only relevant effect: focus the widget.
        // The cursor stays at the tail, so "!" appended below gives "hi!".
        (&mut input as &mut dyn Widget).set_focus(true);

        let typing = WindowEvent::Ime(Ime::Commit("!".to_string()));
        (&mut input as &mut dyn Widget).handle_event(
            &typing,
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: tilo_ui::widget::Edits::lend(&mut doc),
                path: &automerge::ObjId::Root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy: &taffy,
            },
        );

        let dirty = input.take_dirty();
        assert_eq!(dirty.as_deref(), Some("hi!"));
        assert!(input.take_dirty().is_none());
        assert_eq!(input.value(), "hi!");
    }

    // Case `local_textinput_set_text_overrides_without_marking_dirty`.
    {
        let mut input = TextInput::local().with_initial_value("#ff0000");
        assert_eq!(input.value(), "#ff0000");
        assert!(
            input.take_dirty().is_none(),
            "seeded value should not be dirty"
        );
        input.set_text("#00ff00");
        assert_eq!(input.value(), "#00ff00");
        assert!(
            input.take_dirty().is_none(),
            "set_text is a programmatic write, not user edit"
        );
    }
}
