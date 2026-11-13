//! CPU instructions per per-channel operation: what one channel (one record,
//! one MLS group) costs a device to create, to ask for, to be admitted to, to
//! be handed its keys, and to follow a membership change. Instructions are the
//! same on any machine at any load, so instructions × channels is what a
//! workspace of `n` channels spends, whatever it runs on.
//!
//! Run with:
//!
//!     cargo bench -p channel-crypto --bench instructions
//!
//! The digital signature, HPKE and MLS work here is deterministic in its code
//! path, so the counts reproduce; setup (key generation, the frames before the
//! measured one) runs with instrumentation off.

#![expect(
    clippy::expect_used,
    reason = "a benchmark fixture must fail rather than measure an incomplete group"
)]
#![expect(
    clippy::exit,
    reason = "iai-callgrind's generated main propagates benchmark child status with process::exit"
)]

use std::collections::BTreeSet;

use channel_crypto::{Access, ChannelPeer, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsGovernanceFrame};
use ed25519_dalek::SigningKey;
use iai_callgrind::{library_benchmark, library_benchmark_group};

struct Fixture {
    channel: ChannelId,
    key: SigningKey,
    vault: VaultKey,
}

fn fixture() -> Box<Fixture> {
    Box::new(Fixture {
        channel: ChannelId::new(),
        key: SigningKey::from_bytes(&[11; 32]),
        vault: VaultKey::new([7; 32]),
    })
}

fn public(key: &SigningKey) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes())
}

fn allowed_two(
    channel: ChannelId,
    vault: &VaultKey,
) -> (ChannelPeer, ChannelPeer, BTreeSet<Ed25519PublicKey>) {
    let founder_key = SigningKey::from_bytes(&[11; 32]);
    let founder = ChannelPeer::create_now(None, channel, Box::new(founder_key.clone()), vault)
        .expect("genesis");
    let joiner_key = SigningKey::from_bytes(&[22; 32]);
    let joiner =
        ChannelPeer::waiting(None, channel, Box::new(joiner_key.clone()), vault).expect("waiting");
    let allowed: BTreeSet<Ed25519PublicKey> = [public(&founder_key), public(&joiner_key)]
        .into_iter()
        .collect();
    (founder, joiner, allowed)
}

// A channel's genesis, as the device that creates it.
#[library_benchmark(setup = fixture)]
#[bench::one()]
fn genesis(fixture: Box<Fixture>) -> Box<Fixture> {
    let peer = ChannelPeer::create_now(
        None,
        fixture.channel,
        Box::new(fixture.key.clone()),
        &fixture.vault,
    )
    .expect("genesis");
    assert!(peer.gate().encryption_required);
    fixture
}

// A device opening a channel it holds nothing of: minting its member key and
// writing its state.
#[library_benchmark(setup = fixture)]
#[bench::one()]
fn waiting(fixture: Box<Fixture>) -> Box<Fixture> {
    let peer = ChannelPeer::waiting(
        None,
        fixture.channel,
        Box::new(fixture.key.clone()),
        &fixture.vault,
    )
    .expect("waiting");
    assert_eq!(peer.access(), Access::Waiting);
    fixture
}

// The access request that asks to be let into one channel.
#[library_benchmark(setup = fixture)]
#[bench::one()]
fn request(fixture: Box<Fixture>) -> Box<Fixture> {
    let mut peer = ChannelPeer::waiting(
        None,
        fixture.channel,
        Box::new(fixture.key.clone()),
        &fixture.vault,
    )
    .expect("waiting");
    let request = peer
        .access_request_now(1, false, &fixture.vault)
        .expect("request")
        .expect("waiting");
    assert!(!request.key_package.is_empty());
    fixture
}

struct Admit {
    founder: ChannelPeer,
    allowed: BTreeSet<Ed25519PublicKey>,
    vault: VaultKey,
}

fn admitting() -> Box<Admit> {
    let fixture = fixture();
    let (mut founder, mut joiner, allowed) = allowed_two(fixture.channel, &fixture.vault);
    let request = joiner
        .access_request_now(1, false, &fixture.vault)
        .expect("request")
        .expect("waiting");
    founder.learn_request(request).expect("learn");
    Box::new(Admit {
        founder,
        allowed,
        vault: fixture.vault,
    })
}

// One member admitting one device: the commit every channel of a workspace
// makes for one joined person, and its welcome.
#[library_benchmark(setup = admitting)]
#[bench::one()]
fn admit(mut fixture: Box<Admit>) -> Box<Admit> {
    let outcome = fixture
        .founder
        .reconcile_now(&fixture.allowed, &fixture.vault)
        .expect("admit");
    assert!(!outcome.admitted.is_empty());
    assert!(!fixture.founder.outbox().is_empty());
    fixture
}

struct Welcome {
    joiner: ChannelPeer,
    frames: Vec<MlsGovernanceFrame>,
    allowed: BTreeSet<Ed25519PublicKey>,
    vault: VaultKey,
}

