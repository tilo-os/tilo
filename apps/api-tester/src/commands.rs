//! What the widgets ask the app to do.
//!
//! Buttons, rows, tables and dialogs are built from the model and know
//! nothing about it: each pushes a [`Cmd`] onto the shared [`Sink`], and the
//! app applies the queue after the event has finished routing, where it has
//! the document.

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::Id;

/// The queue every widget callback pushes onto.
pub(crate) type Sink = Rc<RefCell<Vec<Cmd>>>;

/// The key/value tables of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Table {
    Params,
    Headers,
    Form,
}

/// An edit to one row of a [`Table`], by durable row id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowEdit {
    Key(String),
    Value(String),
    Enabled(bool),
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
    EditAuth(AuthField, String),
    PickEnvironment(String),
    OpenForm(FormMode, FormTarget),
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
    DeleteRequest,
    NewEnvironment,
    RenameEnvironment,
    DeleteEnvironment,
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
            Self::DeleteRequest => "Delete this request?",
            Self::NewEnvironment => "New environment",
            Self::RenameEnvironment => "Rename environment",
            Self::DeleteEnvironment => "Delete this environment?",
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
            Self::DeleteRequest => "Delete request",
            Self::DeleteEnvironment => "Delete environment",
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
            && !matches!(self, Self::DeleteRequest | Self::DeleteEnvironment)
    }

    pub(crate) fn is_destructive(self) -> bool {
        matches!(
            self,
            Self::DeleteRequest | Self::DeleteEnvironment | Self::DeleteVariable
        )
    }
}

/// What a dialog acts on, when it is not the active selection: the row a
/// hover action was pressed on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FormTarget {
    pub(crate) collection: Option<Id>,
    pub(crate) request: Option<Id>,
}
