//! An action's form, as the window presents it (forms-and-diagrams.md §1).
//!
//! The app sends the form of an action waiting for its arguments ([`wire::Form`]): its fields as
//! a tree of the same nodes views send, built and reconciled by the window's one reconciler, so
//! a field keeps its widget, focus and caret while the app answers each edit. The window adds
//! what every form has: the title, the submit button (disabled while an argument does not read)
//! and Cancel, and the presentation. Only this window waits; the rest of the workspace does not.
//!
//! What the person does in a field reaches the app as an edit of that field
//! ([`ToApp::FormEdit`]); the app keeps it as their view state and answers with the form as it
//! reads now.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as wire, ToApp};
use ui::widget::modal_dialog::DialogAction;
use ui::widget::{ModalDialog, Widget};

use crate::pane::Pane;
use crate::reconcile::{Reconciler, UiEvent};
use crate::{UiNode, Value};

/// The key prefix of a form's nodes in the window, apart from the views' keys.
pub const KEY: &str = "form~";

/// The key of the line saying why the action refused the last submit.
const ERROR: &str = "form~~error";

/// What the person answered in a form's own controls (submit, cancel), for the next batch.
#[derive(Default)]
pub struct Answers {
    submit: Cell<bool>,
    cancel: Cell<bool>,
}

/// The form the window shows.
pub struct Shown {
    pub id: u64,
    /// The tree the form's fields were last built from.
    tree: UiNode,
    /// Whether submit can be taken: what the app said last.
    ready: Rc<Cell<bool>>,
    /// The controls whose choice answers the form by itself, by key.
    completes: Vec<String>,
    /// Whether such a choice was made and the form is not read whole yet.
    picked: Cell<bool>,
    answers: Rc<Answers>,
    /// The submit button's label, as the app named it.
    submit: String,
    /// Whether the footer offers the submit button: a form its quick choices answer shows
    /// Cancel alone until the person is on the way in by hand.
    confirm: bool,
}

/// The dialog the window mounts: the fields, in a pane the reconciler reaches again.
pub type Sheet = ModalDialog<Pane>;

impl Shown {
    /// The form `form` built anew: its dialog, and what the window keeps of it.
    pub fn build(form: &wire::Form, reconciler: &mut Reconciler) -> (Self, Sheet) {
        let tree = tree_of(form);
        let fields = reconciler.build(&tree);
        for (key, text) in &form.fields {
            reconciler.set_text(key, text);
            reconciler.set_flag(key, text == "true");
        }
        let ready = Rc::new(Cell::new(form.ready));
        let answers = Rc::new(Answers::default());
        let gate = Rc::clone(&ready);
        let submit = Rc::clone(&answers);
        let cancel = Rc::clone(&answers);
        let mut sheet = ModalDialog::new(form.title.clone(), Pane::new(fields))
            .with_test_id(format!("{KEY}sheet"))
            .with_destructive(form.destructive)
            .primary_action(form.submit.clone(), move || submit.submit.set(true))
            .secondary_action(crate::i18n::t("tilolang-form-cancel"), move || {
                cancel.cancel.set(true);
            })
            .primary_enabled_if(move |_| gate.get())
            .with_foot_sheet()
            .open(true);
        let mut shown = Self {
            id: form.id,
            tree,
            ready,
            completes: form.answers.clone(),
            picked: Cell::new(false),
            answers,
            submit: form.submit.clone(),
            confirm: true,
        };
        shown.show_confirm(&mut sheet, form.confirm);
        (shown, sheet)
    }

    /// Show or take away the submit button as the app asks: a form a moment answers on one
    /// pick keeps it for when the person picks a day or types a moment instead.
    fn show_confirm(&mut self, sheet: &mut Sheet, confirm: bool) {
        if self.confirm == confirm {
            return;
        }
        self.confirm = confirm;
        let action = confirm.then(|| {
            let submit = Rc::clone(&self.answers);
            DialogAction::new(self.submit.clone(), move || submit.submit.set(true))
        });
        sheet.set_primary_action(action);
    }

