//! LAN/OOB completion for a pre-authorised Tilo device enrollment token.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;
use tilo_relay::root_signer::{RootSigner, load_root as load_approval_root};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tilo_clock::try_unix_now_ns;
use tilo_identity_host::certification_ceremony::pairing_code;
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::enrollment::{Certifier, EnrollmentRole, NonceStore, Redemption};
use tilo_relay::HomePeerRegistration;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationList, decode_enrollment_token,
    encode_enrollment_token, mint_enrollment_token, verify_device_cert, verify_device_cert_at,
    verify_enrollment_token, verify_revocation_entry,
};

/// Well-known UDP port every nearby-pairing listener binds and every
/// soliciting device broadcasts to. Broadcast discovery has no channel to
/// advertise a port before the first datagram, so the rendezvous must be
/// fixed. It stays below Linux's ephemeral range (32768-60999) so an outgoing
/// socket can never be auto-assigned it and block a listener, and inside the
/// 1025-65535 range the desktop firewall zone accepts. IANA leaves it
/// unassigned and no common service uses it.
const NEARBY_PORT: u16 = 23_831;
const MAX_PACKET: usize = 60 * 1024;
const RESPONSE_DOMAIN: &[u8] = b"tilo-host-enrollment-response-v1\0";
const PAIR_DOMAIN: &[u8] = b"tilo-nearby-home-peer-offer-v1\0";
/// Solicitation magic: the device reads HPKE pair offers.
const PAIR_MAGIC: [u8; 8] = *b"TLOPAIR2";

#[derive(Parser)]
#[command(name = "tilo-enroll")]
struct Cli {
    /// The account, by uid, whose identity directory (`--identity-dir`) a
    /// root-run enrollment writes for. Without it the directory must belong
    /// to the account this runs as.
    #[arg(long, global = true)]
    person_uid: Option<u32>,
    #[command(subcommand)]
    command: Command,
}

/// `--person-uid`, set once before any command runs.
static PERSON_UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// Write into the person's identity directory as the person: never as root
/// for another account, and never through a directory swapped for a symlink
/// ([`tilo_identity_host::person_files::as_person`]).
fn as_person<T>(
    identity_dir: &std::path::Path,
    write: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    tilo_identity_host::person_files::as_person(PERSON_UID.get().copied(), identity_dir, write)
}

#[derive(Subcommand)]
enum Command {
    /// Find one unprovisioned home server broadcasting on this LAN.
    Discover {
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
    /// Find one unprovisioned personal Tilo device broadcasting on this LAN.
    DiscoverDevice {
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
    /// Join the identity named by TOKEN, discovering its approving device on this LAN.
    Join {
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/tilo/home-peer.toml")]
        home_peer_output: PathBuf,
    },
    /// Pair with a nearby trusted Tilo device without pre-installing a token.
    Pair {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/tilo/home-peer.toml")]
        home_peer_output: PathBuf,
    },
    /// Join an existing identity as an ordinary personal device.
    PairDevice {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long, default_value = "Tilo device")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        /// Optional world-readable progress file for the first-run UI.
        #[arg(long)]
        status_file: Option<PathBuf>,
        /// Agent hosts and other headless boxes: pair without a compositor
        /// first-use requester lease. Desktop first-use must omit this flag.
        #[arg(long, default_value_t = false)]
        headless: bool,
        /// PID of the compositor that initiated this first-use operation.
        #[arg(long)]
        requester_pid: Option<u32>,
        /// `/proc/<pid>/stat` start-time tick, preventing PID-reuse bypass.
        #[arg(long)]
        requester_start_time: Option<u64>,
    },
    /// Approve one nearby host token issued by this device.
    Approve {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        /// Existing person record, opened through its ordinary secret provider.
        #[arg(long)]
        identity_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long, default_value = "/var/cache/quick-fs")]
        workspace_cache: PathBuf,
        #[arg(long, default_value = "/var/lib/tilo/fs-capabilities.json")]
        acl: PathBuf,
        #[arg(long = "relay-workspace-id")]
        relay_workspace_ids: Vec<u64>,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
    },
    /// Approve one nearby ordinary device and give it the current fleet and
    /// home-peer registry needed to reconnect after this device goes offline.
    ApproveDevice {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
        /// The expected device's certificate, canonically encoded, already
        /// signed by the person's devices that hold the identity's key, each
        /// after asking the person. This device then needs none of the key,
        /// nor the right to admit devices: it only introduces the device.
        #[arg(long, requires = "expected_device")]
        approved_cert: Option<PathBuf>,
    },
    #[command(flatten)]
    OutOfBand(OutOfBand),
}

