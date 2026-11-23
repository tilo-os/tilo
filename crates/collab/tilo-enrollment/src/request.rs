//! Native requester exchange. Solicitation, the authenticated answer and
//! Finalized all use the same device key and the same requester lease.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use tilo_device_identity::IdentityPaths;
use tilo_identity::certification_ceremony::pairing_code;
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_identity::enrollment::NonceStore;
use tilo_user_identity::{decode_enrollment_token, verify_enrollment_token};

use crate::approval::received_packet;
use crate::install::{PersonalInstall, validate_requester};
use crate::protocol::{
    EnrollmentKind, MAX_PACKET, PAIR_MAGIC, PairSolicitation, Request, ResponsePayload,
    decrypt_pair_offer, encrypt_request, make_ack, verify_finalized,
};
use crate::{
    WindowClosed, deadline_after, hardware_summary, now_ns, open_answer, restore_owner_from_parent,
    share_directory_like_parent, socket_timeout,
};

/// Send `bytes` to `destination`, which this machine's own broadcast address
/// takes when the kernel refuses the LAN broadcast: a sandboxed network
/// namespace has no LAN, and the person's own surfaces on this machine listen
/// on the same port.
fn send_solicitation(
    socket: &UdpSocket,
    bytes: &[u8],
    destination: &mut SocketAddrV4,
    port: u16,
) -> std::io::Result<()> {
    match socket.send_to(bytes, *destination) {
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && *destination.ip() == Ipv4Addr::BROADCAST =>
        {
            *destination = SocketAddrV4::new(Ipv4Addr::new(127, 255, 255, 255), port);
            socket.send_to(bytes, *destination).map(|_| ())
        }
        result => result.map(|_| ()),
    }
}

/// What an enrollment request is made of, before any transport carries it:
/// the token it presents, the device it presents from, and where its answer
/// must be sealed.
pub struct PreparedJoin {
    pub token: tilo_user_identity::EnrollmentToken,
    pub request: Request,
    pub request_bytes: Vec<u8>,
    pub response_secret: tilo_seal::SecretKey,
    pub device: ed25519_dalek::SigningKey,
    paths: IdentityPaths,
}

/// Check the token, make this machine's key, and seal the request that asks
/// for its certificate. Over the local network and over iroh alike.
///
/// # Errors
/// The token, requester, device key or request encoding is invalid.
pub fn prepare_join(token_text: &str, options: JoinOptions<'_>) -> anyhow::Result<PreparedJoin> {
    let JoinOptions {
        identity_root,
        label,
        kind,
        identity_dir,
        requester_lease,
        person_uid,
        ..
    } = options;
    validate_requester(requester_lease)?;
    let token = decode_enrollment_token(token_text).context("decode enrollment token")?;
    verify_enrollment_token(&token, now_ns()?).context("verify enrollment token")?;
    if !token.capabilities.can_own_workspaces {
        bail!("enrollment token does not admit a device");
    }
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    if kind == EnrollmentKind::PersonalDevice {
        PersonalInstall::new(
            identity_root,
            identity_dir.context("identity directory is required")?,
            person_uid,
        )
        .recover(requester_lease)?;
    }
    let (device, _) = tilo_device_identity::persist::load_or_create_device_key(&paths)?;
    let request = Request {
        token: token_text.to_owned(),
        device_public_key: device.verifying_key().to_bytes(),
        server_node_id: tilo_server_protocol::server_node_id(&device.to_bytes()),
        label: label.chars().take(80).collect(),
        hardware: hardware_summary(),
        kind,
    };
    let (request_bytes, response_secret) =
        encrypt_request(&request, token.issuer_device_public_key)?;
    Ok(PreparedJoin {
        token,
        request,
        request_bytes,
        response_secret,
        device,
        paths,
    })
}

