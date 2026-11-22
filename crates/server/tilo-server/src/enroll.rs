//! The one enrollment every Tilo image uses, and the device side that admits
//! it. A machine enrolls with the single-use token the person made for it, or
//! by showing a short code one of the person's devices approves; a device of
//! theirs finds a machine on the local network and approves that. Which of
//! those an image offers it declares once (`tilo_identity::enrollment`).
//!
//! The requester exchange, the protocol and the approval service live in
//! [`tilo_enrollment`]. This binary handles the command line and
//! the console for a machine enrolling over iroh.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use tilo_device_identity::IdentityPaths;
use tilo_identity::certification_ceremony::pairing_code;
use tilo_identity::device_fleet::DeviceFleet;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, decode_enrollment_token, verify_device_cert_at,
};

use tilo_enrollment::approval::{
    ApprovalIdentity, ApprovalOptions, PendingApproval, SelectedNearby, accept_request,
    await_approval_ack, commit_approval, expected_pair, load_approval_identity, prepare_approval,
    received_packet,
};
use tilo_enrollment::client::PairingStatus;
use tilo_enrollment::discovery::{self, bind_pair_listener};
use tilo_enrollment::install::PersonalInstall;
use tilo_enrollment::protocol::{
    EnrollmentKind, MAX_PACKET, NEARBY_PORT, PairSolicitation, ResponsePayload, encrypt_pair_offer,
};
use tilo_enrollment::request::{JoinOptions, PairingProgress, join_with_token, pair_device};
use tilo_enrollment::root::{RootSigner, load_root};
use tilo_enrollment::transport;
use tilo_enrollment::{
    deadline_after, fleet_store, now_ns, public_identity_document, sign_root_cert, socket_timeout,
    valid_solicitation, validity_start_ns,
};

#[path = "enroll_transport.rs"]
mod join_transport;

#[derive(Parser)]
#[command(name = "tilo-enroll")]
struct Cli {
    /// The account, by uid, whose identity directory (`--identity-dir`) a
    /// root-run enrollment writes for. Without it the directory must belong
    /// to the account this runs as.
    #[arg(long, global = true)]
    person_uid: Option<u32>,
    /// The file the image declares its enrollment methods in. A machine
    /// without one offers every method.
    #[arg(long, global = true, default_value = tilo_identity::enrollment::DECLARATION)]
    methods_file: PathBuf,
    #[command(subcommand)]
    command: Command,
}

/// The methods this image declares, read once before any command is dispatched.
static METHODS: std::sync::OnceLock<tilo_identity::enrollment::Methods> =
    std::sync::OnceLock::new();

/// Every enrollment goes through the methods the image declares.
fn ensure_offered(method: tilo_identity::enrollment::Method) -> anyhow::Result<()> {
    METHODS
        .get()
        .copied()
        .unwrap_or(tilo_identity::enrollment::Methods::ALL)
        .ensure(method)
        .map_err(anyhow::Error::from)
}

/// A `--relay` value as the relays an endpoint takes as home.
fn parse_relay_option(relay: Option<&str>) -> anyhow::Result<Option<iroh::RelayMap>> {
    relay
        .map(|url| {
            url.parse::<iroh::RelayUrl>()
                .map(iroh::RelayMap::from)
                .map_err(|error| anyhow::anyhow!("{url:?} is not a packet relay: {error}"))
        })
        .transpose()
}

fn declared_methods() -> tilo_identity::enrollment::Methods {
    METHODS
        .get()
        .copied()
        .unwrap_or(tilo_identity::enrollment::Methods::ALL)
}

/// `--person-uid`, set once before any command runs.
static PERSON_UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

