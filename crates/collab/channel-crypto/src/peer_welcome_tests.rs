//! What a waiting device takes from a Welcome, and what it keeps of a join
//! or a commit whose write it put off.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use collab_types::{
    ChannelId, Ed25519PublicKey, JoinMaterial, Joining, MlsAccessRequestV1, MlsGovernanceAction,
    MlsGovernanceFrame, Signature,
};
use ed25519_dalek::{Signer as _, SigningKey};

use super::{Access, ChannelPeer, MemberRecord, PeerEvent, PeerStates};
use crate::{ChannelGroup, ContentKeyring, MemberKey, VaultKey, WelcomeRefusal};

const FOUNDER: u8 = 1;
const HONEST: u8 = 2;
const DISHONEST: u8 = 3;
const JOINER: u8 = 4;
const LATER: u8 = 5;

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

fn device(n: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key(n).verifying_key().to_bytes())
}

fn devices(ns: &[u8]) -> BTreeSet<Ed25519PublicKey> {
    ns.iter().copied().map(device).collect()
}

/// A page's store: what a restart reads back is only what was written.
#[derive(Debug, Default)]
struct Store(Mutex<HashMap<ChannelId, Vec<u8>>>);

impl PeerStates for Store {
    fn read(&self, channel: ChannelId) -> crate::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(&channel).cloned())
    }
    fn write(&self, channel: ChannelId, sealed: &[u8]) -> crate::Result<()> {
        self.0.lock().unwrap().insert(channel, sealed.to_vec());
        Ok(())
    }
    fn forget(&self, channel: ChannelId) -> crate::Result<()> {
        self.0.lock().unwrap().remove(&channel);
        Ok(())
    }
}

/// Device `n` waiting for `channel`, and the request it asks with.
fn asking(
    channel: ChannelId,
    n: u8,
    states: Option<Arc<dyn PeerStates>>,
) -> (ChannelPeer, MlsAccessRequestV1) {
    let mut peer = ChannelPeer::waiting(states, channel, Box::new(key(n)), &vault()).unwrap();
    let request = peer
        .access_request_now(1, false, &vault())
        .unwrap()
        .unwrap();
    (peer, request)
}

/// The copy of `frames` that carries the Welcome, and the members' copy.
fn copies(frames: &[MlsGovernanceFrame]) -> (MlsGovernanceFrame, MlsGovernanceFrame) {
    let full = frames
        .iter()
        .find(|frame| frame.join_material().is_some())
        .unwrap()
        .clone();
    let members = full.withheld().unwrap().unwrap();
    (full, members)
}

/// `frame` as device `n` signs it.
fn signed_by(mut frame: MlsGovernanceFrame, n: u8) -> MlsGovernanceFrame {
    frame.signed_by = device(n);
    frame.signature =
        Signature::from_bytes(key(n).sign(&frame.signing_bytes().unwrap()).to_bytes());
    frame
}

/// `peer`'s group.
fn group(peer: &ChannelPeer) -> &ChannelGroup {
    match &peer.record {
        MemberRecord::Joined(group) => group,
        _ => panic!("not in the group"),
    }
}

/// A channel the founder made and admitted the honest and the dishonest
/// member to.
struct Channel {
    id: ChannelId,
    founder: ChannelPeer,
    honest: ChannelPeer,
    dishonest: ChannelPeer,
}

fn channel() -> Channel {
    let id = ChannelId::new();
    let mut founder = ChannelPeer::create_now(None, id, Box::new(key(FOUNDER)), &vault()).unwrap();
    founder.published();
    let (mut honest, asked) = asking(id, HONEST, None);
    founder.learn_request(asked).unwrap();
    let (mut dishonest, asked) = asking(id, DISHONEST, None);
    founder.learn_request(asked).unwrap();
    let allowed = devices(&[FOUNDER, HONEST, DISHONEST]);
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (full, _) = copies(founder.outbox());
    founder.published();
    for member in [&mut honest, &mut dishonest] {
        let joined = member.receive(full.clone(), &allowed, &vault()).unwrap();
        assert!(matches!(joined, PeerEvent::Joined { .. }), "{joined:?}");
    }
    Channel {
        id,
        founder,
        honest,
        dishonest,
    }
}

