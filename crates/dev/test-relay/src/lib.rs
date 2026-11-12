//! A relay host on this machine's loopback, and device keys no other run shares.
//!
//! A test that lets a session take part as somebody used to name the person's relays, which are
//! number 0's out on the internet: every run of the test reached them, and two runs met there.
//! [`loopback`] is the relay such a test uses instead — one packet relay for the process, on
//! `127.0.0.1` — and [`Host`] is a person's host, governing one workspace, that a test needs the
//! rest of the way (an invitation, its address, the workspace it hosts). Both mint their keys
//! fresh: a key one run shared with another running beside it would make the two one Iroh node,
//! each answering the other's dials, so [`signing_key`] mixes the run (and the test) into every
//! key it hands out.
//!
//! A host runs on its own thread with its own runtime, so a test may start one from inside a
//! runtime or beside one, and the relay's work keeps running beside the test's.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test support: a fixture that cannot start is the test's failure, and the host's relay is kept alive until the test ends"
)]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use ed25519_dalek::SigningKey;
use tilo_endpoint_reachability::EndpointRelays;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_join::MemberRole;
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
    workspaces: tilo_relay::workspaces::Workspaces,
    workspace: Option<String>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _state: tempfile::TempDir,
    cert: DeviceCert,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The host's relay is shut down before its directory goes away: a relay still writing
        // while its directory is removed is a failure the next test would read as its own.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Running {
    /// A host of `owner`'s, a device of its own, on its own thread: serving a packet relay on
    /// loopback and — with `workspace` — governing it under that name.
    fn of(owner: &SigningKey, name: &str, workspace: Option<&str>) -> Self {
        let state = tempfile::tempdir().expect("the host's directory");
        let host_root = state.path().to_path_buf();
        let host_key = fresh_signing_key(name);
        let user_public_key = owner.verifying_key().to_bytes();
        let cert = certified(
            owner,
            &host_key,
            name,
            DeviceCapabilities {
                can_own_workspaces: false,
                ..tilo_user_identity::cloud_enrollment::host_capabilities()
            },
        );
        let (ready, started) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(format!("test relay: {name}"))
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
                        let relay = tilo_relay::daemon::Relay::start(
                            tilo_relay::RelayConfig {
                                state_root: host_root.join("host"),
                                identity_root: host_root.join("identity"),
                                full_replica_status: host_root.join("full-replica/status.json"),
                                packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
                                ..tilo_relay::RelayConfig::default()
                            },
                            tilo_relay::daemon::RelayIdentity {
                                user_public_key,
                                device_signing_key: host_key.clone(),
                                device_cert: cert.clone(),
                                revocations: tilo_user_identity::RevocationList::new(),
                            },
                        )
                        .await
                        .expect("the host starts");
                        tilo_test_env::within(
                            "the host's packet relay is up",
                            relay.endpoint.online(),
                        )
                        .await;
                        let url = relay
                            .node_addr()
                            .relay_urls()
                            .next()
                            .expect("the host names its packet relay")
                            .to_string();
                        let relays =
                            EndpointRelays::for_choice(&settings_store::relay::Relays::Own(url));
                        let workspaces = relay
                            .workspaces()
                            .expect("the host governs workspaces")
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
                        let (stop, stopped) = tokio::sync::oneshot::channel();
                        ready
                            .send((handle, relays, workspaces, workspace, stop))
                            .expect("the host is awaited");
                        let _ = stopped.await;
                        relay.shutdown().await;
                    });
                }
            })
            .expect("the host's thread starts");
        let (runtime, relays, workspaces, workspace, stop) =
            started.recv().expect("the host starts");
        Self {
            runtime,
            relays,
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

/// A relay on this machine's loopback, for a session that only needs its relays to be somewhere
/// this machine is.
pub struct Loopback {
    running: Running,
}

impl Loopback {
    /// A host serving a packet relay on loopback, governing no workspace.
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

/// Make the profile at `config_home` relay through this machine's loopback relay: what a session
/// started on that profile reads as its device's relays, instead of the person's own out on the
/// internet.
pub fn relay_through_loopback_in(config_home: &std::path::Path) {
    use settings_store::SettingsSource as _;

    let profile = config_home.join("tilo");
    std::fs::create_dir_all(&profile).expect("the profile's directory");
    let store = settings_store::Store::open(&profile).expect("the profile's settings");
    settings_store::relay::RELAY
        .set_in(
            store.as_ref(),
            settings_store::relay::Relays::Own(loopback_url()),
        )
        .expect("the device's relay setting");
    // Writes are saved by a writer of its own, and a store dropped before it runs loses them:
    // the session that starts on this profile reads the setting, so it is on disk before this
    // returns.
    store.flush_pending();
    drop(store);
}

/// An endpoint of `key`'s relaying through this machine's loopback relay: a device that dials, or
/// is dialled, as a device outside a test dials through its person's relays.
pub async fn endpoint(key: iroh::SecretKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(relay_mode())
        .secret_key(key)
        .bind()
        .await
        .expect("an endpoint on this machine's loopback")
}

/// An endpoint with no relay and no directory at all, on a key of its own: a test that dials an
/// address this machine holds relayed and published nothing.
pub async fn direct_endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .expect("an endpoint on this machine")
}

/// A person's host on this machine's loopback, governing one workspace: what its devices relay
/// through, and where they are told it is.
pub struct Host(Running);

impl Host {
    /// A host of the person `owner`, governing the workspace `name`.
    #[must_use]
    pub fn of(owner: &SigningKey, name: &str) -> Self {
        Self(Running::of(owner, "The test host", Some(name)))
    }

    /// What a device of this machine's relays through this host.
    #[must_use]
    pub fn relays(&self) -> EndpointRelays {
        self.0.relays()
    }

    /// The workspace this host governs.
    #[must_use]
    pub fn workspace(&self) -> &str {
        self.0
            .workspace
            .as_deref()
            .expect("a host started with a workspace")
    }

    /// An invitation to the workspace that only `person`'s devices redeem.
    #[must_use]
    pub fn invitation(&self, person: [u8; 32], request: u8) -> String {
        let workspace = self.workspace();
        let issued = self
            .0
            .runtime
            .block_on(self.0.workspaces.issue(
                workspace,
                &self.0.cert,
                IssueInvitationRequest {
                    request_id: [request; 32],
                    role: MemberRole::Editor,
                    recipient_user_public_key: Some(person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: None,
                },
            ))
            .expect("an invitation to the workspace");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }

    /// Where this host is reached for its workspace, as it signed it: what a device of the person
    /// is told to dial.
    #[must_use]
    pub fn host_hint(&self) -> tilo_workspace_join::TransportHint {
        let workspace = self.workspace();
        self.0
            .runtime
            .block_on(self.0.workspaces.hosts(workspace))
            .expect("the host signs its own address")
            .into_iter()
            .next()
            .expect("the host serves its workspace")
    }
}

#[cfg(test)]
mod tests {
    use super::{loopback_url, relay_through_loopback_in};

    /// A profile seeded with this machine's relay reads it back as its device's relays: what a
    /// session started on that profile binds with.
    #[test]
    fn a_seeded_profile_relays_through_this_machines_loopback() {
        let config = tempfile::tempdir().expect("a configuration directory");
        relay_through_loopback_in(config.path());
        let store = settings_store::Store::follow(&config.path().join("tilo"))
            .expect("the profile's settings");
        assert_eq!(
            settings_store::relay::RELAY.get_in(store.as_ref()),
            settings_store::relay::Relays::Own(loopback_url())
        );
    }
}
