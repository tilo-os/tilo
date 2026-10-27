//! The text field's tests: the fixtures they share, and one file per subject
//! beside them — the caret and the shaped text it sits in, the menus and
//! spelling, edits and secrets, the pointer and the keys, the multiline field,
//! attributed lines, a field among other controls, and the frame it paints.

use super::*;
use crate::testing::Dispatch;
use winit::event::TouchPhase;

/// The 1× size a role-less field sets its text at.
pub(super) fn body_size() -> f32 {
    crate::tokens::typography::stock().size(TextRole::Body)
}

/// Sends `CursorMoved` first so `last_mouse_pos`/`is_hovered` are set, matching real input order.
#[cfg(test)]
pub(super) fn right_click(ti: &mut TextInput, doc: &mut AutoCommit, x: f64, y: f64) -> bool {
    let mut sender = Dispatch::new().doc(doc);
    let moved = crate::synthetic_input::cursor_moved(x, y);
    let press = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
    sender.send(ti, &moved).handled | sender.send(ti, &press).handled
}

/// A focused en-US field holding `text` with the caret at its end.
pub(super) fn suggesting_field(text: &str) -> TextInput {
    let mut ti = TextInput::local()
        .with_spellcheck(true)
        .with_autocorrect(true)
        .with_spell_language("en_US")
        .with_initial_value(text);
    ti.set_focus(true);
    ti.core.set_caret(ti.text.len());
    ti
}
// Driven through `handle_event`, never by poking private fields, so a
// regression in event routing is caught too.

pub(super) fn mods(ctrl: bool, shift: bool) -> Modifiers {
    use winit::keyboard::ModifiersState;
    let mut s = ModifiersState::empty();
    if ctrl {
        s |= ModifiersState::CONTROL;
    }
    if shift {
        s |= ModifiersState::SHIFT;
    }
    Modifiers::from(s)
}

pub(super) fn command_mods() -> Modifiers {
    use winit::keyboard::ModifiersState;
    Modifiers::from(ModifiersState::SUPER)
}

pub(super) fn middle_press() -> WindowEvent {
    crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Middle)
}

pub(super) fn test_node() -> (TaffyTree<()>, NodeId) {
    let mut taffy = TaffyTree::new();
    let node = taffy.leaf_node(Style::default());
    (taffy, node)
}

pub(super) fn touch(phase: TouchPhase, id: u64, point: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(point.x, point.y),
        force: None,
        id,
    })
}

pub(super) fn drive(
    ti: &mut TextInput,
    doc: &mut AutoCommit,
    ev: &WindowEvent,
    m: Modifiers,
) -> bool {
    Dispatch::new().modifiers(m).doc(doc).send(ti, ev).handled
}

/// Unfocused, so `scroll_x` stays 0 and click math is simple.
pub(super) fn draw_to_populate(ti: &mut TextInput) {
    draw_with_theme(ti, &Theme::dark());
}

pub(super) fn draw_with_theme(ti: &mut TextInput, theme: &Theme) {
    let presence = crate::presence::PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    Widget::draw(ti, &mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
}

/// Mirrors the handler's own `padding_x = 10.0`; assumes `scroll_x == 0`.
pub(super) fn x_for_byte(ti: &TextInput, b: usize) -> f64 {
    ti.rect.x0 + 10.0 + ti.char_x_positions()[b]
}

pub(super) fn ml_input(text: &str, w: f64, h: f64) -> TextInput {
    let mut ti = TextInput::local().multiline(true).with_initial_value(text);
    ti.set_focus(true);
    ti.apply_layout_external(Rect::new(0.0, 0.0, w, h));
    draw_to_populate(&mut ti);
    ti
}

pub(super) fn shaped_row(ti: &TextInput, byte: usize) -> usize {
    ti.single_layout
        .as_ref()
        .expect("multiline input retained its shaped paragraph")
        .visual_line_index(ti.text_to_layout_byte(byte))
}

pub(super) fn draw_rich_line_for_test(
    line: &mut crate::widget::RichTextLine,
    theme: &Theme,
) -> Scene {
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    line.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        theme,
        &crate::presence::PresenceCtx::empty(),
    );
    scene
}

/// One host frame of a multiline field: Taffy's sizing queries at the

/// widths a Scrollable's grid asks for (zero, min/max-content, final), then

/// placement and paint against a window-sized viewport.
pub(super) fn typing_frame(input: &mut TextInput, tr: &mut TextRenderer, theme: &Theme) {
    let mut height = 0.0f32;
    for (known, available) in [
        (Some(0.0), taffy::AvailableSpace::Definite(0.0)),
        (None, taffy::AvailableSpace::MinContent),
        (None, taffy::AvailableSpace::MaxContent),
        (Some(600.0), taffy::AvailableSpace::Definite(600.0)),
    ] {
        height = Widget::measure(
            input,
            taffy::Size {
                width: known,
                height: None,
            },
            taffy::Size {
                width: available,
                height: taffy::AvailableSpace::MaxContent,
            },
            tr,
        )
        .height;
    }
    input.apply_layout_external(Rect::new(0.0, 0.0, 600.0, f64::from(height)));
    let presence = crate::presence::PresenceCtx::empty();
    presence.push_viewport(Rect::new(0.0, 0.0, 600.0, 400.0));
    let mut scene = Scene::new();
    Widget::draw(input, &mut scene, Affine::IDENTITY, tr, theme, &presence);
}

pub(super) fn typing_field(text: &str, tr: &mut TextRenderer, theme: &Theme) -> TextInput {
    let mut input = TextInput::local()
        .multiline(true)
        .with_spell_language("en_US")
        .with_initial_value(text);
    input.set_focus(true);
    let mut taffy = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    Widget::build_layout(&mut input, &mut taffy, theme, &mut nodes);
    typing_frame(&mut input, tr, theme);
    input
}

#[path = "text_input_attributed_tests.rs"]
mod attributed;
#[path = "text_input_caret_tests.rs"]
mod caret;
#[path = "text_input_completion_tests.rs"]
mod completion;
#[path = "text_input_edit_tests.rs"]
mod edit;
#[path = "text_input_form_tests.rs"]
mod form;
#[path = "text_input_frame_tests.rs"]
mod frame;
#[path = "text_input_menu_tests.rs"]
mod menu;
#[path = "text_input_multiline_tests.rs"]
mod multiline;
#[path = "text_input_pointer_tests.rs"]
mod pointer;
