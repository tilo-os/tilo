//! LAN/OOB completion for a pre-authorised qOS device enrollment token.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use qos_relay::HomePeerRegistration;
use qos_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, decode_enrollment_token,
    encode_enrollment_token, mint_enrollment_token, sign_device_cert, verify_device_cert,
    verify_enrollment_token, verify_revocation_entry,
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};

const PORT: u16 = 45_831;
const MAX_PACKET: usize = 60 * 1024;
const RESPONSE_DOMAIN: &[u8] = b"qos-host-enrollment-response-v1\0";
const PAIR_DOMAIN: &[u8] = b"qos-nearby-home-peer-offer-v1\0";
const PAIR_MAGIC: [u8; 8] = *b"QOSPAIR1";

#[derive(Parser)]
#[command(name = "qos-enroll")]
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
    /// Find one unprovisioned personal qOS device broadcasting on this LAN.
    DiscoverDevice {
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
    /// Join the identity named by TOKEN, discovering its approving device on this LAN.
    Join {
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/qos/home-peer.toml")]
        home_peer_output: PathBuf,
    },
    /// Pair with a nearby trusted qOS device without pre-installing a token.
    Pair {
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/qos/home-peer.toml")]
        home_peer_output: PathBuf,
    },
    /// Join an existing identity as an ordinary personal device.
    PairDevice {
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long, default_value = "qOS device")]
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
    /// Approve one nearby host token issued by this device.
    Approve {
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long, default_value = "/var/cache/quick-fs")]
        workspace_cache: PathBuf,
        #[arg(long, default_value = "/var/lib/qos/fs-capabilities.json")]
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
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
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
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    label: String,
    kind: EnrollmentKind,
}

#[derive(Serialize, Deserialize)]
struct EncryptedRequest {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
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

#[derive(Serialize, Deserialize)]
struct EncryptedResponse {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
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

#[derive(Serialize, Deserialize)]
struct PairOffer {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetProjection {
    schema_version: u32,
    identity_id: String,
    certificates: Vec<DeviceCert>,
    revocations: Vec<qos_user_identity::RevocationEntry>,
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
    requester_lease: Option<qos_relay::ProcessLease>,
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

#[allow(dead_code)]
fn legacy_install_personal_projection(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    payload: &ResponsePayload,
) -> anyhow::Result<PathBuf> {
    #[cfg(unix)]
    let owner = {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(identity_dir)
            .or_else(|_| {
                identity_dir
                    .parent()
                    .context("identity directory has no parent")
                    .and_then(|p| std::fs::metadata(p).map_err(Into::into))
            })
            .map(|metadata| (metadata.uid(), metadata.gid()))?
    };
    let identity = payload
        .public_identity_json
        .as_ref()
        .context("approval omitted the public identity")?;
    let fleet = payload
        .fleet_json
        .as_ref()
        .context("approval omitted the fleet")?;
    let peers = payload
        .home_peers_json
        .as_ref()
        .context("approval omitted home peers")?;
    let user = payload.cert.user_public_key;
    let id = hex::encode(user);
    // Validate every transferred object before publishing any of it.
    let projected: FleetProjection = serde_json::from_slice(fleet)?;
    if projected.identity_id != id
        || projected.schema_version != 1
        || projected.certificates.len() > 256
        || projected.revocations.len() > 256
        || !projected
            .certificates
            .iter()
            .any(|cert| cert.device_public_key == payload.cert.device_public_key)
        || projected
            .certificates
            .iter()
            .any(|cert| verify_device_cert(cert).is_err())
        || projected
            .revocations
            .iter()
            .any(|item| verify_revocation_entry(item).is_err())
    {
        bail!("approval returned an invalid device fleet");
    }
    let identity_value: serde_json::Value = serde_json::from_slice(identity)?;
    let threshold = identity_value
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
    let home_entries: Vec<HomePeerRegistration> = serde_json::from_slice(peers)?;
    if home_entries.len() > 128
        || home_entries.iter().any(|entry| {
            entry.user_public_key != id
                || entry.relay_workspace_ids.len() > 4096
                || !qos_relay::valid_home_peer_label(&entry.label)
                || hex::decode(&entry.relay_service_node_id)
                    .ok()
                    .is_none_or(|bytes| bytes.len() != 32)
                || !valid_relay_endpoint_hint(entry)
                || entry.device_cert.as_ref().is_none_or(|cert| {
                    verify_device_cert(cert).is_err()
                        || cert.user_public_key != user
                        || hex::encode(cert.device_public_key) != entry.device_node_id
                })
        })
    {
        bail!("approval returned an invalid home-peer registry");
    }
    std::fs::create_dir_all(identity_dir)?;
    let record_path = identity_dir.join(format!("identity-{}.json", &id[..55]));
    atomic_file::write_with_mode(&record_path, identity, 0o600)?;
    let registry_path = identity_dir.join("registry.json");
    let mut registry = std::fs::read(&registry_path)
        .ok()
        .filter(|bytes| bytes.len() <= 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "schema_version": 1,
                "default_identity": id,
                "identities": {},
                "workspace_identities": {}
            })
        });
    registry["default_identity"] = serde_json::json!(id);
    registry["identities"][&id] = serde_json::json!({
        "id": id,
        "display_name": "My qOS identity",
        "record_path": record_path,
        "user_public_key": user
    });
    atomic_file::write_with_mode(
        &registry_path,
        &serde_json::to_vec_pretty(&registry)?,
        0o600,
    )?;
    atomic_file::write_creating_parent(&fleet_path(identity_dir, user), fleet)?;
    atomic_file::write_with_mode(&identity_root.join("home-peers.json"), peers, 0o644)?;
    #[cfg(unix)]
    for path in [
        identity_dir.to_path_buf(),
        identity_dir.join("fleets"),
        record_path.clone(),
        registry_path,
        fleet_path(identity_dir, user),
        identity_root.join("home-peers.json"),
    ] {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `path` is a live NUL-terminated string and uid/gid came
        // from the user's pre-existing identity directory (or its parent).
        if unsafe { libc::chown(path.as_ptr(), owner.0, owner.1) } != 0 {
            return Err(std::io::Error::last_os_error()).context("restore identity file ownership");
        }
    }
    Ok(record_path)
}

fn checked_personal_projection(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    payload: &ResponsePayload,
) -> anyhow::Result<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> {
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
    let id = hex::encode(user);
    let projected: FleetProjection = serde_json::from_slice(fleet)?;
    if projected.schema_version != 1
        || projected.identity_id != id
        || projected.certificates.len() > 256
        || projected.revocations.len() > 256
        || !projected
            .certificates
            .iter()
            .any(|cert| cert.device_public_key == payload.cert.device_public_key)
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
    let identity_value: serde_json::Value = serde_json::from_slice(identity)?;
    let threshold = identity_value
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
    let incoming: Vec<HomePeerRegistration> = serde_json::from_slice(transferred_peers)?;
    if incoming.len() > 128
        || incoming.iter().any(|entry| {
            entry.user_public_key != id
                || entry.relay_workspace_ids.len() > 4096
                || !qos_relay::valid_home_peer_label(&entry.label)
                || hex::decode(&entry.device_node_id)
                    .ok()
                    .is_none_or(|key| key.len() != 32)
                || hex::decode(&entry.relay_service_node_id)
                    .ok()
                    .is_none_or(|key| key.len() != 32)
                || !valid_relay_endpoint_hint(entry)
                || entry.device_cert.as_ref().is_none_or(|cert| {
                    verify_device_cert(cert).is_err()
                        || cert.user_public_key != user
                        || hex::encode(cert.device_public_key) != entry.device_node_id
                })
        })
    {
        bail!("approval returned an invalid home-peer registry");
    }
    let registry_path = identity_dir.join("registry.json");
    let mut registry = match std::fs::read(&registry_path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .context("existing identity registry is invalid")?
        }
        Ok(_) => bail!("existing identity registry exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({
            "schema_version": 1,
            "default_identity": id,
            "identities": {},
            "workspace_identities": {}
        }),
        Err(error) => return Err(error.into()),
    };
    if !registry.is_object()
        || !registry["identities"].is_object()
        || !registry["workspace_identities"].is_object()
    {
        bail!("existing identity registry has an invalid schema");
    }
    let record_path = identity_dir.join(format!("identity-{}.json", &id[..55]));
    registry["default_identity"] = serde_json::json!(id);
    registry["identities"][&id] = serde_json::json!({
        "id": id,
        "display_name": "My qOS identity",
        "record_path": record_path,
        "user_public_key": user
    });
    let peers_path = identity_root.join("home-peers.json");
    let mut existing: Vec<HomePeerRegistration> = match std::fs::read(&peers_path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            serde_json::from_slice(&bytes).context("existing home-peer registry is invalid")?
        }
        Ok(_) => bail!("existing home-peer registry exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if existing.len() > 128
        || existing.iter().any(|entry| {
            entry.schema_version > HomePeerRegistration::CURRENT_SCHEMA
                || entry.relay_workspace_ids.len() > 4096
                || !qos_relay::valid_home_peer_label(&entry.label)
                || hex::decode(&entry.device_node_id)
                    .ok()
                    .is_none_or(|key| key.len() != 32)
                || hex::decode(&entry.relay_service_node_id)
                    .ok()
                    .is_none_or(|key| key.len() != 32)
                || hex::decode(&entry.user_public_key)
                    .ok()
                    .is_none_or(|key| key.len() != 32)
                || !valid_relay_endpoint_hint(entry)
                || entry.device_cert.as_ref().is_none_or(|cert| {
                    verify_device_cert(cert).is_err()
                        || hex::encode(cert.device_public_key) != entry.device_node_id
                        || hex::encode(cert.user_public_key) != entry.user_public_key
                })
        })
    {
        bail!("existing home-peer registry violates its bounded verified schema");
    }
    // A response projects routes for one identity. Do not use it as a global
    // snapshot and erase other identities' independently verified routes.
    existing.retain(|entry| entry.user_public_key != id);
    existing.extend(incoming);
    if existing.len() > 128 {
        bail!("merged home-peer registry is full");
    }
    existing.sort_by(|left, right| {
        left.user_public_key
            .cmp(&right.user_public_key)
            .then_with(|| left.device_node_id.cmp(&right.device_node_id))
    });
    Ok((
        identity.clone(),
        fleet.clone(),
        serde_json::to_vec_pretty(&existing)?,
        serde_json::to_vec_pretty(&registry)?,
    ))
}

