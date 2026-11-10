//! A stream's frames: the lane and source they belong to, the track they are
//! sealed under, sealing them, and opening what arrives.

use channel_crypto::{ChannelContentKeys, MEDIA_STREAM_TAG_LEN};
use collab_types::scope::Scope;
use tilo_seal::live::FrameCipher;

use crate::roster::Roster;

/// Bytes a sealed frame adds to its body: epoch (8), sequence (4), the
/// random nonce (24) and the tag (16).
pub const OVERHEAD: usize = HEADER_LEN + tilo_seal::live::OVERHEAD;

/// Bytes of header before the ciphertext.
const HEADER_LEN: usize = 12;

/// The full name a frame is sealed under: the workspace, the scope within it
/// and the track ([`TrackName::new`]).
///
/// It is the tag's associated data, so a frame sealed for one track never
/// opens as another's: not another window's, and not the same track name in
/// another scope. The device is not in the name — the frame's key is derived
/// for its sender, and [`Roster`] decides who may send at all.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrackName(String);

impl TrackName {
    /// `track`'s name within `scope`: `<workspace>/<scope id>/<track>`, the
    /// namespace the track is published in and the track within it.
    #[must_use]
    pub fn new(scope: &Scope, track: &str) -> Self {
        Self(format!("{}/{}/{track}", scope.workspace_id(), scope.id()))
    }

    /// The name as the tag covers it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TrackName {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(&self.0)
    }
}

/// The sequence the sealed `frame` names in its header: which frame of its
/// sender's stream it is, as the tag authenticates it. `None` when `frame` is
/// shorter than a seal's header.
#[must_use]
pub fn sequence(frame: &[u8]) -> Option<u32> {
    let (_, rest) = frame.split_first_chunk::<8>()?;
    let (seq, _) = rest.split_first_chunk::<4>()?;
    Some(u32::from_le_bytes(*seq))
}

/// Why a frame was not sealed, or not opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// No content key is held for the epoch the frame is sealed under: this
    /// device joined without that epoch's history, or was removed before it.
    #[error("no content key for epoch {0}")]
    NoKey(u64),
    /// The sending device is not on the workspace's roster, so nothing it
    /// sends is opened however well it authenticates.
    #[error("the sending device is not on the roster")]
    OffRoster,
    /// Shorter than a sealed frame's header.
    #[error("the frame is shorter than a sealed frame")]
    Truncated,
    /// Does not authenticate: another key, another stream or position, or
    /// modified bytes.
    #[error("the frame does not authenticate")]
    Unauthenticated,
    /// The sealing primitive refused, which it does not for a 32-byte key
    /// and a frame this size.
    #[error("the frame could not be sealed")]
    Primitive,
}

/// Which lane of a call a frame travels on.
///
/// Voice travels on the workspace's live plane, not on a lane; the place its
/// variant held is kept so a hello written before still reads.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Lane {
    /// The lane voice was carried on, retired.
    RetiredAudio,
    /// Pictures, on unidirectional streams, one per source, layer and group
    /// of pictures.
    Video,
}

/// One media stream of a call: what its frames carry.
///
/// The tags are keys: one derived key per tag, so a stream's frames are never
/// another's. New streams are appended; an existing tag is never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// The person's microphone.
    Microphone,
    /// The person's camera.
    Camera,
    /// A screen the person shares.
    Screen,
    /// A window's pictures: the same objects as a screen layer, sealed apart
    /// so no window's frame opens as another's.
    Window,
    /// Who is here, and what they are doing.
    Presence,
    /// One window's input events.
    Input,
    /// A low-rate note of what this device is doing.
    Activity,
}

impl Stream {
    /// The two bytes a media key is derived for: the kind of media, then the
    /// stream within it. A key is never one another stream's.
    #[must_use]
    pub const fn tag(self) -> [u8; MEDIA_STREAM_TAG_LEN] {
        const AUDIO: u8 = 0;
        const VIDEO: u8 = 1;
        const PLANE: u8 = 2;
        match self {
            Self::Microphone => [AUDIO, 0],
            Self::Camera => [VIDEO, 0],
            Self::Screen => [VIDEO, 1],
            Self::Presence => [AUDIO, 1],
            Self::Window => [PLANE, 0],
            Self::Input => [PLANE, 1],
            Self::Activity => [PLANE, 2],
        }
    }

    /// The track a call lane seals this stream's frames as. The live plane
    /// names its tracks finer (one per window, camera layer and screen layer)
    /// and passes its own name to [`Sealer::new`].
    #[must_use]
    pub const fn track(self) -> &'static str {
        match self {
            Self::Microphone => "audio",
            Self::Camera => "camera",
            Self::Screen => "screen",
            Self::Window => "window",
            Self::Presence => "presence",
            Self::Input => "input",
            Self::Activity => "activity",
        }
    }
}

