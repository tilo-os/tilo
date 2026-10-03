//! A new write opens only under the channel's current epoch.

use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::{ArtifactId, ChannelId, ContentRetention};

#[test]
fn a_payload_sealed_under_an_older_epoch_opens_as_history_but_not_as_a_new_write() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let mut group = ChannelGroup::create(channel, MemberKey::generate("owner").unwrap()).unwrap();
    let before = group.content_keys().unwrap();
    let (old_metadata, old) = before
        .seal_content(
            artifact,
            ContentRetention::Delta,
            b"written before the rotation",
        )
        .unwrap();
    assert_eq!(
        before
            .open_current_content(artifact, &old_metadata, &old)
            .unwrap()
            .as_slice(),
        b"written before the rotation"
    );
    let member = MemberKey::generate("member").unwrap();
    group
        .add_member(member.fresh_key_package().unwrap())
        .unwrap();
    let after = group.content_keys().unwrap();
    assert!(after.epoch() > before.epoch());
    assert!(
        after.open_content(artifact, &old_metadata, &old).is_ok(),
        "history stays readable"
    );
    assert!(
        after
            .open_current_content(artifact, &old_metadata, &old)
            .is_err(),
        "a stale epoch is refused for a new write"
    );
    let (metadata, fresh) = after
        .seal_content(artifact, ContentRetention::Delta, b"current")
        .unwrap();
    let mut other = metadata;
    other.channel = ChannelId::new();
    assert!(
        after
            .open_current_content(artifact, &other, &fresh)
            .is_err()
    );
    let mut tampered = fresh.clone();
    tampered[0] ^= 1;
    assert!(
        after
            .open_current_content(artifact, &metadata, &tampered)
            .is_err()
    );
    assert!(
        after
            .open_current_content(ArtifactId::new(), &metadata, &fresh)
            .is_err(),
        "the payload is bound to its artifact"
    );
}
