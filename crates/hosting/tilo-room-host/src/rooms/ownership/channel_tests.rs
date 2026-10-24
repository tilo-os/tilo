//! A room's sealed channels moving with it: the host of the device the room
//! is handed to stewards each channel from the group that device held as
//! its member, members keep what they hold and follow it, and a device
//! joining later reads the channel's whole history.

use std::path::{Path, PathBuf};

use channel_crypto::{ChannelContentKeys, ChannelMember, MemberEvent, VaultKey};
use collab_types::{ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use crate::rooms::actions::tests::admit_granted;
use crate::rooms::fixtures::{TestHost, device_host};
use crate::rooms::ownership::tests::{deliver, replica};
use crate::rooms::tests::signed_pre_admission_by;
use crate::rooms::{HeldChannels, Rooms};

/// A person's own device, certified by their root, that hosts the rooms
/// placed on it and keeps its channels as a member under `dir`.
struct Device {
    root: SigningKey,
    key: SigningKey,
    cert: DeviceCert,
    dir: tempfile::TempDir,
}

impl Device {
    fn new(label: &str) -> Self {
        let root = SigningKey::from_bytes(&rand::random());
        let key = SigningKey::from_bytes(&rand::random());
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        Self {
            root,
            key,
            cert,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn public(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.cert.device_public_key)
    }

    fn members(&self) -> PathBuf {
        self.dir.path().join("member")
    }

    async fn host(&self) -> TestHost {
        device_host(&self.dir.path().join("host"), &self.key, &self.cert).await
    }

    /// Admit `device` to `room`, which `rooms` hosts for this device's
    /// person, through their pre-admission.
    async fn admits(&self, rooms: &Rooms, room: &str, seed: u8, device: &Self) {
        let token = [seed; 8];
        admit_granted(
            rooms,
            &self.cert,
            signed_pre_admission_by(&self.root, room, &token),
            token,
            (&device.key, &device.cert),
        )
        .await;
    }

    /// This device's member state of `channel`, its frames from `steward`.
    fn member(&self, channel: ChannelId, steward: &Self) -> ChannelMember {
        std::fs::create_dir_all(self.members()).unwrap();
        ChannelMember::open(
            &self.members(),
            channel,
            self.public(),
            steward.public(),
            &vault(),
        )
        .unwrap()
    }

    /// Ask `rooms`, governing `room`, for access to `member`'s channel, as
    /// this device's runtime does, and let the room's steward take it.
    async fn ask(&self, rooms: &Rooms, room: &str, member: &mut ChannelMember) {
        let request = member
            .access_request(
                &self.key,
                tilo_clock::unix_now_ns() / 1_000_000_000,
                &vault(),
            )
            .unwrap()
            .expect("a waiting member asks");
        let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::MlsAccessRequest {
            room: room.to_owned(),
            request: Box::new(request),
            delivery: 1,
        })
        .unwrap();
        rooms
            .absorb(room, self.cert.device_public_key, &body)
            .await
            .unwrap();
        rooms.steward_notices(room).await.unwrap();
    }

    /// Apply what `rooms`' steward of `channel` owes this device.
    async fn receive(
        &self,
        rooms: &Rooms,
        room: &str,
        member: &mut ChannelMember,
    ) -> Vec<MemberEvent> {
        let channel = member.channel_id();
        let frames = steward_of(rooms, room)
            .await
            .expect("the room's host stewards its channels")
            .channels
            .lock()
            .await
            .owed(channel, self.public());
        frames
            .into_iter()
            .flat_map(|frame| member.receive(frame, &vault()).unwrap())
            .collect()
    }
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

async fn steward_of(
    rooms: &Rooms,
    room: &str,
) -> Option<std::sync::Arc<crate::rooms::stewardship::Steward>> {
    rooms
        .room(room)
        .await
        .unwrap()
        .lock()
        .await
        .stewardship
        .clone()
}

/// The content keys `rooms` holds as the steward of `room`'s `channel`,
/// and its group's epoch.
async fn steward_keys(rooms: &Rooms, room: &str, channel: ChannelId) -> (ChannelContentKeys, u64) {
    steward_of(rooms, room)
        .await
        .unwrap()
        .channels
        .lock()
        .await
        .keys(channel)
        .unwrap()
}

type Sealed = (EncryptionMetadata, Vec<u8>);

fn seal(keys: &ChannelContentKeys, artifact: ArtifactId, text: &[u8]) -> Sealed {
    keys.seal_content(artifact, ContentRetention::Delta, text)
        .unwrap()
}

fn reads(keys: Option<ChannelContentKeys>, artifact: ArtifactId, sealed: &Sealed) -> bool {
    keys.is_some_and(|keys| keys.open_content(artifact, &sealed.0, &sealed.1).is_ok())
}

fn holds(dir: &Path, channel: ChannelId) -> bool {
    ["member", "mirror"]
        .iter()
        .any(|extension| dir.join(format!("{}.{extension}", channel.0)).exists())
}

#[tokio::test]
async fn the_new_owners_host_stewards_the_channels_from_the_group_its_device_held() {
    let owner = Device::new("owner's desk");
    let recipient = Device::new("recipient's desk");
    let writer = Device::new("writer's laptop");
    let later = Device::new("later's phone");
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    // The owner's desk hosts the room and stewards its channel; the
    // recipient's desk and the writer read it.
    let old = owner.host().await;
    let room = old.rooms().create("Studio").await.unwrap().room;
    owner.admits(old.rooms(), &room, 1, &recipient).await;
    owner.admits(old.rooms(), &room, 2, &writer).await;
    let mut held = recipient.member(channel, &owner);
    let mut writing = writer.member(channel, &owner);
    for (device, member) in [(&recipient, &mut held), (&writer, &mut writing)] {
        device.ask(old.rooms(), &room, member).await;
        let events = device.receive(old.rooms(), &room, member).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, MemberEvent::Joined { .. })),
            "{events:?}"
        );
    }
    let before = seal(
        &writing.content_keys().unwrap(),
        artifact,
        b"before the move",
    );
    let (_, epoch) = steward_keys(old.rooms(), &room, channel).await;
    let (mut recipients_copy, seen) = replica(old.rooms(), &room).await;

    // The room is handed to the recipient's desk: the owner's desk stops
    // stewarding it.
    let new = recipient.host().await;
    let offer = old
        .rooms()
        .propose_ownership(&room, &owner.cert, [1; 32], recipient.cert.user_public_key)
        .await
        .unwrap();
    let acceptance = new.rooms().acceptance(&offer).unwrap();
    old.rooms()
        .accept_ownership(&room, &recipient.cert, [2; 32], acceptance)
        .await
        .unwrap();
    assert!(steward_of(old.rooms(), &room).await.is_none());
    deliver(old.rooms(), &room, &mut recipients_copy, seen).await;

    // Its host takes the room up with the group its device held, which
    // leaves the device's member state: one steward state on the device.
    let succession = held.into_succession().unwrap();
    new.rooms()
        .adopt(
            &room,
            &recipients_copy.durable().unwrap(),
            HeldChannels {
                held: vec![succession],
                unheld: Vec::new(),
            },
        )
        .await
        .unwrap();
    ChannelMember::remove_files(&recipient.members(), channel).unwrap();
    assert!(!holds(&recipient.members(), channel));
    let (keys, at) = steward_keys(new.rooms(), &room, channel).await;
    assert_eq!(at, epoch, "the channel goes on at the epoch it was at");
    assert!(reads(Some(keys), artifact, &before));

    // The writer follows the new governor; a device joining after the move
    // is admitted by it with the whole history.
    assert!(writing.follow_steward(recipient.public()).unwrap());
    recipient.admits(new.rooms(), &room, 3, &later).await;
    let mut joining = later.member(channel, &recipient);
    later.ask(new.rooms(), &room, &mut joining).await;
    assert!(
        later
            .receive(new.rooms(), &room, &mut joining)
            .await
            .iter()
            .any(|event| matches!(event, MemberEvent::Joined { .. }))
    );
    let events = writer.receive(new.rooms(), &room, &mut writing).await;
    assert!(
        events
            .iter()
            .all(|event| matches!(event, MemberEvent::Advanced { .. })),
        "{events:?}"
    );
    let after = seal(
        &writing.content_keys().unwrap(),
        artifact,
        b"after the move",
    );
    let (keys, at) = steward_keys(new.rooms(), &room, channel).await;
    assert!(at > epoch);
    for keys in [Some(keys), joining.content_keys()] {
        assert!(reads(keys.clone(), artifact, &before));
        assert!(reads(keys, artifact, &after));
    }
    new.shutdown().await;
    old.shutdown().await;
}
