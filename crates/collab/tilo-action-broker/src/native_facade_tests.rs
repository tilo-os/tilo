#[derive(Default)]
struct NativeFacadeHostSteps(Mutex<Vec<(String, String)>>);

impl NativeFacadeHostSteps {
    fn descriptor(resource: &str, action: &str) -> Vec<tilo_actions::ActionDescriptor> {
        let mut descriptor = directory_state("native-host-step-r1")
            .borrow()
            .row()
            .descriptor
            .unwrap()
            .descriptor(tilo_actions::ExecutionLocus::default());
        descriptor.address = tilo_actions::ActionAddress::new(resource, action);
        descriptor.resource_kind = resource.into();
        descriptor.contract = tilo_actions::ActionContract::view();
        vec![descriptor]
    }
}

impl crate::services::IdentityAdminServices for NativeFacadeHostSteps {
    fn app_id(&self) -> &str {
        "so.tilo.settings"
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        use tilo_actions::remote::identity_admin;
        Self::descriptor(identity_admin::RESOURCE, identity_admin::LIST)
    }
    fn start(
        &self,
        request: &crate::services::IdentityAdminRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::services::IdentityAdminOutcome>, String> {
        self.0
            .lock()
            .unwrap()
            .push((request.room.into(), request.actor.id().into()));
        Ok(tilo_actions::remote::Pending::ready(Ok(Default::default())))
    }
}

impl crate::services::DevicePairingServices for NativeFacadeHostSteps {
    fn app_id(&self) -> &str {
        "so.tilo.settings"
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        use tilo_actions::remote::device_pairing;
        Self::descriptor(device_pairing::RESOURCE, device_pairing::DISCOVER)
    }
    fn start(
        &self,
        request: &crate::services::DevicePairingRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::services::DevicePairingOutcome>, String> {
        self.0
            .lock()
            .unwrap()
            .push((request.room.into(), request.actor.id().into()));
        Ok(tilo_actions::remote::Pending::ready(Ok(Default::default())))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signed_native_facade_runs_host_steps_with_original_certified_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use tilo_actions::remote::{device_pairing, identity_admin};
            let env = Environment::new();
            let steps = Arc::new(NativeFacadeHostSteps::default());
            let rig = Rig::with_services(
                &env,
                HostServices {
                    local_person: Some(Arc::new(NativePersonFixture(env.authority.clone()))),
                    identity_admin: Some(steps.clone()),
                    device_pairing: Some(steps.clone()),
                    ..env.membership.services()
                },
            )
            .await;
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            let evidence = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let native = rig
                .state
                .certified_native_action_host(&caller, &evidence)
                .unwrap();
            let original = rig
                .state
                .verify_action_caller(&env.route.cluster_id, &evidence)
                .unwrap();
            for target in [
                identity_admin::target(identity_admin::LIST),
                device_pairing::target(device_pairing::DISCOVER),
            ] {
                let mut request = local_host_request(false);
                request.target = target;
                local_host_answer(native.invoke(&caller, request.clone()))
                    .await
                    .unwrap();
                assert!(
                    native
                        .observe(&caller, &request.invocation)
                        .unwrap()
                        .is_terminal()
                );
                let record = rig
                    .state
                    .verified_invocation_status(&original, request.invocation)
                    .unwrap()
                    .expect("native host step remains owned by the signed certificate");
                assert!(matches!(
                    record.update,
                    tilo_actions::InvocationUpdate::Succeeded { .. }
                ));
                // Replay must use the original durable admission, not repeat the service.
                local_host_answer(native.invoke(&caller, request))
                    .await
                    .unwrap();
            }
            assert_eq!(
                *steps.0.lock().unwrap(),
                vec![
                    (
                        env.route.cluster_id.clone(),
                        original.admitted.principal_id.clone()
                    );
                    2
                ],
            );
            *env.authority.certificate.lock().unwrap() = None;
            let mut request = local_host_request(false);
            request.target = identity_admin::target(identity_admin::LIST);
            assert!(
                local_host_answer(native.invoke(&caller, request))
                    .await
                    .is_err()
            );
            assert_eq!(steps.0.lock().unwrap().len(), 2);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn keyless_certified_host_steps_require_a_verified_native_projection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::collab_broker_capnp::{
                host_steps::is_local_host_caller, published_invocation::InvocationCaller,
            };
            use tilo_actions::remote::Caller;
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (signed, evidence) = local_host_evidence(&env);
            let original = rig
                .state
                .verify_action_caller(&env.route.cluster_id, &evidence)
                .unwrap();
            assert!(!original.native_projection_verified);
            let person = original.admitted.principal_id.clone();
            let projected = Caller::local_person(&person).in_room(&env.route.cluster_id);
            let origin = |caller, foreign_restriction| InvocationCaller::Certified {
                caller: Box::new(caller),
                foreign_restriction,
            };
            assert!(is_local_host_caller(
                &signed,
                Some(&origin(original.clone(), false))
            ));
            assert!(
                !is_local_host_caller(&projected, Some(&origin(original.clone(), false))),
                "keyless Certified is not itself evidence of native seat custody"
            );
            // Isolate the matcher after the protected-seat verification bit:
            // it must still reject mismatched or restricted projections.
            let mut checked = original;
            checked.native_projection_verified = true;
            assert!(is_local_host_caller(
                &projected,
                Some(&origin(checked.clone(), false))
            ));
            for caller in [
                Caller::local_person("other-person").in_room(&env.route.cluster_id),
                Caller::local_person(&person).in_room("other-room"),
                projected.clone().from_foreign_device(true),
                projected.clone().delegated_to("agent"),
            ] {
                assert!(!is_local_host_caller(
                    &caller,
                    Some(&origin(checked.clone(), false))
                ));
            }
            assert!(!is_local_host_caller(
                &projected,
                Some(&origin(checked.clone(), true))
            ));
            let mut missing_certificate = checked.clone();
            missing_certificate.admitted.certificate = None;
            assert!(!is_local_host_caller(
                &projected,
                Some(&origin(missing_certificate, false))
            ));
            let mut foreign_certificate = checked.clone();
            foreign_certificate
                .admitted
                .certificate
                .as_mut()
                .unwrap()
                .capabilities
                .foreign = true;
            assert!(!is_local_host_caller(
                &projected,
                Some(&origin(foreign_certificate, false))
            ));
            let mut missing_presentation = checked.clone();
            missing_presentation.presentation.clear();
            assert!(!is_local_host_caller(
                &projected,
                Some(&origin(missing_presentation, false))
            ));
            let delegated = checked.delegated_to("agent");
            assert!(!delegated.native_projection_verified);
            assert!(!is_local_host_caller(
                &projected,
                Some(&origin(delegated, false))
            ));
            rig.stop().await;
        })
        .await;
}

#[derive(Default)]
struct NativeFacadeGovernance(Mutex<Vec<(bool, tilo_actions::InvocationId)>>);
impl crate::services::WorkspaceGovernanceServices for NativeFacadeGovernance {
    fn app_id(&self) -> &str {
        "so.tilo.workspace"
    }
    fn handles(&self, action: &str) -> bool {
        action == "inspect-native-origin"
    }
    fn descriptors(
        &self,
        _: &crate::test_support::Workspace,
    ) -> Vec<tilo_actions::ActionDescriptor> {
        let mut descriptor = directory_state("native-r1")
            .borrow()
            .row()
            .descriptor
            .unwrap();
        descriptor.address = tilo_actions::ActionAddress::new("workspace", "inspect-native-origin");
        descriptor.contract = tilo_actions::ActionContract::view();
        vec![descriptor]
    }
    fn invoke(
        &self,
        request: &crate::services::WorkspaceGovernanceRequest<'_>,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((request.local_device, request.invocation));
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signed_native_facade_preserves_physical_intent_without_lending_it_to_other_doors() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let governance = Arc::new(NativeFacadeGovernance::default());
            let rig = Rig::with_services(
                &env,
                HostServices {
                    local_person: Some(Arc::new(NativePersonFixture(env.authority.clone()))),
                    workspace_governance: Some(governance.clone()),
                    ..env.membership.services()
                },
            )
            .await;
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_room(&env.route.cluster_id)
            .in_new_execution();
            let evidence = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let ordinary = rig
                .state
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let native = rig
                .state
                .certified_native_action_host(&caller, &evidence)
                .unwrap();
            let mut request = local_host_request(false);
            request.target = tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::workspace_provider(&env.route.cluster_id),
                address: tilo_actions::ActionAddress::new("workspace", "inspect-native-origin"),
            };
            let ordinary_id = request.invocation;
            local_host_answer(ordinary.invoke(&caller, request.clone()))
                .await
                .unwrap();
            request.invocation = tilo_actions::InvocationId::new();
            let native_id = request.invocation;
            local_host_answer(native.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(
                *governance.0.lock().unwrap(),
                vec![(false, ordinary_id), (true, native_id)]
            );
            assert!(
                native.observe(&caller, &native_id).is_some(),
                "same signed owner observes its native completion"
            );
            assert!(ordinary.observe(&caller, &ordinary_id).is_some());
            *env.authority.certificate.lock().unwrap() = None;
            request.invocation = tilo_actions::InvocationId::new();
            assert!(
                local_host_answer(native.invoke(&caller, request))
                    .await
                    .is_err()
            );
            assert!(
                local_host_answer(native.query(&caller, Default::default()))
                    .await
                    .is_err()
            );
            assert_eq!(
                governance.0.lock().unwrap().len(),
                2,
                "a refused native seat never falls back"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn signed_caller_without_protected_seat_cannot_acquire_physical_intent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let (caller, evidence) = local_host_evidence(&env);
            assert!(
                rig.state
                    .certified_local_action_host(&caller, &evidence)
                    .is_ok()
            );
            assert!(
                rig.state
                    .certified_native_action_host(&caller, &evidence)
                    .is_err()
            );
            rig.stop().await;
        })
        .await;
}
