//! Browser-thread plumbing — owns the Servo `ServoEngine` and pumps a
//! command channel. This is shared between the iced host (Phase A) and
//! the quick-ui host (Phase B); each host implements [`crate::host::BrowserHost`]
//! and supplies its own frame sink + wake notifier.
//!
//! The actual ServoEngine struct still lives in `servo_engine.rs`; this
//! module is the glue that drives it from a background thread.

use crate::host::SharedHost;
use crate::servo_engine::ServoEngine;
use crossbeam_channel::{Receiver, Sender};
use servo::InputEvent;

/// One-shot commands the host can issue to the browser thread.
#[derive(Debug, Clone)]
pub enum BrowserCommand {
    /// Navigate the WebView to the given URL.
    LoadUrl(String),
    /// Forward an input (mouse / keyboard) event into Servo.
    Input(InputEvent),
    /// Move keyboard focus into the embedded WebView so subsequent keys
    /// are routed there.
    Focus,
    /// Resize Servo's render surface. Width/height are in *physical*
    /// pixels (logical × scale_factor).
    Resize(u32, u32),
    /// Update the hi-DPI scale factor reported to Servo.
    ScaleFactor(f32),
}

/// Spawn the Servo engine thread. The thread:
///   1. Constructs a `ServoEngine` against `host`.
///   2. In a loop: spins Servo, drains the command channel, paints
///      periodically, sleeps ~16 ms.
///
/// Returns the join handle so the caller can keep the thread alive for
/// the lifetime of the app. In practice the binary exits via
/// `libc::_exit(0)` to side-step Servo's noisy teardown, so the handle
/// usually isn't joined.
pub fn spawn_engine_thread(
    initial_url: String,
    width: u32,
    height: u32,
    scale_factor: f32,
    host: SharedHost,
    cmd_rx: Receiver<BrowserCommand>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let engine = ServoEngine::new(initial_url, width, height, scale_factor, host);

        let mut i = 0u64;
        loop {
            if let Some(servo) = &engine.servo {
                servo.spin_event_loop();
            }

            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    BrowserCommand::LoadUrl(url) => {
                        if let (Ok(u), Some(wv)) = (url::Url::parse(&url), &engine.webview) {
                            wv.load(u);
                        }
                    }
                    BrowserCommand::Input(event) => {
                        if let Some(wv) = &engine.webview {
                            wv.notify_input_event(event);
                        }
                    }
                    BrowserCommand::Focus => {
                        if let Some(wv) = &engine.webview {
                            wv.focus();
                        }
                    }
                    BrowserCommand::Resize(w, h) => {
                        engine.resize(w, h);
                    }
                    BrowserCommand::ScaleFactor(s) => {
                        engine.set_scale_factor(s);
                    }
                }
            }

            i += 1;
            if i % 5 == 0 {
                if let Some(wv) = &engine.webview {
                    wv.paint();
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    })
}

/// Convenience wrapper: a `Sender<BrowserCommand>` pair freshly created
/// alongside its `Receiver`. Used by both hosts.
pub fn command_channel() -> (Sender<BrowserCommand>, Receiver<BrowserCommand>) {
    crossbeam_channel::unbounded()
}
