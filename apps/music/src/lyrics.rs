//! Lyrics — a synced-or-plain lyrics layer with a pluggable source.
//!
//! Lyrics for the now-playing track are shown in a dedicated view. A track's
//! lyrics may be **synced** (time-stamped per line, so the current line can be
//! highlighted against the playhead) or **plain** (an unsynced block). The
//! data model is the same either way: a `Vec<LyricsLine>` whose timestamps are
//! `None` for plain lyrics.
//!
//! ## Bundled and live lyrics
//! A small **bundled** library of sample lyrics ([`bundled`]) is always
//! compiled so the `--shots` screenshot and the offline first-run show a
//! populated, highlightable lyrics view with zero network. The **live** fetch
//! from [LRCLIB](https://lrclib.net) (`GET https://lrclib.net/api/get` keyed
//! on track + artist + album + duration, returning `syncedLyrics` /
//! `plainLyrics`) is gated behind the `lyrics` cargo feature and runs as a
//! task on the app runtime (`tilo_net`), so the UI never blocks. The pure LRC parser below is always
//! compiled and unit-tested, so the contract is exercised even offline.

/// One line of lyrics. `at_ms` is the line's start time for synced lyrics, or
/// `None` for an unsynced (plain) block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsLine {
    /// Line start time in milliseconds (synced lyrics only).
    pub at_ms: Option<u64>,
    pub text: String,
}

/// A resolved lyrics result for one track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<LyricsLine>,
    /// Whether any line carries a timestamp (i.e. the lyrics are synced).
    pub synced: bool,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The index of the line that should be highlighted at `position_ms` for
    /// synced lyrics: the last line whose timestamp is `<= position_ms`.
    /// Returns `None` for plain lyrics or before the first timestamped line.
    pub fn current_line(&self, position_ms: u64) -> Option<usize> {
        if !self.synced {
            return None;
        }
        let mut cur = None;
        for (i, l) in self.lines.iter().enumerate() {
            if let Some(at) = l.at_ms {
                if at <= position_ms {
                    cur = Some(i);
                } else {
                    break;
                }
            }
        }
        cur
    }
}

/// Parse an LRC body (`[mm:ss.xx] text` lines) into synced [`LyricsLine`]s.
/// Lines without a leading timestamp are treated as plain (`at_ms = None`).
/// Metadata tags (`[ar:…]`, `[ti:…]`, etc.) are skipped.
pub fn parse_lrc(body: &str) -> Lyrics {
    let mut lines = Vec::new();
    let mut synced = false;
    for raw in body.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some((ts, text)) = parse_lrc_timestamp(line) {
            synced = true;
            lines.push(LyricsLine {
                at_ms: Some(ts),
                text: text.to_string(),
            });
        } else if line.starts_with('[') && line.contains(':') && line.ends_with(']') {
            // A metadata tag like [ar:Artist] — skip.
        } else {
            lines.push(LyricsLine {
                at_ms: None,
                text: line.to_string(),
            });
        }
    }
    Lyrics { lines, synced }
}

/// Parse a leading `[mm:ss.xx]` (or `[mm:ss]`) timestamp, returning the
/// milliseconds + the remaining text. `None` if there's no valid timestamp.
fn parse_lrc_timestamp(line: &str) -> Option<(u64, &str)> {
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let (ts, after) = rest.split_at(close);
    let after = after.strip_prefix(']').unwrap_or(after);
    let (mm, ss) = ts.split_once(':')?;
    let minutes: u64 = mm.trim().parse().ok()?;
    // Seconds may carry a fractional part: `ss` or `ss.xx`.
    let seconds: f64 = ss.trim().parse().ok()?;
    let ms = minutes * 60_000 + (seconds * 1000.0).round() as u64;
    Some((ms, after.trim_start()))
}

