//! On-device check that video decode reaches the hardware this machine has.
//!
//! ```text
//! cargo run -p tilo-media --features decode --example hw_video_smoke -- [clip...]
//! ```
//!
//! 1. Lists the kernel's V4L2 codec nodes and the largest picture each takes.
//! 2. For each clip (or, with none given, a VP8 clip it encodes itself), opens
//!    it through the playback engine and checks the decoder that was plugged
//!    against what the kernel says the hardware can do: a stream the hardware
//!    takes must decode in hardware and arrive as a dmabuf, never copied.
//! 3. Plays each clip in real time, once keeping frames as dmabufs and once
//!    asking for RGBA, and reports the frame rate reached and the CPU spent.
//!
//! Exits non-zero when a stream the hardware should take did not reach it.

#[cfg(not(feature = "decode"))]
fn main() {
    eprintln!("build with --features decode");
    std::process::exit(2);
}

#[cfg(feature = "decode")]
fn main() {
    use tilo_media::hw::v4l2;

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("--- V4L2 codec nodes ---");
    let devices = v4l2::devices();
    if devices.is_empty() {
        println!("  none");
    }
    for d in devices {
        println!("  {:<12} {}", d.path, d.describe());
    }

    let mut clips: Vec<std::path::PathBuf> = std::env::args()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();
    if clips.is_empty() {
        match generate_clip() {
            Ok(path) => clips.push(path),
            Err(e) => {
                eprintln!("no clip given and none could be generated: {e}");
                std::process::exit(2);
            }
        }
    }

    let mut failed = false;
    for clip in &clips {
        println!("\n--- {} ---", clip.display());
        failed |= !check_clip(clip, devices);
    }
    if failed {
        std::process::exit(1);
    }
}

