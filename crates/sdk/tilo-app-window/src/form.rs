//! An action's form, as the window presents it (forms-and-diagrams.md §1). The app sends the form
//! of an action waiting for its arguments ([`wire::Form`]): its fields as a tree of the same nodes
//! views send, built and reconciled by the window's one reconciler, so a field keeps its widget,
//! focus and caret while the app answers each edit. The window adds the title, the submit button
//! (disabled while an argument does not read), Cancel and the presentation; only this window waits,
//! not the rest of the workspace. What the person does in a field reaches the app as an edit of
//! that field ([`ToApp::FormEdit`]); the app keeps it as their view state and answers with the form
//! as it reads now.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as wire, ToApp};
use ui::widget::modal_dialog::DialogAction;
use ui::widget::{ModalDialog, Widget};

use crate::pane::Pane;
use crate::reconcile::{Reconciler, UiEvent};
use crate::{UiNode, Value};

/// The key prefix of a form's nodes, apart from the views' keys.
pub const KEY: &str = "form~";

const ERROR: &str = "form~~error";

#[derive(Default)]
pub struct Answers {
    submit: Cell<bool>,
    cancel: Cell<bool>,
}

pub struct Shown {
    pub id: u64,
    tree: UiNode,
    /// Whether submit can be taken, as the app last said.
    ready: Rc<Cell<bool>>,
    /// The controls whose choice answers the form by itself, by key.
    completes: Vec<String>,
    /// A quick choice was made and the form does not read whole yet.
    picked: Cell<bool>,
    answers: Rc<Answers>,
    submit: String,
    /// Whether the footer offers the submit button: a form its quick choices answer shows Cancel
    /// alone until the person types or picks by hand.
    confirm: bool,
}

/// The dialog the window mounts: the fields, in a pane the reconciler reaches again.
pub type Sheet = ModalDialog<Pane>;

impl Shown {
    pub fn build(form: &wire::Form, reconciler: &mut Reconciler) -> (Self, Sheet) {
        let tree = tree_of(form);
        let fields = reconciler.build(&tree);
        for (key, text) in &form.fields {
            reconciler.set_text(key, text);
            reconciler.set_flag(key, text == "true");
        }
        let ready = Rc::new(Cell::new(form.ready));
        let answers = Rc::new(Answers::default());
        let check = Rc::clone(&ready);
        let submit = Rc::clone(&answers);
        let cancel = Rc::clone(&answers);
        let mut sheet = ModalDialog::new(form.title.clone(), Pane::new(fields))
            .with_test_id(format!("{KEY}sheet"))
            .with_destructive(form.destructive)
            .primary_action(form.submit.clone(), move || submit.submit.set(true))
            .secondary_action(crate::i18n::t("tilolang-form-cancel"), move || {
                cancel.cancel.set(true);
            })
            .primary_enabled_if(move |_| check.get())
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

    /// A form a moment answers on one pick keeps the submit button for when the person types or
    /// picks a moment instead.
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

    pub fn update(&mut self, form: &wire::Form, sheet: &mut Sheet, reconciler: &mut Reconciler) {
        let tree = tree_of(form);
        let pane = sheet.content_mut();
        if let Some(replaced) = reconciler.update_in(pane.content_mut(), &self.tree, &tree) {
            pane.replace(replaced);
        }
        // A choice or the calendar set a control the person has not touched: the field shows what
        // the app read.
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

    pub fn take_answer(&self) -> Option<ToApp> {
        // A quick choice that says everything the form asks is the answer as soon as the form reads
        // whole, with no second press.
        if self.picked.get() && self.ready.get() {
            self.picked.set(false);
            return Some(ToApp::FormSubmit { form: self.id });
        }
        // A submit pressed while the app has not read the field it typed yet waits for it to: the
        // form it answered reads whole a frame later, and the press is the person's, taken then.
        if self.answers.submit.get() && self.ready.get() {
            self.answers.submit.set(false);
            return Some(ToApp::FormSubmit { form: self.id });
        }
        if self.answers.cancel.take() {
            return Some(ToApp::FormCancel { form: self.id });
        }
        None
    }

    #[must_use]
    pub fn edit(&self, event: &UiEvent) -> Option<ToApp> {
        let (field, text) = match event {
            UiEvent::Edit(key, text) => (key.clone(), text.clone()),
            UiEvent::Toggle(key, checked) => (key.clone(), checked.to_string()),
            UiEvent::Fire(key, event, _, _) => {
                (key.clone(), event.strip_prefix("pick.")?.to_owned())
            }
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

/// A form a popover holds: at most two short fields, shown beside the control that ran its action
/// while that control is still on screen.
#[must_use]
pub fn fits_a_popover(form: &wire::Form) -> bool {
    form.short && form.fields.len() <= 2
}

#[must_use]
pub fn in_form(event: &UiEvent) -> bool {
    match event {
        UiEvent::Edit(key, _)
        | UiEvent::Toggle(key, _)
        | UiEvent::Fire(key, _, _, _)
        | UiEvent::Press(key) => key.starts_with(KEY),
        _ => false,
    }
}

/// The app's fields, with why the action refused the last submit under them.
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

pub fn sheet_of(widget: &mut dyn Widget) -> Option<&mut Sheet> {
    widget.as_any_mut()?.downcast_mut::<Sheet>()
}