/// A member that carries another member's Welcome in a frame of its own is
/// refused: the device that signed the group information must be the one
/// that signed the frame. The device then asks again with a fresh key, and
/// is admitted by an honest commit.
#[test]
fn a_welcome_signed_by_another_device_than_its_frame_is_refused() {
    let Channel {
        id, mut founder, ..
    } = channel();
    let (mut joiner, asked) = asking(id, JOINER, None);
    founder.learn_request(asked).unwrap();
    let allowed = devices(&[FOUNDER, HONEST, DISHONEST, JOINER]);
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (full, _) = copies(founder.outbox());
    founder.published();

    let carried = signed_by(full.clone(), DISHONEST);
    assert_eq!(
        joiner.receive(carried, &allowed, &vault()).unwrap(),
        PeerEvent::WelcomeRefused(WelcomeRefusal::SignedByAnotherDevice)
    );
    assert_eq!(joiner.access(), Access::Waiting);

    // The founder's own frame names the key the joiner gave up.
    assert_eq!(
        joiner.receive(full, &allowed, &vault()).unwrap(),
        PeerEvent::AdmittedWithAnotherKey
    );
    let asked = joiner
        .access_request_now(2, false, &vault())
        .unwrap()
        .unwrap();
    founder.learn_request(asked).unwrap();
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (full, _) = copies(founder.outbox());
    assert!(matches!(
        joiner.receive(full, &allowed, &vault()).unwrap(),
        PeerEvent::Joined { .. }
    ));
}

/// A Welcome into a group holding a device the membership no longer lets
/// read the channel is refused, though its frame adds only the joiner.
#[test]
fn a_welcome_into_a_tree_with_a_device_not_allowed_is_refused() {
    let Channel {
        id, mut founder, ..
    } = channel();
    let (mut joiner, asked) = asking(id, JOINER, None);
    founder.learn_request(asked).unwrap();
    // The founder still lets the honest member read; the joiner's membership
    // no longer does.
    founder
        .reconcile_now(&devices(&[FOUNDER, HONEST, DISHONEST, JOINER]), &vault())
        .unwrap();
    let (full, _) = copies(founder.outbox());
    let allowed = devices(&[FOUNDER, DISHONEST, JOINER]);
    assert!(super::backs(&full, &allowed), "the frame itself is backed");
    assert_eq!(
        joiner.receive(full, &allowed, &vault()).unwrap(),
        PeerEvent::WelcomeRefused(WelcomeRefusal::DeviceNotAllowed)
    );
    assert_eq!(joiner.access(), Access::Waiting);
}

/// A device that cannot name the channel's record still refuses a Welcome
/// whose group information another device signed.
#[test]
fn a_device_following_without_a_membership_still_checks_the_welcomes_signer() {
    let Channel {
        id, mut founder, ..
    } = channel();
    let (mut joiner, asked) = asking(id, JOINER, None);
    founder.learn_request(asked).unwrap();
    founder
        .reconcile_now(&devices(&[FOUNDER, HONEST, DISHONEST, JOINER]), &vault())
        .unwrap();
    let (full, _) = copies(founder.outbox());
    assert_eq!(
        joiner.follow(signed_by(full, DISHONEST), &vault()).unwrap(),
        PeerEvent::WelcomeRefused(WelcomeRefusal::SignedByAnotherDevice)
    );
}

/// A key package as the dishonest member makes one up: it names `device`
/// and claims `signature_key`, the device's real one, but carries an
/// encryption key the dishonest member holds the secret of. It signs it
/// with its own key, as it cannot sign with the device's.
fn made_up_package(named: u8, signature_key: &[u8]) -> crate::KeyPackage {
    use openmls::prelude::{BasicCredential, CredentialWithKey, KeyPackage};
    let forger = MemberKey::generate("forger").unwrap();
    KeyPackage::builder()
        .build(
            crate::CIPHERSUITE,
            &forger.provider,
            &forger.signer,
            CredentialWithKey {
                credential: BasicCredential::new(device(named).as_bytes().to_vec()).into(),
                signature_key: signature_key.to_vec().into(),
            },
        )
        .unwrap()
        .key_package()
        .clone()
}

