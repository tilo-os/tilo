//! What disk unlock needs of the person's identity on this device: the fleet it trusts, whether it
//! holds the person's root, the secret-sync key a half opens with, and the root's signature on a
//! boot key's certificate ([`PersonKeys`]). [`enrol`] certifies a boot key and seals its sibling
//! half; [`with_enrolment`] and [`with_decision`] are what a device keeps of it in its own part of
//! the person's store.

use tilo_unlock_protocol::{
    EnrolReply, EnrolRequest, EnrolResponse, SiblingContact, SlotId, seal_sibling_half,
};
use tilo_user_identity::{
    CertifiedFleet, DeviceCapabilities, DeviceCert, device_cert_signing_digest, verify_device_cert,
};

use crate::unlock::{
    AutoApproval, MAX_UNLOCK_DECISIONS, MergedUnlock, UnlockDecisionEntry, UnlockEnrolment,
    UnlockFabricState, UnlockView,
};
use crate::{DeviceId, FabricStamp};

/// The person's identity this device answers as, and what it holds.
#[derive(Clone, Debug)]
pub struct Ownership {
    /// The devices the person's root certified, and the ones it revoked.
    pub fleet: CertifiedFleet,
    /// Whether this device holds the person's root, so it can release a half and sign.
    pub key_holder: bool,
}

/// The person's identity as disk unlock reads it on this device.
pub trait PersonKeys: Send + Sync {
    /// Who this device answers as, when it holds an identity of the person's.
    fn ownership(&self) -> Option<Ownership>;

    /// The person's secret-sync key, held only by a device with the root: loaded when a half is
    /// released, never before.
    fn secret_sync(&self) -> Option<tilo_seal::SecretKey>;

    /// The person's root's signature on `digest`.
    ///
    /// # Errors
    /// This device holds no root, or it did not sign.
    fn sign_root(&self, digest: &[u8; 32]) -> Result<[u8; 64], String>;

    /// The person's devices their fleet lists at `now_ns`.
    fn person_devices(&self, now_ns: u64) -> Vec<[u8; 32]>;
}

/// The identity this device keeps in its own stores.
#[derive(Clone, Copy, Debug, Default)]
pub struct StoredKeys;

impl StoredKeys {
    fn profile() -> Option<(
        String,
        [u8; 32],
        tilo_identity::device_fleet::DeviceFleetStore,
    )> {
        let registry = tilo_identity::store::IdentityRegistryStore::from_env()
            .ok()?
            .load_reconciled_cached()
            .ok()?;
        let profile = registry.certified_identity()?;
        let store = tilo_identity::device_fleet::DeviceFleetStore::from_env().ok()?;
        Some((profile.id, profile.user_public_key, store))
    }
}

impl PersonKeys for StoredKeys {
    fn ownership(&self) -> Option<Ownership> {
        let (id, user, store) = Self::profile()?;
        let fleet = store.certified_fleet(&id, user).ok()?;
        let key_holder = store.secret_sync_key(&id).is_ok();
        Some(Ownership { fleet, key_holder })
    }

    fn secret_sync(&self) -> Option<tilo_seal::SecretKey> {
        let (id, _, store) = Self::profile()?;
        store.secret_sync_key(&id).ok()
    }

    fn sign_root(&self, digest: &[u8; 32]) -> Result<[u8; 64], String> {
        let (id, _, store) =
            Self::profile().ok_or("this device holds no identity of the person's")?;
        store
            .sign_root(&id, digest)
            .map_err(|error| error.to_string())
    }

    fn person_devices(&self, now_ns: u64) -> Vec<[u8; 32]> {
        let Some((id, user, store)) = Self::profile() else {
            return Vec::new();
        };
        store
            .load(&id)
            .map(|fleet| fleet.personal_devices(&user, now_ns))
            .unwrap_or_default()
    }
}

/// Why an enrolment was not signed here: another device must, or this request is not one this
/// device accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotEnrolled {
    /// This device does not hold the person's root.
    NotKeyHolder,
    /// Anything else, with a reason for the person.
    Reason(String),
}

/// What an enrolment signed here gives: the answer for the device that asked, and what this device
/// keeps of it for the person's other devices.
#[derive(Clone, Debug)]
pub struct Enrolled {
    pub response: EnrolResponse,
    pub enrolment: UnlockEnrolment,
    pub approval: Option<AutoApproval>,
}

