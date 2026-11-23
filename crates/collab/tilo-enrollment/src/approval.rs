//! The serving side of one enrollment: what a device of the person does with
//! a machine's request — check it, redeem its token for a certificate, answer
//! it, and enter the device among the person's once it confirms.
//!
//! Nothing here trusts the connection: the request is sealed to this device's
//! key, the token is checked against this device's certificate and the fleet's
//! revocations, the nonce is spent once, and the certificate the machine gains
//! is signed by the person's own root.

use std::time::Duration;

use anyhow::{Context, bail};
use tilo_device_identity::IdentityPaths;
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_identity::enrollment::{Certifier, NonceStore, Redemption};
use tilo_identity::store::{UNNAMED_DISPLAY_NAME, is_chosen_display_name};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::Introducer;
use tilo_user_identity::{
    DeviceCert, EnrollmentScope, decode_enrollment_token, enrollment_scope, verify_device_cert,
};

use crate::protocol::{
    Approver, EnrollmentKind, PairSolicitation, Request, ResponsePayload, ResponseRoute,
    decrypt_request, encrypt_response, make_finalized,
};
use crate::{
    RootSigner, WaitingMachine, deadline_after, fleet_store, load_root, now_ns,
    remember_personal_devices, restore_owner_from_parent, sign_response, validity_start_ns,
};

/// What a device of the person answering a machine is told to do it. One
/// per enrollment kind a device answers: a machine installing a server, and
/// one of the person's own devices.
#[derive(Clone, Copy)]
pub struct ApprovalOptions<'a> {
    pub identity_root: &'a std::path::Path,
    pub identity_dir: Option<&'a std::path::Path>,
    pub timeout: u64,
    pub workspace_cache: &'a std::path::Path,
    pub acl: &'a std::path::Path,
    pub server_workspace_ids: &'a [WorkspaceId],
    pub expected_device: Option<&'a str>,
    pub expected_service: Option<&'a str>,
    pub kind: EnrollmentKind,
    /// Port the local network listener binds; 0 lets the OS choose one.
    pub port: u16,
    /// Receives the bound listener port once the approver is listening.
    pub ready: Option<&'a std::sync::mpsc::Sender<u16>>,
    /// The expected device's certificate, signed by the person's devices
    /// that hold the identity's key; `None` signs with the root held here.
    pub approved: Option<&'a DeviceCert>,
    /// The account the person's fleet belongs to, for an enrollment started as root on
    /// another's behalf.
    pub person_uid: Option<u32>,
}

/// What signs this device's side: the identity's key when it holds it, and
/// its own certified key otherwise.
pub struct ApprovalIdentity {
    /// The identity's root when this device holds it and signs itself.
    pub root: Option<Box<dyn RootSigner>>,
    pub device: ed25519_dalek::SigningKey,
    /// This device's certificate, the issuer of the tokens it mints.
    pub issuer_cert: DeviceCert,
    pub user_public_key: [u8; 32],
    pub server_workspace_ids: Vec<WorkspaceId>,
}

impl ApprovalIdentity {
    /// What signs this device's side: the identity's key when it holds it,
    /// so a device on any build can pair with it; otherwise its own
    /// certified key, as the introducer.
    #[must_use]
    pub fn approver(&self) -> Approver<'_> {
        self.root.as_deref().map_or(
            Approver::Introducer {
                device: &self.device,
                issuer: &self.issuer_cert,
            },
            Approver::Root,
        )
    }
}

/// A machine this device offered a token to, as the nearby flow selected it.
pub struct SelectedNearby {
    pub solicitation: PairSolicitation,
    pub token_text: String,
}

/// A machine's request, once its token and selection check out.
pub struct PendingApproval {
    pub request: Request,
    pub response_route: ResponseRoute,
    pub token: tilo_user_identity::EnrollmentToken,
    /// The datagram sender, when the local network carried it.
    pub sender: Option<std::net::SocketAddr>,
}

/// What the approval hands the device besides its certificate.
struct ApprovalProjection {
    public_identity: Option<Vec<u8>>,
    fleet: Option<Vec<u8>>,
}

/// The signed configuration a machine is answered with, and where to.
pub struct PreparedApproval {
    pub cert: DeviceCert,
    pub server_node_id: [u8; 32],
    pub encrypted: Vec<u8>,
}

