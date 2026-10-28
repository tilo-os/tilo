//! An idle connection between two tilo-fs endpoints is kept alive by one
//! keep-alive per path every [`IDLE_KEEP_ALIVE`], not iroh's 5 s, and
//! outlives iroh's 30 s idle timeout.

use super::*;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};

const ECHO_ALPN: &[u8] = b"tilo/test/keep-alive/0";

/// Echoes each stream back, for as long as the connection lives.
#[derive(Clone, Debug)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            let bytes = recv
                .read_to_end(64)
                .await
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))?;
            send.write_all(&bytes)
                .await
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))?;
            send.finish()
                .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))?;
        }
        Ok(())
    }
}

/// The production transport configuration, without relays, so the test
/// runs on loopback alone.
async fn bind() -> Endpoint {
    endpoint_builder(iroh::SecretKey::generate(), &MemoryLookup::new())
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap()
}

async fn echo(connection: &Connection, bytes: &[u8]) -> Vec<u8> {
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    send.write_all(bytes).await.unwrap();
    send.finish().unwrap();
    recv.read_to_end(64).await.unwrap()
}

/// Longer than iroh's 30 s connection idle timeout, and long enough for
/// iroh's 5 s keep-alive to fire seven times on each path.
const IDLE: Duration = Duration::from_secs(35);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_connection_keeps_alive_every_25_s_and_outlives_30_s() {
    let server = bind().await;
    let _router = Router::builder(server.clone())
        .accept(ECHO_ALPN, Echo)
        .spawn();
    let client = bind().await;
    let connection = client.connect(server.addr(), ECHO_ALPN).await.unwrap();
    assert_eq!(echo(&connection, b"before").await, b"before");
    // Let the paths settle: iroh opens the other loopback paths just after
    // the handshake.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let before = connection.stats();
    let mut paths = connection.paths().len();
    let started = std::time::Instant::now();
    while started.elapsed() < IDLE {
        tokio::time::sleep(Duration::from_secs(1)).await;
        paths = paths.max(connection.paths().len());
    }
    let after = connection.stats();
    let pings = after.frame_tx.ping.saturating_sub(before.frame_tx.ping);
    let sent = after
        .udp_tx
        .datagrams
        .saturating_sub(before.udp_tx.datagrams);
    eprintln!(
        "an idle connection over {} s on {paths} paths: {pings} keep-alives, {sent} datagrams sent",
        IDLE.as_secs()
    );

    assert!(
        connection.close_reason().is_none(),
        "an idle connection must outlive iroh's 30 s idle timeout: {:?}",
        connection.close_reason()
    );
    assert_eq!(
        echo(&connection, b"after").await,
        b"after",
        "the idle connection must still carry a stream"
    );
    // One path keep-alive per path in the window, and at most one more for
    // the connection as a whole; iroh's 5 s cadence sends seven per path.
    let bound = u64::try_from(paths).unwrap().saturating_add(1);
    assert!(
        pings <= bound,
        "{pings} keep-alives on {paths} paths in {} s: more than one per path every {} s",
        IDLE.as_secs(),
        IDLE_KEEP_ALIVE.as_secs()
    );
}
