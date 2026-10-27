//! Tube diagnostics: probes of the live backends, offscreen stills from
//! live data, and a scroll benchmark. The app itself is the `tube` binary.
//!
//! Run with `cargo run -p tube --example probe -- <mode>`:
//! - `--probe [Q]`: construct the live backend, run a `YouTube` search for
//!   `Q` (default "rust programming") + trending, print what came back.
//! - `--probe-trending`: the dedicated trending endpoint failing, then the
//!   composed "popular" feed (view-count-sorted search).
//! - `--probe-engine [ID]`: resolve a stream and decode moving frames through
//!   the same shared [`media::MediaEngine`] used by the watch page.
//! - `--probe-ytdlp [Q]`, `--probe-avatars [Q]`: the yt-dlp extractor and the
//!   creator-avatar pipeline.
//! - `--live-shots DIR [Q]`: offscreen stills from live data.
//! - `--bench-scroll [FRAMES] [--watch [--loaded]]`: time the per-frame passes
//!   of a scrolled Home grid or watch page.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
use std::collections::HashMap;

use taffy::prelude::*;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // `--probe-trending` — show the dedicated endpoint's error and the composed
    // popular feed it falls back to.
    if args.iter().any(|a| a == "--probe-trending") {
        return run_probe_trending();
    }

    // `--probe-engine [id]` — exercise playback through the shared media engine:
    // resolve a stream, open a `media::MediaEngine`, and decode a sequence of
    // frames, printing engine events and checksums.
    if let Some(id) = parse_flag_id(&args, "--probe-engine", "aqz-KE-bpKQ") {
        return run_probe_engine(&id);
    }

    // `--live-shots <dir> [query]` — render offscreen stills from live data:
    // search results with fetched thumbnails and a decoded watch frame.
    // Requires `--features "live media"`.
    if let Some((dir, query)) = parse_live_shots_arg(&args) {
        return run_live_shots(&dir, &query);
    }

    // `--probe-ytdlp [query]` — exercise the yt-dlp extractor: search + video +
    // channel (with avatar/banner urls). Requires `--features live`.
    if let Some(query) = parse_opt_query(&args, "--probe-ytdlp", "blender open movie") {
        return run_probe_ytdlp(&query);
    }

    // `--probe-avatars [query]` — diagnose the creator-avatar pipeline: what
    // channel id/name/avatar yt-dlp vs rustypipe report for the same search, the
    // by-id fold match rate, and whether a per-channel fetch returns +
    // decodes an avatar. Requires `--features live`.
    if let Some(query) = parse_opt_query(&args, "--probe-avatars", "blender open movie") {
        return run_probe_avatars(&query);
    }

    // `--probe [query]` — run a live backend query.
    if let Some(query) = parse_probe_arg(&args) {
        return run_probe(&query);
    }

    // `--bench-scroll [frames]` — headless scroll-performance benchmark. Times
    // the exact per-frame passes the live event loop runs (compute_layout /
    // apply_layout / draw) over a large Home grid, in two modes — the pre-fix
    // "always re-solve" baseline and the scroll-reposition fast path — and
    // prints the numbers. No window, no GPU present (matches the TILO_UI_PERF
    // main-thread breakdown, which excludes the render worker).
    if let Some(frames) = parse_flag_usize(&args, "--bench-scroll", 90) {
        // `--watch` benches the watch page (full-bleed player + a long comment
        // list) instead of the Home grid — the heaviest scroll scenario.
        let watch = args.iter().any(|a| a == "--watch");
        // `--loaded` (with `--watch`) loads a real decoded frame into the player
        // and pauses it, reproducing a paused-but-loaded watch page — and drives
        // `sync` each frame, so the per-frame engine cost is measured, not just
        // layout + draw.
        let loaded = args.iter().any(|a| a == "--loaded");
        return run_bench_scroll(frames, watch, loaded);
    }

    eprintln!(
        "usage: probe --probe [Q] | --probe-trending | --probe-engine [ID] | --probe-ytdlp [Q] | --probe-avatars [Q] | --live-shots DIR [Q] | --bench-scroll [FRAMES] [--watch [--loaded]]"
    );
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
            return Some(if rest.is_empty() {
                DEFAULT_Q.to_string()
            } else {
                rest.to_string()
            });
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
            return Some(if rest.is_empty() {
                default.to_string()
            } else {
                rest.to_string()
            });
        }
    }
    None
}

