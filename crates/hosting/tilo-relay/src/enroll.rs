//! LAN/OOB completion for a pre-authorised Tilo device enrollment token.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tilo_clock::try_unix_now_ns;
use tilo_relay::HomePeerRegistration;
use tilo_relay::home_peer_registry::{
    HomePeerAdmission, register_home_peer, valid_relay_endpoint_hint,
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationList, decode_enrollment_token,
    encode_enrollment_token, mint_enrollment_token, verify_device_cert, verify_device_cert_at,
    verify_enrollment_token, verify_revocation_entry,
};

const PORT: u16 = 45_831;
const MAX_PACKET: usize = 60 * 1024;
const RESPONSE_DOMAIN: &[u8] = b"tilo-host-enrollment-response-v1\0";
const PAIR_DOMAIN: &[u8] = b"tilo-nearby-home-peer-offer-v1\0";
/// Solicitation magic of devices that only read v1 pair offers.
const PAIR_MAGIC_V1: [u8; 8] = *b"TLOPAIR1";
/// Solicitation magic of devices that read v2 (HPKE) pair offers.
const PAIR_MAGIC_V2: [u8; 8] = *b"TLOPAIR2";

#[derive(Parser)]
#[command(name = "tilo-enroll")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
        /// PID of the compositor that initiated this first-use operation.
        #[arg(long)]
        requester_pid: u32,
        /// `/proc/<pid>/stat` start-time tick, preventing PID-reuse bypass.
        #[arg(long)]
        requester_start_time: u64,
    },
    /// Approve one nearby home server, signing its certificate with the
    /// personal identity in `--identity-dir`. Run it as that identity's owner.
    Approve {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        /// The personal identity directory holding the active record
        /// (`primary.json`), usually `~/.config/tilo/identity`.
        #[arg(long)]
        identity_dir: PathBuf,
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
    /// Pair this machine as a room host: a host that serves an identity's
    /// collaboration rooms (such as a public community room it was delegated)
    /// and nothing else. It receives a host certificate without workspace
    /// read authority and no Files home-peer configuration, so it can never
    /// stand in for a home peer, replicate Files, or federate as one.
    PairRoomHost {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Room host")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
    },
    /// Approve one nearby room host (see `pair-room-host`), signing its
    /// certificate with the personal identity in `--identity-dir`. Grants no
    /// room: each room it serves is delegated separately
    /// (`tilo-relay-admin community host`).
    ApproveRoomHost {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        /// The personal identity directory holding the active record
        /// (`primary.json`), usually `~/.config/tilo/identity`.
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
    },
    /// Approve one nearby ordinary device and give it the current fleet and
    /// home-peer registry needed to reconnect after this device goes offline.
    /// Signs with the personal identity in `--identity-dir`; run it as its owner.
    ApproveDevice {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        /// The personal identity directory holding the active record
        /// (`primary.json`) and this identity's device fleet.
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum EnrollmentKind {
    HomePeer,
    PersonalDevice,
    /// A host of an identity's collaboration rooms only. Appended to keep
    /// the wire discriminants of the other kinds.
    RoomHost,
}

impl EnrollmentKind {
    /// The certificate capabilities this kind is enrolled with.
    fn capabilities(self) -> DeviceCapabilities {
        match self {
            Self::HomePeer => DeviceCapabilities::host(),
            Self::PersonalDevice => DeviceCapabilities::member(),
            Self::RoomHost => DeviceCapabilities::room_host(),
        }
    }

    /// Whether an enrollment token grants exactly this kind's role. A token
    /// minted for one kind never enrolls another.
    fn accepts(self, capabilities: &DeviceCapabilities) -> bool {
        *capabilities == self.capabilities()
    }

    /// Kinds that end as a host certificate installed as the machine's own.
    fn is_host(self) -> bool {
        matches!(self, Self::HomePeer | Self::RoomHost)
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

/// The v1 envelope framing shared by the pair offer, request and response.
#[derive(Serialize, Deserialize)]
struct LegacyEnvelope {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
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

/// Wire version a peer spoke, which the answer mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WireVersion {
    V1,
    V2,
}

/// Where an enrollment response is sealed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseRoute {
    /// A v1 requester: its ephemeral X25519 key from the request envelope.
    V1 {
        requester_ephemeral_public_key: [u8; 32],
    },
    /// A v2 requester: the response key inside its sealed request.
    V2 { response_public_key: [u8; 32] },
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

#[derive(Serialize, Deserialize)]
struct Response {
    payload: ResponsePayload,
    signature: Vec<u8>,
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
    DefaultCert,
}

fn enrollment_commit_dir(identity_root: &std::path::Path) -> PathBuf {
    identity_root.join("enrollment-commits")
}

fn enrollment_stage_dir(identity_root: &std::path::Path) -> PathBuf {
    identity_root.join("enrollment-staging")
}

fn personal_marker_path(identity_root: &std::path::Path, nonce: [u8; 16]) -> PathBuf {
    enrollment_commit_dir(identity_root).join(format!("personal-{}.json", hex::encode(nonce)))
}

fn staged(path: &std::path::Path, name: &str) -> PathBuf {
    path.join(name)
}

fn fleet_path(identity_dir: &std::path::Path, user: [u8; 32]) -> PathBuf {
    identity_dir
        .join("fleets")
        .join(format!("{}.json", hex::encode(user)))
}

fn fleet_revokes_device(
    identity_dir: &std::path::Path,
    user: [u8; 32],
    device: [u8; 32],
) -> anyhow::Result<bool> {
    let bytes = std::fs::read(fleet_path(identity_dir, user))?;
    if bytes.len() > 1024 * 1024 {
        bail!("device fleet exceeds 1 MiB");
    }
    let fleet: FleetProjection =
        serde_json::from_slice(&bytes).context("device fleet is invalid")?;
    if fleet.schema_version != 1
        || fleet.identity_id != hex::encode(user)
        || fleet.certificates.len() > 256
        || fleet.revocations.len() > 256
        || fleet
            .certificates
            .iter()
            .any(|cert| cert.user_public_key != user || verify_device_cert(cert).is_err())
    {
        bail!("device fleet violates its bounded verified schema");
    }
    let mut revocations = RevocationList::new();
    for entry in fleet.revocations {
        if entry.user_public_key != user {
            bail!("device fleet contains a revocation from another identity");
        }
        revocations
            .insert(entry)
            .context("device fleet contains an invalid revocation")?;
    }
    Ok(revocations.is_revoked(&device))
}

/// Keep enrollment output owned by the already-validated identity directory
/// when the polkit broker launches this process as root.
fn restore_owner_from_directory(
    path: &std::path::Path,
    directory: &std::path::Path,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;

        let owner = std::fs::metadata(directory)?;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `path` is NUL-terminated and originates from a validated
        // identity/status path. The uid/gid come from its trusted directory.
        if unsafe { libc::chown(path.as_ptr(), owner.uid(), owner.gid()) } != 0 {
            return Err(anyhow::Error::from(std::io::Error::last_os_error())
                .context("restore enrollment output ownership"));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, directory);
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
    #[cfg(unix)]
    let owner = {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(&path)
            .or_else(|_| std::fs::metadata(identity_dir))
            .map(|metadata| (metadata.uid(), metadata.gid()))?
    };
    atomic_file::write_creating_parent(&path, fleet)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `path` is NUL-terminated and owner comes from the existing
        // user-owned fleet file or identity directory.
        if unsafe { libc::chown(path.as_ptr(), owner.0, owner.1) } != 0 {
            return Err(std::io::Error::last_os_error()).context("restore fleet ownership");
        }
    }
    Ok(())
}

fn bounded_home_peer_registry(
    identity_root: &std::path::Path,
    user: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let path = identity_root.join("home-peers.json");
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            let entries: Vec<HomePeerRegistration> = serde_json::from_slice(&bytes)?;
            if entries.len() > 128
                || entries.iter().any(|entry| {
                    entry.schema_version > HomePeerRegistration::CURRENT_SCHEMA
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
    entry.schema_version <= HomePeerRegistration::CURRENT_SCHEMA
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

    let peers_path = identity_root.join("home-peers.json");
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
    let stage_name = format!(
        "personal-{}-{}",
        hex::encode(payload.nonce),
        hex::encode(payload.cert.device_public_key)
    );
    let stage_dir = enrollment_stage_dir(identity_root).join(&stage_name);
    std::fs::create_dir_all(&stage_dir)?;
    let cert = tilo_user_identity::canonical::canonical_bytes(&payload.cert)?;
    atomic_file::write_with_mode(
        &staged(&stage_dir, "record.json"),
        &projection.identity,
        0o600,
    )?;
    atomic_file::write_with_mode(
        &staged(&stage_dir, "registry.json"),
        &projection.registry,
        0o600,
    )?;
    atomic_file::write(&staged(&stage_dir, "fleet.json"), &projection.fleet)?;
    atomic_file::write_with_mode(
        &staged(&stage_dir, "home-peers.json"),
        &projection.peers,
        0o644,
    )?;
    atomic_file::write(
        &staged(&stage_dir, "user-pubkey.bin"),
        &payload.cert.user_public_key,
    )?;
    atomic_file::write(&staged(&stage_dir, "per-user-cert.bin"), &cert)?;
    atomic_file::write(&staged(&stage_dir, "default-cert.bin"), &cert)?;
    let marker = PersonalEnrollmentMarker {
        schema_version: PERSONAL_ENROLLMENT_SCHEMA,
        identity_dir: identity_dir.to_path_buf(),
        stage_name,
        user_public_key: payload.cert.user_public_key,
        device_public_key: payload.cert.device_public_key,
        requester_lease,
        commit_ready: false,
    };
    let marker_path = personal_marker_path(identity_root, payload.nonce);
    std::fs::create_dir_all(enrollment_commit_dir(identity_root))?;
    atomic_file::write_with_mode(&marker_path, &serde_json::to_vec_pretty(&marker)?, 0o600)?;
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
    atomic_file::write_with_mode(marker_path, &serde_json::to_vec_pretty(&marker)?, 0o600)?;
    Ok(marker)
}

fn commit_staged_personal_enrollment(
    identity_root: &std::path::Path,
    paths: &IdentityPaths,
    marker_path: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
    mut checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    if marker.schema_version != PERSONAL_ENROLLMENT_SCHEMA
        || !marker.commit_ready
        || marker.stage_name.contains('/')
        || marker.stage_name.contains('\\')
    {
        bail!("personal enrollment marker is invalid");
    }
    let stage_dir = enrollment_stage_dir(identity_root).join(&marker.stage_name);
    let cert_bytes = std::fs::read(staged(&stage_dir, "default-cert.bin"))?;
    let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&cert_bytes)?;
    if cert.user_public_key != marker.user_public_key
        || cert.device_public_key != marker.device_public_key
        || verify_device_cert_at(&cert, now_ns()?).is_err()
        || std::fs::read(staged(&stage_dir, "user-pubkey.bin"))? != marker.user_public_key
    {
        bail!("staged personal enrollment no longer validates");
    }
    let owner_dir = if marker.identity_dir.exists() {
        marker.identity_dir.clone()
    } else {
        marker
            .identity_dir
            .parent()
            .context("identity directory has no parent")?
            .to_path_buf()
    };
    // The first callback is the requester-liveness gate in production. Keep
    // it before even creating the user identity directory so a compositor
    // that died while approval was in flight causes no publication mutation.
    checkpoint(PersonalCommitCheckpoint::Record)?;
    std::fs::create_dir_all(&marker.identity_dir)?;
    let record = identity_record_path(&marker.identity_dir, marker.user_public_key);
    let fleet = fleet_path(&marker.identity_dir, marker.user_public_key);
    atomic_file::write_with_mode(
        &record,
        &std::fs::read(staged(&stage_dir, "record.json"))?,
        0o600,
    )?;
    checkpoint(PersonalCommitCheckpoint::Fleet)?;
    atomic_file::write_creating_parent(&fleet, &std::fs::read(staged(&stage_dir, "fleet.json"))?)?;
    checkpoint(PersonalCommitCheckpoint::HomePeers)?;
    atomic_file::write_with_mode(
        &identity_root.join("home-peers.json"),
        &std::fs::read(staged(&stage_dir, "home-peers.json"))?,
        0o644,
    )?;
    checkpoint(PersonalCommitCheckpoint::Registry)?;
    atomic_file::write_with_mode(
        &marker.identity_dir.join("registry.json"),
        &std::fs::read(staged(&stage_dir, "registry.json"))?,
        0o600,
    )?;
    checkpoint(PersonalCommitCheckpoint::UserPublicKey)?;
    atomic_file::write(&paths.user_pubkey, &marker.user_public_key)?;
    checkpoint(PersonalCommitCheckpoint::PerUserCert)?;
    atomic_file::write_creating_parent(
        &paths.device_cert_for_user(&marker.user_public_key),
        &std::fs::read(staged(&stage_dir, "per-user-cert.bin"))?,
    )?;
    // Publish this last: every projection file has been durably installed.
    checkpoint(PersonalCommitCheckpoint::DefaultCert)?;
    atomic_file::write(&paths.device_cert, &cert_bytes)?;
    // A polkit-launched enrollment may have written these as root. Restore
    // the identity owner's uid/gid before the marker is cleared, so recovery
    // repeats this step if it is interrupted.
    for path in [
        marker.identity_dir.join("fleets"),
        record.clone(),
        fleet,
        marker.identity_dir.join("registry.json"),
    ] {
        restore_owner_from_directory(&path, &owner_dir)?;
    }
    for path in [
        identity_root.join("home-peers.json"),
        paths.user_pubkey.clone(),
        paths.device_cert_for_user(&marker.user_public_key),
        paths.device_cert.clone(),
    ] {
        restore_owner_from_directory(&path, identity_root)?;
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

#[cfg(test)]
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

fn response_preimage(payload: &ResponsePayload) -> anyhow::Result<Vec<u8>> {
    let mut preimage = RESPONSE_DOMAIN.to_vec();
    preimage.extend(postcard::to_stdvec(payload)?);
    Ok(preimage)
}

/// The user root an approval signs with. Production approval signs only
/// through the owner's personal identity record; no root key file is read.
trait RootSigner {
    fn root_public_key(&self) -> [u8; 32];
    fn root_sign(&self, message: &[u8]) -> anyhow::Result<[u8; 64]>;
}

impl RootSigner for runtime_identity::SigningIdentity {
    fn root_public_key(&self) -> [u8; 32] {
        self.group_public_key()
    }

    fn root_sign(&self, message: &[u8]) -> anyhow::Result<[u8; 64]> {
        self.sign(message)
            .map_err(|error| anyhow::anyhow!("the personal identity could not sign: {error}"))
    }
}

/// Sign `cert` with the user root and check the result verifies.
fn sign_cert_with_root(cert: &mut DeviceCert, root: &impl RootSigner) -> anyhow::Result<()> {
    if cert.user_public_key != root.root_public_key() {
        bail!("device certificate names a different identity than its signer");
    }
    let digest = tilo_user_identity::device_cert_signing_digest(cert)?;
    cert.signature = root.root_sign(&digest)?;
    verify_device_cert(cert).context("signed device certificate does not verify")?;
    Ok(())
}

fn sign_response(payload: ResponsePayload, root: &impl RootSigner) -> anyhow::Result<Response> {
    let signature = root.root_sign(&response_preimage(&payload)?)?.to_vec();
    Ok(Response { payload, signature })
}

fn verify_response(response: &Response, user_public_key: [u8; 32]) -> anyhow::Result<()> {
    let signature: [u8; 64] = response
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("enrollment response signature has the wrong length"))?;
    VerifyingKey::from_bytes(&user_public_key)?
        .verify(
            &response_preimage(&response.payload)?,
            &Signature::from_bytes(&signature),
        )
        .context("enrollment response was changed in transit")
}

fn pairing_code(device_public_key: [u8; 32], relay_service_node_id: [u8; 32]) -> String {
    let mut input = PAIR_DOMAIN.to_vec();
    input.extend(device_public_key);
    input.extend(relay_service_node_id);
    let digest = blake3::hash(&input);
    let [first, second, third, fourth, ..] = *digest.as_bytes();
    let number = u32::from_le_bytes([first, second, third, fourth]);
    format!("{:06}", number % 1_000_000)
}

/// The pair-offer version a solicitation asks for.
fn solicitation_version(solicitation: &PairSolicitation) -> Option<WireVersion> {
    match solicitation.magic {
        PAIR_MAGIC_V1 => Some(WireVersion::V1),
        PAIR_MAGIC_V2 => Some(WireVersion::V2),
        _ => None,
    }
}

fn valid_solicitation(solicitation: &PairSolicitation) -> bool {
    solicitation_version(solicitation).is_some()
        && VerifyingKey::from_bytes(&solicitation.device_public_key).is_ok()
        && solicitation.relay_service_node_id != [0; 32]
        && tilo_relay::valid_home_peer_label(&solicitation.label)
}

/// Bind the well-known LAN-discovery port without making an already-running
/// discovery view or approval sheet prevent another Tilo surface from joining
/// the same nearby-device flow.  UDP broadcast is deliberately delivered to
/// every reuse-port listener; each recipient still has to authenticate the
/// encrypted offer or enrollment request before it can act on it.
fn bind_pair_listener() -> anyhow::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())?;
    Ok(socket.into())
}

fn discover(timeout: u64, kind: EnrollmentKind) -> anyhow::Result<()> {
    let socket = bind_pair_listener()?;
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

// Wire versions of the three sealed envelopes (pair offer, request, response).
//
// v1 (legacy): `postcard({ ephemeral_public_key: [u8; 32], nonce: [u8; 12],
// ciphertext })` = `epk(32) || nonce(12) || varint(len) || ciphertext+tag`,
// an ad-hoc X25519 agreement hashed with BLAKE3 into a ChaCha20-Poly1305
// key. Readers still accept it, and a responder answers a v1 peer in v1
// during the transition.
//
// v2: `"TLOENRL" || 0x02 || tilo-seal HPKE sealed box` (RFC 9180 base mode,
// X25519-HKDF-SHA256 / ChaCha20-Poly1305), with one tilo-seal label per
// step so an envelope for one step never opens as another.
//
// Discrimination: a v2 envelope always starts with the 8-byte prefix
// `TLOENRL\x02`; a v1 envelope starts with its sender's random ephemeral
// X25519 public key. Bytes with the v2 prefix are parsed only as v2, all
// other bytes only as v1, so no envelope is ever tried under both parsers.
// This build's v1 writer re-draws any ephemeral key whose encoding would
// begin with the prefix, so nothing it writes is ambiguous; an old peer's
// random key starts with it with probability 2^-64, and that envelope then
// merely fails to open.
const ENVELOPE_MAGIC: &[u8; 7] = b"TLOENRL";
const ENVELOPE_V2: u8 = 2;
const REQUEST_KEY_DOMAIN_V1: &[u8] = b"tilo-host-enrollment-request-key-v1\0";
const RESPONSE_KEY_DOMAIN_V1: &[u8] = b"tilo-host-enrollment-key-v1\0";

fn v2_header() -> [u8; 8] {
    let [m0, m1, m2, m3, m4, m5, m6] = *ENVELOPE_MAGIC;
    [m0, m1, m2, m3, m4, m5, m6, ENVELOPE_V2]
}

/// The v2 sealed box inside `bytes`, or `None` when `bytes` is not v2.
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

/// A fresh ephemeral key for a v1 envelope whose encoding cannot be
/// mistaken for the v2 prefix.
fn v1_ephemeral() -> anyhow::Result<(tilo_seal::SecretKey, [u8; 32])> {
    for _ in 0..8 {
        let secret = tilo_seal::SecretKey::generate()?;
        let public = secret.public_key()?.to_bytes();
        if !public.starts_with(ENVELOPE_MAGIC) {
            return Ok((secret, public));
        }
    }
    bail!("could not draw an unambiguous ephemeral key")
}

/// Open the v1 framing shared by all three legacy envelopes. `context`
/// lists the key-derivation inputs after the shared secret, given the
/// sender's ephemeral public key.
fn open_v1(
    bytes: &[u8],
    secret: &tilo_seal::SecretKey,
    domain: &[u8],
    context: impl FnOnce(&[u8; 32]) -> Vec<Vec<u8>>,
    aad: impl FnOnce(&[u8; 32]) -> Vec<u8>,
) -> anyhow::Result<(zeroize::Zeroizing<Vec<u8>>, [u8; 32])> {
    let envelope: LegacyEnvelope = postcard::from_bytes(bytes)?;
    let context = context(&envelope.ephemeral_public_key);
    let parts: Vec<&[u8]> = context.iter().map(Vec::as_slice).collect();
    let key = tilo_seal::legacy::blake3_x25519_key(
        secret,
        &tilo_seal::PublicKey::from_bytes(envelope.ephemeral_public_key),
        domain,
        &parts,
    )?;
    let plaintext = tilo_seal::legacy::open_chacha20poly1305(
        &key,
        &envelope.nonce,
        &aad(&envelope.ephemeral_public_key),
        &envelope.ciphertext,
    )?;
    Ok((plaintext, envelope.ephemeral_public_key))
}

/// Seal in the v1 framing, only to answer a peer that spoke v1.
fn seal_v1_for_transition(
    peer: &tilo_seal::PublicKey,
    domain: &[u8],
    context: impl FnOnce(&[u8; 32]) -> Vec<Vec<u8>>,
    aad: &[u8],
    plaintext: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let (ephemeral, ephemeral_public_key) = v1_ephemeral()?;
    let context = context(&ephemeral_public_key);
    let parts: Vec<&[u8]> = context.iter().map(Vec::as_slice).collect();
    let key = tilo_seal::legacy::blake3_x25519_key(&ephemeral, peer, domain, &parts)?;
    let (nonce, ciphertext) =
        tilo_seal::legacy::seal_chacha20poly1305_for_transition(&key, aad, plaintext)?;
    Ok(postcard::to_stdvec(&LegacyEnvelope {
        ephemeral_public_key,
        nonce,
        ciphertext,
    })?)
}

fn pair_offer_aad(recipient: [u8; 32]) -> Vec<u8> {
    [PAIR_DOMAIN, &recipient].concat()
}

/// Seal an enrollment token to the soliciting device, in the offer version
/// its solicitation asked for.
fn encrypt_pair_offer(
    token: &str,
    recipient: [u8; 32],
    version: WireVersion,
) -> anyhow::Result<Vec<u8>> {
    let recipient_public = device_sealing_public(recipient)?;
    let aad = pair_offer_aad(recipient);
    match version {
        WireVersion::V2 => Ok(wrap_v2(&tilo_seal::hpke::seal(
            &recipient_public,
            tilo_seal::Label::RelayEnrollOffer,
            &aad,
            token.as_bytes(),
        )?)),
        WireVersion::V1 => seal_v1_for_transition(
            &recipient_public,
            PAIR_DOMAIN,
            |ephemeral| vec![recipient.to_vec(), ephemeral.to_vec()],
            &aad,
            token.as_bytes(),
        ),
    }
}

fn decrypt_pair_offer(
    bytes: &[u8],
    recipient: &ed25519_dalek::SigningKey,
) -> anyhow::Result<String> {
    let recipient_public = recipient.verifying_key().to_bytes();
    let secret = device_sealing_secret(recipient);
    let aad = pair_offer_aad(recipient_public);
    let plaintext = match v2_body(bytes) {
        Some(body) => {
            tilo_seal::hpke::open_bytes(&secret, tilo_seal::Label::RelayEnrollOffer, &aad, body)
                .map_err(anyhow::Error::from)
        }
        None => open_v1(
            bytes,
            &secret,
            PAIR_DOMAIN,
            |ephemeral| vec![recipient_public.to_vec(), ephemeral.to_vec()],
            |_| aad.clone(),
        )
        .map(|(plaintext, _)| plaintext),
    }
    .map_err(|_| anyhow::anyhow!("nearby pairing offer was not encrypted for this server"))?;
    String::from_utf8(plaintext.to_vec()).context("nearby pairing offer is not text")
}

fn response_aad(token_nonce: [u8; 16], recipient: [u8; 32]) -> Vec<u8> {
    let mut aad = RESPONSE_DOMAIN.to_vec();
    aad.extend(token_nonce);
    aad.extend(recipient);
    aad
}

/// Seal an enrollment request to the issuing device (always v2). Returns
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

/// Open an enrollment request of either wire version, returning where its
/// response must be sealed to (which also fixes the response's version).
fn decrypt_request(
    bytes: &[u8],
    issuer_device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<(Request, ResponseRoute)> {
    let secret = device_sealing_secret(issuer_device);
    let not_for_us = || anyhow::anyhow!("enrollment request was not encrypted for this issuer");
    if let Some(body) = v2_body(bytes) {
        let plaintext = tilo_seal::hpke::open_bytes(
            &secret,
            tilo_seal::Label::RelayEnrollRequest,
            &v2_header(),
            body,
        )
        .map_err(|_| not_for_us())?;
        let decoded: RequestV2Owned = postcard::from_bytes(&plaintext)?;
        return Ok((
            decoded.request,
            ResponseRoute::V2 {
                response_public_key: decoded.response_public_key,
            },
        ));
    }
    let (plaintext, ephemeral_public_key) = open_v1(
        bytes,
        &secret,
        REQUEST_KEY_DOMAIN_V1,
        |ephemeral| vec![ephemeral.to_vec()],
        |ephemeral| ephemeral.to_vec(),
    )
    .map_err(|_| not_for_us())?;
    Ok((
        postcard::from_bytes(&plaintext)?,
        ResponseRoute::V1 {
            requester_ephemeral_public_key: ephemeral_public_key,
        },
    ))
}

/// Seal the signed response to the requester, mirroring its wire version.
fn encrypt_response(
    response: &Response,
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    route: ResponseRoute,
) -> anyhow::Result<Vec<u8>> {
    let plaintext = zeroize::Zeroizing::new(postcard::to_stdvec(response)?);
    let aad = response_aad(token_nonce, recipient);
    match route {
        ResponseRoute::V2 {
            response_public_key,
        } => Ok(wrap_v2(&tilo_seal::hpke::seal(
            &tilo_seal::PublicKey::from_bytes(response_public_key),
            tilo_seal::Label::RelayEnrollResponse,
            &aad,
            &plaintext,
        )?)),
        ResponseRoute::V1 {
            requester_ephemeral_public_key,
        } => seal_v1_for_transition(
            &tilo_seal::PublicKey::from_bytes(requester_ephemeral_public_key),
            RESPONSE_KEY_DOMAIN_V1,
            |_| vec![token_nonce.to_vec(), recipient.to_vec()],
            &aad,
            &plaintext,
        ),
    }
}

fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    response_secret: &tilo_seal::SecretKey,
) -> anyhow::Result<Response> {
    let aad = response_aad(token_nonce, recipient);
    let plaintext = match v2_body(bytes) {
        Some(body) => tilo_seal::hpke::open_bytes(
            response_secret,
            tilo_seal::Label::RelayEnrollResponse,
            &aad,
            body,
        )
        .map_err(anyhow::Error::from),
        None => open_v1(
            bytes,
            response_secret,
            RESPONSE_KEY_DOMAIN_V1,
            |_| vec![token_nonce.to_vec(), recipient.to_vec()],
            |_| aad.clone(),
        )
        .map(|(plaintext, _)| plaintext),
    }
    .map_err(|_| anyhow::anyhow!("enrollment response was not encrypted for this device"))?;
    postcard::from_bytes(&plaintext).map_err(Into::into)
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
    root: &impl RootSigner,
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    Ok(postcard::to_stdvec(&Finalized {
        payload,
        signature: root.root_sign(&encoded)?.to_vec(),
    })?)
}

fn verify_finalized(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    user_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let finalized: Finalized = postcard::from_bytes(bytes)?;
    if finalized.payload.nonce != nonce
        || finalized.payload.device_public_key != device_public_key
        || finalized.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment completion did not match this exchange");
    }
    let signature: [u8; 64] = finalized
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("completion signature has the wrong length"))?;
    VerifyingKey::from_bytes(&user_public_key)?.verify(
        &postcard::to_stdvec(&finalized.payload)?,
        &Signature::from_bytes(&signature),
    )?;
    Ok(())
}

fn consume_nonce(
    identity_root: &std::path::Path,
    nonce: &[u8; 16],
    device_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let dir = identity_root.join("enrollment-nonces");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(hex::encode(nonce));
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read_to_string(path)?;
            if existing.trim() == hex::encode(device_public_key) {
                return Ok(());
            }
            bail!("enrollment token has already been consumed by another device");
        }
        Err(error) => return Err(error.into()),
    };
    file.write_all(hex::encode(device_public_key).as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn now_ns() -> anyhow::Result<u64> {
    try_unix_now_ns().context("system clock precedes the Unix epoch")
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
        identity_root,
        label,
        timeout,
        EnrollmentKind::HomePeer,
        Some(home_peer_output),
        None,
        None,
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the join state machine keeps all authenticated inputs visible at its single protocol boundary"
)]
fn join_with_token(
    token_text: &str,
    identity_root: &std::path::Path,
    label: &str,
    timeout: u64,
    kind: EnrollmentKind,
    home_peer_output: Option<&std::path::Path>,
    identity_dir: Option<&std::path::Path>,
    requester_lease: Option<tilo_relay::ProcessLease>,
) -> anyhow::Result<()> {
    validate_requester(requester_lease)?;
    let token = decode_enrollment_token(token_text).context("decode enrollment token")?;
    verify_enrollment_token(&token, now_ns()?).context("verify enrollment token")?;
    if !kind.accepts(&token.capabilities) {
        bail!("enrollment token grants the wrong device role");
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
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT);
    let mut reply = vec![0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        socket.send_to(&bytes, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, sender)) => {
                let response_bytes = received_packet(&reply, len)?.to_vec();
                validate_requester(requester_lease)?;
                let Ok(response) = decrypt_response(
                    &response_bytes,
                    token.nonce,
                    device.verifying_key().to_bytes(),
                    &request_ephemeral,
                ) else {
                    continue;
                };
                if verify_response(&response, token.user_public_key).is_err() {
                    continue;
                }
                if response.payload.nonce != token.nonce
                    || response.payload.recipient_device_public_key != request.device_public_key
                    || response.payload.relay_service_node_id != request.relay_service_node_id
                    || response.payload.cert.device_public_key != request.device_public_key
                    || response.payload.cert.user_public_key != token.user_public_key
                    || response.payload.cert.capabilities != token.capabilities
                {
                    continue;
                }
                verify_device_cert_at(&response.payload.cert, now_ns()?)
                    .context("verify enrolled device certificate validity")?;
                if kind == EnrollmentKind::HomePeer {
                    let home_peer_toml = response
                        .payload
                        .home_peer_toml
                        .as_ref()
                        .context("approving device omitted home-peer configuration")?;
                    let _: fs::full_replica::HomePeerSet = toml::from_str(home_peer_toml)
                        .context("approving device returned an invalid home-peer configuration")?;
                    atomic_file::write(
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
                        &response.payload,
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
                                token.user_public_key,
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
                                        atomic_file::write(
                                            &paths.user_pubkey,
                                            &response.payload.cert.user_public_key,
                                        )?;
                                        None
                                    };
                                // Publish the default cert as the completion
                                // marker only after the approver durably registers us,
                                // authenticates Finalized, and the member projection is
                                // fully installed. A crash can therefore only leave a
                                // retryable partial enrollment, never a bootable one.
                                if kind.is_host() {
                                    tilo_user_identity::persist::write_device_cert(
                                        &paths.device_cert_for_user(
                                            &response.payload.cert.user_public_key,
                                        ),
                                        &response.payload.cert,
                                    )?;
                                    tilo_user_identity::persist::write_device_cert(
                                        &paths.device_cert,
                                        &response.payload.cert,
                                    )?;
                                }
                                for directory in [
                                    paths.user_pubkey.parent(),
                                    paths.device_privkey.parent(),
                                    paths
                                        .device_cert_for_user(
                                            &response.payload.cert.user_public_key,
                                        )
                                        .parent(),
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    restore_owner_from_directory(directory, identity_root)?;
                                }
                                for output in [
                                    &paths.user_pubkey,
                                    &paths.device_privkey,
                                    &paths.device_cert_for_user(
                                        &response.payload.cert.user_public_key,
                                    ),
                                    &paths.device_cert,
                                ] {
                                    restore_owner_from_directory(output, identity_root)?;
                                }
                                consume_nonce(
                                    identity_root,
                                    &token.nonce,
                                    device.verifying_key().to_bytes(),
                                )?;
                                if kind == EnrollmentKind::PersonalDevice {
                                    println!(
                                        "{}",
                                        serde_json::json!({
                                            "identity": hex::encode(response.payload.cert.user_public_key),
                                            "record_path": record_path.context("record path is required")?,
                                            "label": response.payload.cert.device_label,
                                        })
                                    );
                                } else {
                                    println!("enrolled {}", response.payload.cert.device_label);
                                }
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
    bail!("no approving Tilo device answered before the enrollment window closed")
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the pairing state machine keeps its UI outputs and authenticated requester lease at one boundary"
)]
fn pair(
    identity_root: &std::path::Path,
    label: &str,
    timeout: u64,
    kind: EnrollmentKind,
    home_peer_output: Option<&std::path::Path>,
    identity_dir: Option<&std::path::Path>,
    status_file: Option<&std::path::Path>,
    requester_lease: Option<tilo_relay::ProcessLease>,
) -> anyhow::Result<()> {
    if kind == EnrollmentKind::PersonalDevice && requester_lease.is_none() {
        bail!("personal-device pairing requires a live first-use requester");
    }
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
        magic: PAIR_MAGIC_V2,
        device_public_key: device.verifying_key().to_bytes(),
        relay_service_node_id,
        label: label.clone(),
        kind,
    })?;
    let code = pairing_code(device.verifying_key().to_bytes(), relay_service_node_id);
    if let Some(path) = status_file {
        atomic_file::write_with_mode(
            path,
            &serde_json::to_vec(&serde_json::json!({
                "state": "waiting_for_approval",
                "verification_code": code,
                "label": label,
            }))?,
            0o644,
        )?;
        if let Some(parent) = path.parent() {
            restore_owner_from_directory(path, parent)?;
        }
    }
    println!("Nearby pairing code: {code}");
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT);
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
                    || !kind.accepts(&token.capabilities)
                {
                    continue;
                }
                println!("Nearby Tilo device approved code {code}");
                join_with_token(
                    token_text.trim(),
                    identity_root,
                    &label,
                    timeout,
                    kind,
                    home_peer_output,
                    identity_dir,
                    requester_lease,
                )?;
                if let Some(path) = status_file {
                    atomic_file::write_with_mode(
                        path,
                        &serde_json::to_vec(&serde_json::json!({
                            "state": "complete",
                            "verification_code": code,
                        }))?,
                        0o644,
                    )?;
                    if let Some(parent) = path.parent() {
                        restore_owner_from_directory(path, parent)?;
                    }
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
    bail!("no trusted Tilo device approved nearby pairing before the window closed")
}