/// What this machine installs once its signed answer arrived, whatever carried
/// it: the home-peer configuration, its staged personal enrollment, and — once
/// `confirm` has sent the acknowledgement and checked the approver's Finalized —
/// its certificate. The personal commit point stays the durable marker this
/// stages; a crash before it publishes nothing.
///
/// # Errors
/// The response could not be confirmed or installed.
#[allow(
    clippy::too_many_lines,
    reason = "one ordered install: every step depends on the certificate the answer carried"
)]
pub fn install_answer(
    prepared: &PreparedJoin,
    options: JoinOptions<'_>,
    payload: &ResponsePayload,
    response_bytes: &[u8],
    confirm: &mut dyn FnMut(&[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<Option<PathBuf>> {
    let PreparedJoin {
        token,
        device,
        paths,
        ..
    } = prepared;
    let JoinOptions {
        identity_root,
        kind,
        identity_dir,
        requester_lease,
        person_uid,
        ..
    } = options;
    // The device this one asks into the person's workspace, once its service
    // starts: one of the person's.
    if let Some(introducer) = &payload.introducer {
        introducer
            .checked(&token.user_public_key)
            .map_err(anyhow::Error::msg)
            .context("approval named no device of the person's to ask in")?;
    }
    if kind == EnrollmentKind::HomePeer {
        // The person's own store, kept here under the identity this machine
        // was enrolled into: it lists every device they certified, which is
        // how a machine with no session of theirs finds the devices it may
        // ask.
        let identity_id = hex::encode(token.user_public_key);
        let store = DeviceFleetStore::of_device(paths.clone(), &identity_id);
        let fleet = store
            .accept_transferred(
                &identity_id,
                payload
                    .fleet_json
                    .as_deref()
                    .context("approving device omitted the person's devices")?,
                payload.cert.device_public_key,
            )
            .context("approval returned an invalid device fleet")?;
        share_directory_like_parent(&{
            let fleets = store.fleet_file(&identity_id);
            fleets
                .parent()
                .context("the device fleet has no directory")?
                .to_path_buf()
        })?;
        store
            .adopt_transferred(&fleet)
            .context("install the person's devices")?;
    }
    let mut confirm_answer = || {
        let ack = make_ack(response_bytes, token.nonce, device)?;
        confirm(&ack)
    };
    let resource_path = if kind == EnrollmentKind::PersonalDevice {
        Some(
            PersonalInstall::new(
                identity_root,
                identity_dir.context("identity directory is required")?,
                person_uid,
            )
            .install(payload, requester_lease, confirm_answer)?,
        )
    } else {
        confirm_answer()?;
        validate_requester(requester_lease)?;
        // The home-peer path has no multi-file member projection, but still waits
        // for Finalized before changing its active identity binding.
        tilo_device_identity::persist::write_public_key(
            &paths.user_pubkey,
            &payload.cert.user_public_key,
        )?;
        None
    };
    tilo_server_protocol::servers::keep_introducer(identity_root, payload.introducer.as_ref())?;
    // Publish the default cert as the completion marker only after the approver
    // durably registers us, authenticates Finalized, and the member projection
    // is fully installed. A crash can therefore only leave a retryable partial
    // enrollment, never a bootable one.
    if kind == EnrollmentKind::HomePeer {
        let per_user_cert = paths.device_cert_for_user(&payload.cert.user_public_key);
        share_directory_like_parent(
            per_user_cert
                .parent()
                .context("per-user certificate has no directory")?,
        )?;
        tilo_device_identity::persist::write_device_cert(&per_user_cert, &payload.cert)?;
        tilo_device_identity::persist::write_device_cert(&paths.device_cert, &payload.cert)?;
    }
    // The identity directories themselves are image layout and keep their owner
    // and group; only what this enrollment created below them takes its
    // directory's.
    let per_user_cert = paths.device_cert_for_user(&payload.cert.user_public_key);
    let home_peer_fleet = (kind == EnrollmentKind::HomePeer).then(|| {
        DeviceFleetStore::of_device(paths.clone(), &hex::encode(token.user_public_key))
            .fleet_file(&hex::encode(token.user_public_key))
    });
    let introducer = tilo_server_protocol::servers::introducer_path(identity_root);
    for output in [
        payload.introducer.is_some().then_some(introducer.as_path()),
        per_user_cert.parent(),
        Some(per_user_cert.as_path()),
        Some(paths.user_pubkey.as_path()),
        Some(paths.device_privkey.as_path()),
        Some(paths.device_cert.as_path()),
        home_peer_fleet.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        restore_owner_from_parent(output)?;
    }
    NonceStore::under(identity_root).record(&token.nonce, device.verifying_key().to_bytes())?;
    Ok(resource_path)
}

/// Join with `token_text` over the local network; a personal device gets back
/// the identity it joined.
///
/// # Errors
/// The exchange timed out or the answer could not be installed.
#[allow(
    clippy::too_many_lines,
    reason = "one ordered join exchange; each step relies on the token and identity checks before it"
)]
pub fn join_with_token(
    token_text: &str,
    options: JoinOptions<'_>,
) -> anyhow::Result<(Option<PathBuf>, String)> {
    let JoinOptions {
        timeout,
        requester_lease,
        port,
        ..
    } = options;
    let prepared = prepare_join(token_text, options)?;
    let PreparedJoin {
        token,
        request,
        request_bytes,
        response_secret,
        device,
        ..
    } = &prepared;
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = deadline_after(socket_timeout(timeout))?;
    let mut destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);
    let mut reply = vec![0_u8; MAX_PACKET];
    while tilo_clock::now() < deadline {
        validate_requester(requester_lease)?;
        send_solicitation(&socket, request_bytes, &mut destination, port)?;
        match socket.recv_from(&mut reply) {
            Ok((len, sender)) => {
                let response_bytes = received_packet(&reply, len)?.to_vec();
                validate_requester(requester_lease)?;
                let Ok((payload, approver_public_key)) =
                    open_answer(&response_bytes, token, request, response_secret)
                else {
                    continue;
                };
                let mut confirm = |ack: &[u8]| -> anyhow::Result<()> {
                    socket.set_read_timeout(Some(Duration::from_millis(300)))?;
                    // The approver commits (durable writes) before it sends
                    // Finalized, so its storage latency, not a fixed allowance,
                    // bounds this wait; the enrollment window is the budget.
                    let completion_deadline = deadline_after(socket_timeout(timeout))?;
                    while tilo_clock::now() < completion_deadline {
                        validate_requester(requester_lease)?;
                        socket.send_to(ack, sender)?;
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
                    bail!("approving device did not confirm enrollment; retrying is safe")
                };
                let record =
                    install_answer(&prepared, options, &payload, &response_bytes, &mut confirm)?;
                return Ok((record, payload.cert.device_label));
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

/// Progress before the signed answer is installed.
pub enum PairingProgress<'a> {
    Waiting { code: &'a str, label: &'a str },
    Approved { code: &'a str },
}

/// The personal identity installed by a confirmed nearby enrollment.
pub struct JoinedDevice {
    pub code: String,
    pub identity: [u8; 32],
    pub record_path: PathBuf,
    pub label: String,
}

/// Solicit a nearby approval and install the confirmed personal enrollment.
///
/// # Errors
/// The image disallows the method, the requester expired, the window closed,
/// or the verified answer could not be installed or reported.
pub fn pair_device(
    methods: tilo_identity::enrollment::Methods,
    options: JoinOptions<'_>,
    mut progress: impl FnMut(PairingProgress<'_>) -> anyhow::Result<()>,
) -> anyhow::Result<JoinedDevice> {
    methods.ensure(tilo_identity::enrollment::Method::LocalNetwork)?;
    let options = JoinOptions {
        kind: EnrollmentKind::PersonalDevice,
        ..options
    };
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
    let (device, _) = tilo_device_identity::persist::load_or_create_device_key(&paths)?;
    let server_node_id = tilo_server_protocol::server_node_id(&device.to_bytes());
    let label: String = label.chars().take(80).collect();
    if !tilo_server_protocol::valid_device_label(&label) {
        bail!("home-server label must be a printable single-line name");
    }
    let solicitation = postcard::to_stdvec(&PairSolicitation {
        magic: PAIR_MAGIC,
        device_public_key: device.verifying_key().to_bytes(),
        server_node_id,
        label: label.clone(),
        hardware: hardware_summary(),
        kind,
    })?;
    let code = pairing_code(device.verifying_key().to_bytes(), server_node_id);
    progress(PairingProgress::Waiting {
        code: &code,
        label: &label,
    })?;
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = deadline_after(socket_timeout(timeout))?;
    let mut destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);
    let mut reply = vec![0_u8; MAX_PACKET];
    while tilo_clock::now() < deadline {
        validate_requester(requester_lease)?;
        send_solicitation(&socket, &solicitation, &mut destination, port)?;
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
                    || !token.capabilities.can_own_workspaces
                {
                    continue;
                }
                progress(PairingProgress::Approved { code: &code })?;
                let (record_path, label) = join_with_token(
                    token_text.trim(),
                    JoinOptions {
                        label: &label,
                        ..options
                    },
                )?;
                return Ok(JoinedDevice {
                    code,
                    identity: token.user_public_key,
                    record_path: record_path.context("record path is required")?,
                    label,
                });
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

/// How one requester-side enrollment runs: where its identity lives, the
/// label it asks for, the window, the device role, where a home peer's
/// descriptor goes, the requester lease that must stay live and the
/// nearby port.
#[derive(Clone, Copy)]
pub struct JoinOptions<'a> {
    pub identity_root: &'a std::path::Path,
    pub label: &'a str,
    pub timeout: u64,
    pub kind: EnrollmentKind,
    pub identity_dir: Option<&'a std::path::Path>,
    pub requester_lease: Option<tilo_server_protocol::ProcessLease>,
    pub port: u16,
    pub person_uid: Option<u32>,
}
