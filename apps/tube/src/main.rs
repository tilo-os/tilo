//! Tube — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`. The root
//!                       app is `online()`: it spawns the fetch worker and pulls
//!                       real YouTube data (home/trending/search/…) via the
//!                       `live` backend, degrading to the bundled sample when
//!                       the network is unreachable.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).
//!                       Always offline/hermetic — no worker, no network.
//!   * `--probe [Q]`  — construct the real backend, run a live YouTube search
//!                       for `Q` (default "rust programming") + trending, print
//!                       what came back, and exit. Proof that live fetch works;
//!                       requires `--features live` to do anything real.
//!   * `--probe-trending`
//!                     — prove the Home/Trending feed: show YouTube's retired
//!                       dedicated endpoint failing for real, then the composed
//!                       real "popular" feed (view-count-sorted search).
//!   * `--probe-play [ID]`
//!                     — prove real playback: resolve a googlevideo H.264 stream,
//!                       download + demux it to Annex-B, and decode one real frame
//!                       through the openh264 decoder. Prints itag/mime/size and
//!                       the decoded frame dimensions.
//!   * `--probe-stream [ID] [SECONDS]`
//!                     — prove CONTINUOUS (moving) playback: resolve the stream,
//!                       page through its media fragments, and decode a *sequence*
//!                       of real frames over `SECONDS` through one reused decoder.
//!                       Prints per-frame index/time/checksum + achieved fps.
//!
//! Shots-only flags:
//!   * `--viewport WxH` — render at WxH logical px instead of the default
//!                         1280x800. A sub-`NAV_BREAKPOINT` width exercises the
//!                         responsive overlay-flap layout.
//!   * `--narrow`       — build the narrow variant (rail collapsed to an open
//!                         overlay flap), so the still captures the summoned
//!                         rail + scrim. Use together with a narrow `--viewport`.

use std::collections::HashMap;

use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // `--probe-trending` — prove the real Home/Trending feed: show the dedicated
    // endpoint's real error and the composed real popular feed it falls back to.
    if args.iter().any(|a| a == "--probe-trending") {
        return run_probe_trending();
    }

    // `--probe-engine [id]` — prove REAL playback through the shared media engine
    // (the exact production path): resolve a stream, open a `media::MediaEngine`,
    // and decode a sequence of frames, printing engine events + motion evidence.
    if let Some(id) = parse_flag_id(&args, "--probe-engine", "aqz-KE-bpKQ") {
        return run_probe_engine(&id);
    }

    // `--live-shots <dir> [query]` — render REAL offscreen stills: a search page
    // with real YouTube results + fetched thumbnails, and a watch page with a
    // real frame decoded through the media engine from a rangeable proxy stream.
    // Everything is live data + real decode; requires `--features "live media"`.
    if let Some((dir, query)) = parse_live_shots_arg(&args) {
        return run_live_shots(&dir, &query);
    }

    // `--probe-ytdlp [query]` — prove the yt-dlp extractor: search + video +
    // channel (with avatar/banner urls). Requires `--features live`.
    if let Some(query) = parse_opt_query(&args, "--probe-ytdlp", "blender open movie") {
        return run_probe_ytdlp(&query);
    }

    // `--probe-avatars [query]` — diagnose the creator-avatar pipeline: what
    // channel id/name/avatar yt-dlp vs rustypipe report for the same search, the
    // by-id fold match rate, and whether a per-channel fetch actually returns +
    // decodes an avatar. Requires `--features live`.
    if let Some(query) = parse_opt_query(&args, "--probe-avatars", "blender open movie") {
        return run_probe_avatars(&query);
    }

    // `--probe-gst [id] [seconds]` — prove the GStreamer decode path: resolve a
    // real stream URL via yt-dlp and decode a sequence of real frames (checksums
    // prove motion). Requires `--features "live media"`.
    if let Some((id, seconds)) = parse_probe_gst_arg(&args) {
        return run_probe_gst(&id, seconds);
    }

    // `--probe-stream <id> [seconds]` — prove CONTINUOUS playback: resolve a
    // stream, page through its fragments, and decode a *sequence* of real frames
    // over time through one reused decoder, printing per-frame evidence.
    if let Some((id, seconds, fetch_delay_ms)) = parse_probe_stream_arg(&args) {
        return run_probe_stream(&id, seconds, fetch_delay_ms);
    }

    // `--probe-play <id>` — prove real playback: resolve a stream, download +
    // demux it, and decode at least one real H.264 frame.
    if let Some(id) = parse_probe_play_arg(&args) {
        return run_probe_play(&id);
    }

    // `--probe [query]` — prove the live backend actually fetches real data.
    if let Some(query) = parse_probe_arg(&args) {
        return run_probe(&query);
    }

    // `--bench-scroll [frames]` — headless scroll-performance benchmark. Times
    // the exact per-frame passes the live event loop runs (compute_layout /
    // apply_layout / draw) over a large Home grid, in two modes — the pre-fix
    // "always re-solve" baseline and the scroll-reposition fast path — and
    // prints the numbers. No window, no GPU present (matches the QUICK_UI_PERF
    // main-thread breakdown, which excludes the render worker).
    if let Some(frames) = parse_flag_usize(&args, "--bench-scroll", 90) {
        // `--watch` benches the watch page (full-bleed player + a long comment
        // list) instead of the Home grid, so the reported numbers cover the
        // screen the scroll-perf complaint was about.
        let watch = args.iter().any(|a| a == "--watch");
        // `--loaded` (with `--watch`) loads a real decoded frame into the player
        // and pauses it, reproducing the "paused-but-loaded watch page" the
        // scroll-perf complaint is about — and drives `sync` each frame, so the
        // per-frame engine cost is measured, not just layout + draw.
        let loaded = args.iter().any(|a| a == "--loaded");
        return run_bench_scroll(frames, watch, loaded);
    }

    if let Some(out) = parse_shots_arg(&args) {
        let viewport = parse_viewport_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let (vw, vh) = viewport;
        let narrow = args.iter().any(|a| a == "--narrow");
        let palette = args.iter().any(|a| a == "--palette");
        // `--play-demo`: open a watch page with the shared `VideoPlayer` already
        // playing (a synthesized frame + revealed controls), so the still
        // captures the real player surface + transport — a visual companion to
        // `--probe-gst` (which proves the *decode*) for eyeballing the on-screen
        // player without a network or a live decoder.
        let play_demo = args.iter().any(|a| a == "--play-demo");
        // `--empty`: the cold-start loading skeleton (no fabricated sample).
        let empty_loading = args.iter().any(|a| a == "--empty");
        // Seed the app with the render viewport so the single offscreen frame
        // resolves the responsive layout (rail default, grid columns, watch
        // player size) from the true width instead of the unknown-width
        // fallback. Shots-only — the live event loop below is untouched.
        let mut app = tube::TubeApp::new().with_viewport(vw as f64, vh as f64);
        if empty_loading {
            app = app.with_loading_home_shot();
        } else if palette {
            // Open the command palette pre-seeded so the still captures the
            // Ctrl+K overlay with live results.
            let q = parse_palette_query_arg(&args).unwrap_or_else(|| "rust".to_string());
            app = app.with_palette_open_for_test(&q);
        } else if play_demo {
            app = app.with_playing_demo_for_test();
        } else if narrow {
            app = app.with_open_sidebar_shot();
        } else if let Some(route) = parse_route_arg(&args) {
            app = app.with_route_shot(route);
        }
        render_shot_to_png(Box::new(app) as Box<dyn Widget>, viewport, &out)?;
        return Ok(());
    }

    let state = ui::State::new();
    // The live binary is `online()`: it reaches out to the real backend and
    // auto-falls-back to the sample if the network is down. (`app_root()`, used
    // by `--shots`, stays offline for hermetic screenshots.)
    let app = ui::App::new(state, Box::new(tube::TubeApp::new().online()));
    app.run();
    Ok(())
}

/// Parse `--probe` / `--probe=<query>` / `--probe <query>`. Returns the query
/// to probe with (default "rust programming") when the flag is present.
fn parse_probe_arg(args: &[String]) -> Option<String> {
    const DEFAULT_Q: &str = "rust programming";
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--probe" {
            return Some(match iter.next() {
                Some(q) if !q.starts_with("--") => q.clone(),
                _ => DEFAULT_Q.to_string(),
            });
        }
        if let Some(rest) = arg.strip_prefix("--probe=") {
            return Some(if rest.is_empty() { DEFAULT_Q.to_string() } else { rest.to_string() });
        }
    }
    None
}

/// Parse `--<flag> [id]` / `--<flag>=<id>` with a default id.
fn parse_flag_id(args: &[String], flag: &str, default: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return Some(match iter.next() {
                Some(id) if !id.starts_with("--") => id.clone(),
                _ => default.to_string(),
            });
        }
        if let Some(rest) = arg.strip_prefix(eq.as_str()) {
            return Some(if rest.is_empty() { default.to_string() } else { rest.to_string() });
        }
    }
    None
}

