//! Lyrics: a synced or plain lyrics layer, with a bundled library always present and
//! an LRCLIB fetch under `live`, run on the app runtime so the UI never blocks.

/// One lyric line; `at_ms` is its start for synced lyrics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsLine {
    /// Line start time in milliseconds, synced lyrics only.
    pub at_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<LyricsLine>,
    pub synced: bool,
}

impl Lyrics {
    /// The line to highlight at `position_ms`; `None` for plain lyrics or before the first.
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

/// Parse an LRC body (`[mm:ss.xx] text`); a line without a timestamp stays plain and
/// `[ar:…]` tags are skipped.
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
        } else {
            lines.push(LyricsLine {
                at_ms: None,
                text: line.to_string(),
            });
        }
    }
    Lyrics { lines, synced }
}

/// A leading `[mm:ss.xx]` timestamp in milliseconds, with the text after it.
fn parse_lrc_timestamp(line: &str) -> Option<(u64, &str)> {
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let (ts, after) = rest.split_at(close);
    let after = after.strip_prefix(']').unwrap_or(after);
    let (mm, ss) = ts.split_once(':')?;
    let minutes: u64 = mm.trim().parse().ok()?;
    let seconds: f64 = ss.trim().parse().ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a timestamp's seconds and fraction, read from the words themselves"
    )]
    let ms = minutes
        .saturating_mul(60_000)
        .saturating_add((seconds * 1000.0).round() as u64);
    Some((ms, after.trim_start()))
}

/// A plain, unsynced block from raw text.
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

/// Bundled lyrics first, then LRCLIB under `live`; `None` when there are none.
pub async fn resolve(meta: &TrackMeta) -> Option<Lyrics> {
    if let Some(l) = bundled::for_track(&meta.track_id) {
        return Some(l);
    }
    live_fetch(meta).await
}

/// Without `live` this is a stub that returns `None`.
#[cfg(not(feature = "live"))]
#[allow(
    clippy::unused_async,
    reason = "`resolve` awaits it, and the live build's fetch is async"
)]
async fn live_fetch(meta: &TrackMeta) -> Option<Lyrics> {
    let _ = lrclib_url(meta);
    None
}

/// The words of the song called `title` by `artist`, waited for on the app runtime: what the
/// connector asks when an app asks for lyrics. Empty lines for a song nothing knows.
///
/// # Errors
///
/// Why the words could not be asked for: this process holds no app runtime to ask on.
pub fn fetch(title: &str, artist: &str) -> Result<Lyrics, String> {
    let runtime = ui::rt::try_app().map_err(|e| e.to_string())?;
    let meta = TrackMeta {
        title: title.to_owned(),
        artist: artist.to_owned(),
        ..TrackMeta::default()
    };
    runtime
        .block_on(resolve(&meta))
        .map(Option::unwrap_or_default)
        .map_err(|error| error.to_string())
}

/// Lyrics from LRCLIB under `live`: an 8 s timeout, normalised metadata, hits and misses
/// cached, an exact lookup falling back to search.
#[cfg(feature = "live")]
async fn live_fetch(meta: &TrackMeta) -> Option<Lyrics> {
    let key = cache_key(meta);
    match cache_get(&key) {
        CacheEntry::Hit(l) => return Some(l),
        CacheEntry::Miss => return None,
        CacheEntry::Absent => {}
    }
    let result = live_fetch_uncached(meta).await.ok()?;
    cache_put(key, result.clone());
    result
}

#[cfg(feature = "live")]
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

#[cfg(feature = "live")]
async fn live_fetch_uncached(meta: &TrackMeta) -> Result<Option<Lyrics>, Unreachable> {
    let client = lrclib_client().ok_or(Unreachable)?;
    // Exact match first, by the normalised track, artist, album and duration.
    if let Some(l) = lrclib_from_object(client, &lrclib_url(meta)).await? {
        return Ok(Some(l));
    }
    // Durations rarely match LRCLIB's to the second: search, best candidate first.
    lrclib_from_search(client, meta).await
}

/// LRCLIB could not be asked, unlike a miss: nothing is cached, so the next visit asks again.
#[cfg(feature = "live")]
struct Unreachable;

/// GET `url` as text: `Ok(None)` when LRCLIB answered that it has nothing.
#[cfg(feature = "live")]
async fn get_text(client: &tilo_net::Client, url: &str) -> Result<Option<String>, Unreachable> {
    const MAX_LYRICS_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
    let resp = client.get(url).send().await.map_err(|_| Unreachable)?;
    let status = resp.status();
    if status == tilo_net::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(Unreachable);
    }
    if !status.is_success() {
        return Ok(None);
    }
    match resp.bytes_limited(MAX_LYRICS_RESPONSE_BYTES).await {
        Ok(body) => Ok(Some(String::from_utf8_lossy(&body).into_owned())),
        Err(tilo_net::NetError::TooLarge { .. }) => Ok(None),
        Err(_) => Err(Unreachable),
    }
}

/// A `/api/get` resource as lyrics, synced preferred.
#[cfg(feature = "live")]
async fn lrclib_from_object(
    client: &tilo_net::Client,
    url: &str,
) -> Result<Option<Lyrics>, Unreachable> {
    let Some(body) = get_text(client, url).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| lyrics_from_resource(&json)))
}

/// Search LRCLIB by track and artist: the best candidate's lyrics.
#[cfg(feature = "live")]
async fn lrclib_from_search(
    client: &tilo_net::Client,
    meta: &TrackMeta,
) -> Result<Option<Lyrics>, Unreachable> {
    let url = lrclib_search_url(meta);
    let Some(body) = get_text(client, &url).await? else {
        return Ok(None);
    };
    Ok(best_candidate(&body))
}

