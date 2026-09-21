//! Rolling protocol compatibility checks against the deployed qOS contract.
//!
//! The frozen side computes challenge signatures from literal deployed bytes;
//! it deliberately does not call the current auth-domain helpers.
//! The qOS v2 transport commit (`8b8c7595`) never reached `origin/main` or a
//! release tag after the Tilo rename (`cc0a1901`), so the transient
//! `qos-relay/2` + Tilo-domain pair was not a deployed wire contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ed25519_dalek::{Signer, Verifier};
use iroh::endpoint::{Endpoint, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::EndpointAddr;
use tilo_relay::auth::{Hello, HelloAck, HelloAck2};
use tilo_relay::framing::{read_frame, write_frame};
use tilo_relay::transport::{
    ALPN, ContentEndpointAdvertisement, ContentEndpointRequest, HomePeerManagementRequest,
    HomePeerManagementResponse, LEGACY_ALPN, LEGACY_DISCOVERY_ALPN, LEGACY_MANAGEMENT_ALPN,
    MAX_FRAME_BYTES,
};
use tilo_relay::{
    ClientError, Envelope, RelayClient, RelayConfig, WorkspaceBinding,
    daemon::{Relay, RelayIdentity},
};
use tilo_user_identity::{DeviceCert, RevocationList, verify_device_cert_at};

use crate::common::{TestDevice, TestUser, client_endpoint};

const DEPLOYED_AUTH_TAG: &[u8] = b"qos-relay-auth-v1\0";
const CURRENT_AUTH_TAG: &[u8] = b"tilo-relay-auth-v1\0";
const DEPLOYED_CERT_TAG: &[u8] = b"qos-devcert-v1\0";
const CURRENT_CERT_TAG: &[u8] = b"tilo-devcert-v1\0";

#[derive(serde::Serialize)]
struct FrozenDeviceCertView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    device_label: &'a str,
    capabilities: &'a tilo_user_identity::DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
}

fn frozen_cert_digest(tag: &[u8], cert: &DeviceCert) -> [u8; 32] {
    let view = FrozenDeviceCertView {
        user_public_key: &cert.user_public_key,
        device_public_key: &cert.device_public_key,
        device_label: &cert.device_label,
        capabilities: &cert.capabilities,
        issued_at_ns: cert.issued_at_ns,
        expires_at_ns: cert.expires_at_ns,
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag);
    hasher.update(&postcard::to_allocvec(&view).unwrap());
    *hasher.finalize().as_bytes()
}

fn resign_cert(cert: &mut DeviceCert, root: &ed25519_dalek::SigningKey, tag: &[u8]) {
    cert.signature = root.sign(&frozen_cert_digest(tag, cert)).to_bytes();
}

fn frozen_verify_cert(cert: &DeviceCert, tag: &[u8]) -> Result<(), &'static str> {
    let verifier = ed25519_dalek::VerifyingKey::from_bytes(&cert.user_public_key)
        .map_err(|_| "invalid user key")?;
    verifier
        .verify(
            &frozen_cert_digest(tag, cert),
            &ed25519_dalek::Signature::from_bytes(&cert.signature),
        )
        .map_err(|_| "certificate signature mismatch")
}

fn frozen_digest(tag: &[u8], nonce: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag);
    hasher.update(nonce);
    *hasher.finalize().as_bytes()
}

