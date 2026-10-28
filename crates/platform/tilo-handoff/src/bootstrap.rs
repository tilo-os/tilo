//! Canonical identity bootstrap for a destination that has no person yet.
//!
//! A medium prepared for another computer names the person the package
//! belongs to. That person's preparing computer holds the root signing
//! capability in its prepared-identity vault. A fresh destination proves its
//! own new device key with the canonical [`CertificationRequest`] and asks
//! the preparing computer to admit exactly that key. The person approves the
//! six-digit code on the preparing computer; only then does it sign a member
//! certificate and an identity export, bound by a typed enrollment receipt to
//! the exact device key, handoff operation and package digest. The
//! destination verifies that receipt before it writes any identity custody.
//!
//! This is the same request, root signature and public member record the
//! nearby personal-device enrollment uses. The root never leaves the source,
//! the destination never self-signs, and nothing is trusted because a display
//! name matches.

use super::invalid;
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use std::{io, os::unix::fs::PermissionsExt as _, path::Path, sync::Arc};
use tilo_seal::Label;
use tilo_user_identity::{
    CertificationRequest, DeviceCapabilities, DeviceCert, IdentityPaths,
    device_cert_signing_digest, verify_device_cert_at,
};

const PROJECTION_SCHEMA: u32 = 1;
const MAX_PROJECTION: u64 = 1024 * 1024;
const MAX_HOME_PEERS: usize = 128;
const ISSUANCE_CLOCK_ALLOWANCE_NS: u64 = 60 * 1_000_000_000;
const RECORD_ID_HEX_CHARS: usize = 55;

/// Signs one canonical digest with the person's root. The prepared-identity
/// vault remains the only holder of the root secret.
type SourceSigner = Arc<dyn Fn(&[u8]) -> io::Result<[u8; 64]> + Send + Sync>;

/// The preparing computer's root capability and the public identity it
/// exports. The signer is a callback so the prepared-identity vault remains
/// the only holder of the root secret.
pub struct SourceIdentity {
    pub identity_public_key: [u8; 32],
    /// Canonical public member record for this person.
    pub identity_record: Vec<u8>,
    /// Certificates already in the person's fleet. The newly admitted device
    /// is added to this set before export.
    pub certificates: Vec<DeviceCert>,
    /// The person's home-peer registry as JSON, filtered to this identity.
    pub home_peers: Vec<u8>,
    pub signer: SourceSigner,
}

impl SourceIdentity {
    /// Authenticate the public export before an endpoint advertises it.
    ///
    /// # Errors
    ///
    /// Refuses an empty or oversized record, a record for another person, a
    /// certificate outside the person's fleet, or an invalid home-peer list.
    pub fn valid(&self) -> io::Result<()> {
        if self.identity_public_key == [0; 32]
            || self.identity_record.is_empty()
            || self.identity_record.len() as u64 > MAX_PROJECTION
        {
            return Err(invalid("The preparing identity export is invalid"));
        }
        let metadata = runtime_identity::load_metadata_from_bytes(&self.identity_record)
            .map_err(|_| invalid("The preparing identity export is invalid"))?;
        if metadata.group_public_key != self.identity_public_key
            || self.certificates.len() > 256
            || self.certificates.iter().any(|cert| {
                cert.user_public_key != self.identity_public_key
                    || verify_device_cert_at(cert, now_ns()).is_err()
            })
        {
            return Err(invalid("The preparing identity export is invalid"));
        }
        if !self.home_peers.is_empty() {
            home_peer_array(&self.home_peers)?;
        }
        Ok(())
    }
}

/// One signed proof of possession for the bootstrap rendezvous. It binds the
/// destination's new device key and ephemeral key to the exact operation and
/// package, so a relay cannot substitute another device or transfer.
#[derive(Serialize)]
struct BootstrapProof<'a> {
    tag: &'static str,
    operation_id: &'a str,
    reply_ticket: &'a str,
    identity_public_key: [u8; 32],
    package_sha256: &'a str,
    expires_at_ns: u64,
    request_digest: [u8; 32],
    device_public_key: [u8; 32],
    ephemeral_public_key: [u8; 32],
}

