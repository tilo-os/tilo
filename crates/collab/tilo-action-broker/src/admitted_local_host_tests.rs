// Uses the real signed authority, private worker, RPC directory and SDK authorizer above.
fn local_host_evidence(env: &Environment) -> (tilo_actions::remote::Caller, Vec<u8>) {
    member_device_evidence(env, 93, false)
}

/// A member device `seed` of the host's person, foreign or not, that the
/// host's authority currently accepts.
fn member_device_evidence(
    env: &Environment,
    seed: u8,
    foreign: bool,
) -> (tilo_actions::remote::Caller, Vec<u8>) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = env.cert.clone();
    cert.device_public_key = key.verifying_key().to_bytes();
    cert.capabilities = tilo_user_identity::DeviceCapabilities::member();
    cert.capabilities.foreign = foreign;
    cert.expires_at_ns = Some(cert.issued_at_ns + 6_000_000_000);
    tilo_user_identity::sign_device_cert(&mut cert, &env.person).unwrap();
    env.authority
        .additional_certificates
        .lock()
        .unwrap()
        .push(cert.clone());
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_room(&env.route.cluster_id)
            .in_new_execution();
    (caller, presentation(&env.route.cluster_id, &cert, &key))
}

fn local_host_request(directory: bool) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "903".into(),
            address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
        },
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: directory.then(|| "original-r1".into()),
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

async fn local_host_answer<T>(mut pending: tilo_actions::remote::Pending<T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match pending.poll_ready() {
                tilo_actions::remote::PendingPoll::Ready(value) => return Some(value),
                tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
                tilo_actions::remote::PendingPoll::Abandoned => return None,
            }
        }
    })
    .await
    .expect("actual broker admission completes")
    .expect("actual broker admission remains open")
}

async fn local_host_lease(
    rig: &Rig,
    caller: &tilo_actions::remote::Caller,
    invocation: tilo_actions::InvocationId,
) -> ui::invocation_scope::PublishedInvocationLease {
    let delivered =
        tokio::time::timeout(Duration::from_secs(2), rig.stream.as_ref().unwrap().read())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(delivered.invocation_uuid, *invocation.0.as_bytes());
    let request = ui::invocation_scope::PublishedInvocationRequest {
        wire_id: delivered.id,
        invocation,
        address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
        argument: delivered.argument,
        actor: delivered.actor,
        participant: Some(tilo_actions::ParticipantId::for_execution(
            caller.execution_id().unwrap(),
        )),
    };
    let lease = (rig.authorize)(&request).expect("actual worker receives original custody");
    lease.check().unwrap();
    lease
}

