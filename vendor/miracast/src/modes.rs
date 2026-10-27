//! Video modes: the CEA and VESA resolution tables `wfd_video_formats`
//! indexes, H.264 levels, and how a source picks a mode a sink supports.

/// Which resolution table a mode belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Table {
    /// The CEA-861 table (TV timings).
    Cea,
    /// The VESA table (computer timings).
    Vesa,
}

/// One progressive video mode a sink can advertise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoMode {
    /// The table the mode is in.
    pub table: Table,
    /// Its bit index in that table's mask.
    pub index: u8,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
}

const fn cea(index: u8, width: u32, height: u32, fps: u32) -> VideoMode {
    VideoMode {
        table: Table::Cea,
        index,
        width,
        height,
        fps,
    }
}

const fn vesa(index: u8, width: u32, height: u32, fps: u32) -> VideoMode {
    VideoMode {
        table: Table::Vesa,
        index,
        width,
        height,
        fps,
    }
}

/// The progressive CEA modes. The interlaced ones and the anamorphic
/// 720-pixel-wide SD modes are left out: a desktop is never sent in either.
pub const CEA_MODES: &[VideoMode] = &[
    cea(0, 640, 480, 60),
    cea(5, 1280, 720, 30),
    cea(6, 1280, 720, 60),
    cea(7, 1920, 1080, 30),
    cea(8, 1920, 1080, 60),
    cea(10, 1280, 720, 25),
    cea(11, 1280, 720, 50),
    cea(12, 1920, 1080, 25),
    cea(13, 1920, 1080, 50),
    cea(15, 1280, 720, 24),
    cea(16, 1920, 1080, 24),
];

/// The VESA modes, all progressive.
pub const VESA_MODES: &[VideoMode] = &[
    vesa(0, 800, 600, 30),
    vesa(1, 800, 600, 60),
    vesa(2, 1024, 768, 30),
    vesa(3, 1024, 768, 60),
    vesa(4, 1152, 864, 30),
    vesa(5, 1152, 864, 60),
    vesa(6, 1280, 768, 30),
    vesa(7, 1280, 768, 60),
    vesa(8, 1280, 800, 30),
    vesa(9, 1280, 800, 60),
    vesa(10, 1360, 768, 30),
    vesa(11, 1360, 768, 60),
    vesa(12, 1366, 768, 30),
    vesa(13, 1366, 768, 60),
    vesa(14, 1280, 1024, 30),
    vesa(15, 1280, 1024, 60),
    vesa(16, 1400, 1050, 30),
    vesa(17, 1400, 1050, 60),
    vesa(18, 1440, 900, 30),
    vesa(19, 1440, 900, 60),
    vesa(20, 1600, 900, 30),
    vesa(21, 1600, 900, 60),
    vesa(22, 1600, 1200, 30),
    vesa(23, 1600, 1200, 60),
    vesa(24, 1680, 1024, 30),
    vesa(25, 1680, 1024, 60),
    vesa(26, 1680, 1050, 30),
    vesa(27, 1680, 1050, 60),
    vesa(28, 1920, 1200, 30),
    vesa(29, 1920, 1200, 60),
];

/// 1280x720 at 30 frames: the HD mode every HD-capable sink must support.
pub const MANDATORY_HD: VideoMode = cea(5, 1280, 720, 30);

impl VideoMode {
    /// The mode's bit in its table's mask.
    #[must_use]
    pub fn mask_bit(&self) -> u32 {
        1 << self.index
    }

    /// The `native` byte naming this mode: the index in bits 7:3, the table
    /// in bits 2:0.
    #[must_use]
    pub fn native_byte(&self) -> u8 {
        let table = match self.table {
            Table::Cea => 0,
            Table::Vesa => 1,
        };
        (self.index << 3) | table
    }

    /// Macroblocks per frame.
    #[must_use]
    pub fn macroblocks(&self) -> u32 {
        self.width.div_ceil(16) * self.height.div_ceil(16)
    }

    /// The lowest H.264 level that carries this mode.
    #[must_use]
    pub fn minimum_level(&self) -> Option<H264Level> {
        H264Level::ALL.into_iter().find(|level| level.carries(self))
    }

