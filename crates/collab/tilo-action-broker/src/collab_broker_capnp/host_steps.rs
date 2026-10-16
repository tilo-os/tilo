//! Steps the host itself runs for the person at this device: pairing a
//! nearby device, and administering the person's identities and the devices
//! trusted under them.
//!
//! Each is answered under the host's own provider, offered only to a caller
//! on this machine confined to a room, and admitted against that room's
//! policy under the service's app id — the application whose controls reach
//! the steps as the person using it. The door treats them alike; only the
//! service that runs a step differs.

use std::sync::Arc;

use tilo_actions::remote::{Pending, device_pairing, identity_admin};
use tilo_actions::{ActionAddress, ActionDescriptor};

use super::BrokerActor;
use crate::services::{
    DevicePairingOutcome, DevicePairingRequest, DevicePairingServices, HostServices,
    IdentityAdminRequest, IdentityAdminServices,
};

/// One installed host-step service.
#[derive(Clone)]
pub(super) enum HostSteps {
    Pairing(Arc<dyn DevicePairingServices>),
    Identity(Arc<dyn IdentityAdminServices>),
}

impl HostSteps {
    /// Every host-step service `services` installs.
    pub(super) fn installed(services: &HostServices) -> Vec<Self> {
        let pairing = services.device_pairing.clone().map(Self::Pairing);
        let identity = services.identity_admin.clone().map(Self::Identity);
        pairing.into_iter().chain(identity).collect()
    }

    /// The installed service that answers under `provider`, if any.
    pub(super) fn for_provider(services: &HostServices, provider: &str) -> Option<Self> {
        Self::installed(services)
            .into_iter()
            .find(|steps| steps.provider() == provider)
    }

    /// The provider the steps are answered under.
    pub(super) fn provider(&self) -> &'static str {
        match self {
            Self::Pairing(_) => device_pairing::PROVIDER,
            Self::Identity(_) => identity_admin::PROVIDER,
        }
    }

    /// The application whose controls reach the steps as their person.
    pub(super) fn app_id(&self) -> &str {
        match self {
            Self::Pairing(service) => service.app_id(),
            Self::Identity(service) => service.app_id(),
        }
    }

    /// Every step, described.
    pub(super) fn descriptors(&self) -> Vec<ActionDescriptor> {
        match self {
            Self::Pairing(service) => service.descriptors(),
            Self::Identity(service) => service.descriptors(),
        }
    }

    /// Start the admitted step `address` for `actor` in `room`.
    ///
    /// # Errors
    ///
    /// Why the host refuses to start it.
    pub(super) fn start(
        &self,
        room: &str,
        actor: &BrokerActor,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<Pending<DevicePairingOutcome>, String> {
        match self {
            Self::Pairing(service) => service.start(&DevicePairingRequest {
                room,
                actor,
                address,
                argument,
            }),
            Self::Identity(service) => service.start(&IdentityAdminRequest {
                room,
                actor,
                address,
                argument,
            }),
        }
    }
}

#[cfg(test)]
mod identity_admin_tests {
    use super::super::action_host::RowScope;
    use super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
    use crate::services::{IdentityAdminOutcome, IdentityAdminRequest, IdentityAdminServices};
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::{
        ActionHostError, Caller, Completer, Confirmation, InvokeArgument, InvokeRequest, Pending,
        RemoteActionMessage, identity_admin, pending,
    };

    /// The host's identity store, standing in: it records each step it was
    /// asked to start and answers when the test completes it.
    #[derive(Default)]
    struct Host {
        started: Mutex<Vec<(String, String, String)>>,
        answers: Mutex<Vec<Completer<IdentityAdminOutcome>>>,
    }