/// `--probe-engine [id]`: prove real playback through the shared `media::MediaEngine`
/// — the exact path the watch page streams through. Resolves a directly-playable
/// stream (direct extractor first, then the rangeable Invidious proxy), opens the
/// engine, plays, and decodes a sequence of frames; prints engine events + a
/// per-frame checksum proving motion. Exits non-zero if no frame decodes.
#[cfg(all(feature = "live", feature = "media"))]
fn run_probe_engine(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};
    use tube::backend::{proxy, ytdlp::YtDlpBackend, YtBackend};

    eprintln!("probe-engine: constructing the yt-dlp backend…");
    let backend = YtDlpBackend::new().map_err(|e| format!("yt-dlp backend: {e}"))?;
    let (url, quality) = backend
        .resolve_playback_source(id)
        .ok()
        .map(|s| (s.url, s.quality))
        .or_else(|| proxy::resolve_rangeable(id).map(|r| (r.url, r.quality)))
        .ok_or("no directly-playable stream resolved")?;
    println!("probe-engine: resolved {quality}; url(host)={}", short_url(&url));

    let mut engine = media::MediaEngine::new();
    println!(
        "probe-engine: engine real_decode={} (decode compiled={})",
        engine.is_real_decode(),
        media::MediaEngine::is_decode_compiled()
    );
    engine.open(media::Source::Remote { url });
    engine.play();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut sums: HashSet<u64> = HashSet::new();
    let mut distinct = 0u64;
    let mut errored = false;
    while Instant::now() < deadline {
        for ev in engine.poll() {
            match ev {
                media::MediaEvent::Error(e) => {
                    eprintln!("probe-engine: ENGINE ERROR: {e}");
                    errored = true;
                }
                media::MediaEvent::DurationKnown { ms } => {
                    println!("probe-engine: duration known = {ms} ms");
                }
                media::MediaEvent::Buffering { percent } => {
                    if percent % 25 == 0 {
                        println!("probe-engine: buffering {percent}%");
                    }
                }
                _ => {}
            }
        }
        if let Some(f) = engine.latest_frame() {
            let s = checksum(&f.rgba);
            if sums.insert(s) {
                distinct += 1;
                if distinct <= 3 {
                    println!(
                        "  frame {distinct}: {}x{} pts={}ms checksum=0x{:016x}",
                        f.width, f.height, f.pts_ms, s
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    println!(
        "probe-engine: {distinct} DISTINCT frames; final pos={}ms dur={:?}ms buffering={}%",
        engine.position_ms(),
        engine.duration_ms(),
        engine.buffering_percent()
    );
    if distinct == 0 {
        eprintln!(
            "probe-engine: the media engine produced NO frame (errored={errored}) — reported \
             honestly, not faked."
        );
        std::process::exit(3);
    }
    println!("probe-engine: REAL playback through the media engine — {distinct} distinct pictures.");
    Ok(())
}

#[cfg(not(all(feature = "live", feature = "media")))]
fn run_probe_engine(_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("probe-engine: needs `--features \"live media\"` (real resolve + media-engine decode).");
    Ok(())
}

/// Parse `--live-shots <dir> [query]`. Returns the output directory and the
/// search query (default "rust programming") to render real stills for.
fn parse_live_shots_arg(args: &[String]) -> Option<(String, String)> {
    const DEFAULT_Q: &str = "rust programming";
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let dir = if arg == "--live-shots" {
            iter.next()?.clone()
        } else if let Some(rest) = arg.strip_prefix("--live-shots=") {
            rest.to_string()
        } else {
            continue;
        };
        let query = match iter.next() {
            Some(q) if !q.starts_with("--") => q.clone(),
            _ => DEFAULT_Q.to_string(),
        };
        return Some((dir, query));
    }
    None
}

/// Parse `--probe-play <id>` / `--probe-play=<id>`. Returns the video id to
/// resolve + decode. Defaults to a stable, always-available clip.
fn parse_probe_play_arg(args: &[String]) -> Option<String> {
    const DEFAULT_ID: &str = "aqz-KE-bpKQ"; // "Big Buck Bunny" (Blender, CC-BY)
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--probe-play" {
            return Some(match iter.next() {
                Some(id) if !id.starts_with("--") => id.clone(),
                _ => DEFAULT_ID.to_string(),
            });
        }
        if let Some(rest) = arg.strip_prefix("--probe-play=") {
            return Some(if rest.is_empty() { DEFAULT_ID.to_string() } else { rest.to_string() });
        }
    }
    None
}


/// Parse `--flag [query]` / `--flag=<query>` with a default when the flag is
/// present but no value follows.
fn parse_opt_query(args: &[String], flag: &str, default: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return Some(match iter.next() {
                Some(q) if !q.starts_with("--") => q.clone(),
                _ => default.to_string(),
            });
        }
        if let Some(rest) = arg.strip_prefix(eq.as_str()) {
            return Some(if rest.is_empty() { default.to_string() } else { rest.to_string() });
        }
    }
    None
}

/// Parse `--probe-gst <id> [seconds]` → (id, seconds). Defaults to Big Buck
/// Bunny for 5 seconds.
fn parse_probe_gst_arg(args: &[String]) -> Option<(String, u64)> {
    const DEFAULT_ID: &str = "aqz-KE-bpKQ";
    const DEFAULT_SECS: u64 = 5;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let id = if arg == "--probe-gst" {
            match iter.next() {
                Some(id) if !id.starts_with("--") => id.clone(),
                _ => DEFAULT_ID.to_string(),
            }
        } else if let Some(rest) = arg.strip_prefix("--probe-gst=") {
            if rest.is_empty() { DEFAULT_ID.to_string() } else { rest.to_string() }
        } else {
            continue;
        };
        let seconds = iter
            .clone()
            .find_map(|s| s.parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_SECS);
        return Some((id, seconds));
    }
    None
}

/// Parse `--probe-stream <id> [seconds] [fetch_delay_ms]`. Returns the video id
/// to stream, how many seconds of wall-time to decode for (default 5), and an
/// optional delay between fragment fetches (default 0 = as fast as possible).
/// A non-zero delay approximates the live app's playback-rate fetch spacing,
/// which avoids googlevideo's burst-download 403. The id defaults to a stable,
/// always-available clip.
fn parse_probe_stream_arg(args: &[String]) -> Option<(String, u64, u64)> {
    const DEFAULT_ID: &str = "aqz-KE-bpKQ"; // "Big Buck Bunny" (Blender, CC-BY)
    const DEFAULT_SECS: u64 = 5;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let id = if arg == "--probe-stream" {
            match iter.next() {
                Some(id) if !id.starts_with("--") => id.clone(),
                _ => DEFAULT_ID.to_string(),
            }
        } else if let Some(rest) = arg.strip_prefix("--probe-stream=") {
            if rest.is_empty() { DEFAULT_ID.to_string() } else { rest.to_string() }
        } else {
            continue;
        };
        // Optional trailing positional numbers: seconds then fetch_delay_ms.
        let mut rest = iter.clone().filter_map(|s| s.parse::<u64>().ok());
        let seconds = rest.next().filter(|s| *s > 0).unwrap_or(DEFAULT_SECS);
        let fetch_delay_ms = rest.next().unwrap_or(0);
        return Some((id, seconds, fetch_delay_ms));
    }
    None
}

