//! Black-box latency + throughput benchmark for the WPE `WebKit` engine
//! behind `tilo_ui::webview` (feature `webview-wpe`).
//!
//! Treats the engine purely through the [`tilo_ui::webview::WebViewEngine`]
//! trait — commands in, frames out of the [`tilo_ui::webview::FrameSlot`]
//! mailbox — so the numbers reflect exactly what the `WebView` widget
//! observes, including the SHM copy + BGRA→RGBA swizzle per frame.
//!
//! Two measurements:
//!  * **Frame throughput** — continuous `Scroll` commands at ~60 Hz for
//!    a fixed window; distinct frames are counted via `Arc` pointer
//!    identity of `Frame::rgba` (the engine allocates a fresh buffer
//!    per publish, and the widget uses the same staleness test).
//!  * **Input→frame latency** — one `Scroll`, then busy-poll the slot
//!    until the frame pointer changes; the elapsed time is one sample.
//!
//! Run with the prebuilt WPE stack (Fedora ships no WPE packages):
//!
//! ```sh
//! export PKG_CONFIG_PATH=$HOME/.cache/tilo-wpe/prefix/lib64/pkgconfig \
//!        LD_LIBRARY_PATH=$HOME/.cache/tilo-wpe/prefix/lib64
//! cargo run -p ui-examples --features webview-wpe --example ui-wpe -- wpe_latency_bench [URL]
//! ```

#[cfg(not(feature = "webview-wpe"))]
pub fn run(_args: &[String]) {
    eprintln!("wpe_latency_bench: build with --features webview-wpe");
}

#[cfg(feature = "webview-wpe")]
pub fn run(args: &[String]) {
    bench::run(args);
}

#[cfg(feature = "webview-wpe")]
mod bench {
    use std::sync::Arc;
    use std::time::Duration;

    use tilo_ui::webview::wpe::WpeEngine;
    use tilo_ui::webview::{FrameSlot, WebViewCommand, WebViewEngine, WebViewEvent};

    const DEFAULT_URL: &str = "https://en.wikipedia.org/wiki/Web_browser";
    const WIDTH: u32 = 1400;
    const HEIGHT: u32 = 900;
    const SCALE: f32 = 1.0;

    /// Scroll pointer position: viewport centre.
    const POINTER: (f32, f32) = (WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0);
    /// Wheel-step magnitude per scroll command, in physical pixels.
    const SCROLL_STEP: f32 = 40.0;

    /// Continuous-scroll window for the throughput measurement.
    const THROUGHPUT_WINDOW: Duration = Duration::from_secs(3);
    /// Scroll cadence during the throughput window (~60 Hz input rate).
    const SCROLL_INTERVAL: Duration = Duration::from_millis(16);

    /// Latency samples kept, and leading samples discarded as warmup
    /// (first responses include cold caches / JIT / raster warm-up).
    const LATENCY_SAMPLES: usize = 50;
    const LATENCY_WARMUP: usize = 5;
    /// Poll cadence while waiting for a response frame.
    const POLL_INTERVAL: Duration = Duration::from_micros(500);
    /// Give up on a single latency sample after this long (counted as a
    /// miss, not folded into the stats).
    const SAMPLE_TIMEOUT: Duration = Duration::from_secs(2);
    /// Hard deadline for page load + first painted frame.
    const LOAD_TIMEOUT: Duration = Duration::from_secs(90);

    /// Identity + dimensions of the frame currently in the slot.
    /// Pointer identity of the `rgba` allocation is the staleness test
    /// the `WebView` widget itself uses — a changed pointer is a new
    /// engine publish.
    fn frame_key(slot: &FrameSlot) -> Option<(usize, u32, u32)> {
        slot.lock().unwrap().as_ref().map(|f| {
            (
                Arc::as_ptr(&f.rgba) as *const u8 as usize,
                f.width,
                f.height,
            )
        })
    }

    /// True once the slot holds a frame with any non-transparent pixel —
    /// WebKit publishes fully transparent buffers before first paint.
    fn frame_painted(slot: &FrameSlot) -> bool {
        slot.lock()
            .unwrap()
            .as_ref()
            .map(|f| f.rgba.chunks_exact(4).any(|px| px[3] != 0))
            .unwrap_or(false)
    }

    /// Alternating scroll direction so long measurement runs oscillate
    /// in place instead of pinning at the bottom of the page (a pinned
    /// scroll produces no repaint and would fake terrible numbers).
    fn scroll_cmd(flip: bool) -> WebViewCommand {
        WebViewCommand::Scroll {
            dx: 0.0,
            dy: if flip { SCROLL_STEP } else { -SCROLL_STEP },
            x: POINTER.0,
            y: POINTER.1,
        }
    }

    /// Nearest-rank percentile over an already-sorted sample slice.
    fn percentile(sorted: &[f64], p: f64) -> f64 {
        let n = sorted.len();
        let rank = ((p / 100.0) * n as f64).ceil() as usize;
        sorted[rank.clamp(1, n) - 1]
    }

    pub fn run(args: &[String]) {
        // WebKit's bubblewrap sandbox can't nest inside the dev
        // container; on Tilo the workspace sandbox is the outer boundary
        // anyway. Must be set before the engine boots.
        unsafe { std::env::set_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1") };

        let url = args.first().cloned().unwrap_or_else(|| DEFAULT_URL.into());

        let boot_start = tilo_ui::time::now();
        let engine = match WpeEngine::new() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("wpe_latency_bench: WPE engine failed to boot: {e}");
                std::process::exit(1);
            }
        };
        let boot_time = boot_start.elapsed();
        let slot = engine.frames();

