//! LAN/OOB completion for a pre-authorised Tilo device enrollment token.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;
use tilo_relay::root_signer::RootSigner;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use tilo_clock::try_unix_now_ns;
use tilo_relay::HomePeerRegistration;
#[path = "enroll/person_home.rs"]
mod person_home;

use person_home::PersonHome;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationList, decode_enrollment_token,
    encode_enrollment_token, mint_enrollment_token, verify_device_cert, verify_device_cert_at,
    verify_enrollment_token, verify_revocation_entry,
};

use tilo_relay::nearby::{MAX_PACKET, NEARBY_PORT, bind_listener, broadcast};
const RESPONSE_DOMAIN: &[u8] = b"tilo-host-enrollment-response-v1\0";
const PAIR_DOMAIN: &[u8] = b"tilo-nearby-home-peer-offer-v1\0";
/// Solicitation magic: the device reads HPKE pair offers.
const PAIR_MAGIC: [u8; 8] = *b"TLOPAIR2";

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
        /// The account whose identity directory (and status file) this joins
        /// into. Run as root, the join writes them only as that account.
        #[arg(long)]
        person_uid: Option<u32>,
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
        /// The workspace rooms the new device joins. Defaults to the list
        /// this account's session exports.
        #[arg(long)]
        personal_rooms: Option<PathBuf>,
        /// The account whose identity directory and room list this approves
        /// for. Required when run as root, which then reads and writes them
        /// only as that account.
        #[arg(long)]
        person_uid: Option<u32>,
    },
    /// Read or write one file in the person's home as this process's own
    /// account; the child a root-run enrollment delegates to.
    #[command(name = "person-home", hide = true)]
    PersonHome(PersonHomeArgs),
    #[command(flatten)]
    OutOfBand(OutOfBand),
}

/// One access the delegated `person-home` child performs.
#[derive(clap::Args)]
struct PersonHomeArgs {
    operation: person_home::ChildOperation,
    #[arg(long)]
    path: PathBuf,
    #[arg(long)]
    subdir: Option<String>,
    #[arg(long)]
    name: String,
    /// Octal permission bits of a written file.
    #[arg(long, default_value = "600")]
    mode: String,
    #[arg(long, default_value_t = false)]
    create_dir: bool,
    #[arg(long, default_value_t = person_home::MAX_FILE_BYTES)]
    limit: usize,
}