/// `--probe-stream <id> [seconds]`: prove CONTINUOUS playback end-to-end.
/// Resolves a real adaptive H.264 stream, downloads its init + segment index,
/// then pages through the media fragments decoding a *sequence* of frames with a
/// single reused decoder for `seconds` of wall time. Prints per-frame evidence
/// (index, timestamp, dimensions, a per-frame checksum proving the picture
/// actually changes) and the achieved decode fps. Exits non-zero if the stream
/// can't be resolved or the continuous index is unavailable — no faking.
#[cfg(feature = "live")]
fn run_probe_stream(
    id: &str,
    seconds: u64,
    fetch_delay_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    use tube::backend::{mp4, stream, YtBackend};
    use tube::playback::Playback;

    eprintln!("probe-stream: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;

    println!("probe-stream: resolving a playable stream for [{id}]…");
    let mut handle = match backend.resolve_video_stream(id) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("probe-stream: STREAM RESOLUTION FAILED: {e}");
            std::process::exit(2);
        }
    };
    println!(
        "  RESOLVED: itag={} {} {}x{}@{}fps adaptive={} content_length={:?}",
        handle.itag,
        handle.quality,
        handle.width,
        handle.height,
        handle.fps,
        handle.adaptive,
        handle.content_length
    );

    let client = stream::http_client();
    println!("probe-stream: downloading init segment + segment index (sidx)…");
    let plan = match stream::fetch_stream_plan(&client, &handle) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("probe-stream: CONTINUOUS INDEX UNAVAILABLE: {e}");
            std::process::exit(2);
        }
    };
    let total_dur: f64 = plan
        .durations
        .iter()
        .map(|d| *d as f64 / plan.timescale as f64)
        .sum();
    println!(
        "  PLANNED {} media fragments from {} bytes of init+sidx (\u{2248}{:.0}s of video @ {}fps)",
        plan.fragments.len(),
        plan.head_bytes,
        total_dur,
        handle.fps
    );

    if fetch_delay_ms > 0 {
        println!(
            "probe-stream: pacing fragment fetches {fetch_delay_ms}ms apart (approximating the \
             live app's playback-rate spacing, which avoids googlevideo's burst 403)…"
        );
    }
    println!(
        "probe-stream: decoding continuously for up to {seconds}s through ONE reused openh264 \
         decoder…"
    );
    let mut playback = Playback::new();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    // (frame index, wall-time seconds, w, h, checksum)
    let mut records: Vec<(u64, f64, u32, u32, u64)> = Vec::new();
    let mut bytes_downloaded = plan.head_bytes;
    let mut fragments_used = 0usize;
    let mut reresolves = 0u32;

    'outer: for (fi, frag) in plan.fragments.iter().enumerate() {
        if Instant::now() >= deadline {
            break;
        }
        // Space fetches out (optionally) so this mirrors the live app's gentle,
        // playback-paced download rather than a hammering burst.
        if fetch_delay_ms > 0 && fi > 0 {
            std::thread::sleep(Duration::from_millis(fetch_delay_ms));
        }
        let bytes = match stream::fetch_fragment_bytes(&client, &handle, *frag) {
            Ok(b) => b,
            Err(e) => {
                // googlevideo caps a PoToken-less stream URL at ~500 KiB, then
                // 403s. Re-resolving the video yields a FRESH URL with a fresh
                // budget, so playback can continue past the cap. Retry once.
                if e.contains("403") && reresolves < 8 {
                    reresolves += 1;
                    println!(
                        "  [~500KiB URL budget hit at fragment {}; re-resolving for a fresh URL \
                         (re-resolve #{reresolves})…]",
                        fi + 1
                    );
                    match backend.resolve_video_stream(id) {
                        Ok(h) => {
                            handle = h;
                            match stream::fetch_fragment_bytes(&client, &handle, *frag) {
                                Ok(b) => b,
                                Err(e2) => {
                                    eprintln!("probe-stream: fetch after re-resolve failed: {e2}");
                                    break;
                                }
                            }
                        }
                        Err(e2) => {
                            eprintln!("probe-stream: re-resolve failed: {e2}");
                            break;
                        }
                    }
                } else {
                    eprintln!("probe-stream: fragment fetch failed: {e}");
                    break;
                }
            }
        };
        bytes_downloaded += bytes.len();
        fragments_used += 1;
        let units = mp4::demux_fragment(&bytes, &plan.config);
        let au_count = units.len();
        let mut decoded_here = 0u32;
        for au in units {
            if Instant::now() >= deadline {
                break 'outer;
            }
            if playback.feed(&au) {
                if let Some((width, height, rgba)) = playback.latest_rgba() {
                    decoded_here += 1;
                    let idx = playback.frames_decoded();
                    let t = start.elapsed().as_secs_f64();
                    records.push((idx, t, width, height, checksum(&rgba)));
                }
            }
        }
        if fragments_used <= 6 || fragments_used % 10 == 0 {
            let dur = plan
                .durations
                .get(fragments_used - 1)
                .map(|d| *d as f64 / plan.timescale as f64)
                .unwrap_or(0.0);
            println!(
                "  [fragment {fragments_used}: {} bytes, {:.2}s → {} access units → {} frames decoded]",
                bytes.len(),
                dur,
                au_count,
                decoded_here
            );
        }
    }
    // Drain the decoder's reorder tail so trailing frames are counted too.
    for (width, height, rgba) in playback.flush() {
        let idx = playback.frames_decoded();
        let t = start.elapsed().as_secs_f64();
        records.push((idx, t, width, height, checksum(&rgba)));
    }

    let elapsed = start.elapsed().as_secs_f64();
    let n = records.len();
    if n == 0 {
        eprintln!(
            "probe-stream: NO frames decoded from the fetched fragments — the stream resolved and \
             was indexed, but openh264 produced no pictures. Reported honestly, not faked."
        );
        std::process::exit(3);
    }

    // Show the first few and last few frames as evidence of motion over time.
    let show = |r: &(u64, f64, u32, u32, u64)| {
        println!(
            "  frame {:>4} @ {:>5.2}s  {}x{}  checksum=0x{:016x}",
            r.0, r.1, r.2, r.3, r.4
        );
    };
    println!("  --- first frames ---");
    for r in records.iter().take(5) {
        show(r);
    }
    if n > 10 {
        println!("  … {} more …", n - 10);
    }
    if n > 5 {
        println!("  --- last frames ---");
        for r in records.iter().skip(n.saturating_sub(5)) {
            show(r);
        }
    }

    let distinct: std::collections::HashSet<u64> = records.iter().map(|r| r.4).collect();
    let fps = n as f64 / elapsed.max(1e-6);
    // Frames decoded ÷ stream fps = seconds of actual video that plays.
    let video_secs = n as f64 / (handle.fps as f64).max(1.0);
    println!(
        "probe-stream: decoded {n} frames in {elapsed:.2}s across {fragments_used} fragments \
         ({} KiB) → {fps:.1} fps decode throughput (stream is {} fps).",
        bytes_downloaded / 1024,
        handle.fps
    );
    println!(
        "  {} / {} frames have a DISTINCT checksum — this is REAL moving video, not a still.",
        distinct.len(),
        n
    );
    println!(
        "  That is \u{2248}{video_secs:.0}s of continuous {} video, decoded at {:.0}x real time \
         (headroom to keep up easily at the stream's {} fps).",
        handle.quality,
        fps / (handle.fps as f64).max(1.0),
        handle.fps
    );
    println!(
        "  LIMITATION (honest): a PoToken-less googlevideo URL is hard-capped at ~500 KiB per \
         video, then 403s — re-resolving yields a fresh URL that ALSO 403s at the same offset, so \
         the cap is per-video, not per-URL. At {}, that budget is the \u{2248}{video_secs:.0}s above. \
         Full-length playback needs a PoToken (BotGuard), which this build deliberately does not \
         enable. Audio is out of scope (video only).",
        handle.quality
    );
    if distinct.len() < 2 {
        eprintln!("probe-stream: frames did NOT change — no motion proven. Failing loudly.");
        std::process::exit(3);
    }
    Ok(())
}

#[cfg(not(feature = "live"))]
fn run_probe_stream(
    _id: &str,
    _seconds: u64,
    _fetch_delay_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe-stream: built WITHOUT the `live` feature — rebuild with `--features live` to \
         stream and decode a real continuous H.264 sequence."
    );
    Ok(())
}

/// Cheap 64-bit rolling checksum of a decoded frame's RGBA bytes, used only to
/// demonstrate that successive decoded pictures actually differ (motion) rather
/// than being the same still repeated.
#[cfg(feature = "live")]
fn checksum(rgba: &[u8]) -> u64 {
    let mut h: u64 = 1469598103934665603; // FNV-1a offset basis
    for &b in rgba.iter().step_by(97) {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// `--probe-trending`: prove the Home/Trending feed is real. Shows YouTube's
/// dedicated trending endpoint failing for real, then the composed popular feed
/// (view-count-sorted search) that replaces it — with real titles.
#[cfg(feature = "live")]
fn run_probe_trending() -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    eprintln!("probe-trending: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;

    println!("probe-trending: calling the dedicated trending endpoint (FEtrending)…");
    match backend.trending() {
        Ok(BackendResult::Videos(vs)) if !vs.is_empty() => {
            println!("  REAL trending returned {} videos:", vs.len());
            for (i, v) in vs.iter().take(15).enumerate() {
                println!("  {:>2}. [{}] {} — {}", i + 1, v.id, v.title, v.channel_name);
            }
            return Ok(());
        }
        Ok(_) => println!("  dedicated endpoint returned EMPTY (no error, no videos)."),
        Err(e) => println!("  dedicated endpoint FAILED (as expected, it is retired): {e}"),
    }

    println!(
        "\nprobe-trending: composing a REAL popular feed via view-count-sorted, \
         last-week search…"
    );
    const SEEDS: [&str; 6] = ["news", "music", "gaming", "technology", "sports", "science"];
    let mut total = 0usize;
    for seed in SEEDS {
        match backend.popular_search(seed) {
            Ok(BackendResult::Videos(vs)) if !vs.is_empty() => {
                total += vs.len();
                println!(
                    "  {seed:>10}: {} popular videos; top: [{}] {} ({} views)",
                    vs.len(),
                    vs[0].id,
                    vs[0].title,
                    vs[0].views
                );
            }
            Ok(_) => println!("  {seed:>10}: empty"),
            Err(e) => eprintln!("  {seed:>10}: FAILED: {e}"),
        }
    }
    if total == 0 {
        eprintln!("probe-trending: no popular results at all — network/parse failure.");
        std::process::exit(2);
    }
    println!(
        "probe-trending: composed feed draws on {total} real popular videos across {} topics.",
        SEEDS.len()
    );
    Ok(())
}

#[cfg(not(feature = "live"))]
fn run_probe_trending() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe-trending: built WITHOUT the `live` feature — rebuild with \
         `--features live` to hit real YouTube."
    );
    Ok(())
}

