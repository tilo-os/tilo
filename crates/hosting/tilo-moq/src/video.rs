//! The transport-independent tilo-video/1 object profile: a call's camera
//! and screen pictures, each in several resolution layers.
//!
//! # Tracks
//!
//! A participant's broadcast carries its voice on [`crate::relay::VOICE_TRACK`]
//! and, while it shows them, one track per picture [`Source`] and [`Layer`]
//! (`camera/low`, `camera/mid`, `camera/high`, `screen/low`, `screen/high`).
//! A layer is a complete stream of its own (simulcast), so a subscriber picks
//! the one that fits where it shows the picture ([`Layer::for_tile`]) and the
//! relay forwards only the tracks someone asked for. A publisher encodes a
//! layer only while someone asks for it.
//!
//! A room's window carrier carries one more kind of track: a single window
//! of a host program the publisher runs (a terminal, any Wayland or X
//! client), one track per window, named by the window's tab
//! ([`window_track`], `window/<tab as 16 hex digits>`). Its pictures are the
//! window's own size in the [`Layer::High`] layer, and their payload is
//! sealed for the room's members, so the relay forwards what it cannot read.
//!
//! # Groups and objects
//!
//! A group is one picture sequence that starts with a keyframe: a subscriber
//! that switches layers, or joins late, starts at the next group. Each object
//! is one encoded picture (an access unit) with a fixed header:
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 1 | version, `1` |
//! | 1 | flags: bit 0 keyframe |
//! | 1 | [`Codec`] |
//! | 1 | [`Layer`] |
//! | 8 | sequence (LE), counting objects of the track |
//! | 8 | capture time in microseconds (LE), on the publisher's clock |
//! | 2 | width (LE) |
//! | 2 | height (LE) |
//! | 4 | payload length (LE) |
//! | 4 | reserved, zero |
//!
//! followed by the payload.

use encoded_frame::{
    FrameMeta, MAX_MOQ_OBJECT, MAX_PICTURE_SIDE, Profile, validate_payload, validate_side,
};

pub const HEADER_BYTES: usize = 32;
/// The largest object: a keyframe of the largest layer at its bitrate.
pub const MAX_OBJECT_BYTES: usize = MAX_MOQ_OBJECT;
/// Pictures one group holds at most: a keyframe at least every four seconds
/// at thirty pictures a second.
pub const MAX_GROUP_OBJECTS: u64 = 120;
/// The largest picture side a layer may claim.
pub const MAX_SIDE: u16 = MAX_PICTURE_SIDE;

/// What a picture shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Camera,
    /// A screen or window the participant shares.
    Screen,
}

/// How large a layer's pictures are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    /// A face in a filmstrip or a small grid: 320 × 180.
    Low,
    /// A tile in a grid: 640 × 360.
    Mid,
    /// The active speaker or a stage: 1280 × 720 for a camera, the screen's
    /// own size up to 1920 × 1080 for a screen.
    High,
}

/// The codec an object names, shared with the callstream profile.
pub use encoded_frame::Codec;

impl Layer {
    /// Every layer, smallest first.
    pub const ALL: [Self; 3] = [Self::Low, Self::Mid, Self::High];

    const fn wire(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Mid => 1,
            Self::High => 2,
        }
    }
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Low),
            1 => Some(Self::Mid),
            2 => Some(Self::High),
            _ => None,
        }
    }

    /// The picture size a layer is encoded at, `(width, height)`, for a
    /// 16:9 source.
    #[must_use]
    pub const fn size(self, source: Source) -> (u16, u16) {
        match (self, source) {
            (Self::Low, _) => (320, 180),
            (Self::Mid, _) => (640, 360),
            (Self::High, Source::Camera) => (1280, 720),
            (Self::High, Source::Screen) => (1920, 1080),
        }
    }

    /// The bitrate a layer is encoded at, in bits a second.
    #[must_use]
    pub const fn bitrate(self, source: Source) -> u32 {
        match (self, source) {
            (Self::Low, _) => 150_000,
            (Self::Mid, _) => 500_000,
            (Self::High, Source::Camera) => 1_500_000,
            (Self::High, Source::Screen) => 2_500_000,
        }
    }

    /// The smallest layer whose pictures cover a tile `width` × `height`
    /// device pixels: what a subscriber asks for, so nobody receives more
    /// than they can show.
    #[must_use]
    pub fn for_tile(width: f64, height: f64, source: Source) -> Self {
        Self::ALL
            .into_iter()
            .filter(|layer| source == Source::Camera || *layer != Self::Mid)
            .find(|layer| {
                let (w, h) = layer.size(source);
                f64::from(w) >= width && f64::from(h) >= height
            })
            .unwrap_or(Self::High)
    }

    /// The layers a source is published in: a screen is shared at its own
    /// size and as a preview.
    #[must_use]
    pub fn of(source: Source) -> &'static [Self] {
        match source {
            Source::Camera => &[Self::Low, Self::Mid, Self::High],
            Source::Screen => &[Self::Low, Self::High],
        }
    }
}

