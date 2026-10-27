//! Frames the GPU has not finished drawing yet.
//!
//! The page draws on animation frames, and a browser keeps asking for them
//! while the GPU still works on earlier frames: a slow GPU (a software one,
//! a small device's) then queues frame after frame, and what the person did
//! reaches the screen later and later, seconds behind a dragged window. A
//! mount draws while fewer than [`IN_FLIGHT`] of its frames are unfinished;
//! otherwise it keeps what changed for the frame after the GPU catches up,
//! which then draws all of it at once.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The frames that may be submitted and not yet done. Two keep the CPU's
/// next frame overlapping the GPU's last one, so a GPU that keeps up with
/// the display never holds a frame, however late the browser reports work
/// done.
pub const IN_FLIGHT: u64 = 2;

/// Counts a mount's submitted frames against those its GPU finished.
#[derive(Default)]
pub struct FramePacing {
    submitted: u64,
    done: Arc<AtomicU64>,
}

impl FramePacing {
    /// Whether a frame may be drawn now.
    #[must_use]
    pub fn may_draw(&self) -> bool {
        self.submitted
            .saturating_sub(self.done.load(Ordering::Acquire))
            < IN_FLIGHT
    }

    /// A frame was submitted: call the returned completion when the GPU
    /// finished its work.
    #[must_use]
    pub fn submitted(&mut self) -> Done {
        self.submitted = self.submitted.saturating_add(1);
        Done(Arc::clone(&self.done))
    }
}

/// One submitted frame's completion.
pub struct Done(Arc<AtomicU64>);

impl Done {
    pub fn finished(self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mount's frame loop as the page runs it: input marks it pending
    /// and is laid out by the next frame drawn; every animation frame
    /// draws when something is pending and the pacing allows.
    #[derive(Default)]
    struct Loop {
        pacing: FramePacing,
        pending: Vec<char>,
        drawn: Vec<Vec<char>>,
        gpu: Vec<Done>,
    }

    impl Loop {
        fn input(&mut self, key: char) {
            self.pending.push(key);
        }
        fn animation_frame(&mut self) {
            if self.pending.is_empty() || !self.pacing.may_draw() {
                return;
            }
            self.drawn.push(std::mem::take(&mut self.pending));
            self.gpu.push(self.pacing.submitted());
        }
        fn gpu_finishes_one(&mut self) {
            if !self.gpu.is_empty() {
                self.gpu.remove(0).finished();
            }
        }
    }

    #[test]
    fn keys_typed_while_the_gpu_is_behind_reach_the_next_frame_together() {
        let mut page = Loop::default();
        page.input('a');
        page.animation_frame();
        page.input('b');
        page.animation_frame();
        assert_eq!(
            page.drawn,
            [vec!['a'], vec!['b']],
            "two frames may be in flight"
        );
        // The GPU takes many display frames for each: keys keep coming.
        for key in ['c', 'd', 'e', 'f'] {
            page.input(key);
            page.animation_frame();
        }
        assert_eq!(page.drawn.len(), 2, "no frame queues behind a busy GPU");
        page.gpu_finishes_one();
        page.animation_frame();
        page.animation_frame();
        assert_eq!(
            page.drawn,
            [vec!['a'], vec!['b'], vec!['c', 'd', 'e', 'f']],
            "exactly one following frame draws every key held for it"
        );
        page.gpu_finishes_one();
        page.gpu_finishes_one();
        page.animation_frame();
        assert_eq!(page.drawn.len(), 3, "nothing pending draws nothing");
        page.input('g');
        page.animation_frame();
        assert_eq!(
            page.drawn.last(),
            Some(&vec!['g']),
            "no key is ever dropped"
        );
    }

    #[test]
    fn a_gpu_that_keeps_up_never_holds_a_frame() {
        let typed = "every key gets its own frame";
        let mut page = Loop::default();
        for key in typed.chars() {
            page.input(key);
            page.animation_frame();
            // Done is reported late, after the next frame was submitted.
            if page.gpu.len() > 1 {
                page.gpu_finishes_one();
            }
        }
        assert_eq!(page.drawn.len(), typed.len());
    }
}
