//! Live backend — real YouTube data via **yt-dlp**, the community-maintained
//! extractor. **Compiled only under `--features live`.**
//!
//! Instead of re-implementing YouTube's InnerTube protocol in-process (the
//! `rustypipe` path, which is fragile against YouTube changes and needs
//! PoToken/BotGuard for streams), this backend shells out to the `yt-dlp`
//! binary — `yt-dlp -J <url>` / `--dump-single-json` — and maps its JSON onto
//! Tube's plain [`crate::model`] structs. yt-dlp tracks YouTube's changes for
//! us; when it updates, extraction keeps working with no code change here.
//!
//! ## What each method runs
//! | Method                    | yt-dlp invocation                              |
//! |---------------------------|------------------------------------------------|
//! | [`search`]                | `--flat-playlist -J "ytsearch<N>:<query>"`     |
//! | [`popular_search`]        | `--flat-playlist -J "ytsearch<N>:<query>"`     |
//! | [`video`]                 | `-J <watch_url>`                               |
//! | [`channel`]               | `--flat-playlist -J "<channel>/videos"`        |
//! | [`related`]               | (empty — handled by the local heuristic)       |
//! | [`trending`]              | `--flat-playlist -J ".../feed/trending"`       |
//! | [`resolve_playback_source`] | `-J <watch_url>` → best playable format url   |
//!
//! [`search`]: crate::backend::YtBackend::search
//! [`popular_search`]: crate::backend::YtBackend::popular_search
//! [`video`]: crate::backend::YtBackend::video
//! [`channel`]: crate::backend::YtBackend::channel
//! [`related`]: crate::backend::YtBackend::related
//! [`trending`]: crate::backend::YtBackend::trending
//! [`resolve_playback_source`]: crate::backend::YtBackend::resolve_playback_source
//!
//! It is a subprocess, not a crate, so it adds **no** compile-time dependency:
//! the only requirement is that `yt-dlp` is on `PATH` (it is, in the distrobox
//! build env and on the Tilo image — see the module notes). Construction probes
//! `yt-dlp --version` so a missing binary degrades cleanly to the offline
//! sample rather than erroring at query time.

#![cfg(feature = "live")]

use std::time::Duration;

use serde_json::Value;

use crate::backend::{
    BackendError, BackendOutcome, BackendResult, PlaybackSource, YtBackend, yt_thumb_url,
};
use crate::model::{Channel, Video};

/// How many results to pull for search / channel / trending listings.
const LIST_LIMIT: usize = 30;
const YTDLP_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// The yt-dlp-backed data source.
pub struct YtDlpBackend {
    /// The resolved `yt-dlp` executable name/path.
    bin: String,
    /// The version string reported at construction, for the status log.
    version: String,
}

impl YtDlpBackend {
    /// Construct, probing that `yt-dlp` is actually runnable. Returns an error
    /// (so the worker falls back to another source) when the binary is absent.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `yt-dlp --version` cannot be run or fails.
    pub fn new() -> Result<Self, BackendError> {
        let bin = "yt-dlp".to_string();
        let out = bounded_command::run(
            &bin,
            &["--version"],
            None,
            64 * 1024,
            Duration::from_secs(3),
        )
        .map_err(|e| BackendError(format!("yt-dlp not runnable: {e}")))?;
        if !out.status.success() {
            return Err(BackendError("yt-dlp --version failed".into()));
        }
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(Self { bin, version })
    }

