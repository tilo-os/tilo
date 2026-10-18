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