    /// The form as the app sends it now, shown in `sheet`: its fields reconciled in place, so
    /// what the person is typing keeps its field.
    pub fn update(&mut self, form: &wire::Form, sheet: &mut Sheet, reconciler: &mut Reconciler) {
        let tree = tree_of(form);
        let pane = sheet.content_mut();
        if let Some(replaced) = reconciler.update_in(pane.content_mut(), &self.tree, &tree) {
            pane.replace(replaced);
        }
        // What the app read each field as now: a choice or the calendar set a control the
        // person had not touched, and the fields show it as the app does.
        for (key, text) in &form.fields {
            reconciler.set_text(key, text);
            reconciler.set_flag(key, text == "true");
        }
        sheet.invalidate_layout();
        self.tree = tree;
        self.ready.set(form.ready);
        self.completes.clone_from(&form.answers);
        if self.submit != form.submit {
            self.submit.clone_from(&form.submit);
            self.confirm = !form.confirm;
        }
        self.show_confirm(sheet, form.confirm);
    }

    /// What the person answered since this was last asked: submit, or cancel (Escape, or the
    /// Cancel button).
    pub fn take_answer(&self) -> Option<ToApp> {
        // A quick choice that says everything the form asks is answered as soon as the form
        // reads whole: choosing it is the answer, with no second press.
        if self.picked.get() && self.ready.get() {
            self.picked.set(false);
            return Some(ToApp::FormSubmit { form: self.id });
        }
        if self.answers.submit.take() && self.ready.get() {
            return Some(ToApp::FormSubmit { form: self.id });
        }
        if self.answers.cancel.take() {
            return Some(ToApp::FormCancel { form: self.id });
        }
        None
    }

    /// What the person did in a field of the form, as the edit the app hears of it; `None`
    /// for an event of another control than the form's.
    #[must_use]
    pub fn edit(&self, event: &UiEvent) -> Option<ToApp> {
        let (field, text) = match event {
            UiEvent::Edit(key, text) => (key.clone(), text.clone()),
            UiEvent::Toggle(key, checked) => (key.clone(), checked.to_string()),
            UiEvent::Fire(key, event, _) => (key.clone(), event.strip_prefix("pick.")?.to_owned()),
            _ => return None,
        };
        let answered = self.completes.contains(&field);
        let edit = ToApp::FormEdit {
            form: self.id,
            field,
            text,
        };
        if !matches!(&edit, ToApp::FormEdit { field, .. } if field.starts_with(KEY)) {
            return None;
        }
        if answered {
            self.picked.set(true);
        }
        Some(edit)
    }
}

/// Whether the form is one a popover holds: at most two short fields. Shown beside the
/// control that ran its action, when that control is still on screen.
#[must_use]
pub fn fits_a_popover(form: &wire::Form) -> bool {
    form.short && form.fields.len() <= 2
}

/// Whether `event` happened in a form's control.
#[must_use]
pub fn in_form(event: &UiEvent) -> bool {
    match event {
        UiEvent::Edit(key, _)
        | UiEvent::Toggle(key, _)
        | UiEvent::Fire(key, _, _)
        | UiEvent::Press(key) => key.starts_with(KEY),
        _ => false,
    }
}

/// The fields' tree as the window builds it: the app's, and under it why the action refused
/// the last submit.
fn tree_of(form: &wire::Form) -> UiNode {
    let mut tree = crate::reconcile::normalize(&form.tree);
    if let Some(error) = &form.error {
        let mut props = BTreeMap::new();
        props.insert("value".to_owned(), Value::Text(error.clone()));
        props.insert("role".to_owned(), Value::Enum("caption".to_owned()));
        tree.children.push(UiNode {
            kind: crate::adapter::kind("Text"),
            key: ERROR.to_owned(),
            props,
            binding: None,
            events: BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        });
    }
    tree
}

