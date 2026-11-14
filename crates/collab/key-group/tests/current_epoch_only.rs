//! A new write opens only under the workspace's current epoch.

use collab_types::{ArtifactId, ContentRetention, WorkspaceId};
use key_group::{KeyGroup, MemberKey};

#[test]
fn a_payload_sealed_under_an_older_epoch_opens_as_history_but_not_as_a_new_write() {
    let workspace = WorkspaceId::new();
    let artifact = ArtifactId::new();
    let mut group = KeyGroup::create(workspace, MemberKey::generate("owner").unwrap()).unwrap();
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
    assert!(
        after
            .open_current_content(ArtifactId::new(), &metadata, &fresh)
            .is_err(),
        "content sealed for one record does not open as another"
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
