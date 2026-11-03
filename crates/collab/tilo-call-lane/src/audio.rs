//! A call's voice: one device's microphone sealed into the tilo-voice/1
//! bundles a lane's datagrams carry, and what the other devices hear from it.

use crate::voice::{self, Rebundler, VoiceObject};
use channel_crypto::ChannelContentKeys;

use crate::frame::{self, Opener, Refusal, Sealer, Stream, TrackName};
use collab_types::scope::Scope;

/// One more object than the profile puts in a datagram can repeat: a
/// datagram is always the newest object plus the ones before it.
const MAX_REPEATS: usize = voice::MAX_BUNDLE_OBJECTS - 1;

/// The microphone's track in `scope`.
fn track(scope: &Scope) -> TrackName {
    TrackName::new(scope, Stream::Microphone.track())
}

/// One device's microphone on a lane.
///
/// Frames are numbered and sealed once each, then bundled: a datagram is the
/// newest frame's object followed by up to `repeats` of the objects just
/// before it, so a single lost datagram is repaired by the next one instead
/// of costing 20 ms of silence. Nothing here waits: a frame that does not fit
/// a datagram is refused, and the caller drops it, as a call drops audio it
/// cannot carry.
pub struct Microphone {
    sealer: Sealer,
    /// The sequence of the next frame.
    next: u64,
    /// The objects just sent, which each datagram repeats.
    bundles: Rebundler,
    repeats: usize,
}

impl Microphone {
    /// Seal `sender`'s microphone frames as the audio track of `scope`, under
    /// the epoch `keys` is at, each datagram repeating the `repeats` frames
    /// sealed just before its newest one.
    ///
    /// The scope's epoch moves when its membership does, and with it the key:
    /// a microphone that sees the new keys moves to them with [`Self::rekey`],
    /// so the stream it speaks keeps the place it had on the lane.
    ///
    /// # Errors
    ///
    /// As [`Sealer::new`].
    pub fn new(
        keys: &ChannelContentKeys,
        sender: [u8; 32],
        scope: &Scope,
        repeats: usize,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            sealer: Sealer::new(keys, sender, Stream::Microphone, track(scope))?,
            next: 0,
            bundles: Rebundler::default(),
            repeats: repeats.min(MAX_REPEATS),
        })
    }

    /// Seal this microphone's next frames under `keys` instead, as the same
    /// `sender`: the stream keeps its sequence, so every device hearing it
    /// goes on hearing it across the epoch it moved to.
    ///
    /// The frames bundled under the old epoch's key are dropped: what a
    /// datagram repeats is what the receivers of that epoch already hold.
    ///
    /// # Errors
    ///
    /// As [`Sealer::new`].
    pub fn rekey(
        &mut self,
        keys: &ChannelContentKeys,
        sender: [u8; 32],
        scope: &Scope,
    ) -> Result<(), Refusal> {
        self.sealer = Sealer::new(keys, sender, Stream::Microphone, track(scope))?;
        self.bundles = Rebundler::default();
        Ok(())
    }

    /// The epoch this seals under.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.sealer.epoch()
    }

    /// The datagram carrying `opus` as the next frame, whose first sample is
    /// at `timestamp_48k`.
    ///
    /// `None` when the frame has no datagram to go in: it is larger than a
    /// call's datagram budget, or the bundler has already forwarded it. A
    /// call drops that frame and sends the next; the frame's place in the
    /// stream is spent either way, so the sender's timeline keeps the media
    /// time it was captured at.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoKey`] and [`Refusal::Primitive`], as [`Sealer::seal`].
    pub fn frame(&mut self, timestamp_48k: u64, opus: &[u8]) -> Result<Option<Vec<u8>>, Refusal> {
        let sequence = self.next;
        self.next = sequence.checked_add(1).ok_or(Refusal::Primitive)?;
        let seq = u32::try_from(sequence).map_err(|_| Refusal::Primitive)?;
        let sealed = self.sealer.seal(seq, opus)?;
        let object = VoiceObject {
            sequence,
            timestamp: timestamp_48k,
            duration: voice::FRAME_SAMPLES,
            payload: sealed,
        }
        .encode()
        .map_err(|_| Refusal::Primitive)?;
        let Some(bundle) = self
            .bundles
            .next(&object, self.repeats)
            .map_err(|_| Refusal::Primitive)?
        else {
            return Ok(None);
        };
        // The profile's sender budget: a datagram larger than this is not one
        // the browser and relay side of a call would send either.
        if bundle.len() > voice::MAX_BUNDLE_BYTES {
            return Ok(None);
        }
        Ok(Some(bundle))
    }
}

