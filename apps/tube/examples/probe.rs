//! Tube diagnostics: probes of the live backends, offscreen stills from live data, and a scroll
//! benchmark. The app itself is the `tube` binary.

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
use ui::widget::DrawChild;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--probe-trending") {
        return run_probe_trending();
    }

    if let Some(id) = parse_flag_id(&args, "--probe-engine", "aqz-KE-bpKQ") {
        return run_probe_engine(&id);
    }

    if let Some((dir, query)) = parse_live_shots_arg(&args) {
        return run_live_shots(&dir, &query);
    }

    if let Some(query) = parse_probe_arg(&args) {
        return run_probe(&query);
    }

    if let Some(frames) = parse_flag_usize(&args, "--bench-scroll", 90) {
        let watch = args.iter().any(|a| a == "--watch");
        let loaded = args.iter().any(|a| a == "--loaded");
        return run_bench_scroll(frames, watch, loaded);
    }

    eprintln!(
        "usage: probe --probe [Q] | --probe-trending | --probe-engine [ID] | --live-shots DIR [Q] | --bench-scroll [FRAMES] [--watch [--loaded]]"
    );
    Ok(())
}

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

#[cfg(all(feature = "live", feature = "media"))]
#[allow(
    clippy::exit,
    reason = "a diagnostic CLI probe: an empty/malformed live result is reported \
              with a distinct machine-readable exit code, not folded into the generic \
              error path main() already uses for construction/network failures"
)]
fn run_probe_engine(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashSet;
    use ui::time::Duration;

    eprintln!("probe-engine: constructing the rustypipe backend…");
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("rustypipe backend: {e}"))?;
    let source = backend
        .play(id, None)
        .map_err(|said| said.unwrap_or_else(|| "no directly-playable stream resolved".into()))?;
    let (url, quality) = (source.url, source.quality);
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

#[cfg(all(feature = "live", feature = "media"))]
#[allow(
    clippy::too_many_lines,
    reason = "one diagnostic step per pipeline stage (backend construction, fetch, \
              decode, report); the length is the number of steps probed, not \
              accidental complexity in any one of them"
)]
fn run_live_shots(dir: &str, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use tube::backend::{BackendResult, YtBackend, yt_thumb_url};
    use tube::model::{DataSource, Route, TubeModel, Video};

    let out_dir = std::path::Path::new(dir);
    if let Ok((parent, name)) = tilo_files::file_at(out_dir) {
        let _ = tilo_files::blocking::create_dir_all(&parent, &name);
    }
    let backend = tube::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("rustypipe backend: {e}"))?;

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
    let resolved = backend
        .play(watch_id, None)
        .ok()
        .map(|s| (s.url, s.quality));
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
                "live-shots[watch]: no stream resolved — skipped the watch still (reported \
                 honestly, not faked)."
            );
        }
    }

    let prewarm = |store: &ui::ImageStore, urls: &[String]| {
        for u in urls {
            if !u.is_empty() {
                store.get(u);
            }
        }
    };

    println!("live-shots[search]: rustypipe search for {query:?}…");
    match backend.search_items(query) {
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

            let resolved_pl = items.playlists.iter().find_map(|p| {
                println!("live-shots[playlist]: fetching {}…", p.id);
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

type ScrollBenchPass =
    Result<(Vec<u128>, Vec<u128>, Vec<u128>, Vec<u128>, bool), Box<dyn std::error::Error>>;

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

        solve(&mut taffy, &node_map, &mut tr)?;
        widget.apply_layout(&taffy, child, Point::ORIGIN);
        {
            let mut scene = Scene::new();
            widget.draw_child(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            widget.mark_drawn();
        }

        let mods = winit::event::Modifiers::default();
        let mut sync = Vec::with_capacity(frames);
        let mut compute = Vec::with_capacity(frames);
        let mut apply = Vec::with_capacity(frames);
        let mut draw = Vec::with_capacity(frames);
        let mut ever_reposition_only = false;

        for _ in 0..frames {
            let t = ui::time::now();
            widget.sync(&state.doc, &state.root);
            sync.push(t.elapsed().as_micros());

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
            widget.draw_child(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
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

#[cfg(all(feature = "live", feature = "media"))]
fn checksum(rgba: &[u8]) -> u64 {
    let mut h: u64 = 1_469_598_103_934_665_603; // FNV-1a offset basis
    for &b in rgba.iter().step_by(97) {
        h ^= u64::from(b);
        h = h.wrapping_mul(1_099_511_628_211);
    }
    h
}
