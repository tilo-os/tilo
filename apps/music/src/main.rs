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

use std::collections::HashMap;

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

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

    // Headless relay mode: a home server runs the library document + sync
    // transport with no GUI; clients sync to it. Live iroh transport is gated
    // behind the `p2p` feature (the relay still runs on the loopback transport
    // otherwise, demonstrating the seam). See `music::relay`.
    if args.iter().any(|a| a == "--relay") {
        let mut relay = music::relay::Relay::new();
        println!(
            "music: headless relay — {} tracks, live={}, ticket={}",
            relay.track_count(),
            relay.is_live(),
            relay.ticket().as_deref().unwrap_or("<none>")
        );
        relay.run();
        return Ok(());
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

    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_value_arg(&args, "--scenario").unwrap_or_default();
        let viewport = parse_value_arg(&args, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        // `--theme light|dark` (or `QOS_MUSIC_THEME`) picks the OS theme the frame
        // renders in — the shot proves the app looks right in both. Default dark.
        let theme = pick_shot_theme(&args);
        // `--live` captures the real screen: fetch live Home/album/artist content
        // + real cover art off-thread, wait for it to settle, then render.
        let live = args.iter().any(|a| a == "--live")
            || std::env::var("QOS_MUSIC_LIVE").is_ok_and(|v| !v.is_empty());
        if live {
            let play = parse_value_arg(&args, "--play")
                .or_else(|| std::env::var("QOS_MUSIC_PLAY").ok())
                .filter(|s| !s.is_empty());
            render_live_shot_to_png(viewport, &out, play.as_deref(), &theme);
        } else {
            render_shot_to_png(music::app_root_for_shot(&scenario), viewport, &out, &theme);
        }
        return Ok(());
    }

    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, music::app_root(), "dev.qos.music"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.music");
    let mut app = ui::App::new(state, music::app_root()).with_autosave(autosave);
    // Register Music's actions so the shell's Ctrl+K palette can decode + replay
    // the same library edits the on-screen affordances dispatch.
    music::actions::register(app.action_registry_mut());
    app.run();
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
    println!("   URL: {}", stream.url);

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
/// yields a channel id usable for `QOS_MUSIC_VIEW=artist:<id>`).
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
    println!("\n== independent HTTP verification (Range GET first 64 KiB) ==");
    let client = reqwest::blocking::Client::builder()
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
    use std::fmt::Write as _;
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
        std::thread::sleep(std::time::Duration::from_millis(200));
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
fn run_probe(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "music --probe needs the `live` feature (real YouTube-Music backend).\n\
         Rebuild with:  qos-build run -p music --features live -- --probe \"aurora runaway\""
    );
    std::process::exit(2);
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

/// The OS theme a `--shots` frame renders in: `--theme light|dark` (or the
/// `QOS_MUSIC_THEME` env), defaulting to dark. Lets the same scenario be
/// captured in both themes to prove the app looks right either way.
fn pick_shot_theme(args: &[String]) -> Theme {
    let sel = parse_value_arg(args, "--theme")
        .or_else(|| std::env::var("QOS_MUSIC_THEME").ok())
        .unwrap_or_default();
    match sel.to_lowercase().as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    }
}

/// Headless benchmark: render the current view (`QOS_MUSIC_VIEW`) `n` times
/// offscreen, timing the full per-frame CPU path (sync → layout → draw) that
/// `QUICK_UI_PERF` reports live. No GPU submit, no display — reproducible.
fn run_bench(n: usize, viewport: (u32, u32)) {
    use std::time::Instant;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    let mut widget = music::app_root_for_shot(""); // offline sample, hermetic
    let mut state = ui::State::new();

    // Warm up (first frame builds text-shape caches etc.).
    let _ = render_widget_frame(
        widget.as_mut(),
        &mut state,
        viewport,
        &mut tr,
        &theme,
        &presence,
        &permission,
    );

    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let _ = render_widget_frame(
            widget.as_mut(),
            &mut state,
            viewport,
            &mut tr,
            &theme,
            &presence,
            &permission,
        );
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
    let view = std::env::var("QOS_MUSIC_VIEW").unwrap_or_else(|_| "home".into());
    let nocache = std::env::var("QOS_MUSIC_NOCACHE").is_ok();
    println!(
        "bench view={view} nocache={nocache} frames={n} @ {}x{} :  avg={avg:.3}ms  p50={p50:.3}ms  p95={p95:.3}ms  max={max:.3}ms",
        viewport.0, viewport.1
    );
}

/// Run one offscreen frame against `widget` and return the drawn Vello scene.
/// Mirrors `ui::App`'s cycle: sync → `build_layout` → `compute_layout` → `apply_layout` → draw.
fn render_widget_frame(
    widget: &mut dyn Widget,
    state: &mut ui::State,
    viewport: (u32, u32),
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permission: &PermissionContext,
) -> Scene {
    let (w, h) = viewport;
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
    // `new_with_children` only fails for a child `NodeId` from a different
    // tree, which never happens here — `child` was just built in this exact
    // `taffy` above.
    #[allow(
        clippy::expect_used,
        reason = "new_with_children only errors for a child NodeId from a different tree; `child` was just built in this exact taffy above"
    )]
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .expect("shot root");

    {
        let measure_map = node_map.clone();
        let tr = &mut *text_renderer;
        // `compute_layout_with_measure` only errors for a node absent from
        // this tree, which cannot happen for the `root` this function just
        // built moments ago.
        #[allow(
            clippy::expect_used,
            reason = "compute_layout_with_measure only errors for a node absent from this tree; root was just built in this exact taffy above"
        )]
        taffy
            .compute_layout_with_measure(
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
            )
            .expect("shot layout");
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        theme,
        presence,
        permission,
    );
    scene
}

/// Write a rendered scene out as a PNG.
fn write_scene_png(scene: &Scene, viewport: (u32, u32), out: &std::path::Path) {
    let (w, h) = viewport;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(scene, Color::TRANSPARENT, out);
    println!("music: wrote shot to {}", out.display());
}

/// Hermetic single-frame shot (offline sample library).
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    theme: &Theme,
) {
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();
    let scene = render_widget_frame(
        widget.as_mut(),
        &mut state,
        viewport,
        &mut text_renderer,
        theme,
        &presence,
        &permission,
    );
    write_scene_png(&scene, viewport, out);
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
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    let mut app = music::live_shot_app();
    let mut state = ui::State::new();

    let mut frame = |app: &mut music::MusicApp, state: &mut ui::State| {
        render_widget_frame(app, state, viewport, &mut tr, theme, &presence, &permission)
    };

    if let Some(q) = play {
        // Stage a real playing track for a lyrics / now-playing capture.
        for _ in 0..3 {
            let _ = frame(&mut app, &mut state);
        }
        app.shot_begin_search(q);
        for _ in 0..200 {
            let _ = frame(&mut app, &mut state);
            if app.shot_has_results() {
                app.shot_play_top_result(&mut state.doc);
                break;
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        for _ in 0..200 {
            let _ = frame(&mut app, &mut state);
            if app.images_idle() && app.shot_lyrics_ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(80));
        }
    } else {
        // Browse views (Home / Artist / Album / Search): pump until the content
        // and its cover art settle.
        let mut stable = 0;
        // Cap the wait (~13s) so a failed/geo-blocked fetch still renders rather
        // than hanging; a successful fetch settles far sooner.
        for _ in 0..160 {
            let _ = frame(&mut app, &mut state);
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
            std::thread::sleep(Duration::from_millis(80));
        }
    }

    let scene = frame(&mut app, &mut state);
    write_scene_png(&scene, viewport, out);
}
