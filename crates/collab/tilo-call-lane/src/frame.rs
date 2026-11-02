//! A stream's frames: the lane and source they belong to, the track they are
//! sealed under, sealing them, and opening what arrives.

use channel_crypto::{ChannelContentKeys, MEDIA_STREAM_TAG_LEN};
use collab_types::scope::Scope;
use tilo_seal::discord_voice::{TransportCipher, XCHACHA};

use crate::roster::Roster;

/// Bytes a sealed frame adds to its body: epoch (8), sequence (4) and the
/// tag (16).
pub const OVERHEAD: usize = 28;

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

/// Which of a call's two lanes a frame travels on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Lane {
    /// Voice. Frames are 20 ms Opus pictures carried as datagrams, dropped
    /// rather than delayed.
    Audio,
    /// Pictures, on unidirectional streams, one per source, layer and group
    /// of pictures.
    Video,
}

/// One media stream of a call: what its frames carry, and the lane they
/// travel on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// The person's microphone.
    Microphone,
    /// The person's camera.
    Camera,
    /// A screen the person shares.
    Screen,
}

impl Stream {
    /// The lane this stream travels on.
    #[must_use]
    pub const fn lane(self) -> Lane {
        match self {
            Self::Microphone => Lane::Audio,
            Self::Camera | Self::Screen => Lane::Video,
        }
    }

    /// The two bytes a media key is derived for: the lane, then the source
    /// within it. A key is never one another stream's.
    #[must_use]
    pub const fn tag(self) -> [u8; MEDIA_STREAM_TAG_LEN] {
        const AUDIO: u8 = 0;
        const VIDEO: u8 = 1;
        match self {
            Self::Microphone => [AUDIO, 0],
            Self::Camera => [VIDEO, 0],
            Self::Screen => [VIDEO, 1],
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
        }
    }
}

/// What one device seals one stream's frames with: the stream's media key at
/// the epoch the channel is on, derived once, and the track those frames are
/// named as.
///
/// The channel's epoch moves when its membership changes, and with it the
/// key: a carrier that sees a new epoch builds a new sealer.
pub struct Sealer {
    cipher: TransportCipher,
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
        let cipher =
            TransportCipher::new(XCHACHA, key.expose_secret()).map_err(|_| Refusal::Primitive)?;
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

    /// `body` as frame `seq` of the stream: a sender's counter under this
    /// epoch's key, never repeated under one key.
    ///
    /// # Errors
    ///
    /// [`Refusal::Primitive`] if the cipher refuses the frame.
    pub fn seal(&self, seq: u32, body: &[u8]) -> Result<Vec<u8>, Refusal> {
        let sealed = self
            .cipher
            .seal(seq, &aad(self.epoch, seq, self.stream, &self.track), body)
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
        let cipher =
            TransportCipher::new(XCHACHA, key.expose_secret()).map_err(|_| Refusal::Primitive)?;
        cipher
            .open(seq, &aad(epoch, seq, stream, track), sealed)
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
