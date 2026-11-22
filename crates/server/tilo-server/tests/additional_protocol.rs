#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use iroh::endpoint::presets;
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_server::{Envelope, Server, ServerClient, ServerConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint};

const TEST_ALPN: &[u8] = b"tilo-workspace-join/test";
const TEST_ALPN_TWO: &[u8] = b"tilo-shell-surface/test";

#[derive(Clone, Debug)]
struct Echo {
    shutdown: Arc<AtomicBool>,
}

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let request = tilo_server::daemon::read_frame(&mut recv, 64).await?;
        tilo_server::daemon::write_frame(&mut send, &request).await?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_protocol_and_server_share_one_endpoint() {
    let user = TestUser::fresh("owner");
    let host = user.enrol("host", DeviceCapabilities::member());
    let device = user.enrol("laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_two = Arc::new(AtomicBool::new(false));
    // Reached on its own network only, as the test dials it: the server is told
    // so, and does not wait for an address that nothing will name.
    let endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let endpoint_addr = endpoint.addr();
    let server = Server::start_on_endpoint_with_protocols(
        ServerConfig::loopback(state.path().to_path_buf()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: user.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
            revocations: RevocationList::new(),
        },
        endpoint,
        tilo_server::Reaching::OnItsOwn,
        vec![
            (
                TEST_ALPN.to_vec(),
                Box::<dyn DynProtocolHandler>::from(Echo {
                    shutdown: shutdown.clone(),
                }),
            ),
            (
                TEST_ALPN_TWO.to_vec(),
                Box::<dyn DynProtocolHandler>::from(Echo {
                    shutdown: shutdown_two.clone(),
                }),
            ),
        ],
    )
    .await
    .unwrap();
    assert_eq!(server.node_addr().id, endpoint_addr.id);

    let client = test_server::direct_endpoint().await;
    let connection = client.connect(server.node_addr(), TEST_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    tilo_server::daemon::write_frame(&mut send, b"join")
        .await
        .unwrap();
    send.finish().unwrap();
    assert_eq!(
        tilo_server::daemon::read_frame(&mut recv, 64)
            .await
            .unwrap(),
        b"join"
    );
    drop(connection);

    let connection_two = client
        .connect(server.node_addr(), TEST_ALPN_TWO)
        .await
        .unwrap();
    let (mut send_two, mut recv_two) = connection_two.open_bi().await.unwrap();
    tilo_server::daemon::write_frame(&mut send_two, b"surface")
        .await
        .unwrap();
    send_two.finish().unwrap();
    assert_eq!(
        tilo_server::daemon::read_frame(&mut recv_two, 64)
            .await
            .unwrap(),
        b"surface"
    );
    drop(connection_two);

    let server_client = ServerClient::connect(
        client_endpoint().await,
        server.node_addr(),
        user.user_pk,
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    server_client.send(Envelope::Ping).unwrap();
    assert!(matches!(server_client.recv().await, Some(Envelope::Pong)));
    drop(server_client);
    server.shutdown().await;
    assert!(shutdown.load(Ordering::SeqCst));
    assert!(shutdown_two.load(Ordering::SeqCst));
}
