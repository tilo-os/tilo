//! The new-container form: one labelled field per draft entry, each showing
//! the draft's collaborative text in the shared document.
//!
//! Every edit in a field is a splice of its text through the resource's
//! dispatcher ([`TextControl`]), so the draft merges with a second person
//! typing in the same form, undoes a word at a time, and shows a restore or
//! an agent's edit as it lands.

use ui::framework::{Dispatcher, TextControl};
use ui::tilo_resource::PlainText;
use ui::widget::{Column, Density, Field, Form, FormControl, FormSection, TextInput, Widget};

use crate::doc::{CreateDoc, CreateField};

/// One draft field: a [`TextInput`] over the draft's text of its field.
pub(crate) struct DraftInput {
    field: CreateField,
    text: TextControl<TextInput>,
}

impl DraftInput {
    fn new(field: CreateField, input: TextInput) -> Self {
        let input = input
            .with_id(&format!("container_create_{}", field.key()))
            .with_placeholder(field.placeholder());
        Self {
            field,
            text: TextControl::view_only(input, move |doc| CreateDoc::field_obj(doc, field)),
        }
    }

    pub(crate) fn field(&self) -> CreateField {
        self.field
    }

    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        self.text.control_mut()
    }

    /// Edit the draft through `dispatcher` from now on.
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<PlainText>) {
        self.text.attach(dispatcher);
    }
}

impl FormControl for DraftInput {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        let control = self.text.control_mut();
        let named = std::mem::replace(control, TextInput::local()).with_accessible_name(name);
        *control = named;
        self
    }
}

impl ui::widget::Decorator for DraftInput {
    type Inner = TextControl<TextInput>;

    fn inner(&self) -> &TextControl<TextInput> {
        &self.text
    }

    fn inner_mut(&mut self) -> &mut TextControl<TextInput> {
        &mut self.text
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// The field answers for its node, so input the host routes by node
    /// (keys, accessibility actions) reaches the control's splices.
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = Widget::build_layout(&mut self.text, taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }
}

/// What a field is called and the one line that says how to fill it.
fn copy(field: CreateField) -> (&'static str, Option<&'static str>) {
    match field {
        CreateField::Image => ("Image", None),
        CreateField::Name => ("Name", None),
        CreateField::Ports => (
            "Ports",
            Some("Host and container port, separated by commas"),
        ),
        CreateField::Env => ("Environment", Some("KEY=value pairs, separated by commas")),
        CreateField::Volumes => ("Volumes", Some("Volume and path, separated by commas")),
        CreateField::Command => ("Command", Some("Leave empty to run the image's default")),
    }
}

/// The order the form asks in: what to run first.
pub(crate) const ORDER: [CreateField; 6] = [
    CreateField::Image,
    CreateField::Name,
    CreateField::Ports,
    CreateField::Env,
    CreateField::Volumes,
    CreateField::Command,
];

/// The form, with a fresh input per field from `make_input`. It is a
/// compact form: the labels stand beside their fields where the sheet has
/// the room, so the whole draft fits a small window.
pub(crate) fn form(make_input: fn() -> TextInput) -> Column {
    let mut section = FormSection::untitled();
    for field in ORDER {
        let (label, help) = copy(field);
        let mut row = Field::new(label, DraftInput::new(field, make_input()));
        if let Some(help) = help {
            row = row.with_help(help);
        }
        section = section.field(row);
    }
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(
            Form::new()
                .with_density(Some(Density::Compact))
                .section(section),
        ))
}

/// Visit every draft field under `widget`.
pub(crate) fn for_each_input(widget: &mut dyn Widget, f: &mut dyn FnMut(&mut DraftInput)) {
    if let Some(input) = widget
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<DraftInput>())
    {
        f(input);
        return;
    }
    for child in widget.children_mut() {
        for_each_input(child, f);
    }
}