impl PersonHomeArgs {
    fn serve(self) -> anyhow::Result<()> {
        person_home::serve_child(&person_home::ChildRequest {
            operation: self.operation,
            path: self.path,
            subdir: self.subdir,
            name: self.name,
            mode: libc::mode_t::from_str_radix(&self.mode, 8).context("--mode must be octal")?,
            create_dir: self.create_dir,
            limit: self.limit,
        })
    }
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
        /// The account whose identity directory this joins into. Run as
        /// root, the join writes it only as that account.
        #[arg(long)]
        person_uid: Option<u32>,
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
    /// The approver's workspace rooms, which the new device joins as the
    /// same person.
    personal_rooms_json: Option<Vec<u8>>,
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
    /// The account the enrollment was staged for: the one whose home and
    /// peers it publishes into.
    person_uid: u32,
    identity_dir: PathBuf,
    /// `personal-<token>`, with a token from the system's random source, so
    /// nobody can know the name before the join creates it.
    stage_name: String,
    /// The identity being enrolled.
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

const PERSONAL_ENROLLMENT_SCHEMA: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalCommitCheckpoint {
    Record,
    Fleet,
    HomePeers,
    PersonalLinks,
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

/// The commit marker of the enrollment staged as `stage_name`: named by the
/// same unpredictable token.
fn personal_marker_path(identity_root: &std::path::Path, stage_name: &str) -> PathBuf {
    enrollment_commit_dir(identity_root).join(format!("{stage_name}.json"))
}

/// Whether `name` is a stage name [`create_stage_dir`] gives.
fn valid_stage_name(name: &str) -> bool {
    name.strip_prefix("personal-").is_some_and(|token| {
        token.len() == 32
            && token
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The account a join publishes for: the person a root join acts for, else
/// this process's own.
fn enrolled_uid(home: &PersonHome) -> u32 {
    match home {
        PersonHome::Direct => person_home::local::own_uid(),
        PersonHome::Delegated { person, .. } => person.uid,
    }
}

#[cfg(test)]
fn fleet_path(identity_dir: &std::path::Path, user: [u8; 32]) -> PathBuf {
    identity_dir
        .join("fleets")
        .join(format!("{}.json", hex::encode(user)))
}

fn fleet_revokes_device(
    home: &PersonHome,
    identity_dir: &std::path::Path,
    user: [u8; 32],
    device: [u8; 32],
) -> anyhow::Result<bool> {
    let bytes = home
        .read_fleet(identity_dir, user)?
        .context("this identity has no device fleet")?;
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

/// Whether a certificate names one of this person's own devices: one that
/// holds their workspaces, as opposed to an always-on host (recorded as a home
/// peer instead), an administering machine, or someone else's device.
fn certifies_personal_device(cert: &DeviceCert) -> bool {
    cert.capabilities.can_own_workspaces
        && !cert.capabilities.can_act_as_host
        && !cert.capabilities.foreign
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
            && certifies_personal_device(cert)
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
    stage_dir: &std::os::fd::OwnedFd,
    marker: &PersonalEnrollmentMarker,
) -> anyhow::Result<PathBuf> {
    remember_personal_devices(
        identity_root,
        &certified_personal_devices(
            &read_staged(stage_dir, "fleet.json")?,
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

        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
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

/// Publish a public identity file under `identity_root`, at `relative`
/// (`user-identity/pubkey.bin`, `device/cert.bin` or
/// `device/certs/<identity>.bin`).
///
/// The device-trust directories are writable by the device-trust group, so
/// a member could swap a directory on the way for a link. Each component is
/// opened from the one before without following a link, the identity root
/// and the top-level directory must be root's (when this runs as root) and
/// not writable by every account, a missing subdirectory is created like its
/// parent, and the file is created exclusively and renamed into place inside
/// the directory that was checked, with the public mode whatever the umask.
fn publish_device_file(
    identity_root: &std::path::Path,
    relative: &std::path::Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    use person_home::local;
    let components: Vec<&str> = relative
        .iter()
        .map(|part| part.to_str().context("identity file path is not UTF-8"))
        .collect::<anyhow::Result<_>>()?;
    let Some((name, dirs)) = components.split_last() else {
        bail!("identity file path is empty");
    };
    let Some((top, below)) = dirs.split_first() else {
        bail!("identity file {} has no directory", relative.display());
    };
    let euid = local::own_uid();
    let trusted = |fd: &std::os::fd::OwnedFd, what: &str| -> anyhow::Result<()> {
        let stat = local::fstat(fd)?;
        if (euid == 0 && stat.st_uid != 0) || stat.st_mode & 0o002 != 0 {
            bail!("{what} must be root's and not writable by every account");
        }
        Ok(())
    };
    let root = local::open_dir(identity_root)?;
    trusted(&root, &identity_root.display().to_string())?;
    let mut dir = local::open_shared_subdir(&root, top, None)?;
    trusted(&dir, top)?;
    // Directories below take the top-level directory's mode, which was
    // just checked to be root's (when this runs as root).
    let mode = local::fstat(&dir)?.st_mode;
    for part in below {
        dir = local::open_shared_subdir(&dir, part, Some(mode))?;
    }
    local::replace_shared_file(
        &dir,
        name,
        bytes,
        tilo_user_identity::persist::PUBLIC_FILE_MODE,
    )
    .with_context(|| format!("publish {}", relative.display()))
}

/// `path` relative to `identity_root`.
fn under_root<'a>(
    identity_root: &std::path::Path,
    path: &'a std::path::Path,
) -> anyhow::Result<&'a std::path::Path> {
    path.strip_prefix(identity_root)
        .with_context(|| format!("{} is outside the identity root", path.display()))
}

/// Whether this process runs as the owner of `directory`.
fn owns_directory(directory: &std::path::Path) -> anyhow::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid has no preconditions.
        Ok(std::fs::metadata(directory)?.uid() == unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(true)
    }
}

fn project_personal_device(
    home: &PersonHome,
    identity_dir: &std::path::Path,
    cert: &DeviceCert,
) -> anyhow::Result<Vec<u8>> {
    verify_device_cert_at(cert, now_ns()?).context("new personal-device certificate validity")?;
    let mut fleet = match home.read_fleet(identity_dir, cert.user_public_key)? {
        Some(bytes) => {
            serde_json::from_slice::<FleetProjection>(&bytes).context("device fleet is invalid")?
        }
        None => FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(cert.user_public_key),
            certificates: Vec::new(),
            revocations: Vec::new(),
        },
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

/// Record the approved device in the person's fleet. The fleet is read again
/// here rather than reusing the copy sent to the device, so a revocation the
/// session recorded while the device was acknowledging is kept. The person's
/// own account writes it, so it belongs to them without any ownership change.
fn commit_personal_device(
    home: &PersonHome,
    identity_dir: &std::path::Path,
    cert: &DeviceCert,
) -> anyhow::Result<()> {
    let fleet = project_personal_device(home, identity_dir, cert)?;
    home.write_fleet(identity_dir, cert.user_public_key, &fleet)
        .context("record the approved device in the person's fleet")
}

/// The home-peer registry's bytes: its directory is writable by the identity
/// group, so a link, FIFO or oversized file planted there is refused rather
/// than followed, waited on or read whole.
fn read_registry_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "home-peer registry is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn bounded_home_peer_registry(
    identity_root: &std::path::Path,
    user: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let path = tilo_relay::home_peer_registry_path(identity_root);
    match read_registry_file(&path) {
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
    direct_peers: Vec<u8>,
    rooms: Vec<u8>,
}

/// The person's other ordinary devices the new device dials directly: the
/// approver and every other certified, unrevoked member of `identity`'s
/// fleet. Home servers are reached through the home-peer registry instead.
/// Each entry records `identity`, the root that certified the new device.
fn direct_peers_from_fleet(
    fleet: &[u8],
    identity: [u8; 32],
    enrolled_device: [u8; 32],
    approver_device: Option<[u8; 32]>,
) -> anyhow::Result<Vec<tilo_relay_protocol::personal_devices::DirectPeer>> {
    use tilo_relay_protocol::personal_devices::DirectPeer;
    let fleet: FleetProjection = serde_json::from_slice(fleet)?;
    let mut revocations = RevocationList::new();
    for entry in fleet.revocations {
        revocations
            .insert(entry)
            .context("approval returned an invalid revocation")?;
    }
    let revoked = |device: &[u8; 32]| revocations.revocation_of(&identity, device).is_some();
    if approver_device.is_some_and(|device| revoked(&device)) {
        bail!("the approving device is revoked in the fleet it sent");
    }
    let mut peers: Vec<DirectPeer> = approver_device
        .into_iter()
        .map(|device| DirectPeer { identity, device })
        .collect();
    for cert in fleet.certificates {
        let peer = DirectPeer {
            identity,
            device: cert.device_public_key,
        };
        if cert.user_public_key == identity
            && cert.device_public_key != enrolled_device
            && !cert.capabilities.can_act_as_host
            && verify_device_cert(&cert).is_ok()
            && !revoked(&cert.device_public_key)
            && !peers.contains(&peer)
        {
            peers.push(peer);
        }
    }
    peers.truncate(tilo_relay_protocol::personal_devices::MAX_DIRECT_PEERS);
    Ok(peers)
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
    match read_registry_file(path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => serde_json::from_slice(&bytes)
            .with_context(|| format!("{description} is invalid"))
            .map(Some),
        Ok(_) => bail!("{description} exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The staged direct-peer and room lists of a personal enrollment.
fn personal_links(
    payload: &ResponsePayload,
    approver_device: Option<[u8; 32]>,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let fleet = payload
        .fleet_json
        .as_ref()
        .context("approval omitted the fleet")?;
    let direct_peers = direct_peers_from_fleet(
        fleet,
        payload.cert.user_public_key,
        payload.cert.device_public_key,
        approver_device,
    )?;
    let rooms = payload
        .personal_rooms_json
        .as_deref()
        .map(tilo_relay_protocol::personal_devices::parse_rooms)
        .transpose()
        .context("approval returned an invalid room list")?
        .unwrap_or_default();
    Ok((
        tilo_relay_protocol::personal_devices::encode_direct_peers(&direct_peers)?,
        serde_json::to_vec_pretty(&rooms)?,
    ))
}

/// The person's identity registry, or a fresh one naming `identity_id` as
/// the default when they have none yet.
fn identity_registry(
    home: &PersonHome,
    identity_dir: &std::path::Path,
    identity_id: &str,
) -> anyhow::Result<serde_json::Value> {
    let existing_registry = home
        .read_file(
            identity_dir,
            person_home::HomeFile::at("registry.json"),
            1024 * 1024,
        )
        .context("read the existing identity registry")?
        .map(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .context("existing identity registry is invalid")
        })
        .transpose()?;
    Ok(existing_registry.unwrap_or_else(|| {
        serde_json::json!({
            "schema_version": 1,
            "default_identity": identity_id,
            "identities": {},
            "workspace_identities": {}
        })
    }))
}

fn checked_personal_projection(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    home: &PersonHome,
    payload: &ResponsePayload,
    approver_device: Option<[u8; 32]>,
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
    let (direct_peers, rooms) = personal_links(payload, approver_device)?;

    let incoming: Vec<HomePeerRegistration> = serde_json::from_slice(transferred_peers)?;
    if incoming.len() > 128
        || incoming.iter().any(|entry| {
            entry.user_public_key != identity_id || !valid_home_peer_registration(entry)
        })
    {
        bail!("approval returned an invalid home-peer registry");
    }

    let mut registry = identity_registry(home, identity_dir, &identity_id)?;
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
        direct_peers,
        rooms,
    })
}

#[derive(Clone, Copy)]
struct PersonalStage<'a> {
    identity_root: &'a std::path::Path,
    identity_dir: &'a std::path::Path,
    home: &'a PersonHome,
    paths: &'a IdentityPaths,
    /// The device that approved this enrollment, absent for a certificate
    /// delivered out of band.
    approver_device: Option<[u8; 32]>,
    requester_lease: Option<tilo_relay::ProcessLease>,
}

/// Largest staged enrollment file or marker read back.
const MAX_STAGED_BYTES: usize = 1024 * 1024;

/// The enrollment's private staging directory under the shared staging area,
/// created by and belonging to this process's account. The area is writable
/// by the device-trust group, so it is opened without following a link and
/// an entry another account placed there is refused.
fn open_stage_dir(
    identity_root: &std::path::Path,
    stage_name: &str,
    create: bool,
) -> anyhow::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let staging = enrollment_stage_dir(identity_root);
    if create {
        std::fs::create_dir_all(&staging)?;
    }
    let staging = person_home::local::open_search_dir(&staging)
        .with_context(|| format!("open {}", enrollment_stage_dir(identity_root).display()))?;
    let stage = person_home::local::open_owned_subdir(
        &staging,
        stage_name,
        person_home::local::own_uid(),
        create.then_some(0o700),
    )?
    .with_context(|| format!("staged enrollment {stage_name} is missing"))?;
    Ok((staging, stage))
}

/// Create a new private staging directory for one enrollment under a name
/// from the system's random source, owned by this process's account and
/// `0700`, and claim its commit marker, holding `marker_for(name)`, under the
/// same name. The staging area cannot be listed by the group, so nobody
/// learns the name before the marker claims it. A name anything already
/// holds, in staging or as a marker, is never used; another is drawn.
fn create_stage_dir(
    identity_root: &std::path::Path,
    marker_for: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<(String, std::os::fd::OwnedFd)> {
    create_stage_dir_drawing(identity_root, marker_for, || {
        let mut token = [0_u8; 16];
        UnwrapErr(SysRng).fill_bytes(&mut token);
        Ok(token)
    })
}

fn create_stage_dir_drawing(
    identity_root: &std::path::Path,
    marker_for: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
    mut draw: impl FnMut() -> anyhow::Result<[u8; 16]>,
) -> anyhow::Result<(String, std::os::fd::OwnedFd)> {
    use std::os::fd::AsRawFd as _;
    let staging_path = enrollment_stage_dir(identity_root);
    let commits_path = enrollment_commit_dir(identity_root);
    std::fs::create_dir_all(&staging_path)?;
    std::fs::create_dir_all(&commits_path)?;
    let staging = person_home::local::open_search_dir(&staging_path)
        .with_context(|| format!("open {}", staging_path.display()))?;
    let commits = person_home::local::open_dir(&commits_path)
        .with_context(|| format!("open {}", commits_path.display()))?;
    let me = person_home::local::own_uid();
    for _ in 0..16 {
        let name = format!("personal-{}", hex::encode(draw()?));
        let Some(stage) = person_home::local::create_fresh_subdir(&staging, &name, me, 0o700)?
        else {
            continue;
        };
        if person_home::local::create_fresh_file(
            &commits,
            &format!("{name}.json"),
            me,
            &marker_for(&name)?,
            0o600,
        )? {
            return Ok((name, stage));
        }
        // The marker's name is taken: give the directory back and draw again.
        if let Ok(c) = std::ffi::CString::new(name.as_str()) {
            // SAFETY: `staging` is open and `c` is NUL-terminated.
            unsafe { libc::unlinkat(staging.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) };
        }
    }
    bail!("no free staging name in {}", staging_path.display())
}

/// Every file an enrollment stages.
const STAGED_FILES: [&str; 9] = [
    "record.json",
    "registry.json",
    "fleet.json",
    "home-peers.json",
    "direct-peers.json",
    "rooms.json",
    "user-pubkey.bin",
    "per-user-cert.bin",
    "default-cert.bin",
];

/// Remove a committed enrollment's staging through the descriptors that were
/// checked: its known files, then the directory itself, which fails rather
/// than recursing if anything else is in it.
fn remove_stage(staging: &std::os::fd::OwnedFd, stage: &std::os::fd::OwnedFd, stage_name: &str) {
    use std::os::fd::AsRawFd as _;
    for name in STAGED_FILES {
        let Ok(name) = std::ffi::CString::new(name) else {
            continue;
        };
        // SAFETY: `stage` is open and `name` is NUL-terminated.
        unsafe { libc::unlinkat(stage.as_raw_fd(), name.as_ptr(), 0) };
    }
    if let Ok(name) = std::ffi::CString::new(stage_name) {
        // SAFETY: `staging` is open and `name` is NUL-terminated.
        unsafe { libc::unlinkat(staging.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
    }
}

fn write_staged(
    stage: &std::os::fd::OwnedFd,
    name: &str,
    bytes: &[u8],
    mode: libc::mode_t,
) -> anyhow::Result<()> {
    person_home::local::replace_owned_file(stage, name, bytes, person_home::local::own_uid(), mode)
        .with_context(|| format!("stage {name}"))
}

/// A staged file, read back without following a link and only if this
/// process's account wrote it.
fn read_staged(stage: &std::os::fd::OwnedFd, name: &str) -> anyhow::Result<Vec<u8>> {
    person_home::local::read_owned_file(
        stage,
        name,
        person_home::local::own_uid(),
        MAX_STAGED_BYTES,
    )
    .with_context(|| format!("read staged {name}"))?
    .with_context(|| format!("staged {name} is missing"))
}

/// Replace the commit marker at `marker_path` in the shared commit area,
/// anchored to that directory and never through a link.
fn write_marker(
    marker_path: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
) -> anyhow::Result<()> {
    let (Some(dir), Some(name)) = (
        marker_path.parent(),
        marker_path.file_name().and_then(|name| name.to_str()),
    ) else {
        bail!("personal enrollment marker path is invalid");
    };
    std::fs::create_dir_all(dir)?;
    let dir = person_home::local::open_dir(dir)?;
    person_home::local::replace_owned_file(
        &dir,
        name,
        &serde_json::to_vec_pretty(marker)?,
        person_home::local::own_uid(),
        0o600,
    )
    .context("write the personal enrollment marker")
}

fn stage_personal_enrollment(
    stage: &PersonalStage<'_>,
    payload: &ResponsePayload,
) -> anyhow::Result<(PathBuf, PersonalEnrollmentMarker)> {
    let PersonalStage {
        identity_root,
        identity_dir,
        home,
        paths,
        approver_device,
        requester_lease,
    } = *stage;
    let projection =
        checked_personal_projection(identity_root, identity_dir, home, payload, approver_device)?;
    let marker_named = |stage_name: &str| PersonalEnrollmentMarker {
        schema_version: PERSONAL_ENROLLMENT_SCHEMA,
        person_uid: enrolled_uid(home),
        identity_dir: identity_dir.to_path_buf(),
        stage_name: stage_name.to_owned(),
        user_public_key: payload.cert.user_public_key,
        device_public_key: payload.cert.device_public_key,
        requester_lease,
        commit_ready: false,
    };
    let (stage_name, stage_dir) = create_stage_dir(identity_root, |name| {
        Ok(serde_json::to_vec_pretty(&marker_named(name))?)
    })?;
    let marker_path = personal_marker_path(identity_root, &stage_name);
    let staged = write_stage_files(&stage_dir, &projection, &payload.cert);
    if let Err(error) = staged {
        // Nothing of a half-staged enrollment stays behind.
        if let Ok(staging) =
            person_home::local::open_search_dir(&enrollment_stage_dir(identity_root))
        {
            remove_stage(&staging, &stage_dir, &stage_name);
        }
        let _ = std::fs::remove_file(&marker_path);
        return Err(error);
    }
    let _ = paths; // binds staging to the active device layout at the call site.
    Ok((marker_path, marker_named(&stage_name)))
}

/// Write every file of one staged enrollment.
fn write_stage_files(
    stage_dir: &std::os::fd::OwnedFd,
    projection: &CheckedPersonalProjection,
    certificate: &DeviceCert,
) -> anyhow::Result<()> {
    let cert = tilo_user_identity::canonical::canonical_bytes(certificate)?;
    write_staged(stage_dir, "record.json", &projection.identity, 0o600)?;
    write_staged(stage_dir, "registry.json", &projection.registry, 0o600)?;
    write_staged(stage_dir, "fleet.json", &projection.fleet, 0o600)?;
    write_staged(stage_dir, "home-peers.json", &projection.peers, 0o600)?;
    write_staged(
        stage_dir,
        "direct-peers.json",
        &projection.direct_peers,
        0o600,
    )?;
    write_staged(stage_dir, "rooms.json", &projection.rooms, 0o600)?;
    write_staged(
        stage_dir,
        "user-pubkey.bin",
        &certificate.user_public_key,
        0o600,
    )?;
    write_staged(stage_dir, "per-user-cert.bin", &cert, 0o600)?;
    write_staged(stage_dir, "default-cert.bin", &cert, 0o600)?;
    Ok(())
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
    write_marker(marker_path, &marker)?;
    Ok(marker)
}

/// Merge a staged enrollment's direct peers and rooms into the person's
/// directory beside the identity root, written as the person `home` reaches.
/// Merging is idempotent, so recovery may repeat it.
fn install_staged_links(
    identity_root: &std::path::Path,
    stage_dir: &std::os::fd::OwnedFd,
    marker: &PersonalEnrollmentMarker,
    home: &PersonHome,
) -> anyhow::Result<()> {
    use tilo_relay_protocol::personal_devices;
    let direct_peers =
        personal_devices::parse_direct_peers(&read_staged(stage_dir, "direct-peers.json")?)?;
    let rooms = personal_devices::parse_rooms(&read_staged(stage_dir, "rooms.json")?)?;
    home.merge_peers(
        &personal_devices::peers_dir(identity_root),
        &marker.user_public_key,
        &direct_peers,
        &rooms,
    )
    .context("record the joined devices and rooms")?;
    Ok(())
}

/// Publish a confirmed enrollment. What lands under the identity root is
/// written by this process into directories the image lays out set-group-ID,
/// so it takes their group without any ownership change. What lands in the
/// person's identity directory is written through `home`: as the person when
/// this runs as root for them, and always without following a link.
fn commit_staged_personal_enrollment(
    identity_root: &std::path::Path,
    paths: &IdentityPaths,
    home: &PersonHome,
    marker_path: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
    mut checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    if !marker.commit_ready {
        bail!("personal enrollment marker is invalid");
    }
    check_marker_person(identity_root, marker_path, marker, home)?;
    let (staging, stage_dir) = open_stage_dir(identity_root, &marker.stage_name, false)?;
    let cert_bytes = read_staged(&stage_dir, "default-cert.bin")?;
    let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&cert_bytes)?;
    if cert.user_public_key != marker.user_public_key
        || cert.device_public_key != marker.device_public_key
        || verify_device_cert_at(&cert, now_ns()?).is_err()
        || read_staged(&stage_dir, "user-pubkey.bin")? != marker.user_public_key
    {
        bail!("staged personal enrollment no longer validates");
    }
    // The first callback is the requester-liveness gate in production. Keep
    // it before even creating the user identity directory so a compositor
    // that died while approval was in flight causes no publication mutation.
    checkpoint(PersonalCommitCheckpoint::Record)?;
    let record = identity_record_path(&marker.identity_dir, marker.user_public_key);
    let record_name = record
        .file_name()
        .and_then(|name| name.to_str())
        .context("joined identity record has no file name")?
        .to_owned();
    home.write_file(
        &marker.identity_dir,
        person_home::HomeFile::at(&record_name),
        &read_staged(&stage_dir, "record.json")?,
        0o600,
        true,
    )
    .context("install the joined identity record")?;
    checkpoint(PersonalCommitCheckpoint::Fleet)?;
    home.write_fleet(
        &marker.identity_dir,
        marker.user_public_key,
        &read_staged(&stage_dir, "fleet.json")?,
    )
    .context("install the joined identity's fleet")?;
    checkpoint(PersonalCommitCheckpoint::HomePeers)?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &tilo_relay::home_peer_registry_path(identity_root),
        &read_staged(&stage_dir, "home-peers.json")?,
        0o644,
    )?;
    checkpoint(PersonalCommitCheckpoint::PersonalLinks)?;
    install_staged_links(identity_root, &stage_dir, marker, home)?;
    checkpoint(PersonalCommitCheckpoint::Registry)?;
    home.write_file(
        &marker.identity_dir,
        person_home::HomeFile::at("registry.json"),
        &read_staged(&stage_dir, "registry.json")?,
        0o600,
        false,
    )
    .context("install the identity registry")?;
    checkpoint(PersonalCommitCheckpoint::UserPublicKey)?;
    // The public key and certificates are public, whatever the umask this
    // commit runs under: the session reads them to verify the device.
    publish_device_file(
        identity_root,
        under_root(identity_root, &paths.user_pubkey)?,
        &marker.user_public_key,
    )?;
    checkpoint(PersonalCommitCheckpoint::PerUserCert)?;
    publish_device_file(
        identity_root,
        under_root(
            identity_root,
            &paths.device_cert_for_user(&marker.user_public_key),
        )?,
        &read_staged(&stage_dir, "per-user-cert.bin")?,
    )?;
    // This device and the approver become each other's personal devices. The
    // approver's certificate comes from the root-signed fleet staged with this
    // enrollment and must verify under the identity this device joins.
    checkpoint(PersonalCommitCheckpoint::PersonalPeers)?;
    let personal_peers = remember_staged_personal_devices(identity_root, &stage_dir, marker)?;
    // Publish this last: every projection file has been durably installed.
    checkpoint(PersonalCommitCheckpoint::DefaultCert)?;
    publish_device_file(
        identity_root,
        under_root(identity_root, &paths.device_cert)?,
        &cert_bytes,
    )?;
    // The joined identity is the one this device now acts for: select it, or
    // the next login resolves the absent default and asks to create one. The
    // first-use UI reads a selected joined identity as a finished join, so it
    // is selected only once every file of the join is in place. Only the
    // directory's owner can select (the selector is theirs alone); a root-run
    // join into someone else's directory leaves it to them.
    if !home.is_delegated() && owns_directory(&marker.identity_dir)? {
        let record_id = record
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("joined identity record has no runtime id")?;
        runtime_identity::set_active_identity(&marker.identity_dir, record_id)
            .context("select the joined identity")?;
    }
    restore_owner_from_parent(&personal_peers)?;
    std::fs::remove_file(marker_path)?;
    if let Some(parent) = marker_path.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        directory.sync_all()?;
    }
    remove_stage(&staging, &stage_dir, &marker.stage_name);
    Ok(record)
}

/// Refuse `marker` unless it names its own staging, was staged for the
/// account `home` publishes for, and its staging enrolls the identity it
/// records. Nothing is written before this holds.
fn check_marker_person(
    identity_root: &std::path::Path,
    marker_path: &std::path::Path,
    marker: &PersonalEnrollmentMarker,
    home: &PersonHome,
) -> anyhow::Result<()> {
    let expected_name = format!("{}.json", marker.stage_name);
    if marker.schema_version != PERSONAL_ENROLLMENT_SCHEMA
        || !valid_stage_name(&marker.stage_name)
        || marker_path.file_name() != Some(std::ffi::OsStr::new(&expected_name))
    {
        bail!("personal enrollment marker is invalid");
    }
    let account = enrolled_uid(home);
    if marker.person_uid != account {
        bail!(
            "this enrollment was staged for account {}, not {account}",
            marker.person_uid
        );
    }
    let (_, stage_dir) = open_stage_dir(identity_root, &marker.stage_name, false)?;
    let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&read_staged(
        &stage_dir,
        "default-cert.bin",
    )?)?;
    if cert.user_public_key != marker.user_public_key
        || read_staged(&stage_dir, "user-pubkey.bin")? != marker.user_public_key
    {
        bail!("this enrollment's staging enrolls another identity than its marker records");
    }
    Ok(())
}

fn recover_personal_enrollments(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    paths: &IdentityPaths,
    home: &PersonHome,
    mut checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let directory = enrollment_commit_dir(identity_root);
    let commits = match person_home::local::open_dir(&directory) {
        Ok(commits) => commits,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // The commit area is shared with the device-trust group: only a
        // marker this account wrote, read without following a link, is
        // recovered; another account's is left to it.
        let bytes = match person_home::local::read_owned_file(
            &commits,
            &name,
            person_home::local::own_uid(),
            MAX_STAGED_BYTES,
        ) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(error) => {
                eprintln!("note: not recovering enrollment marker {name}: {error}");
                continue;
            }
        };
        // An unreadable marker of this account's cannot be finished; it must
        // not block this join either.
        let Ok(marker) = serde_json::from_slice::<PersonalEnrollmentMarker>(&bytes) else {
            eprintln!("warning: abandoning unreadable enrollment marker {name}");
            continue;
        };
        if marker.identity_dir == identity_dir && marker.commit_ready {
            // A confirmed enrollment whose staging is gone or no longer this
            // account's, or that was staged for another account or identity,
            // cannot be finished here; it must not block this join.
            if let Err(error) = check_marker_person(identity_root, &entry.path(), &marker, home) {
                eprintln!("warning: abandoning enrollment marker {name}: {error:#}");
                continue;
            }
            commit_staged_personal_enrollment(
                identity_root,
                paths,
                home,
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
    home: &PersonHome,
    payload: &ResponsePayload,
) -> anyhow::Result<PathBuf> {
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    let (marker_path, marker) = stage_personal_enrollment(
        &PersonalStage {
            identity_root,
            identity_dir,
            home,
            paths: &paths,
            approver_device: None,
            requester_lease: None,
        },
        payload,
    )?;
    let marker = authorize_personal_commit(&marker_path, &marker, None)?;
    commit_staged_personal_enrollment(identity_root, &paths, home, &marker_path, &marker, |_| {
        Ok(())
    })
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
        relay_endpoint: None,
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

fn sign_response(
    payload: ResponsePayload,
    root: &(impl RootSigner + ?Sized),
) -> anyhow::Result<Response> {
    let signature = root.sign_message(&response_preimage(&payload)?)?.to_vec();
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

fn valid_solicitation(solicitation: &PairSolicitation) -> bool {
    solicitation.magic == PAIR_MAGIC
        && VerifyingKey::from_bytes(&solicitation.device_public_key).is_ok()
        && solicitation.relay_service_node_id != [0; 32]
        && tilo_relay::valid_home_peer_label(&solicitation.label)
}

fn discover(timeout: u64, kind: EnrollmentKind) -> anyhow::Result<()> {
    let socket = bind_listener(NEARBY_PORT)?;
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
    response: &Response,
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    route: ResponseRoute,
) -> anyhow::Result<Vec<u8>> {
    let plaintext = zeroize::Zeroizing::new(postcard::to_stdvec(response)?);
    let aad = response_aad(token_nonce, recipient);
    Ok(wrap_v2(&tilo_seal::hpke::seal(
        &tilo_seal::PublicKey::from_bytes(route.response_public_key),
        tilo_seal::Label::RelayEnrollResponse,
        &aad,
        &plaintext,
    )?))
}

fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    response_secret: &tilo_seal::SecretKey,
) -> anyhow::Result<Response> {
    let aad = response_aad(token_nonce, recipient);
    let plaintext = v2_body(bytes)
        .and_then(|body| {
            tilo_seal::hpke::open_bytes(
                response_secret,
                tilo_seal::Label::RelayEnrollResponse,
                &aad,
                body,
            )
            .ok()
        })
        .ok_or_else(|| anyhow::anyhow!("enrollment response was not encrypted for this device"))?;
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
    root: &(impl RootSigner + ?Sized),
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    Ok(postcard::to_stdvec(&Finalized {
        payload,
        signature: root.sign_message(&encoded)?.to_vec(),
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
            home: &PersonHome::Direct,
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
        home,
        requester_lease,
        port,
    } = options;
    validate_requester(requester_lease)?;
    let token = decode_enrollment_token(token_text).context("decode enrollment token")?;
    verify_enrollment_token(&token, now_ns()?).context("verify enrollment token")?;
    if kind == EnrollmentKind::HomePeer
        && (!token.capabilities.can_act_as_host || !token.capabilities.can_read_all_workspaces)
    {
        bail!("enrollment token does not grant home-server capabilities");
    }
    if kind == EnrollmentKind::PersonalDevice
        && (token.capabilities.can_act_as_host || token.capabilities.can_admit_devices)
    {
        bail!("enrollment token grants the wrong device role");
    }
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    if kind == EnrollmentKind::PersonalDevice {
        recover_personal_enrollments(
            identity_root,
            identity_dir.context("identity directory is required")?,
            &paths,
            home,
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
    let mut reply = vec![0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        broadcast(&socket, &bytes, port)?;
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
                        &PersonalStage {
                            identity_root,
                            identity_dir: identity_dir.context("identity directory is required")?,
                            home,
                            paths: &paths,
                            approver_device: Some(token.issuer_device_public_key),
                            requester_lease,
                        },
                        &response.payload,
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
                                            home,
                                            marker_path,
                                            &marker,
                                            |_| validate_requester(requester_lease),
                                        )?)
                                    } else {
                                        // The host path has no multi-file member
                                        // projection, but still waits for
                                        // Finalized before changing its active
                                        // identity binding.
                                        publish_device_file(
                                            identity_root,
                                            under_root(identity_root, &paths.user_pubkey)?,
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
                                    let cert = tilo_user_identity::canonical::canonical_bytes(
                                        &response.payload.cert,
                                    )?;
                                    publish_device_file(
                                        identity_root,
                                        under_root(
                                            identity_root,
                                            &paths.device_cert_for_user(
                                                &response.payload.cert.user_public_key,
                                            ),
                                        )?,
                                        &cert,
                                    )?;
                                    publish_device_file(
                                        identity_root,
                                        under_root(identity_root, &paths.device_cert)?,
                                        &cert,
                                    )?;
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
                                            "record_path": record_path.as_ref().context("record path is required")?,
                                            "label": response.payload.cert.device_label,
                                        })
                                    );
                                } else {
                                    println!("enrolled {}", response.payload.cert.device_label);
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

#[allow(
    clippy::too_many_lines,
    reason = "one ordered pairing exchange; the requester lease is revalidated at every step of it"
)]
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
            options.home,
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
    let deadline = deadline_after(socket_timeout(timeout))?;
    let mut reply = vec![0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        validate_requester(requester_lease)?;
        broadcast(&socket, &solicitation, port)?;
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
                    || (kind == EnrollmentKind::HomePeer
                        && (!token.capabilities.can_act_as_host
                            || !token.capabilities.can_read_all_workspaces))
                    || (kind == EnrollmentKind::PersonalDevice
                        && (token.capabilities.can_act_as_host
                            || token.capabilities.can_admit_devices))
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
                        options.home,
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

/// Publish first-use pairing progress for the UI that started this process,
/// written as the person the pairing runs for.
fn write_pairing_status(
    home: &PersonHome,
    path: &std::path::Path,
    status: &serde_json::Value,
) -> anyhow::Result<()> {
    home.write_path(path, &serde_json::to_vec(status)?, 0o644)
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
fn report_pairing_failure(home: &PersonHome, path: &std::path::Path, error: &anyhow::Error) {
    let _ = write_pairing_status(
        home,
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
    /// How the person's identity directory and status file are reached.
    home: &'a PersonHome,
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
    /// The approver's exported workspace rooms, sent to a personal device.
    personal_rooms: Option<&'a std::path::Path>,
    /// How the person's identity directory and room list are reached.
    home: &'a PersonHome,
    kind: EnrollmentKind,
    /// Port the listener binds; 0 lets the OS choose one.
    port: u16,
    /// Receives the bound listener port once the approver is listening.
    ready: Option<&'a std::sync::mpsc::Sender<u16>>,
}

struct ApprovalIdentity {
    root: Box<dyn RootSigner>,
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
    personal_rooms_json: Option<Vec<u8>>,
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

/// Enrollment resolves a record signer only in the account that owns it.
/// The shared signer still owns seed precedence and provider access.
fn person_approval_root(
    identity_root: &std::path::Path,
    identity_dir: Option<&std::path::Path>,
    home: &PersonHome,
) -> anyhow::Result<Box<dyn RootSigner>> {
    const MISSING_ROOT: &str =
        "this device does not hold the identity root needed to approve enrollment";
    let paths = IdentityPaths::under(identity_root);
    let seed_absent = matches!(
        std::fs::symlink_metadata(&paths.user_privkey),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    let Some(directory) = identity_dir.filter(|_| seed_absent) else {
        return tilo_relay::root_signer::load_root(identity_root, identity_dir);
    };
    let identity_dir = directory;
    if !home.is_delegated() && person_home::local::own_uid() == 0 {
        // Root acting for itself opens only an identity directory root owns;
        // a person's directory and anything linked from it are theirs to open.
        person_home::local::open_owned_dir(identity_dir, 0).with_context(|| {
            format!(
                "{MISSING_ROOT}: {} is not root's own identity directory; run this as the \
                 account that owns it",
                identity_dir.display()
            )
        })?;
    }
    if home.is_delegated() {
        bail!(
            "{MISSING_ROOT}: without a seed under {}, approval signs with the person's identity \
             record, which only their own account opens; run this as that account",
            identity_root.display()
        );
    }
    match tilo_relay::root_signer::load_root(identity_root, Some(directory)) {
        Err(error) if error.to_string().contains("threshold identity") => {
            let user = runtime_identity::resolve_active_identity(directory)
                .and_then(|record| runtime_identity::load_from_path(record).ok())
                .map(|identity| identity.group_public_key());
            match user {
                Some(user) => Err(error.context(
                    tilo_user_identity::RootOnly::AdmitDevice.refusal(
                        root_holders(home, directory, user)
                            .iter()
                            .map(String::as_str),
                    ),
                )),
                None => Err(error),
            }
        }
        result => result,
    }
}

/// Labels of the person's devices that hold the root, from this account's
/// fleet; empty when it cannot be read.
fn root_holders(home: &PersonHome, identity_dir: &std::path::Path, user: [u8; 32]) -> Vec<String> {
    let Ok(Some(bytes)) = home.read_fleet(identity_dir, user) else {
        return Vec::new();
    };
    let Ok(fleet) = serde_json::from_slice::<FleetProjection>(&bytes) else {
        return Vec::new();
    };
    let mut revocations = RevocationList::new();
    for entry in fleet.revocations {
        let _ = revocations.insert(entry);
    }
    let Ok(now) = now_ns() else {
        return Vec::new();
    };
    let mine: Vec<DeviceCert> = fleet
        .certificates
        .into_iter()
        .filter(|cert| cert.user_public_key == user)
        .collect();
    tilo_user_identity::root_holder_labels(&mine, &revocations, now)
        .into_iter()
        .map(str::to_owned)
        .collect()
}
/// Load the signer and verify that this device remains authorised to admit peers.
fn load_approval_identity(options: &ApprovalOptions<'_>) -> anyhow::Result<ApprovalIdentity> {
    let paths = IdentityPaths::under(options.identity_root);
    let root = person_approval_root(options.identity_root, options.identity_dir, options.home)?;
    let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let user_public_key = root.public_key();
    let issuer_cert = tilo_user_identity::persist::read_device_cert(
        &paths.device_cert_for_user(&user_public_key),
    )
    .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))?;
    verify_device_cert_at(&issuer_cert, now_ns()?)
        .context("verify approving device certificate validity")?;
    if issuer_cert.user_public_key != user_public_key
        || issuer_cert.device_public_key != device.verifying_key().to_bytes()
        || !issuer_cert.capabilities.can_admit_devices
    {
        bail!("this device is not authorised to approve enrollment");
    }
    if let Some(directory) = options.identity_dir
        && fleet_revokes_device(
            options.home,
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
        let issued_at_ns = validity_start_ns(now);
        let expires_at_ns = now.saturating_add(5 * 60 * 1_000_000_000);
        let capabilities = match kind {
            EnrollmentKind::HomePeer => DeviceCapabilities::host(),
            EnrollmentKind::PersonalDevice => DeviceCapabilities::joined(),
        };
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
        && match kind {
            EnrollmentKind::HomePeer => {
                token.capabilities.can_act_as_host && token.capabilities.can_read_all_workspaces
            }
            EnrollmentKind::PersonalDevice => {
                !token.capabilities.can_act_as_host && !token.capabilities.can_admit_devices
            }
        }
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
            let capabilities: collab_types::FsAuthority = serde_json::from_str(&capability_json)
                .context("filesystem capability export is invalid")?;
            let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign_with(
                ticket.workspace_id(),
                1,
                capabilities,
                false,
                identity.user_public_key,
                |payload| identity.root.sign_message(payload),
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
                personal_rooms_json: None,
            })
        }
        EnrollmentKind::PersonalDevice => {
            let identity_dir = options
                .identity_dir
                .context("identity directory is required for personal-device approval")?;
            Ok(ApprovalProjection {
                home_peer_toml: None,
                public_identity_json: Some(public_identity_record(
                    identity.user_public_key,
                    "My Tilo identity",
                )?),
                fleet_json: Some(project_personal_device(options.home, identity_dir, cert)?),
                home_peers_json: Some(bounded_home_peer_registry(
                    options.identity_root,
                    identity.user_public_key,
                )?),
                personal_rooms_json: Some(serde_json::to_vec(&approver_rooms(
                    options.home,
                    options.personal_rooms,
                ))?),
            })
        }
    }
}

/// The rooms this approver shares with a new personal device. The export is
/// the session's convenience copy of its room list: a missing or malformed
/// one shares no rooms and is reported, rather than failing the approval.
fn approver_rooms(home: &PersonHome, path: Option<&std::path::Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    let rooms = home.read_rooms(path).and_then(|bytes| {
        bytes.map_or_else(
            || Ok(Vec::new()),
            |bytes| Ok(tilo_relay_protocol::personal_devices::parse_rooms(&bytes)?),
        )
    });
    match rooms {
        Ok(rooms) => {
            if rooms.is_empty() {
                eprintln!(
                    "note: {} lists no workspace rooms; the new device starts without shared \
                     workspaces until this device's session has run once",
                    path.display()
                );
            }
            rooms
        }
        Err(error) => {
            eprintln!(
                "warning: not sharing workspace rooms: {}: {error}",
                path.display()
            );
            Vec::new()
        }
    }
}

/// Record a newly approved personal device as one this machine dials
/// directly, in the approving person's directory beside the identity root.
fn remember_approved_device(identity_root: &std::path::Path, home: &PersonHome, cert: &DeviceCert) {
    use tilo_relay_protocol::personal_devices;
    let peers = personal_devices::peers_dir(identity_root);
    let recorded = home.merge_peers(
        &peers,
        &cert.user_public_key,
        &[personal_devices::DirectPeer {
            identity: cert.user_public_key,
            device: cert.device_public_key,
        }],
        &[],
    );
    if let Err(error) = recorded {
        eprintln!(
            "warning: could not record the new device in {}: {error:#}",
            peers.display()
        );
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
        issued_at_ns: validity_start_ns(now_ns()?),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_root_cert(&mut cert, identity.root.as_ref())?;
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
            personal_rooms_json: projection.personal_rooms_json,
        },
        identity.root.as_ref(),
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
    match options.kind {
        EnrollmentKind::HomePeer => register_home_peer(
            options.identity_root,
            &prepared.signed.payload.cert,
            prepared.signed.payload.relay_service_node_id,
            &identity.relay_workspace_ids,
        )?,
        EnrollmentKind::PersonalDevice => {
            commit_personal_device(
                options.home,
                options
                    .identity_dir
                    .context("identity directory is required")?,
                &prepared.signed.payload.cert,
            )?;
            // Both devices are certified under the person's root.
            let personal_peers = remember_personal_devices(
                options.identity_root,
                &[
                    identity.device.verifying_key().to_bytes(),
                    prepared.signed.payload.cert.device_public_key,
                ],
            )?;
            restore_owner_from_parent(&personal_peers)?;
        }
    }
    consume_nonce(
        options.identity_root,
        &pending.token.nonce,
        pending.request.device_public_key,
    )?;
    let finalized = make_finalized(
        &prepared.encrypted,
        pending.token.nonce,
        pending.request.device_public_key,
        identity.root.as_ref(),
    )?;
    for _ in 0..8 {
        socket.send_to(&finalized, pending.sender)?;
        std::thread::sleep(Duration::from_millis(125));
    }
    let cert = &prepared.signed.payload.cert;
    if options.kind == EnrollmentKind::PersonalDevice {
        remember_approved_device(options.identity_root, options.home, cert);
    }
    println!(
        "{}",
        serde_json::json!({
            "approved_device": hex::encode(cert.device_public_key),
            "label": cert.device_label,
            "certificate": hex::encode(tilo_user_identity::canonical::canonical_bytes(cert)?),
        })
    );
    Ok(())
}

fn approve(options: &ApprovalOptions<'_>) -> anyhow::Result<()> {
    let identity = load_approval_identity(options)?;
    let socket = bind_listener(options.port)?;
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
        personal_rooms_json: None,
        cert,
    })
}

struct CertifyOptions<'a> {
    root_key: Option<&'a std::path::Path>,
    identity_root: &'a std::path::Path,
    identity_dir: Option<&'a std::path::Path>,
    device: &'a str,
    label: &'a str,
    capabilities: &'a [String],
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
        out,
    } = *options;
    let root: Box<dyn RootSigner> = match root_key {
        Some(path) => Box::new(
            tilo_user_identity::persist::read_owned_signing_key(path)
                .with_context(|| format!("read {}", path.display()))?,
        ),
        None => person_approval_root(identity_root, identity_dir, &PersonHome::Direct)?,
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
        commit_personal_device(&PersonHome::Direct, identity_dir, &cert)?;
    }
    tilo_user_identity::persist::write_device_cert(out, &cert)?;
    Ok(())
}

fn install_cert(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    cert_path: &std::path::Path,
    home: &PersonHome,
) -> anyhow::Result<()> {
    require_own_identity_root(identity_root)?;
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
        home,
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
            out,
        } => certify(&CertifyOptions {
            root_key: root_key.as_deref(),
            identity_root: &identity_root,
            identity_dir: identity_dir.as_deref(),
            device: &device,
            label: &label,
            capabilities: &capabilities,
            out: &out,
        }),
        OutOfBand::InstallCert {
            identity_root,
            identity_dir,
            cert,
            person_uid,
        } => install_cert(
            &identity_root,
            &identity_dir,
            &cert,
            &PersonHome::for_this_process(person_uid)?,
        ),
    }
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    // Whatever umask the invoking account passed through pkexec, files root
    // creates under /var/lib/tilo are never group- or world-writable.
    if person_home::local::own_uid() == 0 {
        // SAFETY: umask has no preconditions.
        unsafe { libc::umask(0o022) };
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

/// Join an identity as an ordinary personal device for the person
/// `person_uid` names (or this process's own account). As root the join
/// writes under the identity root, which must be root's own, and reaches the
/// person's identity directory and status file only as the person.
fn pair_device_cli(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    label: &str,
    timeout: u64,
    status_file: Option<&std::path::Path>,
    requester_lease: anyhow::Result<Option<tilo_relay::ProcessLease>>,
    person_uid: Option<u32>,
) -> anyhow::Result<()> {
    let home = PersonHome::for_this_process(person_uid)?;
    let result = require_own_identity_root(identity_root)
        .and(requester_lease)
        .and_then(|requester_lease| {
            pair(
                JoinOptions {
                    identity_root,
                    label,
                    timeout,
                    kind: EnrollmentKind::PersonalDevice,
                    home_peer_output: None,
                    identity_dir: Some(identity_dir),
                    home: &home,
                    requester_lease,
                    port: NEARBY_PORT,
                },
                status_file,
            )
        });
    if let (Err(error), Some(path)) = (&result, status_file) {
        report_pairing_failure(&home, path, error);
    }
    result
}

/// Root publishes a join only under an identity root it owns (the image's
/// `/var/lib/tilo`); one inside a person's home is theirs to join into.
fn require_own_identity_root(identity_root: &std::path::Path) -> anyhow::Result<()> {
    if person_home::local::own_uid() == 0 {
        person_home::local::open_owned_dir(identity_root, 0).with_context(|| {
            format!(
                "{} is not root's own identity root; join as the account that owns it",
                identity_root.display()
            )
        })?;
    }
    Ok(())
}

/// Approve a nearby personal device for the person whose identity directory
/// this names; as root, that directory is reached only as `person_uid`.
fn approve_device_cli(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    timeout: u64,
    expected_device: Option<&str>,
    expected_service: Option<&str>,
    personal_rooms: Option<PathBuf>,
    person_uid: Option<u32>,
) -> anyhow::Result<()> {
    let home = PersonHome::for_this_process(person_uid)?;
    // Root's own environment says nothing about where the person's session
    // exports its rooms; only an explicit path is read then.
    let personal_rooms = if home.is_delegated() {
        personal_rooms
    } else {
        personal_rooms.or_else(tilo_relay_protocol::personal_devices::rooms_export_path)
    };
    approve(&ApprovalOptions {
        identity_root,
        identity_dir: Some(identity_dir),
        timeout,
        workspace_cache: std::path::Path::new(""),
        acl: std::path::Path::new(""),
        relay_workspace_ids: &[],
        expected_device,
        expected_service,
        personal_rooms: personal_rooms.as_deref(),
        home: &home,
        kind: EnrollmentKind::PersonalDevice,
        port: NEARBY_PORT,
        ready: None,
    })
}

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
            JoinOptions {
                identity_root: &identity_root,
                label: &label,
                timeout,
                kind: EnrollmentKind::HomePeer,
                home_peer_output: Some(&home_peer_output),
                identity_dir: None,
                home: &PersonHome::Direct,
                requester_lease: None,
                port: NEARBY_PORT,
            },
            None,
        ),
        Command::PairDevice {
            identity_root,
            identity_dir,
            label,
            timeout,
            status_file,
            headless,
            requester_pid,
            requester_start_time,
            person_uid,
        } => pair_device_cli(
            &identity_root,
            &identity_dir,
            &label,
            timeout,
            status_file.as_deref(),
            personal_device_requester_lease(headless, requester_pid, requester_start_time),
            person_uid,
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
            personal_rooms: None,
            home: &PersonHome::Direct,
            kind: EnrollmentKind::HomePeer,
            port: NEARBY_PORT,
            ready: None,
        }),
        Command::ApproveDevice {
            identity_root,
            identity_dir,
            timeout,
            expected_device,
            expected_service,
            personal_rooms,
            person_uid,
        } => approve_device_cli(
            &identity_root,
            &identity_dir,
            timeout,
            expected_device.as_deref(),
            expected_service.as_deref(),
            personal_rooms,
            person_uid,
        ),
        Command::PersonHome(args) => args.serve(),
        Command::OutOfBand(command) => out_of_band(command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };

    /// Stage a confirmed personal enrollment for `identity_dir`, returning
    /// the marker ready to commit.
    fn staged_ready(
        identity_root: &std::path::Path,
        identity_dir: &std::path::Path,
        nonce: [u8; 16],
    ) -> (IdentityPaths, PathBuf, PersonalEnrollmentMarker) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
        let paths = IdentityPaths::under(identity_root);
        paths.ensure_directories().unwrap();
        let (marker_path, marker) = stage_personal_enrollment(
            &PersonalStage {
                identity_root,
                identity_dir,
                home: &PersonHome::Direct,
                paths: &paths,
                approver_device: None,
                requester_lease: None,
            },
            &personal_payload(&root, &member, nonce),
        )
        .unwrap();
        let marker = authorize_personal_commit(&marker_path, &marker, None).unwrap();
        (paths, marker_path, marker)
    }

