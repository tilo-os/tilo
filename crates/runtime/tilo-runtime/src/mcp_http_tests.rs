use super::*;

#[path = "mcp_admission_tests.rs"]
mod admission_tests;
#[path = "mcp_settle_tests.rs"]
mod settle_tests;
use hyper::HeaderMap;
use std::sync::Mutex as StdMutex;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};

mod schema_rows {
    use tilo_resource::{Model, resource};

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub title: String,
    }

    #[resource(kind = "test.note", label = "Note")]
    impl Note {
        /// Rename the note.
        #[action]
        pub fn rename(&mut self, #[param("New title")] title: String) {
            self.title = title;
        }
    }
}

#[derive(Clone, Copy, Default)]
enum CatalogueAvailability {
    #[default]
    Ready,
    Hidden,
    NotPermitted,
    ExitDuringQuery,
}

#[derive(Clone, Default)]
struct StubHost {
    activation: bool,
    availability: CatalogueAvailability,
    exited: Arc<std::sync::atomic::AtomicBool>,
    record: Option<String>,
    room: Option<String>,
    invoked: Arc<StdMutex<Vec<InvokeRequest>>>,
    callers: Arc<StdMutex<Vec<Caller>>>,
    confirmation: tilo_actions::ConfirmationPolicy,
    /// Observations a started invocation stays accepted for before it
    /// succeeds.
    running_for: Arc<std::sync::atomic::AtomicUsize>,
    /// What a finished invocation produced.
    output: Arc<StdMutex<tilo_actions::ActionOutput>>,
    /// The kind of operation the published action is.
    kind: Arc<StdMutex<tilo_actions::OperationKind>>,
}

impl StubHost {
    fn row(&self) -> CatalogueRow {
        let resource = tilo_resource::publish::<schema_rows::Note>(
            "test.note:1",
            "Note",
            &schema_rows::Note::default(),
        );
        let mut descriptor = tilo_actions::door::survey(&[resource])
            .into_iter()
            .find(|descriptor| descriptor.address.action == tilo_resource::APPLY)
            .unwrap();
        descriptor.contract.confirmation = self.confirmation.clone();
        descriptor.kind = *self.kind.lock().unwrap();
        if self.activation {
            descriptor.address.action = tilo_actions::remote::APPLICATION_ACTIVATE_ACTION.into();
            descriptor.kind = tilo_actions::OperationKind::View;
            descriptor.contract = tilo_actions::ActionContract::view();
            descriptor.params.clear();
            descriptor.input = tilo_actions::door::InputSpec::None;
        }
        CatalogueRow {
            provider: "71".into(),
            app_id: "so.tilo.test".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: "test.note:1".into(),
                kind: "test.note".into(),
                label: "Note".into(),
                presentation: None,
            },
            action: Some(descriptor),
            has_children: false,
            fresh_binding: None,
            visibility: None,
        }
    }
}

impl ActionHost for StubHost {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if matches!(self.availability, CatalogueAvailability::ExitDuringQuery)
            && query.provider.is_some()
        {
            self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: "owner exited".into(),
            }));
        }
        if matches!(self.availability, CatalogueAvailability::NotPermitted)
            && query.provider.is_some()
        {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        let mut row = self.row();
        if let Some(provider) = query.provider.clone() {
            row.provider = provider;
        }
        let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
        page.providers = vec![CatalogueProvider {
            provider: "71".into(),
            app_id: "so.tilo.test".into(),
            room: self.room.clone().unwrap_or_else(|| "room".into()),
            record_id: self.record.clone().unwrap_or_default(),
            browsable: true,
        }];
        if matches!(self.availability, CatalogueAvailability::Hidden)
            || self.exited.load(std::sync::atomic::Ordering::SeqCst)
        {
            page.providers.clear();
        }
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        if let tilo_actions::ConfirmationPolicy::Phrase { phrase } = &self.confirmation
            && request.confirmation
                != Some(tilo_actions::remote::Confirmation::Phrase(phrase.clone()))
        {
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        }
        self.callers.lock().unwrap().push(caller.clone());
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationState::new(
            invocation,
            InvocationUpdate::Accepted { invocation },
        )
        .receipt()))
    }

    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        use std::sync::atomic::Ordering;
        let started = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.invocation == *id);
        let running = self
            .running_for
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        Some(InvocationState::new(
            *id,
            if started && !running {
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: self.output.lock().unwrap().clone(),
                }
            } else {
                InvocationUpdate::Accepted { invocation: *id }
            },
        ))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        Ok(())
    }
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn default_activation_rechecks_an_owner_that_exits_during_admission() {
    let host = StubHost {
        activation: true,
        availability: CatalogueAvailability::ExitDuringQuery,
        ..StubHost::default()
    };
    let invoked = host.invoked.clone();
    let server = McpServer::new(host, "room".into(), "owner".into());
    assert_eq!(run(server.activate_existing("so.tilo.test")).unwrap(), None);
    assert!(invoked.lock().unwrap().is_empty());
}

#[test]
fn default_activation_uses_the_existing_permission_checked_view_door() {
    let host = StubHost {
        activation: true,
        ..StubHost::default()
    };
    let invoked = host.invoked.clone();
    let server = McpServer::new(host, "room".into(), "owner".into());
    run(async {
        assert_eq!(
            server.activate_existing("so.tilo.test").await.unwrap(),
            Some(71)
        );
    });
    let requests = invoked.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target.provider, "71");
    assert_eq!(requests[0].target.address.resource, "test.note:1");
    assert_eq!(
        requests[0].target.address.action,
        tilo_actions::remote::APPLICATION_ACTIVATE_ACTION
    );
    drop(requests);
}

