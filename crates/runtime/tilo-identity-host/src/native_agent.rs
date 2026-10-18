//! Host-managed native agent enrollment. This is deliberately not a broker
//! operation: apps cannot choose a root, supply signing bytes, or obtain keys.
//! The host supplies an approved, bounded request for the person selected in
//! a room. Both the certificate and delegation are signed in that person's
//! existing custody; the actor's distinct private key stays here.

use std::fs::OpenOptions;
use std::io::{self, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_relay_protocol::delegation::{
    Delegate, DelegateProposal, DelegateScope, Delegation, MAX_DELEGATE_TEXT_BYTES,
    MAX_DELEGATED_APPS,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::device_fleet::DeviceFleetStore;
use crate::registry::{IdentityProfile, IdentityRegistryStore};

/// Enrollment never authorizes more than one day of native agent activity.
pub const MAX_NATIVE_AGENT_LIFETIME_NS: u64 = 24 * 3600 * 1_000_000_000;

/// A host-approved native agent. Apps must be explicit: an empty list would
/// mean every app in the canonical delegation format and is refused here.
#[derive(Clone)]
pub struct NativeAgentRequest {
    pub agent_id: String,
    pub label: String,
    pub color: [u8; 3],
    pub apps: Vec<String>,
    pub expires_at_ns: u64,
}

/// Retained host custody, with only public enrollment material exposed.
/// No Debug, serialization, key getter, or arbitrary signing operation.
pub struct ManagedNativeAgent {
    room: String,
    profile_id: String,
    key_path: PathBuf,
    key: SigningKey,
    certificate: DeviceCert,
    delegation: Delegation,
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn minimal_capabilities() -> DeviceCapabilities {
    DeviceCapabilities {
        can_own_workspaces: false,
        can_admit_devices: false,
        can_act_as_host: false,
        can_read_all_workspaces: false,
        foreign: false,
        can_administer_devices: false,
    }
}

fn check_request(room: &str, request: &NativeAgentRequest, now: u64) -> io::Result<()> {
    if tilo_desktop_model::RoomId::parse(room).is_none() {
        return Err(invalid("invalid native agent room"));
    }
    for text in [&request.agent_id, &request.label] {
        if text.trim().is_empty() || text.len() > MAX_DELEGATE_TEXT_BYTES {
            return Err(invalid("native agent id or label is empty or too long"));
        }
    }
    if request.apps.is_empty()
        || request.apps.len() > MAX_DELEGATED_APPS
        || request
            .apps
            .iter()
            .any(|app| app.trim().is_empty() || app.len() > 128)
    {
        return Err(invalid("native agent needs a bounded, explicit app scope"));
    }
    if request.expires_at_ns <= now
        || request.expires_at_ns.saturating_sub(now) > MAX_NATIVE_AGENT_LIFETIME_NS
    {
        return Err(invalid("native agent expiry must be within the next day"));
    }
    Ok(())
}

fn selected(room: &str) -> io::Result<IdentityProfile> {
    crate::registry::selected_identity_profile(room)?
        .ok_or_else(|| denied("no person is selected for the native agent room"))
}

fn check_person(room: &str, profile: &IdentityProfile) -> io::Result<DeviceCert> {
    let current = selected(room)?;
    if current.id != profile.id || current.user_public_key != profile.user_public_key {
        return Err(denied("native agent room selected another person"));
    }
    let (_, issuer) = crate::broker::trusted_current_device(room)?;
    if issuer.user_public_key != profile.user_public_key || !issuer.capabilities.can_admit_devices {
        return Err(denied("selected device cannot enroll native agents"));
    }
    Ok(issuer)
}

/// Provision a native actor for the room's actual selected person. The
/// stable (person, agent id) key is persisted privately before it is returned.
/// Repeating enrollment retains the actor key, never revives a revoked actor,
/// and reuses its exact certificate while it covers the requested lifetime.
/// Expiry or a longer lifetime requires a separate certificate lifecycle;
/// this path never replaces an enrolled certificate for the same actor key.
///
/// # Errors
/// Fails closed on missing/changed selection, unavailable signing custody,
/// invalid bounds, untrusted issuer, revoked actor or private storage errors.
/// Threshold identities require a real multi-device signing ceremony; this
/// local enrollment path does not conduct one and refuses them.
pub fn provision(room: &str, request: &NativeAgentRequest) -> io::Result<ManagedNativeAgent> {
    provision_inner(room, request, false)
}

/// Restore host-managed custody without renewing its certificate. The requested
/// expiry is an upper bound: an enrolled actor uses at most its remaining
/// certificate lifetime. Expired or revoked actors still require an explicit
/// lifecycle operation; a restart never extends their authorization.
///
/// # Errors
/// Fails under the same custody, selection and request checks as [`provision`].
pub fn provision_retained(
    room: &str,
    request: &NativeAgentRequest,
) -> io::Result<ManagedNativeAgent> {
    provision_inner(room, request, true)
}

fn provision_inner(
    room: &str,
    request: &NativeAgentRequest,
    retain_lifetime: bool,
) -> io::Result<ManagedNativeAgent> {
    let now = tilo_clock::unix_now_ns();
    check_request(room, request, now)?;
    let profile = selected(room)?;
    let issuer = check_person(room, &profile)?;
    let registry = IdentityRegistryStore::from_env()?;
    let directory = registry.identity_dir().join("native-agents");
    crate::provisioning::prepare_identity_directory(&directory)?;
    let directory = directory.join(hex::encode(profile.user_public_key));
    crate::provisioning::prepare_identity_directory(&directory)?;
    let directory_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(&directory)?;
    let _lock = advisory_lock::FileLock::exclusive(directory_file)?;
    let key_path = directory.join(format!("{}.key", hex::encode(request.agent_id.as_bytes())));
    let key = match read_key(&key_path) {
        Ok(key) => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            SigningKey::generate(&mut rand_core::UnwrapErr(getrandom::SysRng))
        }
        Err(error) => return Err(error),
    };
    let fleet = DeviceFleetStore::from_env()?;
    let enrolled = fleet.load(&profile.id)?;
    let actor_key = key.verifying_key().to_bytes();
    if enrolled.is_revoked(&actor_key) {
        return Err(denied("native agent actor is revoked"));
    }
    let (signer, loaded) = fleet.identity(&profile.id)?;
    if loaded.user_public_key != profile.user_public_key {
        return Err(denied("native agent signing custody changed"));
    }
    require_local_signing(&signer)?;
    let certificate = if let Some(existing) = enrolled
        .certificates
        .iter()
        .find(|cert| cert.device_public_key == actor_key)
    {
        if existing.user_public_key != profile.user_public_key
            || existing.capabilities != minimal_capabilities()
        {
            return Err(denied(
                "native agent certificate identity or capabilities changed",
            ));
        }
        match existing.expires_at_ns {
            Some(expiry) if expiry <= now => {
                return Err(denied(
                    "native agent certificate expired; requires explicit renewal or rotation",
                ));
            }
            Some(expiry) if retain_lifetime || request.expires_at_ns <= expiry => {}
            _ => {
                return Err(denied(
                    "native agent certificate lifetime requires explicit renewal or rotation",
                ));
            }
        }
        // Admission pins the entire certificate, not just its actor key.
        // Reissuing here would conflict even if only the issue time changed.
        existing.clone()
    } else {
        let mut certificate = DeviceCert {
            user_public_key: profile.user_public_key,
            device_public_key: actor_key,
            device_label: request.label.clone(),
            capabilities: minimal_capabilities(),
            issued_at_ns: now,
            expires_at_ns: Some(request.expires_at_ns),
            signature: [0; 64],
        };
        certificate.signature = signer
            .sign(&tilo_user_identity::device_cert_signing_digest(&certificate).map_err(other)?)
            .map_err(other)?;
        certificate
    };
    tilo_user_identity::verify_device_cert_at(&certificate, now).map_err(other)?;
    if certificate.expires_at_ns.is_none_or(|expiry| {
        expiry <= certificate.issued_at_ns
            || expiry.saturating_sub(certificate.issued_at_ns) > MAX_NATIVE_AGENT_LIFETIME_NS
    }) {
        return Err(denied(
            "native agent certificate must have a bounded one-day lifetime",
        ));
    }
    let mut effective_request = request.clone();
    effective_request.expires_at_ns = request.expires_at_ns.min(
        certificate
            .expires_at_ns
            .ok_or_else(|| denied("native agent certificate must expire"))?,
    );
    if issuer
        .expires_at_ns
        .is_some_and(|expiry| effective_request.expires_at_ns > expiry)
    {
        return Err(denied("native agent would outlive its enrolling device"));
    }
    let delegation = sign_delegation(&signer, &certificate, room, &effective_request, now)?;
    check_person(room, &profile)?;
    // Atomic replacement is made under the directory lock. A failed issuance
    // never changes a pre-existing key; a failed fleet write leaves a key that
    // the next enrollment can reuse, not an unpersisted actor in the caller.
    let seed = zeroize::Zeroizing::new(key.to_bytes());
    tilo_files::native::atomic::write_private(&key_path, seed.as_ref())?;
    fleet.remember_certificate(&profile.id, certificate.clone())?;
    let actor = ManagedNativeAgent {
        room: room.to_owned(),
        profile_id: profile.id,
        key_path,
        key,
        certificate,
        delegation,
    };
    actor.check_live()?;
    Ok(actor)
}

fn read_key(path: &Path) -> io::Result<SigningKey> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() != 32
    {
        return Err(denied(
            "native agent key must be a private, owned 32-byte file",
        ));
    }
    let mut seed = zeroize::Zeroizing::new([0; 32]);
    file.read_exact(seed.as_mut())?;
    Ok(SigningKey::from_bytes(&seed))
}

fn require_local_signing(signer: &runtime_identity::SigningIdentity) -> io::Result<()> {
    if signer.threshold() > 1 || signer.total_shares() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native agent enrollment requires a multi-device signing ceremony for threshold identities",
        ));
    }
    if !signer.is_single_device() || signer.signing_public_key() != signer.group_public_key() {
        return Err(denied(
            "native agent enrollment requires actual person root custody",
        ));
    }
    Ok(())
}

