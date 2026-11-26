//! A server that is started and then let go — dropped without a shutdown, as an embedder that
//! forgets or a task that panics does — frees the address it bound, shortly after: whatever it
//! started must not keep an endpoint handle alive past it, or a host cannot start again where it
//! was.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use tilo_runtime::device::keep::server::ServerIdentity;
use tilo_server_protocol::config::ServerConfig;
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::keep::common::{TestDevice, TestUser, start_server};

/// How long a server that is no longer running may keep its address.
const RELEASED: Duration = Duration::from_secs(2);

/// An address no other socket on this machine listens on, kept across the restart: bound once to
/// have the kernel name a free port, then let go.
fn free_address() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("a free loopback port");
    let address = socket.local_addr().expect("the socket's address");
    drop(socket);
    address
}

/// Wait, for at most [`RELEASED`], until `listen` can be bound again.
fn wait_for_free(listen: SocketAddr) {
    tilo_test_env::wait_for_within(
        format_args!("{listen} to be free within {RELEASED:?}"),
        RELEASED,
        || UdpSocket::bind(listen).ok().map(drop),
    );
}

/// A server on a free address, with the user, device and state root that keep it alive.
async fn a_server() -> (
    TestUser,
    TestDevice,
    tempfile::TempDir,
    SocketAddr,
    tilo_runtime::device::keep::server::Server,
) {
    let user = TestUser::fresh("Alice");
    let host: TestDevice = user.enrol("home-server", DeviceCapabilities::member());
    let state = tempfile::tempdir().expect("a state root");
    let listen = free_address();
    let server = start_server(
        ServerConfig {
            state_root: state.path().to_path_buf(),
            iroh_listen: listen,
            ..ServerConfig::default()
        },
        ServerIdentity {
            user_public_key: user.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .expect("the server starts");
    (user, host, state, listen, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_server_frees_its_address() {
    let (_user, _host, _state, listen, server) = a_server().await;
    drop(server);
    wait_for_free(listen);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shut_down_server_frees_its_address() {
    let (_user, _host, _state, listen, server) = a_server().await;
    server.shutdown().await;
    wait_for_free(listen);
}

/// The address is held by the endpoint the server's workspaces keep: a server dropped must let
/// go of them, or its endpoint outlives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_server_lets_go_of_its_workspaces() {
    let (_user, _host, _state, _listen, server) = a_server().await;
    let workspaces = server
        .workspaces()
        .expect("the server hosts workspaces")
        .downgrade();
    drop(server);
    tilo_test_env::wait_for_within(
        "a dropped server to let go of its workspaces",
        RELEASED,
        || workspaces.upgrade().is_none().then_some(()),
    );
}