/// Certification delivered outside a pairing session.
#[derive(Subcommand)]
enum OutOfBand {
    /// Create this device's key if it has none and print, as one JSON line,
    /// its public key and the endpoint id of a relay it runs.
    DeviceKey {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
    },
    /// Certify a device's key with a root key kept in a file, for delivery
    /// out of band. Writes the canonical certificate to OUT.
    /// Without --root-key the root this device holds signs, as when it
    /// approves a nearby device: IDENTITY_ROOT/user-identity/privkey.bin, or
    /// the active identity record in --identity-dir (run as the account that
    /// owns it, in a session that reaches its secret store).
    Certify {
        /// The user root signing key (32 bytes, owner-only).
        #[arg(long)]
        root_key: Option<PathBuf>,
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        /// The person's identity directory: its active record signs when no
        /// root key file does, and its device fleet lists the new device, so
        /// Settings shows it and can remove it.
        #[arg(long)]
        identity_dir: Option<PathBuf>,
        /// The device's public key, hex.
        #[arg(long)]
        device: String,
        #[arg(long, default_value = "Tilo device")]
        label: String,
        /// own, admit, host, read-all or admin (administer the person's
        /// other devices over their device-admin route); repeatable.
        #[arg(long = "capability", value_name = "CAPABILITY")]
        capabilities: Vec<String>,
        /// Also record the certified host as one of the person's home peers
        /// in the identity root's home-peer registry, so this device shares
        /// workspaces on it: the endpoint id of the relay it runs, hex, as
        /// `device-key` prints it. Needs the `host` capability.
        #[arg(long, value_name = "HEX")]
        home_peer_service: Option<String>,
        /// A direct address that relay listens on, kept as its dial hint;
        /// repeatable. Without one the relay is found by its endpoint id.
        #[arg(
            long = "home-peer-addr",
            value_name = "ADDR",
            requires = "home_peer_service"
        )]
        home_peer_addrs: Vec<std::net::SocketAddr>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Join the identity that certified this device's key without a pairing
    /// session: install a root-signed certificate delivered out of band.
    InstallCert {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        /// The certificate, canonically encoded.
        #[arg(long)]
        cert: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum EnrollmentKind {
    HomePeer,
    PersonalDevice,
}

impl EnrollmentKind {
    /// The role a token for this kind admits, with its one capability preset
    /// and shape check.
    const fn role(self) -> EnrollmentRole {
        match self {
            Self::HomePeer => EnrollmentRole::HomePeer,
            Self::PersonalDevice => EnrollmentRole::PersonalDevice,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    label: String,
    kind: EnrollmentKind,
}

/// Plaintext of a v2 request: the request plus the key its response is
/// sealed to (only the requester holds the matching secret).
#[derive(Serialize)]
struct RequestV2<'a> {
    request: &'a Request,
    response_public_key: [u8; 32],
}

#[derive(Deserialize)]
struct RequestV2Owned {
    request: Request,
    response_public_key: [u8; 32],
}

/// Where an enrollment response is sealed to: the response key inside the
/// requester's sealed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResponseRoute {
    response_public_key: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct ResponsePayload {
    nonce: [u8; 16],
    recipient_device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    cert: DeviceCert,
    home_peer_toml: Option<String>,
    /// A public-only runtime identity record. It deliberately contains no
    /// root signing share; collaboration is authenticated by the separately
    /// certified device key installed in `identity_root`.
    public_identity_json: Option<Vec<u8>>,
    /// Complete bounded fleet projection after this device was admitted.
    fleet_json: Option<Vec<u8>>,
    /// Current verified home-peer registry, so continuity does not depend on
    /// the approving laptop remaining online after setup.
    home_peers_json: Option<Vec<u8>>,
}

/// The approving device's answer when it holds the identity's key: signed by
/// that key, the answer every build understands.
#[derive(Serialize, Deserialize)]
struct Response {
    payload: ResponsePayload,
    signature: Vec<u8>,
}

/// The approving device's answer when it holds none of the identity's key and
/// introduces a device the person's key holders certified: signed by its own
/// certified device key, the device the token named as its issuer.
#[derive(Serialize, Deserialize)]
struct IntroducedResponse {
    payload: ResponsePayload,
    issuer: DeviceCert,
    signature: Vec<u8>,
}

/// What signs the approving side of a pairing.
#[derive(Clone, Copy)]
enum Approver<'a> {
    /// The identity's key, held here.
    Root(&'a dyn RootSigner),
    /// This device's certified key, introducing a device the person's key
    /// holders certified.
    Introducer {
        device: &'a ed25519_dalek::SigningKey,
        issuer: &'a DeviceCert,
    },
}

impl Approver<'_> {
    fn sign(&self, message: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(match self {
            Self::Root(root) => root.sign_message(message)?.to_vec(),
            Self::Introducer { device, .. } => device.sign(message).to_bytes().to_vec(),
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PairSolicitation {
    magic: [u8; 8],
    device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    label: String,
    kind: EnrollmentKind,
}

#[derive(Serialize, Deserialize)]
struct PairCandidate {
    device_node_id: String,
    relay_service_node_id: String,
    label: String,
    verification_code: String,
    kind: EnrollmentKind,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetProjection {
    schema_version: u32,
    identity_id: String,
    certificates: Vec<DeviceCert>,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
}

/// The files of a personal enrollment live in a private, durable staging
/// directory until the approving device has sent `Finalized`.  A marker is
/// deliberately retained while the staged files are being published: a boot
/// or retry can finish the exact same transaction instead of guessing which
/// of a set of independently-atomic renames reached disk.
#[derive(Clone, Serialize, Deserialize)]
struct PersonalEnrollmentMarker {
    schema_version: u32,
    identity_dir: PathBuf,
    stage_name: String,
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    /// The exact compositor lifetime that received and confirmed this
    /// enrollment. Before `commit_ready`, no other requester may authorize it.
    requester_lease: Option<tilo_relay::ProcessLease>,
    /// Set only after the recipient authenticates the approver's Finalized
    /// packet. Prepared-but-unconfirmed transactions must never activate on
    /// a later timeout retry.
    commit_ready: bool,
}

const PERSONAL_ENROLLMENT_SCHEMA: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalCommitCheckpoint {
    Record,
    Fleet,
    HomePeers,
    Registry,
    UserPublicKey,
    PerUserCert,
    PersonalPeers,
    DefaultCert,
}

fn enrollment_commit_dir(identity_root: &std::path::Path) -> PathBuf {
    identity_root.join("enrollment-commits")
}

fn enrollment_stage_dir(identity_root: &std::path::Path) -> PathBuf {
    identity_root.join("enrollment-staging")
}

fn personal_marker_path(identity_root: &std::path::Path, stage_name: &str) -> PathBuf {
    enrollment_commit_dir(identity_root).join(format!("{stage_name}.json"))
}

fn random_stage_name() -> String {
    let mut token = [0_u8; 16];
    UnwrapErr(SysRng).fill_bytes(&mut token);
    format!("personal-{}", hex::encode(token))
}

/// Create an enrollment's staging directory in the shared staging area,
/// which every member of the device-trust group may write: under a name
/// from `name` that nothing holds yet, as this account's private 0700
/// directory. `mkdir` follows no symlink and fails on any name already
/// taken, by a planted link or anything else; a taken name is skipped for
/// the next one `name` gives.
fn create_stage_dir(
    identity_root: &std::path::Path,
    mut name: impl FnMut() -> String,
) -> anyhow::Result<(String, PathBuf)> {
    let parent = enrollment_stage_dir(identity_root);
    std::fs::create_dir_all(&parent)?;
    for _ in 0..8 {
        let stage_name = name();
        let stage_dir = parent.join(&stage_name);
        let mut directory = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        match directory.create(&stage_dir) {
            Ok(()) => return Ok((stage_name, stage_dir)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no free enrollment staging name")
}

/// The staging directory a marker names, if it is still the private
/// directory this account created: not a symlink, owned by this account and
/// closed to everyone else.
fn private_stage_dir(identity_root: &std::path::Path, stage_name: &str) -> anyhow::Result<PathBuf> {
    if stage_name.is_empty()
        || stage_name == ".."
        || stage_name.contains('/')
        || stage_name.contains('\\')
    {
        bail!("personal enrollment marker is invalid");
    }
    let stage_dir = enrollment_stage_dir(identity_root).join(stage_name);
    let metadata = std::fs::symlink_metadata(&stage_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            bail!("personal enrollment staging is not this account's private directory");
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_dir() {
        bail!("personal enrollment staging is not a directory");
    }
    Ok(stage_dir)
}

fn staged(path: &std::path::Path, name: &str) -> PathBuf {
    path.join(name)
}

fn fleet_path(identity_dir: &std::path::Path, user: [u8; 32]) -> PathBuf {
    identity_dir
        .join("fleets")
        .join(format!("{}.json", hex::encode(user)))
}

/// The person's device fleet: the canonical record of their devices and
/// revocations, kept in their identity directory.
fn fleet_store(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
) -> DeviceFleetStore {
    let store = DeviceFleetStore::at(identity_dir, IdentityPaths::under(identity_root));
    match PERSON_UID.get() {
        Some(&uid) => store.for_person(uid),
        None => store,
    }
}

fn fleet_revokes_device(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    user: [u8; 32],
    device: [u8; 32],
) -> anyhow::Result<bool> {
    Ok(fleet_store(identity_root, identity_dir)
        .load(&hex::encode(user))
        .context("device fleet is invalid")?
        .is_revoked(&device))
}

/// The devices a joining device records as its personal peers: itself, and
/// every personal device the approver's fleet certifies under this identity's
/// root, valid now and not revoked. A device the fleet merely names, without
/// a certificate that verifies under `user`, is never included.
fn certified_personal_devices(
    fleet: &[u8],
    user: [u8; 32],
    this_device: [u8; 32],
    now_ns: u64,
) -> anyhow::Result<Vec<[u8; 32]>> {
    if fleet.len() > 1024 * 1024 {
        bail!("staged device fleet exceeds 1 MiB");
    }
    let fleet: FleetProjection =
        serde_json::from_slice(fleet).context("staged device fleet is invalid")?;
    if fleet.schema_version != 1
        || fleet.identity_id != hex::encode(user)
        || fleet.certificates.len() > 256
        || fleet.revocations.len() > 256
    {
        bail!("staged device fleet violates its bounded schema");
    }
    let mut revocations = RevocationList::new();
    for entry in fleet.revocations {
        if entry.user_public_key != user {
            bail!("staged device fleet contains a revocation from another identity");
        }
        revocations
            .insert(entry)
            .context("staged device fleet contains an invalid revocation")?;
    }
    let mut devices = vec![this_device];
    for cert in &fleet.certificates {
        if cert.user_public_key == user
            && cert.capabilities.is_personal_device()
            && verify_device_cert_at(cert, now_ns).is_ok()
            && !revocations.is_revoked(&cert.device_public_key)
            && !devices.contains(&cert.device_public_key)
        {
            devices.push(cert.device_public_key);
        }
    }
    Ok(devices)
}

/// Record `devices` in this device's personal device list, the authorization
/// input that decides which devices remote actions may address. Repeatable:
/// a device already listed is kept once.
fn remember_personal_devices(
    identity_root: &std::path::Path,
    devices: &[[u8; 32]],
) -> anyhow::Result<PathBuf> {
    let path = tilo_relay_protocol::personal_device_peer_path(identity_root);
    for device in devices {
        tilo_relay_protocol::remember_personal_device(&path, *device)
            .context("record the paired device as a personal device")?;
    }
    Ok(path)
}

/// Record this joining device and the personal devices its staged fleet
/// certifies, once the enrollment is committing.
fn remember_staged_personal_devices(
    identity_root: &std::path::Path,
    stage_dir: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
) -> anyhow::Result<PathBuf> {
    remember_personal_devices(
        identity_root,
        &certified_personal_devices(
            &std::fs::read(staged(stage_dir, "fleet.json"))?,
            marker.user_public_key,
            marker.device_public_key,
            now_ns()?,
        )?,
    )
}

/// Give one file or directory this enrollment created the owner and group of
/// the directory it was created in.
///
/// Only a root-run enrollment (a headless host) creates output under another
/// account than its directory's; desktop first use runs as the session user,
/// whose output already belongs to them and inherits the device-trust group
/// from the setgid identity directories. Ownership always comes from the
/// immediate parent, never from the identity root: the root is `root:root`,
/// while the directories below it belong to the device-trust group that the
/// session and relay read the device identity through.
fn restore_owner_from_parent(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;

        if rustix::process::geteuid().as_raw() != 0 {
            return Ok(());
        }
        let parent = path
            .parent()
            .context("enrollment output has no parent directory")?;
        let owner = std::fs::metadata(parent)?;
        let current = std::fs::symlink_metadata(path)?;
        if current.uid() == owner.uid() && current.gid() == owner.gid() {
            return Ok(());
        }
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `path` is NUL-terminated and originates from a validated
        // identity/status path. The uid/gid come from its trusted parent.
        if unsafe { libc::lchown(path.as_ptr(), owner.uid(), owner.gid()) } != 0 {
            return Err(anyhow::Error::from(std::io::Error::last_os_error())
                .context("restore enrollment output ownership"));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Create `directory` if needed and give it its parent's permission bits
/// (including set-group-ID), so a directory an enrollment adds to the shared
/// device-trust layout is reachable the way its parent is, whatever the umask
/// this ran under. A directory this process neither owns nor could change is
/// left as it is.
fn share_directory_like_parent(directory: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let parent = directory
            .parent()
            .context("shared directory has no parent")?;
        let wanted = std::fs::metadata(parent)?.mode() & 0o7777;
        let current = std::fs::symlink_metadata(directory)?;
        if !current.is_dir() {
            bail!("{} is not a directory", directory.display());
        }
        let euid = rustix::process::geteuid().as_raw();
        if current.mode() & 0o7777 != wanted && (euid == 0 || euid == current.uid()) {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(wanted))
                .context("share the enrollment directory like its parent")?;
        }
    }
    Ok(())
}

fn project_personal_device(
    identity_dir: &std::path::Path,
    cert: &DeviceCert,
) -> anyhow::Result<Vec<u8>> {
    verify_device_cert_at(cert, now_ns()?).context("new personal-device certificate validity")?;
    let path = fleet_path(identity_dir, cert.user_public_key);
    let mut fleet = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            serde_json::from_slice::<FleetProjection>(&bytes).context("device fleet is invalid")?
        }
        Ok(_) => bail!("device fleet exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(cert.user_public_key),
            certificates: Vec::new(),
            revocations: Vec::new(),
        },
        Err(error) => return Err(error.into()),
    };
    if fleet.schema_version != 1
        || fleet.identity_id != hex::encode(cert.user_public_key)
        || fleet.certificates.len() > 256
        || fleet.revocations.len() > 256
        || fleet
            .certificates
            .iter()
            .any(|item| verify_device_cert(item).is_err())
        || fleet
            .revocations
            .iter()
            .any(|item| verify_revocation_entry(item).is_err())
    {
        bail!("device fleet violates its bounded verified schema");
    }
    fleet
        .certificates
        .retain(|item| item.device_public_key != cert.device_public_key);
    fleet.certificates.push(cert.clone());
    fleet
        .certificates
        .sort_by_key(|item| item.device_public_key);
    let bytes = serde_json::to_vec_pretty(&fleet)?;
    Ok(bytes)
}

fn commit_personal_device(
    identity_dir: &std::path::Path,
    user: [u8; 32],
    fleet: &[u8],
) -> anyhow::Result<()> {
    let path = fleet_path(identity_dir, user);
    as_person(identity_dir, || {
        tilo_files::native::atomic::write_creating_parent(&path, fleet)
    })?;
    Ok(())
}

fn bounded_home_peer_registry(
    identity_root: &std::path::Path,
    user: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let path = tilo_relay::home_peer_registry_path(identity_root);
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            let entries: Vec<HomePeerRegistration> = serde_json::from_slice(&bytes)?;
            if entries.len() > 128
                || entries.iter().any(|entry| {
                    entry.schema_version != HomePeerRegistration::CURRENT_SCHEMA
                        || !valid_relay_endpoint_hint(entry)
                        || entry.device_cert.as_ref().is_none_or(|cert| {
                            verify_device_cert(cert).is_err()
                                || hex::encode(cert.device_public_key) != entry.device_node_id
                                || hex::encode(cert.user_public_key) != entry.user_public_key
                        })
                })
            {
                bail!("home-peer registry violates its bounded verified schema");
            }
            Ok(serde_json::to_vec_pretty(
                &entries
                    .into_iter()
                    .filter(|entry| entry.user_public_key == hex::encode(user))
                    .collect::<Vec<_>>(),
            )?)
        }
        Ok(_) => bail!("home-peer registry exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(b"[]".to_vec()),
        Err(error) => Err(error.into()),
    }
}

fn valid_relay_endpoint_hint(entry: &HomePeerRegistration) -> bool {
    entry.relay_endpoint.as_ref().is_none_or(|endpoint| {
        !endpoint.is_empty()
            && hex::decode(&entry.relay_service_node_id)
                .ok()
                .is_some_and(|service| service.as_slice() == endpoint.id.as_bytes())
    })
}

fn public_identity_record(user: [u8; 32], display_name: &str) -> anyhow::Result<Vec<u8>> {
    Ok(runtime_identity::public_member_record(
        display_name,
        [88, 101, 242],
        user,
    )?)
}

struct CheckedPersonalProjection {
    identity: Vec<u8>,
    fleet: Vec<u8>,
    peers: Vec<u8>,
    registry: Vec<u8>,
}

fn identity_record_path(identity_dir: &std::path::Path, user: [u8; 32]) -> PathBuf {
    let prefix = hex::encode(user).chars().take(55).collect::<String>();
    identity_dir.join(format!("identity-{prefix}.json"))
}

fn validate_personal_fleet(
    bytes: &[u8],
    identity_id: &str,
    enrolled_device: [u8; 32],
) -> anyhow::Result<()> {
    let projected: FleetProjection = serde_json::from_slice(bytes)?;
    if projected.schema_version != 1
        || projected.identity_id != identity_id
        || projected.certificates.len() > 256
        || projected.revocations.len() > 256
        || !projected
            .certificates
            .iter()
            .any(|cert| cert.device_public_key == enrolled_device)
        || projected
            .certificates
            .iter()
            .any(|cert| verify_device_cert(cert).is_err())
        || projected
            .revocations
            .iter()
            .any(|entry| verify_revocation_entry(entry).is_err())
    {
        bail!("approval returned an invalid device fleet");
    }
    Ok(())
}

fn validate_public_identity(bytes: &[u8], user: [u8; 32]) -> anyhow::Result<()> {
    let identity: serde_json::Value = serde_json::from_slice(bytes)?;
    let threshold = identity
        .get("threshold_identity")
        .and_then(serde_json::Value::as_object)
        .context("approval returned an invalid public identity")?;
    if threshold.get("group_public_key") != Some(&serde_json::json!(user))
        || threshold
            .get("secret_bytes")
            .is_some_and(|secret| secret != &serde_json::json!([]))
        || threshold.get("single_device") != Some(&serde_json::json!(false))
    {
        bail!("approval returned an identity containing private or mismatched key material");
    }
    Ok(())
}

fn valid_home_peer_registration(entry: &HomePeerRegistration) -> bool {
    entry.schema_version == HomePeerRegistration::CURRENT_SCHEMA
        && entry.relay_workspace_ids.len() <= 4096
        && tilo_relay::valid_home_peer_label(&entry.label)
        && hex::decode(&entry.device_node_id)
            .ok()
            .is_some_and(|key| key.len() == 32)
        && hex::decode(&entry.relay_service_node_id)
            .ok()
            .is_some_and(|key| key.len() == 32)
        && hex::decode(&entry.user_public_key)
            .ok()
            .is_some_and(|key| key.len() == 32)
        && valid_relay_endpoint_hint(entry)
        && entry.device_cert.as_ref().is_some_and(|cert| {
            verify_device_cert(cert).is_ok()
                && hex::encode(cert.device_public_key) == entry.device_node_id
                && hex::encode(cert.user_public_key) == entry.user_public_key
        })
}

fn read_bounded_json<T>(path: &std::path::Path, description: &str) -> anyhow::Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => serde_json::from_slice(&bytes)
            .with_context(|| format!("{description} is invalid"))
            .map(Some),
        Ok(_) => bail!("{description} exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn checked_personal_projection(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    payload: &ResponsePayload,
) -> anyhow::Result<CheckedPersonalProjection> {
    let identity = payload
        .public_identity_json
        .as_ref()
        .context("approval omitted the public identity")?;
    let fleet = payload
        .fleet_json
        .as_ref()
        .context("approval omitted the fleet")?;
    let transferred_peers = payload
        .home_peers_json
        .as_ref()
        .context("approval omitted home peers")?;
    let user = payload.cert.user_public_key;
    let identity_id = hex::encode(user);

    validate_personal_fleet(fleet, &identity_id, payload.cert.device_public_key)?;
    validate_public_identity(identity, user)?;

    let incoming: Vec<HomePeerRegistration> = serde_json::from_slice(transferred_peers)?;
    if incoming.len() > 128
        || incoming.iter().any(|entry| {
            entry.user_public_key != identity_id || !valid_home_peer_registration(entry)
        })
    {
        bail!("approval returned an invalid home-peer registry");
    }

    let registry_path = identity_dir.join("registry.json");
    let mut registry: serde_json::Value =
        read_bounded_json(&registry_path, "existing identity registry")?.unwrap_or_else(|| {
            serde_json::json!({
                "schema_version": 1,
                "default_identity": identity_id,
                "identities": {},
                "workspace_identities": {}
            })
        });
    let valid_registry = registry.as_object().is_some_and(|root| {
        root.get("identities")
            .is_some_and(serde_json::Value::is_object)
            && root
                .get("workspace_identities")
                .is_some_and(serde_json::Value::is_object)
    });
    if !valid_registry {
        bail!("existing identity registry has an invalid schema");
    }

    let record_path = identity_record_path(identity_dir, user);
    let registry_root = registry
        .as_object_mut()
        .context("existing identity registry has an invalid schema")?;
    registry_root.insert(
        "default_identity".into(),
        serde_json::Value::String(identity_id.clone()),
    );
    registry_root
        .get_mut("identities")
        .and_then(serde_json::Value::as_object_mut)
        .context("existing identity registry has an invalid identities map")?
        .insert(
            identity_id.clone(),
            serde_json::json!({
                "id": identity_id,
                "display_name": "My Tilo identity",
                "record_path": record_path,
                "user_public_key": user
            }),
        );

    let peers_path = tilo_relay::home_peer_registry_path(identity_root);
    let mut existing: Vec<HomePeerRegistration> =
        read_bounded_json(&peers_path, "existing home-peer registry")?.unwrap_or_default();
    if existing.len() > 128
        || existing
            .iter()
            .any(|entry| !valid_home_peer_registration(entry))
    {
        bail!("existing home-peer registry violates its bounded verified schema");
    }

    // A response projects routes for one identity. Do not use it as a global
    // snapshot and erase other identities' independently verified routes.
    existing.retain(|entry| entry.user_public_key != identity_id);
    existing.extend(incoming);
    if existing.len() > 128 {
        bail!("merged home-peer registry is full");
    }
    existing.sort_by(|left, right| {
        left.user_public_key
            .cmp(&right.user_public_key)
            .then_with(|| left.device_node_id.cmp(&right.device_node_id))
    });

    Ok(CheckedPersonalProjection {
        identity: identity.clone(),
        fleet: fleet.clone(),
        peers: serde_json::to_vec_pretty(&existing)?,
        registry: serde_json::to_vec_pretty(&registry)?,
    })
}

fn stage_personal_enrollment(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    paths: &IdentityPaths,
    payload: &ResponsePayload,
    requester_lease: Option<tilo_relay::ProcessLease>,
) -> anyhow::Result<(PathBuf, PersonalEnrollmentMarker)> {
    let projection = checked_personal_projection(identity_root, identity_dir, payload)?;
    let (stage_name, stage_dir) = create_stage_dir(identity_root, random_stage_name)?;
    let cert = tilo_user_identity::canonical::canonical_bytes(&payload.cert)?;
    tilo_files::native::atomic::write_with_mode(
        &staged(&stage_dir, "record.json"),
        &projection.identity,
        0o600,
    )?;
    tilo_files::native::atomic::write_with_mode(
        &staged(&stage_dir, "registry.json"),
        &projection.registry,
        0o600,
    )?;
    tilo_files::native::atomic::write(&staged(&stage_dir, "fleet.json"), &projection.fleet)?;
    tilo_files::native::atomic::write_with_mode(
        &staged(&stage_dir, "home-peers.json"),
        &projection.peers,
        0o644,
    )?;
    tilo_files::native::atomic::write(
        &staged(&stage_dir, "user-pubkey.bin"),
        &payload.cert.user_public_key,
    )?;
    tilo_files::native::atomic::write(&staged(&stage_dir, "per-user-cert.bin"), &cert)?;
    tilo_files::native::atomic::write(&staged(&stage_dir, "default-cert.bin"), &cert)?;
    let marker_path = personal_marker_path(identity_root, &stage_name);
    let marker = PersonalEnrollmentMarker {
        schema_version: PERSONAL_ENROLLMENT_SCHEMA,
        identity_dir: identity_dir.to_path_buf(),
        stage_name,
        user_public_key: payload.cert.user_public_key,
        device_public_key: payload.cert.device_public_key,
        requester_lease,
        commit_ready: false,
    };
    std::fs::create_dir_all(enrollment_commit_dir(identity_root))?;
    tilo_files::native::atomic::write_with_mode(
        &marker_path,
        &serde_json::to_vec_pretty(&marker)?,
        0o600,
    )?;
    let _ = paths; // binds staging to the active device layout at the call site.
    Ok((marker_path, marker))
}

fn authorize_personal_commit(
    marker_path: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
    requester_lease: Option<tilo_relay::ProcessLease>,
) -> anyhow::Result<PersonalEnrollmentMarker> {
    if marker.requester_lease != requester_lease {
        bail!("personal enrollment requester lease changed before commit");
    }
    validate_requester(requester_lease)?;
    let mut marker = marker.clone();
    marker.commit_ready = true;
    tilo_files::native::atomic::write_with_mode(
        marker_path,
        &serde_json::to_vec_pretty(&marker)?,
        0o600,
    )?;
    Ok(marker)
}

fn commit_staged_personal_enrollment(
    identity_root: &std::path::Path,
    paths: &IdentityPaths,
    marker_path: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
    mut checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    if marker.schema_version != PERSONAL_ENROLLMENT_SCHEMA || !marker.commit_ready {
        bail!("personal enrollment marker is invalid");
    }
    let stage_dir = private_stage_dir(identity_root, &marker.stage_name)?;
    let cert_bytes = std::fs::read(staged(&stage_dir, "default-cert.bin"))?;
    let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&cert_bytes)?;
    if cert.user_public_key != marker.user_public_key
        || cert.device_public_key != marker.device_public_key
        || verify_device_cert_at(&cert, now_ns()?).is_err()
        || std::fs::read(staged(&stage_dir, "user-pubkey.bin"))? != marker.user_public_key
    {
        bail!("staged personal enrollment no longer validates");
    }
    // The first callback is the requester-liveness gate in production. Keep
    // it before even creating the user identity directory so a compositor
    // that died while approval was in flight causes no publication mutation.
    checkpoint(PersonalCommitCheckpoint::Record)?;
    // The identity directory is the person's: a root-run (headless) join
    // writes there as the person, never as root.
    let record = identity_record_path(&marker.identity_dir, marker.user_public_key);
    let fleet = fleet_path(&marker.identity_dir, marker.user_public_key);
    let record_json = std::fs::read(staged(&stage_dir, "record.json"))?;
    as_person(&marker.identity_dir, || {
        std::fs::create_dir_all(&marker.identity_dir)?;
        tilo_files::native::atomic::write_with_mode(&record, &record_json, 0o600)
    })?;
    checkpoint(PersonalCommitCheckpoint::Fleet)?;
    let fleet_json = std::fs::read(staged(&stage_dir, "fleet.json"))?;
    as_person(&marker.identity_dir, || {
        tilo_files::native::atomic::write_creating_parent(&fleet, &fleet_json)
    })?;
    checkpoint(PersonalCommitCheckpoint::HomePeers)?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &tilo_relay::home_peer_registry_path(identity_root),
        &std::fs::read(staged(&stage_dir, "home-peers.json"))?,
        0o644,
    )?;
    checkpoint(PersonalCommitCheckpoint::Registry)?;
    let registry_json = std::fs::read(staged(&stage_dir, "registry.json"))?;
    as_person(&marker.identity_dir, || {
        tilo_files::native::atomic::write_with_mode(
            &marker.identity_dir.join("registry.json"),
            &registry_json,
            0o600,
        )
    })?;
    checkpoint(PersonalCommitCheckpoint::UserPublicKey)?;
    // The public key and certificates are public, whatever the umask this
    // commit runs under: the session reads them to verify the device, and
    // reaches the per-identity ones through a directory shared like its parent.
    let public = tilo_user_identity::persist::PUBLIC_FILE_MODE;
    tilo_files::native::atomic::write_with_mode(
        &paths.user_pubkey,
        &marker.user_public_key,
        public,
    )?;
    checkpoint(PersonalCommitCheckpoint::PerUserCert)?;
    let per_user_cert = paths.device_cert_for_user(&marker.user_public_key);
    share_directory_like_parent(
        per_user_cert
            .parent()
            .context("per-user certificate has no directory")?,
    )?;
    tilo_files::native::atomic::write_with_mode(
        &per_user_cert,
        &std::fs::read(staged(&stage_dir, "per-user-cert.bin"))?,
        public,
    )?;
    // This device and the approver become each other's personal devices. The
    // approver's certificate comes from the root-signed fleet staged with this
    // enrollment and must verify under the identity this device joins.
    checkpoint(PersonalCommitCheckpoint::PersonalPeers)?;
    let personal_peers = remember_staged_personal_devices(identity_root, &stage_dir, marker)?;
    // Publish this last: every projection file has been durably installed.
    checkpoint(PersonalCommitCheckpoint::DefaultCert)?;
    tilo_files::native::atomic::write_with_mode(&paths.device_cert, &cert_bytes, public)?;
    // The joined identity is the one this device now acts for: select it, or
    // the next login resolves the absent default and asks to create one. The
    // first-use UI reads a selected joined identity as a finished join, so it
    // is selected only once every file of the join is in place. Only the
    // directory's owner can select (the selector is theirs alone); a root-run
    // join for someone else (`--person-uid`) leaves it to them.
    if PERSON_UID.get().is_none() {
        let record_id = record
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("joined identity record has no runtime id")?;
        as_person(&marker.identity_dir, || {
            runtime_identity::set_active_identity(&marker.identity_dir, record_id)
        })
        .context("select the joined identity")?;
    }
    // A root-run (headless) enrollment wrote these, under the identity root,
    // as root. Restore each one's directory owner and group before the marker
    // is cleared, so recovery repeats this step if it is interrupted. Parents
    // come before their children.
    for path in [
        tilo_relay::home_peer_registry_path(identity_root),
        personal_peers,
        paths.user_pubkey.clone(),
        per_user_cert
            .parent()
            .map(std::path::Path::to_path_buf)
            .context("per-user certificate has no directory")?,
        per_user_cert,
        paths.device_cert.clone(),
    ] {
        restore_owner_from_parent(&path)?;
    }
    std::fs::remove_file(marker_path)?;
    if let Some(parent) = marker_path.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        directory.sync_all()?;
    }
    let _ = std::fs::remove_dir_all(stage_dir);
    Ok(record)
}

fn recover_personal_enrollments(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    paths: &IdentityPaths,
    mut checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let directory = enrollment_commit_dir(identity_root);
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        // Any member of the device-trust group can leave a file here; only
        // this account's own markers are its enrollments.
        #[cfg(unix)]
        if std::os::unix::fs::MetadataExt::uid(&entry.metadata()?)
            != rustix::process::geteuid().as_raw()
        {
            continue;
        }
        let marker: PersonalEnrollmentMarker =
            serde_json::from_slice(&std::fs::read(entry.path())?)
                .context("personal enrollment recovery marker is invalid")?;
        if marker.identity_dir == identity_dir && marker.commit_ready {
            commit_staged_personal_enrollment(
                identity_root,
                paths,
                &entry.path(),
                &marker,
                &mut checkpoint,
            )?;
        }
    }
    Ok(())
}

fn install_personal_projection(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    payload: &ResponsePayload,
) -> anyhow::Result<PathBuf> {
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    let (marker_path, marker) =
        stage_personal_enrollment(identity_root, identity_dir, &paths, payload, None)?;
    let marker = authorize_personal_commit(&marker_path, &marker, None)?;
    commit_staged_personal_enrollment(identity_root, &paths, &marker_path, &marker, |_| Ok(()))
}

#[derive(Serialize, Deserialize)]
struct AckPayload {
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    response_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Ack {
    payload: AckPayload,
    signature: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct FinalizedPayload {
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    response_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Finalized {
    payload: FinalizedPayload,
    signature: Vec<u8>,
}

fn register_home_peer(
    identity_root: &std::path::Path,
    cert: &DeviceCert,
    relay_service_node_id: [u8; 32],
    relay_workspace_ids: &[u64],
    relay_endpoint: Option<iroh_base::EndpointAddr>,
) -> anyhow::Result<()> {
    let path = tilo_relay::home_peer_registry_path(identity_root);
    if !tilo_relay::valid_home_peer_label(&cert.device_label) || relay_workspace_ids.len() > 4096 {
        bail!("home-peer registration exceeds its bounded schema");
    }
    let mut registrations = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            serde_json::from_slice::<Vec<HomePeerRegistration>>(&bytes)
                .context("existing home-peer registry is invalid")?
        }
        Ok(_) => bail!("existing home-peer registry exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if registrations.len() > 128
        || registrations.iter().any(|entry| {
            entry.device_node_id.len() != 64
                || hex::decode(&entry.device_node_id).is_err()
                || entry.relay_service_node_id.len() != 64
                || hex::decode(&entry.relay_service_node_id).is_err()
                || entry.user_public_key.len() != 64
                || hex::decode(&entry.user_public_key).is_err()
                || !valid_relay_endpoint_hint(entry)
                || entry.device_cert.as_ref().is_some_and(|cert| {
                    verify_device_cert(cert).is_err()
                        || hex::encode(cert.device_public_key) != entry.device_node_id
                        || hex::encode(cert.user_public_key) != entry.user_public_key
                })
                || !tilo_relay::valid_home_peer_label(&entry.label)
                || entry.relay_workspace_ids.len() > 4096
        })
    {
        bail!("existing home-peer registry violates its bounded schema");
    }
    let device_node_id = hex::encode(cert.device_public_key);
    registrations.retain(|entry| entry.device_node_id != device_node_id);
    registrations.push(HomePeerRegistration {
        schema_version: HomePeerRegistration::CURRENT_SCHEMA,
        device_node_id,
        relay_service_node_id: hex::encode(relay_service_node_id),
        relay_endpoint,
        label: cert.device_label.clone(),
        user_public_key: hex::encode(cert.user_public_key),
        device_cert: Some(cert.clone()),
        relay_workspace_ids: relay_workspace_ids.to_vec(),
        environments: tilo_relay_protocol::RelayEnvironments::default(),
    });
    if registrations.len() > 128 {
        bail!("home-peer registry is full");
    }
    registrations.sort_by(|left, right| left.device_node_id.cmp(&right.device_node_id));
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &path,
        &serde_json::to_vec_pretty(&registrations)?,
        0o644,
    )?;
    Ok(())
}

fn response_preimage(payload: &ResponsePayload) -> anyhow::Result<Vec<u8>> {
    let mut preimage = RESPONSE_DOMAIN.to_vec();
    preimage.extend(postcard::to_stdvec(payload)?);
    Ok(preimage)
}

/// Sign a device certificate using the authenticated person root.
fn sign_root_cert(cert: &mut DeviceCert, root: &(impl RootSigner + ?Sized)) -> anyhow::Result<()> {
    if cert.user_public_key != root.public_key() {
        bail!("device certificate names a different identity than the signing root");
    }
    let digest = tilo_user_identity::device_cert_signing_digest(cert)?;
    cert.signature = root.sign_message(&digest)?;
    verify_device_cert(cert).context("signed device certificate does not verify")?;
    Ok(())
}

/// `payload` signed by `approver`, encoded: a [`Response`] from the
/// identity's key, an [`IntroducedResponse`] from an introducer.
fn sign_response(
    payload: ResponsePayload,
    approver: Approver<'_>,
) -> anyhow::Result<zeroize::Zeroizing<Vec<u8>>> {
    let signature = approver.sign(&response_preimage(&payload)?)?;
    Ok(zeroize::Zeroizing::new(match approver {
        Approver::Root(_) => postcard::to_stdvec(&Response { payload, signature })?,
        Approver::Introducer { issuer, .. } => postcard::to_stdvec(&IntroducedResponse {
            payload,
            issuer: issuer.clone(),
            signature,
        })?,
    }))
}

fn verify_signature(public_key: [u8; 32], message: &[u8], signature: &[u8]) -> anyhow::Result<()> {
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature has the wrong length"))?;
    VerifyingKey::from_bytes(&public_key)?.verify(message, &Signature::from_bytes(&signature))?;
    Ok(())
}

/// The payload of the encoded response `bytes` and the key that signed it:
/// the identity's key of `token`, or the device that minted `token` while it
/// was certified by that identity.
fn verify_response(
    bytes: &[u8],
    token: &tilo_user_identity::EnrollmentToken,
) -> anyhow::Result<(ResponsePayload, [u8; 32])> {
    if let Ok(response) = postcard::from_bytes::<Response>(bytes)
        && verify_signature(
            token.user_public_key,
            &response_preimage(&response.payload)?,
            &response.signature,
        )
        .is_ok()
    {
        return Ok((response.payload, token.user_public_key));
    }
    let response: IntroducedResponse = postcard::from_bytes(bytes)
        .context("enrollment response is not signed by the identity or its device")?;
    let issuer = &response.issuer;
    let token_issuer = (token.issuer_device_public_key, token.user_public_key);
    if (issuer.device_public_key, issuer.user_public_key) != token_issuer {
        bail!("enrollment response comes from another device than the token's");
    }
    verify_device_cert_at(issuer, token.issued_at_ns)
        .context("the approving device is not certified by the identity")?;
    verify_signature(
        issuer.device_public_key,
        &response_preimage(&response.payload)?,
        &response.signature,
    )
    .context("enrollment response was changed in transit")?;
    Ok((response.payload, issuer.device_public_key))
}

fn valid_solicitation(solicitation: &PairSolicitation) -> bool {
    solicitation.magic == PAIR_MAGIC
        && VerifyingKey::from_bytes(&solicitation.device_public_key).is_ok()
        && solicitation.relay_service_node_id != [0; 32]
        && tilo_relay::valid_home_peer_label(&solicitation.label)
}

/// Bind a LAN-discovery listener on `port` without making an already-running
/// discovery view or approval sheet prevent another Tilo surface from joining
/// the same nearby-device flow.  UDP broadcast is deliberately delivered to
/// every reuse-port listener; each recipient still has to authenticate the
/// encrypted offer or enrollment request before it can act on it.
fn bind_pair_listener(port: u16) -> anyhow::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    Ok(socket.into())
}

fn discover(timeout: u64, kind: EnrollmentKind) -> anyhow::Result<()> {
    let socket = bind_pair_listener(NEARBY_PORT)?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let deadline = deadline_after(socket_timeout(timeout))?;
    let mut packet = vec![0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut packet) {
            Ok((len, _)) => {
                let Ok(solicitation) =
                    postcard::from_bytes::<PairSolicitation>(received_packet(&packet, len)?)
                else {
                    continue;
                };
                if !valid_solicitation(&solicitation) || solicitation.kind != kind {
                    continue;
                }
                println!(
                    "{}",
                    serde_json::to_string(&PairCandidate {
                        device_node_id: hex::encode(solicitation.device_public_key),
                        relay_service_node_id: hex::encode(solicitation.relay_service_node_id),
                        label: solicitation.label,
                        verification_code: pairing_code(
                            solicitation.device_public_key,
                            solicitation.relay_service_node_id,
                        ),
                        kind: solicitation.kind,
                    })?
                );
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no matching unprovisioned Tilo device was found nearby")
}

// The three sealed envelopes (pair offer, request, response) are
// `"TLOENRL" || 0x02 || tilo-seal HPKE sealed box` (RFC 9180 base mode,
// X25519-HKDF-SHA256 / ChaCha20-Poly1305), with one tilo-seal label per
// step so an envelope for one step never opens as another. Bytes without
// that prefix are refused.
const ENVELOPE_MAGIC: &[u8; 7] = b"TLOENRL";
const ENVELOPE_V2: u8 = 2;

fn v2_header() -> [u8; 8] {
    let [m0, m1, m2, m3, m4, m5, m6] = *ENVELOPE_MAGIC;
    [m0, m1, m2, m3, m4, m5, m6, ENVELOPE_V2]
}

/// The sealed box inside `bytes`, or `None` without the envelope prefix.
fn v2_body(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_prefix(&v2_header()[..])
}

fn wrap_v2(sealed: &tilo_seal::SealedBox) -> Vec<u8> {
    let mut out = v2_header().to_vec();
    out.extend(sealed.to_bytes());
    out
}

/// A device's X25519 sealing secret, derived from its Ed25519 key.
fn device_sealing_secret(device: &ed25519_dalek::SigningKey) -> tilo_seal::SecretKey {
    tilo_seal::SecretKey::from_bytes(device.to_scalar_bytes())
}

/// A device's X25519 sealing public key, derived from its Ed25519 key.
fn device_sealing_public(device_public_key: [u8; 32]) -> anyhow::Result<tilo_seal::PublicKey> {
    Ok(tilo_seal::PublicKey::from_bytes(
        VerifyingKey::from_bytes(&device_public_key)?
            .to_montgomery()
            .to_bytes(),
    ))
}

fn pair_offer_aad(recipient: [u8; 32]) -> Vec<u8> {
    [PAIR_DOMAIN, &recipient].concat()
}

/// Seal an enrollment token to the soliciting device.
fn encrypt_pair_offer(token: &str, recipient: [u8; 32]) -> anyhow::Result<Vec<u8>> {
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &device_sealing_public(recipient)?,
        tilo_seal::Label::RelayEnrollOffer,
        &pair_offer_aad(recipient),
        token.as_bytes(),
    )?))
}

fn decrypt_pair_offer(
    bytes: &[u8],
    recipient: &ed25519_dalek::SigningKey,
) -> anyhow::Result<String> {
    let recipient_public = recipient.verifying_key().to_bytes();
    let secret = device_sealing_secret(recipient);
    let aad = pair_offer_aad(recipient_public);
    let plaintext = v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(&secret, tilo_seal::Label::RelayEnrollOffer, &aad, body)
                .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("nearby pairing offer was not encrypted for this server"))?;
    String::from_utf8(plaintext.to_vec()).context("nearby pairing offer is not text")
}

fn response_aad(token_nonce: [u8; 16], recipient: [u8; 32]) -> Vec<u8> {
    let mut aad = RESPONSE_DOMAIN.to_vec();
    aad.extend(token_nonce);
    aad.extend(recipient);
    aad
}

/// Seal an enrollment request to the issuing device. Returns
/// the envelope and the secret its response will be sealed to.
fn encrypt_request(
    request: &Request,
    issuer_device_public_key: [u8; 32],
) -> anyhow::Result<(Vec<u8>, tilo_seal::SecretKey)> {
    let response_secret = tilo_seal::SecretKey::generate()?;
    let plaintext = zeroize::Zeroizing::new(postcard::to_stdvec(&RequestV2 {
        request,
        response_public_key: response_secret.public_key()?.to_bytes(),
    })?);
    let sealed = tilo_seal::hpke::seal(
        &device_sealing_public(issuer_device_public_key)?,
        tilo_seal::Label::RelayEnrollRequest,
        &v2_header(),
        &plaintext,
    )?;
    Ok((wrap_v2(&sealed), response_secret))
}

/// Open an enrollment request, returning where its response must be sealed
/// to.
fn decrypt_request(
    bytes: &[u8],
    issuer_device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<(Request, ResponseRoute)> {
    let secret = device_sealing_secret(issuer_device);
    let not_for_us = || anyhow::anyhow!("enrollment request was not encrypted for this issuer");
    let body = v2_body(bytes).ok_or_else(not_for_us)?;
    let plaintext = tilo_seal::hpke::open_bytes(
        &secret,
        tilo_seal::Label::RelayEnrollRequest,
        &v2_header(),
        body,
    )
    .map_err(|_| not_for_us())?;
    let decoded: RequestV2Owned = postcard::from_bytes(&plaintext)?;
    Ok((
        decoded.request,
        ResponseRoute {
            response_public_key: decoded.response_public_key,
        },
    ))
}

/// Seal the signed response to the requester.
fn encrypt_response(
    plaintext: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    route: ResponseRoute,
) -> anyhow::Result<Vec<u8>> {
    let aad = response_aad(token_nonce, recipient);
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &tilo_seal::PublicKey::from_bytes(route.response_public_key),
        tilo_seal::Label::RelayEnrollResponse,
        &aad,
        plaintext,
    )?))
}

fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    response_secret: &tilo_seal::SecretKey,
) -> anyhow::Result<zeroize::Zeroizing<Vec<u8>>> {
    let aad = response_aad(token_nonce, recipient);
    v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(
                response_secret,
                tilo_seal::Label::RelayEnrollResponse,
                &aad,
                body,
            )
            .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("enrollment response was not encrypted for this device"))
}

fn response_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn make_ack(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Vec<u8>> {
    let payload = AckPayload {
        nonce,
        device_public_key: device.verifying_key().to_bytes(),
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    let ack = Ack {
        payload,
        signature: device.sign(&encoded).to_bytes().to_vec(),
    };
    Ok(postcard::to_stdvec(&ack)?)
}

fn verify_ack(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let ack: Ack = postcard::from_bytes(bytes)?;
    if ack.payload.nonce != nonce
        || ack.payload.device_public_key != device_public_key
        || ack.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment acknowledgement did not match the response");
    }
    let signature: [u8; 64] = ack
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("acknowledgement signature has the wrong length"))?;
    VerifyingKey::from_bytes(&device_public_key)?.verify(
        &postcard::to_stdvec(&ack.payload)?,
        &Signature::from_bytes(&signature),
    )?;
    Ok(())
}

fn make_finalized(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    approver: Approver<'_>,
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    Ok(postcard::to_stdvec(&Finalized {
        payload,
        signature: approver.sign(&encoded)?,
    })?)
}

