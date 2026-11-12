// The person's invocation of a record action in a workspace another device
// hosts runs on that host: the host is sent it under the record's name, its
// receipt is the answer once it settled there, and the app here is never
// handed it. What only this device runs stays here.

/// A workspace's host that accepts every invocation and finishes it, keeping
/// what it was asked and by whom.
#[derive(Default)]
struct RecordingHost {
    asked: Mutex<
        Vec<(
            tilo_actions::remote::Caller,
            tilo_actions::remote::InvokeRequest,
        )>,
    >,
}

impl RecordingHost {
    fn output() -> tilo_actions::ActionOutput {
        tilo_actions::ActionOutput {
            value: Some(serde_json::json!("ran on the host")),
            resources: Vec::new(),
        }
    }
}

impl tilo_actions::remote::ActionHost for RecordingHost {
    fn query(
        &self,
        _: &tilo_actions::remote::Caller,
        _: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionHostError::UnknownAction,
        ))
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    > {
        let invocation = request.invocation;
        self.asked.lock().unwrap().push((caller.clone(), request));
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
            invocation,
            status: tilo_actions::remote::ReceiptStatus::Accepted,
        }))
    }

    fn observe(
        &self,
        _: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        Some(tilo_actions::remote::InvocationState::new(
            *id,
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: *id,
                output: Self::output(),
            },
        ))
    }

    fn next_change(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Pending<()>> {
        Some(tilo_actions::remote::Pending::ready(()))
    }

    fn cancel(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionHostError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// The one workspace another device hosts, and its host.
struct HostedElsewhere {
    workspace: String,
    host: Arc<RecordingHost>,
}

impl crate::services::HostedWorkspaces for HostedElsewhere {
    fn host_actions(
        &self,
        workspace: &str,
    ) -> Option<Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>> {
        (workspace == self.workspace).then(|| {
            Arc::clone(&self.host) as Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_hosted_workspaces_record_action_runs_on_its_host() {
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, PendingPoll, ReceiptStatus, RemoteActionTarget,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = Arc::new(RecordingHost::default());
            let base = local_directory_fixture();
            let membership = base.membership.as_ref().unwrap().clone();
            let workspace = base.cluster_id.clone();
            drop(base);
            let services = HostServices {
                hosted_workspaces: Some(Arc::new(HostedElsewhere {
                    workspace,
                    host: Arc::clone(&host),
                })),
                ..membership.services()
            };
            let mut fixture = local_directory_fixture_with_authority(
                membership.authority(),
                services,
                Some(72),
                None,
            );
            fixture.membership = Some(membership);
            // An edit of the record, as Notes' new note is: a view would show
            // here.
            fixture.directory_state.borrow_mut().operation = tilo_actions::OperationKind::Document;
            let action =
                published_directory_action(&fixture.directory_state.borrow().row()).unwrap();
            let publish = |fixture: &LocalDirectoryFixture, action: &PublishedAction| {
                fixture.state.surfaces.lock().unwrap().insert(
                    fixture.peer_id,
                    PublishedSurface {
                        peer_id: fixture.peer_id,
                        app_id: fixture.route.app_id.clone(),
                        route: fixture.route.clone(),
                        actions: vec![action.clone()],
                        live: true,
                    },
                );
            };
            let request =
                |fixture: &LocalDirectoryFixture, action: &PublishedAction| InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: fixture.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new(&action.resource, &action.action),
                    },
                    argument: InvokeArgument::Json(serde_json::Map::new()),
                    context: None,
                    confirmation: None,
                    revision: Some("seen-here".into()),
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                };
            let person =
                Caller::local_person("fixture-owner").in_workspace(fixture.cluster_id.clone());

            // The person's own invocation: sent to the host as the record
            // there, answered with what the host's run produced.
            publish(&fixture, &action);
            let sent = request(&fixture, &action);
            let mut pending = fixture.state.action_invoke(&person, None, None, &sent);
            let receipt = match pending.poll_ready() {
                PendingPoll::Ready(Ok(receipt)) => receipt,
                other => panic!("the host's receipt answers at once here: {other:?}"),
            };
            assert_eq!(receipt.invocation, sent.invocation);
            assert!(
                matches!(
                    &receipt.status,
                    ReceiptStatus::Completed { output } if *output == RecordingHost::output()
                ),
                "{receipt:?}"
            );
            {
                let asked = host.asked.lock().unwrap();
                let [(caller, forwarded)] = asked.as_slice() else {
                    panic!("the host is asked once: {asked:?}");
                };
                assert_eq!(caller, &person, "the host decides it for the person");
                assert_eq!(
                    forwarded.target.provider,
                    format!(
                        "{}:{}:{}",
                        tilo_actions::remote::RECORD_PROVIDER,
                        fixture.route.app_id,
                        fixture.route.record_id
                    ),
                    "the host names the record"
                );
                assert_eq!(forwarded.target.address, sent.target.address);
                assert_eq!(forwarded.invocation, sent.invocation);
                assert_eq!(forwarded.revision, None, "a revision is this device's");
            }
            assert!(
                fixture.take_invocation_identity().is_none(),
                "the app here is never handed it"
            );
            assert!(
                fixture
                    .state
                    .remote_job(&person, &sent.invocation)
                    .is_none(),
                "nothing is admitted here"
            );

            // What runs on the invoking device stays here: the app is handed
            // it, and the host never hears of it.
            let mut own_device = action.clone();
            own_device
                .contract
                .tags
                .push(tilo_actions::OWN_DEVICE_SCOPE.to_owned());
            publish(&fixture, &own_device);
            let local = request(&fixture, &own_device);
            let mut local_request = local.clone();
            local_request.revision = None;
            let _ = fixture
                .state
                .action_invoke(&person, None, None, &local_request)
                .poll_ready();
            let (_, delivered) = fixture
                .take_invocation_identity()
                .expect("an own-device action runs here");
            assert_eq!(delivered, local.invocation);
            assert_eq!(host.asked.lock().unwrap().len(), 1);

            // So does an agent's call: it is decided here, as its own.
            publish(&fixture, &action);
            let agent = Caller::local_person("fixture-owner")
                .in_workspace(fixture.cluster_id.clone())
                .delegated_to("agent:helper");
            let mut by_agent = request(&fixture, &action);
            by_agent.revision = None;
            let _ = fixture
                .state
                .action_invoke(&agent, None, None, &by_agent)
                .poll_ready();
            assert_eq!(
                host.asked.lock().unwrap().len(),
                1,
                "an agent's call is not sent to the host"
            );
            // Whatever the workspace's grants made of it here.
            let _ = fixture.take_invocation_identity();

            // An action the app lists in its directory, resolved there, is
            // sent to the host as well.
            fixture
                .state
                .surfaces
                .lock()
                .unwrap()
                .remove(&fixture.peer_id);
            let mut listed = request(&fixture, &action);
            listed.revision = None;
            let mut pending = fixture.state.action_invoke(&person, None, None, &listed);
            let receipt = tilo_test_env::within("the directory's action is sent", async {
                loop {
                    drain_directory_bridge(&fixture).await;
                    if let PendingPoll::Ready(result) = pending.poll_ready() {
                        break result;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the host's receipt");
            assert!(
                matches!(receipt.status, ReceiptStatus::Completed { .. }),
                "{receipt:?}"
            );
            let asked = host.asked.lock().unwrap();
            assert_eq!(asked.len(), 2, "the host is asked for the listed action");
            assert_eq!(asked[1].1.invocation, listed.invocation);
            assert!(fixture.take_invocation_identity().is_none());
        })
        .await;
}
