use super::*;
use crate::shell::collab_broker_capnp::tests::{
    DirectoryResolveMode, drain_directory_bridge, local_directory_fixture_with_membership,
};
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

fn fixture() -> (
    crate::shell::collab_broker_capnp::tests::LocalDirectoryFixture,
    String,
    String,
) {
    let mut tree = WorkspaceTree::default();
    let mut workspace = Workspace::test_default(WorkspaceId(71), "MCP");
    let room = workspace.room.to_string();
    let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
    workspace.owner = Some(actor.clone());
    tree.workspaces.push(workspace);
    tree.set_active(WorkspaceId(71));
    (
        local_directory_fixture_with_membership(tree.shared()),
        room,
        actor,
    )
}
fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":1,"method":method,"params":params})
}
fn call(name: &str, args: Value) -> Value {
    request("tools/call", json!({"name":name,"arguments":args}))
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_tool_is_an_invalid_params_error_with_the_request_id() {
    let (fixture, room, actor) = fixture();
    let mut server = McpServer::new(fixture.broker.clone(), room, actor);
    let reply = server
        .handle(json!({
            "jsonrpc": "2.0",
            "id": "unknown-tool-request",
            "method": "tools/call",
            "params": {"name": "qos_not_a_tool", "arguments": {}}
        }))
        .await
        .unwrap();

    assert_eq!(reply["id"], "unknown-tool-request");
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply.get("result").is_none(), "{reply}");
}

