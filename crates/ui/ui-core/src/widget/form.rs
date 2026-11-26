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