    /// Run yt-dlp with `args`, returning its stdout as text. `timeout` bounds
    /// the call so a hung extraction can't wedge the worker.
    fn run_text(&self, args: &[&str], timeout: Duration) -> Result<String, BackendError> {
        let out = bounded_command::run(&self.bin, args, None, YTDLP_OUTPUT_LIMIT, timeout)
            .map_err(|e| BackendError(format!("yt-dlp failed safely: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err.lines().last().unwrap_or("").trim();
            return Err(BackendError(format!("yt-dlp failed: {last}")));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Run yt-dlp with `args` and parse its stdout as a single JSON value
    /// (used for the `-J` listing calls).
    fn run_json(&self, args: &[&str], timeout: Duration) -> Result<Value, BackendError> {
        let text = self.run_text(args, timeout)?;
        serde_json::from_str(text.trim())
            .map_err(|e| BackendError(format!("yt-dlp JSON parse: {e}")))
    }

    /// Run yt-dlp `--print` for a set of fields on `target`, optionally after a
    /// `-f <selector>`. Each field is rendered as a JSON token (`%(field)j`) and
    /// tab-separated, so the result is an object keyed by field name — parsed
    /// robustly regardless of embedded newlines/tabs in strings.
    ///
    /// This avoids the whole-`-J` extraction, which deciphers `nsig` for *every*
    /// format (tens of seconds with yt-dlp's built-in JS interpreter); `--print`
    /// with a format selector deciphers only the one chosen format (~2s).
    fn run_print(
        &self,
        selector: Option<&str>,
        fields: &[&str],
        target: &str,
        timeout: Duration,
    ) -> Result<Value, BackendError> {
        let template = fields
            .iter()
            .map(|f| format!("%({f})j"))
            .collect::<Vec<_>>()
            .join("\t");
        let mut args: Vec<&str> = vec!["--no-warnings", "--no-playlist"];
        if let Some(sel) = selector {
            args.push("-f");
            args.push(sel);
        }
        args.push("--print");
        args.push(&template);
        args.push(target);
        let text = self.run_text(&args, timeout)?;
        let line = text.lines().next().unwrap_or("");
        let tokens: Vec<&str> = line.split('\t').collect();
        let mut obj = serde_json::Map::new();
        for (i, f) in fields.iter().enumerate() {
            let val = tokens
                .get(i)
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .unwrap_or(Value::Null);
            obj.insert((*f).to_string(), val);
        }
        Ok(Value::Object(obj))
    }

    /// The channel-listing URL for an id/handle/URL. yt-dlp accepts a channel
    /// id (`UC…`), a `@handle`, or a full URL; normalise them all to the
    /// `/videos` tab so the JSON carries uploads + avatar + banner.
    fn channel_videos_url(id: &str) -> String {
        let base = if id.starts_with("http") {
            id.trim_end_matches('/').to_string()
        } else if id.starts_with("UC") {
            format!("https://www.youtube.com/channel/{id}")
        } else if let Some(handle) = id.strip_prefix('@') {
            format!("https://www.youtube.com/@{handle}")
        } else {
            format!("https://www.youtube.com/@{id}")
        };
        if base.ends_with("/videos") {
            base
        } else {
            format!("{base}/videos")
        }
    }
}

/// Map one yt-dlp entry/video JSON object onto Tube's [`Video`]. Works for both
/// a flat-playlist search/channel entry and a full `-J` video object; missing
/// fields fall back to sensible defaults.
fn video_from_json(v: &Value, category: &str) -> Video {
    let id = str_field(v, "id");
    let channel_id = str_field_any(v, &["channel_id", "uploader_id"]);
    let channel_name = str_field_any(v, &["channel", "uploader", "uploader_id"]);
    Video {
        id: id.clone(),
        title: str_field_any(v, &["title", "fulltitle"]),
        dearrow_title: None,
        channel_id: channel_id.clone(),
        channel_name: if channel_name.is_empty() {
            "Unknown".into()
        } else {
            channel_name
        },
        channel_seed: if channel_id.is_empty() {
            id.clone()
        } else {
            channel_id
        },
        // yt-dlp list items don't reliably carry the uploader avatar; leave it
        // to the channel-page fetch and show the initials chip meanwhile.
        channel_avatar_url: String::new(),
        views: u64_field(v, "view_count"),
        published: humanize_date(&str_field(v, "upload_date")),
        duration: f64_field(v, "duration") as u32,
        description: str_field(v, "description"),
        likes: u64_field(v, "like_count"),
        thumbnail_url: best_thumbnail(v, &id),
        seed: id,
        category: category.to_string(),
        dearrow_thumb: false,
        sponsor: Vec::new(),
        comments: Vec::new(),
        live: v
            .get("is_live")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

/// Map a channel `-J` object (its `/videos` tab) onto Tube's [`Channel`] plus
/// the avatar + banner URLs lifted from its `thumbnails`.
fn channel_from_json(v: &Value) -> Channel {
    let id = str_field_any(v, &["channel_id", "id"]);
    let (avatar_url, banner_url) = pick_channel_images(v);
    Channel {
        id: id.clone(),
        name: str_field_any(v, &["channel", "uploader", "title"]),
        handle: {
            let h = str_field_any(v, &["uploader_id", "channel_id"]);
            if h.is_empty() { id.clone() } else { h }
        },
        subscribers: u64_field(v, "channel_follower_count"),
        verified: v
            .get("channel_is_verified")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        description: str_field(v, "description"),
        seed: id,
        avatar_url,
        banner_url,
    }
}

/// Pick `(avatar_url, banner_url)` out of a channel object's `thumbnails`.
/// yt-dlp tags the banner with `id == "banner_uncropped"` and the avatar with
/// `id == "avatar_uncropped"`; fall back to the widest image for the banner and
/// the largest square-ish image for the avatar.
fn pick_channel_images(v: &Value) -> (String, String) {
    let Some(thumbs) = v.get("thumbnails").and_then(|t| t.as_array()) else {
        return (String::new(), String::new());
    };
    let url_of = |t: &Value| str_field(t, "url");
    let dims = |t: &Value| {
        (
            t.get("width")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            t.get("height")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )
    };

    // Explicit tags first.
    let mut avatar = thumbs
        .iter()
        .find(|t| str_field(t, "id") == "avatar_uncropped")
        .map(url_of)
        .unwrap_or_default();
    let mut banner = thumbs
        .iter()
        .find(|t| str_field(t, "id") == "banner_uncropped")
        .map(url_of)
        .unwrap_or_default();

    // Heuristic fallbacks from the dimensioned entries.
    if banner.is_empty() {
        banner = thumbs
            .iter()
            .filter(|t| {
                let (w, h) = dims(t);
                h > 0 && w as f64 / h as f64 >= 3.0
            })
            .max_by_key(|t| dims(t).0)
            .map(url_of)
            .unwrap_or_default();
    }
    if avatar.is_empty() {
        avatar = thumbs
            .iter()
            .filter(|t| {
                let (w, h) = dims(t);
                w > 0 && h > 0 && (w as i64 - h as i64).abs() <= (w as i64 / 4).max(1)
            })
            .max_by_key(|t| dims(t).0)
            .map(url_of)
            .unwrap_or_default();
    }
    (avatar, banner)
}

/// The best video thumbnail: the largest `thumbnails` entry, else the canonical
/// `hqdefault.jpg` derived from the id.
fn best_thumbnail(v: &Value, id: &str) -> String {
    let best = v
        .get("thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter(|t| !str_field(t, "url").is_empty())
                .max_by_key(|t| {
                    let w = t
                        .get("width")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let h = t
                        .get("height")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    // Prefer a real preference index when present, else area.
                    w * h
                        + t.get("preference")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0)
                })
                .map(|t| str_field(t, "url"))
        })
        .unwrap_or_default();
    if best.is_empty() {
        yt_thumb_url(id)
    } else {
        best
    }
}

/// Build a [`PlaybackSource`] from a `--print` field object (the production
/// parse path in [`YtDlpBackend::resolve_playback_source`]). `None` when no URL
/// was produced.
fn playback_source_from_print(v: &Value) -> Option<PlaybackSource> {
    let url = str_field(v, "url");
    if url.is_empty() {
        return None;
    }
    let acodec = str_field(v, "acodec");
    let quality = {
        let note = str_field(v, "format_note");
        if note.is_empty() {
            str_field(v, "resolution")
        } else {
            note
        }
    };
    Some(PlaybackSource {
        url,
        format: format!("{}; {}", str_field(v, "ext"), str_field(v, "vcodec")),
        has_audio: !acodec.is_empty() && acodec != "none",
        width: u64_field(v, "width") as u32,
        height: u64_field(v, "height") as u32,
        fps: f64_field(v, "fps").round().max(1.0) as u8,
        quality,
    })
}

// --- small JSON accessors ---------------------------------------------------

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

fn str_field_any(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        let s = str_field(v, k);
        if !s.is_empty() {
            return s;
        }
    }
    String::new()
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0)
}

fn f64_field(v: &Value, key: &str) -> f64 {
    v.get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

/// Today, as days since the civil epoch (UTC).
fn today_days() -> i64 {
    (ui::time::unix_now() / 86_400) as i64
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Turn a yt-dlp `upload_date` (`YYYYMMDD`) into a human relative age
/// ("3 days ago", "2 weeks ago", "5 months ago") for anything within the last
/// year, and a tidy absolute date ("Nov 3, 2023") beyond that. Anything that
/// isn't an 8-digit date (other backends already hand back "3 days ago" text)
/// passes through unchanged; empty stays empty.
fn humanize_date(d: &str) -> String {
    if d.len() != 8 || !d.chars().all(|c| c.is_ascii_digit()) {
        return d.to_string();
    }
    let (y, m, day) = (
        d.get(0..4).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0),
        d.get(4..6).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
        d.get(6..8).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return d.to_string();
    }
    let month = MONTHS.get((m - 1) as usize).copied().unwrap_or("");
    let absolute = format!("{month} {day}, {y}");
    let age = today_days() - ui::date::days_from_civil(y, m, day);
    age_label(age, &absolute)
}

/// Pure age-in-days → human label. Split out so the relative wording is unit-
/// tested without depending on the system clock. Upload dates carry no time of
/// day, so the same day reads "today" and anything past a year is absolute.
fn age_label(age: i64, absolute: &str) -> String {
    match age {
        0 => "today".to_string(),
        1..=364 => ui::format::relative(age.saturating_mul(86_400)),
        // Clock skew / future date, or older than a year.
        _ => absolute.to_string(),
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn upload_dates_read_as_relative_or_absolute() {
        // Case `relative_ages_read_naturally`.
        {
            assert_eq!(age_label(0, "x"), "today");
            assert_eq!(age_label(1, "x"), "yesterday");
            assert_eq!(age_label(3, "x"), "3 days ago");
            assert_eq!(age_label(10, "x"), "1 week ago");
            assert_eq!(age_label(21, "x"), "3 weeks ago");
            assert_eq!(age_label(30, "x"), "1 month ago");
            assert_eq!(age_label(75, "x"), "2 months ago");
            assert_eq!(age_label(800, "Nov 3, 2023"), "Nov 3, 2023");
            assert_eq!(age_label(-5, "Nov 3, 2023"), "Nov 3, 2023");
        }
        // Case `full_yyyymmdd_absolute_beyond_a_year`.
        {
            // A date well over a year old renders as a tidy absolute date, never a
            // raw ISO string.
            assert_eq!(humanize_date("20200704"), "Jul 4, 2020");
        }
        // Case `non_date_passes_through`.
        {
            assert_eq!(humanize_date("3 days ago"), "3 days ago");
            assert_eq!(humanize_date(""), "");
        }
    }
}

/// Pull [`Video`] rows out of a listing object's `entries`.
fn videos_from_entries(v: &Value, category: &str) -> Vec<Video> {
    v.get("entries")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| !e.is_null() && !str_field(e, "id").is_empty())
                .map(|e| video_from_json(e, category))
                .collect()
        })
        .unwrap_or_default()
}

impl YtBackend for YtDlpBackend {
    fn name(&self) -> &str {
        // Leaked once; the version string lives for the process lifetime.
        Box::leak(format!("yt-dlp {}", self.version).into_boxed_str())
    }

