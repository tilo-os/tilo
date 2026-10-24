//! `Pacer`: a part of the view that steps something on a steady beat while
//! it runs — a replay playing frame by frame, a slideshow advancing.
//!
//! The pacer draws nothing of its own: it wraps its content and, while
//! `running` holds `true`, calls its step once a beat. The step says
//! whether to go on; returning `false` (the last frame was reached) stops
//! the pacer by setting `running` to `false`, so the control that started it
//! reads as stopped again. Between beats the window sleeps: the pacer asks
//! to be woken at the next beat ([`Widget::next_wake_deadline`]) rather than
//! for animation frames.
//!
//! ```ignore
//! let playing = cx.local(false);
//! let position = cx.local(0_usize);
//! let pacer = Pacer::new(content, &playing, Duration::from_millis(850), move || {
//!     position.update(|at| *at += 1);
//!     position.get() + 1 < frames
//! });
//! ```

use std::cell::Cell;

use super::{AppModel, Element, ModelPath, Widget};
use crate::LocalState;
use crate::time::{Duration, Instant};

/// Steps its `step` once a beat while `running` is `true`.
pub struct Pacer {
    content: Element,
    running: LocalState<bool>,
    beat: Duration,
    /// When the next step is due; `None` while stopped.
    due: Cell<Option<Instant>>,
    step: Box<dyn FnMut() -> bool>,
}

impl Pacer {
    /// Wrap `content`; while `running` is `true`, call `step` every `beat`
    /// until it returns `false`.
    pub fn new(
        content: Element,
        running: &LocalState<bool>,
        beat: Duration,
        step: impl FnMut() -> bool + 'static,
    ) -> Self {
        Self {
            content,
            running: running.clone(),
            beat,
            due: Cell::new(None),
            step: Box::new(step),
        }
    }

    /// When the next step is due, starting the beat if it just started.
    fn due(&self, now: Instant) -> Option<Instant> {
        if !self.running.get() {
            self.due.set(None);
            return None;
        }
        let due = self.due.get().unwrap_or_else(|| now + self.beat);
        self.due.set(Some(due));
        Some(due)
    }

    /// Take the step that is due at `now`, if one is; whether one was.
    fn pace(&mut self, now: Instant) -> bool {
        let Some(due) = self.due(now) else {
            return false;
        };
        if now < due {
            return false;
        }
        if (self.step)() {
            // The next beat counts from this one, without a burst to catch
            // up on beats a busy window missed.
            let behind = now.checked_sub(self.beat).map_or(due, |back| due.max(back));
            self.due.set(Some(behind + self.beat));
        } else {
            self.due.set(None);
            self.running.set(false);
        }
        true
    }
}

impl super::Decorator for Pacer {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let own = self.due(crate::time::now());
        own.into_iter()
            .chain(self.content.next_wake_deadline())
            .min()
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let stepped = self.pace(crate::time::now());
        self.content.update(state, path) || stepped
    }

    fn serve_deadline(&mut self, now: Instant) -> bool {
        let stepped = self.pace(now);
        self.content.serve_deadline(now) || stepped
    }
}

#[cfg(test)]
#[path = "pacer_tests.rs"]
mod tests;
