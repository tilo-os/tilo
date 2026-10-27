//! Real native capability cancellation, provider disconnect and retry.
use super::*;
use crate::transport::{FdTransport, Transport};
use std::os::fd::AsFd;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn delta(text: &str, finish: &serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "id":"reply", "object":"chat.completion.chunk", "created":1,
            "model":"test-model", "choices":[{"index":0,"delta":{"content":text},"finish_reason":finish}]
        })
    )
}

async fn read_post(stream: &mut tokio::net::TcpStream) {
    let mut reader = tokio::io::BufReader::new(stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    reader
        .read_exact(&mut vec![0; length.unwrap()])
        .await
        .unwrap();
}

async fn native_stop_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "test-provider".into();
    profile.endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    profile.models = vec![agent_provider_config::ModelDescriptor::new("test-model")];
    ProviderConfig {
        profiles: vec![profile.clone()],
        default_profile: None,
        ..Default::default()
    }
    .save()
    .unwrap();
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
    let (reference, authorization) = credential_binding(&profile).unwrap();
    ServiceCredentials::open_in(&root)
        .store(&reference, &authorization, "fabricated-test-key")
        .unwrap();

    let provider = async {
        let (mut first, _) = listener.accept().await.unwrap();
        read_post(&mut first).await;
        first
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        first
            .write_all(delta("partial", &serde_json::Value::Null).as_bytes())
            .await
            .unwrap();
        // Stopping must close the paid generation immediately, not merely hide
        // its output until the provider's 120 second deadline.
        let mut byte = [0];
        assert_eq!(first.read(&mut byte).await.unwrap(), 0);
        let (mut second, _) = listener.accept().await.unwrap();
        read_post(&mut second).await;
        let body = delta("retry answer", &serde_json::Value::Null)
            + &delta("", &serde_json::json!("stop"))
            + "data: [DONE]\n\n";
        second.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    };
    let (mut original, peer) = UnixStream::pair().unwrap();
    write_frame(
        &mut original,
        &serde_json::json!({"protocol":1,"app_id":CHAT_APP}),
    )
    .unwrap();
    let transport = FdTransport::new(original.as_fd()).unwrap();
    peer.set_nonblocking(true).unwrap();
    let http = provider_client().unwrap();
    let gateway = serve_connection(tokio::net::UnixStream::from_std(peer).unwrap(), &http);
    let client = async move {
        let request = || Request::Complete {
            profile_id: "test-provider".into(),
            model_id: "test-model".into(),
            request: agent_provider_config::protocol::Conversation::new("system", "question")
                .request(vec![], 64),
            timeout_ms: 5000,
            stream: true,
        };
        let mut saw_delta = false;
        let stopped = transport
            .call(request(), &mut |event| {
                saw_delta |= event.event.as_deref() == Some("text_delta");
                false
            })
            .await
            .unwrap_err();
        assert!(saw_delta);
        assert_eq!(
            stopped.kind,
            agent_provider_config::protocol::ErrorKind::Cancelled
        );
        let retry = transport.call(request(), &mut |_| true).await.unwrap();
        assert!(retry.ok, "{:?}", retry.error);
        let answer: agent_provider_config::protocol::ModelResponse =
            serde_json::from_str(retry.output.as_deref().unwrap()).unwrap();
        assert!(
            serde_json::to_string(&answer)
                .unwrap()
                .contains("retry answer")
        );
        drop(transport);
        drop(original);
    };
    Box::pin(tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::join!(gateway, provider, client);
    }))
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn queued_normal_requests_survive_unmatched_cancellation() {
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    HANDOFF_FRAMING
        .write_async(
            &mut client,
            &serde_json::to_vec(&serde_json::json!({"protocol":1,"app_id":AGENT_APP})).unwrap(),
        )
        .await
        .unwrap();
    let http = provider_client().unwrap();
    let service = serve_connection(server, &http);
    let client = async move {
        for request in [
            Request::CancellationSupported,
            Request::SecretStatus {
                profile_id: "never-authorized".into(),
            },
            Request::Cancel { id: 999 },
            Request::SecretStatus {
                profile_id: "still-never-authorized".into(),
            },
        ] {
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
        }
        let mut outputs = Vec::new();
        for _ in 0..4 {
            let body = FRAMING.read_async(&mut client).await.unwrap().unwrap();
            outputs.push(serde_json::from_slice::<Response>(&body).unwrap());
        }
        assert_eq!(outputs[0].output.as_deref(), Some("cancel-v1"));
        assert!(!outputs[1].ok);
        assert_eq!(outputs[2].output.as_deref(), Some("cancelled:999"));
        assert!(!outputs[3].ok);
    };
    Box::pin(tokio::time::timeout(tilo_test_env::hang_bound(), async {
        tokio::join!(service, client);
    }))
    .await
    .unwrap();
}

#[test]
fn native_stop_disconnects_provider_and_second_inference_succeeds() {
    const MARKER: &str = "GATEWAY_NATIVE_CANCEL_CHILD";
    if std::env::var_os(MARKER).is_some() {
        tilo_rt::block_on(native_stop_retry()).unwrap();
        return;
    }
    // Do not mutate environment/configuration shared with parallel tests.
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "service::cancellation_tests::native_stop_disconnects_provider_and_second_inference_succeeds", "--nocapture"])
        .env_clear().env("PATH", root.path()).env("XDG_CONFIG_HOME", root.path())
        .env(MARKER, "1").output().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
