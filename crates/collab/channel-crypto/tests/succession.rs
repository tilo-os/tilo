//! A channel's stewarding moving with its room's governance: the device the
//! room is handed to stewards the channel from the group it holds as a
//! member, under the same channel and epochs, and members follow it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use channel_crypto::{Access, ChannelMember, ChannelSteward, MemberEvent, VaultKey};
use collab_types::{
    ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata,
    MlsAccessRequestV1,
};
use ed25519_dalek::SigningKey;

/// A device: its key, where it keeps a channel as a member, and where it
/// keeps the channels it stewards.
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

    fn members(&self) -> PathBuf {
        self.dir.path().join("member")
    }

    fn stewards(&self) -> PathBuf {
        self.dir.path().join("steward")
    }

    fn member(&self, channel: ChannelId, steward: &Self) -> ChannelMember {
        ChannelMember::open(
            &self.members(),
            channel,
            self.public(),
            steward.public(),
            &vault(),
        )
        .unwrap()
    }

    fn steward(&self, channel: ChannelId) -> ChannelSteward {
        ChannelSteward::open_or_create(&self.stewards(), channel, self.key.clone(), &vault())
            .unwrap()
    }

    fn ask(&self, member: &mut ChannelMember, at: u64) -> MlsAccessRequestV1 {
        member
            .access_request(&self.key, at, &vault())
            .unwrap()
            .expect("a waiting member asks for access")
    }
}

