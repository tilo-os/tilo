#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::*;
use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release, named_key};
use crate::testing::dispatch;
use automerge::AutoCommit;

fn delivery() -> Vec<RadioOption> {
    vec![
        RadioOption::new("Email").with_detail("A copy to everyone on the team"),
        RadioOption::new("Chat"),
        RadioOption::new("Post")
            .with_enabled(false)
            .with_why("The print room is closed this week"),
        RadioOption::new("Don't send it"),
    ]
}

fn laid_out(mut group: RadioGroup, width: f32) -> RadioGroup {
    let mut taffy = TaffyTree::<()>::new();
    group.build_layout(&mut taffy, &Theme::light(), &mut HashMap::new());
    let size = group.measure(
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
    group.rect = Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height));
    group
}

fn told(group: RadioGroup) -> (RadioGroup, std::rc::Rc<std::cell::RefCell<Vec<usize>>>) {
    let told = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = std::rc::Rc::clone(&told);
    (
        group.on_change(move |index| log.borrow_mut().push(index)),
        told,
    )
}

fn click(group: &mut RadioGroup, at: Point) {
    dispatch(group, &cursor_moved(at.x, at.y));
    dispatch(group, &mouse_press());
    dispatch(group, &mouse_release());
}

tilo_test_env::scenario! {
    fn a_radio_group_makes_one_choice_of_several;

    // Each choice is a row at least a target tall, one under the other; a line under a choice's
    // words makes its row taller, and a narrow column wraps long words onto more lines.
    phase the_choices_stand_one_under_another;
    {
        let group = laid_out(RadioGroup::new(delivery(), Some(1)), 480.0);
        let rows: Vec<Rect> = (0..4).map(|index| group.row_rect(index)).collect();
        for pair in rows.windows(2) {
            assert!(pair[1].y0 >= pair[0].y1, "{pair:?}: one under the other");
        }
        assert!(rows.iter().all(|row| row.height() >= 32.0), "{rows:?}");
        assert!(rows[0].height() > rows[1].height(), "the detail adds a line");
        let narrow = laid_out(RadioGroup::new(delivery(), Some(1)), 120.0);
        assert!(
            narrow.row_rect(0).height() > rows[0].height(),
            "a narrow column wraps the words"
        );
    }

    // A press on a row's words chooses it as one on its radio does; a press that leaves the row
    // before it comes up chooses nothing.
    phase a_press_on_a_row_chooses_it;
    {
        let (group, told) = told(RadioGroup::new(delivery(), Some(1)).with_id("send"));
        let mut group = laid_out(group, 480.0);
        let words = Point::new(200.0, group.row_rect(3).center().y);
        click(&mut group, words);
        assert_eq!(group.selected(), Some(3));
        let first = group.row_rect(0).center();
        dispatch(&mut group, &cursor_moved(first.x, first.y));
        dispatch(&mut group, &mouse_press());
        let away = group.row_rect(1).center();
        dispatch(&mut group, &cursor_moved(away.x, away.y));
        dispatch(&mut group, &mouse_release());
        assert_eq!(group.selected(), Some(3), "a press dragged off chooses nothing");
        let locked = group.row_rect(2).center();
        click(&mut group, locked);
        assert_eq!(group.selected(), Some(3), "a choice that cannot be made is not");
        assert_eq!(*told.borrow(), vec![3]);
    }

    // The group is one stop of the keyboard's order: focus lands on the choice made, the arrows
    // move the choice along, past one that cannot be made and round from the end to the start.
    phase the_arrows_move_the_choice;
    {
        let (group, told) = told(RadioGroup::new(delivery(), Some(1)));
        let mut group = laid_out(group, 480.0);
        assert!(Widget::is_focusable(&group));
        Widget::set_focus(&mut group, true);
        assert_eq!(group.cursor, 1);
        assert!(dispatch(&mut group, &named_key(NamedKey::ArrowDown)));
        assert_eq!(group.selected(), Some(3), "Post cannot be chosen: skipped");
        assert!(dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
        assert_eq!(group.selected(), Some(0), "round from the end");
        assert!(dispatch(&mut group, &named_key(NamedKey::ArrowUp)));
        assert_eq!(group.selected(), Some(3), "and back");
        assert!(!dispatch(&mut group, &named_key(NamedKey::Tab)), "Tab leaves the group");
        assert_eq!(*told.borrow(), vec![3, 0, 3]);
    }

    // Nothing chosen yet, focus lands on the first choice that can be made, and Space makes it.
    phase space_makes_the_first_choice;
    {
        let options = vec![
            RadioOption::new("Locked").with_enabled(false),
            RadioOption::new("Open"),
        ];
        let mut group = laid_out(RadioGroup::new(options, None), 480.0);
        Widget::set_focus(&mut group, true);
        assert_eq!(group.cursor, 1);
        assert!(dispatch(&mut group, &named_key(NamedKey::Space)));
        assert_eq!(group.selected(), Some(1));
    }

    // A group that cannot be used is no stop of the keyboard's order and takes no press.
    phase a_disabled_group_takes_no_input;
    {
        let mut group = laid_out(
            RadioGroup::new(delivery(), Some(1)).with_enabled(false),
            480.0,
        );
        assert!(!Widget::is_focusable(&group));
        let first = group.row_rect(0).center();
        click(&mut group, first);
        assert_eq!(group.selected(), Some(1));
    }

    // Assistive technology hears a named group of radio buttons, each with its name, the line
    // under it or why it cannot be chosen, whether it is chosen, whether it can be, and its
    // place; and chooses by its own action. A pointer over a choice that cannot be made is
    // told why.
    phase each_choice_is_a_named_radio_button;
    {
        let mut group = laid_out(
            RadioGroup::new(delivery(), Some(1))
                .with_id("send")
                .with_accessible_name("Send the report by"),
            480.0,
        );
        let node = Widget::accessibility(&group).unwrap();
        assert_eq!(node.name.as_deref(), Some("Send the report by"));
        assert_eq!(node.value.as_deref(), Some("Chat"));
        assert!(node.children.iter().all(|c| c.role == Role::RadioButton));
        assert_eq!(node.children[1].state.checked, Some(true));
        assert_eq!(node.children[0].state.checked, Some(false));
        assert!(node.children[2].state.disabled);
        assert_eq!(
            node.children[2].description.as_deref(),
            Some("The print room is closed this week"),
            "a choice that cannot be made says why"
        );
        let over_post = group.row_rect(2).center();
        assert_eq!(
            Widget::tooltip_target_at(&group, over_post).map(|(_, why)| why),
            Some("The print room is closed this week".to_owned())
        );
        assert_eq!(
            Widget::tooltip_target_at(&group, group.row_rect(1).center()),
            None,
            "one that can be made needs no reason"
        );
        assert_eq!(
            node.children[0].description.as_deref(),
            Some("A copy to everyone on the team")
        );
        assert!(Widget::perform_semantic_action(
            &mut group,
            "send.0",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(group.selected(), Some(0));
        assert!(!Widget::perform_semantic_action(
            &mut group,
            "send.2",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
    }

    // A view that changes what is chosen changes it where the group stands.
}
