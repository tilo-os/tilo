//! The text field's tests: the fixtures they share, and one file per subject beside them — the
//! caret and the shaped text it sits in, the menus and spelling, edits and secrets, the pointer and
//! the keys, the multiline field, attributed lines, a field among other controls, and the frame it
//! paints.
use super::*;
use crate::testing::Dispatch;

pub(super) fn body_size() -> f32 {
    tilo_ui_base::tokens::typography::stock().size(TextRole::Body)
}

#[cfg(test)]
pub(super) fn right_click(ti: &mut TextInput, doc: &mut AutoCommit, x: f64, y: f64) -> bool {
    let mut sender = Dispatch::new().doc(doc);
    let moved = tilo_ui_base::synthetic_input::cursor_moved(x, y);
    let press =
        tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
    sender.send(ti, &moved).handled | sender.send(ti, &press).handled
}

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
    tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Middle)
}

pub(super) fn test_node() -> (TaffyTree<()>, NodeId) {
    let mut taffy = TaffyTree::new();
    let node = taffy.leaf_node(Style::default());
    (taffy, node)
}

pub(super) fn drive(
    ti: &mut TextInput,
    doc: &mut AutoCommit,
    ev: &WindowEvent,
    m: Modifiers,
) -> bool {
    Dispatch::new().modifiers(m).doc(doc).send(ti, ev).handled
}

pub(super) fn draw_to_populate(ti: &mut TextInput) {
    draw_with_theme(ti, &Theme::dark());
}

pub(super) fn draw_with_theme(ti: &mut TextInput, theme: &Theme) {
    let presence = tilo_ui_base::presence::PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    Widget::draw(
        ti,
        &mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            presence.draw_env(),
        ),
    );
}

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

/// The height a field takes in a layout as the theme gives it: its taffy style's
/// when it pins one, else what it measures.
pub(super) fn layout_height(ti: &mut TextInput, theme: &Theme, width: f32) -> f32 {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let node = ti.build_layout(&mut taffy, theme, &mut node_map);
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let size = Widget::measure(
        ti,
        taffy::Size {
            width: Some(width),
            height: None,
        },
        taffy::Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::MaxContent,
        },
        &mut tr,
    );
    match taffy.style(node).unwrap().size.height.into_option() {
        Some(fixed) => fixed,
        None => size.height,
    }
}

