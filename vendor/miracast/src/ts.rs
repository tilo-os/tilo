//! An MPEG transport stream muxer for H.264 video with Wi-Fi Display LPCM
//! audio, and RTP packetization of transport streams (RFC 2250).
//!
//! WFD carries LPCM as `stream_type` 0x83 in `private_stream_1` PES packets
//! that start with a 4-byte sub-stream header and a 3-byte audio frame
//! header. General-purpose muxers write LPCM as Blu-ray audio (`0x80`/`0x8b`
//! style) instead, which sinks such as Microsoft's Wireless Display Adapter
//! ignore, so this muxer writes the WFD layout itself.
//!
//! The layout follows the WFD reference implementation's: PAT and PMT
//! (PMT on PID 0x0100, version 1), PCR on its own PID 0x1000 in
//! adaptation-only packets, video on 0x1011 and audio on 0x1100.

/// Bytes per transport packet.
pub const PACKET_SIZE: usize = 188;
/// Transport packets per RTP packet: 7 x 188 + 12 bytes of RTP header fit a
/// 1500-byte link.
pub const PACKETS_PER_RTP: usize = 7;
/// RTP payload type of MPEG-2 transport streams.
pub const RTP_PAYLOAD_MP2T: u8 = 33;

/// PID of the program map table.
pub const PID_PMT: u16 = 0x0100;
/// PID of the program clock reference.
pub const PID_PCR: u16 = 0x1000;
/// PID of the H.264 stream.
pub const PID_VIDEO: u16 = 0x1011;
/// PID of the audio stream.
pub const PID_AUDIO: u16 = 0x1100;

/// `stream_type` of H.264.
pub const STREAM_TYPE_H264: u8 = 0x1b;
/// `stream_type` of WFD LPCM.
pub const STREAM_TYPE_WFD_LPCM: u8 = 0x83;
/// `stream_type` of AAC in ADTS.
pub const STREAM_TYPE_AAC_ADTS: u8 = 0x0f;

const PID_PAT: u16 = 0x0000;
const PROGRAM_NUMBER: u16 = 1;
const PSI_VERSION: u8 = 1;
const STREAM_ID_VIDEO: u8 = 0xe0;
const STREAM_ID_PRIVATE_1: u8 = 0xbd;
/// Sub-stream header and audio frame header of 48 kHz, 16-bit stereo LPCM:
/// sub_stream_id 0xa0, six frame headers, first access unit at byte 7; then
/// no emphasis or mute, 16-bit samples at 48 kHz, two channels, dynamic
/// range control off.
const LPCM_HEADER: [u8; 7] = [0xa0, 0x06, 0x00, 0x07, 0x00, 0x01, 0x80];
/// Write PAT and PMT before every this many video access units.
const PSI_EVERY: u32 = 10;

/// A 90 kHz clock value, as PES timestamps use.
pub type Ticks90k = u64;

/// Writes the transport stream of one program: H.264 video plus optional
/// WFD LPCM audio.
#[derive(Debug)]
pub struct TsMuxer {
    lpcm: bool,
    continuity: [u8; 5],
    units_since_psi: u32,
}

#[derive(Clone, Copy)]
enum Stream {
    Pat = 0,
    Pmt = 1,
    Pcr = 2,
    Video = 3,
    Audio = 4,
}

impl TsMuxer {
    /// A muxer for video, with an LPCM audio stream when `lpcm`.
    #[must_use]
    pub fn new(lpcm: bool) -> Self {
        Self {
            lpcm,
            continuity: [0; 5],
            units_since_psi: PSI_EVERY,
        }
    }