/// The track a source's layer is published on, e.g. `camera/mid`.
#[must_use]
pub fn track(source: Source, layer: Layer) -> String {
    let source = match source {
        Source::Camera => "camera",
        Source::Screen => "screen",
    };
    let layer = match layer {
        Layer::Low => "low",
        Layer::Mid => "mid",
        Layer::High => "high",
    };
    format!("{source}/{layer}")
}

/// The source and layer of a picture track name, or `None` for any other
/// name: the only video tracks a relay forwards.
#[must_use]
pub fn parse_track(name: &str) -> Option<(Source, Layer)> {
    let (source, layer) = name.split_once('/')?;
    let source = match source {
        "camera" => Source::Camera,
        "screen" => Source::Screen,
        _ => return None,
    };
    let layer = match layer {
        "low" => Layer::Low,
        "mid" => Layer::Mid,
        "high" => Layer::High,
        _ => return None,
    };
    Layer::of(source)
        .contains(&layer)
        .then_some((source, layer))
}

/// Window tracks one publisher may have forwarded at once: every window of
/// it that someone views.
pub const MAX_WINDOW_TRACKS: usize = 32;

/// The track window `tab` is published on: `window/` and the tab as 16
/// lowercase hex digits.
#[must_use]
pub fn window_track(tab: u64) -> String {
    format!("window/{tab:016x}")
}

/// The tab of a window track name, or `None` for any other name.
#[must_use]
pub fn parse_window_track(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("window/")?;
    if digits.len() != 16
        || !digits
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// A track a relay forwards as video: a call's picture layer or one
/// window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Track {
    Picture(Source, Layer),
    /// The window shown in tab `0`, in [`Layer::High`] only.
    Window(u64),
}

impl Track {
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Self::Picture(source, layer) => track(source, layer),
            Self::Window(tab) => window_track(tab),
        }
    }

    /// The video track `name` names, or `None` for any other name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        parse_track(name)
            .map(|(source, layer)| Self::Picture(source, layer))
            .or_else(|| parse_window_track(name).map(Self::Window))
    }

    #[must_use]
    pub const fn layer(self) -> Layer {
        match self {
            Self::Picture(_, layer) => layer,
            Self::Window(_) => Layer::High,
        }
    }
}

/// One encoded picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoObject {
    pub keyframe: bool,
    pub codec: Codec,
    pub layer: Layer,
    pub sequence: u64,
    /// Capture time in microseconds on the publisher's clock.
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid tilo-video/1 object")]
pub struct InvalidVideoObject;

/// Read the fixed-width header field at `at`, refusing a short buffer.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], InvalidVideoObject> {
    let end = at.checked_add(N).ok_or(InvalidVideoObject)?;
    bytes
        .get(at..end)
        .and_then(|field| field.try_into().ok())
        .ok_or(InvalidVideoObject)
}

fn byte(bytes: &[u8], at: usize) -> Result<u8, InvalidVideoObject> {
    bytes.get(at).copied().ok_or(InvalidVideoObject)
}

impl VideoObject {
    /// The shared contract's view of this picture.
    pub fn meta(&self) -> FrameMeta {
        FrameMeta::new(self.keyframe, self.codec, self.sequence)
            .with_timestamp(self.timestamp_us)
            .with_display_size(self.width, self.height)
    }