/// Certify `enrollee`'s boot key with the person's root and seal its sibling half to the
/// secret-sync key, as the device `holder`, which keeps the enrolment. `label_of` names a device
/// as the person's store names it.
///
/// # Errors
/// Why it was not enrolled here.
pub fn enrol(
    keys: &dyn PersonKeys,
    request: &EnrolRequest,
    enrollee: [u8; 32],
    holder: [u8; 32],
    label_of: impl Fn(&[u8; 32]) -> String,
    now_ns: u64,
) -> Result<Enrolled, NotEnrolled> {
    request
        .verify()
        .map_err(|error| NotEnrolled::Reason(format!("enrolment request: {error}")))?;
    let ownership = keys.ownership().ok_or_else(|| {
        NotEnrolled::Reason("this device holds no identity for the person's devices".into())
    })?;
    if !ownership.key_holder {
        return Err(NotEnrolled::NotKeyHolder);
    }
    let refused = NotEnrolled::Reason;
    // The boot key is certified as an ordinary device certificate that grants nothing: it can ask
    // for the half, never act as the device.
    let mut cert = DeviceCert {
        user_public_key: *ownership.fleet.user_public_key(),
        device_public_key: request.boot_public_key,
        device_label: label_of(&enrollee),
        capabilities: DeviceCapabilities::nothing(),
        issued_at_ns: now_ns,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    let digest = device_cert_signing_digest(&cert)
        .map_err(|error| refused(format!("the boot key's certificate: {error}")))?;
    cert.signature = keys.sign_root(&digest).map_err(refused)?;
    verify_device_cert(&cert)
        .map_err(|error| refused(format!("the boot key's certificate: {error}")))?;
    let secret_sync = keys
        .secret_sync()
        .ok_or_else(|| refused("this device holds no secret-sync key".into()))?
        .public_key()
        .map_err(|error| refused(error.to_string()))?;
    let sealed_half = seal_sibling_half(&secret_sync, &cert, &request.sibling_half)
        .map_err(|error| refused(error.to_string()))?;
    // Every one of the person's devices holds the half once the enrolment reaches them, whatever
    // is beside this device at this moment: the list is their fleet, named as the person's store
    // names them. This device is among them, so a person with two devices has one to ask at boot.
    let siblings = keys
        .person_devices(now_ns)
        .into_iter()
        .filter(|key| key != &enrollee)
        .map(|key| SiblingContact {
            device_public_key: key,
            label: label_of(&key),
            endpoint: String::new(),
        })
        .collect();
    // An enrolment for an unattended unlock asks, with the boot chain it measures now, to be
    // released without a prompt. The root signs that acceptance, so any of the person's devices
    // reads it as the owner's.
    let approval = request
        .unattended_chain
        .map(|chain| -> Result<AutoApproval, NotEnrolled> {
            let mut approval = AutoApproval::unsigned(
                *ownership.fleet.user_public_key(),
                enrollee,
                vec![chain],
                now_ns,
            );
            let digest = approval
                .digest()
                .ok_or_else(|| refused("the unlock policy could not be encoded".into()))?;
            approval.signature = keys.sign_root(&digest).map_err(refused)?.to_vec();
            approval.validate().map_err(|error| {
                refused(format!("the unlock policy is not admissible: {error}"))
            })?;
            Ok(approval)
        })
        .transpose()?;
    let enrolment = UnlockEnrolment {
        cert: cert.clone(),
        device_public_key: enrollee,
        sealed_half,
        slot: request.slot,
        attestation_key: request.attestation_key.clone(),
        stamp: FabricStamp {
            counter: 0,
            device: DeviceId::of_key(&holder),
        },
    };
    tracing::info!(
        device = %hex::encode(enrollee),
        slot = %slot_name(&request.slot),
        "device unlock: enrolled",
    );
    Ok(Enrolled {
        response: EnrolResponse { cert, siblings },
        enrolment,
        approval,
    })
}

/// The answer an enrolment's outcome gives the device that asked.
#[must_use]
pub fn enrol_reply(outcome: Result<EnrolResponse, NotEnrolled>) -> EnrolReply {
    match outcome {
        Ok(response) => EnrolReply::Enrolled(Box::new(response)),
        Err(NotEnrolled::NotKeyHolder) => EnrolReply::NotKeyHolder,
        Err(NotEnrolled::Reason(reason)) => EnrolReply::Refused(reason),
    }
}

/// `state` with `enrolment` in place of the one for its slot, and the owner's acceptance of the
/// boot chain it asked for in place of the device's earlier one.
pub fn with_enrolment(
    state: &mut UnlockFabricState,
    enrolment: UnlockEnrolment,
    approval: Option<AutoApproval>,
) {
    state
        .enrolments
        .retain(|current| current.slot != enrolment.slot);
    state.enrolments.push(enrolment);
    if let Some(approval) = approval {
        let device = approval.device_public_key;
        state
            .auto_approvals
            .retain(|current| current.device_public_key != device);
        state.auto_approvals.push(approval);
    }
}

/// `state` with the owner's answer `record` kept: answers that no longer hold give way to the
/// newest ones.
pub fn with_decision(state: &mut UnlockFabricState, record: UnlockDecisionEntry, now_ns: u64) {
    state
        .decisions
        .retain(|current| current.valid_until_ns > now_ns && current.key() != record.key());
    state.decisions.push(record);
    state.decisions.sort_by_key(|kept| kept.decided_at_ns);
    let over = state.decisions.len().saturating_sub(MAX_UNLOCK_DECISIONS);
    state.decisions.drain(..over);
}

/// What the sibling state machine reads: the fleet this device trusts and the person's fabric as
/// it holds it.
#[must_use]
pub fn view_of<'a>(
    ownership: &'a Ownership,
    published: &'a MergedUnlock,
    secret_sync: Option<&'a tilo_seal::SecretKey>,
) -> UnlockView<'a> {
    UnlockView {
        fleet: &ownership.fleet,
        enrolments: &published.enrolments,
        auto_approvals: &published.auto_approvals,
        decisions: &published.decisions,
        key_holder: ownership.key_holder,
        secret_sync,
    }
}

/// The enrolment a boot's certificate is known by.
#[must_use]
pub fn boot_cert_digest(cert: &DeviceCert) -> Option<[u8; 32]> {
    device_cert_signing_digest(cert).ok()
}

/// The slot as the boot screen names it, and as the person's own record of their answers keeps it.
#[must_use]
pub fn slot_name(slot: &SlotId) -> String {
    format!("{}:{}", hex::encode(slot.volume_uuid), slot.keyslot)
}