pub(crate) fn proof_bytes(
    operation_id: &str,
    reply_ticket: &str,
    identity_public_key: [u8; 32],
    package_sha256: &str,
    expires_at_ns: u64,
    request_digest: [u8; 32],
    device_public_key: [u8; 32],
    ephemeral_public_key: [u8; 32],
) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&BootstrapProof {
        tag: "tilo-migration-handoff-bootstrap-v1",
        operation_id,
        reply_ticket,
        identity_public_key,
        package_sha256,
        expires_at_ns,
        request_digest,
        device_public_key,
        ephemeral_public_key,
    })
    .map_err(|_| invalid("Bootstrap proof is invalid"))
}

/// The person approved this exact device for this exact handoff. The source
/// signs it with the person's root; the destination refuses to install any
/// certificate or identity export without it.
#[derive(Serialize)]
struct EnrollmentReceipt<'a> {
    tag: &'static str,
    operation_id: &'a str,
    package_sha256: &'a str,
    identity_public_key: [u8; 32],
    device_public_key: [u8; 32],
    ephemeral_public_key: [u8; 32],
    request_digest: [u8; 32],
    projection_sha256: [u8; 32],
}

pub(crate) fn receipt_bytes(
    operation_id: &str,
    package_sha256: &str,
    identity_public_key: [u8; 32],
    device_public_key: [u8; 32],
    ephemeral_public_key: [u8; 32],
    request_digest: [u8; 32],
    projection_sha256: [u8; 32],
) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&EnrollmentReceipt {
        tag: "tilo-migration-handoff-enrollment-receipt-v1",
        operation_id,
        package_sha256,
        identity_public_key,
        device_public_key,
        ephemeral_public_key,
        request_digest,
        projection_sha256,
    })
    .map_err(|_| invalid("Enrollment receipt is invalid"))
}

pub(crate) fn verify_receipt(
    receipt: &[u8],
    identity_public_key: &[u8; 32],
    operation_id: &str,
    package_sha256: &str,
    device_public_key: &[u8; 32],
    ephemeral_public_key: &[u8; 32],
    request_digest: &[u8; 32],
    projection_sha256: &[u8; 32],
) -> bool {
    if receipt.len() != 64 {
        return false;
    }
    let Ok(verifying) = ed25519_dalek::VerifyingKey::from_bytes(identity_public_key) else {
        return false;
    };
    let Ok(signature) = <[u8; 64]>::try_from(receipt) else {
        return false;
    };
    receipt_bytes(
        operation_id,
        package_sha256,
        *identity_public_key,
        *device_public_key,
        *ephemeral_public_key,
        *request_digest,
        *projection_sha256,
    )
    .is_ok_and(|message| {
        verifying
            .verify(&message, &ed25519_dalek::Signature::from_bytes(&signature))
            .is_ok()
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetProjection {
    schema_version: u32,
    identity_id: String,
    certificates: Vec<DeviceCert>,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
}

/// The encrypted payload of one admitted device: its certificate, the
/// person's public record, the fleet and the person's home peers.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapProjection {
    schema: u32,
    identity_record: String,
    certificate: String,
    fleet: String,
    home_peers: String,
}

impl BootstrapProjection {
    pub(crate) fn encode(&self) -> io::Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("Identity export is invalid"))?;
        if bytes.len() as u64 > MAX_PROJECTION {
            return Err(invalid("Identity export is too large"));
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_PROJECTION {
            return Err(invalid("Identity export is too large"));
        }
        serde_json::from_slice(bytes).map_err(|_| invalid("Identity export is invalid"))
    }
}

pub(crate) fn seal_projection(
    projection: &BootstrapProjection,
    ephemeral_public_key: &[u8; 32],
    aad: &[u8],
) -> io::Result<String> {
    let sealed = tilo_seal::hpke::seal_to_bytes(
        &tilo_seal::PublicKey::from_bytes(*ephemeral_public_key),
        Label::RelayEnrollResponse,
        aad,
        &projection.encode()?,
    )
    .map_err(|_| invalid("Could not seal the identity export"))?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        sealed,
    ))
}

pub(crate) fn open_projection(
    sealed: &str,
    ephemeral: &tilo_seal::SecretKey,
    aad: &[u8],
) -> io::Result<BootstrapProjection> {
    let sealed = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, sealed)
        .map_err(|_| invalid("The identity export is invalid"))?;
    let opened = tilo_seal::hpke::open_bytes(ephemeral, Label::RelayEnrollResponse, aad, &sealed)
        .map_err(|_| invalid("The identity export did not open"))?;
    BootstrapProjection::decode(&opened)
}

pub(crate) fn projection_digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes).into()
}

