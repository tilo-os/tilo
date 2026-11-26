/// A launch's committed reads of the records every app of its workspace opens
/// (`collab_types::WORKSPACE_DOCUMENTS`), through the actual channel handle
/// and its live launcher ownership: any app's window of the workspace reads and
/// changes them, as the workspace's grants on their owner decide, and nothing
/// else of their owner's.
mod workspace_document_ownership {
    use super::*;

    const LAUNCHED: &str = "so.tilo.notes";

    fn emoji() -> &'static collab_types::WorkspaceDocument {
        collab_types::WORKSPACE_DOCUMENTS
            .first()
            .expect("the workspace's emoji")
    }

    /// A channel on `route` held by a connection the launcher attested for
    /// `LAUNCHED` in the fixture's workspace.
    fn launched_channel(
        workspace: &signed_worker::OriginalWorkspace,
        identity: &AdmittedJoinIdentity,
        route: &CrdtRoute,
        socket: &tokio::net::UnixStream,
    ) -> JoinedChannel {
        launched_as(LAUNCHED, workspace, identity, route, socket)
    }

    /// A channel on `route` held by a connection the launcher attested for
    /// `app` in the fixture's workspace.
    fn launched_as(
        app: &str,
        workspace: &signed_worker::OriginalWorkspace,
        identity: &AdmittedJoinIdentity,
        route: &CrdtRoute,
        socket: &tokio::net::UnixStream,
    ) -> JoinedChannel {
        let local = app_process::LocalAppConnection::attested(
            app.into(),
            workspace.route.cluster_id.clone(),
            app_process::PeerProcess::from_socket(socket).unwrap(),
            crate::services::AuthenticatedApplicationLaunch {
                continuation_instance: Some(91),
                launch_ticket: None,
                windowless: false,
                opened_for: None,
            },
        )
        .unwrap();
        JoinedChannel {
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: workspace.state.clone(),
            directories: workspace.directories.clone(),
            channel_id: route.channel_key(),
            peer_id: 91,
            connection_id: 91,
            verified_identity: identity.clone(),
            resource_type: route_resource_type(route, None).unwrap(),
            lease: Some(Rc::new(ConnectionLease {
                local_media: Some(local),
                state: workspace.state.clone(),
                directories: workspace.directories.clone(),
                peer_id: 91,
            })),
            _hold: None,
        }
    }

    /// The certified person the workspace admitted, and the connection that
    /// keeps them admitted.
    async fn admitted(
        workspace: &signed_worker::OriginalWorkspace,
    ) -> (AdmittedJoinIdentity, signed_worker::Connection) {
        let (connection, _) = workspace.join_worker(90).await;
        let mut identity = workspace
            .state
            .host_workers
            .lock()
            .unwrap()
            .only(90)
            .unwrap()
            .clone();
        identity.worker_registration = None;
        (identity, connection)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn any_app_of_the_workspace_reads_its_workspace_records_and_no_other_record_of_their_owner()
     {
        tokio::task::LocalSet::new()
            .run_until(async {
                let workspace = signed_worker::OriginalWorkspace::new(
                    tilo_user_identity::DeviceCapabilities::primary(),
                    None,
                    None,
                );
                let (identity, _connection) = admitted(&workspace).await;
                let (socket, _peer) = tokio::net::UnixStream::pair().unwrap();
                let cluster = workspace.route.cluster_id.clone();
                let record = collab_types::workspace_document_id(emoji().kind);

                let route = CrdtRoute::record(emoji().app, &cluster, &record);
                let channel = launched_channel(&workspace, &identity, &route, &socket);
                let context = channel
                    .document_read_context()
                    .expect("a window of the workspace reads the workspace's record");
                assert_eq!(context.resource_type(), Some(emoji().kind));
                for (route, why) in [
                    (
                        CrdtRoute::record(emoji().app, &cluster, "tab:0000000000000001"),
                        "the owner's other records stay its own",
                    ),
                    (
                        CrdtRoute::record(emoji().app, &cluster, ""),
                        "the owner's primary record stays its own",
                    ),
                    (
                        CrdtRoute::record("so.tilo.code-editor", &cluster, &record),
                        "a record of the kind kept by an app that does not own it",
                    ),
                    (
                        CrdtRoute::record(
                            emoji().app,
                            tilo_desktop_model::WorkspaceId::new().to_string(),
                            &record,
                        ),
                        "the workspace record of another workspace",
                    ),
                ] {
                    let channel = launched_channel(&workspace, &identity, &route, &socket);
                    let refused = channel.document_read_context().err();
                    assert!(refused.is_some(), "{why}");
                }
            })
            .await;
    }

    /// Another app's record a launch opened to read (its manifest declares the
    /// read) is read under the launch's own ownership and the person's access
    /// on that record, whether or not the workspace already holds it: a launch
    /// that declares no such read is refused, as is a record of another
    /// workspace, and the read ends with the launch.
    #[tokio::test(flavor = "current_thread")]
    async fn a_launch_reads_another_apps_record_it_joined_to_read() {
        tokio::task::LocalSet::new()
            .run_until(async {
                // The records are a workspace's that this device keeps, and the device's one
                // reader decides a read of them, as a device serves its apps.
                let capabilities = tilo_user_identity::DeviceCapabilities::primary();
                let (key, cert) = signed_worker::OriginalWorkspace::device(capabilities.clone());
                let kept_here = tempfile::tempdir().unwrap();
                let host = tilo_workspace_service::workspaces::fixtures::test_host(kept_here.path(), &key, &cert)
                    .await;
                let kept = host.workspaces().create("Planning").await.unwrap().workspace;
                let workspace = signed_worker::OriginalWorkspace::in_workspace(
                    capabilities,
                    tilo_desktop_model::WorkspaceId::parse(&kept).unwrap(),
                );
                workspace.state.serve_records(tilo_workspace_service::workspaces::RecordReads::with_apps(
                    host.workspaces(),
                    &workspace.state,
                    None,
                ));
                let (identity, _connection) = admitted(&workspace).await;
                let (socket, _peer) = tokio::net::UnixStream::pair().unwrap();
                let cluster = workspace.route.cluster_id.clone();
                collab_types::app_surface::register_package(
                    "test.ownership-tasks",
                    "{\"resources\":[{\"kind\":\"test.ownership-tasks.Task\",\"label\":\"Task\",\"version\":1}]}".as_bytes(),
                );
                collab_types::app_surface::register_package(
                    "test.ownership-planner",
                    "{\"reads\":[\"test.ownership-tasks.Task\"],\"resources\":[{\"kind\":\"test.ownership-planner.Plan\",\"label\":\"Plan\",\"version\":1}]}".as_bytes(),
                );
                let read = |route: &CrdtRoute| {
                    launched_as("test.ownership-planner", &workspace, &identity, route, &socket)
                };
                let tasks = CrdtRoute::record("test.ownership-tasks", &cluster, "");
                let channel = read(&tasks);
                let context = channel
                    .document_read_context()
                    .expect("a launch reads the record it opened to read");
                let refused = launched_channel(&workspace, &identity, &tasks, &socket)
                    .document_read_context()
                    .err();
                assert!(refused.is_some(), "a launch that declares no read of Tasks reads none");
                let elsewhere = CrdtRoute::record(
                    "test.ownership-tasks",
                    tilo_desktop_model::WorkspaceId::new().to_string(),
                    "",
                );
                assert!(
                    read(&elsewhere).document_read_context().is_err(),
                    "nor of another workspace"
                );
                channel
                    .lease
                    .as_ref()
                    .and_then(|lease| lease.local_media.as_ref())
                    .unwrap()
                    .stop();
                assert!(
                    context.revalidate().is_err(),
                    "the read ends with the launch"
                );
                host.shutdown().await;
            })
            .await;
    }

    #[test]
    fn a_workspace_record_route_is_joined_as_its_own_kind() {
        let workspace = tilo_desktop_model::WorkspaceId::new().to_string();
        let record = collab_types::workspace_document_id(emoji().kind);
        let route = CrdtRoute::record(emoji().app, &workspace, &record);
        assert_eq!(
            route_resource_type(&route, None).unwrap().as_deref(),
            Some(emoji().kind),
            "an owner route joined without a kind holds the workspace record's"
        );
        assert_eq!(
            route_resource_type(&route, Some(emoji().kind.into()))
                .unwrap()
                .as_deref(),
            Some(emoji().kind)
        );
        assert!(route_resource_type(&route, Some("chat.workspace".into())).is_err());
        let other = CrdtRoute::record(emoji().app, &workspace, "tab:0000000000000001");
        assert_eq!(route_resource_type(&other, None).unwrap(), None);
        assert_eq!(
            route_resource_type(&other, Some("chat.workspace".into()))
                .unwrap()
                .as_deref(),
            Some("chat.workspace")
        );
    }
}
