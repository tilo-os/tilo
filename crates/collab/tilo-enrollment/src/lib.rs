//! The one enrollment every Tilo image uses: a machine joins the person's
//! identity with the single-use token they made for it, or by showing a short
//! code one of their devices approves.
//!
//! This crate is the protocol both ends speak — the messages, their sealing,
//! the answer that admits a machine and the certificate it gains — and the
//! serving side: what a device of the person's does when a machine dials it
//! over the internet, whoever is there to answer. The image's helper
//! (`tilo-enroll`) and the person's session both call it, so an enrollment is
//! one implementation, not one per surface.
//!
//! Which methods an image offers is its own declaration
//! ([`tilo_identity::enrollment`]); the enrollment itself is the same.

pub mod approval;
pub mod protocol;
pub mod root;
pub mod transport;
pub mod waiting;

use anyhow::{Context, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_server_protocol::HomePeerRegistration;
use tilo_user_identity::{DeviceCert, IdentityPaths};

pub use protocol::{
    Ack, AckPayload, Approver, EnrollmentKind, Finalized, FinalizedPayload, IntroducedResponse,
    MAX_PACKET, NEARBY_PORT, PAIR_DOMAIN, PAIR_MAGIC, PairSolicitation, Request, RequestV2,
    RequestV2Owned, Response, ResponsePayload, ResponseRoute,
};
pub use root::{RootSigner, load_root};
pub use waiting::{Console, Decision, Person, WaitingMachine, hardware_summary};

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

/// What the clock says, for what an enrollment issues and answers.
pub fn now_ns() -> anyhow::Result<u64> {
    tilo_clock::try_unix_now_ns().context("system clock precedes the Unix epoch")
}

/// The not-before for something issued at `now_ns`.
#[must_use]
pub fn validity_start_ns(now_ns: u64) -> u64 {
    now_ns.saturating_sub(ISSUANCE_CLOCK_ALLOWANCE_NS)
}

/// How long one side waits for the other, bounded to a window a person can sit
/// through.
#[must_use]
pub fn socket_timeout(seconds: u64) -> std::time::Duration {
    std::time::Duration::from_secs(seconds.clamp(1, 300))
}

/// The instant `duration` from now.
pub fn deadline_after(duration: std::time::Duration) -> anyhow::Result<std::time::Instant> {
    std::time::Instant::now()
        .checked_add(duration)
        .context("enrollment timeout exceeds the platform clock range")
}

/// The person's device fleet: the canonical list of their devices and
/// revocations, kept in their identity directory. `person_uid` names the
/// account the fleet belongs to for an enrollment started as root on another's behalf.
#[must_use]
pub fn fleet_store(
    identity_root: &std::path::Path,
    identity_dir: &std::path::Path,
    person_uid: Option<u32>,
) -> DeviceFleetStore {
    let store = DeviceFleetStore::at(identity_dir, IdentityPaths::under(identity_root));
    match person_uid {
        Some(uid) => store.for_person(uid),
        None => store,
    }
}

/// This device and the person's other personal devices, leaving out a device
/// the home-peer registry names: a home peer is reached through that registry,
/// not as one of the person's personal devices.
#[must_use]
pub fn personal_devices_of(
    device_public_key: [u8; 32],
    user_public_key: [u8; 32],
    fleet: &tilo_identity::device_fleet::DeviceFleet,
    home_peers: &[HomePeerRegistration],
    now_ns: u64,
) -> Vec<[u8; 32]> {
    let home_peers: Vec<[u8; 32]> = home_peers
        .iter()
        .filter_map(|peer| hex::decode(&peer.device_node_id).ok()?.try_into().ok())
        .collect();
    let mut devices = vec![device_public_key];
    devices.extend(
        fleet
            .personal_devices(&user_public_key, now_ns)
            .into_iter()
            .filter(|device| !home_peers.contains(device)),
    );
    devices
}

/// Add `devices` to this device's personal device list, the authorization
/// input that decides which devices remote actions may address. Repeatable:
/// a device already listed is kept once.
pub fn remember_personal_devices(
    identity_root: &std::path::Path,
    devices: &[[u8; 32]],
) -> anyhow::Result<std::path::PathBuf> {
    let path = tilo_server_protocol::personal_device_peer_path(identity_root);
    for device in devices {
        tilo_server_protocol::remember_personal_device(&path, *device)
            .context("enter the paired device as a personal device")?;
    }
    Ok(path)
}

/// Give one file or directory this enrollment created the owner and group of
/// the directory it was created in.
///
/// Only an enrollment started as root (a headless host) creates output under another
/// account than its directory's; desktop first use works as the session user,
/// whose output already belongs to them and inherits the device-trust group
/// from the setgid identity directories. Ownership always comes from the
/// immediate parent, never from the identity root: the root is `root:root`,
/// while the directories below it belong to the device-trust group that the
/// session and relay read the device identity through.
pub fn restore_owner_from_parent(path: &std::path::Path) -> anyhow::Result<()> {
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
pub fn share_directory_like_parent(directory: &std::path::Path) -> anyhow::Result<()> {
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

/// This identity's home peers, checked, as an approval hands them to a
/// joining device: the registry holds every identity's routes.
pub fn bounded_home_peer_registry(
    identity_root: &std::path::Path,
    user: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let peers = tilo_server_protocol::read_home_peers(
        &tilo_server_protocol::home_peer_registry_path(identity_root),
    )?;
    Ok(serde_json::to_vec_pretty(
        &peers
            .into_iter()
            .filter(|entry| entry.user_public_key == hex::encode(user))
            .collect::<Vec<_>>(),
    )?)
}

/// A public-only runtime identity file for `user`: what a joining device is
/// handed so its first start names the person.
pub fn public_identity_document(user: [u8; 32], display_name: &str) -> anyhow::Result<Vec<u8>> {
    Ok(runtime_identity::public_member_record(
        display_name,
        [88, 101, 242],
        user,
    )?)
}

/// The bytes of `path`, when it exists and is within the size a registration
/// file may have.
pub fn read_bounded(path: &std::path::Path, description: &str) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => Ok(Some(bytes)),
        Ok(_) => bail!("{description} exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// [`read_bounded`] as the JSON it holds.
pub fn read_bounded_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    description: &str,
) -> anyhow::Result<Option<T>> {
    read_bounded(path, description)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).with_context(|| format!("{description} is invalid"))
        })
        .transpose()
}