/// `--live-shots <dir> [query]`: render REAL offscreen stills — a search page
/// with real YouTube results + fetched thumbnails, and a watch page with a real
/// frame decoded through the shared media engine from a rangeable proxy stream.
#[cfg(all(feature = "live", feature = "media"))]
fn run_live_shots(dir: &str, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use tube::backend::{proxy, ytdlp::YtDlpBackend, yt_thumb_url, BackendResult, YtBackend};
    use tube::model::{DataSource, Route, TubeModel, Video};

    let out_dir = std::path::Path::new(dir);
    std::fs::create_dir_all(out_dir).ok();
    let backend = YtDlpBackend::new().map_err(|e| format!("yt-dlp backend: {e}"))?;

    // ---- Real watch page with a real decoded frame ----
    //
    // Rendered FIRST on purpose: the media engine's GStreamer decode must run
    // before any offscreen `render_shot_to_png` initialises the process-wide
    // wgpu/EGL context — that context otherwise grabs GPU/EGL resources and
    // starves the decode pipeline, so a search render done first would leave the
    // watch surface black. (Proven with `--probe-engine`, which decodes fine with
    // no wgpu in the process.)
    let watch_id = "aqz-KE-bpKQ"; // Big Buck Bunny (Blender, CC-BY) — long + stable.
    println!("live-shots[watch]: metadata + rangeable stream for [{watch_id}]…");
    let video = match backend.video(watch_id) {
        Ok(BackendResult::Video(v)) => *v,
        _ => Video {
            id: watch_id.to_string(),
            title: "Big Buck Bunny".into(),
            dearrow_title: None,
            channel_id: String::new(),
            channel_name: "Blender".into(),
            channel_seed: watch_id.to_string(),
            channel_avatar_url: String::new(),
            views: 0,
            published: String::new(),
            duration: 0,
            description: String::new(),
            likes: 0,
            seed: watch_id.to_string(),
            thumbnail_url: yt_thumb_url(watch_id),
            category: "Video".into(),
            dearrow_thumb: false,
            sponsor: Vec::new(),
            comments: Vec::new(),
            live: false,
        },
    };
    // For a single-frame still, decode the yt-dlp direct progressive URL first
    // (small + fast to preroll — the live app instead uses the rangeable
    // Invidious proxy for seekable, full-length playback), falling back to the
    // proxy if the direct extractor is unavailable.
    let resolved = backend
        .resolve_playback_source(watch_id)
        .ok()
        .map(|s| (s.url, s.quality))
        .or_else(|| proxy::resolve_rangeable(watch_id).map(|r| (r.url, r.quality)));
    match resolved {
        Some((url, quality)) => {
            println!("  resolved {quality} stream; decoding a real frame via the media engine…");
            let mut wmodel = TubeModel::default();
            wmodel.ensure_channel_stub(&video.channel_id, &video.channel_name);
            wmodel.upsert_video(video.clone());
            wmodel.data_source = DataSource { live: true, label: "live".into() };
            // Fetch the uploader's real avatar so the watch-page channel row shows
            // a real photo (issue 1, watch surface), and prewarm it into a store.
            let wstore = tube::images::image_store();
            if let Ok(BackendResult::Channel { channel, .. }) = backend.channel(&video.channel_id) {
                wmodel.note_channel_avatar(&channel.id, &channel.avatar_url);
                if !channel.avatar_url.is_empty() {
                    println!("  creator avatar: {}", short_url(&channel.avatar_url));
                    wstore.get(&channel.avatar_url);
                    wstore.block_until_idle(Duration::from_secs(20));
                }
            }
            let watch_app = tube::TubeApp::new()
                .with_viewport(1280.0, 860.0)
                .with_model_for_test(wmodel)
                .with_image_store(wstore)
                .with_live_frame_for_shot(watch_id, url);
            let watch_png = out_dir.join("watch.png");
            render_shot_to_png(Box::new(watch_app) as Box<dyn Widget>, (1280, 860), &watch_png)?;
            println!("  wrote {}", watch_png.display());
        }
        None => {
            eprintln!(
                "live-shots[watch]: no Invidious proxy served bytes and no direct URL resolved \
                 — skipped the watch still (reported honestly, not faked)."
            );
        }
    }

    // ---- Real search / home / playlist pages with real thumbnails + avatars ----
    //
    // Use rustypipe's search: it returns videos (with real creator avatars),
    // playlists AND creator rows in one call — exactly the enrichment the live
    // app folds into the yt-dlp results, so these stills prove real avatars
    // (issue 1), the Video·Playlist·Creator filter (issue 3) and playlists
    // (issue 4) end to end.
    let rp = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("rustypipe backend: {e}"))?;

    // Prewarm every thumbnail + avatar into one shared store so both the search
    // and home stills paint real images.
    let prewarm = |store: &ui::ImageStore, urls: &[String]| {
        for u in urls {
            if !u.is_empty() {
                store.get(u);
            }
        }
    };

    println!("live-shots[search]: rustypipe search for {query:?}…");
    match rp.search_items(query) {
        Ok(items) if !(items.videos.is_empty() && items.playlists.is_empty() && items.channels.is_empty()) => {
            println!(
                "  {} videos, {} playlists, {} creators",
                items.videos.len(),
                items.playlists.len(),
                items.channels.len()
            );
            let mut smodel = TubeModel::default();
            let vids: Vec<String> = items.videos.iter().map(|v| v.id.clone()).collect();
            for v in &items.videos {
                smodel.ensure_channel_stub(&v.channel_id, &v.channel_name);
                smodel.adopt_channel_avatar(&v.channel_id, &v.channel_name, &v.channel_avatar_url);
                smodel.upsert_video(v.clone());
            }
            let pids: Vec<String> = items.playlists.iter().map(|p| p.id.clone()).collect();
            for p in &items.playlists {
                smodel.ensure_channel_stub(&p.channel_id, &p.channel_name);
                smodel.upsert_playlist(p.clone());
            }
            let cids: Vec<String> = items.channels.iter().map(|c| c.id.clone()).collect();
            for c in &items.channels {
                smodel.adopt_channel_avatar(&c.id, &c.name, &c.avatar_url);
                smodel.upsert_channel(c.clone());
            }
            smodel.reconcile_channel_avatars();
            // The live app fills the remaining (dominant-channel) avatars via a
            // per-channel fetch; do the same here so the still reflects what the
            // running app converges to — fetch the channel page for the most
            // common uploaders that still lack an avatar (bounded), and adopt it.
            {
                use std::collections::HashMap;
                let mut freq: HashMap<String, usize> = HashMap::new();
                for v in &items.videos {
                    if smodel.channel(&v.channel_id).map(|c| c.avatar_url.is_empty()).unwrap_or(true) {
                        *freq.entry(v.channel_id.clone()).or_insert(0) += 1;
                    }
                }
                let mut ranked: Vec<(String, usize)> = freq.into_iter().collect();
                ranked.sort_by(|a, b| b.1.cmp(&a.1));
                for (cid, _) in ranked.into_iter().take(5) {
                    if let Ok(BackendResult::Channel { channel, .. }) = backend.channel(&cid) {
                        smodel.adopt_channel_avatar(&channel.id, &channel.name, &channel.avatar_url);
                        println!("  fetched avatar for {:?}: {}", channel.name, short_url(&channel.avatar_url));
                    }
                }
                smodel.reconcile_channel_avatars();
            }
            smodel.search_query = query.to_string();
            smodel.search_hits_query = query.to_string();
            smodel.search_hits = vids;
            smodel.search_playlist_hits = pids;
            smodel.search_channel_hits = cids;
            smodel.route = Route::Search(query.to_string());
            smodel.data_source = DataSource { live: true, label: "live".into() };

            // Collect every image URL the search page will paint — including the
            // avatars now stamped onto the channel records by the fetch above.
            let mut urls: Vec<String> = Vec::new();
            for v in &items.videos {
                urls.push(v.thumbnail_url.clone());
            }
            for ch in &smodel.channels {
                urls.push(ch.avatar_url.clone());
            }
            for p in &items.playlists {
                urls.push(p.thumbnail_url.clone());
            }
            let store = tube::images::image_store();
            prewarm(&store, &urls);
            println!("  prewarming {} thumbnails + avatars…", urls.iter().filter(|u| !u.is_empty()).count());
            store.block_until_idle(Duration::from_secs(45));
            println!("  images settled: {} still pending", store.pending());

            // DIAGNOSTIC: for every channel a card will read, print the record's
            // avatar url + whether the store has it decoded at render time.
            println!("  -- render-time channel-record state --");
            for ch in &smodel.channels {
                let decoded = store.get(&ch.avatar_url).is_some();
                println!(
                    "     id={:?} name={:?} avatar={} store={}",
                    ch.id,
                    ch.name,
                    if ch.avatar_url.is_empty() { "<EMPTY>" } else { "set" },
                    if decoded { "DECODED" } else { "miss" },
                );
            }

            let search_app = tube::TubeApp::new()
                .with_viewport(1280.0, 860.0)
                .with_model_for_test(smodel.clone())
                .with_image_store(store.clone());
            let search_png = out_dir.join("search.png");
            render_shot_to_png(Box::new(search_app) as Box<dyn Widget>, (1280, 860), &search_png)?;
            println!("  wrote {}", search_png.display());

            // ---- Home page (same catalogue on the Home route) — avatar proof #1 ----
            let mut hmodel = smodel.clone();
            hmodel.route = Route::Home;
            let home_app = tube::TubeApp::new()
                .with_viewport(1280.0, 860.0)
                .with_model_for_test(hmodel)
                .with_image_store(store.clone());
            let home_png = out_dir.join("home.png");
            render_shot_to_png(Box::new(home_app) as Box<dyn Widget>, (1280, 860), &home_png)?;
            println!("  wrote {}", home_png.display());

            // ---- Playlist page (issue 4) — try each search-result playlist
            // until one resolves (some listed playlists are stale/empty). ----
            // yt-dlp resolves playlists reliably (rustypipe's current parser does
            // not); try each search-result playlist until one resolves.
            let resolved_pl = items.playlists.iter().find_map(|p| {
                println!("live-shots[playlist]: fetching {} via yt-dlp…", p.id);
                backend.playlist(&p.id).ok()
            });
            if let Some((pl, pvids)) = resolved_pl {
                let pid = pl.id.clone();
                let mut pmodel = TubeModel::default();
                for v in &pvids {
                    pmodel.ensure_channel_stub(&v.channel_id, &v.channel_name);
                    pmodel.note_channel_avatar(&v.channel_id, &v.channel_avatar_url);
                    pmodel.upsert_video(v.clone());
                }
                pmodel.ensure_channel_stub(&pl.channel_id, &pl.channel_name);
                let purls: Vec<String> = std::iter::once(pl.thumbnail_url.clone())
                    .chain(pvids.iter().map(|v| v.thumbnail_url.clone()))
                    .collect();
                pmodel.upsert_playlist(pl.clone());
                pmodel.route = Route::Playlist(pid.clone());
                let pstore = tube::images::image_store();
                prewarm(&pstore, &purls);
                pstore.block_until_idle(Duration::from_secs(30));
                let pl_app = tube::TubeApp::new()
                    .with_viewport(1280.0, 860.0)
                    .with_model_for_test(pmodel)
                    .with_image_store(pstore);
                let pl_png = out_dir.join("playlist.png");
                render_shot_to_png(Box::new(pl_app) as Box<dyn Widget>, (1280, 860), &pl_png)?;
                println!("  wrote {} ({} videos)", pl_png.display(), pvids.len());
            } else {
                eprintln!("live-shots[playlist]: no listed playlist resolved — skipping playlist still.");
            }
        }
        Ok(_) => eprintln!("live-shots[search]: empty result set — skipping search/home/playlist stills."),
        Err(e) => eprintln!("live-shots[search]: rustypipe search failed: {e}"),
    }

    println!("live-shots: done ({})", out_dir.display());
    Ok(())
}