/// Build a plain (unsynced) lyrics block from raw text.
pub fn plain(body: &str) -> Lyrics {
    let lines = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| LyricsLine {
            at_ms: None,
            text: l.trim().to_string(),
        })
        .collect();
    Lyrics {
        lines,
        synced: false,
    }
}

/// Metadata needed to resolve lyrics for a track from a remote source.
#[derive(Clone, Debug, Default)]
pub struct TrackMeta {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_s: u32,
}

/// Resolve lyrics for a track. Tries the bundled library first (always
/// present for the seed tracks + offline), then the live source when the
/// `lyrics` feature is enabled. Returns `None` when nothing is available.
///
/// This is the resolution seam the worker ([`LyricsWorker`]) runs on the app
/// runtime; the bundled path makes it usable offline / in `--shots` with no
/// network.
pub async fn resolve(meta: &TrackMeta) -> Option<Lyrics> {
    if let Some(l) = bundled::for_track(&meta.track_id) {
        return Some(l);
    }
    live_fetch(meta).await
}

/// The live LRCLIB fetch. Without an HTTP stack (`lyrics` or `live` feature —
/// both pull `tilo-net`) this is a stub that documents the request it *would*
/// make and returns `None` (the worker treats this as "no lyrics right now").
#[cfg(not(any(feature = "lyrics", feature = "live")))]
#[allow(clippy::unused_async, reason = "the same signature as the live fetch")]
async fn live_fetch(meta: &TrackMeta) -> Option<Lyrics> {
    let _ = lrclib_url(meta);
    None
}

/// Real synced-or-plain lyrics from LRCLIB. Compiled whenever an HTTP stack is
/// available (`lyrics` or the `live` build), so `--features live,audio` fetches
/// real lyrics for the playing track.
///
/// - **normalisation** — `YouTube` titles/artists carry noise (`(Official Music
///   Video)`, `… - Topic`, `feat. …`) that never matches LRCLIB's clean
///   metadata. [`lrclib_url`] / [`lrclib_search_url`] normalise both before
///   querying, so `Californication (Official Music Video)` by
///   `Red Hot Chili Peppers - Topic` actually resolves.
/// - **retry + backoff + timeout** — a transient network blip or an LRCLIB
///   rate-limit (429 / 5xx) is retried a couple of times with backoff behind
///   an 8 s per-request timeout.
/// - **cache** — a resolved result (hit *or* miss) is memoised by normalised
///   identity, so a revisit / prefetch is instant and never re-hammers LRCLIB.
/// - **fallback** — an exact `/api/get` miss (`YouTube` durations rarely match
///   to the second) falls back to `/api/search`.
#[cfg(any(feature = "lyrics", feature = "live"))]
async fn live_fetch(meta: &TrackMeta) -> Option<Lyrics> {
    let key = cache_key(meta);
    match cache_get(&key) {
        CacheEntry::Hit(l) => return Some(l),
        CacheEntry::Miss => return None,
        CacheEntry::Absent => {}
    }
    let result = live_fetch_uncached(meta).await;
    cache_put(key, result.clone());
    result
}

/// The shared LRCLIB client.
#[cfg(any(feature = "lyrics", feature = "live"))]
fn lrclib_client() -> Option<&'static tilo_net::Client> {
    static CLIENT: std::sync::OnceLock<Option<tilo_net::Client>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .user_agent("Tilo-Music (https://tilo)")
                .build()
                .ok()
        })
        .as_ref()
}

#[cfg(any(feature = "lyrics", feature = "live"))]
async fn live_fetch_uncached(meta: &TrackMeta) -> Option<Lyrics> {
    let client = lrclib_client()?;
    // 1. Exact match by (normalised) track/artist/album/duration.
    if let Some(l) = lrclib_from_object(client, &lrclib_url(meta)).await {
        return Some(l);
    }
    // 2. Fuzzy search fallback — YouTube durations rarely match LRCLIB's to the
    //    second, so `/api/get` frequently 404s. `/api/search` returns candidates
    //    (best first); take the first with real synced (then plain) lyrics.
    lrclib_from_search(client, meta).await
}