/// The committer itself forges the Welcome: its frame carries the real
/// commit that adds the joiner, and a Welcome into a group of its own whose
/// tree keeps the honest member's real signature key with an encryption key
/// the committer holds the secret of. That passes both rules (the committer
/// signed the group information and its frame, and every leaf names a device
/// the membership allows), and the joiner takes it: it leaves leaf
/// signatures unchecked. What it bought the committer is a joiner cut
/// off from the channel, which it could have done by sending nothing: the
/// tree differs from the real one, so its hash and every key derived from it
/// do, and the next real commit does not apply.
#[test]
fn a_tree_the_committer_forged_cannot_follow_the_next_real_commit() {
    let Channel {
        id,
        mut founder,
        honest,
        mut dishonest,
    } = channel();
    let (mut joiner, asked) = asking(id, JOINER, None);
    let package = crate::decode_key_package(&asked.key_package).unwrap();
    dishonest.learn_request(asked).unwrap();
    let allowed = devices(&[FOUNDER, HONEST, DISHONEST, JOINER]);
    dishonest.reconcile_now(&allowed, &vault()).unwrap();
    let (real, _) = copies(dishonest.outbox());

    // The group it forges: itself, the founder and the honest member with
    // their real signature keys and encryption keys the committer holds the
    // secrets of, and the joiner.
    let real_keys = group(&honest).signature_keys();
    let real_key = |n: u8| real_keys[device(n).as_bytes().as_slice()].clone();
    let honest_signature_key = real_key(HONEST);
    let mut forged =
        ChannelGroup::create(id, MemberKey::for_device(&device(DISHONEST)).unwrap()).unwrap();
    let empty = ContentKeyring::new();
    forged
        .stage_commit(
            &[],
            &[
                made_up_package(FOUNDER, &real_key(FOUNDER)),
                made_up_package(HONEST, &honest_signature_key),
            ],
            &empty,
        )
        .unwrap();
    forged.merge_pending_transaction().unwrap();
    let staged = forged.stage_commit(&[], &[package], &empty).unwrap();
    forged.merge_pending_transaction().unwrap();
    let (welcome, key_history) = staged.joining.unwrap();
    assert_eq!(staged.epoch, real.epoch);
    let mut carried = real;
    if let MlsGovernanceAction::Commit { joining, .. } = &mut carried.action {
        *joining = Some(Joining::Material(JoinMaterial {
            welcome,
            key_history,
        }));
    }
    let carried = signed_by(carried, DISHONEST);

    // The members take the real commit it carries.
    let members_copy = carried.withheld().unwrap().unwrap();
    assert!(matches!(
        founder.receive(members_copy, &allowed, &vault()).unwrap(),
        PeerEvent::Advanced { .. }
    ));
    // The joiner takes the forged Welcome: it breaks neither rule.
    assert!(matches!(
        joiner.receive(carried, &allowed, &vault()).unwrap(),
        PeerEvent::Joined { .. }
    ));
    assert_eq!(
        group(&joiner)
            .signature_keys()
            .get(device(HONEST).as_bytes().as_slice()),
        Some(&honest_signature_key),
        "the forged tree keeps the honest member's real signature key"
    );

    // The next real commit is out of its reach.
    let (_, asked) = asking(id, LATER, None);
    founder.learn_request(asked).unwrap();
    let allowed = devices(&[FOUNDER, HONEST, DISHONEST, JOINER, LATER]);
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (_, next) = copies(founder.outbox());
    let epoch = group(&joiner).epoch();
    let followed = joiner.receive(next, &allowed, &vault());
    assert!(
        !matches!(followed, Ok(PeerEvent::Advanced { .. })),
        "{followed:?}"
    );
    if let MemberRecord::Joined(group) = &joiner.record {
        assert_eq!(group.epoch(), epoch);
    }
}

