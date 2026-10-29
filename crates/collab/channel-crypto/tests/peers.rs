//! Every participant runs the same [`ChannelPeer`]: any of them commits, every
//! one checks what it receives against the membership, and the group
//! converges whatever order frames arrive in.

use std::collections::BTreeSet;
use std::path::Path;

use channel_crypto::{Access, ChannelPeer, PeerEvent, VaultKey};
use collab_types::{
    ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata,
    MlsGovernanceAction, MlsGovernanceFrame,
};
use ed25519_dalek::SigningKey;

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

struct Device {
    key: SigningKey,
    peer: ChannelPeer,
}

impl Device {
    fn public(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes())
    }

    fn creating(seed: u8, channel: ChannelId) -> Self {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let peer = ChannelPeer::create_now(None, channel, Box::new(key.clone()), &vault()).unwrap();
        Self { key, peer }
    }

    fn waiting(seed: u8, channel: ChannelId) -> Self {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let peer = ChannelPeer::waiting(None, channel, Box::new(key.clone()), &vault()).unwrap();
        Self { key, peer }
    }

    /// Take the frames this device queued, as a transport does.
    fn take(&mut self) -> Vec<MlsGovernanceFrame> {
        let frames = self.peer.outbox().to_vec();
        self.peer.published(&vault()).unwrap();
        frames
    }

    fn ask(&mut self, fresh: bool) -> collab_types::MlsAccessRequestV1 {
        self.peer
            .access_request_now(1, fresh, &vault())
            .unwrap()
            .unwrap()
    }

    fn receive(
        &mut self,
        frame: &MlsGovernanceFrame,
        allowed: &BTreeSet<Ed25519PublicKey>,
    ) -> channel_crypto::Result<PeerEvent> {
        self.peer.receive(frame.clone(), allowed, &vault())
    }

    fn epoch(&self) -> Option<u64> {
        match self.peer.access() {
            Access::Ready { epoch } => Some(epoch),
            Access::Waiting | Access::Removed => None,
        }
    }

    fn seal(&self, text: &str) -> (ArtifactId, EncryptionMetadata, Vec<u8>) {
        let artifact = ArtifactId::new();
        let (metadata, ciphertext) = self
            .peer
            .content_keys()
            .unwrap()
            .seal_content(artifact, ContentRetention::Delta, text.as_bytes())
            .unwrap();
        (artifact, metadata, ciphertext)
    }

    fn opens(&self, sealed: &(ArtifactId, EncryptionMetadata, Vec<u8>)) -> Option<String> {
        let keys = self.peer.content_keys()?;
        keys.open_content(sealed.0, &sealed.1, &sealed.2)
            .ok()
            .map(|plain| String::from_utf8(plain.to_vec()).unwrap())
    }
}

fn allowed(devices: &[&Device]) -> BTreeSet<Ed25519PublicKey> {
    devices.iter().map(|device| device.public()).collect()
}

/// Deliver `frames` to every device, as a host fanning them out does: the
/// copy carrying join material reaches only the devices it adds.
fn deliver(
    frames: &[MlsGovernanceFrame],
    devices: &mut [&mut Device],
    allowed: &BTreeSet<Ed25519PublicKey>,
) {
    for frame in frames {
        for device in devices.iter_mut() {
            let adds_it = frame.added().contains(&device.public());
            if frame.join_material().is_some() != adds_it {
                continue;
            }
            device.receive(frame, allowed).unwrap();
        }
    }
}

#[test]
fn genesis_closes_the_plaintext_gate_for_everyone() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    assert!(alice.peer.gate().encryption_required);
    assert!(!bob.peer.gate().encryption_required);
    let everyone = allowed(&[&alice, &bob]);
    let genesis = alice.take();
    assert_eq!(genesis.len(), 1);
    assert_eq!(
        bob.receive(&genesis[0], &everyone).unwrap(),
        PeerEvent::Genesis
    );
    assert!(bob.peer.gate().encryption_required);
    assert_eq!(
        bob.receive(&genesis[0], &everyone).unwrap(),
        PeerEvent::Duplicate
    );
    assert_eq!(bob.peer.access(), Access::Waiting);
}

