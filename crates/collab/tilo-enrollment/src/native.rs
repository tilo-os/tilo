use crate::{protocol, root, waiting};

use anyhow::{Context, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tilo_device_identity::IdentityPaths;
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_user_identity::DeviceCert;
use tilo_user_identity::canonical::signing_digest;

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
pub const ISSUANCE_CLOCK_ALLOWANCE_NS: u64 = 60 * 1_000_000_000;

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

/// This device and the person's other devices, servers among them: each is a
/// device of theirs.
#[must_use]
pub fn personal_devices_of(
    device_public_key: [u8; 32],
    user_public_key: [u8; 32],
    fleet: &tilo_identity::device_fleet::DeviceFleet,
    now_ns: u64,
) -> Vec<[u8; 32]> {
    let mut devices = vec![device_public_key];
    devices.extend(fleet.personal_devices(&user_public_key, now_ns));
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

/// A public-only runtime identity file for `user`: what a joining device is
/// handed so its first start names the person.
pub fn public_identity_document(user: [u8; 32], display_name: &str) -> anyhow::Result<Vec<u8>> {
    Ok(tilo_identity::store::public_member_record(
        display_name,
        [88, 101, 242],
        user,
    )?)
}

/// The bytes of `path`, when it exists and is within the size a registration
/// file may have.
pub fn read_bounded(path: &std::path::Path, description: &str) -> anyhow::Result<Option<Vec<u8>>> {
    match tilo_files::native::bounded::read(path, 1024 * 1024) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
            bail!("{description} exceeds 1 MiB")
        }
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

/// A response's signature digest: the domain tag then the payload.
pub fn response_statement_digest(payload: &ResponsePayload) -> anyhow::Result<[u8; 32]> {
    signing_digest(protocol::RESPONSE_DOMAIN, payload).map_err(|error| anyhow::anyhow!("{error}"))
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
    let signature = approver.sign(&response_statement_digest(&payload)?)?;
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
            &response_statement_digest(&response.payload)?,
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
        &response_statement_digest(&response.payload)?,
        &response.signature,
    )
    .context("enrollment response was changed in transit")?;
    Ok((response.payload, issuer.device_public_key))
}

/// Open the answer to this device's enrollment request and verify the resulting certificate.
///
/// # Errors
/// The answer cannot be opened, names another request or device, or has an invalid signature
/// or certificate.
pub fn open_answer(
    bytes: &[u8],
    token: &tilo_user_identity::EnrollmentToken,
    request: &Request,
    response_secret: &tilo_seal::SecretKey,
) -> anyhow::Result<(ResponsePayload, [u8; 32])> {
    let this_device = request.device_public_key;
    let (payload, approver) =
        protocol::decrypt_response(bytes, token.nonce, this_device, response_secret)
            .and_then(|plaintext| verify_response(&plaintext, token))
            .context("the approving device's answer did not verify")?;
    let answers_this_request =
        payload.nonce == token.nonce && payload.server_node_id == request.server_node_id;
    let addressed_to_this_device = payload.recipient_device_public_key == this_device
        && payload.cert.device_public_key == this_device;
    let certifies_the_token = payload.cert.user_public_key == token.user_public_key
        && payload.cert.capabilities == token.capabilities;
    if !(answers_this_request && addressed_to_this_device && certifies_the_token) {
        bail!("enrollment answer does not match this request");
    }
    tilo_user_identity::verify_device_cert_at(&payload.cert, now_ns()?)
        .context("verify enrolled device certificate validity")?;
    Ok((payload, approver))
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
        && tilo_server_protocol::valid_device_label(&solicitation.label)
        && solicitation.hardware.len() <= 200
        && !solicitation.hardware.chars().any(char::is_control)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixture creation and assertions in unit tests"
)]
mod tests {
    use super::*;

    #[test]
    fn read_bounded_handles_missing_exact_oversized_and_unbounded_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read_bounded(&dir.path().join("missing"), "registration").expect("missing"),
            None
        );

        let exact = dir.path().join("exact");
        let payload = vec![b'a'; 1024 * 1024];
        std::fs::write(&exact, &payload).expect("exact");
        assert_eq!(
            read_bounded(&exact, "registration").expect("exact"),
            Some(payload)
        );

        let over = dir.path().join("over");
        std::fs::write(&over, vec![b'a'; 1024 * 1024 + 1]).expect("over");
        let error = read_bounded(&over, "registration").expect_err("over limit");
        assert!(error.to_string().contains("registration exceeds 1 MiB"));

        #[cfg(unix)]
        {
            let zero = std::path::Path::new("/dev/zero");
            let error = read_bounded(zero, "registration").expect_err("unbounded source");
            assert!(error.to_string().contains("registration exceeds 1 MiB"));
        }
    }
}