/// GET `url` as text, retrying a couple of times with linear backoff on a
/// *transient* failure (a network error, a 429 rate-limit, or a 5xx). A 404 or
/// any other definitive non-success returns `None` immediately so the caller
/// moves on to the next strategy rather than burning retries on a real miss.
#[cfg(any(feature = "lyrics", feature = "live"))]
async fn get_text_retry(client: &tilo_net::Client, url: &str) -> Option<String> {
    use tilo_net::StatusCode;
    const MAX_LYRICS_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
    for attempt in 0..3u32 {
        let backoff = std::time::Duration::from_millis(300 * u64::from(attempt + 1));
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::NOT_FOUND {
                    return None; // a real miss — don't retry, let the fallback run
                }
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    ui::rt::sleep(backoff).await;
                    continue; // transient — back off and retry
                }
                if !status.is_success() {
                    return None;
                }
                match resp.bytes_limited(MAX_LYRICS_RESPONSE_BYTES).await {
                    Ok(body) => return Some(String::from_utf8_lossy(&body).into_owned()),
                    Err(tilo_net::NetError::TooLarge { .. }) => return None,
                    Err(_) => ui::rt::sleep(backoff).await,
                }
            }
            Err(_) => ui::rt::sleep(backoff).await,
        }
    }
    None
}

/// Parse a single LRCLIB record (from `/api/get`) into [`Lyrics`], preferring
/// synced over plain. `None` on a 404 / empty record.
#[cfg(any(feature = "lyrics", feature = "live"))]
async fn lrclib_from_object(client: &tilo_net::Client, url: &str) -> Option<Lyrics> {
    let body = get_text_retry(client, url).await?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    lyrics_from_record(&json)
}

/// Search LRCLIB by track + artist and return the best candidate's lyrics.
#[cfg(any(feature = "lyrics", feature = "live"))]
async fn lrclib_from_search(client: &tilo_net::Client, meta: &TrackMeta) -> Option<Lyrics> {
    let url = lrclib_search_url(meta);
    let body = get_text_retry(client, &url).await?;
    let arr: serde_json::Value = serde_json::from_str(&body).ok()?;
    let list = arr.as_array()?;
    // Prefer the first candidate carrying synced lyrics; else the first plain.
    for rec in list {
        if rec
            .get("syncedLyrics")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
        {
            return lyrics_from_record(rec);
        }
    }
    for rec in list {
        if let Some(l) = lyrics_from_record(rec) {
            return Some(l);
        }
    }
    None
}

/// Extract synced-then-plain lyrics from one LRCLIB JSON record.
#[cfg(any(feature = "lyrics", feature = "live"))]
fn lyrics_from_record(json: &serde_json::Value) -> Option<Lyrics> {
    if let Some(synced) = json.get("syncedLyrics").and_then(|v| v.as_str())
        && !synced.trim().is_empty()
    {
        return Some(parse_lrc(synced));
    }
    if let Some(p) = json.get("plainLyrics").and_then(|v| v.as_str())
        && !p.trim().is_empty()
    {
        return Some(plain(p));
    }
    None
}

/// Collapse internal runs of whitespace to a single space and trim the ends.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keywords that mark a bracketed segment as a *tag* (a video/remaster/quality
/// note) rather than part of the actual song name — so it's dropped from the
/// LRCLIB query. Genuine parentheticals like `(Acoustic)` are kept.
const TITLE_TAG_KEYWORDS: &[&str] = &[
    "official",
    "video",
    "audio",
    "lyric",
    "lyrics",
    "visualizer",
    "visualiser",
    "remaster",
    "remastered",
    "explicit",
    "hd",
    "hq",
    "4k",
    "mv",
    "m/v",
    "feat",
    "ft",
    "featuring",
];