fn verify_finalized(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    approver_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let finalized: Finalized = postcard::from_bytes(bytes)?;
    if finalized.payload.nonce != nonce
        || finalized.payload.device_public_key != device_public_key
        || finalized.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment completion did not match this exchange");
    }
    verify_signature(
        approver_public_key,
        &postcard::to_stdvec(&finalized.payload)?,
        &finalized.signature,
    )
}

fn now_ns() -> anyhow::Result<u64> {
    try_unix_now_ns().context("system clock precedes the Unix epoch")
}

/// How far before the issuer's clock an enrollment token or device
/// certificate begins to be valid.
///
/// What this side issues is checked on another machine's clock the moment it
/// arrives, and a not-before in that machine's future is refused. Two
/// machines' clocks never agree exactly: a guest whose wall clock came from a
/// one-second RTC at boot, or a fresh install whose time sync has not yet
/// slewed, trails its issuer by up to a few seconds, and a certificate issued
/// a millisecond earlier was "not yet valid" on the very device it named.
/// The issuer decides when validity begins, so it begins slightly in the
/// past; every verifier keeps its exact not-before check. The allowance is
/// the one signed peer actions already tolerate
/// (`ACTION_SIGNATURE_TOLERANCE_NS`). It grants nothing: expiry is unchanged
/// (a token still lapses a fixed time after it is minted, a personal
/// certificate has none), revocation is by key, and certificates this side
/// issues keep their relative order.
const ISSUANCE_CLOCK_ALLOWANCE_NS: u64 = 60 * 1_000_000_000;