/// What one device seals one stream's frames with: the stream's media key at
/// the epoch the channel is on, derived once, and the track those frames are
/// named as.
///
/// The key is the same for every session of the device and every track of
/// the stream in that epoch, and each of them counts its own sequence, so the
/// sequence is never the nonce: each frame carries a fresh random one.
///
/// The channel's epoch moves when its membership changes, and with it the
/// key: a carrier that sees a new epoch builds a new sealer.
pub struct Sealer {
    cipher: FrameCipher,
    epoch: u64,
    stream: Stream,
    track: TrackName,
}

impl Sealer {
    /// Seal `stream`'s frames as `sender`, under the epoch `keys` is at, as
    /// the track `track` names.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoKey`] when `keys` holds no key for its own epoch, and
    /// [`Refusal::Primitive`] if the cipher cannot be built for the key.
    pub fn new(
        keys: &ChannelContentKeys,
        sender: [u8; 32],
        stream: Stream,
        track: TrackName,
    ) -> Result<Self, Refusal> {
        let epoch = keys.epoch();
        let key = keys
            .media_key(epoch, sender, stream.tag())
            .map_err(|_| Refusal::NoKey(epoch))?;
        let cipher = FrameCipher::new(&key).map_err(|_| Refusal::Primitive)?;
        Ok(Self {
            cipher,
            epoch,
            stream,
            track,
        })
    }

    /// The epoch this seals under.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The track this seals.
    #[must_use]
    pub fn track(&self) -> &TrackName {
        &self.track
    }

    /// `body` as frame `seq` of the stream: the sender's position in it,
    /// authenticated with the frame and named in its header.
    ///
    /// # Errors
    ///
    /// [`Refusal::Primitive`] if the cipher refuses the frame.
    pub fn seal(&self, seq: u32, body: &[u8]) -> Result<Vec<u8>, Refusal> {
        let sealed = self
            .cipher
            .seal(&aad(self.epoch, seq, self.stream, &self.track), body)
            .map_err(|_| Refusal::Primitive)?;
        let mut frame = Vec::with_capacity(body.len().saturating_add(OVERHEAD));
        frame.extend_from_slice(&self.epoch.to_le_bytes());
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&sealed);
        Ok(frame)
    }
}

/// What opens the frames a call receives: the channel's keys, and who may
/// send.
pub struct Opener {
    keys: ChannelContentKeys,
    roster: Roster,
}

impl Opener {
    /// Open frames under `keys`, from the devices `roster` names.
    #[must_use]
    pub const fn new(keys: ChannelContentKeys, roster: Roster) -> Self {
        Self { keys, roster }
    }

    /// Who may send from now on, under the same keys: a call's roster moves
    /// with its membership while the epoch stays as it was.
    pub fn set_roster(&mut self, roster: Roster) {
        self.roster = roster;
    }

    /// The frame `sender` sealed as `seq` of `stream`, as the track `track`
    /// names, or why it is refused.
    ///
    /// A frame from a device the roster does not name is refused before
    /// anything is derived or decrypted ([`Refusal::OffRoster`]); one sealed
    /// under an epoch this device holds no key for is refused as
    /// [`Refusal::NoKey`], which is what a device removed before that epoch
    /// gets. Repeat and reordering are not the frames' business: a carrier's
    /// jitter buffer drops what it has already played.
    ///
    /// # Errors
    ///
    /// As its description above, and [`Refusal::Unauthenticated`] for a
    /// frame that is another stream's, another track's, another position's,
    /// another epoch's, or one whose bytes were changed.
    pub fn open(
        &self,
        sender: [u8; 32],
        stream: Stream,
        track: &TrackName,
        frame: &[u8],
    ) -> Result<Vec<u8>, Refusal> {
        if !self.roster.admits(sender) {
            return Err(Refusal::OffRoster);
        }
        let (epoch, rest) = frame.split_first_chunk::<8>().ok_or(Refusal::Truncated)?;
        let (seq, sealed) = rest.split_first_chunk::<4>().ok_or(Refusal::Truncated)?;
        let epoch = u64::from_le_bytes(*epoch);
        let seq = u32::from_le_bytes(*seq);
        let key = self
            .keys
            .media_key(epoch, sender, stream.tag())
            .map_err(|_| Refusal::NoKey(epoch))?;
        let cipher = FrameCipher::new(&key).map_err(|_| Refusal::Primitive)?;
        cipher
            .open(&aad(epoch, seq, stream, track), sealed)
            .map_err(|_| Refusal::Unauthenticated)
    }
}

