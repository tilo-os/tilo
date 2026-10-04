//! The sheets that collect a name or a confirmation for a durable action:
//! create, save, rename and delete.

use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::{Column, Element, Field, ModalDialog, Text, TextInput, TextInputKind, Widget};

use crate::commands::{Cmd, FormMode, Sink};

/// The values a sheet collects, shared with the app that reads them on
/// submit.
#[derive(Clone)]
pub(crate) struct Fields {
    pub(crate) name: LocalState<String>,
    pub(crate) key: LocalState<String>,
    pub(crate) value: LocalState<String>,
}

impl Fields {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: LocalState::new(name.to_owned()),
            key: LocalState::new(String::new()),
            value: LocalState::new(String::new()),
        }
    }
}

fn field(label: &str, state: &LocalState<String>, kind: TextInputKind, focused: bool) -> Element {
    let mut input = TextInput::from_state(state)
        .with_kind(kind)
        .with_accessible_name(label);
    if focused {
        input.set_focus(true);
    }
    Box::new(Field::new(label, input))
}

fn sentence(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Body))
}

/// The sheet for `mode`. `subject` names what a delete removes.
pub(crate) fn build(mode: FormMode, subject: &str, fields: &Fields, sink: &Sink) -> Element {
    let mut body = Column::new().with_no_padding();
    if mode.needs_name() {
        body = body.push(field("Name", &fields.name, TextInputKind::Name, true));
    }
    if mode.needs_variable_fields() {
        body = body.push(field("Variable", &fields.key, TextInputKind::Code, true));
        if mode.needs_value() {
            body = body.push(field("Value", &fields.value, TextInputKind::Code, false));
        }
    }
    match mode {
        FormMode::DeleteRequest => {
            body = body.push(sentence(&format!(
                "\u{201c}{subject}\u{201d} will be removed from its collection."
            )));
        }
        FormMode::DeleteEnvironment => {
            body = body.push(sentence(&format!(
                "The \u{201c}{subject}\u{201d} environment and its variables will be removed."
            )));
        }
        _ => {}
    }
    let confirm = sink.clone();
    let cancel = sink.clone();
    let dismiss = sink.clone();
    Box::new(
        ModalDialog::new(mode.heading(), body)
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
