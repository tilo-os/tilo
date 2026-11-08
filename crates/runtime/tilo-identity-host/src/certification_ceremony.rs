//! Adding a device to the person from any of their devices.
//!
//! The identity's key signs device certificates. It may sit whole on one
//! device, or split so that `t` of the identity's `n` devices must sign
//! together (FROST, [`threshold_identity`]); most devices hold none of it. A
//! device that pairs a new one but holds no key — or only one share — asks
//! the person's other devices to certify it:
//!
//! 1. The asking device proposes the exact certificate ([`Ask`] with
//!    [`Step::Approve`]) to each of the person's devices.
//! 2. Each device that holds key material asks the person there, showing
//!    the new device's label and its pairing code ([`pairing_code`]). A
//!    device holding the whole key answers with the signature; a device
//!    holding a share answers with its FROST commitment ([`Signer`]).
//! 3. Once `t` shares have approved, the asking device sends the chosen
//!    commitments back ([`Step::Sign`]) and aggregates their partial
//!    signatures ([`Gathering`]).
//!
//! The result is an ordinary root-signed certificate; nothing downstream
//! knows how it was signed. Every key holder checks the proposal itself
//! ([`check_proposal`]): the identity's own, a personal device, a revoked
//! device never — revocation wins over any approval.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use threshold_identity::{SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces};
use tilo_user_identity::{DeviceCert, device_cert_signing_digest, verify_device_cert};

use crate::enrollment::EnrollmentRole;

/// How long an approved FROST share waits for the signing round, and how
/// far a proposed certificate may start from the key holder's clock.
pub const SESSION_LIFETIME_NS: u64 = 10 * 60 * 1_000_000_000;

const PAIR_DOMAIN: &[u8] = b"tilo-nearby-home-peer-offer-v1\0";

/// The six-digit code a pairing device shows: the same on the new device,
/// the device that found it and every device asked to approve it.
#[must_use]
pub fn pairing_code(device_public_key: [u8; 32], relay_service_node_id: [u8; 32]) -> String {
    let mut input = PAIR_DOMAIN.to_vec();
    input.extend(device_public_key);
    input.extend(relay_service_node_id);
    let digest = blake3::hash(&input);
    let [first, second, third, fourth, ..] = *digest.as_bytes();
    let number = u32::from_le_bytes([first, second, third, fourth]);
    format!("{:06}", number % 1_000_000)
}

/// Why this device does not add a device by itself, said as a person
/// pairing reads it: whatever the key's layout, their other devices approve.
#[must_use]
pub fn cannot_sign_alone() -> String {
    "this device does not add devices by itself: pair from here, and your other devices ask \
     you to approve the new one"
        .into()
}

/// What one device of the person asks another.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ask {
    /// Names this certification on every device it involves.
    pub session: [u8; 16],
    /// The certificate to sign, exactly; its signature is all zeros.
    pub cert: DeviceCert,
    /// The relay service the new device announced: with its key, the
    /// pairing code the person compares.
    pub service: [u8; 32],
    pub step: Step,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Step {
    /// Ask the person whether the device may join.
    Approve,
    /// Ask a key holder to sign `cert`, which renews the certificate
    /// `held` the same device already acts with: no new device joins, so
    /// nobody is asked here. Only a certificate that
    /// [`DeviceCert::renews`] `held` is answered, and only while `held`
    /// still is that device's.
    Renew {
        /// The certificate this one replaces, as the asking device's
        /// custody held it.
        held: DeviceCert,
    },
    /// Sign with the share approved for this session, together with the
    /// shares that made these commitments.
    Sign {
        commitments: Vec<SigningRound1Commitment>,
    },
}

/// A device's answer to an [`Ask`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    pub session: [u8; 16],
    pub reply: Reply,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The whole key signed the certificate: its 64-byte signature.
    Signed(Vec<u8>),
    /// The person approved; this share's commitment for the signing round.
    Committed(SigningRound1Commitment),
    /// This share's part of the signature.
    Partial(SigningRound2Partial),
    /// The person declined here.
    Declined,
    /// This device holds none of the identity's key.
    HoldsNoKey,
    /// The device refused the ask itself, with why.
    Refused(String),
}

