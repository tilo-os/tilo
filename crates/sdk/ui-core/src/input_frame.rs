//! Where a key's time goes inside the app, stage by stage.
//!
//! With `RUST_LOG` enabling `tilo::input_frame=debug` (the launcher forwards
//! `RUST_LOG` into the sandbox), each key press the window dispatches starts a
//! record, and the first frame submitted after it ends one: one content-free
//! `INPUT-FRAME` line on stderr, which the session log keeps beside the
//! compositor's `KEY-LATENCY` for the same key.
//!
//! `INPUT-FRAME keys=1 handle_us=180 wait_us=2100 frame_us=3400 ui.frame_sync=2900/1 ui.layout=300/1 ... present_us=900 total_us=6580`
//!
//! - `handle_us`: the window handling the key event;
//! - `wait_us`: from the event handled to the frame that answers it starting;
//! - `frame_us`: that frame, start to its scene handed to the render thread;
//! - then every named scope (`tilo_resource::profiling::Phase`) that ran from
//!   the key to that frame, as microseconds and calls: the framework's sync,
//!   layout and draw, its publication, the app's own; those that ran while
//!   the key was handled, before the frame, are marked `key:`. Scopes nest,
//!   so they do not add up;
//! - `repaint`: `full` with why (`full:structure+presence`), or the pixels
//!   the frame's damage repaints (`repaint_px`), in how many rectangles, and
//!   the largest;
//! - `present_us`: the render thread rasterising and presenting it, which
//!   commits the surface;
//! - `total_us`: dispatch to present.
//!
//! Delivery to the app is not here: a Wayland key's timestamp has no defined
//! base. The compositor's `KEY-LATENCY` key-to-commit for the same key, less
//! `total_us`, is the time from the compositor sending the key to the window
//! dispatching it.
//!
//! Keys that arrive before the frame starts join it (`keys`). Off, each call
//! here is one thread-local flag read, and scopes read no clock.

use std::cell::{Cell, RefCell};

use crate::time::Instant;

/// The tracing target that turns the record on.
pub const TARGET: &str = "tilo::input_frame";

struct Pending {
    dispatched: Instant,
    handled: Option<Instant>,
    keys: u32,
    frame_started: Option<Instant>,
    /// The scopes that ran before the frame started: handling the key.
    before_frame: Vec<tilo_resource::profiling::Sample>,
}

thread_local! {
    /// Whether the target is on; read once, after the app installed its
    /// subscriber (the first key comes long after).
    static ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

fn enabled() -> bool {
    ENABLED.with(|enabled| {
        if let Some(on) = enabled.get() {
            return on;
        }
        let on = tracing::enabled!(target: TARGET, tracing::Level::DEBUG);
        enabled.set(Some(on));
        on
    })
}

fn pending() -> bool {
    ENABLED.with(|enabled| enabled.get() == Some(true))
        && PENDING.with(|pending| pending.borrow().is_some())
}

/// The window starts dispatching `event`: a key press starts a record, or
/// joins the one waiting for its frame.
pub(crate) fn key_dispatched(event: &winit::event::WindowEvent) {
    use winit::event::{ElementState, Ime, WindowEvent};
    let is_key = matches!(event,
        WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed)
        || matches!(event, WindowEvent::Ime(Ime::Commit(_)));
    if !is_key || !enabled() {
        return;
    }
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        match pending.as_mut() {
            Some(waiting) if waiting.frame_started.is_none() => {
                waiting.keys = waiting.keys.saturating_add(1);
            }
            Some(_) => {}
            None => {
                *pending = Some(Pending {
                    dispatched: crate::time::now(),
                    handled: None,
                    keys: 1,
                    frame_started: None,
                    before_frame: Vec::new(),
                });
                tilo_resource::profiling::begin_live();
            }
        }
    });
}

/// The window has handled the event it started dispatching.
pub(crate) fn event_handled() {
    if !pending() {
        return;
    }
    PENDING.with(|pending| {
        if let Some(waiting) = pending.borrow_mut().as_mut()
            && waiting.handled.is_none()
        {
            waiting.handled = Some(crate::time::now());
        }
    });
}

/// A frame starts: the first after a key answers it.
pub(crate) fn frame_started() {
    if !pending() {
        return;
    }
    PENDING.with(|pending| {
        if let Some(waiting) = pending.borrow_mut().as_mut()
            && waiting.frame_started.is_none()
        {
            waiting.frame_started = Some(crate::time::now());
            waiting.before_frame = tilo_resource::profiling::take_live();
            tilo_resource::profiling::begin_live();
        }
    });
}

/// The frame that started after a key is handed to the render thread: what
/// to call once the render thread presented it, which writes the record.
/// What a frame repaints: the whole window, and why, or the pixels its
/// damage covers.
pub(crate) enum Repaint<'a> {
    Full(RepaintWhy),
    /// The damage's rectangles, in the frame's pixels.
    Damage(&'a [vello::kurbo::Rect]),
}

/// Why a frame repaints its whole window: each reason the frame found, by
/// name. A few bits a frame, written only into a record.
#[derive(Clone, Copy, Default)]
pub(crate) struct RepaintWhy(u32);

