//! Steward and member state machines driven through their public API:
//! several people, several devices each, frames delivered out of order,
//! twice, and after restarts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use channel_crypto::{Access, ChannelMember, ChannelSteward, MemberEvent, MemberKey, VaultKey};
use collab_types::{
    ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, MlsAccessRequestV1,
    MlsGovernanceFrameV1,
};
use ed25519_dalek::SigningKey;

struct Device {
    key: SigningKey,
    dir: tempfile::TempDir,
}

impl Device {
    fn new(seed: u8) -> Self {
        Self {
            key: SigningKey::from_bytes(&[seed; 32]),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn public(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes())
    }

    fn member(&self, channel: ChannelId, steward: &Device) -> ChannelMember {
        ChannelMember::open(
            self.dir.path(),
            channel,
            self.public(),
            steward.public(),
            &vault(),
        )
        .unwrap()
    }
}

fn vault() -> VaultKey {
    VaultKey::new([3; 32])
}

fn steward(device: &Device, channel: ChannelId) -> ChannelSteward {
    ChannelSteward::open_or_create(device.dir.path(), channel, device.key.clone(), &vault())
        .unwrap()
}

fn request(member: &mut ChannelMember, device: &Device, at: u64) -> MlsAccessRequestV1 {
    member
        .access_request(&device.key, at, &vault())
        .unwrap()
        .expect("a waiting member asks for access")
}

/// Hand every frame the steward still owes `device` to its member, then
/// acknowledge; returns the events.
fn deliver(
    steward: &mut ChannelSteward,
    member: &mut ChannelMember,
    device: &Device,
) -> Vec<MemberEvent> {
    let frames = frames_for(steward, device.public());
    let mut events = Vec::new();
    for frame in frames {
        events.extend(member.receive(frame, &vault()).unwrap());
    }
    if let Some(ack) = member.acknowledgement(&device.key).unwrap() {
        steward.acknowledge(&ack).unwrap();
    }
    events
}

fn frames_for(steward: &ChannelSteward, device: Ed25519PublicKey) -> Vec<MlsGovernanceFrameV1> {
    steward
        .outbox()
        .into_iter()
        .find(|outgoing| outgoing.recipient == device)
        .map(|outgoing| outgoing.frames)
        .unwrap_or_default()
}

fn allowed(devices: &[&Device]) -> BTreeSet<Ed25519PublicKey> {
    devices.iter().map(|device| device.public()).collect()
}

fn requests(
    entries: Vec<(&Device, MlsAccessRequestV1)>,
) -> BTreeMap<Ed25519PublicKey, MlsAccessRequestV1> {
    entries
        .into_iter()
        .map(|(device, request)| (device.public(), request))
        .collect()
}

fn seal(
    steward: &ChannelSteward,
    artifact: ArtifactId,
    text: &[u8],
) -> (collab_types::EncryptionMetadata, Vec<u8>) {
    steward
        .content_keys()
        .unwrap()
        .seal_content(artifact, ContentRetention::Delta, text)
        .unwrap()
}

fn opens(
    member: &ChannelMember,
    artifact: ArtifactId,
    sealed: &(collab_types::EncryptionMetadata, Vec<u8>),
) -> bool {
    member
        .content_keys()
        .is_some_and(|keys| keys.open_content(artifact, &sealed.0, &sealed.1).is_ok())
}

#[test]
fn genesis_closes_the_plaintext_gate_on_the_steward_and_every_member() {
    let channel = ChannelId::new();
    let owner = Device::new(1);
    let laptop = Device::new(2);
    let mut steward = steward(&owner, channel);
    assert!(steward.journal().encryption_required());
    assert!(!steward.journal().accepts_document_encryption(None, channel));

    let mut member = laptop.member(channel, &owner);
    assert_eq!(member.access(), Access::Waiting);
    assert!(member.content_keys().is_none());
    let asked = request(&mut member, &laptop, 1);
    steward
        .reconcile(
            &allowed(&[&laptop]),
            &requests(vec![(&laptop, asked)]),
            &vault(),
        )
        .unwrap();
    deliver(&mut steward, &mut member, &laptop);
    assert!(member.journal().encryption_required());
    assert!(!member.journal().accepts_document_encryption(None, channel));
}

#[test]
fn several_people_with_several_devices_join_read_history_and_lose_access() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let alice_desktop = Device::new(10);
    let alice_laptop = Device::new(11);
    let bob_phone = Device::new(20);
    let bob_desktop = Device::new(21);
    let mut steward = steward(&alice_desktop, channel);
    let before_anyone = seal(&steward, artifact, b"written alone");

