//! Editing the embedded hex field changes the picker's colour.
//! Typing a valid `#rrggbb` updates `picker.color()`; invalid
//! input is ignored without throwing the picker into a bad state.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::ColorCompat;
use tilo_ui::Widget;
use tilo_ui::widget::ColorPicker;
use vello::peniko::Color;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, Modifiers, MouseButton, WindowEvent};

fn layout_picker(picker: &mut ColorPicker) -> (TaffyTree<()>, NodeId) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = (picker as &mut dyn Widget).build_layout(
        &mut taffy,
        &tilo_ui::theme::Theme::dark(),
        &mut node_map,
    );
    taffy
        .compute_layout(
            root_node,
            Size {
                width: AvailableSpace::Definite(500.0),
                height: AvailableSpace::Definite(400.0),
            },
        )
        .unwrap();
    (picker as &mut dyn Widget).apply_layout(&taffy, root_node, vello::kurbo::Point::ZERO);
    (taffy, root_node)
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn embedded_hex_input_takes_focus_and_applies_only_valid_hex() {
    // Case `typing_hex_into_picker_updates_picker_color`.
    {
        let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
        let (taffy, root_node) = layout_picker(&mut picker);

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let modifiers = Modifiers::default();

        picker.set_hex_text_for_test("#10C0F0");
        // The event is a stand-in: the parse pass is driven by the test
        // sentinel set inside `set_hex_text_for_test`.
        let typing = WindowEvent::Ime(Ime::Commit(String::new()));
        (&mut picker as &mut dyn Widget).handle_event(
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

        let c = picker.color();
        assert_eq!((c.r(), c.g(), c.b()), (0x10, 0xC0, 0xF0));
    }

    // Case `typing_invalid_hex_leaves_picker_color_unchanged`.
    {
        let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
        let (taffy, root_node) = layout_picker(&mut picker);

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let modifiers = Modifiers::default();

        picker.set_hex_text_for_test("not a colour");
        let typing = WindowEvent::Ime(Ime::Commit(String::new()));
        (&mut picker as &mut dyn Widget).handle_event(
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

        let c = picker.color();
        assert_eq!((c.r(), c.g(), c.b()), (99, 102, 241));
    }

    // Case `clicking_hex_input_focuses_it`.
    // Clicking the embedded hex input must actually focus it, not just route
    // focus to the picker's own node id. Drives a real cursor-move +
    // mouse-press flow (no test sentinel).
    {
        let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
        let (taffy, root_node) = layout_picker(&mut picker);

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let modifiers = Modifiers::default();

        let hex_rect = picker.hex_input_rect();
        let center_x = f64::midpoint(hex_rect.x0, hex_rect.x1);
        let center_y = f64::midpoint(hex_rect.y0, hex_rect.y1);

        let move_ev = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(center_x, center_y),
        };
        (&mut picker as &mut dyn Widget).handle_event(
            &move_ev,
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

        // The picker proposes focus on its own node id (the App needs that to
        // blur sibling widgets); set_focus(true) then redirects onto the input.
        let press_ev = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        (&mut picker as &mut dyn Widget).handle_event(
            &press_ev,
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

        // Simulate App::set_focused_node manually — tests don't run App.run().
        assert_eq!(
            focus_target,
            Some(root_node),
            "picker must propose its own node as the focus target"
        );
        (&mut picker as &mut dyn Widget).set_focus(true);

        assert!(
            picker.is_hex_input_focused_for_test(),
            "Click inside hex_input_rect must focus the embedded TextInput. \
         If this fails, the focus-routing fix in color_picker.rs was lost.",
        );
    }

    // Case `clicking_outside_hex_input_releases_input_focus`.
    // Press inside the SV square after focusing the input must release the
    // input's focus — otherwise keyboard events keep flowing into it even
    // after the user clicks away.
    {
        let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
        let (taffy, root_node) = layout_picker(&mut picker);

        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let modifiers = Modifiers::default();

        let hex_rect = picker.hex_input_rect();
        let in_x = f64::midpoint(hex_rect.x0, hex_rect.x1);
        let in_y = f64::midpoint(hex_rect.y0, hex_rect.y1);
        let move_in = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(in_x, in_y),
        };
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        (&mut picker as &mut dyn Widget).handle_event(
            &move_in,
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
        (&mut picker as &mut dyn Widget).handle_event(
            &press,
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
        // Drive the App-side focus routing (see other test).
        (&mut picker as &mut dyn Widget).set_focus(true);
        assert!(
            picker.is_hex_input_focused_for_test(),
            "precondition: input must be focused after click-in"
        );

        let release = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        (&mut picker as &mut dyn Widget).handle_event(
            &release,
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

        // Well outside hex_input_rect, near the SV square rather than the
        // lower-right input.
        let sv = picker.hex_input_rect();
        let out_x = sv.x0 - 10.0;
        let out_y = 8.0;
        let move_out = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(out_x, out_y),
        };
        (&mut picker as &mut dyn Widget).handle_event(
            &move_out,
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
        (&mut picker as &mut dyn Widget).handle_event(
            &press,
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

        assert!(
            !picker.is_hex_input_focused_for_test(),
            "Click outside hex_input_rect must release the input's focus.",
        );
    }
}