/// A join whose write was put off and lost (the page closed first) leaves
/// the device waiting as it was: the Welcome taken again admits it, and
/// without it the device asks again and is admitted with the channel's
/// history.
#[test]
fn a_join_lost_before_it_was_written_is_asked_for_again() {
    let Channel {
        id, mut founder, ..
    } = channel();
    let store = Arc::new(Store::default());
    let states = || Some(Arc::clone(&store) as Arc<dyn PeerStates>);
    let (mut joiner, asked) = asking(id, JOINER, states());
    founder.learn_request(asked).unwrap();
    let allowed = devices(&[FOUNDER, HONEST, DISHONEST, JOINER]);
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (full, _) = copies(founder.outbox());
    founder.published();
    let written = store.read(id).unwrap();
    assert!(matches!(
        joiner.receive(full.clone(), &allowed, &vault()).unwrap(),
        PeerEvent::Joined { .. }
    ));
    assert!(joiner.has_unsaved());
    assert_eq!(
        store.read(id).unwrap(),
        written,
        "the join is not written yet"
    );
    drop(joiner);

    // The host still holds the frame: taken again, it admits the device.
    let open = || {
        ChannelPeer::open(states(), id, Box::new(key(JOINER)), &vault())
            .unwrap()
            .unwrap()
    };
    let mut reopened = open();
    assert_eq!(reopened.access(), Access::Waiting);
    assert!(matches!(
        reopened.receive(full, &allowed, &vault()).unwrap(),
        PeerEvent::Joined { .. }
    ));
    reopened.save_deferred(&vault()).unwrap();
    assert!(!reopened.has_unsaved());
    assert_ne!(store.read(id).unwrap(), written);
    assert_eq!(open().access(), Access::Ready { epoch: 2 });

    // Lost again, and the host has gone on: the device asks again, with a
    // fresh key as a device whose Welcome never came does, and is admitted.
    store.write(id, written.as_deref().unwrap()).unwrap();
    let mut reopened = open();
    let asked = reopened
        .access_request_now(2, true, &vault())
        .unwrap()
        .unwrap();
    founder.learn_request(asked).unwrap();
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (full, _) = copies(founder.outbox());
    assert!(matches!(
        reopened.receive(full, &allowed, &vault()).unwrap(),
        PeerEvent::Joined { epoch: 3 }
    ));
    let (first, _) = reopened.keyring().unwrap().epoch_range().unwrap();
    assert!(
        first <= 2,
        "the epoch the lost join had is readable: {first}"
    );
}

/// A commit whose write was put off and lost leaves the member where it
/// was: the commit taken again moves it on, and a member that missed it
/// starts over and asks again.
#[test]
fn a_commit_lost_before_it_was_written_is_found_again_or_asked_around() {
    let Channel {
        id, mut founder, ..
    } = channel();
    let store = Arc::new(Store::default());
    let states = || Some(Arc::clone(&store) as Arc<dyn PeerStates>);
    let (mut member, asked) = asking(id, JOINER, states());
    founder.learn_request(asked).unwrap();
    let mut allowed = devices(&[FOUNDER, HONEST, DISHONEST, JOINER]);
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (full, _) = copies(founder.outbox());
    founder.published();
    member.receive(full, &allowed, &vault()).unwrap();
    member.save_deferred(&vault()).unwrap();
    let written = store.read(id).unwrap();

    let (_, asked) = asking(id, LATER, None);
    founder.learn_request(asked).unwrap();
    allowed.insert(device(LATER));
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (_, commit) = copies(founder.outbox());
    founder.published();
    assert_eq!(
        member.receive(commit.clone(), &allowed, &vault()).unwrap(),
        PeerEvent::Advanced { epoch: 3 }
    );
    assert_eq!(
        store.read(id).unwrap(),
        written,
        "the commit is not written yet"
    );
    drop(member);

    let open = || {
        ChannelPeer::open(states(), id, Box::new(key(JOINER)), &vault())
            .unwrap()
            .unwrap()
    };
    let mut reopened = open();
    assert_eq!(reopened.access(), Access::Ready { epoch: 2 });
    assert_eq!(
        reopened.receive(commit, &allowed, &vault()).unwrap(),
        PeerEvent::Advanced { epoch: 3 }
    );

    // The host went on past it: the member starts over.
    let mut reopened = open();
    allowed.remove(&device(LATER));
    founder.reconcile_now(&allowed, &vault()).unwrap();
    let (_, beyond) = copies_or_only(founder.outbox());
    assert_eq!(
        reopened.receive(beyond, &allowed, &vault()).unwrap(),
        PeerEvent::Desynchronised
    );
    assert_eq!(reopened.access(), Access::Waiting);
}

/// The commit of `frames` as members get it, when it carries no Welcome.
fn copies_or_only(frames: &[MlsGovernanceFrame]) -> (MlsGovernanceFrame, MlsGovernanceFrame) {
    if frames.iter().any(|frame| frame.join_material().is_some()) {
        return copies(frames);
    }
    let frame = frames.first().unwrap().clone();
    (frame.clone(), frame)
}