pub(super) fn shaped_row(ti: &TextInput, byte: usize) -> usize {
    ti.single_layout
        .as_ref()
        .expect("multiline input retained its shaped paragraph")
        .visual_line_index(ti.text_to_layout_byte(byte))
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

tilo_test_env::scenario! {
    fn a_field_asks_names_and_shows_what_it_holds;

    phase a_field_asks_for_the_keyboard_once_and_names_a_page_by_its_title;
    // The field a command opens takes the keyboard once, and never asks again: a person who
    // points elsewhere keeps the keyboard where they put it. A large-title field is the page's
    // heading; a body field is not, however it is worded.
    {
        let mut field = TextInput::local()
            .with_placeholder("New task")
            .wants_focus();
        assert_eq!(
            field.take_focus_request().as_deref(),
            Some(field.test_id().unwrap_or_default()),
            "it asks as the window lays it out"
        );
        assert_eq!(
            field.take_focus_request(),
            None,
            "and keeps its place after"
        );
        let mut quiet = TextInput::local().with_placeholder("New task");
        assert_eq!(
            quiet.take_focus_request(),
            None,
            "a field that says nothing asks nothing"
        );

        let field = |role: TextRole| {
            TextInput::local()
                .with_initial_value("Call the landlord about the boiler")
                .with_text_role(role)
        };
        assert_eq!(
            Widget::own_page_title(&field(TextRole::LargeTitle)).as_deref(),
            Some("Call the landlord about the boiler"),
            "the page's title is the words its large-title field shows"
        );
        assert_eq!(
            Widget::own_page_title(&field(TextRole::Body)),
            None,
            "a field of the body is no heading, however it is worded"
        );
        assert_eq!(
            Widget::own_page_title(&TextInput::local().with_text_role(TextRole::LargeTitle)),
            None,
            "a blank title names nothing"
        );
    }

    phase a_field_shows_its_value_from_where_the_person_reads_it;
    // A one-line field is as wide as its text plus [`TextInput::field_padding`] before it scrolls: at
    // that width the whole value shows, from its first letter; anything narrower scrolls to the
    // caret and cuts the start off. A value put there to read (a pairing link in the add-a-device
    // sheet) starts at its beginning however the caret would follow it, and a focused field that
    // widens shows its whole value again.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::dark();
        {
            let mut input = TextInput::local().with_initial_value("Meeting notes");
            input.set_focus(true);
            input.apply_layout_external(Rect::new(0.0, 0.0, 600.0, 40.0));
            draw_with_theme(&mut input, &theme);
            let text_w = *input
                .char_x_positions()
                .last()
                .expect("the value is measured");
            assert!(text_w > 20.0, "fixture: the name is measured: {text_w}");
            let spare = input.field_padding(&theme);
            assert!(
                spare > 4.0,
                "a field keeps its inset on both edges and its carets: {spare}"
            );

            input.apply_layout_external(Rect::new(0.0, 0.0, text_w + spare, 40.0));
            draw_with_theme(&mut input, &theme);
            assert_eq!(
                input.scroll_x(),
                0.0,
                "the field's own area shows the name whole, from its first letter"
            );

            input.apply_layout_external(Rect::new(0.0, 0.0, text_w + 4.0, 40.0));
            draw_with_theme(&mut input, &theme);
            assert!(
                input.scroll_x() > 0.0,
                "narrower than its area, the field scrolls to the caret"
            );
        }

        // The add-a-device sheet shows the pairing link in a read-only field and hands the field the
        // keyboard: the person reads the link from where it starts, not from the meaningless tail its
        // caret would drag into view.
        {
            let link = "https://tilo.coffee/#/@eu1.tilo.coffee/w/\
                        0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c?pair=abababababababababababab\
                        abababababababababababababababab0123456789abcdef0123456789abcdef";
            let drawn = |mut field: TextInput| {
                field.set_focus(true);
                field.apply_layout_external(Rect::new(0.0, 0.0, 320.0, 40.0));
                draw_with_theme(&mut field, &theme);
                field
            };
            let at_start = |field: &TextInput, what: &str| {
                assert!(
                    field.char_x_positions().last().copied().unwrap_or(0.0) > 320.0,
                    "{what}: fixture, the link is longer than the field"
                );
                assert_eq!(field.scroll_x(), 0.0, "{what}: the value shows from its start");
                assert_eq!(field.cursor_pos(), 0, "{what}: the caret is where reading starts");
            };
            // The sheet's own order: the value, then that the field is one to read.
            at_start(
                &drawn(TextInput::local().with_initial_value(link).with_read_only(true)),
                "a link put in before the field was read-only",
            );
            at_start(
                &drawn(TextInput::local().with_read_only(true).with_initial_value(link)),
                "a link put in after",
            );
            // A link that changes while the sheet is up arrives the same way.
            let mut arrived = TextInput::local().with_read_only(true);
            arrived.set_focus(true);
            arrived.set_text(link);
            arrived.apply_layout_external(Rect::new(0.0, 0.0, 320.0, 40.0));
            draw_with_theme(&mut arrived, &theme);
            at_start(&arrived, "a refreshed link");
            // A field the person types in keeps its caret at the end, where typing goes on.
            let typed = drawn(TextInput::local().with_initial_value(link));
            assert!(
                typed.scroll_x() > 0.0,
                "an editable field follows the caret to the end of what was typed"
            );
        }

        // A focused field that widens shows its text from the start again.
        {
            let name = "Travel";
            let mut input = TextInput::local().with_initial_value(name);
            input.set_focus(true);
            input.set_cursor_pos(name.len());
            let draw_at = |input: &mut TextInput, width: f64| {
                input.apply_layout_external(Rect::new(0.0, 0.0, width, 32.0));
                draw_with_theme(input, &theme);
            };
            draw_at(&mut input, 40.0);
            assert!(input.scroll_x() > 0.0, "narrow, it follows the caret");
            draw_at(&mut input, 400.0);
            assert_eq!(input.scroll_x(), 0.0, "wide, the whole name shows");
        }
    }
}