#[cfg(not(all(feature = "live", feature = "media")))]
fn run_live_shots(_dir: &str, _query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("live-shots: needs `--features \"live media\"` (real data + real decode + images).");
    Ok(())
}

/// Shorten a URL for log lines (host + a hint), so proof output stays readable.
#[cfg(feature = "live")]
fn short_url(url: &str) -> String {
    if url.is_empty() {
        return "<none>".into();
    }
    let head = url.split('?').next().unwrap_or(url);
    if head.len() > 72 {
        format!("{}…", &head[..72])
    } else {
        head.to_string()
    }
}

/// `--probe-ytdlp [query]`: prove the yt-dlp extractor end-to-end — search,
/// video metadata, and a channel page with its real avatar + banner URLs.
#[cfg(feature = "live")]
fn run_probe_ytdlp(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    eprintln!("probe-ytdlp: constructing the yt-dlp backend…");
    let backend = tube::backend::ytdlp::YtDlpBackend::new()
        .map_err(|e| format!("yt-dlp backend: {e}"))?;
    println!("probe-ytdlp: backend = {}", backend.name());

    println!("probe-ytdlp: searching YouTube for {query:?} …");
    let (first_id, first_channel) = match backend.search(query)? {
        BackendResult::Videos(vs) => {
            if vs.is_empty() {
                eprintln!("probe-ytdlp: EMPTY result set — reached yt-dlp but nothing parsed.");
                std::process::exit(2);
            }
            println!("  {} REAL results from YouTube:", vs.len());
            for (i, v) in vs.iter().take(10).enumerate() {
                println!(
                    "  {:>2}. [{}] {} — {} ({} views)\n        thumb={}",
                    i + 1,
                    v.id,
                    v.title,
                    v.channel_name,
                    v.views,
                    short_url(&v.thumbnail_url)
                );
            }
            (vs[0].id.clone(), vs[0].channel_id.clone())
        }
        _ => {
            println!("probe-ytdlp: unexpected result variant");
            return Ok(());
        }
    };

    println!("\nprobe-ytdlp: video_details [{first_id}] …");
    if let BackendResult::Video(v) = backend.video(&first_id)? {
        println!(
            "  title={:?} channel={:?} views={} likes={} desc_len={}\n  thumb={}",
            v.title,
            v.channel_name,
            v.views,
            v.likes,
            v.description.len(),
            short_url(&v.thumbnail_url)
        );
    }

    println!("\nprobe-ytdlp: channel [{first_channel}] …");
    if let BackendResult::Channel { channel, uploads } = backend.channel(&first_channel)? {
        println!(
            "  {:?} ({} subscribers, verified={}); {} uploads",
            channel.name,
            channel.subscribers,
            channel.verified,
            uploads.len()
        );
        println!("  avatar_url = {}", short_url(&channel.avatar_url));
        println!("  banner_url = {}", short_url(&channel.banner_url));
        if channel.avatar_url.is_empty() || channel.banner_url.is_empty() {
            eprintln!("probe-ytdlp: WARNING — channel avatar/banner url missing.");
        }
        for u in uploads.iter().take(3) {
            println!("    upload [{}] {} — thumb={}", u.id, u.title, short_url(&u.thumbnail_url));
        }
    }
    Ok(())
}

#[cfg(not(feature = "live"))]
fn run_probe_ytdlp(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("probe-ytdlp: built WITHOUT `live` — rebuild with `--features live`.");
    Ok(())
}