    /// Appends one H.264 access unit (Annex B byte stream) presented at
    /// `pts`, with the program clock at `pcr`, to `out`. Tables and the clock
    /// reference come first when due.
    pub fn video(&mut self, access_unit: &[u8], pts: Ticks90k, pcr: Ticks90k, out: &mut Vec<u8>) {
        if self.units_since_psi >= PSI_EVERY {
            self.tables(out);
            self.units_since_psi = 0;
        }
        self.units_since_psi += 1;
        self.pcr(pcr, out);
        let header = pes_header(STREAM_ID_VIDEO, pts, None);
        self.packetize(Stream::Video, PID_VIDEO, &header, access_unit, out);
    }

    /// Appends big-endian 16-bit stereo samples at 48 kHz presented at
    /// `pts` to `out`. Does nothing for a muxer without audio.
    pub fn lpcm(&mut self, samples: &[u8], pts: Ticks90k, out: &mut Vec<u8>) {
        if !self.lpcm || samples.is_empty() {
            return;
        }
        let payload_length = LPCM_HEADER.len() + samples.len();
        let header = pes_header(STREAM_ID_PRIVATE_1, pts, Some(payload_length));
        let mut payload = Vec::with_capacity(payload_length);
        payload.extend_from_slice(&LPCM_HEADER);
        payload.extend_from_slice(samples);
        self.packetize(Stream::Audio, PID_AUDIO, &header, &payload, out);
    }

    fn tables(&mut self, out: &mut Vec<u8>) {
        let pat = [
            (PROGRAM_NUMBER >> 8) as u8,
            PROGRAM_NUMBER as u8,
            0xe0 | (PID_PMT >> 8) as u8,
            PID_PMT as u8,
        ];
        let section = psi_section(0x00, 0x0001, &pat);
        self.section_packet(Stream::Pat, PID_PAT, &section, out);

        let mut pmt = vec![0xe0 | (PID_PCR >> 8) as u8, PID_PCR as u8, 0xf0, 0x00];
        let mut stream = |stream_type: u8, pid: u16| {
            pmt.extend_from_slice(&[stream_type, 0xe0 | (pid >> 8) as u8, pid as u8, 0xf0, 0x00]);
        };
        stream(STREAM_TYPE_H264, PID_VIDEO);
        if self.lpcm {
            stream(STREAM_TYPE_WFD_LPCM, PID_AUDIO);
        }
        let section = psi_section(0x02, PROGRAM_NUMBER, &pmt);
        self.section_packet(Stream::Pmt, PID_PMT, &section, out);
    }

    fn section_packet(&mut self, stream: Stream, pid: u16, section: &[u8], out: &mut Vec<u8>) {
        let mut packet = self.header(stream, pid, true, 0b01);
        packet.push(0x00); // pointer_field
        packet.extend_from_slice(section);
        packet.resize(PACKET_SIZE, 0xff);
        out.extend_from_slice(&packet);
    }

    fn pcr(&mut self, pcr: Ticks90k, out: &mut Vec<u8>) {
        // Adaptation field only: no payload, so the continuity counter
        // does not advance (ISO 13818-1 2.4.3.3).
        let counter = self.continuity[Stream::Pcr as usize];
        let mut packet = vec![
            0x47,
            (PID_PCR >> 8) as u8,
            PID_PCR as u8,
            0x20 | counter,
            183,  // adaptation_field_length
            0x10, // PCR_flag
        ];
        let base = pcr & 0x1_ffff_ffff;
        packet.extend_from_slice(&[
            (base >> 25) as u8,
            (base >> 17) as u8,
            (base >> 9) as u8,
            (base >> 1) as u8,
            ((base & 1) << 7) as u8 | 0x7e,
            0x00,
        ]);
        packet.resize(PACKET_SIZE, 0xff);
        out.extend_from_slice(&packet);
    }

