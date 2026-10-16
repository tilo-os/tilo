//! HTTP credentials are mandatory even when construction bypasses Config.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn every_listen_address_requires_a_nonempty_token() {
    for address in ["0.0.0.0:7070", "127.0.0.1:7070", "[::]:7070", "[::1]:7070"] {
        for token in [None, Some(""), Some(" \t\n"), Some("secret")] {
            let config = Config {
                address: address.parse().unwrap(),
                token: token.map(str::to_owned),
                room: None,
            };
            assert_eq!(
                config.validate().is_ok(),
                token == Some("secret"),
                "{address}"
            );
        }
    }
}

#[test]
fn token_files_must_exist_and_contain_a_credential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token");
    assert!(read_token(&path).is_err());
    for contents in ["", " \t\n"] {
        std::fs::write(&path, contents).unwrap();
        assert!(read_token(&path).is_err());
    }
    std::fs::write(&path, "secret\n").unwrap();
    assert_eq!(read_token(&path).unwrap(), "secret");
}

struct CountViewer(Arc<AtomicUsize>);
impl crate::viewer::HostOperator for CountViewer {
    fn call(
        &self,
        _action: &str,
        _params: Option<Value>,
    ) -> Result<Value, crate::viewer::ViewerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(crate::viewer::ViewerError {
            status: 500,
            message: "unexpected call".into(),
        })
    }
}

#[test]
fn missing_endpoint_credentials_deny_every_protected_route_before_side_effects() {
    for token in [None, Some(""), Some(" \t")] {
        let dir = tempfile::tempdir().unwrap();
        let viewer_calls = Arc::new(AtomicUsize::new(0));
        let viewer = crate::viewer::Viewer::new(
            "https://viewer.example".into(),
            Box::new(CountViewer(viewer_calls.clone())),
            dir.path().join("ledger.json"),
        )
        .unwrap();
        let sink = Arc::new(WatchRecorder::default());
        let endpoint = Endpoint::build(
            StubHost::default(),
            "room".into(),
            "owner".into(),
            token.map(str::to_owned),
            None,
            Some(Arc::new(viewer)),
            Some(WatchLease::new(sink.clone(), WATCH_LEASE)),
        );
        let admissions = Arc::new(AtomicUsize::new(0));
        let count = admissions.clone();
        endpoint
            .serve_sessions(Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::other("unexpected admission"))
            }))
            .unwrap();
        run(async {
            let revoke = format!("/viewer/invitations/{}", "ab".repeat(32));
            for (method, path) in [
                (Method::POST, "/mcp"),
                (Method::DELETE, "/mcp"),
                (Method::GET, "/mcp"),
                (Method::POST, "/watch"),
                (Method::POST, "/viewer/invitations"),
                (Method::DELETE, &revoke),
            ] {
                for headers in [HeaderMap::new(), bearer_headers(&[])] {
                    let response = endpoint
                        .dispatch(&method, path, &headers, &initialize("a"))
                        .await;
                    assert_eq!(
                        response.status(),
                        StatusCode::UNAUTHORIZED,
                        "{method} {path}"
                    );
                }
            }
            let health = endpoint
                .dispatch(&Method::GET, "/health", &HeaderMap::new(), b"")
                .await;
            assert_eq!(health.status(), StatusCode::OK);
        });
        assert_eq!(endpoint.session_count(), 0);
        assert_eq!(admissions.load(Ordering::SeqCst), 0);
        assert_eq!(viewer_calls.load(Ordering::SeqCst), 0);
        assert!(sink.0.lock().unwrap().is_empty());
    }
}

#[test]
fn a_session_id_never_replaces_the_token_for_calls_or_deletion() {
    let host = StubHost::default();
    let invoked = host.invoked.clone();
    let endpoint = endpoint(host);
    run(async {
        let id = open(&endpoint, "authenticated agent").await;
        let execution = endpoint.find(&id).unwrap().execution_caller();
        let call = json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{
            "name":"tilo_invoke", "arguments":{"peer_id":71, "revision":"1",
            "resource":"test.note:1", "action":tilo_resource::APPLY, "arguments":{}}
        }})
        .to_string();
        for authorization in [
            None,
            Some("Bearer wrong"),
            Some("Basic secret"),
            Some("Bearer "),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(SESSION_HEADER, HeaderValue::from_str(&id).unwrap());
            if let Some(value) = authorization {
                headers.insert("authorization", HeaderValue::from_str(value).unwrap());
            }
            for method in [Method::POST, Method::DELETE] {
                let response = endpoint
                    .dispatch(&method, "/mcp", &headers, call.as_bytes())
                    .await;
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(endpoint.session_count(), 1);
                assert!(execution.execution_is_live());
                assert!(invoked.lock().unwrap().is_empty());
            }
        }
        let result = invoke(&endpoint, &id).await;
        assert_eq!(result["result"]["isError"], false, "{result}");
        assert_eq!(invoked.lock().unwrap().len(), 1);
        let deleted = endpoint
            .dispatch(
                &Method::DELETE,
                "/mcp",
                &bearer_headers(&[(SESSION_HEADER, &id)]),
                b"",
            )
            .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert_eq!(endpoint.session_count(), 0);
        assert!(!execution.execution_is_live());
    });
}