async fn deployed_client_handshake(
    endpoint: &Endpoint,
    relay_addr: EndpointAddr,
    alpn: &'static [u8],
    client: &TestDevice,
    expected_server_device: [u8; 32],
) -> (
    iroh::endpoint::Connection,
    iroh::endpoint::SendStream,
    iroh::endpoint::RecvStream,
) {
    let connection = endpoint.connect(relay_addr, alpn).await.unwrap();
    let (mut send, mut receive) = connection.open_bi().await.unwrap();
    let client_nonce = [0x31; 32];
    write_frame(
        &mut send,
        &postcard::to_allocvec(&Hello {
            cert: client.cert.clone(),
            client_nonce,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let acknowledgement: HelloAck =
        postcard::from_bytes(&read_frame(&mut receive, MAX_FRAME_BYTES).await.unwrap()).unwrap();
    frozen_verify_cert(&acknowledgement.cert, DEPLOYED_CERT_TAG).unwrap();
    assert_eq!(
        acknowledgement.cert.device_public_key,
        expected_server_device
    );
    let verifier =
        ed25519_dalek::VerifyingKey::from_bytes(&acknowledgement.cert.device_public_key).unwrap();
    verifier
        .verify(
            &frozen_digest(DEPLOYED_AUTH_TAG, &client_nonce),
            &ed25519_dalek::Signature::from_bytes(&acknowledgement.sig_over_client_nonce),
        )
        .unwrap();
    let final_ack = HelloAck2 {
        sig_over_server_nonce: client
            .sk
            .sign(&frozen_digest(
                DEPLOYED_AUTH_TAG,
                &acknowledgement.server_nonce,
            ))
            .to_bytes(),
    };
    write_frame(&mut send, &postcard::to_allocvec(&final_ack).unwrap())
        .await
        .unwrap();
    (connection, send, receive)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deployed_v1_and_v2_clients_authenticate_actual_new_handlers() {
    let user = TestUser::fresh("owner");
    let mut relay_device = user.enrol("home", tilo_user_identity::DeviceCapabilities::host());
    resign_cert(&mut relay_device.cert, &user.user_sk, DEPLOYED_CERT_TAG);
    let relay_public_key = relay_device.device_pubkey();
    let mut client = user.enrol("laptop", tilo_user_identity::DeviceCapabilities::primary());
    resign_cert(&mut client.cert, &user.user_sk, DEPLOYED_CERT_TAG);
    let temp = tempfile::tempdir().unwrap();
    let filesystem =
        EndpointAddr::new(iroh::SecretKey::from_bytes(&relay_device.sk.to_bytes()).public())
            .with_ip_addr("127.0.0.1:40123".parse().unwrap());
    let status_path = temp.path().join("status.json");
    let mut status = fs::full_replica::FullReplicaStatus::syncing();
    status.state = "ready".into();
    status.full_content_replication = true;
    status.filesystem_endpoints.push(filesystem.clone());
    fs::full_replica::save_status(&status_path, &status).unwrap();
    let bundle_path = temp.path().join("home-peer.toml");
    let ticket = fs::Ticket {
        node: EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x66; 32]).public()),
        workspace: [0x67; 32],
    }
    .encode()
    .unwrap();
    let bundle = fs::full_replica::HomePeerSet {
        status_file: status_path.clone(),
        workspaces: vec![fs::full_replica::HomePeerBundle {
            ticket: ticket.clone(),
            cache_dir: temp.path().join("cache"),
            capability_json: "[]".into(),
            authority: None,
            principals: Vec::new(),
            relay_workspace_ids: vec![7],
        }],
    };
    std::fs::write(&bundle_path, toml::to_string(&bundle).unwrap()).unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: temp.path().join("relay"),
            full_replica_status: status_path,
            home_peer_bundle: bundle_path,
            workspaces: vec![WorkspaceBinding {
                id: 7,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![hex::encode(user.user_pk)],
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
            }],
            ..RelayConfig::default()
        },
        RelayIdentity {
            user_public_key: user.user_pk,
            device_signing_key: relay_device.sk.clone(),
            device_cert: relay_device.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    relay
        .set_workspace_membership(
            7,
            true,
            vec![client.device_pubkey()],
            temp.path().to_path_buf(),
        )
        .await
        .unwrap();
    let endpoint = client_endpoint().await;

    let (v1_connection, mut v1_send, mut v1_receive) = deployed_client_handshake(
        &endpoint,
        relay.node_addr(),
        LEGACY_ALPN,
        &client,
        relay_public_key,
    )
    .await;
    v1_send
        .write_all(&tilo_relay::transport::encode(&Envelope::Ping).unwrap())
        .await
        .unwrap();
    assert_eq!(
        tilo_relay::transport::decode(&read_frame(&mut v1_receive, MAX_FRAME_BYTES).await.unwrap())
            .unwrap(),
        Envelope::Pong
    );
    v1_connection.close(0u32.into(), b"");

    let (v2_connection, mut v2_send, mut v2_receive) = deployed_client_handshake(
        &endpoint,
        relay.node_addr(),
        tilo_relay::transport_v2::LEGACY_ALPN_V2,
        &client,
        relay_public_key,
    )
    .await;
    v2_send
        .write_all(
            &tilo_relay::transport_v2::encode(&tilo_relay::transport_v2::EnvelopeV2::State(
                Envelope::Ping,
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        tilo_relay::transport_v2::decode(
            &read_frame(&mut v2_receive, MAX_FRAME_BYTES + 1)
                .await
                .unwrap()
        )
        .unwrap(),
        tilo_relay::transport_v2::EnvelopeV2::State(Envelope::Pong)
    );
    v2_connection.close(0u32.into(), b"");

    // Management uses a separate ALPN, but retains the same deployed
    // qOS-domain mutual challenge contract.
    let (management_connection, mut management_send, mut management_receive) =
        deployed_client_handshake(
            &endpoint,
            relay.node_addr(),
            LEGACY_MANAGEMENT_ALPN,
            &client,
            relay_public_key,
        )
        .await;
    write_frame(
        &mut management_send,
        &postcard::to_allocvec(&HomePeerManagementRequest::Status).unwrap(),
    )
    .await
    .unwrap();
    let response: HomePeerManagementResponse = postcard::from_bytes(
        &read_frame(&mut management_receive, MAX_FRAME_BYTES)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response.device_public_key, relay_public_key);
    management_connection.close(0u32.into(), b"");

    let (discovery_connection, mut discovery_send, mut discovery_receive) =
        deployed_client_handshake(
            &endpoint,
            relay.node_addr(),
            LEGACY_DISCOVERY_ALPN,
            &client,
            relay_public_key,
        )
        .await;
    write_frame(
        &mut discovery_send,
        &postcard::to_allocvec(&ContentEndpointRequest { workspace_id: 7 }).unwrap(),
    )
    .await
    .unwrap();
    let advertisement: ContentEndpointAdvertisement = postcard::from_bytes(
        &read_frame(&mut discovery_receive, MAX_FRAME_BYTES)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(advertisement.workspace_id, 7);
    assert_eq!(advertisement.filesystem_endpoints, vec![filesystem]);
    assert_eq!(advertisement.filesystem_ticket, ticket);
    discovery_connection.close(0u32.into(), b"");

    endpoint.close().await;
    relay.shutdown().await;
}

#[derive(Clone)]
struct FrozenServer {
    cert: DeviceCert,
    signing_key: ed25519_dalek::SigningKey,
    expected_client_user: [u8; 32],
    auth_tag: &'static [u8],
    client_cert_tag: &'static [u8],
    accepted: Arc<AtomicUsize>,
    exchange: Option<(Vec<u8>, Vec<u8>)>,
}

impl std::fmt::Debug for FrozenServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FrozenServer")
    }
}

fn accept_error(error: impl std::fmt::Display) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(error.to_string()))
}