fn parse_method(name: &str) -> Result<tilo_identity::enrollment::Method, String> {
    tilo_identity::enrollment::Method::parse(name).ok_or_else(|| {
        format!(
            "{name:?} is not an enrollment method ({})",
            tilo_identity::enrollment::Method::ALL
                .map(tilo_identity::enrollment::Method::name)
                .join(", ")
        )
    })
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
    /// Enroll this machine: with the token it was given, or by showing a code
    /// a device of the person's approves. Both are the one enrollment every
    /// image uses; the image declares which it offers.
    Enroll {
        /// The token the person made for this machine. The image's placeholder
        /// and an absent file are no token.
        #[arg(long, default_value = "/etc/tilo/enrollment-token.txt")]
        token_file: PathBuf,
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/tilo/home-peer.toml")]
        home_peer_output: PathBuf,
        /// Where the token's nonce is written, when this machine enrolled with
        /// one: the plan it was handed is named by it.
        #[arg(long)]
        nonce_output: Option<PathBuf>,
        /// token or code; by default a token when this machine was handed one,
        /// and a code otherwise.
        #[arg(long, value_parser = parse_method)]
        method: Option<tilo_identity::enrollment::Method>,
        /// The packet relay a machine that shows a code is reached through,
        /// for one behind a network address translation.
        #[arg(long)]
        relay: Option<String>,
        /// The disk recovery key this machine installs with, root-only: a
        /// machine planned to install with nobody at its screen hands it to
        /// the person's devices with its enrollment, and keeps no copy.
        #[arg(long)]
        recovery_key_file: Option<PathBuf>,
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
        #[arg(long = "server-workspace-id")]
        server_workspace_ids: Vec<WorkspaceId>,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
        /// Serve enrollments over iroh instead of the local network: the
        /// machines that dial this device with a token.
        #[arg(long)]
        iroh: bool,
        /// The packet relay this device is reached through, for the token a
        /// machine behind a network translation is handed.
        #[arg(long)]
        relay: Option<String>,
        /// Dial a machine that shows a code (`tilo://enroll?…` as it prints
        /// it) instead of listening.
        #[arg(long)]
        dial: Option<String>,
        /// Ask the person before admitting each machine that dials this one:
        /// one JSON line per machine on standard output, then `approve <ask>`
        /// or `decline <ask>` on standard input. Serving goes on for the whole
        /// window, so a person has time to look at what wants to join.
        #[arg(long)]
        ask: bool,
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
    },
    /// Withdraw a device the root certified: sign a revocation with a root key
    /// kept in a file and append it to a server's revocation log, so that host
    /// refuses the device as it connects from its next start on.
    Revoke(RevokeArgs),
    #[command(flatten)]
    OutOfBand(OutOfBand),
}

/// Certification delivered outside a pairing session.
#[derive(Subcommand)]
enum OutOfBand {
    /// Create this device's key if it has none and print, as one JSON line,
    /// its public key and the endpoint id of a server it runs.
    DeviceKey {
        #[arg(long, default_value = "/var/lib/tilo")]
        identity_root: PathBuf,
    },
    /// Certify a device's key with a root key kept in a file, for delivery
    /// out of band. Writes the canonical certificate to OUT.
    /// Without --root-key the root this device holds signs, as when it
    /// approves a nearby device: the active identity record in
    /// --identity-dir (run as the account that owns it, in a session that
    /// reaches its secret store).
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
        /// own, admit or admin (administer the person's other devices over
        /// their device-admin route); repeatable.
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
    },
}

/// `tilo-enroll revoke`: withdraw a device a root certified.
#[derive(clap::Args)]
struct RevokeArgs {
    /// The user root signing key (32 bytes, owner-only).
    #[arg(long)]
    root_key: PathBuf,
    /// The device's public key, hex.
    #[arg(long)]
    device: String,
    /// The server's state directory, whose revocation log the host reads: the
    /// `server` directory under `tilo-server workspace --state`, or the plain
    /// daemon's configured state root.
    #[arg(long)]
    state: PathBuf,
    /// lost, stolen, retired or compromised.
    #[arg(long, default_value = "compromised")]
    reason: String,
}

fn print_discovery(timeout: u64, kind: EnrollmentKind) -> anyhow::Result<()> {
    let candidate = discovery::discover(kind, timeout, declared_methods())?;
    println!(
        "{}",
        serde_json::json!({
            "device_node_id": hex::encode(candidate.device_public_key),
            "server_node_id": hex::encode(candidate.server_node_id),
            "label": candidate.label,
            "verification_code": candidate.verification_code,
            "kind": candidate.kind,
        })
    );
    Ok(())
}

