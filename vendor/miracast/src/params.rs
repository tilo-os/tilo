//! The `wfd_*` parameters exchanged in `GET_PARAMETER` and `SET_PARAMETER`
//! bodies, and the RTSP `Transport` header.

use crate::modes::{H264Level, SinkVideo, VideoMode};

/// Parameter names WFD uses.
pub mod names {
    /// The sink's and then the chosen video format.
    pub const VIDEO_FORMATS: &str = "wfd_video_formats";
    /// The sink's and then the chosen audio codec.
    pub const AUDIO_CODECS: &str = "wfd_audio_codecs";
    /// The UDP ports the sink receives RTP on.
    pub const CLIENT_RTP_PORTS: &str = "wfd_client_rtp_ports";
    /// HDCP support.
    pub const CONTENT_PROTECTION: &str = "wfd_content_protection";
    /// The URL the sink sets the session up on.
    pub const PRESENTATION_URL: &str = "wfd_presentation_URL";
    /// Which request the sink should send next.
    pub const TRIGGER_METHOD: &str = "wfd_trigger_method";
    /// The sink asks for an IDR frame.
    pub const IDR_REQUEST: &str = "wfd_idr_request";
}

/// `name: value` lines of a parameter body. Names are compared without case
/// by [`get`].
#[must_use]
pub fn parse(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// The value of `name` in parsed parameters.
#[must_use]
pub fn get<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The parameter names a `GET_PARAMETER` body asks for, one per line.
#[must_use]
pub fn requested_names(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Reads the H.264 entry to use from a sink's `wfd_video_formats`: the first
/// that offers constrained baseline, otherwise the first.
///
/// The value is `native preferred-display-mode` followed by one or more
/// comma-separated H.264 entries: `profile level cea vesa hh latency
/// min-slice slice-enc frame-rate-control max-hres max-vres`.
#[must_use]
pub fn parse_sink_video(value: &str) -> Option<SinkVideo> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    let mut fields = value.split_whitespace();
    let _native = fields.next()?;
    let _preferred = fields.next()?;
    let rest: String = fields.collect::<Vec<_>>().join(" ");
    let entries: Vec<SinkVideo> = rest
        .split(',')
        .filter_map(|entry| {
            let tokens: Vec<&str> = entry.split_whitespace().collect();
            let hex8 = |at: usize| u8::from_str_radix(tokens.get(at)?, 16).ok();
            let hex32 = |at: usize| u32::from_str_radix(tokens.get(at)?, 16).ok();
            Some(SinkVideo {
                profiles: hex8(0)?,
                levels: hex8(1)?,
                cea: hex32(2)?,
                vesa: hex32(3)?,
            })
        })
        .collect();
    entries
        .iter()
        .find(|entry| entry.profiles & 0x01 != 0)
        .or_else(|| entries.first())
        .copied()
}

/// The `wfd_video_formats` value naming `mode` in constrained baseline at
/// `level`.
#[must_use]
pub fn format_video(mode: &VideoMode, level: H264Level) -> String {
    let (cea, vesa) = match mode.table {
        crate::modes::Table::Cea => (mode.mask_bit(), 0),
        crate::modes::Table::Vesa => (0, mode.mask_bit()),
    };
    format!(
        "{:02x} 00 01 {:02x} {cea:08x} {vesa:08x} 00000000 00 0000 0000 00 none none",
        mode.native_byte(),
        level.bit(),
    )
}

/// An audio format a sink lists or the source chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    /// Uncompressed 16-bit PCM, two channels.
    Lpcm,
    /// AAC-LC, two channels.
    Aac,
}

/// What a sink listed in `wfd_audio_codecs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SinkAudio {
    /// Whether it decodes 48 kHz stereo LPCM.
    pub lpcm_48k: bool,
    /// Whether it decodes 48 kHz stereo AAC.
    pub aac_48k: bool,
}

/// Reads `wfd_audio_codecs: LPCM 00000003 00, AAC 00000001 00`.
#[must_use]
pub fn parse_sink_audio(value: &str) -> SinkAudio {
    let mut audio = SinkAudio::default();
    for entry in value.split(',') {
        let mut tokens = entry.split_whitespace();
        let (Some(codec), Some(modes)) = (tokens.next(), tokens.next()) else {
            continue;
        };
        let modes = u32::from_str_radix(modes, 16).unwrap_or(0);
        if codec.eq_ignore_ascii_case("LPCM") {
            // Bit 0 is 44.1 kHz stereo, bit 1 48 kHz stereo.
            audio.lpcm_48k |= modes & 0b10 != 0;
        } else if codec.eq_ignore_ascii_case("AAC") {
            // Bit 0 is 48 kHz stereo.
            audio.aac_48k |= modes & 0b1 != 0;
        }
    }
    audio
}

