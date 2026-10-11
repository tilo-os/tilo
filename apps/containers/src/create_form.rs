//! The new-container form: one labelled field per draft entry, each showing
//! the draft's collaborative text in the shared document.
//!
//! Every edit in a field is a splice of its text through the resource's
//! dispatcher ([`TextControl`]), so the draft merges with a second person
//! typing in the same form, undoes a word at a time, and shows a restore or
//! an agent's edit as it lands.

use std::cell::RefCell;
use std::rc::Rc;
use ui::framework::{Dispatcher, TextControl};
use ui::tilo_resource::PlainText;

use ui::icons::Icon;
use ui::widget::{
    Autocomplete, Column, Density, Field, Form, FormControl, FormSection, Suggestion, TextInput,
    Widget,
};

use crate::doc::{CreateDoc, CreateField};

/// The installed images' references, which the image field suggests. The
/// app keeps it current as the engine changes.
pub(crate) type InstalledImages = Rc<RefCell<Vec<String>>>;

/// One draft field over the draft's text of its field; a field can suggest
/// what to type (the image field offers the installed images).
pub(crate) struct DraftInput {
    field: CreateField,
    text: TextControl<Autocomplete>,
}

impl DraftInput {
    fn new(field: CreateField, input: TextInput, images: &InstalledImages) -> Self {
        let kind = input.kind();
        let input = input.with_placeholder(field.placeholder());
        let mut control =
            Autocomplete::with_input(input).with_id(&format!("container_create_{}", field.key()));
        // The field keeps what its text is; suggesting does not make it a
        // search.
        control.input_mut().set_kind(kind);
        if field == CreateField::Image {
            let images = Rc::clone(images);
            control.set_suggester(move |typed| suggest_images(&images.borrow(), typed));
        }
        Self {
            field,
            text: TextControl::view_only(control, move |doc| CreateDoc::field_obj(doc, field)),
        }
    }

    pub(crate) fn field(&self) -> CreateField {
        self.field
    }

    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        self.text.control_mut().input_mut()
    }

    /// Edit the draft through `dispatcher` from now on.
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<PlainText>) {
        self.text.attach(dispatcher);
    }
}

impl FormControl for DraftInput {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        let control = self.input_mut();
        let named = std::mem::replace(control, TextInput::local()).with_accessible_name(name);
        *control = named;
        self
    }
}

impl ui::widget::Decorator for DraftInput {
    type Inner = TextControl<Autocomplete>;

    fn inner(&self) -> &TextControl<Autocomplete> {
        &self.text
    }

    fn inner_mut(&mut self) -> &mut TextControl<Autocomplete> {
        &mut self.text
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

/// The references of `engine`'s images that a container can be created
/// from (an untagged image has none).
pub(crate) fn image_references(engine: &crate::model::Engine) -> Vec<String> {
    engine
        .images
        .iter()
        .filter(|image| image.repository != "<none>")
        .map(crate::model::Image::reference)
        .collect()
}

/// The installed images whose reference holds what was typed, in the
/// engine's order; nothing to suggest once the field names one exactly.
fn suggest_images(images: &[String], typed: &str) -> Vec<Suggestion> {
    let typed = typed.trim().to_lowercase();
    if images.iter().any(|image| image.to_lowercase() == typed) {
        return Vec::new();
    }
    images
        .iter()
        .filter(|image| image.to_lowercase().contains(&typed))
        .map(|image| Suggestion::new(image.clone()).with_icon(Icon::Layers))
        .collect()
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
pub(crate) fn form(make_input: fn() -> TextInput, images: &InstalledImages) -> Column {
    let mut section = FormSection::untitled();
    for field in ORDER {
        let (label, help) = copy(field);
        let mut row = Field::new(label, DraftInput::new(field, make_input(), images));
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