// The three sealed envelopes (pair offer, request, response) are
// `"TLOENRL" || 0x02 || tilo-seal HPKE sealed box` (RFC 9180 base mode,
// X25519-HKDF-SHA256 / ChaCha20-Poly1305), with one tilo-seal label per
// step so an envelope for one step never opens as another. Bytes without
// that prefix are refused.

/// The disk recovery key a machine installs with, read from the root-only file
/// the installer kept: one bounded line of text, never a log or a command
/// argument.
fn read_recovery_key(path: &std::path::Path) -> anyhow::Result<String> {
    const MAX: u64 = 1024;
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read {}: no such file", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX {
        bail!("{} is not a recovery key file", path.display());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let key = text.trim();
    if key.is_empty() || key.chars().any(char::is_control) {
        bail!("{} holds no recovery key", path.display());
    }
    Ok(key.to_owned())
}

/// What `tilo-enroll enroll` was asked to do: the machine it enrolls, where its
/// identity and handoff go, and the two secrets it may carry — the token's
/// nonce's destination and the disk recovery key to hand over.
#[derive(Clone, Copy)]
struct EnrollRequest<'a> {
    token_file: &'a std::path::Path,
    identity_root: &'a std::path::Path,
    label: &'a str,
    timeout: u64,
    home_peer_output: &'a std::path::Path,
    nonce_output: Option<&'a std::path::Path>,
    method: Option<tilo_identity::enrollment::Method>,
    relay: Option<&'a str>,
    recovery_key_file: Option<&'a std::path::Path>,
}

/// The one enrollment every image uses: with the token the machine was given,
/// or by showing a code a device of the person's approves.
fn enroll(request: EnrollRequest<'_>) -> anyhow::Result<()> {
    let EnrollRequest {
        token_file,
        identity_root,
        label,
        timeout,
        home_peer_output,
        nonce_output,
        method,
        relay,
        recovery_key_file,
    } = request;
    let recovery_key = recovery_key_file
        .map(read_recovery_key)
        .transpose()
        .context("read the disk recovery key to hand over")?;
    let token = tilo_identity::enrollment::given_token(token_file)
        .with_context(|| format!("read {}", token_file.display()))?;
    let method = declared_methods()
        .choose(method, token.is_some())
        .map_err(anyhow::Error::from)?;
    let options = JoinOptions {
        identity_root,
        label,
        timeout,
        kind: EnrollmentKind::HomePeer,
        home_peer_output: Some(home_peer_output),
        identity_dir: None,
        requester_lease: None,
        port: NEARBY_PORT,
        person_uid: PERSON_UID.get().copied(),
        recovery_key: recovery_key.as_deref(),
    };
    match method {
        tilo_identity::enrollment::Method::Token => {
            let token = token
                .as_deref()
                .context("this machine was handed no enrollment token")?;
            let nonce = decode_enrollment_token(token)?.nonce;
            // A token that names where the person's devices are is dialed; one
            // that names none is presented on this network.
            if decode_enrollment_token(token)?.dial.is_empty() {
                let (_, label) = join_with_token(token, options)?;
                println!("enrolled {label}");
            } else {
                join_transport::join(token, options)?;
            }
            // What this machine is to install from is known by the token's
            // nonce: it is kept before the token is spent, so the server can
            // find the plan it was handed.
            if let Some(path) = nonce_output {
                write_nonce(path, &nonce)?;
            }
            // The token is spent by the certificate it bought.
            tilo_identity::enrollment::spend_token(token_file)
                .with_context(|| format!("burn {}", token_file.display()))
        }
        tilo_identity::enrollment::Method::Code => {
            join_transport::wait_for_approval(options, parse_relay_option(relay)?.as_ref(), None)
        }
        tilo_identity::enrollment::Method::LocalNetwork => bail!(
            "a machine enrolls with a token or a code; local-network pairing is approved from \
             one of the person's devices"
        ),
    }
}