fn segment_is_tag(seg: &str) -> bool {
    let low = seg.to_ascii_lowercase();
    TITLE_TAG_KEYWORDS.iter().any(|k| {
        low.split(|c: char| !c.is_ascii_alphanumeric() && c != '/')
            .any(|w| w == *k)
    })
}

/// Cut a trailing `feat.` / `ft.` / `featuring` credit off a title (LRCLIB keys
/// on the base song name, not the featured-artist list).
fn strip_feat(s: &str) -> String {
    let low = s.to_ascii_lowercase();
    for marker in [" feat.", " feat ", " ft.", " ft ", " featuring "] {
        if let Some(idx) = low.find(marker) {
            return s.get(..idx).unwrap_or(s).to_string();
        }
    }
    s.to_string()
}

/// Normalise a (`YouTube`) track title for an LRCLIB lookup: drop bracketed tag
/// segments (`(Official Music Video)`, `[Remastered 2011]`, …) and any trailing
/// `feat.` credit, then collapse whitespace. Idempotent on already-clean titles,
/// so a plain `Neon Mileage` is returned unchanged.
pub fn normalize_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        let close = match ch {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            _ => {
                out.push(ch);
                continue;
            }
        };
        let mut seg = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == close {
                closed = true;
                break;
            }
            seg.push(c);
        }
        if !segment_is_tag(&seg) {
            // Keep a genuine parenthetical (e.g. `(Acoustic)`).
            out.push(ch);
            out.push_str(&seg);
            if closed {
                out.push(close);
            }
        }
    }
    collapse_ws(&strip_feat(&out))
}

/// Normalise a (`YouTube` Music) artist name for an LRCLIB lookup: strip the
/// auto-generated `… - Topic` channel suffix and collapse whitespace.
pub fn normalize_artist(raw: &str) -> String {
    let mut s = collapse_ws(raw);
    if s.to_ascii_lowercase().ends_with(" - topic") {
        s.truncate(s.len() - " - Topic".len());
        s = collapse_ws(&s);
    }
    s
}

/// Build the LRCLIB `/api/search?track_name=…&artist_name=…` URL from the
/// normalised metadata.
#[cfg(any(feature = "lyrics", feature = "live"))]
fn lrclib_search_url(meta: &TrackMeta) -> String {
    format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        ui::uri::encode_component(&normalize_title(&meta.title)),
        ui::uri::encode_component(&normalize_artist(&meta.artist)),
    )
}

/// Build the LRCLIB `/api/get` request URL for a track from its normalised
/// metadata. Always compiled so the request contract is documented + testable
/// even offline.
pub fn lrclib_url(meta: &TrackMeta) -> String {
    format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
        ui::uri::encode_component(&normalize_title(&meta.title)),
        ui::uri::encode_component(&normalize_artist(&meta.artist)),
        ui::uri::encode_component(&normalize_title(&meta.album)),
        meta.duration_s
    )
}

/// Process-wide memo of resolved lyrics keyed by normalised (title, artist), so
/// a revisit / prefetch of the same song is instant and never re-hammers
/// LRCLIB. Caches misses (`None`) too, so a track without lyrics isn't refetched
/// on every visit.
#[cfg(any(feature = "lyrics", feature = "live"))]
fn cache_key(meta: &TrackMeta) -> String {
    format!(
        "{}\u{1}{}",
        normalize_title(&meta.title).to_ascii_lowercase(),
        normalize_artist(&meta.artist).to_ascii_lowercase(),
    )
}