    fn packetize(
        &mut self,
        stream: Stream,
        pid: u16,
        header: &[u8],
        payload: &[u8],
        out: &mut Vec<u8>,
    ) {
        let mut data = Vec::with_capacity(header.len() + payload.len());
        data.extend_from_slice(header);
        data.extend_from_slice(payload);
        let mut rest = data.as_slice();
        let mut first = true;
        while !rest.is_empty() {
            let take = rest.len().min(PACKET_SIZE - 4);
            let mut packet = if take == PACKET_SIZE - 4 {
                self.header(stream, pid, first, 0b01)
            } else {
                // Fill the short last packet with adaptation-field stuffing,
                // never by padding the payload.
                let mut packet = self.header(stream, pid, first, 0b11);
                let field_length = PACKET_SIZE - 4 - take - 1;
                packet.push(field_length as u8);
                if field_length > 0 {
                    packet.push(0x00);
                    packet.resize(packet.len() + field_length - 1, 0xff);
                }
                packet
            };
            packet.extend_from_slice(&rest[..take]);
            debug_assert_eq!(packet.len(), PACKET_SIZE);
            out.extend_from_slice(&packet);
            rest = &rest[take..];
            first = false;
        }
    }

    fn header(&mut self, stream: Stream, pid: u16, unit_start: bool, control: u8) -> Vec<u8> {
        let counter = &mut self.continuity[stream as usize];
        let header = vec![
            0x47,
            (u8::from(unit_start) << 6) | (pid >> 8) as u8,
            pid as u8,
            (control << 4) | *counter,
        ];
        *counter = (*counter + 1) & 0x0f;
        let mut packet = Vec::with_capacity(PACKET_SIZE);
        packet.extend_from_slice(&header);
        packet
    }
}

