//! A page's side of a call: sealing what its microphone and camera make as
//! the device that bridges it, opening what the call's other devices send,
//! and the exchange the two sides make on every turn.
//!
//! A page is a member of its channel and holds its content keys, so it seals
//! and opens its call's media itself, exactly as a native device does: what
//! it hands the bridge is already sealed. The bridge copies those frames onto
//! the call's lane and copies back what arrives, derives nothing and holds no
//! key of the call.
//!
//! The frames name the bridge's device as their sender, because that is the
//! device the lane's connections come from and whose membership the
//! workspace's roster admits. A receiving device derives the key it opens them
//! with for that device, and the frame's tag still bounds every frame to its
//! stream, its sequence and its epoch.
//!
//! [`Up`] and [`Down`] are what the two sides say on one turn, encoded with
//! postcard over whatever carries the page: the page's session to its host.

use std::collections::BTreeMap;

use crate::picture::{Codec, Layer, Source, VideoObject};
use channel_crypto::ChannelContentKeys;

use crate::audio::{self, Heard, Microphone};
use crate::frame::{Opener, Refusal, Sealer, Stream};
use crate::roster::Roster;
use crate::wire::{layer_of_wire, layer_wire, source_of_wire, stream_of};

/// The route a page posts its turns to, below the host's API prefix.
pub const PATH: &str = "/call/lane";

/// The microphones a page's frames repeat, so one lost datagram costs no
/// silence: the call's own count.
pub const REPEATS: usize = 2;

/// The view of a page's call: what it seals its microphone and pictures with,
/// and what opens what the call sends.
///
/// The page's app owns the capture, the encoder, the decoder and the playout;
/// this seals what it made and opens what arrived, which is all a page needs
/// of the call's key schedule.
pub struct Page {
    workspace: String,
    call: String,
    /// The device bridging the page: whose key its frames are sealed under,
    /// and who the lane's other devices see them come from.
    bridge: [u8; 32],
    opener: Opener,
    microphone: Microphone,
    /// What seals the page's pictures, and the next frame of each source: its
    /// layers are sealed under the one key.
    pictures: BTreeMap<Source, (Sealer, u32)>,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("call", &self.call)
            .field("epoch", &self.microphone.epoch())
            .finish_non_exhaustive()
    }
}

impl Page {
    /// The page's side of `workspace`'s call `call`, sealed as `bridge` under
    /// the channel keys `keys`, for the devices `roster` names.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoKey`] when `keys` holds no key for its own epoch, and
    /// [`Refusal::Primitive`] if the cipher cannot be built for it.
    pub fn new(
        workspace: String,
        call: String,
        bridge: [u8; 32],
        keys: &ChannelContentKeys,
        roster: Roster,
    ) -> Result<Self, Refusal> {
        let mut page = Self {
            workspace,
            call,
            bridge,
            opener: Opener::new(keys.clone(), roster),
            microphone: Microphone::new(keys, bridge, REPEATS)?,
            pictures: BTreeMap::new(),
        };
        page.seal_pictures(keys)?;
        Ok(page)
    }

    /// The epoch's keys from now on: a channel whose membership moved seals
    /// with its new keys, and opens under them.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn set_keys(&mut self, keys: &ChannelContentKeys, roster: Roster) -> Result<(), Refusal> {
        let microphone = Microphone::new(keys, self.bridge, REPEATS)?;
        let opener = Opener::new(keys.clone(), roster);
        self.seal_pictures(keys)?;
        self.microphone = microphone;
        self.opener = opener;
        Ok(())
    }

    fn seal_pictures(&mut self, keys: &ChannelContentKeys) -> Result<(), Refusal> {
        let camera = Sealer::new(keys, self.bridge, Stream::Camera)?;
        let screen = Sealer::new(keys, self.bridge, Stream::Screen)?;
        let mut pictures = BTreeMap::new();
        pictures.insert(Source::Camera, (camera, 0));
        pictures.insert(Source::Screen, (screen, 0));
        self.pictures = pictures;
        Ok(())
    }

    /// The workspace and the call this page is in.
    #[must_use]
    pub fn call(&self) -> (&str, &str) {
        (&self.workspace, &self.call)
    }

    /// The device bridging this page.
    #[must_use]
    pub const fn bridge(&self) -> [u8; 32] {
        self.bridge
    }

    /// The epoch the page seals under.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.microphone.epoch()
    }

    /// The datagram carrying `opus` as the microphone's next frame, whose
    /// first sample is at `timestamp_48k`. `None` when the frame has no
    /// datagram to go in: the page drops it and sends the next.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoKey`] and [`Refusal::Primitive`], as [`Sealer::seal`].
    pub fn audio(&mut self, timestamp_48k: u64, opus: &[u8]) -> Result<Option<Vec<u8>>, Refusal> {
        self.microphone.frame(timestamp_48k, opus)
    }

    /// `picture` as the sealed tilo-video/1 frame the bridge writes on the
    /// source's stream, numbered as the next frame of it.
    ///
    /// # Errors
    ///
    /// [`Refusal::Truncated`] when the picture is not a valid tilo-video/1
    /// object, and [`Refusal::NoKey`] and [`Refusal::Primitive`], as
    /// [`Sealer::seal`].
    pub fn picture(&mut self, picture: Picture) -> Result<Vec<u8>, Refusal> {
        let object = VideoObject {
            keyframe: picture.keyframe,
            // The lane's one codec, which every device in a call decodes.
            codec: Codec::H264,
            layer: picture.layer,
            sequence: picture.sequence,
            timestamp_us: picture.timestamp_us,
            width: picture.width,
            height: picture.height,
            payload: picture.payload,
        }
        .encode()
        .map_err(|_| Refusal::Truncated)?;
        let (sealer, next) = self
            .pictures
            .get_mut(&picture.source)
            .ok_or(Refusal::Primitive)?;
        let sequence = *next;
        *next = next.checked_add(1).ok_or(Refusal::Primitive)?;
        sealer.seal(sequence, &object)
    }

    /// The frames of `datagram`, which `from` sent: the page's playout drops
    /// the ones it has played.
    ///
    /// # Errors
    ///
    /// As [`audio::heard`].
    pub fn heard(&self, from: [u8; 32], datagram: &[u8]) -> Result<Vec<Heard>, Refusal> {
        audio::heard(&self.opener, from, datagram)
    }

    /// The turn this page's next exchange carries, for the call it is in: its
    /// app fills in what its microphone and pictures sealed, and what it shows
    /// of the call's devices.
    #[must_use]
    pub fn up(&self) -> Up {
        Up {
            workspace: self.workspace.clone(),
            call: self.call.clone(),
            ..Up::default()
        }
    }

    /// The tilo-video/1 object `sealed` carries, which `from` sent of
    /// `source`, if it authenticates.
    ///
    /// # Errors
    ///
    /// [`Refusal::OffRoster`] for a device the roster does not name, and
    /// [`Refusal::NoKey`], [`Refusal::Truncated`], [`Refusal::Unauthenticated`]
    /// and [`Refusal::Primitive`], as [`Opener::open`].
    pub fn opened(
        &self,
        from: [u8; 32],
        source: Source,
        sealed: &[u8],
    ) -> Result<Vec<u8>, Refusal> {
        self.opener.open(from, stream_of(source), sealed)
    }
}

