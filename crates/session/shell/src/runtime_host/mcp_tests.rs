#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
use crate::shell::collab_broker_capnp::tests::{
    DirectoryResolveMode, drain_directory_bridge, local_directory_fixture_with_membership,
};
use crate::shell::remote_actions::BrokerActionHost;
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
use serde_json::{Value, json};
use tilo_runtime::mcp::McpServer;

fn fixture() -> (
    crate::shell::collab_broker_capnp::tests::LocalDirectoryFixture,
    String,
    String,
) {
    let mut tree = WorkspaceTree::default();
    let mut workspace = Workspace::new(WorkspaceId(71), "MCP");
    let room = workspace.room.to_string();
    let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
    workspace.set_owner(actor.clone());
    tree.desktop.workspaces.push(workspace);
    tree.set_active(WorkspaceId(71));
    (
        local_directory_fixture_with_membership(tree.shared()),
        room,
        actor,
    )
}
/// The next invocation reached the app as the MCP client's agent, recorded
/// as an agent acting for `person`: never as the person themself.
fn assert_agent_for(
    fixture: &mut crate::shell::collab_broker_capnp::tests::LocalDirectoryFixture,
    person: &str,
) {
    let (actor, invoker) = fixture
        .take_invocation_invoker()
        .expect("the invocation reached the app");
    assert!(actor.starts_with("agent:mcp-"), "{actor}");
    let invoker = invoker.expect("the host records the invoker");
    assert_eq!(invoker.actor, actor);
    assert_eq!(invoker.kind, tilo_actions::ActorKind::Agent);
    assert_eq!(invoker.on_behalf_of.as_deref(), Some(person));
}

