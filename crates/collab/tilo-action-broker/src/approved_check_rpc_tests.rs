// Genuine signed target RPC exercises custody before any execution service.
#[tokio::test(flavor = "current_thread")]
async fn approved_check_rpc_requires_exact_retained_target_and_accepted_id() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let room = signed_worker::OriginalRoom::new(
                tilo_user_identity::DeviceCapabilities::host(),
                None,
                None,
            );
            let (target, _) = room.join_worker(981).await;
            let (other, _) = room.join_worker(982).await;
            let state = signed_worker::directory_state("approved-check-r1");
            let mut descriptor = state.borrow().row().descriptor.unwrap();
            descriptor.action = runtime_collab::approved_checks::ACTION_ID.into();
            descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
                kind: descriptor.resource_kind.clone(),
                key: descriptor.resource.clone(),
            });
            descriptor.operation_kind = tilo_actions::OperationKind::Effect;
            descriptor.contract = tilo_actions::ActionContract {
                permission: tilo_actions::PermissionRequirement::Write,
                result: tilo_actions::ResultSpec::Job,
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                ..Default::default()
            };
            target
                .handle
                .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                .await
                .unwrap();
            let stream = target.handle.subscribe_stable_invocations().await.unwrap();
            let (wire, _) = target
                .handle
                .invoke_published_action(
                    981,
                    "workspace:atlas",
                    runtime_collab::approved_checks::ACTION_ID,
                    Some(r#"{"check_id":"regression"}"#),
                    "{}",
                )
                .await
                .unwrap();
            let delivered = stream.read().await.unwrap();
            assert_eq!(delivered.id, wire);
            let request = runtime_collab::approved_checks::Request {
                version: 1,
                invocation: uuid::Uuid::from_bytes(delivered.invocation_uuid).to_string(),
                check_id: "regression".into(),
            };
            let error = target
                .handle
                .approved_check(&request)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("unsupported by this host"),
                "genuine accepted target custody must reach the real host-service gate: {error}"
            );
            let mut changed = descriptor.clone();
            changed.semantic_identity.as_mut().unwrap().key = "different-resource-key".into();
            target
                .handle
                .publish_actions(&room.route.app_id, &[changed])
                .await
                .unwrap();
            assert!(
                target
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("semantic identity changed"),
                "same kind cannot replace the accepted resource key"
            );
            let mut missing = descriptor.clone();
            missing.semantic_identity = None;
            target
                .handle
                .publish_actions(&room.route.app_id, &[missing])
                .await
                .unwrap();
            assert!(
                target.handle.approved_check(&request).await.is_err(),
                "missing live semantic identity refuses original custody"
            );
            target
                .handle
                .publish_actions(&room.route.app_id, &[descriptor])
                .await
                .unwrap();
            let wrong = runtime_collab::approved_checks::Request {
                check_id: "other".into(),
                ..request.clone()
            };
            assert!(
                target
                    .handle
                    .approved_check(&wrong)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("differs from accepted action")
            );
            assert!(
                other
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("no retained target invocation")
            );
            target
                .handle
                .cancel_published_invocation(wire)
                .await
                .unwrap();
            assert!(
                target
                    .handle
                    .approved_check(&request)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("retired or cancelled")
            );
            room.state.remove_connection(981);
            assert!(
                target.handle.approved_check(&request).await.is_err(),
                "retired connection cannot recover accepted target custody"
            );
        })
        .await;
}