fn stage_personal_enrollment(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    paths: &IdentityPaths,
    payload: &ResponsePayload,
    requester_lease: Option<qos_relay::ProcessLease>,
) -> anyhow::Result<(PathBuf, PersonalEnrollmentMarker)> {
    let (identity, fleet, peers, registry) =
        checked_personal_projection(identity_root, identity_dir, payload)?;
    let stage_name = format!(
        "personal-{}-{}",
        hex::encode(payload.nonce),
        hex::encode(payload.cert.device_public_key)
    );
    let stage_dir = enrollment_stage_dir(identity_root).join(&stage_name);
    std::fs::create_dir_all(&stage_dir)?;
    let cert = qos_user_identity::canonical::canonical_bytes(&payload.cert)?;
    atomic_file::write_with_mode(&staged(&stage_dir, "record.json"), &identity, 0o600)?;
    atomic_file::write_with_mode(&staged(&stage_dir, "registry.json"), &registry, 0o600)?;
    atomic_file::write(&staged(&stage_dir, "fleet.json"), &fleet)?;
    atomic_file::write_with_mode(&staged(&stage_dir, "home-peers.json"), &peers, 0o644)?;
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
    requester_lease: Option<qos_relay::ProcessLease>,
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
    let cert: DeviceCert = qos_user_identity::canonical::from_canonical_bytes(&cert_bytes)?;
    if cert.user_public_key != marker.user_public_key
        || cert.device_public_key != marker.device_public_key
        || verify_device_cert(&cert).is_err()
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
    let record = marker.identity_dir.join(format!(
        "identity-{}.json",
        &hex::encode(marker.user_public_key)[..55]
    ));
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

#[allow(dead_code)]
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
) -> anyhow::Result<()> {
    let path = identity_root.join("home-peers.json");
    if !qos_relay::valid_home_peer_label(&cert.device_label) || relay_workspace_ids.len() > 4096 {
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
                || !qos_relay::valid_home_peer_label(&entry.label)
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
        relay_endpoint: None,
        label: cert.device_label.clone(),
        user_public_key: hex::encode(cert.user_public_key),
        device_cert: Some(cert.clone()),
        relay_workspace_ids: relay_workspace_ids.to_vec(),
    });
    if registrations.len() > 128 {
        bail!("home-peer registry is full");
    }
    registrations.sort_by(|left, right| left.device_node_id.cmp(&right.device_node_id));
    atomic_file::write_with_mode(&path, &serde_json::to_vec_pretty(&registrations)?, 0o644)?;
    Ok(())
}