/// The rangeable Invidious proxy's stream URL and quality for `id`.
#[cfg(all(feature = "live", feature = "media"))]
fn resolve_proxied(id: &str) -> Option<(String, String)> {
    ui::rt::block_on(tube::backend::proxy::resolve_rangeable(id, None))
        .ok()
        .flatten()
        .map(|r| (r.url, r.quality))
}

/// `--probe-engine [id]`: exercise playback through the shared `media::MediaEngine`
/// — the exact path the watch page streams through. Resolves a directly-playable
/// stream (direct extractor first, then the rangeable Invidious proxy), opens the
/// engine, plays, and decodes a sequence of frames; prints engine events and a
/// per-frame checksum. Exits non-zero if no frame decodes.
#[cfg(all(feature = "live", feature = "media"))]
#[allow(
    clippy::exit,
    reason = "a diagnostic CLI probe: an empty/malformed live result is reported \
              with a distinct machine-readable exit code, not folded into the generic \
              error path main() already uses for construction/network failures"
)]
fn run_probe_engine(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashSet;
    use tube::backend::{YtBackend, ytdlp::YtDlpBackend};
    use ui::time::Duration;

    eprintln!("probe-engine: constructing the yt-dlp backend…");
    let backend = YtDlpBackend::new().map_err(|e| format!("yt-dlp backend: {e}"))?;
    let (url, quality) = backend
        .resolve_playback_source(id, None)
        .ok()
        .map(|s| (s.url, s.quality))
        .or_else(|| resolve_proxied(id))
        .ok_or("no directly-playable stream resolved")?;
    println!(
        "probe-engine: resolved {quality}; url(host)={}",
        short_url(&url)
    );

    let mut engine = media::MediaEngine::new();
    println!(
        "probe-engine: engine real_decode={} (decode compiled={})",
        engine.is_real_decode(),
        media::MediaEngine::is_decode_compiled()
    );
    engine.open(media::Source::Remote { url });
    engine.play();

    let deadline = ui::time::now() + Duration::from_secs(20);
    let mut sums: HashSet<u64> = HashSet::new();
    let mut distinct = 0u64;
    let mut errored = false;
    while ui::time::now() < deadline {
        for ev in engine.poll() {
            match ev {
                media::MediaEvent::Error(e) => {
                    eprintln!("probe-engine: ENGINE ERROR: {e}");
                    errored = true;
                }
                media::MediaEvent::DurationKnown { ms } => {
                    println!("probe-engine: duration known = {ms} ms");
                }
                media::MediaEvent::Buffering { percent } if percent % 25 == 0 => {
                    println!("probe-engine: buffering {percent}%");
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
        let _ = ui::rt::sleep_thread(Duration::from_millis(15));
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
    println!(
        "probe-engine: REAL playback through the media engine — {distinct} distinct pictures."
    );
    Ok(())
}

#[cfg(not(all(feature = "live", feature = "media")))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the disabled-feature probe keeps the same fallible signature as the live implementation"
)]
fn run_probe_engine(_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe-engine: needs `--features \"live media\"` (real resolve + media-engine decode)."
    );
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
            return Some(if rest.is_empty() {
                default.to_string()
            } else {
                rest.to_string()
            });
        }
    }
    None
}