        engine.send(WebViewCommand::Resize {
            width: WIDTH,
            height: HEIGHT,
            scale_factor: SCALE,
        });
        engine.send(WebViewCommand::Navigate(url.clone()));

        // ---- Load: wait for LoadFinished AND a painted frame ----
        let load_start = tilo_ui::time::now();
        let mut load_finished = false;
        let mut painted = false;
        while !(load_finished && painted) {
            if load_start.elapsed() > LOAD_TIMEOUT {
                eprintln!(
                    "wpe_latency_bench: page did not load+paint within {LOAD_TIMEOUT:?} \
                     (load_finished={load_finished}, painted={painted})"
                );
                std::process::exit(1);
            }
            for ev in engine.drain_events() {
                match ev {
                    WebViewEvent::LoadFinished => load_finished = true,
                    WebViewEvent::LoadFailed { uri, error } => {
                        eprintln!("wpe_latency_bench: load failed for {uri}: {error}");
                        std::process::exit(1);
                    }
                    _ => {}
                }
            }
            if !painted {
                painted = frame_painted(&slot);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let load_time = load_start.elapsed();

        // Let late-loading content (fonts, images, layout shifts) settle
        // so the measurement windows aren't polluted by load-time paints.
        std::thread::sleep(Duration::from_millis(1500));
        let _ = engine.drain_events();

        let mut dims_seen: Vec<(u32, u32)> = Vec::new();
        let mut note_dims = |key: Option<(usize, u32, u32)>| {
            if let Some((_, w, h)) = key {
                if !dims_seen.contains(&(w, h)) {
                    dims_seen.push((w, h));
                }
            }
        };
        note_dims(frame_key(&slot));

        // ---- Throughput: continuous scroll, count distinct frames ----
        // The slot is a latest-frame mailbox, so we poll far faster than
        // the engine paints (0.5 ms vs. multi-ms frames) to observe every
        // publish; scrolls go out on their own 16 ms cadence.
        let mut frames = 0u64;
        let mut last_key = frame_key(&slot);
        let mut flip = false;
        let t0 = tilo_ui::time::now();
        let mut next_scroll = t0;
        while t0.elapsed() < THROUGHPUT_WINDOW {
            if tilo_ui::time::now() >= next_scroll {
                engine.send(scroll_cmd(flip));
                flip = !flip;
                next_scroll += SCROLL_INTERVAL;
            }
            let key = frame_key(&slot);
            if key.is_some() && key != last_key {
                frames += 1;
                note_dims(key);
                last_key = key;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        let window_secs = t0.elapsed().as_secs_f64();
        let fps = frames as f64 / window_secs;

        // Drain the tail of in-flight paints before latency sampling.
        std::thread::sleep(Duration::from_millis(300));
        let _ = engine.drain_events();

        // ---- Latency: one scroll -> first new frame, per sample ----
        let mut samples_ms: Vec<f64> = Vec::new();
        let mut misses = 0u32;
        let mut flip = false;
        for i in 0..(LATENCY_SAMPLES + LATENCY_WARMUP) {
            // Re-read the baseline pointer immediately before sending so
            // a paint that landed during the inter-sample settle isn't
            // credited to this command.
            let before = frame_key(&slot);
            let sent = tilo_ui::time::now();
            engine.send(scroll_cmd(flip));
            flip = !flip;

            let elapsed = loop {
                let key = frame_key(&slot);
                if key.is_some() && key != before {
                    note_dims(key);
                    break Some(sent.elapsed());
                }
                if sent.elapsed() > SAMPLE_TIMEOUT {
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            };
            match elapsed {
                Some(d) if i >= LATENCY_WARMUP => samples_ms.push(d.as_secs_f64() * 1e3),
                Some(_) => {} // warmup, discarded
                None => misses += 1,
            }
            // Let any coalesced follow-up paint land before the next
            // sample's baseline read.
            std::thread::sleep(Duration::from_millis(50));
        }

        if samples_ms.is_empty() {
            eprintln!("wpe_latency_bench: no latency samples completed ({misses} timed out)");
            std::process::exit(1);
        }
        let mut sorted = samples_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean = samples_ms.iter().sum::<f64>() / samples_ms.len() as f64;

        let dims = dims_seen
            .iter()
            .map(|(w, h)| format!("{w}x{h}"))
            .collect::<Vec<_>>()
            .join(", ");

        println!("==== WPE engine baseline (black-box, via WebViewEngine trait) ====");
        println!("url:                {url}");
        println!("viewport:           {WIDTH}x{HEIGHT} @ {SCALE}x");
        println!("engine boot:        {:.2} s", boot_time.as_secs_f64());
        println!("load+first paint:   {:.2} s", load_time.as_secs_f64());
        println!("frame dims seen:    {dims}");
        println!(
            "-- throughput: {:.1} s continuous scroll @ ~60 Hz input --",
            window_secs
        );
        println!("frames observed:    {frames}");
        println!("throughput:         {fps:.1} fps");
        println!(
            "-- input->frame latency: {} samples ({} warmup discarded, {} timed out) --",
            samples_ms.len(),
            LATENCY_WARMUP,
            misses
        );
        println!("mean:               {mean:.2} ms");
        println!("p50:                {:.2} ms", percentile(&sorted, 50.0));
        println!("p95:                {:.2} ms", percentile(&sorted, 95.0));
        println!("p99:                {:.2} ms", percentile(&sorted, 99.0));
        println!("max:                {:.2} ms", sorted[sorted.len() - 1]);
        println!("===================================================================");
    }
}