#[cfg(any(feature = "lyrics", feature = "live"))]
fn lyrics_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Option<Lyrics>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<Lyrics>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The result of a lyrics cache lookup: no entry at all, a cached miss (a
/// prior lookup found no lyrics for this track), or a cached hit.
#[cfg(any(feature = "lyrics", feature = "live"))]
enum CacheEntry {
    Absent,
    Miss,
    Hit(Lyrics),
}

#[cfg(any(feature = "lyrics", feature = "live"))]
fn cache_get(key: &str) -> CacheEntry {
    let Ok(map) = lyrics_cache().lock() else {
        return CacheEntry::Absent;
    };
    match map.get(key).cloned() {
        Some(Some(l)) => CacheEntry::Hit(l),
        Some(None) => CacheEntry::Miss,
        None => CacheEntry::Absent,
    }
}

#[cfg(any(feature = "lyrics", feature = "live"))]
fn cache_put(key: String, value: Option<Lyrics>) {
    if let Ok(mut map) = lyrics_cache().lock() {
        map.insert(key, value);
    }
}

/// The lyrics worker: each request resolves as a task on the app runtime and
/// delivers over a channel. The default live build can fetch network lyrics;
/// builds without `live` and `lyrics` resolve bundled lyrics through the same
/// interface.
pub struct LyricsWorker {
    tx: std::sync::mpsc::Sender<(String, Lyrics)>,
    rx: std::sync::mpsc::Receiver<(String, Lyrics)>,
}

impl LyricsWorker {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(String, Lyrics)>();
        Self { tx, rx }
    }

    /// Request lyrics for a track. Non-blocking; the result arrives via
    /// [`Self::poll`].
    pub fn request(&self, meta: TrackMeta) {
        let tx = self.tx.clone();
        // ALWAYS deliver a result for the requested track — an empty `Lyrics`
        // on a miss — so the UI clears its "Finding lyrics…" pending state and
        // shows "No lyrics found" instead of hanging forever when a lookup
        // misses or a fetch fails.
        match ui::rt::try_app() {
            Ok(runtime) => drop(runtime.spawn(async move {
                let lyrics = resolve(&meta).await.unwrap_or_default();
                let _ = tx.send((meta.track_id, lyrics));
            })),
            Err(_) => {
                let _ = tx.send((meta.track_id, Lyrics::default()));
            }
        }
    }

    /// Drain any resolved lyrics that have arrived (track id + lyrics).
    pub fn poll(&self) -> Option<(String, Lyrics)> {
        self.rx.try_recv().ok()
    }
}

impl Default for LyricsWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// The bundled offline sample lyrics keyed by seed track id. Synced where it
/// makes a more interesting screenshot; plain otherwise.
pub mod bundled {
    use super::{Lyrics, parse_lrc, plain};

    /// Look up bundled lyrics for a seed track id, if any.
    pub fn for_track(track_id: &str) -> Option<Lyrics> {
        let lrc = match track_id {
            "tk_g1" => GLASSLIGHT_LRC,
            "tk_a1" => NEON_MILEAGE_LRC,
            "tk_u1" => PULSEWIDTH_PLAIN,
            "tk_h1" => HARBOR_LIGHTS_LRC,
            "tk_p1" => PAPER_BOATS_LRC,
            _ => return None,
        };
        Some(if lrc.contains('[') {
            parse_lrc(lrc)
        } else {
            plain(lrc)
        })
    }

    const GLASSLIGHT_LRC: &str = "\
[00:00.00] (Glasslight — Aurora Skies)
[00:08.50] Held to the window, a paper sun
[00:14.20] Counting the colours one by one
[00:21.00] Slow as the morning, soft as the rain
[00:28.40] Glasslight is breaking over the lane
[00:36.10] Carry me, carry me, easy and bright
[00:43.80] Into the hush of the glasslight
[00:52.00] Shadows fold quiet behind the door
[01:00.20] Nothing to keep and nothing to store";

    const NEON_MILEAGE_LRC: &str = "\
[00:00.00] (Neon Mileage — Midnight Transit)
[00:06.00] Tail lights bleeding down the interstate
[00:12.50] Radio humming, it's getting late
[00:19.00] Neon mileage on an empty tank
[00:25.50] Chasing the dark to the riverbank
[00:33.00] Drive, just drive, till the morning comes
[00:40.00] Drive, just drive, to the wordless hum";