fn response_preimage(payload: &ResponsePayload) -> anyhow::Result<Vec<u8>> {
    let mut preimage = RESPONSE_DOMAIN.to_vec();
    preimage.extend(postcard::to_stdvec(payload)?);
    Ok(preimage)
}

fn sign_response(
    payload: ResponsePayload,
    root: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Response> {
    let signature = root.sign(&response_preimage(&payload)?).to_bytes().to_vec();
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
    let number = u32::from_le_bytes(digest.as_bytes()[..4].try_into().expect("four bytes"));
    format!("{:06}", number % 1_000_000)
}

fn valid_solicitation(solicitation: &PairSolicitation) -> bool {
    solicitation.magic == PAIR_MAGIC
        && VerifyingKey::from_bytes(&solicitation.device_public_key).is_ok()
        && solicitation.relay_service_node_id != [0; 32]
        && qos_relay::valid_home_peer_label(&solicitation.label)
}

/// Bind the well-known LAN-discovery port without making an already-running
/// discovery view or approval sheet prevent another qOS surface from joining
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
    let deadline = std::time::Instant::now() + socket_timeout(timeout);
    let mut packet = [0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut packet) {
            Ok((len, _)) => {
                let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(&packet[..len])
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
    bail!("no matching unprovisioned qOS device was found nearby")
}

fn pair_offer_key(
    shared: &[u8; 32],
    recipient: [u8; 32],
    ephemeral_public_key: [u8; 32],
) -> [u8; 32] {
    let mut input = PAIR_DOMAIN.to_vec();
    input.extend(shared);
    input.extend(recipient);
    input.extend(ephemeral_public_key);
    *blake3::hash(&input).as_bytes()
}

fn encrypt_pair_offer(token: &str, recipient: [u8; 32]) -> anyhow::Result<Vec<u8>> {
    let mut seed = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let ephemeral = x25519_dalek::StaticSecret::from(seed);
    let ephemeral_public_key = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let recipient_public = VerifyingKey::from_bytes(&recipient)?
        .to_montgomery()
        .to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(recipient_public));
    let key = pair_offer_key(shared.as_bytes(), recipient, ephemeral_public_key);
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = ChaCha20Poly1305::new((&key).into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: token.as_bytes(),
                aad: &[PAIR_DOMAIN, &recipient].concat(),
            },
        )
        .map_err(|_| anyhow::anyhow!("encrypt nearby pairing offer"))?;
    postcard::to_stdvec(&PairOffer {
        ephemeral_public_key,
        nonce,
        ciphertext,
    })
    .map_err(Into::into)
}

fn decrypt_pair_offer(
    bytes: &[u8],
    recipient: &ed25519_dalek::SigningKey,
) -> anyhow::Result<String> {
    let offer: PairOffer = postcard::from_bytes(bytes)?;
    let recipient_public = recipient.verifying_key().to_bytes();
    let recipient_secret = x25519_dalek::StaticSecret::from(recipient.to_scalar_bytes());
    let shared =
        recipient_secret.diffie_hellman(&x25519_dalek::PublicKey::from(offer.ephemeral_public_key));
    let key = pair_offer_key(
        shared.as_bytes(),
        recipient_public,
        offer.ephemeral_public_key,
    );
    let plaintext = ChaCha20Poly1305::new((&key).into())
        .decrypt(
            Nonce::from_slice(&offer.nonce),
            Payload {
                msg: &offer.ciphertext,
                aad: &[PAIR_DOMAIN, &recipient_public].concat(),
            },
        )
        .map_err(|_| anyhow::anyhow!("nearby pairing offer was not encrypted for this server"))?;
    String::from_utf8(plaintext).context("nearby pairing offer is not text")
}

fn response_aad(token_nonce: [u8; 16], recipient: [u8; 32]) -> Vec<u8> {
    let mut aad = RESPONSE_DOMAIN.to_vec();
    aad.extend(token_nonce);
    aad.extend(recipient);
    aad
}

fn request_key(shared: &[u8; 32], ephemeral_public_key: [u8; 32]) -> [u8; 32] {
    let mut input = b"qos-host-enrollment-request-key-v1\0".to_vec();
    input.extend(shared);
    input.extend(ephemeral_public_key);
    *blake3::hash(&input).as_bytes()
}

fn encrypt_request(
    request: &Request,
    issuer_device_public_key: [u8; 32],
) -> anyhow::Result<(Vec<u8>, x25519_dalek::StaticSecret)> {
    let mut seed = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let ephemeral = x25519_dalek::StaticSecret::from(seed);
    let ephemeral_public_key = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let issuer_public = VerifyingKey::from_bytes(&issuer_device_public_key)?
        .to_montgomery()
        .to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(issuer_public));
    let key = request_key(shared.as_bytes(), ephemeral_public_key);
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = ChaCha20Poly1305::new((&key).into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &postcard::to_stdvec(request)?,
                aad: &ephemeral_public_key,
            },
        )
        .map_err(|_| anyhow::anyhow!("encrypt enrollment request"))?;
    Ok((
        postcard::to_stdvec(&EncryptedRequest {
            ephemeral_public_key,
            nonce,
            ciphertext,
        })?,
        ephemeral,
    ))
}