/// Write the token's `nonce` to `path`, readable by the server account only:
/// the plan this machine installs from is named by it.
fn write_nonce(path: &std::path::Path, nonce: &[u8; 16]) -> anyhow::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("make {}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(hex::encode(nonce).as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .with_context(|| format!("restrict {}", path.display()))
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
            solicitation.device_public_key != device || solicitation.server_node_id != service
        })
    {
        return Ok(());
    }
    if selected.as_ref().is_some_and(|current| {
        current.solicitation.device_public_key != solicitation.device_public_key
            || current.solicitation.server_node_id != solicitation.server_node_id
    }) {
        return Ok(());
    }
    if selected.is_none() {
        let now = now_ns()?;
        // Backdated for the joining device's clock, but never to before this
        // device was certified: redemption checks the issuer at that instant.
        let issued_at_ns = validity_start_ns(now).max(identity.issuer_cert.issued_at_ns);
        let expires_at_ns = now.saturating_add(5 * 60 * 1_000_000_000);
        let token_text = tilo_identity::enrollment::mint_token(
            identity.user_public_key,
            DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            &[],
            &identity.device,
        )?;
        let code = pairing_code(solicitation.device_public_key, solicitation.server_node_id);
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
        if let Some(pending) =
            accept_request(bytes, options, identity, selected.as_ref(), Some(sender))?
        {
            return Ok(pending);
        }
    }
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
    let finalized = commit_approval(options, &identity, &pending, &prepared)?;
    let sender = pending
        .sender
        .context("a local-network approval has a sender")?;
    for _ in 0..8 {
        session.send_to(&finalized, sender)?;
        std::thread::sleep(Duration::from_millis(125));
    }
    Ok(())
}

