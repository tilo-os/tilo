//! Genuine native installed owner/private-worker door. No model is synthesized.
use super::native_effect_tests::{NativeFixture, await_membership_source, fixture};
use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use futures_util::io::{BufReader, BufWriter};
use std::{rc::Rc, time::SystemTime};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

struct MessageDirectory;

fn row(mut out: runtime_collab::collab_capnp::action_directory_row::Builder<'_>) {
    out.set_resource("native-opaque-agent");
    out.set_resource_kind("chat.agent");
    out.set_resource_label("Scribe");
    out.set_action("message");
    out.set_visibility_action("message");
    let contract = serde_json::to_string(&tilo_actions::ActionContract::default()).unwrap();
    out.set_visibility_contract_json(&contract);
    let mut descriptor = out.init_descriptor();
    descriptor.set_resource("native-opaque-agent");
    descriptor.set_resource_kind("chat.agent");
    descriptor.set_resource_label("Scribe");
    descriptor.set_action("message");
    descriptor.set_action_label("Message");
    descriptor.set_operation_kind("document");
    descriptor.set_params_json("[]");
    descriptor.set_contract_json(&contract);
    let mut semantic = descriptor.reborrow().init_semantic_identity();
    semantic.set_kind("chat.agent");
    semantic.set_key("scribe");
    let mut text = descriptor.init_input().init_text();
    text.set_prompt("Message");
}

impl runtime_collab::collab_capnp::action_directory::Server for MessageDirectory {
    fn query(
        self: Rc<Self>,
        _params: runtime_collab::collab_capnp::action_directory::QueryParams,
        mut results: runtime_collab::collab_capnp::action_directory::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let mut page = results.get().init_page();
        page.set_revision("native-message-r1");
        page.set_exhausted(true);
        row(page.init_rows(1).get(0));
        Promise::ok(())
    }

    fn resolve(
        self: Rc<Self>,
        _params: runtime_collab::collab_capnp::action_directory::ResolveParams,
        mut results: runtime_collab::collab_capnp::action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        row(results.get().init_resolution().init_found());
        Promise::ok(())
    }
}

fn native_worker(
    fixture: &NativeFixture,
) -> Promise<(runtime_collab::ChannelHandle, tokio::task::JoinHandle<()>), capnp::Error> {
    native_worker_route(fixture, "so.tilo.chat", "")
}

fn native_worker_route(
    fixture: &NativeFixture,
    app: &str,
    record: &str,
) -> Promise<(runtime_collab::ChannelHandle, tokio::task::JoinHandle<()>), capnp::Error> {
    let route =
        tilo_action_broker::collab_broker::CrdtRoute::record(app, fixture.room.to_string(), record);
    let (socket, _authorizer) = fixture
        .broker
        .connect_local_worker_with_authorizer(route.clone(), &fixture.certificate)
        .unwrap();
    let evidence = presentation(fixture);
    Promise::from_future(async move {
        socket.set_nonblocking(true).unwrap();
        let (read, write) = tokio::net::UnixStream::from_std(socket)
            .unwrap()
            .into_split();
        let network = Box::new(twoparty::VatNetwork::new(
            BufReader::new(read.compat()),
            BufWriter::new(write.compat_write()),
            rpc_twoparty_capnp::Side::Client,
            capnp::message::ReaderOptions::default(),
        ));
        let mut rpc = RpcSystem::new(network, None);
        let client =
            runtime_collab::CollabClient::from_raw(rpc.bootstrap(rpc_twoparty_capnp::Side::Server));
        let task = tokio::task::spawn_local(async move {
            let _ = rpc.await;
        });
        let channel = client
            .join_channel(&route.channel_key(), &evidence)
            .await
            .unwrap();
        if route.app_id == "so.tilo.chat" && route.record_id.is_empty() {
            channel
                .publish_action_directory("so.tilo.chat", capnp_rpc::new_client(MessageDirectory))
                .await
                .unwrap();
        }
        Ok((channel, task))
    })
}

