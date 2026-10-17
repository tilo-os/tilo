// An admitted offloadable invocation runs where the device's route places
// it: answered by the door with another device's output, or handed to the
// app, whose own answer stands and whose run time the route hears.

/// A route that answers what it is told to, recording what it was asked.
#[derive(Default)]
struct ScriptedRoute {
    answers: Mutex<std::collections::VecDeque<tilo_actions::offload::Offloaded>>,
    asked: Mutex<Vec<tilo_actions::offload::OffloadRequest>>,
    ran_here: Mutex<Vec<tilo_actions::offload::OffloadRequest>>,
}

impl tilo_actions::offload::ActionOffload for ScriptedRoute {
    fn offload(
        &self,
        request: tilo_actions::offload::OffloadRequest,
    ) -> tilo_actions::offload::OffloadFuture<'_> {
        self.asked.lock().unwrap().push(request);
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted answer");
        Box::pin(async move { answer })
    }

    fn ran_here(&self, request: &tilo_actions::offload::OffloadRequest, _: std::time::Duration) {
        self.ran_here.lock().unwrap().push(request.clone());
    }
}

#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one invocation journey, placed elsewhere then here, that reads top to bottom"
)]
async fn an_admitted_offloadable_invocation_runs_where_the_route_places_it() {
    use tilo_actions::offload::{OffloadRequest, Offloaded};
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let route = Arc::new(ScriptedRoute::default());
            let base = local_directory_fixture();
            let membership = base.membership.as_ref().unwrap().clone();
            drop(base);
            let services = HostServices {
                action_offload: Some(route.clone()),
                ..membership.services()
            };
            let mut fixture = local_directory_fixture_with_authority(
                membership.authority(),
                services,
                Some(71),
                None,
            );
            fixture.membership = Some(membership);
            // A content transform published as Photos publishes its
            // thumbnails: offloadable, its inputs a list of content.
            let declared = tilo_actions::Offload {
                app: "so.tilo.thumbnails".into(),
                entrypoint: "thumbnail-batch".into(),
                gpu: false,
                cpu_time_ms: 120_000,
                memory_bytes: 1024 * 1024 * 1024,
                output_bytes: 16 * 1024 * 1024,
            };
            let mut action =
                published_directory_action(&fixture.directory_state.borrow().row()).unwrap();
            action.contract.result = tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.into(),
            };
            action.contract.offload = Some(declared.clone());
            assert!(action.contract.validate_declaration().is_empty());
            action.params = vec![
                tilo_actions::ActionParam::required(
                    "photos",
                    tilo_actions::ParamType::Array,
                    "The photographs",
                )
                .as_content_list(),
            ];
            let offered = PublishedSurface {
                peer_id: fixture.peer_id,
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                actions: vec![action.clone()],
                live: true,
            };
            fixture
                .state
                .surfaces
                .lock()
                .unwrap()
                .insert(fixture.peer_id, offered.clone());
            let photo = "a".repeat(64);
            let thumbnail = tilo_actions::content_resource(&"b".repeat(64)).unwrap();
            let invoke = |fixture: &LocalDirectoryFixture| {
                let caller =
                    Caller::local_person("fixture-owner").in_room(fixture.cluster_id.clone());
                let request = InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: fixture.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new(
                            &action.resource,
                            &action.action,
                        ),
                    },
                    argument: InvokeArgument::Json(
                        serde_json::json!({ "photos": [photo.clone()] })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    context: None,
                    confirmation: None,
                    revision: None,
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                };
                let mut pending = fixture.state.action_invoke(&caller, None, None, &request);
                let receipt = match pending.poll_ready() {
                    PendingPoll::Ready(Ok(receipt)) => receipt,
                    PendingPoll::Ready(Err(error)) => {
                        panic!("an offloadable action is admitted as any other: {error:?}")
                    }
                    PendingPoll::Waiting | PendingPoll::Abandoned => {
                        panic!("admission answers at once")
                    }
                };
                assert_eq!(receipt.invocation, request.invocation);
                request.invocation
            };
            let job_of = |fixture: &LocalDirectoryFixture, invocation| {
                fixture
                    .state
                    .pending_invocations
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(_, pending)| {
                        pending.update
                            == tilo_actions::InvocationUpdate::Accepted { invocation }
                    })
                    .map(|(id, _)| *id)
                    .expect("admitted and pending")
            };

            // Another device is better placed: the door answers with its
            // output, and the app is never handed the invocation.
            let remote = tilo_actions::ActionOutput {
                value: None,
                resources: vec![thumbnail.clone()],
            };
            route.answers.lock().unwrap().push_back(Offloaded::Ran {
                output: remote.clone(),
                device: "desktop".into(),
                elapsed: std::time::Duration::from_millis(5),
            });
            let first = job_of(&fixture, invoke(&fixture));
            assert!(
                fixture.take_invocation_identity().is_none(),
                "where it runs is decided before the app is handed it"
            );
            fixture.state.drain_offloads();
            let mut answered = None;
            for _ in 0..64 {
                tokio::task::yield_now().await;
                answered = fixture
                    .state
                    .invocation_status(first)
                    .filter(tilo_actions::InvocationUpdate::is_terminal);
                if answered.is_some() {
                    break;
                }
            }
            assert!(
                matches!(
                    &answered,
                    Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) if *output == remote
                ),
                "{answered:?}"
            );
            assert!(fixture.take_invocation_identity().is_none());
            assert_eq!(
                route.asked.lock().unwrap().as_slice(),
                &[OffloadRequest {
                    room: fixture.cluster_id.clone(),
                    app_id: fixture.route.app_id.clone(),
                    address: tilo_actions::ActionAddress::new(&action.resource, &action.action),
                    offload: declared.clone(),
                    inputs: vec![photo.clone()],
                }],
                "the route reads exactly the content the arguments name"
            );

            // No device is better placed: the app is handed the invocation,
            // its own answer stands, and the route hears it ran here.
            route.answers.lock().unwrap().push_back(Offloaded::RunHere {
                reason: "no other device is better placed".into(),
            });
            let second = invoke(&fixture);
            fixture.state.drain_offloads();
            let mut delivered = None;
            for _ in 0..64 {
                tokio::task::yield_now().await;
                delivered = fixture.take_invocation_identity();
                if delivered.is_some() {
                    break;
                }
            }
            let (job, invocation) = delivered.expect("the app runs it here");
            assert_eq!(invocation, second);
            assert!(route.ran_here.lock().unwrap().is_empty());
            fixture
                .complete_invocation(
                    job,
                    &tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: remote.clone(),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                fixture.state.invocation_status(job),
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) if output == remote
            ));
            assert_eq!(route.ran_here.lock().unwrap().len(), 1);
            // The completing channel closed as an app's does; the app is
            // connected again.
            fixture.invocation_rx = fixture
                .state
                .capture_invocations_for_test(fixture.peer_id);

            // An action that declares no offload goes to the app at once.
            let mut ordinary = action.clone();
            ordinary.contract.offload = None;
            fixture.state.surfaces.lock().unwrap().insert(
                fixture.peer_id,
                PublishedSurface {
                    actions: vec![ordinary],
                    ..offered
                },
            );
            let third = invoke(&fixture);
            let (_, delivered) = fixture
                .take_invocation_identity()
                .expect("handed to the app at once");
            assert_eq!(delivered, third);
            assert_eq!(route.asked.lock().unwrap().len(), 2, "never asked about it");
        })
        .await;
}
