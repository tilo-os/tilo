//! An application asking, on its own connection, to open an application:
//! another app by its workspace `open` (`ApplicationLaunch`), a resource in
//! its app (`application:<room>:<app>` `open`), or an installed app through
//! the published-application launch the Store uses.
//!
//! Such a request is no one's just because the app sent it. It is decided as
//! one of two callers, else refused:
//!
//! - the caller of the running invocation the request continues
//!   (`on_behalf_of`), which the broker delivered to this connection: the
//!   person at one of the host's own doors (the palette, search, a Shortcut,
//!   the control socket), an agent, or another member, each by their own
//!   grants;
//! - the person using the app, when their own press reached one of its
//!   windows ([`crate::services::LauncherServices::person_initiated`]: a
//!   click, a key or a tap the compositor delivered, never input an agent
//!   or a remote participant sends through the shared seat), which this
//!   open uses up.
//!
//! An invocation the app made of its own action through this same door
//! (a button in its window runs its published action) speaks for the app,
//! not for the person: an open made for it needs the person's press too.

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller,
    InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionMessage, application_provider,
    workspace_provider_room,
};

use super::{CapnpBrokerState, InvocationCaller};

/// What lets an application's connection open an application.
pub(super) enum OpenAuthority {
    /// The running invocation the open continues.
    Invocation(Box<Continued>),
    /// The person's own press in one of the app's windows, now used up.
    Press,
}

/// The running invocation an open continues: decided as its caller, with
/// that invocation's own custody.
pub(super) struct Continued {
    caller: Caller,
    original: Option<std::sync::Arc<super::OriginalInvocationData>>,
}

/// Who an app's open is decided as, and the custody it is admitted under
/// when that is not the app's connection's own.
pub(super) type OpenCaller = (Caller, Option<InvocationCaller>);

impl CapnpBrokerState {
    /// Who application `app_id`'s `message` on connection `peer_id` is
    /// decided as, when it opens an application; `None` for any other
    /// message. `person` is who admitted the connection.
    pub(super) fn app_open_caller(
        &self,
        peer_id: u64,
        app_id: &str,
        person: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<Result<OpenCaller, Box<RemoteActionMessage>>> {
        let RemoteActionMessage::Invoke(invoke) = message else {
            return None;
        };
        if !opens_application(invoke) {
            return None;
        }
        Some(
            match self.app_open_authority(peer_id, app_id, invoke.on_behalf_of) {
                Some(OpenAuthority::Invocation(continued)) => {
                    let Continued { caller, original } = *continued;
                    Ok((
                        caller,
                        original.map(|original| original.launch_child_caller(invoke.invocation)),
                    ))
                }
                Some(OpenAuthority::Press) => Ok((person.clone(), None)),
                None => {
                    tracing::info!(app = %app_id, target = ?invoke.target, "app's open with nothing behind it refused");
                    Err(Box::new(RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation: invoke.invocation,
                        status: ReceiptStatus::Rejected {
                            error: ActionHostError::NotPermitted,
                        },
                    })))
                }
            },
        )
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
        if let Some(parent) = parent {
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
            if !original
                .as_ref()
                .is_none_or(|original| original.through_app_door())
            {
                return self.continuing_caller(peer_id, parent).map(|caller| {
                    OpenAuthority::Invocation(Box::new(Continued { caller, original }))
                });
            }
        }
        self.services
            .launcher
            .as_ref()
            .is_some_and(|launcher| launcher.person_initiated(app_id))
            .then_some(OpenAuthority::Press)
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