/// `--probe-trending`: show `YouTube`'s dedicated trending endpoint failing,
/// then the composed popular feed (view-count-sorted search) that replaces it.
#[cfg(feature = "live")]
#[allow(
    clippy::exit,
    reason = "a diagnostic CLI probe: an empty/malformed live result is reported \
              with a distinct machine-readable exit code, not folded into the generic \
              error path main() already uses for construction/network failures"
)]
fn run_probe_trending() -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    const SEEDS: [&str; 6] = ["news", "music", "gaming", "technology", "sports", "science"];

    eprintln!("probe-trending: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;

    println!("probe-trending: calling the dedicated trending endpoint (FEtrending)…");
    match backend.trending() {
        Ok(BackendResult::Videos(vs)) if !vs.is_empty() => {
            println!("  REAL trending returned {} videos:", vs.len());
            for (i, v) in vs.iter().take(15).enumerate() {
                println!(
                    "  {:>2}. [{}] {} — {}",
                    i + 1,
                    v.id,
                    v.title,
                    v.channel_name
                );
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
    let mut total = 0usize;
    for seed in SEEDS {
        match backend.popular_search(seed) {
            Ok(BackendResult::Videos(vs)) if !vs.is_empty() => {
                total += vs.len();
                if let Some(top) = vs.first() {
                    println!(
                        "  {seed:>10}: {} popular videos; top: [{}] {} ({} views)",
                        vs.len(),
                        top.id,
                        top.title,
                        top.views
                    );
                }
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
#[allow(
    clippy::unnecessary_wraps,
    reason = "the disabled-feature probe keeps the same fallible signature as the live implementation"
)]
fn run_probe_trending() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "probe-trending: built WITHOUT the `live` feature — rebuild with \
         `--features live` to hit real YouTube."
    );
    Ok(())
}

/// `--live-shots <dir> [query]`: render REAL offscreen stills — a search page
/// with real `YouTube` results + fetched thumbnails, and a watch page with a real
/// frame decoded through the shared media engine from a rangeable proxy stream.
#[cfg(all(feature = "live", feature = "media"))]
#[allow(
    clippy::too_many_lines,
    reason = "one diagnostic step per pipeline stage (backend construction, fetch, \
              decode, report); the length is the number of steps probed, not \
              accidental complexity in any one of them"
)]
fn run_live_shots(dir: &str, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use tube::backend::{BackendResult, YtBackend, yt_thumb_url, ytdlp::YtDlpBackend};
    use tube::model::{DataSource, Route, TubeModel, Video};

    let out_dir = std::path::Path::new(dir);
    if let Ok((parent, name)) = tilo_files::file_at(out_dir) {
        let _ = tilo_files::blocking::create_dir_all(&parent, &name);
    }
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
        .resolve_playback_source(watch_id, None)
        .ok()
        .map(|s| (s.url, s.quality))
        .or_else(|| resolve_proxied(watch_id));
    match resolved {
        Some((url, quality)) => {
            println!("  resolved {quality} stream; decoding a real frame via the media engine…");
            let mut wmodel = TubeModel::default();
            wmodel.ensure_channel_stub(&video.channel_id, &video.channel_name);
            wmodel.upsert_video(video.clone());
            wmodel.data_source = DataSource {
                live: true,
                label: "live".into(),
            };
            // Fetch the uploader's real avatar so the watch-page channel row shows
            // a real photo, and prewarm it into a store.
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
            render_shot_to_png(
                Box::new(watch_app) as Box<dyn Widget>,
                (1280, 860),
                &watch_png,
            )?;
            println!("  wrote {}", watch_png.display());
        }
        None => {
            eprintln!(
                "live-shots[watch]: no Invidious proxy served bytes and no direct URL resolved \
                 — skipped the watch still (reported honestly, not faked)."
            );
        }
    }

    // ---- Search / home / playlist pages with thumbnails + avatars ----
    //
    // Use rustypipe's search: it returns videos (with creator avatars),
    // playlists AND creator rows in one call — exactly the enrichment the live
    // app folds into the yt-dlp results, so these stills cover avatars, the
    // Video·Playlist·Creator filter, and playlists end to end.
    let rp = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("rustypipe backend: {e}"))?;

    // Prewarm every thumbnail + avatar into one shared store so both the search
    // and home stills paint fetched images.
    let prewarm = |store: &ui::ImageStore, urls: &[String]| {
        for u in urls {
            if !u.is_empty() {
                store.get(u);
            }
        }
    };

    println!("live-shots[search]: rustypipe search for {query:?}…");
    match rp.search_items(query) {
        Ok(items)
            if !(items.videos.is_empty()
                && items.playlists.is_empty()
                && items.channels.is_empty()) =>
        {
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
                    if smodel
                        .channel(&v.channel_id)
                        .is_none_or(|c| c.avatar_url.is_empty())
                    {
                        *freq.entry(v.channel_id.clone()).or_insert(0) += 1;
                    }
                }
                let mut ranked: Vec<(String, usize)> = freq.into_iter().collect();
                ranked.sort_by_key(|r| std::cmp::Reverse(r.1));
                for (cid, _) in ranked.into_iter().take(5) {
                    if let Ok(BackendResult::Channel { channel, .. }) = backend.channel(&cid) {
                        smodel.adopt_channel_avatar(
                            &channel.id,
                            &channel.name,
                            &channel.avatar_url,
                        );
                        println!(
                            "  fetched avatar for {:?}: {}",
                            channel.name,
                            short_url(&channel.avatar_url)
                        );
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
            smodel.data_source = DataSource {
                live: true,
                label: "live".into(),
            };

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
            println!(
                "  prewarming {} thumbnails + avatars…",
                urls.iter().filter(|u| !u.is_empty()).count()
            );
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
                    if ch.avatar_url.is_empty() {
                        "<EMPTY>"
                    } else {
                        "set"
                    },
                    if decoded { "DECODED" } else { "miss" },
                );
            }

            let search_app = tube::TubeApp::new()
                .with_viewport(1280.0, 860.0)
                .with_model_for_test(smodel.clone())
                .with_image_store(store.clone());
            let search_png = out_dir.join("search.png");
            render_shot_to_png(
                Box::new(search_app) as Box<dyn Widget>,
                (1280, 860),
                &search_png,
            )?;
            println!("  wrote {}", search_png.display());

            let mut hmodel = smodel.clone();
            hmodel.route = Route::Home;
            let home_app = tube::TubeApp::new()
                .with_viewport(1280.0, 860.0)
                .with_model_for_test(hmodel)
                .with_image_store(store);
            let home_png = out_dir.join("home.png");
            render_shot_to_png(
                Box::new(home_app) as Box<dyn Widget>,
                (1280, 860),
                &home_png,
            )?;
            println!("  wrote {}", home_png.display());

            // ---- Playlist page: yt-dlp resolves playlists reliably (rustypipe's
            // current parser does not); try each search-result playlist until one
            // resolves (some listed playlists are stale/empty). ----
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
                pmodel.route = Route::Playlist(pid);
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
                eprintln!(
                    "live-shots[playlist]: no listed playlist resolved — skipping playlist still."
                );
            }
        }
        Ok(_) => eprintln!(
            "live-shots[search]: empty result set — skipping search/home/playlist stills."
        ),
        Err(e) => eprintln!("live-shots[search]: rustypipe search failed: {e}"),
    }

    println!("live-shots: done ({})", out_dir.display());
    Ok(())
}