/// The `wfd_audio_codecs` value for the chosen codec, `none` without audio.
#[must_use]
pub fn format_audio(codec: Option<AudioCodec>) -> &'static str {
    match codec {
        Some(AudioCodec::Lpcm) => "LPCM 00000002 00",
        Some(AudioCodec::Aac) => "AAC 00000001 00",
        None => "none",
    }
}

/// The sink's RTP and RTCP ports from `RTP/AVP/UDP;unicast 19000 0
/// mode=play`. An RTCP port of 0 means none.
#[must_use]
pub fn parse_client_rtp_ports(value: &str) -> Option<(u16, u16)> {
    let mut tokens = value.split_whitespace();
    let profile = tokens.next()?;
    if !profile.to_ascii_uppercase().starts_with("RTP/AVP/") {
        return None;
    }
    let rtp = tokens.next()?.parse().ok()?;
    let rtcp = tokens
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or(0);
    (rtp != 0).then_some((rtp, rtcp))
}

/// The `wfd_client_rtp_ports` value for the sink's ports.
#[must_use]
pub fn format_client_rtp_ports(rtp: u16, rtcp: u16) -> String {
    format!("RTP/AVP/UDP;unicast {rtp} {rtcp} mode=play")
}

/// The `client_port=` of a `Transport` header: RTP and optional RTCP.
#[must_use]
pub fn parse_transport_client_ports(value: &str) -> Option<(u16, u16)> {
    let ports = value
        .split(';')
        .find_map(|part| part.trim().strip_prefix("client_port="))?;
    let (rtp, rtcp) = ports.split_once('-').unwrap_or((ports, "0"));
    let rtp = rtp.trim().parse().ok()?;
    Some((rtp, rtcp.trim().parse().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::MANDATORY_HD;

    #[test]
    fn a_sink_video_format_with_several_entries_picks_baseline() {
        let value = "40 00 02 10 0001ffff 3fffffff 00000000 00 0000 0000 00 none none, \
                     01 08 000000e1 00000000 00000000 00 0000 0000 00 none none";
        let sink = parse_sink_video(value).expect("entry");
        assert_eq!(sink.profiles, 0x01);
        assert_eq!(sink.levels, 0x08);
        assert_eq!(sink.cea, 0xe1);
        let only_high = "00 00 02 10 000001e1 00000000 00000000 00 0000 0000 00 none none";
        assert_eq!(parse_sink_video(only_high).expect("entry").cea, 0x1e1);
        assert_eq!(parse_sink_video("none"), None);
        assert_eq!(parse_sink_video("00 00"), None);
    }

    #[test]
    fn the_chosen_format_names_one_mode() {
        assert_eq!(
            format_video(&MANDATORY_HD, H264Level::L3_1),
            "28 00 01 01 00000020 00000000 00000000 00 0000 0000 00 none none"
        );
    }

    #[test]
    fn audio_ports_and_transport_parse() {
        let audio = parse_sink_audio("LPCM 00000003 00, AAC 00000001 00");
        assert!(audio.lpcm_48k && audio.aac_48k);
        assert_eq!(parse_sink_audio("LPCM 00000001 00"), SinkAudio::default());
        assert_eq!(parse_sink_audio("none"), SinkAudio::default());
        assert_eq!(
            parse_client_rtp_ports("RTP/AVP/UDP;unicast 19000 0 mode=play"),
            Some((19000, 0))
        );
        assert_eq!(
            parse_client_rtp_ports("RTP/AVP/UDP;unicast 0 0 mode=play"),
            None
        );
        assert_eq!(
            parse_transport_client_ports("RTP/AVP/UDP;unicast;client_port=1028-1029"),
            Some((1028, 1029))
        );
        assert_eq!(
            parse_transport_client_ports("RTP/AVP/UDP;unicast;client_port=19000"),
            Some((19000, 0))
        );
        let params = parse("wfd_audio_codecs: AAC 00000001 00\r\nWFD_Client_RTP_Ports: x\r\n");
        assert_eq!(get(&params, "wfd_client_rtp_ports"), Some("x"));
    }
}
