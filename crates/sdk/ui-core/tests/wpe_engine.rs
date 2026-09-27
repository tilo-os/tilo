//! Live WPE WebKit engine test (feature `webview-wpe`): boots real
//! WebKit offscreen, loads a `data:` page, and asserts pixels come back
//! through the frame slot. No compositor, no network.
//!
//! Run with the WPE stack from os/dev/build-wpe.sh on the search paths:
//!
//! ```sh
//! export PKG_CONFIG_PATH=$HOME/.cache/tilo-wpe/prefix/lib64/pkgconfig
//! export LD_LIBRARY_PATH=$HOME/.cache/tilo-wpe/prefix/lib64
//! cargo test -p ui --features webview-wpe --test wpe_engine
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_ui::webview::wpe::WpeEngine;
use tilo_ui::webview::{WebViewCommand, WebViewEngine, WebViewEvent};

#[test]
fn wpe_engine_renders_a_page_offscreen() {
    // WebKit's own bubblewrap sandbox cannot nest inside the dev
    // container / test environment; the per-cluster Tilo sandbox is the
    // outer boundary in production anyway.
    unsafe { std::env::set_var("WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1") };

    let engine = WpeEngine::new().expect("WPE engine boots");
    engine.send(WebViewCommand::Resize {
        width: 320,
        height: 240,
        scale_factor: 1.0,
    });
    engine.send(WebViewCommand::Navigate(
        "data:text/html,<body style=\"background:%23ff0000;margin:0\"><h1>Tilo</h1></body>".into(),
    ));

    let deadline = tilo_test_env::hang_deadline();
    let mut finished = false;
    let mut frame = None;
    while Instant::now() < deadline {
        for event in engine.drain_events() {
            if matches!(event, WebViewEvent::LoadFinished) {
                finished = true;
            }
        }
        if finished {
            // The first exported buffers can predate the paint (blank /
            // fully transparent); wait for one with actual content.
            if let Some(f) = engine.frames().lock().unwrap().clone() {
                let painted = f.rgba.chunks_exact(4).any(|px| px[3] != 0);
                if painted {
                    frame = Some(f);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let frame = frame.expect("engine produced a painted frame after LoadFinished");
    assert!(frame.width > 0 && frame.height > 0, "frame has dimensions");
    assert_eq!(
        frame.rgba.len(),
        frame.width as usize * frame.height as usize * 4,
        "tightly packed RGBA"
    );

    let centre = ((frame.height / 2) * frame.width + frame.width / 2) as usize * 4;
    let px = &frame.rgba[centre..centre + 4];
    assert!(
        px[0] > 150 && px[1] < 100 && px[2] < 100,
        "centre pixel {px:?} should be red (RGBA order)",
    );
}
