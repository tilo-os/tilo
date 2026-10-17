// Eager admission must not run on the authenticated caller's UI turn.
mod eager_bridge_tests {
    use super::*;
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
    };

    fn request(fixture: &LocalDirectoryFixture) -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: fixture.peer_id.to_string(),
                address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn offer(fixture: &LocalDirectoryFixture) {
        fixture.state.surfaces.lock().unwrap().insert(
            fixture.peer_id,
            PublishedSurface {
                peer_id: fixture.peer_id,
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                actions: vec![
                    published_directory_action(&fixture.directory_state.borrow().row()).unwrap(),
                ],
                live: true,
            },
        );
    }

    // Deliberately missing its original certificate: dispatch must revalidate
    // this retained custody, not turn the queue into an admission bypass.
    fn uncertified_origin(fixture: &LocalDirectoryFixture) -> InvocationCaller {
        InvocationCaller::Local {
            route: fixture.route.clone(),
            admitted: Box::new(AdmittedJoinIdentity {
                host_scope: None,
                worker_registration: None,
                certificate: None,
                verified: VerifiedJoinIdentity {
                    user_public_key: [0; 32],
                    device_public_key: [0; 32],
                    capped: false,
                },
                principal_id: "fixture-owner".into(),
                display_handle: None,
                role: CapRole::Editor,
                agent_id: None,
                session_delegated: false,
            }),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authenticated_eager_admission_waits_for_broker_and_revalidates_custody() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut fixture = local_directory_fixture();
                offer(&fixture);
                let caller =
                    Caller::local_person("fixture-owner").in_room(fixture.cluster_id.clone());
                let request = request(&fixture);
                let origin = uncertified_origin(&fixture);
                let mut response = fixture.state.action_invoke_with_origin(
                    &caller,
                    None,
                    None,
                    Some(&origin),
                    &request,
                );
                assert_eq!(response.poll_ready(), PendingPoll::Waiting);
                assert!(
                    fixture
                        .state
                        .remote_job(&caller, &request.invocation)
                        .is_none()
                );
                assert!(fixture.take_invocation_identity().is_none());
                // Even scheduling other tasks cannot admit work before the drain.
                tokio::task::yield_now().await;
                assert_eq!(response.poll_ready(), PendingPoll::Waiting);
                drain_directory_bridge(&fixture).await;
                assert!(matches!(response.poll_ready(), PendingPoll::Ready(Err(_))));
                assert!(fixture.take_invocation_identity().is_none());

                // Legacy/system calls still admit immediately, without a drain.
                let request = self::request(&fixture);
                let mut legacy = fixture.state.action_invoke(&caller, None, None, &request);
                assert!(matches!(legacy.poll_ready(), PendingPoll::Ready(Ok(_))));
                assert_eq!(
                    fixture.take_invocation_identity().unwrap().1,
                    request.invocation
                );
            })
            .await;
    }

    #[test]
    fn authenticated_eager_queue_is_bounded_without_admission() {
        let mut fixture = local_directory_fixture();
        offer(&fixture);
        let caller = Caller::local_person("fixture-owner").in_room(fixture.cluster_id.clone());
        let origin = uncertified_origin(&fixture);
        let mut waiting = Vec::new();
        for _ in 0..ActionDirectoryRequestQueue::CAPACITY {
            let request = request(&fixture);
            let mut response = fixture.state.action_invoke_with_origin(
                &caller,
                None,
                None,
                Some(&origin),
                &request,
            );
            assert_eq!(response.poll_ready(), PendingPoll::Waiting);
            assert!(
                fixture
                    .state
                    .remote_job(&caller, &request.invocation)
                    .is_none()
            );
            waiting.push(response);
        }
        let mut refused = fixture.state.action_invoke_with_origin(
            &caller,
            None,
            None,
            Some(&origin),
            &request(&fixture),
        );
        assert!(matches!(
            refused.poll_ready(),
            PendingPoll::Ready(Err(
                tilo_actions::remote::ActionHostError::Unavailable { .. }
            ))
        ));
        assert!(fixture.take_invocation_identity().is_none());
    }
}