/// A field tells its listener the first change of an empty field, with the text it holds now: what
/// makes the item a list's entry names, from the first keystroke.
#[test]
fn a_fields_first_change_is_reported_so_an_entry_makes_its_item() {
    use tilo_ui_base::{component::Event, component::Fired, component::Intent};
    use winit::event::Ime;
    let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let mut ti = TextInput::local().with_entry(true);
    ti.set_begin_event(Event::View(Intent::new(&queue, "step-entry", "begin")));
    let mut doc = AutoCommit::new();
    Widget::set_focus(&mut ti, true);
    Dispatch::new()
        .doc(&mut doc)
        .send(&mut ti, &WindowEvent::Ime(Ime::Commit("C".into())));
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "the first change is told: {fired:?}");
    assert_eq!(fired[0].key, "step-entry");
    assert_eq!(fired[0].event, "begin");
    assert_eq!(
        fired[0].value,
        Some(tilo_ui_base::component::testing::text("C"))
    );
    // A further character is no first change: the item already exists.
    Dispatch::new()
        .doc(&mut doc)
        .send(&mut ti, &WindowEvent::Ime(Ime::Commit("all".into())));
    assert!(queue.take().is_empty(), "the first change is told once");
}

/// A field tells its listener every change as the person types, carrying what it holds now: what
/// acts on the words while they are typed, as a window's search does.
#[test]
fn a_field_reports_every_change_as_the_words_are_typed() {
    use tilo_ui_base::{component::Event, component::Fired, component::Intent};
    use winit::event::Ime;
    let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let mut ti = TextInput::local();
    ti.set_changed_event(Event::View(Intent::new(&queue, "bar-search", "changed")));
    let mut doc = AutoCommit::new();
    Widget::set_focus(&mut ti, true);
    Dispatch::new()
        .doc(&mut doc)
        .send(&mut ti, &WindowEvent::Ime(Ime::Commit("n".into())));
    Dispatch::new()
        .doc(&mut doc)
        .send(&mut ti, &WindowEvent::Ime(Ime::Commit("o".into())));
    let fired = queue.take();
    assert_eq!(fired.len(), 2, "every change is told: {fired:?}");
    assert_eq!(fired[0].key, "bar-search");
    assert_eq!(fired[0].event, "changed");
    assert_eq!(
        fired[0].value,
        Some(tilo_ui_base::component::testing::text("n"))
    );
    assert_eq!(
        fired[1].value,
        Some(tilo_ui_base::component::testing::text("no"))
    );
}

/// A field tells its listener when it loses the keyboard: an entry left empty is taken away, so no
/// empty item is left behind.
#[test]
fn a_field_left_told_goes_so_an_empty_entry_is_taken_away() {
    use tilo_ui_base::{component::Event, component::Fired, component::Intent};
    let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
    let mut ti = TextInput::local().with_entry(true);
    ti.set_blur_event(Event::View(Intent::new(&queue, "step-entry", "blur")));
    Widget::set_focus(&mut ti, true);
    assert!(queue.take().is_empty(), "taking the keyboard tells nothing");
    Widget::set_focus(&mut ti, false);
    let fired = queue.take();
    assert_eq!(fired.len(), 1, "leaving is told: {fired:?}");
    assert_eq!(fired[0].event, "blur");
}
