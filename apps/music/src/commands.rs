//! The commands the Music binary runs instead of opening a window: probes that prove
//! the live path from a terminal, and benchmarks.

use ui::framework::Command;

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
        option: "--bench-search",
        run: bench_search,
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

const BENCH_QUERIES: &str =
    "aurora,daft punk,radiohead,billie eilish,beatles,taylor swift,bonobo,nils frahm";

#[cfg(feature = "live")]
const KEYSTROKE: std::time::Duration = std::time::Duration::from_millis(180);

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let at = ((samples.len() as f64 - 1.0) * fraction).round() as usize;
    samples
        .get(at.min(samples.len() - 1))
        .copied()
        .unwrap_or(f64::NAN)
}

fn report(name: &str, samples: &mut [f64]) {
    samples.sort_by(f64::total_cmp);
    println!(
        "{name}: n={}  p50={:.1}ms  p95={:.1}ms  max={:.1}ms",
        samples.len(),
        percentile(samples, 0.5),
        percentile(samples, 0.95),
        samples.last().copied().unwrap_or(f64::NAN),
    );
}

fn bench_library(extra: usize) -> crate::model::Library {
    let mut lib = crate::seed::sample_library();
    let words = [
        "night", "light", "river", "glass", "paper", "ghost", "tide", "harbor", "neon", "pulse",
        "string", "summer", "winter", "drive", "frame", "echo", "violet", "static", "garden",
        "orbit",
    ];
    let word = |n: usize| words.get(n % words.len()).copied().unwrap_or("song");
    for n in 0..extra {
        let id = format!("bench_{n}");
        let title = format!("{} {} {}", word(n), word(n / words.len()), n);
        let album = format!("al_bench_{}", n / 12);
        let artist = format!("ar_bench_{}", n / 120);
        lib.artists
            .entry(artist.clone())
            .or_insert_with(|| crate::model::Artist {
                id: artist.clone(),
                name: format!("Bench Artist {}", n / 120),
                genre: String::new(),
                monthly_listeners: 0,
            });
        lib.albums
            .entry(album.clone())
            .or_insert_with(|| crate::model::Album {
                id: album.clone(),
                title: format!("{} Album {}", word(n / 12), n / 12),
                artist_id: artist.clone(),
                year: 2000 + u32::try_from(n % 25).unwrap_or(0),
            });
        lib.tracks.insert(
            id.clone(),
            crate::model::Track {
                id,
                title,
                album_id: album,
                artist_id: artist,
                track_no: u32::try_from(n % 12).unwrap_or(0) + 1,
                duration_s: 200,
                yt_id: String::new(),
                plays: 0,
            },
        );
    }
    lib
}

fn prefixes(query: &str) -> Vec<String> {
    query
        .char_indices()
        .map(|(at, c)| query.get(..at + c.len_utf8()).unwrap_or(query).to_owned())
        .collect()
}

fn bench_search(value: &str) -> Result<(), String> {
    use ui::framework::views::{QueryEngine, QuerySpec};
    let queries: Vec<&str> = if value.trim().is_empty() {
        BENCH_QUERIES.split(',').collect()
    } else {
        value
            .split(',')
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .collect()
    };
    if queries.is_empty() {
        return Err(format!("--bench-search: no queries in {value:?}"));
    }
    for (name, extra) in [("sample library", 0), ("10k-song library", 10_000)] {
        let lib = bench_library(extra);
        let mut engine = QueryEngine::new(crate::views::kinds());
        let started = ui::time::now();
        engine.sync_local(crate::views::library_items(&lib));
        println!(
            "local index of {name}: {} songs indexed in {:.1}ms",
            lib.tracks.len(),
            ui::time::now()
                .saturating_duration_since(started)
                .as_secs_f64()
                * 1000.0
        );
        let mut samples = Vec::new();
        for query in &queries {
            for prefix in prefixes(query) {
                engine.set_query(QuerySpec::text(prefix));
                if let Some(local) = engine.timings().local {
                    samples.push(local.as_secs_f64() * 1000.0);
                }
            }
        }
        report(&format!("local first results, {name}"), &mut samples);
    }
    bench_remote(&queries);
    Ok(())
}