    /// `1280x720p30`
    #[must_use]
    pub fn name(&self) -> String {
        format!("{}x{}p{}", self.width, self.height, self.fps)
    }
}

/// The H.264 levels `wfd_video_formats` can signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum H264Level {
    /// Level 3.1
    L3_1,
    /// Level 3.2
    L3_2,
    /// Level 4
    L4,
    /// Level 4.1
    L4_1,
    /// Level 4.2
    L4_2,
    /// Level 5 (Wi-Fi Display R2)
    L5,
    /// Level 5.1 (Wi-Fi Display R2)
    L5_1,
}

impl H264Level {
    /// Every level, lowest first.
    pub const ALL: [Self; 7] = [
        Self::L3_1,
        Self::L3_2,
        Self::L4,
        Self::L4_1,
        Self::L4_2,
        Self::L5,
        Self::L5_1,
    ];

    /// The level's bit in `wfd_video_formats`.
    #[must_use]
    pub fn bit(self) -> u8 {
        match self {
            Self::L3_1 => 0x01,
            Self::L3_2 => 0x02,
            Self::L4 => 0x04,
            Self::L4_1 => 0x08,
            Self::L4_2 => 0x10,
            Self::L5 => 0x20,
            Self::L5_1 => 0x40,
        }
    }

    /// The highest level set in a sink's level bitmap.
    #[must_use]
    pub fn highest_in(bits: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .rev()
            .find(|level| bits & level.bit() != 0)
    }

    /// `"3.1"`, as GStreamer caps spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L3_1 => "3.1",
            Self::L3_2 => "3.2",
            Self::L4 => "4",
            Self::L4_1 => "4.1",
            Self::L4_2 => "4.2",
            Self::L5 => "5",
            Self::L5_1 => "5.1",
        }
    }

    /// Maximum macroblocks per second and per frame (H.264 Table A-1).
    fn limits(self) -> (u32, u32) {
        match self {
            Self::L3_1 => (108_000, 3_600),
            Self::L3_2 => (216_000, 5_120),
            Self::L4 | Self::L4_1 => (245_760, 8_192),
            Self::L4_2 => (522_240, 8_704),
            Self::L5 => (589_824, 22_080),
            Self::L5_1 => (983_040, 36_864),
        }
    }

    /// Whether this level is high enough for `mode`.
    #[must_use]
    pub fn carries(self, mode: &VideoMode) -> bool {
        let (per_second, per_frame) = self.limits();
        let frame = mode.macroblocks();
        frame <= per_frame && frame * mode.fps <= per_second
    }
}

/// What a sink said it decodes, from one H.264 entry of its
/// `wfd_video_formats`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkVideo {
    /// Profile bitmap: `0x01` constrained baseline, `0x02` constrained high.
    pub profiles: u8,
    /// Level bitmap.
    pub levels: u8,
    /// Supported CEA modes.
    pub cea: u32,
    /// Supported VESA modes.
    pub vesa: u32,
}

impl SinkVideo {
    /// Whether the sink lists `mode` and decodes the level it needs.
    #[must_use]
    pub fn supports(&self, mode: &VideoMode) -> bool {
        let mask = match mode.table {
            Table::Cea => self.cea,
            Table::Vesa => self.vesa,
        };
        let level_ok = match (H264Level::highest_in(self.levels), mode.minimum_level()) {
            (Some(highest), Some(needed)) => needed <= highest,
            (None, Some(_)) => true,
            (_, None) => false,
        };
        mask & mode.mask_bit() != 0 && level_ok
    }
}

/// What the source would like to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModePreference {
    /// The largest frame to send, if limited.
    pub max_size: Option<(u32, u32)>,
    /// The highest frame rate to send.
    pub max_fps: u32,
}