// Actual kernel process plus launcher handoff exercises the native target branch.
// The fixture admits only its own process/app/room, and executes no guest command.
#[tokio::test(flavor = "current_thread")]
async fn approved_check_rpc_native_target_requires_live_launcher_connection() {
    struct ExactFixtureLaunch {
        pid: u32,
        app: String,
        room: String,
    }
    impl crate::services::LauncherServices for ExactFixtureLaunch {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
            Err("fixture never launches applications".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            pid == self.pid && app == self.app && room == self.room
        }
    }
    tokio::task::LocalSet::new().run_until(async {
        let room = signed_worker::OriginalRoom::new(tilo_user_identity::DeviceCapabilities::primary(), None, None);
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(room.membership.authority()),
            None, None,
            HostServices {
                launcher: Some(Arc::new(ExactFixtureLaunch { pid: std::process::id(), app: room.route.app_id.clone(), room: room.route.cluster_id.clone() })),
                ..Default::default()
            },
        );
        assert!(state.certificate_authority.set(room.authority.clone()).is_ok());
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
        let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC).unwrap();
        let handoff = serde_json::to_vec(&serde_json::json!({"protocol":launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,"app_id":room.route.app_id,"workspace_id":room.route.cluster_id})).unwrap();
        std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_FRAMING.encode(&handoff).unwrap()).unwrap();
        let (rpc, server, client) = signed_worker::rpc_pair(state.clone(), 991, server, client, None, directories);
        let handle = rpc.join_channel(&room.route.channel_key(), &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key)).await.unwrap();
        let connection = signed_worker::Connection { handle, server, client };
        let directory = signed_worker::directory_state("native-check-r1");
        let mut descriptor = directory.borrow().row().descriptor.unwrap();
        descriptor.action = runtime_collab::approved_checks::ACTION_ID.into();
            descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity { kind: descriptor.resource_kind.clone(), key: descriptor.resource.clone() });
        descriptor.operation_kind = tilo_actions::OperationKind::Effect;
        descriptor.contract = tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Write,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        };
        connection.handle.publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor)).await.unwrap();
        let stream = connection.handle.subscribe_stable_invocations().await.unwrap();
        let (wire, _) = connection.handle.invoke_published_action(991, "workspace:atlas", runtime_collab::approved_checks::ACTION_ID, Some(r#"{"check_id":"regression"}"#), "{}").await.unwrap();
        let delivered = stream.read().await.unwrap();
        assert_eq!(delivered.id, wire);
        let request = runtime_collab::approved_checks::Request { version: 1, invocation: uuid::Uuid::from_bytes(delivered.invocation_uuid).to_string(), check_id: "regression".into() };
        let error = connection.handle.approved_check(&request).await.unwrap_err().to_string();
        assert!(error.contains("unsupported by this host"), "actual native custody must reach the trusted service gate: {error}");
        let mut replacement = descriptor;
        replacement.resource = "fresh-opaque-resource-lifetime".into();
        connection.handle.publish_actions(&room.route.app_id, &[replacement]).await.unwrap();
        assert!(connection.handle.approved_check(&request).await.unwrap_err().to_string().contains("publication retired"),
            "new publication cannot revive the old opaque target invocation");
        let (new_wire, _) = connection.handle.invoke_published_action(991, "fresh-opaque-resource-lifetime", runtime_collab::approved_checks::ACTION_ID, Some(r#"{"check_id":"regression"}"#), "{}").await.unwrap();
        let next = stream.read().await.unwrap();
        assert_eq!(next.id, new_wire);
        let new_request = runtime_collab::approved_checks::Request { invocation: uuid::Uuid::from_bytes(next.invocation_uuid).to_string(), ..request.clone() };
        assert!(connection.handle.approved_check(&new_request).await.unwrap_err().to_string().contains("unsupported by this host"),
            "fresh opaque address retains authenticated stable identity and reaches the real host service gate");
        let terminal = tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(next.invocation_uuid)),
            output: tilo_actions::ActionOutput::default(),
        };
        connection.handle.update_invocation(new_wire, &serde_json::to_string(&terminal).unwrap()).await.unwrap();
        let ended = connection.handle.approved_check(&new_request).await.unwrap_err().to_string();
        assert!(ended.contains("no retained target invocation"),
            "successful target completion must retire original native custody: {ended}");
        assert!(connection.handle.approved_check(&new_request).await.is_err(),
            "an exact request retry cannot resurrect terminal native custody");
        let foreign_route = CrdtRoute::record(&room.route.app_id, &room.route.cluster_id, "foreign-record");
        let foreign = rpc.join_channel(&foreign_route.channel_key(), &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key)).await.unwrap();
        assert!(foreign.approved_check(&request).await.unwrap_err().to_string().contains("target action or contract mismatch"), "same process and app cannot send an accepted invocation over a foreign record channel");
        connection.handle.cancel_published_invocation(wire).await.unwrap();
        assert!(connection.handle.approved_check(&request).await.unwrap_err().to_string().contains("retired or cancelled"));
        state.remove_connection(991);
        assert!(connection.handle.approved_check(&request).await.is_err());
    }).await;
}