#[cfg(not(all(feature = "live", feature = "media")))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the disabled-feature shot path keeps the same fallible signature as the live implementation"
)]
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
        format!("{}…", head.get(..72).unwrap_or(head))
    } else {
        head.to_string()
    }
}

/// `--probe-ytdlp [query]`: prove the yt-dlp extractor end-to-end — search,
/// video metadata, and a channel page with its real avatar + banner URLs.
#[cfg(feature = "live")]
#[allow(
    clippy::exit,
    reason = "a diagnostic CLI probe: an empty/malformed live result is reported \
              with a distinct machine-readable exit code, not folded into the generic \
              error path main() already uses for construction/network failures"
)]
fn run_probe_ytdlp(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    eprintln!("probe-ytdlp: constructing the yt-dlp backend…");
    let backend =
        tube::backend::ytdlp::YtDlpBackend::new().map_err(|e| format!("yt-dlp backend: {e}"))?;
    println!("probe-ytdlp: backend = {}", backend.name());

    println!("probe-ytdlp: searching YouTube for {query:?} …");
    let (first_id, first_channel) = if let BackendResult::Videos(vs) = backend.search(query)? {
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
        let Some(top) = vs.first() else {
            eprintln!("probe-ytdlp: EMPTY result set — reached yt-dlp but nothing parsed.");
            std::process::exit(2);
        };
        (top.id.clone(), top.channel_id.clone())
    } else {
        println!("probe-ytdlp: unexpected result variant");
        return Ok(());
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
            println!(
                "    upload [{}] {} — thumb={}",
                u.id,
                u.title,
                short_url(&u.thumbnail_url)
            );
        }
    }
    Ok(())
}

