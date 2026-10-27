#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use iroh::endpoint::presets;
use iroh::protocol::{AcceptError, DynProtocolHandler, ProtocolHandler};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_relay::{Envelope, Relay, RelayClient, RelayConfig};
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
        let request = tilo_relay::daemon::read_frame(&mut recv, 64).await?;
        tilo_relay::daemon::write_frame(&mut send, &request).await?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }

    async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_protocol_and_relay_share_one_endpoint() {
    let user = TestUser::fresh("owner");
    let host = user.enrol("host", DeviceCapabilities::host());
    let device = user.enrol("laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_two = Arc::new(AtomicBool::new(false));
    let endpoint = iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let endpoint_addr = endpoint.addr();
    let relay = Relay::start_on_endpoint_with_protocols(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: user.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
            revocations: RevocationList::new(),
        },
        endpoint,
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
    assert_eq!(relay.node_addr().id, endpoint_addr.id);

    let client = iroh::Endpoint::builder(presets::N0).bind().await.unwrap();
    let connection = client.connect(relay.node_addr(), TEST_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    tilo_relay::daemon::write_frame(&mut send, b"join")
        .await
        .unwrap();
    send.finish().unwrap();
    assert_eq!(
        tilo_relay::daemon::read_frame(&mut recv, 64).await.unwrap(),
        b"join"
    );
    drop(connection);

    let connection_two = client
        .connect(relay.node_addr(), TEST_ALPN_TWO)
        .await
        .unwrap();
    let (mut send_two, mut recv_two) = connection_two.open_bi().await.unwrap();
    tilo_relay::daemon::write_frame(&mut send_two, b"surface")
        .await
        .unwrap();
    send_two.finish().unwrap();
    assert_eq!(
        tilo_relay::daemon::read_frame(&mut recv_two, 64)
            .await
            .unwrap(),
        b"surface"
    );
    drop(connection_two);

    let relay_client = RelayClient::connect(
        client_endpoint().await,
        relay.node_addr(),
        user.user_pk,
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    relay_client.send(Envelope::Ping).unwrap();
    assert!(matches!(relay_client.recv().await, Some(Envelope::Pong)));
    drop(relay_client);
    relay.shutdown().await;
    assert!(shutdown.load(Ordering::SeqCst));
    assert!(shutdown_two.load(Ordering::SeqCst));
}
