//! Cancellation negotiated on one native connection.
use super::*;

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
            Request::ListModels {
                profile_id: "never-authorized".into(),
            },
            Request::Cancel { id: 999 },
            Request::ListModels {
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
