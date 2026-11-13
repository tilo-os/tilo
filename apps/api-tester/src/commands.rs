//! What the widgets ask the app to do: each pushes a [`Cmd`] onto the shared [`Sink`], and
//! the app applies the queue after the event has routed, where it has the document.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::ActionControl;
use ui::tilo_resource::{Edit, Path};
use vello::kurbo::{Point, Rect};

use crate::model::Id;
use crate::resource::Workspace;

#[derive(Clone)]
pub(crate) struct Sink {
    queue: Rc<RefCell<Vec<Cmd>>>,
    pub(crate) dispatcher: Rc<RefCell<Option<ui::framework::Dispatcher<Workspace>>>>,
    pub(crate) actions: Rc<RefCell<Option<ui::framework::Actions<Workspace>>>>,
    pub(crate) method: ui::LocalState<String>,
    pub(crate) url: Rc<RefCell<String>>,
    pub(crate) reset_area: ui::LocalState<bool>,
    /// Whether the person moved the divider between the request and the response.
    pub(crate) area_resized: ui::LocalState<bool>,
}
impl Sink {
    pub(crate) fn borrow(&self) -> std::cell::Ref<'_, Vec<Cmd>> {
        self.queue.borrow()
    }
    pub(crate) fn borrow_mut(&self) -> std::cell::RefMut<'_, Vec<Cmd>> {
        self.queue.borrow_mut()
    }
    /// `control` bound to the field at `path`: each committed value becomes the field edit
    /// `edit(path, value)`. Unavailable until the window is attached.
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
    /// Bind `effect` to `control`, so a copy or a cancel is the same action the palette and
    /// an agent invoke.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Table {
    Params,
    Headers,
    Form,
}

impl Table {
    pub(crate) fn rows(self) -> Path<Workspace, ui::tilo_resource::Collection<crate::record::Row>> {
        let request = Workspace::paths().request;
        match self {
            Table::Params => request.params,
            Table::Headers => request.headers,
            Table::Form => request.form,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowEdit {
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthField {
    Token,
    Username,
    Password,
    Header,
    Value,
}

#[derive(Clone, Debug)]
pub(crate) enum Cmd {
    SetTab(usize),
    SetView(usize),
    SetBodyMode(usize),
    SetAuthKind(usize),
    LoadRequest(usize, usize),
    /// Open the sent request at `index` in the builder again.
    ReplayHistory(usize),
    /// Keep the sent request at `index` as a saved request: the sheet names it and says
    /// where it goes.
    SaveHistory(usize),
    /// Forget the sent request at `index`; Undo brings it back.
    DeleteHistory(usize),
    /// Show the actions of the sidebar row `id`, under its `lift`, at `at`.
    RowMenu {
        id: String,
        lift: Rect,
        at: Point,
    },
    Edit {
        table: Table,
        row: String,
        edit: RowEdit,
    },
    PickEnvironment(String),
    OpenForm(FormMode, FormTarget),
    DeleteRequest(FormTarget),
    SubmitForm,
    CancelForm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormMode {
    NewCollection,
    SaveRequest,
    SaveSent,
    RenameCollection,
    RenameRequest,
    NewEnvironment,
    RenameEnvironment,
    NewVariable,
    DeleteVariable,
}

impl FormMode {
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::NewCollection => "New collection",
            Self::SaveRequest | Self::SaveSent => "Save request",
            Self::RenameCollection => "Rename collection",
            Self::RenameRequest => "Rename request",
            Self::NewEnvironment => "New environment",
            Self::RenameEnvironment => "Rename environment",
            Self::NewVariable => "Add variable",
            Self::DeleteVariable => "Delete variable",
        }
    }

    pub(crate) fn confirm_label(self) -> &'static str {
        match self {
            Self::NewCollection | Self::NewEnvironment => "Create",
            Self::SaveRequest | Self::SaveSent => "Save",
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

/// What a dialog acts on, when it is not the active selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FormTarget {
    pub(crate) collection: Option<Id>,
    /// The sent request a form acts on.
    pub(crate) sent: Option<Id>,
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