impl Ask {
    /// The pairing code of the device this asks to certify.
    #[must_use]
    pub fn code(&self) -> String {
        pairing_code(self.cert.device_public_key, self.service)
    }

    /// The digest the identity's key signs.
    ///
    /// # Errors
    ///
    /// When the certificate cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], String> {
        device_cert_signing_digest(&self.cert).map_err(|error| error.to_string())
    }
}

/// Check what `ask` proposes before anyone is asked about it: a certificate
/// of `person` for a personal device that is not revoked, unsigned, named,
/// starting about now and not expiring.
///
/// # Errors
///
/// Why the proposal is refused.
pub fn check_proposal(
    ask: &Ask,
    person: [u8; 32],
    revoked: &dyn Fn(&[u8; 32]) -> bool,
    now_ns: u64,
) -> Result<(), String> {
    let cert = &ask.cert;
    if cert.user_public_key != person {
        return Err("the device was proposed for another identity".into());
    }
    if cert.signature != [0; 64] {
        return Err("the proposed certificate is already signed".into());
    }
    if ed25519_dalek::VerifyingKey::from_bytes(&cert.device_public_key).is_err() {
        return Err("the proposed device key is not a key".into());
    }
    if revoked(&cert.device_public_key) {
        return Err("that device was removed from this identity and cannot join again".into());
    }
    if cert.capabilities != EnrollmentRole::PersonalDevice.preset() {
        return Err("a home server is added from the device you set up first".into());
    }
    let label = cert.device_label.trim();
    if label.is_empty()
        || cert.device_label.chars().count() > 80
        || cert.device_label.chars().any(char::is_control)
    {
        return Err("the proposed device needs a printable name of at most 80 characters".into());
    }
    if cert.issued_at_ns.abs_diff(now_ns) > SESSION_LIFETIME_NS || cert.expires_at_ns.is_some() {
        return Err("the proposed certificate does not start now".into());
    }
    if ask.service == [0; 32] {
        return Err("the proposed device announced no relay service".into());
    }
    Ok(())
}

/// Whether a renewal's lifetime is the shape the certificate it replaces had:
/// both expire, or neither does. The length may differ — a policy may have
/// shortened the lifetime — but a renewal never turns an expiring
/// certificate into one that never expires, or the reverse.
fn renewal_expiry_shape(held: Option<u64>, renewed: Option<u64>) -> Result<(), ()> {
    match (held, renewed) {
        (None, None) | (Some(_), Some(_)) => Ok(()),
        _ => Err(()),
    }
}

/// Check what a [`Step::Renew`] ask proposes before a key holder signs it:
/// `ask.cert` renews `held` ([`DeviceCert::renews`]) — the same device key
/// under the same person's root, issued later, granting nothing more — and
/// neither is revoked or for another person. `current` is the certificate
/// the key holder's fleet holds for that device, when it holds one; a
/// renewal replaces that and nothing else, so a stale one is refused.
///
/// A renewal adds no device and grants nothing new, so unlike
/// [`check_proposal`] it is not the person's to approve: the device is
/// already theirs, and the person already trusts what it may do.
///
/// # Errors
///
/// Why the renewal is refused.
pub fn check_renewal(
    ask: &Ask,
    held: &DeviceCert,
    current: Option<&DeviceCert>,
    person: [u8; 32],
    revoked: &dyn Fn(&[u8; 32]) -> bool,
) -> Result<(), String> {
    let Step::Renew { held: proposed } = &ask.step else {
        return Err("not a renewal".into());
    };
    if ask.cert.user_public_key != person || held.user_public_key != person {
        return Err("the renewal was proposed for another identity".into());
    }
    if proposed != held {
        return Err("the renewal names a certificate other than the one it replaces".into());
    }
    if ask.cert.signature != [0; 64] {
        return Err("the proposed certificate is already signed".into());
    }
    if !ask.cert.renews(held) {
        return Err(
            "a renewal is the same device under the same root, later, granting no more".into(),
        );
    }
    // The renewal keeps the name and the lifetime shape: a certificate that
    // expires keeps expiring, and one that never did stays that way.
    if ask.cert.device_label != held.device_label {
        return Err("a renewal keeps the device's name".into());
    }
    let held_lifetime = held
        .expires_at_ns
        .map(|expires| expires.saturating_sub(held.issued_at_ns));
    let renewed_lifetime = ask
        .cert
        .expires_at_ns
        .map(|expires| expires.saturating_sub(ask.cert.issued_at_ns));
    if renewal_expiry_shape(held_lifetime, renewed_lifetime).is_err() {
        return Err("a renewal keeps the lifetime the certificate it replaces had".into());
    }
    if let Some(current) = current
        && !ask.cert.renews(current)
    {
        return Err("the renewal does not replace the certificate this identity holds".into());
    }
    if revoked(&ask.cert.device_public_key) {
        return Err("that device was removed from this identity and cannot be renewed".into());
    }
    Ok(())
}

