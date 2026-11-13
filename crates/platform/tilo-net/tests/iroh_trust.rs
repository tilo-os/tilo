//! An iroh endpoint trusts relays as every Tilo client trusts servers: the
//! relay connection and the net report's HTTPS probe against the system
//! store, plus the extra roots Tilo is configured with, as
//! `client_config_also_trusting` trusts them.

#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use iroh::Watcher as _;
use iroh::endpoint::presets;
use iroh_relay::server::{CertConfig, RelayConfig, TlsConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const ALPN: &[u8] = b"tilo-test/iroh-trust/0";

struct LocalCa {
    root: CertificateDer<'static>,
    root_pem: String,
    relay_chain_pem: String,
    relay_key_pem: String,
}

fn local_ca() -> LocalCa {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Tilo test local CA");
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let ca = rcgen::CertifiedIssuer::self_signed(
        ca_params,
        rcgen::KeyPair::generate().expect("a CA key"),
    )
    .expect("the CA certifies itself");

    let relay_key = rcgen::KeyPair::generate().expect("a relay key");
    let relay_cert =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
            .expect("relay params")
            .signed_by(&relay_key, &ca)
            .expect("the CA issues the relay certificate");
    LocalCa {
        root: ca.der().clone(),
        root_pem: ca.pem(),
        relay_chain_pem: format!("{}{}", relay_cert.pem(), ca.pem()),
        relay_key_pem: relay_key.serialize_pem(),
    }
}

fn relay_tls(chain_pem: &str, key_pem: &str) -> rustls::ServerConfig {
    use rustls::pki_types::pem::PemObject as _;
    let chain = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("the relay chain parses");
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).expect("the relay key parses");
    rustls::ServerConfig::builder_with_provider(tilo_net::crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS 1.2 and 1.3")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("the relay certificate and key match")
}

impl LocalCa {
    fn relay_tls(&self) -> rustls::ServerConfig {
        relay_tls(&self.relay_chain_pem, &self.relay_key_pem)
    }
}

async fn relay_under(
    tls: rustls::ServerConfig,
) -> (tilo_test_env::relay::Relay, iroh::RelayUrl, iroh::RelayMap) {
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config: tls },
    ));
    let served = tilo_test_env::relay::Relay::with(relay).await;
    let url = served.url().clone();
    let relays = served.relays();
    (served, url, relays)
}

fn endpoint_builder(relays: &iroh::RelayMap) -> iroh::endpoint::Builder {
    iroh::Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(relays.clone()))
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("a loopback bind address")
}

fn also_trusting(extra: CertificateDer<'static>) -> iroh::tls::CaTlsConfig {
    iroh::tls::CaTlsConfig::custom_server_cert_verifier(Arc::new(move |_provider| {
        tilo_net::server_verifier_also_trusting(vec![extra.clone()]).map_err(std::io::Error::other)
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endpoints_trusting_a_local_ca_reach_each_other_through_its_relay() {
    let ca = local_ca();
    let (server, url, relays) = relay_under(ca.relay_tls()).await;

    let bind = || async {
        let endpoint = endpoint_builder(&relays)
            .ca_tls_config(also_trusting(ca.root.clone()))
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("the endpoint binds");
        tilo_test_env::within("the endpoint to get its home relay", endpoint.online()).await;
        endpoint
    };
    let dialer = bind().await;
    let listener = bind().await;
    assert_eq!(
        listener.home_relay_status().get().len(),
        1,
        "the listener has the local CA's relay as home relay"
    );

    let accepting = tokio::spawn({
        let listener = listener.clone();
        async move {
            let connection = listener
                .accept()
                .await
                .expect("the listener is open")
                .await
                .expect("the dialer's connection completes");
            let (mut send, mut recv) = connection.accept_bi().await.expect("a stream");
            let got = recv.read_to_end(64).await.expect("the request");
            send.write_all(&got).await.expect("the echo");
            send.finish().expect("the echo ends");
            connection.closed().await;
        }
    });

    // Only the id and the relay: the connection starts through the relay.
    let addr = iroh::EndpointAddr::new(listener.id()).with_relay_url(url);
    let connection = tilo_test_env::within(
        "the dialer to connect through the relay",
        dialer.connect(addr, ALPN),
    )
    .await
    .expect("the dialer connects");
    let (mut send, mut recv) = connection.open_bi().await.expect("a stream");
    send.write_all(b"through the local CA's relay")
        .await
        .expect("the request");
    send.finish().expect("the request ends");
    let echoed = tilo_test_env::within("the echo", recv.read_to_end(64))
        .await
        .expect("the echo arrives");
    assert_eq!(echoed, b"through the local CA's relay");
    connection.close(0u32.into(), b"done");
    accepting.await.expect("the listener's side finishes");

    assert!(
        server.server().metrics().server.accepts.get() >= 2,
        "the relay registered both endpoints"
    );
    dialer.close().await;
    listener.close().await;
}

/// An endpoint left at the default trust refuses a relay whose CA is not in
/// the system store: the default is a real store, not a skipped check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_trust_refuses_a_ca_outside_the_store() {
    let ca = local_ca();
    let (server, _url, relays) = relay_under(ca.relay_tls()).await;
    let https = server.server().https_addr().expect("relay HTTPS");
    assert!(
        tilo_net_handshake(https).await.is_err(),
        "a Tilo client trusted a CA outside the store"
    );

    let endpoint = endpoint_builder(&relays)
        .bind()
        .await
        .expect("the endpoint binds with the default trust");
    // Several net reports' worth: the HTTPS probe fails each time.
    let online = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
    assert!(
        online.is_err(),
        "the endpoint trusted a relay under a CA outside the store"
    );
    assert!(
        endpoint.home_relay_status().get().is_empty(),
        "the endpoint took the untrusted relay as home relay"
    );
    assert_eq!(
        server.server().metrics().server.accepts.get(),
        0,
        "the relay registered a client that should have refused it"
    );
    endpoint.close().await;
}

async fn tilo_net_handshake(addr: std::net::SocketAddr) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let config = tilo_net::client_config().map_err(|error| error.to_string())?;
        let name = rustls::pki_types::ServerName::try_from("127.0.0.1").expect("an IP name");
        let mut tls = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|error| error.to_string())?;
        let mut tcp = std::net::TcpStream::connect(addr).map_err(|error| error.to_string())?;
        while tls.is_handshaking() {
            tls.complete_io(&mut tcp)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })
    .await
    .expect("the handshake task finishes")
}