/// The not-before for something issued at `now_ns`.
fn validity_start_ns(now_ns: u64) -> u64 {
    now_ns.saturating_sub(ISSUANCE_CLOCK_ALLOWANCE_NS)
}

fn socket_timeout(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(1, 300))
}

fn deadline_after(duration: Duration) -> anyhow::Result<std::time::Instant> {
    std::time::Instant::now()
        .checked_add(duration)
        .context("enrollment timeout exceeds the platform clock range")
}

fn received_packet(buffer: &[u8], received: usize) -> anyhow::Result<&[u8]> {
    buffer
        .get(..received)
        .context("socket reported a packet larger than its receive buffer")
}

fn validate_requester(lease: Option<tilo_relay::ProcessLease>) -> anyhow::Result<()> {
    if let Some(lease) = lease {
        lease
            .validate()
            .context("the first-use requester exited; nearby pairing was revoked")?;
    }
    Ok(())
}

fn join(
    token_file: &std::path::Path,
    identity_root: &std::path::Path,
    label: &str,
    timeout: u64,
    home_peer_output: &std::path::Path,
) -> anyhow::Result<()> {
    let token_text = std::fs::read_to_string(token_file)
        .with_context(|| format!("read {}", token_file.display()))?;
    join_with_token(
        token_text.trim(),
        JoinOptions {
            identity_root,
            label,
            timeout,
            kind: EnrollmentKind::HomePeer,
            home_peer_output: Some(home_peer_output),
            identity_dir: None,
            requester_lease: None,
            port: NEARBY_PORT,
        },
    )
    .map(|_| ())
}

