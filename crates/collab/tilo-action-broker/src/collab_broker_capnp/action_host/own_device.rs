//! An application's action on another of the person's own devices.
//!
//! The broker admits the request here — the person at this device, for an
//! application's own action — and hands it to the host's network, which
//! sends it to the other device as an ordinary invocation and follows it
//! there. The other device's outcome becomes this invocation's, so the
//! application observes it like any of its own invocations.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionHostError, Caller, CallerPrincipal, InvocationReceipt, InvokeRequest, Reservation,
    own_device,
};

use super::super::CapnpBrokerState;

/// Whether `caller` is the person at this device acting themselves: not
/// another device, not an agent, bot or app, not a Shortcut's step. Only
/// they reach their other devices, so nobody gains a reach across devices
/// they lack here.
fn is_person_here(caller: &Caller) -> bool {
    matches!(caller.principal(), CallerPrincipal::Person(_))
        && caller.device_public_key().is_none()
        && caller.acting_for_person().is_none()
        && caller.shortcut().is_none()
        && !caller.is_foreign_device()
}

impl CapnpBrokerState {
    /// Admit one own-device invocation for `caller` and send it on.
    ///
    /// The caller must be the person at this device, confined to a room
    /// they may act in (an application reaches this as its person only for
    /// its own actions, which its door decides). The host's network then
    /// decides whether the device is one of theirs and carries the request.
    /// The invocation answers `Accepted` at once and records the other
    /// device's outcome when it arrives; a retry under the same id observes
    /// the first.
    pub(super) fn invoke_own_device(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let abandon = |error: ActionHostError| {
            if let Ok(mut ledger) = self.remote_invocations.lock() {
                ledger.abandon(&request.invocation);
            }
            error
        };
        let reservation = self
            .remote_invocations
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            })?
            .reserve(caller, request)?;
        if matches!(reservation, Reservation::Admitted) {
            return self
                .remote_job(caller, &request.invocation)
                .map(|job| self.receipt_for(request.invocation, job))
                .ok_or(ActionHostError::UnknownInvocation);
        }
        let Some(service) = self.services.own_device.as_ref() else {
            return Err(abandon(ActionHostError::UnknownAction));
        };
        if request.target != own_device::target() {
            return Err(abandon(ActionHostError::UnknownAction));
        }
        if !is_person_here(caller) {
            return Err(abandon(ActionHostError::NotPermitted));
        }
        let Some(room) = caller.room() else {
            return Err(abandon(ActionHostError::NotPermitted));
        };
        let Some(actor) = self.decide_for(caller, room, |_, actor, _| actor.clone()) else {
            return Err(abandon(ActionHostError::NotPermitted));
        };
        let checked = own_device::Invocation::from_argument(&request.argument)
            .and_then(|invocation| invocation.check())
            .map_err(abandon)?;
        let started = service.invoke(caller, &checked, request).map_err(abandon)?;
        let id = self.follow_host_step(&actor, started);
        if let Ok(mut ledger) = self.remote_invocations.lock() {
            ledger.complete(&request.invocation, id);
        }
        Ok(self.receipt_for(request.invocation, id))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
    use crate::services::{OwnDeviceActions, OwnDeviceOutcome};
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::{
        ActionHostError, Caller, Completer, InvokeArgument, InvokeRequest, Pending,
        RemoteActionMessage, own_device, pending,
    };

    const SETTINGS: &str = "so.tilo.settings";

    /// The host's network, standing in: it records what it was asked to
    /// send and answers when the test completes it.
    #[derive(Default)]
    struct Network {
        sent: Mutex<Vec<(own_device::Checked, InvokeRequest)>>,
        answers: Mutex<Vec<Completer<OwnDeviceOutcome>>>,
        refuse: Option<ActionHostError>,
    }

    impl OwnDeviceActions for Network {
        fn invoke(
            &self,
            _caller: &Caller,
            checked: &own_device::Checked,
            request: &InvokeRequest,
        ) -> Result<Pending<OwnDeviceOutcome>, ActionHostError> {
            if let Some(refused) = &self.refuse {
                return Err(refused.clone());
            }
            self.sent
                .lock()
                .unwrap()
                .push((checked.clone(), request.clone()));
            let (answer, pending) = pending();
            self.answers.lock().unwrap().push(answer);
            Ok(pending)
        }
    }

    fn broker(network: Arc<Network>) -> (Arc<CapnpBrokerState>, String, String) {
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceId(9), "Devices");
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
                own_device: Some(network),
                ..Default::default()
            },
        );
        (state, owner, room)
    }

    fn invocation(app: &str) -> own_device::Invocation {
        own_device::Invocation {
            device: hex::encode([42; 32]),
            app: app.into(),
            resource: "settings.root".into(),
            action: "storage-summary".into(),
            arguments: None,
        }
    }

    fn request(invocation: &own_device::Invocation) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: own_device::target(),
            argument: invocation.argument(),
            context: None,
            confirmation: None,
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

    /// Nobody but the person at this device reaches another device: not a
    /// remote device (of theirs or anyone's), an agent, a bot, an app under its own
    /// identity, a Shortcut's step, a foreign device, or a caller in no
    /// room. Nothing is sent for any of them. (Which person this device
    /// belongs to, and whether the target is one of theirs, the host's
    /// network decides.)
    #[test]
    fn own_device_refuses_every_caller_but_the_person_here() {
        let network = Arc::new(Network::default());
        let (state, owner, room) = broker(Arc::clone(&network));
        let asked = request(&invocation(SETTINGS));
        let refused = [
            Caller::authenticated([41; 32], [43; 32]).in_room(room.clone()),
            Caller::authenticated([77; 32], [78; 32]).in_room(room.clone()),
            Caller::local_agent("assistant").in_room(room.clone()),
            Caller::delegated_agent("assistant", [41; 32], [44; 32]).in_room(room.clone()),
            Caller::bot("helper", [41; 32], [45; 32]).in_room(room.clone()),
            Caller::local_app(SETTINGS).in_room(room.clone()),
            Caller::local_person(owner.clone())
                .delegated_to("assistant")
                .in_room(room.clone()),
            Caller::local_person(owner.clone())
                .running_shortcut("nightly")
                .in_room(room.clone()),
            Caller::local_person(owner.clone())
                .from_foreign_device(true)
                .in_room(room),
            Caller::local_person(owner),
        ];
        for caller in refused {
            let mut fresh = asked.clone();
            fresh.invocation = tilo_actions::InvocationId::new();
            assert!(
                invoked(&state, &caller, &fresh).is_err(),
                "{:?} must not reach another device",
                caller.principal()
            );
        }
        assert!(network.sent.lock().unwrap().is_empty(), "nothing was sent");
    }

    /// The person reaches the service once per invocation id, a retry
    /// observes the first run, and the other device's outcome becomes the
    /// invocation's.
    #[test]
    fn own_device_sends_once_per_invocation_and_follows_the_other_device() {
        let network = Arc::new(Network::default());
        let (state, owner, room) = broker(Arc::clone(&network));
        let person = Caller::local_person(owner).in_room(room);
        let asked = request(&invocation(SETTINGS));

        let receipt = invoked(&state, &person, &asked).expect("admitted");
        let again = invoked(&state, &person, &asked).expect("the retry is the same run");
        assert_eq!(again.invocation, receipt.invocation);
        let sent = network.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "a retry is not sent again");
        let (checked, forwarded) = &sent[0];
        assert_eq!(checked.device, [42; 32]);
        assert_eq!(checked.app, SETTINGS);
        assert_eq!(checked.address.action, "storage-summary");
        assert_eq!(forwarded.invocation, asked.invocation);

        let job = state
            .remote_job(&person, &receipt.invocation)
            .expect("the invocation has a job");
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Accepted { .. })
        ));
        let summary = serde_json::json!({"workspaces": []});
        network
            .answers
            .lock()
            .unwrap()
            .pop()
            .expect("sent")
            .complete(Ok(tilo_actions::ActionOutput {
                value: Some(summary.clone()),
                resources: Vec::new(),
            }));
        let Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            state.invocation_status(job)
        else {
            panic!("the other device's answer is the invocation's outcome");
        };
        assert_eq!(output.value, Some(summary));
    }

    /// A device the host does not recognise as one of the person's is
    /// refused by the host, and the refusal is the invocation's: the id may
    /// be used again once the device is enrolled.
    #[test]
    fn a_device_the_host_refuses_is_refused_and_the_id_is_released() {
        let network = Arc::new(Network {
            refuse: Some(ActionHostError::Refused {
                reason: "the device is not one of the person's enrolled devices".into(),
            }),
            ..Network::default()
        });
        let (state, owner, room) = broker(network);
        let person = Caller::local_person(owner).in_room(room);
        let asked = request(&invocation(SETTINGS));
        assert!(matches!(
            invoked(&state, &person, &asked),
            Err(ActionHostError::Refused { .. })
        ));
        assert!(state.remote_job(&person, &asked.invocation).is_none());
        let mut malformed = asked;
        malformed.invocation = tilo_actions::InvocationId::new();
        malformed.argument = InvokeArgument::Text("aa".into());
        assert!(matches!(
            invoked(&state, &person, &malformed),
            Err(ActionHostError::InvalidRequest { .. })
        ));
    }

    /// An application reaches its own actions on another device as the
    /// person using it; another application's actions are not reachable
    /// this way at all, whatever it declared.
    #[test]
    fn an_app_reaches_only_its_own_actions_on_another_device() {
        let (state, owner, room) = broker(Arc::new(Network::default()));
        let person = Caller::local_person(owner).in_room(room);
        let own = RemoteActionMessage::Invoke(request(&invocation(SETTINGS)));
        assert_eq!(
            state
                .app_door(&person, SETTINGS, &own)
                .map(|caller| caller.principal().clone())
                .ok(),
            Some(person.principal().clone())
        );
        let other = RemoteActionMessage::Invoke(request(&invocation("so.tilo.notes")));
        assert!(state.app_door(&person, SETTINGS, &other).is_err());
        assert!(state.app_door(&person, "so.tilo.notes", &own).is_err());
    }
}
