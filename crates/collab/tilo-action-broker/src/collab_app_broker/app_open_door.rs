//! What an application asks on its own connection: who it is decided as.
//!
//! A request the app makes while running an invocation the broker delivered
//! to it (`on_behalf_of`) continues that invocation: an edit its effect
//! submits, or another app's published action its effect calls. It is
//! decided as that invocation's caller (the person at one of the host's own
//! doors: the palette, search, a Shortcut, the control socket; an agent; or
//! another member), by their own grants, and admitted under the custody the
//! broker kept for them, so the child's history names them and their undo
//! takes it back. The app's own identity adds nothing, and its own grants
//! are not used. It reaches no further than that invocation was admitted
//! to ([`tilo_action_policy::Reach`]): a reading invocation's edit, say, is
//! put to the person as its own request first.
//!
//! An invocation the app made of its own action through this same door (a
//! button in its window runs its published action) speaks for the app, not
//! for the person: a request continuing it is decided as the app's own,
//! as if it named no invocation.
//!
//! Opening an application (another app by its workspace `open`
//! (`ApplicationLaunch`), a resource in its app (`application:<room>:<app>`
//! `open`), or an installed app through the published-application launch the
//! Store uses) is no one's just because the app sent it. It is decided as
//! the caller of the invocation it continues, as above, or as the person
//! using the app, when their own press reached one of its windows
//! ([`crate::services::LauncherServices::person_initiated`]: a click, a key
//! or a tap the compositor delivered, never input an agent or a remote
//! participant sends through the shared seat), which this open uses up;
//! else it is refused.

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller,
    InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionMessage, application_provider,
    workspace_provider_room,
};

use super::{AppBrokerState, InvocationCaller};

/// What lets an application's connection open an application.
pub(super) enum OpenAuthority {
    /// The running invocation the open continues.
    Invocation(Box<Continued>),
    /// The person's own press in one of the app's windows, now used up.
    Press,
}

/// The running invocation a request continues: decided as its caller, with
/// that invocation's own custody.
pub(super) struct Continued {
    caller: Caller,
    original: Option<std::sync::Arc<super::OriginalInvocationData>>,
}

/// Who an app's request is decided as, and the custody it is admitted
/// under when that is not the app's connection's own.
pub(super) type RequestCaller = (Caller, Option<InvocationCaller>);

/// What a request names as the invocation it continues.
pub(super) enum Continuing {
    /// Someone else's invocation, delivered to this connection and still
    /// running: the request is decided as its caller, under its custody.
    Invocation(Box<Continued>),
    /// An invocation the app made of its own action through its own door:
    /// nobody's authority but the app's.
    OwnDoor,
}

impl AppBrokerState {
    /// Who application `app_id`'s `message` on connection `peer_id` is
    /// decided as, and the custody it is admitted under, when it opens an
    /// application or continues an invocation; `None` for a request the
    /// app's own door decides. `person` is who admitted the connection.
    pub(super) fn app_request_caller(
        &self,
        peer_id: u64,
        app_id: &str,
        person: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<Result<RequestCaller, Box<RemoteActionMessage>>> {
        let RemoteActionMessage::Invoke(invoke) = message else {
            return None;
        };
        let refused = || {
            Box::new(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: invoke.invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionHostError::NotPermitted,
                },
            }))
        };
        if opens_application(invoke) {
            return Some(
                match self.app_open_authority(peer_id, app_id, invoke.on_behalf_of) {
                    Some(OpenAuthority::Invocation(continued)) => {
                        Ok(continued.child(invoke.invocation))
                    }
                    Some(OpenAuthority::Press) => Ok((person.clone(), None)),
                    None => {
                        tracing::info!(app = %app_id, target = ?invoke.target, "app's open with nothing behind it refused");
                        Err(refused())
                    }
                },
            );
        }
        let parent = invoke.on_behalf_of?;
        // A child is an invocation of its own: reusing the one it continues
        // (or none) would make the two one at-most-once entry.
        if invoke.invocation == parent || invoke.invocation.0.is_nil() {
            return Some(Err(refused()));
        }
        match self.continuing(peer_id, app_id, parent) {
            Some(Continuing::Invocation(continued)) => Some(Ok(continued.child(invoke.invocation))),
            Some(Continuing::OwnDoor) => None,
            None => {
                tracing::info!(app = %app_id, target = ?invoke.target, "app's request continuing nothing running refused");
                Some(Err(refused()))
            }
        }
    }

    /// What `parent`, an invocation application `app_id`'s connection
    /// `peer_id` names, is: one the broker delivered to it that still runs,
    /// for someone else or for the app itself. `None` when it runs no such
    /// invocation, two share the id, or its caller no longer stands.
    pub(super) fn continuing(
        &self,
        peer_id: u64,
        app_id: &str,
        parent: tilo_actions::InvocationId,
    ) -> Option<Continuing> {
        let mut running = self
            .pending_invocations
            .lock()
            .ok()?
            .values()
            .filter(|entry| {
                entry.peer_id == peer_id
                    && entry.update.invocation() == parent
                    && !entry.update.is_terminal()
            })
            .map(|entry| entry.original.clone())
            .take(2)
            .collect::<Vec<_>>();
        let original = running.pop().filter(|_| running.is_empty())?;
        if original
            .as_ref()
            .is_some_and(|original| original.through_door_of(app_id))
        {
            return Some(Continuing::OwnDoor);
        }
        self.child_caller(peer_id, parent)
            .map(|caller| Continuing::Invocation(Box::new(Continued { caller, original })))
    }

    /// What lets application `app_id`, on connection `peer_id`, open an
    /// application for `parent`, the invocation it names, if any: that
    /// invocation's caller when the broker delivered it to this connection
    /// and it is still running (unless the app invoked it itself), else the
    /// person's press in one of the app's windows. `None` when neither is
    /// behind it.
    pub(super) fn app_open_authority(
        &self,
        peer_id: u64,
        app_id: &str,
        parent: Option<tilo_actions::InvocationId>,
    ) -> Option<OpenAuthority> {
        if let Some(Continuing::Invocation(continued)) =
            parent.and_then(|parent| self.continuing(peer_id, app_id, parent))
        {
            return Some(OpenAuthority::Invocation(continued));
        }
        self.services
            .launcher
            .as_ref()
            .is_some_and(|launcher| launcher.person_initiated(app_id))
            .then_some(OpenAuthority::Press)
    }
}

impl Continued {
    /// The caller a child request `child` of this invocation is decided
    /// as, and the custody kept for them it is admitted under.
    fn child(self, child: tilo_actions::InvocationId) -> RequestCaller {
        let Self { caller, original } = self;
        (
            caller,
            original.map(|original| original.launch_child_caller(child)),
        )
    }
}

/// Whether `invoke` opens an application: another app's workspace `open`,
/// or a resource's `open` in its app.
fn opens_application(invoke: &InvokeRequest) -> bool {
    let target = &invoke.target;
    target.address.action == APPLICATION_OPEN_ACTION
        && ((workspace_provider_room(&target.provider).is_some()
            && target
                .address
                .resource
                .starts_with(APPLICATION_RESOURCE_PREFIX))
            || application_provider(&target.provider).is_some())
}