fn decrypt_request(
    bytes: &[u8],
    issuer_device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<(Request, [u8; 32])> {
    let encrypted: EncryptedRequest = postcard::from_bytes(bytes)?;
    let issuer_secret = x25519_dalek::StaticSecret::from(issuer_device.to_scalar_bytes());
    let shared = issuer_secret.diffie_hellman(&x25519_dalek::PublicKey::from(
        encrypted.ephemeral_public_key,
    ));
    let key = request_key(shared.as_bytes(), encrypted.ephemeral_public_key);
    let plaintext = ChaCha20Poly1305::new((&key).into())
        .decrypt(
            Nonce::from_slice(&encrypted.nonce),
            Payload {
                msg: &encrypted.ciphertext,
                aad: &encrypted.ephemeral_public_key,
            },
        )
        .map_err(|_| anyhow::anyhow!("enrollment request was not encrypted for this issuer"))?;
    Ok((
        postcard::from_bytes(&plaintext)?,
        encrypted.ephemeral_public_key,
    ))
}

fn response_key(shared: &[u8; 32], token_nonce: [u8; 16], recipient: [u8; 32]) -> [u8; 32] {
    let mut input = b"qos-host-enrollment-key-v1\0".to_vec();
    input.extend(shared);
    input.extend(token_nonce);
    input.extend(recipient);
    *blake3::hash(&input).as_bytes()
}

fn encrypt_response(
    response: &Response,
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    recipient_ephemeral_public_key: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let mut ephemeral_seed = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut ephemeral_seed);
    let ephemeral = x25519_dalek::StaticSecret::from(ephemeral_seed);
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(
        recipient_ephemeral_public_key,
    ));
    let key = response_key(shared.as_bytes(), token_nonce, recipient);
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = ChaCha20Poly1305::new((&key).into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &postcard::to_stdvec(response)?,
                aad: &response_aad(token_nonce, recipient),
            },
        )
        .map_err(|_| anyhow::anyhow!("encrypt enrollment response"))?;
    Ok(postcard::to_stdvec(&EncryptedResponse {
        ephemeral_public_key: x25519_dalek::PublicKey::from(&ephemeral).to_bytes(),
        nonce,
        ciphertext,
    })?)
}

fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    request_ephemeral: &x25519_dalek::StaticSecret,
) -> anyhow::Result<Response> {
    let encrypted: EncryptedResponse = postcard::from_bytes(bytes)?;
    let shared = request_ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(
        encrypted.ephemeral_public_key,
    ));
    let key = response_key(shared.as_bytes(), token_nonce, recipient);
    let plaintext = ChaCha20Poly1305::new((&key).into())
        .decrypt(
            Nonce::from_slice(&encrypted.nonce),
            Payload {
                msg: &encrypted.ciphertext,
                aad: &response_aad(token_nonce, recipient),
            },
        )
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
    root: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    Ok(postcard::to_stdvec(&Finalized {
        payload,
        signature: root.sign(&encoded).to_bytes().to_vec(),
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
    Ok(u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos(),
    )
    .unwrap_or(u64::MAX))
}

fn socket_timeout(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(1, 300))
}

fn validate_requester(lease: Option<qos_relay::ProcessLease>) -> anyhow::Result<()> {
    if let Some(lease) = lease {
        lease
            .validate()
            .context("the first-use requester exited; nearby pairing was revoked")?;
    }
    Ok(())
}