/// Encode a short 720p60 VP8 clip — the one codec both every `GStreamer`
/// install can encode and the RK3566's decoder takes.
#[cfg(feature = "decode")]
fn generate_clip() -> Result<std::path::PathBuf, String> {
    use tilo_media::gst;
    use tilo_media::gst::prelude::*;

    gst::init().map_err(|e| e.to_string())?;
    let path = std::env::temp_dir().join(format!("tilo-hw-smoke-{}.webm", std::process::id()));
    let pipeline = gst::parse::launch(&format!(
        "videotestsrc num-buffers=120 pattern=ball ! \
         video/x-raw,width=1280,height=720,framerate=60/1 ! \
         vp8enc deadline=1 cpu-used=16 target-bitrate=4000000 ! webmmux ! \
         filesink location={}",
        path.display()
    ))
    .map_err(|e| e.to_string())?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("pipeline has no bus")?;
    let message = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(120),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let result = match message.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => Ok(path),
        Some(gst::MessageView::Error(e)) => Err(e.error().to_string()),
        _ => Err("encoding the clip timed out".to_string()),
    };
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Probe and play one clip. Returns whether it met expectations.
#[cfg(feature = "decode")]
fn check_clip(clip: &std::path::Path, devices: &[tilo_media::hw::v4l2::V4l2Device]) -> bool {
    use tilo_media::hw::v4l2;
    use tilo_media::{EngineConfig, FrameAcceptance, MediaEngine, Source};

    let mut engine = MediaEngine::with_config(EngineConfig::gpu().silent());
    if !engine.is_real_decode() {
        println!("  FAIL: GStreamer decode is not available");
        return false;
    }
    engine.open(Source::LocalFile(clip.to_path_buf()));
    // Wait for the preroll picture, keeping any error: the reason a stream
    // cannot be decoded travels in it.
    let mut errors = Vec::new();
    let started = std::time::Instant::now();
    while engine.latest().is_none()
        && errors.is_empty()
        && started.elapsed() < std::time::Duration::from_secs(20)
    {
        for event in engine.poll() {
            if let tilo_media::MediaEvent::Error(text) = event {
                errors.push(text);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if engine.latest().is_none() {
        // A stream nothing here can decode, with the reason stated, is a
        // property of the device, not a failure of the decode path.
        if errors
            .iter()
            .any(|e| e.contains("cannot be decoded on this device"))
        {
            println!("  note: {}", errors.join("; "));
            return true;
        }
        println!(
            "  FAIL: no picture after {:?} ({}) {}",
            started.elapsed(),
            engine.hw_report().describe(),
            errors.join("; ")
        );
        return false;
    }
    let report = engine.hw_report();
    let Some(frame) = engine.latest().cloned() else {
        println!("  FAIL: no picture");
        return false;
    };
    println!("  decoder: {}", report.describe());
    println!(
        "  picture: {}x{} {:?}",
        frame.width, frame.height, frame.payload
    );

    let media_type = report
        .decoder_element
        .as_deref()
        .and_then(v4l2::element_media_type);
    let expect_hw = media_type.is_some_and(|mt| match v4l2::decoder_limit(devices, mt) {
        Some(Some(limit)) => v4l2::fits(limit, frame.width, frame.height),
        Some(None) => true,
        None => false,
    });
    let mut ok = true;
    if expect_hw {
        if report.is_hardware() && frame.is_gpu_resident() {
            println!("  PASS: decoded in hardware, picture stays a dmabuf");
        } else {
            println!(
                "  FAIL: the kernel has a {} decoder for {}x{}, but the picture came from {}",
                media_type.map_or("?", v4l2::codec_label),
                frame.width,
                frame.height,
                report.describe()
            );
            ok = false;
        }
    } else {
        println!(
            "  note: no hardware decoder on this device takes this stream; {}",
            if report.is_hardware() {
                "yet a hardware element decoded it"
            } else {
                "software is the expected path"
            }
        );
    }
    drop(engine);

    for acceptance in [FrameAcceptance::Native, FrameAcceptance::Rgba] {
        let (fps, cpu) = play(clip, acceptance);
        println!("  playback {acceptance:?}: {fps:.1} fps, {cpu:.0}% of one core");
    }
    ok
}

/// Play `clip` in real time for up to ten seconds and count the distinct
/// pictures that arrive. Returns frames per second and this process's CPU
/// time as a percentage of one core.
#[cfg(feature = "decode")]
fn play(clip: &std::path::Path, acceptance: tilo_media::FrameAcceptance) -> (f64, f64) {
    use tilo_media::{EngineConfig, MediaEngine, MediaEvent, Source};

    let config = EngineConfig {
        acceptance,
        ..EngineConfig::default().silent()
    };
    let mut engine = MediaEngine::with_config(config);
    engine.open(Source::LocalFile(clip.to_path_buf()));
    let _ = engine.seek_and_wait(0, std::time::Duration::from_secs(20));
    let cpu_before = cpu_seconds();
    let started = std::time::Instant::now();
    engine.play();
    let mut frames = 0u32;
    let mut last_pts = None;
    'outer: while started.elapsed() < std::time::Duration::from_secs(10) {
        for event in engine.poll() {
            if matches!(event, MediaEvent::Ended | MediaEvent::Error(_)) {
                break 'outer;
            }
        }
        if let Some(pts) = engine.latest().map(|f| f.pts_ms)
            && last_pts != Some(pts)
        {
            last_pts = Some(pts);
            frames = frames.saturating_add(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let wall = started.elapsed().as_secs_f64().max(f64::EPSILON);
    let cpu = cpu_seconds() - cpu_before;
    (f64::from(frames) / wall, cpu / wall * 100.0)
}

/// User plus system CPU time this process has used, in seconds.
#[cfg(feature = "decode")]
fn cpu_seconds() -> f64 {
    // SAFETY: `getrusage` writes a plain-old-data struct it is handed.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a live, exclusively borrowed `rusage`.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } != 0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "process CPU seconds are far below 2^52; a report, not arithmetic"
    )]
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    secs(usage.ru_utime) + secs(usage.ru_stime)
}