/// One frame heard from another device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard {
    /// The sender's counter for this frame: which frame of its stream it is.
    pub sequence: u64,
    /// The media time of the frame's first sample, at 48 kHz.
    pub timestamp_48k: u64,
    /// The Opus packet.
    pub opus: Vec<u8>,
}

/// The frames `sender`'s audio datagram carries, newest first.
///
/// A datagram is one tilo-voice/1 bundle, so the frames behind the newest one
/// are repeats of frames already heard: the caller's playout drops the ones
/// it has played, which is what lets a loss be repaired without waiting.
///
/// # Errors
///
/// [`Refusal::OffRoster`] for a sender the roster does not name, [`Refusal::NoKey`]
/// for a frame sealed under an epoch this device holds no key for,
/// [`Refusal::Truncated`] for a datagram that is not a bundle of complete
/// objects, and [`Refusal::Unauthenticated`] for a frame that does not
/// authenticate, or whose clear sequence is not the one it was sealed as, or
/// which was sealed for another track.
pub fn heard(
    opener: &Opener,
    sender: [u8; 32],
    scope: &Scope,
    datagram: &[u8],
) -> Result<Vec<Heard>, Refusal> {
    let track = track(scope);
    let objects = voice::bundle_objects(datagram).map_err(|_| Refusal::Truncated)?;
    let mut heard = Vec::with_capacity(objects.len());
    for object in objects {
        // `bundle_objects` validated the object's header without copying it.
        let object = VoiceObject::decode(object).map_err(|_| Refusal::Truncated)?;
        let opus = opener.open(sender, Stream::Microphone, &track, &object.payload)?;
        let sealed_sequence = frame::sequence(&object.payload).ok_or(Refusal::Truncated)?;
        if u64::from(sealed_sequence) != object.sequence {
            return Err(Refusal::Unauthenticated);
        }
        heard.push(Heard {
            sequence: object.sequence,
            timestamp_48k: object.timestamp,
            opus,
        });
    }
    Ok(heard)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::{Microphone, heard};
    use crate::frame::{Opener, Refusal, Sealer, Stream, TrackName};
    use crate::roster::Roster;
    use crate::voice;
    use channel_crypto::ChannelContentKeys;
    use collab_types::ChannelId;
    use collab_types::scope::Scope;

    /// The workspace the test's call is in: its scope is the call's.
    fn scope() -> Scope {
        Scope::workspace("w1")
    }

    /// The same call's camera, which the microphone's frames must not open as.
    fn camera() -> TrackName {
        TrackName::new(&scope(), Stream::Camera.track())
    }

    const ALICE: [u8; 32] = [1; 32];
    const BOB: [u8; 32] = [2; 32];
    const STRANGER: [u8; 32] = [3; 32];

    fn keys(key: u8) -> ChannelContentKeys {
        keys_at(1, key)
    }

    fn keys_at(epoch: u64, key: u8) -> ChannelContentKeys {
        let wire = (1_u8, ChannelId::new(), epoch, vec![(epoch, [key; 32])]);
        ChannelContentKeys::from_bytes(&postcard::to_stdvec(&wire).unwrap()).unwrap()
    }

    fn ear(keys: ChannelContentKeys, roster: &[[u8; 32]]) -> Opener {
        Opener::new(keys, Roster::new(roster.iter().copied()))
    }

    #[test]
    fn a_microphones_frames_open_at_the_place_and_time_they_were_sent() {
        let keys = keys(7);
        let mut microphone = Microphone::new(&keys, ALICE, &scope(), 0).unwrap();
        let opener = ear(keys, &[ALICE, BOB]);
        let first = microphone.frame(960, b"one opus frame").unwrap().unwrap();
        let second = microphone.frame(1_920, b"another").unwrap().unwrap();
        assert_eq!(
            heard(&opener, ALICE, &scope(), &first).unwrap(),
            vec![super::Heard {
                sequence: 0,
                timestamp_48k: 960,
                opus: b"one opus frame".to_vec()
            }]
        );
        assert_eq!(
            heard(&opener, ALICE, &scope(), &second).unwrap(),
            vec![super::Heard {
                sequence: 1,
                timestamp_48k: 1_920,
                opus: b"another".to_vec()
            }]
        );
    }

    #[test]
    fn a_microphones_stream_keeps_its_place_when_its_key_moves_to_another_epoch() {
        let speaking = keys(7);
        let mut microphone = Microphone::new(&speaking, ALICE, &scope(), 2).unwrap();
        let first = microphone.frame(0, b"hello").unwrap().unwrap();
        assert_eq!(
            heard(&ear(speaking, &[ALICE]), ALICE, &scope(), &first)
                .unwrap()
                .len(),
            1
        );
        // The channel's membership moves: the same stream goes on under the
        // new epoch's key, and the frames it carried before are not repeated
        // under a key the receivers of that epoch cannot open them with.
        let moved = keys_at(2, 7);
        microphone.rekey(&moved, ALICE, &scope()).unwrap();
        assert_eq!(microphone.epoch(), 2);
        let second = microphone.frame(960, b"still here").unwrap().unwrap();
        assert_eq!(
            heard(&ear(moved, &[ALICE]), ALICE, &scope(), &second).unwrap(),
            vec![super::Heard {
                sequence: 1,
                timestamp_48k: 960,
                opus: b"still here".to_vec()
            }]
        );
    }

    #[test]
    fn a_datagram_repeats_the_frames_just_before_its_newest_one() {
        let keys = keys(7);
        let mut microphone = Microphone::new(&keys, ALICE, &scope(), 2).unwrap();
        let opener = ear(keys, &[ALICE]);
        for sequence in 0..3 {
            microphone.frame(sequence * 960, b"frame").unwrap().unwrap();
        }
        let datagram = microphone.frame(2_880, b"newest").unwrap().unwrap();
        assert!(datagram.len() <= voice::MAX_BUNDLE_BYTES);
        let newest_first = heard(&opener, ALICE, &scope(), &datagram).unwrap();
        assert_eq!(
            newest_first
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(newest_first[0].opus, b"newest");
    }

    #[test]
    fn a_third_devices_frames_and_another_streams_frames_open_nothing() {
        let keys = keys(7);
        let mut microphone = Microphone::new(&keys, ALICE, &scope(), 0).unwrap();
        let datagram = microphone.frame(0, b"hello").unwrap().unwrap();
        let opener = ear(keys.clone(), &[ALICE]);
        assert_eq!(
            heard(&opener, STRANGER, &scope(), &datagram),
            Err(Refusal::OffRoster)
        );
        // Alice's voice does not open as her camera, nor under other keys.
        let mouth = Sealer::new(
            &keys,
            ALICE,
            Stream::Microphone,
            TrackName::new(&scope(), Stream::Microphone.track()),
        )
        .unwrap();
        let camera_only = Opener::new(keys, Roster::new([ALICE]));
        let as_camera = mouth.seal(0, b"hello").unwrap();
        assert_eq!(
            camera_only.open(ALICE, Stream::Camera, &camera(), &as_camera),
            Err(Refusal::Unauthenticated)
        );
        // Another device's key ring at the same epoch derives another key.
        assert_eq!(
            heard(
                &ear(keys_of_someone_else(), &[ALICE]),
                ALICE,
                &scope(),
                &datagram
            ),
            Err(Refusal::Unauthenticated)
        );
    }

    fn keys_of_someone_else() -> ChannelContentKeys {
        keys(8)
    }

    #[test]
    fn a_frame_too_large_for_a_datagram_is_refused_rather_than_delayed() {
        let keys = keys(7);
        let mut microphone = Microphone::new(&keys, ALICE, &scope(), 0).unwrap();
        assert_eq!(microphone.frame(0, &[0; voice::MAX_BUNDLE_BYTES]), Ok(None));
        // The refusal does not consume the frame's place: the next one is 1.
        let datagram = microphone.frame(960, b"small").unwrap().unwrap();
        assert_eq!(
            heard(&ear(keys, &[ALICE]), ALICE, &scope(), &datagram).unwrap()[0].sequence,
            1
        );
    }

    #[test]
    fn a_datagram_that_is_not_a_bundle_of_frames_is_refused() {
        let keys = keys(7);
        let opener = ear(keys, &[ALICE]);
        assert_eq!(
            heard(&opener, ALICE, &scope(), &[]),
            Err(Refusal::Truncated)
        );
        assert_eq!(
            heard(&opener, ALICE, &scope(), b"not a voice object at all"),
            Err(Refusal::Truncated)
        );
    }
}
