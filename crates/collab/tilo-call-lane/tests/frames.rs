//! Call frames between the members of one channel, over real MLS groups: a
//! member's frame seals and opens, a late joiner reads a lagging sender's
//! older-epoch frame, a removed device derives nothing past its removal, and
//! nothing else opens.

#![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(clippy::indexing_slicing, reason = "in a test these are the assertion")]

use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::ChannelId;
use tilo_call_lane::{Lane, OVERHEAD, Opener, Refusal, Roster, Sealer, Stream};

const ALICE: [u8; 32] = [1; 32];
const BOB: [u8; 32] = [2; 32];
const CAROL: [u8; 32] = [3; 32];
const DAVE: [u8; 32] = [4; 32];

fn member(name: &str) -> MemberKey {
    MemberKey::generate(name).unwrap()
}

/// A channel Alice owns.
fn channel_with_owner() -> (ChannelId, ChannelGroup) {
    let channel = ChannelId::new();
    let alice = ChannelGroup::create(channel, member("alice")).unwrap();
    (channel, alice)
}

/// `name` joins `channel` through `alice`, and every existing member in
/// `others` processes the commit: all of them end at the new epoch.
fn join(
    channel: ChannelId,
    alice: &mut ChannelGroup,
    others: &mut [&mut ChannelGroup],
    name: &str,
) -> ChannelGroup {
    let key = member(name);
    let addition = alice.admit(key.fresh_key_package().unwrap()).unwrap();
    for other in others {
        other
            .process_commit(addition.commit.as_ref().unwrap())
            .unwrap();
    }
    ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        key,
    )
    .unwrap()
}

/// Alice and Bob in one channel, both at the same epoch.
fn channel_with_two_members() -> (ChannelId, ChannelGroup, ChannelGroup) {
    let (channel, mut alice) = channel_with_owner();
    let bob = join(channel, &mut alice, &mut [], "bob");
    (channel, alice, bob)
}

fn sealer(group: &ChannelGroup, device: [u8; 32], stream: Stream) -> Sealer {
    Sealer::new(&group.content_keys().unwrap(), device, stream).unwrap()
}

fn opener(keys: channel_crypto::ChannelContentKeys, roster: &[[u8; 32]]) -> Opener {
    Opener::new(keys, Roster::new(roster.iter().copied()))
}

/// What a person gets: someone in the call is heard, and only as themselves.
/// Runs each phase below in order; every phase builds its own members.
#[test]
fn call_frames_seal_and_open_only_for_their_sender_epoch_and_stream() {
    a_frame_seals_and_opens();
    a_joiner_opens_a_lagging_senders_older_epoch_frame();
    a_joiner_without_history_holds_only_the_epoch_it_joined_at();
    a_removed_device_derives_no_key_past_its_removal();
    a_tampered_or_misfiled_frame_is_refused();
    a_frame_from_a_device_off_the_roster_is_refused();
}

fn a_frame_seals_and_opens() {
    let (_channel, alice, bob) = channel_with_two_members();
    let body = b"an opus frame of twenty milliseconds";
    let sealing = sealer(&alice, ALICE, Stream::Microphone);
    assert_eq!(sealing.epoch(), alice.epoch());
    assert_eq!(Stream::Microphone.lane(), Lane::Audio);

    let frame = sealing.seal(0, body).unwrap();
    assert_eq!(frame.len(), OVERHEAD + body.len());
    assert!(
        !frame.windows(body.len()).any(|window| window == body),
        "the body travels sealed"
    );
    assert_eq!(frame[..8], alice.epoch().to_le_bytes(), "the epoch travels");
    assert_eq!(frame[8..12], 0_u32.to_le_bytes(), "the sequence travels");

    let hearing = opener(bob.content_keys().unwrap(), &[ALICE, BOB]);
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &frame).unwrap(),
        body
    );
    // Bob's own frames open too, and none of them as Alice's.
    let bobs = sealer(&bob, BOB, Stream::Microphone)
        .seal(7, b"hello from bob")
        .unwrap();
    assert_eq!(
        hearing.open(BOB, Stream::Microphone, &bobs).unwrap(),
        b"hello from bob"
    );
    assert_eq!(
        hearing.open(BOB, Stream::Microphone, &frame),
        Err(Refusal::Unauthenticated),
        "Alice's frame does not speak as Bob"
    );
}

fn a_joiner_opens_a_lagging_senders_older_epoch_frame() {
    let (channel, mut alice, bob) = channel_with_two_members();
    let lagging = bob.epoch();
    // Carol joins; Bob never sees that commit, so he still seals under the
    // epoch before it.
    let carol = join(channel, &mut alice, &mut [], "carol");
    assert_eq!(bob.epoch(), lagging, "Bob has not caught up");
    assert_eq!(carol.epoch(), lagging + 1);

    let frame = sealer(&bob, BOB, Stream::Camera)
        .seal(3, b"a picture from before the commit")
        .unwrap();
    let hearing = opener(carol.content_keys().unwrap(), &[ALICE, BOB, CAROL]);
    assert_eq!(
        hearing.open(BOB, Stream::Camera, &frame).unwrap(),
        b"a picture from before the commit",
        "Carol holds every epoch's key her welcome carried"
    );
}

