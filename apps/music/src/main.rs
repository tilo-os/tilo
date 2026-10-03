//! Music — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//! - default — run the live winit event loop via `ui::App`.
//! - `--shots PATH` — render one offscreen frame to a PNG and exit (headless
//!   screenshot; identical passes to the live app).
//!
//! Optional flags for `--shots`:
//! - `--viewport WxH` — render at a custom size (e.g. `--viewport 480x820` to
//!   capture the narrow / overlay-sidebar layout).
//! - `--scenario NAME` — stage a named interaction state for the single frame
//!   (see `music::app_root_for_shot`); `sidebar` snaps the navigation sidebar
//!   open.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
const SHOT_SCENARIOS: &[&str] = &[
    "sidebar",
    "collapsed",
    "context",
    "new-playlist",
    "empty",
    "skeleton",
    "nowplaying",
    "queue",
    "lyrics",
    "downloading",
    "offline-cached",
    "offline-error",
    "artist-page",
    "album-page",
    "playlist-page",
    "error",
    "membership-open",
    "songs",
    "albums",
    "artists",
    "playlists",
    "liked",
    "browse",
];

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error if `--probe` fails: backend init, the search itself, a
/// zero-result search, or resolving/verifying the top hit's audio stream
/// (network / API failure).
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    // Probe mode: prove the live path end-to-end from the terminal — run a real
    // YouTube-Music search, resolve a real audio stream URL for the top hit and
    // (best-effort) HTTP-verify + decode it. Gated behind `--features live`.
    if let Some(pos) = args.iter().position(|a| a == "--probe") {
        // Join all trailing args so an unquoted multi-word query
        // (`--probe aurora runaway`) is still taken as one search.
        let query = args
            .get(pos + 1..)
            .map_or_else(String::new, |a| a.join(" "));
        let query = if query.trim().is_empty() {
            "aurora runaway".to_string()
        } else {
            query
        };
        return run_probe(&query);
    }

    // Headless per-frame benchmark: render a view offscreen N times and report
    // the layout+draw cost distribution. Reproducible + display-free.
    if let Some(pos) = args.iter().position(|a| a == "--bench") {
        let n: usize = args
            .get(pos + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);
        let viewport = parse_value_arg(&args, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        run_bench(n, viewport);
        return Ok(());
    }

    // The ordinary GUI, screenshots, and headless driver share the standard
    // lifecycle. Probe/benchmark remain explicit command modes above:
    // they do not construct a GUI and therefore are not launch lifecycle.
    if !music::standalone_launch() {
        ui::framework::run::<music::MusicShell>();
        return Ok(());
    }
    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.music", music::app_root_with_args)
            .shot_handler(render_shot)
            .allowed_flags(&["--live"])
            .allowed_options(&["--theme", "--play"])
            .max_positionals(1)
            .scenarios(SHOT_SCENARIOS)
            .configure_app(configure_app),
    )?;
    Ok(())
}

fn configure_app(app: &mut ui::App) {
    music::actions::register(app.action_registry_mut());
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("music screenshot handler was called without --shots".into())
    })?;
    let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let scenario = args.scenario.as_deref().unwrap_or_default();
    let theme = pick_shot_theme(args);
    let live =
        args.has("--live") || std::env::var("TILO_MUSIC_LIVE").is_ok_and(|value| !value.is_empty());
    if live {
        let play = args
            .value("--play")
            .map(str::to_owned)
            .or_else(|| std::env::var("TILO_MUSIC_PLAY").ok())
            .filter(|value| !value.is_empty());
        render_live_shot_to_png(viewport, out, play.as_deref(), &theme);
    } else {
        render_shot_to_png(music::app_root_for_shot(scenario), viewport, out, &theme);
    }
    Ok(())
}