/// The dialog `widget` is, when it is a form's.
pub fn sheet_of(widget: &mut dyn Widget) -> Option<&mut Sheet> {
    widget.as_any_mut()?.downcast_mut::<Sheet>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A form asking one question: the moment the person is to be reminded, the quick choices
    /// standing beside it, and whether the app reads an answer yet.
    fn moment_form(ready: bool) -> wire::Form {
        let mut props = BTreeMap::new();
        props.insert(
            "cases".to_owned(),
            Value::List(vec![Value::List(vec![
                Value::text("2026-11-08T17:00:00Z"),
                Value::text("Tomorrow morning"),
                Value::Opt(None),
            ])]),
        );
        wire::Form {
            id: 7,
            title: "Remind me…".to_owned(),
            submit: "Remind me".to_owned(),
            destructive: false,
            ready,
            error: None,
            from: None,
            fields: vec![(
                "form~when~quick".to_owned(),
                if ready { "2026-11-08T17:00:00Z" } else { "" }.to_owned(),
            )],
            short: false,
            answers: vec!["form~when~quick".to_owned()],
            confirm: false,
            tree: UiNode {
                kind: "form".to_owned(),
                key: KEY.to_owned(),
                props: BTreeMap::new(),
                binding: None,
                events: BTreeMap::new(),
                anchor: None,
                children: vec![UiNode {
                    kind: "choice".to_owned(),
                    key: "form~when~quick".to_owned(),
                    props,
                    binding: None,
                    events: BTreeMap::new(),
                    anchor: None,
                    children: Vec::new(),
                }],
            },
        }
    }

    /// Picking the quick choice of a form that asks one thing answers it: the app reads the
    /// pick, and the window submits as soon as it reads whole, with no second press.
    #[test]
    fn a_picked_quick_choice_answers_the_form() {
        let mut reconciler = Reconciler::new(Rc::default());
        let (mut shown, mut sheet) = Shown::build(&moment_form(false), &mut reconciler);
        assert_eq!(
            shown.take_answer(),
            None,
            "nothing is answered before a pick"
        );

        let pick = UiEvent::Fire(
            "form~when~quick".to_owned(),
            "pick.2026-11-08T17:00:00Z".to_owned(),
            None,
        );
        assert_eq!(
            shown.edit(&pick),
            Some(ToApp::FormEdit {
                form: 7,
                field: "form~when~quick".to_owned(),
                text: "2026-11-08T17:00:00Z".to_owned(),
            }),
            "the pick is the field's answer"
        );
        assert_eq!(
            shown.take_answer(),
            None,
            "the form does not read whole yet: the app has not answered the edit"
        );

        shown.update(&moment_form(true), &mut sheet, &mut reconciler);
        assert_eq!(
            shown.take_answer(),
            Some(ToApp::FormSubmit { form: 7 }),
            "the pick submits once the form reads whole"
        );
        assert_eq!(shown.take_answer(), None, "a form answers once");
    }

    /// A form asking more than one thing is submitted by its button: a pick answers one field,
    /// not the whole form.
    #[test]
    fn a_pick_does_not_answer_a_form_asking_more_than_one_thing() {
        let mut asked = moment_form(false);
        asked.answers.clear();
        let mut reconciler = Reconciler::new(Rc::default());
        let (mut shown, mut sheet) = Shown::build(&asked, &mut reconciler);
        let pick = UiEvent::Fire(
            "form~when~quick".to_owned(),
            "pick.2026-11-08T17:00:00Z".to_owned(),
            None,
        );
        assert!(
            shown.edit(&pick).is_some(),
            "the pick still reaches the app"
        );
        let mut read = moment_form(true);
        read.answers.clear();
        shown.update(&read, &mut sheet, &mut reconciler);
        assert_eq!(
            shown.take_answer(),
            None,
            "a form with a field left to fill waits for its submit"
        );
    }

    /// The footer's buttons, as a reader reaches them: the dialog's own actions, in the order
    /// they stand.
    fn footer_buttons(sheet: &Sheet) -> Vec<String> {
        let dialog = sheet
            .accessibility()
            .expect("the open sheet is in the tree");
        dialog
            .children
            .iter()
            .filter(|child| child.role == ui::a11y::Role::Button)
            .filter_map(|child| child.name.clone())
            .collect()
    }

    /// A form its moments answer on one pick offers Cancel alone: the submit button stands
    /// there only once the person is on the way in by hand, typing a moment or picking a day.
    #[test]
    fn the_confirm_button_stands_only_on_the_way_in_by_hand() {
        let mut reconciler = Reconciler::new(Rc::default());
        let (mut shown, mut sheet) = Shown::build(&moment_form(false), &mut reconciler);
        assert_eq!(
            footer_buttons(&sheet),
            vec!["Cancel".to_owned()],
            "one pick answers the form: nothing is left to confirm"
        );

        let typed = wire::Form {
            confirm: true,
            ..moment_form(true)
        };
        shown.update(&typed, &mut sheet, &mut reconciler);
        assert_eq!(
            footer_buttons(&sheet),
            vec!["Remind me".to_owned(), "Cancel".to_owned()],
            "a moment typed or picked by hand is confirmed"
        );

        shown.update(&moment_form(true), &mut sheet, &mut reconciler);
        assert_eq!(
            footer_buttons(&sheet),
            vec!["Cancel".to_owned()],
            "clearing the entry takes the confirm button away"
        );
    }
}
