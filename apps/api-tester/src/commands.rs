//! What the widgets ask the app to do.
//!
//! Buttons, rows, tables and dialogs are built from the model and know
//! nothing about it: each pushes a [`Cmd`] onto the shared [`Sink`], and the
//! app applies the queue after the event has finished routing, where it has
//! the document.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::ActionControl;
use ui::tilo_resource::{Edit, Path};

use crate::model::Id;
use crate::resource::Workspace;

/// The queue every widget callback pushes onto.
#[derive(Clone)]
pub(crate) struct Sink {
    queue: Rc<RefCell<Vec<Cmd>>>,
    pub(crate) dispatcher: Rc<RefCell<Option<ui::framework::Dispatcher<Workspace>>>>,
    pub(crate) actions: Rc<RefCell<Option<ui::framework::Actions<Workspace>>>>,
    pub(crate) method: ui::LocalState<String>,
    /// The builder's URL as the record holds it, for the variable hint.
    pub(crate) url: Rc<RefCell<String>>,
    pub(crate) reset_area: ui::LocalState<bool>,
    /// Whether the person moved the divider between the request and the
    /// response: only then does resetting the area change anything.
    pub(crate) area_resized: ui::LocalState<bool>,
}
impl Sink {
    pub(crate) fn borrow(&self) -> std::cell::Ref<'_, Vec<Cmd>> {
        self.queue.borrow()
    }
    pub(crate) fn borrow_mut(&self) -> std::cell::RefMut<'_, Vec<Cmd>> {
        self.queue.borrow_mut()
    }
    /// `control` bound to the field at `path`: each committed value becomes
    /// the field edit `edit(path, value)`. Unavailable until the window is
    /// attached, and for a field nobody may edit.
    pub(crate) fn bind_field<C: ActionControl<V>, V: 'static>(
        &self,
        control: C,
        path: Path<Workspace, V>,
        edit: impl Fn(&Path<Workspace, V>, V) -> Edit<Workspace> + Send + Sync + 'static,
    ) -> C {
        match self
            .actions
            .borrow()
            .as_ref()
            .and_then(|actions| actions.edit_binding(path, edit))
        {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    pub(crate) fn retry(&self, state: ui::widget::EmptyState) -> ui::widget::EmptyState {
        match self.actions.borrow().as_ref() {
            Some(actions) => {
                actions.bind_effect_invocation(state, &crate::actions::ApiTesterEffect::Send)
            }
            None => state.unavailable(),
        }
    }
    /// Bind `effect` to `control`, so a copy or a cancel is the same action
    /// the palette and an agent invoke.
    pub(crate) fn effect<V: ActionControl<()>>(
        &self,
        control: V,
        effect: &crate::actions::ApiTesterEffect,
    ) -> V {
        match self.actions.borrow().as_ref() {
            Some(actions) => actions.bind_effect_invocation(control, effect),
            None => control.unavailable(),
        }
    }

    pub(crate) fn send(&self, button: ui::widget::Button) -> ui::widget::Button {
        match self.actions.borrow().as_ref() {
            Some(actions) => {
                actions.bind_effect_invocation(button, &crate::actions::ApiTesterEffect::Send)
            }
            None => button.with_enabled(false),
        }
    }
}

/// The key/value tables of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Table {
    Params,
    Headers,
    Form,
}

impl Table {
    /// The path of the table's rows in the builder's request.
    pub(crate) fn rows(self) -> Path<Workspace, ui::tilo_resource::Collection<crate::record::Row>> {
        let request = Workspace::paths().request;
        match self {
            Table::Params => request.params,
            Table::Headers => request.headers,
            Table::Form => request.form,
        }
    }
}

/// An edit to one row of a [`Table`], by durable row id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowEdit {
    Remove,
}

/// One field of the request's authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthField {
    Token,
    Username,
    Password,
    Header,
    Value,
}

/// A user action collected from a widget callback.
#[derive(Clone, Debug)]
pub(crate) enum Cmd {
    SetTab(usize),
    SetView(usize),
    SetBodyMode(usize),
    SetAuthKind(usize),
    LoadRequest(usize, usize),
    ReplayHistory(usize),
    Edit {
        table: Table,
        row: String,
        edit: RowEdit,
    },
    PickEnvironment(String),
    OpenForm(FormMode, FormTarget),
    /// Delete a saved request, once the person confirms what the host asks.
    DeleteRequest(FormTarget),
    SubmitForm,
    CancelForm,
}

/// The durable actions a dialog collects a name or a confirmation for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormMode {
    NewCollection,
    SaveRequest,
    RenameCollection,
    RenameRequest,
    NewEnvironment,
    RenameEnvironment,
    NewVariable,
    DeleteVariable,
}

impl FormMode {
    /// The dialog's title: what is being decided.
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::NewCollection => "New collection",
            Self::SaveRequest => "Save request",
            Self::RenameCollection => "Rename collection",
            Self::RenameRequest => "Rename request",
            Self::NewEnvironment => "New environment",
            Self::RenameEnvironment => "Rename environment",
            Self::NewVariable => "Add variable",
            Self::DeleteVariable => "Delete variable",
        }
    }

    /// The primary action names what happens.
    pub(crate) fn confirm_label(self) -> &'static str {
        match self {
            Self::NewCollection | Self::NewEnvironment => "Create",
            Self::SaveRequest => "Save",
            Self::RenameCollection | Self::RenameRequest | Self::RenameEnvironment => "Rename",
            Self::NewVariable => "Add variable",
            Self::DeleteVariable => "Delete variable",
        }
    }

    pub(crate) fn needs_variable_fields(self) -> bool {
        matches!(self, Self::NewVariable | Self::DeleteVariable)
    }

    pub(crate) fn needs_value(self) -> bool {
        matches!(self, Self::NewVariable)
    }

    pub(crate) fn needs_name(self) -> bool {
        !self.needs_variable_fields()
    }

    pub(crate) fn is_destructive(self) -> bool {
        matches!(self, Self::DeleteVariable)
    }
}

/// What a dialog acts on, when it is not the active selection: the row a
/// hover action was pressed on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FormTarget {
    pub(crate) collection: Option<Id>,
    pub(crate) request: Option<Id>,
}

impl Default for Sink {
    fn default() -> Self {
        Self {
            queue: Rc::default(),
            dispatcher: Rc::default(),
            actions: Rc::default(),
            method: ui::LocalState::new(String::new()),
            url: Rc::default(),
            reset_area: ui::LocalState::new(false),
            area_resized: ui::LocalState::new(false),
        }
    }
}