#[test]
fn default_activation_does_not_confuse_other_records_or_absence_with_a_live_owner() {
    for host in [
        StubHost {
            activation: true,
            availability: CatalogueAvailability::Hidden,
            ..StubHost::default()
        },
        StubHost {
            activation: true,
            record: Some("uuid:another-record".into()),
            ..StubHost::default()
        },
    ] {
        let invoked = host.invoked.clone();
        let server = McpServer::new(host, "room".into(), "owner".into());
        run(async {
            assert_eq!(
                server.activate_existing("so.tilo.test").await.unwrap(),
                None
            );
        });
        assert!(invoked.lock().unwrap().is_empty());
    }
}

#[test]
fn default_activation_refuses_unready_and_older_owners_without_starting_an_action() {
    for host in [
        StubHost::default(),
        StubHost {
            availability: CatalogueAvailability::NotPermitted,
            ..StubHost::default()
        },
    ] {
        let invoked = host.invoked.clone();
        let server = McpServer::new(host, "room".into(), "owner".into());
        run(async {
            assert!(server.activate_existing("so.tilo.test").await.is_err());
        });
        assert!(invoked.lock().unwrap().is_empty());
    }
}

fn endpoint(host: StubHost, token: Option<&str>) -> Arc<Endpoint<StubHost>> {
    Endpoint::new(
        host,
        "room".into(),
        "owner".into(),
        token.map(str::to_owned),
        None,
    )
}

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(*name, HeaderValue::from_str(value).unwrap());
    }
    map
}

async fn body(response: Response<Full<Bytes>>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn initialize(client: &str) -> Vec<u8> {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":client,"version":"1"}}}).to_string().into_bytes()
}

async fn open(endpoint: &Arc<Endpoint<StubHost>>, client: &str) -> String {
    let response = endpoint
        .dispatch(
            &Method::POST,
            "/mcp",
            &HeaderMap::new(),
            &initialize(client),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get(SESSION_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

async fn invoke(endpoint: &Arc<Endpoint<StubHost>>, session: &str) -> Value {
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_invoke","arguments":{"peer_id":71,"revision":"1","resource":"test.note:1","action":tilo_resource::APPLY,"arguments":{}}}});
    let response = endpoint
        .dispatch(
            &Method::POST,
            "/mcp",
            &headers(&[(SESSION_HEADER, session)]),
            call.to_string().as_bytes(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body(response).await
}

#[test]
fn health_answers_without_a_token() {
    let endpoint = endpoint(StubHost::default(), Some("secret"));
    let response = run(endpoint.dispatch(&Method::GET, "/health", &HeaderMap::new(), b""));
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn a_configured_token_is_required_for_mcp() {
    let endpoint = endpoint(StubHost::default(), Some("secret"));
    run(async {
        let refused = endpoint
            .dispatch(&Method::POST, "/mcp", &HeaderMap::new(), &initialize("a"))
            .await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        let wrong = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[("authorization", "Bearer nope")]),
                &initialize("a"),
            )
            .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let admitted = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[("authorization", "Bearer secret")]),
                &initialize("a"),
            )
            .await;
        assert_eq!(admitted.status(), StatusCode::OK);
    });
}

#[test]
fn sessions_are_created_by_initialize_and_required_after() {
    let endpoint = endpoint(StubHost::default(), None);
    run(async {
        let list = json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}).to_string();
        let missing = endpoint
            .dispatch(&Method::POST, "/mcp", &HeaderMap::new(), list.as_bytes())
            .await;
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        let unknown = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[(SESSION_HEADER, "nope")]),
                list.as_bytes(),
            )
            .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let session = open(&endpoint, "a").await;
        let listed = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[(SESSION_HEADER, &session)]),
                list.as_bytes(),
            )
            .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let tools = body(listed).await;
        let invoke = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "tilo_invoke")
            .unwrap()
            .clone();
        assert_eq!(invoke["annotations"]["destructiveHint"], true);

        let notification =
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
        let accepted = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[(SESSION_HEADER, &session)]),
                notification.as_bytes(),
            )
            .await;
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);

        let deleted = endpoint
            .dispatch(
                &Method::DELETE,
                "/mcp",
                &headers(&[(SESSION_HEADER, &session)]),
                b"",
            )
            .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        let gone = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers(&[(SESSION_HEADER, &session)]),
                list.as_bytes(),
            )
            .await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);
        assert_eq!(endpoint.session_count(), 0);
    });
}

#[test]
fn each_session_is_its_own_attributed_agent_run() {
    let host = StubHost::default();
    let invoked = Arc::clone(&host.invoked);
    let callers = Arc::clone(&host.callers);
    let endpoint = endpoint(host, None);
    run(async {
        let first = open(&endpoint, "Researcher").await;
        let second = open(&endpoint, "Writer").await;
        assert_ne!(first, second);
        invoke(&endpoint, &first).await;
        invoke(&endpoint, &second).await;
        invoke(&endpoint, &first).await;
    });
    let groups: Vec<_> = invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.context.clone().unwrap().group.unwrap())
        .collect();
    assert_eq!(groups.len(), 3);
    assert_ne!(groups[0].id, groups[1].id);
    assert_eq!(groups[0], groups[2]);
    assert!(groups.iter().all(|group| group.kind == "agent-run"));
    assert_eq!(groups[0].label, "Researcher");
    assert_eq!(groups[1].label, "Writer");
    let executions: Vec<_> = callers
        .lock()
        .unwrap()
        .iter()
        .map(|caller| caller.execution_id().expect("admitted execution"))
        .collect();
    assert_ne!(executions[0], executions[1]);
    assert_eq!(executions[0], executions[2]);
}