/// The mode to send: the largest frame within the preference that the sink
/// supports, then the highest frame rate, CEA before VESA at equal size.
///
/// A sink that advertised nothing usable still gets a stream: 720p30 when the
/// preference allows it, otherwise the smallest mode. Sinks commonly decode
/// more than they list, and refusing to stream helps nobody.
#[must_use]
pub fn choose_mode(sink: Option<&SinkVideo>, preference: &ModePreference) -> VideoMode {
    let fits = |mode: &VideoMode| {
        preference
            .max_size
            .is_none_or(|(width, height)| mode.width <= width && mode.height <= height)
    };
    let best = CEA_MODES
        .iter()
        .chain(VESA_MODES)
        .filter(|mode| sink.is_some_and(|sink| sink.supports(mode)))
        .filter(|mode| fits(mode) && mode.fps <= preference.max_fps)
        .max_by_key(|mode| {
            (
                mode.width * mode.height,
                mode.fps,
                matches!(mode.table, Table::Cea),
            )
        });
    if let Some(mode) = best {
        return *mode;
    }
    // Nothing within the frame-rate limit: the slowest supported mode that fits.
    let slowest = CEA_MODES
        .iter()
        .chain(VESA_MODES)
        .filter(|mode| sink.is_some_and(|sink| sink.supports(mode)) && fits(mode))
        .min_by_key(|mode| (mode.fps, u32::MAX - mode.width * mode.height));
    if let Some(mode) = slowest {
        return *mode;
    }
    if fits(&MANDATORY_HD) {
        MANDATORY_HD
    } else {
        CEA_MODES[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(cea: u32, vesa: u32, levels: u8) -> SinkVideo {
        SinkVideo {
            profiles: 0x01,
            levels,
            cea,
            vesa,
        }
    }

    #[test]
    fn native_bytes_and_levels_follow_the_tables() {
        assert_eq!(MANDATORY_HD.native_byte(), 0x28);
        assert_eq!(cea(8, 1920, 1080, 60).native_byte(), 0x40);
        assert_eq!(vesa(28, 1920, 1200, 30).native_byte(), 0xe1);
        assert_eq!(MANDATORY_HD.minimum_level(), Some(H264Level::L3_1));
        assert_eq!(cea(6, 1280, 720, 60).minimum_level(), Some(H264Level::L3_2));
        assert_eq!(cea(7, 1920, 1080, 30).minimum_level(), Some(H264Level::L4));
        assert_eq!(
            cea(8, 1920, 1080, 60).minimum_level(),
            Some(H264Level::L4_2)
        );
        assert_eq!(
            vesa(29, 1920, 1200, 60).minimum_level(),
            Some(H264Level::L5)
        );
        assert_eq!(H264Level::highest_in(0x1f), Some(H264Level::L4_2));
    }

    #[test]
    fn the_largest_supported_mode_within_the_limits_wins() {
        // 640x480p60, 720p30/60, 1080p30/60; levels up to 4.2.
        let tv = sink(0x1e1, 0, 0x1f);
        let any = ModePreference {
            max_size: None,
            max_fps: 60,
        };
        assert_eq!(choose_mode(Some(&tv), &any).name(), "1920x1080p60");
        let thirty = ModePreference { max_fps: 30, ..any };
        assert_eq!(choose_mode(Some(&tv), &thirty).name(), "1920x1080p30");
        let hd = ModePreference {
            max_size: Some((1280, 720)),
            max_fps: 60,
        };
        assert_eq!(choose_mode(Some(&tv), &hd).name(), "1280x720p60");
        // The sink's level caps the mode even when it lists a larger one.
        let low_level = sink(0x1e1, 0, 0x03);
        assert_eq!(choose_mode(Some(&low_level), &any).name(), "1280x720p60");
    }

    #[test]
    fn vesa_modes_count_and_cea_wins_ties() {
        let monitor = sink(0x20, 0x3000_0000, 0x7f);
        let any = ModePreference {
            max_size: None,
            max_fps: 60,
        };
        let mode = choose_mode(Some(&monitor), &any);
        assert_eq!(
            (mode.table, mode.name().as_str()),
            (Table::Vesa, "1920x1200p60")
        );
    }

    #[test]
    fn a_sink_without_usable_modes_still_gets_720p30() {
        let any = ModePreference {
            max_size: None,
            max_fps: 60,
        };
        assert_eq!(choose_mode(None, &any), MANDATORY_HD);
        assert_eq!(choose_mode(Some(&sink(0, 0, 0)), &any), MANDATORY_HD);
        let tiny = ModePreference {
            max_size: Some((640, 480)),
            max_fps: 60,
        };
        assert_eq!(choose_mode(None, &tiny).name(), "640x480p60");
    }
}