/// Join with `token_text`; a personal device gets back its joined identity
/// record.
#[allow(
    clippy::too_many_lines,
    reason = "one ordered join exchange; each step relies on the token and identity checks before it"
)]
fn join_with_token(token_text: &str, options: JoinOptions<'_>) -> anyhow::Result<Option<PathBuf>> {
    let JoinOptions {
        identity_root,
        label,
        timeout,
        kind,
        home_peer_output,
        identity_dir,
        requester_lease,
        port,
    } = options;
    validate_requester(requester_lease)?;
    let token = decode_enrollment_token(token_text).context("decode enrollment token")?;
    verify_enrollment_token(&token, now_ns()?).context("verify enrollment token")?;
    if !kind.role().admits(&token.capabilities) {
        match kind {
            EnrollmentKind::HomePeer => {
                bail!("enrollment token does not grant home-server capabilities")
            }
            EnrollmentKind::PersonalDevice => {
                bail!("enrollment token grants the wrong device role")
            }
        }
    }
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    if kind == EnrollmentKind::PersonalDevice {
        recover_personal_enrollments(
            identity_root,
            identity_dir.context("identity directory is required")?,
            &paths,
            |_| validate_requester(requester_lease),
        )?;
    }
    let (device, _) = tilo_user_identity::persist::load_or_create_device_key(&paths)?;
    let request = Request {
        token: token_text.to_owned(),
        device_public_key: device.verifying_key().to_bytes(),
        relay_service_node_id: tilo_relay::daemon::relay_service_node_id(&device),
        label: label.chars().take(80).collect(),
        kind,
    };
    let (bytes, request_ephemeral) = encrypt_request(&request, token.issuer_device_public_key)?;
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = deadline_after(socket_timeout(timeout))?;
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);
    let mut reply = vec![0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        socket.send_to(&bytes, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, sender)) => {
                let response_bytes = received_packet(&reply, len)?.to_vec();
                validate_requester(requester_lease)?;
                let Ok((payload, approver_public_key)) = decrypt_response(
                    &response_bytes,
                    token.nonce,
                    device.verifying_key().to_bytes(),
                    &request_ephemeral,
                )
                .and_then(|plaintext| verify_response(&plaintext, &token)) else {
                    continue;
                };
                let this_device = request.device_public_key;
                let answers_this_request = payload.nonce == token.nonce
                    && payload.relay_service_node_id == request.relay_service_node_id;
                let addressed_to_this_device = payload.recipient_device_public_key == this_device
                    && payload.cert.device_public_key == this_device;
                let certifies_the_token = payload.cert.user_public_key == token.user_public_key
                    && payload.cert.capabilities == token.capabilities;
                if !(answers_this_request && addressed_to_this_device && certifies_the_token) {
                    continue;
                }
                verify_device_cert_at(&payload.cert, now_ns()?)
                    .context("verify enrolled device certificate validity")?;
                if kind == EnrollmentKind::HomePeer {
                    let home_peer_toml = payload
                        .home_peer_toml
                        .as_ref()
                        .context("approving device omitted home-peer configuration")?;
                    let _: fs::full_replica::HomePeerSet = toml::from_str(home_peer_toml)
                        .context("approving device returned an invalid home-peer configuration")?;
                    tilo_files::native::atomic::write(
                        home_peer_output.context("home-peer output is required")?,
                        home_peer_toml.as_bytes(),
                    )?;
                }
                // Preparing a personal enrollment only touches the private
                // staging area. In particular, an existing device's global
                // user key and default certificate remain untouched until
                // Finalized has been authenticated.
                let staged_personal = if kind == EnrollmentKind::PersonalDevice {
                    Some(stage_personal_enrollment(
                        identity_root,
                        identity_dir.context("identity directory is required")?,
                        &paths,
                        &payload,
                        requester_lease,
                    )?)
                } else {
                    None
                };
                let ack = make_ack(&response_bytes, token.nonce, &device)?;
                socket.set_read_timeout(Some(Duration::from_millis(300)))?;
                // The approver commits (durable writes) before it sends
                // Finalized, so its storage latency, not a fixed allowance,
                // bounds this wait; the enrollment window is the budget.
                let completion_deadline = deadline_after(socket_timeout(timeout))?;
                while std::time::Instant::now() < completion_deadline {
                    validate_requester(requester_lease)?;
                    socket.send_to(&ack, sender)?;
                    match socket.recv_from(&mut reply) {
                        Ok((final_len, final_sender)) if final_sender == sender => {
                            if verify_finalized(
                                received_packet(&reply, final_len)?,
                                &response_bytes,
                                token.nonce,
                                device.verifying_key().to_bytes(),
                                approver_public_key,
                            )
                            .is_ok()
                            {
                                validate_requester(requester_lease)?;
                                let record_path =
                                    if let Some((marker_path, marker)) = staged_personal.as_ref() {
                                        // This durable marker is the commit point:
                                        // both Finalized and the exact originating
                                        // compositor lease are valid. A crash before
                                        // it publishes nothing; recovery after it may
                                        // finish this already-authorized transaction.
                                        let marker = authorize_personal_commit(
                                            marker_path,
                                            marker,
                                            requester_lease,
                                        )?;
                                        Some(commit_staged_personal_enrollment(
                                            identity_root,
                                            &paths,
                                            marker_path,
                                            &marker,
                                            |_| validate_requester(requester_lease),
                                        )?)
                                    } else {
                                        // The host path has no multi-file member
                                        // projection, but still waits for
                                        // Finalized before changing its active
                                        // identity binding.
                                        tilo_user_identity::persist::write_public_key(
                                            &paths.user_pubkey,
                                            &payload.cert.user_public_key,
                                        )?;
                                        None
                                    };
                                // Publish the default cert as the completion
                                // marker only after the approver durably registers us,
                                // authenticates Finalized, and the member projection is
                                // fully installed. A crash can therefore only leave a
                                // retryable partial enrollment, never a bootable one.
                                if kind == EnrollmentKind::HomePeer {
                                    let per_user_cert =
                                        paths.device_cert_for_user(&payload.cert.user_public_key);
                                    share_directory_like_parent(
                                        per_user_cert
                                            .parent()
                                            .context("per-user certificate has no directory")?,
                                    )?;
                                    tilo_user_identity::persist::write_device_cert(
                                        &per_user_cert,
                                        &payload.cert,
                                    )?;
                                    tilo_user_identity::persist::write_device_cert(
                                        &paths.device_cert,
                                        &payload.cert,
                                    )?;
                                }
                                // The identity directories themselves are
                                // image layout and keep their owner and group;
                                // only what this enrollment created below them
                                // takes its directory's.
                                let per_user_cert =
                                    paths.device_cert_for_user(&payload.cert.user_public_key);
                                for output in [
                                    per_user_cert.parent(),
                                    Some(per_user_cert.as_path()),
                                    Some(paths.user_pubkey.as_path()),
                                    Some(paths.device_privkey.as_path()),
                                    Some(paths.device_cert.as_path()),
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    restore_owner_from_parent(output)?;
                                }
                                NonceStore::under(identity_root)
                                    .record(&token.nonce, device.verifying_key().to_bytes())?;
                                if kind == EnrollmentKind::PersonalDevice {
                                    println!(
                                        "{}",
                                        serde_json::json!({
                                            "identity": hex::encode(payload.cert.user_public_key),
                                            "record_path": record_path.as_ref().context("record path is required")?,
                                            "label": payload.cert.device_label,
                                        })
                                    );
                                } else {
                                    println!("enrolled {}", payload.cert.device_label);
                                }
                                return Ok(record_path);
                            }
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                bail!("approving device did not confirm enrollment; retrying is safe");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(
        WindowClosed("no approving Tilo device answered before the enrollment window closed")
            .into(),
    )
}

fn pair(options: JoinOptions<'_>, status_file: Option<&std::path::Path>) -> anyhow::Result<()> {
    let JoinOptions {
        identity_root,
        label,
        timeout,
        kind,
        requester_lease,
        port,
        ..
    } = options;
    // Desktop first-use always supplies a compositor lease. Agent hosts pass
    // `--headless` so the lease is absent on purpose; validate_requester is a
    // no-op for None.
    validate_requester(requester_lease)?;
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    let (device, _) = tilo_user_identity::persist::load_or_create_device_key(&paths)?;
    let relay_service_node_id = tilo_relay::daemon::relay_service_node_id(&device);
    let label: String = label.chars().take(80).collect();
    if !tilo_relay::valid_home_peer_label(&label) {
        bail!("home-server label must be a printable single-line name");
    }
    let solicitation = postcard::to_stdvec(&PairSolicitation {
        magic: PAIR_MAGIC,
        device_public_key: device.verifying_key().to_bytes(),
        relay_service_node_id,
        label: label.clone(),
        kind,
    })?;
    let code = pairing_code(device.verifying_key().to_bytes(), relay_service_node_id);
    if let Some(path) = status_file {
        write_pairing_status(
            path,
            &serde_json::json!({
                "state": "waiting_for_approval",
                "verification_code": code,
                "label": label,
            }),
        )?;
    }
    println!("Nearby pairing code: {code}");
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);
    let deadline = deadline_after(socket_timeout(timeout))?;
    let mut reply = vec![0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        socket.send_to(&solicitation, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, _sender)) => {
                let Ok(token_text) = decrypt_pair_offer(received_packet(&reply, len)?, &device)
                else {
                    continue;
                };
                let Ok(token) = decode_enrollment_token(token_text.trim()) else {
                    continue;
                };
                if verify_enrollment_token(&token, now_ns()?).is_err()
                    || !kind.role().admits(&token.capabilities)
                {
                    continue;
                }
                println!("Nearby Tilo device approved code {code}");
                let record_path = join_with_token(
                    token_text.trim(),
                    JoinOptions {
                        label: &label,
                        ..options
                    },
                )?;
                if let Some(path) = status_file {
                    // The first-use UI completes from this record alone; it
                    // must not depend on reading this process's output.
                    write_pairing_status(
                        path,
                        &serde_json::json!({
                            "state": "complete",
                            "verification_code": code,
                            "identity": hex::encode(token.user_public_key),
                            "record_path": record_path,
                        }),
                    )?;
                }
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(
        WindowClosed("no trusted Tilo device approved nearby pairing before the window closed")
            .into(),
    )
}

/// Publish first-use pairing progress for the UI that started this process.
fn write_pairing_status(path: &std::path::Path, status: &serde_json::Value) -> anyhow::Result<()> {
    tilo_files::native::atomic::write_with_mode(path, &serde_json::to_vec(status)?, 0o644)?;
    restore_owner_from_parent(path)
}

/// An enrollment window ended with no device answering.
#[derive(Debug)]
struct WindowClosed(&'static str);

impl std::fmt::Display for WindowClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for WindowClosed {}

/// What made pairing fail, as the status file names it for the first-use UI:
/// `permission_denied`, `storage_full`, `read_only`, `unreachable`,
/// `timed_out`, or `unknown` when the error carries no cause the person can
/// act on.
fn pairing_failure_cause(error: &anyhow::Error) -> &'static str {
    use std::io::ErrorKind;

    if error
        .chain()
        .any(<dyn std::error::Error>::is::<WindowClosed>)
    {
        return "timed_out";
    }
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .find_map(|io| match io.kind() {
            ErrorKind::PermissionDenied => Some("permission_denied"),
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Some("storage_full"),
            ErrorKind::ReadOnlyFilesystem => Some("read_only"),
            ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::AddrNotAvailable
            | ErrorKind::ConnectionRefused => Some("unreachable"),
            ErrorKind::TimedOut => Some("timed_out"),
            _ => None,
        })
        .unwrap_or("unknown")
}

/// Record why pairing ended without joining, so the first-use UI shows the
/// reason and offers a retry instead of waiting on a process that is gone.
fn report_pairing_failure(path: &std::path::Path, error: &anyhow::Error) {
    let _ = write_pairing_status(
        path,
        &serde_json::json!({
            "state": "failed",
            "error": format!("{error:#}"),
            "cause": pairing_failure_cause(error),
        }),
    );
}

/// How one requester-side enrollment runs: where its identity lives, the
/// label it asks for, the window, the device role, where a home peer's
/// descriptor goes, the requester lease that must stay live and the
/// nearby port.
#[derive(Clone, Copy)]
struct JoinOptions<'a> {
    identity_root: &'a std::path::Path,
    label: &'a str,
    timeout: u64,
    kind: EnrollmentKind,
    home_peer_output: Option<&'a std::path::Path>,
    identity_dir: Option<&'a std::path::Path>,
    requester_lease: Option<tilo_relay::ProcessLease>,
    port: u16,
}

struct ApprovalOptions<'a> {
    identity_root: &'a std::path::Path,
    identity_dir: Option<&'a std::path::Path>,
    timeout: u64,
    workspace_cache: &'a std::path::Path,
    acl: &'a std::path::Path,
    relay_workspace_ids: &'a [u64],
    expected_device: Option<&'a str>,
    expected_service: Option<&'a str>,
    kind: EnrollmentKind,
    /// Port the listener binds; 0 lets the OS choose one.
    port: u16,
    /// Receives the bound listener port once the approver is listening.
    ready: Option<&'a std::sync::mpsc::Sender<u16>>,
    /// The expected device's certificate, signed by the person's devices
    /// that hold the identity's key; `None` signs with the root held here.
    approved: Option<&'a DeviceCert>,
}

impl ApprovalIdentity {
    /// What signs this device's side: the identity's key when it holds it,
    /// so a device on any build can pair with it; otherwise its own
    /// certified key, as the introducer.
    fn approver(&self) -> Approver<'_> {
        self.root.as_deref().map_or(
            Approver::Introducer {
                device: &self.device,
                issuer: &self.issuer_cert,
            },
            Approver::Root,
        )
    }
}

struct ApprovalIdentity {
    /// The identity's root when this device holds it and signs itself.
    root: Option<Box<dyn RootSigner>>,
    device: ed25519_dalek::SigningKey,
    /// This device's certificate, the issuer of the tokens it mints.
    issuer_cert: DeviceCert,
    user_public_key: [u8; 32],
    relay_workspace_ids: Vec<u64>,
}

struct SelectedNearby {
    solicitation: PairSolicitation,
    token_text: String,
}

struct PendingApproval {
    request: Request,
    response_route: ResponseRoute,
    token: tilo_user_identity::EnrollmentToken,
    sender: std::net::SocketAddr,
}

struct ApprovalProjection {
    home_peer_toml: Option<String>,
    public_identity_json: Option<Vec<u8>>,
    fleet_json: Option<Vec<u8>>,
    home_peers_json: Option<Vec<u8>>,
}

struct PreparedApproval {
    cert: DeviceCert,
    relay_service_node_id: [u8; 32],
    encrypted: Vec<u8>,
}

fn expected_pair(options: &ApprovalOptions<'_>) -> anyhow::Result<Option<([u8; 32], [u8; 32])>> {
    match (options.expected_device, options.expected_service) {
        (Some(device), Some(service)) => Ok(Some((
            hex::decode(device)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected device ID has the wrong length"))?,
            hex::decode(service)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected service ID has the wrong length"))?,
        ))),
        (None, None) => Ok(None),
        _ => bail!("expected device and service IDs must be supplied together"),
    }
}

/// Load the signer and verify that this device remains authorised to admit
/// peers: with the root it holds, or as the introducer of a device whose
/// certificate the person's key holders approved.
fn load_approval_identity(options: &ApprovalOptions<'_>) -> anyhow::Result<ApprovalIdentity> {
    let paths = IdentityPaths::under(options.identity_root);
    let (root, user_public_key) = if let Some(approved) = options.approved {
        if options.kind != EnrollmentKind::PersonalDevice {
            bail!("a home server is added from the device you set up first");
        }
        verify_device_cert(approved).context("the approved certificate does not verify")?;
        if expected_pair(options)?.is_some_and(|(device, _)| device != approved.device_public_key) {
            bail!("the approved certificate is for another device");
        }
        (None, approved.user_public_key)
    } else {
        let root = load_approval_root(options.identity_root, options.identity_dir)?;
        let user_public_key = root.public_key();
        (Some(root), user_public_key)
    };
    let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let issuer_cert = tilo_user_identity::persist::read_device_cert(
        &paths.device_cert_for_user(&user_public_key),
    )
    .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))?;
    verify_device_cert_at(&issuer_cert, now_ns()?)
        .context("verify approving device certificate validity")?;
    if issuer_cert.user_public_key != user_public_key
        || issuer_cert.device_public_key != device.verifying_key().to_bytes()
        || (root.is_some() && !issuer_cert.capabilities.can_admit_devices)
    {
        bail!("this device is not authorised to approve enrollment");
    }
    if let Some(directory) = options.identity_dir
        && fleet_revokes_device(
            options.identity_root,
            directory,
            user_public_key,
            issuer_cert.device_public_key,
        )
        .context("verify approving device fleet")?
    {
        bail!("this device is revoked and cannot approve enrollment");
    }

    let mut relay_workspace_ids = options.relay_workspace_ids.to_vec();
    if options.kind == EnrollmentKind::HomePeer {
        relay_workspace_ids.push(tilo_relay::personal_continuity_workspace_id(
            &user_public_key,
        ));
    }
    Ok(ApprovalIdentity {
        root,
        device,
        issuer_cert,
        user_public_key,
        relay_workspace_ids,
    })
}

