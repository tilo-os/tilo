//! The sheet's one view: every kind of question from the same toolkit
//! dialog, so what is drawn, what is pressed and what a reader meets are
//! one layout.

use ui::framework::Dispatcher;
use ui::prelude::*;
use ui::widget::{
    Button, ButtonRole, Column, Element, FieldRole, FieldSpec, ListRow, ModalDialog, SecretKind,
    SecretPrompt, Text, TextInput, TextInputKind, Widget,
};

use super::{
    Body, Device, Dialog, DraftRow, DraftValue, Field, Focus, PortalEffect, PortalSeat,
    PortalSheet, Prompt, PromptKind, Shortcut, Shown,
};
use crate::t;

/// The id of the sheet's dialog, which the shell finds it by.
pub(super) const DIALOG_ID: &str = "portal-sheet";
/// The id of the secret prompt.
pub(super) const PROMPT_ID: &str = "portal-secret-prompt";

/// What every part of the view acts through.
#[derive(Clone)]
pub(super) struct Parts {
    pub(super) dispatch: Dispatcher<PortalSheet>,
    pub(super) shown: LocalState<Shown>,
    pub(super) seat: PortalSeat,
}

impl Parts {
    pub(super) fn send(&self, effect: &PortalEffect) {
        self.dispatch.effect(effect);
    }

    pub(super) fn focus(&self, focus: Focus) {
        self.shown.update(|shown| shown.focus = focus);
    }

    /// Enter, or the primary button: the next page while more of the
    /// review follows, else the answer.
    pub(super) fn confirm(&self) {
        let more = self
            .shown
            .get()
            .dialog
            .as_ref()
            .is_some_and(Dialog::reviewing_more);
        self.send(if more {
            &PortalEffect::Next
        } else {
            &PortalEffect::Confirm
        });
    }
}