#[cfg(target_os = "linux")]
const STORE: &str = "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem";

#[cfg(target_os = "linux")]
const RELAY_FILES: &str = "TILO_TEST_RELAY_FILES";

/// A CA installed in the system store is trusted by an endpoint left at the
/// default, as by every other Tilo client: a deployment's local CA works as
/// a public one does. The store is a fixed path, so the check runs in a
/// sandbox whose store holds only that CA.
#[cfg(target_os = "linux")]
#[test]
fn a_ca_installed_in_the_store_is_trusted_by_default() {
    use tilo_test_env::SpawnUnderEnvLock as _;

    tilo_test_env::require_capability!(
        UserNamespace,
        tilo_test_env::bubblewrap(),
        "the system store is replaced inside a bubblewrap sandbox"
    );
    let ca = local_ca();
    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("tls-ca-bundle.pem");
    std::fs::write(&store, &ca.root_pem).expect("the store is written");
    std::fs::write(dir.path().join("relay.pem"), &ca.relay_chain_pem).expect("the relay chain");
    std::fs::write(dir.path().join("relay.key"), &ca.relay_key_pem).expect("the relay key");
    let status = std::process::Command::new("bwrap")
        .args(["--die-with-parent", "--unshare-all", "--ro-bind", "/", "/"])
        .args(["--proc", "/proc", "--dev", "/dev", "--ro-bind"])
        .arg(&store)
        .arg(STORE)
        .arg("--")
        .arg(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "default_trust_inside_a_store_holding_the_ca",
            "--ignored",
            "--nocapture",
        ])
        .env(RELAY_FILES, dir.path())
        .status_locked()
        .expect("bwrap runs");
    assert!(status.success(), "the sandboxed run failed: {status}");
}

/// The sandboxed half of [`a_ca_installed_in_the_store_is_trusted_by_default`].
#[cfg(target_os = "linux")]
#[ignore = "run by a_ca_installed_in_the_store_is_trusted_by_default, in a sandbox whose store holds the CA"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_trust_inside_a_store_holding_the_ca() {
    let files =
        std::path::PathBuf::from(std::env::var_os(RELAY_FILES).expect("run in the sandbox"));
    let read = |name: &str| std::fs::read_to_string(files.join(name)).expect("a relay file");
    assert_eq!(
        std::fs::read_to_string(STORE).expect("the sandbox's store"),
        read("tls-ca-bundle.pem"),
        "the system store holds the local CA alone"
    );
    let (server, _url, relays) =
        relay_under(relay_tls(&read("relay.pem"), &read("relay.key"))).await;
    tilo_net_handshake(server.server().https_addr().expect("relay HTTPS"))
        .await
        .expect("a Tilo client trusts the CA in the store");

    let endpoint = endpoint_builder(&relays)
        .bind()
        .await
        .expect("the endpoint binds with the default trust");
    tilo_test_env::within(
        "the endpoint at the default trust to get the store's relay as home relay",
        endpoint.online(),
    )
    .await;
    assert_eq!(endpoint.home_relay_status().get().len(), 1);
    endpoint.close().await;
}