    let mut laptop = alice_laptop.member(channel, &alice_desktop);
    let mut phone = bob_phone.member(channel, &alice_desktop);
    let mut desktop = bob_desktop.member(channel, &alice_desktop);
    let pending = requests(vec![
        (&alice_laptop, request(&mut laptop, &alice_laptop, 1)),
        (&bob_phone, request(&mut phone, &bob_phone, 1)),
        (&bob_desktop, request(&mut desktop, &bob_desktop, 1)),
    ]);

    // Only Alice's laptop is allowed so far: Bob was not invited yet.
    let outcome = steward
        .reconcile(&allowed(&[&alice_laptop]), &pending, &vault())
        .unwrap();
    assert_eq!(outcome.admitted, vec![alice_laptop.public()]);
    assert!(
        deliver(&mut steward, &mut laptop, &alice_laptop)
            .iter()
            .any(|event| matches!(event, MemberEvent::Joined { .. }))
    );
    assert!(opens(&laptop, artifact, &before_anyone));
    assert!(frames_for(&steward, alice_laptop.public()).is_empty());

    // Bob is invited: both of his devices are admitted in one reconcile.
    let everyone = allowed(&[&alice_laptop, &bob_phone, &bob_desktop]);
    let outcome = steward.reconcile(&everyone, &pending, &vault()).unwrap();
    assert_eq!(outcome.admitted.len(), 2);
    for (device, member) in [(&bob_phone, &mut phone), (&bob_desktop, &mut desktop)] {
        deliver(&mut steward, member, device);
        assert!(matches!(member.access(), Access::Ready { .. }));
        assert!(
            opens(member, artifact, &before_anyone),
            "joiners read history"
        );
    }
    deliver(&mut steward, &mut laptop, &alice_laptop);
    let epoch = steward.epoch();
    for member in [&laptop, &phone, &desktop] {
        assert_eq!(member.access(), Access::Ready { epoch });
    }
    let shared = seal(&steward, artifact, b"for everyone");
    assert!(opens(&phone, artifact, &shared));

    // Bob's phone is revoked: it keeps what it had, and nothing after.
    let without_phone = allowed(&[&alice_laptop, &bob_desktop]);
    let outcome = steward
        .reconcile(&without_phone, &pending, &vault())
        .unwrap();
    assert_eq!(outcome.removed, vec![bob_phone.public()]);
    assert!(deliver(&mut steward, &mut phone, &bob_phone).contains(&MemberEvent::Removed));
    deliver(&mut steward, &mut laptop, &alice_laptop);
    deliver(&mut steward, &mut desktop, &bob_desktop);
    let after_revocation = seal(&steward, artifact, b"not for the lost phone");
    assert_eq!(phone.access(), Access::Removed);
    assert!(!opens(&phone, artifact, &after_revocation));
    assert!(opens(&desktop, artifact, &after_revocation));
    assert!(opens(&laptop, artifact, &after_revocation));
    // A stale-epoch op from before the removal is refused by every gate.
    assert!(
        !desktop
            .journal()
            .accepts_document_encryption(Some(&shared.0), channel)
    );
    assert!(
        desktop
            .journal()
            .accepts_document_encryption(Some(&after_revocation.0), channel)
    );
    assert!(steward.outbox().is_empty(), "every delivery acknowledged");
}

