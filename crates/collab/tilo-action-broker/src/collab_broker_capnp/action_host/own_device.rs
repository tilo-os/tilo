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
fn is_person_here(
    caller: &Caller,
    origin: Option<&super::super::published_invocation::InvocationCaller>,
) -> bool {
    matches!(caller.principal(), CallerPrincipal::Person(_))
        && super::super::host_steps::is_local_host_caller(caller, origin)
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
        origin: Option<&super::super::published_invocation::InvocationCaller>,
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
        if !is_person_here(caller, origin) {
            return Err(abandon(ActionHostError::NotPermitted));
        }
        let Some(room) = caller.room() else {
            return Err(abandon(ActionHostError::NotPermitted));
        };
        let Some(()) = self.decide_for(caller, room, |_, _, _| ()) else {
            return Err(abandon(ActionHostError::NotPermitted));
        };
        let checked = own_device::Invocation::from_argument(&request.argument)
            .and_then(|invocation| invocation.check())
            .map_err(abandon)?;
        // This descriptor describes forwarding, not the target action's contract:
        // the receiving device resolves and authorizes that action independently.
        let descriptor = tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: own_device::target().address,
            resource_kind: own_device::RESOURCE.into(),
            resource_label: "Device".into(),
            action_label: "Invoke".into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                ..Default::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        };
        let admitted = self
            .admit_host_action(
                caller,
                origin,
                super::super::host_action_admission::HostActionAdmission {
                    room,
                    app_id: &checked.app,
                    descriptor: &descriptor,
                    request,
                },
            )
            .map_err(abandon)?;
        let id = admitted.id;
        if let Ok(mut ledger) = self.remote_invocations.lock() {
            ledger.complete(&request.invocation, id);
        }
        // The journal barrier can block while custody or room policy changes.
        // Resolve the original admission again immediately before forwarding.
        self.framework_effect_ready(&admitted, || {
            let (workspace, actor, now) = self.current_framework_authority(caller, origin, room)?;
            self.admit_answer(
                (&workspace, &actor, now),
                &checked.app,
                &descriptor,
                (None, request.confirmation.as_ref()),
                false,
            )
        })?;
        match service.invoke(caller, &checked, request) {
            Ok(started) => self.follow_admitted_host_step(admitted, started),
            Err(error) => {
                self.complete_host_action(
                    &admitted,
                    tilo_actions::InvocationUpdate::Failed {
                        invocation: request.invocation,
                        error: tilo_actions::ActionFailure {
                            code: "own_device_refused".into(),
                            message: error.to_string(),
                            details: None,
                            retryable: true,
                        },
                    },
                )?;
                return Err(error);
            }
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
        callers: Mutex<Vec<Caller>>,
        answers: Mutex<Vec<Completer<OwnDeviceOutcome>>>,
        refuse: Option<ActionHostError>,
        admission_check: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    impl OwnDeviceActions for Network {
        fn invoke(
            &self,
            caller: &Caller,
            checked: &own_device::Checked,
            request: &InvokeRequest,
        ) -> Result<Pending<OwnDeviceOutcome>, ActionHostError> {
            if let Some(check) = self.admission_check.lock().unwrap().as_ref() {
                check();
            }
            if let Some(refused) = &self.refuse {
                return Err(refused.clone());
            }
            self.callers.lock().unwrap().push(caller.clone());
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

    #[test]
    fn certified_own_device_refuses_foreign_and_unproven_device_callers() {
        for foreign in [false, true] {
            let network = Arc::new(Network::default());
            let services = crate::services::HostServices {
                own_device: Some(network.clone()),
                ..Default::default()
            };
            let f = if foreign {
                super::super::super::host_steps::signed_test_support::Fixture::new_certified_foreign(
                    services,
                )
            } else {
                super::super::super::host_steps::signed_test_support::Fixture::new_certified(
                    services,
                )
            };
            let asked = request(&invocation(SETTINGS));
            if foreign {
                assert!(f.invoke(&asked).is_err());
            } else {
                assert!(invoked(&f.state, &f.caller, &asked).is_err());
            }
            assert!(network.sent.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn certified_own_device_preserves_service_caller_and_signed_ownership() {
        let network = Arc::new(Network::default());
        let f = super::super::super::host_steps::signed_test_support::Fixture::new_certified(
            crate::services::HostServices {
                own_device: Some(network.clone()),
                ..Default::default()
            },
        );
        let asked = request(&invocation(SETTINGS));
        *network.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
        f.invoke(&asked).unwrap();
        f.assert_signed(&asked, false);
        assert_eq!(
            network.callers.lock().unwrap().as_slice(),
            &[f.caller.clone()]
        );
        assert!(
            f.caller.device_public_key().is_some(),
            "certificate custody is retained"
        );
        network
            .answers
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .complete(Ok(Default::default()));
        f.assert_signed(&asked, true);
        f.invoke(&asked).unwrap();
        assert_eq!(network.sent.lock().unwrap().len(), 1);
        f.authority
            .revoked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f.invoke(&request(&invocation(SETTINGS))).is_err());
        assert_eq!(network.sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn own_device_rechecks_authority_after_blocked_durable_admission() {
        use std::sync::{atomic::Ordering, mpsc};
        use std::time::Duration;
        use tilo_actions::InvocationUpdate;

        for certified in [false, true] {
            for change in [
                "unchanged",
                "certificate",
                "execution",
                "permission",
                "time",
            ] {
                let network = Arc::new(Network::default());
                let services = crate::services::HostServices {
                    own_device: Some(network.clone()),
                    ..Default::default()
                };
                let mut f = if certified {
                    super::super::super::host_steps::signed_test_support::Fixture::new_certified(
                        services,
                    )
                } else {
                    super::super::super::host_steps::signed_test_support::Fixture::new(services)
                };
                f.caller = f.caller.clone().in_new_execution();
                let asked = request(&invocation(SETTINGS));
                let (entered, blocked) = mpsc::sync_channel(0);
                let (release, resume) = mpsc::sync_channel(0);
                f.state
                    .journal
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .after_admission = Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv_timeout(Duration::from_secs(5)).unwrap();
                }));
                let result = std::thread::scope(|scope| {
                    let state = f.state.clone();
                    let caller = f.caller.clone();
                    let origin = f.origin.clone();
                    let asked = &asked;
                    let worker =
                        scope.spawn(move || state.invoke_own_device(&caller, Some(&origin), asked));
                    blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                    match change {
                        "certificate" => f.authority.revoked.store(true, Ordering::SeqCst),
                        "execution" => f.caller.retire_execution(),
                        "permission" => f.tree.write().workspaces[0].set_owner("someone-else"),
                        "time" => f.retire_policy_origin(),
                        _ => {}
                    }
                    release.send(()).unwrap();
                    worker.join().unwrap()
                });
                if change == "unchanged" {
                    result.expect("unchanged custody must still forward");
                    assert_eq!(network.sent.lock().unwrap().len(), 1);
                    network
                        .answers
                        .lock()
                        .unwrap()
                        .pop()
                        .unwrap()
                        .complete(Ok(Default::default()));
                    let record = f.assert_signed(&asked, true);
                    assert!(matches!(record.update, InvocationUpdate::Succeeded { .. }));
                } else {
                    assert!(result.is_err(), "{certified}/{change}");
                    assert!(
                        network.sent.lock().unwrap().is_empty(),
                        "{certified}/{change}"
                    );
                    let record = f.assert_signed(&asked, true);
                    assert!(!record.uncertain);
                    assert!(
                        matches!(record.update, InvocationUpdate::Failed { ref error, .. }
                        if error.code == "host_action_refused")
                    );
                }
            }
        }
    }

    #[test]
    fn own_device_signs_before_send_and_preserves_uuid_through_completion() {
        let network = Arc::new(Network::default());
        let f = super::super::super::host_steps::signed_test_support::Fixture::new(
            crate::services::HostServices {
                own_device: Some(network.clone()),
                ..Default::default()
            },
        );
        let asked = request(&invocation(SETTINGS));
        *network.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
        let receipt = f.invoke(&asked).unwrap();
        let accepted = f.assert_signed(&asked, false);
        assert_eq!(receipt.invocation, asked.invocation);
        let repeated = f.invoke(&asked).unwrap();
        assert_eq!(receipt, repeated);
        assert_eq!(network.sent.lock().unwrap().len(), 1);
        let mut changed = asked.clone();
        changed.argument = invocation("so.tilo.notes").argument();
        assert!(
            f.invoke(&changed).is_err(),
            "changed target cannot reuse an admitted id"
        );
        network
            .answers
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .complete(Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::json!({"workspaces": []})),
                resources: Vec::new(),
            }));
        let completed = f.assert_signed(&asked, true);
        assert_eq!(completed.admission, accepted.admission);
        assert!(
            matches!(completed.update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == asked.invocation)
        );
        assert_eq!(
            completed.terminal.as_ref().unwrap().category,
            crate::action_evidence::TerminalCategory::Succeeded
        );
        f.reopen_journal();
        assert_eq!(f.assert_signed(&asked, true), completed);
    }

    #[test]
    fn own_device_signs_failure_and_a_lost_remote_answer_without_resending() {
        for dropped in [false, true] {
            let network = Arc::new(Network::default());
            let f = super::super::super::host_steps::signed_test_support::Fixture::new(
                crate::services::HostServices {
                    own_device: Some(network.clone()),
                    ..Default::default()
                },
            );
            let asked = request(&invocation(SETTINGS));
            f.invoke(&asked).unwrap();
            let answer = network.answers.lock().unwrap().pop().unwrap();
            if dropped {
                drop(answer);
            } else {
                answer.complete(Err(Box::new(tilo_actions::ActionFailure {
                    code: "device_unreachable".into(),
                    message: "the device disconnected".into(),
                    details: None,
                    retryable: true,
                })));
            }
            let completed = f.assert_signed(&asked, true);
            assert!(
                matches!(completed.update, tilo_actions::InvocationUpdate::Failed { invocation, .. } if invocation == asked.invocation)
            );
            assert_eq!(
                completed.terminal.unwrap().category,
                if dropped {
                    crate::action_evidence::TerminalCategory::Uncertain
                } else {
                    crate::action_evidence::TerminalCategory::Failed
                }
            );
            f.invoke(&asked).unwrap();
            assert_eq!(network.sent.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn own_device_journal_barriers_precede_dispatch_and_reported_success() {
        for admission_failure in [false, true] {
            let network = Arc::new(Network::default());
            let f = super::super::super::host_steps::signed_test_support::Fixture::new(
                crate::services::HostServices {
                    own_device: Some(network.clone()),
                    ..Default::default()
                },
            );
            let asked = request(&invocation(SETTINGS));
            if admission_failure {
                f.fail_next_barrier();
                assert!(f.invoke(&asked).is_err());
                assert!(network.sent.lock().unwrap().is_empty());
                assert!(f.state.remote_job(&f.caller, &asked.invocation).is_none());
            } else {
                let receipt = f.invoke(&asked).unwrap();
                let job = f.state.remote_job(&f.caller, &receipt.invocation).unwrap();
                f.fail_next_barrier();
                network
                    .answers
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .complete(Ok(tilo_actions::ActionOutput::default()));
                assert!(
                    matches!(f.state.invocation_status(job), Some(
                    tilo_actions::InvocationUpdate::Accepted { invocation }
                ) if invocation == asked.invocation),
                    "failed durability must not publish success"
                );
            }
        }
    }

    #[test]
    fn own_device_immediate_refusal_is_signed_and_replayed_without_sending() {
        let network = Arc::new(Network {
            refuse: Some(ActionHostError::Refused {
                reason: "the device is not enrolled".into(),
            }),
            ..Default::default()
        });
        let f = super::super::super::host_steps::signed_test_support::Fixture::new(
            crate::services::HostServices {
                own_device: Some(network.clone()),
                ..Default::default()
            },
        );
        let asked = request(&invocation(SETTINGS));
        *network.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
        assert!(matches!(
            f.invoke(&asked),
            Err(ActionHostError::Refused { .. })
        ));
        let completed = f.assert_signed(&asked, true);
        assert_eq!(
            completed.terminal.unwrap().category,
            crate::action_evidence::TerminalCategory::Failed
        );
        assert!(
            f.invoke(&asked).is_ok(),
            "retry observes the durable refusal"
        );
        assert!(network.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn own_device_missing_signer_and_revoked_custody_never_reach_the_network() {
        for revoked in [false, true] {
            let network = Arc::new(Network::default());
            let f = super::super::super::host_steps::signed_test_support::Fixture::new(
                crate::services::HostServices {
                    own_device: Some(network.clone()),
                    ..Default::default()
                },
            );
            if revoked {
                f.authority
                    .revoked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            } else {
                std::fs::remove_file(
                    tilo_identity_host::provisioning::collaboration_identity_paths().device_privkey,
                )
                .unwrap();
            }
            let asked = request(&invocation(SETTINGS));
            assert!(f.invoke(&asked).is_err());
            assert!(network.sent.lock().unwrap().is_empty());
            assert!(f.state.remote_job(&f.caller, &asked.invocation).is_none());
        }
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
    /// refused by the host, and the refusal remains the invocation's outcome.
    /// A retry observes that refusal rather than attempting another dispatch.
    #[test]
    fn a_device_the_host_refuses_retains_a_failed_invocation() {
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
        let job = state
            .remote_job(&person, &asked.invocation)
            .expect("refusal is retained");
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Failed { invocation, .. }) if invocation == asked.invocation
        ));
        let repeated = invoked(&state, &person, &asked).expect("retry observes retained failure");
        assert_eq!(repeated.invocation, asked.invocation);
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