impl ProtocolHandler for FrozenServer {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        self.accepted.fetch_add(1, Ordering::SeqCst);
        let (mut send, mut receive) = connection.accept_bi().await?;
        let hello: Hello = postcard::from_bytes(
            &read_frame(&mut receive, MAX_FRAME_BYTES)
                .await
                .map_err(accept_error)?,
        )
        .map_err(accept_error)?;
        frozen_verify_cert(&hello.cert, self.client_cert_tag).map_err(accept_error)?;
        verify_device_cert_at(&hello.cert, tilo_relay::clock::now_ns()).map_err(accept_error)?;
        if hello.cert.user_public_key != self.expected_client_user {
            return Err(accept_error("client user is not admitted"));
        }
        let server_nonce = [0x42; 32];
        let acknowledgement = HelloAck {
            cert: self.cert.clone(),
            server_nonce,
            sig_over_client_nonce: self
                .signing_key
                .sign(&frozen_digest(self.auth_tag, &hello.client_nonce))
                .to_bytes(),
        };
        write_frame(
            &mut send,
            &postcard::to_allocvec(&acknowledgement).map_err(accept_error)?,
        )
        .await
        .map_err(accept_error)?;
        let final_ack: HelloAck2 = postcard::from_bytes(
            &read_frame(&mut receive, MAX_FRAME_BYTES)
                .await
                .map_err(accept_error)?,
        )
        .map_err(accept_error)?;
        let verifier = ed25519_dalek::VerifyingKey::from_bytes(&hello.cert.device_public_key)
            .map_err(accept_error)?;
        verifier
            .verify(
                &frozen_digest(self.auth_tag, &server_nonce),
                &ed25519_dalek::Signature::from_bytes(&final_ack.sig_over_server_nonce),
            )
            .map_err(accept_error)?;
        if let Some((expected_request, response)) = &self.exchange {
            let request = read_frame(&mut receive, MAX_FRAME_BYTES)
                .await
                .map_err(accept_error)?;
            if request != *expected_request {
                return Err(accept_error("unexpected frozen request"));
            }
            write_frame(&mut send, response)
                .await
                .map_err(accept_error)?;
            // A dropped QUIC send stream is reset. Finish it so the frozen
            // server models the deployed peer's orderly frame delivery.
            send.finish().map_err(accept_error)?;
            connection.closed().await;
        }
        Ok(())
    }
}

