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
use tilo_actions::remote::{ActionHostError, InvokeArgument};
use tilo_reference::ResourceRef;
use ui_core::widget::ChipLead;
use ui_core::{Arrival, InvocationCall, RemoteActions};

/// Ask the host to open what `href`, a `tilo://open#…` address, names: the
/// app that keeps that resource shows it, launched first when no window
/// shows it. An address that names no app of this desktop is refused.
///
/// # Errors
/// Why it cannot be asked: the address names no app, or the window is in no
/// workspace.
pub(crate) fn open_link(opener: &Opener, href: &str) -> Result<(), String> {
    let link = tilo_reference::link::Link::parse_native(href).map_err(|error| error.to_string())?;
    let address = link
        .target
        .resource
        .ok_or_else(|| "The link names no resource".to_owned())?;
    let app = match &link.target.record {
        Some(_) => link.target.app.as_ref().map(|app| app.as_str().to_owned()),
        None => ui_core::app_surface::app_of_kind(&address.kind),
    }
    .ok_or_else(|| "No app of this desktop keeps what the link names".to_owned())?;
    // `-` is the app's own record of the workspace; any other id names it.
    let record = link
        .target
        .record
        .as_ref()
        .map(tilo_reference::link::RecordSegment::provider_part)
        .filter(|id| !id.is_empty());
    opener.open(&app, record, &address.into_ref(String::new()))
}

/// The action a refused open is told as.
pub(crate) const OPEN: &str = tilo_actions::remote::APPLICATION_OPEN_ACTION;

/// What leads a reference's label on its chip.
pub const LEAD: ChipLead = ChipLead::Glyph(ui_core::icons::Icon::Link);

/// The workspace this window is in, or why nothing opens from it.
pub fn room() -> Result<String, String> {
    // A window hosted among others in one process (the browser) is in the
    // workspace its host put it in, not the process's.
    if let Some(room) = ui_core::window_toolbar::workspace() {
        return Ok(room);
    }
    let room = ui_core::collaboration::local_cluster_id();
    if room.is_nil() {
        Err("This window is in no workspace".into())
    } else {
        Ok(room.to_string())
    }
}

/// A view's opens, and the actions of its own record it sends through the
/// host ([`Dispatcher::through_host`](super::Dispatcher::through_host)), on
/// their own client of the session's action door, which the window's root
/// carries to the host.
#[derive(Clone, Default)]
pub(crate) struct Opener(Rc<RefCell<Opening>>);

#[derive(Default)]
struct Opening {
    door: Option<RemoteActions>,
    /// Each call, by the action it invokes.
    calls: Vec<(String, InvocationCall)>,
    /// The reads views follow ([`super::query::Query`]), each kept while
    /// the scope that asked for it lives.
    reads: Vec<(std::rc::Weak<()>, Rc<dyn std::any::Any>)>,
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
        self.invoke(target, InvokeArgument::None);
        Ok(())
    }

    /// Ask the host to invoke `address`, an action of the window's own
    /// record, with `arguments`, as the person using it.
    pub(crate) fn invoke_own(
        &self,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        self.invoke(
            tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::OWN_PROVIDER.into(),
                address,
            },
            InvokeArgument::Json(arguments),
        );
    }

    pub(crate) fn invoke(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        argument: InvokeArgument,
    ) {
        let action = target.address.action.clone();
        let mut opening = self.0.borrow_mut();
        let call = opening
            .door
            .get_or_insert_with(RemoteActions::current)
            .invoke(target, argument, None);
        opening.calls.push((action, call));
    }

    /// Follow the result of the action at `target` on this view's door,
    /// telling `then` each update ([`RemoteActions::follow_with`]).
    pub(crate) fn follow(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        argument: InvokeArgument,
        then: impl FnMut(&Arrival<InvocationUpdate, ActionHostError>) + Send + 'static,
    ) -> ui_core::Followed {
        self.0
            .borrow_mut()
            .door
            .get_or_insert_with(RemoteActions::current)
            .follow_with(target, argument, then)
    }

    /// Keep `read` while `alive` lives.
    pub(crate) fn keep(&self, alive: std::rc::Weak<()>, read: Rc<dyn std::any::Any>) {
        self.0.borrow_mut().reads.push((alive, read));
    }

    /// Why each call that ended since the last call did not go ahead, by
    /// the action it invoked.
    pub(crate) fn take_refusals(&self) -> Vec<(String, String)> {
        let mut refusals = Vec::new();
        self.0.borrow_mut().calls.retain(|(action, call)| {
            call.with(|call| match call {
                Arrival::Pending { .. }
                | Arrival::Ready(
                    InvocationUpdate::Accepted { .. }
                    | InvocationUpdate::Progress { .. }
                    | InvocationUpdate::Item { .. },
                ) => true,
                Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                    refusals.push((action.clone(), error.message.clone()));
                    false
                }
                Arrival::Failed(error) => {
                    refusals.push((action.clone(), error.to_string()));
                    false
                }
                Arrival::Ready(_) => false,
            })
        });
        refusals
    }

    /// The client, once anything was opened: the root carries its requests
    /// and answers.
    pub(crate) fn door(&self) -> Option<RemoteActions> {
        // A read whose scope is gone stops here, before the door carries
        // its cancellation.
        let gone: Vec<_> = {
            let mut opening = self.0.borrow_mut();
            let (kept, gone) = std::mem::take(&mut opening.reads)
                .into_iter()
                .partition(|(alive, _)| alive.strong_count() > 0);
            opening.reads = kept;
            gone
        };
        drop(gone);
        self.0.borrow().door.clone()
    }
}
