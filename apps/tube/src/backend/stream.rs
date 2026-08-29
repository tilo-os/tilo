//! Fetch a real playable H.264 byte range for a [`StreamHandle`] and demux it
//! into Annex-B NAL units ready for [`crate::playback::Playback`].
//!
//! This is the network half of playback: given a resolved googlevideo stream
//! URL (see [`crate::backend::YtBackend::resolve_video_stream`]), it downloads
//! just enough of the container — the init segment plus the first media
//! fragment — using googlevideo's native `range=` query parameter, then hands
//! the bytes to [`crate::backend::mp4`] to lift out an Annex-B bitstream.
//!
//! It intentionally downloads a **bounded** prefix, not the whole file: one GOP
//! from the first fragment is all that is needed to decode a frame and prove
//! the pipeline. The blocking `reqwest` client means this must run on the fetch
//! worker (or a probe), never the UI thread.

use std::time::Duration;

use crate::backend::StreamHandle;
use crate::backend::mp4;

/// Outcome of downloading + demuxing a stream prefix.
#[derive(Clone, Debug)]
pub struct FetchedStream {
    /// Annex-B H.264 (`[SPS][PPS][NAL…]`) ready for `Playback::feed`.
    pub annexb: Vec<u8>,
    /// Bytes actually downloaded from googlevideo.
    pub downloaded: usize,
    /// Frame NAL units recovered from the first `mdat`.
    pub nalu_count: usize,
    /// SPS / PPS counts from the init segment's `avcC`.
    pub sps_count: usize,
    pub pps_count: usize,
    /// HTTP status of the range request.
    pub http_status: u16,
}

/// Maximum bytes to pull in the single prefix request.
///
/// googlevideo rejects a `range=` span larger than ~1 MiB with a 403 (500 KiB
/// is served, 1 MiB is refused), so the prefix is bounded well under that. At
/// 144p this comfortably spans the init segment plus the first several media
/// fragments — far more than the one keyframe GOP needed to decode a frame.
const MAX_PREFIX_BYTES: u64 = 480_000;

/// Build a blocking HTTP client for googlevideo range downloads.
///
/// Deliberately sets **no** default User-Agent: googlevideo validates the media
/// request's UA against the client that minted the URL, so [`fetch_h264`] sets
/// the exact per-stream UA ([`StreamHandle::user_agent`]) on each request. A
/// client-level default would be *appended* alongside it, producing two
/// `User-Agent` headers and a 403.
pub fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        // Only a fixed timeout is set (no proxy/TLS config that could fail to
        // parse), so `build()` cannot return `Err` here.
        .expect("reqwest client builder cannot fail with these fixed options")
}

/// Append googlevideo's native `range=start-end` query parameter to a stream
/// URL. googlevideo answers this with a `200` whose body is exactly that byte
/// range — more reliable across client types than an HTTP `Range` header.
fn with_range(url: &str, start: u64, end: u64) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}range={start}-{end}")
}

/// Download a bounded prefix of `handle`'s stream and demux it to Annex-B.
///
/// For an adaptive (DASH, video-only) stream the prefix is `0 ..= index_end +
/// FRAGMENT_TAIL`, which spans the init segment (`ftyp`+`moov` with the `avcC`)
/// and the first `moof`+`mdat` fragment. For a progressive/muxed stream (no
/// index) it falls back to a fixed cap; that path only decodes if the leading
/// `mdat` bytes happen to be video NAL units.
pub fn fetch_h264(
    client: &reqwest::blocking::Client,
    handle: &StreamHandle,
) -> Result<FetchedStream, String> {
    // A single bounded prefix from byte 0 spans the init segment (`ftyp`+`moov`
    // with the `avcC`) and the first media fragment(s). Never ask past the known
    // content length or the server's per-request cap.
    let end = handle
        .content_length
        .map(|len| MAX_PREFIX_BYTES.min(len.saturating_sub(1)))
        .unwrap_or(MAX_PREFIX_BYTES);

    let url = with_range(&handle.url, 0, end);
    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, &handle.user_agent)
        .send()
        .map_err(|e| format!("stream GET failed: {e}"))?;
    let http_status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(format!("stream GET returned HTTP {http_status}"));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("reading stream body: {e}"))?;
    let downloaded = bytes.len();
    if downloaded == 0 {
        return Err("stream body was empty".to_string());
    }

    let demuxed = mp4::extract_annexb(&bytes).map_err(|e| format!("demux: {e}"))?;
    Ok(FetchedStream {
        annexb: demuxed.annexb,
        downloaded,
        nalu_count: demuxed.nalu_count,
        sps_count: demuxed.sps_count,
        pps_count: demuxed.pps_count,
        http_status,
    })
}

