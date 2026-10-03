//! The new-container form: one labelled field per draft entry, each bound to
//! the draft's collaborative `Text` in the shared document.
//!
//! A field edits locally and hands its new text to the app as an
//! [`Intent::EditDraft`]; the app records it as a replayable
//! [`ContainersAction::SetCreateField`](crate::resource::ContainersAction) and
//! the field binds to the stored text on its next sync. The draft therefore
//! survives undo, History and a second person editing the same form.

use ui::widget::{
    AppModel, Column, Field, FormControl, LabelPlacement, ModelPath, TextInput, Widget,
};

use crate::doc::{CreateField, StoredCreateField, ViewDoc};
use crate::view::{Intent, Intents};

/// One draft field: a [`TextInput`] bound to `view.create.<field>`.
pub(crate) struct DraftInput {
    field: CreateField,
    input: TextInput,
    intents: Intents,
}

impl DraftInput {
    fn new(field: CreateField, input: TextInput, intents: &Intents) -> Self {
        let mut input = input
            .with_id(&format!("container_create_{}", field.key()))
            .with_placeholder(field.placeholder());
        input.set_direct_crdt_writes(false);
        Self {
            field,
            input,
            intents: intents.clone(),
        }
    }

    pub(crate) fn field(&self) -> CreateField {
        self.field
    }

    #[cfg(test)]
    pub(crate) fn input(&self) -> &TextInput {
        &self.input
    }

    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    /// Bind to the stored draft text (or clear when there is none).
    pub(crate) fn bind(&mut self, state: &mut AppModel, path: &ModelPath) {
        let stored = {
            let (doc, _) = ui::doc::document(state, path);
            ViewDoc::stored_create_field(doc, self.field)
        };
        match stored {
            StoredCreateField::Text { object, .. } => {
                if self.input.bound_obj() != Some(&object) {
                    self.input.bind_text(object);
                }
                Widget::sync(&mut self.input, state, path);
            }
            StoredCreateField::Absent
            | StoredCreateField::WrongType
            | StoredCreateField::Conflict => {
                self.input.clear_text_binding();
                if !self.input.value().is_empty() {
                    self.input.set_text("");
                }
            }
        }
    }

    /// Hand a changed value to the app.
    fn report(&self, before: &str) {
        if self.input.value() != before {
            self.intents
                .push(Intent::EditDraft(self.field, self.input.value().to_owned()));
        }
    }
}

impl FormControl for DraftInput {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        self.input = self.input.with_accessible_name(name);
        self
    }
}

impl ui::widget::Decorator for DraftInput {
    type Inner = TextInput;

    fn inner(&self) -> &TextInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.bind(state, path);
    }

    /// The field answers for its node, so input the host routes by node
    /// (keys, accessibility actions) reaches [`Self::report`].
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.input.build_layout(taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let before = self.input.value().to_owned();
        Widget::handle_event(&mut self.input, event, ctx);
        self.report(&before);
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let before = self.input.value().to_owned();
        let handled = Widget::perform_action(&mut self.input, action, state, path);
        self.report(&before);
        handled
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let before = self.input.value().to_owned();
        let handled = Widget::perform_action_at(&mut self.input, action, key_path, state, path);
        self.report(&before);
        handled
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

/// The form, with a fresh input per field from `make_input`.
pub(crate) fn form(make_input: fn() -> TextInput, intents: &Intents) -> Column {
    let mut column = Column::new().with_intrinsic_height().with_no_padding();
    for field in ORDER {
        let (label, help) = copy(field);
        let mut row = Field::new(label, DraftInput::new(field, make_input(), intents))
            .with_label_placement(Some(LabelPlacement::Above));
        if let Some(help) = help {
            row = row.with_help(help);
        }
        column = column.push(Box::new(row));
    }
    column
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