struct ApprovalOptions<'a> {
    identity_root: &'a std::path::Path,
    identity_dir: &'a std::path::Path,
    timeout: u64,
    workspace_cache: &'a std::path::Path,
    acl: &'a std::path::Path,
    relay_workspace_ids: &'a [u64],
    expected_device: Option<&'a str>,
    expected_service: Option<&'a str>,
    kind: EnrollmentKind,
    ready: Option<&'a std::sync::mpsc::Sender<()>>,
}

struct ApprovalIdentity {
    root: runtime_identity::SigningIdentity,
    device: ed25519_dalek::SigningKey,
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
    signed: Response,
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

fn load_approval_identity(options: &ApprovalOptions<'_>) -> anyhow::Result<ApprovalIdentity> {
    let paths = IdentityPaths::under(options.identity_root);
    let root = runtime_identity::load_root_signer(options.identity_dir)
        .context("this device cannot sign with the identity root needed to approve enrollment")?;
    let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let user_public_key = root.group_public_key();
    let issuer_cert = tilo_user_identity::persist::read_device_cert(
        &paths.device_cert_for_user(&user_public_key),
    )
    .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))?;
    verify_device_cert_at(&issuer_cert, now_ns()?)
        .context("verify approving device certificate validity")?;
    if issuer_cert.device_public_key != device.verifying_key().to_bytes()
        || !issuer_cert.capabilities.can_admit_devices
    {
        bail!("this device is not authorised to approve enrollment");
    }
    if fleet_revokes_device(
        options.identity_dir,
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
        let issued_at_ns = now_ns()?;
        let expires_at_ns = issued_at_ns.saturating_add(5 * 60 * 1_000_000_000);
        let capabilities = kind.capabilities();
        let token = mint_enrollment_token(
            identity.user_public_key,
            capabilities,
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
    if let (Some(selection), Some(version)) = (selected, solicitation_version(solicitation)) {
        // Each solicitation is answered in the offer version it asked for.
        let offer = encrypt_pair_offer(
            &selection.token_text,
            selection.solicitation.device_public_key,
            version,
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
        && kind.accepts(&token.capabilities)
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

fn approval_projection(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    cert: &DeviceCert,
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
            let capabilities = serde_json::from_str(&capability_json)
                .context("filesystem capability export is invalid")?;
            let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign_with(
                ticket.workspace_id(),
                1,
                capabilities,
                false,
                identity.user_public_key,
                |payload| identity.root.root_sign(payload),
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
        // A room host takes no Files workspace, authority envelope or
        // registry: its only authority is each room's own delegation.
        EnrollmentKind::RoomHost => Ok(ApprovalProjection {
            home_peer_toml: None,
            public_identity_json: None,
            fleet_json: None,
            home_peers_json: None,
        }),
        EnrollmentKind::PersonalDevice => {
            let identity_dir = options.identity_dir;
            Ok(ApprovalProjection {
                home_peer_toml: None,
                public_identity_json: Some(public_identity_record(
                    identity.user_public_key,
                    "My Tilo identity",
                )?),
                fleet_json: Some(project_personal_device(identity_dir, cert)?),
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
    let mut cert = DeviceCert {
        user_public_key: identity.user_public_key,
        device_public_key: pending.request.device_public_key,
        device_label: pending.request.label.clone(),
        capabilities: pending.token.capabilities.clone(),
        issued_at_ns: now_ns()?,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_cert_with_root(&mut cert, &identity.root)?;
    let projection = approval_projection(options, identity, &cert)?;
    let signed = sign_response(
        ResponsePayload {
            nonce: pending.token.nonce,
            recipient_device_public_key: pending.request.device_public_key,
            relay_service_node_id: pending.request.relay_service_node_id,
            cert,
            home_peer_toml: projection.home_peer_toml,
            public_identity_json: projection.public_identity_json,
            fleet_json: projection.fleet_json,
            home_peers_json: projection.home_peers_json,
        },
        &identity.root,
    )?;
    let encrypted = encrypt_response(
        &signed,
        pending.token.nonce,
        pending.request.device_public_key,
        pending.response_route,
    )?;
    Ok(PreparedApproval { signed, encrypted })
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
    if options.kind == EnrollmentKind::PersonalDevice {
        commit_personal_device(
            options.identity_dir,
            identity.user_public_key,
            prepared
                .signed
                .payload
                .fleet_json
                .as_deref()
                .context("fleet is required")?,
        )?;
    }
    // The approver's single-use ledger belongs to the approving identity, so
    // approval needs no write access to the machine-wide identity root.
    consume_nonce(
        options.identity_dir,
        &pending.token.nonce,
        pending.request.device_public_key,
    )?;
    let finalized = make_finalized(
        &prepared.encrypted,
        pending.token.nonce,
        pending.request.device_public_key,
        &identity.root,
    )?;
    for _ in 0..8 {
        socket.send_to(&finalized, pending.sender)?;
        std::thread::sleep(Duration::from_millis(125));
    }
    println!(
        "approved nearby device {}",
        hex::encode(prepared.signed.payload.cert.device_public_key)
    );
    Ok(())
}

/// The privileged writer of a machine-wide identity root's home-peer registry.
const HOME_PEER_CONTROL: &str = "/usr/libexec/tilo-home-peer-control";

/// Record an approved home peer. An identity root this user can write is
/// updated directly; the machine-wide one only through the privileged
/// helper, which receives the finished root-signed admission and verifies it.
fn record_home_peer(
    identity_root: &std::path::Path,
    admission: &HomePeerAdmission,
) -> anyhow::Result<()> {
    if directory_writable(identity_root) {
        return register_home_peer(
            identity_root,
            &admission.cert,
            admission.relay_service_node_id,
            &admission.relay_workspace_ids,
        );
    }
    let mut command = std::process::Command::new("pkexec");
    command
        .args([HOME_PEER_CONTROL, "register-home-peer", "--identity-root"])
        .arg(identity_root);
    let output = bounded_command::run_prepared(
        command,
        Some(&serde_json::to_vec(admission)?),
        64 * 1024,
        Duration::from_secs(120),
    )
    .context("start the home-peer registry helper")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} did not record the home server ({}): {}",
            identity_root.join("home-peers.json").display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn directory_writable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for this call.
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

fn approve(options: &ApprovalOptions<'_>) -> anyhow::Result<()> {
    let identity = load_approval_identity(options)?;
    let socket = bind_pair_listener()?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    if let Some(ready) = options.ready {
        let _ = ready.send(());
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
    if options.kind == EnrollmentKind::HomePeer {
        // Registered before the signed response is sent: the requester keeps
        // asking for the whole join window, which leaves room for an
        // authorization prompt, while the completion window after its
        // acknowledgement is only seconds.
        record_home_peer(
            options.identity_root,
            &HomePeerAdmission {
                cert: prepared.signed.payload.cert.clone(),
                relay_service_node_id: prepared.signed.payload.relay_service_node_id,
                relay_workspace_ids: identity.relay_workspace_ids.clone(),
            },
        )?;
    }
    await_approval_ack(
        &session,
        &mut packet,
        &pending,
        &prepared,
        socket_timeout(options.timeout),
    )?;
    commit_approval(&session, options, &identity, &pending, &prepared)
}

/// [`approve`] for a kind that shares no Files workspace: a personal device
/// or a room host.
fn approve_without_files(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    timeout: u64,
    (expected_device, expected_service): (Option<&str>, Option<&str>),
    kind: EnrollmentKind,
) -> anyhow::Result<()> {
    approve(&ApprovalOptions {
        identity_root,
        identity_dir,
        timeout,
        workspace_cache: std::path::Path::new(""),
        acl: std::path::Path::new(""),
        relay_workspace_ids: &[],
        expected_device,
        expected_service,
        kind,
        ready: None,
    })
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}

#[allow(
    clippy::too_many_lines,
    reason = "one dispatcher arm per subcommand, each only unpacking its arguments"
)]
fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
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
        } => pair(
            &identity_root,
            &label,
            timeout,
            EnrollmentKind::HomePeer,
            Some(&home_peer_output),
            None,
            None,
            None,
        ),
        Command::PairDevice {
            identity_root,
            identity_dir,
            label,
            timeout,
            status_file,
            requester_pid,
            requester_start_time,
        } => pair(
            &identity_root,
            &label,
            timeout,
            EnrollmentKind::PersonalDevice,
            None,
            Some(&identity_dir),
            status_file.as_deref(),
            Some(tilo_relay::ProcessLease::from_parts(
                requester_pid,
                requester_start_time,
            )),
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
            identity_dir: &identity_dir,
            timeout,
            workspace_cache: &workspace_cache,
            acl: &acl,
            relay_workspace_ids: &relay_workspace_ids,
            expected_device: expected_device.as_deref(),
            expected_service: expected_service.as_deref(),
            kind: EnrollmentKind::HomePeer,
            ready: None,
        }),
        Command::PairRoomHost {
            identity_root,
            label,
            timeout,
        } => pair(
            &identity_root,
            &label,
            timeout,
            EnrollmentKind::RoomHost,
            None,
            None,
            None,
            None,
        ),
        Command::ApproveRoomHost {
            identity_root,
            identity_dir,
            timeout,
            expected_device,
            expected_service,
        } => approve_without_files(
            &identity_root,
            &identity_dir,
            timeout,
            (expected_device.as_deref(), expected_service.as_deref()),
            EnrollmentKind::RoomHost,
        ),
        Command::ApproveDevice {
            identity_root,
            identity_dir,
            timeout,
            expected_device,
            expected_service,
        } => approve_without_files(
            &identity_root,
            &identity_dir,
            timeout,
            (expected_device.as_deref(), expected_service.as_deref()),
            EnrollmentKind::PersonalDevice,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };

    impl RootSigner for ed25519_dalek::SigningKey {
        fn root_public_key(&self) -> [u8; 32] {
            self.verifying_key().to_bytes()
        }

        fn root_sign(&self, message: &[u8]) -> anyhow::Result<[u8; 64]> {
            Ok(self.sign(message).to_bytes())
        }
    }

    fn personal_payload(
        root: &ed25519_dalek::SigningKey,
        device: &ed25519_dalek::SigningKey,
        nonce: [u8; 16],
    ) -> ResponsePayload {
        let user = root.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "New laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        let fleet = FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(user),
            certificates: vec![cert.clone()],
            revocations: Vec::new(),
        };
        ResponsePayload {
            nonce,
            recipient_device_public_key: cert.device_public_key,
            relay_service_node_id: [8; 32],
            cert,
            home_peer_toml: None,
            public_identity_json: Some(public_identity_record(user, "My Tilo identity").unwrap()),
            fleet_json: Some(serde_json::to_vec_pretty(&fleet).unwrap()),
            home_peers_json: Some(b"[]".to_vec()),
        }
    }

    fn payload(root: &ed25519_dalek::SigningKey) -> ResponsePayload {
        let user_public_key = root.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key,
            device_public_key: [7; 32],
            device_label: "NAS".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        ResponsePayload {
            nonce: [3; 16],
            recipient_device_public_key: [7; 32],
            relay_service_node_id: [8; 32],
            cert,
            home_peer_toml: Some("status_file = '/var/lib/tilo/full-replica/status.json'".into()),
            public_identity_json: None,
            fleet_json: None,
            home_peers_json: None,
        }
    }

    #[test]
    fn each_enrollment_kind_accepts_only_its_own_role() {
        let kinds = [
            EnrollmentKind::HomePeer,
            EnrollmentKind::PersonalDevice,
            EnrollmentKind::RoomHost,
        ];
        for minted in kinds {
            for joining in kinds {
                assert_eq!(
                    joining.accepts(&minted.capabilities()),
                    minted == joining,
                    "a {minted:?} token must {}enroll a {joining:?}",
                    if minted == joining { "" } else { "not " }
                );
            }
        }
        let room_host = EnrollmentKind::RoomHost.capabilities();
        assert!(room_host.can_act_as_host);
        assert!(
            !room_host.can_read_all_workspaces && !room_host.is_full_host(),
            "a room host is never a home peer"
        );
        assert!(EnrollmentKind::RoomHost.is_host());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn enrollment_responses_are_signed_encrypted_and_certificate_bound() {
        // Phase `expired_device_certificate_is_not_projected_into_personal_state`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[74; 32]);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "Expired laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: Some(2),
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            let identity = tempfile::tempdir().unwrap();

            let error = project_personal_device(identity.path(), &cert).unwrap_err();

            assert!(error.to_string().contains("certificate validity"));
            assert!(!fleet_path(identity.path(), cert.user_public_key).exists());
        }
        // Phase `revoked_device_cannot_use_approval_capability`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[75; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[76; 32]);
            let user = root.verifying_key().to_bytes();
            let device_public_key = device.verifying_key().to_bytes();
            let mut revocation = RevocationEntry {
                user_public_key: user,
                revoked_device_public_key: device_public_key,
                revoked_at_ns: 10,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut revocation, &root).unwrap();
            let identity = tempfile::tempdir().unwrap();
            atomic_file::write_creating_parent(
                &fleet_path(identity.path(), user),
                &serde_json::to_vec(&FleetProjection {
                    schema_version: 1,
                    identity_id: hex::encode(user),
                    certificates: Vec::new(),
                    revocations: vec![revocation],
                })
                .unwrap(),
            )
            .unwrap();

            assert!(fleet_revokes_device(identity.path(), user, device_public_key).unwrap());
        }
        // Phase `signed_response_rejects_bundle_tampering`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let mut response = sign_response(payload(&root), &root).unwrap();
            verify_response(&response, root.verifying_key().to_bytes()).unwrap();
            response
                .payload
                .home_peer_toml
                .as_mut()
                .unwrap()
                .push_str("\n# changed");
            assert!(verify_response(&response, root.verifying_key().to_bytes()).is_err());
        }
        // Phase `approval_target_requires_a_complete_pair_of_bounded_ids`.
        {
            let mut options = ApprovalOptions {
                identity_root: std::path::Path::new(""),
                identity_dir: std::path::Path::new(""),
                timeout: 1,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                relay_workspace_ids: &[],
                expected_device: None,
                expected_service: None,
                kind: EnrollmentKind::HomePeer,
                ready: None,
            };
            assert_eq!(expected_pair(&options).unwrap(), None);

            options.expected_device = Some("00");
            assert!(expected_pair(&options).is_err());

            let device = hex::encode([7; 32]);
            let service = hex::encode([8; 32]);
            options.expected_device = Some(&device);
            options.expected_service = Some(&service);
            assert_eq!(expected_pair(&options).unwrap(), Some(([7; 32], [8; 32])));
        }
        // Phase `provisioning_bundle_is_encrypted_for_the_recipient_device`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let mut request_seed = [0_u8; 32];
            request_seed[0] = 11;
            let request_ephemeral = tilo_seal::SecretKey::from_bytes(request_seed);
            let request_public = request_ephemeral.public_key().unwrap().to_bytes();
            let mut payload = payload(&root);
            payload.home_peer_toml = Some("ticket = 'secret-ticket-material'".into());
            let signed = sign_response(payload, &root).unwrap();
            let encrypted = encrypt_response(
                &signed,
                [3; 16],
                device.verifying_key().to_bytes(),
                ResponseRoute::V2 {
                    response_public_key: request_public,
                },
            )
            .unwrap();
            assert!(v2_body(&encrypted).is_some());
            assert!(
                !encrypted
                    .windows(b"secret-ticket-material".len())
                    .any(|window| window == b"secret-ticket-material")
            );
            let decrypted = decrypt_response(
                &encrypted,
                [3; 16],
                device.verifying_key().to_bytes(),
                &request_ephemeral,
            )
            .unwrap();
            verify_response(&decrypted, root.verifying_key().to_bytes()).unwrap();
            let wrong = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
            assert!(
                decrypt_response(
                    &encrypted,
                    [3; 16],
                    wrong.verifying_key().to_bytes(),
                    &request_ephemeral,
                )
                .is_err()
            );
        }
        // Phase `bearer_token_is_encrypted_for_the_issuing_device`.
        {
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            let request = Request {
                token: "reusable-secret-token".into(),
                device_public_key: [7; 32],
                relay_service_node_id: [8; 32],
                label: "NAS".into(),
                kind: EnrollmentKind::HomePeer,
            };
            let (encrypted, _) =
                encrypt_request(&request, issuer.verifying_key().to_bytes()).unwrap();
            assert!(
                !encrypted
                    .windows(b"reusable-secret-token".len())
                    .any(|window| window == b"reusable-secret-token")
            );
            let (decrypted, _) = decrypt_request(&encrypted, &issuer).unwrap();
            assert_eq!(decrypted.token, request.token);
            let wrong = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            assert!(decrypt_request(&encrypted, &wrong).is_err());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn personal_projection_publishes_only_live_valid_identity_state() {
        // Phase `personal_enrollment_staging_keeps_an_existing_device_unchanged`.
        fn personal_enrollment_staging_keeps_an_existing_device_unchanged() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
            let member = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
            let old_root = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
            let old_device = ed25519_dalek::SigningKey::from_bytes(&[54; 32]);
            let identity_root = tempfile::tempdir().unwrap();
            let identity_dir = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(identity_root.path());
            paths.ensure_directories().unwrap();
            tilo_user_identity::persist::write_public_key(
                &paths.user_pubkey,
                &old_root.verifying_key().to_bytes(),
            )
            .unwrap();
            let mut old_cert = DeviceCert {
                user_public_key: old_root.verifying_key().to_bytes(),
                device_public_key: old_device.verifying_key().to_bytes(),
                device_label: "Existing device".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut old_cert, &old_root).unwrap();
            tilo_user_identity::persist::write_device_cert(&paths.device_cert, &old_cert).unwrap();
            let before_key = std::fs::read(&paths.user_pubkey).unwrap();
            let before_cert = std::fs::read(&paths.device_cert).unwrap();

            let mut requester = std::process::Command::new("/bin/sh")
                .args(["-c", "while :; do sleep 1; done"])
                .spawn()
                .unwrap();
            let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
            let payload = personal_payload(&root, &member, [9; 16]);
            let (marker_path, marker) = stage_personal_enrollment(
                identity_root.path(),
                identity_dir.path(),
                &paths,
                &payload,
                Some(lease),
            )
            .unwrap();
            // This models an approval timeout: the recipient has a durable,
            // retryable staging marker but no active identity mutation.
            assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
            assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
            assert!(marker_path.exists());
            // A timeout/restart must not turn a merely prepared response into an
            // active enrollment.
            recover_personal_enrollments(identity_root.path(), identity_dir.path(), &paths, |_| {
                Ok(())
            })
            .unwrap();
            assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
            assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);

            let marker = authorize_personal_commit(&marker_path, &marker, Some(lease)).unwrap();
            assert!(
                commit_staged_personal_enrollment(
                    identity_root.path(),
                    &paths,
                    &marker_path,
                    &marker,
                    |checkpoint| {
                        lease.validate()?;
                        if checkpoint == PersonalCommitCheckpoint::DefaultCert {
                            bail!("simulated crash before default certificate")
                        }
                        Ok(())
                    },
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
            requester.kill().unwrap();
            requester.wait().unwrap();
            assert!(lease.validate().is_err());
            // `commit_ready` is the durable boundary: Finalized and the original
            // live lease were authenticated before it was written. A subsequent
            // explicit retry may complete that exact transaction under its own
            // live lease rather than leaving mixed durable state.
            let retry_lease = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
            recover_personal_enrollments(identity_root.path(), identity_dir.path(), &paths, |_| {
                retry_lease.validate().map_err(Into::into)
            })
            .unwrap();
            assert_eq!(
                std::fs::read(&paths.user_pubkey).unwrap(),
                root.verifying_key().to_bytes()
            );
            assert_eq!(
                tilo_user_identity::persist::read_device_cert(&paths.device_cert)
                    .unwrap()
                    .device_public_key,
                member.verifying_key().to_bytes()
            );
        }
        // Phase `personal_projection_loads_without_root_signing_material`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[19; 32]);
            let user = root.verifying_key().to_bytes();
            let device = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
            let mut cert = DeviceCert {
                user_public_key: user,
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "New laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            let fleet = FleetProjection {
                schema_version: 1,
                identity_id: hex::encode(user),
                certificates: vec![cert.clone()],
                revocations: Vec::new(),
            };
            let payload = ResponsePayload {
                nonce: [1; 16],
                recipient_device_public_key: cert.device_public_key,
                relay_service_node_id: [8; 32],
                cert,
                home_peer_toml: None,
                public_identity_json: Some(
                    public_identity_record(user, "My Tilo identity").unwrap(),
                ),
                fleet_json: Some(serde_json::to_vec_pretty(&fleet).unwrap()),
                home_peers_json: Some(b"[]".to_vec()),
            };
            let identity_root = tempfile::tempdir().unwrap();
            let identity_dir = tempfile::tempdir().unwrap();
            let record =
                install_personal_projection(identity_root.path(), identity_dir.path(), &payload)
                    .unwrap();
            let loaded = runtime_identity::load_from_path(record).unwrap();
            assert_eq!(loaded.group_public_key(), user);
            assert!(!loaded.is_single_device());
            assert!(loaded.sign(b"root-only operation").is_err());
            assert_eq!(
                std::fs::read(identity_root.path().join("home-peers.json")).unwrap(),
                b"[]"
            );
            let registry: serde_json::Value = serde_json::from_slice(
                &std::fs::read(identity_dir.path().join("registry.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(registry["default_identity"], hex::encode(user));
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                let uid = unsafe { libc::geteuid() };
                assert_eq!(
                    std::fs::metadata(identity_dir.path().join("registry.json"))
                        .unwrap()
                        .uid(),
                    uid
                );
                assert_eq!(
                    std::fs::metadata(fleet_path(identity_dir.path(), user))
                        .unwrap()
                        .uid(),
                    uid
                );
            }
        }
        // Phase `personal_projection_merges_and_preserves_unrelated_home_peers`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
            let member = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
            let other_root = ed25519_dalek::SigningKey::from_bytes(&[63; 32]);
            let other_device = ed25519_dalek::SigningKey::from_bytes(&[64; 32]);
            let identity_root = tempfile::tempdir().unwrap();
            let identity_dir = tempfile::tempdir().unwrap();
            let mut other_cert = DeviceCert {
                user_public_key: other_root.verifying_key().to_bytes(),
                device_public_key: other_device.verifying_key().to_bytes(),
                device_label: "Other identity home".into(),
                capabilities: DeviceCapabilities::host(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut other_cert, &other_root).unwrap();
            let other = HomePeerRegistration {
                schema_version: HomePeerRegistration::CURRENT_SCHEMA,
                device_node_id: hex::encode(other_cert.device_public_key),
                relay_service_node_id: hex::encode([65; 32]),
                relay_endpoint: None,
                label: other_cert.device_label.clone(),
                user_public_key: hex::encode(other_cert.user_public_key),
                device_cert: Some(other_cert),
                relay_workspace_ids: vec![7],
            };
            atomic_file::write(
                &identity_root.path().join("home-peers.json"),
                &serde_json::to_vec_pretty(&vec![other.clone()]).unwrap(),
            )
            .unwrap();
            install_personal_projection(
                identity_root.path(),
                identity_dir.path(),
                &personal_payload(&root, &member, [10; 16]),
            )
            .unwrap();
            let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
                &std::fs::read(identity_root.path().join("home-peers.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(peers, vec![other]);
        }
        // Phase `home_peer_projection_rejects_future_schemas_and_forged_device_ids`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "Verified home peer".into(),
                capabilities: DeviceCapabilities::host(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            let mut registration = HomePeerRegistration {
                schema_version: HomePeerRegistration::CURRENT_SCHEMA,
                device_node_id: hex::encode(cert.device_public_key),
                relay_service_node_id: hex::encode([83; 32]),
                relay_endpoint: None,
                label: cert.device_label.clone(),
                user_public_key: hex::encode(cert.user_public_key),
                device_cert: Some(cert),
                relay_workspace_ids: vec![7],
            };
            assert!(valid_home_peer_registration(&registration));

            registration.schema_version = HomePeerRegistration::CURRENT_SCHEMA.saturating_add(1);
            assert!(!valid_home_peer_registration(&registration));

            registration.schema_version = HomePeerRegistration::CURRENT_SCHEMA;
            registration.device_node_id = hex::encode([84; 32]);
            assert!(!valid_home_peer_registration(&registration));
        }
        // Phase `personal_enrollment_staging_keeps_an_existing_device_unchanged` (defined above).
        personal_enrollment_staging_keeps_an_existing_device_unchanged();
        // Phase `expired_requester_lease_blocks_personal_identity_publication`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
            let member = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
            let identity_root = tempfile::tempdir().unwrap();
            let identity_parent = tempfile::tempdir().unwrap();
            let identity_dir = identity_parent.path().join("identities");
            let paths = IdentityPaths::under(identity_root.path());
            paths.ensure_directories().unwrap();
            let mut requester = std::process::Command::new("/bin/sh")
                .args(["-c", "while :; do sleep 1; done"])
                .spawn()
                .unwrap();
            let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
            let payload = personal_payload(&root, &member, [12; 16]);
            let (marker_path, marker) = stage_personal_enrollment(
                identity_root.path(),
                &identity_dir,
                &paths,
                &payload,
                Some(lease),
            )
            .unwrap();
            let unrelated = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
            assert!(
                authorize_personal_commit(&marker_path, &marker, Some(unrelated)).is_err(),
                "another live requester cannot adopt a prepared enrollment"
            );
            requester.kill().unwrap();
            requester.wait().unwrap();

            assert!(
                authorize_personal_commit(&marker_path, &marker, Some(lease)).is_err(),
                "a dead compositor cannot cross the durable commit point"
            );
            let mut simulated_ready = marker;
            simulated_ready.commit_ready = true;
            assert!(
                commit_staged_personal_enrollment(
                    identity_root.path(),
                    &paths,
                    &marker_path,
                    &simulated_ready,
                    |_| lease.validate().map_err(Into::into),
                )
                .is_err()
            );
            assert!(!identity_dir.exists());
            assert!(!paths.user_pubkey.exists());
            assert!(!paths.device_cert.exists());
            assert!(marker_path.exists(), "staging remains retryable");
        }
        // Phase `pair_device_rejects_an_expired_requester_before_creating_identity_state`.
        {
            let mut requester = std::process::Command::new("/bin/sh")
                .args(["-c", "while :; do sleep 1; done"])
                .spawn()
                .unwrap();
            let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
            requester.kill().unwrap();
            requester.wait().unwrap();

            let identity_root = tempfile::tempdir().unwrap();
            let identity_dir = tempfile::tempdir().unwrap();
            assert!(
                pair(
                    identity_root.path(),
                    "New laptop",
                    1,
                    EnrollmentKind::PersonalDevice,
                    None,
                    Some(identity_dir.path()),
                    None,
                    Some(lease),
                )
                .is_err()
            );
            let paths = IdentityPaths::under(identity_root.path());
            assert!(!paths.device_privkey.exists());
            assert!(!paths.user_pubkey.exists());
            assert!(!paths.device_cert.exists());
        }
    }

    /// Nearby approval binds the fixed LAN pairing port, so the tests that
    /// run one take turns.
    static LAN_PAIRING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An approving device as `shell --provision-identity` leaves it on a
    /// session without Secret Service: the user root lives only in the
    /// secured personal record (`primary.json` plus its broker-file secret),
    /// the device key and certificates live in the identity root, and the
    /// identity's fleet lists this device. No root key file is written.
    fn provisioned_approver(
        identity_root: &std::path::Path,
        identity_dir: &std::path::Path,
        seed: u8,
    ) -> runtime_identity::SigningIdentity {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::create_dir_all(identity_dir).unwrap();
        std::fs::set_permissions(identity_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let secret = [seed; 32];
        let group = ed25519_dalek::SigningKey::from_bytes(&secret)
            .verifying_key()
            .to_bytes();
        let record = serde_json::json!({
            "display_name": "",
            "avatar_color": [128, 128, 128],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": group,
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": secret,
            },
            "created_at": "",
            "schema_version": 1,
        });
        runtime_identity::import_identity_with_provider(
            identity_dir,
            "primary",
            &serde_json::to_vec(&record).unwrap(),
            &runtime_identity::BrokerFileProvider::new(identity_dir),
        )
        .unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(identity_dir.join("primary.json")).unwrap())
                .unwrap();
        assert_eq!(stored["schema_version"], 2);
        assert!(stored["threshold_identity"].get("secret_bytes").is_none());
        let root = runtime_identity::load_root_signer(identity_dir).unwrap();
        let user = root.root_public_key();

        let paths = IdentityPaths::under(identity_root);
        paths.ensure_directories().unwrap();
        tilo_user_identity::persist::write_public_key(&paths.user_pubkey, &user).unwrap();
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &issuer).unwrap();
        let mut issuer_cert = DeviceCert {
            user_public_key: user,
            device_public_key: issuer.verifying_key().to_bytes(),
            device_label: "Trusted laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_cert_with_root(&mut issuer_cert, &root).unwrap();
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &issuer_cert).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&user),
            &issuer_cert,
        )
        .unwrap();
        atomic_file::write_creating_parent(
            &fleet_path(identity_dir, user),
            &serde_json::to_vec_pretty(&FleetProjection {
                schema_version: 1,
                identity_id: hex::encode(user),
                certificates: vec![issuer_cert],
                revocations: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!paths.user_privkey.exists());
        root
    }

    #[test]
    fn a_room_host_enrols_without_files_and_is_never_a_home_peer() {
        let _lan = LAN_PAIRING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let approver_root = tempfile::tempdir().unwrap();
        let approver_identities = tempfile::tempdir().unwrap();
        let root = provisioned_approver(approver_root.path(), approver_identities.path(), 61);
        let user = root.root_public_key();

        let server_root = approver_root.path().to_path_buf();
        let server_identities = approver_identities.path().to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        // No Files workspace exists on the approving side at all.
        let approval = std::thread::spawn(move || {
            approve(&ApprovalOptions {
                identity_root: &server_root,
                identity_dir: &server_identities,
                timeout: 15,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                relay_workspace_ids: &[],
                expected_device: None,
                expected_service: None,
                kind: EnrollmentKind::RoomHost,
                ready: Some(&ready_tx),
            })
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("approval listener became ready");
        let host = tempfile::tempdir().unwrap();
        pair(
            host.path(),
            "Community host",
            15,
            EnrollmentKind::RoomHost,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        approval.join().unwrap().unwrap();

        let host_paths = IdentityPaths::under(host.path());
        let cert = tilo_user_identity::persist::read_device_cert(&host_paths.device_cert).unwrap();
        tilo_user_identity::verify_device_cert(&cert).unwrap();
        assert_eq!(cert.user_public_key, user);
        assert_eq!(cert.capabilities, DeviceCapabilities::room_host());
        assert!(cert.capabilities.can_act_as_host);
        assert!(
            !cert.capabilities.is_full_host(),
            "a room host never reads every workspace, so it never federates or replicates Files"
        );
        assert!(
            !approver_root.path().join("home-peers.json").exists(),
            "a room host is not registered as the identity's home peer"
        );
        assert!(
            std::fs::read_dir(host.path())
                .unwrap()
                .flatten()
                .all(|entry| entry.file_name() != "home-peer.toml"),
            "no Files home-peer configuration reaches a room host"
        );
    }

    #[test]
    fn nearby_home_server_approval_signs_with_a_freshly_provisioned_identity() {
        let _lan = LAN_PAIRING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let approver_root = tempfile::tempdir().unwrap();
        let approver_identities = tempfile::tempdir().unwrap();
        let root = provisioned_approver(approver_root.path(), approver_identities.path(), 51);
        let user = root.root_public_key();
        let workspace_cache = tempfile::tempdir().unwrap();
        let ticket = fs::Ticket {
            node: iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[53; 32]).public()),
            workspace: [54; 32],
        };
        std::fs::write(
            workspace_cache.path().join("workspace-ticket"),
            ticket.encode().unwrap(),
        )
        .unwrap();
        let acl = workspace_cache.path().join("fs-capabilities.json");
        std::fs::write(&acl, b"[]").unwrap();

        let server_root = approver_root.path().to_path_buf();
        let server_identities = approver_identities.path().to_path_buf();
        let server_cache = workspace_cache.path().to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let approval = std::thread::spawn(move || {
            approve(&ApprovalOptions {
                identity_root: &server_root,
                identity_dir: &server_identities,
                timeout: 15,
                workspace_cache: &server_cache,
                acl: &acl,
                relay_workspace_ids: &[7],
                expected_device: None,
                expected_service: None,
                kind: EnrollmentKind::HomePeer,
                ready: Some(&ready_tx),
            })
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("approval listener became ready");
        let server = tempfile::tempdir().unwrap();
        let bundle_path = server.path().join("home-peer.toml");
        pair(
            server.path(),
            "Home server",
            15,
            EnrollmentKind::HomePeer,
            Some(&bundle_path),
            None,
            None,
            None,
        )
        .unwrap();
        approval.join().unwrap().unwrap();

        let server_paths = IdentityPaths::under(server.path());
        let server_device =
            tilo_user_identity::persist::read_device_key(&server_paths.device_privkey).unwrap();
        let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(approver_root.path().join("home-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].device_node_id,
            hex::encode(server_device.verifying_key().to_bytes())
        );
        assert_eq!(peers[0].user_public_key, hex::encode(user));
        let cert = peers[0].device_cert.clone().unwrap();
        assert_eq!(cert.capabilities, DeviceCapabilities::host());
        assert!(verify_device_cert(&cert).is_ok());
        assert!(peers[0].relay_workspace_ids.contains(&7));
        assert!(
            peers[0]
                .relay_workspace_ids
                .contains(&tilo_relay::personal_continuity_workspace_id(&user))
        );
        let bundle: fs::full_replica::HomePeerSet =
            toml::from_str(&std::fs::read_to_string(&bundle_path).unwrap()).unwrap();
        bundle.workspaces[0]
            .authority
            .as_ref()
            .expect("the home peer receives the owner's signed workspace authority")
            .verify(ticket.workspace_id(), user)
            .unwrap();
        assert!(
            approver_identities
                .path()
                .join("enrollment-nonces")
                .is_dir()
        );
        assert!(!approver_root.path().join("enrollment-nonces").exists());
        assert!(
            !IdentityPaths::under(approver_root.path())
                .user_privkey
                .exists()
        );
    }

    #[test]
    fn approval_names_the_fix_when_no_personal_identity_is_provisioned() {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let options = ApprovalOptions {
            identity_root: identity_root.path(),
            identity_dir: identity_dir.path(),
            timeout: 1,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            relay_workspace_ids: &[],
            expected_device: None,
            expected_service: None,
            kind: EnrollmentKind::HomePeer,
            ready: None,
        };
        let Err(error) = load_approval_identity(&options) else {
            panic!("approval without a personal identity must fail");
        };
        let message = format!("{error:#}");
        assert!(message.contains("primary.json"), "{message}");
        assert!(message.contains("shell --provision-identity"), "{message}");
    }

    #[test]
    fn approval_refuses_an_identity_received_from_another_device() {
        use std::os::unix::fs::PermissionsExt as _;

        let identity_dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(identity_dir.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let member = public_identity_record([61; 32], "My Tilo identity").unwrap();
        let path = identity_dir.path().join("primary.json");
        std::fs::write(&path, member).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let Err(error) = runtime_identity::load_root_signer(identity_dir.path()) else {
            panic!("a member projection cannot approve enrollment");
        };
        assert!(error.to_string().contains("holds no user root"), "{error}");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end test keeps the approver and joining-device setup in one auditable protocol scenario"
    )]
    fn nearby_personal_device_pairing_transfers_cert_fleet_and_home_peer() {
        let _lan = LAN_PAIRING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let approver_root = tempfile::tempdir().unwrap();
        let approver_identities = tempfile::tempdir().unwrap();
        let root = provisioned_approver(approver_root.path(), approver_identities.path(), 31);
        let user = root.root_public_key();
        let home_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let mut home_cert = DeviceCert {
            user_public_key: user,
            device_public_key: home_device.verifying_key().to_bytes(),
            device_label: "Home peer".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_cert_with_root(&mut home_cert, &root).unwrap();
        register_home_peer(
            approver_root.path(),
            &home_cert,
            [43; 32],
            &[tilo_relay::personal_continuity_workspace_id(&user)],
        )
        .unwrap();

        // Seed the new device's key so the approval pins exactly this device:
        // every nearby pairing on the host shares one broadcast port, and an
        // unpinned approver answers whichever device solicits first.
        let new_root = tempfile::tempdir().unwrap();
        let new_identities = tempfile::tempdir().unwrap();
        let new_device = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
        let new_paths = IdentityPaths::under(new_root.path());
        new_paths.ensure_directories().unwrap();
        tilo_user_identity::persist::write_device_key(&new_paths.device_privkey, &new_device)
            .unwrap();
        let expected_device = hex::encode(new_device.verifying_key().to_bytes());
        let expected_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&new_device));

        let server_root = approver_root.path().to_path_buf();
        let server_identities = approver_identities.path().to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let approval = std::thread::spawn(move || {
            approve(&ApprovalOptions {
                identity_root: &server_root,
                identity_dir: &server_identities,
                timeout: 60,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                relay_workspace_ids: &[],
                expected_device: Some(&expected_device),
                expected_service: Some(&expected_service),
                kind: EnrollmentKind::PersonalDevice,
                ready: Some(&ready_tx),
            })
        });
        match ready_rx.recv_timeout(tilo_test_env::hang_bound()) {
            Ok(()) => {}
            // An approver that ended before listening dropped the sender; its
            // own error names why, such as the shared pairing port.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the approver ended before listening: {:?}", approval.join())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("the approval listener did not become ready within the hang bound")
            }
        }
        // Other nearby-pairing surfaces on this host (an open discovery
        // view, another approval sheet) share the well-known port. Which of
        // them the kernel hands a unicast datagram must not decide whether
        // the enrollment completes.
        let _other_surfaces: Vec<UdpSocket> =
            (0..3).map(|_| bind_pair_listener().unwrap()).collect();
        let requester = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
        pair(
            new_root.path(),
            "New laptop",
            60,
            EnrollmentKind::PersonalDevice,
            None,
            Some(new_identities.path()),
            None,
            Some(requester),
        )
        .unwrap();
        approval.join().unwrap().unwrap();

        let enrolled_paths = IdentityPaths::under(new_root.path());
        let cert =
            tilo_user_identity::persist::read_device_cert(&enrolled_paths.device_cert).unwrap();
        assert_eq!(cert.user_public_key, user);
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert!(verify_device_cert(&cert).is_ok());
        assert!(!enrolled_paths.user_privkey.exists());
        assert!(
            !IdentityPaths::under(approver_root.path())
                .user_privkey
                .exists(),
            "approval must sign through the personal record, never a root key file"
        );
        assert!(
            approver_identities
                .path()
                .join("enrollment-nonces")
                .is_dir(),
            "the approver's single-use ledger belongs to its identity"
        );
        let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(new_root.path().join("home-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(peers.len(), 1);
        assert!(
            peers[0]
                .relay_workspace_ids
                .contains(&tilo_relay::personal_continuity_workspace_id(&user))
        );
        let record = identity_record_path(new_identities.path(), user);
        let identity = runtime_identity::load_from_path(record).unwrap();
        assert!(
            identity
                .sign(b"root/admin signing must stay unavailable")
                .is_err()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn pairing_exchange_is_private_single_use_and_resumable() {
        // Phase `automatic_pair_offer_is_private_to_the_soliciting_server`.
        {
            let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let token = "signed-host-enrollment-token";
            let encrypted =
                encrypt_pair_offer(token, server.verifying_key().to_bytes(), WireVersion::V2)
                    .unwrap();
            assert!(v2_body(&encrypted).is_some());
            assert!(
                !encrypted
                    .windows(token.len())
                    .any(|window| window == token.as_bytes())
            );
            assert_eq!(decrypt_pair_offer(&encrypted, &server).unwrap(), token);
            let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
            assert!(decrypt_pair_offer(&encrypted, &other).is_err());
        }
        // Phase `nearby_pairing_code_is_stable_and_six_digits`.
        {
            let code = pairing_code([7; 32], [8; 32]);
            assert_eq!(code, pairing_code([7; 32], [8; 32]));
            assert_eq!(code.len(), 6);
            assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
            assert_ne!(code, pairing_code([7; 32], [9; 32]));
        }
        // Phase `unrelated_lan_datagram_is_not_a_pairing_message`.
        {
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            assert!(decrypt_request(b"ambient discovery traffic", &issuer).is_err());
            let mut request_seed = [0_u8; 32];
            request_seed[0] = 11;
            let request_ephemeral = tilo_seal::SecretKey::from_bytes(request_seed);
            assert!(
                decrypt_response(
                    b"not an enrollment response",
                    [3; 16],
                    [7; 32],
                    &request_ephemeral,
                )
                .is_err()
            );
        }
        // Phase `enrollment_nonce_is_consumed_once`.
        {
            let dir = tempfile::tempdir().unwrap();
            consume_nonce(dir.path(), &[4; 16], [7; 32]).unwrap();
            consume_nonce(dir.path(), &[4; 16], [7; 32]).unwrap();
            assert!(consume_nonce(dir.path(), &[4; 16], [8; 32]).is_err());
        }
        // Phase `approved_home_peer_registration_exposes_distinct_stable_node_ids`.
        {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().unwrap();
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let cert = payload(&root).cert;
            register_home_peer(dir.path(), &cert, [8; 32], &[2, 9]).unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join("home-peers.json")).unwrap())
                    .unwrap();
            assert_eq!(
                json[0]["device_node_id"],
                hex::encode(cert.device_public_key)
            );
            assert_eq!(json[0]["relay_service_node_id"], hex::encode([8; 32]));
            assert_eq!(json[0]["relay_workspace_ids"], serde_json::json!([2, 9]));
            assert_eq!(
                std::fs::metadata(dir.path().join("home-peers.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );
        }
        // Phase `invalid_existing_home_peer_registry_is_never_overwritten`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("home-peers.json");
            atomic_file::write(&path, b"not json").unwrap();
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            assert!(register_home_peer(dir.path(), &payload(&root).cert, [8; 32], &[2]).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        }
        // Phase `dropped_response_can_be_retried_with_the_same_authenticated_ack`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let response =
                postcard::to_stdvec(&sign_response(payload(&root), &root).unwrap()).unwrap();
            // The first datagram is modeled as dropped. Retransmission uses the
            // exact same signed bytes, and the recipient ACK binds their key,
            // nonce, and that response digest.
            let retransmitted = response.clone();
            assert_eq!(response, retransmitted);
            let ack = make_ack(&retransmitted, [3; 16], &device).unwrap();
            verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
        }
        // Phase `dropped_all_acks_can_resume_after_process_restart`.
        {
            let dir = tempfile::tempdir().unwrap();
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let response =
                postcard::to_stdvec(&sign_response(payload(&root), &root).unwrap()).unwrap();

            // No ACK reached the first approver, so it did not consume the
            // admission nonce. A restarted join/approve exchange may authenticate
            // the same device and finish registration idempotently.
            assert!(!dir.path().join("enrollment-nonces").exists());
            let ack = make_ack(&response, [3; 16], &device).unwrap();
            verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
            consume_nonce(dir.path(), &[3; 16], device.verifying_key().to_bytes()).unwrap();
            let finalized =
                make_finalized(&response, [3; 16], device.verifying_key().to_bytes(), &root)
                    .unwrap();
            verify_finalized(
                &finalized,
                &response,
                [3; 16],
                device.verifying_key().to_bytes(),
                root.verifying_key().to_bytes(),
            )
            .unwrap();
            consume_nonce(dir.path(), &[3; 16], device.verifying_key().to_bytes()).unwrap();
            assert!(consume_nonce(dir.path(), &[3; 16], [6; 32]).is_err());
        }
    }

    // Byte fixtures produced by the enrollment code before a crypto-stack
    // upgrade. Enrollment blobs cross devices running different builds, so
    // decrypting them proves Ed25519-to-X25519 conversion, X25519 agreement,
    // ChaCha20-Poly1305 and the postcard framing all stayed byte-compatible.
    const KAT_PAIR_OFFER: &str = "d0e014c51b6faaf168eb9a629804a5cf9eabbb666318369dccaec9eb3a8dbc0d\
        07189ed4428bc2d1c596e9782167343cec81004105d1275f7105e70365bedcfe\
        fc511381c19fef25960fd9444a62";
    const KAT_REQUEST: &str = "49681ddb0d310c132f72f076bbaac37b9be5edbe98ee074e216b42299532274d\
        1465fd79f168e36085d553de66f27e784ca62d250acb45d8040a55346ccd5dd3\
        4f78c085e59326b7fd8374279b404d0f3ef67bcf405fda1a8d83f706d3420904\
        26aaf976690c8a63213d242e47765f3640906728c76823bc3e189762bf3eae40\
        c6883b3a5176c6d99d12fff3f424405981e733";
    const KAT_RESPONSE: &str = "7a7009cf2b5bedfe755de254721eca30a874669a6694d7014baece97b5448510\
        b6817a3cf12ffce507ab4b2ce7022762eb9d9dfc5fc8ce997f151429a3b17c46\
        8bc8b61e724e5064d8918ec4e5450388294a24e29b8626145fd6d5cb782a3f27\
        c18725e4f9664fe265ee832db061bf70d49082a4f8946133bc784aa36ec8565a\
        90ca8fb99ff752c06f3b6677b7a95d4409d4eb1985fdebd10044f1e7b72e0347\
        3beeea80a610ba90b6d027b81211ecf818304d6fbba3bab98908e8e2ebaf584d\
        6b511b9f4c017c7476d2a144b5e519c73a6de2a1ed8e36415e5081553b6b6b37\
        27603b281f6e9ead1c690fafbaa1286d39a3eb685e9fb154a87800aa1e580015\
        8061468aec871810f870d33413b8fb61d48c8760ce1bf1f94075a9a48441beb7\
        aebe3cf154d76054060ee25110c7e78481abbd493f8d47ec8c7215c48d086f5a\
        d42343e8923340ae8e1461dd874130c8907631761c1169f9cd89afca1c316025\
        3b8ab6ba3b0eef0eff1abaf1f1c45a2028801ecdbdfcd5c8f2589f12aeaad3ea\
        fb16e4d76d3037caef5a0f242c4f5547bf575f6450";
    const KAT_RESPONSE_SIGNATURE: &str = "a645d9aa10752fbf8b19d1ccca48b9cf5405d6ec88eb69fea45df36466edda10\
        e8b1bf1f0d85e9f224b21f75a7251771b1482ce6ceed2ce018d43895124e6f0c";
    const KAT_ACK: &str = "0404040404040404040404040404040417cb79fb2b4120f2b1ec65e4198d6e08\
        b28e813feb01e4a400839b85e18080ce0aa39056fd697d25fd6d7afbe84dbb81\
        f52d9e543b4aa167cf6472c602c8079a40440274b6b4d4b42e7fd529bbbdf4f0\
        1cfc5dd55c7f0755cf6d7cc821f1cb7fdb971e77b7e4c0b1cffcadc3b3be2aef\
        423888209b7aab3703f999ab61869f950d";

    #[test]
    fn pairing_crypto_matches_known_answers() {
        // Phase `known_answer_x25519_agreement_and_ed25519_conversion`.
        {
            let a = tilo_seal::SecretKey::from_bytes([0x11; 32]);
            let b = tilo_seal::SecretKey::from_bytes([0x22; 32]);
            assert_eq!(
                hex::encode(a.public_key().unwrap().to_bytes()),
                "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13"
            );
            // The legacy KDF is BLAKE3(domain || shared || context); with an
            // empty domain and context it is BLAKE3 of the raw agreement, which
            // pins the X25519 output an earlier build computed.
            let shared =
                hex::decode("9e004098efc091d4ec2663b4e9f5cfd4d7064571690b4bea97ab146ab9f35056")
                    .unwrap();
            assert_eq!(
                tilo_seal::legacy::blake3_x25519_key(&a, &b.public_key().unwrap(), b"", &[])
                    .unwrap()
                    .expose_secret(),
                blake3::hash(&shared).as_bytes()
            );
            let recipient = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
            assert_eq!(
                hex::encode(recipient.to_scalar_bytes()),
                "c25a999cf27ed91865db781f67e404f9d541d12e3e761786c409131fe595096d"
            );
            assert_eq!(
                hex::encode(recipient.verifying_key().to_montgomery().to_bytes()),
                "931aad030ad93d3ff8042699324fdb230585be4daf500266eaa4b8f747543f50"
            );
        }
        // Phase `known_answer_pair_offer_opens`.
        {
            let recipient = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
            let offer = hex::decode(KAT_PAIR_OFFER).unwrap();
            assert_eq!(
                decrypt_pair_offer(&offer, &recipient).unwrap(),
                "kat-pairing-token"
            );
            let wrong = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
            assert!(decrypt_pair_offer(&offer, &wrong).is_err());
        }
        // Phase `known_answer_request_opens`.
        {
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            let bytes = hex::decode(KAT_REQUEST).unwrap();
            let (request, route) = decrypt_request(&bytes, &issuer).unwrap();
            assert_eq!(request.token, "kat-bearer-token");
            assert_eq!(request.device_public_key, [7; 32]);
            assert_eq!(request.relay_service_node_id, [8; 32]);
            assert_eq!(request.label, "NAS");
            assert!(matches!(request.kind, EnrollmentKind::HomePeer));
            assert_eq!(
                route,
                ResponseRoute::V1 {
                    requester_ephemeral_public_key: bytes[..32].try_into().unwrap()
                }
            );
        }
        // Phase `known_answer_response_opens_and_verifies`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let signed = sign_response(payload(&root), &root).unwrap();
            assert_eq!(hex::encode(&signed.signature), KAT_RESPONSE_SIGNATURE);
            let request_ephemeral = tilo_seal::SecretKey::from_bytes([0x5a; 32]);
            let decrypted = decrypt_response(
                &hex::decode(KAT_RESPONSE).unwrap(),
                [3; 16],
                device.verifying_key().to_bytes(),
                &request_ephemeral,
            )
            .unwrap();
            assert_eq!(hex::encode(&decrypted.signature), KAT_RESPONSE_SIGNATURE);
            assert_eq!(
                postcard::to_stdvec(&decrypted.payload).unwrap(),
                postcard::to_stdvec(&signed.payload).unwrap()
            );
            verify_response(&decrypted, root.verifying_key().to_bytes()).unwrap();
        }
        // Phase `known_answer_ack_signature`.
        {
            let device = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
            let ack = make_ack(b"kat-response", [4; 16], &device).unwrap();
            assert_eq!(hex::encode(&ack), KAT_ACK);
            verify_ack(
                &ack,
                b"kat-response",
                [4; 16],
                device.verifying_key().to_bytes(),
            )
            .unwrap();
        }
    }

    /// A request in the v1 wire format, as a device running an earlier
    /// build still sends it. Returns the envelope and the requester's
    /// ephemeral secret that a v1 response is sealed to.
    fn legacy_v1_request(
        request: &Request,
        issuer_device_public_key: [u8; 32],
    ) -> (Vec<u8>, tilo_seal::SecretKey) {
        let (ephemeral, ephemeral_public_key) = v1_ephemeral().unwrap();
        let issuer = device_sealing_public(issuer_device_public_key).unwrap();
        let key = tilo_seal::legacy::blake3_x25519_key(
            &ephemeral,
            &issuer,
            REQUEST_KEY_DOMAIN_V1,
            &[&ephemeral_public_key],
        )
        .unwrap();
        let (nonce, ciphertext) = tilo_seal::legacy::seal_chacha20poly1305_for_transition(
            &key,
            &ephemeral_public_key,
            &postcard::to_stdvec(request).unwrap(),
        )
        .unwrap();
        let bytes = postcard::to_stdvec(&LegacyEnvelope {
            ephemeral_public_key,
            nonce,
            ciphertext,
        })
        .unwrap();
        (bytes, ephemeral)
    }

    fn sample_request() -> Request {
        Request {
            token: "bearer".into(),
            device_public_key: ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .to_bytes(),
            relay_service_node_id: [8; 32],
            label: "NAS".into(),
            kind: EnrollmentKind::HomePeer,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn pairing_envelope_versions_are_negotiated_and_step_bound() {
        // Phase `v2_request_gets_a_v2_response_only_the_requester_opens`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let (bytes, response_secret) =
                encrypt_request(&sample_request(), issuer.verifying_key().to_bytes()).unwrap();
            assert_eq!(&bytes[..8], b"TLOENRL\x02");
            let (request, route) = decrypt_request(&bytes, &issuer).unwrap();
            assert_eq!(request.token, "bearer");
            assert!(matches!(route, ResponseRoute::V2 { .. }));
            let signed = sign_response(payload(&root), &root).unwrap();
            let response =
                encrypt_response(&signed, [3; 16], device.verifying_key().to_bytes(), route)
                    .unwrap();
            assert!(v2_body(&response).is_some());
            let opened = decrypt_response(
                &response,
                [3; 16],
                device.verifying_key().to_bytes(),
                &response_secret,
            )
            .unwrap();
            verify_response(&opened, root.verifying_key().to_bytes()).unwrap();
            // Another key, another token nonce or another recipient device fail.
            let other = tilo_seal::SecretKey::generate().unwrap();
            assert!(
                decrypt_response(
                    &response,
                    [3; 16],
                    device.verifying_key().to_bytes(),
                    &other
                )
                .is_err()
            );
            assert!(
                decrypt_response(
                    &response,
                    [4; 16],
                    device.verifying_key().to_bytes(),
                    &response_secret
                )
                .is_err()
            );
            assert!(decrypt_response(&response, [3; 16], [1; 32], &response_secret).is_err());
        }
        // Phase `v1_request_is_still_accepted_and_answered_in_v1`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let (bytes, requester_ephemeral) =
                legacy_v1_request(&sample_request(), issuer.verifying_key().to_bytes());
            assert!(v2_body(&bytes).is_none());
            let (request, route) = decrypt_request(&bytes, &issuer).unwrap();
            assert_eq!(request.token, "bearer");
            assert_eq!(
                route,
                ResponseRoute::V1 {
                    requester_ephemeral_public_key: requester_ephemeral
                        .public_key()
                        .unwrap()
                        .to_bytes()
                }
            );
            let signed = sign_response(payload(&root), &root).unwrap();
            let response =
                encrypt_response(&signed, [3; 16], device.verifying_key().to_bytes(), route)
                    .unwrap();
            // An earlier build parses exactly the v1 framing.
            assert!(v2_body(&response).is_none());
            let envelope: LegacyEnvelope = postcard::from_bytes(&response).unwrap();
            assert_eq!(postcard::to_stdvec(&envelope).unwrap(), response);
            let opened = decrypt_response(
                &response,
                [3; 16],
                device.verifying_key().to_bytes(),
                &requester_ephemeral,
            )
            .unwrap();
            verify_response(&opened, root.verifying_key().to_bytes()).unwrap();
        }
        // Phase `pair_offers_mirror_the_solicitation_version`.
        {
            let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let mut solicitation = PairSolicitation {
                magic: PAIR_MAGIC_V1,
                device_public_key: server.verifying_key().to_bytes(),
                relay_service_node_id: [8; 32],
                label: "NAS".into(),
                kind: EnrollmentKind::HomePeer,
            };
            assert_eq!(solicitation_version(&solicitation), Some(WireVersion::V1));
            let v1 = encrypt_pair_offer("token", solicitation.device_public_key, WireVersion::V1)
                .unwrap();
            assert!(v2_body(&v1).is_none());
            assert!(postcard::from_bytes::<LegacyEnvelope>(&v1).is_ok());
            assert_eq!(decrypt_pair_offer(&v1, &server).unwrap(), "token");

            solicitation.magic = PAIR_MAGIC_V2;
            assert_eq!(solicitation_version(&solicitation), Some(WireVersion::V2));
            let v2 = encrypt_pair_offer("token", solicitation.device_public_key, WireVersion::V2)
                .unwrap();
            assert!(v2_body(&v2).is_some());
            assert_eq!(decrypt_pair_offer(&v2, &server).unwrap(), "token");

            solicitation.magic = *b"TLOPAIR9";
            assert!(!valid_solicitation(&solicitation));
        }
        // Phase `version_prefix_decides_the_parser_and_tampering_fails`.
        {
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            let (bytes, _) =
                encrypt_request(&sample_request(), issuer.verifying_key().to_bytes()).unwrap();
            // Every byte of a v2 envelope is covered: flipping the version or any
            // sealed byte fails, and a stripped prefix is not read as v1.
            for index in 0..bytes.len() {
                let mut tampered = bytes.clone();
                tampered[index] ^= 1;
                assert!(decrypt_request(&tampered, &issuer).is_err(), "byte {index}");
            }
            assert!(decrypt_request(&bytes[8..], &issuer).is_err());
            let wrong = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            assert!(decrypt_request(&bytes, &wrong).is_err());
            // A v1 envelope never begins with the v2 prefix.
            for _ in 0..32 {
                let (legacy, _) =
                    legacy_v1_request(&sample_request(), issuer.verifying_key().to_bytes());
                assert!(v2_body(&legacy).is_none());
            }
        }
        // Phase `v2_envelopes_do_not_open_as_another_step`.
        {
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let offer =
                encrypt_pair_offer("token", device.verifying_key().to_bytes(), WireVersion::V2)
                    .unwrap();
            // Same recipient key, different step label: the request reader
            // must refuse the pairing offer.
            assert!(decrypt_request(&offer, &device).is_err());
        }
    }
}
