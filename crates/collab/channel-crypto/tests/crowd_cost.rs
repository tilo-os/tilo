//! What admitting a device costs a channel's committing participant and its
//! members, counted rather than timed: the same in a group of 300 as in one of
//! 30, and a crowd asking together is admitted in one commit.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use channel_crypto::{ChannelPeer, PeerStates, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsGovernanceFrame};
use ed25519_dalek::SigningKey;

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

/// A store that counts the writes a peer makes.
#[derive(Debug, Default)]
struct Counted {
    writes: AtomicUsize,
}

impl Counted {
    fn writes(&self) -> usize {
        self.writes.load(Ordering::Relaxed)
    }
}

impl PeerStates for Counted {
    fn read(&self, _: ChannelId) -> channel_crypto::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn write(&self, _: ChannelId, _: &[u8]) -> channel_crypto::Result<()> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn forget(&self, _: ChannelId) -> channel_crypto::Result<()> {
        Ok(())
    }
}

fn key(n: u32) -> SigningKey {
    let mut seed = [1u8; 32];
    seed[..4].copy_from_slice(&n.to_le_bytes());
    SigningKey::from_bytes(&seed)
}

fn public(key: &SigningKey) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes())
}

/// A device that asks for the channel, keeping its state in `states`.
fn asking(
    channel: ChannelId,
    n: u32,
    states: Option<Arc<dyn PeerStates>>,
) -> (ChannelPeer, collab_types::MlsAccessRequestV1) {
    let mut peer = ChannelPeer::waiting(states, channel, Box::new(key(n)), &vault()).unwrap();
    let request = peer
        .access_request_now(1, false, &vault())
        .unwrap()
        .unwrap();
    (peer, request)
}

/// What one admission cost.
#[derive(Debug, PartialEq, Eq)]
struct Cost {
    /// Writes of the committer's state, from the requests' arrival until the
    /// commit left.
    committer_writes: usize,
    /// Commits the admission took.
    commits: usize,
    /// Bytes of the commit every member already in receives.
    members_commit_bytes: usize,
    /// Writes of a member's state as it follows the commit.
    member_writes: usize,
}

/// A group of `members` (the committer, one member that follows, and the
/// rest), then `arriving` devices asking together and admitted.
fn admission(members: u32, arriving: u32) -> Cost {
    let channel = ChannelId::new();
    let states = Arc::new(Counted::default());
    let mut committer = ChannelPeer::create_now(
        Some(Arc::clone(&states) as Arc<dyn PeerStates>),
        channel,
        Box::new(key(0)),
        &vault(),
    )
    .unwrap();
    committer.published();
    let mut allowed: BTreeSet<Ed25519PublicKey> = [public(&key(0))].into();
    let follower_states = Arc::new(Counted::default());
    let mut follower = None;
    for n in 1..members {
        let states = (n == 1).then(|| Arc::clone(&follower_states) as Arc<dyn PeerStates>);
        let (peer, request) = asking(channel, n, states);
        committer.learn_request(request).unwrap();
        allowed.insert(public(&key(n)));
        if n == 1 {
            follower = Some(peer);
        }
    }
    committer.reconcile_now(&allowed, &vault()).unwrap();
    let founding: Vec<MlsGovernanceFrame> = committer.outbox().to_vec();
    committer.published();
    let mut follower = follower.unwrap();
    let full = founding
        .iter()
        .find(|frame| frame.join_material().is_some())
        .unwrap();
    follower.receive(full.clone(), &allowed, &vault()).unwrap();

    // The crowd asks, each request on its own, and the committer settles.
    let before = states.writes();
    for n in 0..arriving {
        let (_, request) = asking(channel, 100_000 + n, None);
        committer.learn_request(request).unwrap();
        allowed.insert(public(&key(100_000 + n)));
    }
    let outcome = committer.reconcile_now(&allowed, &vault()).unwrap();
    assert_eq!(outcome.admitted.len(), arriving as usize);
    let frames = committer.outbox().to_vec();
    committer.published();
    // Settled: asking again changes nothing.
    assert!(
        !committer
            .reconcile_now(&allowed, &vault())
            .unwrap()
            .changed()
    );
    let committer_writes = states.writes() - before;
    let members_copy: Vec<&MlsGovernanceFrame> = frames
        .iter()
        .filter(|frame| frame.join_material().is_none())
        .collect();
    let follower_before = follower_states.writes();
    for frame in &members_copy {
        follower
            .receive((*frame).clone(), &allowed, &vault())
            .unwrap();
    }
    Cost {
        committer_writes,
        commits: members_copy.len(),
        members_commit_bytes: members_copy
            .iter()
            .map(|frame| postcard::to_allocvec(frame).unwrap().len())
            .sum(),
        member_writes: follower_states.writes() - follower_before,
    }
}

#[test]
fn admitting_a_device_costs_the_same_at_300_members_as_at_30() {
    let small = admission(30, 1);
    let large = admission(300, 1);
    assert_eq!(
        small,
        Cost {
            committer_writes: 1,
            commits: 1,
            members_commit_bytes: small.members_commit_bytes,
            member_writes: 1,
        },
        "an admission at 30 members"
    );
    assert_eq!(
        large, small,
        "an admission at 300 members against one at 30"
    );
}

#[test]
fn a_crowd_asking_together_is_admitted_in_one_commit() {
    let one = admission(300, 1);
    let crowd = admission(300, 20);
    assert_eq!(crowd.commits, 1, "{crowd:?}");
    assert_eq!(crowd.committer_writes, one.committer_writes, "{crowd:?}");
    assert_eq!(crowd.member_writes, one.member_writes, "{crowd:?}");
}