    fn contract(step: &str) -> tilo_actions::ActionContract {
        match step {
            identity_admin::LIST | identity_admin::DEVICES => tilo_actions::ActionContract::view(),
            identity_admin::HOME_PEER_RETRY => tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                ..tilo_actions::ActionContract::default()
            },
            _ => tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                risk: tilo_actions::RiskLevel::Privileged,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                ..tilo_actions::ActionContract::default()
            },
        }
    }

    impl IdentityAdminServices for Host {
        fn app_id(&self) -> &'static str {
            "so.tilo.settings"
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            identity_admin::STEPS
                .into_iter()
                .map(|step| tilo_actions::ActionDescriptor {
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(identity_admin::RESOURCE, step),
                    resource_kind: identity_admin::RESOURCE.into(),
                    resource_label: "Identities".into(),
                    action_label: step.into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::default(),
                    kind: tilo_actions::OperationKind::Effect,
                    contract: contract(step),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                })
                .collect()
        }

        fn start(
            &self,
            request: &IdentityAdminRequest<'_>,
        ) -> Result<Pending<IdentityAdminOutcome>, String> {
            self.started.lock().unwrap().push((
                request.room.to_owned(),
                request.actor.id().to_owned(),
                request.address.action.clone(),
            ));
            let (answer, pending) = pending();
            self.answers.lock().unwrap().push(answer);
            Ok(pending)
        }
    }

    fn broker(host: Arc<Host>) -> (Arc<CapnpBrokerState>, String, String) {
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceId(9), "Identities");
        workspace.set_owner(owner.clone());
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared().authority()),
            None,
            None,
            crate::services::HostServices {
                identity_admin: Some(host),
                ..Default::default()
            },
        );
        (state, owner, room)
    }

    fn request(step: &str, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: identity_admin::target(step),
            argument: InvokeArgument::None,
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn invoked(
        state: &Arc<CapnpBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    #[test]
    fn identity_admin_is_offered_to_settings_as_its_person_and_removal_asks_first() {
        let host = Arc::new(Host::default());
        let (state, owner, room) = broker(Arc::clone(&host));
        let person = Caller::local_person(owner.clone()).in_room(room.clone());

        let rows = state
            .eager_catalogue(&person, RowScope::Provider(identity_admin::PROVIDER))
            .0;
        assert_eq!(
            rows.len(),
            identity_admin::STEPS.len(),
            "every step is offered to the person here"
        );
        assert!(rows.iter().all(
            |row| row.app_id == "so.tilo.settings" && row.provider == identity_admin::PROVIDER
        ));

        // Settings reaches the steps as the person using it, without
        // declaring `[actions] invoke`; another app does not.
        let list = RemoteActionMessage::Invoke(request(identity_admin::LIST, None));
        assert_eq!(
            state
                .app_door(&person, "so.tilo.settings", &list)
                .map(|caller| caller.principal().clone())
                .ok(),
            Some(person.principal().clone())
        );
        assert!(state.app_door(&person, "so.tilo.notes", &list).is_err());

        for step in [
            identity_admin::ACTIVATE,
            identity_admin::REMOVE,
            identity_admin::PROTECT,
            identity_admin::REVOKE,
            identity_admin::HOME_PEER_REMOVE,
        ] {
            assert_eq!(
                invoked(&state, &person, &request(step, None)),
                Err(ActionHostError::ConfirmationRequired),
                "{step} needs the person's confirmation"
            );
        }
        assert!(host.started.lock().unwrap().is_empty(), "nothing started");

        // Listing asks nothing: the person sees their own identities.
        invoked(&state, &person, &request(identity_admin::LIST, None)).expect("listed");
        host.started.lock().unwrap().clear();
        host.answers.lock().unwrap().clear();

        let receipt = invoked(
            &state,
            &person,
            &request(identity_admin::REMOVE, Some(Confirmation::Confirmed)),
        )
        .expect("admitted");
        assert_eq!(
            *host.started.lock().unwrap(),
            [(room, owner, identity_admin::REMOVE.to_owned())]
        );
        let job = state
            .remote_job(&person, &receipt.invocation)
            .expect("the invocation has a job");
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Accepted { .. })
        ));
        let answer = host.answers.lock().unwrap().pop().expect("started");
        answer.complete(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"removed": "work"})),
            resources: Vec::new(),
        }));
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn identity_admin_is_refused_to_strangers_remote_devices_and_unconfined_callers() {
        let host = Arc::new(Host::default());
        let (state, owner, room) = broker(Arc::clone(&host));

        // A person the room's policy grants nothing is offered no step that
        // changes the identity, and the broker refuses one before the host
        // hears of it.
        let stranger = Caller::local_person(hex::encode([7; 32])).in_room(room.clone());
        let offered = state
            .eager_catalogue(&stranger, RowScope::Provider(identity_admin::PROVIDER))
            .0;
        assert!(
            offered.iter().all(|row| row
                .action
                .as_ref()
                .is_some_and(
                    |descriptor| descriptor.contract.risk != tilo_actions::RiskLevel::Privileged
                )),
            "{offered:?}"
        );
        for step in [
            identity_admin::ACTIVATE,
            identity_admin::PROTECT,
            identity_admin::REMOVE,
            identity_admin::REVOKE,
        ] {
            assert!(
                invoked(
                    &state,
                    &stranger,
                    &request(step, Some(Confirmation::Confirmed))
                )
                .is_err(),
                "{step} is refused to a stranger"
            );
        }

        // A remote device, or a caller confined to no room, is never offered
        // this machine's identity store.
        let remote = Caller::authenticated([41; 32], [42; 32]).in_room(room);
        assert!(
            state
                .eager_catalogue(&remote, RowScope::Provider(identity_admin::PROVIDER))
                .0
                .is_empty()
        );
        assert!(invoked(&state, &remote, &request(identity_admin::LIST, None)).is_err());
        let unconfined = Caller::local_person(owner);
        assert!(invoked(&state, &unconfined, &request(identity_admin::LIST, None)).is_err());
        assert!(host.started.lock().unwrap().is_empty(), "nothing started");
    }
}