fn handle_solicitation(
    socket: &UdpSocket,
    sender: std::net::SocketAddr,
    solicitation: &PairSolicitation,
    kind: EnrollmentKind,
    expected: Option<([u8; 32], [u8; 32])>,
    identity: &ApprovalIdentity,
    selected: &mut Option<SelectedNearby>,
) -> anyhow::Result<()> {
    if !valid_solicitation(solicitation)
        || solicitation.kind != kind
        || expected.is_some_and(|(device, service)| {
            solicitation.device_public_key != device
                || solicitation.relay_service_node_id != service
        })
    {
        return Ok(());
    }
    if selected.as_ref().is_some_and(|current| {
        current.solicitation.device_public_key != solicitation.device_public_key
            || current.solicitation.relay_service_node_id != solicitation.relay_service_node_id
    }) {
        return Ok(());
    }
    if selected.is_none() {
        let mut nonce = [0_u8; 16];
        UnwrapErr(SysRng).fill_bytes(&mut nonce);
        let now = now_ns()?;
        // Backdated for the joining device's clock, but never to before this
        // device was certified: redemption checks the issuer at that instant.
        let issued_at_ns = validity_start_ns(now).max(identity.issuer_cert.issued_at_ns);
        let expires_at_ns = now.saturating_add(5 * 60 * 1_000_000_000);
        let token = mint_enrollment_token(
            identity.user_public_key,
            kind.role().preset(),
            issued_at_ns,
            expires_at_ns,
            nonce,
            &identity.device,
        )?;
        let token_text = encode_enrollment_token(&token)?;
        let code = pairing_code(
            solicitation.device_public_key,
            solicitation.relay_service_node_id,
        );
        println!(
            "Approving nearby device '{}' with code {code}",
            solicitation.label
        );
        *selected = Some(SelectedNearby {
            solicitation: solicitation.clone(),
            token_text,
        });
    }
    if let Some(selection) = selected
        && valid_solicitation(solicitation)
    {
        let offer = encrypt_pair_offer(
            &selection.token_text,
            selection.solicitation.device_public_key,
        )?;
        socket.send_to(&offer, sender)?;
    }
    Ok(())
}

fn valid_pending_request(
    request: &Request,
    token: &tilo_user_identity::EnrollmentToken,
    kind: EnrollmentKind,
    identity: &ApprovalIdentity,
    selected: Option<&SelectedNearby>,
) -> anyhow::Result<bool> {
    let wrong_selection = selected.is_some_and(|selection| {
        request.device_public_key != selection.solicitation.device_public_key
            || request.relay_service_node_id != selection.solicitation.relay_service_node_id
            || request.label != selection.solicitation.label
            || request.kind != selection.solicitation.kind
            || request.token != selection.token_text
    });
    Ok(verify_enrollment_token(token, now_ns()?).is_ok()
        && token.user_public_key == identity.user_public_key
        && token.issuer_device_public_key == identity.device.verifying_key().to_bytes()
        && request.kind == kind
        && kind.role().admits(&token.capabilities)
        && !wrong_selection)
}

