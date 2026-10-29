//! An action's form, as the window presents it (forms-and-diagrams.md §1).
//!
//! The app sends the form of an action waiting for its arguments ([`wire::Form`]): its fields as
//! a tree of the same nodes views send, built and reconciled by the window's one reconciler, so
//! a field keeps its widget, focus and caret while the app answers each edit. The window adds
//! what every form has: the title, the submit button (disabled while an argument does not read)
//! and Cancel, and the presentation. Only this window waits; the rest of the workspace does not.
//!
//! What the person does in a field reaches the app as an edit of that field
//! ([`ToApp::FormEdit`]); the app keeps it as their lens state and answers with the form as it
//! reads now.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as wire, ToApp};
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
    answers: Rc<Answers>,
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
        let sheet = ModalDialog::new(form.title.clone(), Pane::new(fields))
            .with_test_id(format!("{KEY}sheet"))
            .with_destructive(form.destructive)
            .primary_action(form.submit.clone(), move || submit.submit.set(true))
            .secondary_action(crate::i18n::t("tilolang-form-cancel"), move || {
                cancel.cancel.set(true);
            })
            .primary_enabled_if(move |_| gate.get())
            .with_foot_sheet()
            .open(true);
        (
            Self {
                id: form.id,
                tree,
                ready,
                answers,
            },
            sheet,
        )
    }

    /// The form as the app sends it now, shown in `sheet`: its fields reconciled in place, so
    /// what the person is typing keeps its field.
    pub fn update(&mut self, form: &wire::Form, sheet: &mut Sheet, reconciler: &mut Reconciler) {
        let tree = tree_of(form);
        let pane = sheet.content_mut();
        if let Some(replaced) = reconciler.update_in(pane.content_mut(), &self.tree, &tree) {
            pane.replace(replaced);
        }
        sheet.invalidate_layout();
        self.tree = tree;
        self.ready.set(form.ready);
    }

    /// What the person answered since this was last asked: submit, or cancel (Escape, or the
    /// Cancel button).
    pub fn take_answer(&self) -> Option<ToApp> {
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
        let edit = ToApp::FormEdit {
            form: self.id,
            field,
            text,
        };
        matches!(&edit, ToApp::FormEdit { field, .. } if field.starts_with(KEY)).then_some(edit)
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
