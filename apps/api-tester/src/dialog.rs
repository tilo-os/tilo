//! The sheets that collect what a durable action needs: a name to create,
//! save or rename with, a variable to add or remove. Deleting a request or
//! an environment needs no sheet: the host asks the person to confirm.

use ui::LocalState;
use ui::widget::{Column, Element, Field, ModalDialog, TextInput, TextInputKind, Widget};

use crate::commands::{Cmd, FormMode, Sink};

/// The values a sheet collects, shared with the app that reads them on
/// submit.
#[derive(Clone)]
pub(crate) struct Fields {
    pub(crate) name: LocalState<String>,
    pub(crate) key: LocalState<String>,
    pub(crate) value: LocalState<String>,
    pub(crate) secret: LocalState<bool>,
}

impl Fields {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: LocalState::new(name.to_owned()),
            key: LocalState::new(String::new()),
            value: LocalState::new(String::new()),
            secret: LocalState::new(false),
        }
    }
}

fn field(label: &str, state: &LocalState<String>, kind: TextInputKind, focused: bool) -> Element {
    let id = match label {
        "Name" => "api.form-name",
        "Variable key" => "api.form-variable-key",
        "Variable value" => "api.form-variable-value",
        _ => "api.form-field",
    };
    let mut input = TextInput::from_state(state)
        .with_id(id)
        .with_kind(kind)
        .with_accessible_name(label);
    if focused {
        input.set_focus(true);
    }
    Box::new(Field::new(label, input))
}

/// The sheet for `mode`.
pub(crate) fn build(mode: FormMode, fields: &Fields, sink: &Sink) -> Element {
    let mut body = Column::new().with_no_padding();
    if mode.needs_name() {
        body = body.push(field("Name", &fields.name, TextInputKind::Name, true));
    }
    if mode.needs_variable_fields() {
        body = body.push(field(
            "Variable key",
            &fields.key,
            TextInputKind::Code,
            true,
        ));
        if mode.needs_value() {
            body = body
                .push(field(
                    "Variable value",
                    &fields.value,
                    TextInputKind::Password,
                    false,
                ))
                .push(Box::new(Field::new(
                    "Keep value on this device",
                    ui::widget::Toggle::from_state(&fields.secret),
                )));
        }
    }
    let confirm = sink.clone();
    let cancel = sink.clone();
    let dismiss = sink.clone();
    Box::new(
        ModalDialog::new(mode.heading(), body)
            .with_test_id("api.form")
            .open(true)
            .with_max_width(440.0)
            .with_destructive(mode.is_destructive())
            .primary_action(mode.confirm_label(), move || {
                confirm.borrow_mut().push(Cmd::SubmitForm);
            })
            .secondary_action("Cancel", move || {
                cancel.borrow_mut().push(Cmd::CancelForm);
            })
            .on_dismiss(move || dismiss.borrow_mut().push(Cmd::CancelForm)),
    )
}