#[test]
fn delivery_is_idempotent_and_ordered_whatever_the_transport_does() {
    let channel = ChannelId::new();
    let owner = Device::new(30);
    let first = Device::new(31);
    let second = Device::new(32);
    let mut steward = steward(&owner, channel);
    let mut one = first.member(channel, &owner);
    let mut two = second.member(channel, &owner);
    let pending = requests(vec![
        (&first, request(&mut one, &first, 1)),
        (&second, request(&mut two, &second, 1)),
    ]);
    steward
        .reconcile(&allowed(&[&first, &second]), &pending, &vault())
        .unwrap();

    // Both are admitted by one frame, sent after genesis; delivered twice,
    // each is applied exactly once.
    let frames = frames_for(&steward, second.public());
    assert_eq!(frames.len(), 2);
    assert_eq!(frames, frames_for(&steward, first.public()));
    let mut events = Vec::new();
    for frame in frames.iter().chain(frames.iter()) {
        events.extend(two.receive(frame.clone(), &vault()).unwrap());
    }
    assert!(events.contains(&MemberEvent::Duplicate));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, MemberEvent::Joined { .. }))
            .count(),
        1
    );
    assert_eq!(
        two.access(),
        Access::Ready {
            epoch: steward.epoch()
        }
    );
    let ack = two.acknowledgement(&second.key).unwrap().unwrap();
    assert!(steward.acknowledge(&ack).unwrap() > 0);
    assert_eq!(
        steward.acknowledge(&ack).unwrap(),
        0,
        "a repeated ack is a no-op"
    );

    // A frame signed by anyone but the steward is refused outright.
    let forged = MlsGovernanceFrameV1::signed(
        channel,
        [9; 32],
        99,
        two.journal().head_digest(),
        99,
        collab_types::MlsGovernanceAction::Genesis,
        &first.key,
    )
    .unwrap();
    assert!(two.receive(forged, &vault()).is_err());
}

#[test]
fn state_survives_restarts_on_both_sides() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let owner = Device::new(40);
    let tablet = Device::new(41);
    {
        let mut steward = steward(&owner, channel);
        let mut member = tablet.member(channel, &owner);
        let pending = requests(vec![(&tablet, request(&mut member, &tablet, 1))]);
        steward
            .reconcile(&allowed(&[&tablet]), &pending, &vault())
            .unwrap();
        // The frames are delivered only after both sides restarted.
    }
    let mut steward = steward(&owner, channel);
    let mut member = tablet.member(channel, &owner);
    assert_eq!(member.access(), Access::Waiting);
    deliver(&mut steward, &mut member, &tablet);
    drop(member);
    let member = tablet.member(channel, &owner);
    let sealed = seal(&steward, artifact, b"after restart");
    assert!(opens(&member, artifact, &sealed));
    assert!(steward.outbox().is_empty());
}

#[test]
fn a_device_that_lost_its_state_is_admitted_again() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let owner = Device::new(50);
    let phone = Device::new(51);
    let mut steward = steward(&owner, channel);
    let mut member = phone.member(channel, &owner);
    let first = request(&mut member, &phone, 1);
    steward
        .reconcile(
            &allowed(&[&phone]),
            &requests(vec![(&phone, first.clone())]),
            &vault(),
        )
        .unwrap();
    deliver(&mut steward, &mut member, &phone);
    drop(member);

    // The phone is wiped: no member state, no mirror.
    wipe(phone.dir.path());
    let mut member = phone.member(channel, &owner);
    assert_eq!(member.access(), Access::Waiting);
    // The old request replayed by a relay changes nothing.
    let unchanged = steward
        .reconcile(
            &allowed(&[&phone]),
            &requests(vec![(&phone, first)]),
            &vault(),
        )
        .unwrap();
    assert!(!unchanged.changed());
    let again = request(&mut member, &phone, 2);
    // Replaying the journal, with the admission of the key package the
    // phone lost, asks for nothing more: the request it just sent stands.
    let mut replayed = Vec::new();
    for frame in steward.journal().frames().to_vec() {
        replayed.extend(member.receive(frame, &vault()).unwrap());
    }
    assert!(!replayed.contains(&MemberEvent::Desynchronised));
    assert_eq!(member.access(), Access::Waiting);
    let outcome = steward
        .reconcile(
            &allowed(&[&phone]),
            &requests(vec![(&phone, again)]),
            &vault(),
        )
        .unwrap();
    assert_eq!(outcome.removed, vec![phone.public()]);
    assert_eq!(outcome.admitted, vec![phone.public()]);
    deliver(&mut steward, &mut member, &phone);
    let sealed = seal(&steward, artifact, b"welcome back");
    assert!(opens(&member, artifact, &sealed));
}