fn index(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The whole sheet, or nothing while no question is up.
pub(super) fn sheet(shown: &Shown, parts: &Parts) -> Element {
    let Some(dialog) = &shown.dialog else {
        return Box::new(Column::new());
    };
    if let Body::Secret(prompt) = &dialog.body {
        return secret(dialog, prompt, parts);
    }
    let gap = ui::tokens::Spacing::BASE.s3;
    let mut body = Column::new().with_gap(gap);
    if !dialog.heading.is_empty() {
        body = body.push(Box::new(
            Text::new(&dialog.heading).with_role(TextRole::Label),
        ));
    }
    if !dialog.detail.is_empty() {
        body = body.push(Box::new(
            Text::new(&dialog.detail).with_role(TextRole::Body),
        ));
    }
    body = body.push(rows(dialog, shown.focus, parts));
    if let Some(error) = &dialog.error {
        body = body.push(Box::new(
            Text::new(error)
                .with_role(TextRole::Caption)
                .with_id("portal-sheet-error"),
        ));
    }
    let mut sheet = ModalDialog::new(dialog.title.clone(), body)
        .with_test_id(DIALOG_ID)
        .with_description(t("portal-sheet-description"))
        .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
        .open(true);
    let cancel = parts.clone();
    sheet = sheet.secondary_action(dialog.cancel_label.clone(), move || {
        cancel.send(&PortalEffect::Cancel);
    });
    if let Some((page, _)) = dialog.page.filter(|(page, _)| *page > 0) {
        let back = parts.clone();
        let _ = page;
        sheet = sheet.tertiary_action(t("portal-back"), move || back.send(&PortalEffect::Back));
    }
    // A sheet whose choices are its answers has no button of its own.
    if !matches!(dialog.body, Body::Answers { .. }) {
        let label = if dialog.reviewing_more() {
            t("portal-continue")
        } else {
            dialog.confirm_label.clone()
        };
        let confirm = parts.clone();
        sheet = sheet.primary_action(label, move || confirm.confirm());
    }
    // A press beside the sheet answers nothing: declining is Escape or
    // the button that says so.
    Box::new(sheet)
}

/// What the person chooses or types, under the detail.
fn rows(dialog: &Dialog, focus: Focus, parts: &Parts) -> Element {
    let mut column = Column::new().with_gap(ui::tokens::Spacing::BASE.s1);
    match &dialog.body {
        Body::Review | Body::Secret(_) => {}
        Body::Choices(choices) => column = choices_rows(column, dialog, choices, focus, parts),
        Body::Answers {
            choices,
            suggested,
            remember,
        } => {
            if let Some(remember) = remember {
                let press = parts.clone();
                column = column.push(Box::new(
                    ListRow::new("portal-remember", remember.label.clone())
                        .checkbox(remember.on, move |_| press.send(&PortalEffect::Remember)),
                ));
            }
            for (at, choice) in choices.iter().enumerate() {
                let press = parts.clone();
                let mut button = Button::new(&format!("portal-answer-{at}"), choice)
                    .role(if at == *suggested {
                        ButtonRole::Primary
                    } else {
                        ButtonRole::Secondary
                    })
                    .on_click(move || press.send(&PortalEffect::Answer { index: index(at) }));
                button.set_focus(at == dialog.selected);
                column = column.push(Box::new(button));
            }
        }
        Body::Devices(devices) => {
            for (at, device) in devices.iter().enumerate() {
                column = column.push(device_row(at, device, dialog.selected == at, parts));
            }
        }
        Body::Shortcuts(rows) => {
            for (at, row) in rows.iter().enumerate() {
                column = column.push(shortcut_row(at, row, dialog.selected == at, parts));
            }
        }
        Body::Launcher {
            name,
            icons,
            icon_editable,
        } => {
            if let Some(name) = name {
                let mut field = text_field(Field::Name, name, parts)
                    .with_accessible_name(t("portal-a11y-name"));
                field.set_focus(focus == Focus::Field(Field::Name));
                column = column.push(Box::new(field));
            }
            for (at, icon) in icons.iter().enumerate() {
                let press = parts.clone();
                column = column.push(Box::new(
                    ListRow::new(format!("portal-icon-{at}"), icon.clone())
                        .checked(dialog.selected == at)
                        .selected(focus == Focus::Icons && dialog.selected == at)
                        .with_enabled(*icon_editable)
                        .on_activate(move || {
                            press.focus(Focus::Icons);
                            press.send(&PortalEffect::Icon { index: index(at) });
                        }),
                ));
            }
        }
        Body::Entry {
            masked,
            required: _,
            choices,
        } => {
            column = choices_rows(column, dialog, choices, focus, parts);
            let mut field = text_field(Field::Answer, "", parts)
                .with_secret(*masked)
                .with_accessible_name(t("portal-a11y-answer"));
            field.set_focus(focus == Focus::Field(Field::Answer));
            if dialog.error.is_some() {
                field.set_invalid(true);
            }
            column = column.push(Box::new(field));
        }
        Body::Draft { rows, choices } => {
            for row in rows {
                column = column.push(draft_row(row, focus, dialog.error.is_some(), parts));
            }
            column = choices_rows(column, dialog, choices, focus, parts);
        }
    }
    Box::new(column)
}

/// One row per choice, the highlighted one checked.
fn choices_rows(
    mut column: Column,
    dialog: &Dialog,
    choices: &[String],
    focus: Focus,
    parts: &Parts,
) -> Column {
    for (at, choice) in choices.iter().enumerate() {
        let press = parts.clone();
        let chosen = dialog.selected == at;
        column = column.push(Box::new(
            ListRow::new(format!("portal-choice-{at}"), choice.clone())
                .checked(chosen)
                .selected(chosen && focus == Focus::Choices)
                .on_activate(move || {
                    press.focus(Focus::Choices);
                    press.send(&PortalEffect::Select { index: index(at) });
                }),
        ));
    }
    column
}

fn device_row(at: usize, device: &Device, focused: bool, parts: &Parts) -> Element {
    let press = parts.clone();
    Box::new(
        ListRow::new(format!("portal-device-{at}"), device.label.clone())
            .subtitle(t(if device.writable {
                "portal-usb-row-read-write"
            } else {
                "portal-usb-row-read-only"
            }))
            .selected(focused)
            .checkbox(device.selected, move |_| {
                press.send(&PortalEffect::Select { index: index(at) });
                press.send(&PortalEffect::Toggle { index: index(at) });
            }),
    )
}

fn shortcut_row(at: usize, row: &Shortcut, focused: bool, parts: &Parts) -> Element {
    let press = parts.clone();
    let mut list_row = ListRow::new(format!("portal-shortcut-{at}"), row.name.clone())
        .value(
            row.keys
                .clone()
                .unwrap_or_else(|| t("portal-shortcut-not-set")),
        )
        .selected(focused)
        .on_activate(move || press.send(&PortalEffect::Select { index: index(at) }));
    if let Some(status) = &row.status {
        list_row = list_row.subtitle(status.clone()).warning();
    }
    Box::new(list_row)
}

fn draft_row(row: &DraftRow, focus: Focus, invalid: bool, parts: &Parts) -> Element {
    match &row.value {
        DraftValue::Flag(on) => {
            let press = parts.clone();
            let at = row.index;
            Box::new(
                ListRow::new(format!("portal-draft-{at}"), row.label.clone())
                    .selected(focus == Focus::Flag(at))
                    .with_enabled(row.editable)
                    .checkbox(*on, move |_| {
                        press.focus(Focus::Flag(at));
                        press.send(&PortalEffect::Toggle { index: index(at) });
                    }),
            )
        }
        DraftValue::Text {
            text,
            multiline,
            number,
            withheld,
        } => {
            let field_id = Field::Draft(row.index);
            let mut field = text_field(field_id, text, parts)
                .with_accessible_name(row.label.clone())
                .with_read_only(!row.editable);
            if !row.editable {
                // Read as text, not as a field waiting for typing.
                field = field.with_plain_chrome();
            }
            if *number {
                field = field.with_kind(TextInputKind::Number);
            }
            if *multiline {
                field = field.with_min_lines(3);
            }
            if *withheld {
                field.set_secret(true);
            }
            field.set_focus(focus == Focus::Field(field_id));
            if invalid && row.editable {
                field.set_invalid(true);
            }
            Box::new(
                Column::new()
                    .with_gap(ui::tokens::Spacing::BASE.s1)
                    .push(Box::new(Text::new(&row.label).with_role(TextRole::Caption)))
                    .push(Box::new(field)),
            )
        }
    }
}

/// A typed field of the sheet: it starts from what the seat holds for it
/// (or `initial`), and everything typed goes to the seat, never to an
/// effect.
fn text_field(field: Field, initial: &str, parts: &Parts) -> TextInput {
    let typed = parts.seat.clone();
    let held = parts.seat.texts.borrow().get(&field).cloned();
    if held.is_none() {
        parts
            .seat
            .texts
            .borrow_mut()
            .insert(field, zeroize::Zeroizing::new(initial.to_owned()));
    }
    let text = held.unwrap_or_else(|| zeroize::Zeroizing::new(initial.to_owned()));
    let mut input = TextInput::local()
        .with_id(&field_id(field))
        .with_initial_value(&text)
        .on_change(move |text| {
            typed
                .texts
                .borrow_mut()
                .insert(field, zeroize::Zeroizing::new(text.to_owned()));
        });
    input.set_cursor_pos(text.len());
    input
}

/// The test id of a typed field.
pub(super) fn field_id(field: Field) -> String {
    match field {
        Field::Name => "portal-name".to_owned(),
        Field::Answer => "portal-answer".to_owned(),
        Field::Draft(index) => format!("portal-draft-field-{index}"),
    }
}

/// A secret prompt: the toolkit's own, as a dialog, its fields kept in the
/// seat's buffers so nothing typed is lost when the shell says more.
fn secret(dialog: &Dialog, prompt: &Prompt, parts: &Parts) -> Element {
    let built = match prompt.kind {
        PromptKind::Fields => SecretPrompt::with_fields(
            prompt
                .fields
                .iter()
                .map(|field| {
                    let spec = if field.masked {
                        FieldSpec::secret(field.label.clone())
                    } else {
                        FieldSpec::text(field.label.clone()).with_initial(field.initial.clone())
                    };
                    if field.required {
                        spec
                    } else {
                        spec.optional()
                    }
                })
                .collect(),
        ),
        PromptKind::SimPin { attempts_left } => {
            let mut sim = SecretPrompt::new(SecretKind::SimPin { attempts_left });
            if let Some(field) = prompt
                .fields
                .first()
                .filter(|field| !field.label.is_empty())
            {
                sim = sim.with_label(FieldRole::Pin, field.label.clone());
            }
            sim
        }
        PromptKind::SimPuk { attempts_left } => {
            SecretPrompt::new(SecretKind::SimPuk { attempts_left })
        }
    };
    let mut built = built
        .with_id(PROMPT_ID)
        .with_title(dialog.title.clone())
        .with_resource(prompt.resource.clone())
        .with_primary_label(prompt.primary_label.clone())
        .with_cancel_label(dialog.cancel_label.clone())
        .with_field_buffers(parts.seat.secrets())
        .with_busy(prompt.busy.clone());
    if let Some(icon) = prompt.icon {
        built = built.with_icon(icon);
    }
    if let Some((label, choices)) = &prompt.choices {
        built = built.with_choices(label.clone(), choices.clone(), dialog.selected);
    }
    if let Some((label, on)) = &prompt.option {
        built = built.with_option(label.clone(), parts.seat.option().unwrap_or(*on));
    }
    if let Some((at, error)) = &prompt.error {
        built = built.with_error_at(*at, Some(error.clone()));
    }
    Widget::set_focus(&mut built, true);
    Box::new(built.into_dialog().with_test_id(DIALOG_ID))
}