/// One encoded picture of a page's, before it is sealed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub source: Source,
    pub layer: Layer,
    /// The object's place in its source's stream, as the page's encoder
    /// numbers it: a decoder starts over when it skips one.
    pub sequence: u64,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    /// The encoded access unit.
    pub payload: Vec<u8>,
}

/// One turn of the page's side: what it hands its bridge.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Up {
    /// The workspace and the call, so a host routes the turn to the bridge
    /// that carries it and a bridge refuses another call's frames.
    pub workspace: String,
    pub call: String,
    /// Sealed tilo-voice/1 datagrams: the page's microphone frames, oldest
    /// first.
    pub audio: Vec<Vec<u8>>,
    /// Sealed pictures of the page's camera or its screen.
    pub pictures: Vec<Sealed>,
    /// What the page shows of each device: the layers of its camera and of
    /// its screen.
    pub shows: Vec<Showing>,
    /// Sources the page asks a device to start over at a keyframe.
    pub keyframes: Vec<Asked>,
}

/// One sealed picture, as it travels between a page and its bridge.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sealed {
    /// The camera or the screen, as [`source_wire`](crate::wire::source_wire)
    /// codes it.
    pub source: u8,
    pub layer: u8,
    pub keyframe: bool,
    /// The sealed tilo-video/1 object.
    pub sealed: Vec<u8>,
}

impl Sealed {
    /// The source this picture belongs to, or `None` for a byte no source
    /// has.
    #[must_use]
    pub fn source(&self) -> Option<Source> {
        source_of_wire(self.source)
    }

    /// The layer this picture is of, or `None` for a byte no layer has.
    #[must_use]
    pub fn layer(&self) -> Option<Layer> {
        layer_of_wire(self.layer)
    }
}

/// What one side shows of a device: the layers of its camera, and of its
/// screen. Empty means nothing of it is drawn here.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Showing {
    pub device: [u8; 32],
    /// The layers, as [`layer_wire`](crate::wire::layer_wire) codes them.
    pub camera: Vec<u8>,
    pub screen: Vec<u8>,
}

/// A source one side asks the other to start over at a keyframe: the
/// stream fell behind, and nothing of it decodes until one.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Asked {
    pub device: [u8; 32],
    /// The source, as [`source_wire`](crate::wire::source_wire) codes it.
    pub source: u8,
}

impl Asked {
    /// The source this asks a keyframe of, or `None` for a code this device
    /// does not know.
    #[must_use]
    pub const fn source(&self) -> Option<Source> {
        source_of_wire(self.source)
    }
}

/// One turn of the bridge's side: what it hands back to the page.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Down {
    /// The devices the bridge has a lane to: the call as the lane knows it.
    pub devices: Vec<[u8; 32]>,
    /// The audio datagrams the devices sent, each with the device that sent
    /// it, oldest first.
    pub audio: Vec<Carried>,
    /// The pictures the devices sent, sealed as they came off the lane.
    pub pictures: Vec<CarriedPicture>,
    /// What each device says it shows of the page: the layers to encode for.
    pub shows: Vec<Showing>,
    /// Sources a device asks the page to start over at a keyframe.
    pub keyframes: Vec<Asked>,
}

/// One sealed audio datagram, with the device whose lane it came off.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Carried {
    pub from: [u8; 32],
    pub datagram: Vec<u8>,
}

/// One sealed picture, with the device whose lane it came off.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CarriedPicture {
    pub from: [u8; 32],
    /// The sealed tilo-video/1 object.
    pub sealed: Vec<u8>,
    /// The camera or the screen, as
    /// [`source_wire`](crate::wire::source_wire) codes it.
    pub source: u8,
}

/// The layer codes a [`Showing`] lists.
#[must_use]
pub fn layers(codes: &[u8]) -> Vec<Layer> {
    codes
        .iter()
        .filter_map(|byte| layer_of_wire(*byte))
        .collect()
}

/// The codes a list of layers is said with.
#[must_use]
pub fn codes(layers: &[Layer]) -> Vec<u8> {
    layers.iter().copied().map(layer_wire).collect()
}