fn welcomed() -> Box<Welcome> {
    let fixture = fixture();
    let (mut founder, mut joiner, allowed) = allowed_two(fixture.channel, &fixture.vault);
    let request = joiner
        .access_request_now(1, false, &fixture.vault)
        .expect("request")
        .expect("waiting");
    founder.learn_request(request).expect("learn");
    founder
        .reconcile_now(&allowed, &fixture.vault)
        .expect("admit");
    let frames = founder.outbox().to_vec();
    founder.published();
    Box::new(Welcome {
        joiner,
        frames,
        allowed,
        vault: fixture.vault,
    })
}

// The frame that admits a device: its Welcome, the key history of every past
// epoch, and the commit above it.
#[library_benchmark(setup = welcomed)]
#[bench::one()]
fn welcome(mut fixture: Box<Welcome>) -> Box<Welcome> {
    let frame = fixture
        .frames
        .iter()
        .find(|frame| frame.added().contains(&fixture.joiner.device()))
        .expect("the material frame")
        .clone();
    fixture
        .joiner
        .receive(frame, &fixture.allowed, &fixture.vault)
        .expect("welcome");
    assert!(matches!(fixture.joiner.access(), Access::Ready { .. }));
    fixture
}

struct MembershipChange {
    member: ChannelPeer,
    frame: MlsGovernanceFrame,
    allowed: BTreeSet<Ed25519PublicKey>,
    vault: VaultKey,
}

/// A channel of three devices where the second must apply the commit that
/// admits the third: what every other member of a group does for one
/// membership change, with the join material withheld.
fn membership_change() -> Box<MembershipChange> {
    let vault = VaultKey::new([7; 32]);
    let channel = ChannelId::new();
    let founder_key = SigningKey::from_bytes(&[11; 32]);
    let mut founder = ChannelPeer::create_now(None, channel, Box::new(founder_key.clone()), &vault)
        .expect("genesis");
    let second_key = SigningKey::from_bytes(&[22; 32]);
    let mut second =
        ChannelPeer::waiting(None, channel, Box::new(second_key.clone()), &vault).expect("waiting");
    let request = second
        .access_request_now(1, false, &vault)
        .expect("request")
        .expect("waiting");
    founder.learn_request(request).expect("learn");
    let two: BTreeSet<Ed25519PublicKey> = [public(&founder_key), public(&second_key)]
        .into_iter()
        .collect();
    founder.reconcile_now(&two, &vault).expect("admit");
    let frames = founder.outbox().to_vec();
    founder.published();
    for frame in &frames {
        if frame.added().contains(&public(&second_key)) {
            second
                .receive(frame.clone(), &two, &vault)
                .expect("welcome");
        }
    }
    assert!(matches!(second.access(), Access::Ready { .. }));

    let third_key = SigningKey::from_bytes(&[33; 32]);
    let mut third =
        ChannelPeer::waiting(None, channel, Box::new(third_key.clone()), &vault).expect("waiting");
    let request = third
        .access_request_now(1, false, &vault)
        .expect("request")
        .expect("waiting");
    founder.learn_request(request).expect("learn");
    let three: BTreeSet<Ed25519PublicKey> = [
        public(&founder_key),
        public(&second_key),
        public(&third_key),
    ]
    .into_iter()
    .collect();
    founder.reconcile_now(&three, &vault).expect("admit");
    let frame = founder
        .outbox()
        .iter()
        .find(|frame| frame.join_material().is_none())
        .expect("the withheld frame")
        .clone();
    Box::new(MembershipChange {
        member: second,
        frame,
        allowed: three,
        vault,
    })
}

// One other member applying a commit that adds a device.
#[library_benchmark(setup = membership_change)]
#[bench::one()]
fn commit(mut fixture: Box<MembershipChange>) -> Box<MembershipChange> {
    fixture
        .member
        .receive(fixture.frame.clone(), &fixture.allowed, &fixture.vault)
        .expect("commit applies");
    fixture
}

library_benchmark_group!(
    name = channel;
    benchmarks = waiting, genesis, request, admit, welcome, commit
);

mod entry {
    use iai_callgrind::{LibraryBenchmarkConfig, main};

    use super::channel;

    main!(
        config = LibraryBenchmarkConfig::default()
            .pass_through_envs(["VALGRIND_LIB", "TMPDIR", "HOME", "XDG_RUNTIME_DIR"]);
        library_benchmark_groups = channel
    );

    pub(super) fn run() {
        main();
    }
}

fn main() {
    // A test runner lists and runs every target by executing it, and this one
    // measures under Valgrind for minutes. Only a benchmark run, or the
    // callgrind runner re-entering this binary, measures.
    if !std::env::args()
        .skip(1)
        .any(|arg| arg == "--bench" || arg == "--iai-run")
    {
        return;
    }
    entry::run();
}
