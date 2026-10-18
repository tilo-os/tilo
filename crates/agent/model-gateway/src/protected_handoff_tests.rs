//! Launcher-authenticated role restrictions on the standalone socket.
use super::*;
use serde_json::json;

fn legacy_requests() -> Vec<Request> {
    let completion = json!({
        "messages": [{"role":"user", "content":[{"type":"text", "text":"question"}]}],
        "tools":[], "tool_choice":"none", "max_output_tokens":16
    });
    [
        json!({"verb":"plan", "profile_id":"", "model_id":"", "body":{}, "timeout_ms":100}),
        json!({"verb":"transform", "profile_id":"", "model_id":"", "instructions":"", "input":"", "max_output_tokens":16, "timeout_ms":100}),
        json!({"verb":"complete", "profile_id":"", "model_id":"", "request":completion, "timeout_ms":100}),
        json!({"verb":"complete_default", "request":completion, "timeout_ms":100}),
    ].into_iter().map(|value| serde_json::from_value(value).unwrap()).collect()
}

#[tokio::test]
async fn managed_launcher_handoff_blocks_plain_and_cancellable_legacy_inference() {
    // Exact metadata emitted by launcher::services::EnrolledServiceContext.
    // Its presence restricts the FD but supplies no native lease custody.
    for binding in [
        json!({
            "room":"room", "agent":"helper", "launch":"launch", "generation":1,
            "runtime_epoch":"epoch", "channel_id":"channel", "scope":{"task":"task", "run":"run"}
        }),
        serde_json::Value::Null,
        json!({}),
    ] {
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        HANDOFF_FRAMING
            .write_async(
                &mut client,
                &serde_json::to_vec(&json!({
                    "protocol":1, "app_id":AGENT_APP, "managed_actor_binding":binding
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let http = provider_client().unwrap();
        let service = serve_connection(server, &http);
        let client = async move {
            let mut requests = vec![Request::CancellationSupported];
            requests.extend(legacy_requests());
            requests.extend(
                legacy_requests()
                    .into_iter()
                    .enumerate()
                    .map(|(id, request)| Request::Cancellable {
                        id: id as u64 + 1,
                        request: Box::new(request),
                    }),
            );
            // Native verbs cannot turn correlation metadata into authority.
            requests.push(
                serde_json::from_value(json!({
                    "verb":"native_plan", "invocation":{}, "profile_id":"", "model_id":"",
                    "body":{}, "timeout_ms":100
                }))
                .unwrap(),
            );
            let count = requests.len();
            for request in requests {
                FRAMING
                    .write_async(
                        &mut client,
                        &serde_json::to_vec(&Frame {
                            version: RPC_VERSION,
                            value: request,
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                let body = FRAMING.read_async(&mut client).await.unwrap().unwrap();
                let response: Response = serde_json::from_slice(&body).unwrap();
                if response.output.as_deref() == Some("cancel-v1") {
                    assert!(response.ok);
                } else {
                    assert_eq!(
                        response.model_error.unwrap().kind,
                        ErrorKind::PermissionDenied
                    );
                }
            }
            assert_eq!(count, 10);
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(service, client);
        })
        .await
        .unwrap();
    }
}

#[test]
fn ordinary_agent_role_and_managed_selection_metadata_are_preserved() {
    const MARKER: &str = "GATEWAY_PROTECTED_HANDOFF_CHILD";
    if std::env::var_os(MARKER).is_some() {
        tilo_rt::block_on(ordinary_and_selection()).unwrap();
        return;
    }
    // CompleteDefault must never consult a developer's real provider account.
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "service::protected_handoff_tests::ordinary_agent_role_and_managed_selection_metadata_are_preserved", "--nocapture"])
        .env_clear().env("PATH", root.path()).env("XDG_CONFIG_HOME", root.path())
        .env(MARKER, "1").output().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn ordinary_and_selection() {
    let ordinary: Handoff =
        serde_json::from_value(json!({"protocol":1,"app_id":AGENT_APP})).unwrap();
    assert!(matches!(
        ordinary.gateway_source(),
        native::GatewaySource::Ordinary
    ));
    let managed: Handoff = serde_json::from_value(json!({
        "protocol":1,"app_id":AGENT_APP,"managed_actor_binding":{}
    }))
    .unwrap();
    assert!(matches!(
        managed.gateway_source(),
        native::GatewaySource::NativeUnavailable
    ));
    let http = provider_client().unwrap();
    for request in legacy_requests() {
        let response = ordinary.execute(request, &http, &mut |_| true).await;
        assert_ne!(
            response.error.as_deref(),
            Some("Provider permission denied")
        );
    }
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.id = "metadata-only".into();
    profile.models = vec![agent_provider_config::ModelDescriptor::new("selected")];
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
    }
    .save()
    .unwrap();
    // No key, keyring, sign-in or native custody is installed in this child.
    let response = managed
        .execute(
            Request::SelectionConfigured {
                profile_id: "metadata-only".into(),
                model_id: "selected".into(),
                protocol: ProviderProtocol::OpenAiCompatible,
            },
            &http,
            &mut |_| true,
        )
        .await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.configured, Some(true));

    struct Protected;
    impl crate::caller::Session for Protected {
        fn caller(&self) -> crate::caller::Caller {
            crate::caller::Caller::Agent
        }
        fn active(&self) -> bool {
            true
        }
        fn gateway_source(&self) -> native::GatewaySource<'_> {
            native::GatewaySource::NativeUnavailable
        }
    }
    let gateway = Gateway::new().unwrap();
    let response = gateway
        .execute(
            &Protected,
            Request::SelectionConfigured {
                profile_id: "metadata-only".into(),
                model_id: "selected".into(),
                protocol: ProviderProtocol::OpenAiCompatible,
            },
            &mut |_| true,
        )
        .await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.configured, Some(true));
    for request in legacy_requests() {
        let response = gateway.execute(&Protected, request, &mut |_| true).await;
        assert_eq!(
            response.model_error.unwrap().kind,
            ErrorKind::PermissionDenied
        );
    }
}