// ---------------------------------------------------------------------------
// Continuous streaming
//
// `fetch_h264` above pulls one bounded prefix and decodes a single frame. Moving
// video needs the whole timeline: download the init segment + `sidx` once to
// learn every fragment's byte range, then page through the fragments on demand.
// Each fragment is self-contained (`moof`+`mdat`), so it can be demuxed into its
// per-frame access units ([`mp4::demux_fragment`]) and fed to a single reused
// decoder as playback advances.
// ---------------------------------------------------------------------------

/// A plan for streaming an adaptive DASH video: the decoder config from the init
/// segment plus the inclusive byte range of every media fragment, in playback
/// order. Produced by [`fetch_stream_plan`]; consumed fragment-by-fragment.
#[derive(Clone, Debug)]
pub struct StreamPlan {
    /// SPS/PPS + NAL length size from the init segment's `avcC`.
    pub config: mp4::H264Config,
    /// Inclusive `(start, end)` byte range of each media fragment.
    pub fragments: Vec<(u64, u64)>,
    /// Bytes downloaded to build the plan (init segment + `sidx`).
    pub head_bytes: usize,
    /// Per-fragment duration in `sidx` timescale ticks, aligned with `fragments`.
    pub durations: Vec<u32>,
    /// `sidx` timescale (ticks per second).
    pub timescale: u32,
}

/// googlevideo caps a single `range=` span at ~1 MiB. Fragments larger than this
/// (rare at 144p/240p) are fetched as contiguous sub-requests and concatenated.
const RANGE_CAP: u64 = 900_000;

/// Download an inclusive `[start, end]` byte range of the stream, replaying the
/// per-stream User-Agent googlevideo validates against.
fn download_range(
    client: &reqwest::blocking::Client,
    handle: &StreamHandle,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, String> {
    let url = with_range(&handle.url, start, end);
    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, &handle.user_agent)
        .send()
        .map_err(|e| format!("range GET failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "range GET returned HTTP {}",
            resp.status().as_u16()
        ));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("reading range body: {e}"))?;
    Ok(bytes.to_vec())
}

/// Download the init segment (`ftyp`+`moov`) and the segment index (`sidx`) and
/// build a [`StreamPlan`]. Requires an adaptive stream carrying an `index_range`
/// (the `sidx` location); errors otherwise so the caller can fall back to the
/// single bounded-prefix decode.
pub fn fetch_stream_plan(
    client: &reqwest::blocking::Client,
    handle: &StreamHandle,
) -> Result<StreamPlan, String> {
    let index = handle
        .index_range
        .ok_or("stream has no index_range (sidx); not a fragmented DASH stream")?;
    // One request from byte 0 to the end of the sidx captures ftyp+moov (with the
    // avcC) AND the sidx together — a few tens of KiB, well under the range cap.
    let head = download_range(client, handle, 0, index.1)?;
    if head.is_empty() {
        return Err("init/sidx download was empty".into());
    }
    let config = mp4::parse_init(&head).ok_or("no avcC in init segment")?;
    let sidx = mp4::parse_sidx(&head).ok_or("no/invalid sidx in index range")?;

    let mut fragments = Vec::with_capacity(sidx.sizes.len());
    let mut durations = Vec::with_capacity(sidx.sizes.len());
    // The first fragment begins just after the sidx box, offset by `first_offset`.
    let mut cursor = index.1 + 1 + sidx.first_offset;
    for (size, dur) in sidx.sizes.iter().zip(sidx.durations.iter()) {
        if *size == 0 {
            continue;
        }
        let start = cursor;
        let end = cursor + *size as u64 - 1;
        fragments.push((start, end));
        durations.push(*dur);
        cursor = end + 1;
    }
    if fragments.is_empty() {
        return Err("sidx listed no media fragments".into());
    }
    Ok(StreamPlan {
        config,
        fragments,
        head_bytes: head.len(),
        durations,
        timescale: sidx.timescale.max(1),
    })
}

/// Download one media fragment by its (inclusive) byte range, sub-splitting into
/// contiguous requests when the span exceeds the server's per-request cap.
pub fn fetch_fragment_bytes(
    client: &reqwest::blocking::Client,
    handle: &StreamHandle,
    (start, end): (u64, u64),
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut s = start;
    while s <= end {
        let e = (s + RANGE_CAP - 1).min(end);
        let chunk = download_range(client, handle, s, e)?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
        s = e + 1;
    }
    if out.is_empty() {
        return Err("fragment download was empty".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_range_appends_query_param() {
        assert_eq!(
            with_range("https://x/vp?a=1", 0, 99),
            "https://x/vp?a=1&range=0-99"
        );
        assert_eq!(with_range("https://x/vp", 0, 99), "https://x/vp?range=0-99");
    }
}