#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_preserves_eager_and_directory_original_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for (directory, member_worker) in [(false, false), (true, false), (true, true)] {
                let mut env = Environment::new();
                if member_worker {
                    // Existing ordinary-member private registration: no host promotion.
                    env.cert.capabilities = tilo_user_identity::DeviceCapabilities::member();
                    env.cert.capabilities.foreign = true;
                    let person = env.person.clone();
                    tilo_user_identity::sign_device_cert(&mut env.cert, &person).unwrap();
                    env.authority = CurrentHost::new(
                        &env.route,
                        &env.cert,
                        Some(env.origin.capture().unwrap()),
                    );
                }
                let rig = Rig::new(&env).await;
                if member_worker {
                    let workers = rig.state.host_workers.lock().unwrap();
                    let admitted = workers.get(&903).unwrap();
                    assert!(admitted.host_scope.is_none());
                    assert!(admitted.worker_registration.is_some());
                }
                if !directory {
                    let row = directory_state("original-r1").borrow().row();
                    rig.worker
                        .handle
                        .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                        .await
                        .unwrap();
                }
                let (caller, evidence) = local_host_evidence(&env);
                let facade =
                    crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
                let host = facade
                    .certified_local_action_host(&caller, &evidence)
                    .unwrap();
                let request = local_host_request(directory);
                let mut events = rig.state.subscribe_execution_activity();
                let pending = host.invoke(&caller, request.clone());
                rig.state.drain_action_directory_bridge(&rig.directories);
                let receipt = local_host_answer(pending).await.unwrap();
                assert_eq!(receipt.invocation, request.invocation);
                let event = events.try_recv().unwrap();
                assert_eq!(event.invocation, request.invocation);
                assert_eq!(event.execution, caller.execution_id().unwrap());
                let retry = local_host_answer(host.invoke(&caller, request.clone()))
                    .await
                    .unwrap();
                assert_eq!(retry, receipt);
                let lease = local_host_lease(&rig, &caller, request.invocation).await;
                no_delivery(&rig).await;
                assert!(events.try_recv().is_err(), "retry is not new execution");
                // Rollback cannot keep the original caller certificate alive.
                env.control.set_wall_ns(1);
                env.control.advance_monotonic(Duration::from_secs(7));
                assert!(lease.check().is_err());
                assert!(host.observe(&caller, &request.invocation).is_none());
                assert!(
                    local_host_answer(host.invoke(&caller, request))
                        .await
                        .is_err()
                );
                no_delivery(&rig).await;
                rig.stop().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_refuses_forged_or_rebound_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence) = local_host_evidence(&env);
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let mut forged = evidence.clone();
            *forged.last_mut().unwrap() ^= 1;
            assert!(
                facade
                    .certified_local_action_host(&caller, &forged)
                    .is_err()
            );
            assert!(
                facade
                    .certified_local_action_host(&caller, &vec![0; 16 * 1024 + 1])
                    .is_err()
            );
            let other_room = caller.clone().in_room(uuid::Uuid::new_v4().to_string());
            assert!(
                facade
                    .certified_local_action_host(&other_room, &evidence)
                    .is_err()
            );
            let other_device = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            assert!(
                facade
                    .certified_local_action_host(&other_device, &evidence)
                    .is_err()
            );
            let keyless = tilo_actions::remote::Caller::local_person(principal_id_for_root(
                env.cert.user_public_key,
            ))
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            assert!(
                facade
                    .certified_local_action_host(&keyless, &evidence)
                    .is_err()
            );
            let other_run = caller.clone().in_new_execution();
            assert!(
                local_host_answer(host.invoke(&other_run, local_host_request(true)))
                    .await
                    .is_err()
            );
            let other_person = tilo_actions::remote::Caller::authenticated([3; 32], [4; 32])
                .in_room(&env.route.cluster_id)
                .in_new_execution();
            assert!(
                facade
                    .certified_local_action_host(&other_person, &evidence)
                    .is_err()
            );
            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .clear();
            assert!(
                local_host_answer(host.invoke(&caller, local_host_request(true)))
                    .await
                    .is_err()
            );
            no_delivery(&rig).await;
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_directory_wait_cannot_refresh_original_authority() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for lapse in ["execution", "certificate", "clock"] {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let (caller, evidence) = local_host_evidence(&env);
                let facade =
                    crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
                let host = facade
                    .certified_local_action_host(&caller, &evidence)
                    .unwrap();
                let mut pending = host.invoke(&caller, local_host_request(true));
                assert!(matches!(
                    pending.poll_ready(),
                    tilo_actions::remote::PendingPoll::Waiting
                ));
                match lapse {
                    "execution" => caller.retire_execution(),
                    "certificate" => env
                        .authority
                        .additional_certificates
                        .lock()
                        .unwrap()
                        .clear(),
                    "clock" => env.origin.invalidate(),
                    _ => unreachable!(),
                }
                rig.state.drain_action_directory_bridge(&rig.directories);
                assert!(
                    local_host_answer(pending).await.is_err(),
                    "{lapse} cannot rebind queued work"
                );
                no_delivery(&rig).await;
                rig.stop().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn execution_uuid_requires_stable_delivery_and_preserves_unscoped_requests() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let mut rig = Rig::new(&env).await;
            let row = directory_state("original-r1").borrow().row();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                .await
                .unwrap();
            let (caller, _) = local_host_evidence(&env);
            let mut events = rig.state.subscribe_execution_activity();
            let request = local_host_request(false);
            let receipt = local_host_answer(rig.state.action_invoke(&caller, None, None, &request))
                .await
                .unwrap();
            let delivered =
                tokio::time::timeout(Duration::from_secs(2), rig.stream.as_ref().unwrap().read())
                    .await
                    .expect("scoped stable delivery timed out")
                    .unwrap();
            assert_eq!(delivered.invocation_uuid, *request.invocation.0.as_bytes());
            assert_eq!(receipt.invocation, request.invocation);
            assert_eq!(events.try_recv().unwrap().invocation, request.invocation);
            assert_eq!(
                local_host_answer(rig.state.action_invoke(&caller, None, None, &request))
                    .await
                    .unwrap(),
                receipt
            );
            no_delivery(&rig).await;
            rig.stream = Some(rig.worker.handle.subscribe_invocations().await.unwrap());
            let refused = local_host_answer(rig.state.action_invoke(
                &caller,
                None,
                None,
                &local_host_request(false),
            ))
            .await;
            assert!(
                refused
                    .unwrap_err()
                    .to_string()
                    .contains("stable_identity_unavailable")
            );
            no_delivery(&rig).await;
            // The timed-out client read may still own the server-side recv.
            // Isolate the subsequent positive control on a fresh subscription.
            rig.stream = Some(rig.worker.handle.subscribe_invocations().await.unwrap());
            let legacy = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                *caller.device_public_key().unwrap(),
            )
            .in_room(&env.route.cluster_id);
            let legacy_request = local_host_request(false);
            let legacy_receipt =
                local_host_answer(
                    rig.state
                        .action_invoke(&legacy, None, None, &legacy_request),
                )
                .await
                .unwrap();
            let delivered =
                tokio::time::timeout(Duration::from_secs(2), rig.stream.as_ref().unwrap().read())
                    .await
                    .expect("unscoped legacy delivery timed out")
                    .unwrap();
            assert_eq!(
                delivered.invocation_uuid,
                *legacy_request.invocation.0.as_bytes(),
                "an explicit request identity is independent of execution authority"
            );
            assert_eq!(legacy_receipt.invocation, legacy_request.invocation);
            assert_ne!(
                legacy_request.invocation.0.as_u128(),
                u128::from(delivered.id)
            );
            // A legacy subscriber can still complete using its numeric wire
            // handle. The broker maps it to the original lifecycle identity.
            rig.worker
                .handle
                .complete_invocation(delivered.id, "", "")
                .await
                .unwrap();
            assert!(matches!(
                rig.state.invocation_status(delivered.id),
                Some(tilo_actions::InvocationUpdate::Succeeded { invocation, .. })
                    if invocation == legacy_request.invocation
            ));
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_requires_a_registered_policy_origin() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.authority = CurrentHost::new(&env.route, &env.cert, None);
            let (caller, evidence) = local_host_evidence(&env);
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.clone()),
                None,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let error = state
                .certified_local_action_host(&caller, &evidence)
                .err()
                .expect("a legacy wall clock is not original custody");
            assert_eq!(error, "original policy clock unavailable");
            state.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retired_execution_cannot_receive_a_queued_directory_query() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, _) = local_host_evidence(&env);
            let query = tilo_actions::remote::CatalogueQuery {
                provider: Some("903".into()),
                ..Default::default()
            };
            let mut answer = rig.state.action_query(&caller, &query);
            assert!(matches!(
                answer.poll_ready(),
                tilo_actions::remote::PendingPoll::Waiting
            ));
            caller.retire_execution();
            rig.state.drain_action_directory_bridge(&rig.directories);
            let error = local_host_answer(answer).await.unwrap_err();
            assert!(error.to_string().contains("execution session ended"));
            no_delivery(&rig).await;
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_retains_stricter_foreign_restriction_in_actual_worker_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for directory in [false, true] {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                if !directory {
                    let mut action = directory_state("original-r1")
                        .borrow()
                        .row()
                        .descriptor
                        .unwrap();
                    action.operation_kind = tilo_actions::OperationKind::Effect;
                    action.contract = tilo_actions::ActionContract {
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        ..tilo_actions::ActionContract::default()
                    };
                    rig.worker
                        .handle
                        .publish_actions(&env.route.app_id, &[action])
                        .await
                        .unwrap();
                }
                let (native_caller, evidence) = local_host_evidence(&env);
                let restricted = native_caller
                    .clone()
                    .in_new_execution()
                    .from_foreign_device(true);
                let facade =
                    crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
                let restricted_host = facade
                    .certified_local_action_host(&restricted, &evidence)
                    .unwrap();
                let native_host = facade
                    .certified_local_action_host(&native_caller, &evidence)
                    .unwrap();
                let mut leases = Vec::new();
                for (host, caller) in [
                    (&restricted_host, &restricted),
                    (&native_host, &native_caller),
                ] {
                    let request = local_host_request(directory);
                    let pending = host.invoke(caller, request.clone());
                    rig.state.drain_action_directory_bridge(&rig.directories);
                    local_host_answer(pending).await.unwrap();
                    leases.push(local_host_lease(&rig, caller, request.invocation).await);
                }
                // The certificate itself is native; only the trusted caller carries
                // the stricter foreign restriction. Both initially may write.
                assert!(
                    !env.authority
                        .additional_certificates
                        .lock()
                        .unwrap()
                        .last()
                        .unwrap()
                        .capabilities
                        .foreign
                );
                {
                    use crate::test_support::Grants as _;
                    env.membership.write().workspaces[0].deny(
                        tilo_action_policy::Subject::ForeignDevice,
                        &tilo_action_policy::TreePath::root(),
                        tilo_action_policy::KindSet::UNDOABLE,
                    );
                }
                let mut leases = leases.into_iter();
                assert!(
                    leases.next().unwrap().check().is_err(),
                    "retained restricted write custody must obey the tightened cap"
                );
                leases.next().unwrap().check().unwrap();
                no_delivery(&rig).await;
                rig.stop().await;
            }
        })
        .await;
}