/// The identity key material a device holds, as the ceremony uses it.
pub trait IdentityKey {
    /// The person the key belongs to.
    fn person(&self) -> [u8; 32];
    /// Whether this device holds the whole key or a share of it.
    fn holds_key(&self) -> bool;
    /// `Some` signature when this device holds the whole key.
    ///
    /// # Errors
    ///
    /// When holding the whole key, signing failed.
    fn sign_whole(&self, digest: &[u8; 32]) -> Result<Option<[u8; 64]>, String>;
    /// `Some` commitment when this device holds a share.
    fn commit(&self) -> Option<(SigningRound1Commitment, SigningSessionNonces)>;
    /// This share's partial signature over `digest`.
    ///
    /// # Errors
    ///
    /// When the commitments do not include this share's or signing fails.
    fn partial(
        &self,
        nonces: SigningSessionNonces,
        digest: &[u8; 32],
        commitments: &[SigningRound1Commitment],
    ) -> Result<SigningRound2Partial, String>;
}

impl IdentityKey for runtime_identity::SigningIdentity {
    fn person(&self) -> [u8; 32] {
        self.group_public_key()
    }

    fn holds_key(&self) -> bool {
        self.is_single_device() || self.threshold() >= 2
    }

    fn sign_whole(&self, digest: &[u8; 32]) -> Result<Option<[u8; 64]>, String> {
        if !self.is_single_device() {
            return Ok(None);
        }
        self.sign(digest)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn commit(&self) -> Option<(SigningRound1Commitment, SigningSessionNonces)> {
        if self.is_single_device() || self.threshold() < 2 {
            return None;
        }
        let mut rng = getrandom::rand_core::UnwrapErr(getrandom::SysRng);
        self.threshold_sign_round1(&mut rng)
            .ok()
            .map(|round| (round.commitment, round.nonces))
    }

    fn partial(
        &self,
        nonces: SigningSessionNonces,
        digest: &[u8; 32],
        commitments: &[SigningRound1Commitment],
    ) -> Result<SigningRound2Partial, String> {
        self.threshold_sign_round2(nonces, digest, commitments)
            .map_err(|error| error.to_string())
    }
}

struct Approved {
    digest: [u8; 32],
    nonces: SigningSessionNonces,
    until_ns: u64,
}

/// A key holder's side: what the person approved here, until it is signed.
#[derive(Default)]
pub struct Signer {
    approved: HashMap<[u8; 16], Approved>,
}

impl Signer {
    /// Answer an [`Step::Approve`] ask the person approved (`approve`) or
    /// declined here. Run [`check_proposal`] before asking them.
    pub fn approve(
        &mut self,
        key: &dyn IdentityKey,
        ask: &Ask,
        approve: bool,
        now_ns: u64,
    ) -> Reply {
        self.approved
            .retain(|_, approved| approved.until_ns > now_ns);
        if !approve {
            return Reply::Declined;
        }
        if ask.cert.user_public_key != key.person() {
            return Reply::Refused("the device was proposed for another identity".into());
        }
        let digest = match ask.digest() {
            Ok(digest) => digest,
            Err(error) => return Reply::Refused(error),
        };
        match key.sign_whole(&digest) {
            Ok(Some(signature)) => return Reply::Signed(signature.to_vec()),
            Ok(None) => {}
            Err(error) => return Reply::Refused(error),
        }
        let Some((commitment, nonces)) = key.commit() else {
            return Reply::HoldsNoKey;
        };
        self.approved.insert(
            ask.session,
            Approved {
                digest,
                nonces,
                until_ns: now_ns.saturating_add(SESSION_LIFETIME_NS),
            },
        );
        Reply::Committed(commitment)
    }