/// A stdio connection (one `--connect` client, as `.cursor/mcp.json` runs)
/// is also its own agent run, named by the client at `initialize`, so its
/// edits are attributed in history like an HTTP session's.
#[test]
fn each_stdio_connection_is_its_own_attributed_agent_run() {
    let host = StubHost::default();
    let invoked = Arc::clone(&host.invoked);
    run(async {
        for name in ["Painter", "Critic"] {
            let server = crate::mcp::McpServer::new(host.clone(), "room".into(), "owner".into());
            server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":name,"version":"1"}}}))
                .await
                .unwrap();
            tool(
                &server,
                "tilo_invoke",
                json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY}),
            )
            .await;
        }
    });
    let groups: Vec<_> = invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.context.clone().unwrap().group.unwrap())
        .collect();
    assert_eq!(groups.len(), 2);
    assert_ne!(groups[0].id, groups[1].id);
    assert_eq!(
        (groups[0].label.as_str(), groups[1].label.as_str()),
        ("Painter", "Critic")
    );
    assert!(groups.iter().all(|group| group.kind == "agent-run"));
}

/// A client that connects again names the run its first connection was
/// answered with, and its edits stay that one run's: undo reaches them.
#[test]
fn a_connection_continues_the_run_an_earlier_one_was_answered_with() {
    let host = StubHost::default();
    let invoked = Arc::clone(&host.invoked);
    let endpoint = endpoint(host.clone(), None);
    run(async {
        let first = crate::mcp::McpServer::new(host.clone(), "room".into(), "owner".into());
        let answer = first
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Painter","version":"1"}}}))
            .await
            .unwrap();
        let run_id = answer["result"]["capabilities"]["experimental"]["so.tilo.mcp"]["run"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(run_id.starts_with("mcp:"), "{answer}");
        tool(
            &first,
            "tilo_invoke",
            json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY}),
        )
        .await;
        drop(first);

        let again = crate::mcp::McpServer::new(host.clone(), "room".into(), "owner".into());
        let answer = again
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"experimental":{"so.tilo.mcp":{"run":run_id}}},"clientInfo":{"name":"Painter","version":"1"}}}))
            .await
            .unwrap();
        assert_eq!(
            answer["result"]["capabilities"]["experimental"]["so.tilo.mcp"]["run"],
            json!(run_id)
        );
        tool(
            &again,
            "tilo_invoke",
            json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY}),
        )
        .await;

        // An HTTP session continues a run the same way.
        let request = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"experimental":{"so.tilo.mcp":{"run":run_id}}},"clientInfo":{"name":"Painter","version":"1"}}});
        let response = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &HeaderMap::new(),
                request.to_string().as_bytes(),
            )
            .await;
        let session = response
            .headers()
            .get(SESSION_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        invoke(&endpoint, &session).await;

        let bad = crate::mcp::McpServer::new(host, "room".into(), "owner".into());
        let refused = bad
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"experimental":{"so.tilo.mcp":{"run":"two words"}}}}}))
            .await
            .unwrap();
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("run must be"),
            "{refused}"
        );
    });
    let groups: Vec<_> = invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.context.clone().unwrap().group.unwrap())
        .collect();
    assert_eq!(groups.len(), 3);
    assert!(
        groups.iter().all(|group| group.id == groups[0].id),
        "{groups:?}"
    );
    assert!(
        groups.iter().all(|group| group.label == "Painter"),
        "{groups:?}"
    );
}

#[test]
fn a_contract_asking_for_confirmation_is_confirmed_by_the_admitted_call() {
    for (policy, expected) in [
        (
            tilo_actions::ConfirmationPolicy::None,
            Some(tilo_actions::remote::Confirmation::Confirmed),
        ),
        (
            tilo_actions::ConfirmationPolicy::Confirm,
            Some(tilo_actions::remote::Confirmation::Confirmed),
        ),
        (
            tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "ERASE".into(),
            },
            Some(tilo_actions::remote::Confirmation::Phrase("ERASE".into())),
        ),
    ] {
        let host = StubHost {
            confirmation: policy,
            ..StubHost::default()
        };
        let invoked = Arc::clone(&host.invoked);
        let endpoint = endpoint(host, None);
        let reply = run(async {
            let session = open(&endpoint, "a").await;
            invoke(&endpoint, &session).await
        });
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let confirmations: Vec<_> = invoked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.confirmation.clone())
            .collect();
        assert_eq!(confirmations, vec![expected], "one admitted start");
    }
}

#[test]
fn beyond_loopback_needs_a_token() {
    let open = Config {
        address: "0.0.0.0:7070".parse().unwrap(),
        token: None,
        room: None,
    };
    assert!(open.validate().is_err());
    let loopback = Config {
        address: "127.0.0.1:7070".parse().unwrap(),
        token: None,
        room: None,
    };
    assert!(loopback.validate().is_ok());
}

#[test]
fn explicit_mcp_room_is_only_a_canonical_room_identifier() {
    let mut config = Config {
        address: "127.0.0.1:7070".parse().unwrap(),
        token: None,
        room: None,
    };
    assert!(config.validate().is_ok());
    for room in [
        "",
        "Writer",
        "00000000-0000-0000-0000-000000000000",
        "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
    ] {
        config.room = Some(room.into());
        assert!(config.validate().is_err());
    }
    config.room = Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into());
    assert!(
        config.validate().is_ok(),
        "syntax validation does not grant admission"
    );
}