/// The first candidate with synced lyrics, else the first with plain ones.
#[cfg(feature = "live")]
fn best_candidate(body: &str) -> Option<Lyrics> {
    let arr: serde_json::Value = serde_json::from_str(body).ok()?;
    let list = arr.as_array()?;
    for rec in list {
        if rec
            .get("syncedLyrics")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
        {
            return lyrics_from_resource(rec);
        }
    }
    for rec in list {
        if let Some(l) = lyrics_from_resource(rec) {
            return Some(l);
        }
    }
    None
}

#[cfg(feature = "live")]
fn lyrics_from_resource(json: &serde_json::Value) -> Option<Lyrics> {
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

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Bracketed segments naming a video, remaster or quality note rather than the song;
/// `(Acoustic)` stays.
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

/// Drop a trailing `feat.` credit: LRCLIB keys on the base song name.
fn strip_feat(s: &str) -> String {
    let low = s.to_ascii_lowercase();
    for marker in [" feat.", " feat ", " ft.", " ft ", " featuring "] {
        if let Some(idx) = low.find(marker) {
            return s.get(..idx).unwrap_or(s).to_string();
        }
    }
    s.to_string()
}

/// A `YouTube` title for an LRCLIB lookup: tag segments and `feat.` credits dropped and
/// whitespace collapsed; a clean title is returned unchanged.
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
            out.push(ch);
            out.push_str(&seg);
            if closed {
                out.push(close);
            }
        }
    }
    collapse_ws(&strip_feat(&out))
}

/// A `YouTube` artist name without its auto-generated `- Topic` suffix.
pub fn normalize_artist(raw: &str) -> String {
    let mut s = collapse_ws(raw);
    if s.to_ascii_lowercase().ends_with(" - topic") {
        s.truncate(s.len().saturating_sub(" - Topic".len()));
        s = collapse_ws(&s);
    }
    s
}

/// The LRCLIB `/api/search` URL for a track.
#[cfg(feature = "live")]
fn lrclib_search_url(meta: &TrackMeta) -> String {
    format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        ui::uri::encode_component(&normalize_title(&meta.title)),
        ui::uri::encode_component(&normalize_artist(&meta.artist)),
    )
}

/// The LRCLIB `/api/get` URL for a track, compiled in every build so the contract stays
/// testable offline.
pub fn lrclib_url(meta: &TrackMeta) -> String {
    format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
        ui::uri::encode_component(&normalize_title(&meta.title)),
        ui::uri::encode_component(&normalize_artist(&meta.artist)),
        ui::uri::encode_component(&normalize_title(&meta.album)),
        meta.duration_s
    )
}

/// Lyrics memoised by normalised title and artist, misses included, so a track without
/// lyrics is not asked again.
#[cfg(feature = "live")]
fn cache_key(meta: &TrackMeta) -> String {
    format!(
        "{}\u{1}{}",
        normalize_title(&meta.title).to_ascii_lowercase(),
        normalize_artist(&meta.artist).to_ascii_lowercase(),
    )
}

#[cfg(feature = "live")]
fn lyrics_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Option<Lyrics>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<Lyrics>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(feature = "live")]
enum CacheEntry {
    Absent,
    Miss,
    Hit(Lyrics),
}

#[cfg(feature = "live")]
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

#[cfg(feature = "live")]
fn cache_put(key: String, value: Option<Lyrics>) {
    if let Ok(mut map) = lyrics_cache().lock() {
        map.insert(key, value);
    }
}

/// Resolves each request on the app runtime and delivers over a channel.
pub struct LyricsWorker {
    tx: std::sync::mpsc::Sender<(String, Lyrics)>,
    rx: std::sync::mpsc::Receiver<(String, Lyrics)>,
}

impl LyricsWorker {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(String, Lyrics)>();
        Self { tx, rx }
    }

    /// Request lyrics for a track; the result arrives through [`Self::poll`].
    pub fn request(&self, meta: TrackMeta) {
        let tx = self.tx.clone();
        // Always deliver a result, an empty one on a miss, so the view never waits forever.
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

    pub fn poll(&self) -> Option<(String, Lyrics)> {
        self.rx.try_recv().ok()
    }
}

impl Default for LyricsWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Sample lyrics for the seed tracks, synced where a capture shows it better.
pub mod bundled {
    use super::{Lyrics, parse_lrc, plain};

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
[ti:Glasslight]
[ar:Aurora Skies]
[00:08.50] Held to the window, a paper sun
[00:14.20] Counting the colours one by one
[00:21.00] Slow as the morning, soft as the rain
[00:28.40] Glasslight is breaking over the lane
[00:36.10] Carry me, carry me, easy and bright
[00:43.80] Into the hush of the glasslight
[00:52.00] Shadows fold quiet behind the API
[01:00.20] Nothing to keep and nothing to store";

    const NEON_MILEAGE_LRC: &str = "\
[ti:Neon Mileage]
[ar:Midnight Transit]
[00:06.00] Tail lights bleeding down the interstate
[00:12.50] Radio humming, it's getting late
[00:19.00] Neon mileage on an empty tank
[00:25.50] Chasing the dark to the riverbank
[00:33.00] Drive, just drive, till the morning comes
[00:40.00] Drive, just drive, to the wordless hum";

    const HARBOR_LIGHTS_LRC: &str = "\
[ti:Harbor Lights]
[ar:Neon Harbor]
[00:07.00] Ferries are humming across the bay
[00:13.40] Harbor lights wash the dusk away
[00:20.10] Cassette-warm summer on a salt-air breeze
[00:27.00] Boulevard glowing through the harbor trees";

    const PAPER_BOATS_LRC: &str = "\
[ti:Paper Boats]
[ar:Aurora Skies]
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
