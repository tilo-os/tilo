//! Fetch a real playable H.264 byte range for a [`StreamHandle`] and demux it
//! into Annex-B NAL units ready for [`ui::widget::VideoPlayer`].
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

use crate::backend::mp4;
use crate::backend::StreamHandle;

/// Outcome of downloading + demuxing a stream prefix.
#[derive(Clone, Debug)]
pub struct FetchedStream {
    /// Annex-B H.264 (`[SPS][PPS][NAL…]`) ready for `VideoPlayer::feed_frame`.
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
        .expect("reqwest client")
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
    let bytes = resp.bytes().map_err(|e| format!("reading stream body: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_range_appends_query_param() {
        assert_eq!(with_range("https://x/vp?a=1", 0, 99), "https://x/vp?a=1&range=0-99");
        assert_eq!(with_range("https://x/vp", 0, 99), "https://x/vp?range=0-99");
    }
}
