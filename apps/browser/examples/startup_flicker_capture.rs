//! Capture the browser's startup frame sequence to diagnose the "weird
//! flickers before the image stabilizes" report. Polls the engine's frame
//! mailbox at ~120 Hz from t=0, saves every *visually distinct* frame as a
//! PNG (coarse 8×8 signature, L1 threshold), and prints a de-duplicated
//! timeline: frame #, time since start, uniformity, dominant colour.
//!
//! Run:
//!   WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
//!   cargo run -p browser --example startup_flicker_capture --features wpe --release -- https://duckduckgo.com

use std::sync::Arc;
use ui::time::Duration;

use ui::webview::wpe::WpeEngine;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEngine};

fn output_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("tilo-browser-flicker-{}", std::process::id()))
}

fn latest(engine: &dyn WebViewEngine) -> Option<(Arc<Vec<u8>>, u32, u32)> {
    engine
        .frames()
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| (f.rgba.clone(), f.width, f.height))
}

/// 8×8 average-colour signature (192 bytes) for cheap change detection.
fn signature(rgba: &[u8], w: u32, h: u32) -> [u16; 192] {
    let mut sig = [0u16; 192];
    if w == 0 || h == 0 {
        return sig;
    }
    let (bw, bh) = (w / 8, h / 8);
    for by in 0..8u32 {
        for bx in 0..8u32 {
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in (by * bh)..((by + 1) * bh) {
                for x in (bx * bw)..((bx + 1) * bw) {
                    let i = ((y * w + x) * 4) as usize;
                    if i + 2 < rgba.len() {
                        r += rgba[i] as u64;
                        g += rgba[i + 1] as u64;
                        b += rgba[i + 2] as u64;
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            let idx = ((by * 8 + bx) * 3) as usize;
            sig[idx] = (r / n) as u16;
            sig[idx + 1] = (g / n) as u16;
            sig[idx + 2] = (b / n) as u16;
        }
    }
    sig
}

fn l1(a: &[u16; 192], b: &[u16; 192]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
        .sum()
}

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

/// Mean colour of the whole frame plus mean alpha — good enough to name a
/// flash colour and tell a transparent frame (composites over the
/// placeholder) from an opaque one (a real flash the user sees).
fn mean(rgba: &[u8]) -> (u8, u8, u8, u8) {
    let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        r += px[0] as u64;
        g += px[1] as u64;
        b += px[2] as u64;
        a += px[3] as u64;
        n += 1;
    }
    let n = n.max(1);
    ((r / n) as u8, (g / n) as u8, (b / n) as u8, (a / n) as u8)
}

fn save_png(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) {
    ui::png::write_rgba8(path, w, h, rgba).unwrap();
}

fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://duckduckgo.com".into());
    let output = output_dir();
    let _ = std::fs::create_dir_all(&output);
    for e in std::fs::read_dir(&output).into_iter().flatten().flatten() {
        let _ = std::fs::remove_file(e.path());
    }

    // Width arg lets us reproduce the mobile-breakpoint (600px) reload:
    // < 600 flips the UA to mobile, which — if it happens after the first
    // load — forces a reload (the startup flicker).
    let width: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);

    let engine: SharedEngine = WpeEngine::new().expect("engine");
    let start = ui::time::now();
    engine.send(WebViewCommand::Resize {
        width,
        height: 600,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate(url.clone()));
    engine.send(WebViewCommand::SetFocused(true));
    println!("capturing startup for {url} …\n  #   t+ms   uniform  mean(rgb)      note");

    let mut last_sig: Option<[u16; 192]> = None;
    let mut saved = 0u32;
    let mut seen = 0u32;
    let deadline = start + Duration::from_secs(5);
    while ui::time::now() < deadline && saved < 60 {
        if let Some((rgba, w, h)) = latest(&*engine) {
            let sig = signature(&rgba, w, h);
            // Save the first 8 distinct engine frames unconditionally (fast
            // early flashes), then only perceptible whole-frame shifts.
            let changed = match &last_sig {
                None => true,
                Some(prev) => {
                    let d = l1(prev, &sig);
                    if seen < 8 { d > 150 } else { d > 1200 }
                }
            };
            if changed {
                seen += 1;
                let ms = start.elapsed().as_millis();
                let u = uniformity(&rgba) * 100.0;
                let (mr, mg, mb, ma) = mean(&rgba);
                let opaque = ma > 250;
                let (ri, gi, bi) = (mr as i32, mg as i32, mb as i32);
                let note = if u > 97.0 {
                    if ma < 5 {
                        "transparent (shows placeholder)"
                    } else if bi > ri + 30 && bi > gi + 30 {
                        "SOLID BLUE — flash"
                    } else if mr > 230 && mg > 230 && mb > 230 {
                        "solid white — flash"
                    } else if mr < 20 && mg < 20 && mb < 20 {
                        "SOLID BLACK — flash"
                    } else {
                        "solid"
                    }
                } else if opaque {
                    "content"
                } else {
                    "content (partial alpha)"
                };
                let path = output.join(format!("{saved:02}_t{ms:04}.png"));
                save_png(&path, &rgba, w, h);
                println!(
                    "  {saved:2}  {ms:5}   {u:5.1}%  ({mr:3},{mg:3},{mb:3})  a{ma:3}   {note}"
                );
                last_sig = Some(sig);
                saved += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    println!("\nsaved {saved} distinct frames to {}", output.display());
}