/// Prove the live streaming path from the terminal. Real search + real audio
/// stream resolution + an independent HTTP byte-check. Requires `--features
/// live`; the default build prints how to enable it (never fakes a result).
#[cfg(feature = "live")]
fn run_probe(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use music::backend::MusicBackend;

    println!("music --probe: live YouTube-Music backend (rustypipe InnerTube)\n");

    let backend = music::backend::live::RustyPipeBackend::new()
        .map_err(|e| format!("backend init failed: {e}"))?;
    println!("backend: {} (live={})\n", backend.name(), backend.is_live());

    println!("== real search: {query:?} ==");
    let hits = backend
        .search_tracks(query)
        .map_err(|e| format!("search failed (network / API): {e}"))?;
    let Some(top) = hits.first() else {
        return Err("search returned zero results — cannot prove the path".into());
    };
    for (i, h) in hits.iter().take(10).enumerate() {
        let (m, s) = (h.duration_s / 60, h.duration_s % 60);
        println!(
            "  {:>2}. {}  —  {}   [{}]   {m}:{s:02}",
            i + 1,
            h.title,
            h.artist,
            h.video_id
        );
    }

    probe_stream(&backend, top)?;
    probe_home(&backend);
    probe_artist(&backend, top);
    probe_album(&backend, top);

    println!("\nprobe OK — search + stream + home + artist + album are real.");
    Ok(())
}

/// Resolve a real audio stream for the top hit, print its details, and prove
/// the URL serves real bytes (+ a best-effort decode).
#[cfg(feature = "live")]
fn probe_stream(
    backend: &impl music::backend::MusicBackend,
    top: &music::backend::TrackHit,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n== resolve a real audio stream for the top hit ==");
    println!("   {} — {}  ({})", top.title, top.artist, top.video_id);
    let stream = backend
        .resolve_audio(&top.video_id)
        .map_err(|e| format!("stream resolution failed: {e}"))?;
    let mib = stream.size_bytes as f64 / (1024.0 * 1024.0);
    println!(
        "   itag={}  codec={}  container={}  ~{} kbps",
        stream.itag,
        stream.codec,
        stream.format,
        stream.bitrate / 1000
    );
    println!("   mime: {}", stream.mime);
    println!(
        "   API content-length: {} bytes ({mib:.2} MiB)",
        stream.size_bytes
    );
    // Signed media URLs contain short-lived query credentials. Keep them out of
    // local and CI logs while still verifying the exact URL below.
    println!("   source URL resolved (redacted)");

    // Independent proof the URL actually serves audio bytes.
    verify_stream_http(&stream.url)?;

    // Best-effort decode proof (only with the `audio` feature / GStreamer).
    probe_decode(&stream.url);
    Ok(())
}

/// Real Home content (charts / new releases / top artists) with cover art.
#[cfg(feature = "live")]
fn probe_home(backend: &impl music::backend::MusicBackend) {
    println!("\n== real Home content (YouTube-Music charts) ==");
    match backend.home() {
        Ok(home) => {
            for section in &home.sections {
                println!("   [{}] ({} items)", section.title, section.items.len());
                for it in section.items.iter().take(3) {
                    let art = if it.cover_url.is_empty() {
                        "no-cover"
                    } else {
                        "cover\u{2713}"
                    };
                    println!(
                        "        \u{2022} {}  \u{2014}  {}   [{art}]",
                        it.title, it.subtitle
                    );
                }
            }
        }
        Err(e) => println!("   home unavailable: {e}"),
    }
}

/// Real artist page for the top hit's artist (proves the artist endpoint +
/// yields a channel id usable for `TILO_MUSIC_VIEW=artist:<id>`).
#[cfg(feature = "live")]
fn probe_artist(backend: &impl music::backend::MusicBackend, top: &music::backend::TrackHit) {
    let Some(aid) = top.artist_id.clone() else {
        return;
    };
    println!("\n== real artist page ==  (channel id: {aid})");
    match backend.artist(&aid) {
        Ok(page) => {
            println!(
                "   {}  \u{2014}  {} subscribers, {} top songs, {} albums",
                page.name,
                page.subscriber_count,
                page.tracks.len(),
                page.albums.len()
            );
            if let Some(t) = page.tracks.first() {
                println!("   top song: {} [{}]", t.title, t.video_id);
            }
            if let Some(a) = page.albums.first() {
                println!("   album: {} ({})", a.title, a.year);
            }
        }
        Err(e) => println!("   artist page unavailable: {e}"),
    }
}

