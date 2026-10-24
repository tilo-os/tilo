use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

pub(crate) const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a92";

/// One device: its certificate and key.
pub(crate) struct Device {
    pub cert: DeviceCert,
    pub key: SigningKey,
}

pub(crate) fn device(root: &SigningKey, seed: u8, expires_at_ns: Option<u64>) -> Device {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device {seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    Device { cert, key }
}

/// The people of a handoff: the owner, whose device `governor` governs the
/// room and whose device `proposer` asks to hand it on; the recipient, whose
/// device `successor` accepts; and another member.
pub(crate) struct Parties {
    pub owner: SigningKey,
    pub governor: Device,
    pub proposer: Device,
    pub recipient: SigningKey,
    pub successor: Device,
    pub member: Device,
}

impl Parties {
    pub(crate) fn new() -> Self {
        Self::with_successor_expiry(None)
    }

    /// The parties, the successor's certificate expiring at
    /// `expires_at_ns`.
    pub(crate) fn with_successor_expiry(expires_at_ns: Option<u64>) -> Self {
        let owner = SigningKey::from_bytes(&[41; 32]);
        let recipient = SigningKey::from_bytes(&[51; 32]);
        let member = SigningKey::from_bytes(&[61; 32]);
        Self {
            governor: device(&owner, 42, None),
            proposer: device(&owner, 43, None),
            successor: device(&recipient, 52, expires_at_ns),
            member: device(&member, 62, None),
            owner,
            recipient,
        }
    }

    pub(crate) fn roster(&self) -> Vec<DeviceCert> {
        vec![
            self.governor.cert.clone(),
            self.proposer.cert.clone(),
            self.successor.cert.clone(),
            self.member.cert.clone(),
        ]
    }

    /// The owner's offer `id` of the room to the recipient, open from 10 to
    /// 1 000 000.
    pub(crate) fn offer(&self, id: u8) -> OwnershipOffer {
        OwnershipOffer {
            version: HANDOFF_VERSION,
            room: ROOM.into(),
            offer_id: [id; 32],
            owner: self.owner.verifying_key().to_bytes(),
            recipient: self.recipient.verifying_key().to_bytes(),
            proposed_by: self.proposer.cert.clone(),
            offered_at_ns: 10,
            expires_at_ns: 1_000_000,
        }
    }

    /// The successor's acceptance of `offer` at 20.
    pub(crate) fn accept(&self, offer: &OwnershipOffer) -> HandoffAcceptance {
        HandoffAcceptance::sign(
            offer,
            self.successor.cert.clone(),
            hint(&self.successor),
            20,
            &self.successor.key,
        )
        .unwrap()
    }

    /// The governor's handoff of `offer`, accepted, in generation
    /// `generation` at 30.
    pub(crate) fn handoff(&self, offer: &OwnershipOffer, generation: u64) -> GovernorHandoff {
        GovernorHandoff::sign(
            offer.clone(),
            self.accept(offer),
            self.governor.cert.clone(),
            generation,
            30,
            &self.governor.key,
        )
        .unwrap()
    }
}

/// Where `host` is reached, signed by it.
pub(crate) fn hint(host: &Device) -> TransportHint {
    tilo_workspace_join::sign_transport_hint(
        "test",
        "{}",
        host.cert.clone(),
        host.key.verifying_key().to_bytes(),
        &host.key,
    )
    .unwrap()
}

#[test]
fn a_committed_handoff_verifies_with_no_clock() {
    let parties = Parties::new();
    let offer = parties.offer(1);
    let acceptance = parties.accept(&offer);
    assert_eq!(
        check_acceptance(ROOM, &parties.governor.cert, &offer, &acceptance, 30),
        Ok(())
    );
    let handoff = parties.handoff(&offer, 2);
    assert_eq!(handoff.verify(ROOM, &parties.governor.cert, 0), Ok(()));
    assert_eq!(handoff.successor(), &parties.successor.cert);
}

#[test]
fn only_the_owner_offers_the_room() {
    let parties = Parties::new();
    let mut offer = parties.offer(1);
    offer.owner = parties.recipient.verifying_key().to_bytes();
    assert_eq!(
        check_offer(ROOM, &parties.governor.cert, &offer),
        Err(HandoffError::NotOwner)
    );
    let mut offer = parties.offer(1);
    offer.proposed_by = parties.member.cert.clone();
    assert_eq!(
        check_offer(ROOM, &parties.governor.cert, &offer),
        Err(HandoffError::NotOwner),
        "a member's device asking"
    );
    let mut offer = parties.offer(1);
    offer.proposed_by.device_label = "forged".into();
    assert_eq!(
        check_offer(ROOM, &parties.governor.cert, &offer),
        Err(HandoffError::ProposerCertificate),
        "a device the owner's root did not certify"
    );
    let mut offer = parties.offer(1);
    offer.recipient = offer.owner;
    assert_eq!(
        check_offer(ROOM, &parties.governor.cert, &offer),
        Err(HandoffError::SameOwner)
    );
    let mut offer = parties.offer(1);
    offer.room = "another".into();
    assert_eq!(
        check_offer(ROOM, &parties.governor.cert, &offer),
        Err(HandoffError::RoomMismatch)
    );
    let mut offer = parties.offer(1);
    offer.expires_at_ns = offer.offered_at_ns + MAX_OFFER_NS + 1;
    assert_eq!(
        check_offer(ROOM, &parties.governor.cert, &offer),
        Err(HandoffError::Expired),
        "open for longer than an offer may be"
    );
}

#[test]
fn the_accepting_device_is_the_recipients_and_signs_its_acceptance() {
    let parties = Parties::new();
    let offer = parties.offer(1);
    let check = |acceptance: &HandoffAcceptance| {
        check_acceptance(ROOM, &parties.governor.cert, &offer, acceptance, 30)
    };

    // The member's device accepting what was offered to the recipient.
    let member = HandoffAcceptance::sign(
        &offer,
        parties.member.cert.clone(),
        hint(&parties.member),
        20,
        &parties.member.key,
    )
    .unwrap();
    assert_eq!(check(&member), Err(HandoffError::SuccessorNotRecipient));

    // A certificate naming the recipient that its root did not sign.
    let impostor_root = SigningKey::from_bytes(&[71; 32]);
    let mut impostor = device(&impostor_root, 72, None);
    impostor.cert.user_public_key = parties.recipient.verifying_key().to_bytes();
    let forged = HandoffAcceptance::sign(
        &offer,
        impostor.cert.clone(),
        hint(&impostor),
        20,
        &impostor.key,
    )
    .unwrap();
    assert_eq!(check(&forged), Err(HandoffError::SuccessorCertificate));

    // Signed by another key than the accepting device's.
    let wrong_signer = HandoffAcceptance::sign(
        &offer,
        parties.successor.cert.clone(),
        hint(&parties.successor),
        20,
        &parties.member.key,
    )
    .unwrap();
    assert_eq!(check(&wrong_signer), Err(HandoffError::AcceptanceSigner));

    // A host hint for another device.
    let elsewhere = HandoffAcceptance::sign(
        &offer,
        parties.successor.cert.clone(),
        hint(&parties.member),
        20,
        &parties.successor.key,
    )
    .unwrap();
    assert_eq!(check(&elsewhere), Err(HandoffError::HintMismatch));
}

#[test]
fn the_governor_alone_checks_the_time_once_when_it_takes_the_acceptance() {
    let parties = Parties::new();
    let offer = parties.offer(1);
    let acceptance = parties.accept(&offer);
    assert_eq!(
        check_acceptance(ROOM, &parties.governor.cert, &offer, &acceptance, 1_000_000),
        Err(HandoffError::Expired)
    );
    assert_eq!(
        check_acceptance(ROOM, &parties.governor.cert, &offer, &acceptance, 15),
        Err(HandoffError::FutureAcceptance),
        "dated after the governor's own now"
    );
    assert_eq!(
        GovernorHandoff::sign(
            offer,
            acceptance,
            parties.governor.cert.clone(),
            2,
            15,
            &parties.governor.key,
        ),
        Err(HandoffError::FutureAcceptance)
    );
}

#[test]
fn a_handoff_is_signed_by_the_governor_it_hands_the_room_from() {
    let parties = Parties::new();
    let offer = parties.offer(1);
    let forged = GovernorHandoff::sign(
        offer.clone(),
        parties.accept(&offer),
        parties.governor.cert.clone(),
        2,
        30,
        &parties.proposer.key,
    )
    .unwrap();
    assert_eq!(
        forged.verify(ROOM, &parties.governor.cert, 0),
        Err(HandoffError::GovernorSigner)
    );
    let handoff = parties.handoff(&offer, 2);
    assert_eq!(
        handoff.verify(ROOM, &parties.proposer.cert, 0),
        Err(HandoffError::NotCurrentGovernor),
        "from a device that does not govern the room"
    );
}

#[test]
fn an_old_handoff_is_not_followed_again() {
    let parties = Parties::new();
    let handoff = parties.handoff(&parties.offer(1), 2);
    assert_eq!(
        handoff.verify(ROOM, &parties.governor.cert, 2),
        Err(HandoffError::Replayed)
    );
    // Followed once: from its successor, nothing more to follow.
    let log = vec![handoff.clone()];
    assert_eq!(follow(ROOM, &parties.governor.cert, &log), log);
    assert!(follow(ROOM, &parties.successor.cert, &log).is_empty());
}

#[test]
fn an_acceptance_names_the_exact_offer_it_accepts() {
    let parties = Parties::new();
    let first = parties.offer(1);
    let second = parties.offer(2);
    let acceptance = parties.accept(&first);
    assert_eq!(
        check_acceptance(ROOM, &parties.governor.cert, &second, &acceptance, 30),
        Err(HandoffError::WrongOffer)
    );
}
