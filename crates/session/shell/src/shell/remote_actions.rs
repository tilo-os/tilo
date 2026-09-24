//! The session's action broker as an [`ActionHost`].
//!
//! [`BrokerActionHost`] is the one door through which anything other than
//! the publishing application reaches a published action: a peer on the
//! shell network, an app or agent on this machine over the collaboration
//! socket, the MCP bridge, the palette. Each supplies a [`Caller`] its own
//! transport authenticated; the broker admits, permission-checks and
//! dispatches the invocation exactly as it always has, and the shared
//! invocation ledger makes a retried [`InvocationId`] idempotent whichever
//! way it arrives.
//!
//! [`InvocationId`]: tilo_actions::InvocationId

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, Catalogue, InvocationReceipt, InvocationState,
    InvokeRequest,
};

use crate::shell::collab_broker::AppActionBroker;

/// [`ActionHost`] over the live application broker.
#[derive(Clone)]
pub struct BrokerActionHost {
    broker: AppActionBroker,
    /// The application connection the caller itself is, when it is one; it
    /// is what [`tilo_actions::remote::OWN_PROVIDER`] names.
    own_peer: Option<u64>,
}

impl BrokerActionHost {
    #[must_use]
    pub fn new(broker: AppActionBroker) -> Self {
        Self {
            broker,
            own_peer: None,
        }
    }

    /// The host as seen from application connection `peer_id`.
    #[must_use]
    pub fn for_connection(broker: AppActionBroker, peer_id: u64) -> Self {
        Self {
            broker,
            own_peer: Some(peer_id),
        }
    }
}

impl ActionHost for BrokerActionHost {
    fn catalogue(&self, caller: &Caller) -> Result<Catalogue, ActionHostError> {
        Ok(self.broker.state().action_catalogue_for(caller))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        self.broker
            .state()
            .action_invoke(caller, self.own_peer, &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.broker.state().action_observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.broker.state().action_cancel(caller, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::collab_broker::CrdtRoute;
    use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::ReceiptStatus;
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
        workspace.set_owner("a1".repeat(32));
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
                    "erase",
                    tilo_actions::ActionContract {
                        risk: tilo_actions::RiskLevel::Destructive,
                        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
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
            confirmation: None,
        }
    }

    #[test]
    fn the_catalogue_offers_only_what_the_caller_may_use() {
        let (host, _) = host();
        let catalogue = host.catalogue(&owner()).expect("owner catalogue");
        let offered: Vec<_> = catalogue
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.provider.split(':').next().unwrap_or_default(),
                    entry.descriptor.address.action.as_str(),
                )
            })
            .collect();
        assert_eq!(
            offered,
            [
                ("7", "refresh"),
                ("7", "erase"),
                ("7", "rename"),
                ("workspace", "profile.save"),
                ("workspace", "profile.assign"),
            ],
            "a remote device is not offered launching apps on this desktop"
        );
        assert!(
            host.catalogue(&stranger())
                .expect("stranger catalogue")
                .entries
                .is_empty(),
            "a caller outside the workspace is offered nothing"
        );
    }

    #[test]
    fn confirmation_is_evidence_the_contract_checks() {
        let (host, mut invocations) = host();
        let mut erase = refresh(tilo_actions::InvocationId::new());
        erase.target.address = ActionAddress::new("files.entry:/tmp/report", "erase");
        assert_eq!(
            host.invoke(&owner(), erase.clone()),
            Err(ActionHostError::ConfirmationRequired)
        );
        erase.confirmation = Some(tilo_actions::remote::Confirmation::Phrase("erase".into()));
        assert_eq!(
            host.invoke(&owner(), erase.clone()),
            Err(ActionHostError::ConfirmationRequired),
            "the wrong kind of evidence is not accepted"
        );
        assert!(invocations.try_recv().is_err());
        erase.confirmation = Some(tilo_actions::remote::Confirmation::Confirmed);
        host.invoke(&owner(), erase).expect("confirmed");
        assert!(invocations.try_recv().is_ok(), "dispatched once confirmed");
    }

    #[test]
    fn a_room_confined_caller_sees_only_its_room() {
        let (host, _) = host();
        let elsewhere = Caller::local_person("a1".repeat(32)).in_room("not-this-room");
        assert!(
            host.catalogue(&elsewhere)
                .expect("catalogue")
                .entries
                .is_empty(),
            "a connection admitted to another room reaches nothing here"
        );
        assert_eq!(
            host.invoke(&elsewhere, refresh(tilo_actions::InvocationId::new())),
            Err(ActionHostError::UnknownAction)
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