fn vault() -> VaultKey {
    VaultKey::new([5; 32])
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

/// Hand `member` every frame `steward` owes `device`, then acknowledge.
fn deliver(
    steward: &mut ChannelSteward,
    member: &mut ChannelMember,
    device: &Device,
) -> Vec<MemberEvent> {
    let frames = steward
        .outbox()
        .into_iter()
        .find(|outgoing| outgoing.recipient == device.public())
        .map(|outgoing| outgoing.frames)
        .unwrap_or_default();
    let mut events = Vec::new();
    for frame in frames {
        events.extend(member.receive(frame, &vault()).unwrap());
    }
    if let Some(ack) = member.acknowledgement(&device.key).unwrap() {
        steward.acknowledge(&ack).unwrap();
    }
    events
}

type Sealed = (EncryptionMetadata, Vec<u8>);

fn written(member: &ChannelMember, artifact: ArtifactId, text: &[u8]) -> Sealed {
    member
        .content_keys()
        .unwrap()
        .seal_content(artifact, ContentRetention::Delta, text)
        .unwrap()
}

fn reads(
    keys: Option<channel_crypto::ChannelContentKeys>,
    artifact: ArtifactId,
    sealed: &Sealed,
) -> bool {
    keys.is_some_and(|keys| keys.open_content(artifact, &sealed.0, &sealed.1).is_ok())
}

fn exists(dir: &Path, channel: ChannelId, extension: &str) -> bool {
    dir.join(format!("{}.{extension}", channel.0)).exists()
}

#[test]
fn the_new_governor_stewards_the_channel_from_its_own_group_and_members_follow() {
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let old_host = Device::new(1);
    let recipient = Device::new(2);
    let writer = Device::new(3);
    let asking = Device::new(4);
    let later = Device::new(5);

    // Before the move: the old host stewards, the recipient's device and a
    // writer are members, and one device's request is still pending.
    let mut old = old_host.steward(channel);
    let mut successor_view = recipient.member(channel, &old_host);
    let mut writing = writer.member(channel, &old_host);
    let first = requests(vec![
        (&recipient, recipient.ask(&mut successor_view, 1)),
        (&writer, writer.ask(&mut writing, 1)),
    ]);
    old.reconcile(&allowed(&[&recipient, &writer]), &first, &vault())
        .unwrap();
    deliver(&mut old, &mut successor_view, &recipient);
    deliver(&mut old, &mut writing, &writer);
    let mut queued = asking.member(channel, &old_host);
    let pending = asking.ask(&mut queued, 1);
    let before = written(&writing, artifact, b"before the move");
    let epoch = old.epoch();
    assert_eq!(successor_view.access(), Access::Ready { epoch });

    // The room is handed to the recipient's device: its group moves from
    // its member state to the steward's, and the member state is gone.
    let succession = successor_view.into_succession().unwrap();
    assert_eq!(succession.epoch(), epoch);
    let mut steward = ChannelSteward::succeed_in(
        &recipient.stewards(),
        recipient.key.clone(),
        succession,
        &vault(),
    )
    .unwrap();
    ChannelMember::remove_files(&recipient.members(), channel).unwrap();
    assert!(!exists(&recipient.members(), channel, "member"));
    assert!(!exists(&recipient.members(), channel, "mirror"));
    assert!(exists(&recipient.stewards(), channel, "group"));
    assert_eq!(steward.epoch(), epoch);
    assert!(steward.journal().encryption_required());
    assert!(writing.follow_steward(recipient.public()).unwrap());
    assert!(queued.follow_steward(recipient.public()).unwrap());

    // Its first frame takes the old host out, at the next epoch.
    let outcome = steward
        .reconcile(&allowed(&[&writer, &asking]), &BTreeMap::new(), &vault())
        .unwrap();
    assert_eq!(outcome.removed, vec![old_host.public()]);
    assert_eq!(steward.epoch(), epoch + 1);
    assert_eq!(
        deliver(&mut steward, &mut writing, &writer),
        [MemberEvent::Advanced { epoch: epoch + 1 }]
    );

    // The request the old host left pending is answered once the waiting
    // device asks the new steward again.
    let outcome = steward
        .reconcile(
            &allowed(&[&writer, &asking]),
            &requests(vec![(&asking, pending)]),
            &vault(),
        )
        .unwrap();
    assert_eq!(outcome.admitted, vec![asking.public()]);
    assert_eq!(
        deliver(&mut steward, &mut writing, &writer),
        [MemberEvent::Advanced { epoch: epoch + 2 }]
    );
    assert!(
        deliver(&mut steward, &mut queued, &asking)
            .iter()
            .any(|event| matches!(event, MemberEvent::Joined { .. }))
    );
    assert_eq!(
        writing.access(),
        Access::Ready {
            epoch: steward.epoch()
        }
    );

    // The new governor reads what was written before and after the move,
    // and so does the device whose request waited through it.
    let after = written(&writing, artifact, b"after the move");
    for keys in [steward.content_keys().ok(), queued.content_keys()] {
        assert!(reads(keys.clone(), artifact, &before));
        assert!(reads(keys, artifact, &after));
    }
    // The old host reads nothing written after it left.
    assert!(reads(old.content_keys().ok(), artifact, &before));
    assert!(!reads(old.content_keys().ok(), artifact, &after));

    // A device joining after the move reads the whole history.
    let mut joining = later.member(channel, &recipient);
    let asked = later.ask(&mut joining, 2);
    steward
        .reconcile(
            &allowed(&[&writer, &asking, &later]),
            &requests(vec![(&later, asked)]),
            &vault(),
        )
        .unwrap();
    deliver(&mut steward, &mut joining, &later);
    assert!(reads(joining.content_keys(), artifact, &before));
    assert!(reads(joining.content_keys(), artifact, &after));
}

#[test]
fn a_member_follows_only_the_steward_governance_names() {
    let channel = ChannelId::new();
    let old_host = Device::new(11);
    let recipient = Device::new(12);
    let writer = Device::new(13);
    let stranger = Device::new(14);
    let mut old = old_host.steward(channel);
    let mut successor_view = recipient.member(channel, &old_host);
    let mut writing = writer.member(channel, &old_host);
    let first = requests(vec![
        (&recipient, recipient.ask(&mut successor_view, 1)),
        (&writer, writer.ask(&mut writing, 1)),
    ]);
    old.reconcile(&allowed(&[&recipient, &writer]), &first, &vault())
        .unwrap();
    deliver(&mut old, &mut successor_view, &recipient);
    deliver(&mut old, &mut writing, &writer);
    let mut steward = ChannelSteward::succeed_in(
        &recipient.stewards(),
        recipient.key.clone(),
        successor_view.into_succession().unwrap(),
        &vault(),
    )
    .unwrap();
    writing.follow_steward(recipient.public()).unwrap();

    // The old host goes on changing its group: the member takes none of it.
    let mut joining = stranger.member(channel, &old_host);
    let asked = stranger.ask(&mut joining, 1);
    old.reconcile(
        &allowed(&[&recipient, &writer, &stranger]),
        &requests(vec![(&stranger, asked)]),
        &vault(),
    )
    .unwrap();
    let from_old = old
        .outbox()
        .into_iter()
        .find(|outgoing| outgoing.recipient == writer.public())
        .unwrap()
        .frames;
    for frame in from_old {
        assert!(writing.receive(frame, &vault()).is_err());
    }

    // Reopened after a restart under the steward governance names, the
    // member keeps its group and takes the new steward's frames.
    drop(writing);
    let mut writing = writer.member(channel, &recipient);
    steward
        .reconcile(&allowed(&[&writer]), &BTreeMap::new(), &vault())
        .unwrap();
    assert_eq!(
        deliver(&mut steward, &mut writing, &writer),
        [MemberEvent::Advanced {
            epoch: steward.epoch()
        }]
    );
    assert_eq!(
        writing.access(),
        Access::Ready {
            epoch: steward.epoch()
        }
    );
}

#[test]
fn a_device_already_stewarding_the_channel_takes_no_second_group() {
    let channel = ChannelId::new();
    let old_host = Device::new(21);
    let recipient = Device::new(22);
    let mut old = old_host.steward(channel);
    let mut view = recipient.member(channel, &old_host);
    let asked = requests(vec![(&recipient, recipient.ask(&mut view, 1))]);
    old.reconcile(&allowed(&[&recipient]), &asked, &vault())
        .unwrap();
    deliver(&mut old, &mut view, &recipient);
    let _held = recipient.steward(channel);
    assert!(
        ChannelSteward::succeed_in(
            &recipient.stewards(),
            recipient.key.clone(),
            view.into_succession().unwrap(),
            &vault(),
        )
        .is_err()
    );
}

#[test]
fn a_member_not_admitted_has_nothing_to_steward_from() {
    let channel = ChannelId::new();
    let old_host = Device::new(31);
    let recipient = Device::new(32);
    let _old = old_host.steward(channel);
    let view = recipient.member(channel, &old_host);
    assert!(view.into_succession().is_err());
}