    const HARBOR_LIGHTS_LRC: &str = "\
[00:00.00] (Harbor Lights — Neon Harbor)
[00:07.00] Ferries are humming across the bay
[00:13.40] Harbor lights wash the dusk away
[00:20.10] Cassette-warm summer on a salt-air breeze
[00:27.00] Boulevard glowing through the harbor trees";

    const PAPER_BOATS_LRC: &str = "\
[00:00.00] (Paper Boats — Aurora Skies)
[00:09.00] Folding the evening into little boats
[00:16.00] Setting them loose where the river floats
[00:23.50] Undertow pulling the day from view
[00:31.00] Lighthouse turning, I'm coming through";

    const PULSEWIDTH_PLAIN: &str = "\
Pulsewidth (instrumental)

A modular pulse, widening slow —
no words here, only the low
sidechained tide of a resonant glow.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn lrc_parsing_and_current_line_tracking() {
        // Phase `parse_lrc_extracts_timestamps`.
        {
            let lr = parse_lrc("[00:08.50] hello\n[01:00.00] world");
            assert!(lr.synced);
            assert_eq!(lr.lines.len(), 2);
            assert_eq!(lr.lines[0].at_ms, Some(8_500));
            assert_eq!(lr.lines[0].text, "hello");
            assert_eq!(lr.lines[1].at_ms, Some(60_000));
        }

        // Phase `parse_lrc_skips_metadata_tags`.
        {
            let lr = parse_lrc("[ar:Aurora]\n[ti:Glasslight]\n[00:01.00] first line");
            assert_eq!(lr.lines.len(), 1);
            assert_eq!(lr.lines[0].text, "first line");
        }

        // Phase `current_line_tracks_playhead`.
        {
            let lr = parse_lrc("[00:00.00] a\n[00:10.00] b\n[00:20.00] c");
            assert_eq!(lr.current_line(0), Some(0));
            assert_eq!(lr.current_line(9_999), Some(0));
            assert_eq!(lr.current_line(10_000), Some(1));
            assert_eq!(lr.current_line(25_000), Some(2));
        }

        // Phase `plain_lyrics_have_no_current_line`.
        {
            let lr = plain("just\nsome\nwords");
            assert!(!lr.synced);
            assert_eq!(lr.lines.len(), 3);
            assert_eq!(lr.current_line(5_000), None);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bundled_lyrics_resolve_first_and_reach_the_worker() {
        // Phase `bundled_glasslight_is_synced`.
        {
            let lr = bundled::for_track("tk_g1").expect("bundled lyrics for tk_g1");
            assert!(lr.synced);
            assert!(lr.lines.len() >= 6);
            // A later playhead lands on a later line.
            assert!(lr.current_line(60_000).unwrap() > lr.current_line(10_000).unwrap());
        }

        // Phase `bundled_pulsewidth_is_plain`.
        {
            let lr = bundled::for_track("tk_u1").expect("bundled lyrics for tk_u1");
            assert!(!lr.synced);
            assert!(!lr.is_empty());
        }

        // Phase `unknown_track_has_no_bundled_lyrics`.
        {
            assert!(bundled::for_track("tk_does_not_exist").is_none());
        }

        // Phase `resolve_prefers_bundled`.
        {
            let meta = TrackMeta {
                track_id: "tk_g1".into(),
                ..Default::default()
            };
            let lr = ui::rt::app()
                .block_on(resolve(&meta))
                .expect("the app runtime runs")
                .expect("bundled resolves offline");
            assert!(lr.synced);
        }

        // Phase `worker_delivers_bundled_lyrics`.
        {
            let w = LyricsWorker::new();
            w.request(TrackMeta {
                track_id: "tk_g1".into(),
                ..Default::default()
            });
            // Poll briefly for the worker thread to resolve + deliver.
            let mut got = None;
            for _ in 0..200 {
                if let Some(r) = w.poll() {
                    got = Some(r);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let (id, lyrics) = got.expect("worker delivered lyrics");
            assert_eq!(id, "tk_g1");
            assert!(lyrics.synced);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn lrclib_queries_normalise_youtube_metadata_and_encode() {
        // Phase `lrclib_url_encodes_query`.
        {
            let meta = TrackMeta {
                track_id: "x".into(),
                title: "Neon Mileage".into(),
                artist: "Midnight Transit".into(),
                album: "After Hours Drive".into(),
                duration_s: 252,
            };
            let url = lrclib_url(&meta);
            assert!(url.starts_with("https://lrclib.net/api/get?"));
            assert!(url.contains("track_name=Neon%20Mileage"));
            assert!(url.contains("artist_name=Midnight%20Transit"));
            assert!(url.contains("duration=252"));
        }

        // Phase `normalize_title_strips_youtube_tags_but_keeps_real_parentheticals`.
        {
            assert_eq!(
                normalize_title("Californication (Official Music Video)"),
                "Californication"
            );
            assert_eq!(normalize_title("Otherside [Remastered 2011]"), "Otherside");
            assert_eq!(
                normalize_title("Scar Tissue (Official Audio)"),
                "Scar Tissue"
            );
            assert_eq!(
                normalize_title("Say It Right feat. Nelly Furtado"),
                "Say It Right"
            );
            assert_eq!(
                normalize_title("Under the Bridge (feat. Someone)"),
                "Under the Bridge"
            );
            // A genuine version parenthetical is preserved.
            assert_eq!(
                normalize_title("Californication (Acoustic)"),
                "Californication (Acoustic)"
            );
            // Already clean → unchanged (idempotent).
            assert_eq!(normalize_title("Neon Mileage"), "Neon Mileage");
        }

        // Phase `normalize_artist_strips_topic_suffix`.
        {
            assert_eq!(
                normalize_artist("Red Hot Chili Peppers - Topic"),
                "Red Hot Chili Peppers"
            );
            assert_eq!(
                normalize_artist("Red Hot Chili Peppers"),
                "Red Hot Chili Peppers"
            );
            assert_eq!(
                normalize_artist("  Midnight   Transit  "),
                "Midnight Transit"
            );
        }

        // Phase `lrclib_query_normalizes_youtube_metadata`.
        // A YouTube-flavoured title + a `… - Topic` artist must produce a
        // well-formed, normalised LRCLIB query, not one that carries the raw
        // noise through and misses on `/api/get`.
        {
            let meta = TrackMeta {
                track_id: "yt:abc".into(),
                title: "Californication (Official Music Video)".into(),
                artist: "Red Hot Chili Peppers - Topic".into(),
                album: "Californication (Remastered)".into(),
                duration_s: 329,
            };
            let url = lrclib_url(&meta);
            assert!(url.starts_with("https://lrclib.net/api/get?"));
            assert!(
                url.contains("track_name=Californication&"),
                "title tag stripped: {url}"
            );
            assert!(
                url.contains("artist_name=Red%20Hot%20Chili%20Peppers&"),
                "topic suffix stripped: {url}"
            );
            assert!(
                url.contains("album_name=Californication&"),
                "album tag stripped: {url}"
            );
            assert!(url.ends_with("duration=329"), "{url}");
        }

        // Phase `lrclib_search_url_uses_normalized_metadata`.
        #[cfg(any(feature = "lyrics", feature = "live"))]
        {
            let meta = TrackMeta {
                track_id: "yt:abc".into(),
                title: "Californication (Official Music Video)".into(),
                artist: "Red Hot Chili Peppers - Topic".into(),
                album: String::new(),
                duration_s: 329,
            };
            let url = lrclib_search_url(&meta);
            assert!(url.starts_with("https://lrclib.net/api/search?"));
            assert!(url.contains("track_name=Californication"), "{url}");
            assert!(
                url.contains("artist_name=Red%20Hot%20Chili%20Peppers"),
                "{url}"
            );
        }
    }
}