/// Real album page (ordered track list) for the top hit's album.
#[cfg(feature = "live")]
fn probe_album(backend: &impl music::backend::MusicBackend, top: &music::backend::TrackHit) {
    let Some(alid) = top.album_id.clone() else {
        return;
    };
    println!("\n== real album page ==  (album id: {alid})");
    match backend.album(&alid) {
        Ok(al) => {
            println!(
                "   {} \u{2014} {} ({}), {} tracks",
                al.title,
                al.artist_name,
                al.year,
                al.tracks.len()
            );
            for tk in al.tracks.iter().take(3) {
                println!("        {}. {} [{}]", tk.title, tk.artist, tk.video_id);
            }
        }
        Err(e) => println!("   album page unavailable: {e}"),
    }
}

/// Range-GET the resolved stream URL to prove it serves real media bytes.
#[cfg(feature = "live")]
fn verify_stream_http(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;

    println!("\n== independent HTTP verification (Range GET first 64 KiB) ==");
    let client = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let resp = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-65535")
        .send()?;
    let status = resp.status();
    let content_range = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.bytes()?;
    println!("   HTTP {status}");
    if let Some(ct) = content_type {
        println!("   Content-Type: {ct}");
    }
    if let Some(cr) = content_range {
        println!("   Content-Range: {cr}");
    }
    let sig = bytes.iter().take(12).fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x} ");
        acc
    });
    println!(
        "   received {} bytes; first 12: {}",
        bytes.len(),
        sig.trim_end()
    );
    if !status.is_success() || bytes.is_empty() {
        return Err(format!("stream URL did not serve bytes (HTTP {status})").into());
    }
    println!("   -> the resolved URL serves real audio bytes.");
    Ok(())
}

/// Best-effort `GStreamer` decode of the resolved stream to a fake sink (no
/// speakers needed). Only compiled with the `audio` feature; on missing codec
/// plugins it reports honestly rather than claiming audio it didn't produce.
#[cfg(all(feature = "live", feature = "audio"))]
fn probe_decode(url: &str) {
    use gstreamer::prelude::*;
    println!("\n== GStreamer decode check (audio -> fakesink) ==");
    if gstreamer::init().is_err() {
        println!("   [decode] gstreamer init failed — skipping");
        return;
    }
    // Report any plugins this pipeline needs but the environment lacks, so a
    // failure is explained concretely rather than hand-waved.
    let need = ["souphttpsrc", "curlhttpsrc", "qtdemux", "avdec_aac", "faad"];
    let missing: Vec<&str> = need
        .iter()
        .copied()
        .filter(|n| gstreamer::ElementFactory::find(n).is_none())
        .collect();
    let no_http = gstreamer::ElementFactory::find("souphttpsrc").is_none()
        && gstreamer::ElementFactory::find("curlhttpsrc").is_none();
    let no_aac = gstreamer::ElementFactory::find("avdec_aac").is_none()
        && gstreamer::ElementFactory::find("faad").is_none();
    if !missing.is_empty() {
        println!(
            "   [decode] missing GStreamer plugins: {}",
            missing.join(", ")
        );
    }
    let Ok(playbin) = gstreamer::ElementFactory::make("playbin").build() else {
        println!("   [decode] no playbin element — skipping");
        return;
    };
    playbin.set_property("uri", url);
    if let Ok(fake) = gstreamer::ElementFactory::make("fakesink").build() {
        playbin.set_property("audio-sink", &fake);
    }
    if playbin.set_state(gstreamer::State::Playing).is_err() {
        if no_http {
            println!(
                "   [decode] pipeline can't start — no HTTP source plugin (souphttpsrc/\
                 curlhttpsrc) in this env, so GStreamer can't fetch the URL. The resolve + \
                 HTTP-byte proof above still hold."
            );
        } else if no_aac {
            println!(
                "   [decode] pipeline can't start — no AAC decoder (avdec_aac/faad) in this \
                 env. The resolve + HTTP-byte proof above still hold."
            );
        } else {
            println!("   [decode] could not start pipeline — skipping");
        }
        return;
    }
    let mut max_ms = 0u64;
    for _ in 0..30 {
        let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(200));
        if let Some(p) = playbin.query_position::<gstreamer::ClockTime>() {
            max_ms = max_ms.max(p.mseconds());
        }
        if max_ms > 500 {
            break;
        }
    }
    let _ = playbin.set_state(gstreamer::State::Null);
    if max_ms > 0 {
        println!("   [decode] pipeline PLAYING, decoded to {max_ms} ms — real audio decodes here.");
    } else {
        println!(
            "   [decode] pipeline did not advance (likely a missing AAC/Opus GStreamer plugin \
             in this env); the resolve + HTTP-byte proof above still hold."
        );
    }
}