    /// Answer a [`Step::Renew`] ask the person did not have to approve:
    /// [`check_renewal`] is what a caller runs first. A device holding the
    /// whole key signs the renewal; one holding a share answers with its
    /// commitment, and the shares gather as for any certificate
    /// ([`Gathering`]). The session is retained so a [`Step::Sign`] round
    /// can use the share, exactly as [`Self::approve`] retains it.
    pub fn renew(&mut self, key: &dyn IdentityKey, ask: &Ask, now_ns: u64) -> Reply {
        self.approved
            .retain(|_, approved| approved.until_ns > now_ns);
        if ask.cert.user_public_key != key.person() {
            return Reply::Refused("the renewal was proposed for another identity".into());
        }
        let digest = match ask.digest() {
            Ok(digest) => digest,
            Err(error) => return Reply::Refused(error),
        };
        match key.sign_whole(&digest) {
            Ok(Some(signature)) => return Reply::Signed(signature.to_vec()),
            Ok(None) => {}
            Err(error) => return Reply::Refused(error),
        }
        let Some((commitment, nonces)) = key.commit() else {
            return Reply::HoldsNoKey;
        };
        self.approved.insert(
            ask.session,
            Approved {
                digest,
                nonces,
                until_ns: now_ns.saturating_add(SESSION_LIFETIME_NS),
            },
        );
        Reply::Committed(commitment)
    }

    /// Answer a [`Step::Sign`] ask: only for a session the person approved
    /// here, over the same certificate, once.
    pub fn sign(&mut self, key: &dyn IdentityKey, ask: &Ask, now_ns: u64) -> Reply {
        let Step::Sign { commitments } = &ask.step else {
            return Reply::Refused("not a signing round".into());
        };
        let Some(approved) = self.approved.remove(&ask.session) else {
            return Reply::Refused("this device did not approve that device".into());
        };
        if approved.until_ns <= now_ns || ask.digest().ok() != Some(approved.digest) {
            return Reply::Refused("this device did not approve that certificate".into());
        }
        match key.partial(approved.nonces, &approved.digest, commitments) {
            Ok(partial) => Reply::Partial(partial),
            Err(error) => Reply::Refused(error),
        }
    }
}

/// Where a certification stands for the device that asked.
#[derive(Debug)]
pub enum Progress {
    /// Signed by the identity's key.
    Certified(DeviceCert),
    /// Send these asks, each to its device.
    Send(Vec<([u8; 32], Ask)>),
    /// Waiting for more of the person's devices.
    Waiting,
    /// It cannot happen; why, plainly.
    Failed(String),
}

/// The asking device's side: the approvals gathered so far.
pub struct Gathering {
    ask: Ask,
    threshold: u16,
    total: u16,
    commitments: BTreeMap<[u8; 32], SigningRound1Commitment>,
    signers: Vec<[u8; 32]>,
    partials: BTreeMap<[u8; 32], SigningRound2Partial>,
    declined: BTreeSet<[u8; 32]>,
    keyless: BTreeSet<[u8; 32]>,
}

impl Gathering {
    /// Certify `cert` (unsigned) for the new device that announced
    /// `service`, under an identity whose key `threshold` of its `total`
    /// devices must sign.
    #[must_use]
    pub fn new(
        session: [u8; 16],
        cert: DeviceCert,
        service: [u8; 32],
        threshold: u16,
        total: u16,
    ) -> Self {
        Self::over(
            Ask {
                session,
                cert,
                service,
                step: Step::Approve,
            },
            threshold,
            total,
        )
    }

    /// Renew `cert` (unsigned), which replaces `held` for the same device,
    /// under an identity whose key `threshold` of its `total` devices must
    /// sign. `service` is the relay service the device announced, which
    /// names the pairing code a renewal is shown under like any
    /// certificate.
    #[must_use]
    pub fn renewal(
        session: [u8; 16],
        cert: DeviceCert,
        held: DeviceCert,
        service: [u8; 32],
        threshold: u16,
        total: u16,
    ) -> Self {
        Self::over(
            Ask {
                session,
                cert,
                service,
                step: Step::Renew { held },
            },
            threshold,
            total,
        )
    }