#[cfg(not(feature = "live"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the disabled-feature probe keeps the same fallible signature as the live implementation"
)]
fn run_probe_ytdlp(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("probe-ytdlp: built WITHOUT `live` — rebuild with `--features live`.");
    Ok(())
}

/// `--probe-avatars [query]`: diagnose the creator-avatar pipeline end to end.
#[cfg(feature = "live")]
#[allow(
    clippy::too_many_lines,
    reason = "one diagnostic step per pipeline stage (backend construction, fetch, \
              decode, report); the length is the number of steps probed, not \
              accidental complexity in any one of them"
)]
fn run_probe_avatars(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;
    use tube::backend::{BackendResult, YtBackend, ytdlp::YtDlpBackend};

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
            if v.channel_avatar_url.is_empty() {
                "<EMPTY>"
            } else {
                "present"
            }
        );
    }
    for v in &yt_videos {
        let e = yt_channel_freq
            .entry(v.channel_id.clone())
            .or_insert_with(|| (v.channel_name.clone(), 0));
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
            if v.channel_avatar_url.is_empty() {
                "<none>".into()
            } else {
                short_url(&v.channel_avatar_url)
            }
        );
        if !v.channel_id.is_empty() && !v.channel_avatar_url.is_empty() {
            rp_avatars
                .entry(v.channel_id.clone())
                .or_insert_with(|| v.channel_avatar_url.clone());
        }
    }
    for c in &items.channels {
        println!(
            "  creator id={:?} name={:?} avatar={}",
            c.id,
            c.name,
            if c.avatar_url.is_empty() {
                "<none>".into()
            } else {
                short_url(&c.avatar_url)
            }
        );
        if !c.id.is_empty() && !c.avatar_url.is_empty() {
            rp_avatars
                .entry(c.id.clone())
                .or_insert_with(|| c.avatar_url.clone());
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
    let matched_name = yt_videos
        .iter()
        .filter(|v| rp_names.contains_key(&norm(&v.channel_name)))
        .count();
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
                if channel.avatar_url.is_empty() {
                    "<EMPTY>".into()
                } else {
                    short_url(&channel.avatar_url)
                }
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
                if channel.avatar_url.is_empty() {
                    "<EMPTY>".into()
                } else {
                    short_url(&channel.avatar_url)
                }
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
        if b.get(0..3) == Some(b"\xFF\xD8\xFF".as_slice()) {
            "JPEG"
        } else if b.get(0..4) == Some(b"\x89PNG".as_slice()) {
            "PNG"
        } else if b.get(0..4) == Some(b"RIFF".as_slice())
            && b.get(8..12) == Some(b"WEBP".as_slice())
        {
            "WEBP"
        } else if b.get(0..4) == Some(b"GIF8".as_slice()) {
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
            None => println!(
                "  {label}: FETCH FAILED (store.decode={store_ok}) — {}",
                short_url(url)
            ),
        }
    };
    // The dominant channel's avatar from each source the app might use.
    if let Some(v) = items
        .videos
        .iter()
        .find(|v| !v.channel_avatar_url.is_empty())
    {
        probe_one("rustypipe video-item avatar", &v.channel_avatar_url);
    }
    if let Some(c) = items.channels.iter().find(|c| !c.avatar_url.is_empty()) {
        probe_one("rustypipe creator-row avatar", &c.avatar_url);
    }
    if let Some((cid, _)) = yt_channel_freq.iter().max_by_key(|(_, (_, n))| *n)
        && let Ok(BackendResult::Channel { channel, .. }) = yt.channel(cid)
    {
        probe_one("yt-dlp channel-page avatar", &channel.avatar_url);
    }
    // A known-good thumbnail for comparison (these always paint on the cards).
    if let Some(v) = items.videos.iter().find(|v| !v.thumbnail_url.is_empty()) {
        probe_one("video thumbnail (control)", &v.thumbnail_url);
    }
    Ok(())
}