/// The device and service a token was made for, when the caller names them.
pub fn expected_pair(
    options: &ApprovalOptions<'_>,
) -> anyhow::Result<Option<([u8; 32], [u8; 32])>> {
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
pub fn load_approval_identity(options: &ApprovalOptions<'_>) -> anyhow::Result<ApprovalIdentity> {
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
        let root = load_root(options.identity_dir)?;
        let user_public_key = root.public_key();
        (Some(root), user_public_key)
    };
    let (device, issuer_cert) =
        tilo_identity::enrollment::device_for(&paths, user_public_key, now_ns()?)
            .context("verify approving device certificate validity")?;
    if root.is_some() && !issuer_cert.capabilities.can_admit_devices {
        bail!("this device is not authorised to approve enrollment");
    }
    if let Some(directory) = options.identity_dir
        && fleet_store(options.identity_root, directory, options.person_uid)
            .load(&hex::encode(user_public_key))
            .context("verify approving device fleet")?
            .is_revoked(&issuer_cert.device_public_key)
    {
        bail!("this device is revoked and cannot approve enrollment");
    }

    Ok(ApprovalIdentity {
        root,
        device,
        issuer_cert,
        user_public_key,
        server_workspace_ids: options.server_workspace_ids.to_vec(),
    })
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
            || request.server_node_id != selection.solicitation.server_node_id
            || request.label != selection.solicitation.label
            || request.kind != selection.solicitation.kind
            || request.token != selection.token_text
    });
    let offered = match enrollment_scope(token, now_ns()?) {
        Ok(EnrollmentScope::AnyDevice) => true,
        Ok(EnrollmentScope::PersonalDevice) => kind == EnrollmentKind::PersonalDevice,
        Err(_) => false,
    };
    Ok(offered
        && token.user_public_key == identity.user_public_key
        && token.issuer_device_public_key == identity.device.verifying_key().to_bytes()
        && request.kind == kind
        && token.capabilities.can_own_workspaces
        && !wrong_selection)
}

/// The request a device sent, once its token and selection check out. The
/// approver answers whatever carried it: a datagram on the local network, or a
/// stream a machine anywhere dialled.
pub fn accept_request(
    bytes: &[u8],
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    selected: Option<&SelectedNearby>,
    sender: Option<std::net::SocketAddr>,
) -> anyhow::Result<Option<PendingApproval>> {
    let Ok((request, response_route)) = decrypt_request(bytes, &identity.device) else {
        return Ok(None);
    };
    let Ok(token) = decode_enrollment_token(request.token.trim()) else {
        return Ok(None);
    };
    Ok(
        valid_pending_request(&request, &token, options.kind, identity, selected)?.then_some(
            PendingApproval {
                request,
                response_route,
                token,
                sender,
            },
        ),
    )
}

/// What the approval hands the device besides its certificate. The personal
/// device's fleet is filled in once the token is redeemed, because the fleet
/// then lists the new certificate.
fn approval_projection(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
) -> anyhow::Result<ApprovalProjection> {
    match options.kind {
        EnrollmentKind::HomePeer => Ok(ApprovalProjection {
            public_identity: None,
            fleet: None,
        }),
        EnrollmentKind::PersonalDevice => {
            let directory = options
                .identity_dir
                .context("identity directory is required for personal-device approval")?;
            let name =
                personal_display_name(directory, options.person_uid, identity.user_public_key);
            Ok(ApprovalProjection {
                public_identity: Some(crate::public_identity_document(
                    identity.user_public_key,
                    name.as_deref().unwrap_or(UNNAMED_DISPLAY_NAME),
                )?),
                fleet: None,
            })
        }
    }
}

/// Read the current name at approval time, for the same person the certificate names.
fn personal_display_name(
    directory: &std::path::Path,
    person_uid: Option<u32>,
    user_public_key: [u8; 32],
) -> Option<String> {
    use tilo_identity::person_files::as_person;
    use tilo_identity::provisioning::is_throwaway_test_display_name;
    use tilo_identity::store::resolve_active_identity;
    use tilo_identity_client::load_metadata_from_path;

    as_person(person_uid, directory, || {
        let name = resolve_active_identity(directory).and_then(|record| {
            let metadata = load_metadata_from_path(&record).ok()?;
            let name = &metadata.display_name;
            if metadata.group_public_key != user_public_key
                || !is_chosen_display_name(name)
                || is_throwaway_test_display_name(&record, name)
            {
                return None;
            }
            tilo_user_identity::display_name(name)
                .ok()
                .map(str::to_owned)
        });
        Ok(name)
    })
    .ok()
    .flatten()
}

