//! Form composition that keeps visible and accessible labels in one contract.

use super::{
    Autocomplete, Checkbox, ColorPicker, Column, DatePicker, Dropdown, NumberInput, Row, Slider,
    Text, TextInput, TimePicker, Toggle, ToggleGroup,
};
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::Widget;

#[path = "labelled_component.rs"]
mod component;
pub use component::LabelledProps;

/// `label` names `content`, which is a control composed as plain elements
/// rather than one [`FormControl`] — a chooser, a field holding parts of its
/// own, anything already boxed as an [`Element`](tilo_ui_base::widget::Element). It is the
/// naming a [`Field`](crate::widget::Field)'s label gives its control, for a
/// layout that places the label itself (over the control, in a stack).
///
/// The visible label is the caller's; this stands in for it in the
/// accessibility tree, so a control that carried no name of its own is
/// reached by the words the person reads beside it.
pub struct Labelled {
    label: String,
    description: String,
    content: Box<dyn Widget>,
}

impl Labelled {
    #[must_use]
    pub fn new(label: impl Into<String>, content: Box<dyn Widget>) -> Self {
        Self {
            label: label.into(),
            description: String::new(),
            content,
        }
    }

    /// A line that explains the control, read after its name.
    #[must_use]
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

impl tilo_ui_base::widget::Decorator for Labelled {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    /// The control's own node, named by its label where it had no name (or
    /// only the value or placeholder it shows): the visible label is read
    /// once, as the control's name.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let mut node = self.content.accessibility()?;
        if node.name.is_none() || node.name_found {
            node.name = Some(tilo_ui_base::i18n::widget_text(&self.label));
            node.name_found = false;
        }
        if node.description.is_none() && !self.description.is_empty() {
            node.description = Some(tilo_ui_base::i18n::widget_text(&self.description));
        }
        Some(node)
    }
}

/// A control that can receive the accessible name of a visible form label.
///
/// Implemented by the toolkit's standard text, choice, numeric, date, time,
/// colour, checkbox, toggle, and slider controls. Application-defined
/// composite controls can implement this trait to participate in
/// [`labeled_field`] without changing their widget tree.
pub trait FormControl: Widget + Sized {
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

impl FormControl for ToggleGroup {
    fn with_form_accessible_name(self, name: &str) -> Self {
        self.with_accessible_name(name)
    }
}

impl FormControl for Autocomplete {
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
///     Checkbox::bound("remember", "remember-value"),
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
    use tilo_ui_base::a11y::Role;

    #[test]
    fn labeled_controls_share_one_key_for_visible_and_spoken_labels() {
        fn labeled_field_hugs_its_content_inside_a_tall_form() {
            let field = labeled_field(
                "profile-name-label",
                TextInput::local().with_id("profile-name"),
            );
            let app = crate::app::driver::Driver::new(
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
                    labeled_field(
                        "advanced-label",
                        ColorPicker::new(Color::from_rgb8(20, 40, 60)),
                    ),
                    Role::ComboBox,
                ),
                (
                    labeled_field(
                        "advanced-label",
                        ToggleGroup::new(
                            vec![
                                crate::widget::Segment::new("Day"),
                                crate::widget::Segment::new("Week"),
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
                .push(Box::new(Text::new("Display name").naming_its_control()))
                .push(Box::new(Labelled::new(
                    "Display name",
                    Box::new(TextInput::local().with_id("display-name")),
                )));
            let app =
                crate::app::driver::Driver::new(crate::State::new(), Box::new(field), (400, 200));
            let named = app.a11y_nodes_named("Display name");
            assert_eq!(named.len(), 1, "the label names the field once: {named:?}");
            let node = app.a11y_node(named[0]).expect("the exported node");
            assert_eq!(node.role(), crate::accesskit::Role::TextInput);
            let input = app.targets().get("display-name").copied();
            assert!(input.is_some(), "the field is the one drawn");
        }

        /// A control that already says what it is keeps its own name; the label
        /// only fills in the name one lacks.
        fn a_named_control_keeps_its_name_under_a_label() {
            let input = TextInput::local()
                .with_id("email")
                .with_accessible_name("Email address");
            let node = Labelled::new("Display name", Box::new(input))
                .accessibility()
                .expect("the field's semantics");
            assert_eq!(node.name.as_deref(), Some("Email address"));
        }

        /// A label names a waiting block, which says "Loading" only as a fallback of its own:
        /// the shell holds an app-drawn card's place by the words that name the block, not by
        /// the skeleton's placeholder.
        fn a_label_names_a_waiting_block() {
            let block = Labelled::new(
                "Turn left onto Main Street in Tasks",
                Box::new(crate::widget::Skeleton::new(
                    tilo_ui_base::widget::SkeletonShape::Block { height: 160.0 },
                )),
            );
            let node = block.accessibility().expect("the block's semantics");
            assert_eq!(
                node.name.as_deref(),
                Some("Turn left onto Main Street in Tasks")
            );
            assert!(node.state.busy, "it still says the block waits");
        }

        labeled_field_hugs_its_content_inside_a_tall_form();
        standard_boolean_and_range_controls_share_the_contract();
        composite_inputs_share_the_contract();
        a_label_over_a_boxed_control_is_the_controls_name();
        a_named_control_keeps_its_name_under_a_label();
        a_label_names_a_waiting_block();
    }
}
