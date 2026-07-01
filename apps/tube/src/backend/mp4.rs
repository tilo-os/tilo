//! Minimal MP4 / fragmented-MP4 demux: lift H.264 **Annex-B** NAL units out of a
//! downloaded byte prefix so [`ui::widget::VideoPlayer`] (an openh264 Annex-B
//! decoder) can decode a real frame.
//!
//! YouTube ships H.264 inside an ISO-BMFF (MP4) container. The NAL units there
//! are **length-prefixed** (AVCC framing: a big-endian length of `lengthSize`
//! bytes in front of each NAL) and the SPS/PPS live in the `avcC` box in the
//! init segment — *not* inline. openh264 wants the opposite: Annex-B start-code
//! (`00 00 00 01`) framed NAL units with SPS/PPS in-band. This module bridges
//! the two: it parses the `avcC` config (SPS/PPS + NAL length size) from the
//! `moov`, walks the length-prefixed NAL units in the first `mdat`, and emits a
//! single Annex-B buffer `[SPS][PPS][NAL…]` ready to feed to the decoder.
//!
//! It is deliberately dependency-free and pure (bytes in, bytes out) so it is
//! unit-testable without the network. It targets **adaptive, video-only**
//! fragmented-MP4 streams: each of their `mdat` boxes is exactly a run of video
//! NAL units, so walking the box as consecutive length-prefixed NALs is
//! correct. (Progressive/muxed files interleave audio samples inside `mdat`, so
//! the naive walk does not apply — the stream resolver prefers adaptive streams
//! for exactly this reason.)

/// H.264 parameter sets + NAL length framing size parsed from an `avcC` box.
#[derive(Clone, Debug, Default)]
pub struct H264Config {
    /// NAL length prefix width in bytes (`lengthSizeMinusOne + 1`, 1..=4).
    pub length_size: usize,
    /// Sequence parameter sets (raw NAL bytes, no start code).
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter sets (raw NAL bytes, no start code).
    pub pps: Vec<Vec<u8>>,
}

/// Result of turning a downloaded MP4 prefix into an Annex-B bitstream.
#[derive(Clone, Debug)]
pub struct Demuxed {
    /// Annex-B H.264: `[SPS][PPS][frame NAL units…]`, start-code framed.
    pub annexb: Vec<u8>,
    /// Number of frame NAL units lifted out of the first `mdat`.
    pub nalu_count: usize,
    /// SPS count taken from `avcC`.
    pub sps_count: usize,
    /// PPS count taken from `avcC`.
    pub pps_count: usize,
    /// True when no `avcC` was found and we relied on in-band parameter sets
    /// (avc3 / progressive) — SPS/PPS then come from the `mdat` itself.
    pub inband_params: bool,
}

const START_CODE: [u8; 4] = [0, 0, 0, 1];