fn request(method: &str, params: &Value) -> Value {
    json!({"jsonrpc":"2.0", "id":1,"method":method,"params":params})
}
fn call(name: &str, args: &Value) -> Value {
    request("tools/call", &json!({"name":name,"arguments":args}))
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn protocol_errors_and_request_metadata_are_validated() {
    // Phase `unknown_tool_is_an_invalid_params_error_with_the_request_id`.
    {
        let (fixture, room, actor) = fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
        let reply = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": "unknown-tool-request",
                "method": "tools/call",
                "params": {"name": "tilo_not_a_tool", "arguments": {}}
            }))
            .await
            .unwrap();

        assert_eq!(reply["id"], "unknown-tool-request");
        assert_eq!(reply["error"]["code"], -32602);
        assert!(reply.get("result").is_none(), "{reply}");
    }
    // Phase `null_id_is_rejected_before_mutating_dispatch_and_valid_id_boundaries_hold`.
    tokio::task::LocalSet::new()
        .run_until(async {
            let (fixture, room, actor) = fixture();
            let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
            let mut null_id = call(
                "tilo_invoke",
                &json!({
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
    // Phase `protocol_and_argument_boundaries`.
    {
        let (fixture, room, actor) = fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
        let init = server.handle(request("initialize",&json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}))).await.unwrap();
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
            server.handle(request("ping", &json!({}))).await.unwrap()["result"],
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
            server.handle(request("missing", &json!({}))).await.unwrap()["error"]["code"],
            -32601
        );
        let tools = server
            .handle(request("tools/list", &json!({})))
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
                server.handle(call("tilo_list_apps", &args)).await.unwrap()["result"]["isError"],
                true
            );
        }
        assert_eq!(
            server
                .handle(call(
                    "tilo_status",
                    &json!({"invocation_id":"00000000-0000-0000-0000-0000000003e7"})
                ))
                .await
                .unwrap()["result"]["isError"],
            true
        );
        assert_eq!(
            server
                .handle(call(
                    "tilo_cancel",
                    &json!({"invocation_id":"00000000-0000-0000-0000-0000000003e7"})
                ))
                .await
                .unwrap()["result"]["isError"],
            true
        );
    }
    // Phase `request_metadata_is_accepted_without_becoming_broker_authority`.
    tokio::task::LocalSet::new().run_until(async {
        let (mut fixture, room, actor) = fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor.clone());
        let metadata = json!({"progressToken":"progress-1","actor":"foreign","workspace":"foreign-room"});
        let listed = server.handle(request("resources/list", &json!({"_meta":metadata}))).await.unwrap();
        assert_eq!(listed["result"]["resources"][0]["uri"], "tilo://apps/71", "{listed}");
        let read = server.handle(request("resources/read", &json!({"uri":"tilo://apps/71","_meta":metadata})));
        let (reply, ()) = tokio::join!(read, async { tokio::task::yield_now().await; drain_directory_bridge(&fixture).await; });
        assert!(reply.unwrap()["result"]["contents"][0]["text"].is_string());
        let mut invoke = call("tilo_invoke", &json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{},"wait_ms":0}));
        invoke["params"]["_meta"] = metadata.clone();
        let invoke = server.handle(invoke);
        let (reply, ()) = tokio::join!(invoke, async { tokio::task::yield_now().await; drain_directory_bridge(&fixture).await; });
        assert_eq!(reply.unwrap()["result"]["isError"], false);
        assert_agent_for(&mut fixture, &actor);
        let mut spoof = call("tilo_list_apps", &json!({"actor":"foreign"}));
        spoof["params"]["_meta"] = metadata;
        assert_eq!(server.handle(spoof).await.unwrap()["result"]["isError"], true);
    }).await;
    // Phase `request_metadata_must_be_an_object_with_a_valid_progress_token`.
    {
        let (fixture, room, actor) = fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
        for metadata in [
            json!(null),
            json!(false),
            json!([]),
            json!({"progressToken":{}}),
            json!({"progressToken":1.5}),
        ] {
            for mut input in [
                call("tilo_list_apps", &json!({})),
                request("initialize", &json!({"protocolVersion":"2025-06-18"})),
                request("ping", &json!({})),
                request("tools/list", &json!({})),
                request("resources/list", &json!({})),
                request("resources/read", &json!({"uri":"tilo://apps/71"})),
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
            let mut input = call("tilo_list_apps", &json!({}));
            input["params"]["_meta"] = metadata;
            assert_eq!(
                server.handle(input).await.unwrap()["result"]["isError"],
                false
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn broker_queries_and_resources_are_live_and_authority_scoped() {
    // Phase `actual_broker_query_resolve_and_stale_invocation`.
    tokio::task::LocalSet::new().run_until(async {
        let (fixture, room, actor) = fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room,actor);
        let roots=server.handle(call("tilo_list_apps",&json!({}))).await.unwrap();
        assert_eq!(roots["result"]["structuredContent"]["apps"][0]["peer_id"],71);
        let query=server.handle(call("tilo_query",&json!({"peer_id":71,"detail":"full"})));
        let (reply,())=tokio::join!(query, async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        assert_eq!(reply["result"]["structuredContent"]["revision"],"directory-r1");
        assert!(reply["result"]["structuredContent"]["rows"][0]["descriptor"]["params"].is_array());
        fixture.set_resolve_mode(DirectoryResolveMode::Stale);
        let invoke=server.handle(call("tilo_invoke",&json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}})));
        let (reply,())=tokio::join!(invoke, async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert_eq!(reply.unwrap()["result"]["isError"],true);
    }).await;
    // Phase `resources_are_live_room_scoped_and_reads_use_authority`.
    tokio::task::LocalSet::new().run_until(Box::pin(async {
        let (fixture, room, actor) = fixture();
        let server=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room.clone(),actor);
        let resources=server.handle(request("resources/list",&json!({}))).await.unwrap();
        assert_eq!(resources["result"]["resources"][0]["uri"],"tilo://apps/71");
        let read=server.handle(request("resources/read",&json!({"uri":"tilo://apps/71"})));
        let (reply,())=tokio::join!(read,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert!(reply.unwrap()["result"]["contents"][0]["text"].as_str().unwrap().contains("directory-r1"));
        let other=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),"different-room".into(),"outsider".into());
        assert_eq!(other.handle(request("resources/list",&json!({}))).await.unwrap()["result"]["resources"],json!([]));
        assert!(other.handle(request("resources/read",&json!({"uri":"tilo://apps/71"}))).await.unwrap().get("error").is_some());
        let denied=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room,"outsider".into());
        let read=denied.handle(request("resources/read",&json!({"uri":"tilo://apps/71"})));
        let (reply,())=tokio::join!(read,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        // Admission to the room is not membership: an agent acting for
        // someone who is not a member takes part in nothing there, so the
        // read is refused and names nothing of the room.
        let reply = reply.unwrap();
        assert!(reply.get("error").is_some(), "{reply}");
        assert!(reply.get("result").is_none(), "{reply}");
        assert!(!reply.to_string().contains("workspace:atlas"), "{reply}");
        let call=denied.handle(call("tilo_invoke",&json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{}})));
        let (reply,())=tokio::join!(call,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        assert_eq!(reply.unwrap()["result"]["isError"],true);
    })).await;
    // Phase `root_identity_pagination_and_disconnect_are_exact`.
    {
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
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
        let first = server
            .handle(call("tilo_list_apps", &json!({"max_results":500})))
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
            .handle(call("tilo_list_apps", &json!({"after_peer":128})))
            .await
            .unwrap();
        assert_eq!(
            last["result"]["structuredContent"]["apps"][0]["record_id"],
            "auxiliary/129"
        );
        assert_eq!(
            last["result"]["structuredContent"]["apps"][1]["uri"],
            "tilo://apps/130"
        );
        fixture
            .broker
            .replace_action_directory_roots_for_test(vec![]);
        assert_eq!(
            server
                .handle(call("tilo_query", &json!({"peer_id":129})))
                .await
                .unwrap()["result"]["isError"],
            true
        );
    }
    // Phase `first_allocated_application_peer_supports_query_resolve_and_invoke`.
    // Boxed: the query, resolve and invoke turns keep a large future alive.
    tokio::task::LocalSet::new().run_until(Box::pin(async {
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId(71), "First connection");
        let room = workspace.room.to_string();
        let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
        workspace.set_owner(actor.clone());
        tree.desktop.workspaces.push(workspace);
        tree.set_active(WorkspaceId(71));
        let mut fixture = crate::shell::collab_broker_capnp::tests::local_directory_fixture_with_first_allocated_peer(tree.shared());
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor.clone());
        let listed = server.handle(call("tilo_list_apps", &json!({}))).await.unwrap();
        let app = &listed["result"]["structuredContent"]["apps"][0];
        let peer_id = app["peer_id"].as_u64().unwrap();
        assert_eq!(app["record_id"], "atlas");
        let query = server.handle(call("tilo_query", &json!({"peer_id":peer_id})));
        let (reply, ()) = tokio::join!(query, async {tokio::task::yield_now().await; drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        assert_eq!(reply["result"]["isError"], false, "first published peer {peer_id} must be queryable: {reply}");
        let page = &reply["result"]["structuredContent"];
        let resource = &page["resources"][0]["resource"];
        assert_eq!(resource, "workspace:atlas", "{page}");
        let action = page["resources"][0]["actions"].as_object().and_then(|actions| actions.keys().next()).cloned();
        let target = json!({"peer_id":peer_id,"revision":page["revision"],"resource":resource,"action":action});
        let resolve = server.handle(call("tilo_resolve", &target));
        let (reply, ()) = tokio::join!(resolve, async {tokio::task::yield_now().await; drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["row"]["resource"], "workspace:atlas");
        let mut target = target;
        target["arguments"] = json!({});
        target["wait_ms"] = json!(0);
        let invoke = server.handle(call("tilo_invoke", &target));
        let (reply, ()) = tokio::join!(invoke, async {tokio::task::yield_now().await; drain_directory_bridge(&fixture).await;});
        let reply = reply.unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(reply["result"]["structuredContent"]["update"]["state"], "accepted");
        assert_agent_for(&mut fixture, &actor);
        assert_eq!(fixture.resolve_count(), 2, "invoke must re-resolve, not execute the previous descriptor");
    })).await;
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn invocations_preserve_actor_arguments_and_start_once() {
    // Phase `accepted_invocation_preserves_actor_and_cancellation_ownership`.
    tokio::task::LocalSet::new().run_until(async {
        let (mut fixture,room,actor)=fixture();
        let server=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room.clone(),actor.clone());
        let invoke=server.handle(call("tilo_invoke",&json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{},"wait_ms":0})));
        let (reply,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        assert_eq!(reply["result"]["isError"],false,"{reply}");
        let id=reply["result"]["structuredContent"]["invocation_id"].as_str().unwrap().to_owned();
        assert_eq!(reply["result"]["structuredContent"]["update"]["state"],"accepted");
        assert_agent_for(&mut fixture, &actor);
        assert_eq!(fixture.resolve_count(),1);
        let foreign=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room,"foreign".into());
        for tool in ["tilo_status","tilo_cancel"] {
            assert_eq!(foreign.handle(call(tool,&json!({"invocation_id":id}))).await.unwrap()["result"]["isError"],true);
        }
        let status=server.handle(call("tilo_status",&json!({"invocation_id":id}))).await.unwrap();
        assert_eq!(status["result"]["structuredContent"]["invocation_id"],id);
        assert_ne!(status["result"]["structuredContent"]["update"]["state"],"succeeded");
    }).await;
    // Phase `a_retried_invocation_id_starts_the_action_once`.
    tokio::task::LocalSet::new().run_until(async {
        let (mut fixture,room,actor)=fixture();
        let server=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room.clone(),actor.clone());
        let id="6a1e5c3a-9d2f-4a6b-8c1e-2f3a4b5c6d7e";
        let args=json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{},"invocation_id":id,"wait_ms":0});
        let invoke=server.handle(call("tilo_invoke",&args));
        let (first,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let first=first.unwrap();
        assert_eq!(first["result"]["structuredContent"]["invocation_id"],id,"{first}");
        assert_agent_for(&mut fixture, &actor);
        let retry=server.handle(call("tilo_invoke",&args)).await.unwrap();
        assert_eq!(retry["result"]["structuredContent"],first["result"]["structuredContent"]);
        assert_eq!(fixture.take_invocation_actor(),None,"a retry never reaches the app again");
        assert_eq!(fixture.resolve_count(),1,"nor re-resolves the directory");
        let other=json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{"changed":true},"invocation_id":id});
        assert_eq!(server.handle(call("tilo_invoke",&other)).await.unwrap()["result"]["isError"],true,"an id cannot be reused for another request");
    }).await;
    // Phase `invocation_preserves_typed_argument_maps_through_the_broker`.
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut fixture, room, actor) = fixture();
            let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
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
                    "tilo_invoke",
                    &json!({
                        "peer_id": 71,
                        "revision": "directory-r1",
                        "resource": "workspace:atlas",
                        "action": "open",
                        "arguments": arguments,
                        "wait_ms": 0
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
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn invocation_targets_queue_limits_and_results_are_honest() {
    // Phase `targets_require_a_nonempty_revision_before_enqueueing`.
    {
        let (fixture, room, actor) = fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
        for name in ["tilo_resolve", "tilo_invoke"] {
            let mut args =
                json!({"peer_id":71,"revision":"","resource":"workspace:atlas","action":"open"});
            if name == "tilo_invoke" {
                args["arguments"] = json!({});
            }
            let reply = tilo_test_env::within(
                "invalid revision must be rejected without waiting for broker",
                server.handle(call(name, &args)),
            )
            .await
            .unwrap();
            assert_eq!(reply["result"]["isError"], true);
        }
    }
    // Phase `actual_queue_backpressure_timeout_and_disconnect_are_tool_errors`.
    {
        let (fixture, room, actor) = fixture();
        let server = McpServer::new(
            BrokerActionHost::new(fixture.broker.clone()),
            room.clone(),
            actor.clone(),
        );
        #[expect(
            clippy::collection_is_never_read,
            reason = "the queued requests are held only to keep the directory queue full"
        )]
        let mut pending = Vec::new();
        for _ in 0..64 {
            pending.push(
                fixture
                    .broker
                    .state()
                    .action_directory_request_queue()
                    .try_query(
                        crate::shell::collab_broker_capnp::BrokerActor::person(&actor),
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
            .handle(call("tilo_query", &json!({"peer_id":71})))
            .await
            .unwrap();
        assert_eq!(busy["result"]["isError"], true);
        assert!(
            busy["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Busy")),
            "{busy}"
        );
        drop(fixture);
        let disconnected = server
            .handle(call("tilo_query", &json!({"peer_id":71})))
            .await
            .unwrap();
        assert!(
            disconnected["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Unavailable")),
            "{disconnected}"
        );
        let (fixture, room, actor) = self::fixture();
        let server = McpServer::new(BrokerActionHost::new(fixture.broker.clone()), room, actor);
        let timeout = server
            .handle(call("tilo_query", &json!({"peer_id":71})))
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
    // Phase `the_person_confirms_on_their_device_and_terminal_content_survives_the_broker`.
    // The MCP client's own allow/deny decision confirms nothing: an action
    // whose contract asks for confirmation is put to the person on their
    // device, and runs on their answer.
    tokio::task::LocalSet::new().run_until(Box::pin(async {
        let (mut fixture,room,actor)=fixture();
        let actor_name=actor.clone();
        let asked=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        fixture.broker.set_app_action_consent(std::sync::Arc::new(AnsweringPerson {
            asked: std::sync::Arc::clone(&asked),
            answer: AppActionConsent::Deny,
        }));
        let server=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room.clone(),actor);
        fixture.set_confirmation_required(true);
        let args=json!({"peer_id":71,"revision":"directory-r1","resource":"workspace:atlas","action":"open","arguments":{},"wait_ms":0});
        let invoke=server.handle(call("tilo_invoke",&args));
        let (reply,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        assert_eq!(reply["result"]["isError"],true,"the person declined: {reply}");
        fixture.broker.set_app_action_consent(std::sync::Arc::new(AnsweringPerson {
            asked: std::sync::Arc::clone(&asked),
            answer: AppActionConsent::Once,
        }));
        // Resolved twice: asked, then started on the person's answer.
        let invoke=server.handle(call("tilo_invoke",&args));
        tokio::pin!(invoke);
        let reply=loop {
            tokio::select! {
                reply = &mut invoke => break reply,
                () = async {
                    tokio::task::yield_now().await;
                    drain_directory_bridge(&fixture).await;
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                } => {}
            }
        };
        let reply=reply.unwrap();
        assert_eq!(reply["result"]["isError"],false,"{reply}");
        assert_eq!(reply["result"]["structuredContent"]["update"]["state"],"accepted");
        {
            let asked = asked.lock().unwrap();
            assert_eq!(asked.len(), 2, "the person was asked each time");
            assert!(asked.iter().all(|question| question.confirming && question.person == actor_name));
        }
        fixture.set_confirmation_required(false);
        fixture.set_result_spec(tilo_actions::ResultSpec::Json {schema:json!({"type":"object"})});
        let invoke=server.handle(call("tilo_invoke",&args));
        let (reply,())=tokio::join!(invoke,async {tokio::task::yield_now().await;drain_directory_bridge(&fixture).await;});
        let reply=reply.unwrap();
        let id=reply["result"]["structuredContent"]["invocation_id"].as_str().unwrap().to_owned();
        let invocation:tilo_actions::InvocationId=serde_json::from_value(reply["result"]["structuredContent"]["update"]["invocation"].clone()).unwrap();
        assert_eq!(invocation.0.to_string(),id,"the update speaks in the caller's invocation id");
        // The job is the MCP client's agent's, acting for the person.
        let tilo_actions::remote::CallerPrincipal::Agent(agent) = server.execution_caller().principal().clone() else {
            panic!("the MCP client is an agent");
        };
        assert_eq!(server.execution_caller().acting_for_person(), Some(actor_name.as_str()));
        let job=fixture.broker.statuses(&format!("agent:{agent}")).into_iter().map(|(job,_)|job).max().expect("the broker job behind it");
        let output=tilo_actions::ActionOutput {value:Some(json!({"content":"sha256:123","width":200})),resources:vec!["content:sha256:123".into()]};
        // Scoped applications answer with the exact stable UUID they received.
        let (first_job,first_invocation)=fixture.take_invocation_identity().expect("first actual delivery");
        let (delivered_job,delivered_invocation)=fixture.take_invocation_identity().expect("second actual delivery");
        assert_ne!(first_job,delivered_job);
        assert_ne!(first_invocation,delivered_invocation);
        assert_eq!(delivered_job,job);
        assert_eq!(delivered_invocation,invocation);
        fixture.complete_invocation(job,&tilo_actions::InvocationUpdate::Succeeded {invocation:delivered_invocation,output:output.clone()}).await.unwrap();
        let update=tilo_actions::InvocationUpdate::Succeeded {invocation,output};
        let status=server.handle(call("tilo_status",&json!({"invocation_id":id}))).await.unwrap();
        assert_eq!(status["result"]["structuredContent"],json!({"invocation_id":id,"update":update}));
        let other=McpServer::new(BrokerActionHost::new(fixture.broker.clone()),room,"foreign".into());
        assert_eq!(other.handle(call("tilo_status",&json!({"invocation_id":id}))).await.unwrap()["result"]["isError"],true);
    })).await;
}

/// The person at their device, answering every confirmation `answer`.
struct AnsweringPerson {
    asked: std::sync::Arc<
        std::sync::Mutex<Vec<tilo_action_broker::app_actions::AppActionConsentRequest>>,
    >,
    answer: AppActionConsent,
}

impl tilo_action_broker::app_actions::AppActionConsentPrompter for AnsweringPerson {
    fn ask(
        &self,
        request: tilo_action_broker::app_actions::AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionConsent>> {
        self.asked.lock().unwrap().push(request);
        let (answer, asked) = tokio::sync::oneshot::channel();
        let _ = answer.send(self.answer);
        Some(asked)
    }
}

use tilo_action_broker::app_actions::AppActionConsent;