#[test]
fn any_participant_admits_and_late_joiners_read_all_past_content() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    let mut carol = Device::waiting(3, channel);
    let first = alice.seal("before anyone joined");

    // Alice admits Bob.
    let mut members = allowed(&[&alice, &bob]);
    let request = bob.ask(false);
    alice.peer.learn_request(request, &vault()).unwrap();
    assert!(
        alice
            .peer
            .reconcile_now(&members, &vault())
            .unwrap()
            .changed()
    );
    let frames = alice.take();
    deliver(&frames, &mut [&mut bob], &members);
    assert_eq!(bob.epoch(), Some(1));
    let second = bob.seal("bob at epoch one");

    // Bob, not the channel's creator, admits Carol.
    members = allowed(&[&alice, &bob, &carol]);
    let request = carol.ask(false);
    bob.peer.learn_request(request.clone(), &vault()).unwrap();
    alice.peer.learn_request(request, &vault()).unwrap();
    let outcome = bob.peer.reconcile_now(&members, &vault()).unwrap();
    assert_eq!(outcome.admitted, vec![carol.public()]);
    let frames = bob.take();
    deliver(&frames, &mut [&mut alice, &mut carol], &members);
    assert_eq!(alice.epoch(), Some(2));
    assert_eq!(carol.epoch(), Some(2));
    // Alice holds Carol's request too, and sees her admitted.
    assert!(!alice.peer.differs(&members));

    for reader in [&alice, &bob, &carol] {
        assert_eq!(
            reader.opens(&first).as_deref(),
            Some("before anyone joined")
        );
        assert_eq!(reader.opens(&second).as_deref(), Some("bob at epoch one"));
    }

    // Carol removes Bob once the membership drops him.
    members = allowed(&[&alice, &carol]);
    let outcome = carol.peer.reconcile_now(&members, &vault()).unwrap();
    assert_eq!(outcome.removed, vec![bob.public()]);
    let frames = carol.take();
    assert_eq!(
        bob.receive(&frames[0], &members).unwrap(),
        PeerEvent::Removed
    );
    deliver(&frames, &mut [&mut alice], &members);
    assert_eq!(alice.epoch(), Some(3));
    let after = alice.seal("after bob left");
    assert_eq!(carol.opens(&after).as_deref(), Some("after bob left"));
    assert!(bob.opens(&after).is_none());
}

#[test]
fn a_commit_the_membership_does_not_back_is_refused() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    let mut eve = Device::waiting(9, channel);
    let both = allowed(&[&alice, &bob]);
    alice.peer.learn_request(bob.ask(false), &vault()).unwrap();
    alice.peer.reconcile_now(&both, &vault()).unwrap();
    deliver(&alice.take(), &mut [&mut bob], &both);
    assert_eq!(bob.epoch(), Some(1));

    // Alice's view lets Eve in; Bob's membership does not.
    let with_eve = allowed(&[&alice, &bob, &eve]);
    alice.peer.learn_request(eve.ask(false), &vault()).unwrap();
    alice.peer.reconcile_now(&with_eve, &vault()).unwrap();
    let frames = alice.take();
    let withheld = frames
        .iter()
        .find(|frame| frame.join_material().is_none())
        .unwrap();
    assert!(bob.receive(withheld, &both).is_err());
    assert_eq!(bob.epoch(), Some(1), "a refused commit changes nothing");
    // Once Bob's membership names Eve too, the same commit applies.
    assert_eq!(
        bob.receive(withheld, &with_eve).unwrap(),
        PeerEvent::Advanced { epoch: 2 }
    );
    let joining = frames
        .iter()
        .find(|frame| frame.join_material().is_some())
        .unwrap();
    assert_eq!(
        eve.receive(joining, &with_eve).unwrap(),
        PeerEvent::Joined { epoch: 2 }
    );

    // A frame declaring other changes than its MLS commit makes is refused.
    let mut dave = Device::waiting(4, channel);
    let all = allowed(&[&alice, &bob, &eve, &dave]);
    alice.peer.learn_request(dave.ask(false), &vault()).unwrap();
    alice.peer.reconcile_now(&all, &vault()).unwrap();
    let real = alice
        .take()
        .into_iter()
        .find(|frame| frame.join_material().is_none())
        .unwrap();
    let MlsGovernanceAction::Commit {
        commit, joining, ..
    } = real.action.clone()
    else {
        panic!("a commit");
    };
    let forged = MlsGovernanceFrame::signed(
        channel,
        real.previous_frame_digest,
        real.epoch,
        MlsGovernanceAction::Commit {
            removed: Vec::new(),
            added: vec![eve.public()],
            commit,
            joining,
            history: None,
        },
        &alice.key,
    )
    .unwrap();
    assert!(bob.receive(&forged, &all).is_err());
    assert_eq!(bob.epoch(), Some(2));
    // A commit signed by someone outside the membership is refused outright.
    assert!(bob.receive(&real, &allowed(&[&bob, &eve, &dave])).is_err());
    assert_eq!(
        bob.receive(&real, &all).unwrap(),
        PeerEvent::Advanced { epoch: 3 }
    );
    let _ = &mut dave;
}