/// On a host that governs the room, a certified caller from another device
/// is held to the foreign-device cap in the edits its effect submits, just as
/// in its own invocation, and so is a stricter restriction the host retained
/// for it: continuing it never makes the caller look like the host itself.
#[tokio::test(flavor = "current_thread")]
async fn a_continuing_certified_caller_keeps_its_foreign_device_cap() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::collab_broker_capnp::{
                ContinuingCaller, published_invocation::InvocationCaller,
            };
            let env = Environment::new();
            env.authority.governs_room.store(true, Ordering::SeqCst);
            let rig = Rig::new(&env).await;
            let room = env.route.cluster_id.clone();
            for (seed, foreign, restricted) in [(94, true, false), (95, false, true)] {
                let (_, evidence) = member_device_evidence(&env, seed, foreign);
                let verified = rig.state.verify_action_caller(&room, &evidence).unwrap();
                assert_eq!(verified.admitted.broker_actor().capped, foreign);
                let continued = ContinuingCaller::from_origin(&InvocationCaller::Certified {
                    caller: Box::new(verified),
                    foreign_restriction: restricted,
                })
                .current(&rig.state)
                .expect("the caller still verifies");
                assert!(
                    rig.state.caller_actor(&continued, &room).capped,
                    "a foreign device (certified {foreign}, restricted {restricted}) stays capped"
                );
            }
            // The control: a device that is not foreign is not capped.
            let (_, evidence) = member_device_evidence(&env, 96, false);
            let verified = rig.state.verify_action_caller(&room, &evidence).unwrap();
            let continued = ContinuingCaller::from_origin(&InvocationCaller::Certified {
                caller: Box::new(verified),
                foreign_restriction: false,
            })
            .current(&rig.state)
            .unwrap();
            assert!(!rig.state.caller_actor(&continued, &room).capped);
            rig.stop().await;
        })
        .await;
}