/// The preimage of a response's signature: the domain tag then the payload.
pub fn response_preimage(payload: &ResponsePayload) -> anyhow::Result<Vec<u8>> {
    let mut preimage = protocol::RESPONSE_DOMAIN.to_vec();
    preimage.extend(postcard::to_stdvec(payload)?);
    Ok(preimage)
}

/// Sign a device certificate using the authenticated person root.
pub fn sign_root_cert(cert: &mut DeviceCert, root: &dyn RootSigner) -> anyhow::Result<()> {
    tilo_user_identity::sign_device_cert(cert, root)
        .context("sign the device certificate with the person's root")?;
    tilo_user_identity::verify_device_cert(cert)
        .context("signed device certificate does not verify")?;
    Ok(())
}

/// `payload` signed by `approver`, encoded: a [`Response`] from the
/// identity's key, an [`IntroducedResponse`] from an introducer.
pub fn sign_response(
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

/// Check `signature` under `public_key` over `message`.
pub fn verify_signature(
    public_key: [u8; 32],
    message: &[u8],
    signature: &[u8],
) -> anyhow::Result<()> {
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature has the wrong length"))?;
    VerifyingKey::from_bytes(&public_key)?.verify(message, &Signature::from_bytes(&signature))?;
    Ok(())
}

/// The payload of the encoded response `bytes` and the key that signed it:
/// the identity's key of `token`, or the device that minted `token` while it
/// was certified by that identity.
pub fn verify_response(
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
    tilo_user_identity::verify_device_cert_at(issuer, token.issued_at_ns)
        .context("the approving device is not certified by the identity")?;
    verify_signature(
        issuer.device_public_key,
        &response_preimage(&response.payload)?,
        &response.signature,
    )
    .context("enrollment response was changed in transit")?;
    Ok((response.payload, issuer.device_public_key))
}

/// A window in which no machine or device answered.
#[derive(Debug)]
pub struct WindowClosed(pub &'static str);

impl std::fmt::Display for WindowClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for WindowClosed {}

/// Whether `solicitation` is one this side answers: a well-formed device
/// announcing itself, not a flood of arbitrary bytes.
#[must_use]
pub fn valid_solicitation(solicitation: &PairSolicitation) -> bool {
    solicitation.magic == PAIR_MAGIC
        && VerifyingKey::from_bytes(&solicitation.device_public_key).is_ok()
        && solicitation.server_node_id != [0; 32]
        && tilo_server_protocol::valid_home_peer_label(&solicitation.label)
        && solicitation.hardware.len() <= 200
        && !solicitation.hardware.chars().any(char::is_control)
}
