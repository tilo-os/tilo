//! Music — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!                       (headless screenshot; identical passes to the live app).
//!
//! Optional flags for `--shots`:
//!   * `--viewport WxH`   — render at a custom size (e.g. `--viewport 480x820`
//!                          to capture the narrow / overlay-sidebar layout).
//!   * `--scenario NAME`  — stage a named interaction state for the single
//!                          frame (see `music::app_root_for_shot`); `sidebar`
//!                          snaps the navigation sidebar open.

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

    // Probe mode: prove the live path end-to-end from the terminal — run a real
    // YouTube-Music search, resolve a real audio stream URL for the top hit and
    // (best-effort) HTTP-verify + decode it. Gated behind `--features live`.
    if let Some(pos) = args.iter().position(|a| a == "--probe") {
        // Join all trailing args so an unquoted multi-word query
        // (`--probe aurora runaway`) is still taken as one search.
        let query = args[pos + 1..].join(" ");
        let query = if query.trim().is_empty() { "aurora runaway".to_string() } else { query };
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

    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_value_arg(&args, "--scenario").unwrap_or_default();
        let viewport = parse_value_arg(&args, "--viewport")
            .and_then(|s| parse_viewport(&s))
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        render_shot_to_png(music::app_root_for_shot(&scenario), viewport, &out)?;
        return Ok(());
    }

    let state = ui::State::new();
    let app = ui::App::new(state, music::app_root());
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
    if hits.is_empty() {
        return Err("search returned zero results — cannot prove the path".into());
    }
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

    let top = &hits[0];
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
    println!("   API content-length: {} bytes ({mib:.2} MiB)", stream.size_bytes);
    println!("   URL: {}", stream.url);

    // Independent proof the URL actually serves audio bytes.
    verify_stream_http(&stream.url)?;

    // Best-effort decode proof (only with the `audio` feature / GStreamer).
    probe_decode(&stream.url);

    println!("\nprobe OK — search + stream resolution are real.");
    Ok(())
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
    let sig: String = bytes.iter().take(12).map(|b| format!("{b:02x} ")).collect();
    println!("   received {} bytes; first 12: {}", bytes.len(), sig.trim_end());
    if !status.is_success() || bytes.is_empty() {
        return Err(format!("stream URL did not serve bytes (HTTP {status})").into());
    }
    println!("   -> the resolved URL serves real audio bytes.");
    Ok(())
}

/// Best-effort GStreamer decode of the resolved stream to a fake sink (no
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
        println!("   [decode] missing GStreamer plugins: {}", missing.join(", "));
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
    println!("music: wrote shot to {}", out.display());
    Ok(())
}