#[cfg(not(feature = "live"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the disabled-feature probe keeps the same fallible signature as the live implementation"
)]
fn run_probe_avatars(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("probe-avatars: built WITHOUT `live` — rebuild with `--features live`.");
    Ok(())
}

/// Construct the live backend, query `YouTube`, and print the response.
/// Compiled only under `--features live`.
#[cfg(feature = "live")]
#[allow(
    clippy::exit,
    reason = "a diagnostic CLI probe: an empty/malformed live result is reported \
              with a distinct machine-readable exit code, not folded into the generic \
              error path main() already uses for construction/network failures"
)]
fn run_probe(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use tube::backend::{BackendResult, YtBackend};

    eprintln!("probe: constructing the live rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("live backend construction failed: {e}"))?;
    println!("probe: backend = {}", backend.name());

    println!("probe: searching YouTube for {query:?} …");
    let (first_id, first_channel) = match backend.search(query) {
        Ok(BackendResult::Videos(vs)) => {
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
            let Some(f) = vs.first() else {
                eprintln!(
                    "probe: EMPTY result set — network reached YouTube but no videos parsed."
                );
                std::process::exit(2);
            };
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
            vs.first().map_or("<none>", |v| v.title.as_str())
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
            uploads.first().map_or("<none>", |v| v.title.as_str())
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
            vs.first().map_or("<none>", |v| v.title.as_str())
        ),
        Ok(_) => {}
        Err(e) => eprintln!("  trending failed: {e}"),
    }
    Ok(())
}

/// Non-live builds have no real backend compiled in.
#[cfg(not(feature = "live"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the disabled-feature probe keeps the same fallible signature as the live implementation"
)]
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
    s.get(s.len() / 2).copied().unwrap_or(0)
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

/// One measured pass of [`run_bench_scroll`]'s inner `run_pass` closure: the
/// per-frame sync/compute/apply/draw timing series (µs) and whether the pass
/// used the scroll fast path.
type ScrollBenchPass =
    Result<(Vec<u128>, Vec<u128>, Vec<u128>, Vec<u128>, bool), Box<dyn std::error::Error>>;

