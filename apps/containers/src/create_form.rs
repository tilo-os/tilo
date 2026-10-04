//! The new-container form: one labelled field per draft entry, each bound to
//! the draft's collaborative `Text` in the shared document.
//!
//! Each field emits collaborative text splices through the framework.

use crate::doc::{CreateDoc, CreateField};
use crate::view::Intents;

use ui::framework::{Dispatcher, TextControl};
use ui::tilo_resource::PlainText;
use ui::widget::{
    AppModel, Column, Density, Field, Form, FormControl, FormSection, ModelPath, TextInput, Widget,
};

pub(crate) struct DraftInput {
    field: CreateField,
    input: TextControl<TextInput>,
    suggestions: ui::widget::Dropdown,
    options: Vec<String>,
    intents: Intents,
}
impl DraftInput {
    fn new(field: CreateField, input: TextInput, intents: &Intents) -> Self {
        let input = input
            .with_id(&format!("container_create_{}", field.key()))
            .with_placeholder(field.placeholder());
        let selected = intents.clone();
        let suggestions =
            ui::widget::Dropdown::from_state(Vec::new(), &ui::LocalState::new(String::new()))
                .with_id("container-image-suggestions")
                .on_change(move |value| {
                    selected.push(crate::view::Intent::EditDraft(
                        CreateField::Image,
                        value.to_owned(),
                    ))
                });
        Self {
            suggestions,
            options: Vec::new(),
            intents: intents.clone(),
            field,
            input: TextControl::view_only(input, move |doc| CreateDoc::field_obj(doc, field)),
        }
    }
    pub(crate) fn set_options(&mut self, options: &[String]) {
        if self.field == CreateField::Image && self.options != options {
            self.options = options.to_vec();
            self.filter_suggestions();
        }
    }
    fn filter_suggestions(&mut self) {
        let matches = crate::combobox::filter_options(&self.options, self.input.control().value());
        self.suggestions.set_options(
            matches
                .into_iter()
                .filter_map(|i| self.options.get(i).cloned())
                .collect(),
        );
    }
    pub(crate) fn open_suggestions(&mut self) {
        if self.field == CreateField::Image {
            self.filter_suggestions();
            self.suggestions.open_menu();
        }
    }
    pub(crate) fn close_suggestions(&mut self) {
        self.suggestions.close();
    }
    pub(crate) fn field(&self) -> CreateField {
        self.field
    }
    pub(crate) fn input(&self) -> &TextInput {
        self.input.control()
    }
    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        self.input.control_mut()
    }
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<PlainText>) {
        self.input.attach(dispatcher);
    }
}
impl FormControl for DraftInput {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        let input = std::mem::replace(self.input.control_mut(), TextInput::local());
        *self.input.control_mut() = input.with_accessible_name(name);
        self
    }
}
impl ui::widget::Decorator for DraftInput {
    type Inner = TextControl<TextInput>;
    fn inner(&self) -> &Self::Inner {
        &self.input
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.input
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        Widget::sync(&mut self.input, state, path);
        self.suggestions.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        nodes: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = Widget::build_layout(&mut self.input, taffy, theme, nodes);
        ui::widget::decorator::register_node(nodes, node, self);
        node
    }
    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        use ui::winit::event::{ElementState, WindowEvent};
        use ui::winit::keyboard::{Key, NamedKey};
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            if self.field == CreateField::Image
                && matches!(
                    key.logical_key,
                    Key::Named(
                        NamedKey::ArrowDown
                            | NamedKey::ArrowUp
                            | NamedKey::Escape
                            | NamedKey::Enter
                    )
                )
                && self.suggestions.is_open()
            {
                self.suggestions.set_focus(true);
                self.suggestions.handle_event(event, ctx);
                *ctx.handled = true;
                return;
            }
            if key.logical_key == Key::Named(NamedKey::Enter) {
                self.intents.push(crate::view::Intent::SubmitCreate);
                *ctx.handled = true;
                return;
            }
        }
        if self.suggestions.is_open()
            && matches!(
                event,
                WindowEvent::MouseInput { .. }
                    | WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseWheel { .. }
            )
        {
            self.suggestions.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        let before = self.input.control().value().to_owned();
        Widget::handle_event(&mut self.input, event, ctx);
        if before != self.input.control().value() {
            self.open_suggestions();
        }
    }
    fn draw_overlay(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        Widget::draw_overlay(&mut self.input, scene, transform, text, theme, presence);
        self.suggestions
            .apply_layout_external(Widget::get_rect(&self.input));
        self.suggestions
            .draw_overlay(scene, transform, text, theme, presence);
    }
    fn wants_pointer_capture(&self) -> bool {
        self.suggestions.is_open() || Widget::wants_pointer_capture(&self.input)
    }
    fn set_focus(&mut self, focused: bool) {
        Widget::set_focus(&mut self.input, focused);
        if !focused {
            self.suggestions.close();
        }
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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
    CreateField::Ports,
    CreateField::Env,
    CreateField::Volumes,
    CreateField::Command,
    CreateField::Name,
];

/// The form, with a fresh input per field from `make_input`. It is a
/// compact form: the labels stand beside their fields where the sheet has
/// the room, so the whole draft fits a small window.
pub(crate) fn form(make_input: fn() -> TextInput, intents: &Intents) -> Column {
    let mut section = FormSection::new("");
    for field in ORDER {
        let (label, help) = copy(field);
        let mut row = Field::new(label, DraftInput::new(field, make_input(), intents));
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

/// Visit every draft field under `widget`, read-only.
#[cfg(test)]
pub(crate) fn each_input(widget: &dyn Widget, f: &mut dyn FnMut(&DraftInput)) {
    if let Some(input) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<DraftInput>())
    {
        f(input);
        return;
    }
    for child in widget.children() {
        each_input(child, f);
    }
}