/// The certified local door's effect edits are admitted for its caller only
/// while that caller's certificate still verifies: revoked mid-run, the
/// running effect continues nothing, as the caller's own invoke would fail.
#[tokio::test(flavor = "current_thread")]
async fn a_certified_local_continuation_ends_with_its_certificate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let row = directory_state("original-r1").borrow().row();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                .await
                .unwrap();
            let (caller, evidence) = local_host_evidence(&env);
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let request = local_host_request(false);
            local_host_answer(host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            let _lease = local_host_lease(&rig, &caller, request.invocation).await;
            let continued = rig
                .state
                .continuing_caller(903, request.invocation)
                .expect("the running invocation continues for its caller");
            assert_eq!(continued.execution_id(), caller.execution_id());
            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .clear();
            assert!(
                rig.state
                    .continuing_caller(903, request.invocation)
                    .is_none(),
                "a revoked certificate continues nothing"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_person_context_uses_signed_owner_device_and_current_execution() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.cluster_id = collab_types::person_room(&env.cert.user_public_key).to_string();
            env.membership.write().workspaces[0].room =
                tilo_desktop_model::RoomId::parse(&env.route.cluster_id).unwrap();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.clone()),
                None,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let (caller, evidence) = local_host_evidence(&env);
            let context = state.certified_person_context(&caller, &evidence).unwrap();
            assert_eq!(context.principal(), hex::encode(env.cert.user_public_key));
            assert_eq!(
                context.device(),
                hex::encode(caller.device_public_key().unwrap())
            );
            context.revalidate().unwrap();
            let forged = tilo_actions::remote::Caller::local_person(context.principal())
                .in_room(&env.route.cluster_id)
                .in_new_execution();
            assert!(state.certified_person_context(&forged, &evidence).is_err());
            assert!(
                state
                    .certified_person_context(&caller.clone().from_foreign_device(true), &evidence)
                    .is_err()
            );
            caller.retire_execution();
            assert!(context.revalidate().is_err());
            let (fresh, evidence) = member_device_evidence(&env, 94, false);
            let context = state.certified_person_context(&fresh, &evidence).unwrap();
            context.revalidate().unwrap();
            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .clear();
            assert!(context.revalidate().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_person_context_refuses_other_rooms_and_expired_original_policy() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.cluster_id = uuid::Uuid::new_v4().to_string();
            env.membership.write().workspaces[0].room =
                tilo_desktop_model::RoomId::parse(&env.route.cluster_id).unwrap();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));

            let fanout = Arc::new(FanoutState::new());
            let (ordinary, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.clone()),
                None,
            );
            assert!(
                ordinary
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let (caller, evidence) = local_host_evidence(&env);
            ordinary
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            assert!(
                ordinary
                    .certified_person_context(&caller, &evidence)
                    .is_err()
            );
            env.route.cluster_id = collab_types::person_room(&env.cert.user_public_key).to_string();
            env.membership.write().workspaces[0].room =
                tilo_desktop_model::RoomId::parse(&env.route.cluster_id).unwrap();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let (state, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                env.publisher.clone(),
                Some(env.membership.clone()),
                None,
            );
            assert!(
                state
                    .certificate_authority
                    .set(env.authority.clone())
                    .is_ok()
            );
            let (caller, evidence) = member_device_evidence(&env, 95, false);
            let context = state.certified_person_context(&caller, &evidence).unwrap();
            context.revalidate().unwrap();
            env.control.advance_monotonic(Duration::from_secs(7));
            assert!(context.revalidate().is_err());
            assert!(state.certified_person_context(&caller, &evidence).is_err());
        })
        .await;
}