/// A member that asks again within the same second (its Welcome did not
/// open, or the admission it joined with was replaced) still asks with a
/// newer request, so the steward admits it again instead of leaving it
/// without keys.
#[test]
fn a_member_asking_again_within_a_second_is_admitted_again() {
    let channel = ChannelId::new();
    let owner = Device::new(1);
    let laptop = Device::new(2);
    let mut steward = steward(&owner, channel);
    let mut member = laptop.member(channel, &owner);
    let first = request(&mut member, &laptop, 100);
    let second = request(&mut member, &laptop, 100);
    assert!(second.issued_at > first.issued_at);
    let allowed = allowed(&[&laptop]);
    steward
        .reconcile(&allowed, &requests(vec![(&laptop, first)]), &vault())
        .unwrap();
    let replaced = steward
        .reconcile(&allowed, &requests(vec![(&laptop, second)]), &vault())
        .unwrap();
    assert_eq!(replaced.admitted, vec![laptop.public()]);
    // Only the newest admission is sent, and it names the newer request.
    deliver(&mut steward, &mut member, &laptop);
    assert!(matches!(member.access(), Access::Ready { .. }));
    let artifact = ArtifactId::new();
    let sealed = seal(&steward, artifact, b"after the readmission");
    assert!(opens(&member, artifact, &sealed));
}

/// A device that starts over with no state (a page whose storage was
/// cleared) asks with a fresh key package. Its earlier instance may have
/// minted requests ahead of the clock; the steward still admits the fresh
/// one rather than keep it waiting behind an admission it cannot use.
#[test]
fn a_member_starting_over_behind_its_earlier_requests_is_admitted_again() {
    let channel = ChannelId::new();
    let owner = Device::new(1);
    let laptop = Device::new(2);
    let mut steward = steward(&owner, channel);
    let allowed = allowed(&[&laptop]);
    let mut earlier = laptop.member(channel, &owner);
    // Asked again and again within one second: the requests ran ahead.
    let _ = request(&mut earlier, &laptop, 100);
    let _ = request(&mut earlier, &laptop, 100);
    let ahead = request(&mut earlier, &laptop, 100);
    assert!(ahead.issued_at > 100);
    steward
        .reconcile(
            &allowed,
            &requests(vec![(&laptop, ahead.clone())]),
            &vault(),
        )
        .unwrap();

    // The same device starts over, with none of its earlier state, at the
    // same second.
    let restarted = Device::new(2);
    let mut fresh = restarted.member(channel, &owner);
    let again = request(&mut fresh, &restarted, 100);
    assert!(again.issued_at < ahead.issued_at);
    let readmitted = steward
        .reconcile(&allowed, &requests(vec![(&laptop, again)]), &vault())
        .unwrap();
    assert_eq!(readmitted.admitted, vec![laptop.public()]);
    deliver(&mut steward, &mut fresh, &restarted);
    let artifact = ArtifactId::new();
    let sealed = seal(&steward, artifact, b"after starting over");
    assert!(opens(&fresh, artifact, &sealed));

    // The earlier admission's request, replayed, admits nothing.
    let replayed = steward
        .reconcile(&allowed, &requests(vec![(&laptop, ahead)]), &vault())
        .unwrap();
    assert!(replayed.admitted.is_empty());
}

