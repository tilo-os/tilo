// Genuine original admission, signed private worker, installed native owner,
// and Unix RPC source/upload delivery. No model output is synthesized here.
struct NativeMessageTestDirectory(runtime_collab::ActionDirectoryRow);

impl action_directory::Server for NativeMessageTestDirectory {
    fn query(
        self: Rc<Self>,
        _params: action_directory::QueryParams,
        mut results: action_directory::QueryResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from(runtime_collab::write_action_directory_page(
            results.get().init_page(),
            &runtime_collab::ActionDirectoryPage {
                revision: "message-r1".into(),
                rows: vec![self.0.clone()],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            },
        ))
    }
    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from(runtime_collab::write_action_directory_row(
            results.get().init_resolution().init_found(),
            &self.0,
        ))
    }
}

fn native_message_test_directory() -> action_directory::Client {
    let directory = signed_worker::directory_state("message-r1");
    let mut row = directory.borrow().row();
    row.resource = "opaque-agent-resource".into();
    row.resource_kind = "chat.agent".into();
    row.action = Some("message".into());
    row.visibility
        .as_mut()
        .expect("test row has visibility")
        .action
        .clone_from(row.action.as_ref().expect("test row has an action"));
    let descriptor = row.descriptor.as_mut().unwrap();
    descriptor.resource.clone_from(&row.resource);
    descriptor.resource_kind.clone_from(&row.resource_kind);
    descriptor.action = "message".into();
    descriptor.operation_kind = tilo_actions::OperationKind::Document;
    descriptor.input = runtime_collab::ExportedInput::Text {
        prompt: "Message".into(),
        placeholder: String::new(),
        wants_content: false,
    };
    descriptor.params = vec![tilo_actions::ActionParam::required(
        tilo_actions::PROMPTED_PARAM,
        tilo_actions::ParamType::Text,
        "Message",
    )];
    descriptor.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
        kind: "chat.agent".into(),
        key: "chat.person:scribe".into(),
    });
    capnp_rpc::new_client(NativeMessageTestDirectory(row))
}

#[tokio::test(flavor = "current_thread")]
async fn native_message_source_repeated_delivery_deduplicates_and_replacement_retires_upload() {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    tokio::task::LocalSet::new()
        .run_until(async {
            let owner = MessagePublisherOwner::enrolled();
            let room = signed_worker::OriginalRoom::with_message_service(owner);
            let (worker, _) = room.join_worker(103).await;
            let stream = worker.handle.subscribe_stable_invocations().await.unwrap();
            worker
                .handle
                .publish_action_directory(&room.route.app_id, native_message_test_directory())
                .await
                .unwrap();
            let caller = canonical_caller(&room);
            let host = room
                .state
                .certified_local_action_host(
                    &caller,
                    &signed_worker::presentation(&room.route.cluster_id, &room.cert, &room.key),
                )
                .unwrap();
            let page = canonical_pending(&room, host.query(&caller, canonical_query(&room)))
                .await
                .unwrap();
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: page.providers.first().unwrap().provider.clone(),
                    address: tilo_actions::ActionAddress::new("opaque-agent-resource", "message"),
                },
                argument: InvokeArgument::Json(
                    serde_json::json!({"value": "Actual original input"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                context: None,
                confirmation: None,
                revision: Some("message-r1".into()),
                fresh_binding: None,
                on_behalf_of: None,
                wait_ms: None,
            };
            let receipt = canonical_pending(&room, host.invoke(&caller, request.clone()))
                .await
                .unwrap();
            assert_eq!(receipt.invocation, request.invocation);
            let delivered = stream.read_with_document_source().await.unwrap();
            let source = delivered
                .document_source
                .clone()
                .expect("native Message must carry closed source custody");
            let original = InvocationEnvelope {
                id: delivered.id,
                invocation: request.invocation,
                resource: delivered.resource.clone(),
                action: delivered.action.clone(),
                argument: delivered.argument.clone(),
                actor: delivered.actor.clone(),
                context: if delivered.context_json.is_empty() {
                    None
                } else {
                    Some(serde_json::from_str(&delivered.context_json).unwrap())
                },
            };
            for _ in 0..crate::collab_broker_capnp::workspace_messages::MAX_MESSAGE_SOURCES + 2 {
                room.state
                    .invocation_subscribers
                    .lock()
                    .unwrap()
                    .get(&103)
                    .unwrap()
                    .send(original.clone())
                    .unwrap();
                let repeated = stream.read_with_document_source().await.unwrap();
                assert_eq!(repeated.invocation_uuid, delivered.invocation_uuid);
                assert_eq!(repeated.document_source.as_ref(), Some(&source));
                assert_eq!(
                    room.state.message_sources.lock().unwrap().retained_counts(),
                    (1, 0)
                );
            }
            let mut upload = worker
                .handle
                .open_document_contribution(&source, 4)
                .await
                .unwrap();
            assert!(
                worker
                    .handle
                    .open_document_contribution(&source, 4)
                    .await
                    .is_err(),
                "same source must not reserve a second buffer"
            );
            upload.write(&[1, 2]).await.unwrap();
            let reference = room
                .state
                .message_sources
                .lock()
                .unwrap()
                .retained_reference();
            // Exercise the actual server capability, not the exclusive SDK
            // writer: a hostile client can send concurrent same-offset chunks.
            let raw: runtime_collab::collab_capnp::document_contribution_writer::Client =
                capnp_rpc::new_client(NativeContributionWriter {
                    state: Arc::downgrade(&room.state),
                    peer: 103,
                    route: room.route.clone(),
                    reference,
                });
            let mut first = raw.write_request();
            first.get().set_offset(0);
            first.get().set_chunk(&[1, 2]);
            let mut duplicate = raw.write_request();
            duplicate.get().set_offset(0);
            duplicate.get().set_chunk(&[1, 2]);
            let (first, duplicate) = tokio::join!(first.send().promise, duplicate.send().promise);
            assert!(first.is_err());
            assert!(duplicate.is_err());
            // Refused duplicates cannot advance, reorder, or poison the
            // legitimate writer's exact contiguous next chunk.
            upload.write(&[3, 4]).await.unwrap();
            assert!(upload.reference().is_ok());
            assert_eq!(
                room.state.message_sources.lock().unwrap().retained_counts(),
                (1, 1)
            );
            let replacement = worker.handle.subscribe_stable_invocations().await.unwrap();
            assert_eq!(
                room.state.message_sources.lock().unwrap().retained_counts(),
                (0, 0)
            );
            assert!(
                worker
                    .handle
                    .open_document_contribution(&source, 4)
                    .await
                    .is_err(),
                "old opaque source cannot be revived by a replacement stream"
            );
            drop(replacement);
        })
        .await;
}