/// What the tag covers besides the body: the epoch and sequence in the
/// header, the stream the frame names, and the track it belongs to.
fn aad(epoch: u64, seq: u32, stream: Stream, track: &TrackName) -> Vec<u8> {
    let name = track.as_str().as_bytes();
    let mut aad = Vec::with_capacity(
        HEADER_LEN
            .saturating_add(MEDIA_STREAM_TAG_LEN)
            .saturating_add(name.len()),
    );
    aad.extend_from_slice(&epoch.to_le_bytes());
    aad.extend_from_slice(&seq.to_le_bytes());
    aad.extend_from_slice(&stream.tag());
    aad.extend_from_slice(name);
    aad
}

#[cfg(test)]
mod tests {
    use channel_crypto::{ChannelGroup, MemberKey};

    use super::*;
    use crate::roster::Roster;

    const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
    const DEVICE: [u8; 32] = [3; 32];

    fn keys() -> ChannelContentKeys {
        let scope = Scope::workspace(WORKSPACE);
        ChannelGroup::create(scope.channel_id(), MemberKey::generate("owner").unwrap())
            .unwrap()
            .content_keys()
            .unwrap()
    }

    fn sealer(keys: &ChannelContentKeys, track: &str) -> Sealer {
        let name = TrackName::new(&Scope::workspace(WORKSPACE), track);
        Sealer::new(keys, DEVICE, Stream::Presence, name).unwrap()
    }

    /// The bytes after the clear header: what the stream cipher produced.
    fn ciphertext(frame: &[u8]) -> &[u8] {
        frame.get(HEADER_LEN..).unwrap()
    }

    /// Two sessions of one device (a restart, a second tab) in one epoch
    /// both start their sequence at 1 under the same derived key, and two
    /// tracks of one stream (camera layers, windows) count independently. A
    /// nonce taken from the sequence would seal both under one keystream:
    /// the XOR of the ciphertexts would be the XOR of the plaintexts.
    #[test]
    fn frames_with_one_sequence_under_one_key_never_share_a_keystream() {
        let keys = keys();
        let first = sealer(&keys, "presence");
        let second = sealer(&keys, "presence");
        let other_track = sealer(&keys, "presence/2");
        let a = b"state=present, window=notes".as_slice();
        let b = b"state=away,    window=files".as_slice();
        let plain_xor: Vec<u8> = a.iter().zip(b).map(|(x, y)| x ^ y).collect();

        for other in [&second, &other_track] {
            let sealed_a = first.seal(1, a).unwrap();
            let sealed_b = other.seal(1, b).unwrap();
            let cipher_xor: Vec<u8> = ciphertext(&sealed_a)
                .iter()
                .zip(ciphertext(&sealed_b))
                .map(|(x, y)| x ^ y)
                .collect();
            assert!(
                !cipher_xor.windows(plain_xor.len()).any(|w| w == plain_xor),
                "two frames sealed as sequence 1 reused one keystream"
            );
        }

        // The same body sealed twice at one sequence never repeats either.
        assert_ne!(first.seal(1, a).unwrap(), first.seal(1, a).unwrap());
    }

    #[test]
    fn a_sealed_frame_opens_only_as_its_sender_stream_track_and_sequence() {
        let keys = keys();
        let scope = Scope::workspace(WORKSPACE);
        let name = TrackName::new(&scope, "presence");
        let frame = sealer(&keys, "presence").seal(7, b"here").unwrap();
        let opener = Opener::new(keys, Roster::new([DEVICE]));
        assert_eq!(sequence(&frame), Some(7));
        assert_eq!(
            opener
                .open(DEVICE, Stream::Presence, &name, &frame)
                .unwrap(),
            b"here"
        );
        assert_eq!(
            opener.open(DEVICE, Stream::Camera, &name, &frame),
            Err(Refusal::Unauthenticated)
        );
        let other = TrackName::new(&scope, "presence/2");
        assert_eq!(
            opener.open(DEVICE, Stream::Presence, &other, &frame),
            Err(Refusal::Unauthenticated)
        );
        let mut moved = frame.clone();
        moved[8] ^= 1;
        assert_eq!(
            opener.open(DEVICE, Stream::Presence, &name, &moved),
            Err(Refusal::Unauthenticated)
        );
        assert_eq!(
            opener.open([4; 32], Stream::Presence, &name, &frame),
            Err(Refusal::OffRoster)
        );
        let mut changed = frame;
        let last = changed.len() - 1;
        changed[last] ^= 1;
        assert_eq!(
            opener.open(DEVICE, Stream::Presence, &name, &changed),
            Err(Refusal::Unauthenticated)
        );
    }
}
