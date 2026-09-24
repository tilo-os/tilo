//! The session's action broker, offered to authenticated remote callers.
//!
//! [`BrokerActionHost`] adapts [`AppActionBroker`] — the same broker that
//! serves the palette, the control socket and local agents — to
//! [`ActionHost`]. It adds no second execution path: a remote invocation is
//! admitted, permission-checked and dispatched exactly like a local one,
//! attributed to the caller's user root. What it adds is the remote
//! contract: actions outside the caller's workspaces are absent from the
//! catalogue, and invocation ids are idempotent across retries.

use std::sync::Mutex;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, Catalogue, CatalogueEntry, InvocationLedger,
    InvocationReceipt, InvocationState, InvokeRequest, ReceiptStatus,
};
use tilo_actions::{ActionAddress, ActionDescriptor, Collects, ExecutionLocus, InputSpec};

use crate::shell::collab_broker::AppActionBroker;
use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};

/// [`ActionHost`] over the live application broker.
pub struct BrokerActionHost {
    broker: AppActionBroker,
    /// Remote invocation id to the broker's own job id.
    ledger: Mutex<InvocationLedger<u64>>,
}

impl BrokerActionHost {
    #[must_use]
    pub fn new(broker: AppActionBroker) -> Self {
        Self {
            broker,
            ledger: Mutex::new(InvocationLedger::default()),
        }
    }

    /// Surfaces offered to `caller`, with actions its authority excludes
    /// already removed.
    fn offered(&self, caller: &Caller) -> Vec<PublishedSurface> {
        self.broker.surfaces_offered_to_member(&person(caller))
    }

    fn job(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<u64> {
        self.ledger
            .lock()
            .ok()
            .and_then(|ledger| ledger.get(caller, id).copied())
    }
}

/// The person the broker's workspace policy knows `caller` as.
fn person(caller: &Caller) -> String {
    crate::shell::collab_broker_capnp::principal_id_for_root(*caller.user_public_key())
}

fn descriptor(action: &PublishedAction) -> ActionDescriptor {
    ActionDescriptor {
        address: ActionAddress::new(&action.resource, &action.action),
        resource_kind: action.resource_kind.clone(),
        resource_label: action.resource_label.clone(),
        action_label: action.action_label.clone(),
        description: action.description.clone(),
        // The host runs the action wherever its app lives; the published
        // surface carries no locus of its own.
        locus: ExecutionLocus::default(),
        kind: action.operation_kind,
        contract: action.contract.clone(),
        input: match &action.input {
            PublishedInput::None => InputSpec::None,
            PublishedInput::Text {
                prompt,
                placeholder,
                wants_content,
            } => InputSpec::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                collects: if *wants_content {
                    Collects::Content
                } else {
                    Collects::Words
                },
            },
        },
        params: action.params.clone(),
    }
}

/// Map a broker refusal onto the remote vocabulary. The broker words its
/// refusals for people; only the distinctions a remote caller can act on
/// are recovered, everything else stays a plain refusal.
fn refusal(reason: String) -> ActionHostError {
    if reason.starts_with("action_confirmation_required") {
        ActionHostError::ConfirmationRequired
    } else if reason.contains("not allowed") {
        ActionHostError::NotPermitted
    } else if reason.contains("no longer publishes") {
        ActionHostError::UnknownAction
    } else {
        ActionHostError::Refused { reason }
    }
}