#[test]
fn a_request_naming_another_device_is_refused() {
    let channel = ChannelId::new();
    let owner = Device::new(60);
    let honest = Device::new(61);
    let thief = Device::new(62);
    let mut steward = steward(&owner, channel);
    // The thief signs a request for its own key but puts in a key package
    // whose credential names the honest device.
    let package = channel_crypto::encode_key_package(
        &MemberKey::for_device(&honest.public())
            .unwrap()
            .fresh_key_package()
            .unwrap(),
    )
    .unwrap();
    let forged = MlsAccessRequestV1::signed(channel, package, 1, &thief.key).unwrap();
    let outcome = steward
        .reconcile(
            &allowed(&[&thief]),
            &requests(vec![(&thief, forged)]),
            &vault(),
        )
        .unwrap();
    assert_eq!(outcome.refused, vec![thief.public()]);
    assert!(!outcome.changed());
    assert_eq!(steward.members(), allowed(&[&owner]));
}

fn wipe(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
}

/// Everyone who asked since the last change is admitted by one frame, the
/// steward keeps only that head, and a device joining later needs nothing
/// but its own admission. A member that missed a commit finds the next one
/// out of reach, waits again, and is admitted afresh.
#[test]
fn a_crowd_joins_in_one_change_and_a_newcomer_needs_only_its_own_welcome() {
    let channel = ChannelId::new();
    let owner = Device::new(40);
    let mut steward = steward(&owner, channel);
    let crowd: Vec<Device> = (41..49).map(Device::new).collect();
    let mut members: Vec<ChannelMember> = crowd
        .iter()
        .map(|device| device.member(channel, &owner))
        .collect();
    let pending = requests(
        crowd
            .iter()
            .zip(members.iter_mut())
            .map(|(device, member)| (device, request(member, device, 1)))
            .collect(),
    );
    let everyone: Vec<&Device> = crowd.iter().collect();
    let outcome = steward
        .reconcile(&allowed(&everyone), &pending, &vault())
        .unwrap();
    assert_eq!(outcome.admitted.len(), crowd.len());
    assert_eq!(steward.epoch(), 1, "one commit for the whole crowd");
    assert_eq!(steward.journal().frames().len(), 2, "genesis and the head");
    for (device, member) in crowd.iter().zip(members.iter_mut()) {
        deliver(&mut steward, member, device);
        assert_eq!(member.access(), Access::Ready { epoch: 1 });
    }

    // A newcomer is admitted in the next change: the crowd's Welcome is
    // gone, and its own is all it receives.
    let newcomer = Device::new(49);
    let mut late = newcomer.member(channel, &owner);
    let mut with_newcomer = everyone.clone();
    with_newcomer.push(&newcomer);
    steward
        .reconcile(
            &allowed(&with_newcomer),
            &requests(vec![(&newcomer, request(&mut late, &newcomer, 1))]),
            &vault(),
        )
        .unwrap();
    assert_eq!(frames_for(&steward, newcomer.public()).len(), 2);
    deliver(&mut steward, &mut late, &newcomer);
    let artifact = ArtifactId::new();
    let sealed = seal(&steward, artifact, b"hello everyone");
    assert!(opens(&late, artifact, &sealed));

    // The first of the crowd misses that commit and the next one.
    let (laggard, rest) = members.split_first_mut().unwrap();
    for (device, member) in crowd.iter().skip(1).zip(rest.iter_mut()) {
        deliver(&mut steward, member, device);
        assert!(opens(member, artifact, &sealed));
    }
    let another = Device::new(50);
    let mut third = another.member(channel, &owner);
    let mut with_another = with_newcomer.clone();
    with_another.push(&another);
    steward
        .reconcile(
            &allowed(&with_another),
            &requests(vec![(&another, request(&mut third, &another, 1))]),
            &vault(),
        )
        .unwrap();
    let events = deliver(&mut steward, laggard, &crowd[0]);
    assert!(events.contains(&MemberEvent::Desynchronised));
    assert_eq!(laggard.access(), Access::Waiting);
    steward
        .reconcile(
            &allowed(&with_another),
            &requests(vec![(&crowd[0], request(laggard, &crowd[0], 2))]),
            &vault(),
        )
        .unwrap();
    deliver(&mut steward, laggard, &crowd[0]);
    let sealed = seal(&steward, artifact, b"welcome back");
    assert!(opens(laggard, artifact, &sealed));
}
