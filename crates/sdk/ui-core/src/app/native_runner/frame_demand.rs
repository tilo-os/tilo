//! Why a window keeps drawing when nobody touches it.
//!
//! With `RUST_LOG` enabling `tilo::frame_demand=debug`, the event loop names
//! what asked for each frame or wake it schedules, and once a second while
//! anything asks, one `FRAME-DEMAND` line on stderr sums it up: frames
//! submitted, how many repainted the whole window and why (`full:layer=3`),
//! then each asker and how often it asked. A settled window writes nothing.
//! Off, each call is one thread-local flag read.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use crate::time::{Duration, Instant};

const TARGET: &str = "tilo::frame_demand";
const EVERY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Window {
    since: Option<Instant>,
    frames: u32,
    full: BTreeMap<String, u32>,
    asks: BTreeMap<String, u32>,
}

thread_local! {
    static ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static WINDOW: RefCell<Window> = RefCell::new(Window::default());
}

pub(super) fn enabled() -> bool {
    ENABLED.with(|enabled| {
        if let Some(on) = enabled.get() {
            return on;
        }
        let on = tracing::enabled!(target: TARGET, tracing::Level::DEBUG);
        enabled.set(Some(on));
        on
    })
}

/// The loop asked for a frame or a wake, for `askers`.
pub(super) fn asked(askers: Vec<String>) {
    if askers.is_empty() {
        return;
    }
    let now = crate::time::now();
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        let since = *window.since.get_or_insert(now);
        for asker in askers {
            *window.asks.entry(asker).or_default() += 1;
        }
        if now.saturating_duration_since(since) >= EVERY {
            let window = std::mem::take(&mut *window);
            let mut line = format!("FRAME-DEMAND frames={}", window.frames);
            for (why, count) in &window.full {
                line.push_str(&format!(" full:{why}={count}"));
            }
            for (asker, count) in &window.asks {
                line.push_str(&format!(" {asker}={count}"));
            }
            crate::diag!("{line}");
        }
    });
}

/// A frame was handed to the render thread, repainting its whole window
/// for `full` or only its damage.
pub(super) fn submitted(full: Option<crate::input_frame::RepaintWhy>) {
    if !enabled() {
        return;
    }
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        if window.since.is_some() {
            window.frames = window.frames.saturating_add(1);
            if let Some(why) = full {
                *window.full.entry(why.names()).or_default() += 1;
            }
        }
    });
}