fn psi_section(table_id: u8, id: u16, body: &[u8]) -> Vec<u8> {
    // section_length counts from after itself through the CRC.
    let length = 5 + body.len() + 4;
    let mut section = vec![
        table_id,
        0xb0 | (length >> 8) as u8,
        length as u8,
        (id >> 8) as u8,
        id as u8,
        0xc1 | (PSI_VERSION << 1),
        0x00,
        0x00,
    ];
    section.extend_from_slice(body);
    let crc = crc32_mpeg2(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

fn pes_header(stream_id: u8, pts: Ticks90k, payload_length: Option<usize>) -> Vec<u8> {
    const HEADER_DATA: usize = 5;
    // Video PES packets may be unbounded (length 0); audio ones are sized.
    let length = payload_length.map_or(0, |length| (3 + HEADER_DATA + length).min(0xffff));
    let pts = pts & 0x1_ffff_ffff;
    vec![
        0x00,
        0x00,
        0x01,
        stream_id,
        (length >> 8) as u8,
        length as u8,
        0x80, // marker bits, no scrambling
        0x80, // PTS only
        HEADER_DATA as u8,
        0x21 | ((pts >> 29) & 0x0e) as u8,
        (pts >> 22) as u8,
        0x01 | ((pts >> 14) & 0xfe) as u8,
        (pts >> 7) as u8,
        0x01 | ((pts << 1) & 0xfe) as u8,
    ]
}

/// The CRC-32 of MPEG-2 tables.
#[must_use]
pub fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in data {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Packs a transport stream into RTP packets of up to seven transport
/// packets each.
#[derive(Debug)]
pub struct RtpPacketizer {
    sequence: u16,
    ssrc: u32,
}

impl RtpPacketizer {
    /// A packetizer for the stream identified by `ssrc`.
    #[must_use]
    pub fn new(ssrc: u32) -> Self {
        Self { sequence: 0, ssrc }
    }

    /// Calls `send` with each RTP packet carrying `ts`, stamped `timestamp`
    /// (90 kHz). `ts` must be whole transport packets.
    pub fn packetize(&mut self, ts: &[u8], timestamp: Ticks90k, mut send: impl FnMut(&[u8])) {
        let mut packet = Vec::with_capacity(12 + PACKETS_PER_RTP * PACKET_SIZE);
        for chunk in ts.chunks(PACKETS_PER_RTP * PACKET_SIZE) {
            packet.clear();
            packet.extend_from_slice(&[0x80, RTP_PAYLOAD_MP2T]);
            packet.extend_from_slice(&self.sequence.to_be_bytes());
            packet.extend_from_slice(&((timestamp & 0xffff_ffff) as u32).to_be_bytes());
            packet.extend_from_slice(&self.ssrc.to_be_bytes());
            packet.extend_from_slice(chunk);
            self.sequence = self.sequence.wrapping_add(1);
            send(&packet);
        }
    }
}

/// Facts read back from a transport stream: what a test sink checks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TsStats {
    /// Transport packets read.
    pub packets: u64,
    /// Packets that did not start with the sync byte.
    pub sync_errors: u64,
    /// The PMT PID the PAT named.
    pub pmt_pid: Option<u16>,
    /// `(pid, stream_type)` from the PMT.
    pub streams: Vec<(u16, u8)>,
    /// PES packets started per PID.
    pub pes_starts: Vec<(u16, u64)>,
    /// Whether an H.264 IDR slice was seen in a video PES start.
    pub idr_seen: bool,
    /// Packets that carried a PCR.
    pub pcr_packets: u64,
}

impl TsStats {
    /// Reads whole transport packets from `data`.
    pub fn read(&mut self, data: &[u8]) {
        for packet in data.as_chunks::<PACKET_SIZE>().0 {
            self.packet(packet);
        }
    }

    /// PES packets started on `pid`.
    #[must_use]
    pub fn pes_count(&self, pid: u16) -> u64 {
        self.pes_starts
            .iter()
            .find(|(seen, _)| *seen == pid)
            .map_or(0, |(_, count)| *count)
    }

    /// The PID of the first stream with `stream_type`.
    #[must_use]
    pub fn pid_of(&self, stream_type: u8) -> Option<u16> {
        self.streams
            .iter()
            .find(|(_, kind)| *kind == stream_type)
            .map(|(pid, _)| *pid)
    }

    fn packet(&mut self, packet: &[u8]) {
        self.packets += 1;
        if packet[0] != 0x47 {
            self.sync_errors += 1;
            return;
        }
        let unit_start = packet[1] & 0x40 != 0;
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        let control = (packet[3] >> 4) & 0b11;
        let mut offset = 4;
        if control & 0b10 != 0 {
            let length = usize::from(packet[4]);
            if length > 0 && packet[5] & 0x10 != 0 {
                self.pcr_packets += 1;
            }
            offset += 1 + length;
        }
        if control & 0b01 == 0 || offset >= PACKET_SIZE {
            return;
        }
        let payload = &packet[offset..];
        if pid == PID_PAT && unit_start {
            if let Some([_, _, hi, lo, ..]) = section(payload) {
                self.pmt_pid = Some((u16::from(hi & 0x1f) << 8) | u16::from(*lo));
            }
            return;
        }
        if Some(pid) == self.pmt_pid && unit_start {
            if let Some(section) = section(payload) {
                self.read_pmt(section);
            }
            return;
        }
        if unit_start && payload.starts_with(&[0, 0, 1]) {
            match self.pes_starts.iter_mut().find(|(seen, _)| *seen == pid) {
                Some((_, count)) => *count += 1,
                None => self.pes_starts.push((pid, 1)),
            }
            if payload.len() > 9 {
                let body = &payload[9 + usize::from(payload[8])..];
                if body
                    .windows(4)
                    .any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5)
                {
                    self.idr_seen = true;
                }
            }
        }
    }

    fn read_pmt(&mut self, section: &[u8]) {
        let info_length = (usize::from(section[2] & 0x0f) << 8) | usize::from(section[3]);
        let mut rest = section.get(4 + info_length..).unwrap_or_default();
        self.streams.clear();
        while let [kind, hi, lo, l0, l1, tail @ ..] = rest {
            let pid = (u16::from(hi & 0x1f) << 8) | u16::from(*lo);
            self.streams.push((pid, *kind));
            let skip = ((usize::from(l0 & 0x0f) << 8) | usize::from(*l1)).min(tail.len());
            rest = &tail[skip..];
        }
    }
}

/// The body of the PSI section at the start of `payload`, between the
/// section header and its CRC, if the CRC holds.
fn section(payload: &[u8]) -> Option<&[u8]> {
    let pointer = usize::from(*payload.first()?);
    let section = payload.get(1 + pointer..)?;
    let length = (usize::from(*section.get(1)? & 0x0f) << 8) | usize::from(*section.get(2)?);
    let whole = section.get(..3 + length)?;
    if crc32_mpeg2(whole) != 0 {
        return None;
    }
    whole.get(8..whole.len() - 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_mpeg2_check_value() {
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376_e6e7);
    }

    #[test]
    fn a_muxed_stream_reads_back_with_lpcm_as_stream_type_0x83() {
        let mut muxer = TsMuxer::new(true);
        let mut out = Vec::new();
        // An IDR access unit larger than one packet, then audio.
        let mut access_unit = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0x1f, 0, 0, 0, 1, 0x65];
        access_unit.resize(1000, 0xab);
        muxer.video(&access_unit, 9000, 0, &mut out);
        muxer.lpcm(&[0x12; 1920], 9000, &mut out);
        muxer.video(&[0, 0, 0, 1, 0x41, 1, 2, 3], 12_000, 3000, &mut out);
        assert_eq!(out.len() % PACKET_SIZE, 0);

        let mut stats = TsStats::default();
        stats.read(&out);
        assert_eq!(stats.sync_errors, 0);
        assert_eq!(stats.pmt_pid, Some(PID_PMT));
        assert_eq!(
            stats.streams,
            vec![
                (PID_VIDEO, STREAM_TYPE_H264),
                (PID_AUDIO, STREAM_TYPE_WFD_LPCM)
            ]
        );
        assert_eq!(stats.pes_count(PID_VIDEO), 2);
        assert_eq!(stats.pes_count(PID_AUDIO), 1);
        assert_eq!(stats.pcr_packets, 2);
        assert!(stats.idr_seen);

        // The audio PES starts with the WFD LPCM headers right after its
        // 14-byte PES header.
        let audio = out
            .as_chunks::<PACKET_SIZE>()
            .0
            .iter()
            .find(|packet| {
                packet[1] & 0x40 != 0
                    && ((u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])) == PID_AUDIO
            })
            .expect("audio packet");
        let payload = &audio[4..];
        assert_eq!(&payload[..4], &[0, 0, 1, STREAM_ID_PRIVATE_1]);
        assert_eq!(&payload[14..21], &LPCM_HEADER);
    }

    #[test]
    fn short_payloads_are_stuffed_in_the_adaptation_field() {
        let mut muxer = TsMuxer::new(false);
        let mut out = Vec::new();
        muxer.video(&[0, 0, 0, 1, 0x41, 7], 0, 0, &mut out);
        let video = out
            .as_chunks::<PACKET_SIZE>()
            .0
            .last()
            .expect("video packet");
        assert_eq!(video[3] >> 4, 0b11);
        let stuffing = usize::from(video[4]);
        let payload = &video[5 + stuffing..];
        assert_eq!(payload.len(), 14 + 6);
        assert_eq!(&payload[payload.len() - 6..], &[0, 0, 0, 1, 0x41, 7]);
    }

    #[test]
    fn rtp_packets_carry_seven_transport_packets() {
        let mut packetizer = RtpPacketizer::new(0x1234_5678);
        let ts = vec![0x47; PACKET_SIZE * 10];
        let mut sizes = Vec::new();
        packetizer.packetize(&ts, 90_000, |packet| {
            assert_eq!(packet[1], RTP_PAYLOAD_MP2T);
            sizes.push(packet.len());
        });
        assert_eq!(sizes, vec![12 + 7 * PACKET_SIZE, 12 + 3 * PACKET_SIZE]);
    }
}