/// `--probe-avatars [query]`: diagnose the creator-avatar pipeline end to end.
#[cfg(feature = "live")]
fn run_probe_avatars(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;
    use tube::backend::{ytdlp::YtDlpBackend, BackendResult, YtBackend};

    eprintln!("probe-avatars: constructing yt-dlp + rustypipe backends…");
    let yt = YtDlpBackend::new().map_err(|e| format!("yt-dlp: {e}"))?;
    let rp = tube::backend::live::RustyPipeBackend::new().map_err(|e| format!("rustypipe: {e}"))?;

    // 1) yt-dlp search: what channel id/name/avatar does each result carry?
    println!("\n== yt-dlp search {query:?} ==");
    let yt_videos = match yt.search(query)? {
        BackendResult::Videos(v) => v,
        _ => Vec::new(),
    };
    let mut yt_channel_freq: HashMap<String, (String, usize)> = HashMap::new();
    for v in yt_videos.iter().take(12) {
        println!(
            "  [{}] chan_id={:?} name={:?} avatar={}",
            v.id,
            v.channel_id,
            v.channel_name,
            if v.channel_avatar_url.is_empty() { "<EMPTY>" } else { "present" }
        );
    }
    for v in &yt_videos {
        let e = yt_channel_freq.entry(v.channel_id.clone()).or_insert((v.channel_name.clone(), 0));
        e.1 += 1;
    }

    // 2) rustypipe search: videos (with channel avatar?) + creator rows.
    println!("\n== rustypipe search_items {query:?} ==");
    let items = rp.search_items(query)?;
    println!(
        "  {} videos, {} playlists, {} creators",
        items.videos.len(),
        items.playlists.len(),
        items.channels.len()
    );
    let mut rp_avatars: HashMap<String, String> = HashMap::new();
    for v in items.videos.iter().take(12) {
        println!(
            "  vid [{}] chan_id={:?} name={:?} avatar={}",
            v.id,
            v.channel_id,
            v.channel_name,
            if v.channel_avatar_url.is_empty() { "<none>".into() } else { short_url(&v.channel_avatar_url) }
        );
        if !v.channel_id.is_empty() && !v.channel_avatar_url.is_empty() {
            rp_avatars.entry(v.channel_id.clone()).or_insert(v.channel_avatar_url.clone());
        }
    }
    for c in &items.channels {
        println!(
            "  creator id={:?} name={:?} avatar={}",
            c.id,
            c.name,
            if c.avatar_url.is_empty() { "<none>".into() } else { short_url(&c.avatar_url) }
        );
        if !c.id.is_empty() && !c.avatar_url.is_empty() {
            rp_avatars.entry(c.id.clone()).or_insert(c.avatar_url.clone());
        }
    }

    // 2b) Fetch-test every distinct rustypipe avatar URL through the REAL image
    // store (same reqwest fetch + `image` decode the render path uses), so a URL
    // that is "present" but unrenderable is caught here rather than silently
    // degrading to initials on the card.
    println!("\n== fetch-test rustypipe avatar URLs through the real ImageStore ==");
    let store = tube::images::image_store();
    for url in rp_avatars.values() {
        store.get(url);
    }
    store.block_until_idle(std::time::Duration::from_secs(30));
    for (cid, url) in &rp_avatars {
        let ok = store.get(url).is_some();
        println!(
            "  {} id={cid:?}\n     {}",
            if ok { "DECODED" } else { "FAILED " },
            url
        );
    }

    // 3) By-id fold match rate: how many yt-dlp videos get an avatar?
    let matched = yt_videos
        .iter()
        .filter(|v| rp_avatars.contains_key(&v.channel_id))
        .count();
    println!(
        "\n== by-CHANNEL-ID fold match: {}/{} yt-dlp videos would get an avatar ==",
        matched,
        yt_videos.len()
    );
    // Name-based match (normalized), to see if names line up when ids don't.
    let norm = |s: &str| s.trim().to_lowercase();
    let rp_names: HashMap<String, String> = items
        .videos
        .iter()
        .filter(|v| !v.channel_avatar_url.is_empty())
        .map(|v| (norm(&v.channel_name), v.channel_avatar_url.clone()))
        .chain(
            items
                .channels
                .iter()
                .filter(|c| !c.avatar_url.is_empty())
                .map(|c| (norm(&c.name), c.avatar_url.clone())),
        )
        .collect();
    let matched_name = yt_videos.iter().filter(|v| rp_names.contains_key(&norm(&v.channel_name))).count();
    println!(
        "== by-NORMALIZED-NAME fold match: {}/{} yt-dlp videos would get an avatar ==",
        matched_name,
        yt_videos.len()
    );

    // 4) Dominant yt-dlp channel: can a per-channel fetch resolve its avatar?
    if let Some((cid, (cname, freq))) = yt_channel_freq.iter().max_by_key(|(_, (_, n))| *n) {
        println!("\n== dominant yt-dlp channel: {cname:?} id={cid:?} ({freq} videos) ==");
        match yt.channel(cid) {
            Ok(BackendResult::Channel { channel, .. }) => println!(
                "  yt-dlp channel({cid}) → id={:?} name={:?} subs={} avatar={}",
                channel.id,
                channel.name,
                channel.subscribers,
                if channel.avatar_url.is_empty() { "<EMPTY>".into() } else { short_url(&channel.avatar_url) }
            ),
            Ok(_) => println!("  yt-dlp channel: unexpected variant"),
            Err(e) => println!("  yt-dlp channel FAILED: {e}"),
        }
        match rp.channel(cid) {
            Ok(BackendResult::Channel { channel, .. }) => println!(
                "  rustypipe channel({cid}) → id={:?} name={:?} subs={} avatar={}",
                channel.id,
                channel.name,
                channel.subscribers,
                if channel.avatar_url.is_empty() { "<EMPTY>".into() } else { short_url(&channel.avatar_url) }
            ),
            Ok(_) => println!("  rustypipe channel: unexpected variant"),
            Err(e) => println!("  rustypipe channel FAILED: {e}"),
        }
    }

    // 5) Fetch + decode the ACTUAL avatar URLs the cards would load — this is
    //    where "URL present in model" and "photo paints on the card" diverge.
    //    We hit the very same reqwest fetcher the shots/live app use, then decode
    //    with the `image` crate exactly like the ImageStore, and report the
    //    content-type + decode result per URL.
    println!("\n== fetch + decode avatar URLs (real transport + ImageStore decoder) ==");
    let fetch = tube::images::image_fetcher();
    let store = tube::images::image_store();
    let magic = |b: &[u8]| -> &'static str {
        if b.len() >= 3 && &b[0..3] == b"\xFF\xD8\xFF" {
            "JPEG"
        } else if b.len() >= 4 && &b[0..4] == b"\x89PNG" {
            "PNG"
        } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
            "WEBP"
        } else if b.len() >= 4 && &b[0..4] == b"GIF8" {
            "GIF"
        } else {
            "OTHER"
        }
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut probe_one = |label: &str, url: &str| {
        if url.is_empty() || !seen.insert(url.to_string()) {
            return;
        }
        // Production decode path: kick the store, wait, ask if it produced an image.
        let _ = store.get(url);
        store.block_until_idle(std::time::Duration::from_secs(15));
        let store_ok = store.get(url).is_some();
        match fetch(url) {
            Some(bytes) => println!(
                "  {label}: {} bytes, format={}, ImageStore.decode={} — {}",
                bytes.len(),
                magic(&bytes),
                if store_ok { "OK" } else { "FAILED" },
                short_url(url),
            ),
            None => println!("  {label}: FETCH FAILED (store.decode={store_ok}) — {}", short_url(url)),
        }
    };
    // The dominant channel's avatar from each source the app might use.
    if let Some(v) = items.videos.iter().find(|v| !v.channel_avatar_url.is_empty()) {
        probe_one("rustypipe video-item avatar", &v.channel_avatar_url);
    }
    if let Some(c) = items.channels.iter().find(|c| !c.avatar_url.is_empty()) {
        probe_one("rustypipe creator-row avatar", &c.avatar_url);
    }
    if let Some((cid, _)) = yt_channel_freq.iter().max_by_key(|(_, (_, n))| *n) {
        if let Ok(BackendResult::Channel { channel, .. }) = yt.channel(cid) {
            probe_one("yt-dlp channel-page avatar", &channel.avatar_url);
        }
    }
    // A known-good thumbnail for comparison (these always paint on the cards).
    if let Some(v) = items.videos.iter().find(|v| !v.thumbnail_url.is_empty()) {
        probe_one("video thumbnail (control)", &v.thumbnail_url);
    }
    Ok(())
}

#[cfg(not(feature = "live"))]
fn run_probe_avatars(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("probe-avatars: built WITHOUT `live` — rebuild with `--features live`.");
    Ok(())
}

/// `--probe-gst <id> [seconds]`: prove the GStreamer decode path. Resolves a
/// real stream URL via yt-dlp and decodes a *sequence* of real frames, using
/// per-frame checksums to prove the picture actually moves (not one still).
#[cfg(all(feature = "live", feature = "media"))]
fn run_probe_gst(id: &str, seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use tube::backend::{gst_playback, ytdlp::YtDlpBackend, YtBackend};

    eprintln!("probe-gst: constructing the yt-dlp backend…");
    let backend = YtDlpBackend::new().map_err(|e| format!("yt-dlp backend: {e}"))?;

    println!("probe-gst: resolving a directly-playable URL for [{id}] …");
    let src = backend
        .resolve_playback_source(id)
        .map_err(|e| format!("resolve: {e}"))?;
    println!(
        "  RESOLVED: {} {}x{}@{}fps audio={} [{}]\n  url(host)={}",
        src.quality,
        src.width,
        src.height,
        src.fps,
        src.has_audio,
        src.format,
        short_url(&src.url)
    );

    let max_frames = (seconds as usize * 60).max(30);
    println!("probe-gst: decoding through GStreamer for up to {seconds}s…");
    let frames = gst_playback::decode_frames(&src.url, max_frames, Duration::from_secs(seconds + 25))
        .map_err(|e| format!("gstreamer decode: {e}"))?;
    if frames.is_empty() {
        eprintln!("probe-gst: NO frames decoded — reported honestly, not faked.");
        std::process::exit(3);
    }

    let sums: Vec<u64> = frames.iter().map(|(_, _, rgba)| checksum(rgba)).collect();
    let distinct: std::collections::HashSet<u64> = sums.iter().copied().collect();
    let (w, h, _) = &frames[0];
    println!(
        "probe-gst: decoded {} frames at {}x{}; {} of {} have a DISTINCT checksum \
         — REAL moving video, not a still.",
        frames.len(),
        w,
        h,
        distinct.len(),
        frames.len()
    );
    for (i, (fw, fh, rgba)) in frames.iter().enumerate().take(3) {
        println!("  frame {i}: {fw}x{fh}  checksum=0x{:016x}", checksum(rgba));
    }
    if distinct.len() < 2 {
        eprintln!("probe-gst: frames did NOT change — no motion proven. Failing loudly.");
        std::process::exit(3);
    }
    Ok(())
}

