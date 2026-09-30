//! A state chunk re-issued while its first attempt is still staging.
use super::data_tests::{Harness, drive, frame, native_edit};
use hyper::StatusCode;
use tilo_delivery_protocol::delivery_v3::{
    framing::{self, MessageType},
    state::StateItemChunk,
};

/// A request whose owner reply outruns its deadline is answered with a
/// retryable 503 while its staging worker keeps running, so the client's
/// re-issue of the same chunk can arrive while it is still in flight. That
/// re-issue must be deferred again, never refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunk_re_issued_while_still_staging_is_deferred_not_refused() {
    let mut h = Harness::start().await;
    // Larger than one range, so its first range is a staged partial chunk.
    let canonical = native_edit(&h.f.owner, "staged", vec![7; 600 * 1024]);
    let chunk = StateItemChunk::new(
        [91; 16],
        *blake3::hash(&canonical).as_bytes(),
        canonical.len() as u64,
        0,
        &canonical[..240 * 1024],
    )
    .unwrap();
    let body = frame(MessageType::StateItemChunk, &chunk.encode().unwrap());
    let send = |h: &Harness| {
        let client = super::tests::Client {
            address: h.client.address,
            tls: h.client.tls.clone(),
        };
        let proof = h.proof("POST", "/peer/v3/state", &body);
        let body = body.clone();
        tokio::spawn(async move {
            client
                .request_method("POST", "/peer/v3/state", body, Some(proof))
                .await
        })
    };
    let (first, second) = (send(&h), send(&h));
    // Hand both to the owner before any staging result is pumped, so the
    // second finds the first still in flight.
    for _ in 0..2 {
        let request = tilo_test_env::within("a state chunk at the ingress", h.ingress.recv())
            .await
            .unwrap();
        h.ingress.handle(&mut h.f.owner, request);
    }
    let replies = drive(&mut h.ingress, &mut h.f.owner, async {
        [first.await.unwrap(), second.await.unwrap()]
    })
    .await;
    let deferred = |(status, body): &(StatusCode, Vec<u8>)| {
        *status == StatusCode::SERVICE_UNAVAILABLE && body == b"temporarily-unavailable"
    };
    for reply in &replies {
        assert!(
            reply.0 == StatusCode::OK || deferred(reply),
            "a re-issued chunk is never refused: {} {}",
            reply.0,
            String::from_utf8_lossy(&reply.1)
        );
    }
    assert!(
        replies.iter().any(|reply| reply.0 == StatusCode::OK),
        "one attempt stages the chunk"
    );
    if replies.iter().any(deferred) {
        let (status, bytes) = h.signed("POST", "/peer/v3/state", body).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let (kind, _, rest) = framing::decode_frame_prefix(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(
            kind,
            MessageType::FlowCredit,
            "the staged prefix is credited"
        );
    }
}