    /// Encode this object in the tilo-video/1 wire layout.
    pub fn encode(&self) -> Result<Vec<u8>, InvalidVideoObject> {
        let max_payload = MAX_OBJECT_BYTES.saturating_sub(HEADER_BYTES);
        if self.payload.is_empty() {
            return Err(InvalidVideoObject);
        }
        validate_payload(self.payload.len(), max_payload).map_err(|_| InvalidVideoObject)?;
        validate_side(self.width, self.height, MAX_SIDE).map_err(|_| InvalidVideoObject)?;
        let codec = Profile::Moq
            .codec_id(self.codec)
            .ok_or(InvalidVideoObject)?;
        let length = u32::try_from(self.payload.len()).map_err(|_| InvalidVideoObject)?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES.saturating_add(self.payload.len()));
        bytes.extend_from_slice(&[1, u8::from(self.keyframe), codec, self.layer.wire()]);
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp_us.to_le_bytes());
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        debug_assert_eq!(bytes.len(), HEADER_BYTES);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Decode one complete, valid tilo-video/1 object.
    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidVideoObject> {
        let header = validate(bytes)?;
        Ok(Self {
            keyframe: header.meta.keyframe,
            codec: header.meta.codec,
            layer: header.layer,
            sequence: header.meta.sequence,
            timestamp_us: header.meta.timestamp_us.unwrap_or(0),
            width: header.meta.width(),
            height: header.meta.height(),
            payload: bytes
                .get(HEADER_BYTES..)
                .ok_or(InvalidVideoObject)?
                .to_vec(),
        })
    }
}

/// What a relay checks of an object without copying its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub meta: FrameMeta,
    pub layer: Layer,
}