#[test]
fn client_metadata_cannot_replace_the_host_selected_mcp_audience() {
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let host = StubHost {
        room: Some(room.into()),
        ..Default::default()
    };
    let endpoint = Endpoint::new(
        host.clone(),
        room.into(),
        "profile-actor".into(),
        None,
        None,
    );
    run(async {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "clientInfo":{"name":"forged-room-name","room":"other"},
            "room":"other", "actor":"forged", "capabilities":{}
        }});
        let response = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &HeaderMap::new(),
                request.to_string().as_bytes(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session = response.headers()[SESSION_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let result = invoke(&endpoint, &session).await;
        assert_ne!(result["result"]["isError"], true, "{result}");
        let callers = host.callers.lock().unwrap().clone();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].room(), Some(room));
        assert_eq!(
            callers[0].principal(),
            &tilo_actions::remote::CallerPrincipal::Person("profile-actor".into())
        );
        assert!(callers[0].execution_id().is_some());
    });
}

#[test]
fn client_labels_are_bounded_and_printable() {
    let long = "x".repeat(500);
    let label = client_label(&json!({"params":{"clientInfo":{"name":format!("a\u{7}b{long}")}}}));
    assert!(label.starts_with("ab"));
    assert_eq!(label.chars().count(), MAX_LABEL_CHARS);
    assert_eq!(client_label(&json!({})), "MCP agent");
}

#[derive(Default)]
struct WatchRecorder(StdMutex<Vec<bool>>);

impl crate::watch::WatchSink for WatchRecorder {
    fn set_watching(&self, watching: bool) -> Result<(), String> {
        self.0.lock().unwrap().push(watching);
        Ok(())
    }
}

fn watched_endpoint(token: Option<&str>) -> (Arc<Endpoint<StubHost>>, Arc<WatchRecorder>) {
    let sink = Arc::new(WatchRecorder::default());
    let lease = WatchLease::new(sink.clone(), WATCH_LEASE);
    let endpoint = Endpoint::build(
        StubHost::default(),
        "room".into(),
        "owner".into(),
        token.map(str::to_owned),
        None,
        None,
        Some(lease),
    );
    (endpoint, sink)
}

#[test]
fn watch_is_a_bearer_authenticated_lease_answered_with_its_length() {
    let (endpoint, sink) = watched_endpoint(Some("secret"));
    let bearer = headers(&[("authorization", "Bearer secret")]);
    run(async {
        let refused = endpoint
            .dispatch(
                &Method::POST,
                "/watch",
                &headers(&[("authorization", "Bearer nope")]),
                br#"{"watching":true}"#,
            )
            .await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        assert!(sink.0.lock().unwrap().is_empty());

        for watching in [true, false] {
            let body_bytes = json!({"watching": watching}).to_string();
            let reply = endpoint
                .dispatch(&Method::POST, "/watch", &bearer, body_bytes.as_bytes())
                .await;
            assert_eq!(reply.status(), StatusCode::OK);
            assert_eq!(
                body(reply).await,
                json!({"watching": watching, "lease_seconds": 60})
            );
        }
        let wrong_method = endpoint
            .dispatch(&Method::GET, "/watch", &bearer, b"")
            .await;
        assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    });
    assert_eq!(*sink.0.lock().unwrap(), [true, false]);
}

#[test]
fn watch_accepts_exactly_one_boolean() {
    let (endpoint, sink) = watched_endpoint(None);
    run(async {
        for bad in [
            &b""[..],
            b"true",
            b"{}",
            br#"{"watching":1}"#,
            br#"{"watching":"true"}"#,
            br#"{"watching":true,"lease_seconds":600}"#,
        ] {
            let reply = endpoint
                .dispatch(&Method::POST, "/watch", &HeaderMap::new(), bad)
                .await;
            assert_eq!(
                reply.status(),
                StatusCode::BAD_REQUEST,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    });
    assert!(sink.0.lock().unwrap().is_empty());
}

#[test]
fn watch_is_not_found_without_a_compositor() {
    let endpoint = endpoint(StubHost::default(), None);
    let reply = run(endpoint.dispatch(
        &Method::POST,
        "/watch",
        &HeaderMap::new(),
        br#"{"watching":true}"#,
    ));
    assert_eq!(reply.status(), StatusCode::NOT_FOUND);
}

#[test]
fn watch_reaches_the_compositor_as_its_control_command() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let compositor = std::thread::spawn(move || {
        let mut lines = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            lines.push(serde_json::from_str::<Value>(&line).unwrap());
            (&stream).write_all(b"{\"ok\":{\"frame\":0}}\n").unwrap();
        }
        lines
    });
    let computer = Arc::new(crate::computer::Computer::new(
        socket,
        dir.path().join("scratch"),
    ));
    let endpoint = Endpoint::new(
        StubHost::default(),
        "room".into(),
        "owner".into(),
        None,
        Some(computer),
    );
    run(async {
        for watching in [true, false] {
            let body_bytes = json!({"watching": watching}).to_string();
            let reply = endpoint
                .dispatch(
                    &Method::POST,
                    "/watch",
                    &HeaderMap::new(),
                    body_bytes.as_bytes(),
                )
                .await;
            assert_eq!(reply.status(), StatusCode::OK);
        }
    });
    assert_eq!(
        compositor.join().unwrap(),
        [
            json!({"op":"watch","watching":true}),
            json!({"op":"watch","watching":false})
        ]
    );
}

#[test]
fn the_workspace_itself_is_addressable_for_installed_apps() {
    let host = StubHost::default();
    let invoked = Arc::clone(&host.invoked);
    let endpoint = endpoint(host, None);
    let (open, both) = run(async {
        let session = open(&endpoint, "a").await;
        let call = |arguments: Value| {
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"tilo_invoke","arguments":arguments}}).to_string()
        };
        let headers = headers(&[(SESSION_HEADER, &session)]);
        let open = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers,
                call(json!({"workspace":true,"revision":"1","resource":"test.note:1","action":tilo_resource::APPLY,"arguments":{}})).as_bytes(),
            )
            .await;
        let both = endpoint
            .dispatch(
                &Method::POST,
                "/mcp",
                &headers,
                call(json!({"peer_id":71,"workspace":true,"revision":"1","resource":"x","action":"y","arguments":{}})).as_bytes(),
            )
            .await;
        (body(open).await, body(both).await)
    });
    assert_eq!(open["result"]["isError"], false, "{open}");
    assert_eq!(both["result"]["isError"], true, "{both}");
    let invoked = invoked.lock().unwrap().clone();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].target.provider, "workspace:room");
}