/// `--bench-scroll [frames] [--watch]`: measure the per-frame main-thread cost
/// of scrolling either a large Home grid or the watch page (full-bleed player +
/// a long comment list), in two modes, and report the numbers.
#[allow(
    clippy::too_many_lines,
    reason = "one timed pipeline stage per widget-cycle phase (sync, compute, apply, draw), \
              measured twice (baseline + fast path); the length is the number of phases \
              instrumented, not accidental complexity in any one of them"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "w/h are the viewport size and s/c/a/d the per-phase (sync/compute/apply/draw) \
              timing series threaded through the report() closure"
)]
fn run_bench_scroll(
    frames: usize,
    watch: bool,
    loaded: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use tube::model::{Route, TubeModel};

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
            let Some(mut v) = base.get(i % base.len()).cloned() else {
                break;
            };
            v.id = format!("{}_{i}", v.id);
            v.id.clone_into(&mut v.seed);
            m.videos.push(v);
            i += 1;
        }
        m.route = Route::Home;
        m
    };

    // The heaviest scroll scenario: a full-bleed player over a long comment
    // list (the real content that makes the scroll tree deep). Grow the
    // chosen video's comments to a realistic count.
    let watch_model = || -> TubeModel {
        let mut m = tube::sample::model();
        let id = m
            .videos
            .iter()
            .find(|v| !v.comments.is_empty())
            .map_or_else(
                || m.videos.first().map(|v| v.id.clone()).unwrap_or_default(),
                |v| v.id.clone(),
            );
        if let Some(v) = m.videos.iter_mut().find(|v| v.id == id) {
            let base = v.comments.clone();
            let mut i = 0usize;
            while v.comments.len() < 60 && !base.is_empty() {
                let Some(next) = base.get(i % base.len()).cloned() else {
                    break;
                };
                v.comments.push(next);
                i += 1;
            }
        }
        m.route = Route::Watch(id);
        m
    };
    let big_model = || -> TubeModel { if watch { watch_model() } else { home_model() } };

    // One measured pass. `force_compute` = the pre-fix baseline (always re-solve
    // taffy every frame). `false` = the scroll fast path (apply_layout only when
    // nothing needs re-measuring). Returns (compute[], apply[], draw[]) in µs.
    let run_pass = |force_compute: bool| -> ScrollBenchPass {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let model = big_model();
        let watch_id = match &model.route {
            Route::Watch(id) => id.clone(),
            _ => String::new(),
        };
        let mut app = tube::TubeApp::new()
            .with_viewport(f64::from(w), f64::from(h))
            .with_model_for_test(model);
        // Load + pause a real (synthesized) frame so the bench measures the true
        // per-frame cost of a paused-but-loaded watch page.
        if watch && loaded && !watch_id.is_empty() {
            app = app.with_bench_loaded_watch(&watch_id);
        }
        let mut widget: Box<dyn Widget> = Box::new(app);
        let mut state = ui::State::new();
        widget.sync(&state.doc, &state.root);
        let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.new_with_children(
            Style {
                size: Size::from_lengths(w as f32, h as f32),
                ..Default::default()
            },
            &[child],
        )?;

        let solve = |taffy: &mut TaffyTree<()>,
                     node_map: &HashMap<NodeId, *mut dyn Widget>,
                     tr: &mut TextRenderer|
         -> Result<(), Box<dyn std::error::Error>> {
            taffy.compute_layout_with_measure(
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
            )?;
            Ok(())
        };

        // Initial full solve + layout + a warm draw (fills the Text/scene caches
        // so the timed frames measure steady-state scroll, not first-paint).
        solve(&mut taffy, &node_map, &mut tr)?;
        widget.apply_layout(&taffy, child, Point::ORIGIN);
        {
            let mut scene = Scene::new();
            widget.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
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
            let t = ui::time::now();
            widget.sync(&state.doc, &state.root);
            sync.push(t.elapsed().as_micros());

            // Real scroll: a wheel notch routed through the widget tree, exactly
            // as the live event loop delivers it.
            let wheel = ui::synthetic_input::mouse_wheel(0.0, -3.0);
            let mut handled = false;
            let mut focus_target = None;
            widget.handle_event(
                &wheel,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: ui::widget::Edits::lend(&mut state.doc),
                    path: &state.root,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: child,
                    taffy: &taffy,
                },
            );

            // The fast path skips the solve when nothing needs re-measuring.
            let needs_measure = widget.layout_needs_measure();
            if !needs_measure {
                ever_reposition_only = true;
            }
            if force_compute || needs_measure {
                let t = ui::time::now();
                solve(&mut taffy, &node_map, &mut tr)?;
                compute.push(t.elapsed().as_micros());
            } else {
                compute.push(0);
            }
            let t = ui::time::now();
            widget.apply_layout(&taffy, child, Point::ORIGIN);
            apply.push(t.elapsed().as_micros());

            let t = ui::time::now();
            let mut scene = Scene::new();
            let prev = presence.push_viewport(vello::kurbo::Rect::new(
                0.0,
                0.0,
                f64::from(w),
                f64::from(h),
            ));
            widget.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            presence.pop_viewport(prev);
            widget.mark_drawn();
            draw.push(t.elapsed().as_micros());
        }
        Ok((sync, compute, apply, draw, ever_reposition_only))
    };

    let report = |label: &str, s: &[u128], c: &[u128], a: &[u128], d: &[u128]| {
        let total: Vec<u128> = s
            .iter()
            .zip(c)
            .zip(a)
            .zip(d)
            .map(|(((s, c), a), d)| s + c + a + d)
            .collect();
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

    let loaded_note = if watch && loaded {
        " [loaded+paused player]"
    } else {
        ""
    };
    let scene_label = if watch {
        format!("watch page (player + 60 comments){loaded_note}")
    } else {
        format!("{CARDS} Home cards")
    };
    println!("tube scroll bench: {scene_label}, {w}x{h}, {frames} scroll frames\n");
    let (bs, bc, ba, bd, _) = run_pass(true)?;
    report("BASELINE (re-solve)", &bs, &bc, &ba, &bd);
    let (fs, fc, fa, fd, repos) = run_pass(false)?;
    report("FAST (reposition)", &fs, &fc, &fa, &fd);
    println!(
        "\n  scroll frames took the reposition-only fast path: {}",
        if repos {
            "yes (compute_layout skipped)"
        } else {
            "NO — investigate"
        }
    );
    let fast_total: Vec<u128> = fs
        .iter()
        .zip(&fc)
        .zip(&fa)
        .zip(&fd)
        .map(|(((s, c), a), d)| s + c + a + d)
        .collect();
    println!(
        "  FAST-path median frame (incl. sync): {:.2}ms  (sync med {:.2}ms)",
        median_us(&fast_total) as f64 / 1000.0,
        median_us(&fs) as f64 / 1000.0,
    );
    Ok(())
}

/// Render `widget` offscreen through the live window's frame lifecycle,
/// settled (animations finished, images loaded), and write it to `out` as PNG.
#[cfg(all(feature = "live", feature = "media"))]
fn render_shot_to_png(
    widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ui::headless::FrameHarness::new_with_theme(
        ui::State::new(),
        widget,
        viewport,
        ui::launch::shot_theme_from_env()?,
    );
    frame.settle_for_capture(ui::launch::CAPTURE_SETTLE_BOUND);
    frame.render_png(out);
    println!("tube: wrote shot to {}", out.display());
    Ok(())
}

/// FNV-1a over every 97th byte of a frame — enough to tell two decoded
/// frames apart without hashing megabytes per frame. Restored here because
/// `refactor(tube): use the shared media engine exclusively` removed the
/// definition and left the call site above, which left `tube`'s binary not
/// compiling.
#[cfg(all(feature = "live", feature = "media"))]
fn checksum(rgba: &[u8]) -> u64 {
    let mut h: u64 = 1_469_598_103_934_665_603; // FNV-1a offset basis
    for &b in rgba.iter().step_by(97) {
        h ^= u64::from(b);
        h = h.wrapping_mul(1_099_511_628_211);
    }
    h
}