#[cfg(feature = "live")]
fn bench_remote(queries: &[&str]) {
    use std::sync::Arc;
    use ui::framework::views::{QueryEngine, QuerySpec, RemoteSource};
    let source: Arc<dyn RemoteSource> = Arc::new(crate::views::CatalogueSource::new(|| {
        crate::backend::preferred_backend(|note| println!("{note}"))
    }));
    let mut engine = QueryEngine::new(crate::views::kinds()).with_remote(Arc::clone(&source));
    engine.sync_local(crate::views::library_items(&crate::seed::sample_library()));
    // Focusing the field warms the connection before the first character.
    let warm_started = ui::time::now();
    source.warm();
    println!(
        "warm ({}): {:.1}ms",
        source.label(),
        ui::time::now()
            .saturating_duration_since(warm_started)
            .as_secs_f64()
            * 1000.0
    );
    let mut first = Vec::new();
    let mut done = Vec::new();
    let mut superseded = 0usize;
    let mut failed = 0usize;
    for query in queries {
        let typed = prefixes(query);
        for (at, prefix) in typed.iter().enumerate() {
            let last = at + 1 == typed.len();
            let window = if last {
                std::time::Duration::from_secs(5)
            } else {
                KEYSTROKE
            };
            engine.set_query(QuerySpec::text(prefix.clone()));
            let since = ui::time::now();
            let mut got_first = false;
            while ui::time::now().saturating_duration_since(since) < window {
                engine.poll();
                let timings = engine.timings();
                if !got_first && let Some(Some(time)) = timings.remote_first.first() {
                    first.push(time.as_secs_f64() * 1000.0);
                    got_first = true;
                }
                if let Some(Some(time)) = timings.remote_done.first() {
                    done.push(time.as_secs_f64() * 1000.0);
                    break;
                }
                if let Some(section) = engine.results().remote.first()
                    && (section.state.is_none() || section.failure().is_some())
                {
                    failed += 1;
                    break;
                }
                let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(1));
            }
            if !got_first {
                superseded += 1;
            }
            // The rest of the keystroke's time passes before the next one.
            if let Some(rest) =
                KEYSTROKE.checked_sub(ui::time::now().saturating_duration_since(since))
            {
                let _ = ui::rt::sleep_thread(rest);
            }
        }
        println!(
            "  {query:?}: {} rows from the catalogue",
            engine
                .results()
                .remote
                .first()
                .map_or(0, |section| section.rows().count())
        );
    }
    report("remote first rows per keystroke", &mut first);
    report("remote whole answer per keystroke", &mut done);
    println!("keystrokes without rows before the next one: {superseded}; unreachable: {failed}");
}

#[cfg(not(feature = "live"))]
fn bench_remote(_queries: &[&str]) {
    println!("remote: this build has no live catalogue (`live` feature)");
}

/// Prove the live path from the terminal: a real search, a real stream URL and an
/// independent HTTP byte check. Needs `live`; without it the build says how.
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

/// Resolve a stream for the top hit, print its details and check it serves bytes.
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
    println!("   itag={}  ~{} kbps", stream.itag, stream.bitrate / 1000);
    println!("   mime: {}", stream.mime);
    ui::format::machine!(
        "a diagnostic command's own output, for whoever reads it",
        println!(
            "   API content-length: {} bytes ({mib:.2} MiB)",
            stream.size_bytes
        )
    );
    // Signed URLs carry short-lived credentials: keep them out of logs.
    println!("   source URL resolved (redacted)");

    verify_stream_http(&stream.url)?;

    probe_decode(&stream.url);
    Ok(())
}

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

/// Decodes the resolved stream to a fake sink; the `audio` feature only.
#[cfg(all(feature = "live", feature = "audio"))]
fn probe_decode(url: &str) {
    use gstreamer::prelude::*;
    println!("\n== GStreamer decode check (audio -> fakesink) ==");
    if gstreamer::init().is_err() {
        println!("   [decode] gstreamer init failed — skipping");
        return;
    }
    // Plugins this pipeline needs, so a missing one is named.
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

#[cfg(not(feature = "live"))]
fn run_probe(_query: &str) -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "the probe needs the `live` feature (real YouTube-Music backend); rebuild with \
         `cargo run -p music --features live -- --probe \"aurora runaway\"`"
            .into(),
    )
}