struct IssuingHost;
impl crate::viewer::HostOperator for IssuingHost {
    fn call(
        &self,
        action: &str,
        _params: Option<Value>,
    ) -> Result<Value, crate::viewer::ViewerError> {
        if action != "invitation.issue" {
            return Err(crate::viewer::ViewerError {
                status: 404,
                message: action.into(),
            });
        }
        Ok(json!({"invite":"signed","invite_id":"ab".repeat(32),"expires_at_ms":1_000_000}))
    }
}

#[test]
fn viewer_invitations_need_a_configured_viewer_and_the_bearer_token() {
    let issue = br#"{"mode":"watch","ttl_seconds":300}"#;
    let bare = endpoint(StubHost::default(), Some("secret"));
    let response = run(bare.dispatch(
        &Method::POST,
        "/viewer/invitations",
        &headers(&[("authorization", "Bearer secret")]),
        issue,
    ));
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "no viewer: unsupported"
    );
    let dir = tempfile::tempdir().unwrap();
    let viewer = crate::viewer::Viewer::new(
        "https://c1.viewer.example".into(),
        Box::new(IssuingHost),
        dir.path().join("ledger.json"),
    )
    .unwrap();
    let served = Endpoint::with_viewer(
        StubHost::default(),
        "room".into(),
        "owner".into(),
        Some("secret".into()),
        None,
        Some(Arc::new(viewer)),
    );
    let response = run(served.dispatch(
        &Method::POST,
        "/viewer/invitations",
        &HeaderMap::new(),
        issue,
    ));
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let authorized = headers(&[("authorization", "Bearer secret")]);
    let response = run(served.dispatch(&Method::POST, "/viewer/invitations", &authorized, issue));
    assert_eq!(response.status(), StatusCode::OK);
    let reply = run(body(response));
    assert_eq!(reply["origin"], "https://c1.viewer.example");
    assert_eq!(reply["invitation"], "signed");
    assert_eq!(reply["role"], "viewer");
    let unknown = format!("/viewer/invitations/{}", "cd".repeat(32));
    let response = run(served.dispatch(&Method::DELETE, &unknown, &authorized, b""));
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = run(served.dispatch(&Method::GET, "/viewer/invitations", &authorized, b""));
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = run(served.dispatch(
        &Method::DELETE,
        "/viewer/invitations/not-hex",
        &authorized,
        b"",
    ));
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = run(served.dispatch(
        &Method::POST,
        "/viewer/invitations",
        &authorized,
        br#"{"mode":"watch"}"#,
    ));
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn tool(server: &crate::mcp::McpServer<StubHost>, name: &str, arguments: Value) -> Value {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    reply["result"].clone()
}