    fn search(&self, query: &str) -> BackendOutcome {
        let spec = format!("ytsearch{LIST_LIMIT}:{query}");
        let v = self.run_json(
            &["-J", "--flat-playlist", "--no-warnings", &spec],
            Duration::from_secs(30),
        )?;
        Ok(BackendResult::Videos(videos_from_entries(&v, "Search")))
    }

    fn video(&self, id: &str) -> BackendOutcome {
        let url = format!("https://www.youtube.com/watch?v={id}");
        // Metadata only (no format URLs), so no per-format `nsig` deciphering —
        // fast (~2-3s) even without a JS runtime.
        let v = self.run_print(
            None,
            &[
                "id",
                "title",
                "channel",
                "channel_id",
                "uploader_id",
                "view_count",
                "like_count",
                "duration",
                "upload_date",
                "description",
                "is_live",
            ],
            &url,
            Duration::from_secs(45),
        )?;
        Ok(BackendResult::Video(Box::new(video_from_json(&v, "Video"))))
    }

    fn channel(&self, id: &str) -> BackendOutcome {
        let url = Self::channel_videos_url(id);
        let end = format!("{LIST_LIMIT}");
        let v = self.run_json(
            &[
                "-J",
                "--flat-playlist",
                "--no-warnings",
                "--playlist-end",
                &end,
                &url,
            ],
            Duration::from_secs(90),
        )?;
        let channel = channel_from_json(&v);
        // Flat-playlist entries often omit `channel_id`/`channel`, but every
        // upload on a channel's /videos tab belongs to that channel — stamp its
        // id + name so `TubeModel::channel_videos` matches them and the uploader
        // renders correctly.
        let mut uploads = videos_from_entries(&v, "Channel");
        for u in &mut uploads {
            if !channel.id.is_empty() {
                channel.id.clone_into(&mut u.channel_id);
            }
            if u.channel_name.is_empty() || u.channel_name == "Unknown" {
                channel.name.clone_into(&mut u.channel_name);
            }
            channel.id.clone_into(&mut u.channel_seed);
        }
        Ok(BackendResult::Channel {
            channel: Box::new(channel),
            uploads,
        })
    }