    fn commit(
        identity_root: &std::path::Path,
        paths: &IdentityPaths,
        marker_path: &std::path::Path,
        marker: &PersonalEnrollmentMarker,
    ) -> anyhow::Result<PathBuf> {
        commit_staged_personal_enrollment(
            identity_root,
            paths,
            &PersonHome::Direct,
            marker_path,
            marker,
            |_| Ok(()),
        )
    }

    fn stage_dir_of(identity_root: &std::path::Path, marker: &PersonalEnrollmentMarker) -> PathBuf {
        enrollment_stage_dir(identity_root).join(&marker.stage_name)
    }

    #[test]
    fn a_staging_name_anything_already_holds_is_never_used() {
        use std::os::unix::fs::PermissionsExt as _;
        let scope = tempfile::tempdir().unwrap();
        let identity_root = scope.path().join("root");
        let staging = enrollment_stage_dir(&identity_root);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(enrollment_commit_dir(&identity_root)).unwrap();
        // The first name drawn is a directory someone planted; the second
        // is free in staging but already a marker's name.
        let planted = staging.join(format!("personal-{}", hex::encode([1_u8; 16])));
        std::fs::create_dir(&planted).unwrap();
        std::fs::write(planted.join("record.json"), b"planted").unwrap();
        std::fs::write(
            personal_marker_path(
                &identity_root,
                &format!("personal-{}", hex::encode([2_u8; 16])),
            ),
            b"planted",
        )
        .unwrap();
        let mut draws = [[1_u8; 16], [2; 16], [3; 16]].into_iter();
        let marker = |_: &str| Ok(b"{}".to_vec());
        let (name, stage) =
            create_stage_dir_drawing(&identity_root, marker, || Ok(draws.next().unwrap())).unwrap();
        assert_eq!(name, format!("personal-{}", hex::encode([3_u8; 16])));
        // The directory drawn for the taken marker name was given back, and
        // the marker of the one used is this account's.
        assert!(
            !staging
                .join(format!("personal-{}", hex::encode([2_u8; 16])))
                .exists()
        );
        assert_eq!(
            std::fs::read(personal_marker_path(&identity_root, &name)).unwrap(),
            b"{}"
        );
        assert!(valid_stage_name(&name));
        assert_eq!(
            std::fs::read(planted.join("record.json")).unwrap(),
            b"planted"
        );
        assert_eq!(
            std::fs::metadata(staging.join(&name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            person_home::local::fstat(&stage).unwrap().st_uid,
            person_home::local::own_uid()
        );
        // Nothing free at all: refused, never reused.
        assert!(create_stage_dir_drawing(&identity_root, marker, || Ok([1; 16])).is_err());
    }

    #[test]
    fn a_marker_for_another_person_or_identity_is_refused_and_skipped() {
        for tamper in [
            (|marker: &mut PersonalEnrollmentMarker| {
                marker.person_uid = marker.person_uid.wrapping_add(1);
            }) as fn(&mut PersonalEnrollmentMarker),
            |marker: &mut PersonalEnrollmentMarker| marker.user_public_key = [9; 32],
        ] {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let (paths, marker_path, mut marker) =
                staged_ready(&identity_root, &identity_dir, [51; 16]);
            tamper(&mut marker);
            write_marker(&marker_path, &marker).unwrap();
            assert!(commit(&identity_root, &paths, &marker_path, &marker).is_err());
            assert_eq!(std::fs::read_dir(&identity_dir).unwrap().count(), 0);
            assert!(!paths.device_cert.exists(), "nothing is published");
            recover_personal_enrollments(
                &identity_root,
                &identity_dir,
                &paths,
                &PersonHome::Direct,
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(std::fs::read_dir(&identity_dir).unwrap().count(), 0);
            assert!(!paths.device_cert.exists(), "recovery skips it too");
            assert!(marker_path.exists());
        }
        // Root recovering for one person never finishes an enrollment
        // staged for another: it is skipped before anything runs as anyone.
        let scope = tempfile::tempdir().unwrap();
        let identity_root = scope.path().join("root");
        let identity_dir = scope.path().join("identities");
        std::fs::create_dir(&identity_dir).unwrap();
        let (paths, marker_path, mut marker) =
            staged_ready(&identity_root, &identity_dir, [52; 16]);
        marker.person_uid = 4002;
        write_marker(&marker_path, &marker).unwrap();
        let for_a = PersonHome::Delegated {
            program: PathBuf::from("/nonexistent/tilo-enroll"),
            person: person_home::Person {
                uid: 4001,
                gid: 4001,
            },
        };
        recover_personal_enrollments(&identity_root, &identity_dir, &paths, &for_a, |_| Ok(()))
            .unwrap();
        assert!(!paths.device_cert.exists());
        assert!(marker_path.exists());
    }

    /// Hand everything below `path` to `uid`, except `keep` (left as it is).
    fn hand_tree_to(path: &std::path::Path, uid: u32, keep: &[PathBuf]) {
        if keep.iter().any(|kept| kept == path) {
            return;
        }
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c` is NUL-terminated.
        assert_eq!(unsafe { libc::lchown(c.as_ptr(), uid, uid) }, 0);
        if std::fs::symlink_metadata(path).unwrap().is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                hand_tree_to(&entry.unwrap().path(), uid, keep);
            }
        }
    }

    /// Recovery finishes only enrollments the account running it wrote. A
    /// marker root left while joining for someone, even one anyone may read,
    /// is not the person's to finish when they join unprivileged later, and
    /// does not stop their own enrollment from completing.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "root stages, the person recovers in a child, and both sides are checked"
    )]
    fn a_marker_root_wrote_is_neither_finished_nor_blocking_in_the_persons_recovery() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        const PERSON: u32 = 4001;
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            // Only root can leave a marker another account then meets.
            return;
        }
        let scope = tempfile::tempdir().unwrap();
        std::fs::set_permissions(scope.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let identity_root = scope.path().join("root");
        let identity_dir = scope.path().join("identities");
        std::fs::create_dir(&identity_dir).unwrap();
        let paths = IdentityPaths::under(&identity_root);
        paths.ensure_directories().unwrap();
        // Root's enrollment is of another identity and device, so whichever
        // one completes shows in the published certificate.
        let root_identity = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let (root_marker, root_staged) = stage_personal_enrollment(
            &PersonalStage {
                identity_root: &identity_root,
                identity_dir: &identity_dir,
                home: &PersonHome::Direct,
                paths: &paths,
                approver_device: None,
                requester_lease: None,
            },
            &personal_payload(
                &root_identity,
                &ed25519_dalek::SigningKey::from_bytes(&[72; 32]),
                [41; 16],
            ),
        )
        .unwrap();
        authorize_personal_commit(&root_marker, &root_staged, None).unwrap();
        let root_stage = stage_dir_of(&identity_root, &root_staged);
        // Readable to the person, so only the ownership rule refuses it.
        std::fs::set_permissions(&root_marker, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&root_stage, std::fs::Permissions::from_mode(0o755)).unwrap();
        for staged in std::fs::read_dir(&root_stage).unwrap() {
            std::fs::set_permissions(
                staged.unwrap().path(),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        hand_tree_to(
            scope.path(),
            PERSON,
            &[root_marker.clone(), root_stage.clone()],
        );
        for shared in [
            enrollment_stage_dir(&identity_root),
            enrollment_commit_dir(&identity_root),
            tilo_relay_protocol::personal_devices::peers_dir(&identity_root),
        ] {
            std::fs::create_dir_all(&shared).unwrap();
            std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        }

        let person_identity = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        // SAFETY: the test process forks once; the child only does this
        // file work as the person and exits without unwinding.
        let child = unsafe { libc::fork() };
        if child == 0 {
            let outcome = std::panic::catch_unwind(|| {
                // SAFETY: plain credential changes in the child.
                unsafe {
                    assert_eq!(libc::setgroups(0, std::ptr::null()), 0);
                    assert_eq!(libc::setgid(PERSON), 0);
                    assert_eq!(libc::setuid(PERSON), 0);
                }
                let (marker_path, marker) = stage_personal_enrollment(
                    &PersonalStage {
                        identity_root: &identity_root,
                        identity_dir: &identity_dir,
                        home: &PersonHome::Direct,
                        paths: &paths,
                        approver_device: None,
                        requester_lease: None,
                    },
                    &personal_payload(
                        &person_identity,
                        &ed25519_dalek::SigningKey::from_bytes(&[62; 32]),
                        [42; 16],
                    ),
                )
                .unwrap();
                authorize_personal_commit(&marker_path, &marker, None).unwrap();
                recover_personal_enrollments(
                    &identity_root,
                    &identity_dir,
                    &paths,
                    &PersonHome::Direct,
                    |_| Ok(()),
                )
                .unwrap();
                let published =
                    tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
                assert_eq!(
                    published.user_public_key,
                    person_identity.verifying_key().to_bytes(),
                    "the person's own join completed, not root's"
                );
                assert!(root_marker.exists(), "root's marker is left to root");
                assert!(root_stage.exists(), "root's staging is left to root");
                // The person's directory beside the identity root is theirs,
                // and root's identity has none.
                let peers = tilo_relay_protocol::personal_devices::peers_dir(&identity_root);
                let person = tilo_relay_protocol::personal_devices::PersonPeers::find(
                    &peers,
                    &person_identity.verifying_key().to_bytes(),
                    PERSON,
                )
                .unwrap()
                .unwrap();
                assert_eq!(std::fs::metadata(person.dir()).unwrap().uid(), PERSON);
                assert!(
                    tilo_relay_protocol::personal_devices::PersonPeers::find(
                        &peers,
                        &root_identity.verifying_key().to_bytes(),
                        PERSON,
                    )
                    .unwrap()
                    .is_none()
                );
            });
            // SAFETY: leave the child without running the parent's
            // destructors.
            unsafe { libc::_exit(i32::from(outcome.is_err())) };
        }
        let mut status = 0;
        // SAFETY: `child` is this process's child.
        assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "the person's recovery failed: {status}"
        );
        assert!(root_marker.exists());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario per planted link, each against the same staged enrollment"
    )]
    fn a_join_never_publishes_through_a_link_planted_between_staging_and_commit() {
        // An identity directory swapped for a link after staging.
        {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let (paths, marker_path, marker) =
                staged_ready(&identity_root, &identity_dir, [31; 16]);
            let elsewhere = scope.path().join("etc");
            std::fs::create_dir(&elsewhere).unwrap();
            std::fs::rename(&identity_dir, scope.path().join("moved")).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &identity_dir).unwrap();

            assert!(commit(&identity_root, &paths, &marker_path, &marker).is_err());
            assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
            assert!(!paths.device_cert.exists(), "nothing is published");
            assert!(marker_path.exists(), "the enrollment stays retryable");
        }
        // A staged file swapped for a link to a file only root may read.
        {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let (paths, marker_path, marker) =
                staged_ready(&identity_root, &identity_dir, [32; 16]);
            let secret = scope.path().join("shadow");
            std::fs::write(&secret, b"root:secret").unwrap();
            let staged_record = stage_dir_of(&identity_root, &marker).join("record.json");
            std::fs::remove_file(&staged_record).unwrap();
            std::os::unix::fs::symlink(&secret, &staged_record).unwrap();

            assert!(commit(&identity_root, &paths, &marker_path, &marker).is_err());
            for entry in std::fs::read_dir(&identity_dir).unwrap() {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap_or_default();
                assert_ne!(bytes, b"root:secret", "the secret never reaches the home");
            }
            assert!(!paths.device_cert.exists());
        }
        // The whole staging directory swapped for a link to look-alike files.
        {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let (paths, marker_path, marker) =
                staged_ready(&identity_root, &identity_dir, [33; 16]);
            let stage = stage_dir_of(&identity_root, &marker);
            let planted = scope.path().join("planted");
            std::fs::rename(&stage, &planted).unwrap();
            std::os::unix::fs::symlink(&planted, &stage).unwrap();

            assert!(commit(&identity_root, &paths, &marker_path, &marker).is_err());
            assert_eq!(std::fs::read_dir(&identity_dir).unwrap().count(), 0);
            assert!(!paths.device_cert.exists());
        }
        // A group member who knows the one-time code pre-creates every name
        // a join used to derive from it: a link where it staged, a marker
        // where it committed. The join neither uses them nor stops.
        {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
            let member = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
            let payload = personal_payload(&root, &member, [34; 16]);
            let old_stage = format!(
                "personal-{}-{}",
                hex::encode(payload.nonce),
                hex::encode(payload.cert.device_public_key)
            );
            let elsewhere = scope.path().join("etc");
            std::fs::create_dir(&elsewhere).unwrap();
            std::fs::create_dir_all(enrollment_stage_dir(&identity_root)).unwrap();
            std::fs::create_dir_all(enrollment_commit_dir(&identity_root)).unwrap();
            std::os::unix::fs::symlink(
                &elsewhere,
                enrollment_stage_dir(&identity_root).join(&old_stage),
            )
            .unwrap();
            let old_marker = enrollment_commit_dir(&identity_root)
                .join(format!("personal-{}.json", hex::encode(payload.nonce)));
            std::fs::write(&old_marker, b"planted").unwrap();
            let paths = IdentityPaths::under(&identity_root);
            paths.ensure_directories().unwrap();
            let (marker_path, marker) = stage_personal_enrollment(
                &PersonalStage {
                    identity_root: &identity_root,
                    identity_dir: &identity_dir,
                    home: &PersonHome::Direct,
                    paths: &paths,
                    approver_device: None,
                    requester_lease: None,
                },
                &payload,
            )
            .unwrap();
            assert!(valid_stage_name(&marker.stage_name));
            assert_ne!(marker.stage_name, old_stage);
            assert_ne!(marker_path, old_marker);
            assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
            assert_eq!(std::fs::read(&old_marker).unwrap(), b"planted");
            let marker = authorize_personal_commit(&marker_path, &marker, None).unwrap();
            commit(&identity_root, &paths, &marker_path, &marker).unwrap();
            assert!(paths.device_cert.exists(), "the join completed");
        }
        // A commit marker that is a link is never recovered.
        {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let (paths, marker_path, _marker) =
                staged_ready(&identity_root, &identity_dir, [35; 16]);
            let real = scope.path().join("marker.json");
            std::fs::rename(&marker_path, &real).unwrap();
            std::os::unix::fs::symlink(&real, &marker_path).unwrap();
            recover_personal_enrollments(
                &identity_root,
                &identity_dir,
                &paths,
                &PersonHome::Direct,
                |_| Ok(()),
            )
            .unwrap();
            assert!(!paths.device_cert.exists());
            assert_eq!(std::fs::read_dir(&identity_dir).unwrap().count(), 0);
        }
        // A confirmed enrollment whose staging another account took away is
        // abandoned by recovery rather than blocking every later join.
        {
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            std::fs::create_dir(&identity_dir).unwrap();
            let (paths, marker_path, marker) =
                staged_ready(&identity_root, &identity_dir, [37; 16]);
            std::fs::rename(
                stage_dir_of(&identity_root, &marker),
                scope.path().join("taken"),
            )
            .unwrap();
            recover_personal_enrollments(
                &identity_root,
                &identity_dir,
                &paths,
                &PersonHome::Direct,
                |_| Ok(()),
            )
            .unwrap();
            assert!(!paths.device_cert.exists());
            assert!(marker_path.exists());
        }
        // Undisturbed, the same enrollment publishes into the person's
        // directory, owned by the account that ran it, with no chown.
        {
            use std::os::unix::fs::MetadataExt as _;
            let scope = tempfile::tempdir().unwrap();
            let identity_root = scope.path().join("root");
            let identity_dir = scope.path().join("identities");
            let (paths, marker_path, marker) =
                staged_ready(&identity_root, &identity_dir, [36; 16]);
            let record = commit(&identity_root, &paths, &marker_path, &marker).unwrap();
            // SAFETY: geteuid has no preconditions.
            let uid = unsafe { libc::geteuid() };
            for path in [
                record,
                identity_dir.join("registry.json"),
                fleet_path(&identity_dir, marker.user_public_key),
            ] {
                assert_eq!(std::fs::metadata(&path).unwrap().uid(), uid);
            }
            assert!(paths.device_cert.exists());
            assert!(!marker_path.exists());
            assert!(!stage_dir_of(&identity_root, &marker).exists());
        }
    }

    #[test]
    fn device_trust_files_are_never_published_through_a_planted_link() {
        use std::os::unix::fs::PermissionsExt as _;
        let scope = tempfile::tempdir().unwrap();
        let identity_root = scope.path().join("tilo");
        let device = identity_root.join("device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o2770)).unwrap();
        let elsewhere = scope.path().join("etc");
        std::fs::create_dir(&elsewhere).unwrap();
        let secret = scope.path().join("shadow");
        std::fs::write(&secret, b"root:secret").unwrap();
        let per_user = std::path::Path::new("device/certs/ab.bin");

        // A member swaps the per-identity directory for a link.
        std::os::unix::fs::symlink(&elsewhere, device.join("certs")).unwrap();
        assert!(publish_device_file(&identity_root, per_user, b"cert").is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        // The certificate itself planted as a link to a file root owns.
        std::fs::remove_file(device.join("certs")).unwrap();
        std::os::unix::fs::symlink(&secret, device.join("cert.bin")).unwrap();
        assert!(
            publish_device_file(
                &identity_root,
                std::path::Path::new("device/cert.bin"),
                b"c"
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&secret).unwrap(), b"root:secret");

        // Undisturbed, the per-identity directory is created like its parent
        // and the certificate published with the public mode.
        publish_device_file(&identity_root, per_user, b"cert").unwrap();
        assert_eq!(
            std::fs::metadata(device.join("certs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o2770
        );
        let published = device.join("certs/ab.bin");
        assert_eq!(std::fs::read(&published).unwrap(), b"cert");
        assert_eq!(
            std::fs::metadata(&published).unwrap().permissions().mode() & 0o777,
            tilo_user_identity::persist::PUBLIC_FILE_MODE
        );

        // A directory every account may write is refused.
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(publish_device_file(&identity_root, per_user, b"cert").is_err());
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o2770)).unwrap();

        // As root, a top-level directory another account owns is refused.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            // SAFETY: `path` is NUL-terminated.
            let path = std::ffi::CString::new(device.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::chown(path.as_ptr(), 65_534, 65_534) }, 0);
            assert!(publish_device_file(&identity_root, per_user, b"other").is_err());
            assert_eq!(std::fs::read(&published).unwrap(), b"cert");
        }
    }

    #[test]
    fn a_planted_home_peer_registry_is_not_followed_or_waited_on() {
        let identity_root = tempfile::tempdir().unwrap();
        let path = tilo_relay::home_peer_registry_path(identity_root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user = [3; 32];
        assert_eq!(
            bounded_home_peer_registry(identity_root.path(), user).unwrap(),
            b"[]"
        );
        let secret = identity_root.path().join("secret");
        std::fs::write(&secret, b"[]").unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        assert!(bounded_home_peer_registry(identity_root.path(), user).is_err());
        std::fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `fifo` is NUL-terminated.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(bounded_home_peer_registry(identity_root.path(), user).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
        assert!(bounded_home_peer_registry(identity_root.path(), user).is_err());
    }

    #[test]
    fn host_record_approval_keeps_admission_and_revocation_checks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tilo");
        let identities = temp.path().join("identities");
        let paths = IdentityPaths::under(&root);
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identities,
            &paths,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        assert!(!paths.user_privkey.exists());
        let options = ApprovalOptions {
            identity_root: &root,
            identity_dir: Some(&identities),
            timeout: 1,
            workspace_cache: temp.path(),
            acl: temp.path(),
            relay_workspace_ids: &[],
            personal_rooms: None,
            home: &PersonHome::Direct,
            expected_device: None,
            expected_service: None,
            kind: EnrollmentKind::HomePeer,
            port: 0,
            ready: None,
        };
        let identity = load_approval_identity(&options).unwrap();
        let mut cert = payload(&ed25519_dalek::SigningKey::from_bytes(&[74; 32])).cert;
        cert.user_public_key = identity.user_public_key;
        sign_root_cert(&mut cert, identity.root.as_ref()).unwrap();
        verify_device_cert(&cert).unwrap();
        let mut revocation = RevocationEntry {
            user_public_key: identity.user_public_key,
            revoked_device_public_key: identity.device.verifying_key().to_bytes(),
            revoked_at_ns: 1,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        revocation.signature = identity
            .root
            .sign_message(&tilo_user_identity::revocation_signing_digest(&revocation).unwrap())
            .unwrap();
        let path = fleet_path(&identities, identity.user_public_key);
        let mut fleet: FleetProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        fleet.revocations.push(revocation);
        std::fs::write(&path, serde_json::to_vec(&fleet).unwrap()).unwrap();
        let error = load_approval_identity(&options).err().unwrap();
        assert!(error.to_string().contains("revoked"));
        fleet.revocations.clear();
        std::fs::write(&path, serde_json::to_vec(&fleet).unwrap()).unwrap();
        let mut issuer = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        issuer.capabilities = DeviceCapabilities::member();
        sign_root_cert(&mut issuer, identity.root.as_ref()).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&identity.user_public_key),
            &issuer,
        )
        .unwrap();
        let error = load_approval_identity(&options).err().unwrap();
        assert!(error.to_string().contains("not authorised"));
        issuer.capabilities = DeviceCapabilities::primary();
        issuer.issued_at_ns = 1;
        issuer.expires_at_ns = Some(2);
        sign_root_cert(&mut issuer, identity.root.as_ref()).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&identity.user_public_key),
            &issuer,
        )
        .unwrap();
        let error = load_approval_identity(&options).err().unwrap();
        assert!(error.to_string().contains("validity"));
        let unrelated_root = ed25519_dalek::SigningKey::from_bytes(&[76; 32]);
        issuer.expires_at_ns = None;
        issuer.user_public_key = unrelated_root.verifying_key().to_bytes();
        sign_device_cert(&mut issuer, &unrelated_root).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&identity.user_public_key),
            &issuer,
        )
        .unwrap();
        let error = load_approval_identity(&options).err().unwrap();
        assert!(error.to_string().contains("not authorised"));
        assert!(!paths.user_privkey.exists());
    }

    /// The root a device holds certifies a developer's machine for
    /// administration and lists it in the person's fleet, where Settings can
    /// remove it; an unknown capability names the ones there are.
    #[test]
    fn a_held_root_certifies_an_administrator_into_the_fleet() {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        paths.ensure_directories().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
        let admin = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
        let device = hex::encode(admin.verifying_key().to_bytes());
        let out = identity_dir.path().join("admin.cert");
        let options = |capabilities| CertifyOptions {
            root_key: None,
            identity_root: identity_root.path(),
            identity_dir: Some(identity_dir.path()),
            device: &device,
            label: "workstation (tilo-vm)",
            capabilities,
            out: &out,
        };
        let shell = ["shell".to_owned()];
        let unknown = certify(&options(&shell)).unwrap_err();
        assert!(
            unknown.to_string().contains("read-all or admin"),
            "{unknown}"
        );

        let admin_only = ["admin".to_owned()];
        certify(&options(&admin_only)).unwrap();
        let cert = tilo_user_identity::persist::read_device_cert(&out).unwrap();
        verify_device_cert(&cert).unwrap();
        assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(cert.capabilities, DeviceCapabilities::administrator());
        let fleet: FleetProjection = serde_json::from_slice(
            &std::fs::read(fleet_path(identity_dir.path(), cert.user_public_key)).unwrap(),
        )
        .unwrap();
        assert_eq!(fleet.certificates, vec![cert]);
    }

    #[test]
    fn an_out_of_band_certificate_joins_its_identity() {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        device_key(identity_root.path()).unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let user = root.verifying_key().to_bytes();
        let issue = |device_public_key: [u8; 32], root: &ed25519_dalek::SigningKey| {
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key,
                device_label: "Node".into(),
                capabilities: DeviceCapabilities::host(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, root).unwrap();
            let path = identity_dir
                .path()
                .join(format!("{}.cert", hex::encode(&cert.signature[..8])));
            tilo_user_identity::persist::write_device_cert(&path, &cert).unwrap();
            path
        };

        let other_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let foreign = issue(other_device.verifying_key().to_bytes(), &root);
        assert!(
            install_cert(
                identity_root.path(),
                identity_dir.path(),
                &foreign,
                &PersonHome::Direct
            )
            .is_err()
        );
        assert!(!paths.device_cert.exists());

        let root_key = identity_dir.path().join("root.key");
        tilo_user_identity::persist::write_signing_key(&root_key, &root).unwrap();
        let own = identity_dir.path().join("own.cert");
        let capabilities = ["own", "host", "read-all"].map(String::from);
        certify(&CertifyOptions {
            root_key: Some(&root_key),
            identity_root: identity_root.path(),
            identity_dir: None,
            device: &hex::encode(device.verifying_key().to_bytes()),
            label: "Node",
            capabilities: &capabilities,
            out: &own,
        })
        .unwrap();
        install_cert(
            identity_root.path(),
            identity_dir.path(),
            &own,
            &PersonHome::Direct,
        )
        .unwrap();
        // Delivered again (a restart before the launcher noticed): unchanged.
        install_cert(
            identity_root.path(),
            identity_dir.path(),
            &own,
            &PersonHome::Direct,
        )
        .unwrap();
        let installed = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        assert_eq!(installed.user_public_key, user);
        assert!(
            installed.capabilities.can_act_as_host && !installed.capabilities.can_admit_devices
        );
        assert_eq!(
            tilo_user_identity::persist::read_public_key(&paths.user_pubkey).unwrap(),
            user
        );
        let record =
            runtime_identity::load_from_path(identity_record_path(identity_dir.path(), user))
                .unwrap();
        assert_eq!(record.group_public_key(), user);
        assert!(record.sign(b"root-only operation").is_err());

        let stranger = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        let takeover = issue(device.verifying_key().to_bytes(), &stranger);
        assert!(
            install_cert(
                identity_root.path(),
                identity_dir.path(),
                &takeover,
                &PersonHome::Direct
            )
            .is_err()
        );
        assert_eq!(
            tilo_user_identity::persist::read_public_key(&paths.user_pubkey).unwrap(),
            user
        );
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
            personal_rooms_json: None,
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
            personal_rooms_json: None,
        }
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

            let error =
                project_personal_device(&PersonHome::Direct, identity.path(), &cert).unwrap_err();

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
            tilo_files::native::atomic::write_creating_parent(
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

            assert!(
                fleet_revokes_device(
                    &PersonHome::Direct,
                    identity.path(),
                    user,
                    device_public_key
                )
                .unwrap()
            );
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
                identity_dir: None,
                timeout: 1,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                relay_workspace_ids: &[],
                expected_device: None,
                expected_service: None,
                personal_rooms: None,
                home: &PersonHome::Direct,
                kind: EnrollmentKind::HomePeer,
                port: 0,
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
                ResponseRoute {
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

            // Keep one live requester on its stdin pipe. Killing a shell loop
            // leaves its timer child holding the test's output handles.
            let mut requester = std::process::Command::new("/bin/cat")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
            let payload = personal_payload(&root, &member, [9; 16]);
            let (marker_path, marker) = stage_personal_enrollment(
                &PersonalStage {
                    identity_root: identity_root.path(),
                    identity_dir: identity_dir.path(),
                    home: &PersonHome::Direct,
                    paths: &paths,
                    approver_device: Some([7; 32]),
                    requester_lease: Some(lease),
                },
                &payload,
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
                &PersonHome::Direct,
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
            assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
            let personal_peers =
                tilo_relay_protocol::personal_device_peer_path(identity_root.path());
            assert!(
                !personal_peers.exists(),
                "an unconfirmed enrollment records no personal device"
            );

            let marker = authorize_personal_commit(&marker_path, &marker, Some(lease)).unwrap();
            assert!(
                commit_staged_personal_enrollment(
                    identity_root.path(),
                    &paths,
                    &PersonHome::Direct,
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
            // First use reads a selected joined identity as a finished join;
            // an interrupted commit has selected nothing yet.
            assert_eq!(
                runtime_identity::resolve_active_identity(identity_dir.path()),
                None,
                "an unfinished join selected its identity"
            );
            requester.kill().unwrap();
            requester.wait().unwrap();
            assert!(lease.validate().is_err());
            // `commit_ready` is the durable boundary: Finalized and the original
            // live lease were authenticated before it was written. A subsequent
            // explicit retry may complete that exact transaction under its own
            // live lease rather than leaving mixed durable state.
            let retry_lease = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
            // A root-run commit may run under a strict umask (the pairing
            // helper's, or a script's); the public files stay readable.
            // SAFETY: umask has no preconditions; this test process is alone.
            let umask = unsafe { libc::umask(0o077) };
            let recovered = recover_personal_enrollments(
                identity_root.path(),
                identity_dir.path(),
                &paths,
                &PersonHome::Direct,
                |_| retry_lease.validate().map_err(Into::into),
            );
            // SAFETY: as above, restoring the previous mask.
            unsafe { libc::umask(umask) };
            recovered.unwrap();
            for public in [
                paths.user_pubkey.clone(),
                paths.device_cert.clone(),
                paths.device_cert_for_user(&root.verifying_key().to_bytes()),
                personal_peers.clone(),
            ] {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    std::fs::metadata(&public).unwrap().permissions().mode() & 0o777,
                    tilo_user_identity::persist::PUBLIC_FILE_MODE,
                    "{} is not readable to the session",
                    public.display()
                );
            }
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
            let recorded: Vec<String> =
                serde_json::from_slice(&std::fs::read(&personal_peers).unwrap()).unwrap();
            assert_eq!(
                recorded,
                vec![hex::encode(member.verifying_key().to_bytes())]
            );
            assert!(
                runtime_identity::resolve_active_identity(identity_dir.path()).is_some(),
                "the finished join selects its identity"
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
                personal_rooms_json: None,
            };
            let identity_root = tempfile::tempdir().unwrap();
            let identity_dir = tempfile::tempdir().unwrap();
            let record = install_personal_projection(
                identity_root.path(),
                identity_dir.path(),
                &PersonHome::Direct,
                &payload,
            )
            .unwrap();
            // The joined identity is what this device acts for from the next
            // login on, not an absent default that asks to create one.
            assert_eq!(
                runtime_identity::resolve_active_identity(identity_dir.path()),
                Some(record.clone())
            );
            let loaded = runtime_identity::load_from_path(record).unwrap();
            assert_eq!(loaded.group_public_key(), user);
            assert!(!loaded.is_single_device());
            assert!(loaded.sign(b"root-only operation").is_err());
            assert_eq!(
                std::fs::read(tilo_relay::home_peer_registry_path(identity_root.path())).unwrap(),
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
                environments: tilo_relay_protocol::RelayEnvironments::default(),
            };
            tilo_files::native::atomic::write_creating_parent(
                &tilo_relay::home_peer_registry_path(identity_root.path()),
                &serde_json::to_vec_pretty(&vec![other.clone()]).unwrap(),
            )
            .unwrap();
            install_personal_projection(
                identity_root.path(),
                identity_dir.path(),
                &PersonHome::Direct,
                &personal_payload(&root, &member, [10; 16]),
            )
            .unwrap();
            let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
                &std::fs::read(tilo_relay::home_peer_registry_path(identity_root.path())).unwrap(),
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
                environments: tilo_relay_protocol::RelayEnvironments::default(),
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
            let mut requester = std::process::Command::new("/bin/cat")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
            let payload = personal_payload(&root, &member, [12; 16]);
            let (marker_path, marker) = stage_personal_enrollment(
                &PersonalStage {
                    identity_root: identity_root.path(),
                    identity_dir: &identity_dir,
                    home: &PersonHome::Direct,
                    paths: &paths,
                    approver_device: Some([7; 32]),
                    requester_lease: Some(lease),
                },
                &payload,
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
                    &PersonHome::Direct,
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
            let mut requester = std::process::Command::new("/bin/cat")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
            requester.kill().unwrap();
            requester.wait().unwrap();

            let identity_root = tempfile::tempdir().unwrap();
            let identity_dir = tempfile::tempdir().unwrap();
            assert!(
                pair(
                    JoinOptions {
                        identity_root: identity_root.path(),
                        label: "New laptop",
                        timeout: 1,
                        kind: EnrollmentKind::PersonalDevice,
                        home_peer_output: None,
                        identity_dir: Some(identity_dir.path()),
                        home: &PersonHome::Direct,
                        requester_lease: Some(lease),
                        port: 0,
                    },
                    None,
                )
                .is_err()
            );
            let paths = IdentityPaths::under(identity_root.path());
            assert!(!paths.device_privkey.exists());
            assert!(!paths.user_pubkey.exists());
            assert!(!paths.device_cert.exists());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end test keeps the approver and joining-device setup in one auditable protocol scenario"
    )]
    fn nearby_personal_device_pairing_links_both_devices() {
        use tilo_relay_protocol::personal_devices;
        let approver_root = tempfile::tempdir().unwrap();
        let approver_identities = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(approver_root.path());
        paths.ensure_directories().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
        tilo_user_identity::persist::write_public_key(
            &paths.user_pubkey,
            &root.verifying_key().to_bytes(),
        )
        .unwrap();
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &issuer).unwrap();
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
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &issuer_cert).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&root.verifying_key().to_bytes()),
            &issuer_cert,
        )
        .unwrap();
        tilo_files::native::atomic::write_creating_parent(
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
            &[tilo_relay::personal_continuity_workspace_id(
                &root.verifying_key().to_bytes(),
            )],
        )
        .unwrap();

        // Seed the new device's key so the approval pins exactly this device:
        // an unpinned approver answers whichever device solicits first.
        let new_root = tempfile::tempdir().unwrap();
        let new_identities = tempfile::tempdir().unwrap();
        let new_device = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
        let new_paths = IdentityPaths::under(new_root.path());
        new_paths.ensure_directories().unwrap();
        tilo_user_identity::persist::write_device_key(&new_paths.device_privkey, &new_device)
            .unwrap();
        let expected_device = hex::encode(new_device.verifying_key().to_bytes());
        let expected_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&new_device));

        let shared_room = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let rooms_export = approver_identities.path().join("exported-rooms.json");
        std::fs::write(&rooms_export, serde_json::to_vec(&[shared_room]).unwrap()).unwrap();
        let server_root = approver_root.path().to_path_buf();
        let server_identities = approver_identities.path().to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let started_ns = now_ns().unwrap();
        let approval = std::thread::spawn(move || {
            approve(&ApprovalOptions {
                identity_root: &server_root,
                identity_dir: Some(&server_identities),
                timeout: 60,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                relay_workspace_ids: &[],
                expected_device: Some(&expected_device),
                expected_service: Some(&expected_service),
                personal_rooms: Some(&rooms_export),
                home: &PersonHome::Direct,
                kind: EnrollmentKind::PersonalDevice,
                // An OS-assigned port keeps concurrent tests off each other's
                // listeners and off the well-known port.
                port: 0,
                ready: Some(&ready_tx),
            })
        });
        let port = match ready_rx.recv_timeout(tilo_test_env::hang_bound()) {
            Ok(port) => port,
            // An approver that ended before listening dropped the sender; its
            // own error names why.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the approver ended before listening: {:?}", approval.join())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("the approval listener did not become ready within the hang bound")
            }
        };
        // Other nearby-pairing surfaces on this host (an open discovery
        // view, another approval sheet) share the approver's port. Which of
        // them the kernel hands a unicast datagram must not decide whether
        // the enrollment completes.
        let _other_surfaces: Vec<UdpSocket> =
            (0..3).map(|_| bind_listener(port).unwrap()).collect();
        let requester = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
        pair(
            JoinOptions {
                identity_root: new_root.path(),
                label: "New laptop",
                timeout: 60,
                kind: EnrollmentKind::PersonalDevice,
                home_peer_output: None,
                identity_dir: Some(new_identities.path()),
                home: &PersonHome::Direct,
                requester_lease: Some(requester),
                port,
            },
            None,
        )
        .unwrap();
        approval.join().unwrap().unwrap();

        let enrolled_paths = IdentityPaths::under(new_root.path());
        let cert =
            tilo_user_identity::persist::read_device_cert(&enrolled_paths.device_cert).unwrap();
        assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(cert.capabilities, DeviceCapabilities::joined());
        assert!(verify_device_cert(&cert).is_ok());
        // The new device checks its certificate on its own clock the moment it
        // arrives, and that clock can trail the approver's. One that reads a
        // full allowance behind the end of this pairing still accepts it, and
        // validity begins no further back than the allowance.
        let finished_ns = now_ns().unwrap();
        verify_device_cert_at(&cert, finished_ns - ISSUANCE_CLOCK_ALLOWANCE_NS)
            .expect("a device whose clock trails the approver's accepts its certificate");
        assert!(cert.issued_at_ns >= started_ns - ISSUANCE_CLOCK_ALLOWANCE_NS);
        assert!(!enrolled_paths.user_privkey.exists());
        let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(tilo_relay::home_peer_registry_path(new_root.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(peers.len(), 1);
        assert!(peers[0].relay_workspace_ids.contains(
            &tilo_relay::personal_continuity_workspace_id(&root.verifying_key().to_bytes())
        ));
        let record = identity_record_path(new_identities.path(), root.verifying_key().to_bytes());
        assert_eq!(
            runtime_identity::resolve_active_identity(new_identities.path()),
            Some(record.clone())
        );
        let identity = runtime_identity::load_from_path(record).unwrap();
        assert!(
            identity
                .sign(b"root/admin signing must stay unavailable")
                .is_err()
        );

        // Both devices dial each other directly afterwards, and the new
        // device joins the approver's rooms. The home server stays a
        // home-peer route, never a direct peer.
        let person_of = |identity_root: &std::path::Path| {
            personal_devices::PersonPeers::find_own(
                &personal_devices::peers_dir(identity_root),
                &root.verifying_key().to_bytes(),
            )
            .unwrap()
            .expect("the person's directory exists")
        };
        assert_eq!(
            person_of(new_root.path()).read_direct_peers().unwrap(),
            vec![personal_devices::DirectPeer {
                identity: root.verifying_key().to_bytes(),
                device: issuer.verifying_key().to_bytes(),
            }]
        );
        assert_eq!(
            person_of(new_root.path()).read_rooms().unwrap(),
            vec![shared_room.to_string()]
        );
        assert_eq!(
            person_of(approver_root.path()).read_direct_peers().unwrap(),
            vec![personal_devices::DirectPeer {
                identity: root.verifying_key().to_bytes(),
                device: new_device.verifying_key().to_bytes(),
            }]
        );
        // Each machine now lists the other, and itself, as this person's
        // devices: the list remote actions and wake address. The always-on
        // home peer is a home peer, not a personal device.
        let personal = |root: &std::path::Path| -> Vec<String> {
            serde_json::from_slice(
                &std::fs::read(tilo_relay_protocol::personal_device_peer_path(root))
                    .expect("pairing recorded the personal devices"),
            )
            .unwrap()
        };
        let approver_key = hex::encode(issuer.verifying_key().to_bytes());
        let new_key = hex::encode(new_device.verifying_key().to_bytes());
        assert_eq!(
            personal(approver_root.path()),
            vec![approver_key.clone(), new_key.clone()]
        );
        assert_eq!(personal(new_root.path()), vec![new_key, approver_key]);
    }

    /// A joining device records only the personal devices whose certificates
    /// verify under the identity it joined, are valid now and are not
    /// revoked; a malformed fleet records nothing.
    #[test]
    fn a_joining_device_records_only_certified_live_personal_devices() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
        let user = root.verifying_key().to_bytes();
        let cert =
            |device: u8, capabilities: DeviceCapabilities, signer: &ed25519_dalek::SigningKey| {
                let mut cert = DeviceCert {
                    user_public_key: signer.verifying_key().to_bytes(),
                    device_public_key: [device; 32],
                    device_label: format!("Device {device}"),
                    capabilities,
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, signer).unwrap();
                cert
            };
        let approver = cert(1, DeviceCapabilities::primary(), &root);
        let sibling = cert(2, DeviceCapabilities::member(), &root);
        let revoked = cert(3, DeviceCapabilities::member(), &root);
        let host = cert(4, DeviceCapabilities::host(), &root);
        let administrator = cert(5, DeviceCapabilities::administrator(), &root);
        let foreign_root = cert(6, DeviceCapabilities::member(), &stranger);
        let mut forged = cert(7, DeviceCapabilities::member(), &root);
        forged.device_public_key = [8; 32];
        let mut expired = cert(9, DeviceCapabilities::member(), &root);
        expired.expires_at_ns = Some(2);
        sign_device_cert(&mut expired, &root).unwrap();
        let mut revocation = RevocationEntry {
            user_public_key: user,
            revoked_device_public_key: revoked.device_public_key,
            revoked_at_ns: 2,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        let fleet = |revocations: Vec<RevocationEntry>| {
            serde_json::to_vec(&FleetProjection {
                schema_version: 1,
                identity_id: hex::encode(user),
                certificates: vec![
                    approver.clone(),
                    sibling.clone(),
                    revoked.clone(),
                    host.clone(),
                    administrator.clone(),
                    foreign_root.clone(),
                    forged.clone(),
                    expired.clone(),
                ],
                revocations,
            })
            .unwrap()
        };
        let now = now_ns().unwrap();
        assert_eq!(
            certified_personal_devices(&fleet(vec![revocation.clone()]), user, [10; 32], now)
                .unwrap(),
            vec![[10; 32], [1; 32], [2; 32]]
        );

        let mut tampered = revocation;
        tampered.revoked_device_public_key = [2; 32];
        assert!(certified_personal_devices(&fleet(vec![tampered]), user, [10; 32], now).is_err());
        let other_identity = serde_json::to_vec(&FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(stranger.verifying_key().to_bytes()),
            certificates: vec![approver],
            revocations: Vec::new(),
        })
        .unwrap();
        assert!(certified_personal_devices(&other_identity, user, [10; 32], now).is_err());
        assert!(certified_personal_devices(b"[]", user, [10; 32], now).is_err());
    }

    #[test]
    fn a_failed_first_use_pairing_reports_its_reason_in_the_status_file() {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let status = identity_dir.path().join(".nearby-enrollment-status.json");
        // A requester that is already gone: the lease check fails before
        // anything is broadcast, as an expired or foreign compositor would.
        let error = pair_device_cli(
            identity_root.path(),
            identity_dir.path(),
            "New laptop",
            1,
            Some(&status),
            personal_device_requester_lease(false, Some(u32::MAX - 1), Some(1)),
            None,
        )
        .unwrap_err();
        let recorded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(recorded["state"], "failed");
        assert_eq!(recorded["error"], format!("{error:#}"));
        assert!(
            recorded["error"]
                .as_str()
                .unwrap()
                .contains("first-use requester exited"),
            "the reason reaches the UI: {recorded}"
        );
    }

    #[test]
    fn a_failed_pairing_names_its_cause() {
        let denied = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EACCES))
            .context("publish the enrollment commit");
        assert_eq!(pairing_failure_cause(&denied), "permission_denied");
        let full = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENOSPC));
        assert_eq!(pairing_failure_cause(&full), "storage_full");
        let read_only = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EROFS));
        assert_eq!(pairing_failure_cause(&read_only), "read_only");
        let unreachable = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENETUNREACH));
        assert_eq!(pairing_failure_cause(&unreachable), "unreachable");
        let closed = anyhow::Error::from(WindowClosed("the window closed"));
        assert_eq!(pairing_failure_cause(&closed), "timed_out");
        assert_eq!(
            pairing_failure_cause(&anyhow::anyhow!("first-use requester exited")),
            "unknown"
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
            let encrypted = encrypt_pair_offer(token, server.verifying_key().to_bytes()).unwrap();
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
            let json: serde_json::Value = serde_json::from_slice(
                &std::fs::read(tilo_relay::home_peer_registry_path(dir.path())).unwrap(),
            )
            .unwrap();
            assert_eq!(
                json[0]["device_node_id"],
                hex::encode(cert.device_public_key)
            );
            assert_eq!(json[0]["relay_service_node_id"], hex::encode([8; 32]));
            assert_eq!(json[0]["relay_workspace_ids"], serde_json::json!([2, 9]));
            assert_eq!(
                std::fs::metadata(tilo_relay::home_peer_registry_path(dir.path()))
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
            let path = tilo_relay::home_peer_registry_path(dir.path());
            tilo_files::native::atomic::write_creating_parent(&path, b"not json").unwrap();
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

    const KAT_RESPONSE_SIGNATURE: &str = "1def838ab80e77a765625a5834e34224ab4917d56075c437ee85506be214ca9d\
        81bdf95c9464eb9e86fbb89618fb094f2a6c5135435cd8e0e916458d35b19704";
    const KAT_ACK: &str = "0404040404040404040404040404040417cb79fb2b4120f2b1ec65e4198d6e08\
        b28e813feb01e4a400839b85e18080ce0aa39056fd697d25fd6d7afbe84dbb81\
        f52d9e543b4aa167cf6472c602c8079a40440274b6b4d4b42e7fd529bbbdf4f0\
        1cfc5dd55c7f0755cf6d7cc821f1cb7fdb971e77b7e4c0b1cffcadc3b3be2aef\
        423888209b7aab3703f999ab61869f950d";

    #[test]
    fn pairing_crypto_matches_known_answers() {
        // Phase `known_answer_x25519_and_ed25519_conversion`.
        {
            let a = tilo_seal::SecretKey::from_bytes([0x11; 32]);
            assert_eq!(
                hex::encode(a.public_key().unwrap().to_bytes()),
                "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13"
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
        // Phase `known_answer_response_signature_verifies`.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
            let signed = sign_response(payload(&root), &root).unwrap();
            assert_eq!(hex::encode(&signed.signature), KAT_RESPONSE_SIGNATURE);
            verify_response(&signed, root.verifying_key().to_bytes()).unwrap();
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
    fn pairing_envelopes_are_step_bound() {
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
        // Phase `pair_offers_answer_only_the_current_solicitation_magic`.
        {
            let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let mut solicitation = PairSolicitation {
                magic: PAIR_MAGIC,
                device_public_key: server.verifying_key().to_bytes(),
                relay_service_node_id: [8; 32],
                label: "NAS".into(),
                kind: EnrollmentKind::HomePeer,
            };
            assert!(valid_solicitation(&solicitation));
            let offer = encrypt_pair_offer("token", solicitation.device_public_key).unwrap();
            assert!(v2_body(&offer).is_some());
            assert_eq!(decrypt_pair_offer(&offer, &server).unwrap(), "token");

            for magic in [*b"TLOPAIR1", *b"TLOPAIR9"] {
                solicitation.magic = magic;
                assert!(!valid_solicitation(&solicitation));
            }
        }
        // Phase `version_prefix_decides_the_parser_and_tampering_fails`.
        {
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
            let (bytes, _) =
                encrypt_request(&sample_request(), issuer.verifying_key().to_bytes()).unwrap();
            // Every byte of an envelope is covered: flipping the version or any
            // sealed byte fails, and a stripped prefix is refused.
            for index in 0..bytes.len() {
                let mut tampered = bytes.clone();
                tampered[index] ^= 1;
                assert!(decrypt_request(&tampered, &issuer).is_err(), "byte {index}");
            }
            assert!(decrypt_request(&bytes[8..], &issuer).is_err());
            let wrong = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            assert!(decrypt_request(&bytes, &wrong).is_err());
        }
        // Phase `v2_envelopes_do_not_open_as_another_step`.
        {
            let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let offer = encrypt_pair_offer("token", device.verifying_key().to_bytes()).unwrap();
            // Same recipient key, different step label: the request reader
            // must refuse the pairing offer.
            assert!(decrypt_request(&offer, &device).is_err());
        }
    }
}