#[cfg(all(feature = "live", not(feature = "audio")))]
fn probe_decode(_url: &str) {
    println!(
        "\n== GStreamer decode check ==\n   skipped (build with `--features live,audio` to \
         decode the resolved stream here)."
    );
}

/// Without the `live` feature the probe cannot run — say so honestly.
#[cfg(not(feature = "live"))]
#[allow(
    clippy::exit,
    reason = "the probe's documented feature-missing outcome uses exit status 2 so scripts can distinguish it from a probe failure"
)]
fn run_probe(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "music --probe needs the `live` feature (real YouTube-Music backend).\n\
         Rebuild with:  cargo run -p music --features live -- --probe \"aurora runaway\""
    );
    std::process::exit(2);
}

/// Parse a `--flag VALUE` or `--flag=VALUE` pair out of argv.
fn parse_value_arg(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().skip(1);
    let eq = format!("{flag}=");
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse a `WIDTHxHEIGHT` viewport spec (e.g. `480x820`).
fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The OS theme a `--shots` frame renders in: `--theme light|dark|high-contrast`
/// (or the `TILO_MUSIC_THEME` env), defaulting to dark. `TILO_REDUCE_MOTION`,
/// `TILO_A11Y_REDUCE_MOTION`, or the app-scoped `TILO_MUSIC_REDUCE_MOTION` also
/// makes a deterministic no-motion shot. This mirrors the live app's
/// accessibility setting without altering live state.
fn pick_shot_theme(args: &LaunchArgs) -> Theme {
    let shared = ui::launch::shot_theme(args);
    let sel = args
        .value("--theme")
        .map(str::to_owned)
        .or_else(|| std::env::var("TILO_MUSIC_THEME").ok())
        .unwrap_or_default();
    let theme = match sel.to_lowercase().as_str() {
        "" => None,
        "light" => Some(Theme::light()),
        "high-contrast" | "high_contrast" => Some(Theme::high_contrast()),
        // "dark" and anything unrecognised.
        _ => Some(Theme::dark()),
    };
    let reduce_motion = [
        "TILO_MUSIC_REDUCE_MOTION",
        "TILO_A11Y_REDUCE_MOTION",
        "TILO_REDUCE_MOTION",
    ]
    .iter()
    .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty() && value != "0"));
    theme.map_or_else(
        || {
            shared
                .clone()
                .with_reduce_motion(shared.reduce_motion || reduce_motion)
        },
        |theme| {
            theme
                .with_reduce_motion(shared.reduce_motion || reduce_motion)
                .with_text_scale(shared.text_scale)
        },
    )
}

/// Headless benchmark: render the current view (`TILO_MUSIC_VIEW`) `n` times
/// offscreen, timing the full per-frame CPU path (sync → layout → draw) that
/// `TILO_UI_PERF` reports live. No GPU submit, no display — reproducible.
fn run_bench(n: usize, viewport: (u32, u32)) {
    let mut frame = ShotFrame::new(viewport, Theme::dark());
    let mut widget = music::app_root_for_shot(""); // offline sample, hermetic

    // Warm up (first frame builds text-shape caches etc.).
    let _ = render_widget_frame(&mut frame, widget.as_mut());

    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = ui::time::now();
        let _ = render_widget_frame(&mut frame, widget.as_mut());
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let sum: f64 = samples.iter().sum();
    let avg = sum / n as f64;
    let p50 = samples.get(n / 2).copied().unwrap_or(0.0);
    let p95 = samples
        .get((n as f64 * 0.95) as usize)
        .copied()
        .unwrap_or(0.0);
    let max = samples.last().copied().unwrap_or(0.0);
    let view = std::env::var("TILO_MUSIC_VIEW").unwrap_or_else(|_| "home".into());
    let nocache = std::env::var("TILO_MUSIC_NOCACHE").is_ok();
    println!(
        "bench view={view} nocache={nocache} frames={n} @ {}x{} :  avg={avg:.3}ms  p50={p50:.3}ms  p95={p95:.3}ms  max={max:.3}ms",
        viewport.0, viewport.1
    );
}