/// Redeem the request's token and sign the machine's configuration: what the
/// machine is answered with, before it has confirmed anything.
pub fn prepare_approval(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    pending: &PendingApproval,
) -> anyhow::Result<PreparedApproval> {
    // Every refusal happens before the token is redeemed: a refusal
    // after it would spend the token on a device that never hears back.
    let mut projection = approval_projection(options, identity)?;
    // Whichever kind of device joined, it is a device of the person's: its
    // certificate enters the person's own store when its token is redeemed, as
    // every device's does, and it is handed that store, which lists every
    // device they certified. A server that received no session of theirs
    // learns the others from it alone.
    // Every approval reaching here names its identity directory: the root
    // that signs a server's certificate is opened from it, and a personal
    // device's name is read from it.
    let store = options
        .identity_dir
        .map(|identity_dir| fleet_store(options.identity_root, identity_dir, options.person_uid))
        .context(
            "this device holds no identity directory to hand the machine the person's devices from",
        )?;
    let cert = redeem_pending(options, identity, pending, &store)?;
    projection.fleet = Some(
        store
            .transferred(&hex::encode(cert.user_public_key), &cert)
            .context("hand the joining device this identity's fleet")?,
    );
    let signed = sign_response(
        ResponsePayload {
            nonce: pending.token.nonce,
            recipient_device_public_key: pending.request.device_public_key,
            server_node_id: pending.request.server_node_id,
            cert: cert.clone(),
            public_identity_json: projection.public_identity,
            fleet_json: projection.fleet,
            // Whichever kind joins, it asks this device, one of the person's,
            // into their workspace.
            introducer: Some(Introducer::of_device(
                identity.issuer_cert.clone(),
                &identity.device,
            )),
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
        server_node_id: pending.request.server_node_id,
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
    fleet: &DeviceFleetStore,
) -> anyhow::Result<DeviceCert> {
    let now = now_ns()?;
    let sign_here = |digest: &[u8]| {
        let digest: &[u8; 32] = digest
            .try_into()
            .map_err(|_| std::io::Error::other("a certificate digest is 32 bytes"))?;
        identity
            .root
            .as_ref()
            .ok_or_else(|| std::io::Error::other("this device holds no identity root"))?
            .sign_digest(digest)
            .map_err(|error| std::io::Error::other(error.to_string()))
    };
    tilo_identity::enrollment::redeem(
        Some(fleet),
        &NonceStore::under(options.identity_root),
        &Redemption {
            token: &pending.token,
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

/// Enter the admitted device among the person's and answer with the Finalized
/// the machine waits for.
pub fn commit_approval(
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    pending: &PendingApproval,
    prepared: &PreparedApproval,
) -> anyhow::Result<Vec<u8>> {
    // The person's devices have listed it since its token was redeemed. The
    // device acknowledged the certificate this device just issued under the
    // identity's root: both are now this person's devices, a server as much
    // as any other.
    let personal_peers = remember_personal_devices(
        options.identity_root,
        &[
            identity.device.verifying_key().to_bytes(),
            prepared.cert.device_public_key,
        ],
    )?;
    restore_owner_from_parent(&personal_peers)?;
    tracing::info!(
        device = %hex::encode(prepared.cert.device_public_key),
        "approved device"
    );
    // The Finalized the machine waits for, for the caller to carry: eight
    // datagrams on the local network, one stream over iroh.
    make_finalized(
        &prepared.encrypted,
        pending.token.nonce,
        pending.request.device_public_key,
        identity.approver(),
    )
}

/// The ask a machine waiting to join is put to the person as.
#[must_use]
pub fn waiting_of(pending: &PendingApproval) -> WaitingMachine {
    WaitingMachine::of(
        pending.request.device_public_key,
        pending.request.server_node_id,
        &pending.request.label,
        &pending.request.hardware,
    )
    .with_nonce(pending.token.nonce)
}

/// Wait for a device to acknowledge its signed configuration on a local
/// network datagram: bounded by the window, not by a fixed allowance a slow
/// disk on the device could exceed.
pub fn await_approval_ack(
    socket: &std::net::UdpSocket,
    packet: &mut [u8],
    pending: &PendingApproval,
    prepared: &PreparedApproval,
    window: Duration,
) -> anyhow::Result<()> {
    let sender = pending
        .sender
        .context("a local-network approval has a sender")?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let deadline = deadline_after(window)?;
    while std::time::Instant::now() < deadline {
        socket.send_to(&prepared.encrypted, sender)?;
        match socket.recv_from(packet) {
            Ok((len, ack_sender)) if Some(ack_sender) == pending.sender => {
                if crate::protocol::verify_ack(
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

/// The `received` prefix of `buffer`, as a datagram's bytes.
pub fn received_packet(buffer: &[u8], received: usize) -> anyhow::Result<&[u8]> {
    buffer
        .get(..received)
        .context("socket reported a packet larger than its receive buffer")
}
