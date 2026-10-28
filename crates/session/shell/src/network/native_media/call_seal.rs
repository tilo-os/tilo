use channel_crypto::ChannelContentKeys;
use collab_types::{ArtifactId, ChannelId};
use tilo_moq::video::{Source, VideoObject};

use crate::network::sealed_picture;
use crate::shell::native_media::CallTarget;
use crate::shell::state::RoomId;

const CALL_PICTURE_KIND: u8 = 0x43;
const CALL_AUDIO_KIND: u8 = 0x41;

/// The channel whose keys seal a call's pictures: its record's, which only
/// member devices that read the record hold. A relay holds none, so it
/// forwards pictures it cannot open.
#[derive(Clone, Copy)]
pub(crate) struct CallChannel {
    room: RoomId,
    channel: ChannelId,
}

impl CallChannel {
    pub(crate) fn of(target: &CallTarget) -> Option<Self> {
        let room = RoomId::parse(&target.workspace)?;
        Some(Self {
            room,
            channel: collab_types::document_channel_id(
                &target.app,
                &room.to_string(),
                &target.record,
            ),
        })
    }

    pub(crate) fn keys(self) -> Option<ChannelContentKeys> {
        crate::shell::live_presence::session_keys(self.room, self.channel)
    }

    pub(crate) const fn channel(self) -> ChannelId {
        self.channel
    }
}

/// What this device seals its pictures with, as the publication it sends.
#[derive(Clone)]
pub(crate) struct Sealing {
    pub(crate) keys: ChannelContentKeys,
    pub(crate) publication: String,
}

fn artifact(publication: &str, source: Source, object: &VideoObject) -> ArtifactId {
    sealed_picture::artifact(
        b"tilo-call-picture-v1\0",
        &[
            publication.as_bytes(),
            tilo_moq::video::track(source, object.layer).as_bytes(),
            &[u8::from(object.keyframe)],
            &object.width.to_le_bytes(),
            &object.height.to_le_bytes(),
            &object.timestamp_us.to_le_bytes(),
        ],
        CALL_PICTURE_KIND,
        object.sequence,
    )
}

fn audio_artifact(publication: &str, sequence: u64, timestamp: u64, duration: u16) -> ArtifactId {
    sealed_picture::artifact(
        b"tilo-call-audio-v1\0",
        &[
            publication.as_bytes(),
            &timestamp.to_le_bytes(),
            &duration.to_le_bytes(),
        ],
        CALL_AUDIO_KIND,
        sequence,
    )
}

/// Opus frame `sequence` (at `timestamp`, `duration` samples long) sealed as
/// `sealing`'s publication sends it.
pub(crate) fn seal_audio(
    sealing: &Sealing,
    sequence: u64,
    (timestamp, duration): (u64, u16),
    payload: &[u8],
) -> Option<Vec<u8>> {
    sealed_picture::seal(
        &sealing.keys,
        audio_artifact(&sealing.publication, sequence, timestamp, duration),
        payload,
    )
}

/// The Opus frame in `object`, received from `publication`; `None` when it
/// does not authenticate as that publication's.
pub(crate) fn open_audio(
    keys: &ChannelContentKeys,
    channel: ChannelId,
    publication: &str,
    object: &tilo_moq::voice::VoiceObject,
) -> Option<Vec<u8>> {
    sealed_picture::open(
        keys,
        channel,
        audio_artifact(
            publication,
            object.sequence,
            object.timestamp,
            object.duration,
        ),
        &object.payload,
    )
}

/// `object` with its picture sealed as `source`'s, sent by `sealing`'s
/// publication.
pub(crate) fn seal(sealing: &Sealing, source: Source, object: &VideoObject) -> Option<VideoObject> {
    let payload = sealed_picture::seal(
        &sealing.keys,
        artifact(&sealing.publication, source, object),
        &object.payload,
    )?;
    Some(VideoObject {
        keyframe: object.keyframe,
        codec: object.codec,
        layer: object.layer,
        sequence: object.sequence,
        timestamp_us: object.timestamp_us,
        width: object.width,
        height: object.height,
        payload,
    })
}

