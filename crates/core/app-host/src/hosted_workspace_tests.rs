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
            value: Some(serde_json::json!("ran on the provider")),
            resources: Vec::new(),
        }
    }
}

impl tilo_actions::remote::ActionProvider for RecordingHost {
    fn query(
        &self,
        _: &tilo_actions::remote::Caller,
        _: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownAction,
        ))
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
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
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// What applies the typed actions of every workspace record on this device, as the resource
/// provider every workspace service registers does: it keeps what it was asked.
struct AppliesRecords(Arc<RecordingHost>);

impl tilo_actions::remote::ActionProvider for AppliesRecords {
    fn serves(&self, provider: &str) -> bool {
        tilo_actions::remote::is_resource_provider(provider)
    }

    fn query(
        &self,
        caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        self.0.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        self.0.invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.0.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &tilo_actions::remote::Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.0.cancel(caller, id)
    }
}

/// The one workspace another device hosts, and its host.
struct HostedElsewhere {
    workspace: String,
    provider: Arc<RecordingHost>,
}

impl crate::services::HostedWorkspaces for HostedElsewhere {
    fn host_actions(
        &self,
        workspace: &str,
    ) -> Option<Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>> {
        (workspace == self.workspace).then(|| {
            Arc::clone(&self.provider)
                as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>
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
            let provider = Arc::new(RecordingHost::default());
            let base = local_directory_fixture();
            let membership = base.membership.as_ref().unwrap().clone();
            let workspace = base.cluster_id.clone();
            drop(base);
            let services = ProcessServices {
                hosted_workspaces: Some(Arc::new(HostedElsewhere {
                    workspace,
                    provider: Arc::clone(&provider),
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
                fixture.state.publishers.surfaces.lock().unwrap().insert(
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
                    asks: false,
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
                    signed: None,
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
                other => panic!("the provider's receipt answers at once here: {other:?}"),
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
                let asked = provider.asked.lock().unwrap();
                let [(caller, forwarded)] = asked.as_slice() else {
                    panic!("the provider is asked once: {asked:?}");
                };
                assert_eq!(caller, &person, "the provider decides it for the person");
                assert_eq!(
                    forwarded.target.provider,
                    format!(
                        "{}:{}:{}",
                        tilo_actions::remote::RESOURCE_PROVIDER,
                        fixture.route.app_id,
                        fixture.route.document_id
                    ),
                    "the provider names the record"
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
            assert_eq!(provider.asked.lock().unwrap().len(), 1);

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
                provider.asked.lock().unwrap().len(),
                1,
                "an agent's call is not sent to the provider"
            );
            // Whatever the workspace's grants made of it here.
            let _ = fixture.take_invocation_identity();

            // An action the app lists in its directory, resolved there, is
            // sent to the host as well.
            fixture
                .state
                .publishers
                .surfaces
                .lock()
                .unwrap()
                .remove(&fixture.peer_id);
            let mut listed = request(&fixture, &action);
            listed.revision = None;
            let pending = fixture.state.action_invoke(&person, None, None, &listed);
            let receipt = fixture
                .drive(pending)
                .await
                .expect("the provider's receipt");
            assert!(
                matches!(receipt.status, ReceiptStatus::Completed { .. }),
                "{receipt:?}"
            );
            let asked = provider.asked.lock().unwrap();
            assert_eq!(
                asked.len(),
                2,
                "the provider is asked for the listed action"
            );
            assert_eq!(asked[1].1.invocation, listed.invocation);
            assert!(fixture.take_invocation_identity().is_none());
        })
        .await;
}

// The device's one dispatch sends a call of a typed action of a record of a workspace placed on a
// host to that host, and follows it there: what the person's shell asks of the daemon reaches the
// host the way the broker's own forwarding does. On a device that keeps no copy of the workspace
// (one confined to it) the provider here that applies records' actions answers none of the placed
// workspace's in the host's place. A device that keeps a copy applies them to it, its host
// reached or not, and its copy carries the change there.
#[test]
fn the_dispatch_sends_a_record_action_to_the_device_holding_its_workspace() {
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, PendingPoll, ReceiptStatus, RemoteActionTarget,
    };
    let provider = Arc::new(RecordingHost::default());
    let hosted = "0b7a5f4e-2c1d-4e8a-9f3b-6d5c4b3a2918";
    let providers = tilo_workspace_service::workspaces::providers::Providers::default();
    let elsewhere = HostedElsewhere {
        workspace: hosted.to_owned(),
        provider: Arc::clone(&provider),
    };
    providers.place_on_hosts(
        Arc::new(move |workspace: &str| {
            crate::services::HostedWorkspaces::host_actions(&elsewhere, workspace)
        }),
        tilo_workspace_service::workspaces::providers::HostsAnswer::Every,
    );
    let applied_here = Arc::new(RecordingHost::default());
    providers.provide(
        tilo_actions::remote::RESOURCE_PROVIDER,
        tilo_actions::remote::RESOURCE_PROVIDER,
        tilo_workspace_service::workspaces::providers::Reach::Devices,
        Arc::new(AppliesRecords(Arc::clone(&applied_here))),
    );
    let request = || InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: format!("{}:notes:a-note", tilo_actions::remote::RESOURCE_PROVIDER),
            address: tilo_actions::ActionAddress::new("note", "rename"),
        },
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let person = Caller::local_person("fixture-owner").in_workspace(hosted.to_owned());

    let sent = request();
    let mut receipt = providers
        .invoke(&person, sent.clone())
        .expect("the host takes a record's action of the workspace it holds");
    match receipt.poll_ready() {
        PendingPoll::Ready(Ok(receipt)) => {
            assert!(
                matches!(receipt.status, ReceiptStatus::Accepted),
                "{receipt:?}"
            );
        }
        other => panic!("the host's receipt answers at once here: {other:?}"),
    }
    {
        let asked = provider.asked.lock().unwrap();
        let [(caller, forwarded)] = asked.as_slice() else {
            panic!("the host is asked once: {asked:?}");
        };
        assert_eq!(caller, &person, "the host decides it for the person");
        assert_eq!(
            forwarded.invocation, sent.invocation,
            "the host is sent the call as it was made"
        );
        assert_eq!(forwarded.target.provider, sent.target.provider);
        assert_eq!(forwarded.target.address, sent.target.address);
    }
    let state = providers
        .observe(&person, &sent.invocation)
        .expect("the dispatch knows who took the call")
        .expect("the host tells where it stands");
    assert!(state.is_terminal(), "it is followed on the host: {state:?}");

    assert!(
        applied_here.asked.lock().unwrap().is_empty(),
        "nothing here applies a placed workspace's record in its host's place"
    );

    // A workspace no host takes the work of is this device's to apply, and a call that names no
    // record is none of the host's.
    let here = Caller::local_person("fixture-owner")
        .in_workspace("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned());
    assert!(providers.invoke(&here, request()).is_some());
    assert_eq!(applied_here.asked.lock().unwrap().len(), 1);
    let mut not_a_record = request();
    not_a_record.target.provider = "so.tilo.elsewhere".to_owned();
    assert!(providers.invoke(&person, not_a_record).is_none());
    assert_eq!(provider.asked.lock().unwrap().len(), 1);

    // What a workspace's VM device holds no keys for (its media keys, a window's share) is asked
    // of the host that placed it, through the same forwarding.
    let mut media_keys = request();
    media_keys.target = tilo_action_services::environment_device::target(
        tilo_action_services::environment_device::MEDIA_KEYS,
    );
    assert!(providers.invoke(&person, media_keys.clone()).is_some());
    let asked = provider.asked.lock().unwrap();
    assert_eq!(asked.len(), 2, "the host is asked for the VM's media keys");
    assert_eq!(asked[1].1.target, media_keys.target);
    drop(asked);

    // A member device that keeps a copy of the placed workspace applies the record's action to
    // it here: the host is not asked, so the change is made while it cannot be reached.
    let unreached = Arc::new(RecordingHost::default());
    let member = tilo_workspace_service::workspaces::providers::Providers::default();
    let elsewhere = HostedElsewhere {
        workspace: hosted.to_owned(),
        provider: Arc::clone(&unreached),
    };
    member.place_on_hosts(
        Arc::new(move |workspace: &str| {
            crate::services::HostedWorkspaces::host_actions(&elsewhere, workspace)
        }),
        tilo_workspace_service::workspaces::providers::HostsAnswer::Unapplied,
    );
    let kept_here = Arc::new(RecordingHost::default());
    member.provide(
        tilo_actions::remote::RESOURCE_PROVIDER,
        tilo_actions::remote::RESOURCE_PROVIDER,
        tilo_workspace_service::workspaces::providers::Reach::Devices,
        Arc::new(AppliesRecords(Arc::clone(&kept_here))),
    );
    let sent = request();
    assert!(
        member.invoke(&person, sent.clone()).is_some(),
        "the member's own copy takes the record's action"
    );
    let applied = kept_here.asked.lock().unwrap();
    let [(caller, applied)] = applied.as_slice() else {
        panic!("the member applies it once: {applied:?}");
    };
    assert_eq!(caller, &person);
    assert_eq!(applied.invocation, sent.invocation);
    assert!(
        unreached.asked.lock().unwrap().is_empty(),
        "a member's change waits for no host"
    );
}
