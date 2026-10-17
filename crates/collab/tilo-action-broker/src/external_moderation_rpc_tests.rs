// Exercise the production RPC and retained target custody without opening an
// identity key or making a network request. HTTP passes structural validation,
// but the host worker must reject it at its HTTPS gate before signing.
mod external_moderation_rpc_tests {
    use super::*;
    use registry_types::{ExternalModerationOperation, ModerationDestination, ModerationMutation};
    use runtime_collab::external_moderation::{PROTOCOL, Request};

    struct ExactLaunch {
        app: String,
        room: String,
    }

    impl crate::services::LauncherServices for ExactLaunch {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("fixture never launches applications".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            pid == std::process::id() && app == self.app && room == self.room
        }
    }

    struct Room {
        original: signed_worker::OriginalRoom,
        route: CrdtRoute,
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        _owner_mailbox: crate::test_owner::owner_commit::OwnerCommitMailbox,
    }

    impl Room {
        fn new(native: bool) -> Self {
            let original = signed_worker::OriginalRoom::new(
                if native {
                    tilo_user_identity::DeviceCapabilities::primary()
                } else {
                    tilo_user_identity::DeviceCapabilities::host()
                },
                None,
                None,
            );
            let route = CrdtRoute::record("so.tilo.store", &original.route.cluster_id, "store");
            let fanout = Arc::new(FanoutState::new());
            let mut services = original.membership.services();
            if native {
                services.launcher = Some(Arc::new(ExactLaunch {
                    app: route.app_id.clone(),
                    room: route.cluster_id.clone(),
                }));
            }
            let (owner_handle, owner_mailbox) =
                crate::test_owner::owner_commit::OwnerCommitMailbox::bounded(8);
            let publisher =
                PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(owner_handle));
            let (state, bridge) = CapnpBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                Some(original.membership.authority()),
                None,
                None,
                services,
            );
            let authority = signed_worker::CurrentHost::new(
                &route,
                &original.cert,
                Some(original.origin.capture().unwrap()),
            );
            assert!(state.certificate_authority.set(authority).is_ok());
            Self {
                original,
                route,
                state,
                directories: Rc::new(LocalActionDirectoryRegistry::new(bridge)),
                _owner_mailbox: owner_mailbox,
            }
        }

        async fn worker(&self, peer: u64) -> signed_worker::Connection {
            signed_worker::join_worker(
                &self.state,
                &self.directories,
                &self.route,
                &self.original.cert,
                &self.original.key,
                peer,
            )
            .await
            .0
        }
    }

    fn operation(appeal: bool) -> ExternalModerationOperation {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: "http://registry.invalid".into(),
                owner_pubkey_hex: "ab".repeat(32),
            },
            request_id: "custody-report".into(),
            issued_at_unix: now,
            expires_at_unix: now + 3600,
            mutation: if appeal {
                ModerationMutation::PublisherAppeal {
                    case_id: "case-one".into(),
                    reason: "The reported problem has been addressed".into(),
                    evidence_digest_hex: None,
                }
            } else {
                ModerationMutation::FileReport {
                    package_id: "org.example.app".into(),
                    category: "misleading".into(),
                    summary: "The package description is misleading".into(),
                    evidence_digest_hex: "cd".repeat(32),
                }
            },
        }
    }

    fn descriptor(action: &str) -> runtime_collab::ExportedAction {
        let directory = signed_worker::directory_state("moderation-r1");
        let mut descriptor = directory.borrow().row().descriptor.unwrap();
        descriptor.action = action.into();
        descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
            kind: descriptor.resource_kind.clone(),
            key: descriptor.resource.clone(),
        });
        descriptor.operation_kind = tilo_actions::OperationKind::Effect;
        descriptor.contract = tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            result: tilo_actions::ResultSpec::Job,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            ..Default::default()
        };
        descriptor
    }

    fn argument(operation: &ExternalModerationOperation) -> String {
        serde_json::json!({
            "app": "org.example.app",
            "operation": serde_json::to_string(operation).unwrap(),
        })
        .to_string()
    }

    async fn refused(
        handle: &runtime_collab::ChannelHandle,
        request: &Request,
        on_behalf_of: [u8; 16],
        expected: &str,
    ) {
        let error = handle
            .external_moderation(request, on_behalf_of)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_binds_exact_operation_and_live_target_custody() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for action in ["submit-report", "appeal"] {
                    let room = Room::new(false);
                    let target = room.worker(971).await;
                    let other = room.worker(972).await;
                    let descriptor = descriptor(action);
                    target
                        .handle
                        .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                    let operation = operation(action == "appeal");
                    let (wire, _) = target
                        .handle
                        .invoke_published_action(
                            971,
                            &descriptor.resource,
                            action,
                            Some(&argument(&operation)),
                            "{}",
                        )
                        .await
                        .unwrap();
                    let delivered = stream.read().await.unwrap();
                    assert_eq!(delivered.id, wire);
                    let id = delivered.invocation_uuid;
                    let request = Request {
                        protocol: PROTOCOL.into(),
                        invocation: uuid::Uuid::from_bytes(id).to_string(),
                        operation,
                    };
                    request.validate().unwrap();
                    // This proves admission reached the production host worker,
                    // not just a custody helper that always refuses requests.
                    refused(&target.handle, &request, id, "HTTPS").await;
                    for field in ["destination", "owner", "mutation"] {
                        let mut changed = request.clone();
                        match field {
                            "destination" => changed.operation.destination.base_url.push('/'),
                            "owner" => {
                                changed.operation.destination.owner_pubkey_hex = "ef".repeat(32)
                            }
                            "mutation" => match &mut changed.operation.mutation {
                                ModerationMutation::FileReport { summary, .. } => summary.push('!'),
                                ModerationMutation::PublisherAppeal { reason, .. } => {
                                    reason.push('!')
                                }
                                _ => unreachable!(),
                            },
                            _ => unreachable!(),
                        }
                        refused(&target.handle, &changed, id, "differs").await;
                    }
                    let mut wrong_mutation = request.clone();
                    wrong_mutation.operation.mutation = operation_for_other_action(action);
                    refused(&target.handle, &wrong_mutation, id, "differs").await;
                    let unknown = uuid::Uuid::new_v4().into_bytes();
                    refused(&target.handle, &request, unknown, "invocation").await;
                    let unknown_request = Request {
                        invocation: uuid::Uuid::from_bytes(unknown).to_string(),
                        ..request.clone()
                    };
                    refused(
                        &target.handle,
                        &unknown_request,
                        unknown,
                        "no retained target invocation",
                    )
                    .await;
                    refused(&other.handle, &request, id, "no retained target invocation").await;

                    let mut changed = descriptor.clone();
                    changed.contract.kind = tilo_actions::ActionKind::Read;
                    target
                        .handle
                        .publish_actions(&room.route.app_id, &[changed])
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "mismatch").await;
                    target
                        .handle
                        .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "HTTPS").await;
                    target
                        .handle
                        .cancel_published_invocation(wire)
                        .await
                        .unwrap();
                    refused(&target.handle, &request, id, "retired or cancelled").await;
                    room.state.remove_connection(971);
                    assert!(
                        target
                            .handle
                            .external_moderation(&request, id)
                            .await
                            .is_err()
                    );
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_refuses_expired_exact_operation_before_signing() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = Room::new(false);
                let target = room.worker(975).await;
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(&room.route.app_id, &[descriptor])
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let mut operation = operation(false);
                operation.issued_at_unix = 1;
                operation.expires_at_unix = 100;
                target
                    .handle
                    .invoke_published_action(
                        975,
                        "workspace:atlas",
                        "submit-report",
                        Some(&argument(&operation)),
                        "{}",
                    )
                    .await
                    .unwrap();
                let delivered = stream.read().await.unwrap();
                let id = delivered.invocation_uuid;
                let request = Request {
                    protocol: PROTOCOL.into(),
                    invocation: uuid::Uuid::from_bytes(id).to_string(),
                    operation,
                };
                refused(&target.handle, &request, id, "operation expired").await;
            })
            .await;
    }

    fn operation_for_other_action(action: &str) -> ModerationMutation {
        operation(action == "submit-report").mutation
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_native_target_requires_original_channel_and_publication() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = Room::new(true);
                let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
                std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC)
                    .unwrap();
                let handoff = serde_json::to_vec(&serde_json::json!({
                    "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
                    "app_id": room.route.app_id,
                    "workspace_id": room.route.cluster_id,
                }))
                .unwrap();
                std::io::Write::write_all(
                    &mut client,
                    &launcher::services::COLLAB_LAUNCH_FRAMING
                        .encode(&handoff)
                        .unwrap(),
                )
                .unwrap();
                let (rpc, server, client) = signed_worker::rpc_pair(
                    room.state.clone(),
                    973,
                    server,
                    client,
                    None,
                    room.directories.clone(),
                );
                let presentation = signed_worker::presentation(
                    &room.route.cluster_id,
                    &room.original.cert,
                    &room.original.key,
                );
                let handle = rpc
                    .join_channel(&room.route.channel_key(), &presentation)
                    .await
                    .unwrap();
                let target = signed_worker::Connection {
                    handle,
                    server,
                    client,
                };
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let (wire, _) = target
                    .handle
                    .invoke_published_action(
                        973,
                        &descriptor.resource,
                        "submit-report",
                        Some(&argument(&operation)),
                        "{}",
                    )
                    .await
                    .unwrap();
                let delivered = stream.read().await.unwrap();
                assert_eq!(delivered.id, wire);
                let id = delivered.invocation_uuid;
                let request = Request {
                    protocol: PROTOCOL.into(),
                    invocation: uuid::Uuid::from_bytes(id).to_string(),
                    operation,
                };
                refused(&target.handle, &request, id, "HTTPS").await;
                let foreign_route =
                    CrdtRoute::record(&room.route.app_id, &room.route.cluster_id, "foreign-store");
                let foreign = rpc
                    .join_channel(&foreign_route.channel_key(), &presentation)
                    .await
                    .unwrap();
                refused(&foreign, &request, id, "mismatch").await;
                let mut replacement = descriptor;
                replacement.resource = "replacement-store-resource".into();
                target
                    .handle
                    .publish_actions(&room.route.app_id, &[replacement])
                    .await
                    .unwrap();
                refused(&target.handle, &request, id, "publication retired").await;
                room.state.remove_connection(973);
                assert!(
                    target
                        .handle
                        .external_moderation(&request, id)
                        .await
                        .is_err()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_moderation_rpc_rechecks_expired_original_policy() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let room = Room::new(false);
                let target = room.worker(974).await;
                let descriptor = descriptor("submit-report");
                target
                    .handle
                    .publish_actions(&room.route.app_id, std::slice::from_ref(&descriptor))
                    .await
                    .unwrap();
                let stream = target.handle.subscribe_stable_invocations().await.unwrap();
                let operation = operation(false);
                let (wire, _) = target
                    .handle
                    .invoke_published_action(
                        974,
                        &descriptor.resource,
                        "submit-report",
                        Some(&argument(&operation)),
                        "{}",
                    )
                    .await
                    .unwrap();
                let delivered = stream.read().await.unwrap();
                assert_eq!(delivered.id, wire);
                let id = delivered.invocation_uuid;
                let request = Request {
                    protocol: PROTOCOL.into(),
                    invocation: uuid::Uuid::from_bytes(id).to_string(),
                    operation,
                };
                refused(&target.handle, &request, id, "HTTPS").await;
                room.original
                    .time_control
                    .advance_monotonic(std::time::Duration::from_secs(51));
                let error = target
                    .handle
                    .external_moderation(&request, id)
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(
                    !error.contains("HTTPS"),
                    "expired authority reached the effect worker: {error}"
                );
                assert!(
                    error.contains("certified collaboration session is no longer admitted")
                        && error.contains("not a workspace member"),
                    "expected original authority expiry refusal: {error}"
                );
            })
            .await;
    }
}
