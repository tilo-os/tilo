//! Form composition that keeps visible and accessible labels in one contract.

use super::{
    Checkbox, ColorPicker, Column, DatePicker, Dropdown, NumberInput, Slider, Text, TextInput,
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

/// Compose a visible semantic label with the control it names.
///
/// `label` is normally a Fluent message key. Both the [`Text`] and control
/// resolve that same key through the live localization catalog, preventing the
/// visible copy and accessibility name from drifting. The returned ordinary
/// [`Column`] remains chainable and participates in the standard retained
/// lifecycle.
///
/// ```
/// use ui::widget::{TextInput, labeled_field};
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
    crate::column![Text::new(&label).with_role(TextRole::Label), control,].with_no_padding()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::Role;

    #[test]
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

    #[test]
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

    #[test]
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
}