#[test]
fn concurrent_commits_on_one_epoch_converge_on_the_lower_digest() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    let mut carol = Device::waiting(3, channel);
    let mut dave = Device::waiting(4, channel);
    let three = allowed(&[&alice, &bob, &carol]);
    alice.peer.learn_request(bob.ask(false), &vault()).unwrap();
    alice
        .peer
        .learn_request(carol.ask(false), &vault())
        .unwrap();
    alice.peer.reconcile_now(&three, &vault()).unwrap();
    deliver(&alice.take(), &mut [&mut bob, &mut carol], &three);
    assert_eq!((bob.epoch(), carol.epoch()), (Some(1), Some(1)));

    // Dave asks; Alice and Bob both admit him on epoch 1 before either
    // hears of the other's commit.
    let everyone = allowed(&[&alice, &bob, &carol, &dave]);
    let request = dave.ask(false);
    for device in [&mut alice, &mut bob, &mut carol] {
        device
            .peer
            .learn_request(request.clone(), &vault())
            .unwrap();
    }
    alice.peer.reconcile_now(&everyone, &vault()).unwrap();
    bob.peer.reconcile_now(&everyone, &vault()).unwrap();
    let from_alice = alice.take();
    let from_bob = bob.take();
    let head = |frames: &[MlsGovernanceFrame]| frames[0].digest().unwrap();
    let (winner, loser) = if head(&from_alice) < head(&from_bob) {
        (from_alice.clone(), from_bob.clone())
    } else {
        (from_bob.clone(), from_alice.clone())
    };

    // Carol hears the loser first, then the winner; Alice and Bob each hear
    // the other's; Dave gets both Welcomes, the loser's first.
    deliver(&loser, &mut [&mut carol, &mut dave], &everyone);
    deliver(&winner, &mut [&mut carol, &mut dave], &everyone);
    deliver(&from_bob, &mut [&mut alice], &everyone);
    deliver(&from_alice, &mut [&mut bob], &everyone);

    let alice_won = head(&winner) == head(&from_alice);
    let mut devices = [&mut alice, &mut bob, &mut carol];
    for device in &devices {
        assert_eq!(device.epoch(), Some(2));
        assert!(
            device.peer.asking(&everyone).is_empty(),
            "the winner admitted Dave"
        );
    }
    // The writer of the winning commit publishes it again for the hosts
    // that kept the loser, which arrived later.
    let republished: Vec<MlsGovernanceFrame> = devices
        .iter_mut()
        .flat_map(|device| device.take())
        .collect();
    assert!(
        republished
            .iter()
            .all(|frame| frame.digest().unwrap() == head(&winner))
    );
    assert!(!republished.is_empty());

    // Dave joined from the loser's Welcome, then took the winner's.
    assert_eq!(dave.epoch(), Some(2));
    let sealed = devices[0].seal("one epoch for everyone");
    for reader in devices.iter().map(|device| &**device).chain([&dave]) {
        assert_eq!(
            reader.opens(&sealed).as_deref(),
            Some("one epoch for everyone")
        );
    }

    // The writer of the losing commit and Dave hold the losing epoch's
    // key, and hand it to every member, so nothing sealed under it is lost.
    let losing_writer = usize::from(alice_won);
    assert!(devices[losing_writer].peer.differs(&everyone));
    assert!(dave.peer.differs(&everyone));
    settle(
        &mut [&mut alice, &mut bob, &mut carol, &mut dave],
        &everyone,
    );
    for device in [&alice, &bob, &carol, &dave] {
        assert!(!device.peer.differs(&everyone));
    }
}

