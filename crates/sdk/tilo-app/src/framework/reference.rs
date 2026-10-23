//! Opening a reference from any app.
//!
//! A [`ResourceRef`] names something another app keeps: a chat message's
//! page, an issue's link, what "Tell an agent" was asked about. Opening it
//! is one action, the resource's `open` in its own app
//! ([`ResourceRef::open_target`]), sent through the session's action door
//! as the person who asked: the host launches the app on the record when no
//! window shows it and admits the open under that person's grants. A chip
//! ([`ViewCx::reference`](super::ViewCx::reference), or [`LEAD`] on a
//! painted [`ChipFace`](ui_core::widget::ChipFace)) is how a reference shows.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::InvokeArgument;
use tilo_reference::ResourceRef;
use ui_core::widget::ChipLead;
use ui_core::{InvocationCall, LocalState, RemoteActions, RemoteCall};

/// The action a refused open is told as.
pub(crate) const OPEN: &str = tilo_actions::remote::APPLICATION_OPEN_ACTION;

/// What leads a reference's label on its chip.
pub const LEAD: ChipLead = ChipLead::Glyph(ui_core::icons::Icon::Link);

/// The workspace this window is in, or why nothing opens from it.
pub fn room() -> Result<String, String> {
    let room = ui_core::collaboration::local_cluster_id();
    if room.is_nil() {
        Err("This window is in no workspace".into())
    } else {
        Ok(room.to_string())
    }
}

/// A view's opens, on their own client of the session's action door, which
/// the window's root carries to the host.
#[derive(Clone, Default)]
pub(crate) struct Opener(Rc<RefCell<Opening>>);

#[derive(Default)]
struct Opening {
    door: Option<RemoteActions>,
    calls: Vec<LocalState<InvocationCall>>,
}

impl Opener {
    /// Ask the host to open `reference`, which `app` keeps in `record`.
    ///
    /// # Errors
    /// Why it cannot be asked: the window is in no workspace.
    pub(crate) fn open(
        &self,
        app: &str,
        record: Option<&str>,
        reference: &ResourceRef,
    ) -> Result<(), String> {
        let target = reference.open_target(&room()?, app, record);
        let mut opening = self.0.borrow_mut();
        let call = opening
            .door
            .get_or_insert_with(RemoteActions::current)
            .invoke(target, InvokeArgument::None, None);
        opening.calls.push(call);
        Ok(())
    }

    /// Why each open that ended since the last call did not open.
    pub(crate) fn take_refusals(&self) -> Vec<String> {
        let mut refusals = Vec::new();
        self.0.borrow_mut().calls.retain(|call| {
            call.with(|call| match call {
                RemoteCall::Pending
                | RemoteCall::Done(
                    InvocationUpdate::Accepted { .. }
                    | InvocationUpdate::Progress { .. }
                    | InvocationUpdate::Item { .. },
                ) => true,
                RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => {
                    refusals.push(error.message.clone());
                    false
                }
                RemoteCall::Refused(error) => {
                    refusals.push(error.to_string());
                    false
                }
                RemoteCall::Done(_) => false,
            })
        });
        refusals
    }

    /// The client, once anything was opened: the root carries its requests
    /// and answers.
    pub(crate) fn door(&self) -> Option<RemoteActions> {
        self.0.borrow().door.clone()
    }
}