fn receive_approval_request(
    socket: &UdpSocket,
    packet: &mut [u8],
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
) -> anyhow::Result<PendingApproval> {
    let expected = expected_pair(options)?;
    let deadline = deadline_after(socket_timeout(options.timeout))?;
    let mut selected = None;
    loop {
        if std::time::Instant::now() >= deadline {
            bail!("no valid matching nearby device request arrived");
        }
        let (len, sender) = match socket.recv_from(packet) {
            Ok(packet) => packet,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let bytes = received_packet(packet, len)?;
        if let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(bytes) {
            handle_solicitation(
                socket,
                sender,
                &solicitation,
                options.kind,
                expected,
                identity,
                &mut selected,
            )?;
            continue;
        }
        let Ok((request, response_route)) = decrypt_request(bytes, &identity.device) else {
            continue;
        };
        let Ok(token) = decode_enrollment_token(request.token.trim()) else {
            continue;
        };
        if valid_pending_request(&request, &token, options.kind, identity, selected.as_ref())? {
            return Ok(PendingApproval {
                request,
                response_route,
                token,
                sender,
            });
        }
    }
}

/// What the approval hands the device besides its certificate. The personal
/// device's fleet is filled in once the token is redeemed, because the fleet
/// then lists the new certificate.
fn approval_projection(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
) -> anyhow::Result<ApprovalProjection> {
    match options.kind {
        EnrollmentKind::HomePeer => {
            let ticket_text =
                std::fs::read_to_string(options.workspace_cache.join("workspace-ticket"))
                    .context("the local Files workspace is not ready to share")?;
            let ticket = fs::Ticket::decode(ticket_text.trim())
                .context("local workspace ticket is invalid")?;
            let capability_json = std::fs::read_to_string(options.acl)
                .with_context(|| format!("read {}", options.acl.display()))?;
            let capabilities: collab_types::FsAuthority = serde_json::from_str(&capability_json)
                .context("filesystem capability export is invalid")?;
            let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign_with(
                ticket.workspace_id(),
                1,
                capabilities,
                false,
                identity.user_public_key,
                |payload| {
                    identity
                        .root
                        .as_ref()
                        .context("this device holds no identity root")?
                        .sign_message(payload)
                },
            )?;
            let mut relay_workspace_ids = identity.relay_workspace_ids.clone();
            relay_workspace_ids.sort_unstable();
            relay_workspace_ids.dedup();
            let home_peer = fs::full_replica::HomePeerSet {
                status_file: PathBuf::from("/var/lib/tilo/full-replica/status.json"),
                workspaces: vec![fs::full_replica::HomePeerBundle {
                    ticket: ticket.encode()?,
                    cache_dir: PathBuf::from("/var/lib/tilo/full-replica")
                        .join(ticket.workspace_id().to_string()),
                    capability_json,
                    authority: Some(authority),
                    principals: Vec::new(),
                    relay_workspace_ids,
                }],
            };
            Ok(ApprovalProjection {
                home_peer_toml: Some(toml::to_string_pretty(&home_peer)?),
                public_identity_json: None,
                fleet_json: None,
                home_peers_json: None,
            })
        }
        EnrollmentKind::PersonalDevice => {
            options
                .identity_dir
                .context("identity directory is required for personal-device approval")?;
            Ok(ApprovalProjection {
                home_peer_toml: None,
                public_identity_json: Some(public_identity_record(
                    identity.user_public_key,
                    "My Tilo identity",
                )?),
                fleet_json: None,
                home_peers_json: Some(bounded_home_peer_registry(
                    options.identity_root,
                    identity.user_public_key,
                )?),
            })
        }
    }
}

fn prepare_approval(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    pending: &PendingApproval,
) -> anyhow::Result<PreparedApproval> {
    // Everything that can refuse runs before the token is redeemed: a refusal
    // after it would spend the token on a device that never hears back.
    let mut projection = approval_projection(options, identity)?;
    let cert = redeem_pending(options, identity, pending)?;
    if let Some(identity_dir) = options.identity_dir
        && options.kind == EnrollmentKind::PersonalDevice
    {
        projection.fleet_json = Some(project_personal_device(identity_dir, &cert)?);
    }
    let signed = sign_response(
        ResponsePayload {
            nonce: pending.token.nonce,
            recipient_device_public_key: pending.request.device_public_key,
            relay_service_node_id: pending.request.relay_service_node_id,
            cert: cert.clone(),
            home_peer_toml: projection.home_peer_toml,
            public_identity_json: projection.public_identity_json,
            fleet_json: projection.fleet_json,
            home_peers_json: projection.home_peers_json,
        },
        identity.approver(),
    )?;
    let encrypted = encrypt_response(
        &signed,
        pending.token.nonce,
        pending.request.device_public_key,
        pending.response_route,
    )?;
    Ok(PreparedApproval {
        cert,
        relay_service_node_id: pending.request.relay_service_node_id,
        encrypted,
    })
}

/// Redeem the pending token for the device's root-signed certificate, the way
/// every redeemer does: the issuer is checked at the token's issue time and
/// now and against the fleet's revocations, the nonce is spent in the shared
/// store, and the certificate is in the person's fleet before it is sent.
fn redeem_pending(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    pending: &PendingApproval,
) -> anyhow::Result<DeviceCert> {
    let now = now_ns()?;
    let fleet = options
        .identity_dir
        .map(|identity_dir| fleet_store(options.identity_root, identity_dir));
    let sign_here = |digest: &[u8]| {
        identity
            .root
            .as_ref()
            .ok_or_else(|| std::io::Error::other("this device holds no identity root"))?
            .sign_message(digest)
            .map_err(|error| std::io::Error::other(error.to_string()))
    };
    tilo_identity_host::enrollment::redeem(
        fleet.as_ref(),
        &NonceStore::under(options.identity_root),
        &Redemption {
            token: &pending.token,
            role: options.kind.role(),
            device_public_key: pending.request.device_public_key,
            label: &pending.request.label,
            now_ns: now,
            valid_from_ns: validity_start_ns(now),
            local_issuer: Some(&identity.issuer_cert),
        },
        options
            .approved
            .map_or(Certifier::Root(&sign_here), Certifier::Approved),
    )
    .context("redeem the enrollment token")
}

/// Wait for the device to acknowledge its signed configuration. The device
/// durably stages the whole enrollment before it acknowledges, so the wait
/// is bounded by the enrollment window rather than a fixed allowance that a
/// slow disk on the device side can exceed.
fn await_approval_ack(
    socket: &UdpSocket,
    packet: &mut [u8],
    pending: &PendingApproval,
    prepared: &PreparedApproval,
    window: Duration,
) -> anyhow::Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let deadline = deadline_after(window)?;
    while std::time::Instant::now() < deadline {
        socket.send_to(&prepared.encrypted, pending.sender)?;
        match socket.recv_from(packet) {
            Ok((len, sender)) if sender == pending.sender => {
                if verify_ack(
                    received_packet(packet, len)?,
                    &prepared.encrypted,
                    pending.token.nonce,
                    pending.request.device_public_key,
                )
                .is_ok()
                {
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("nearby device did not acknowledge its signed configuration")
}

fn commit_approval(
    socket: &UdpSocket,
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    pending: &PendingApproval,
    prepared: &PreparedApproval,
) -> anyhow::Result<()> {
    match options.kind {
        EnrollmentKind::HomePeer => register_home_peer(
            options.identity_root,
            &prepared.cert,
            prepared.relay_service_node_id,
            &identity.relay_workspace_ids,
            None,
        )?,
        EnrollmentKind::PersonalDevice => {
            // The fleet has listed the device since its token was redeemed.
            // The device acknowledged the certificate this device just issued
            // under the identity's root: both are now this person's devices.
            let personal_peers = remember_personal_devices(
                options.identity_root,
                &[
                    identity.device.verifying_key().to_bytes(),
                    prepared.cert.device_public_key,
                ],
            )?;
            restore_owner_from_parent(&personal_peers)?;
        }
    }
    let finalized = make_finalized(
        &prepared.encrypted,
        pending.token.nonce,
        pending.request.device_public_key,
        identity.approver(),
    )?;
    for _ in 0..8 {
        socket.send_to(&finalized, pending.sender)?;
        std::thread::sleep(Duration::from_millis(125));
    }
    println!(
        "approved nearby device {}",
        hex::encode(prepared.cert.device_public_key)
    );
    Ok(())
}

fn approve(options: &ApprovalOptions<'_>) -> anyhow::Result<()> {
    let identity = load_approval_identity(options)?;
    let socket = bind_pair_listener(options.port)?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    if let Some(ready) = options.ready {
        let _ = ready.send(socket.local_addr()?.port());
    }
    let mut packet = vec![0_u8; MAX_PACKET];
    let pending = receive_approval_request(&socket, &mut packet, options, &identity)?;
    // The listener shares the well-known port with every other nearby-pairing
    // surface, and the kernel delivers a unicast datagram to only one socket
    // of a reuse-port group. The device acknowledges to the address the
    // signed configuration came from, so the rest of the exchange runs on a
    // socket of this approver's own: another listener on the port can never
    // receive the acknowledgement in its place.
    drop(socket);
    let session = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    session.set_read_timeout(Some(Duration::from_secs(1)))?;
    let prepared = prepare_approval(options, &identity, &pending)?;
    await_approval_ack(
        &session,
        &mut packet,
        &pending,
        &prepared,
        socket_timeout(options.timeout),
    )?;
    commit_approval(&session, options, &identity, &pending, &prepared)
}

fn device_key(identity_root: &std::path::Path) -> anyhow::Result<()> {
    let paths = IdentityPaths::under(identity_root);
    let (device, _) = tilo_user_identity::persist::load_or_create_device_key(&paths)?;
    println!(
        "{}",
        serde_json::json!({
            "device_public_key": hex::encode(device.verifying_key().to_bytes()),
            "relay_service_node_id": hex::encode(tilo_relay::daemon::relay_service_node_id(&device)),
        })
    );
    Ok(())
}

/// The personal projection a certificate alone implies: the public identity
/// of its root, and a fleet of this device.
fn out_of_band_payload(
    cert: DeviceCert,
    device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<ResponsePayload> {
    let user = cert.user_public_key;
    let fleet = FleetProjection {
        schema_version: 1,
        identity_id: hex::encode(user),
        certificates: vec![cert.clone()],
        revocations: Vec::new(),
    };
    let digest = blake3::hash(&tilo_user_identity::canonical::canonical_bytes(&cert)?);
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(ResponsePayload {
        nonce,
        recipient_device_public_key: cert.device_public_key,
        relay_service_node_id: tilo_relay::daemon::relay_service_node_id(device),
        home_peer_toml: None,
        public_identity_json: Some(public_identity_record(user, "My Tilo identity")?),
        fleet_json: Some(serde_json::to_vec_pretty(&fleet)?),
        home_peers_json: Some(b"[]".to_vec()),
        cert,
    })
}

/// A host certified out of band that this device records as one of its
/// person's home peers: the relay it runs, and where that relay listens.
struct OutOfBandHomePeer {
    relay_service_node_id: [u8; 32],
    relay_endpoint: Option<iroh_base::EndpointAddr>,
}

impl OutOfBandHomePeer {
    /// `service` is the relay's endpoint id in hex, as `device-key` prints it.
    fn parse(service: &str, addrs: &[std::net::SocketAddr]) -> anyhow::Result<Self> {
        let relay_service_node_id: [u8; 32] = hex::decode(service.trim())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .context("--home-peer-service must be a 32-byte hex endpoint id")?;
        let id = iroh_base::EndpointId::from_bytes(&relay_service_node_id)
            .context("--home-peer-service is not an endpoint id")?;
        let relay_endpoint = (!addrs.is_empty()).then(|| {
            addrs
                .iter()
                .fold(iroh_base::EndpointAddr::new(id), |endpoint, addr| {
                    endpoint.with_ip_addr(*addr)
                })
        });
        Ok(Self {
            relay_service_node_id,
            relay_endpoint,
        })
    }
}

struct CertifyOptions<'a> {
    root_key: Option<&'a std::path::Path>,
    identity_root: &'a std::path::Path,
    identity_dir: Option<&'a std::path::Path>,
    device: &'a str,
    label: &'a str,
    capabilities: &'a [String],
    home_peer: Option<&'a OutOfBandHomePeer>,
    out: &'a std::path::Path,
}

fn certify(options: &CertifyOptions<'_>) -> anyhow::Result<()> {
    let CertifyOptions {
        root_key,
        identity_root,
        identity_dir,
        device,
        label,
        capabilities,
        home_peer,
        out,
    } = *options;
    let root: Box<dyn RootSigner> = match root_key {
        Some(path) => Box::new(
            tilo_user_identity::persist::read_owned_signing_key(path)
                .with_context(|| format!("read {}", path.display()))?,
        ),
        None => load_approval_root(identity_root, identity_dir)?,
    };
    let device_public_key: [u8; 32] = hex::decode(device.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("--device must be a 32-byte hex public key")?;
    let mut granted = DeviceCapabilities {
        can_own_workspaces: false,
        can_admit_devices: false,
        can_act_as_host: false,
        can_read_all_workspaces: false,
        foreign: false,
        can_administer_devices: false,
    };
    for capability in capabilities {
        match capability.as_str() {
            "own" => granted.can_own_workspaces = true,
            "admit" => granted.can_admit_devices = true,
            "host" => granted.can_act_as_host = true,
            "read-all" => granted.can_read_all_workspaces = true,
            "admin" => granted.can_administer_devices = true,
            other => {
                bail!("unknown capability {other}: expected own, admit, host, read-all or admin")
            }
        }
    }
    if home_peer.is_some() && !granted.can_act_as_host {
        bail!("only a device certified with the host capability can be a home peer");
    }
    let mut cert = DeviceCert {
        user_public_key: root.public_key(),
        device_public_key,
        device_label: label.into(),
        capabilities: granted,
        issued_at_ns: validity_start_ns(now_ns()?),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_root_cert(&mut cert, root.as_ref())?;
    if let Some(identity_dir) = identity_dir {
        let fleet = project_personal_device(identity_dir, &cert)?;
        commit_personal_device(identity_dir, cert.user_public_key, &fleet)?;
    }
    if let Some(home_peer) = home_peer {
        // The person's continuity shard, which every device of the person
        // dials, as a nearby approval registers it.
        register_home_peer(
            identity_root,
            &cert,
            home_peer.relay_service_node_id,
            &[tilo_relay::personal_continuity_workspace_id(
                &cert.user_public_key,
            )],
            home_peer.relay_endpoint.clone(),
        )?;
    }
    tilo_user_identity::persist::write_device_cert(out, &cert)?;
    Ok(())
}

fn install_cert(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    cert_path: &std::path::Path,
) -> anyhow::Result<()> {
    let cert = tilo_user_identity::persist::read_device_cert(cert_path)
        .with_context(|| format!("read {}", cert_path.display()))?;
    verify_device_cert_at(&cert, now_ns()?).context("the certificate does not verify")?;
    let paths = IdentityPaths::under(identity_root);
    let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
        .context("this device has no key; run device-key first")?;
    if device.verifying_key().to_bytes() != cert.device_public_key {
        bail!("the certificate names another device");
    }
    if let Ok(user) = tilo_user_identity::persist::read_public_key(&paths.user_pubkey)
        && user != cert.user_public_key
    {
        bail!("this device already belongs to another identity");
    }
    let record = install_personal_projection(
        identity_root,
        identity_dir,
        &out_of_band_payload(cert, &device)?,
    )?;
    println!("installed {}", record.display());
    Ok(())
}

fn out_of_band(command: OutOfBand) -> anyhow::Result<()> {
    match command {
        OutOfBand::DeviceKey { identity_root } => device_key(&identity_root),
        OutOfBand::Certify {
            root_key,
            identity_root,
            identity_dir,
            device,
            label,
            capabilities,
            home_peer_service,
            home_peer_addrs,
            out,
        } => {
            let home_peer = home_peer_service
                .map(|service| OutOfBandHomePeer::parse(&service, &home_peer_addrs))
                .transpose()?;
            certify(&CertifyOptions {
                root_key: root_key.as_deref(),
                identity_root: &identity_root,
                identity_dir: identity_dir.as_deref(),
                device: &device,
                label: &label,
                capabilities: &capabilities,
                home_peer: home_peer.as_ref(),
                out: &out,
            })
        }
        OutOfBand::InstallCert {
            identity_root,
            identity_dir,
            cert,
        } => install_cert(&identity_root, &identity_dir, &cert),
    }
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}

fn personal_device_requester_lease(
    headless: bool,
    requester_pid: Option<u32>,
    requester_start_time: Option<u64>,
) -> anyhow::Result<Option<tilo_relay::ProcessLease>> {
    match (headless, requester_pid, requester_start_time) {
        (true, None, None) => Ok(None),
        (true, _, _) => {
            bail!("--headless cannot be combined with --requester-pid or --requester-start-time")
        }
        (false, Some(pid), Some(start_time)) => {
            Ok(Some(tilo_relay::ProcessLease::from_parts(pid, start_time)))
        }
        (false, _, _) => bail!(
            "personal-device pairing requires --requester-pid and --requester-start-time unless --headless"
        ),
    }
}

fn pair_device_cli(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    label: &str,
    timeout: u64,
    status_file: Option<&std::path::Path>,
    headless: bool,
    requester_pid: Option<u32>,
    requester_start_time: Option<u64>,
) -> anyhow::Result<()> {
    let result = personal_device_requester_lease(headless, requester_pid, requester_start_time)
        .and_then(|requester_lease| {
            pair(
                JoinOptions {
                    identity_root,
                    label,
                    timeout,
                    kind: EnrollmentKind::PersonalDevice,
                    home_peer_output: None,
                    identity_dir: Some(identity_dir),
                    requester_lease,
                    port: NEARBY_PORT,
                },
                status_file,
            )
        });
    if let (Err(error), Some(path)) = (&result, status_file) {
        report_pairing_failure(path, error);
    }
    result
}

/// `tilo-enroll pair`: join the person's home peers from this machine.
fn pair_home_peer(
    identity_root: &std::path::Path,
    label: &str,
    timeout: u64,
    home_peer_output: &std::path::Path,
) -> anyhow::Result<()> {
    pair(
        JoinOptions {
            identity_root,
            label,
            timeout,
            kind: EnrollmentKind::HomePeer,
            home_peer_output: Some(home_peer_output),
            identity_dir: None,
            requester_lease: None,
            port: NEARBY_PORT,
        },
        None,
    )
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(uid) = cli.person_uid {
        let _ = PERSON_UID.set(uid);
    }
    match cli.command {
        Command::Discover { timeout } => discover(timeout, EnrollmentKind::HomePeer),
        Command::DiscoverDevice { timeout } => discover(timeout, EnrollmentKind::PersonalDevice),
        Command::Join {
            token_file,
            identity_root,
            label,
            timeout,
            home_peer_output,
        } => join(
            &token_file,
            &identity_root,
            &label,
            timeout,
            &home_peer_output,
        ),
        Command::Pair {
            identity_root,
            label,
            timeout,
            home_peer_output,
        } => pair_home_peer(&identity_root, &label, timeout, &home_peer_output),
        Command::PairDevice {
            identity_root,
            identity_dir,
            label,
            timeout,
            status_file,
            headless,
            requester_pid,
            requester_start_time,
        } => pair_device_cli(
            &identity_root,
            &identity_dir,
            &label,
            timeout,
            status_file.as_deref(),
            headless,
            requester_pid,
            requester_start_time,
        ),
        Command::Approve {
            identity_root,
            identity_dir,
            timeout,
            workspace_cache,
            acl,
            relay_workspace_ids,
            expected_device,
            expected_service,
        } => approve(&ApprovalOptions {
            identity_root: &identity_root,
            identity_dir: identity_dir.as_deref(),
            timeout,
            workspace_cache: &workspace_cache,
            acl: &acl,
            relay_workspace_ids: &relay_workspace_ids,
            expected_device: expected_device.as_deref(),
            expected_service: expected_service.as_deref(),
            kind: EnrollmentKind::HomePeer,
            port: NEARBY_PORT,
            ready: None,
            approved: None,
        }),
        Command::ApproveDevice {
            identity_root,
            identity_dir,
            timeout,
            expected_device,
            expected_service,
            approved_cert,
        } => approve(&ApprovalOptions {
            identity_root: &identity_root,
            identity_dir: Some(&identity_dir),
            timeout,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            relay_workspace_ids: &[],
            expected_device: expected_device.as_deref(),
            expected_service: expected_service.as_deref(),
            kind: EnrollmentKind::PersonalDevice,
            port: NEARBY_PORT,
            ready: None,
            approved: approved_cert
                .map(|path| tilo_user_identity::persist::read_device_cert(&path))
                .transpose()
                .context("read the approved certificate")?
                .as_ref(),
        }),
        Command::OutOfBand(command) => out_of_band(command),
    }
}

#[cfg(test)]
mod enroll_tests;
