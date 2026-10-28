use super::*;
use crate::synthetic_input::{cursor_moved, mouse_release, named_key};
use crate::testing::dispatch;
use automerge::AutoCommit;

fn sizes() -> Vec<PictureOption> {
    [
        ("Small", 0.85),
        ("Default", 1.0),
        ("Large", 1.15),
        ("Larger", 1.3),
    ]
    .into_iter()
    .map(|(label, scale)| PictureOption::new(label, Picture::TextSize(scale)))
    .collect()
}

fn laid_out(mut picker: PictureChoice, width: f32) -> PictureChoice {
    let mut taffy = TaffyTree::<()>::new();
    picker.build_layout(&mut taffy, &Theme::light(), &mut HashMap::new());
    let size = picker.measure(
        Size {
            width: Some(width),
            height: None,
        },
        Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::MaxContent,
        },
        &mut TextRenderer::new(),
    );
    picker.rect = Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
    picker
}

/// Four tiles share a wide line; a narrow one shows two lines of two, never
/// three and an orphan.
#[test]
fn the_tiles_fill_even_lines() {
    let wide = laid_out(PictureChoice::new(sizes(), Some(1)), 720.0);
    assert_eq!(wide.geometry().0, 4);
    assert!(
        (wide.tile_rect(3).x1 - 720.0).abs() < 0.5,
        "the line is filled"
    );
    let narrow = laid_out(PictureChoice::new(sizes(), Some(1)), 300.0);
    assert_eq!(narrow.geometry().0, 2);
    assert!(narrow.tile_rect(2).y0 > narrow.tile_rect(0).y1);
    assert!(
        narrow.get_rect().height() > wide.get_rect().height(),
        "two lines stand taller than one"
    );
}

/// A pointer, the keys and an assistive action each choose a tile; an
/// unbound choice shows it at once and tells its owner.
#[test]
fn a_tile_is_chosen_by_pointer_keys_and_assistive_action() {
    let told = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = std::rc::Rc::clone(&told);
    let mut picker = laid_out(
        PictureChoice::new(sizes(), Some(1))
            .with_id("text-size")
            .on_change(move |index| log.borrow_mut().push(index)),
        720.0,
    );
    let third = picker.tile_rect(2).center();
    dispatch(&mut picker, &cursor_moved(third.x, third.y));
    assert!(dispatch(&mut picker, &mouse_release()));
    assert_eq!(picker.selected(), Some(2));

    Widget::set_focus(&mut picker, true);
    assert!(dispatch(&mut picker, &named_key(NamedKey::End)));
    assert!(dispatch(&mut picker, &named_key(NamedKey::Enter)));
    assert_eq!(picker.selected(), Some(3));

    assert!(Widget::perform_semantic_action(
        &mut picker,
        "text-size.0",
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT,
    ));
    assert_eq!(picker.selected(), Some(0));
    assert_eq!(*told.borrow(), vec![2, 3, 0]);
}

/// Assistive technology hears a named group of radio buttons, each with
/// its name, whether it is chosen and its place.
#[test]
fn each_tile_is_a_named_radio_button() {
    let picker = laid_out(
        PictureChoice::new(sizes(), Some(1)).with_accessible_name("Text size"),
        720.0,
    );
    let node = Widget::accessibility(&picker).unwrap();
    assert_eq!(node.name.as_deref(), Some("Text size"));
    assert_eq!(node.value.as_deref(), Some("Default"));
    let names: Vec<_> = node
        .children
        .iter()
        .filter_map(|c| c.name.clone())
        .collect();
    assert_eq!(names, ["Small", "Default", "Large", "Larger"]);
    assert!(node.children.iter().all(|c| c.role == Role::RadioButton));
    assert_eq!(node.children[1].state.checked, Some(true));
    assert_eq!(node.children[0].state.checked, Some(false));
}

/// A disabled choice takes no input and is not a tab stop.
#[test]
fn a_disabled_choice_takes_no_input() {
    let mut picker = laid_out(
        PictureChoice::new(sizes(), Some(1)).with_enabled(false),
        720.0,
    );
    assert!(!Widget::is_focusable(&picker));
    let first = picker.tile_rect(0).center();
    dispatch(&mut picker, &cursor_moved(first.x, first.y));
    dispatch(&mut picker, &mouse_release());
    assert_eq!(picker.selected(), Some(1));
}

/// A choice kept in view state writes the chosen value, and follows a value
/// set elsewhere.
#[test]
fn a_choice_kept_in_view_state_follows_it_both_ways() {
    let state = crate::LocalState::new("Default".to_owned());
    let choices = sizes()
        .into_iter()
        .map(|option| (option.label.clone(), option))
        .collect();
    let mut picker = laid_out(PictureChoice::from_state(choices, &state), 720.0);
    assert_eq!(picker.selected(), Some(1));
    let last = picker.tile_rect(3).center();
    dispatch(&mut picker, &cursor_moved(last.x, last.y));
    dispatch(&mut picker, &mouse_release());
    assert_eq!(state.get(), "Larger");
    state.set("Small".to_owned());
    Widget::sync(&mut picker, &AutoCommit::new(), &automerge::ROOT);
    assert_eq!(picker.selected(), Some(0));
}
