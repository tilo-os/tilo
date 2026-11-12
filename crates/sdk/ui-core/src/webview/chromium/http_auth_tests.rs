use super::*;
use crate::webview::{HttpAuthChallenge, HttpAuthCredentials};

#[test]
fn a_servers_sign_in_reaches_only_a_tab_that_answers_them() {
    const PASSWORD: &str = "correct-horse-9";
    let tx = Arc::new(CommandQueue::default());
    let shared = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let runtime = Arc::new(Runtime {
        tx: tx.clone(),
        process: shared.clone(),
        supervisor: Mutex::default(),
        routes,
        next_tab: AtomicU32::new(2),
        download_events: Arc::default(),
        server: Arc::default(),
        link: None,
        service_session: None,
        exited: Arc::new(AtomicBool::new(false)),
    });
    let engine = ChromiumEngine {
        tx: tx.clone(),
        shared,
        runtime: runtime.clone(),
        tab: 1,
    };
    engine.accept_http_auth();
    assert!(engine.drain_events().is_empty());
    let queued = || tx.state.lock().unwrap().0.len();
    assert_eq!(queued(), 0, "an older host never sees the command");

    let (ready, _receiver) = sync_channel(1);
    let route = |event: Value| {
        route_native_event(&event, &runtime.routes, &runtime.download_events, &ready)
    };
    route(json!({"event":"ready","cef_version":"152","chromium_version":"152","http_auth":true}))
        .unwrap();
    engine.drain_events();
    assert_eq!(
        tx.pop().expect("told once the host holds sign-ins"),
        json!({"op":"http_auth","tab":1,"enabled":true})
    );
    engine.drain_events();
    assert_eq!(queued(), 0, "told once");

    route(
        json!({"event":"http_auth","tab":1,"id":7,"asker":"https://example.org",
        "proxy":false,"realm":"Staff room","scheme":"basic","retry":false}),
    )
    .unwrap();
    route(
        json!({"event":"http_auth","tab":1,"id":8,"asker":"proxy.example:3128",
        "proxy":true,"realm":"","scheme":"digest","retry":true}),
    )
    .unwrap();
    route(json!({"event":"http_auth_closed","tab":1,"id":7})).unwrap();
    assert_eq!(
        engine.drain_events(),
        [
            WebViewEvent::HttpAuth(HttpAuthChallenge {
                id: 7,
                asker: "https://example.org".into(),
                proxy: false,
                realm: "Staff room".into(),
                scheme: "basic".into(),
                retry: false,
            }),
            WebViewEvent::HttpAuth(HttpAuthChallenge {
                id: 8,
                asker: "proxy.example:3128".into(),
                proxy: true,
                realm: String::new(),
                scheme: "digest".into(),
                retry: true,
            }),
            WebViewEvent::HttpAuthClosed { id: 7 },
        ]
    );
    // A challenge that does not say who asks, or says more than an origin,
    // is no challenge: nobody is shown it.
    let asks = |change: &dyn Fn(&mut Value)| {
        let mut event = json!({"event":"http_auth","tab":1,"id":3,"asker":"https://example.org",
            "proxy":false,"realm":"Staff room","scheme":"basic","retry":false});
        change(&mut event);
        event
    };
    for malformed in [
        asks(&|event| event["id"] = json!(0)),
        asks(&|event| event["asker"] = json!("")),
        asks(&|event| event["asker"] = json!("example.org")),
        asks(&|event| event["asker"] = json!("https://example.org/login")),
        asks(&|event| event["asker"] = json!("https://ada@example.org")),
        asks(&|event| event["asker"] = json!("proxy.example:3128")),
        asks(&|event| event["proxy"] = json!(true)),
        asks(&|event| event["proxy"] = json!("no")),
        asks(&|event| event["realm"] = json!("Staff\nroom")),
        asks(&|event| event["realm"] = json!("x".repeat(257))),
        asks(&|event| event["scheme"] = json!("Basic")),
        asks(&|event| event["retry"] = Value::Null),
        asks(&|event| {
            event.as_object_mut().unwrap().remove("realm");
        }),
        json!({"event":"http_auth_closed","tab":1,"id":-1}),
    ] {
        assert!(route(malformed.clone()).is_err(), "{malformed}");
    }

    // The answer names the challenge and the asker it was shown; none of
    // the credentials cancels.
    let answer = |id, asker: &str, credentials| WebViewCommand::AnswerHttpAuth {
        id,
        asker: asker.into(),
        credentials,
    };
    engine.send(answer(
        7,
        "https://example.org",
        Some(HttpAuthCredentials::new("ada", PASSWORD)),
    ));
    assert_eq!(
        tx.pop().unwrap(),
        json!({"op":"http_auth_reply","tab":1,"id":7,"asker":"https://example.org",
            "username":"ada","password":PASSWORD})
    );
    engine.send(answer(8, "proxy.example:3128", None));
    assert_eq!(
        tx.pop().unwrap(),
        json!({"op":"http_auth_reply","tab":1,"id":8,"asker":"proxy.example:3128"})
    );
    // What the host would end the engine over is never sent.
    let long = "x".repeat(crate::webview::http_auth::MAX_HTTP_AUTH_CREDENTIAL + 1);
    for refused in [
        answer(0, "https://example.org", None),
        answer(9, "", None),
        answer(
            9,
            "https://example.org",
            Some(HttpAuthCredentials::new(long.clone(), "p")),
        ),
        answer(
            9,
            "https://example.org",
            Some(HttpAuthCredentials::new("ada", long)),
        ),
    ] {
        engine.send(refused);
        assert_eq!(queued(), 0);
    }
    // The command prints without its credentials, wherever it is printed.
    let printed = format!(
        "{:?}",
        answer(
            7,
            "https://example.org",
            Some(HttpAuthCredentials::new("ada", PASSWORD))
        )
    );
    assert!(
        printed.contains("AnswerHttpAuth") && !printed.contains(PASSWORD),
        "{printed}"
    );
}
