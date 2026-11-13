//! The sheets that collect what a durable action needs: a name to create, save or rename
//! with, a variable to add or remove. Deleting asks the host to confirm instead.

use ui::LocalState;
use ui::widget::{Column, Element, Field, ModalDialog, TextInput, TextInputKind, Widget};

use crate::commands::{Cmd, FormMode, Sink};

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

fn field(
    id: &str,
    label: &str,
    state: &LocalState<String>,
    kind: TextInputKind,
    focused: bool,
) -> Element {
    let mut input = TextInput::from_state(state)
        .with_id(id)
        .with_kind(kind)
        .with_accessible_name(label);
    if focused {
        input.set_focus(true);
    }
    Box::new(Field::new(label, input))
}

pub(crate) fn build(mode: FormMode, fields: &Fields, sink: &Sink) -> Element {
    // As tall as its fields: a sheet for one name is a short one.
    let mut body = Column::new().with_no_padding().with_intrinsic_height();
    if mode.needs_name() {
        body = body.push(field(
            "api.form-name",
            &crate::locales::CATALOG.t("api-tester-name"),
            &fields.name,
            TextInputKind::Name,
            true,
        ));
    }
    if mode.needs_variable_fields() {
        body = body.push(field(
            "api.form-variable-key",
            &crate::locales::CATALOG.t("api-tester-variable-key-field"),
            &fields.key,
            TextInputKind::Code,
            true,
        ));
        if mode.needs_value() {
            body = body
                .push(field(
                    "api.form-variable-value",
                    &crate::locales::CATALOG.t("api-tester-variable-value-field"),
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