fn be_u32(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be_u64(d: &[u8], off: usize) -> Option<u64> {
    d.get(off..off + 8)
        .map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// A parsed box: its four-character type and the byte range of its **payload**
/// (i.e. excluding the box header) within the original buffer.
#[derive(Clone, Copy, Debug)]
struct Box4 {
    typ: [u8; 4],
    payload: (usize, usize),
}

/// Iterate the boxes located directly within `data[start..end]`.
fn boxes(data: &[u8], start: usize, end: usize) -> Vec<Box4> {
    let end = end.min(data.len());
    let mut out = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        let size32 = match be_u32(data, pos) {
            Some(s) => s as u64,
            None => break,
        };
        let typ = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
        let (header_len, box_len) = if size32 == 1 {
            // 64-bit largesize follows the type.
            match be_u64(data, pos + 8) {
                Some(s) => (16usize, s),
                None => break,
            }
        } else if size32 == 0 {
            // Extends to the end of the enclosing range.
            (8usize, (end - pos) as u64)
        } else {
            (8usize, size32)
        };
        if box_len < header_len as u64 {
            break;
        }
        let box_end = pos + box_len as usize;
        if box_end > end {
            // Truncated (partial download) — still record what we can see so a
            // caller looking for a specific header can find it.
            out.push(Box4 { typ, payload: (pos + header_len, end) });
            break;
        }
        out.push(Box4 { typ, payload: (pos + header_len, box_end) });
        pos = box_end;
    }
    out
}

/// First child box of type `want` directly inside `payload`.
fn child(data: &[u8], payload: (usize, usize), want: &[u8; 4]) -> Option<(usize, usize)> {
    boxes(data, payload.0, payload.1)
        .into_iter()
        .find(|b| &b.typ == want)
        .map(|b| b.payload)
}

/// Parse an `avcC` (AVCDecoderConfigurationRecord) payload.
fn parse_avcc(b: &[u8]) -> Option<H264Config> {
    // configurationVersion(1) profile(1) compat(1) level(1) lengthSizeMinusOne(1)
    // numSPS(1) [len(2) sps]… numPPS(1) [len(2) pps]…
    if b.len() < 7 {
        return None;
    }
    let length_size = (b[4] & 0x03) as usize + 1;
    let num_sps = (b[5] & 0x1f) as usize;
    let mut off = 6usize;
    let mut sps = Vec::with_capacity(num_sps);
    for _ in 0..num_sps {
        let len = u16::from_be_bytes([*b.get(off)?, *b.get(off + 1)?]) as usize;
        off += 2;
        sps.push(b.get(off..off + len)?.to_vec());
        off += len;
    }
    let num_pps = *b.get(off)? as usize;
    off += 1;
    let mut pps = Vec::with_capacity(num_pps);
    for _ in 0..num_pps {
        let len = u16::from_be_bytes([*b.get(off)?, *b.get(off + 1)?]) as usize;
        off += 2;
        pps.push(b.get(off..off + len)?.to_vec());
        off += len;
    }
    Some(H264Config { length_size, sps, pps })
}

/// Navigate `moov → trak → mdia → minf → stbl → stsd → avc[13] → avcC` for the
/// first video track and return its H.264 config.
fn avc_config_from_moov(data: &[u8], moov: (usize, usize)) -> Option<H264Config> {
    for trak in boxes(data, moov.0, moov.1).into_iter().filter(|b| &b.typ == b"trak") {
        let Some(mdia) = child(data, trak.payload, b"mdia") else { continue };
        let Some(minf) = child(data, mdia, b"minf") else { continue };
        let Some(stbl) = child(data, minf, b"stbl") else { continue };
        let Some(stsd) = child(data, stbl, b"stsd") else { continue };
        // stsd payload: version(1)+flags(3)+entry_count(4) then sample entries.
        let entries = (stsd.0 + 8, stsd.1);
        for entry in boxes(data, entries.0, entries.1) {
            if &entry.typ[..3] == b"avc" {
                // VisualSampleEntry has a fixed 78-byte prefix before its child
                // boxes (SampleEntry 8 + VisualSampleEntry 70).
                let children = (entry.payload.0 + 78, entry.payload.1);
                if let Some(avcc) = child(data, children, b"avcC") {
                    if let Some(cfg) = parse_avcc(data.get(avcc.0..avcc.1)?) {
                        return Some(cfg);
                    }
                }
            }
        }
    }
    None
}

/// Append the length-prefixed NAL units found in `mdat` as Annex-B to `out`.
fn append_length_prefixed_nalus(mdat: &[u8], length_size: usize, out: &mut Vec<u8>) -> usize {
    let mut count = 0;
    let mut p = 0usize;
    while p + length_size <= mdat.len() {
        let mut n = 0usize;
        for i in 0..length_size {
            n = (n << 8) | mdat[p + i] as usize;
        }
        p += length_size;
        if n == 0 || p + n > mdat.len() {
            // Zero length is invalid; an over-run means the last NAL was cut off
            // by the partial download — stop cleanly with what we have.
            break;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&mdat[p..p + n]);
        p += n;
        count += 1;
    }
    count
}

/// Turn a downloaded MP4 prefix (init segment + first media fragment, or a
/// progressive prefix) into an Annex-B H.264 bitstream.
pub fn extract_annexb(data: &[u8]) -> Result<Demuxed, String> {
    let top = boxes(data, 0, data.len());
    let cfg = top
        .iter()
        .find(|b| &b.typ == b"moov")
        .and_then(|b| avc_config_from_moov(data, b.payload));

    let length_size = cfg.as_ref().map(|c| c.length_size).unwrap_or(4);
    let (sps_count, pps_count) =
        cfg.as_ref().map(|c| (c.sps.len(), c.pps.len())).unwrap_or((0, 0));

    let mut annexb = Vec::new();
    if let Some(cfg) = &cfg {
        for s in &cfg.sps {
            annexb.extend_from_slice(&START_CODE);
            annexb.extend_from_slice(s);
        }
        for p in &cfg.pps {
            annexb.extend_from_slice(&START_CODE);
            annexb.extend_from_slice(p);
        }
    }

    let mdat = top
        .iter()
        .find(|b| &b.typ == b"mdat")
        .map(|b| b.payload)
        .ok_or_else(|| "no `mdat` box in the downloaded prefix".to_string())?;
    let mdat_bytes = data.get(mdat.0..mdat.1).ok_or("mdat out of range")?;
    let nalu_count = append_length_prefixed_nalus(mdat_bytes, length_size, &mut annexb);

    if nalu_count == 0 {
        return Err(format!(
            "found `mdat` ({} bytes) but no length-prefixed NAL units (length_size={length_size})",
            mdat_bytes.len()
        ));
    }

    Ok(Demuxed {
        annexb,
        nalu_count,
        sps_count,
        pps_count,
        inband_params: cfg.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `[size:4][type:4][payload]` box.
    fn mk_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = size.to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn boxes_walks_siblings_and_reports_payload_ranges() {
        let mut buf = mk_box(b"ftyp", &[1, 2, 3, 4]);
        buf.extend(mk_box(b"mdat", &[9, 9]));
        let bs = boxes(&buf, 0, buf.len());
        assert_eq!(bs.len(), 2);
        assert_eq!(&bs[0].typ, b"ftyp");
        assert_eq!(&buf[bs[0].payload.0..bs[0].payload.1], &[1, 2, 3, 4]);
        assert_eq!(&bs[1].typ, b"mdat");
        assert_eq!(&buf[bs[1].payload.0..bs[1].payload.1], &[9, 9]);
    }

    #[test]
    fn parse_avcc_extracts_sps_pps_and_length_size() {
        // version=1 profile=0x42 compat=0 level=0x1e
        // lengthSizeMinusOne byte: 0xff -> length_size = (0x03)+1 = 4
        // numSPS byte: 0xe1 -> num = 1
        // sps len=4, bytes 67 42 00 1e
        // numPPS=1, pps len=4, bytes 68 ce 3c 80
        let avcc = [
            0x01, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1e, 0x01, 0x00,
            0x04, 0x68, 0xce, 0x3c, 0x80,
        ];
        let cfg = parse_avcc(&avcc).expect("avcC parses");
        assert_eq!(cfg.length_size, 4);
        assert_eq!(cfg.sps, vec![vec![0x67, 0x42, 0x00, 0x1e]]);
        assert_eq!(cfg.pps, vec![vec![0x68, 0xce, 0x3c, 0x80]]);
    }

    #[test]
    fn nalu_walk_converts_avcc_framing_to_annexb() {
        // Two NALs of 3 and 2 bytes, 4-byte length prefixes.
        let mut mdat = Vec::new();
        mdat.extend_from_slice(&(3u32).to_be_bytes());
        mdat.extend_from_slice(&[0x65, 0xaa, 0xbb]);
        mdat.extend_from_slice(&(2u32).to_be_bytes());
        mdat.extend_from_slice(&[0x41, 0xcc]);
        let mut out = Vec::new();
        let n = append_length_prefixed_nalus(&mdat, 4, &mut out);
        assert_eq!(n, 2);
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0, 0, 0, 1, 0x41, 0xcc]
        );
    }

    #[test]
    fn nalu_walk_stops_on_truncated_tail() {
        // Declares a 10-byte NAL but only 3 bytes are present (partial download).
        let mut mdat = (10u32).to_be_bytes().to_vec();
        mdat.extend_from_slice(&[1, 2, 3]);
        let mut out = Vec::new();
        assert_eq!(append_length_prefixed_nalus(&mdat, 4, &mut out), 0);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_annexb_end_to_end_from_synthetic_fmp4() {
        // Build stsd → avc1 → avcC nested inside moov/trak/mdia/minf/stbl, plus
        // an mdat with one length-prefixed NAL, and prove the SPS/PPS are
        // prepended and the frame NAL emitted as Annex-B.
        let avcc_payload = [
            0x01, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x02, 0x67, 0x10, 0x01, 0x00, 0x02, 0x68,
            0xee,
        ];
        let avcc = mk_box(b"avcC", &avcc_payload);
        // avc1 sample entry: 78-byte VisualSampleEntry prefix then avcC.
        let mut avc1_payload = vec![0u8; 78];
        avc1_payload.extend_from_slice(&avcc);
        let avc1 = mk_box(b"avc1", &avc1_payload);
        // stsd: version/flags(4) + entry_count(4) + avc1
        let mut stsd_payload = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd_payload.extend_from_slice(&avc1);
        let stsd = mk_box(b"stsd", &stsd_payload);
        let stbl = mk_box(b"stbl", &stsd);
        let minf = mk_box(b"minf", &stbl);
        let mdia = mk_box(b"mdia", &minf);
        let trak = mk_box(b"trak", &mdia);
        let moov = mk_box(b"moov", &trak);

        let mut mdat_payload = (3u32).to_be_bytes().to_vec();
        mdat_payload.extend_from_slice(&[0x65, 0x01, 0x02]);
        let mdat = mk_box(b"mdat", &mdat_payload);

        let mut file = mk_box(b"ftyp", &[0; 4]);
        file.extend(moov);
        file.extend(mdat);

        let out = extract_annexb(&file).expect("demux succeeds");
        assert_eq!(out.sps_count, 1);
        assert_eq!(out.pps_count, 1);
        assert_eq!(out.nalu_count, 1);
        assert!(!out.inband_params);
        assert_eq!(
            out.annexb,
            vec![
                0, 0, 0, 1, 0x67, 0x10, // SPS
                0, 0, 0, 1, 0x68, 0xee, // PPS
                0, 0, 0, 1, 0x65, 0x01, 0x02, // frame NAL
            ]
        );
    }
}