fn a_joiner_without_history_holds_only_the_epoch_it_joined_at() {
    let (channel, mut alice, bob) = channel_with_two_members();
    let frame = sealer(&bob, BOB, Stream::Microphone)
        .seal(1, b"before the join")
        .unwrap();
    // Carol joins with the Welcome alone: her keys begin at the epoch she
    // joined at, so the call she hears starts there too.
    let carol_key = member("carol");
    let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
    let carol = ChannelGroup::process_welcome(channel, &addition.welcome, carol_key).unwrap();
    assert_eq!(carol.keyring().len(), 1);

    let hearing = opener(carol.content_keys().unwrap(), &[ALICE, BOB, CAROL]);
    assert_eq!(
        hearing.open(BOB, Stream::Microphone, &frame),
        Err(Refusal::NoKey(bob.epoch()))
    );
}

fn a_removed_device_derives_no_key_past_its_removal() {
    let (channel, mut alice, mut bob) = channel_with_two_members();
    let mut carol = join(channel, &mut alice, &mut [&mut bob], "carol");
    let mut dave = join(channel, &mut alice, &mut [&mut bob, &mut carol], "dave");
    // What Dave holds on the day he is removed.
    let dave_keys = dave.content_keys().unwrap();
    assert_eq!(dave_keys.epoch(), alice.epoch());

    let removal = alice.remove_member(3).unwrap();
    bob.process_commit(&removal).unwrap();
    carol.process_commit(&removal).unwrap();
    dave.process_commit(&removal).unwrap();
    let after = alice.epoch();
    assert_eq!(after, dave_keys.epoch() + 1);
    assert!(
        dave.content_keys().is_err(),
        "a removed member has no keys of its own to hand out"
    );
    assert!(
        dave_keys
            .media_key(after, DAVE, Stream::Microphone.tag())
            .is_err(),
        "the epoch after the removal derives nothing for Dave"
    );

    let frame = sealer(&alice, ALICE, Stream::Microphone)
        .seal(0, b"after the removal")
        .unwrap();
    let hearing = opener(dave_keys, &[ALICE, BOB, CAROL, DAVE]);
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &frame),
        Err(Refusal::NoKey(after)),
        "Dave cannot open what the call says after he was removed"
    );
}

fn a_tampered_or_misfiled_frame_is_refused() {
    let (_channel, alice, bob) = channel_with_two_members();
    let frame = sealer(&alice, ALICE, Stream::Microphone)
        .seal(9, b"an opus frame")
        .unwrap();
    let hearing = opener(bob.content_keys().unwrap(), &[ALICE, BOB]);
    assert!(hearing.open(ALICE, Stream::Microphone, &frame).is_ok());

    let changed = |at: usize| {
        let mut copy = frame.clone();
        copy[at] = copy[at].wrapping_add(1);
        copy
    };
    let sealed_from = OVERHEAD;
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &changed(sealed_from)),
        Err(Refusal::Unauthenticated),
        "a changed body"
    );
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &changed(frame.len() - 1)),
        Err(Refusal::Unauthenticated),
        "a changed tag"
    );
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &changed(8)),
        Err(Refusal::Unauthenticated),
        "another sequence under the same key"
    );
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &changed(0)),
        Err(Refusal::NoKey(alice.epoch() + 1)),
        "another epoch"
    );
    assert_eq!(
        hearing.open(ALICE, Stream::Camera, &frame),
        Err(Refusal::Unauthenticated),
        "the microphone's frame does not open as the camera's"
    );
    assert_eq!(
        hearing.open(BOB, Stream::Microphone, &frame),
        Err(Refusal::Unauthenticated),
        "nor as another device's"
    );
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &frame[..11]),
        Err(Refusal::Truncated)
    );
    assert_eq!(
        hearing.open(ALICE, Stream::Microphone, &frame[..sealed_from + 8]),
        Err(Refusal::Unauthenticated),
        "a frame cut short of its tag"
    );
}

fn a_frame_from_a_device_off_the_roster_is_refused() {
    let (channel, mut alice, mut bob) = channel_with_two_members();
    let mut carol = join(channel, &mut alice, &mut [&mut bob], "carol");
    let dave = join(channel, &mut alice, &mut [&mut bob, &mut carol], "dave");
    // Dave is off the roster this device holds: the workspace removed him.
    // He keeps every key he was given, so his frames still authenticate, and
    // the roster is what refuses them.
    let daves = dave.content_keys().unwrap();
    let frame = Sealer::new(&daves, DAVE, Stream::Screen)
        .unwrap()
        .seal(4, b"a shared screen")
        .unwrap();

    let without_dave = opener(bob.content_keys().unwrap(), &[ALICE, BOB, CAROL]);
    assert_eq!(
        without_dave.open(DAVE, Stream::Screen, &frame),
        Err(Refusal::OffRoster)
    );
    let while_dave_was_a_member = opener(bob.content_keys().unwrap(), &[ALICE, BOB, CAROL, DAVE]);
    assert_eq!(
        while_dave_was_a_member
            .open(DAVE, Stream::Screen, &frame)
            .unwrap(),
        b"a shared screen",
        "the frame itself authenticates: the roster refuses it"
    );
}
