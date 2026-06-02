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
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, Modifiers, MouseButton, WindowEvent};

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

/// Synthesise a `DeviceId`. Winit doesn't expose a public
/// constructor; zero-init is the project convention (see
/// `color_picker.rs` unit tests).
fn synth_device_id() -> winit::event::DeviceId {
    unsafe { std::mem::zeroed() }
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
    // The event itself is a stand-in — the parse pass is actually
    // driven by the test sentinel set inside `set_hex_text_for_test`.
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
    // The event itself is a stand-in — the parse pass is actually
    // driven by the test sentinel set inside `set_hex_text_for_test`.
    let typing = WindowEvent::Ime(Ime::Commit(String::new()));
    (&mut picker as &mut dyn Widget).handle_event(
        &typing, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    let c = picker.color();
    assert_eq!((c.r(), c.g(), c.b()), (99, 102, 241));
}

/// Regression test for the focus-routing bug: clicking the
/// embedded hex input must actually focus it, not just route
/// focus to the picker's own node id. We drive a real
/// cursor-move + mouse-press flow (no test sentinel) and then
/// assert that the input is focused. If the picker forwards the
/// TextInput's focus proposal to the App by node id, the input
/// itself never receives `set_focus(true)` and this assertion
/// fails.
#[test]
fn clicking_hex_input_focuses_it() {
    let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
    let (taffy, root_node) = layout_picker(&mut picker);

    let mut doc = AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    let modifiers = Modifiers::default();

    let hex_rect = picker.hex_input_rect();
    let center_x = (hex_rect.x0 + hex_rect.x1) / 2.0;
    let center_y = (hex_rect.y0 + hex_rect.y1) / 2.0;

    // Move cursor into the centre of the embedded input.
    let move_ev = WindowEvent::CursorMoved {
        device_id: synth_device_id(),
        position: PhysicalPosition::new(center_x, center_y),
    };
    (&mut picker as &mut dyn Widget).handle_event(
        &move_ev, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    // Press inside the input — this must focus the embedded
    // TextInput, not just propose focus on the picker's node id.
    let press_ev = WindowEvent::MouseInput {
        device_id: synth_device_id(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    (&mut picker as &mut dyn Widget).handle_event(
        &press_ev, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    assert!(
        picker.is_hex_input_focused_for_test(),
        "Click inside hex_input_rect must focus the embedded TextInput. \
         If this fails, the focus-routing fix in color_picker.rs was lost.",
    );
}

/// Press inside the SV square after focusing the input must
/// release the input's focus — otherwise keyboard events keep
/// flowing into the (now invisible) input even after the user
/// clicks away.
#[test]
fn clicking_outside_hex_input_releases_input_focus() {
    let mut picker = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
    let (taffy, root_node) = layout_picker(&mut picker);

    let mut doc = AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    let modifiers = Modifiers::default();

    // Step 1 — focus the input via a click inside it.
    let hex_rect = picker.hex_input_rect();
    let in_x = (hex_rect.x0 + hex_rect.x1) / 2.0;
    let in_y = (hex_rect.y0 + hex_rect.y1) / 2.0;
    let move_in = WindowEvent::CursorMoved {
        device_id: synth_device_id(),
        position: PhysicalPosition::new(in_x, in_y),
    };
    let press = WindowEvent::MouseInput {
        device_id: synth_device_id(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };
    (&mut picker as &mut dyn Widget).handle_event(
        &move_in, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );
    (&mut picker as &mut dyn Widget).handle_event(
        &press, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );
    assert!(picker.is_hex_input_focused_for_test(), "precondition: input must be focused after click-in");

    // Step 2 — release, then move outside the input rect and press.
    let release = WindowEvent::MouseInput {
        device_id: synth_device_id(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    (&mut picker as &mut dyn Widget).handle_event(
        &release, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    // The SV square is well outside the hex_input_rect.
    let sv = picker.hex_input_rect();
    // Pick a point far above the input — the SV square is at the
    // top of the picker, the input is in the lower-right.
    let out_x = sv.x0 - 10.0;
    let out_y = 8.0;
    let move_out = WindowEvent::CursorMoved {
        device_id: synth_device_id(),
        position: PhysicalPosition::new(out_x, out_y),
    };
    (&mut picker as &mut dyn Widget).handle_event(
        &move_out, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );
    (&mut picker as &mut dyn Widget).handle_event(
        &press, 1.0, &modifiers, &mut doc, &automerge::ObjId::Root,
        &mut handled, &mut focus_target, root_node, &taffy,
    );

    assert!(
        !picker.is_hex_input_focused_for_test(),
        "Click outside hex_input_rect must release the input's focus.",
    );
}