/// Run the protocol until nothing changes: every waiting device asks, every
/// member reconciles, and every frame reaches every device (the copy
/// carrying join material only the devices it adds). Frames a device cannot
/// use are dropped, as a transport's receiver logs and drops them.
fn settle(devices: &mut [&mut Device], allowed: &BTreeSet<Ed25519PublicKey>) {
    let mut settled = false;
    for _ in 0..32 {
        let requests: Vec<_> = devices
            .iter_mut()
            .filter(|device| device.peer.access() == Access::Waiting)
            .map(|device| device.ask(true))
            .collect();
        for device in devices.iter_mut() {
            for request in &requests {
                if request.device != device.public() {
                    device
                        .peer
                        .learn_request(request.clone(), &vault())
                        .unwrap();
                }
            }
        }
        let mut frames = Vec::new();
        for device in devices.iter_mut() {
            device.peer.reconcile_now(allowed, &vault()).unwrap();
            frames.extend(device.take());
        }
        if frames.is_empty() && requests.is_empty() {
            settled = true;
            break;
        }
        for frame in &frames {
            for device in devices.iter_mut() {
                let adds_it = frame.added().contains(&device.public());
                if frame.join_material().is_some() == adds_it {
                    device.receive(frame, allowed).ok();
                }
            }
        }
    }
    assert!(settled, "the channel never settled");
}

/// Two participants create a channel's group at once and each seals content
/// before hearing of the other: one group remains, and everything written
/// under either genesis opens for every member, a later joiner included.
#[test]
fn concurrent_geneses_leave_one_group_and_all_content_readable() {
    // Which genesis wins is down to its digest: enough channels that both
    // sides win.
    for _ in 0..8 {
        concurrent_geneses_leave_one_group_and_all_content_readable_once();
    }
}

fn concurrent_geneses_leave_one_group_and_all_content_readable_once() {
    let channel = ChannelId::new();
    let mut alice = Device::waiting(1, channel);
    let mut bob = Device::waiting(2, channel);
    assert!(alice.peer.found_now(&vault()).unwrap());
    assert!(bob.peer.found_now(&vault()).unwrap());
    assert!(!alice.peer.found_now(&vault()).unwrap(), "one genesis each");
    let from_alice = alice.seal("from alice's group");
    let from_bob = bob.seal("from bob's group");
    let mut carol = Device::waiting(3, channel);
    let everyone = allowed(&[&alice, &bob, &carol]);
    settle(&mut [&mut alice, &mut bob], &everyone);
    assert_eq!(alice.peer.members(), bob.peer.members());
    assert_eq!(alice.peer.members().len(), 2);
    settle(&mut [&mut alice, &mut bob, &mut carol], &everyone);
    for reader in [&alice, &bob, &carol] {
        assert_eq!(
            reader.opens(&from_alice).as_deref(),
            Some("from alice's group")
        );
        assert_eq!(reader.opens(&from_bob).as_deref(), Some("from bob's group"));
        assert_eq!(reader.epoch(), alice.epoch());
    }
}

/// A device that missed a channel's genesis during an outage creates a
/// second group and writes there. Whichever genesis wins, no one loses what
/// was sealed under the other, and a device that has seen a genesis never
/// creates another.
#[test]
fn a_device_that_missed_the_genesis_destroys_no_history() {
    // Which genesis wins is down to its digest: enough channels that both
    // sides win.
    for _ in 0..8 {
        a_device_that_missed_the_genesis_destroys_no_history_once();
    }
}

fn a_device_that_missed_the_genesis_destroys_no_history_once() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    let mut carol = Device::waiting(3, channel);
    let everyone = allowed(&[&alice, &bob, &carol]);
    settle(&mut [&mut alice, &mut bob], &everyone);
    let before = alice.seal("before the outage");
    let during = bob.seal("during the outage");

    // Carol was offline for all of it and creates the group herself.
    assert!(carol.peer.found_now(&vault()).unwrap());
    let offline = carol.seal("written offline");
    settle(&mut [&mut alice, &mut bob, &mut carol], &everyone);
    for reader in [&alice, &bob, &carol] {
        for (sealed, text) in [
            (&before, "before the outage"),
            (&during, "during the outage"),
            (&offline, "written offline"),
        ] {
            assert_eq!(reader.opens(sealed).as_deref(), Some(text));
        }
    }
    assert_eq!(alice.peer.members().len(), 3);
    for device in [&mut alice, &mut bob, &mut carol] {
        assert!(!device.peer.found_now(&vault()).unwrap());
    }
}

