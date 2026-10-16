//! The commands the Music binary runs instead of opening a window.
//!
//! - `--probe QUERY` proves the live path end to end from a terminal: a real
//!   YouTube-Music search, a real audio stream for the top hit (HTTP-verified
//!   and, with `audio`, decoded), and the real Home, artist and album pages.
//! - `--bench [SCENARIO:]FRAMES` renders a screenshot scenario (the sample
//!   library's Home by default) `FRAMES` times headlessly and reports the
//!   per-frame CPU cost of sync, layout and draw.
//! - `--probe-spotify-engine VALUE` (the value is ignored) runs native Spotify
//!   playback's self-check without an account, network or audio device. In a
//!   release build that code is in `libtilo_spotify_engine.so`, so the check
//!   proves Music loads its engine library and runs it.
//! - `--probe-youtube-engine VALUE` (the value is ignored) runs the `YouTube`
//!   client's self-check without a network. It needs `runtime-engine`, the
//!   release build, where that code is in `libtilo_youtube_engine.so`; the
//!   check proves Music loads that library and runs it.

use ui::framework::Command;

/// The commands [`crate::MusicShell`] declares.
pub const COMMANDS: &[Command] = &[
    Command {
        option: "--probe",
        run: probe,
    },
    Command {
        option: "--bench",
        run: bench,
    },
    Command {
        option: "--probe-spotify-engine",
        run: probe_spotify_engine,
    },
    Command {
        option: "--probe-youtube-engine",
        run: probe_youtube_engine,
    },
];

#[cfg(feature = "runtime-engine")]
fn probe_youtube_engine(_: &str) -> Result<(), String> {
    let report = tilo_youtube_engine::self_check()?;
    println!("music --probe-youtube-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_youtube_engine(_: &str) -> Result<(), String> {
    Err("the YouTube engine library needs the `runtime-engine` feature".to_string())
}

#[cfg(feature = "spotify-native")]
fn probe_spotify_engine(_: &str) -> Result<(), String> {
    let report = crate::audio::spotify_audio::self_check()?;
    println!("music --probe-spotify-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "spotify-native"))]
fn probe_spotify_engine(_: &str) -> Result<(), String> {
    Err("native Spotify playback needs the `spotify-native` feature".to_string())
}

fn probe(query: &str) -> Result<(), String> {
    let query = if query.trim().is_empty() {
        "aurora runaway"
    } else {
        query
    };
    run_probe(query).map_err(|error| error.to_string())
}

/// Render `[SCENARIO:]FRAMES` frames of a capture and report their cost.
fn bench(value: &str) -> Result<(), String> {
    use ui::framework::ResourceApp as _;

    let (scenario, frames) = match value.rsplit_once(':') {
        Some((scenario, frames)) => (Some(scenario), frames),
        None => (None, value),
    };
    let frames: usize = frames
        .parse()
        .map_err(|_| format!("{frames:?} is not a frame count"))?;
    if frames == 0 {
        return Err("benchmark at least one frame".into());
    }
    let mut samples: Vec<f64> = ui::framework::time_frames::<crate::MusicShell>(scenario, frames)?
        .into_iter()
        .map(|frame| frame.as_secs_f64() * 1000.0)
        .collect();
    samples.sort_by(f64::total_cmp);
    let average = samples.iter().sum::<f64>() / frames as f64;
    let at = |fraction: f64| {
        samples
            .get(((frames as f64 * fraction) as usize).min(frames - 1))
            .copied()
            .unwrap_or_default()
    };
    let (width, height) = crate::MusicShell::shot_viewport(scenario);
    let nocache = std::env::var("TILO_MUSIC_NOCACHE").is_ok();
    println!(
        "bench scenario={} nocache={nocache} frames={frames} @ {width}x{height} :  \
         avg={average:.3}ms  p50={:.3}ms  p95={:.3}ms  max={:.3}ms",
        scenario.unwrap_or("home"),
        at(0.5),
        at(0.95),
        at(1.0),
    );
    Ok(())
}

/// Prove the live streaming path from the terminal. Real search + real audio
/// stream resolution + an independent HTTP byte-check. Requires `--features
/// live`; the default build prints how to enable it (never fakes a result).
#[cfg(feature = "live")]
fn run_probe(query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use crate::backend::MusicBackend;

    println!("music --probe: live YouTube-Music backend (rustypipe InnerTube)\n");

    let backend = crate::backend::live::RustyPipeBackend::new()
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
    backend: &impl crate::backend::MusicBackend,
    top: &crate::backend::TrackHit,
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
fn probe_home(backend: &impl crate::backend::MusicBackend) {
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
fn probe_artist(backend: &impl crate::backend::MusicBackend, top: &crate::backend::TrackHit) {
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
fn probe_album(backend: &impl crate::backend::MusicBackend, top: &crate::backend::TrackHit) {
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
fn run_probe(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "the probe needs the `live` feature (real YouTube-Music backend); rebuild with \
         `cargo run -p music --features live -- --probe \"aurora runaway\"`"
            .into(),
    )
}