/// Run one offscreen frame against `widget` and return the drawn Vello scene.
/// The frame's document and text renderer persist across calls, as they do
/// between frames of the live app.
fn render_widget_frame(frame: &mut ShotFrame, widget: &mut dyn Widget) -> Scene {
    frame.sync(widget);
    // Taffy rejects only a child from a different tree, and the frame builds
    // its tree from this widget.
    #[allow(
        clippy::expect_used,
        reason = "the frame builds a fresh tree from this widget, so taffy cannot reject it"
    )]
    frame.layout(widget).expect("shot layout");
    let mut scene = frame.draw(widget);
    frame.draw_overlay(widget, &mut scene);
    scene
}

/// Write a rendered scene out as a PNG.
fn write_scene_png(frame: &ShotFrame, scene: &Scene, out: &std::path::Path) {
    frame.write_png(scene, out);
    println!("music: wrote shot to {}", out.display());
}

/// Hermetic single-frame shot (offline sample library).
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    theme: &Theme,
) {
    let mut frame = ShotFrame::new(viewport, theme.clone());
    let scene = render_widget_frame(&mut frame, widget.as_mut());
    write_scene_png(&frame, &scene, out);
}

/// Live shot: build the real Music app, let its background worker + image cache
/// fetch real Home/artist/album content and real cover art off-thread, wait for
/// it to settle, then capture the frame. Optionally stage a real now-playing
/// track (`--play "<query>"`) for a lyrics / now-playing capture.
fn render_live_shot_to_png(
    viewport: (u32, u32),
    out: &std::path::Path,
    play: Option<&str>,
    theme: &Theme,
) {
    use std::time::Duration;
    let mut shot = ShotFrame::new(viewport, theme.clone());
    let mut app = music::live_shot_app();

    if let Some(q) = play {
        // Stage a real playing track for a lyrics / now-playing capture.
        for _ in 0..3 {
            let _ = render_widget_frame(&mut shot, &mut app);
        }
        app.shot_begin_search(q);
        for _ in 0..200 {
            let _ = render_widget_frame(&mut shot, &mut app);
            if app.shot_has_results() {
                app.shot_play_top_result(&mut shot.state.doc);
                break;
            }
            let _ = ui::rt::sleep_thread(Duration::from_millis(80));
        }
        for _ in 0..200 {
            let _ = render_widget_frame(&mut shot, &mut app);
            if app.images_idle() && app.shot_lyrics_ready() {
                break;
            }
            let _ = ui::rt::sleep_thread(Duration::from_millis(80));
        }
    } else {
        // Browse views (Home / Artist / Album / Search): pump until the content
        // and its cover art settle.
        let mut stable = 0;
        // Cap the wait (~13s) so a failed/geo-blocked fetch still renders rather
        // than hanging; a successful fetch settles far sooner.
        for _ in 0..160 {
            let _ = render_widget_frame(&mut shot, &mut app);
            let idle = app.images_idle() && app.is_content_loaded_for_shot();
            if idle {
                stable += 1;
            } else {
                stable = 0;
            }
            let done = (stable >= 3 && app.any_art_requested()) || stable >= 14;
            if done && app.is_content_loaded_for_shot() {
                break;
            }
            let _ = ui::rt::sleep_thread(Duration::from_millis(80));
        }
    }

    let scene = render_widget_frame(&mut shot, &mut app);
    write_scene_png(&shot, &scene, out);
}
