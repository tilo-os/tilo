//! Component transport proof. The installed host dispatcher is qualified separately.
use super::*;
use crate::{
    collaboration::{CollaborationService, IssueInvitationRequest},
    host::LocalHost,
    https::{HttpsAdapter, IngressReceiver},
    owner::resources::OwnerOperation,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::StatusCode;
use hyper_util::rt::TokioIo;
use qos_peer_state::join::*;
use rustls::pki_types::{PrivatePkcs8KeyDer, ServerName};
use std::{sync::Arc, time::Duration};
use tilo_relay::delivery_v3::onboarding_wire::{OnboardingResponse, ReplyBodyKind};
use tokio::time::timeout;

struct Client {
    address: std::net::SocketAddr,
    tls: Arc<rustls::ClientConfig>,
}
impl Client {
    async fn post(&self, path: &str, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
        let tcp = tokio::net::TcpStream::connect(self.address).await.unwrap();
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .unwrap();
        let driving = tokio::spawn(connection);
        let response = sender
            .send_request(
                hyper::Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", format!("localhost:{}", self.address.port()))
                    .header(
                        "origin",
                        format!("https://localhost:{}", self.address.port()),
                    )
                    .header("content-type", "application/octet-stream")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        drop(sender);
        driving.abort();
        let _ = driving.await;
        (status, body)
    }
}

async fn call(
    f: &mut Fixture,
    receiver: &mut IngressReceiver,
    service: &CollaborationService,
    host: &LocalHost,
    client: &Client,
    inner: JoinTransportRequest,
    nonce: u8,
) -> JoinTransportResponse {
    let wire = f.wire_for(&inner, [nonce; 32]);
    let original = OnboardingRequest::decode(&wire).unwrap();
    let transport = client.post(original.operation.path(), wire.clone());
    let actor = async {
        let request = timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        // The exact HTTPS receiver and original body charge enter the real owner turn.
        let mut scope = f
            .owner
            .prepare_owner_scope(
                &f.ingress.host_bytes(),
                OwnerOperation::Join {
                    input_len: request.body().len(),
                },
            )
            .unwrap();
        let (exchange, outcome) = f
            .owner
            .with_owner_view_scope(&mut scope, &f.current, |view, owner| {
                let mut exchange = f.registry.admit(owner, view, request, f.now)?;
                let outcome = exchange.invoke(owner, service, f.now)?;
                anyhow::Ok((exchange, outcome))
            })
            .unwrap()
            .unwrap();
        // Keep the scope and request custody, but no Current guard, across reconciliation.
        if outcome.reconcile {
            host.update_membership(&f.owner).await.unwrap();
        }
        let current = f.current.read().unwrap();
        assert_eq!(current.view().generation(), f.owner.committed_generation());
        f.registry
            .finish(exchange, &f.owner, current.view(), outcome.response, f.now)
            .unwrap();
        drop(current);
        drop(scope);
    };
    let ((status, bytes), ()) = timeout(Duration::from_secs(15), async {
        tokio::join!(transport, actor)
    })
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    let response = OnboardingResponse::decode(&bytes).unwrap();
    response.verify_host_signature(&original, f.now).unwrap();
    assert_eq!(response.body_kind, ReplyBodyKind::InlineLegacy);
    postcard::from_bytes(response.body).unwrap()
}

#[tokio::test]
async fn onboarding_pinned_tls_five_operations_reach_actual_scoped_service() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("https://localhost:{}", address.port());
    let mut f = Fixture::at_origin(&origin);
    let service = CollaborationService::new(vec![]);
    let administrator = f.owner.peer().authority().device_public_key;
    let invitation = f
        .current
        .with_owner(&mut f.owner, |owner| {
            service.issue(
                owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [160; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(f.certificate.user_public_key),
                    invitee_name: Some("Independent TLS person".into()),
                    expires_at_ns: f.now + 120_000_000_000,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    let host = LocalHost::start(&f.owner).await.unwrap();
    let (ingress, mut receiver) = ingress_channel();
    f.registry = OnboardingRegistry::new([133; 16], ingress.host_bytes()).unwrap();
    f.ingress = ingress.clone();
    f.owner
        .attach_data_resources(&ingress.host_bytes())
        .unwrap();
    let assets = tempfile::tempdir().unwrap();
    std::fs::write(assets.path().join("index.html"), b"onboarding test").unwrap();
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let client = Client {
        address,
        tls: Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        ),
    };
    let adapter = HttpsAdapter::new(
        listener,
        &origin,
        crate::https_assets::AssetRoot::open(assets.path()).unwrap(),
        vec![cert.der().clone()],
        PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        ingress,
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(adapter.serve(stopped));

    assert!(matches!(
        call(
            &mut f,
            &mut receiver,
            &service,
            &host,
            &client,
            JoinTransportRequest::Inspect {
                signed_invitation: invitation.signed_invitation.clone()
            },
            160
        )
        .await,
        JoinTransportResponse::Inspection(_)
    ));
    let certificate = f.certificate.clone();
    let JoinTransportResponse::Challenge(challenge) = call(
        &mut f,
        &mut receiver,
        &service,
        &host,
        &client,
        JoinTransportRequest::Challenge(JoinChallengeRequest {
            signed_invitation: invitation.signed_invitation,
            device_cert: certificate,
            request_id: [161; 32],
        }),
        161,
    )
    .await
    else {
        panic!("real join challenge")
    };
    let proof = sign_join_proof(challenge.challenge, &f.device).unwrap();
    let JoinTransportResponse::Complete(joined) = call(
        &mut f,
        &mut receiver,
        &service,
        &host,
        &client,
        JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
        162,
    )
    .await
    else {
        panic!("real admitted join")
    };
    assert_eq!(joined.device_cert, f.certificate);
    assert_eq!(
        joined.governance_messages,
        f.owner
            .outgoing()
            .iter()
            .map(|v| v.bytes.to_vec())
            .collect::<Vec<_>>()
    );
    let membership = MembershipChallengeRequest {
        room: f.owner.peer().room().into(),
        workspace_id: f.owner.workspace_id(),
        device_cert: f.certificate.clone(),
        request_id: [163; 32],
    };
    let JoinTransportResponse::MembershipChallenge(challenge) = call(
        &mut f,
        &mut receiver,
        &service,
        &host,
        &client,
        JoinTransportRequest::MembershipChallenge(membership),
        163,
    )
    .await
    else {
        panic!("real membership challenge")
    };
    let proof = sign_membership_proof(challenge.challenge, &f.device).unwrap();
    let JoinTransportResponse::MembershipComplete(bootstrap) = call(
        &mut f,
        &mut receiver,
        &service,
        &host,
        &client,
        JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
        164,
    )
    .await
    else {
        panic!("real current membership")
    };
    assert_eq!(bootstrap.governance_messages, joined.governance_messages);
    assert!(
        f.owner
            .peer()
            .permissions(f.certificate.device_public_key, at(f.now))
            .is_ok()
    );
    stop.send(true).unwrap();
    timeout(Duration::from_secs(5), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    host.shutdown().await.unwrap();
}
