//! A form's labels name their controls once, visibly and to a reader.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

fn labeled_controls_share_one_key_for_visible_and_spoken_labels() {
    fn labeled_field_hugs_its_content_inside_a_tall_form() {
        let field = labeled_field(
            "profile-name-label",
            TextInput::local().with_id("profile-name"),
        );
        let app = Driver::new(
            tilo_ui::State::new(),
            Box::new(tilo_ui::column![field]),
            (400, 600),
        );
        let targets = app.targets();
        let field = targets
            .get("Column[0]/Column[0]")
            .expect("field wrapper target");
        let input = targets.get("profile-name").expect("field input target");

        assert!(field.height() < 120.0, "field filled the form: {field:?}");
        assert!(
            field.height() >= input.height(),
            "field clipped input: {field:?}"
        );
    }

    fn standard_boolean_and_range_controls_share_the_contract() {
        let checkbox = labeled_field(
            "remember-label",
            Checkbox::bound("remember", "remember-value"),
        );
        let toggle = labeled_field("sync-label", Toggle::bound("sync", "sync-value"));
        let slider = labeled_field("volume-label", Slider::new("volume", "volume-value"));

        for (field, expected) in [
            (checkbox, Role::Checkbox),
            (toggle, Role::Switch),
            (slider, Role::Slider),
        ] {
            let children = Widget::children(&field);
            let control = children
                .get(1)
                .and_then(|child| child.accessibility())
                .expect("control is accessible");
            assert_eq!(control.role, expected);
            assert!(control.name.is_some());
        }
    }

    fn a_colour_pickers_group_says_its_value_in_a_box() {
        use vello::peniko::Color;
        let field = labeled_field(
            "advanced-label",
            ColorPicker::new(Color::from_rgb8(20, 40, 60)),
        );
        let group = Widget::children(&field)
            .get(1)
            .and_then(|child| child.accessibility())
            .expect("the picker is accessible");
        assert_eq!(group.role, Role::Group);
        assert!(
            group
                .children
                .iter()
                .any(|child| child.role == Role::ComboBox),
            "the group holds the box that says its value"
        );
    }

    fn composite_inputs_share_the_contract() {
        use jiff::civil::Date;
        use vello::peniko::Color;

        let controls = vec![
            (
                labeled_field("advanced-label", NumberInput::new(42.0)),
                Role::SpinButton,
            ),
            (
                labeled_field(
                    "advanced-label",
                    Dropdown::new(vec!["One".into(), "Two".into()], "choice"),
                ),
                Role::ComboBox,
            ),
            (
                labeled_field(
                    "advanced-label",
                    DatePicker::new(Date::new(2026, 9, 1).unwrap()),
                ),
                Role::ComboBox,
            ),
            (
                labeled_field("advanced-label", TimePicker::new(10, 30)),
                Role::ComboBox,
            ),
            (
                // The picker stands as a group holding its own controls, its value among
                // them; the box that says the value is a ComboBox inside it.
                labeled_field(
                    "advanced-label",
                    ColorPicker::new(Color::from_rgb8(20, 40, 60)),
                ),
                Role::Group,
            ),
            (
                labeled_field(
                    "advanced-label",
                    ToggleGroup::new(
                        vec![
                            tilo_ui::widget::Segment::new("Day"),
                            tilo_ui::widget::Segment::new("Week"),
                        ],
                        0,
                    ),
                ),
                Role::Group,
            ),
            (
                labeled_field("advanced-label", Autocomplete::new()),
                Role::TextInput,
            ),
        ];

        for (field, expected_role) in controls {
            let control = Widget::children(&field)
                .get(1)
                .and_then(|child| child.accessibility())
                .expect("composite form control is accessible");
            assert_eq!(control.role, expected_role);
            assert_eq!(control.name.as_deref(), Some("advanced-label"));
        }
    }

    /// A label placed over a boxed control names it, and is read once: in
    /// Settings' pages, whose controls are composed as plain elements.
    fn a_label_over_a_boxed_control_is_the_controls_name() {
        let field = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new("Display name").naming_its_control_for_tests(),
            ))
            .push(Box::new(Labelled::new(
                "Display name",
                Box::new(TextInput::local().with_id("display-name")),
            )));
        let app = Driver::new(tilo_ui::State::new(), Box::new(field), (400, 200));
        let named = app.a11y_nodes_named("Display name");
        assert_eq!(named.len(), 1, "the label names the field once: {named:?}");
        let node = app.a11y_node(named[0]).expect("the exported node");
        assert_eq!(node.role(), accesskit::Role::TextInput);
        let input = app.targets().get("display-name").copied();
        assert!(input.is_some(), "the field is the one drawn");
    }

    /// A control that already says what it is keeps its own name; the label
    /// only fills in the name one lacks.
    fn a_named_control_keeps_its_name_under_a_label() {
        let input = TextInput::local()
            .with_id("email")
            .with_accessible_name("Email address");
        let node = Widget::accessibility(&Labelled::new("Display name", Box::new(input)))
            .expect("the field's semantics");
        assert_eq!(node.name.as_deref(), Some("Email address"));
    }

    /// A label names a waiting block, which says "Loading" only as a fallback of its own:
    /// the shell holds an app-drawn card's place by the words that name the block, not by
    /// the skeleton's placeholder.
    fn a_label_names_a_waiting_block() {
        let block = Labelled::new(
            "Turn left onto Main Street in Tasks",
            Box::new(tilo_ui::widget::Skeleton::new(
                tilo_ui_base::widget::SkeletonShape::Block { height: 160.0 },
            )),
        );
        let node = Widget::accessibility(&block).expect("the block's semantics");
        assert_eq!(
            node.name.as_deref(),
            Some("Turn left onto Main Street in Tasks")
        );
        assert!(node.state.busy, "it still says the block waits");
    }

    labeled_field_hugs_its_content_inside_a_tall_form();
    standard_boolean_and_range_controls_share_the_contract();
    composite_inputs_share_the_contract();
    a_colour_pickers_group_says_its_value_in_a_box();
    a_label_over_a_boxed_control_is_the_controls_name();
    a_named_control_keeps_its_name_under_a_label();
    a_label_names_a_waiting_block();
}