/// `object`, received from `publication`, with its picture opened; `None`
/// when it does not authenticate as that publication's `source`.
pub(crate) fn open(
    keys: &ChannelContentKeys,
    channel: ChannelId,
    publication: &str,
    source: Source,
    mut object: VideoObject,
) -> Option<VideoObject> {
    object.payload = sealed_picture::open(
        keys,
        channel,
        artifact(publication, source, &object),
        &object.payload,
    )?;
    Some(object)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;
    use tilo_moq::video::{Codec, Layer};

    fn keys_of(channel: ChannelId, key: u8) -> ChannelContentKeys {
        let wire = (1_u8, channel, 1_u64, vec![(1_u64, [key; 32])]);
        ChannelContentKeys::from_bytes(&postcard::to_stdvec(&wire).unwrap()).unwrap()
    }

    #[test]
    fn a_call_picture_opens_only_as_the_publication_track_and_place_it_was_sealed_as() {
        let target = CallTarget {
            workspace: RoomId::new().to_string(),
            app: "so.tilo.chat".into(),
            record: "general".into(),
            channel: "voice".into(),
        };
        let channel = CallChannel::of(&target).unwrap().channel();
        let keys = keys_of(channel, 7);
        let sealing = Sealing {
            keys: keys.clone(),
            publication: "call/alice".into(),
        };
        let picture = VideoObject {
            keyframe: true,
            codec: Codec::H264,
            layer: Layer::Mid,
            sequence: 4,
            timestamp_us: 1_000,
            width: 640,
            height: 360,
            payload: b"a picture".to_vec(),
        };
        let sealed = seal(&sealing, Source::Camera, &picture).unwrap();
        assert!(
            !sealed
                .payload
                .windows(b"a picture".len())
                .any(|bytes| bytes == b"a picture")
        );
        assert_eq!(
            open(&keys, channel, "call/alice", Source::Camera, sealed.clone()),
            Some(picture)
        );
        assert!(open(&keys, channel, "call/bob", Source::Camera, sealed.clone()).is_none());
        assert!(open(&keys, channel, "call/alice", Source::Screen, sealed.clone()).is_none());
        let moved = |change: fn(&mut VideoObject)| {
            let mut copy = sealed.clone();
            change(&mut copy);
            open(&keys, channel, "call/alice", Source::Camera, copy)
        };
        assert!(moved(|object| object.sequence = 5).is_none());
        assert!(moved(|object| object.layer = Layer::High).is_none());
        assert!(moved(|object| object.keyframe = false).is_none());
        assert!(
            open(
                &keys_of(channel, 8),
                channel,
                "call/alice",
                Source::Camera,
                sealed
            )
            .is_none()
        );
    }

    #[test]
    fn a_call_audio_frame_opens_only_as_the_publication_and_place_it_was_sealed_as() {
        let channel = collab_types::document_channel_id("so.tilo.chat", "room", "general");
        let keys = keys_of(channel, 7);
        let sealing = Sealing {
            keys: keys.clone(),
            publication: "call/alice".into(),
        };
        let payload = seal_audio(&sealing, 12, (11_520, 960), b"an opus frame").unwrap();
        let frame = |sequence: u64, timestamp: u64| tilo_moq::voice::VoiceObject {
            sequence,
            timestamp,
            duration: 960,
            payload: payload.clone(),
        };
        assert_eq!(
            open_audio(&keys, channel, "call/alice", &frame(12, 11_520)).as_deref(),
            Some(b"an opus frame".as_slice())
        );
        assert!(
            open_audio(
                &keys_of(channel, 8),
                channel,
                "call/alice",
                &frame(12, 11_520)
            )
            .is_none()
        );
        assert!(open_audio(&keys, channel, "call/bob", &frame(12, 11_520)).is_none());
        assert!(open_audio(&keys, channel, "call/alice", &frame(13, 11_520)).is_none());
        assert!(open_audio(&keys, channel, "call/alice", &frame(12, 12_480)).is_none());
    }
}
