use super::*;
use crate::{
    delivery_http::tests::Fixture,
    room_media_policy::tests::{CALL, activate},
};
use bytes::Bytes;
use ed25519_dalek::Signer;
use http_body_util::{BodyExt, Full};
use hyper::StatusCode;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{PrivatePkcs8KeyDer, ServerName};
use std::{sync::Arc, time::Duration};
use tilo_relay::delivery_v3::{
    auth,
    http_proof::{Request, RequestProof},
    media_wire::*,
};
use tokio::time::timeout;
pub(super) struct Client {
    pub(super) address: std::net::SocketAddr,
    pub(super) tls: Arc<rustls::ClientConfig>,
}
impl Client {
    pub(super) async fn request(
        &self,
        path: &str,
        body: Vec<u8>,
        proof: Option<Vec<u8>>,
    ) -> (StatusCode, Vec<u8>) {
        self.request_method("POST", path, body, proof).await
    }
    pub(super) async fn request_method(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
        proof: Option<Vec<u8>>,
    ) -> (StatusCode, Vec<u8>) {
        let tcp = tokio::net::TcpStream::connect(self.address).await.unwrap();
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .unwrap();
        let driving = tokio::spawn(connection);
        let mut builder = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header("host", self.address.to_string());
        if let Some(proof) = proof {
            builder = builder.header("x-tilo-delivery-proof", hex::encode(proof));
        }
        let response = sender
            .send_request(builder.body(Full::new(Bytes::from(body))).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        drop(sender);
        driving.abort();
        let _ = driving.await;
        (status, bytes)
    }
}
async fn exchange(
    ingress: &mut InstalledIngress,
    owner: &mut SessionOwner,
    client: &Client,
    path: &str,
    body: Vec<u8>,
    proof: Option<Vec<u8>>,
) -> Vec<u8> {
    let transport = client.request(path, body, proof);
    let actor = async {
        let request = timeout(Duration::from_secs(5), ingress.recv())
            .await
            .unwrap()
            .unwrap();
        ingress.handle(owner, request)
    };
    let ((status, bytes), ()) = tokio::join!(transport, actor);
    assert_eq!(status, StatusCode::OK);
    bytes
}
async fn command(
    ingress: &mut InstalledIngress,
    f: &mut Fixture,
    client: &Client,
    id: [u8; 16],
    digest: [u8; 32],
    cmd: ControlCommand,
) -> ControlResult {
    let path = crate::media_control::route(&cmd).path();
    let body = ControlRequest {
        version: 1,
        command: cmd,
    }
    .encode()
    .unwrap();
    let proof = RequestProof::sign(
        id,
        digest,
        &Request::new("POST", path, &body).unwrap(),
        f.now,
        f.now + 30_000_000_000,
        &f.owner.identity.native,
    )
    .unwrap()
    .encode()
    .unwrap();
    ControlReply::decode(&exchange(ingress, &mut f.owner, client, path, body, Some(proof)).await)
        .unwrap()
        .result
}
#[tokio::test]
async fn installed_tls_native_owner_and_same_origin_draft21_share_real_custody() {
    let mut f = Fixture::new();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("https://127.0.0.1:{}", address.port());
    f.current
        .with_owner(&mut f.owner, |owner| {
            activate(owner);
            owner.install_delivery_transport(
                &origin,
                0,
                web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(f.now),
            )
        })
        .unwrap()
        .unwrap();
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let certificate = cert.cert.der().clone();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.clone()).unwrap();
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let client = Client {
        address,
        tls: Arc::new(tls),
    };
    let assets = tempfile::tempdir().unwrap();
    std::fs::write(assets.path().join("index.html"), "isolated").unwrap();
    let config = IngressConfig {
        origin: origin.clone(),
        assets: AssetRoot::open(assets.path()).unwrap(),
        certificates: vec![certificate.clone()],
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der())),
        local_enrollment: None,
    };
    let mut ingress = InstalledIngress::start(&mut f.owner, f.current.clone(), listener, config)
        .await
        .unwrap();
    {
        let guard = f.current.read().unwrap();
        let descriptor = f
            .owner
            .verified_delivery_transport(
                guard.view(),
                web_time::SystemTime::now(),
                crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            )
            .unwrap();
        assert_eq!(
            descriptor.media_capabilities(),
            tilo_relay::delivery_v3::descriptor::MEDIA_VOICE_V1
                | tilo_relay::delivery_v3::live_wire::MEDIA_LIVE_V1
        );
    }
    let locator_hint = f
        .owner
        .sign_delivery_origin_hint(&ingress)
        .expect("only actual installed ingress emits invitation locator");
    let locator = tilo_relay::workspace_join::DeliveryOriginLocatorV1::from_transport_hint(
        &locator_hint,
        f.owner.host_certificate(),
    )
    .unwrap();
    assert_eq!(locator.origin, origin);
    let mut foreign = Fixture::new();
    assert!(foreign.owner.sign_delivery_origin_hint(&ingress).is_err());
    let service = crate::collaboration::CollaborationService::new(vec![locator_hint.clone()]);
    let administrator = f.owner.peer().authority().device_public_key;
    let issued = f
        .current
        .with_owner(&mut f.owner, |owner| {
            service.issue(
                owner,
                administrator,
                crate::collaboration::IssueInvitationRequest {
                    request_id: [94; 32],
                    role: qos_peer_state::join::MemberRole::Editor,
                    recipient_user_public_key: None,
                    invitee_name: None,
                    expires_at_ns: f.now + 300_000_000_000,
                },
                f.now,
            )
        })
        .unwrap()
        .unwrap();
    let verified_invite = qos_peer_state::join::decode_and_verify_invitation(
        &issued.signed_invitation,
        None,
        &tilo_user_identity::RevocationList::new(),
        f.now,
    )
    .unwrap();
    assert_eq!(
        verified_invite.grant().transport,
        vec![locator_hint.clone()]
    );
    let before_read = f.owner.committed_generation();
    let transport = client.request_method("GET", "/peer/v3/descriptor", vec![], None);
    let actor = async {
        let request = timeout(Duration::from_secs(5), ingress.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.route(), Route::Descriptor);
        assert!(request.proof().is_none());
        ingress.handle(&mut f.owner, request);
    };
    let ((status, bytes), ()) = tokio::join!(transport, actor);
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.len() <= tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES);
    assert_eq!(
        bytes,
        postcard::to_stdvec(f.owner.stored_delivery_hint().unwrap()).unwrap()
    );
    assert_eq!(f.owner.committed_generation(), before_read);
    for (path, body, proof) in [
        ("/peer/v3/descriptor/", vec![], None),
        ("/peer/v3/descriptor?x=1", vec![], None),
        ("/peer/v3/%64escriptor", vec![], None),
        ("/peer/v3/descriptor", vec![1], None),
        ("/peer/v3/descriptor", vec![], Some(vec![0])),
    ] {
        let (status, _) = client.request_method("GET", path, body, proof).await;
        assert!(!status.is_success());
        assert!(ingress.receiver.try_recv().is_none());
    }
    let transport = client.request_method("GET", "/peer/v3/descriptor", vec![], None);
    let actor = async {
        let request = timeout(Duration::from_secs(5), ingress.recv())
            .await
            .unwrap()
            .unwrap();
        ingress.handle(&mut foreign.owner, request);
    };
    let ((status, _), ()) = tokio::join!(transport, actor);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let was_failed = ingress.failed;
    ingress.failed = true;
    assert!(f.owner.sign_delivery_origin_hint(&ingress).is_err());
    ingress.failed = was_failed;
    let mut discovery = serde_json::json!({"bootstrap":{"room":f.owner.peer().room()}});
    assert!(discovery["bootstrap"].get("delivery_hint").is_none());
    crate::enrollment::append_delivery_hint(&mut discovery, &f.owner, &ingress).unwrap();
    let hinted: qos_peer_state::join::TransportHint =
        serde_json::from_value(discovery["bootstrap"]["delivery_hint"].clone()).unwrap();
    assert_eq!(&hinted, f.owner.stored_delivery_hint().unwrap());
    assert!(serde_json::to_vec(&discovery).unwrap().len() < 128 * 1024);
    let nonce = [15; 32];
    let challenge = f.request(nonce);
    let server = auth::ServerProof::decode(
        &exchange(
            &mut ingress,
            &mut f.owner,
            &client,
            "/peer/v3/challenge",
            challenge,
            None,
        )
        .await,
    )
    .unwrap();
    f.now = tilo_relay::clock::now_ns();
    let finish = f.client(&server, nonce).encode().unwrap();
    let ready = exchange(
        &mut ingress,
        &mut f.owner,
        &client,
        "/peer/v3/sessions",
        finish,
        None,
    )
    .await;
    let (_, id, digest, _, _): (u16, [u8; 16], [u8; 32], u64, u64) =
        postcard::from_bytes(&ready).unwrap();
    let intent = GrantRequest {
        device: f.owner.identity.native_cert.device_public_key,
        room: f.owner.peer().room().into(),
        call_id: hex::encode(CALL),
        participation_id: [12; 32],
        generation: 0,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
    };
    let ControlResult::Challenge(challenge) = command(
        &mut ingress,
        &mut f,
        &client,
        id,
        digest,
        ControlCommand::GrantChallenge {
            request: intent,
            operation: GrantOperation::Join,
        },
    )
    .await
    else {
        panic!()
    };
    let ChallengeTranscript::Native(inner) = &challenge.transcript else {
        panic!()
    };
    let signature = split_signature(
        f.owner
            .identity
            .native
            .sign(&challenge.transcript.signing_digest().unwrap())
            .to_bytes(),
    );
    let ControlResult::Granted { grant } = command(
        &mut ingress,
        &mut f,
        &client,
        id,
        digest,
        ControlCommand::GrantComplete {
            nonce: inner.nonce,
            signature,
        },
    )
    .await
    else {
        panic!()
    };
    let ControlResult::Challenge(relay) = command(
        &mut ingress,
        &mut f,
        &client,
        id,
        digest,
        ControlCommand::RelayChallenge { grant },
    )
    .await
    else {
        panic!()
    };
    let ChallengeTranscript::Relay(inner) = &relay.transcript else {
        panic!()
    };
    let sid = inner.session_id;
    let setup = crate::media_service::encode_relay_proof(
        sid,
        inner.nonce,
        f.owner
            .identity
            .native
            .sign(&relay.transcript.signing_digest().unwrap())
            .to_bytes(),
    )
    .unwrap();
    let wt = web_transport_quinn::ClientBuilder::new()
        .with_server_certificates(vec![certificate])
        .unwrap();
    let transport = timeout(
        Duration::from_secs(5),
        wt.connect(
            web_transport_quinn::proto::ConnectRequest::new(
                format!("{origin}/moq/v21/voice")
                    .parse::<url::Url>()
                    .unwrap(),
            )
            .with_protocol("moqt-21"),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let (session, driver) = timeout(
        Duration::from_secs(5),
        moq_net::Client::new()
            .with_versions(
                "moq-transport-21"
                    .parse::<moq_net::Version>()
                    .unwrap()
                    .into(),
            )
            .with_authorization(setup)
            .unwrap()
            .connect(transport),
    )
    .await
    .unwrap()
    .unwrap();
    let driver = tokio::spawn(driver);
    let ControlResult::Present(presence) = command(
        &mut ingress,
        &mut f,
        &client,
        id,
        digest,
        ControlCommand::Presence { session_id: sid },
    )
    .await
    else {
        panic!()
    };
    assert!(presence.context.inventory.namespaces.is_empty());
    assert!(matches!(
        command(
            &mut ingress,
            &mut f,
            &client,
            id,
            digest,
            ControlCommand::RelayClose { session_id: sid }
        )
        .await,
        ControlResult::Closed
    ));
    timeout(Duration::from_secs(5), session.closed())
        .await
        .unwrap();
    let _ = timeout(Duration::from_secs(5), driver).await;
    let result = client
        .request("/peer/v3/media/presence/", vec![], None)
        .await;
    assert_ne!(result.0, StatusCode::OK);
    let old = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
        f.owner.stored_delivery_hint().unwrap(),
        f.owner.host_certificate(),
    )
    .unwrap();
    // Renewal is due once less than a fifth of the signed lifetime is left.
    let renewal_due = old.expires_at_ns - (old.expires_at_ns - old.issued_at_ns) / 5;
    assert!(!ingress.refresh_at(&mut f.owner, renewal_due).unwrap());
    assert!(ingress.sessions.has_session(id));
    assert!(ingress.refresh_at(&mut f.owner, old.expires_at_ns).unwrap());
    // The live HTTPS session survives the renewal (retained generation).
    assert!(ingress.sessions.has_session(id));
    let renewed = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
        f.owner.stored_delivery_hint().unwrap(),
        f.owner.host_certificate(),
    )
    .unwrap();
    assert_eq!(renewed.generation, old.generation + 1);
    assert_ne!(renewed.descriptor_digest, old.descriptor_digest);
    {
        let guard = f.current.read().unwrap();
        let descriptor = f
            .owner
            .verified_delivery_transport(
                guard.view(),
                web_time::SystemTime::UNIX_EPOCH
                    + web_time::Duration::from_nanos(old.expires_at_ns),
                crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            )
            .unwrap();
        assert_eq!(
            descriptor.media_capabilities(),
            tilo_relay::delivery_v3::descriptor::MEDIA_VOICE_V1
                | tilo_relay::delivery_v3::live_wire::MEDIA_LIVE_V1
        );
    }
    // Real maintenance -> enrollment discovery -> fresh governor bootstrap,
    // all using the same explicit trusted owner clock after the old expiry.
    let mut renewed_enrollment = serde_json::json!({"bootstrap":{}});
    crate::enrollment::append_delivery_hint_at(
        &mut renewed_enrollment,
        &f.owner,
        &ingress,
        old.expires_at_ns,
    )
    .unwrap();
    let advertised: qos_peer_state::join::TransportHint =
        serde_json::from_value(renewed_enrollment["bootstrap"]["delivery_hint"].clone()).unwrap();
    assert_eq!(
        postcard::to_stdvec(&advertised).unwrap(),
        postcard::to_stdvec(f.owner.stored_delivery_hint().unwrap()).unwrap()
    );
    use tilo_relay::delivery_v3::media_wire::{BootstrapRequest, BootstrapResponse};
    let mut bootstrap = BootstrapRequest {
        version: 1,
        device: f.owner.identity.native_cert.device_public_key,
        nonce: [91; 32],
        workspace: f.owner.workspace_id(),
        root: f.owner.peer().authority().user_public_key,
        room: f.owner.peer().room().into(),
        candidate_digest: renewed.descriptor_digest,
        signature: [[0; 32]; 2],
    };
    bootstrap.signature = split_signature(
        f.owner
            .identity
            .native
            .sign(&bootstrap.signing_digest().unwrap())
            .to_bytes(),
    );
    let response = ingress
        .sessions
        .bootstrap(
            &f.owner,
            &f.current,
            &bootstrap.encode().unwrap(),
            old.expires_at_ns,
        )
        .unwrap();
    let response = BootstrapResponse::decode(&response).unwrap();
    assert_eq!(response.request, bootstrap);
    assert_eq!(response.generation, renewed.generation);
    assert_eq!(response.hint, postcard::to_stdvec(&advertised).unwrap());
    f.owner
        .identity
        .native
        .verifying_key()
        .verify_strict(
            &response.signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&join_signature(response.signature)),
        )
        .unwrap();
    let renewed_discovery = ingress
        .descriptor_bytes_at(&f.owner, old.expires_at_ns)
        .unwrap();
    assert_eq!(
        renewed_discovery,
        postcard::to_stdvec(f.owner.stored_delivery_hint().unwrap()).unwrap()
    );
    let mut stale = bootstrap;
    stale.nonce = [92; 32];
    stale.candidate_digest = old.descriptor_digest;
    stale.signature = split_signature(
        f.owner
            .identity
            .native
            .sign(&stale.signing_digest().unwrap())
            .to_bytes(),
    );
    assert!(
        ingress
            .sessions
            .bootstrap(
                &f.owner,
                &f.current,
                &stale.encode().unwrap(),
                old.expires_at_ns
            )
            .is_err()
    );
    let mut other = Fixture::new();
    assert!(
        ingress
            .refresh_at(&mut other.owner, old.expires_at_ns)
            .is_err()
    );
    f.current
        .with_owner_view(&mut f.owner, |_view, owner| {
            owner.install_delivery_transport(
                &origin,
                0,
                web_time::SystemTime::UNIX_EPOCH
                    + web_time::Duration::from_nanos(old.expires_at_ns),
            )
        })
        .unwrap()
        .unwrap();
    assert!(
        ingress
            .refresh_at(&mut f.owner, old.expires_at_ns)
            .unwrap_err()
            .to_string()
            .contains("replaced")
    );
    ingress.stop.send(true).unwrap();
    assert!(f.owner.sign_delivery_origin_hint(&ingress).is_err());
    assert!(ingress.refresh_at(&mut f.owner, old.expires_at_ns).is_err());
    ingress.shutdown().await.unwrap();
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn installed_discovery_cannot_rollback_the_actual_owner_clock() {
    let mut f = Fixture::new();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let origin = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    f.current
        .with_owner(&mut f.owner, |owner| {
            activate(owner);
            owner.install_delivery_transport(
                &origin,
                0,
                web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(f.now),
            )
        })
        .unwrap()
        .unwrap();
    let (clock, control) =
        qos_action_broker::peer_actions::OwnerPolicyClock::for_test(f.now + 200_000_000_000);
    f.current = CommittedAuthority::new_with_test_clock(&f.owner, &clock);
    let certificate =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let assets = tempfile::tempdir().unwrap();
    std::fs::write(assets.path().join("index.html"), "isolated").unwrap();
    let mut ingress = InstalledIngress::start(
        &mut f.owner,
        f.current.clone(),
        listener,
        IngressConfig {
            origin,
            assets: AssetRoot::open(assets.path()).unwrap(),
            certificates: vec![certificate.cert.der().clone()],
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                certificate.signing_key.serialize_der(),
            )),
            local_enrollment: None,
        },
    )
    .await
    .unwrap();
    let old = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
        &ingress.discovery_hint(&f.owner).unwrap(),
        f.owner.host_certificate(),
    )
    .unwrap();
    control.set_wall_ns(old.expires_at_ns + 1);
    assert_eq!(clock.now_ns().unwrap(), old.expires_at_ns + 1);
    control.set_wall_ns(f.now + 50);
    assert!(
        ingress.discovery_hint(&f.owner).is_err(),
        "installed discovery accepted a descriptor already expired on its actual owner clock"
    );
    assert!(ingress.refresh_current(&mut f.owner).unwrap());
    let renewed = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
        &ingress.discovery_hint(&f.owner).unwrap(),
        f.owner.host_certificate(),
    )
    .unwrap();
    assert_eq!(renewed.generation, old.generation + 1);
    assert_eq!(renewed.issued_at_ns, old.expires_at_ns + 1);
    assert!(!ingress.refresh_current(&mut f.owner).unwrap());
    ingress.shutdown().await.unwrap();
}
