//! Form composition that keeps visible and accessible labels in one contract.

use super::{
    Checkbox, ColorPicker, Column, DatePicker, Dropdown, NumberInput, Row, Slider, Text, TextInput,
    TimePicker, Toggle, Widget,
};
use crate::tokens::TextRole;

/// A control that can receive the accessible name of a visible form label.
///
/// Implemented by the toolkit's standard text, choice, numeric, date, time,
/// colour, checkbox, toggle, and slider controls. Application-defined
/// composite controls can implement this trait to participate in
/// [`labeled_field`] without changing their widget tree.
pub trait FormControl: Widget + Sized {
    /// Attach the same localization key rendered by the field's visible label.
    #[must_use]
    fn with_form_accessible_name(self, name: &str) -> Self;
}

impl FormControl for TextInput {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for Checkbox {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for Toggle {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for Slider {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for NumberInput {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for Dropdown {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for DatePicker {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for TimePicker {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for ColorPicker {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

/// A boolean form control conventionally paired with an inline label.
pub trait CheckControl: FormControl {}

impl CheckControl for Checkbox {}
impl CheckControl for Toggle {}

/// Compose a visible semantic label with the control it names.
///
/// `label` is normally a Fluent message key. Both the [`Text`] and control
/// resolve that same key through the live localization catalog, preventing the
/// visible copy and accessibility name from drifting. The returned ordinary
/// [`Column`] hugs the label and control rather than claiming the form's full
/// height; it remains chainable and participates in the standard retained
/// lifecycle.
///
/// ```
/// use tilo_ui::widget::{TextInput, labeled_field};
///
/// let email = labeled_field(
///     "account-email-label",
///     TextInput::local()
///         .with_id("account-email")
///         .with_placeholder("account-email-placeholder"),
/// );
/// ```
pub fn labeled_field<C>(label: impl Into<String>, control: C) -> Column
where
    C: FormControl + 'static,
{
    let label = label.into();
    let control = control.with_form_accessible_name(&label);
    crate::column![Text::new(&label).with_role(TextRole::Label), control,]
        .with_no_padding()
        .with_intrinsic_height()
}

/// Compose a checkbox or toggle with one inline visible and accessible label.
///
/// The control comes first, matching platform form conventions. `label` is a
/// Fluent message key resolved live by both the semantic [`Text`] and the
/// control's accessibility node.
///
/// ```
/// use tilo_ui::widget::{Checkbox, labeled_check};
///
/// let remember = labeled_check(
///     "account-remember-label",
///     Checkbox::new("remember", "remember-value"),
/// );
/// ```
pub fn labeled_check<C>(label: impl Into<String>, control: C) -> Row
where
    C: CheckControl + 'static,
{
    let label = label.into();
    let control = control.with_form_accessible_name(&label);
    crate::row![control, Text::new(&label).with_role(TextRole::Label),].no_wrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::Role;

    #[test]
    fn labeled_controls_share_one_key_for_visible_and_spoken_labels() {
        fn one_key_names_the_visible_label_and_text_input() {
            let field = labeled_field(
                "profile-name-label",
                TextInput::local().with_id("profile-name"),
            );
            let children = Widget::children(&field);

            let label = children
                .first()
                .and_then(|child| child.accessibility())
                .expect("visible label is accessible");
            let input = children
                .get(1)
                .and_then(|child| child.accessibility())
                .expect("input is accessible");
            assert_eq!(label.role, Role::StaticText);
            assert_eq!(label.name.as_deref(), Some("profile-name-label"));
            assert_eq!(input.role, Role::TextInput);
            assert_eq!(input.name.as_deref(), Some("profile-name-label"));
        }

        fn labeled_field_hugs_its_content_inside_a_tall_form() {
            let field = labeled_field(
                "profile-name-label",
                TextInput::local().with_id("profile-name"),
            );
            let app = crate::headless::Driver::new(
                crate::State::new(),
                Box::new(crate::column![field]),
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
                Checkbox::new("remember", "remember-value"),
            );
            let toggle = labeled_field("sync-label", Toggle::new("sync", "sync-value"));
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

        fn composite_inputs_share_the_contract() {
            use chrono::NaiveDate;
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
                        DatePicker::new(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()),
                    ),
                    Role::ComboBox,
                ),
                (
                    labeled_field("advanced-label", TimePicker::new(10, 30)),
                    Role::ComboBox,
                ),
                (
                    labeled_field(
                        "advanced-label",
                        ColorPicker::new(Color::from_rgb8(20, 40, 60)),
                    ),
                    Role::ComboBox,
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

        fn inline_check_uses_one_key_for_visible_and_spoken_labels() {
            let check = labeled_check(
                "remember-label",
                Checkbox::new("remember", "remember-value"),
            );
            let children = Widget::children(&check);
            let control = children[0].accessibility().expect("checkbox semantics");
            let label = children[1]
                .accessibility()
                .expect("visible label semantics");

            assert_eq!(control.role, Role::Checkbox);
            assert_eq!(control.name.as_deref(), Some("remember-label"));
            assert_eq!(label.role, Role::StaticText);
            assert_eq!(label.name.as_deref(), Some("remember-label"));
        }

        one_key_names_the_visible_label_and_text_input();
        labeled_field_hugs_its_content_inside_a_tall_form();
        standard_boolean_and_range_controls_share_the_contract();
        composite_inputs_share_the_contract();
        inline_check_uses_one_key_for_visible_and_spoken_labels();
    }
}