#[cfg(not(all(feature = "live", feature = "media")))]
fn run_probe_gst(_id: &str, _seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    println!("probe-gst: needs `--features \"live media\"` (yt-dlp resolve + GStreamer decode).");
    Ok(())
}


/// `--probe-play <id>`: prove real playback end-to-end. Resolves a real
/// googlevideo H.264 stream, downloads a bounded prefix, demuxes it to Annex-B,
/// and decodes at least one frame through the same [`tube::playback::Playback`]
/// the watch page uses. Exits non-zero (loudly) if any stage fails — no faking.
#[cfg(feature = "live")]
fn run_probe_play(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{stream, YtBackend};
    use tube::playback::Playback;

    eprintln!("probe-play: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;

    println!("probe-play: resolving a playable stream for [{id}] via player()…");
    let handle = match backend.resolve_video_stream(id) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("probe-play: STREAM RESOLUTION FAILED: {e}");
            std::process::exit(2);
        }
    };
    println!(
        "  RESOLVED stream: itag={} mime={:?} quality={} {}x{}@{}fps adaptive={} \
         content_length={:?}",
        handle.itag,
        handle.mime,
        handle.quality,
        handle.width,
        handle.height,
        handle.fps,
        handle.adaptive,
        handle.content_length
    );
    println!(
        "  init_range={:?} index_range={:?} ua={:?}\n  url(host)={}",
        handle.init_range,
        handle.index_range,
        handle.user_agent,
        handle.url.split('?').next().unwrap_or("<url>")
    );

    println!("probe-play: downloading a bounded prefix + demuxing to Annex-B H.264…");
    let client = stream::http_client();
    let fetched = match stream::fetch_h264(&client, &handle) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("probe-play: DOWNLOAD/DEMUX FAILED: {e}");
            std::process::exit(2);
        }
    };
    println!(
        "  downloaded {} bytes (HTTP {}); demuxed {} SPS + {} PPS + {} frame NAL units \
         → {} Annex-B bytes",
        fetched.downloaded,
        fetched.http_status,
        fetched.sps_count,
        fetched.pps_count,
        fetched.nalu_count,
        fetched.annexb.len()
    );

    println!("probe-play: decoding through openh264 (tube::playback::Playback)…");
    let mut playback = Playback::new();
    let decoded = playback.feed(&fetched.annexb);
    match (decoded, playback.frame_dimensions()) {
        (true, Some((w, h))) if w > 0 && h > 0 => {
            println!(
                "  ✅ REAL frame decoded: {}x{} px, frames_decoded={}",
                w,
                h,
                playback.frames_decoded()
            );
            Ok(())
        }
        _ => {
            eprintln!(
                "probe-play: openh264 did not emit a frame from the fetched bitstream \
                 (decoded={decoded}, dims={:?}). The stream resolved and demuxed, but no \
                 picture came out — reported honestly, not faked.",
                playback.frame_dimensions()
            );
            std::process::exit(3);
        }
    }
}

#[cfg(not(feature = "live"))]
fn run_probe_play(_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe-play: built WITHOUT the `live` feature — rebuild with \
         `--features live` to resolve and decode a real stream."
    );
    Ok(())
}