/// Check one object's header and length.
pub fn validate(bytes: &[u8]) -> Result<Header, InvalidVideoObject> {
    validate_payload(bytes.len(), MAX_OBJECT_BYTES).map_err(|_| InvalidVideoObject)?;
    if bytes.len() <= HEADER_BYTES {
        return Err(InvalidVideoObject);
    }
    let flags = byte(bytes, 1)?;
    if byte(bytes, 0)? != 1 || flags & !1 != 0 || field::<4>(bytes, 28)? != [0; 4] {
        return Err(InvalidVideoObject);
    }
    let codec = Profile::Moq
        .codec(byte(bytes, 2)?)
        .ok_or(InvalidVideoObject)?;
    let layer = Layer::from_wire(byte(bytes, 3)?).ok_or(InvalidVideoObject)?;
    let width = u16::from_le_bytes(field(bytes, 20)?);
    let height = u16::from_le_bytes(field(bytes, 22)?);
    let length = u32::from_le_bytes(field(bytes, 24)?);
    validate_side(width, height, MAX_SIDE).map_err(|_| InvalidVideoObject)?;
    if usize::try_from(length).ok() != bytes.len().checked_sub(HEADER_BYTES) {
        return Err(InvalidVideoObject);
    }
    Ok(Header {
        meta: FrameMeta::new(flags & 1 == 1, codec, u64::from_le_bytes(field(bytes, 4)?))
            .with_timestamp(u64::from_le_bytes(field(bytes, 12)?))
            .with_display_size(width, height),
        layer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(keyframe: bool) -> VideoObject {
        VideoObject {
            keyframe,
            codec: Codec::H264,
            layer: Layer::Mid,
            sequence: 7,
            timestamp_us: 1_000_000,
            width: 640,
            height: 360,
            payload: vec![0, 0, 0, 1, 0x65],
        }
    }

    #[test]
    fn a_picture_round_trips_and_a_malformed_one_is_refused() {
        let object = picture(true);
        let bytes = object.encode().unwrap();
        assert_eq!(VideoObject::decode(&bytes).unwrap(), object);
        let header = validate(&bytes).unwrap();
        assert!(header.meta.keyframe);
        assert_eq!(header.layer, Layer::Mid);
        assert_eq!(header.meta, object.meta());

        let mut versioned = bytes.clone();
        versioned[0] = 2;
        assert!(validate(&versioned).is_err());
        let mut flagged = bytes.clone();
        flagged[1] = 0b10;
        assert!(validate(&flagged).is_err());
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(validate(&truncated).is_err(), "the length says the payload");
        let mut zero = bytes;
        zero[20] = 0;
        zero[21] = 0;
        assert!(validate(&zero).is_err());
        assert!(
            VideoObject {
                payload: Vec::new(),
                ..picture(false)
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn a_picture_keeps_the_pinned_tilo_video_1_header_bytes() {
        let object = VideoObject {
            keyframe: true,
            codec: Codec::H264,
            layer: Layer::Mid,
            sequence: 1,
            timestamp_us: 2,
            width: 640,
            height: 360,
            payload: vec![0x65],
        };
        let bytes = object.encode().unwrap();
        assert_eq!(
            &bytes[..HEADER_BYTES],
            &[
                1, // version
                1, // keyframe
                1, // H.264
                1, // mid layer
                1, 0, 0, 0, 0, 0, 0, 0, // sequence
                2, 0, 0, 0, 0, 0, 0, 0, // capture time
                0x80, 0x02, // width 640
                0x68, 0x01, // height 360
                1, 0, 0, 0, // payload length
                0, 0, 0, 0, // reserved
            ]
        );
        assert_eq!(&bytes[HEADER_BYTES..], &[0x65]);
    }

    #[test]
    fn the_profile_names_the_shared_codecs_and_bounds_the_object() {
        for codec in [Codec::H264, Codec::Vp8, Codec::Vp9, Codec::Av1] {
            let mut object = picture(codec == Codec::H264);
            object.codec = codec;
            let bytes = object.encode().unwrap();
            let header = validate(&bytes).unwrap();
            assert_eq!(header.meta.codec, codec);
            let id = Profile::Moq.codec_id(codec).unwrap();
            assert_eq!(Profile::Moq.codec(id), Some(codec));
        }
        let mut hevc = picture(true);
        hevc.codec = Codec::HevcMain10;
        assert!(
            hevc.encode().is_err(),
            "the MoQ profile does not carry HEVC"
        );

        let mut oversized = picture(true);
        oversized.payload = vec![0; MAX_OBJECT_BYTES];
        assert!(oversized.encode().is_err());
        let mut too_wide = picture(true);
        too_wide.width = MAX_SIDE.saturating_add(1);
        assert!(too_wide.encode().is_err());
        let mut zero_wide = picture(true);
        zero_wide.width = 0;
        assert!(zero_wide.encode().is_err());

        let valid = picture(true).encode().unwrap();
        assert!(
            validate(&[0; HEADER_BYTES]).is_err(),
            "a header with no payload is not an object"
        );
        assert!(validate(&valid[..HEADER_BYTES.saturating_sub(1)]).is_err());
        assert!(validate(&valid[..HEADER_BYTES]).is_err());
        let mut unknown_codec = valid.clone();
        unknown_codec[2] = 9;
        assert!(validate(&unknown_codec).is_err());
        let mut unknown_layer = valid.clone();
        unknown_layer[3] = 9;
        assert!(validate(&unknown_layer).is_err());
        let mut reserved = valid.clone();
        reserved[28] = 1;
        assert!(validate(&reserved).is_err());
        let mut oversized_object = valid;
        oversized_object.resize(MAX_OBJECT_BYTES.saturating_add(1), 0);
        assert!(validate(&oversized_object).is_err());
    }

    #[test]
    fn a_subscriber_asks_for_the_smallest_layer_that_covers_its_tile() {
        assert_eq!(Layer::for_tile(200.0, 112.0, Source::Camera), Layer::Low);
        assert_eq!(Layer::for_tile(480.0, 270.0, Source::Camera), Layer::Mid);
        assert_eq!(Layer::for_tile(1100.0, 620.0, Source::Camera), Layer::High);
        assert_eq!(Layer::for_tile(3840.0, 2160.0, Source::Camera), Layer::High);
        // A screen has no middle layer: past its preview it is the screen.
        assert_eq!(Layer::for_tile(480.0, 270.0, Source::Screen), Layer::High);
    }

    #[test]
    fn only_picture_tracks_are_named_as_video() {
        for source in [Source::Camera, Source::Screen] {
            for layer in Layer::of(source) {
                assert_eq!(parse_track(&track(source, *layer)), Some((source, *layer)));
            }
        }
        for name in [
            "audio",
            "camera",
            "screen/mid",
            "camera/huge",
            "mic/low",
            "",
        ] {
            assert_eq!(parse_track(name), None, "{name}");
        }
    }

    #[test]
    fn a_window_track_names_its_tab_and_nothing_else_parses_as_one() {
        let tab = 0x5EED_0000_0000_0007;
        assert_eq!(window_track(tab), "window/5eed000000000007");
        assert_eq!(Track::parse(&window_track(tab)), Some(Track::Window(tab)));
        assert_eq!(Track::Window(tab).layer(), Layer::High);
        assert_eq!(
            Track::parse("camera/mid"),
            Some(Track::Picture(Source::Camera, Layer::Mid))
        );
        for name in [
            "window/",
            "window/5EED000000000007",
            "window/5eed00000000007",
            "window/5eed0000000000070",
            "window/+eed000000000007",
            "windows/5eed000000000007",
            "audio",
        ] {
            assert_eq!(Track::parse(name), None, "{name}");
        }
    }
}
