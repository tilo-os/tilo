#[tokio::test(flavor = "current_thread")]
async fn only_the_launched_assistant_reviews_proposals_as_its_person_on_the_workspace_host() {
    use tilo_actions::remote::{
        ReceiptStatus, RemoteActionMessage, TestProvider, workspace_initiative as initiative,
    };
    use tilo_app_id::apps::ASSISTANT;

    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let workspace = &env.route.cluster_id;
            let host = Arc::new(TestProvider::new().completing(|_| RecordingHost::output()));
            let rig = Rig::with_services(
                &env,
                ProcessServices {
                    launcher: Some(Arc::new(LaunchedHere {
                        app: ASSISTANT.to_owned(),
                        workspace: workspace.clone(),
                    })),
                    local_person: Some(Arc::new(signed_worker::NativePerson(
                        env.authority.clone(),
                    ))),
                    hosted_workspaces: Some(Arc::new(HostedElsewhere {
                        workspace: workspace.clone(),
                        provider: host.clone(),
                    })),
                    ..env.membership.services()
                },
            )
            .await;
            let (app, server, client) =
                launched_app_connection(&rig.state, &rig.directories, 905, ASSISTANT, workspace);
            let route = CrdtRoute::app(ASSISTANT, workspace.clone());
            let channel = app
                .join_channel(
                    &route.channel_key(),
                    &presentation(workspace, &env.cert, &env.key),
                )
                .await
                .expect("the installed Assistant joins as the person at this device");
            let review = || {
                signed_worker::invoke_request(
                    initiative::PROVIDER,
                    initiative::PROPOSALS,
                    initiative::ACCEPT,
                    Some(serde_json::json!({"proposal": "proposal-1"})),
                )
            };

            // An attested app also needs its manifest's invoke capability.
            assert!(refused(&app_asks(&channel, review()).await));
            rig.state.set_app_action_access(Arc::new(|app| {
                crate::apps::app_actions::AppActionAccess {
                    invoke: app == ASSISTANT,
                    query: false,
                }
            }));
            let request = review();
            let receipt = signed_worker::receipt(app_asks(&channel, request.clone()).await);
            assert_eq!(receipt.invocation, request.invocation);
            assert_eq!(
                receipt.status,
                ReceiptStatus::Completed {
                    output: RecordingHost::output()
                }
            );
            assert_eq!(
                host.callers(),
                [signed_worker::native_person(&route, &env.cert)]
            );
            assert_eq!(host.take_asked(), [request]);

            // A valid person's signature on a channel named for Assistant
            // cannot take the place of the launcher's authentication.
            let unlaunched = rig.app_caller(&env, 906, ASSISTANT, None).await;
            assert!(refused(&app_asks(&unlaunched.handle, review()).await));
            let mut continued = review();
            continued.on_behalf_of = Some(tilo_actions::InvocationId::new());
            assert!(refused(&app_asks(&channel, continued).await));
            let mut propose = review();
            propose.target = initiative::target(initiative::PROPOSALS, initiative::PROPOSE);
            let result = signed_worker::ask(&channel, RemoteActionMessage::Invoke(propose))
                .await
                .expect("an action outside proposal review is answered");
            assert!(matches!(
                signed_worker::receipt(result).status,
                ReceiptStatus::Rejected { .. }
            ));
            assert!(host.asked().is_empty());
            drop((unlaunched, channel, app));
            server.abort();
            client.abort();
            rig.stop().await;
        })
        .await;
}