    fn related(&self, _id: &str) -> BackendOutcome {
        // yt-dlp has no cheap "related" endpoint; leave it to the model's local
        // same-category heuristic (the worker only adopts non-empty related).
        Ok(BackendResult::Videos(Vec::new()))
    }

    fn playlist(
        &self,
        id: &str,
    ) -> Result<(crate::model::Playlist, Vec<crate::model::Video>), BackendError> {
        // yt-dlp extracts playlists reliably (unlike the current rustypipe
        // playlist parser). Flat-list the entries so it stays fast.
        let url = if id.starts_with("http") {
            id.to_string()
        } else {
            format!("https://www.youtube.com/playlist?list={id}")
        };
        let end = format!("{LIST_LIMIT}");
        let v = self.run_json(
            &[
                "-J",
                "--flat-playlist",
                "--no-warnings",
                "--playlist-end",
                &end,
                &url,
            ],
            Duration::from_secs(60),
        )?;
        let videos = videos_from_entries(&v, "Playlist");
        let pl_id = {
            let got = str_field_any(&v, &["id"]);
            if got.is_empty() { id.to_string() } else { got }
        };
        let thumbnail_url = videos
            .first()
            .map(|x| x.thumbnail_url.clone())
            .unwrap_or_default();
        let video_count = v
            .get("playlist_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(videos.len() as u64);
        let playlist = crate::model::Playlist {
            id: pl_id.clone(),
            title: str_field_any(&v, &["title", "playlist_title"]),
            channel_id: str_field_any(&v, &["channel_id", "uploader_id"]),
            channel_name: str_field_any(&v, &["channel", "uploader"]),
            thumbnail_url,
            seed: pl_id,
            video_count,
            video_ids: videos.iter().map(|x| x.id.clone()).collect(),
        };
        Ok((playlist, videos))
    }

    fn trending(&self) -> BackendOutcome {
        let v = self.run_json(
            &[
                "-J",
                "--flat-playlist",
                "--no-warnings",
                "--playlist-end",
                "30",
                "https://www.youtube.com/feed/trending",
            ],
            Duration::from_secs(30),
        )?;
        Ok(BackendResult::Videos(videos_from_entries(&v, "Trending")))
    }

    fn resolve_playback_source(&self, id: &str) -> Result<PlaybackSource, BackendError> {
        // Let yt-dlp's format selector pick a single playable stream — a
        // progressive muxed https format (audio+video in one URL) when available,
        // else the best mp4/whatever — so only that one format's `nsig` is
        // deciphered (~2s). GStreamer negotiates the codec regardless.
        const SELECTOR: &str = "best[protocol^=https][vcodec!=none][acodec!=none]/best[ext=mp4][protocol^=https]/best[protocol^=https]/best";
        let url = format!("https://www.youtube.com/watch?v={id}");
        let v = self.run_print(
            Some(SELECTOR),
            &[
                "url",
                "width",
                "height",
                "fps",
                "format_note",
                "resolution",
                "ext",
                "vcodec",
                "acodec",
            ],
            &url,
            Duration::from_secs(45),
        )?;
        playback_source_from_print(&v).ok_or_else(|| {
            BackendError(format!(
                "no directly-playable https format for {id} (DRM-only or extraction changed)"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_entries_map_to_videos_and_channel_images() {
        // Case `maps_search_entry_to_video_with_thumbnail`:
        // A canned flat-playlist search entry (the shape yt-dlp emits) must map
        // onto a `Video` with the real title, channel and a thumbnail URL. This is
        // hermetic — no yt-dlp invocation, just the JSON→model mapping the live
        // path relies on.
        {
            let entry = serde_json::json!({
                "id": "WhWc3b3KhnY",
                "title": "Spring - Blender Open Movie",
                "channel": "Blender Studio",
                "channel_id": "UCz75RVbH8q2jdBJ4SnwuZZQ",
                "duration": 465,
                "view_count": 10_815_517u64,
                "thumbnails": [
                    {"url": "https://i.ytimg.com/vi/WhWc3b3KhnY/hq720.jpg", "width": 720, "height": 404},
                    {"url": "https://i.ytimg.com/vi/WhWc3b3KhnY/small.jpg", "width": 360, "height": 202}
                ]
            });
            let v = video_from_json(&entry, "Search");
            assert_eq!(v.id, "WhWc3b3KhnY");
            assert_eq!(v.title, "Spring - Blender Open Movie");
            assert_eq!(v.channel_name, "Blender Studio");
            assert_eq!(v.channel_id, "UCz75RVbH8q2jdBJ4SnwuZZQ");
            assert_eq!(v.duration, 465);
            assert_eq!(v.views, 10_815_517);
            // Largest thumbnail is chosen.
            assert_eq!(
                v.thumbnail_url,
                "https://i.ytimg.com/vi/WhWc3b3KhnY/hq720.jpg"
            );
        }
        // Case `falls_back_to_hqdefault_thumbnail`:
        // A video with no thumbnails still gets a usable image: the canonical
        // `hqdefault.jpg` derived from the id.
        {
            let entry = serde_json::json!({ "id": "abc123", "title": "T" });
            let v = video_from_json(&entry, "Search");
            assert_eq!(
                v.thumbnail_url,
                "https://i.ytimg.com/vi/abc123/hqdefault.jpg"
            );
        }
        // Case `extracts_channel_avatar_and_banner`:
        // The channel object's avatar + banner are lifted from `thumbnails` by
        // their yt-dlp `id` tags (`avatar_uncropped` / `banner_uncropped`).
        {
            let ch = serde_json::json!({
                "channel_id": "UCz75RVbH8q2jdBJ4SnwuZZQ",
                "channel": "Blender Studio",
                "uploader_id": "@BlenderStudio",
                "channel_follower_count": 560_000u64,
                "channel_is_verified": true,
                "thumbnails": [
                    {"id": "banner_uncropped", "url": "https://yt3.googleusercontent.com/banner"},
                    {"id": "avatar_uncropped", "url": "https://yt3.googleusercontent.com/avatar"},
                    {"id": "7", "url": "https://yt3.googleusercontent.com/sq", "width": 900, "height": 900}
                ],
                "entries": [
                    {"id": "_KjTlL5HBdc", "title": "Log 30", "channel": "Blender Studio"}
                ]
            });
            let c = channel_from_json(&ch);
            assert_eq!(c.name, "Blender Studio");
            assert_eq!(c.handle, "@BlenderStudio");
            assert_eq!(c.subscribers, 560_000);
            assert!(c.verified);
            assert_eq!(c.avatar_url, "https://yt3.googleusercontent.com/avatar");
            assert_eq!(c.banner_url, "https://yt3.googleusercontent.com/banner");

            let uploads = videos_from_entries(&ch, "Channel");
            assert_eq!(uploads.len(), 1);
            assert_eq!(uploads[0].id, "_KjTlL5HBdc");
        }
        // Case `channel_image_heuristic_fallbacks`:
        // The banner falls back to the widest image and the avatar to the largest
        // square when the explicit `*_uncropped` tags are absent.
        {
            let ch = serde_json::json!({
                "channel_id": "UCx",
                "thumbnails": [
                    {"id": "0", "url": "https://x/banner", "width": 2560, "height": 424},
                    {"id": "1", "url": "https://x/avatar", "width": 800, "height": 800}
                ]
            });
            let (avatar, banner) = pick_channel_images(&ch);
            assert_eq!(banner, "https://x/banner", "widest → banner");
            assert_eq!(avatar, "https://x/avatar", "square → avatar");
        }
        // Case `channel_url_normalises_id_handle_and_url`.
        {
            assert_eq!(
                YtDlpBackend::channel_videos_url("UCabc"),
                "https://www.youtube.com/channel/UCabc/videos"
            );
            assert_eq!(
                YtDlpBackend::channel_videos_url("@Blender"),
                "https://www.youtube.com/@Blender/videos"
            );
            assert_eq!(
                YtDlpBackend::channel_videos_url("Blender"),
                "https://www.youtube.com/@Blender/videos"
            );
            assert_eq!(
                YtDlpBackend::channel_videos_url("https://www.youtube.com/@Blender"),
                "https://www.youtube.com/@Blender/videos"
            );
        }
    }

    #[test]
    fn playback_sources_parse_muxed_video_only_or_none() {
        // Case `parses_muxed_playback_source_with_audio`:
        // The `--print` field object (what yt-dlp's format selector produces) is
        // parsed into a `PlaybackSource`: a muxed format reports audio.
        {
            let v = serde_json::json!({
                "url": "https://gv/muxed",
                "ext": "mp4", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2",
                "width": 640, "height": 360, "fps": 30, "format_note": "360p"
            });
            let src = playback_source_from_print(&v).expect("a playable source");
            assert_eq!(src.url, "https://gv/muxed");
            assert!(src.has_audio, "muxed format carries audio");
            assert_eq!(src.height, 360);
            assert_eq!(src.width, 640);
            assert_eq!(src.fps, 30);
            assert_eq!(src.quality, "360p");
        }
        // Case `parses_video_only_source_without_audio`:
        // A video-only selection (no audio codec) reports `has_audio == false`, and
        // the quality falls back to `resolution` when `format_note` is absent.
        {
            let v = serde_json::json!({
                "url": "https://gv/vp9-1080",
                "ext": "webm", "vcodec": "vp9", "acodec": "none",
                "width": 1920, "height": 1080, "fps": 60, "resolution": "1920x1080"
            });
            let src = playback_source_from_print(&v).expect("a playable source");
            assert_eq!(src.url, "https://gv/vp9-1080");
            assert!(!src.has_audio);
            assert_eq!(src.quality, "1920x1080");
        }
        // Case `no_url_yields_no_playback_source`:
        // No URL → no source (the caller degrades to the poster).
        {
            let v = serde_json::json!({ "ext": "mp4", "vcodec": "avc1" });
            assert!(playback_source_from_print(&v).is_none());
        }
    }
}
