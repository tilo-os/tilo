//! A stream's frames: the lane and source they belong to, the track they are
//! sealed under, sealing them, and opening what arrives.

use channel_crypto::{ChannelContentKeys, MEDIA_STREAM_TAG_LEN, SenderMediaKeys};
use collab_types::scope::Scope;
use tilo_seal::SymmetricKey;
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

/// What a plane seals and opens under.
///
/// A device that holds its scope's channel content keys seals every stream of
/// every sender with them ([`PlaneKeys::Channel`]). A device that may not hold
/// them — a hosted machine confined to its own record — is handed the media
/// keys of exactly its own streams at one epoch ([`PlaneKeys::Sender`]): it
/// seals what it was given, and it opens nobody's frames, its own included.
#[derive(Clone)]
pub enum PlaneKeys {
    /// The scope's channel content keys.
    Channel(ChannelContentKeys),
    /// One sender's media keys, for the streams it may publish.
    Sender(SenderMediaKeys),
}

impl From<ChannelContentKeys> for PlaneKeys {
    fn from(keys: ChannelContentKeys) -> Self {
        Self::Channel(keys)
    }
}

impl From<SenderMediaKeys> for PlaneKeys {
    fn from(keys: SenderMediaKeys) -> Self {
        Self::Sender(keys)
    }
}

impl PlaneKeys {
    /// The epoch these keys seal under.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        match self {
            Self::Channel(keys) => keys.epoch(),
            Self::Sender(keys) => keys.epoch(),
        }
    }

    /// The key `sender`'s frames of `stream` are sealed under at `epoch`.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoKey`] when these keys do not cover `epoch`, `sender` and
    /// `stream`: content keys cover every sender, one sender's media keys only
    /// that sender and the streams it was handed.
    pub fn media_key(
        &self,
        epoch: u64,
        sender: [u8; 32],
        stream: Stream,
    ) -> Result<SymmetricKey, Refusal> {
        let key = match self {
            Self::Channel(keys) => keys.media_key(epoch, sender, stream.tag()),
            Self::Sender(keys) => keys.media_key(epoch, sender, stream.tag()),
        };
        key.map_err(|_| Refusal::NoKey(epoch))
    }
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
    /// One window's page: its paint stream, sealed apart from every other
    /// window's.
    Page,
    /// One window's input events.
    Input,
    /// A low-rate note of what this device is doing.
    Activity,
}

impl Stream {
    /// The streams a device publishes of its own when it holds none of the
    /// workspace's media keys and is lent theirs instead (a session in a
    /// workspace's VM): who is here, its shared windows and its windows'
    /// pages. Never a call's audio or camera, and nothing anyone else sends.
    pub const OWN: [Self; 3] = [Self::Presence, Self::Window, Self::Page];

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
            Self::Page => [PLANE, 3],
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
            Self::Page => "page",
            Self::Presence => "presence",
            Self::Input => "input",
            Self::Activity => "activity",
        }
    }
}

/// What one device seals one stream's frames with: the stream's media key at
/// the epoch its keys are at, derived once, and the track those frames are
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
    /// [`Refusal::NoKey`] when `keys` do not cover this device and this
    /// stream — a device handed its own media keys seals only the streams it
    /// was handed — and [`Refusal::Primitive`] if the cipher cannot be built
    /// for the key.
    pub fn new(
        keys: &PlaneKeys,
        sender: [u8; 32],
        stream: Stream,
        track: TrackName,
    ) -> Result<Self, Refusal> {
        let epoch = keys.epoch();
        let key = keys.media_key(epoch, sender, stream)?;
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

/// What opens the frames a call receives: the keys frames are sealed under,
/// and who may send.
pub struct Opener {
    keys: PlaneKeys,
    roster: Roster,
}

impl Opener {
    /// Open frames under `keys`, from the devices `roster` names.
    #[must_use]
    pub const fn new(keys: PlaneKeys, roster: Roster) -> Self {
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
    /// gets, and what a device handed only its own media keys gets for every
    /// frame it reads. Repeat and reordering are not the frames' business: a
    /// carrier's jitter buffer drops what it has already played.
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
        if matches!(&self.keys, PlaneKeys::Sender(_)) {
            // A device handed only its own media keys holds nobody's: it
            // opens nothing, not even frames it sealed itself.
            return Err(Refusal::NoKey(epoch));
        }
        let key = self.keys.media_key(epoch, sender, stream)?;
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

    fn sealer(keys: &PlaneKeys, track: &str) -> Sealer {
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
        let keys = PlaneKeys::from(keys());
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

    /// A device handed one sender's media keys seals exactly the streams it
    /// was handed, under its own device and epoch, and opens nothing: what it
    /// seals the member that derived them opens, and nothing else does.
    #[test]
    fn a_device_on_its_own_media_keys_seals_only_what_it_was_handed() {
        let channel = keys();
        let epoch = channel.epoch();
        let scope = Scope::workspace(WORKSPACE);
        let presence = TrackName::new(&scope, "presence");
        let handed = channel
            .sender_media_keys(epoch, DEVICE, &[Stream::Presence.tag(), Stream::Page.tag()])
            .unwrap();
        let keys = PlaneKeys::from(handed);
        assert!(Sealer::new(&keys, DEVICE, Stream::Presence, presence.clone()).is_ok());
        assert!(
            Sealer::new(
                &keys,
                DEVICE,
                Stream::Page,
                TrackName::new(&scope, "page/1")
            )
            .is_ok()
        );
        for stream in [Stream::Microphone, Stream::Camera, Stream::Window] {
            assert_eq!(
                Sealer::new(&keys, DEVICE, stream, TrackName::new(&scope, "elsewhere")).err(),
                Some(Refusal::NoKey(epoch)),
                "a stream it was not handed is not one it can seal"
            );
        }
        assert_eq!(
            Sealer::new(&keys, [4; 32], Stream::Presence, presence.clone()).err(),
            Some(Refusal::NoKey(epoch)),
            "one sender's keys speak for that sender alone"
        );

        let sealed = Sealer::new(&keys, DEVICE, Stream::Presence, presence.clone())
            .unwrap()
            .seal(1, b"here")
            .unwrap();
        let member = Opener::new(PlaneKeys::from(channel), Roster::new([DEVICE]));
        assert_eq!(
            member
                .open(DEVICE, Stream::Presence, &presence, &sealed)
                .unwrap(),
            b"here"
        );

        let held = Opener::new(keys, Roster::new([DEVICE]));
        assert_eq!(
            held.open(DEVICE, Stream::Presence, &presence, &sealed),
            Err(Refusal::NoKey(epoch)),
            "it holds nobody's keys, its own included"
        );
    }

    #[test]
    fn a_sealed_frame_opens_only_as_its_sender_stream_track_and_sequence() {
        let keys = PlaneKeys::from(keys());
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