/// Opening verifies the presentation once; one listing and one invocation
/// admission each verify it once more, the invocation's dispatch reusing the
/// admission its own check just made. Anything later verifies again, so a
/// revoked device still fails the very next call.
#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_verifies_once_per_open_listing_and_invocation() {
    use crate::collab_broker_capnp::admission_memo::verifications_on_this_thread as verifications;
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let row = directory_state("original-r1").borrow().row();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                .await
                .unwrap();
            let (caller, evidence) = local_host_evidence(&env);
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());

            let before = verifications();
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            assert_eq!(
                verifications().saturating_sub(before),
                1,
                "opening verifies once"
            );

            let before = verifications();
            let page = local_host_answer(host.query(
                &caller,
                tilo_actions::remote::CatalogueQuery {
                    max_results: 0,
                    ..tilo_actions::remote::CatalogueQuery::default()
                },
            ))
            .await
            .unwrap();
            assert!(!page.revision.is_empty());
            assert_eq!(
                verifications().saturating_sub(before),
                1,
                "a listing verifies once"
            );

            let request = local_host_request(false);
            let before = verifications();
            let pending = host.invoke(&caller, request.clone());
            let receipt = local_host_answer(pending).await.unwrap();
            assert_eq!(
                verifications().saturating_sub(before),
                1,
                "the queued invocation admission and its dispatch verify once"
            );
            assert_eq!(receipt.invocation, request.invocation);
            let _lease = local_host_lease(&rig, &caller, request.invocation).await;

            // The admission lent to that dispatch ended with the call.
            let before = verifications();
            local_host_answer(host.query(
                &caller,
                tilo_actions::remote::CatalogueQuery {
                    max_results: 0,
                    ..tilo_actions::remote::CatalogueQuery::default()
                },
            ))
            .await
            .unwrap();
            assert_eq!(
                verifications().saturating_sub(before),
                1,
                "a later check verifies again"
            );

            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .clear();
            assert!(
                local_host_answer(host.invoke(&caller, local_host_request(false)))
                    .await
                    .is_err(),
                "an unenrolled device fails the very next invocation"
            );
            assert!(
                facade
                    .certified_local_action_host(&caller.clone().in_new_execution(), &evidence)
                    .is_err(),
                "and the very next open"
            );
            no_delivery(&rig).await;
            rig.stop().await;
        })
        .await;
}

