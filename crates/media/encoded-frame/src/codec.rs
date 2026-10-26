//! The codecs a first-party picture may be in, and the id each profile gives
//! it.

/// A compressed picture format one of Tilo's own video profiles carries.
///
/// The union covers both profiles; a profile supports a subset and its
/// [`Profile::codec_id`] answers `None` outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    /// H.264, 8-bit 4:2:0. The baseline every peer can decode.
    H264,
    /// HEVC Main10, the HDR path when a hardware encoder exists.
    HevcMain10,
    /// `VP8`, the older royalty-free format the `MoQ` profile still accepts.
    Vp8,
    /// VP9 profile 0, royalty-free.
    Vp9,
    /// AV1 main profile (8- or 10-bit), royalty-free.
    Av1,
}

impl Codec {
    /// Every codec this contract knows, in no wire order.
    pub const ALL: [Codec; 5] = [
        Codec::H264,
        Codec::HevcMain10,
        Codec::Vp8,
        Codec::Vp9,
        Codec::Av1,
    ];

    /// The codec's diagnostic label, matching the media stack's wire labels
    /// where the two overlap.
    pub const fn label(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::HevcMain10 => "hevc-main10",
            Codec::Vp8 => "vp8",
            Codec::Vp9 => "vp9",
            Codec::Av1 => "av1",
        }
    }

    /// Parse a diagnostic label. `None` for a codec this build does not know.
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|codec| codec.label() == label)
    }

    /// Bits per sample the codec carries on the real-time path.
    pub const fn bit_depth(self) -> u8 {
        match self {
            Codec::HevcMain10 => 10,
            Codec::H264 | Codec::Vp8 | Codec::Vp9 | Codec::Av1 => 8,
        }
    }

    /// Whether encoding this codec in software needs no patent licence.
    pub const fn is_royalty_free(self) -> bool {
        matches!(self, Codec::Vp8 | Codec::Vp9 | Codec::Av1)
    }
}

/// One of the two wire profiles that carry an encoded picture.
///
/// The variants name the carriage, not a codec or a layer: the codec table
/// and the bounds are what the profiles share here. A third profile adds a
/// variant and its ids, and every caller that matches on this enum sees the
/// compile error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    /// `tilo-callstream`'s 13-byte frame header: a big-endian sequence and
    /// length, a flags byte, and an optional display size.
    Callstream,
    /// `tilo-video/1`'s 32-byte object header: a version byte, flags, codec,
    /// layer, little-endian sequence and capture time, size and length.
    Moq,
}

/// The codecs the callstream profile can name, smallest id first.
const CALLSTREAM_CODECS: [Codec; 4] = [Codec::H264, Codec::Vp9, Codec::Av1, Codec::HevcMain10];
/// The codecs the `MoQ` profile can name, smallest id first.
const MOQ_CODECS: [Codec; 4] = [Codec::H264, Codec::Vp8, Codec::Vp9, Codec::Av1];

impl Profile {
    /// The profile's diagnostic name, as its own crate documents it.
    pub const fn name(self) -> &'static str {
        match self {
            Profile::Callstream => "tilo-callstream/13",
            Profile::Moq => "tilo-video/1",
        }
    }

    /// The codecs this profile carries.
    pub const fn codecs(self) -> &'static [Codec] {
        match self {
            Profile::Callstream => &CALLSTREAM_CODECS,
            Profile::Moq => &MOQ_CODECS,
        }
    }

    /// The id `codec` has in this profile's wire header, or `None` when the
    /// profile does not carry it.
    pub const fn codec_id(self, codec: Codec) -> Option<u8> {
        match self {
            Profile::Callstream => match codec {
                Codec::H264 => Some(0),
                Codec::Vp9 => Some(1),
                Codec::Av1 => Some(2),
                Codec::HevcMain10 => Some(3),
                Codec::Vp8 => None,
            },
            Profile::Moq => match codec {
                Codec::H264 => Some(1),
                Codec::Vp8 => Some(2),
                Codec::Vp9 => Some(3),
                Codec::Av1 => Some(4),
                Codec::HevcMain10 => None,
            },
        }
    }

    /// The codec this profile's id names, or `None` for an id it does not
    /// assign.
    pub const fn codec(self, id: u8) -> Option<Codec> {
        match self {
            Profile::Callstream => match id {
                0 => Some(Codec::H264),
                1 => Some(Codec::Vp9),
                2 => Some(Codec::Av1),
                3 => Some(Codec::HevcMain10),
                _ => None,
            },
            Profile::Moq => match id {
                1 => Some(Codec::H264),
                2 => Some(Codec::Vp8),
                3 => Some(Codec::Vp9),
                4 => Some(Codec::Av1),
                _ => None,
            },
        }
    }
}
