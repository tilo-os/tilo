/// One host-step service under `provider`, offering `action` on
/// `resource`; every step it starts is recorded in the shared log.
struct NativeFacadeHostSteps {
    provider: &'static str,
    resource: &'static str,
    action: &'static str,
    started: Arc<Mutex<Vec<(String, tilo_actions::Actor)>>>,
}

impl tilo_actions::remote::ActionProvider for NativeFacadeHostSteps {
    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::admitted::offered(
            self.provider,
            "so.tilo.settings",
            tilo_actions::remote::admitted::top_level(self.descriptors()),
            &query,
        )
    }
    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        self.run(caller, request, tilo_actions::remote::Admitted::default())
    }
    fn run(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::admitted::settled(
            request.invocation,
            self.start(caller, &request, &admitted),
        )
    }
    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }
    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

impl NativeFacadeHostSteps {
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        let mut descriptor = directory_state("native-host-step-r1")
            .borrow()
            .row()
            .descriptor
            .unwrap()
            .descriptor(tilo_actions::ExecutionLocus::default());
        descriptor.address = tilo_actions::ActionAddress::new(self.resource, self.action);
        descriptor.resource_kind = self.resource.into();
        descriptor.contract = tilo_actions::ActionContract::view();
        vec![descriptor]
    }

    fn start(
        &self,
        caller: &tilo_actions::remote::Caller,
        _request: &tilo_actions::remote::InvokeRequest,
        _admitted: &tilo_actions::remote::Admitted,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::Outcome>, String> {
        let workspace = caller
            .workspace()
            .ok_or("the step was asked in no workspace")?;
        self.started.lock().unwrap().push((
            workspace.into(),
            tilo_broker_core::BrokerActor::for_caller(caller)
                .actor()
                .clone(),
        ));
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signed_native_facade_runs_host_steps_with_original_certified_ownership() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use tilo_server_protocol::device_tickets;
            let env = Environment::new(true);
            let started = Arc::new(Mutex::new(Vec::new()));
            let providers = tilo_workspace_service::workspaces::providers::Providers::default();
            providers.provide(
                device_tickets::PROVIDER,
                "so.tilo.settings",
                tilo_workspace_service::workspaces::providers::Reach::ThisDevice,
                Arc::new(NativeFacadeHostSteps {
                    provider: device_tickets::PROVIDER,
                    resource: device_tickets::RESOURCE,
                    action: device_tickets::IDENTITIES,
                    started: started.clone(),
                }) as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
            );
            let rig = Rig::with_services(
                &env,
                ProcessServices {
                    local_person: Some(Arc::new(super::signed_worker::NativePerson(
                        env.authority.clone(),
                    ))),
                    providers: Some(providers),
                    ..env.membership.services()
                },
            )
            .await;
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_workspace(&env.route.cluster_id)
            .in_new_execution();
            let evidence = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let native = rig
                .state
                .certified_native_action_provider(&caller, &evidence)
                .unwrap();
            let original = rig
                .state
                .verify_action_caller(&env.route.cluster_id, &evidence)
                .unwrap();
            for target in [device_tickets::target(device_tickets::IDENTITIES)] {
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
                *started.lock().unwrap(),
                vec![(
                    env.route.cluster_id.clone(),
                    tilo_actions::Actor::from(tilo_actions::MemberId::person(
                        original.admitted.actor_key.clone()
                    ))
                )],
            );
            let started_before = started.lock().unwrap().len();
            *env.authority.certificate.lock().unwrap() = None;
            let mut request = local_host_request(false);
            request.target = device_tickets::target(device_tickets::IDENTITIES);
            assert!(
                local_host_answer(native.invoke(&caller, request))
                    .await
                    .is_err()
            );
            assert_eq!(
                started.lock().unwrap().len(),
                started_before,
                "a call refused once the certificate is withdrawn starts no host step"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn keyless_certified_host_steps_require_a_verified_native_projection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::collab_app_broker::{
                provided::is_local_host_caller, published_invocation::InvocationCaller,
            };
            use tilo_actions::remote::Caller;
            let env = Environment::new(true);
            let rig = Rig::new(&env).await;
            let (signed, evidence) = local_host_evidence(&env);
            let original = rig
                .state
                .verify_action_caller(&env.route.cluster_id, &evidence)
                .unwrap();
            assert!(!original.native_projection_verified);
            let person = original.admitted.actor_key.clone();
            let projected = Caller::local_person(&person).in_workspace(&env.route.cluster_id);
            let origin = |caller, foreign_restriction| InvocationCaller::Certified {
                caller: Box::new(caller),
                foreign_restriction,
            };
            assert!(is_local_host_caller(
                &signed,
                Some(&origin(original.clone(), false))
            ));
            assert!(
                !is_local_host_caller(&signed, None),
                "a device's key with no admission behind it is not this machine's caller"
            );
            assert!(
                !is_local_host_caller(&projected, Some(&origin(original.clone(), false))),
                "keyless Certified is not itself evidence of native device identity"
            );
            // Isolate the matcher after the protected-device verification bit:
            // it must still reject mismatched or restricted projections.
            let mut checked = original;
            checked.native_projection_verified = true;
            assert!(is_local_host_caller(
                &projected,
                Some(&origin(checked.clone(), false))
            ));
            for caller in [
                Caller::local_person("other-person").in_workspace(&env.route.cluster_id),
                Caller::local_person(&person).in_workspace("other-workspace"),
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
            rig.stop().await;
        })
        .await;
}

#[derive(Default)]
struct NativeFacadeGovernance(Mutex<Vec<(bool, tilo_actions::InvocationId)>>);
impl crate::services::WorkspaceGovernanceServices for NativeFacadeGovernance {
    fn app_id(&self) -> &'static str {
        "tilo.workspace"
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
            .unwrap()
            .descriptor(tilo_actions::ExecutionLocus::default());
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
async fn signed_native_facade_preserves_physical_intent_without_lending_it_to_other_apis() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new(true);
            let governance = Arc::new(NativeFacadeGovernance::default());
            let rig = Rig::with_services(
                &env,
                ProcessServices {
                    local_person: Some(Arc::new(super::signed_worker::NativePerson(
                        env.authority.clone(),
                    ))),
                    workspace_governance: Some(governance.clone()),
                    ..env.membership.services()
                },
            )
            .await;
            let caller = tilo_actions::remote::Caller::authenticated(
                env.cert.user_public_key,
                env.cert.device_public_key,
            )
            .in_workspace(&env.route.cluster_id)
            .in_new_execution();
            let evidence = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let ordinary = rig
                .state
                .certified_local_action_provider(&caller, &evidence, false)
                .unwrap();
            let native = rig
                .state
                .certified_native_action_provider(&caller, &evidence)
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
                local_host_answer(
                    native.query(&caller, tilo_actions::remote::CatalogueQuery::default())
                )
                .await
                .is_err()
            );
            assert_eq!(
                governance.0.lock().unwrap().len(),
                2,
                "a refused native device never falls back"
            );
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn signed_caller_without_protected_local_person_cannot_acquire_physical_intent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new(true);
            let rig = Rig::new(&env).await;
            let (caller, evidence) = local_host_evidence(&env);
            assert!(
                rig.state
                    .certified_local_action_provider(&caller, &evidence, false)
                    .is_ok()
            );
            assert!(
                rig.state
                    .certified_native_action_provider(&caller, &evidence)
                    .is_err()
            );
            rig.stop().await;
        })
        .await;
}
