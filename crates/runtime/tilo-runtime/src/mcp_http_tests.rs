use super::*;
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

#[derive(Clone, Default)]
struct StubHost {
    invoked: Arc<StdMutex<Vec<InvokeRequest>>>,
    confirmation: tilo_actions::ConfirmationPolicy,
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
        let mut row = self.row();
        if let Some(provider) = query.provider.clone() {
            row.provider = provider;
        }
        let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
        page.providers = vec![CatalogueProvider {
            provider: "71".into(),
            app_id: "so.tilo.test".into(),
            room: "room".into(),
            record_id: String::new(),
            browsable: true,
        }];
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        if let tilo_actions::ConfirmationPolicy::Phrase { phrase } = &self.confirmation
            && request.confirmation
                != Some(tilo_actions::remote::Confirmation::Phrase(phrase.clone()))
        {
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        }
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
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Accepted { invocation: *id },
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
    let endpoint = endpoint(host, None);
    run(async {
        let first = open(&endpoint, "Researcher").await;
        let second = open(&endpoint, "Writer").await;
        assert_ne!(first, second);
        invoke(&endpoint, &first).await;
        invoke(&endpoint, &second).await;
    });
    let groups: Vec<_> = invoked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.context.clone().unwrap().group.unwrap())
        .collect();
    assert_eq!(groups.len(), 2);
    assert_ne!(groups[0].id, groups[1].id);
    assert!(groups.iter().all(|group| group.kind == "agent-run"));
    assert_eq!(groups[0].label, "Researcher");
    assert_eq!(groups[1].label, "Writer");
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
    };
    assert!(open.validate().is_err());
    let loopback = Config {
        address: "127.0.0.1:7070".parse().unwrap(),
        token: None,
    };
    assert!(loopback.validate().is_ok());
}

#[test]
fn client_labels_are_bounded_and_printable() {
    let long = "x".repeat(500);
    let label = client_label(&json!({"params":{"clientInfo":{"name":format!("a\u{7}b{long}")}}}));
    assert!(label.starts_with("ab"));
    assert_eq!(label.chars().count(), MAX_LABEL_CHARS);
    assert_eq!(client_label(&json!({})), "MCP agent");
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
        assert_eq!(action, "invitation.issue");
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