    fn over(ask: Ask, threshold: u16, total: u16) -> Self {
        Self {
            ask,
            threshold: threshold.max(1),
            total: total.max(threshold).max(1),
            commitments: BTreeMap::new(),
            signers: Vec::new(),
            partials: BTreeMap::new(),
            declined: BTreeSet::new(),
            keyless: BTreeSet::new(),
        }
    }

    /// The ask every device of the person gets first.
    #[must_use]
    pub fn ask(&self) -> &Ask {
        &self.ask
    }

    /// Take `answer` from the device `from`.
    pub fn answer(&mut self, from: [u8; 32], answer: Answer) -> Progress {
        if answer.session != self.ask.session {
            return Progress::Waiting;
        }
        match answer.reply {
            Reply::Signed(signature) => match <[u8; 64]>::try_from(signature.as_slice()) {
                Ok(signature) => self.finish(signature),
                Err(_) => Progress::Failed("a device answered with a malformed signature".into()),
            },
            Reply::Committed(commitment) => self.committed(from, commitment),
            Reply::Partial(partial) => self.partial(from, partial),
            Reply::Declined => {
                self.declined.insert(from);
                self.commitments.remove(&from);
                if self.declined.len() > usize::from(self.total.saturating_sub(self.threshold)) {
                    Progress::Failed(
                        "the device was declined on another of your devices".to_string(),
                    )
                } else {
                    Progress::Waiting
                }
            }
            Reply::HoldsNoKey => {
                self.keyless.insert(from);
                Progress::Waiting
            }
            Reply::Refused(reason) => Progress::Failed(reason),
        }
    }

    fn committed(&mut self, from: [u8; 32], commitment: SigningRound1Commitment) -> Progress {
        if !self.signers.is_empty() || self.declined.contains(&from) {
            return Progress::Waiting;
        }
        self.commitments.insert(from, commitment);
        if self.commitments.len() < usize::from(self.threshold) {
            return Progress::Waiting;
        }
        let chosen: Vec<_> = self
            .commitments
            .iter()
            .take(usize::from(self.threshold))
            .map(|(device, commitment)| (*device, commitment.clone()))
            .collect();
        self.signers = chosen.iter().map(|(device, _)| *device).collect();
        let commitments: Vec<_> = chosen.into_iter().map(|(_, c)| c).collect();
        let ask = Ask {
            step: Step::Sign { commitments },
            ..self.ask.clone()
        };
        Progress::Send(
            self.signers
                .iter()
                .map(|device| (*device, ask.clone()))
                .collect(),
        )
    }

    fn partial(&mut self, from: [u8; 32], partial: SigningRound2Partial) -> Progress {
        if !self.signers.contains(&from) {
            return Progress::Waiting;
        }
        self.partials.insert(from, partial);
        if self.partials.len() < self.signers.len() {
            return Progress::Waiting;
        }
        let partials: Vec<_> = self.partials.values().cloned().collect();
        let group = collab_types::Ed25519PublicKey::from_bytes(self.ask.cert.user_public_key);
        match threshold_identity::aggregate_signature(&group, &partials, self.threshold) {
            Ok(signature) => self.finish(*signature.as_bytes()),
            Err(error) => {
                Progress::Failed(format!("your devices' signatures did not combine: {error}"))
            }
        }
    }

    fn finish(&self, signature: [u8; 64]) -> Progress {
        let mut cert = self.ask.cert.clone();
        cert.signature = signature;
        match verify_device_cert(&cert) {
            Ok(()) => Progress::Certified(cert),
            Err(_) => Progress::Failed(
                "a device answered with a signature that is not your identity's".into(),
            ),
        }
    }

    /// What is still needed, plainly, for the person.
    #[must_use]
    pub fn status(&self) -> String {
        if self.threshold == 1 {
            return "Approve it on your other device when it asks.".into();
        }
        let approved = if self.signers.is_empty() {
            self.commitments.len()
        } else {
            self.partials.len()
        };
        format!(
            "{} of your devices must approve it when they ask; {} approved so far.",
            self.threshold, approved
        )
    }
}

#[cfg(test)]
mod tests;