fn presentation(fixture: &NativeFixture) -> Vec<u8> {
    use ed25519_dalek::Signer as _;
    let room = fixture.room.to_string();
    let (key, _) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.clone(),
        device_certificate: serde_json::to_vec(&fixture.certificate).unwrap(),
        proof: key
            .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap()
}

fn prepare_chat(fixture: &NativeFixture) {
    use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
    let actions = fixture.broker.app_action_broker().unwrap();
    await_membership_source(&actions, fixture.room);
    let job = actions
        .change_workspace_membership(
            &fixture.room.to_string(),
            &WorkspaceMembershipCommand::GrantAppAuthorship {
                app: "so.tilo.chat".into(),
            },
            &fixture.certificate,
        )
        .unwrap();
    drop(actions);
    let terminal = tilo_test_env::recv("native Chat authorship grant", &job.completion);
    assert!(terminal.code.is_empty(), "{}", terminal.message);
    let (key, _) =
        tilo_identity_host::broker::trusted_current_device(&fixture.room.to_string()).unwrap();
    fixture
        .owner
        .prepare_application_record(
            fixture.room,
            &fixture.certificate,
            &key,
            &tilo_joined_room::ApplicationRecord::declared(
                "so.tilo.chat",
                "",
                "chat.workspace",
                &["chat.workspace".into()],
            )
            .unwrap(),
            SystemTime::now(),
        )
        .unwrap();
}

