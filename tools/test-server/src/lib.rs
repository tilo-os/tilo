//! A relay host on this machine's loopback, and device keys no other run shares, so tests that let
//! a session take part never reach the person's relays on the internet nor meet another run there.
//! [`loopback`] is the relay such a test uses: one packet relay for the process on `127.0.0.1`.
//! [`Host`] is a person's host serving one workspace, for the rest of a journey (an invitation,
//! its address, the workspace it hosts). Both mint their keys fresh, because a key shared with a
//! run beside it would make the two one Iroh node answering each other's dials, so [`signing_key`]
//! mixes the run and the test into every key it hands out.
//!
//! A host runs on its own thread with its own runtime, so a test may start one inside a runtime or
//! beside one. The relay also answers QUIC address discovery, so a test's endpoints pick their
//! relay at once instead of waiting out a probe nothing answers.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test support: a fixture that cannot start is the test's failure, and the host's server is kept alive until the test ends"
)]

#[cfg(feature = "apps")]
pub mod apps;
#[cfg(feature = "apps")]
mod daemon;

#[cfg(feature = "apps")]
pub use daemon::ServerDaemon;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, OnceLock, mpsc};

use ed25519_dalek::SigningKey;
use tilo_endpoint_reachability::EndpointRelays;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_join::Role;
use tilo_workspace_join::invitations::IssueInvitationRequest;

/// This run's key material: the same seed gives the same key all run long, as a test's
/// expectations name the devices it started.
fn run_nonce() -> [u8; 32] {
    static NONCE: OnceLock<[u8; 32]> = OnceLock::new();
    *NONCE.get_or_init(|| {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).expect("this machine draws key material");
        bytes
    })
}

/// This test's key material, on top of the run's: tests run side by side on their own threads, and
/// two of them naming the same seed must not be one node.
fn test_nonce() -> [u8; 32] {
    std::thread_local! {
        static NONCE: [u8; 32] = {
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).expect("this machine draws key material");
            bytes
        };
    }
    NONCE.with(|nonce| *nonce)
}

fn derive(tag: &str, material: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(tag.len() + 1 + material.len());
    input.extend_from_slice(tag.as_bytes());
    input.push(0);
    input.extend_from_slice(material);
    blake3::derive_key("a test's key", &input)
}

/// The key the device `seed` names in this test, as a signing key.
#[must_use]
pub fn signing_key(seed: u8) -> SigningKey {
    let mut material = Vec::with_capacity(65);
    material.extend_from_slice(&run_nonce());
    material.extend_from_slice(&test_nonce());
    material.push(seed);
    SigningKey::from_bytes(&derive("a test's device", &material))
}

/// The key the device `seed` names in this test, as the endpoint it is reached on.
#[must_use]
pub fn endpoint_key(seed: u8) -> iroh::SecretKey {
    iroh::SecretKey::from_bytes(&signing_key(seed).to_bytes())
}

/// A key no other call hands out, named by `tag`: for hosts and devices a test starts in numbers
/// and never names by a seed.
#[must_use]
pub fn fresh_signing_key(tag: &str) -> SigningKey {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut material = Vec::with_capacity(64);
    material.extend_from_slice(&run_nonce());
    material.extend_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    SigningKey::from_bytes(&derive(tag, &material))
}