/// A device that cannot name the channel's record follows commits from the
/// group's members, and a genesis of anyone else's does not make it leave
/// its group.
#[test]
fn a_device_without_the_route_follows_commits_and_keeps_its_group() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    let mut carol = Device::waiting(3, channel);
    let everyone = allowed(&[&alice, &bob, &carol]);
    settle(&mut [&mut alice, &mut bob], &everyone);
    let request = carol.ask(false);
    alice.peer.learn_request(request, &vault()).unwrap();
    alice.peer.reconcile_now(&everyone, &vault()).unwrap();
    for frame in alice.take() {
        if frame.join_material().is_none() {
            assert_eq!(
                bob.peer.follow(frame, &vault()).unwrap(),
                PeerEvent::Advanced { epoch: 2 }
            );
        }
    }
    let mut eve = Device::waiting(5, channel);
    assert!(eve.peer.found_now(&vault()).unwrap());
    let genesis = eve.take().remove(0);
    assert_eq!(
        bob.peer.follow(genesis, &vault()).unwrap(),
        PeerEvent::Ignored
    );
    assert_eq!(bob.epoch(), Some(2));
}

#[test]
fn a_member_that_fell_behind_or_lost_its_state_is_admitted_again() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::waiting(2, channel);
    let mut carol = Device::waiting(3, channel);
    let mut dave = Device::waiting(4, channel);
    let history = alice.seal("history");
    let members = allowed(&[&alice, &bob, &carol, &dave]);
    alice.peer.learn_request(bob.ask(false), &vault()).unwrap();
    alice.peer.reconcile_now(&members, &vault()).unwrap();
    deliver(&alice.take(), &mut [&mut bob], &members);

    // Bob misses two commits; the host only kept the newest.
    alice
        .peer
        .learn_request(carol.ask(false), &vault())
        .unwrap();
    alice.peer.reconcile_now(&members, &vault()).unwrap();
    deliver(&alice.take(), &mut [&mut carol], &members);
    alice.peer.learn_request(dave.ask(false), &vault()).unwrap();
    alice.peer.reconcile_now(&members, &vault()).unwrap();
    let newest = alice.take();
    deliver(&newest, &mut [&mut carol, &mut dave], &members);
    let withheld = newest
        .iter()
        .find(|frame| frame.join_material().is_none())
        .unwrap();
    assert_eq!(
        bob.receive(withheld, &members).unwrap(),
        PeerEvent::Desynchronised
    );
    assert_eq!(bob.peer.access(), Access::Waiting);

    // Carol, any participant, admits Bob again with a fresh key: history
    // included.
    carol.peer.learn_request(bob.ask(false), &vault()).unwrap();
    let outcome = carol.peer.reconcile_now(&members, &vault()).unwrap();
    assert_eq!(outcome.admitted, vec![bob.public()]);
    assert_eq!(outcome.removed, vec![bob.public()]);
    deliver(
        &carol.take(),
        &mut [&mut alice, &mut bob, &mut dave],
        &members,
    );
    assert_eq!(bob.opens(&history).as_deref(), Some("history"));
    assert_eq!(bob.epoch(), alice.epoch());

    // Dave never got his Welcome applied (he lost his state): his fresh
    // request is answered again; a stale one is not.
    let mut dave = Device::waiting(4, channel);
    let stale_epoch = alice.epoch();
    assert!(!alice.peer.differs(&members));
    alice.peer.learn_request(dave.ask(true), &vault()).unwrap();
    assert!(alice.peer.differs(&members));
    alice.peer.reconcile_now(&members, &vault()).unwrap();
    deliver(
        &alice.take(),
        &mut [&mut bob, &mut carol, &mut dave],
        &members,
    );
    assert_eq!(dave.epoch(), alice.epoch());
    assert!(alice.epoch() > stale_epoch);
    assert_eq!(dave.opens(&history).as_deref(), Some("history"));
}