#[tokio::test(flavor = "current_thread")]
async fn null_id_is_rejected_before_mutating_dispatch_and_valid_id_boundaries_hold() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (fixture, room, actor) = fixture();
            let mut server = McpServer::new(fixture.broker.clone(), room, actor);
            let mut null_id = call(
                "qos_invoke",
                json!({
                    "peer_id": 71,
                    "revision": "directory-r1",
                    "resource": "workspace:atlas",
                    "action": "open",
                    "arguments": {}
                }),
            );
            null_id["id"] = Value::Null;
            let reply = server.handle(null_id);
            let (reply, ()) = tokio::join!(reply, async {
                tokio::task::yield_now().await;
                drain_directory_bridge(&fixture).await;
            });
            let reply = reply.unwrap();
            assert_eq!(reply["id"], Value::Null);
            assert_eq!(reply["error"]["code"], -32600);
            assert!(reply.get("result").is_none(), "{reply}");
            assert_eq!(fixture.resolve_count(), 0);

            assert!(
                server
                    .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                    .await
                    .is_none()
            );
            for id in [json!(7), json!("request-7")] {
                let reply = server
                    .handle(json!({
                        "jsonrpc": "2.0",
                        "id": id.clone(),
                        "method": "ping"
                    }))
                    .await
                    .unwrap();
                assert_eq!(reply["id"], id);
                assert_eq!(reply["result"], json!({}));
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn protocol_and_argument_boundaries() {
    let (fixture, room, actor) = fixture();
    let mut server = McpServer::new(fixture.broker.clone(), room, actor);
    let init = server.handle(request("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}))).await.unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(
        init["result"]["capabilities"]["tools"]
            .get("listChanged")
            .is_none()
    );
    assert!(
        server
            .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await
            .is_none()
    );
    assert_eq!(
        server.handle(request("ping", json!({}))).await.unwrap()["result"],
        json!({})
    );
    assert_eq!(
        server
            .handle(json!({"id":1,"method":"ping"}))
            .await
            .unwrap()["error"]["code"],
        -32600
    );
    assert_eq!(
        server.handle(request("missing", json!({}))).await.unwrap()["error"]["code"],
        -32601
    );
    let tools = server
        .handle(request("tools/list", json!({})))
        .await
        .unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);
    for args in [
        json!({"actor":"other"}),
        json!({"workspace":"other"}),
        json!({"confirmed":true}),
        json!([]),
    ] {
        assert_eq!(
            server.handle(call("qos_list_apps", args)).await.unwrap()["result"]["isError"],
            true
        );
    }
    assert_eq!(
        server
            .handle(call("qos_status", json!({"invocation_id":999})))
            .await
            .unwrap()["result"]["isError"],
        true
    );
    assert_eq!(
        server
            .handle(call("qos_cancel", json!({"invocation_id":999})))
            .await
            .unwrap()["result"]["isError"],
        true
    );
}

#[tokio::test(flavor = "current_thread")]
async fn actual_broker_query_resolve_and_stale_invocation() {
    tokio::task::LocalSet::new().run_until(async {
        let (fixture, room, actor) = fixture();
        let mut server = McpServer::new(fixture.broker.clone(),room,actor);
        let roots=server.handle(call("qos_list_apps",json!({}))).await.unwrap();
        assert_eq!(roots["result"]["structuredContent"]["apps"][0]["peer_id"],71);
        let query=server.handle(call("qos_query",json!({"peer_id":71})));
        let (reply,())=tokio::join!(query, async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        assert_eq!(reply["result"]["structuredContent"]["revision"],"directory-r1");
        assert!(reply["result"]["structuredContent"]["rows"][0]["descriptor"]["params"].is_array());
        fixture.set_resolve_mode(DirectoryResolveMode::Stale);
        let invoke=server.handle(call("qos_invoke",json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}})));
        let (reply,())=tokio::join!(invoke, async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert_eq!(reply.unwrap()["result"]["isError"],true);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn resources_are_live_room_scoped_and_reads_use_authority() {
    tokio::task::LocalSet::new().run_until(async {
        let (fixture, room, actor) = fixture();
        let mut server=McpServer::new(fixture.broker.clone(),room.clone(),actor);
        let resources=server.handle(request("resources/list",json!({}))).await.unwrap();
        assert_eq!(resources["result"]["resources"][0]["uri"],"qos://apps/71");
        let read=server.handle(request("resources/read",json!({"uri":"qos://apps/71"})));
        let (reply,())=tokio::join!(read,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert!(reply.unwrap()["result"]["contents"][0]["text"].as_str().unwrap().contains("directory-r1"));
        let mut other=McpServer::new(fixture.broker.clone(),"different-room".into(),"outsider".into());
        assert_eq!(other.handle(request("resources/list",json!({}))).await.unwrap()["result"]["resources"],json!([]));
        assert!(other.handle(request("resources/read",json!({"uri":"qos://apps/71"}))).await.unwrap().get("error").is_some());
        let mut denied=McpServer::new(fixture.broker.clone(),room,"outsider".into());
        let read=denied.handle(request("resources/read",json!({"uri":"qos://apps/71"})));
        let (reply,())=tokio::join!(read,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        let text = reply["result"]["contents"][0]["text"]
            .as_str()
            .expect("same-room outsider read returns the broker-filtered page");
        let page: Value = serde_json::from_str(text).unwrap();
        assert_eq!(page["rows"], json!([]), "{reply}");
        assert!(!text.contains("workspace:atlas"), "{reply}");
        let call=denied.handle(call("qos_invoke",json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}})));
        let (reply,())=tokio::join!(call,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert_eq!(reply.unwrap()["result"]["isError"],true);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn root_identity_pagination_and_disconnect_are_exact() {
    use crate::shell::collab_broker_capnp::ActionDirectoryRoot;
    let (fixture, room, actor) = fixture();
    fixture.broker.replace_action_directory_roots_for_test(
        (1..=130)
            .map(|peer_id| ActionDirectoryRoot {
                peer_id,
                app_id: "same.app".into(),
                cluster_id: room.clone(),
                record_id: format!("auxiliary/{peer_id}"),
            })
            .collect(),
    );
    let mut server = McpServer::new(fixture.broker.clone(), room, actor);
    let first = server
        .handle(call("qos_list_apps", json!({"max_results":500})))
        .await
        .unwrap();
    assert_eq!(
        first["result"]["structuredContent"]["apps"]
            .as_array()
            .unwrap()
            .len(),
        128
    );
    assert_eq!(first["result"]["structuredContent"]["next_after_peer"], 128);
    let last = server
        .handle(call("qos_list_apps", json!({"after_peer":128})))
        .await
        .unwrap();
    assert_eq!(
        last["result"]["structuredContent"]["apps"][0]["record_id"],
        "auxiliary/129"
    );
    assert_eq!(
        last["result"]["structuredContent"]["apps"][1]["uri"],
        "qos://apps/130"
    );
    fixture
        .broker
        .replace_action_directory_roots_for_test(vec![]);
    assert_eq!(
        server
            .handle(call("qos_query", json!({"peer_id":129})))
            .await
            .unwrap()["result"]["isError"],
        true
    );
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_invocation_preserves_actor_and_cancellation_ownership() {
    tokio::task::LocalSet::new().run_until(async {
        let (mut fixture,room,actor)=fixture();
        let mut server=McpServer::new(fixture.broker.clone(),room.clone(),actor.clone());
        let invoke=server.handle(call("qos_invoke",json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}})));
        let (reply,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        assert_eq!(reply["result"]["isError"],false,"{reply}");
        let id=reply["result"]["structuredContent"]["invocation_id"].as_u64().unwrap();
        assert_eq!(reply["result"]["structuredContent"]["update"]["state"],"accepted");
        assert_eq!(fixture.take_invocation_actor(),Some(actor));
        assert_eq!(fixture.resolve_count(),1);
        let mut foreign=McpServer::new(fixture.broker.clone(),room,"foreign".into());
        for tool in ["qos_status","qos_cancel"] {
            assert_eq!(foreign.handle(call(tool,json!({"invocation_id":id}))).await.unwrap()["result"]["isError"],true);
        }
        let status=server.handle(call("qos_status",json!({"invocation_id":id}))).await.unwrap();
        assert_eq!(status["result"]["structuredContent"]["invocation_id"],id);
        assert_ne!(status["result"]["structuredContent"]["update"]["state"],"succeeded");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_preserves_typed_argument_maps_through_the_broker() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut fixture, room, actor) = fixture();
            let mut server = McpServer::new(fixture.broker.clone(), room, actor);
            let original = json!({
                "region": "0.16666666666666666,0,0.6666666666666666,1",
                "json_looking_text": "{\"nested\":true}",
                "scale": 0.5,
                "enabled": true,
                "content": "sha256:0123456789abcdef"
            });

            let mut transported = None;
            for arguments in [original.clone(), json!({})] {
                let invoke = server.handle(call(
                    "qos_invoke",
                    json!({
                        "peer_id": 71,
                        "revision": "directory-r1",
                        "resource": "workspace:atlas",
                        "action": "open",
                        "arguments": arguments
                    }),
                ));
                let (reply, ()) = tokio::join!(invoke, async {
                    tokio::task::yield_now().await;
                    drain_directory_bridge(&fixture).await;
                });
                let reply = reply.unwrap();
                assert_eq!(reply["result"]["isError"], false, "{reply}");
                let delivered = fixture
                    .take_invocation_argument()
                    .flatten()
                    .expect("MCP invocation delivers one encoded argument map");
                let encoded = delivered
                    .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                    .expect("MCP invocation uses the canonical typed-argument envelope");
                let decoded: serde_json::Map<String, Value> =
                    serde_json::from_str(encoded).expect("typed argument envelope contains JSON");
                assert_eq!(&decoded, arguments.as_object().unwrap());
                if arguments == original {
                    transported = Some(decoded);
                }
            }

            let delivered = transported.expect("nonempty argument map crossed the broker");
            let params = [tilo_actions::ActionParam::required(
                "region",
                tilo_actions::ParamType::Text,
                "normalized crop region",
            )];
            let crop_only =
                serde_json::Map::from_iter([("region".to_string(), delivered["region"].clone())]);
            let bound = tilo_actions::ActionArgs::bind_json(&params, &crop_only)
                .expect("a transported crop region remains typed text");
            assert_eq!(
                bound.text("region"),
                Some("0.16666666666666666,0,0.6666666666666666,1")
            );
            let numeric_crop = serde_json::Map::from_iter([("region".to_string(), json!(0.5))]);
            assert!(
                tilo_actions::ActionArgs::bind_json(&params, &numeric_crop).is_err(),
                "a numeric crop region must fail canonical text validation"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_errors_and_terminal_content_are_not_fabricated() {
    use crate::shell::collab_broker_capnp::{
        ActionDirectoryBridgeError as Error, StartedDirectoryAction,
    };
    for failure in [
        Error::Busy,
        Error::TimedOut,
        Error::Unavailable,
        Error::ConfirmationRequired,
        Error::NotFound,
        Error::StaleRevision,
    ] {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(Err(failure)).unwrap();
        assert_eq!(bridge_reply(rx).await.unwrap_err(), format!("{failure:?}"));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    drop(tx);
    assert_eq!(bridge_reply(rx).await.unwrap_err(), "Unavailable");
    let update = tilo_actions::InvocationUpdate::Succeeded {
        invocation: tilo_actions::InvocationId::new(),
        output: tilo_actions::ActionOutput {
            value: Some(json!({"content":"sha256:123","width":200})),
            resources: vec!["content:sha256:123".into()],
        },
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(Ok(Reply::StartedDirectoryAction(StartedDirectoryAction {
        invocation_id: 42,
        initial_update: update.clone(),
    })))
    .unwrap();
    assert_eq!(
        bridge_reply(rx).await.unwrap(),
        json!({"invocation_id":42,"update":update})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn targets_require_a_nonempty_revision_before_enqueueing() {
    let (fixture, room, actor) = fixture();
    let mut server = McpServer::new(fixture.broker.clone(), room, actor);
    for name in ["qos_resolve", "qos_invoke"] {
        let mut args =
            json!({"peer_id":71,"revision":"","resource":"workspace:atlas","action":"open"});
        if name == "qos_invoke" {
            args["arguments"] = json!({});
        }
        let reply = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            server.handle(call(name, args)),
        )
        .await
        .expect("invalid revision must be rejected without waiting for broker")
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_queue_backpressure_timeout_and_disconnect_are_tool_errors() {
    let (fixture, room, actor) = fixture();
    let mut server = McpServer::new(fixture.broker.clone(), room.clone(), actor.clone());
    let mut pending = Vec::new();
    for _ in 0..64 {
        pending.push(
            fixture
                .broker
                .action_directory_request_queue()
                .try_query(
                    BrokerActor::person(&actor),
                    &room,
                    runtime_collab::ActionDirectoryQuery {
                        peer_id: 71,
                        revision: String::new(),
                        cursor: vec![],
                        parent_resource: String::new(),
                        needle: String::new(),
                        max_results: 1,
                        max_bytes: 1024,
                        max_work: 1,
                        max_depth: 1,
                    },
                )
                .unwrap(),
        );
    }
    let busy = server
        .handle(call("qos_query", json!({"peer_id":71})))
        .await
        .unwrap();
    assert_eq!(busy["result"]["isError"], true);
    assert_eq!(busy["result"]["content"][0]["text"], "Busy");
    drop(fixture);
    let disconnected = server
        .handle(call("qos_query", json!({"peer_id":71})))
        .await
        .unwrap();
    assert_eq!(disconnected["result"]["content"][0]["text"], "Unavailable");
    let (fixture, room, actor) = self::fixture();
    let mut server = McpServer::new(fixture.broker.clone(), room, actor);
    let timeout = server
        .handle(call("qos_query", json!({"peer_id":71})))
        .await
        .unwrap();
    assert_eq!(timeout["result"]["isError"], true);
    assert!(
        timeout["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("TimedOut")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn confirmation_is_not_invented_and_terminal_content_survives_the_broker() {
    tokio::task::LocalSet::new().run_until(async {
        let (fixture,room,actor)=fixture();
        let mut server=McpServer::new(fixture.broker.clone(),room.clone(),actor);
        fixture.set_confirmation_required(true);
        let args=json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}});
        let invoke=server.handle(call("qos_invoke",args.clone()));
        let (reply,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert_eq!(reply.unwrap()["result"]["content"][0]["text"],"ConfirmationRequired");
        fixture.set_confirmation_required(false);
        fixture.set_result_spec(tilo_actions::ResultSpec::Json {schema:json!({"type":"object"})});
        let invoke=server.handle(call("qos_invoke",args));
        let (reply,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        let id=reply["result"]["structuredContent"]["invocation_id"].as_u64().unwrap();
        let invocation=serde_json::from_value(reply["result"]["structuredContent"]["update"]["invocation"].clone()).unwrap();
        let update=tilo_actions::InvocationUpdate::Succeeded {invocation,output:tilo_actions::ActionOutput {value:Some(json!({"content":"sha256:123","width":200})),resources:vec!["content:sha256:123".into()]}};
        fixture.complete_invocation(id,&update).await.unwrap();
        let status=server.handle(call("qos_status",json!({"invocation_id":id}))).await.unwrap();
        assert_eq!(status["result"]["structuredContent"],json!({"invocation_id":id,"update":update}));
        let mut other=McpServer::new(fixture.broker.clone(),room,"foreign".into());
        assert_eq!(other.handle(call("qos_status",json!({"invocation_id":id}))).await.unwrap()["result"]["isError"],true);
    }).await;
}

#[test]
fn container_rows_preserve_their_canonical_visibility_contract() {
    let contract = serde_json::to_value(tilo_actions::ActionContract::view()).unwrap();
    let row = runtime_collab::ActionDirectoryRow {
        parent_resource: "root".into(),
        resource: "aux:document".into(),
        resource_kind: "document".into(),
        resource_label: "A decorative label".into(),
        resource_presentation_json: "{}".into(),
        action: None,
        descriptor: None,
        has_children: true,
        visibility_action: Some("read".into()),
        visibility_contract_json: contract.to_string(),
    };
    let value = row_json(&row).unwrap();
    assert_eq!(value["resource"], "aux:document");
    assert_eq!(value["visibility_action"], "read");
    assert_eq!(value["visibility_contract"], contract);
}

#[tokio::test(flavor = "current_thread")]
async fn request_metadata_is_accepted_without_becoming_broker_authority() {
    tokio::task::LocalSet::new().run_until(async {
        let (mut fixture, room, actor) = fixture();
        let mut server = McpServer::new(fixture.broker.clone(), room, actor.clone());
        let metadata = json!({"progressToken":"progress-1","actor":"foreign","workspace":"foreign-room"});
        let listed = server.handle(request("resources/list", json!({"_meta":metadata}))).await.unwrap();
        assert_eq!(listed["result"]["resources"][0]["uri"], "qos://apps/71", "{listed}");
        let read = server.handle(request("resources/read", json!({"uri":"qos://apps/71","_meta":metadata})));
        let (reply, ()) = tokio::join!(read, async { tokio::task::yield_now().await; drain_directory_bridge(&fixture).await; });
        assert!(reply.unwrap()["result"]["contents"][0]["text"].is_string());
        let mut invoke = call("qos_invoke", json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}}));
        invoke["params"]["_meta"] = metadata.clone();
        let invoke = server.handle(invoke);
        let (reply, ()) = tokio::join!(invoke, async { tokio::task::yield_now().await; drain_directory_bridge(&fixture).await; });
        assert_eq!(reply.unwrap()["result"]["isError"], false);
        assert_eq!(fixture.take_invocation_actor(), Some(actor));
        let mut spoof = call("qos_list_apps", json!({"actor":"foreign"}));
        spoof["params"]["_meta"] = metadata;
        assert_eq!(server.handle(spoof).await.unwrap()["result"]["isError"], true);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_metadata_must_be_an_object_with_a_valid_progress_token() {
    let (fixture, room, actor) = fixture();
    let mut server = McpServer::new(fixture.broker.clone(), room, actor);
    for metadata in [
        json!(null),
        json!(false),
        json!([]),
        json!({"progressToken":{}}),
        json!({"progressToken":1.5}),
    ] {
        for mut input in [
            call("qos_list_apps", json!({})),
            request("initialize", json!({"protocolVersion":"2025-06-18"})),
            request("ping", json!({})),
            request("tools/list", json!({})),
            request("resources/list", json!({})),
            request("resources/read", json!({"uri":"qos://apps/71"})),
        ] {
            input["params"]["_meta"] = metadata.clone();
            assert_eq!(server.handle(input).await.unwrap()["error"]["code"], -32602);
        }
    }
    for metadata in [
        json!({}),
        json!({"progressToken":1}),
        json!({"progressToken":"token"}),
    ] {
        let mut input = call("qos_list_apps", json!({}));
        input["params"]["_meta"] = metadata;
        assert_eq!(
            server.handle(input).await.unwrap()["result"]["isError"],
            false
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn first_allocated_application_peer_supports_query_resolve_and_invoke() {
    tokio::task::LocalSet::new().run_until(async {
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::test_default(WorkspaceId(71), "First connection");
        let room = workspace.room.to_string();
        let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
        workspace.owner = Some(actor.clone());
        tree.workspaces.push(workspace);
        tree.set_active(WorkspaceId(71));
        let mut fixture = crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_first_allocated_peer(tree.shared());
        let mut server = McpServer::new(fixture.broker.clone(), room, actor.clone());
        let listed = server.handle(call("qos_list_apps", json!({}))).await.unwrap();
        let app = &listed["result"]["structuredContent"]["apps"][0];
        let peer_id = app["peer_id"].as_u64().unwrap();
        assert_eq!(app["record_id"], "atlas");
        let query = server.handle(call("qos_query", json!({"peer_id":peer_id})));
        let (reply, ()) = tokio::join!(query, async {tokio::task::yield_now().await; drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        assert_eq!(reply["result"]["isError"], false, "first published peer {peer_id} must be queryable: {reply}");
        let page = &reply["result"]["structuredContent"];
        let resource = &page["rows"][0]["resource"];
        assert_eq!(resource, "workspace:atlas");
        let target = json!({"peer_id":peer_id,"revision":page["revision"],"resource":resource,"action":page["rows"][0]["action"]});
        let resolve = server.handle(call("qos_resolve", target.clone()));
        let (reply, ()) = tokio::join!(resolve, async {tokio::task::yield_now().await; drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["row"]["resource"], "workspace:atlas");
        let mut target = target;
        target["arguments"] = json!({});
        let invoke = server.handle(call("qos_invoke", target));
        let (reply, ()) = tokio::join!(invoke, async {tokio::task::yield_now().await; drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["update"]["state"], "accepted");
        assert_eq!(fixture.take_invocation_actor(), Some(actor));
        assert_eq!(fixture.resolve_count(), 2, "invoke must re-resolve, not execute the previous descriptor");
    }).await;
}
