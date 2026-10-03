//! Reproduce the "scroll into a blank area and get stuck" bug.
//!
//! Loads a long page, scrolls down hard, scrolls back up, then reloads —
//! saving a PNG at each stage and reporting per-phase how many *distinct*
//! frames the engine published (a stall shows up as 0 new frames) and how
//! uniform each frame is (a blank frame is ~100% one colour).
//!
//! Run:
//!   WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
//!   cargo run -p ui-examples --example ui-wpe --features webview-wpe --release -- wpe_scroll_repro

use std::sync::Arc;
use std::time::Duration;

use tilo_ui::webview::wpe::WpeEngine;
use tilo_ui::webview::{WebViewCommand, WebViewEngine};

const W: u32 = 1000;
const H: u32 = 700;

fn output_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("tilo-wpe-scroll-repro")
}

fn latest(engine: &dyn WebViewEngine) -> Option<(Arc<Vec<u8>>, u32, u32)> {
    let slot = engine.frames();
    let g = slot.lock().unwrap();
    g.as_ref().map(|f| (f.rgba.clone(), f.width, f.height))
}

fn ptr(f: &Option<(Arc<Vec<u8>>, u32, u32)>) -> usize {
    f.as_ref()
        .map(|(r, _, _)| Arc::as_ptr(r) as usize)
        .unwrap_or(0)
}

/// Fraction of pixels equal to the top-left pixel (≈1.0 == blank/uniform).
fn uniformity(rgba: &[u8]) -> f64 {
    if rgba.len() < 4 {
        return 1.0;
    }
    let (r0, g0, b0) = (rgba[0], rgba[1], rgba[2]);
    let (mut same, mut total) = (0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        total += 1;
        if px[0] == r0 && px[1] == g0 && px[2] == b0 {
            same += 1;
        }
    }
    same as f64 / total.max(1) as f64
}

fn save(f: &Option<(Arc<Vec<u8>>, u32, u32)>, name: &str) {
    if let Some((rgba, w, h)) = f {
        let output_dir = output_dir();
        let _ = std::fs::create_dir_all(&output_dir);
        let path = output_dir.join(name);
        let _ = image::save_buffer(&path, rgba, *w, *h, image::ExtendedColorType::Rgba8);
        println!(
            "  saved {name} ({w}x{h}, uniformity {:.1}%)",
            uniformity(rgba) * 100.0
        );
    } else {
        println!("  {name}: NO FRAME");
    }
}

/// Run `action`, then over `window` count how many distinct frames arrive.
fn phase(engine: &dyn WebViewEngine, label: &str, window: Duration, mut action: impl FnMut()) {
    let start_ptr = ptr(&latest(engine));
    action();
    let deadline = tilo_ui::time::now() + window;
    let (mut changes, mut last) = (0u64, start_ptr);
    while tilo_ui::time::now() < deadline {
        let p = ptr(&latest(engine));
        if p != last && p != 0 {
            changes += 1;
            last = p;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    println!("[{label}] distinct new frames: {changes}");
}

pub fn run(args: &[String]) {
    let url = args
        .first()
        .cloned()
        .unwrap_or_else(|| "https://en.wikipedia.org/wiki/Rust_(programming_language)".into());
    let steps: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    println!("url={url} steps={steps}");

    let engine: Arc<dyn WebViewEngine> = WpeEngine::new().expect("WpeEngine::new");
    engine.send(WebViewCommand::Resize {
        width: W,
        height: H,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate(url));

    // Wait for the first real (non-blank) frame.
    let deadline = tilo_ui::time::now() + Duration::from_secs(25);
    loop {
        if tilo_ui::time::now() > deadline {
            eprintln!("no initial frame in 25s");
            std::process::exit(1);
        }
        if let Some((rgba, _, _)) = latest(&*engine) {
            if uniformity(&rgba) < 0.9 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("== initial ==");
    save(&latest(&*engine), "scroll-1-initial.png");

    println!("== scroll DOWN (60 steps) ==");
    phase(&*engine, "down", Duration::from_secs(3), || {
        for _ in 0..steps {
            engine.send(WebViewCommand::Scroll {
                dx: 0.0,
                dy: -120.0,
                x: 500.0,
                y: 400.0,
            });
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    save(&latest(&*engine), "scroll-2-down.png");

    println!("== scroll UP (60 steps) ==");
    phase(&*engine, "up", Duration::from_secs(3), || {
        for _ in 0..steps {
            engine.send(WebViewCommand::Scroll {
                dx: 0.0,
                dy: 120.0,
                x: 500.0,
                y: 400.0,
            });
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    save(&latest(&*engine), "scroll-3-up.png");

    println!("== RELOAD ==");
    phase(&*engine, "reload", Duration::from_secs(5), || {
        engine.send(WebViewCommand::Reload);
    });
    save(&latest(&*engine), "scroll-4-reload.png");

    println!("done");
}
