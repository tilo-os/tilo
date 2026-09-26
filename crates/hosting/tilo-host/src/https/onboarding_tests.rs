//! Test-only construction uses the real exact route parser and ingress pool.
use super::*;
pub(crate) struct Probe {
    response: oneshot::Receiver<Reply>,
    cancel: watch::Sender<bool>,
}
impl Probe {
    pub(crate) fn cancel(&self) {
        self.cancel.send_replace(true);
    }
    pub(crate) async fn receive(self) -> (StatusCode, Bytes) {
        let reply = self.response.await.expect("native HTTP reply");
        (reply.status, reply.body)
    }
}
pub(crate) fn request(ingress: &Ingress, target: &str, body: Vec<u8>) -> (BufferedRequest, Probe) {
    let head = Request::builder()
        .method("POST")
        .uri(target)
        .header("host", "localhost:9443")
        .header("origin", "https://localhost:9443")
        .header("content-type", "application/octet-stream")
        .header("content-length", body.len())
        .body(())
        .unwrap();
    let (parts, _) = head.into_parts();
    let parsed = parse_head(&parts, &Origin::parse("https://localhost:9443").unwrap()).unwrap();
    assert!(body.len() <= parsed.route.body_limit());
    assert!(
        !parsed.route.control(),
        "onboarding must preserve the control floor"
    );
    let lease = ingress
        .reserve(parsed.route.body_limit(), Lane::Data)
        .unwrap();
    let (cancel, cancelled) = watch::channel(false);
    let (response, received) = oneshot::channel();
    (
        BufferedRequest {
            method: parts.method,
            target: parsed.target,
            body: Bytes::from(body),
            proof: parsed.proof,
            route: parsed.route,
            deadline: Instant::now() + DEADLINE,
            cancelled,
            response,
            lease,
        },
        Probe {
            response: received,
            cancel,
        },
    )
}