/// The real signer waits for a separate UI thread to perform its ordinary
/// lookup, cancellation and enqueue calls. A ledger held across signing would
/// deadlock that thread before it can acknowledge the gate.
#[tokio::test(flavor = "current_thread")]
async fn stalled_eager_signing_keeps_ui_enqueue_responsive_and_uuid_reserved() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let row = directory_state("original-r1").borrow().row();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                .await
                .unwrap();
            let (caller, evidence) = local_host_evidence(&env);
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            let host = facade
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let request = local_host_request(false);
            let mut refused = request.clone();
            refused.target.address.action = "missing".into();
            assert!(
                local_host_answer(host.invoke(&caller, refused))
                    .await
                    .is_err(),
                "failed admission must abandon its UUID reservation"
            );
            let fresh = local_host_request(false);
            let state = rig.state.clone();
            let ui_caller = caller.clone();
            let ui_request = request.clone();
            let ui_fresh = fresh.clone();
            let (send, receive) = std::sync::mpsc::channel();
            let (done, completed) = std::sync::mpsc::channel();
            let ui = std::thread::spawn(move || {
                let facade =
                    crate::collab_broker::AppActionBroker::from_state_for_test(state.clone());
                let host = facade
                    .certified_local_action_host(&ui_caller, &evidence)
                    .unwrap();
                receive.recv().unwrap();
                assert!(
                    state
                        .remote_job(&ui_caller, &ui_request.invocation)
                        .is_none()
                );
                assert!(matches!(
                    host.cancel(&ui_caller, &ui_request.invocation),
                    Err(tilo_actions::remote::ActionHostError::UnknownInvocation)
                ));
                // Keep both pending answers alive: the duplicate must reuse the
                // eventual job; the fresh UUID must enqueue without waiting.
                let retry = host.invoke(&ui_caller, ui_request);
                let next = host.invoke(&ui_caller, ui_fresh);
                done.send((retry, next)).unwrap();
            });
            let (answers, queued) = std::sync::mpsc::channel();
            crate::evidence_signer::BEFORE_SIGN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    send.send(()).unwrap();
                    let pending = completed
                        .recv_timeout(Duration::from_secs(2))
                        .expect("UI lookup/cancel/enqueue must finish while signing is stalled");
                    answers.send(pending).unwrap();
                }));
            });
            let original = local_host_answer(host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            let (retry, next) = queued.recv().unwrap();
            ui.join().unwrap();
            let retried = local_host_answer(retry).await.unwrap();
            assert_eq!(original, retried);
            assert_eq!(
                local_host_answer(next).await.unwrap().invocation,
                fresh.invocation
            );
            let _first = local_host_lease(&rig, &caller, request.invocation).await;
            let _second = local_host_lease(&rig, &caller, fresh.invocation).await;
            no_delivery(&rig).await;
            assert!(rig.state.remote_job(&caller, &request.invocation).is_some());
            assert!(rig.state.remote_job(&caller, &fresh.invocation).is_some());
            rig.stop().await;
        })
        .await;
}