fn device_key(identity_root: &std::path::Path) -> anyhow::Result<()> {
    let paths = IdentityPaths::under(identity_root);
    let (device, _) = tilo_device_identity::persist::load_or_create_device_key(&paths)?;
    println!(
        "{}",
        serde_json::json!({
            "device_public_key": hex::encode(device.verifying_key().to_bytes()),
            "server_node_id": hex::encode(tilo_server::daemon::server_node_id(&device)),
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
    let mut fleet = DeviceFleet::new(hex::encode(user));
    fleet.certificates.push(cert.clone());
    let digest = blake3::hash(&tilo_user_identity::canonical::canonical_bytes(&cert)?);
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(ResponsePayload {
        nonce,
        recipient_device_public_key: cert.device_public_key,
        server_node_id: tilo_server::daemon::server_node_id(device),
        home_peer_toml: None,
        public_identity_json: Some(public_identity_document(user, "My Tilo identity")?),
        fleet_json: Some(serde_json::to_vec_pretty(&fleet)?),
        home_peers_json: Some(b"[]".to_vec()),
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
            tilo_device_identity::persist::read_owned_signing_key(path)
                .with_context(|| format!("read {}", path.display()))?,
        ),
        None => load_root(identity_dir)?,
    };
    let device_public_key: [u8; 32] = hex::decode(device.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("--device must be a 32-byte hex public key")?;
    let mut granted = DeviceCapabilities::nothing();
    for capability in capabilities {
        match capability.as_str() {
            "own" => granted.can_own_workspaces = true,
            "admit" => granted.can_admit_devices = true,
            "admin" => granted.can_administer_devices = true,
            other => {
                bail!("unknown capability {other}: expected own, admit or admin")
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
        issuer: None,
    };
    sign_root_cert(&mut cert, root.as_ref())?;
    if let Some(identity_dir) = identity_dir {
        fleet_store(identity_root, identity_dir, PERSON_UID.get().copied())
            .remember_certificate(&hex::encode(cert.user_public_key), cert.clone())?;
    }
    tilo_device_identity::persist::write_device_cert(out, &cert)?;
    Ok(())
}

/// Withdraw a device the root certified, so a host of that root refuses it as
/// it connects from its next start on.
fn revoke(options: &RevokeArgs) -> anyhow::Result<()> {
    let RevokeArgs {
        root_key,
        device,
        state,
        reason,
    } = options;
    let reason = match reason.as_str() {
        "lost" => tilo_user_identity::RevocationReason::Lost,
        "stolen" => tilo_user_identity::RevocationReason::Stolen,
        "retired" => tilo_user_identity::RevocationReason::Retired,
        "compromised" => tilo_user_identity::RevocationReason::Compromised,
        other => bail!("reason must be lost, stolen, retired or compromised, not {other}"),
    };
    let device: [u8; 32] = hex::decode(device.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("--device must be a 32-byte hex public key")?;
    let root = tilo_device_identity::persist::read_owned_signing_key(root_key)
        .with_context(|| format!("read {}", root_key.display()))?;
    let (entry, changed) = tilo_server::root_signer::revoke_device(state, &root, device, reason)?;
    let device = hex::encode(entry.revoked_device_public_key);
    if changed {
        println!(
            "revoked ({:?}) device {device} in {}",
            entry.reason,
            state.display()
        );
    } else {
        println!("already revoked device {device} in {}", state.display());
    }
    Ok(())
}

fn install_cert(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    cert_path: &std::path::Path,
) -> anyhow::Result<()> {
    let cert = tilo_device_identity::persist::read_device_cert(cert_path)
        .with_context(|| format!("read {}", cert_path.display()))?;
    verify_device_cert_at(&cert, now_ns()?).context("the certificate does not verify")?;
    let paths = IdentityPaths::under(identity_root);
    let device = tilo_device_identity::persist::read_device_key(&paths.device_privkey)
        .context("this device has no key; run device-key first")?;
    if device.verifying_key().to_bytes() != cert.device_public_key {
        bail!("the certificate names another device");
    }
    if let Ok(user) = tilo_device_identity::persist::read_public_key(&paths.user_pubkey)
        && user != cert.user_public_key
    {
        bail!("this device already belongs to another identity");
    }
    let record = PersonalInstall::new(identity_root, identity_dir, PERSON_UID.get().copied())
        .install(&out_of_band_payload(cert, &device)?, None, || Ok(()))?;
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
) -> anyhow::Result<Option<tilo_server::ProcessLease>> {
    match (headless, requester_pid, requester_start_time) {
        (true, None, None) => Ok(None),
        (true, _, _) => {
            bail!("--headless cannot be combined with --requester-pid or --requester-start-time")
        }
        (false, Some(pid), Some(start_time)) => {
            Ok(Some(tilo_server::ProcessLease::from_parts(pid, start_time)))
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
            let joined = pair_device(
                declared_methods(),
                JoinOptions {
                    identity_root,
                    label,
                    timeout,
                    kind: EnrollmentKind::PersonalDevice,
                    home_peer_output: None,
                    identity_dir: Some(identity_dir),
                    requester_lease,
                    port: NEARBY_PORT,
                    person_uid: PERSON_UID.get().copied(),
                    recovery_key: None,
                },
                |progress| match progress {
                    PairingProgress::Waiting { code, label } => {
                        if let Some(path) = status_file {
                            PairingStatus::waiting(code, label).write(path)?;
                        }
                        println!("Nearby pairing code: {code}");
                        Ok(())
                    }
                    PairingProgress::Approved { code } => {
                        println!("Nearby Tilo device approved code {code}");
                        Ok(())
                    }
                },
            )?;
            let identity = hex::encode(joined.identity);
            println!(
                "{}",
                serde_json::json!({
                    "identity": identity,
                    "resource_path": joined.record_path,
                    "label": joined.label,
                })
            );
            if let Some(path) = status_file {
                PairingStatus::complete(joined.code, identity, joined.record_path).write(path)?;
            }
            Ok(())
        });
    if let (Err(error), Some(path)) = (&result, status_file) {
        let _ = PairingStatus::failed(error).write(path);
    }
    result
}

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch: each arm names the arguments of the command it starts"
)]
fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(uid) = cli.person_uid {
        let _ = PERSON_UID.set(uid);
    }
    // The image's declaration is read before any enrollment starts, so a
    // method it does not offer is refused whatever the command line asks.
    let methods = tilo_identity::enrollment::Methods::of_image(&cli.methods_file)?;
    let _ = METHODS.set(methods);
    match cli.command {
        Command::Discover { timeout } => print_discovery(timeout, EnrollmentKind::HomePeer),
        Command::DiscoverDevice { timeout } => {
            print_discovery(timeout, EnrollmentKind::PersonalDevice)
        }
        Command::Enroll {
            token_file,
            identity_root,
            label,
            timeout,
            home_peer_output,
            nonce_output,
            method,
            relay,
            recovery_key_file,
        } => enroll(EnrollRequest {
            token_file: &token_file,
            identity_root: &identity_root,
            label: &label,
            timeout,
            home_peer_output: &home_peer_output,
            nonce_output: nonce_output.as_deref(),
            method,
            relay: relay.as_deref(),
            recovery_key_file: recovery_key_file.as_deref(),
        }),
        Command::PairDevice {
            identity_root,
            identity_dir,
            label,
            timeout,
            status_file,
            headless,
            requester_pid,
            requester_start_time,
        } => {
            ensure_offered(tilo_identity::enrollment::Method::LocalNetwork)?;
            pair_device_cli(
                &identity_root,
                &identity_dir,
                &label,
                timeout,
                status_file.as_deref(),
                headless,
                requester_pid,
                requester_start_time,
            )
        }
        Command::Approve {
            identity_root,
            identity_dir,
            timeout,
            workspace_cache,
            acl,
            server_workspace_ids,
            expected_device,
            expected_service,
            iroh,
            relay,
            dial,
            ask,
        } => {
            ensure_offered(if iroh || dial.is_some() {
                tilo_identity::enrollment::Method::Token
            } else {
                tilo_identity::enrollment::Method::LocalNetwork
            })?;
            let options = ApprovalOptions {
                identity_root: &identity_root,
                identity_dir: identity_dir.as_deref(),
                timeout,
                workspace_cache: &workspace_cache,
                acl: &acl,
                server_workspace_ids: &server_workspace_ids,
                expected_device: expected_device.as_deref(),
                expected_service: expected_service.as_deref(),
                kind: EnrollmentKind::HomePeer,
                port: NEARBY_PORT,
                ready: None,
                approved: None,
                person_uid: PERSON_UID.get().copied(),
            };
            if iroh || dial.is_some() {
                let relay = parse_relay_option(relay.as_deref())?;
                let dialed = match dial.as_deref() {
                    Some(line) => {
                        let (machine, code) = transport::scan(line)?;
                        (Some(machine), code)
                    }
                    None => (None, None),
                };
                let (machine, code) = dialed;
                transport::serve(
                    &options,
                    relay.as_ref(),
                    machine.as_ref(),
                    code.as_deref(),
                    ask,
                )
            } else {
                approve(&options)
            }
        }
        Command::ApproveDevice {
            identity_root,
            identity_dir,
            timeout,
            expected_device,
            expected_service,
        } => {
            ensure_offered(tilo_identity::enrollment::Method::LocalNetwork)?;
            approve(&ApprovalOptions {
                identity_root: &identity_root,
                identity_dir: Some(&identity_dir),
                timeout,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                server_workspace_ids: &[],
                expected_device: expected_device.as_deref(),
                expected_service: expected_service.as_deref(),
                kind: EnrollmentKind::PersonalDevice,
                port: NEARBY_PORT,
                ready: None,
                approved: None,
                person_uid: PERSON_UID.get().copied(),
            })
        }
        Command::Revoke(options) => revoke(&options),
        Command::OutOfBand(command) => out_of_band(command),
    }
}

#[cfg(test)]
mod enroll_tests;

#[cfg(test)]
#[path = "enroll_transport_tests.rs"]
mod enroll_transport_tests;