fn addressed_invocation(
    fixture: &NativeFixture,
) -> Promise<tilo_actions::InvocationId, capnp::Error> {
    {
        use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
        let actions = fixture.broker.app_action_broker().unwrap();
        let room = fixture.room.to_string();
        let evidence = presentation(fixture);
        let caller = Caller::authenticated(
            fixture.certificate.user_public_key,
            fixture.certificate.device_public_key,
        )
        .in_room(&room)
        .in_new_execution();
        let host = actions
            .certified_local_action_host(&caller, &evidence)
            .unwrap();
        drop(actions);
        Promise::from_future(async move {
            let page = host
                .query(
                    &caller,
                    tilo_actions::remote::CatalogueQuery {
                        canonical_route: Some(tilo_actions::remote::CanonicalActionRoute {
                            room,
                            app_id: "so.tilo.chat".into(),
                            record_id: String::new(),
                        }),
                        max_results: 0,
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(page.providers.len(), 1);
            let invocation = tilo_actions::InvocationId::new();
            let receipt = host
                .invoke(
                    &caller,
                    InvokeRequest {
                        invocation,
                        target: RemoteActionTarget {
                            provider: page.providers.first().unwrap().provider.clone(),
                            address: tilo_actions::ActionAddress::new(
                                "native-opaque-agent",
                                "message",
                            ),
                        },
                        argument: InvokeArgument::Text("Explicit native input".into()),
                        context: None,
                        confirmation: None,
                        revision: Some("native-message-r1".into()),
                        fresh_binding: None,
                        wait_ms: None,
                        on_behalf_of: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(receipt.invocation, invocation);
            Ok(invocation)
        })
    }
}

fn native_contribution(
    fixture: &NativeFixture,
    invocation: &runtime_collab::Invocation,
) -> runtime_collab::document_contribution::DocumentContributionBody {
    use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
    let action = tilo_actions::ActionId::new();
    let logical = tilo_actions::InvocationId(uuid::Uuid::from_bytes(invocation.invocation_uuid));
    let room = fixture.room.to_string();
    let mut doc = fixture
        .owner
        .with_current(fixture.room, |view| {
            AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let base_heads = doc.get_heads().iter().map(|hash| hash.0).collect();
    doc.set_actor(automerge::ActorId::from(action.actor_bytes().to_vec()));
    let channel = tilo_actions::agent_message::agent_conversation_channel_id(&room, "scribe");
    let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let conversation = doc.put_object(&channels, &channel, ObjType::Map).unwrap();
    doc.put(&conversation, "workspace", room.as_str()).unwrap();
    doc.put(&conversation, "conversation_with", "scribe")
        .unwrap();
    doc.put(&conversation, "kind", "text").unwrap();
    let messages = doc.put_object(ROOT, "messages", ObjType::Map).unwrap();
    let list = doc.put_object(&messages, &channel, ObjType::List).unwrap();
    let row = doc.insert_object(&list, 0, ObjType::Map).unwrap();
    doc.put(&row, "id", format!("m_{}", logical.0.simple()))
        .unwrap();
    doc.put(&row, "author", invocation.actor.as_str()).unwrap();
    doc.put(&row, "ts", 7i64).unwrap();
    doc.put(&row, "parent", "").unwrap();
    doc.put(&row, "agent_intent", "message").unwrap();
    doc.put(&row, "edited", false).unwrap();
    let text = doc.put_object(&row, "text", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "Explicit native input")
        .unwrap();
    let context = tilo_actions::ActionContext {
        source_invocation: Some(logical),
        executed_action: Some(action),
        ..Default::default()
    };
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    let mut changes: Vec<_> = doc
        .get_changes(&[])
        .iter()
        .filter(|change| change.actor_id().to_bytes() == action.actor_bytes())
        .map(|change| change.hash().0)
        .collect();
    changes.sort_unstable();
    let payload = serde_json::to_vec(&serde_json::json!({"PostAgentMessage": {
        "workspace": room, "agent": "scribe", "message": {
            "ts": 7, "text": "Explicit native input", "intent": "message"
        }
    }}))
    .unwrap();
    let envelope = tilo_actions::ActionEnvelope {
        id: action,
        action: tilo_actions::EncodedAction {
            type_id: "chat.action".into(),
            payload,
        },
        base_heads,
        timestamp: 7,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        actor: None,
        context: Some(context),
    };
    let (key, _) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
    let message = fixture
        .owner
        .with_current(fixture.room, |view| {
            view.peer().clone().publish_document(
                "so.tilo.chat",
                "",
                doc.save(),
                &key,
                SystemTime::now(),
            )
        })
        .unwrap();
    let tilo_peer_state::NeighbourMsg::AppDocument {
        envelope: signed, ..
    } = message
    else {
        panic!("native signed publication must be a document");
    };
    runtime_collab::document_contribution::DocumentContributionBody {
        envelope: serde_json::to_vec(&envelope).unwrap(),
        changes,
        signed_op: postcard::to_stdvec(&signed).unwrap(),
    }
}

fn publish_contribution(
    channel: &runtime_collab::ChannelHandle,
    invocation: &runtime_collab::Invocation,
    body: &runtime_collab::document_contribution::DocumentContributionBody,
) -> Promise<(), runtime_portal::NotAvailable> {
    let channel = channel.clone();
    let bytes = postcard::to_stdvec(body).unwrap();
    let signed_op = body.signed_op.clone();
    let source = invocation.document_source.as_ref().unwrap().clone();
    Promise::from_future(async move {
        let mut upload = channel
            .open_document_contribution(&source, bytes.len())
            .await?;
        upload.write(&bytes).await?;
        channel
            .send_with_contributions(&signed_op, &[upload.reference()?])
            .await
    })
}

#[tokio::test(flavor = "current_thread")]
async fn actual_native_message_source_commits_chat_and_addressed_input_atomically() {
    use tilo_peer_state::workspace_membership::{
        AgentMembershipPreset, WorkspaceMembershipCommand,
    };
    tokio::task::LocalSet::new().run_until(async {
        let fixture = fixture();
        prepare_chat(&fixture);
        let actions = fixture.broker.app_action_broker().unwrap();
        let job = actions.change_workspace_membership(&fixture.room.to_string(),
            &WorkspaceMembershipCommand::AddAgent {agent: "scribe".into(),
                preset: AgentMembershipPreset::Observer}, &fixture.certificate).unwrap();
        drop(actions);
        let terminal = tilo_test_env::recv("native Agent admission", &job.completion);
        drop(job);
        assert!(terminal.code.is_empty(), "{}", terminal.message);
        let (channel, rpc) = native_worker(&fixture).await.unwrap();
        let stream = channel.subscribe_stable_invocations().await.unwrap();
        let original = addressed_invocation(&fixture).await.unwrap();
        let delivered = stream.read_with_document_source().await.unwrap();
        assert_eq!(delivered.invocation_uuid, *original.0.as_bytes());
        let body = native_contribution(&fixture, &delivered);
        publish_contribution(&channel, &delivered, &body).await.unwrap();
        assert!(channel.read_agent_message_progress(&original.0.to_string(), &format!("m_{}", original.0.simple())).await.is_err(),
            "signed publication custody does not grant native document-reader launcher custody");
        fixture.owner.with_current(fixture.room, |view| {
            let bytes = view.peer().document("so.tilo.chat", "").unwrap();
            let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
            assert!(doc.get_heads().iter().any(|hash| body.changes.contains(&hash.0)));
            Ok(())
        }).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.path).unwrap()).unwrap();
        let pending = &saved["native_authority"]["rooms"][fixture.room.to_string()]
            ["agent_messages"][original.0.to_string()];
        drop(stream);
        drop(channel);
        rpc.abort();
        drop(fixture);
        assert_eq!(pending["source"]["invocation"], original.0.to_string());
        assert_eq!(pending["source"]["payload"]["text"], "Explicit native input");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn actual_public_progress_refuses_foreign_app_record_and_original_row() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = fixture();
            let original = uuid::Uuid::from_u128(1701);
            let invocation = original.to_string();
            let row = format!("m_{}", original.simple());
            for (app, record) in [("so.tilo.notes", ""), ("so.tilo.chat", "foreign-record")] {
                let (channel, rpc) = native_worker_route(&fixture, app, record).await.unwrap();
                assert!(
                    channel
                        .read_agent_message_progress(&invocation, &row)
                        .await
                        .is_err(),
                    "an installed noncanonical channel must not introspect Chat owner progress"
                );
                drop(channel);
                rpc.abort();
            }
            let (channel, rpc) = native_worker(&fixture).await.unwrap();
            assert!(
                channel
                    .read_agent_message_progress(&invocation, &row)
                    .await
                    .is_err(),
                "a canonical signed worker still lacks document-reader launcher custody"
            );
            assert!(
                channel
                    .read_agent_message_progress(&invocation, "foreign-row")
                    .await
                    .is_err()
            );
            drop(channel);
            rpc.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn actual_native_message_publisher_installs_and_stream_replacement_is_live() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = fixture();
            prepare_chat(&fixture);
            let (channel, rpc) = native_worker(&fixture).await.unwrap();
            assert!(
                channel.subscribe_agent_messages().await.is_err(),
                "a genuine Chat publisher must not receive private managed Agent input"
            );
            let invocation = uuid::Uuid::new_v4().to_string();
            assert!(
                channel
                    .accept_agent_message(&invocation, [1; 32])
                    .await
                    .is_err(),
                "Chat authorship must not acknowledge Agent execution"
            );
            assert!(
                channel
                    .prepare_agent_message_reply(&invocation, [1; 32], "Unauthorized output")
                    .await
                    .is_err(),
                "Chat authorship must not prepare or commit an Agent reply"
            );
            assert!(
                channel
                    .commit_agent_message_reply(&invocation, [1; 32])
                    .await
                    .is_err(),
                "Chat authorship must not observe or commit private Agent reply custody"
            );
            let stream = channel.subscribe_stable_invocations().await.unwrap();
            let replacement = channel.subscribe_stable_invocations().await.unwrap();
            drop(stream);
            drop(replacement);
            drop(channel);
            rpc.abort();
        })
        .await;
}