async fn frozen_router(current: Option<FrozenServer>, legacy: FrozenServer) -> Router {
    let endpoint = Endpoint::builder(presets::N0).bind().await.unwrap();
    let builder = Router::builder(endpoint);
    let builder = if let Some(current) = current {
        builder.accept(ALPN, current)
    } else {
        builder
    };
    builder.accept(LEGACY_ALPN, legacy).spawn()
}

async fn frozen_one_shot_router(alpn: &'static [u8], legacy: FrozenServer) -> Router {
    let endpoint = Endpoint::builder(presets::N0).bind().await.unwrap();
    Router::builder(endpoint).accept(alpn, legacy).spawn()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_client_falls_back_only_after_actual_tls_no_application_protocol() {
    let user = TestUser::fresh("owner");
    let mut server = user.enrol("old home", tilo_user_identity::DeviceCapabilities::host());
    resign_cert(&mut server.cert, &user.user_sk, DEPLOYED_CERT_TAG);
    let mut client = user.enrol(
        "new laptop",
        tilo_user_identity::DeviceCapabilities::primary(),
    );
    // An updated client retains its persisted qOS-domain certificate.
    resign_cert(&mut client.cert, &user.user_sk, DEPLOYED_CERT_TAG);
    let accepted = Arc::new(AtomicUsize::new(0));
    let protocol = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: DEPLOYED_AUTH_TAG,
        client_cert_tag: DEPLOYED_CERT_TAG,
        accepted: Arc::clone(&accepted),
        exchange: Some((
            postcard::to_allocvec(&Envelope::Ping).unwrap(),
            postcard::to_allocvec(&Envelope::Pong).unwrap(),
        )),
    };
    let router = frozen_router(None, protocol).await;
    let relay_addr = router.endpoint().addr();

    let connected = RelayClient::connect_to_enrolled_home_peer(
        client_endpoint().await,
        relay_addr,
        user.user_pk,
        server.device_pubkey(),
        client.cert,
        client.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    connected.send(Envelope::Ping).unwrap();
    assert_eq!(connected.recv().await, Some(Envelope::Pong));
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    drop(connected);
    router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_one_shot_clients_fall_back_to_frozen_management_and_discovery() {
    let user = TestUser::fresh("owner");
    let mut server = user.enrol("old home", tilo_user_identity::DeviceCapabilities::host());
    resign_cert(&mut server.cert, &user.user_sk, DEPLOYED_CERT_TAG);
    let mut client = user.enrol(
        "updated laptop",
        tilo_user_identity::DeviceCapabilities::primary(),
    );
    resign_cert(&mut client.cert, &user.user_sk, DEPLOYED_CERT_TAG);
    let advertised = EndpointAddr::new(iroh::SecretKey::from_bytes(&[0x71; 32]).public());

    let management_count = Arc::new(AtomicUsize::new(0));
    let management_response = HomePeerManagementResponse {
        device_public_key: server.device_pubkey(),
        relay_endpoint: advertised.clone(),
        label: "old home".into(),
        files_ready: true,
        services: tilo_relay::mesh::ServiceCapabilities::home_peer(),
        decommissioned: false,
    };
    let management = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: DEPLOYED_AUTH_TAG,
        client_cert_tag: DEPLOYED_CERT_TAG,
        accepted: Arc::clone(&management_count),
        exchange: Some((
            postcard::to_allocvec(&HomePeerManagementRequest::Status).unwrap(),
            postcard::to_allocvec(&management_response).unwrap(),
        )),
    };
    let router = frozen_one_shot_router(LEGACY_MANAGEMENT_ALPN, management).await;
    let endpoint = client_endpoint().await;
    let response = RelayClient::manage_home_peer(
        &endpoint,
        router.endpoint().addr(),
        user.user_pk,
        server.device_pubkey(),
        client.cert.clone(),
        client.sk.clone(),
        RevocationList::new(),
        HomePeerManagementRequest::Status,
    )
    .await
    .unwrap();
    assert_eq!(response, management_response);
    assert_eq!(management_count.load(Ordering::SeqCst), 1);
    router.shutdown().await.unwrap();

    let discovery_count = Arc::new(AtomicUsize::new(0));
    let discovery_response = ContentEndpointAdvertisement {
        schema_version: 1,
        workspace_id: 7,
        filesystem_endpoints: vec![advertised.clone()],
        filesystem_ticket: "frozen-qos-ticket".into(),
    };
    let discovery = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: DEPLOYED_AUTH_TAG,
        client_cert_tag: DEPLOYED_CERT_TAG,
        accepted: Arc::clone(&discovery_count),
        exchange: Some((
            postcard::to_allocvec(&ContentEndpointRequest { workspace_id: 7 }).unwrap(),
            postcard::to_allocvec(&discovery_response).unwrap(),
        )),
    };
    let router = frozen_one_shot_router(LEGACY_DISCOVERY_ALPN, discovery).await;
    let registration = RelayClient::discover_content_peer(
        &endpoint,
        router.endpoint().addr(),
        7,
        user.user_pk,
        server.device_pubkey(),
        client.cert,
        client.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert_eq!(registration.endpoint, advertised);
    assert_eq!(registration.ticket, "frozen-qos-ticket");
    assert_eq!(discovery_count.load(Ordering::SeqCst), 1);
    router.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_domain_or_device_pin_failure_never_downgrades() {
    let user = TestUser::fresh("owner");
    let server = user.enrol("home", tilo_user_identity::DeviceCapabilities::host());
    let client = user.enrol("laptop", tilo_user_identity::DeviceCapabilities::primary());
    let current_count = Arc::new(AtomicUsize::new(0));
    let legacy_count = Arc::new(AtomicUsize::new(0));
    let bad_current = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: DEPLOYED_AUTH_TAG,
        client_cert_tag: CURRENT_CERT_TAG,
        accepted: Arc::clone(&current_count),
        exchange: None,
    };
    let legacy = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: DEPLOYED_AUTH_TAG,
        client_cert_tag: CURRENT_CERT_TAG,
        accepted: Arc::clone(&legacy_count),
        exchange: None,
    };
    let router = frozen_router(Some(bad_current), legacy).await;
    let result = RelayClient::connect_to_enrolled_home_peer(
        client_endpoint().await,
        router.endpoint().addr(),
        user.user_pk,
        server.device_pubkey(),
        client.cert,
        client.sk,
        RevocationList::new(),
    )
    .await;
    assert!(matches!(result, Err(ClientError::Auth(_))));
    assert_eq!(current_count.load(Ordering::SeqCst), 1);
    assert_eq!(legacy_count.load(Ordering::SeqCst), 0);
    router.shutdown().await.unwrap();

    let second_client = user.enrol(
        "second laptop",
        tilo_user_identity::DeviceCapabilities::primary(),
    );
    let current_count = Arc::new(AtomicUsize::new(0));
    let legacy_count = Arc::new(AtomicUsize::new(0));
    let good_current = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: CURRENT_AUTH_TAG,
        client_cert_tag: CURRENT_CERT_TAG,
        accepted: Arc::clone(&current_count),
        exchange: None,
    };
    let legacy = FrozenServer {
        cert: server.cert.clone(),
        signing_key: server.sk.clone(),
        expected_client_user: user.user_pk,
        auth_tag: DEPLOYED_AUTH_TAG,
        client_cert_tag: CURRENT_CERT_TAG,
        accepted: Arc::clone(&legacy_count),
        exchange: None,
    };
    let router = frozen_router(Some(good_current), legacy).await;
    let wrong_pin = [0x77; 32];
    let result = RelayClient::connect_to_enrolled_home_peer(
        client_endpoint().await,
        router.endpoint().addr(),
        user.user_pk,
        wrong_pin,
        second_client.cert,
        second_client.sk,
        RevocationList::new(),
    )
    .await;
    assert!(matches!(result, Err(ClientError::Auth(_))));
    assert_eq!(current_count.load(Ordering::SeqCst), 1);
    assert_eq!(legacy_count.load(Ordering::SeqCst), 0);
    router.shutdown().await.unwrap();
}