/// The real proof: construct the live backend, hit YouTube, print what came
/// back. Compiled to do real work only under `--features live`.
#[cfg(feature = "live")]
fn run_probe(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    eprintln!("probe: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;
    println!("probe: backend = {}", backend.name());

    println!("probe: searching YouTube for {query:?} …");
    let (first_id, first_channel) = match backend.search(query) {
        Ok(BackendResult::Videos(vs)) => {
            if vs.is_empty() {
                eprintln!("probe: EMPTY result set — network reached YouTube but no videos parsed.");
                std::process::exit(2);
            }
            println!("probe: {} REAL results from YouTube:", vs.len());
            for (i, v) in vs.iter().take(15).enumerate() {
                println!(
                    "  {:>2}. [{}] {}  — {} ({} views)",
                    i + 1,
                    v.id,
                    v.title,
                    v.channel_name,
                    v.views
                );
            }
            let f = &vs[0];
            (f.id.clone(), f.channel_id.clone())
        }
        Ok(other) => {
            println!("probe: unexpected result variant: {other:?}");
            return Ok(());
        }
        Err(e) => {
            eprintln!("probe: LIVE SEARCH FAILED (network/parse): {e}");
            std::process::exit(2);
        }
    };

    // Video metadata (watch page data path).
    println!("\nprobe: video_details [{first_id}] …");
    match backend.video(&first_id) {
        Ok(BackendResult::Video(v)) => println!(
            "  title={:?} channel={:?} views={} likes={} desc_len={}",
            v.title,
            v.channel_name,
            v.views,
            v.likes,
            v.description.len()
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  video_details failed: {e}"),
    }

    // Real "up next" recommendations (watch-page sidebar data path).
    println!("probe: related [{first_id}] …");
    match backend.related(&first_id) {
        Ok(BackendResult::Videos(vs)) => println!(
            "  {} recommendations; first: {:?}",
            vs.len(),
            vs.first().map(|v| v.title.as_str()).unwrap_or("<none>")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  related failed: {e}"),
    }

    // Channel page + uploads (channel screen data path).
    println!("probe: channel_videos [{first_channel}] …");
    match backend.channel(&first_channel) {
        Ok(BackendResult::Channel { channel, uploads }) => println!(
            "  {:?} ({} subs, verified={}); {} uploads; first: {:?}",
            channel.name,
            channel.subscribers,
            channel.verified,
            uploads.len(),
            uploads.first().map(|v| v.title.as_str()).unwrap_or("<none>")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  channel_videos failed: {e}"),
    }

    // Trending exercises the browse feed (the Home/Trending data path).
    println!("probe: trending …");
    match backend.trending() {
        Ok(BackendResult::Videos(vs)) => println!(
            "  {} videos; first: {:?}",
            vs.len(),
            vs.first().map(|v| v.title.as_str()).unwrap_or("<none>")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  trending failed: {e}"),
    }
    Ok(())
}

/// Non-live builds have no real backend compiled in.
#[cfg(not(feature = "live"))]
fn run_probe(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe: this binary was built WITHOUT the `live` feature, so no real \
         backend is compiled in. Rebuild with `--features live` to probe real \
         YouTube data."
    );
    Ok(())
}

/// Parse `--flag [n]` / `--flag=<n>` into a `usize`, with `default` when the
/// flag is present but no numeric value follows.
fn parse_flag_usize(args: &[String], flag: &str, default: usize) -> Option<usize> {
    let eq = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return Some(iter.next().and_then(|s| s.parse().ok()).unwrap_or(default));
        }
        if let Some(rest) = arg.strip_prefix(eq.as_str()) {
            return Some(rest.parse().ok().unwrap_or(default));
        }
    }
    None
}

/// Median of a slice of microsecond samples (sorted copy).
fn median_us(samples: &[u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}

fn max_us(samples: &[u128]) -> u128 {
    samples.iter().copied().max().unwrap_or(0)
}

fn avg_us(samples: &[u128]) -> u128 {
    if samples.is_empty() {
        0
    } else {
        samples.iter().sum::<u128>() / samples.len() as u128
    }
}

/// `--bench-scroll [frames] [--watch]`: measure the per-frame main-thread cost
/// of scrolling either a large Home grid or the watch page (full-bleed player +
/// a long comment list), in two modes, and report the numbers.
fn run_bench_scroll(frames: usize, watch: bool, loaded: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;
    use tube::model::{Route, TubeModel};
    use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};

    const CARDS: usize = 80;
    let (w, h) = (1280u32, 900u32);

    // A large, realistic Home catalogue: grow the sample by cloning cards with
    // fresh ids so the grid actually stresses layout (the composed live
    // "popular" feed is this size or larger).
    let home_model = || -> TubeModel {
        let mut m = tube::sample::model();
        let base = m.videos.clone();
        let mut i = 0usize;
        while m.videos.len() < CARDS && !base.is_empty() {
            let mut v = base[i % base.len()].clone();
            v.id = format!("{}_{i}", v.id);
            v.seed = v.id.clone();
            m.videos.push(v);
            i += 1;
        }
        m.route = Route::Home;
        m
    };

    // The watch page the scroll-perf complaint was about: a full-bleed player
    // over a long comment list (the real content that makes the scroll tree
    // deep). Grow the chosen video's comments to a realistic count.
    let watch_model = || -> TubeModel {
        let mut m = tube::sample::model();
        let id = m
            .videos
            .iter()
            .find(|v| !v.comments.is_empty())
            .map(|v| v.id.clone())
            .unwrap_or_else(|| m.videos[0].id.clone());
        if let Some(v) = m.videos.iter_mut().find(|v| v.id == id) {
            let base = v.comments.clone();
            let mut i = 0usize;
            while v.comments.len() < 60 && !base.is_empty() {
                v.comments.push(base[i % base.len()].clone());
                i += 1;
            }
        }
        m.route = Route::Watch(id);
        m
    };
    let big_model = || -> TubeModel {
        if watch { watch_model() } else { home_model() }
    };

    // One measured pass. `force_compute` = the pre-fix baseline (always re-solve
    // taffy every frame). `false` = the scroll fast path (apply_layout only when
    // nothing needs re-measuring). Returns (compute[], apply[], draw[]) in µs.
    let run_pass = |force_compute: bool| -> (Vec<u128>, Vec<u128>, Vec<u128>, Vec<u128>, bool) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let model = big_model();
        let watch_id = match &model.route {
            Route::Watch(id) => id.clone(),
            _ => String::new(),
        };
        let mut app = tube::TubeApp::new()
            .with_viewport(w as f64, h as f64)
            .with_model_for_test(model);
        // Load + pause a real (synthesized) frame so the bench measures the true
        // per-frame cost of a paused-but-loaded watch page.
        if watch && loaded && !watch_id.is_empty() {
            app = app.with_bench_loaded_watch(&watch_id);
        }
        let mut widget: Box<dyn Widget> = Box::new(app);
        let mut state = ui::State::new();
        widget.sync(&mut state.doc, &state.root);
        let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size { width: length(w as f32), height: length(h as f32) },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();

        let solve = |taffy: &mut TaffyTree<()>,
                     node_map: &HashMap<NodeId, *mut dyn Widget>,
                     tr: &mut TextRenderer| {
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = node_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        };

        // Initial full solve + layout + a warm draw (fills the Text/scene caches
        // so the timed frames measure steady-state scroll, not first-paint).
        solve(&mut taffy, &node_map, &mut tr);
        widget.apply_layout(&taffy, child, Point::ORIGIN);
        {
            let mut scene = Scene::new();
            widget.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
            widget.mark_drawn();
        }

        let mods = winit::event::Modifiers::default();
        let mut sync = Vec::with_capacity(frames);
        let mut compute = Vec::with_capacity(frames);
        let mut apply = Vec::with_capacity(frames);
        let mut draw = Vec::with_capacity(frames);
        let mut ever_reposition_only = false;

        for _ in 0..frames {
            // Sync first, exactly as the live event loop does: this pumps the
            // media engine (pulling the freshest frame + playhead), so a loaded
            // watch page's per-frame engine cost is measured, not just layout.
            let t = Instant::now();
            widget.sync(&mut state.doc, &state.root);
            sync.push(t.elapsed().as_micros());

            // Real scroll: a wheel notch routed through the widget tree, exactly
            // as the live event loop delivers it.
            let wheel = WindowEvent::MouseWheel {
                device_id: unsafe { std::mem::zeroed() },
                delta: MouseScrollDelta::LineDelta(0.0, -3.0),
                phase: TouchPhase::Moved,
            };
            let mut handled = false;
            let mut focus_target = None;
            widget.handle_event(&wheel, 1.0, &mods, &mut state.doc, &state.root, &mut handled, &mut focus_target, child, &taffy);

            // The fast path skips the solve when nothing needs re-measuring.
            let needs_measure = widget.layout_needs_measure();
            if !needs_measure {
                ever_reposition_only = true;
            }
            if force_compute || needs_measure {
                let t = Instant::now();
                solve(&mut taffy, &node_map, &mut tr);
                compute.push(t.elapsed().as_micros());
            } else {
                compute.push(0);
            }
            let t = Instant::now();
            widget.apply_layout(&taffy, child, Point::ORIGIN);
            apply.push(t.elapsed().as_micros());

            let t = Instant::now();
            let mut scene = Scene::new();
            let prev = presence.push_viewport(vello::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64));
            widget.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
            presence.pop_viewport(prev);
            widget.mark_drawn();
            draw.push(t.elapsed().as_micros());
        }
        (sync, compute, apply, draw, ever_reposition_only)
    };

    let report = |label: &str, s: &[u128], c: &[u128], a: &[u128], d: &[u128]| {
        let total: Vec<u128> = (0..c.len()).map(|i| s[i] + c[i] + a[i] + d[i]).collect();
        println!(
            "  {label:<20} frame(total)  median {:>5.2}ms  avg {:>5.2}ms  max {:>5.2}ms",
            median_us(&total) as f64 / 1000.0,
            avg_us(&total) as f64 / 1000.0,
            max_us(&total) as f64 / 1000.0,
        );
        println!(
            "  {:<20}   sync med/max {:>5.2}/{:>6.2} | compute med/max {:>5.2}/{:>6.2} | apply med/max {:>5.2}/{:>6.2} | draw med/max {:>5.2}/{:>6.2} ms",
            "",
            median_us(s) as f64 / 1000.0,
            max_us(s) as f64 / 1000.0,
            median_us(c) as f64 / 1000.0,
            max_us(c) as f64 / 1000.0,
            median_us(a) as f64 / 1000.0,
            max_us(a) as f64 / 1000.0,
            median_us(d) as f64 / 1000.0,
            max_us(d) as f64 / 1000.0,
        );
    };

    let loaded_note = if watch && loaded { " [loaded+paused player]" } else { "" };
    let scene_label = if watch {
        format!("watch page (player + 60 comments){loaded_note}")
    } else {
        format!("{CARDS} Home cards")
    };
    println!("tube scroll bench: {scene_label}, {w}x{h}, {frames} scroll frames\n");
    let (bs, bc, ba, bd, _) = run_pass(true);
    report("BASELINE (re-solve)", &bs, &bc, &ba, &bd);
    let (fs, fc, fa, fd, repos) = run_pass(false);
    report("FAST (reposition)", &fs, &fc, &fa, &fd);
    println!(
        "\n  scroll frames took the reposition-only fast path: {}",
        if repos { "yes (compute_layout skipped)" } else { "NO — investigate" }
    );
    let fast_total: Vec<u128> = (0..fc.len()).map(|i| fs[i] + fc[i] + fa[i] + fd[i]).collect();
    println!(
        "  FAST-path median frame (incl. sync): {:.2}ms  (sync med {:.2}ms)",
        median_us(&fast_total) as f64 / 1000.0,
        median_us(&fs) as f64 / 1000.0,
    );
    Ok(())
}

fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Parse `--route NAME` (or `--route=NAME`) into a starting [`tube::model::Route`]
/// for a screen-specific shot. Recognises `home`, `trending`, `subs`/
/// `subscriptions`, `history`, `settings`, `watch:<video_id>`, and
/// `search:<query>`.
fn parse_route_arg(args: &[String]) -> Option<tube::model::Route> {
    use tube::model::Route;
    let mut iter = args.iter().skip(1);
    let spec = loop {
        let arg = iter.next()?;
        if arg == "--route" {
            break iter.next()?.to_string();
        }
        if let Some(rest) = arg.strip_prefix("--route=") {
            break rest.to_string();
        }
    };
    let lower = spec.to_ascii_lowercase();
    Some(match lower.as_str() {
        "home" => Route::Home,
        "trending" => Route::Trending,
        "subs" | "subscriptions" => Route::Subscriptions,
        "history" => Route::History,
        "settings" => Route::Settings,
        _ => {
            if let Some(id) = spec.strip_prefix("watch:") {
                Route::Watch(id.to_string())
            } else if let Some(q) = spec.strip_prefix("search:") {
                Route::Search(q.to_string())
            } else {
                Route::Search(spec)
            }
        }
    })
}

/// Parse `--palette-query <q>` (or `--palette-query=<q>`) — the query to seed
/// the command-palette shot with.
fn parse_palette_query_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--palette-query" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--palette-query=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse `--viewport WxH` (or `--viewport=WxH`) into a (width, height) pair.
fn parse_viewport_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let spec = if arg == "--viewport" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--viewport=")
        };
        if let Some(spec) = spec {
            let (w, h) = spec.split_once(['x', 'X'])?;
            return Some((w.trim().parse().ok()?, h.trim().parse().ok()?));
        }
    }
    None
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
/// Mirrors `ui::App`'s cycle: sync → build_layout → compute_layout → apply_layout → draw.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    // Lazy image widgets (thumbnails, avatars, banners) resolve their decoded
    // image in `sync`, but the FIRST sync above ran before `build_layout`, when
    // those widgets didn't exist yet. In the live app the persistent tree
    // re-syncs every frame while images are pending, so a prewarmed image folds
    // in on a later frame; a single-frame shot must sync the now-built tree once
    // more so a prewarmed thumbnail/avatar actually paints instead of the
    // gradient/initials fallback.
    widget.sync(&mut state.doc, &state.root);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("tube: wrote shot to {}", out.display());
    Ok(())
}