fn sign_delegation(
    signer: &runtime_identity::SigningIdentity,
    certificate: &DeviceCert,
    room: &str,
    request: &NativeAgentRequest,
    now: u64,
) -> io::Result<Delegation> {
    require_local_signing(signer)?;
    let proposal = DelegateProposal {
        delegate: Delegate::Agent {
            agent_id: request.agent_id.clone(),
        },
        label: request.label.clone(),
        color: request.color,
        scope: DelegateScope {
            rooms: vec![room.to_owned()],
            apps: request.apps.clone(),
        },
        expires_at_ns: request.expires_at_ns,
    };
    // Admission pins the complete delegation. Use the retained certificate's
    // issue time, not the prepare/restart time, so identical room grants sign
    // identically without extending the certificate's fixed authorization.
    let mut delegation = proposal
        .unsigned(
            certificate.user_public_key,
            certificate.device_public_key,
            certificate.issued_at_ns,
        )
        .map_err(other)?;
    delegation.signature = signer
        .sign(&delegation.signing_bytes().map_err(other)?)
        .map_err(other)?;
    delegation
        .verify(&certificate.user_public_key, now)
        .map_err(other)?;
    Ok(delegation)
}

impl ManagedNativeAgent {
    /// Public canonical actor certificate. Callers still verify live custody
    /// before admission; a retained certificate alone never proves liveness.
    #[must_use]
    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }

    /// Public canonical delegation for the enrollment room.
    #[must_use]
    pub fn delegation(&self) -> &Delegation {
        &self.delegation
    }

    fn check_live(&self) -> io::Result<IdentityProfile> {
        let now = tilo_clock::unix_now_ns();
        let profile = selected(&self.room)?;
        if profile.id != self.profile_id
            || profile.user_public_key != self.certificate.user_public_key
        {
            return Err(denied("native agent selected person changed"));
        }
        check_person(&self.room, &profile)?;
        tilo_user_identity::verify_device_cert_at(&self.certificate, now).map_err(other)?;
        self.delegation
            .verify(&profile.user_public_key, now)
            .map_err(other)?;
        let fleet = DeviceFleetStore::from_env()?.load(&profile.id)?;
        if fleet.is_revoked(&self.certificate.device_public_key)
            || !fleet
                .certificates
                .iter()
                .any(|cert| cert == &self.certificate)
        {
            return Err(denied(
                "native agent actor is revoked or no longer enrolled",
            ));
        }
        if read_key(&self.key_path)?.verifying_key() != self.key.verifying_key() {
            return Err(denied("native agent private custody changed"));
        }
        Ok(profile)
    }

    /// Sign only the fixed collaboration-join domain for the enrollment room.
    /// # Errors
    /// Refuses expired/revoked/changed custody or identity selection.
    pub fn join_proof(&self) -> io::Result<[u8; 64]> {
        self.check_live()?;
        Ok(self
            .key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &self.room,
            ))
            .to_bytes())
    }

    /// Sign a join proof for a task-room delegation issued by this custody.
    /// This never accepts arbitrary bytes or a room without a valid bounded
    /// root-signed delegation for this exact actor, app scope and expiry.
    /// # Errors
    /// Refuses widened grants, another person, or expired/revoked custody.
    pub fn join_proof_for(&self, delegation: &Delegation) -> io::Result<[u8; 64]> {
        let profile = self.check_live()?;
        delegation
            .verify(&profile.user_public_key, tilo_clock::unix_now_ns())
            .map_err(other)?;
        if delegation.device_public_key != self.certificate.device_public_key
            || delegation.delegate != self.delegation.delegate
            || delegation.label != self.delegation.label
            || delegation.color != self.delegation.color
            || delegation.scope.apps != self.delegation.scope.apps
            || delegation.expires_at_ns > self.delegation.expires_at_ns
            || delegation.scope.rooms.len() != 1
        {
            return Err(denied(
                "native agent join delegation exceeds retained custody",
            ));
        }
        let room = delegation
            .scope
            .rooms
            .first()
            .ok_or_else(|| denied("native agent join delegation has no room"))?;
        check_person(room, &profile)?;
        Ok(self
            .key
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes())
    }

    /// Delegate this same actor to one host-created task room for the same
    /// selected person. Actor, apps, display metadata and expiry cannot widen.
    /// Repeating the same grant produces the exact same signed delegation,
    /// including after custody is restored from its retained certificate.
    /// The host must separately govern admission to that room.
    /// # Errors
    /// Refuses invalid room ids, another selected person, or unavailable custody.
    pub fn delegate_room(&self, room: &str) -> io::Result<Delegation> {
        let profile = self.check_live()?;
        check_person(room, &profile)?;
        let request = NativeAgentRequest {
            agent_id: self
                .delegation
                .delegate
                .agent_id()
                .ok_or_else(|| denied("not an agent"))?
                .to_owned(),
            label: self.delegation.label.clone(),
            color: self.delegation.color,
            apps: self.delegation.scope.apps.clone(),
            expires_at_ns: self.delegation.expires_at_ns,
        };
        let now = tilo_clock::unix_now_ns();
        check_request(room, &request, now)?;
        let (signer, _) = DeviceFleetStore::from_env()?.identity(&profile.id)?;
        let delegation = sign_delegation(&signer, &self.certificate, room, &request, now)?;
        self.check_live()?;
        check_person(room, &profile)?;
        Ok(delegation)
    }
}

#[cfg(test)]
mod tests;