/// A tampered proof is never mistaken for the one that verified before.
#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_remembers_only_the_exact_verified_proof() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence) = local_host_evidence(&env);
            let facade =
                crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
            for _ in 0..2 {
                facade
                    .certified_local_action_host(&caller.clone().in_new_execution(), &evidence)
                    .unwrap();
            }
            let presentation = runtime_collab::CertifiedJoinIdentity::decode(&evidence)
                .unwrap()
                .unwrap();
            for byte in [0, 31, 63] {
                let mut forged = presentation.clone();
                forged.proof[byte] ^= 0x01;
                assert!(
                    facade
                        .certified_local_action_host(
                            &caller.clone().in_new_execution(),
                            &forged.encode().unwrap(),
                        )
                        .is_err(),
                    "proof byte {byte} flipped"
                );
            }
            let mut other_room = presentation;
            other_room.workspace_id = uuid::Uuid::new_v4().to_string();
            assert!(
                facade
                    .certified_local_action_host(
                        &caller
                            .clone()
                            .in_room(&other_room.workspace_id)
                            .in_new_execution(),
                        &other_room.encode().unwrap(),
                    )
                    .is_err(),
                "a proof for one room never admits another"
            );
            rig.stop().await;
        })
        .await;
}

/// Wall time of one certified open + provider listing and of one certified
/// open + eager invocation admission, at workspace histories of 0, 1,000
/// and 10,000 effect receipts. Prints the numbers (`--nocapture`); the
/// counting tests are the regression guard.
#[tokio::test(flavor = "current_thread")]
async fn certified_local_host_open_list_and_invoke_cost() {
    const LISTINGS: u32 = 100;
    const INVOKES: u32 = 20;
    tokio::task::LocalSet::new()
        .run_until(async {
            for receipts in [0_u128, 1_000, 10_000] {
                let env = Environment::new();
                {
                    let mut tree = env.membership.write();
                    let workspace = &mut tree.workspaces[0];
                    for n in 0..receipts {
                        let context = tilo_actions::ActionContext {
                            purpose: Some(format!("Rename document {n}")),
                            ..tilo_actions::ActionContext::default()
                        };
                        workspace.effect_receipts.push(
                            tilo_desktop_model::WorkspaceEffectReceipt {
                                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                                    u128::MAX.saturating_sub(n),
                                )),
                                actor: "person".into(),
                                context,
                                status: tilo_actions::EffectReceiptStatus::Succeeded,
                                completed_at: 1,
                            },
                        );
                    }
                }
                let rig = Rig::new(&env).await;
                let row = directory_state("original-r1").borrow().row();
                rig.worker
                    .handle
                    .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                    .await
                    .unwrap();
                let (caller, evidence) = local_host_evidence(&env);
                let facade =
                    crate::collab_broker::AppActionBroker::from_state_for_test(rig.state.clone());
                let mut listing = Duration::ZERO;
                for round in 0..=LISTINGS {
                    let caller = caller.clone().in_new_execution();
                    let started = std::time::Instant::now();
                    let host = facade
                        .certified_local_action_host(&caller, &evidence)
                        .unwrap();
                    let page = local_host_answer(host.query(
                        &caller,
                        tilo_actions::remote::CatalogueQuery {
                            max_results: 0,
                            ..tilo_actions::remote::CatalogueQuery::default()
                        },
                    ))
                    .await
                    .unwrap();
                    // The first round warms caches and is not counted.
                    if round > 0 {
                        listing = listing.saturating_add(started.elapsed());
                    }
                    assert!(!page.revision.is_empty());
                }
                let mut invoking = Duration::ZERO;
                for _ in 0..INVOKES {
                    let caller = caller.clone().in_new_execution();
                    let request = local_host_request(false);
                    let started = std::time::Instant::now();
                    let host = facade
                        .certified_local_action_host(&caller, &evidence)
                        .unwrap();
                    let pending = host.invoke(&caller, request.clone());
                    invoking = invoking.saturating_add(started.elapsed());
                    let receipt = local_host_answer(pending).await.unwrap();
                    assert_eq!(receipt.invocation, request.invocation);
                    let _lease = local_host_lease(&rig, &caller, request.invocation).await;
                }
                eprintln!(
                    "#3911 certified local host, {receipts} receipts: open+list {:?}/call, \
                     open+invoke admission {:?}/call",
                    listing.checked_div(LISTINGS).unwrap_or_default(),
                    invoking.checked_div(INVOKES).unwrap_or_default(),
                );
                rig.stop().await;
            }
        })
        .await;
}