pub(crate) fn aad(
    operation_id: &str,
    package_sha256: &[u8; 32],
    identity_public_key: &[u8; 32],
    device_public_key: &[u8; 32],
    ephemeral_public_key: &[u8; 32],
    request_digest: &[u8; 32],
) -> Vec<u8> {
    let mut aad = b"tilo-migration-handoff-bootstrap-hpke-v1\0".to_vec();
    aad.extend_from_slice(operation_id.as_bytes());
    aad.extend_from_slice(package_sha256);
    aad.extend_from_slice(identity_public_key);
    aad.extend_from_slice(device_public_key);
    aad.extend_from_slice(ephemeral_public_key);
    aad.extend_from_slice(request_digest);
    aad
}

/// Sign the canonical member certificate and the public identity export.
///
/// # Errors
///
/// Refuses a request for another person, a changed validity window, an
/// invalid source export, or a signature that does not verify.
pub(crate) fn issue(
    source: &SourceIdentity,
    request: &CertificationRequest,
    now_ns: u64,
) -> io::Result<(DeviceCert, BootstrapProjection)> {
    source.valid()?;
    request
        .verify_at(now_ns)
        .map_err(|_| invalid("The joining device's request is invalid"))?;
    if request.user_public_key != source.identity_public_key {
        return Err(invalid("The joining device asked for another identity"));
    }
    let issued_at_ns = now_ns.saturating_sub(ISSUANCE_CLOCK_ALLOWANCE_NS);
    let mut cert = DeviceCert {
        user_public_key: source.identity_public_key,
        device_public_key: request.device_public_key,
        device_label: request.device_label.clone(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    let digest = device_cert_signing_digest(&cert)
        .map_err(|_| invalid("The device certificate is invalid"))?;
    cert.signature = (source.signer)(&digest)?;
    verify_device_cert_at(&cert, now_ns)
        .map_err(|_| invalid("The device certificate did not verify"))?;
    let mut certificates = source.certificates.clone();
    certificates.retain(|known| known.device_public_key != cert.device_public_key);
    certificates.push(cert.clone());
    certificates.sort_by_key(|known| known.device_public_key);
    let fleet = FleetProjection {
        schema_version: 1,
        identity_id: hex::encode(source.identity_public_key),
        certificates,
        revocations: Vec::new(),
    };
    let fleet = serde_json::to_vec_pretty(&fleet)
        .map_err(|_| invalid("The person's device fleet is invalid"))?;
    let home_peers = if source.home_peers.is_empty() {
        b"[]".to_vec()
    } else {
        home_peer_array(&source.home_peers)?;
        source.home_peers.clone()
    };
    let projection = BootstrapProjection {
        schema: PROJECTION_SCHEMA,
        identity_record: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &source.identity_record,
        ),
        certificate: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            tilo_user_identity::canonical::canonical_bytes(&cert)
                .map_err(|_| invalid("The device certificate is invalid"))?,
        ),
        fleet: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &fleet),
        home_peers: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &home_peers),
    };
    Ok((cert, projection))
}