fn join(
    token_file: PathBuf,
    identity_root: PathBuf,
    label: String,
    timeout: u64,
    home_peer_output: PathBuf,
) -> anyhow::Result<()> {
    let token_text = std::fs::read_to_string(&token_file)
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

fn join_with_token(
    token_text: &str,
    identity_root: PathBuf,
    label: String,
    timeout: u64,
    kind: EnrollmentKind,
    home_peer_output: Option<PathBuf>,
    identity_dir: Option<PathBuf>,
    requester_lease: Option<qos_relay::ProcessLease>,
) -> anyhow::Result<()> {
    validate_requester(requester_lease)?;
    let token = decode_enrollment_token(token_text).context("decode enrollment token")?;
    verify_enrollment_token(&token, now_ns()?).context("verify enrollment token")?;
    if kind == EnrollmentKind::HomePeer
        && (!token.capabilities.can_act_as_host || !token.capabilities.can_read_all_workspaces)
    {
        bail!("enrollment token does not grant home-server capabilities");
    }
    if kind == EnrollmentKind::PersonalDevice
        && (token.capabilities.can_act_as_host || token.capabilities.can_read_all_workspaces)
    {
        bail!("enrollment token grants the wrong device role");
    }
    let paths = IdentityPaths::under(&identity_root);
    paths.ensure_directories()?;
    if kind == EnrollmentKind::PersonalDevice {
        recover_personal_enrollments(
            &identity_root,
            identity_dir
                .as_deref()
                .context("identity directory is required")?,
            &paths,
            |_| validate_requester(requester_lease),
        )?;
    }
    let (device, _) = qos_user_identity::persist::load_or_create_device_key(&paths)?;
    let request = Request {
        token: token_text.to_owned(),
        device_public_key: device.verifying_key().to_bytes(),
        relay_service_node_id: qos_relay::daemon::relay_service_node_id(&device),
        label: label.chars().take(80).collect(),
        kind,
    };
    let (bytes, request_ephemeral) = encrypt_request(&request, token.issuer_device_public_key)?;
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = std::time::Instant::now() + socket_timeout(timeout);
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT);
    let mut reply = [0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        socket.send_to(&bytes, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, sender)) => {
                let response_bytes = reply[..len].to_vec();
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
                verify_device_cert(&response.payload.cert)?;
                if kind == EnrollmentKind::HomePeer {
                    let home_peer_toml = response
                        .payload
                        .home_peer_toml
                        .as_ref()
                        .context("approving device omitted home-peer configuration")?;
                    let _: fs::full_replica::HomePeerSet = toml::from_str(home_peer_toml)
                        .context("approving device returned an invalid home-peer configuration")?;
                    atomic_file::write(
                        home_peer_output
                            .as_ref()
                            .context("home-peer output is required")?,
                        home_peer_toml.as_bytes(),
                    )?;
                }
                // Preparing a personal enrollment only touches the private
                // staging area. In particular, an existing device's global
                // user key and default certificate remain untouched until
                // Finalized has been authenticated.
                let staged_personal = if kind == EnrollmentKind::PersonalDevice {
                    Some(stage_personal_enrollment(
                        &identity_root,
                        identity_dir
                            .as_deref()
                            .context("identity directory is required")?,
                        &paths,
                        &response.payload,
                        requester_lease,
                    )?)
                } else {
                    None
                };
                let ack = make_ack(&response_bytes, token.nonce, &device)?;
                socket.set_read_timeout(Some(Duration::from_millis(300)))?;
                let completion_deadline = std::time::Instant::now() + Duration::from_secs(10);
                while std::time::Instant::now() < completion_deadline {
                    validate_requester(requester_lease)?;
                    socket.send_to(&ack, sender)?;
                    match socket.recv_from(&mut reply) {
                        Ok((final_len, final_sender)) if final_sender == sender => {
                            if verify_finalized(
                                &reply[..final_len],
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
                                            &identity_root,
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
                                if kind == EnrollmentKind::HomePeer {
                                    qos_user_identity::persist::write_device_cert(
                                        &paths.device_cert_for_user(
                                            &response.payload.cert.user_public_key,
                                        ),
                                        &response.payload.cert,
                                    )?;
                                    qos_user_identity::persist::write_device_cert(
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
                                ] {
                                    if let Some(directory) = directory {
                                        restore_owner_from_directory(directory, &identity_root)?;
                                    }
                                }
                                for output in [
                                    &paths.user_pubkey,
                                    &paths.device_privkey,
                                    &paths.device_cert_for_user(
                                        &response.payload.cert.user_public_key,
                                    ),
                                    &paths.device_cert,
                                ] {
                                    restore_owner_from_directory(output, &identity_root)?;
                                }
                                consume_nonce(
                                    &identity_root,
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
    bail!("no approving qOS device answered before the enrollment window closed")
}

fn pair(
    identity_root: PathBuf,
    label: String,
    timeout: u64,
    kind: EnrollmentKind,
    home_peer_output: Option<PathBuf>,
    identity_dir: Option<PathBuf>,
    status_file: Option<PathBuf>,
    requester_lease: Option<qos_relay::ProcessLease>,
) -> anyhow::Result<()> {
    if kind == EnrollmentKind::PersonalDevice && requester_lease.is_none() {
        bail!("personal-device pairing requires a live first-use requester");
    }
    validate_requester(requester_lease)?;
    let paths = IdentityPaths::under(&identity_root);
    paths.ensure_directories()?;
    let (device, _) = qos_user_identity::persist::load_or_create_device_key(&paths)?;
    let relay_service_node_id = qos_relay::daemon::relay_service_node_id(&device);
    let label: String = label.chars().take(80).collect();
    if !qos_relay::valid_home_peer_label(&label) {
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
    if let Some(path) = &status_file {
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
    let deadline = std::time::Instant::now() + socket_timeout(timeout);
    let mut reply = [0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        socket.send_to(&solicitation, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, _sender)) => {
                let Ok(token_text) = decrypt_pair_offer(&reply[..len], &device) else {
                    continue;
                };
                let Ok(token) = decode_enrollment_token(token_text.trim()) else {
                    continue;
                };
                if verify_enrollment_token(&token, now_ns()?).is_err()
                    || (kind == EnrollmentKind::HomePeer
                        && (!token.capabilities.can_act_as_host
                            || !token.capabilities.can_read_all_workspaces))
                    || (kind == EnrollmentKind::PersonalDevice
                        && (token.capabilities.can_act_as_host
                            || token.capabilities.can_read_all_workspaces))
                {
                    continue;
                }
                println!("Nearby qOS device approved code {code}");
                join_with_token(
                    token_text.trim(),
                    identity_root,
                    label,
                    timeout,
                    kind,
                    home_peer_output,
                    identity_dir,
                    requester_lease,
                )?;
                if let Some(path) = &status_file {
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
    bail!("no trusted qOS device approved nearby pairing before the window closed")
}

fn approve(
    identity_root: PathBuf,
    identity_dir: Option<PathBuf>,
    timeout: u64,
    workspace_cache: PathBuf,
    acl: PathBuf,
    mut relay_workspace_ids: Vec<u64>,
    expected_device: Option<String>,
    expected_service: Option<String>,
    kind: EnrollmentKind,
    ready: Option<std::sync::mpsc::Sender<()>>,
) -> anyhow::Result<()> {
    let expected: Option<([u8; 32], [u8; 32])> = match (expected_device, expected_service) {
        (Some(device), Some(service)) => Some((
            hex::decode(device)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected device ID has the wrong length"))?,
            hex::decode(service)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected service ID has the wrong length"))?,
        )),
        (None, None) => None,
        _ => bail!("expected device and service IDs must be supplied together"),
    };
    let paths = IdentityPaths::under(&identity_root);
    let root = qos_user_identity::persist::read_signing_key(&paths.user_privkey)
        .context("this device does not hold the identity root needed to approve enrollment")?;
    let device = qos_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let user_public_key = root.verifying_key().to_bytes();
    if kind == EnrollmentKind::HomePeer {
        relay_workspace_ids.push(qos_relay::personal_continuity_workspace_id(
            &user_public_key,
        ));
    }
    let issuer_cert =
        qos_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&user_public_key))
            .or_else(|_| qos_user_identity::persist::read_device_cert(&paths.device_cert))?;
    verify_device_cert(&issuer_cert)?;
    if issuer_cert.device_public_key != device.verifying_key().to_bytes()
        || !issuer_cert.capabilities.can_admit_devices
    {
        bail!("this device is not authorised to approve enrollment");
    }
    let socket = bind_pair_listener()?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    if let Some(ready) = ready {
        let _ = ready.send(());
    }
    let mut packet = [0_u8; MAX_PACKET];
    let request_deadline = std::time::Instant::now() + socket_timeout(timeout);
    let mut selected_nearby: Option<(PairSolicitation, String, Vec<u8>)> = None;
    let (request, request_ephemeral_public_key, token, sender) = loop {
        if std::time::Instant::now() >= request_deadline {
            bail!("no valid matching nearby device request arrived");
        }
        let (len, sender) = match socket.recv_from(&mut packet) {
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
        if let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(&packet[..len]) {
            if !valid_solicitation(&solicitation)
                || solicitation.kind != kind
                || expected.is_some_and(|(device, service)| {
                    solicitation.device_public_key != device
                        || solicitation.relay_service_node_id != service
                })
            {
                continue;
            }
            let selection_matches = selected_nearby.as_ref().is_none_or(|(selected, _, _)| {
                selected.device_public_key == solicitation.device_public_key
                    && selected.relay_service_node_id == solicitation.relay_service_node_id
            });
            if !selection_matches {
                continue;
            }
            if selected_nearby.is_none() {
                let mut nonce = [0_u8; 16];
                rand::rngs::OsRng.fill_bytes(&mut nonce);
                let expires_at_ns = now_ns()?.saturating_add(5 * 60 * 1_000_000_000);
                let capabilities = match kind {
                    EnrollmentKind::HomePeer => DeviceCapabilities::host(),
                    EnrollmentKind::PersonalDevice => DeviceCapabilities::member(),
                };
                let token = mint_enrollment_token(
                    user_public_key,
                    capabilities,
                    expires_at_ns,
                    nonce,
                    &device,
                )?;
                let token_text = encode_enrollment_token(&token)?;
                let offer = encrypt_pair_offer(&token_text, solicitation.device_public_key)?;
                let code = pairing_code(
                    solicitation.device_public_key,
                    solicitation.relay_service_node_id,
                );
                println!(
                    "Approving nearby device '{}' with code {code}",
                    solicitation.label
                );
                selected_nearby = Some((solicitation, token_text, offer));
            }
            if let Some((_, _, offer)) = &selected_nearby {
                socket.send_to(offer, sender)?;
            }
            continue;
        }
        let Ok((request, request_ephemeral_public_key)) = decrypt_request(&packet[..len], &device)
        else {
            continue;
        };
        let Ok(token) = decode_enrollment_token(request.token.trim()) else {
            continue;
        };
        if verify_enrollment_token(&token, now_ns()?).is_err()
            || token.user_public_key != user_public_key
            || token.issuer_device_public_key != device.verifying_key().to_bytes()
            || request.kind != kind
            || (kind == EnrollmentKind::HomePeer
                && (!token.capabilities.can_act_as_host
                    || !token.capabilities.can_read_all_workspaces))
            || (kind == EnrollmentKind::PersonalDevice
                && (token.capabilities.can_act_as_host
                    || token.capabilities.can_read_all_workspaces))
        {
            continue;
        }
        if let Some((selected, selected_token, _)) = &selected_nearby {
            if request.device_public_key != selected.device_public_key
                || request.relay_service_node_id != selected.relay_service_node_id
                || request.label != selected.label
                || request.kind != selected.kind
                || request.token != *selected_token
            {
                continue;
            }
        }
        break (request, request_ephemeral_public_key, token, sender);
    };
    let mut cert = DeviceCert {
        user_public_key,
        device_public_key: request.device_public_key,
        device_label: request.label,
        capabilities: token.capabilities,
        issued_at_ns: now_ns()?,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root)?;
    let (home_peer_toml, public_identity_json, fleet_json, home_peers_json) = match kind {
        EnrollmentKind::HomePeer => {
            let ticket_text = std::fs::read_to_string(workspace_cache.join("workspace-ticket"))
                .context("the local Files workspace is not ready to share")?;
            let ticket = fs::Ticket::decode(ticket_text.trim())
                .context("local workspace ticket is invalid")?;
            let capability_json =
                std::fs::read_to_string(&acl).with_context(|| format!("read {}", acl.display()))?;
            let capabilities = serde_json::from_str(&capability_json)
                .context("filesystem capability export is invalid")?;
            let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                ticket.workspace_id(),
                1,
                capabilities,
                false,
                &root,
            )?;
            let home_peer = fs::full_replica::HomePeerSet {
                status_file: PathBuf::from("/var/lib/qos/full-replica/status.json"),
                workspaces: vec![fs::full_replica::HomePeerBundle {
                    ticket: ticket.encode()?,
                    cache_dir: PathBuf::from("/var/lib/qos/full-replica")
                        .join(ticket.workspace_id().to_string()),
                    capability_json,
                    authority: Some(authority),
                    principals: Vec::new(),
                    relay_workspace_ids: {
                        relay_workspace_ids.sort_unstable();
                        relay_workspace_ids.dedup();
                        relay_workspace_ids.clone()
                    },
                }],
            };
            (Some(toml::to_string_pretty(&home_peer)?), None, None, None)
        }
        EnrollmentKind::PersonalDevice => {
            let identity_dir = identity_dir
                .as_deref()
                .context("identity directory is required for personal-device approval")?;
            (
                None,
                Some(public_identity_record(user_public_key, "My qOS identity")?),
                Some(project_personal_device(identity_dir, &cert)?),
                Some(bounded_home_peer_registry(&identity_root, user_public_key)?),
            )
        }
    };
    let signed_response = sign_response(
        ResponsePayload {
            nonce: token.nonce,
            recipient_device_public_key: request.device_public_key,
            relay_service_node_id: request.relay_service_node_id,
            cert,
            home_peer_toml,
            public_identity_json,
            fleet_json,
            home_peers_json,
        },
        &root,
    )?;
    let response = encrypt_response(
        &signed_response,
        token.nonce,
        request.device_public_key,
        request_ephemeral_public_key,
    )?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let ack_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut acked = false;
    while std::time::Instant::now() < ack_deadline {
        socket.send_to(&response, sender)?;
        match socket.recv_from(&mut packet) {
            Ok((len, ack_sender)) if ack_sender == sender => {
                if verify_ack(
                    &packet[..len],
                    &response,
                    token.nonce,
                    request.device_public_key,
                )
                .is_ok()
                {
                    acked = true;
                    break;
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
    if !acked {
        bail!("nearby device did not acknowledge its signed configuration");
    }
    match kind {
        EnrollmentKind::HomePeer => register_home_peer(
            &identity_root,
            &signed_response.payload.cert,
            signed_response.payload.relay_service_node_id,
            &relay_workspace_ids,
        )?,
        EnrollmentKind::PersonalDevice => commit_personal_device(
            identity_dir
                .as_deref()
                .context("identity directory is required")?,
            user_public_key,
            signed_response
                .payload
                .fleet_json
                .as_deref()
                .context("fleet is required")?,
        )?,
    }
    consume_nonce(&identity_root, &token.nonce, request.device_public_key)?;
    let finalized = make_finalized(&response, token.nonce, request.device_public_key, &root)?;
    for _ in 0..8 {
        socket.send_to(&finalized, sender)?;
        std::thread::sleep(Duration::from_millis(125));
    }
    println!(
        "approved nearby device {}",
        hex::encode(signed_response.payload.cert.device_public_key)
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Discover { timeout } => discover(timeout, EnrollmentKind::HomePeer),
        Command::DiscoverDevice { timeout } => discover(timeout, EnrollmentKind::PersonalDevice),
        Command::Join {
            token_file,
            identity_root,
            label,
            timeout,
            home_peer_output,
        } => join(token_file, identity_root, label, timeout, home_peer_output),
        Command::Pair {
            identity_root,
            label,
            timeout,
            home_peer_output,
        } => pair(
            identity_root,
            label,
            timeout,
            EnrollmentKind::HomePeer,
            Some(home_peer_output),
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
            identity_root,
            label,
            timeout,
            EnrollmentKind::PersonalDevice,
            None,
            Some(identity_dir),
            status_file,
            Some(qos_relay::ProcessLease::from_parts(
                requester_pid,
                requester_start_time,
            )),
        ),
        Command::Approve {
            identity_root,
            timeout,
            workspace_cache,
            acl,
            relay_workspace_ids,
            expected_device,
            expected_service,
        } => approve(
            identity_root,
            None,
            timeout,
            workspace_cache,
            acl,
            relay_workspace_ids,
            expected_device,
            expected_service,
            EnrollmentKind::HomePeer,
            None,
        ),
        Command::ApproveDevice {
            identity_root,
            identity_dir,
            timeout,
            expected_device,
            expected_service,
        } => approve(
            identity_root,
            Some(identity_dir),
            timeout,
            PathBuf::new(),
            PathBuf::new(),
            Vec::new(),
            expected_device,
            expected_service,
            EnrollmentKind::PersonalDevice,
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_user_identity::DeviceCapabilities;

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
            public_identity_json: Some(public_identity_record(user, "My qOS identity").unwrap()),
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
            home_peer_toml: Some("status_file = '/var/lib/qos/full-replica/status.json'".into()),
            public_identity_json: None,
            fleet_json: None,
            home_peers_json: None,
        }
    }

    #[test]
    fn signed_response_rejects_bundle_tampering() {
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

    #[test]
    fn provisioning_bundle_is_encrypted_for_the_recipient_device() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut request_seed = [0_u8; 32];
        request_seed[0] = 11;
        let request_ephemeral = x25519_dalek::StaticSecret::from(request_seed);
        let request_public = x25519_dalek::PublicKey::from(&request_ephemeral).to_bytes();
        let mut payload = payload(&root);
        payload.home_peer_toml = Some("ticket = 'secret-ticket-material'".into());
        let signed = sign_response(payload, &root).unwrap();
        let encrypted = encrypt_response(
            &signed,
            [3; 16],
            device.verifying_key().to_bytes(),
            request_public,
        )
        .unwrap();
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

    #[test]
    fn bearer_token_is_encrypted_for_the_issuing_device() {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let request = Request {
            token: "reusable-secret-token".into(),
            device_public_key: [7; 32],
            relay_service_node_id: [8; 32],
            label: "NAS".into(),
            kind: EnrollmentKind::HomePeer,
        };
        let (encrypted, _) = encrypt_request(&request, issuer.verifying_key().to_bytes()).unwrap();
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

    #[test]
    fn personal_projection_loads_without_root_signing_material() {
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
            public_identity_json: Some(public_identity_record(user, "My qOS identity").unwrap()),
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

    #[test]
    fn personal_enrollment_staging_keeps_an_existing_device_unchanged() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let old_root = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
        let old_device = ed25519_dalek::SigningKey::from_bytes(&[54; 32]);
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        paths.ensure_directories().unwrap();
        qos_user_identity::persist::write_public_key(
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
        qos_user_identity::persist::write_device_cert(&paths.device_cert, &old_cert).unwrap();
        let before_key = std::fs::read(&paths.user_pubkey).unwrap();
        let before_cert = std::fs::read(&paths.device_cert).unwrap();

        let mut requester = std::process::Command::new("/bin/sh")
            .args(["-c", "while :; do sleep 1; done"])
            .spawn()
            .unwrap();
        let lease = qos_relay::ProcessLease::capture(requester.id()).unwrap();
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
        recover_personal_enrollments(
            identity_root.path(),
            identity_dir.path(),
            &paths,
            |_| Ok(()),
        )
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
        let retry_lease = qos_relay::ProcessLease::capture(std::process::id()).unwrap();
        recover_personal_enrollments(identity_root.path(), identity_dir.path(), &paths, |_| {
            retry_lease.validate().map_err(Into::into)
        })
        .unwrap();
        assert_eq!(
            std::fs::read(&paths.user_pubkey).unwrap(),
            root.verifying_key().to_bytes()
        );
        assert_eq!(
            qos_user_identity::persist::read_device_cert(&paths.device_cert)
                .unwrap()
                .device_public_key,
            member.verifying_key().to_bytes()
        );
    }

    #[test]
    fn expired_requester_lease_blocks_personal_identity_publication() {
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
        let lease = qos_relay::ProcessLease::capture(requester.id()).unwrap();
        let payload = personal_payload(&root, &member, [12; 16]);
        let (marker_path, marker) = stage_personal_enrollment(
            identity_root.path(),
            &identity_dir,
            &paths,
            &payload,
            Some(lease),
        )
        .unwrap();
        let unrelated = qos_relay::ProcessLease::capture(std::process::id()).unwrap();
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
        let mut simulated_ready = marker.clone();
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

    #[test]
    fn pair_device_rejects_an_expired_requester_before_creating_identity_state() {
        let mut requester = std::process::Command::new("/bin/sh")
            .args(["-c", "while :; do sleep 1; done"])
            .spawn()
            .unwrap();
        let lease = qos_relay::ProcessLease::capture(requester.id()).unwrap();
        requester.kill().unwrap();
        requester.wait().unwrap();

        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        assert!(
            pair(
                identity_root.path().to_path_buf(),
                "New laptop".into(),
                1,
                EnrollmentKind::PersonalDevice,
                None,
                Some(identity_dir.path().to_path_buf()),
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

    #[test]
    fn personal_projection_merges_and_preserves_unrelated_home_peers() {
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

    #[test]
    fn nearby_personal_device_pairing_transfers_cert_fleet_and_home_peer() {
        let approver_root = tempfile::tempdir().unwrap();
        let approver_identities = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(approver_root.path());
        paths.ensure_directories().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        qos_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
        qos_user_identity::persist::write_public_key(
            &paths.user_pubkey,
            &root.verifying_key().to_bytes(),
        )
        .unwrap();
        qos_user_identity::persist::write_device_key(&paths.device_privkey, &issuer).unwrap();
        let mut issuer_cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: issuer.verifying_key().to_bytes(),
            device_label: "Trusted laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut issuer_cert, &root).unwrap();
        qos_user_identity::persist::write_device_cert(&paths.device_cert, &issuer_cert).unwrap();
        qos_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&root.verifying_key().to_bytes()),
            &issuer_cert,
        )
        .unwrap();
        atomic_file::write_creating_parent(
            &fleet_path(approver_identities.path(), root.verifying_key().to_bytes()),
            &serde_json::to_vec_pretty(&FleetProjection {
                schema_version: 1,
                identity_id: hex::encode(root.verifying_key().to_bytes()),
                certificates: vec![issuer_cert],
                revocations: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
        let home_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let mut home_cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: home_device.verifying_key().to_bytes(),
            device_label: "Home peer".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut home_cert, &root).unwrap();
        register_home_peer(
            approver_root.path(),
            &home_cert,
            [43; 32],
            &[qos_relay::personal_continuity_workspace_id(
                &root.verifying_key().to_bytes(),
            )],
        )
        .unwrap();

        let server_root = approver_root.path().to_path_buf();
        let server_identities = approver_identities.path().to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let approval = std::thread::spawn(move || {
            approve(
                server_root,
                Some(server_identities),
                15,
                PathBuf::new(),
                PathBuf::new(),
                Vec::new(),
                None,
                None,
                EnrollmentKind::PersonalDevice,
                Some(ready_tx),
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("approval listener became ready");
        let new_root = tempfile::tempdir().unwrap();
        let new_identities = tempfile::tempdir().unwrap();
        let requester = qos_relay::ProcessLease::capture(std::process::id()).unwrap();
        pair(
            new_root.path().to_path_buf(),
            "New laptop".into(),
            15,
            EnrollmentKind::PersonalDevice,
            None,
            Some(new_identities.path().to_path_buf()),
            None,
            Some(requester),
        )
        .unwrap();
        approval.join().unwrap().unwrap();

        let enrolled_paths = IdentityPaths::under(new_root.path());
        let cert =
            qos_user_identity::persist::read_device_cert(&enrolled_paths.device_cert).unwrap();
        assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert!(verify_device_cert(&cert).is_ok());
        assert!(!enrolled_paths.user_privkey.exists());
        let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(new_root.path().join("home-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(peers.len(), 1);
        assert!(peers[0].relay_workspace_ids.contains(
            &qos_relay::personal_continuity_workspace_id(&root.verifying_key().to_bytes())
        ));
        let record = new_identities.path().join(format!(
            "identity-{}.json",
            &hex::encode(root.verifying_key().to_bytes())[..55]
        ));
        let identity = runtime_identity::load_from_path(record).unwrap();
        assert!(
            identity
                .sign(b"root/admin signing must stay unavailable")
                .is_err()
        );
    }

    #[test]
    fn automatic_pair_offer_is_private_to_the_soliciting_server() {
        let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let token = "signed-host-enrollment-token";
        let encrypted = encrypt_pair_offer(token, server.verifying_key().to_bytes()).unwrap();
        assert!(
            !encrypted
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
        assert_eq!(decrypt_pair_offer(&encrypted, &server).unwrap(), token);
        let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        assert!(decrypt_pair_offer(&encrypted, &other).is_err());
    }

    #[test]
    fn nearby_pairing_code_is_stable_and_six_digits() {
        let code = pairing_code([7; 32], [8; 32]);
        assert_eq!(code, pairing_code([7; 32], [8; 32]));
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        assert_ne!(code, pairing_code([7; 32], [9; 32]));
    }

    #[test]
    fn unrelated_lan_datagram_is_not_a_pairing_message() {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        assert!(decrypt_request(b"ambient discovery traffic", &issuer).is_err());
        let mut request_seed = [0_u8; 32];
        request_seed[0] = 11;
        let request_ephemeral = x25519_dalek::StaticSecret::from(request_seed);
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

    #[test]
    fn enrollment_nonce_is_consumed_once() {
        let dir = tempfile::tempdir().unwrap();
        consume_nonce(dir.path(), &[4; 16], [7; 32]).unwrap();
        consume_nonce(dir.path(), &[4; 16], [7; 32]).unwrap();
        assert!(consume_nonce(dir.path(), &[4; 16], [8; 32]).is_err());
    }

    #[test]
    fn approved_home_peer_registration_exposes_distinct_stable_node_ids() {
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
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(dir.path().join("home-peers.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn invalid_existing_home_peer_registry_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home-peers.json");
        atomic_file::write(&path, b"not json").unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        assert!(register_home_peer(dir.path(), &payload(&root).cert, [8; 32], &[2]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
    }

    #[test]
    fn dropped_response_can_be_retried_with_the_same_authenticated_ack() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let response = postcard::to_stdvec(&sign_response(payload(&root), &root).unwrap()).unwrap();
        // The first datagram is modeled as dropped. Retransmission uses the
        // exact same signed bytes, and the recipient ACK binds their key,
        // nonce, and that response digest.
        let retransmitted = response.clone();
        assert_eq!(response, retransmitted);
        let ack = make_ack(&retransmitted, [3; 16], &device).unwrap();
        verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
    }

    #[test]
    fn dropped_all_acks_can_resume_after_process_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let response = postcard::to_stdvec(&sign_response(payload(&root), &root).unwrap()).unwrap();

        // No ACK reached the first approver, so it did not consume the
        // admission nonce. A restarted join/approve exchange may authenticate
        // the same device and finish registration idempotently.
        assert!(!dir.path().join("enrollment-nonces").exists());
        let ack = make_ack(&response, [3; 16], &device).unwrap();
        verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
        consume_nonce(dir.path(), &[3; 16], device.verifying_key().to_bytes()).unwrap();
        let finalized =
            make_finalized(&response, [3; 16], device.verifying_key().to_bytes(), &root).unwrap();
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
