//! Report what the decode pipeline does with a real file.
//!
//! ```text
//! cargo run -p media --features decode --example decode_probe -- <path> [seek_ms] [native|rgba] [sw]
//! ```
//!
//! Prints the decoder element that was plugged, the caps the sink negotiated,
//! and the timestamp of the picture that came back — the evidence behind any
//! claim that hardware decode is engaging, in a form a human can read at a
//! glance.

#[cfg(not(feature = "decode"))]
fn main() {
    eprintln!("build with --features decode");
    std::process::exit(2);
}

#[cfg(feature = "decode")]
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: decode_probe <path> [seek_ms] [native|rgba] [sw]");
        std::process::exit(2);
    };
    let seek_ms: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(12_500);
    let rest: Vec<String> = args.collect();
    let native = rest.iter().any(|a| a == "native");
    let software = rest.iter().any(|a| a == "sw");

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("--- registry ---");
    for d in tilo_media::gst_backend::decoders_for("video/x-h264") {
        println!("  {:<24} {:<10} rank {}", d.name, d.kind.as_str(), d.rank);
    }

    let mut config = tilo_media::EngineConfig::default().silent();
    if native {
        config.acceptance = tilo_media::FrameAcceptance::Native;
    }
    if software {
        config = config.software_only();
    }
    println!("\n--- config ---");
    println!(
        "  acceptance {:?}, policy {:?}",
        config.acceptance, config.hw_policy
    );

    let mut engine = tilo_media::MediaEngine::with_config(config);
    println!("  real decode: {}", engine.is_real_decode());
    if !engine.is_real_decode() {
        eprintln!("real decoder unavailable; a synthesized frame cannot verify decoding");
        std::process::exit(1);
    }
    engine.open(tilo_media::Source::LocalFile(path.into()));

    let started = std::time::Instant::now();
    let landed = engine.seek_and_wait(seek_ms, std::time::Duration::from_secs(15));
    let elapsed = started.elapsed();

    println!("\n--- result ---");
    println!(
        "  seek to {seek_ms}ms: {} in {:?}",
        match &landed {
            Ok(()) => "frame".to_string(),
            Err(error) => format!("NO FRAME ({error})"),
        },
        elapsed
    );
    println!("  decoder: {}", engine.hw_report().describe());
    println!("  duration: {:?}ms", engine.duration_ms());
    match engine.latest() {
        Some(f) => {
            println!("  frame: {}x{} pts={}ms", f.width, f.height, f.pts_ms);
            println!("  payload: {:?}", f.payload);
            println!(
                "  drift from request: {}{}ms",
                if f.pts_ms < seek_ms { "-" } else { "+" },
                f.pts_ms.abs_diff(seek_ms)
            );
        }
        None => println!("  frame: none"),
    }
    if landed.is_err() {
        std::process::exit(1);
    }
}