#[test]
fn an_invocation_answers_with_its_result_once_it_finishes() {
    let host = StubHost::default();
    let running_for = Arc::clone(&host.running_for);
    let invoked = Arc::clone(&host.invoked);
    let server = crate::mcp::McpServer::new(host, "room".into(), "owner".into());
    run(async {
        let target = json!({"peer_id":71,"revision":"1","resource":"test.note:1","action":tilo_resource::APPLY});
        running_for.store(3, std::sync::atomic::Ordering::SeqCst);
        let done = tool(&server, "tilo_invoke", target.clone()).await;
        assert_eq!(
            done["structuredContent"]["update"]["state"], "succeeded",
            "{done}"
        );

        running_for.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        let mut at_once = target.clone();
        at_once["wait_ms"] = json!(0);
        let reply = tool(&server, "tilo_invoke", at_once).await;
        assert_eq!(
            reply["structuredContent"]["update"]["state"], "accepted",
            "{reply}"
        );
        let mut briefly = target;
        briefly["wait_ms"] = json!(60);
        let started = std::time::Instant::now();
        let reply = tool(&server, "tilo_invoke", briefly).await;
        assert_eq!(reply["structuredContent"]["update"]["state"], "accepted");
        assert!(started.elapsed() >= std::time::Duration::from_millis(60));
        assert!(
            reply["structuredContent"]["pending"]
                .as_str()
                .is_some_and(|hint| hint.starts_with("not finished within 60 ms; tilo_status")),
            "{reply}"
        );
    });
    assert_eq!(invoked.lock().unwrap().len(), 3);
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nthe-look";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0the-next-look";

/// The content service as the stub app `so.tilo.test` reads it: a picture
/// it stored and something that is not an image.
struct StoredByTestApp;

impl crate::MediaReader for StoredByTestApp {
    fn read(&self, app_id: &str, hash: &str) -> std::io::Result<Option<Vec<u8>>> {
        Ok(match (app_id, hash) {
            ("so.tilo.test", "look") => Some(PNG.to_vec()),
            ("so.tilo.test", "next-look") => Some(JPEG.to_vec()),
            ("so.tilo.test", "notes") => Some(b"plain text".to_vec()),
            _ => None,
        })
    }
}

/// An action that produced a picture is answered with the picture, as an
/// MCP image block beside the JSON, both when it finishes within the call
/// and when `tilo_status` reports it later; what is not an image, or not
/// readable with the producing app's own scope, is only named.
/// A picture the session was already shown is named, not sent again.
#[test]
fn an_action_that_produced_an_image_answers_with_it() {
    use base64::Engine as _;
    let host = StubHost::default();
    *host.output.lock().unwrap() = tilo_actions::ActionOutput {
        value: Some(json!({"revision":"r1","width":4,"height":3})),
        resources: vec![
            "tilo.content:look".into(),
            "tilo.content:notes".into(),
            "tilo.content:elsewhere".into(),
        ],
    };
    let running_for = Arc::clone(&host.running_for);
    let output = Arc::clone(&host.output);
    let server = crate::mcp::McpServer::new(host.clone(), "room".into(), "owner".into())
        .with_media(Some(Arc::new(StoredByTestApp)));
    let target = json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY});
    let images = |reply: &Value| -> Vec<Value> {
        reply["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["type"] == "image")
            .cloned()
            .collect()
    };
    run(async {
        let reply = tool(&server, "tilo_invoke", target.clone()).await;
        assert_eq!(
            reply["structuredContent"]["update"]["state"], "succeeded",
            "{reply}"
        );
        let shown = images(&reply);
        assert_eq!(shown.len(), 1, "{reply}");
        assert_eq!(shown[0]["mimeType"], "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(shown[0]["data"].as_str().unwrap())
                .unwrap(),
            PNG
        );
        let text = reply["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("tilo.content:look")
                && !text.contains(shown[0]["data"].as_str().unwrap())
        );
        assert_eq!(
            reply["structuredContent"]["update"]["output"]["value"]["revision"],
            "r1"
        );

        running_for.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        let mut later = target.clone();
        later["wait_ms"] = json!(0);
        let accepted = tool(&server, "tilo_invoke", later).await;
        assert!(
            images(&accepted).is_empty(),
            "nothing is shown before it finishes"
        );
        running_for.store(0, std::sync::atomic::Ordering::SeqCst);
        let id = accepted["structuredContent"]["invocation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let status = tool(&server, "tilo_status", json!({"invocation_id": id})).await;
        assert_eq!(images(&status).len(), 1, "{status}");

        output.lock().unwrap().resources = vec!["tilo.content:next-look".into()];
        let batch = tool(
            &server,
            "tilo_invoke",
            json!({"peer_id":71,"calls":[
                {"resource":"test.note:1","action":tilo_resource::APPLY},
                {"resource":"test.note:1","action":tilo_resource::APPLY}
            ]}),
        )
        .await;
        let shown = images(&batch);
        assert_eq!(shown.len(), 1, "a new picture is shown once: {batch}");
        assert_eq!(shown[0]["mimeType"], "image/jpeg");
        assert!(
            batch["structuredContent"]["results"][1]["unchanged"]
                .as_str()
                .is_some_and(|note| note.contains("tilo_status")),
            "the second call made the same picture, and says so: {batch}"
        );
    });
    let blind = crate::mcp::McpServer::new(host, "room".into(), "owner".into());
    run(async {
        let reply = tool(&blind, "tilo_invoke", target).await;
        assert!(
            images(&reply).is_empty(),
            "without a content service nothing is read"
        );
    });
}

/// An effect's picture (an export) is an artifact, not something to look
/// at: the answer names its address, and `tilo_status` shows it on request.
#[test]
fn an_effect_answers_with_its_picture_address_not_the_picture() {
    let host = StubHost::default();
    *host.kind.lock().unwrap() = tilo_actions::OperationKind::Effect;
    *host.output.lock().unwrap() = tilo_actions::ActionOutput {
        value: Some(json!({"width":4,"height":3})),
        resources: vec!["tilo.content:look".into()],
    };
    let server = crate::mcp::McpServer::new(host, "room".into(), "owner".into())
        .with_media(Some(Arc::new(StoredByTestApp)));
    let images = |reply: &Value| {
        reply["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["type"] == "image")
            .count()
    };
    run(async {
        let reply = tool(
            &server,
            "tilo_invoke",
            json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY}),
        )
        .await;
        assert_eq!(images(&reply), 0, "{reply}");
        assert!(
            reply["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("tilo.content:look")
        );
        let id = reply["structuredContent"]["invocation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let status = tool(&server, "tilo_status", json!({"invocation_id": id})).await;
        assert_eq!(images(&status), 1, "{status}");
    });
}

/// A look at an unchanged picture costs a line, not an image: the answer
/// names the invocation that showed it, whose `tilo_status` shows it again.
/// Each session keeps its own record.
#[test]
fn a_picture_already_shown_is_named_instead_of_sent_again() {
    let host = StubHost::default();
    *host.output.lock().unwrap() = tilo_actions::ActionOutput {
        value: Some(json!({"revision":"r1"})),
        resources: vec!["tilo.content:look".into()],
    };
    let target = json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY});
    let images = |reply: &Value| {
        reply["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["type"] == "image")
            .count()
    };
    let session = || {
        crate::mcp::McpServer::new(host.clone(), "room".into(), "owner".into())
            .with_media(Some(Arc::new(StoredByTestApp)))
    };
    let server = session();
    run(async {
        let first = tool(&server, "tilo_invoke", target.clone()).await;
        assert_eq!(images(&first), 1, "{first}");
        assert!(first["structuredContent"].get("unchanged").is_none());
        let first_id = first["structuredContent"]["invocation_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let again = tool(&server, "tilo_invoke", target.clone()).await;
        assert_eq!(images(&again), 0, "{again}");
        assert!(
            again["structuredContent"]["unchanged"]
                .as_str()
                .is_some_and(|note| note.contains(&first_id)),
            "{again}"
        );
        assert_eq!(again["structuredContent"]["update"]["state"], "succeeded");

        let status = tool(&server, "tilo_status", json!({"invocation_id": first_id})).await;
        assert_eq!(images(&status), 1, "tilo_status shows it again: {status}");
    });
    let other = session();
    run(async {
        let reply = tool(&other, "tilo_invoke", target).await;
        assert_eq!(
            images(&reply),
            1,
            "another session was never shown it: {reply}"
        );
    });
}

#[test]
fn an_omitted_revision_acts_on_the_current_directory_but_an_empty_one_is_refused() {
    let host = StubHost::default();
    let invoked = Arc::clone(&host.invoked);
    let server = crate::mcp::McpServer::new(host, "room".into(), "owner".into());
    run(async {
        let current = json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY});
        let reply = tool(&server, "tilo_invoke", current.clone()).await;
        assert_eq!(reply["isError"], false, "{reply}");
        let resolved = tool(&server, "tilo_resolve", current.clone()).await;
        assert_eq!(
            resolved["structuredContent"]["row"]["resource"], "test.note:1",
            "{resolved}"
        );
        let mut empty = current;
        empty["revision"] = json!("");
        assert_eq!(
            tool(&server, "tilo_invoke", empty.clone()).await["isError"],
            true
        );
        assert_eq!(tool(&server, "tilo_resolve", empty).await["isError"], true);
    });
    let invoked = invoked.lock().unwrap().clone();
    assert_eq!(invoked.len(), 1);
    assert_eq!(
        invoked[0].revision,
        Some("1".into()),
        "MCP pins the current directory without inventing an owner-authored binding"
    );
    assert!(invoked[0].fresh_binding.is_none());
}