/// `key`'s certificate as a device of the person whose root key is `root`.
#[must_use]
pub fn certified(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// A relay host on this machine's loopback: a packet relay nothing out on the internet is asked
/// for, and the person's own relays for a device that relays through it.
struct Running {
    runtime: tokio::runtime::Handle,
    relays: EndpointRelays,
    /// Where the host answers, as its devices are told to dial it.
    addr: iroh::EndpointAddr,
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    workspace: Option<String>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _state: tempfile::TempDir,
    cert: DeviceCert,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

impl Running {
    /// Stop serving: the host's server is shut down before its directory goes away.
    fn halt(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The host's server is shut down before its directory goes away: a server still writing
        // while its directory is removed is a failure the next test would read as its own.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// A host of `owner`'s, a device of its own, on its own thread: serving a packet relay on
    /// loopback and — with `workspace` — serving it under that name.
    fn of(owner: &SigningKey, name: &str, workspace: Option<&str>) -> Self {
        let state = tempfile::tempdir().expect("the host's directory");
        let host_root = state.path().to_path_buf();
        let host_key = fresh_signing_key(name);
        let user_public_key = owner.verifying_key().to_bytes();
        let cert = certified(owner, &host_key, name, DeviceCapabilities::member());
        let (ready, started) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(format!("test server: {name}"))
            .spawn({
                let cert = cert.clone();
                let workspace = workspace.map(str::to_owned);
                move || {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .expect("the host's runtime");
                    let handle = runtime.handle().clone();
                    runtime.block_on(async move {
                        let server = tilo_runtime::device::keep::server::KeptServer::start(
                            tilo_server_protocol::config::ServerConfig {
                                packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
                                ..tilo_server_protocol::config::ServerConfig::default()
                            }
                            .on_loopback(),
                            host_root.join("host"),
                            tilo_runtime::device::keep::server::ServerIdentity {
                                user_public_key,
                                device_signing_key: host_key.clone(),
                                device_cert: cert.clone(),
                                revocations: tilo_user_identity::RevocationList::new(),
                            },
                        )
                        .await
                        .expect("the host starts");
                        let url = server
                            .node_addr()
                            .relay_urls()
                            .next()
                            .expect("the host names its packet relay")
                            .to_string();
                        let relays =
                            EndpointRelays::for_choice(&settings_store::relay::Relays::Own(url));
                        let workspaces = server
                            .workspaces()
                            .expect("the host serves workspaces")
                            .clone();
                        let workspace = match workspace {
                            Some(name) => Some(
                                workspaces
                                    .create(&name)
                                    .await
                                    .expect("the workspace")
                                    .workspace,
                            ),
                            None => None,
                        };
                        let addr = server.node_addr();
                        let (stop, stopped) = tokio::sync::oneshot::channel();
                        ready
                            .send((handle, relays, addr, workspaces, workspace, stop))
                            .expect("the host is awaited");
                        let _ = stopped.await;
                        server.shutdown().await;
                    });
                }
            })
            .expect("the host's thread starts");
        let (runtime, relays, addr, workspaces, workspace, stop) =
            started.recv().expect("the host starts");
        Self {
            runtime,
            relays,
            addr,
            workspaces,
            workspace,
            stop: Some(stop),
            thread: Some(thread),
            _state: state,
            cert,
        }
    }

    fn relays(&self) -> EndpointRelays {
        self.relays.clone()
    }
}

/// A server on this machine's loopback, for a session that only needs its relays to be somewhere
/// this machine is.
pub struct Loopback {
    running: Running,
}

impl Loopback {
    /// A host serving a packet relay on loopback, serving no workspace.
    #[must_use]
    pub fn start() -> Self {
        Self {
            running: Running::of(
                &fresh_signing_key("the loopback relay's person"),
                "The loopback relay",
                None,
            ),
        }
    }

    /// What a device of this machine's relays through it.
    #[must_use]
    pub fn relays(&self) -> EndpointRelays {
        self.running.relays()
    }
}

/// The relays this process's tests use: one packet relay on this machine's loopback, started the
/// first time a test needs it.
#[must_use]
pub fn loopback() -> EndpointRelays {
    static LOOPBACK: OnceLock<Loopback> = OnceLock::new();
    LOOPBACK.get_or_init(Loopback::start).relays()
}

/// [`loopback`] as the mode a host binds its endpoint with: a relay under test relays through the
/// same loopback packet relay its devices do, so neither side asks the internet for anything.
#[must_use]
pub fn relay_mode() -> iroh::RelayMode {
    iroh::RelayMode::Custom(loopback().relays)
}

/// Where this machine's loopback relay is reached, as a relay setting names it.
#[must_use]
pub fn loopback_url() -> String {
    loopback()
        .relays
        .urls::<Vec<_>>()
        .into_iter()
        .next()
        .expect("the loopback host serves its packet relay")
        .to_string()
}

/// What a device started for a test is given in its environment to relay through this machine's
/// loopback relay, instead of the person's own out on the internet: the variable and its value.
#[must_use]
pub fn loopback_relay() -> (&'static str, String) {
    (settings_store::relay::ENV, loopback_url())
}

/// `cfg` for a host a test keeps in its own process: on loopback, reached through this machine's
/// loopback relay as a server daemon started for a test is, so it asks the internet for nothing.
#[must_use]
pub fn on_loopback_relay(
    cfg: tilo_server_protocol::config::ServerConfig,
) -> tilo_server_protocol::config::ServerConfig {
    tilo_server_protocol::config::ServerConfig {
        packet_relay_url: Some(loopback_url()),
        ..cfg.on_loopback()
    }
}

/// The trust this crate's endpoints bind with: the system's, and the certificate the address
/// discovery service on this machine presents.
fn discovery_trust() -> iroh::tls::CaTlsConfig {
    static DISCOVERY: OnceLock<iroh::tls::CaTlsConfig> = OnceLock::new();
    DISCOVERY.get_or_init(serve_discovery).clone()
}

/// Serve QUIC address discovery on this machine's loopback, on the port iroh's endpoints ask every
/// relay for it, and answer what a device of this machine trusts to reach it.
///
/// An endpoint asks each relay where that relay sees it from, and takes the relay that answers as
/// its home — the relay its address then names. A relay that answers nothing leaves the endpoint
/// waiting out the probe (three seconds) before it settles on any relay, and a host's own address,
/// which it signs only once it is reached through one, waits with it.
///
/// One service answers a machine: a port another process already answers is left to it, since what
/// a service answers is where it sees the endpoint from, whoever started it.
fn serve_discovery() -> iroh::tls::CaTlsConfig {
    let (certificates, server_config) =
        iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let mut quic = iroh_relay::server::QuicConfig::new(SocketAddr::from((
        Ipv4Addr::LOCALHOST,
        iroh_relay::defaults::DEFAULT_RELAY_QUIC_PORT,
    )));
    quic.server_config = Some(server_config);
    let mut config = iroh_relay::server::ServerConfig::default();
    config.quic = Some(quic);

    let (ready, started) = mpsc::channel();
    std::thread::Builder::new()
        .name("test server: address discovery".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("the discovery service's runtime");
            runtime.block_on(async move {
                let Ok(serving) = iroh_relay::server::Server::spawn(config).await else {
                    ready.send(false).expect("the service is awaited");
                    return;
                };
                ready.send(true).expect("the service is awaited");
                // The service is this machine's until the process ends: dropping it closes it.
                let _serving = serving;
                std::future::pending::<()>().await;
            });
        })
        .expect("the discovery service's thread starts");
    // The service listens before any endpoint asks it anything.
    started.recv().expect("the service is awaited");
    iroh::tls::CaTlsConfig::custom_server_cert_verifier(Arc::new(move |_provider| {
        tilo_net::server_verifier_also_trusting(certificates.clone()).map_err(std::io::Error::other)
    }))
}

/// An endpoint of `key`'s relaying through this machine's loopback relay: a device that dials, or
/// is dialled, as a device outside a test dials through its person's relays.
pub async fn endpoint(key: iroh::SecretKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(relay_mode())
        .ca_tls_config(discovery_trust())
        .secret_key(key)
        .bind()
        .await
        .expect("an endpoint on this machine's loopback")
}

/// A host that does not answer ([`Host::pause`]) until this drops.
pub struct Paused(Arc<Barrier>);

impl Drop for Paused {
    fn drop(&mut self) {
        self.0.wait();
    }
}

/// A person's host on this machine's loopback, serving one workspace: what its devices relay
/// through, and where they are told it is.
pub struct Host(Running);

impl Host {
    /// A host of the person `owner`, serving the workspace `name`.
    #[must_use]
    pub fn of(owner: &SigningKey, name: &str) -> Self {
        Self(Running::of(owner, "The test host", Some(name)))
    }

    /// A site of the person `owner`, whose home is the workspace `name`: a host that keeps the
    /// workspaces of whoever its home lets in, and lists them theirs, as a community site does.
    #[must_use]
    pub fn site(owner: &SigningKey, name: &str) -> Self {
        let host = Self::of(owner, name);
        assert!(
            host.0
                .workspaces
                .offer_own_workspaces(tilo_workspace_service::workspaces::OwnWorkspaceOffer::free(
                    "site.test"
                ))
                .expect("the site keeps workspaces of people's own")
        );
        assert!(host.0.workspaces.admit_own_through(host.workspace()));
        host
    }

    /// What a device of this machine's relays through this host.
    #[must_use]
    pub fn relays(&self) -> EndpointRelays {
        self.0.relays()
    }

    /// Where this host answers, for a device told to dial it.
    #[must_use]
    pub fn addr(&self) -> iroh::EndpointAddr {
        self.0.addr.clone()
    }

    /// The workspaces this host serves, for a test that hands one of them a package as
    /// a developer does, without a device's session.
    #[must_use]
    pub fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        &self.0.workspaces
    }

    /// The workspace this host serves.
    #[must_use]
    pub fn workspace(&self) -> &str {
        self.0
            .workspace
            .as_deref()
            .expect("a host started with a workspace")
    }

    /// Govern a further workspace named `name`, as the host's own: its id.
    #[must_use]
    pub fn create(&self, name: &str) -> String {
        self.0
            .runtime
            .block_on(self.0.workspaces.create(name))
            .expect("the workspace")
            .workspace
    }

    /// An invitation to the workspace that only `person`'s devices redeem.
    #[must_use]
    pub fn invitation(&self, person: [u8; 32], request: u8) -> String {
        self.invitation_to(self.workspace(), person, request)
    }

    /// An invitation to `workspace`, one this host serves, that only `person`'s devices redeem.
    #[must_use]
    pub fn invitation_to(&self, workspace: &str, person: [u8; 32], request: u8) -> String {
        let issued = self
            .0
            .runtime
            .block_on(self.0.workspaces.issue(
                workspace,
                &self.0.cert,
                IssueInvitationRequest {
                    request_id: [request; 32],
                    role: Role::Editor,
                    recipient_user_public_key: Some(person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: None,
                },
            ))
            .expect("an invitation to the workspace");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }

    /// Take `person` and every device of theirs out of `workspace`, one this host serves, as its
    /// administrator does.
    pub fn remove(&self, workspace: &str, person: [u8; 32], request: u8) {
        self.0
            .runtime
            .block_on(self.0.workspaces.change(
                workspace,
                (&self.0.cert, [request; 32]),
                tilo_action_policy::Change::RemoveMember {
                    person: hex::encode(person),
                },
            ))
            .expect("the host takes the person out");
    }

    /// Stop this host, as a machine that goes away does: nothing answers at its address.
    pub fn stop(&mut self) {
        self.0.halt();
    }

    /// Stop answering until what this returns drops, as a host that hangs or a network that loses
    /// its packets does: what reaches the host's address waits unanswered rather than refused.
    /// Each of its runtime's two workers is held, so nothing it serves runs meanwhile.
    #[must_use]
    pub fn pause(&self) -> Paused {
        let (held, resume) = (Arc::new(Barrier::new(3)), Arc::new(Barrier::new(3)));
        for _ in 0..2 {
            let (held, resume) = (Arc::clone(&held), Arc::clone(&resume));
            self.0.runtime.spawn(async move {
                held.wait();
                resume.wait();
            });
        }
        held.wait();
        Paused(resume)
    }

    /// Where this host is reached for its workspace, as it signed it: what a device of the person
    /// is told to dial.
    #[must_use]
    pub fn host_hint(&self) -> tilo_workspace_join::TransportHint {
        let workspace = self.workspace();
        let own = self.0.cert.device_public_key;
        self.0
            .runtime
            .block_on(self.0.workspaces.hosts(workspace))
            .expect("the host signs its own address")
            .into_iter()
            .find(|hint| hint.host.device_public_key == own)
            .expect("the host serves its workspace")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{endpoint, endpoint_key, loopback_url};

    /// An endpoint picks this machine's relay as its home at once, because the address discovery
    /// service answers where the relay sees it from: without one it waits out the probe first.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_endpoint_picks_this_machines_relay_at_once() {
        let endpoint = endpoint(endpoint_key(9)).await;
        let addr = tilo_endpoint_reachability::relay_addr(&endpoint, Duration::from_secs(1)).await;
        assert_eq!(
            addr.relay_urls().next().map(ToString::to_string),
            Some(loopback_url()),
            "the endpoint names this machine's relay as its home"
        );
        endpoint.close().await;
    }
}