#[test]
fn competing_geneses_keep_the_lower_digest() {
    let channel = ChannelId::new();
    let mut alice = Device::creating(1, channel);
    let mut bob = Device::creating(2, channel);
    let both = allowed(&[&alice, &bob]);
    let from_alice = alice.take();
    let from_bob = bob.take();
    let alice_wins = from_alice[0].digest().unwrap() < from_bob[0].digest().unwrap();
    let (mut winner, mut loser, winning, losing) = if alice_wins {
        (alice, bob, from_alice, from_bob)
    } else {
        (bob, alice, from_bob, from_alice)
    };
    assert_eq!(
        winner.receive(&losing[0], &both).unwrap(),
        PeerEvent::Ignored
    );
    assert_eq!(winner.peer.access(), Access::Ready { epoch: 0 });
    assert_eq!(
        loser.receive(&winning[0], &both).unwrap(),
        PeerEvent::Desynchronised
    );
    assert_eq!(loser.peer.access(), Access::Waiting);
    winner
        .peer
        .learn_request(loser.ask(false), &vault())
        .unwrap();
    winner.peer.reconcile_now(&both, &vault()).unwrap();
    deliver(&winner.take(), &mut [&mut loser], &both);
    assert_eq!(loser.epoch(), Some(1));
}

#[test]
fn state_survives_a_restart_and_an_export() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    let channel = ChannelId::new();
    let key = SigningKey::from_bytes(&[1; 32]);
    let created =
        ChannelPeer::create_now(Some(dir), channel, Box::new(key.clone()), &vault()).unwrap();
    let sealed = {
        let device = Device {
            key: key.clone(),
            peer: created,
        };
        device.seal("kept")
    };
    let reopened = reopen(dir, channel, &key).unwrap().unwrap();
    assert_eq!(reopened.access(), Access::Ready { epoch: 0 });
    assert_eq!(
        reopened.outbox().len(),
        1,
        "the unpublished genesis is kept"
    );
    let device = Device {
        key: key.clone(),
        peer: reopened,
    };
    assert_eq!(device.opens(&sealed).as_deref(), Some("kept"));
    let exported = device.peer.export_sealed(&vault()).unwrap();
    let restored =
        ChannelPeer::restore(channel, Box::new(key.clone()), &exported, &vault()).unwrap();
    assert_eq!(restored.gate(), device.peer.gate());
    assert!(
        ChannelPeer::restore(
            channel,
            Box::new(SigningKey::from_bytes(&[2; 32])),
            &exported,
            &vault()
        )
        .is_err()
    );
    assert!(
        ChannelPeer::restore(ChannelId::new(), Box::new(key.clone()), &exported, &vault()).is_err()
    );
    assert!(
        ChannelPeer::restore(
            channel,
            Box::new(key.clone()),
            &exported,
            &VaultKey::new([8; 32])
        )
        .is_err()
    );
    assert!(reopen(dir, ChannelId::new(), &key).unwrap().is_none());
}

fn reopen(
    dir: &Path,
    channel: ChannelId,
    key: &SigningKey,
) -> channel_crypto::Result<Option<ChannelPeer>> {
    ChannelPeer::open(dir, channel, Box::new(key.clone()), &vault())
}

/// A signer that answers on the second poll, as a browser's `WebCrypto` key
/// does through a promise.
struct LateSigner(SigningKey);

impl channel_crypto::DeviceSigner for LateSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> channel_crypto::SignFuture<'_> {
        let key = self.0.clone();
        Box::pin(async move {
            Yield(false).await;
            Ok(ed25519_dalek::Signer::sign(&key, &message).to_bytes())
        })
    }
}

struct Yield(bool);

impl std::future::Future for Yield {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

#[test]
fn a_signer_that_answers_later_signs_every_frame() {
    let channel = ChannelId::new();
    let key = SigningKey::from_bytes(&[1; 32]);
    assert!(
        ChannelPeer::create_now(None, channel, Box::new(LateSigner(key.clone())), &vault())
            .is_err()
    );
    let mut alice = pollster::block_on(ChannelPeer::create(
        None,
        channel,
        Box::new(LateSigner(key.clone())),
        &vault(),
    ))
    .unwrap();
    let mut bob = Device::waiting(2, channel);
    let both: BTreeSet<_> = [
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        bob.public(),
    ]
    .into();
    alice.learn_request(bob.ask(false), &vault()).unwrap();
    pollster::block_on(alice.reconcile(&both, &vault())).unwrap();
    let frames = alice.outbox().to_vec();
    for frame in &frames {
        frame.verify_signature().unwrap();
    }
    deliver(&frames[1..], &mut [&mut bob], &both);
    assert_eq!(bob.epoch(), Some(1));
}