impl ActionHost for BrokerActionHost {
    fn catalogue(&self, caller: &Caller) -> Result<Catalogue, ActionHostError> {
        let revision = self.broker.surfaces_generation().to_string();
        let entries = self
            .offered(caller)
            .into_iter()
            .flat_map(|surface| {
                let provider = surface.peer_id.to_string();
                surface
                    .actions
                    .into_iter()
                    .map(move |action| CatalogueEntry {
                        provider: provider.clone(),
                        app_id: surface.app_id.clone(),
                        descriptor: descriptor(&action),
                    })
            })
            .collect();
        Ok(Catalogue { revision, entries })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let person = person(caller);
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            })?;
        let (job, _) = ledger.admit(caller, &request, || {
            let peer_id = request
                .target
                .provider
                .parse::<u64>()
                .map_err(|_| ActionHostError::UnknownAction)?;
            let address = &request.target.address;
            let offered = self.offered(caller).into_iter().any(|surface| {
                surface.peer_id == peer_id
                    && surface.actions.iter().any(|action| {
                        action.resource == address.resource && action.action == address.action
                    })
            });
            if !offered {
                return Err(ActionHostError::UnknownAction);
            }
            self.broker
                .start_with_context(
                    peer_id,
                    &address.resource,
                    &address.action,
                    request.argument.to_slot().as_deref(),
                    &person,
                    request.context.clone(),
                )
                .map(|job| job.id)
                .map_err(refusal)
        })?;
        let job = *job;
        drop(ledger);
        Ok(self.broker.status(job).map_or(
            InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Accepted,
            },
            |update| InvocationState::new(request.invocation, update).receipt(),
        ))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        let job = self.job(caller, id)?;
        self.broker
            .status(job)
            .map(|update| InvocationState::new(*id, update))
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        let job = self
            .job(caller, id)
            .ok_or(ActionHostError::UnknownInvocation)?;
        self.broker
            .cancel(job, &person(caller))
            .map_err(|reason| ActionHostError::NotCancellable { reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::collab_broker::CrdtRoute;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

    const OWNER_ROOT: [u8; 32] = [0xA1; 32];

    fn owner() -> Caller {
        Caller::authenticated(OWNER_ROOT, [0xA2; 32])
    }

    fn stranger() -> Caller {
        Caller::authenticated([0xB1; 32], [0xB2; 32])
    }

    fn action(name: &str, contract: tilo_actions::ActionContract) -> PublishedAction {
        PublishedAction {
            resource: "files.entry:/tmp/report".into(),
            action: name.into(),
            resource_kind: "files.entry".into(),
            resource_label: "report".into(),
            action_label: name.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract,
            input: PublishedInput::None,
        }
    }

    /// A broker hosting one workspace the owner's root owns, with one live
    /// files app publishing a cancellable job and a write-gated action.
    fn host() -> (
        BrokerActionHost,
        tokio::sync::mpsc::UnboundedReceiver<crate::shell::collab_broker_capnp::InvocationEnvelope>,
    ) {
        let mut workspace = Workspace::test_default(WorkspaceId(41), "Remote room");
        workspace.set_owner(person(&owner()));
        let route = CrdtRoute::app("so.tilo.files", workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_membership(
                tree.shared(),
            );
        broker.replace_published_surfaces_for_test(vec![PublishedSurface {
            peer_id: 7,
            app_id: "so.tilo.files".into(),
            route,
            actions: vec![
                action(
                    "refresh",
                    tilo_actions::ActionContract {
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        ..tilo_actions::ActionContract::default()
                    },
                ),
                action(
                    "rename",
                    tilo_actions::ActionContract {
                        permission: tilo_actions::PermissionRequirement::Write,
                        ..tilo_actions::ActionContract::default()
                    },
                ),
            ],
            live: true,
        }]);
        let invocations = broker.capture_invocations_for_test(7);
        (BrokerActionHost::new(broker), invocations)
    }

    fn refresh(invocation: tilo_actions::InvocationId) -> InvokeRequest {
        InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: "7".into(),
                address: ActionAddress::new("files.entry:/tmp/report", "refresh"),
            },
            argument: InvokeArgument::None,
            context: None,
        }
    }

    #[test]
    fn the_catalogue_offers_only_what_the_caller_may_use() {
        let (host, _) = host();
        let catalogue = host.catalogue(&owner()).expect("owner catalogue");
        let offered: Vec<_> = catalogue
            .entries
            .iter()
            .map(|entry| entry.descriptor.address.action.as_str())
            .collect();
        assert_eq!(offered, ["refresh", "rename"]);
        assert_eq!(catalogue.entries[0].provider, "7");
        assert!(
            host.catalogue(&stranger())
                .expect("stranger catalogue")
                .entries
                .is_empty(),
            "a caller outside the workspace is offered nothing"
        );
    }

    #[test]
    fn a_retried_remote_invocation_dispatches_once() {
        let (host, mut invocations) = host();
        let id = tilo_actions::InvocationId::new();
        let first = host.invoke(&owner(), refresh(id)).expect("admitted");
        assert_eq!(first.invocation, id);
        assert_eq!(first.status, ReceiptStatus::Accepted);
        assert!(invocations.try_recv().is_ok(), "dispatched to the app");

        let retry = host.invoke(&owner(), refresh(id)).expect("retry");
        assert_eq!(retry, first);
        assert!(
            invocations.try_recv().is_err(),
            "a retry never reaches the app twice"
        );
        let state = host.observe(&owner(), &id).expect("owner observes");
        assert_eq!(state.invocation(), id);
        assert_eq!(host.observe(&stranger(), &id), None);

        assert_eq!(
            host.cancel(&stranger(), &id),
            Err(ActionHostError::UnknownInvocation)
        );
        host.cancel(&owner(), &id).expect("owner cancels");
    }

    #[test]
    fn remote_callers_cannot_reach_what_they_are_not_offered() {
        let (host, mut invocations) = host();
        assert_eq!(
            host.invoke(&stranger(), refresh(tilo_actions::InvocationId::new())),
            Err(ActionHostError::UnknownAction)
        );
        let mut elsewhere = refresh(tilo_actions::InvocationId::new());
        elsewhere.target.provider = "8".into();
        assert_eq!(
            host.invoke(&owner(), elsewhere),
            Err(ActionHostError::UnknownAction)
        );
        assert!(invocations.try_recv().is_err());
    }
}