/// Install the admitted member identity into the destination's own custody.
/// This never writes over an identity that already exists.
///
/// # Errors
///
/// Refuses an active identity, any existing device/root identity file, a
/// certificate or record for another person, or an invalid export.
pub(crate) fn install(
    identity_root: &Path,
    identity_dir: &Path,
    request: &CertificationRequest,
    projection: &BootstrapProjection,
    now_ns: u64,
) -> io::Result<()> {
    if projection.schema != PROJECTION_SCHEMA {
        return Err(invalid("The identity export is invalid"));
    }
    if runtime_identity::resolve_active_identity(identity_dir).is_some()
        || !identity_dir_empty(identity_dir)?
    {
        return Err(invalid("This computer already has an identity"));
    }
    let paths = IdentityPaths::under(identity_root);
    if paths.user_privkey.exists() || paths.user_pubkey.exists() || paths.device_cert.exists() {
        return Err(invalid("This computer already has an identity"));
    }
    let record = decode_b64(&projection.identity_record)?;
    let metadata = runtime_identity::load_metadata_from_bytes(&record)
        .map_err(|_| invalid("The identity export is invalid"))?;
    if metadata.group_public_key != request.user_public_key {
        return Err(invalid("The identity export is for another person"));
    }
    let certificate = decode_b64(&projection.certificate)?;
    let certificate: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&certificate)
        .map_err(|_| invalid("The device certificate is invalid"))?;
    if certificate.user_public_key != request.user_public_key
        || certificate.device_public_key != request.device_public_key
        || certificate.device_label != request.device_label
        || verify_device_cert_at(&certificate, now_ns).is_err()
        || !certificate.capabilities.can_own_workspaces
        || certificate.capabilities.can_admit_devices
        || certificate.capabilities.can_act_as_host
    {
        return Err(invalid("The device certificate is for another device"));
    }
    let fleet = decode_b64(&projection.fleet)?;
    let fleet: FleetProjection =
        serde_json::from_slice(&fleet).map_err(|_| invalid("The device fleet is invalid"))?;
    if fleet.schema_version != 1
        || fleet.identity_id != hex::encode(request.user_public_key)
        || fleet.certificates.len() > 256
        || fleet.revocations.len() > 256
    {
        return Err(invalid("The device fleet is invalid"));
    }
    for known in &fleet.certificates {
        if known.user_public_key != request.user_public_key
            || verify_device_cert_at(known, now_ns).is_err()
        {
            return Err(invalid("The device fleet is invalid"));
        }
    }
    let peers = decode_b64(&projection.home_peers)?;
    home_peer_array(&peers)?;
    let peers: serde_json::Value =
        serde_json::from_slice(&peers).map_err(|_| invalid("The home peers are invalid"))?;
    let (device, _) = tilo_user_identity::persist::load_or_create_device_key(&paths)
        .map_err(|_| invalid("The destination device key is unavailable"))?;
    if device.verifying_key().to_bytes() != certificate.device_public_key {
        return Err(invalid("The device certificate is for another device key"));
    }
    // The device key is generated here and certified by the source, exactly
    // as the canonical nearby join does; the root never arrives with it.
    tilo_user_identity::persist::install_device_cert(&paths, &certificate, now_ns)
        .map_err(|_| invalid("The device certificate could not be installed"))?;
    std::fs::create_dir_all(identity_dir)?;
    std::fs::set_permissions(identity_dir, std::fs::Permissions::from_mode(0o700))?;
    let record_hex: String = hex::encode(request.user_public_key)
        .chars()
        .take(RECORD_ID_HEX_CHARS)
        .collect();
    let record_id = format!("identity-{record_hex}");
    tilo_files::native::atomic::write_with_mode(
        &identity_dir.join(format!("{record_id}.json")),
        &record,
        0o600,
    )?;
    runtime_identity::set_active_identity(identity_dir, &record_id)
        .map_err(|_| invalid("The joined identity could not be selected"))?;
    let store = tilo_identity_host::device_fleet::DeviceFleetStore::at(identity_dir, paths);
    for known in fleet.certificates {
        store
            .remember_certificate(&fleet.identity_id, known)
            .map_err(|_| invalid("The device fleet could not be installed"))?;
    }
    if peers.as_array().is_some_and(|entries| !entries.is_empty()) {
        let path = tilo_user_identity::home_peer_registry_path(identity_root);
        tilo_files::native::atomic::write_creating_parent_with_mode(
            &path,
            &serde_json::to_vec_pretty(&peers)
                .map_err(|_| invalid("The home peers are invalid"))?,
            0o644,
        )?;
    }
    Ok(())
}

fn identity_dir_empty(directory: &Path) -> io::Result<bool> {
    match runtime_identity::list_identities(directory) {
        Ok(identities) => Ok(identities.is_empty()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn decode_b64(value: &str) -> io::Result<Vec<u8>> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value)
        .map_err(|_| invalid("The identity export is invalid"))
}

fn home_peer_array(bytes: &[u8]) -> io::Result<()> {
    if usize::try_from(MAX_PROJECTION).is_ok_and(|limit| bytes.len() > limit) {
        return Err(invalid("The home peers are too large"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("The home peers are invalid"))?;
    let Some(entries) = value.as_array() else {
        return Err(invalid("The home peers are invalid"));
    };
    if entries.len() > MAX_HOME_PEERS {
        return Err(invalid("The home peers are invalid"));
    }
    Ok(())
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| {
            u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
        })
}

/// Encode a canonical public member record for a source identity.
///
/// # Errors
///
/// As [`runtime_identity::public_member_record_with_presentation`].
pub fn public_identity_record(
    display_name: &str,
    avatar_color: [u8; 3],
    identity_public_key: [u8; 32],
    photo: Option<runtime_identity::ProfilePhoto>,
) -> io::Result<Vec<u8>> {
    runtime_identity::public_member_record_with_presentation(
        display_name,
        avatar_color,
        identity_public_key,
        photo,
    )
}