/// The reasons [`RepaintWhy`] names, by bit.
const REASONS: [&str; 13] = [
    "structure",
    "record",
    "rebuilt",
    "moved",
    "render",
    "overlay",
    "decorations",
    "slot-bar",
    "presence",
    "layer",
    "moves",
    "marks",
    "other",
];

impl RepaintWhy {
    /// `full`, noting it as `reason` when it holds.
    pub(crate) fn note(&mut self, reason: &'static str, full: bool) -> bool {
        if full {
            let bit = REASONS
                .iter()
                .position(|known| *known == reason)
                .unwrap_or(REASONS.len() - 1);
            self.0 |= 1 << bit;
        }
        full
    }

    pub(crate) fn names(self) -> String {
        let names: Vec<&str> = REASONS
            .iter()
            .enumerate()
            .filter(|(bit, _)| self.0 & (1 << bit) != 0)
            .map(|(_, name)| *name)
            .collect();
        names.join("+")
    }
}

/// `repaint` says what the frame repaints.
pub(crate) fn submitted(repaint: Repaint<'_>) -> Option<Box<dyn FnOnce() + Send>> {
    if !pending() {
        return None;
    }
    let waiting = PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        pending
            .as_ref()
            .is_some_and(|waiting| waiting.frame_started.is_some())
            .then(|| pending.take())
            .flatten()
    })?;
    let submitted = crate::time::now();
    let mut line = line_until_submit(&waiting, submitted, &tilo_resource::profiling::take_live());
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a pixel count of one window"
    )]
    line.push_str(&match repaint {
        Repaint::Full(why) => format!(" repaint=full:{}", why.names()),
        Repaint::Damage(rects) => {
            let pixels: f64 = rects.iter().map(vello::kurbo::Rect::area).sum();
            let largest = rects
                .iter()
                .max_by(|a, b| a.area().total_cmp(&b.area()))
                .map_or_else(
                    || "none".to_owned(),
                    |rect| format!("{}x{}", rect.width() as u64, rect.height() as u64),
                );
            format!(
                " repaint_px={} rects={} largest={largest}",
                pixels.max(0.0) as u64,
                rects.len()
            )
        }
    });
    Some(Box::new(move || {
        let presented = crate::time::now();
        let present_us = presented.saturating_duration_since(submitted).as_micros();
        let total_us = presented
            .saturating_duration_since(waiting.dispatched)
            .as_micros();
        crate::diag!("INPUT-FRAME {line} present_us={present_us} total_us={total_us}");
    }))
}

fn line_until_submit(
    waiting: &Pending,
    submitted: Instant,
    scopes: &[tilo_resource::profiling::Sample],
) -> String {
    let handled = waiting.handled.unwrap_or(waiting.dispatched);
    let started = waiting.frame_started.unwrap_or(submitted);
    let mut line = format!(
        "keys={} handle_us={} wait_us={} frame_us={}",
        waiting.keys,
        handled
            .saturating_duration_since(waiting.dispatched)
            .as_micros(),
        started.saturating_duration_since(handled).as_micros(),
        submitted.saturating_duration_since(started).as_micros(),
    );
    for (stage, scopes) in [("key:", waiting.before_frame.as_slice()), ("", scopes)] {
        for scope in scopes {
            line.push_str(&format!(
                " {stage}{}={}/{}",
                scope.phase,
                scope.elapsed_ns / 1000,
                scope.calls
            ));
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() {
        ENABLED.with(|enabled| enabled.set(Some(true)));
    }

    #[test]
    fn nothing_is_recorded_with_no_key_waiting() {
        on();
        PENDING.with(|pending| *pending.borrow_mut() = None);
        frame_started();
        event_handled();
        assert!(submitted(Repaint::Damage(&[])).is_none());
    }

    #[test]
    fn the_first_frame_after_a_key_ends_its_record_with_the_scopes_it_ran() {
        on();
        PENDING.with(|pending| {
            *pending.borrow_mut() = Some(Pending {
                dispatched: crate::time::now(),
                handled: None,
                keys: 1,
                frame_started: None,
                before_frame: Vec::new(),
            });
        });
        tilo_resource::profiling::begin_live();
        event_handled();
        assert!(
            submitted(Repaint::Damage(&[])).is_none(),
            "no frame started yet"
        );
        frame_started();
        drop(tilo_resource::profiling::Phase::start("ui.frame_sync"));
        drop(tilo_resource::profiling::Phase::start("ui.frame_sync"));
        let scopes = tilo_resource::profiling::take_live();
        let line = PENDING.with(|pending| {
            let pending = pending.borrow();
            line_until_submit(pending.as_ref().unwrap(), crate::time::now(), &scopes)
        });
        assert!(line.starts_with("keys=1 handle_us="), "{line}");
        assert!(line.contains(" ui.frame_sync="), "{line}");
        assert!(line.ends_with("/2"), "{line}");
        assert!(submitted(Repaint::Full(RepaintWhy::default())).is_some());
        assert!(PENDING.with(|pending| pending.borrow().is_none()));
    }
}