#[test]
fn a_query_answers_compact_by_default_and_matches_every_word_of_a_needle() {
    let server = crate::mcp::McpServer::new(StubHost::default(), "room".into(), "owner".into());
    run(async {
        let compact = tool(&server, "tilo_query", json!({"peer_id":71})).await;
        let page = &compact["structuredContent"];
        assert_eq!(page["resources"][0]["resource"], "test.note:1", "{page}");
        assert!(
            page["resources"][0]["actions"][tilo_resource::APPLY].is_string(),
            "{page}"
        );
        assert!(page.get("rows").is_none());
        let full = tool(&server, "tilo_query", json!({"peer_id":71,"detail":"full"})).await;
        assert!(full["structuredContent"]["rows"][0]["descriptor"]["params"].is_array());

        let found = tool(
            &server,
            "tilo_query",
            json!({"peer_id":71,"needle":"note test"}),
        )
        .await;
        assert_eq!(
            found["structuredContent"]["resources"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{found}"
        );
        assert!(
            found["structuredContent"].get("partial_match").is_none(),
            "{found}"
        );
        // No row has every word: the rows with the most of them, flagged.
        let partial = tool(
            &server,
            "tilo_query",
            json!({"peer_id":71,"needle":"note zebra"}),
        )
        .await;
        assert_eq!(
            partial["structuredContent"]["resources"][0]["resource"], "test.note:1",
            "{partial}"
        );
        assert_eq!(
            partial["structuredContent"]["partial_match"], true,
            "{partial}"
        );
        let full_miss = tool(
            &server,
            "tilo_query",
            json!({"peer_id":71,"needle":"note zebra","detail":"full"}),
        )
        .await;
        assert_eq!(
            full_miss["structuredContent"]["rows"],
            json!([]),
            "version 1 rows match every word: {full_miss}"
        );
        let missed = tool(
            &server,
            "tilo_query",
            json!({"peer_id":71,"needle":"quokka zebra"}),
        )
        .await;
        assert_eq!(
            missed["structuredContent"]["resources"],
            json!([]),
            "{missed}"
        );
    });
}

#[test]
fn an_app_id_addresses_its_newest_running_instance() {
    let host = StubHost::default();
    let invoked = Arc::clone(&host.invoked);
    let server = crate::mcp::McpServer::new(host, "room".into(), "owner".into());
    run(async {
        let page = tool(
            &server,
            "tilo_query",
            json!({"app_id":"so.tilo.test","needle":"note"}),
        )
        .await;
        assert_eq!(page["isError"], false, "{page}");
        let target =
            json!({"app_id":"so.tilo.test","resource":"test.note:1","action":tilo_resource::APPLY});
        assert_eq!(tool(&server, "tilo_invoke", target).await["isError"], false);
        let missing = tool(&server, "tilo_query", json!({"app_id":"so.tilo.absent"})).await;
        assert_eq!(missing["isError"], true, "{missing}");
        let both = tool(
            &server,
            "tilo_query",
            json!({"app_id":"so.tilo.test","peer_id":71}),
        )
        .await;
        assert_eq!(both["isError"], true, "{both}");
    });
    assert_eq!(invoked.lock().unwrap()[0].target.provider, "71");
}

#[test]
fn a_session_can_pin_the_full_rows_it_parses() {
    let server = crate::mcp::McpServer::new(StubHost::default(), "room".into(), "owner".into());
    run(async {
        let init = |capabilities: Value| json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":capabilities}});
        let reply = server.handle(init(json!({}))).await.unwrap();
        assert_eq!(
            reply["result"]["capabilities"]["experimental"]["so.tilo.mcp"]["version"],
            3
        );
        let reply = server
            .handle(init(
                json!({"experimental":{"so.tilo.mcp":{"detail":"sideways"}}}),
            ))
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        server
            .handle(init(
                json!({"experimental":{"so.tilo.mcp":{"detail":"full"}}}),
            ))
            .await
            .unwrap();
        let page = tool(&server, "tilo_query", json!({"peer_id":71})).await;
        assert!(
            page["structuredContent"]["rows"][0]["descriptor"].is_object(),
            "{page}"
        );
        let compact = tool(
            &server,
            "tilo_query",
            json!({"peer_id":71,"detail":"compact"}),
        )
        .await;
        assert!(
            compact["structuredContent"]["resources"].is_array(),
            "{compact}"
        );
    });
}

#[test]
fn opening_a_host_application_answers_without_waiting_for_a_peer() {
    let server = crate::mcp::McpServer::new(StubHost::default(), "room".into(), "owner".into());
    run(async {
        let started = std::time::Instant::now();
        let reply = tool(
            &server,
            "tilo_invoke",
            json!({"workspace":true,"resource":"app.host:org.mozilla.firefox","action":"open"}),
        )
        .await;
        assert_eq!(reply["isError"], false, "{reply}");
        assert!(
            reply["structuredContent"].get("peer_id").is_none(),
            "{reply}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    });
}

#[test]
fn continued_history_and_client_names_cannot_select_execution_identity() {
    let host = StubHost::default();
    let callers = Arc::clone(&host.callers);
    let invoked = Arc::clone(&host.invoked);
    run(async {
        let history = "mcp:11111111-1111-4111-8111-111111111111";
        let mut executions: Vec<String> = Vec::new();
        for _ in 0..2 {
            let server = McpServer::new(host.clone(), "room".into(), "owner".into());
            let forged_execution = executions
                .first()
                .map_or("11111111-1111-4111-8111-111111111111", String::as_str);
            let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","clientInfo":{"name":"Same name","version":"1"},
                "capabilities":{"experimental":{"so.tilo.mcp":{"run":history,"execution_id":forged_execution}}}
            }});
            let first = server.handle(init.clone()).await.unwrap();
            let again = server.handle(init).await.unwrap();
            let execution = first
                .pointer("/result/capabilities/experimental/so.tilo.mcp/execution_id")
                .and_then(Value::as_str)
                .expect("host-issued execution id");
            assert_eq!(
                again
                    .pointer("/result/capabilities/experimental/so.tilo.mcp/execution_id")
                    .and_then(Value::as_str),
                Some(execution)
            );
            assert_ne!(execution, history);
            assert_ne!(execution, forged_execution);
            executions.push(execution.to_owned());
            tool(
                &server,
                "tilo_invoke",
                json!({"peer_id":71,"resource":"test.note:1","action":tilo_resource::APPLY}),
            )
            .await;
        }
        assert_ne!(executions[0], executions[1]);
        let callers = callers.lock().unwrap().clone();
        assert_eq!(callers[0].principal(), callers[1].principal());
        assert_ne!(callers[0].execution_id(), callers[1].execution_id());
        for (caller, execution) in callers.iter().zip(executions) {
            assert_eq!(caller.execution_id().unwrap().to_string(), execution);
        }
        for request in invoked.lock().unwrap().iter() {
            assert_eq!(
                request.context.as_ref().unwrap().group.as_ref().unwrap().id,
                history
            );
        }
    });
}

#[test]
fn session_delete_idle_expiry_and_endpoint_drop_retire_run_lifetimes() {
    let endpoint = endpoint(StubHost::default(), None);
    run(async {
        let first = open(&endpoint, "same label").await;
        let second = open(&endpoint, "same label").await;
        let retained = endpoint.find(&first).unwrap();
        let first_caller = retained.execution_caller();
        let second_caller = endpoint.find(&second).unwrap().execution_caller();
        assert!(first_caller.execution_is_live());
        assert!(second_caller.execution_is_live());
        let deleted = endpoint
            .dispatch(
                &Method::DELETE,
                "/mcp",
                &headers(&[(SESSION_HEADER, &first)]),
                b"",
            )
            .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert!(
            !first_caller.execution_is_live(),
            "retained server cannot revive a deleted run"
        );
        assert!(second_caller.execution_is_live());
        endpoint
            .sessions
            .lock()
            .unwrap()
            .get_mut(&second)
            .unwrap()
            .last_used = Instant::now().checked_sub(SESSION_IDLE).unwrap();
        let third = open(&endpoint, "same label").await;
        assert!(!second_caller.execution_is_live());
        let third_caller = endpoint.find(&third).unwrap().execution_caller();
        assert!(third_caller.execution_is_live());
        drop(endpoint);
        assert!(!third_caller.execution_is_live());
        assert_ne!(first_caller.execution_id(), third_caller.execution_id());
    });
}

#[test]
fn stdio_server_drop_retires_retained_caller_clones() {
    let server = McpServer::new(StubHost::default(), "room".into(), "owner".into());
    let caller = server.execution_caller();
    let identity = caller.clone();
    assert!(caller.execution_is_live());
    drop(server);
    assert!(!caller.execution_is_live());
    assert_eq!(
        caller, identity,
        "liveness does not mutate invocation ownership keys"
    );
}

#[test]
fn logical_session_reconnect_stays_live_but_expired_id_cannot_renew_itself() {
    let endpoint = endpoint(StubHost::default(), None);
    run(async {
        let id = open(&endpoint, "same label").await;
        let connection = endpoint.find(&id).unwrap();
        let caller = connection.execution_caller();
        drop(connection);
        let reconnected = endpoint.find(&id).unwrap();
        let resumed = reconnected.execution_caller();
        assert_eq!(caller.execution_id(), resumed.execution_id());
        assert!(caller.execution_is_live());
        endpoint
            .sessions
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .last_used = Instant::now().checked_sub(SESSION_IDLE).unwrap();
        assert!(endpoint.find(&id).is_none());
        assert!(!caller.execution_is_live());
        assert!(!resumed.execution_is_live());
        let replacement = open(&endpoint, "same label").await;
        let fresh = endpoint.find(&replacement).unwrap().execution_caller();
        assert_ne!(fresh.execution_id(), caller.execution_id());
        assert!(fresh.execution_is_live());
    });
}

#[path = "mcp_retired_execution_tests.rs"]
mod retired_execution_tests;
